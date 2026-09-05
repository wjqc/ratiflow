// 场景 A（手册 §14）：首次启动——无项目/模型/GitLab/SSH。
// 概览按影响展示阻塞；可先添加本地项目浏览文档；不能启动 Agent Run 或部署。
import { expect, test } from '@playwright/test';
import { join } from 'node:path';
import { launchApp, type E2eApp } from './fixtures';

let e2e: E2eApp;

test.afterEach(async () => {
  if (e2e) { await e2e.close(); e2e = undefined as unknown as E2eApp; }
});

test('A0 设置替换项目侧栏，高级能力按需展开', async ({}, testInfo) => {
  e2e = await launchApp();
  const pageErrors: string[] = [];
  const consoleErrors: string[] = [];
  e2e.window.on('pageerror', (error) => pageErrors.push(error.message));
  e2e.window.on('console', (message) => {
    if (message.type() === 'error') consoleErrors.push(message.text());
  });

  await expect(e2e.window).toHaveTitle(/SixGates/);
  await expect(e2e.window.getByRole('complementary', { name: '项目导航' })).toBeVisible();
  await e2e.window.getByRole('button', { name: '设置' }).click();

  const settingsSidebar = e2e.window.getByRole('complementary', { name: '设置导航' });
  await expect(settingsSidebar).toBeVisible();
  await expect(e2e.window.getByRole('complementary', { name: '项目导航' })).toHaveCount(0);
  await expect(e2e.window.locator('aside')).toHaveCount(1);
  await expect(settingsSidebar.getByRole('button', { name: /^使用统计$/ })).toHaveCount(0);
  await expect(settingsSidebar.getByRole('button', { name: '外部集成' })).toHaveCount(0);
  await expect(settingsSidebar.getByRole('button', { name: '外观' })).toHaveCount(0);
  await expect(e2e.window.getByRole('heading', { name: '常规', exact: true })).toBeVisible();
  await expect(e2e.window.getByRole('heading', { name: '外观', exact: true })).toBeVisible();

  await settingsSidebar.getByRole('button', { name: '高级设置' }).click();
  await expect(settingsSidebar.getByRole('button', { name: /^使用统计$/ })).toBeVisible();
  await expect(settingsSidebar.getByRole('button', { name: '审计日志' })).toHaveCount(0);
  await expect(settingsSidebar.getByRole('button', { name: '日志与故障报告' })).toHaveCount(0);

  // 外部集成是普通页：GitLab / SSH 目标机为页内区块，不是独立菜单。
  await settingsSidebar.getByRole('button', { name: '外部集成' }).click();
  await expect(e2e.window.getByRole('heading', { name: '外部集成', exact: true })).toBeVisible();
  await expect(e2e.window.getByRole('heading', { name: 'GitLab', exact: true })).toBeVisible();
  await expect(e2e.window.getByRole('heading', { name: 'SSH 目标机', exact: true })).toBeVisible();
  await expect(settingsSidebar.getByRole('button', { name: 'GitLab' })).toHaveCount(0);

  await e2e.window.screenshot({ path: testInfo.outputPath('settings-single-sidebar.png') });

  await settingsSidebar.getByRole('button', { name: /^使用统计$/ }).click();
  await expect(e2e.window.getByRole('heading', { name: '使用统计', exact: true })).toBeVisible();
  await expect(e2e.window.getByRole('heading', { name: '模型 Token 用量' })).toBeVisible();
  await expect(e2e.window.getByRole('heading', { name: '本地运行' })).toHaveCount(0);
  await e2e.window.screenshot({ path: testInfo.outputPath('settings-runtime-observability.png') });

  await e2e.gotoSettings('integrations');
  await expect(e2e.window.getByRole('heading', { name: '外部集成', exact: true })).toBeVisible();
  await e2e.window.getByRole('button', { name: '新增实例' }).click();
  await expect(e2e.window.getByLabel('名称 *')).toBeVisible();
  await e2e.window.screenshot({ path: testInfo.outputPath('settings-gitlab.png') });

  await e2e.gotoSettings('knowledge-defaults');
  await expect(e2e.window.getByRole('heading', { name: '知识库默认策略' })).toBeVisible();
  await e2e.window.screenshot({ path: testInfo.outputPath('settings-knowledge-defaults.png') });

  await e2e.gotoSettings('projects');
  await expect(e2e.window.getByRole('heading', { name: '项目工作区' })).toBeVisible();
  await e2e.window.screenshot({ path: testInfo.outputPath('settings-projects.png') });

  await settingsSidebar.getByRole('button', { name: '返回工作区' }).click();
  await expect(e2e.window.getByRole('complementary', { name: '项目导航' })).toBeVisible();
  await expect(e2e.window.locator('vite-error-overlay')).toHaveCount(0);
  expect(pageErrors).toEqual([]);
  expect(consoleErrors).toEqual([]);
});

test('A2 可创建本地项目并浏览（不要求集成就绪）', async () => {
  e2e = await launchApp();
  const project = await e2e.rpc<{ id: string; name: string }>('project.create', {
    gitlabInstance: 'https://gitlab.example.com', namespace: 'e2e', project: 'local-only',
    name: 'E2E 本地项目', localRoot: join(process.cwd(), '..', '..'),
  });
  expect(project.id).toBeTruthy();

  const list = await e2e.rpc<{ items: Array<{ id: string; name: string }> }>('project.list', {});
  expect(list.items.some((p) => p.id === project.id)).toBe(true);

  // inspectRoot 可用（本地目录检测）
  const inspect = await e2e.rpc<{ isGitRepo: boolean; stacks: string[] }>('project.inspectRoot', { path: join(process.cwd(), '..', '..') });
  expect(inspect.isGitRepo).toBe(true);
  expect(inspect.stacks.length).toBeGreaterThan(0);
});

test('A3 未配模型时 Agent Run 被阻断（无假 Agent 回复）', async () => {
  e2e = await launchApp();
  // 建项目 + 工作项 + 上下文清单——Agent Run 前置全部可做
  const project = await e2e.rpc<{ id: string }>('project.create', {
    gitlabInstance: 'x', namespace: 'n', project: 'p', name: 'P',
  });
  const wi = await e2e.rpc<{ id: string }>('workitem.create', {
    projectId: project.id, title: 'E2E 任务', description: '场景 A',
  });
  const manifest = await e2e.rpc<{ id: string }>('context.create', {
    projectId: project.id, workItemId: wi.id, query: 'e2e', selectedSources: [],
  });
  expect(manifest.id).toBeTruthy();

  // M0-②（ADR-028）：agent.start 是唯一入口，立即返回 runId，Run 在后台任务执行。
  const started = await e2e.rpc<{ runId: string }>('agent.start', {
    workItemId: wi.id, goal: '测试', contextManifestId: manifest.id,
    toolAllowlist: ['read_file'], idempotencyKey: 'e2e-a3',
  });
  expect(started.runId).toBeTruthy();

  // 非阻塞证据：Run 进行中，其余 RPC 立即返回（读循环与 DB actor 不被占用）。
  const t0 = Date.now();
  await e2e.rpc('project.list');
  expect(Date.now() - t0).toBeLessThan(1000);

  // 未配模型 Profile → fake 脚本耗尽 → 终态 failed（model_unavailable），绝不伪造 completed_execution。
  let run = await e2e.rpc<{ status: string; result: string }>('agent.get', { runId: started.runId });
  for (let i = 0; i < 50 && !['completed_execution', 'failed', 'cancelled'].includes(run.status); i++) {
    await new Promise((resolve) => setTimeout(resolve, 100));
    run = await e2e.rpc<{ status: string; result: string }>('agent.get', { runId: started.runId });
  }
  expect(run.status).toBe('failed');
  expect(run.result).toContain('model_unavailable');
});
