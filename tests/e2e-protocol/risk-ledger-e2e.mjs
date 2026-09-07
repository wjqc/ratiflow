#!/usr/bin/env node
// WP-1 统一风险模型 + Grant 计量协议级 E2E（RDWS 实施计划 v1.4 / RDWS-002·003）：
// ① autonomy.createGrant limits 维度校验（未知维度拒绝）；
// ② Grant 绑定 Run 的模型消费 reserve 超限 → run failed(autonomy_budget_exhausted)，
//    拒绝发生在调用前（零消耗）；
// ③ 宽限额 Grant：无模型环境 run 照常走 open-phase 失败（model_unavailable），
//    记账路径不改变既有失败语义；
// ④ SIXGATES_UNIFIED_RISK=1 不破坏无 grant 的常规生命周期（kill-switch 探针面）。
// 硬规则穷举与并发 reserve/settle 的权威断言在 sg-policy unit（risk_model/ledger_tests）。
// 前置：cargo build --release -p sixgates-core。
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import * as readline from 'node:readline';

const CORE = process.env.CORE_BIN ?? join(process.cwd(), 'target', 'release', 'sixgates-core');

class CoreClient {
  constructor(dataDir, env = {}) {
    this.proc = spawn(CORE, ['app-server', '--data-dir', dataDir], { stdio: ['pipe', 'pipe', 'ignore'], env: { ...process.env, ...env } });
    this.nextId = 1;
    this.pending = new Map();
    this.rl = readline.createInterface({ input: this.proc.stdout });
    this.rl.on('line', (l) => this.onLine(l));
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
  async hello_(timeoutMs = 5000) {
    const deadline = Date.now() + timeoutMs;
    while (!this.hello) {
      if (Date.now() > deadline) throw new Error('等待 hello 超时');
      await new Promise((r) => setTimeout(r, 25));
    }
    return this.hello;
  }
  call(method, params = {}) {
    const id = String(this.nextId++);
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.proc.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
    });
  }
  async rpc(method, params) {
    for (let i = 0; i < 100 && !this.hello; i++) await new Promise((r) => setTimeout(r, 50));
    return this.call(method, params);
  }
  kill() { this.proc.kill('SIGKILL'); }
}

function assert(c, label) {
  if (!c) throw new Error(`risk-ledger-e2e 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function waitTerminal(c, runId, capMs = 20000) {
  const deadline = Date.now() + capMs;
  for (;;) {
    const run = await c.rpc('agent.get', { runId });
    if (['completed_execution', 'failed', 'cancelled'].includes(run.status)) return run;
    if (Date.now() > deadline) throw new Error(`等待终态超时（当前 ${run.status}）`);
    await new Promise((r) => setTimeout(r, 100));
  }
}

async function expectErrorCode(call, code, label) {
  try { await call(); } catch (e) {
    assert((e.code ?? '') === code, `${label}（实际 ${e.code ?? e.message.slice(0, 60)}）`);
    return e;
  }
  throw new Error(`risk-ledger-e2e 断言失败：${label} 期望错误码 ${code}，但调用成功`);
}

async function main() {
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-risk-ledger-'));
  const c = new CoreClient(dataDir, {
    SIXGATES_AUTOMATIONS: '1',
    SIXGATES_UNIFIED_RISK: '1',
  });
  const fail = (error) => {
    console.error(`E2E 失败：${error.message}`);
    c?.kill();
    rmSync(dataDir, { recursive: true, force: true });
    process.exit(1);
  };

  try {
    await c.hello_();
    const project = await c.rpc('project.create', { gitlabInstance: 'x', namespace: 'n', project: 'p', name: 'risk-ledger' });
    const wi = await c.rpc('workitem.create', { projectId: project.id, title: 'Grant 计量', description: '' });
    const manifest = await c.rpc('context.create', { projectId: project.id, workItemId: wi.id, query: 'e2e', selectedSources: [] });

    // ① limits 维度校验：未知维度拒绝（ledger 维度枚举是唯一合法集）。
    await expectErrorCode(
      () => c.rpc('autonomy.createGrant', { workItemId: wi.id, limits: { widgets: 10 } }),
      'invalid_params',
      '① 未知限额维度拒绝',
    );
    const tight = await c.rpc('autonomy.createGrant', { workItemId: wi.id, limits: { tokens_in: 1 } });
    assert(!!tight.grantId, '① 合法限额 grant 创建');

    // ② 紧限额：tokens_in 上界估算必然 > 1 → 调用前 reserve 拒绝（零消耗）。
    const r1 = await c.rpc('agent.start', {
      workItemId: wi.id, goal: '紧限额验证', contextManifestId: manifest.id,
      toolAllowlist: ['read_file'], idempotencyKey: 'rl-tight-1', autonomyGrantId: tight.grantId,
    });
    const t1 = await waitTerminal(c, r1.runId);
    assert(t1.status === 'failed', `② 紧限额 run 失败（实际 ${t1.status}）`);
    assert(/autonomy_budget_exhausted/.test(t1.result ?? ''), `② 失败原因为预算耗尽（实际 ${String(t1.result).slice(0, 60)}）`);

    // ③ 宽限额：reserve 通过；无模型环境照常 open-phase 失败（记账不改变失败语义）。
    const wide = await c.rpc('autonomy.createGrant', { workItemId: wi.id, limits: { model_calls: 100, tokens_in: 10_000_000, tokens_out: 1_000_000 } });
    const r2 = await c.rpc('agent.start', {
      workItemId: wi.id, goal: '宽限额验证', contextManifestId: manifest.id,
      toolAllowlist: ['read_file'], idempotencyKey: 'rl-wide-1', autonomyGrantId: wide.grantId,
    });
    const t2 = await waitTerminal(c, r2.runId);
    assert(t2.status === 'failed', '③ 无模型 run 失败（预期）');
    assert(!/autonomy_budget_exhausted/.test(t2.result ?? ''), `③ 宽限额不触发预算拒绝（实际 ${String(t2.result).slice(0, 60)}）`);

    // ④ 无 grant 的常规生命周期：UNIFIED_RISK=1 不破坏默认路径（run 正常进入模型失败终态）。
    const r3 = await c.rpc('agent.start', {
      workItemId: wi.id, goal: '无 grant 常规路径', contextManifestId: manifest.id,
      toolAllowlist: ['read_file'], idempotencyKey: 'rl-plain-1',
    });
    const t3 = await waitTerminal(c, r3.runId);
    assert(['failed', 'completed_execution'].includes(t3.status), '④ 无 grant 生命周期不受 flag 影响');

    console.log('risk-ledger-e2e 全部通过');
  } catch (error) {
    fail(error);
  } finally {
    c.kill();
    rmSync(dataDir, { recursive: true, force: true });
  }
}

main().catch((e) => { console.error(`E2E 失败：${e.message}`); process.exit(1); });
