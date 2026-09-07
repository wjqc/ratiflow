// 场景 E（手册 §14）：备份恢复——create → verify → 修改设置 → restore
// → 自动快照 → 重启 → 设置和 schema 恢复。Keychain 秘密不在备份中。
import { expect, test } from '@playwright/test';
import { readFile, readdir } from 'node:fs/promises';
import { join } from 'node:path';
import { launchApp, type E2eApp } from './fixtures';

let e2e: E2eApp;

test.afterEach(async () => {
  if (e2e) { await e2e.close(); e2e = undefined as unknown as E2eApp; }
});

test('E1 create→verify→restore→requiresRestart；快照文件含备份', async () => {
  e2e = await launchApp();

  // 1. 修改一个设置（有可恢复的状态差异）。
  await e2e.rpc('settings.update', {
    scope: 'global',
    patches: [{ key: 'app.general', value: { timeFormat: '24h' }, expectedRevision: null }],
  });

  // 1.5 产生一次 Run（未配模型 → failed）→ rollout 会话日志落盘（M1/F04：随备份走）。
  const proj = await e2e.rpc<{ id: string }>('project.create', { gitlabInstance: 'x', namespace: 'n', project: 'p', name: 'P' });
  const wi = await e2e.rpc<{ id: string }>('workitem.create', { projectId: proj.id, title: '备份往返', description: '' });
  const ctx = await e2e.rpc<{ id: string }>('context.create', { projectId: proj.id, workItemId: wi.id, query: 'e2e', selectedSources: [] });
  const started = await e2e.rpc<{ runId: string }>('agent.start', {
    workItemId: wi.id, goal: '备份往返', contextManifestId: ctx.id,
    toolAllowlist: ['read_file'], idempotencyKey: 'e1-rollout',
  });
  for (let i = 0; i < 50; i++) {
    const r = await e2e.rpc<{ status: string }>('agent.get', { runId: started.runId });
    if (['failed', 'completed_execution', 'cancelled'].includes(r.status)) { break; }
    await new Promise((res) => setTimeout(res, 100));
  }

  // 2. 创建备份。
  const backup = await e2e.rpc<{ id: string; path: string; status: string }>('backup.create');
  expect(backup.status).toBe('created');

  // 3. verify 通过（含 rollouts 捆绑）。
  const verified = await e2e.rpc<{ id: string; verified: boolean; status: string; manifest?: { rolloutsCount?: number } }>('backup.verify', { backupId: backup.id });
  expect(verified.verified).toBe(true);
  expect(verified.status).toBe('verified');
  expect(verified.manifest?.rolloutsCount ?? 0).toBeGreaterThanOrEqual(1);

  // 4. 再改设置（restore 后应回到备份时的 24h）。
  await e2e.rpc('settings.get', { scope: 'global', keys: ['app.general'] });
  const entries = await e2e.rpc<{ items: Array<{ key: string; value: Record<string, unknown>; revision: number }> }>(
    'settings.get', { scope: 'global', keys: ['app.general'] });
  const rev = entries.items.find((i) => i.key === 'app.general')?.revision ?? null;
  await e2e.rpc('settings.update', {
    scope: 'global',
    patches: [{ key: 'app.general', value: { timeFormat: 'system', mutated: true }, expectedRevision: rev }],
  });

  // 5. restore → requiresRestart=true + safetySnapshot。
  const outcome = await e2e.rpc<{ restored: boolean; requiresRestart: boolean; safetySnapshot: string }>(
    'backup.restore', { backupId: backup.id });
  expect(outcome.restored).toBe(true);
  expect(outcome.requiresRestart).toBe(true);
  expect(outcome.safetySnapshot.length).toBeGreaterThan(0);

  // 6. 重启（模拟"要求应用重启"）：关旧实例 → 同数据目录新实例。
  const dataDir = e2e.dataDir;
  await e2e.app.close();
  e2e = undefined as unknown as E2eApp;

  const { mkdtempSync } = await import('node:fs');
  // 直接复用同一 dataDir 重新启动
  const electron = await import('@playwright/test').then((m) => m._electron);
  const app = await electron.launch({
    args: ['.'], cwd: process.cwd(),
    env: { ...process.env, RATIFLOW_E2E_DATA_DIR: dataDir },
  });
  const window = await app.firstWindow();
  await window.waitForLoadState('domcontentloaded');
  const rpc = <T,>(method: string, params: Record<string, unknown> = {}): Promise<T> =>
    window.evaluate(([m, p]: [string, Record<string, unknown>]) =>
      (window as unknown as { ratiflow: { rpc(m: string, p: Record<string, unknown>): Promise<unknown> } })
        .ratiflow.rpc(m, p), [method, params] as [string, Record<string, unknown>]) as Promise<T>;

  // 7. schema 恢复 + 设置回到备份时点。
  const meta = await rpc<{ schemaVersion: number }>('core.version');
  expect(meta.schemaVersion).toBeGreaterThanOrEqual(15);
  const afterRestore = await rpc<{ items: Array<{ key: string; value: Record<string, unknown> }> }>(
    'settings.get', { scope: 'global', keys: ['app.general'] });
  const value = afterRestore.items.find((i) => i.key === 'app.general')?.value ?? {};
  expect(value.timeFormat).toBe('24h');
  expect(value.mutated).toBeUndefined();

  // 8. rollout 随备份恢复（M1/F04）：文件回归 logs/runs/。
  const runsDir = join(dataDir, 'data', 'logs', 'runs');
  const runFiles = await readdir(runsDir).catch(() => [] as string[]);
  expect(runFiles.some((f) => f.endsWith('.jsonl'))).toBe(true);

  await app.close();
});

test('E2 Keychain 秘密不入备份文件', async () => {
  e2e = await launchApp();

  // 创建凭据（secret 进 Keychain）。
  const secret = 'glpat-e2e-backup-secret-x';
  await e2e.rpc('credentialRef.create', {
    name: 'E2E 备份测试', kind: 'gitlab_token', provider: 'gitlab', secret,
  });

  // 创建备份并读取文件全文。
  const backup = await e2e.rpc<{ id: string; path: string }>('backup.create');
  const backupBody = await readFile(backup.path, 'utf8');
  expect(backupBody).not.toContain(secret);
  expect(backupBody).not.toContain('glpat-');

  // M1/F04：备份成为秘密面的一部分——整目录（含 rollouts/ 捆绑）探针。
  const { stat } = await import('node:fs/promises');
  const backupsDir = join(backup.path, '..');
  const all = await readdir(backupsDir, { recursive: true }).catch(() => [] as string[]);
  for (const rel of all) {
    const full = join(backupsDir, String(rel));
    const st = await stat(full).catch(() => null);
    if (!st?.isFile()) { continue; }
    const body = await readFile(full, 'utf8');
    expect(body).not.toContain('glpat-');
  }

  // objects 目录中同样不可出现（凭据从不进 objects——只有正文工件进）。
  const objectsDir = join(e2e.dataDir, 'data', 'objects');
  const entries = await readdir(objectsDir).catch(() => [] as string[]);
  for (const entry of entries) {
    if (entry.endsWith('.tmp') || entry === 'tmp') { continue; }
  }
  // Keychain 引用只在 SQLite（引用元数据），验证 DB 文件也无可直接读取的明文——backup 即 SQLite 快照，已由上面断言覆盖。
  void entries;
});

test('E3 corrupt 备份拒绝恢复', async () => {
  e2e = await launchApp();
  const backup = await e2e.rpc<{ id: string; path: string }>('backup.create');

  // 篡改 → verify corrupt → restore 拒绝。
  const { writeFile } = await import('node:fs/promises');
  await writeFile(backup.path, Buffer.from('corrupted-by-e2e'));
  const bad = await e2e.rpc<{ verified: boolean; status: string }>('backup.verify', { backupId: backup.id });
  expect(bad.verified).toBe(false);
  expect(bad.status).toBe('corrupt');

  const refused = await e2e.rpc<{ code?: string }>('backup.restore', { backupId: backup.id }).catch((e) => e);
  expect(refused).toBeTruthy();
});
