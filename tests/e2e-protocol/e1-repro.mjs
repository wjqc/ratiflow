#!/usr/bin/env node
// 复现 E1 核心：settings.update → backup.create/verify → settings.update → restore → 重启 → settings.get
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import * as readline from 'node:readline';

const CORE = join(process.cwd(), 'target', 'release', 'ratiflow-core');
const dataDir = mkdtempSync(join(tmpdir(), 'sg-e1-repro-'));

class CoreClient {
  constructor(dir) {
    this.proc = spawn(CORE, ['app-server', '--data-dir', dir], { stdio: ['pipe', 'pipe', 'pipe'] });
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
  if (!c) throw new Error(`断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function waitHello(c) {
  for (let i = 0; i < 50 && !c.hello; i++) await new Promise((r) => setTimeout(r, 100));
}

async function main() {
  // 第一阶段
  let c = new CoreClient(dataDir);
  await waitHello(c);
  assert(c.hello?.protocolVersion === '1', 'hello 握手');

  // 1. 设置 24h
  await c.call('settings.update', {
    scope: 'global',
    patches: [{ key: 'app.general', value: { timeFormat: '24h' }, expectedRevision: null }],
  });
  let g = await c.call('settings.get', { scope: 'global', keys: ['app.general'] });
  console.log('  设置后:', JSON.stringify(g.items?.[0]?.value));

  // 2. 创建项目+备份
  await c.call('project.create', { gitlabInstance: 'x', namespace: 'n', project: 'p', name: 'P' });
  const backup = await c.call('backup.create');
  console.log('  备份:', backup.id, backup.path);
  const verified = await c.call('backup.verify', { backupId: backup.id });
  assert(verified.verified === true, 'verify 通过');

  // 读取备份 db 文件中的 app_settings，确认备份时点是 24h
  // 直接读 sqlite 不方便，先通过 backup 的 manifest 确认 schema，稍后用新连接检查
  const backupBody = readFileSync(backup.path);
  assert(backupBody.length > 0, '备份文件非空');

  // 3. 修改设置为 system
  const entries = await c.call('settings.get', { scope: 'global', keys: ['app.general'] });
  const rev = entries.items.find((i) => i.key === 'app.general')?.revision ?? null;
  await c.call('settings.update', {
    scope: 'global',
    patches: [{ key: 'app.general', value: { timeFormat: 'system', mutated: true }, expectedRevision: rev }],
  });
  g = await c.call('settings.get', { scope: 'global', keys: ['app.general'] });
  console.log('  修改后:', JSON.stringify(g.items?.[0]?.value));

  // 4. restore
  const outcome = await c.call('backup.restore', { backupId: backup.id });
  assert(outcome.restored === true && outcome.requiresRestart === true, 'restore 返回 requiresRestart');

  // 5. 先不复用连接（core 可能仍持有），直接杀进程重启
  c.kill();
  await new Promise((r) => setTimeout(r, 500));

  c = new CoreClient(dataDir);
  await waitHello(c);
  const meta = await c.call('core.version');
  console.log('  schema:', meta.schemaVersion);
  g = await c.call('settings.get', { scope: 'global', keys: ['app.general'] });
  console.log('  重启后:', JSON.stringify(g.items?.[0]?.value));
  assert(g.items?.[0]?.value?.timeFormat === '24h', 'restore 后 timeFormat 回到 24h');
  assert(g.items?.[0]?.value?.mutated === undefined, 'restore 后 mutated 消失');
  c.kill();
  rmSync(dataDir, { recursive: true, force: true });
  console.log('E1 复现通过 ✓');
}

main().catch((e) => {
  console.error('FAILED:', e.message);
  process.exit(1);
});
