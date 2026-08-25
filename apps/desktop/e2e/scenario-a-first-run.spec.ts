// 场景 A（手册 §14）：首次启动——无项目/模型/GitLab/SSH。
// 概览按影响展示阻塞；可先添加本地项目浏览文档；不能启动 Agent Run 或部署。
import { expect, test } from '@playwright/test';
import { join } from 'node:path';
import { launchApp, type E2eApp } from './fixtures';

let e2e: E2eApp;

test.afterEach(async () => {
  if (e2e) { await e2e.close(); e2e = undefined as unknown as E2eApp; }
});

test('A1 概览按影响展示阻塞（模型/GitLab 阻断 Agent Run）', async () => {
  e2e = await launchApp();
  // 直接 RPC（不经 UI）

  // 等待 settings.summary 数据加载
  await e2e.window.waitForSelector('text=模型', { timeout: 15000 });

  const summary = await e2e.rpc<{ overallStatus: string; blockers: Array<{ id: string; capabilities: string[] }> }>('settings.summary');
  expect(summary.overallStatus).toBe('action_required');
  expect(summary.blockers.some((b) => b.id === 'model_not_configured' && b.capabilities.includes('agent_run'))).toBe(true);
  expect(summary.blockers.some((b) => b.id === 'gitlab_not_configured')).toBe(true);
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

  // Agent Run：模型 Profile 不存在 → 稳定失败（不是假回复）
  // 未配模型 Profile → fake 脚本耗尽 → RPC 稳定失败（model_unavailable）。
  // 关键验收：绝不返回 completed_execution 带伪造内容。
  const result = await e2e.rpc<{ run?: { status: string } }>('agent.run', {
    workItemId: wi.id, goal: '测试', contextManifestId: manifest.id,
    toolAllowlist: ['read_file'], idempotencyKey: 'e2e-a3',
  }).catch((e: Error) => {
    expect(e.message).toContain('model_unavailable');
    return null;
  });
  if (result?.run?.status === 'completed_execution') {
    throw new Error('未配模型时不得返回 completed_execution');
  }
});
