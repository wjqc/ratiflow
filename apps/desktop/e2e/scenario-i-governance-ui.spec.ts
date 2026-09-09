import { expect, test } from '@playwright/test';
import { launchApp, type E2eApp } from './fixtures';

let e2e: E2eApp;

test.afterEach(async () => {
  if (e2e) await e2e.close();
});

test('I1 自动化与任务治理入口在真实 Electron 中可达', async () => {
  e2e = await launchApp({ env: {
    RATIFLOW_AUTOMATIONS: '1',
    RATIFLOW_PLAN_DAG: '1',
    RATIFLOW_REWORK: '1',
    RATIFLOW_GATE_SKIP: '1',
  } });
  const errors: string[] = [];
  e2e.window.on('pageerror', (error) => errors.push(error.message));

  await e2e.gotoSettings('automations');
  await expect(e2e.window.getByRole('heading', { name: '自动化', exact: true })).toBeVisible();
  await e2e.window.getByLabel('规则标识').fill('e2e-governance');
  await e2e.window.getByRole('button', { name: '创建规则' }).click();
  await expect(e2e.window.getByText(/e2e-governance · active/)).toBeVisible();

  const project = await e2e.rpc<{ id: string }>('project.create', {
    gitlabInstance: 'local', namespace: 'e2e', project: 'governance', name: '治理项目',
  });
  await e2e.rpc('workitem.create', { projectId: project.id, title: '治理任务', description: '验证 UI 入口' });
  await e2e.window.evaluate(() => window.dispatchEvent(new CustomEvent('sg:projects-changed')));
  await e2e.window.getByRole('button', { name: '返回工作区' }).click();
  await expect(e2e.window.getByLabel('项目导航')).toBeVisible();
  await expect(e2e.window.getByRole('cell', { name: '治理任务' })).toBeVisible();
  await e2e.window.getByRole('button', { name: '继续闯关' }).click();
  await e2e.window.waitForTimeout(500);
  if (await e2e.window.getByTestId('task-governance-panel').count() === 0) {
    throw new Error(`任务页未显示治理面板：${(await e2e.window.locator('body').innerText()).slice(0, 2000)}`);
  }
  await expect(e2e.window.getByTestId('task-governance-panel')).toBeVisible();
  await e2e.window.getByRole('button', { name: '展开', exact: true }).click();
  await expect(e2e.window.getByRole('region', { name: '关卡治理' })).toBeVisible();
  await expect(e2e.window.getByText('追溯（需求 → 产物 → 证据）')).toBeVisible();
  expect(errors).toEqual([]);
});
