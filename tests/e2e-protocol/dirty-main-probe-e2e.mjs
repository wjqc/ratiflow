#!/usr/bin/env node
// dirty main workspace 探针（RDWS 审计 §9.3/§10.2：修改计数必须恒为 0）：
// 真实 Agent Run（含可写工具）跑完后，断言用户主工作区 git status --porcelain
// 为空——所有写只落在受管目录（data-dir artifacts/{runId}），读不产生副作用。
// 前置：cargo build --release -p ratiflow-core。
import { spawn, execSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import * as readline from 'node:readline';

const CORE = process.env.CORE_BIN ?? join(process.cwd(), 'target', 'release', 'ratiflow-core');

class CoreClient {
  constructor(dataDir, env = {}) {
    this.proc = spawn(CORE, ['app-server', '--data-dir', dataDir], { stdio: ['pipe', 'pipe', 'pipe'], env: { ...process.env, ...env } });
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
  async call(method, params = {}) {
    for (let i = 0; i < 100 && !this.hello; i++) {
      await new Promise((r) => setTimeout(r, 50));
    }
    const id = String(this.nextId++);
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.proc.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
    });
  }
  kill() { this.proc.kill('SIGKILL'); }
}

function assert(c, label) {
  if (!c) throw new Error(`dirty-main-probe 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function waitTerminal(client, runId, capMs = 30000) {
  const deadline = Date.now() + capMs;
  for (;;) {
    const run = await client.call('agent.get', { runId });
    if (['completed_execution', 'failed', 'cancelled'].includes(run.status)) return run;
    if (Date.now() > deadline) throw new Error(`等待终态超时：${runId}（当前 ${run.status}）`);
    await new Promise((r) => setTimeout(r, 100));
  }
}

const NOTES = 'dirty-main-probe-baseline';

async function main() {
  // 受管 data-dir 与用户主工作区（git 仓库）严格分离。
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-dirty-'));
  const projDir = mkdtempSync(join(tmpdir(), 'sg-dirty-proj-'));
  writeFileSync(join(projDir, 'NOTES.md'), NOTES);
  execSync(`git -C "${projDir}" init -b main`, { stdio: 'ignore' });
  execSync(`git -C "${projDir}" config user.email e2e@ratiflow.local`, { stdio: 'ignore' });
  execSync(`git -C "${projDir}" config user.name e2e`, { stdio: 'ignore' });
  execSync(`git -C "${projDir}" add .`, { stdio: 'ignore' });
  execSync(`git -C "${projDir}" commit -m init`, { stdio: 'ignore' });

  // 模型脚本：读主工作区文件 + 写草稿（应落受管 artifacts 目录）→ 收尾。
  const script = [
    { content: JSON.stringify({ action: 'read_file', arguments: { path: 'NOTES.md' }, summary: '读主工作区' }), tokensIn: 10, tokensOut: 5 },
    { content: JSON.stringify({ action: 'write_file', arguments: { path: 'probe/out.md', content: '# 探针输出\n主工作区不被改写。' }, summary: '写受管草稿' }), tokensIn: 10, tokensOut: 5 },
    { content: JSON.stringify({ action: 'final', summary: '探针完成' }), tokensIn: 10, tokensOut: 5 },
  ];
  const scriptPath = join(tmpdir(), `sg-dirty-script-${Date.now()}.json`);
  writeFileSync(scriptPath, JSON.stringify(script));

  const c = new CoreClient(dataDir, {
    RATIFLOW_EXEC_MODE: 'safe_restricted',
    RATIFLOW_FAKE_MODEL_SCRIPT: scriptPath,
  });
  try {
    const project = await c.call('project.create', {
      gitlabInstance: 'x', namespace: 'n', project: 'p', name: '探针项目', localRoot: projDir,
    });
    const wi = await c.call('workitem.create', { projectId: project.id, title: 'dirty-main 探针', description: '' });
    const manifest = await c.call('context.create', { projectId: project.id, workItemId: wi.id, query: 'probe', selectedSources: [] });
    const started = await c.call('agent.start', {
      workItemId: wi.id, goal: '读主工作区并写探针草稿', contextManifestId: manifest.id,
      toolAllowlist: ['read_file', 'write_file'], idempotencyKey: 'dirty-probe-1',
    });
    const run = await waitTerminal(c, started.runId);
    assert(run.status === 'completed_execution', `Run 完成（实际 ${run.status}：${run.result}）`);

    // 读真实发生（模型读到主工作区基线内容——读不是写，不产生 dirty）。
    const props = (await c.call('agent.proposals', { runId: started.runId })).items;
    const read = props.find((p) => p.tool === 'read_file');
    assert(read && read.decision === 'executed' && read.result.includes(NOTES), 'read_file 读到主工作区基线内容');

    // 核心断言（§10.2）：主工作区修改计数恒为 0。
    const porcelain = execSync(`git -C "${projDir}" status --porcelain`, { encoding: 'utf8' });
    assert(porcelain === '', `dirty main workspace 计数=0（实际：${JSON.stringify(porcelain)}）`);

    // 写落受管目录（data-dir artifacts/{runId}），而非主工作区。
    const managed = readFileSync(join(dataDir, 'artifacts', started.runId, 'probe', 'out.md'), 'utf8');
    assert(managed.includes('主工作区不被改写'), 'write_file 落受管 artifacts 目录');
    const wf = props.find((p) => p.tool === 'write_file');
    assert(wf && wf.decision === 'executed' && wf.result.includes(join(dataDir, 'artifacts', started.runId)),
      `write_file 结果指向受管目录（${wf?.result}）`);

    // 基线文件内容未被触碰。
    assert(readFileSync(join(projDir, 'NOTES.md'), 'utf8') === NOTES, '主工作区基线文件字节不变');

    console.log('dirty main workspace 探针通过（计数 0，全部写落在受管目录）。');
  } finally {
    c.kill();
    rmSync(dataDir, { recursive: true, force: true });
    rmSync(projDir, { recursive: true, force: true });
    rmSync(scriptPath, { force: true });
  }
}

main().catch((e) => {
  console.error('dirty-main-probe-e2e 失败：', e.message);
  process.exit(1);
});
