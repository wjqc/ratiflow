#!/usr/bin/env node
// RPC 回执 lease 三态协议级 E2E（RDWS 实施计划 v1.4 WP-0 / RDWS-001）：
// ① 缺 idempotencyKey 拒绝（idempotency_key_required，不退化为直接执行）；
// ② 同 key 异指纹拒绝（receipt_fingerprint_mismatch，执行前拦截）；
// ③ 同 key 同参重放返回首次响应（版本不追加 = 不重执行）；
// ④ deterministic 错误 envelope 重放（同 key 重试返回首次错误，版本不追加）；
// ⑤ 并发同 key 只一 owner 获得执行权（双发 create 只追加一个版本）。
// 前置：cargo build --release -p ratiflow-core。
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
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
  kill() { this.proc.kill('SIGKILL'); }
}

function assert(c, label) {
  if (!c) throw new Error(`rpc-receipt-e2e 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function expectErrorCode(call, code, label) {
  try {
    await call();
  } catch (e) {
    assert((e.code ?? '') === code, `${label}（实际 ${e.code ?? e.message.slice(0, 60)}）`);
    return e;
  }
  throw new Error(`rpc-receipt-e2e 断言失败：${label} 期望错误码 ${code}，但调用成功`);
}

const GATES = (n) => [
  { gateId: 'a', title: '关-a', purpose: '第一关', deliverables: ['doc'] },
  { gateId: 'b', title: '关-b', purpose: '第二关', deliverables: ['code'] },
  { gateId: 'c', title: '关-c', purpose: `第${n}关`, deliverables: ['verification'] },
];

async function templateVersions(c, key) {
  const list = await c.call('workflowTemplate.list', {});
  return (list.items.find((t) => t.key === key)?.versions ?? []).length;
}

async function main() {
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-rpc-receipt-'));
  const c = new CoreClient(dataDir, { RATIFLOW_WORKFLOW_TEMPLATE_V2: '1' });
  const fail = (error) => {
    console.error(`E2E 失败：${error.message}`);
    c?.kill();
    rmSync(dataDir, { recursive: true, force: true });
    process.exit(1);
  };

  try {
    await c.hello_();
    await c.call('project.create', { gitlabInstance: 'local', namespace: 'e2e', project: 'receipt', name: 'Receipt' });

    // ① 缺 idempotencyKey：receipt 门控 mutation 一律拒绝，不退化为直接执行。
    await expectErrorCode(
      () => c.call('workflowTemplate.create', { key: 'no-key-tpl', name: '缺键', gates: GATES('三') }),
      'idempotency_key_required',
      '① 缺 idempotencyKey 拒绝',
    );
    assert(await templateVersions(c, 'no-key-tpl') === 0, '① 拒绝后模板零版本（未执行 mutation）');

    // ② 同 key 异指纹：执行前拒绝。
    const k2 = 'fp-key-1';
    await c.call('workflowTemplate.create', { key: 'fp-tpl', name: '指纹', gates: GATES('三'), idempotencyKey: k2 });
    assert(await templateVersions(c, 'fp-tpl') === 1, '② 首次创建 v1');
    await expectErrorCode(
      () => c.call('workflowTemplate.create', { key: 'fp-tpl', name: '指纹改', gates: GATES('四'), idempotencyKey: k2 }),
      'receipt_fingerprint_mismatch',
      '② 同 key 异指纹拒绝',
    );
    assert(await templateVersions(c, 'fp-tpl') === 1, '② 指纹拒绝不追加版本');

    // ③ 同 key 同参重放：返回首次响应（versionId 一致），版本数不涨。
    const replay = await c.call('workflowTemplate.create', { key: 'fp-tpl', name: '指纹', gates: GATES('三'), idempotencyKey: k2 });
    assert(await templateVersions(c, 'fp-tpl') === 1, '③ 重放不追加版本（仍 v1）');
    assert(replay.version.version_no === 1, '③ 重放返回首次版本号');

    // ④ deterministic 错误 envelope 重放：重复 gateId 首错后，同 key 重试返回同一错误且不建模板。
    const k4 = 'err-key-1';
    const badGates = [
      { gateId: 'dup', title: 'a', deliverables: ['doc'] },
      { gateId: 'dup', title: 'b', deliverables: ['doc'] },
    ];
    const e1 = await expectErrorCode(
      () => c.call('workflowTemplate.create', { key: 'bad-tpl', name: '坏', gates: badGates, idempotencyKey: k4 }),
      'invalid_params',
      '④ 首次 deterministic 错误',
    );
    const e2 = await expectErrorCode(
      () => c.call('workflowTemplate.create', { key: 'bad-tpl', name: '坏', gates: badGates, idempotencyKey: k4 }),
      'invalid_params',
      '④ 错误 envelope 原样重放',
    );
    assert(e1.message === e2.message, '④ 重放错误与首错一致');
    assert(await templateVersions(c, 'bad-tpl') === 0, '④ 错误重放不建模板');

    // ⑤ 并发同 key：双发 create（该域语义 = 追加新 draft 版本），只一 owner 执行 → 版本只 +1。
    const k5 = 'race-key-1';
    const results = await Promise.allSettled([
      c.call('workflowTemplate.create', { key: 'race-tpl', name: '竞态', gates: GATES('三'), idempotencyKey: k5 }),
      c.call('workflowTemplate.create', { key: 'race-tpl', name: '竞态', gates: GATES('三'), idempotencyKey: k5 }),
    ]);
    const okCount = results.filter((r) => r.status === 'fulfilled').length;
    assert(okCount >= 1, `⑤ 并发至少一次成功（${okCount}/2）`);
    const rejected = results.filter((r) => r.status === 'rejected').map((r) => r.reason.code ?? r.reason.message);
    assert(rejected.every((code) => code === 'conflict' || code === 'receipt_fingerprint_mismatch'),
      `⑤ 竞争失败方只允许 in_flight 冲突（实际 ${JSON.stringify(rejected)}）`);
    assert(await templateVersions(c, 'race-tpl') === 1, '⑤ 并发只追加一个版本（单 owner 执行）');

    console.log('rpc-receipt-e2e 全部通过');
  } catch (error) {
    fail(error);
  } finally {
    c.kill();
    rmSync(dataDir, { recursive: true, force: true });
  }
}

main().catch((e) => { console.error(`E2E 失败：${e.message}`); process.exit(1); });
