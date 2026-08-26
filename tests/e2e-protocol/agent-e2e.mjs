// M0-② 协议级 Agent 生命周期 E2E：agent.start 唯一入口、非阻塞、事件推送、终态与幂等。
// 前置：cargo build --release -p sixgates-core；无模型环境变量（FakeModel 脚本耗尽 → model_unavailable）。
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import readline from 'node:readline';

const CORE = process.env.CORE_BIN ?? join(process.cwd(), 'target', 'release', 'sixgates-core');
const dataDir = mkdtempSync(join(tmpdir(), 'sg-agent-e2e-'));

class CoreClient {
  constructor(binary, dataDir) {
    this.proc = spawn(binary, ['app-server', '--data-dir', dataDir], { stdio: ['pipe', 'pipe', 'ignore'] });
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

  console.log('\nAgent 生命周期 E2E 通过。');
} finally {
  client.kill();
  rmSync(dataDir, { recursive: true, force: true });
}
