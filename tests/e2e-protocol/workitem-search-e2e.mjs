#!/usr/bin/env node
// workitem 判重检索协议级 E2E（EvoFlow WP-11 A6；flag=RATIFLOW_WORKITEM_FTS 默认 0）：
// 中文子串 similar 仅提示（禁自动合并）、searchRebuild 全量回填、flag 关闭回退。
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
  if (!c) throw new Error(`workitem-search-e2e 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function main() {
  // ============ 场景一：flag 关闭 ============
  {
    const dataDir = mkdtempSync(join(tmpdir(), 'sg-ws-off-'));
    const c = new CoreClient(dataDir, {});
    try {
      await c.call('project.create', { gitlabInstance: 'local', namespace: 'e2e', project: 'wsoff', name: 'WsOff' });
      const wi = await c.call('workitem.create', { projectId: (await c.call('project.list', {})).items[0].id, title: '支付网关超时修复' });
      await expectError(() => c.call('workitem.similar', { workItemId: wi.id }), 'flag 关闭：similar 拒绝');
      await expectErrorCode(() => c.call('workitem.searchRebuild', {}), 'idempotency_key_required', 'P0-1：缺 idempotencyKey 先于 flag 拒绝');
      await expectError(() => c.call('workitem.searchRebuild', { idempotencyKey: 'rb-off' }), 'flag 关闭：searchRebuild 拒绝');
      console.log('场景一（flag 关闭回退）通过');
    } finally {
      c.kill();
      rmSync(dataDir, { recursive: true, force: true });
    }
  }

  async function expectErrorCode(call, code, label) {
    try {
      await call();
    } catch (e) {
      assert((e.code ?? '') === code, `${label}（实际 ${e.code ?? e.message.slice(0, 60)}）`);
      return;
    }
    throw new Error(`workitem-search-e2e 断言失败：${label} 期望错误码 ${code}，但调用成功`);
  }

  async function expectError(call, label) {
    try {
      await call();
    } catch {
      assert(true, label);
      return;
    }
    throw new Error(`workitem-search-e2e 断言失败：${label} 期望错误，但调用成功`);
  }

  // ============ 场景二：判重全链 ============
  {
    const dataDir = mkdtempSync(join(tmpdir(), 'sg-ws-on-'));
    const c = new CoreClient(dataDir, { RATIFLOW_WORKITEM_FTS: '1' });
    try {
      await c.call('project.create', { gitlabInstance: 'local', namespace: 'e2e', project: 'ws', name: 'Ws' });
      const pj = (await c.call('project.list', {})).items[0].id;
      const a = await c.call('workitem.create', { projectId: pj, title: '支付网关超时修复', description: '网关 504' });
      const b = await c.call('workitem.create', { projectId: pj, title: '支付网关重试优化', description: '网关重试' });
      const cc = await c.call('workitem.create', { projectId: pj, title: '知识库导入工具', description: '导入' });

      // 1) 中文子串命中：a 与 b 共享「支付网关」；无关项不入集；排除自身。
      const sim = await c.call('workitem.similar', { workItemId: a.id });
      const ids = sim.items.map((x) => x.workitemId);
      assert(ids.includes(b.id) && !ids.includes(a.id) && !ids.includes(cc.id), `similar 中文子串命中（实际 ${JSON.stringify(ids)}）`);
      assert(typeof sim.items[0].rank === 'number', 'bm25 rank 落值');

      // 2) searchRebuild：全量回填；同 key transport 重放返回首次响应。
      const rb = await c.call('workitem.searchRebuild', { idempotencyKey: 'rb-1' });
      assert(rb.indexed === 3, `全量回填 3 条（实际 ${rb.indexed}）`);
      const rbReplay = await c.call('workitem.searchRebuild', { idempotencyKey: 'rb-1' });
      assert(rbReplay.indexed === rb.indexed, '同 key 重放返回首次响应（不重执行）');
      const sim2 = await c.call('workitem.similar', { workItemId: a.id });
      assert(sim2.items.length === 1, '重建后 similar 结果一致');

      console.log('场景二（判重全链）通过');
    } finally {
      c.kill();
      rmSync(dataDir, { recursive: true, force: true });
    }
  }

  console.log('判重检索协议 E2E 通过。');
}

main().catch((e) => {
  console.error('workitem-search-e2e 失败：', e.message);
  process.exit(1);
});
