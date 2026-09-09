#!/usr/bin/env node
// 任务级默认运行面 + run 显式技能面 E2E（规范技能化接入）：
// ① skill.importDirectory：目录→draft 技能（路径消歧命名/幂等重导/子目录过滤/秘密 fail-closed）
// ② 激活后 activeList 带版本 id；"/" 语义 = run 显式技能面替换全局 enabled 注入面
// ③ agent.get 技能冻结证据（agent_run_skills）
// ④ "@" 语义 = workitem 默认 Agent（create 冻结 + updateRunDefaults 调整）
// ⑤ 选路证据：workitem_default 档压过项目绑定；revoked 版本 fail-closed
import { spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import * as readline from 'node:readline';

const CORE = process.env.CORE_BIN ?? join(process.cwd(), 'target', 'release', 'ratiflow-core');

class CoreClient {
  constructor(dataDir) {
    this.proc = spawn(CORE, ['app-server', '--data-dir', dataDir], { stdio: ['pipe', 'pipe', 'pipe'] });
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

function assert(c, label, actual) {
  if (!c) throw new Error(`任务默认面 E2E 断言失败：${label}${actual !== undefined ? `（实际 ${JSON.stringify(actual)}）` : ''}`);
  console.log(`  ✓ ${label}${actual !== undefined ? `（实际 ${JSON.stringify(actual)}）` : ''}`);
}

async function waitHello(c) {
  for (let i = 0; i < 50 && !c.hello; i++) await new Promise((r) => setTimeout(r, 100));
}

async function main() {
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-run-defaults-'));
  // 规范目录夹具：两个子目录 + 根 README + 一个泄漏文件（秘密扫描负例）。
  const specDir = join(dataDir, 'spec');
  mkdirSync(join(specDir, 'standards'), { recursive: true });
  mkdirSync(join(specDir, 'templates'), { recursive: true });
  writeFileSync(join(specDir, 'standards', 'side-effect-safety.md'), '# 写操作安全\n写工具必须显式声明并过确认闸门。\n');
  writeFileSync(join(specDir, 'standards', 'database-sql.md'), '# 数据库规范\n迁移必须可回滚。\n');
  writeFileSync(join(specDir, 'templates', 'adr.md'), '# ADR 模板\n背景/决策/后果。\n');
  writeFileSync(join(specDir, 'README.md'), '# 规范套件\n总说明。\n');
  writeFileSync(join(specDir, 'standards', 'leaked.md'), '# 泄漏\npassword = "supersecret123"\n');

  let c = new CoreClient(dataDir);
  await waitHello(c);

  // ① 目录导入：三态清单 + 失败隔离（清单记相对路径，技能名由其转连字符）。
  let out = await c.call('skill.importDirectory', { path: specDir });
  assert(out.imported.length === 4, '四个文件新建 draft 技能', out.imported);
  assert(out.failed.length === 1 && out.failed[0].file.endsWith('leaked.md'), '泄漏文件 fail-closed 且不阻断其余', out.failed);
  assert(out.imported.some((f) => f === 'standards/side-effect-safety.md'), '安全标准已入清单（相对路径）', out.imported);

  // 幂等重导：全部 skipped。
  out = await c.call('skill.importDirectory', { path: specDir });
  assert(out.imported.length === 0 && out.updated.length === 0 && out.skipped.length === 4, '同内容重导幂等（全 skipped）', out);

  // 子目录过滤。
  out = await c.call('skill.importDirectory', { path: specDir, subdirs: ['templates'] });
  assert(out.skipped.length === 1 && out.failed.length === 0, 'subdirs 过滤只命中 templates', out);

  // 非法输入负例。
  let rejected = false;
  try { await c.call('skill.importDirectory', { path: 'relative/path' }); } catch { rejected = true; }
  assert(rejected, '相对路径拒绝');
  rejected = false;
  try { await c.call('skill.importDirectory', { path: '/nonexistent-run-defaults' }); } catch { rejected = true; }
  assert(rejected, '不存在目录拒绝');

  // ② 激活两个规范技能 → activeList 带 id。
  const list = await c.call('skill.list', {});
  const safety = list.items.find((s) => s.name.includes('side-effect-safety'));
  const sql = list.items.find((s) => s.name.includes('database-sql'));
  assert(Boolean(safety && sql), '导入技能在册');
  const safetyVersions = await c.call('skill.versionList', { skillId: safety.id });
  assert(safetyVersions.items[0].status === 'draft', '导入版本为 draft（激活前不进注入面）', safetyVersions.items[0].status);
  await c.call('skill.activateVersion', { versionId: safetyVersions.items[0].id });
  const sqlVersions = await c.call('skill.versionList', { skillId: sql.id });
  await c.call('skill.activateVersion', { versionId: sqlVersions.items[0].id });
  const active = await c.call('skill.activeList', {});
  assert(active.items.length === 2 && active.items.every((i) => i.versionId), 'activeList 携带版本 id', active.items.map((i) => i.name));
  const safetyVersionId = active.items.find((i) => i.name.includes('side-effect-safety')).versionId;
  const sqlVersionId = active.items.find((i) => i.name.includes('database-sql')).versionId;

  // 项目与工作项。
  await c.call('project.create', { gitlabInstance: 'x', namespace: 'n', project: 'p', name: 'P' });
  const projects = await c.call('project.list', {});
  const projectId = projects.items[0].id;

  // ④ "@" 任务默认 Agent：create 冻结。
  const profiles = await c.call('agentProfile.list', {});
  const profile = profiles.items.find((p) => p.enabled && p.versions.length > 0);
  assert(Boolean(profile), '存在可用 Agent profile');
  const latestVersion = profile.versions.reduce((a, b) => (b.versionNo > a.versionNo ? b : a));
  const wi = await c.call('workitem.create', {
    projectId, title: '默认面任务', description: '验证任务级默认运行面',
    agentProfileVersionId: latestVersion.id, skillVersionIds: [safetyVersionId],
  });

  // ⑤ "/" 与 "@" 进 run：显式技能面 + 默认 Agent 选路。
  const started = await c.call('agent.start', {
    workItemId: wi.id, goal: '按规范执行：验证默认面注入', idempotencyKey: 'rd-explicit-1',
  });
  const runInfo = await c.call('agent.get', { runId: started.runId });
  assert(
    Array.isArray(runInfo.skills?.items) && runInfo.skills.items.some((i) => i.versionId === safetyVersionId),
    'run 冻结技能证据含任务默认技能', runInfo.skills,
  );
  assert(
    !runInfo.skills.items.some((i) => i.versionId === sqlVersionId),
    '显式/默认面替换全局 enabled 面（未选技能不注入）', runInfo.skills,
  );
  const selectionId = runInfo.agentSelectionId ?? null;
  assert(Boolean(selectionId), '默认 Agent 生成选路（agent_selection 冻结）', selectionId);

  // updateRunDefaults：调整技能面 + 清空 Agent。
  const updated = await c.call('workitem.updateRunDefaults', {
    workItemId: wi.id, skillVersionIds: [sqlVersionId], agentProfileVersionId: null,
  });
  assert(updated.skillVersionIds.length === 1 && updated.skillVersionIds[0] === sqlVersionId, '默认技能面切换为 sql 规范', updated);
  const started2 = await c.call('agent.start', {
    workItemId: wi.id, goal: '第二轮：验证默认面调整后生效', idempotencyKey: 'rd-explicit-2',
  });
  const run2 = await c.call('agent.get', { runId: started2.runId });
  assert(
    run2.skills.items.length === 1 && run2.skills.items[0].versionId === sqlVersionId,
    '第二轮 run 冻结新默认面', run2.skills,
  );

  // run 显式参数 > 任务默认面。
  const started3 = await c.call('agent.start', {
    workItemId: wi.id, goal: '第三轮：run 显式参数覆盖', skillVersionIds: [safetyVersionId],
    idempotencyKey: 'rd-explicit-3',
  });
  const run3 = await c.call('agent.get', { runId: started3.runId });
  assert(
    run3.skills.items.length === 1 && run3.skills.items[0].versionId === safetyVersionId,
    'run 显式技能参数压过任务默认面', run3.skills,
  );

  // revoked 负例：吊销后显式引用 fail-closed。
  await c.call('skill.revokeVersion', { versionId: safetyVersionId });
  let revokedRejected = false;
  try {
    await c.call('agent.start', {
      workItemId: wi.id, goal: 'revoked 负例', skillVersionIds: [safetyVersionId], idempotencyKey: 'rd-revoked-1',
    });
  } catch (e) {
    revokedRejected = String(e.message).includes('revoked');
  }
  assert(revokedRejected, 'revoked 版本注入 fail-closed');

  // 无面任务：全局 enabled 语义保持（agent_run_skills 空、skills.items 空）。
  const wiPlain = await c.call('workitem.create', { projectId, title: '无默认面任务', description: '' });
  const started4 = await c.call('agent.start', {
    workItemId: wiPlain.id, goal: '无默认面', idempotencyKey: 'rd-plain-1',
  });
  const run4 = await c.call('agent.get', { runId: started4.runId });
  assert(run4.skills.items.length === 0, '未选面 run 零冻结行（回退全局 enabled 注入）', run4.skills);

  c.kill();
  rmSync(dataDir, { recursive: true, force: true });
  console.log('任务级默认运行面 + run 显式技能面协议 E2E 通过。');
}

main().catch((e) => {
  console.error('FAILED:', e.message);
  process.exit(1);
});
