// M0-② 协议级 Agent 生命周期 E2E：agent.start 唯一入口、非阻塞、事件推送、终态与幂等。
// 前置：cargo build --release -p sixgates-core；无模型环境变量（FakeModel 脚本耗尽 → model_unavailable）。
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import readline from 'node:readline';

const CORE = process.env.CORE_BIN ?? join(process.cwd(), 'target', 'release', 'sixgates-core');
const dataDir = mkdtempSync(join(tmpdir(), 'sg-agent-e2e-'));

class CoreClient {
  constructor(binary, dataDir, env = {}) {
    this.proc = spawn(binary, ['app-server', '--data-dir', dataDir],
      { stdio: ['pipe', 'pipe', 'ignore'], env: { ...process.env, ...env } });
    this.nextId = 1;
    this.pending = new Map();
    this.events = [];
    this.rl = readline.createInterface({ input: this.proc.stdout });
    this.rl.on('line', (line) => this.onLine(line));
  }
  onLine(line) {
    if (!line.trim()) return;
    let message;
    try { message = JSON.parse(line); } catch { return; }
    if (message.method === 'event' && message.params) {
      this.events.push(message.params);
      return;
    }
    if (message.protocolVersion) { this.hello = message; return; }
    if (message.id && this.pending.has(message.id)) {
      const { resolve, reject } = this.pending.get(message.id);
      this.pending.delete(message.id);
      if (message.error) {
        reject(Object.assign(new Error(message.error.data?.detail ?? message.error.message), { code: message.error.message }));
      } else {
        resolve(message.result);
      }
    }
  }
  call(method, params = {}) {
    const id = String(this.nextId++);
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.proc.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
    });
  }
  async rpc(method, params) {
    for (let i = 0; i < 100 && !this.hello; i++) {
      await new Promise((r) => setTimeout(r, 50));
    }
    return this.call(method, params);
  }
  kill() { this.proc.kill('SIGKILL'); }
}

function assert(condition, label) {
  if (!condition) {
    throw new Error(`agent-e2e 断言失败：${label}`);
  }
  console.log(`  ✓ ${label}`);
}

async function waitTerminal(client, runId, capMs = 15000) {
  const deadline = Date.now() + capMs;
  for (;;) {
    const run = await client.rpc('agent.get', { runId });
    if (['completed_execution', 'failed', 'cancelled'].includes(run.status)) return run;
    if (Date.now() > deadline) throw new Error(`等待终态超时：${runId}（当前 ${run.status}）`);
    await new Promise((r) => setTimeout(r, 100));
  }
}

const client = new CoreClient(CORE, dataDir);
try {
  assert((await client.rpc('core.version')).protocolVersion === '1', 'hello/版本握手');

  const project = await client.rpc('project.create', {
    gitlabInstance: 'x', namespace: 'n', project: 'p', name: 'agent-e2e',
  });
  const wi = await client.rpc('workitem.create', { projectId: project.id, title: '生命周期', description: '' });
  const manifest = await client.rpc('context.create', {
    projectId: project.id, workItemId: wi.id, query: 'e2e', selectedSources: [],
  });

  // 1) 唯一入口：立即返回 runId（<500ms，本地不含模型往返）。
  const t0 = Date.now();
  const started = await client.rpc('agent.start', {
    workItemId: wi.id, goal: '生命周期验证', contextManifestId: manifest.id,
    toolAllowlist: ['read_file'], idempotencyKey: 'agent-e2e-1',
  });
  assert(!!started.runId, `agent.start 返回 runId（${Date.now() - t0}ms）`);
  assert(Date.now() - t0 < 500, 'agent.start 不执行循环（立即返回）');

  // 2) 非阻塞：Run 进行中其余 RPC 立即返回。
  const t1 = Date.now();
  await client.rpc('project.list');
  assert(Date.now() - t1 < 300, 'Run 期间 project.list 立即返回（读循环+DB actor 双并发）');

  // 3) 未配模型 → 终态 failed（model_unavailable），绝不 completed_execution。
  const run = await waitTerminal(client, started.runId);
  assert(run.status === 'failed', `终态 failed（result=${run.result}）`);
  assert(run.result.includes('model_unavailable'), '失败原因含 model_unavailable');

  // 4) 幂等：同 idempotencyKey 重放返回同一 runId，不再派发任务。
  const replay = await client.rpc('agent.start', {
    workItemId: wi.id, goal: '生命周期验证', contextManifestId: manifest.id,
    toolAllowlist: ['read_file'], idempotencyKey: 'agent-e2e-1',
  });
  assert(replay.runId === started.runId, '幂等重放返回同一 runId');

  // 5) 取消语义：终态 Run 再取消 → 返回既有终态（不重复事件）。
  const cancel = await client.rpc('agent.cancel', { runId: started.runId });
  assert(cancel.status === 'failed', '终态 Run 取消幂等（返回既有终态）');

  // 6) 事件通知：run.started / run.failed 到达（flush 双路：响应后即时 + 250ms 兜底）。
  for (let i = 0; i < 20 && client.events.filter((e) => e.type === 'run.started').length === 0; i++) {
    await new Promise((r) => setTimeout(r, 100));
  }
  const types = client.events.map((e) => e.type);
  assert(types.includes('run.started'), '收到 run.started 通知');
  assert(types.includes('run.failed'), '收到 run.failed 通知');
  assert(types.filter((t) => t === 'run.failed').length === 1, 'run.failed 恰好一条');

  // 7) 契约唯一性：agent.run 已从契约移除 → 调用返回 method_not_found。
  const removed = await client.call('agent.run', {}).catch((e) => e);
  assert(removed && removed.code === 'method_not_found', 'agent.run 已移除（method_not_found）');

  console.log('\nAgent 生命周期 E2E（基础）通过。');

  // ===== 工具执行场景（F05/M0-③）：脚本模型 + safe_restricted + 真实项目根 =====
  const projDir = mkdtempSync(join(tmpdir(), 'sg-agent-e2e-proj-'));
  writeFileSync(join(projDir, 'NOTES.md'), 'hello-m03');
  const script = [
    { content: '{"action":"read_file","arguments":{"path":"NOTES.md"},"summary":"读取"}', tokensIn: 10, tokensOut: 5 },
    { content: '{"action":"search_knowledge","arguments":{"query":"hello","limit":3},"summary":"检索"}', tokensIn: 10, tokensOut: 5 },
    { content: '{"action":"run_command","arguments":{"argv":["wc","-c","NOTES.md"]},"summary":"统计"}', tokensIn: 10, tokensOut: 5 },
    { content: '{"action":"write_file","arguments":{"path":"drafts/out.md","content":"# M0-3 草稿"},"summary":"写草稿"}', tokensIn: 10, tokensOut: 5 },
    { content: '{"action":"final","summary":"工具链验证完成"}', tokensIn: 10, tokensOut: 5 },
  ];
  const scriptPath = join(tmpdir(), `sg-agent-e2e-script-${Date.now()}.json`);
  writeFileSync(scriptPath, JSON.stringify(script));
  const dataDir2 = mkdtempSync(join(tmpdir(), 'sg-agent-e2e2-'));
  const toolClient = new CoreClient(CORE, dataDir2, {
    SIXGATES_EXEC_MODE: 'safe_restricted',     // 确定性执行模式（无 Docker 依赖）
    SIXGATES_FAKE_MODEL_SCRIPT: scriptPath,    // 脚本模型（未配置真实模型时生效）
  });
  try {
    const project = await toolClient.rpc('project.create', {
      gitlabInstance: 'x', namespace: 'n', project: 'p', name: '工具链', localRoot: projDir,
    });
    const wi = await toolClient.rpc('workitem.create', { projectId: project.id, title: '工具链验证', description: '' });
    const manifest = await toolClient.rpc('context.create', {
      projectId: project.id, workItemId: wi.id, query: 'e2e', selectedSources: [],
    });
    const source = await toolClient.rpc('knowledge.create', {
      projectId: project.id, kind: 'repo_path', name: 'proj', locator: projDir,
    });
    await toolClient.rpc('knowledge.scan', { sourceId: source.id, projectRoot: projDir });

    // tool.list 由运行时注册表驱动（含 search_knowledge 与限制字段）。
    const tools = await toolClient.rpc('tool.list');
    const names = tools.items.map((t) => t.tool_id ?? t.name);
    assert(names.includes('search_knowledge'), 'tool.list 含注册表新工具 search_knowledge');
    assert(tools.items.find((t) => (t.tool_id ?? t.name) === 'run_command').max_result_bytes > 0, 'tool.list 带限制字段 max_result_bytes');

    const started = await toolClient.rpc('agent.start', {
      workItemId: wi.id, goal: '读文件→检索→命令→写草稿', contextManifestId: manifest.id,
      toolAllowlist: ['read_file', 'search_knowledge', 'run_command', 'write_file'],
      idempotencyKey: 'agent-e2e-tools-1',
    });
    const run = await waitTerminal(toolClient, started.runId, 30000);
    assert(run.status === 'completed_execution', `工具链 Run 终态 completed_execution（实际 ${run.status}：${run.result}）`);

    const props = (await toolClient.rpc('agent.proposals', { runId: started.runId })).items;
    const byTool = Object.fromEntries(props.map((p) => [p.tool, p]));
    assert(props.length === 4, `4 个提案（实际 ${props.length}）`);
    assert(byTool.read_file.decision === 'executed' && byTool.read_file.result.includes('hello-m03'),
      'read_file 带真实参数读到真实内容');
    assert(byTool.search_knowledge.decision === 'executed' && byTool.search_knowledge.result.includes('NOTES.md'),
      'search_knowledge 库内检索命中项目文件');
    assert(byTool.run_command.decision === 'rejected' && byTool.run_command.result.includes('approval_required'),
      'run_command 高风险未批不执行（approval_required）');
    assert(byTool.write_file.decision === 'executed' && byTool.write_file.result.includes('written'),
      'write_file 落工件草稿区');
    const pending = await toolClient.rpc('approval.list', { limit: 10 });
    assert(pending.items.length >= 1, '审批中心出现 run_command 待审批项');

    console.log('\nAgent 工具链 E2E 通过。');
  } finally {
    toolClient.kill();
    rmSync(dataDir2, { recursive: true, force: true });
    rmSync(projDir, { recursive: true, force: true });
    rmSync(scriptPath, { force: true });
  }
} finally {
  client.kill();
  rmSync(dataDir, { recursive: true, force: true });
}
