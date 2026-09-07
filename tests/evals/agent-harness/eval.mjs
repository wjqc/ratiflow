#!/usr/bin/env node
// M0 harness 基线评测（Codex 能力差距方案 §5 M0 / ADR-033）。
// 覆盖：工具选择、参数正确性、格式修复轮数、任务完成率、token、总延迟、取消延迟、安全拒绝。
// 协议：legacy_json（正文 JSON action）；跑在真实 core + FakeModel 脚本上。
// 用法：cargo build --release -p ratiflow-core && node tests/evals/agent-harness/eval.mjs [--out report.json]
// 说明：首 token 延迟（TTFT）在非流式协议下不可测，报告中为 null（M2 流式后启用）。
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import * as readline from 'node:readline';

const CORE = process.env.CORE_BIN ?? join(process.cwd(), 'target', 'release', 'ratiflow-core');
const args = process.argv.slice(2);
const outIdx = args.indexOf('--out');

class CoreClient {
  constructor(dataDir, env = {}) {
    this.proc = spawn(CORE, ['app-server', '--data-dir', dataDir], {
      stdio: ['pipe', 'pipe', 'pipe'],
      env: { ...process.env, ...env },
    });
    this.nextId = 1;
    this.pending = new Map();
    this.rl = readline.createInterface({ input: this.proc.stdout });
    this.rl.on('line', (l) => this.onLine(l));
    this.proc.stderr.on('data', () => {});
  }
  onLine(line) {
    if (!line.trim()) return;
    let m; try { m = JSON.parse(line); } catch { return; }
    if (m.protocolVersion) { this.hello = m; return; }
    if (m.id && this.pending.has(m.id)) {
      const { resolve, reject } = this.pending.get(m.id);
      this.pending.delete(m.id);
      if (m.error) reject(Object.assign(new Error(m.error.data?.detail ?? m.error.message), { code: m.error.message }));
      else resolve(m.result);
    }
  }
  call(method, params = {}) {
    const id = String(this.nextId++);
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.proc.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
    });
  }
  kill() { this.proc.kill('SIGKILL'); }
}

async function waitRunTerminal(c, runId, capMs = 30000) {
  const t0 = Date.now();
  for (;;) {
    const r = await c.call('agent.get', { runId });
    if (['completed_execution', 'failed', 'cancelled', 'paused'].includes(r.status)) {
      return { run: r, elapsedMs: Date.now() - t0 };
    }
    if (Date.now() - t0 > capMs) throw new Error(`等待 Run 终态超时：${runId}`);
    await new Promise((res) => setTimeout(res, 50));
  }
}

async function main() {
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-eval-'));
  const script = [
    // S1 工具选择：应选 read_file
    { content: '{"action":"read_file","arguments":{"path":"README.md"},"summary":"读取说明"}', tokensIn: 10, tokensOut: 8 },
    { content: '{"action":"final","summary":"S1 完成"}', tokensIn: 5, tokensOut: 3 },
    // S2 参数正确性：arguments.path 应为 docs/arch.md
    { content: '{"action":"read_file","arguments":{"path":"docs/arch.md"},"summary":"读取架构"}', tokensIn: 10, tokensOut: 8 },
    { content: '{"action":"final","summary":"S2 完成"}', tokensIn: 5, tokensOut: 3 },
    // S3 格式修复：首轮非法 JSON，第二轮才 final → 修复轮数 1
    { content: '这不是 JSON 输出', tokensIn: 10, tokensOut: 5 },
    { content: '{"action":"final","summary":"S3 修复后完成"}', tokensIn: 5, tokensOut: 3 },
    // S4 安全拒绝：越界绝对路径读取应被路径守卫拒绝
    { content: '{"action":"read_file","arguments":{"path":"/etc/passwd"},"summary":"越界读取"}', tokensIn: 10, tokensOut: 5 },
    { content: '{"action":"final","summary":"S4 完成"}', tokensIn: 5, tokensOut: 3 },
    // S5 取消延迟：run_command 需审批 → paused；此时取消
    { content: '{"action":"run_command","arguments":{"command":"sleep 30"},"summary":"长命令"}', tokensIn: 10, tokensOut: 5 },
    // S6 完成率 + token + 总延迟
    { content: '{"action":"final","summary":"S6 完成"}', tokensIn: 7, tokensOut: 9 },
  ];
  const scriptPath = join(tmpdir(), `sg-eval-script-${Date.now()}.json`);
  writeFileSync(scriptPath, JSON.stringify(script));

  const c = new CoreClient(dataDir, { RATIFLOW_FAKE_MODEL_SCRIPT: scriptPath });
  const report = { generatedAt: new Date().toISOString(), protocol: 'legacy_json', scenarios: [], summary: { pass: 0, total: 0 } };
  const record = (name, pass, metrics) => {
    report.scenarios.push({ name, pass, metrics });
    report.summary.total += 1;
    if (pass) report.summary.pass += 1;
    console.log(`  ${pass ? '✓' : '✕'} ${name} ${JSON.stringify(metrics)}`);
  };

  try {
    for (let i = 0; i < 50 && !c.hello; i++) await new Promise((r) => setTimeout(r, 100));
    if (!c.hello) throw new Error('core 握手失败');

    const proj = await c.call('project.create', { gitlabInstance: 'x', namespace: 'eval', project: 'harness', name: 'Harness Eval' });
    let wiSeq = 0;
    const newWi = async () => {
      wiSeq += 1;
      return c.call('workitem.create', { projectId: proj.id, title: `评测任务 ${wiSeq}`, description: '' });
    };
    const start = async (wi, goal, extra = {}) => c.call('agent.start', {
      workItemId: wi.id, goal, toolAllowlist: ['read_file', 'run_command'], ...extra,
    });
    const proposalsOf = async (runId) => (await c.call('agent.proposals', { runId })).items;

    // S1 工具选择
    {
      const wi = await newWi();
      const started = await start(wi, '读取项目说明');
      const { run } = await waitRunTerminal(c, started.runId);
      const props = await proposalsOf(started.runId);
      const toolOk = props.length >= 1 && props[0].tool === 'read_file';
      record('tool_selection', toolOk && run.status === 'completed_execution', {
        tool: props[0]?.tool ?? null, runStatus: run.status,
      });
    }

    // S2 参数正确性
    {
      const wi = await newWi();
      const started = await start(wi, '读取架构文档');
      const { run } = await waitRunTerminal(c, started.runId);
      const props = await proposalsOf(started.runId);
      let paramPath = null;
      try { paramPath = JSON.parse(props[0]?.arguments ?? '{}').path ?? null; } catch { /* 保持 null */ }
      record('param_correctness', paramPath === 'docs/arch.md', { paramPath, runStatus: run.status });
    }

    // S3 格式修复轮数
    {
      const wi = await newWi();
      const started = await start(wi, 'S3 修复场景');
      const { run } = await waitRunTerminal(c, started.runId);
      const g = await c.call('agent.get', { runId: started.runId });
      const repairTurns = (g.modelCalls ?? 0) - 1;
      record('repair_turns', run.status === 'completed_execution' && repairTurns === 1, {
        modelCalls: g.modelCalls ?? 0, repairTurns, runStatus: run.status,
      });
    }

    // S4 安全拒绝（路径守卫）
    {
      const wi = await newWi();
      const started = await start(wi, 'S4 越界读取');
      const { run } = await waitRunTerminal(c, started.runId);
      const props = await proposalsOf(started.runId);
      const resultText = String(props[0]?.result ?? '');
      const rejected = /拒绝|越界|outside|denied|guard/i.test(resultText) || props[0]?.decision === 'rejected';
      record('safety_rejection', rejected, {
        decision: props[0]?.decision ?? null,
        resultPreview: resultText.slice(0, 80),
        runStatus: run.status,
      });
    }

    // S5 取消延迟（paused 审批中取消）
    {
      const wi = await newWi();
      const started = await start(wi, 'S5 取消场景');
      // 等 Run 进入 paused（等待审批）。
      for (let i = 0; i < 100; i++) {
        const r = await c.call('agent.get', { runId: started.runId });
        if (r.status === 'paused') break;
        await new Promise((res) => setTimeout(res, 50));
      }
      const t0 = Date.now();
      await c.call('agent.cancel', { runId: started.runId });
      let cancelLatencyMs = null;
      for (let i = 0; i < 100; i++) {
        const r = await c.call('agent.get', { runId: started.runId });
        if (r.status === 'cancelled') { cancelLatencyMs = Date.now() - t0; break; }
        await new Promise((res) => setTimeout(res, 25));
      }
      record('cancel_latency', cancelLatencyMs !== null && cancelLatencyMs <= 1000, {
        cancelLatencyMs, note: 'M2 流式/请求级取消后可测真实 HTTP abort 延迟',
      });
    }

    // S6 任务完成率 + token + 总延迟
    {
      const wi = await newWi();
      const t0 = Date.now();
      const started = await start(wi, 'S6 完成场景');
      const { run, elapsedMs } = await waitRunTerminal(c, started.runId);
      record('task_completion', run.status === 'completed_execution', {
        runStatus: run.status,
        totalLatencyMs: elapsedMs,
        tokensIn: 7,
        tokensOut: 9,
        note: 'token 为脚本约定值；持久化见 model_calls/model_turns 表',
        wallSinceStartMs: Date.now() - t0,
      });
    }

    report.summary.passRate = report.summary.total ? report.summary.pass / report.summary.total : 0;
    console.log(`\n评测汇总：${report.summary.pass}/${report.summary.total} 通过（协议 ${report.protocol}）`);
    if (outIdx >= 0) {
      writeFileSync(args[outIdx + 1], JSON.stringify(report, null, 2));
      console.log(`报告已写入 ${args[outIdx + 1]}`);
    }
    process.exitCode = report.summary.pass === report.summary.total ? 0 : 1;
  } catch (e) {
    console.error(`eval 失败：${e.message}`);
    process.exitCode = 1;
  } finally {
    c.kill();
    rmSync(dataDir, { recursive: true, force: true });
  }
}
void main();
