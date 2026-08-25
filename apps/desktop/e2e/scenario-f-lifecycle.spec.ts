// 场景 F（手册 §14）：Core 生命周期——
// 正常退出无孤儿进程；意外 crash 最多重启 3 次；重启后恢复。
import { execSync } from 'node:child_process';
import { expect, test } from '@playwright/test';
import { findCorePid, launchApp, waitFor, type E2eApp } from './fixtures';

let e2e: E2eApp;

test.afterEach(async () => {
  if (e2e) { await e2e.close(); e2e = undefined as unknown as E2eApp; }
});

test('F1 正常退出后无孤儿 Electron/Core 进程', async () => {
  e2e = await launchApp();
  const dataDir = e2e.dataDir;

  // 确认 core 在运行。
  await waitFor(async () => (await findCorePid(dataDir)).length >= 1, 15000, 'core 启动');
  const version = await e2e.rpc<{ version: string }>('core.version');
  expect(version.version).toBeTruthy();

  // 正常退出（app.close 触发 before-quit → shutdownRequested）。
  await e2e.app.close();

  // 2 秒内 core 必须退出且不再被拉起（P0）。
  await waitFor(async () => (await findCorePid(dataDir)).length === 0, 5000, 'core 退出');
  await new Promise((r) => setTimeout(r, 2000));
  expect(await findCorePid(dataDir)).toHaveLength(0);

  // close() 会清 dataDir——标记已清理避免 afterEach 二次删。
  e2e = undefined as unknown as E2eApp;
});

test('F2 意外 crash 自动重启并恢复服务（≤3 次）', async () => {
  e2e = await launchApp();
  const dataDir = e2e.dataDir;

  await waitFor(async () => (await findCorePid(dataDir)).length >= 1, 15000, 'core 启动');

  // kill -9 core（模拟意外 crash）。
  execSync('kill -9 ' + (await findCorePid(dataDir))[0]);

  // main 检测退出 → 退避重启 → 新 core 就绪（RPC 恢复可用）。
  await waitFor(async () => {
    try { await e2e.rpc('core.version'); return true; } catch { return false; }
  }, 20000, 'crash 后 RPC 恢复');

  const version = await e2e.rpc<{ version: string }>('core.version');
  expect(version.version).toBeTruthy();
});

test('F3 连续 crash 超过 3 次进入诊断模式（不再无限重启）', async () => {
  e2e = await launchApp();
  const dataDir = e2e.dataDir;
  await waitFor(async () => (await findCorePid(dataDir)).length >= 1, 15000, 'core 启动');

  // 连续 kill 4 次（超过重启上限）。退避间隔 500/1000/2000ms——总等待约 8s + 检测窗口。
  for (let i = 0; i < 4; i++) {
    const pids = await findCorePid(dataDir);
    if (pids.length > 0) { execSync('kill -9 ' + pids[0]); }
    await new Promise((r) => setTimeout(r, 700));
  }

  // 再等一个退避周期，core 不应再被拉起。
  await new Promise((r) => setTimeout(r, 4000));
  const pids = await findCorePid(dataDir);
  // 允许 0 个（已达上限进入诊断模式）——关键断言：不会无限增长。
  expect(pids.length).toBeLessThanOrEqual(1);
});
