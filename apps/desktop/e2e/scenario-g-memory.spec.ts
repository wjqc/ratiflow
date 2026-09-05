// 场景 G（ADR-032 / 实施方案 §14.5）：项目记忆 S12 端到端。
// G1 开启→新建→搜索→打开→编辑；G2 Run 上下文记忆证据；G3 归档后不采用；
// G4 导入/导出+业务 ID reveal；G5 Secret 拒绝；G6 revision 冲突保留草稿；
// G7 候选沉淀（M4 接线后启用）；G8 purge 影响预览与墓碑；G9 1180×760/1440×900 无裁切+键盘+AX。
// 全程 console/pageerror = 0；真实 Electron + 真实 Core。
import { expect, test } from '@playwright/test';
import { writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launchApp, type E2eApp } from './fixtures';

let e2e: E2eApp;
let consoleErrors: string[] = [];

async function launchWithConsoleGuard(): Promise<void> {
  e2e = await launchApp();
  consoleErrors = [];
  e2e.window.on('console', (msg) => {
    if (msg.type() === 'error') consoleErrors.push(msg.text());
  });
  e2e.window.on('pageerror', (err) => consoleErrors.push(String(err)));
}

/** preload IPC 会重建 Error（自定义属性丢失）；Core 的稳定 token 在 detail/message 前缀。 */
function errorCode(e: unknown): string {
  return String((e as Error)?.message ?? '').split(':')[0];
}

function expectNoConsoleErrors(): void {
  expect(consoleErrors, `console/pageerror 应为 0，实际：${consoleErrors.join(' | ')}`).toHaveLength(0);
}

test.afterEach(async () => {
  if (e2e) {
    await e2e.close();
    e2e = undefined as unknown as E2eApp;
  }
});

async function createProject(): Promise<string> {
  const proj = await e2e.rpc<{ id: string }>('project.create', {
    gitlabInstance: 'x', namespace: 'n', project: 'g', name: '记忆 G 场景',
  });
  return proj.id;
}

test('G1 开启→新建→搜索→打开→编辑', async () => {
  await launchWithConsoleGuard();
  await createProject();
  await e2e.gotoSettings('memory');

  // 工作区记忆开关（项目级偏好，无全局门禁）。
  const sw = e2e.window.getByRole('switch', { name: /启用记忆注入/ });
  await expect(sw).toBeVisible({ timeout: 15000 });
  await expect(sw).toBeEnabled();
  await sw.click();
  await expect(sw).toBeChecked();

  // 新建记忆。
  await e2e.window.getByRole('button', { name: '更多记忆操作' }).click();
  await e2e.window.getByRole('menuitem', { name: '新建记忆' }).click();
  const form = e2e.window.getByRole('form', { name: '新建记忆' });
  await form.getByLabel('标题').fill('部署健康检查');
  await form.getByLabel('正文（Markdown）').fill('部署前必须检查健康端点与日志。');
  await form.getByRole('button', { name: /创建（直接生效）/ }).click();

  // 列表出现新行。
  const row = e2e.window.locator('#sg-memory-list-zone button', { hasText: '部署健康检查' });
  await expect(row).toBeVisible({ timeout: 10000 });

  // 搜索（250ms debounce）。
  await e2e.window.getByRole('searchbox', { name: '搜索记忆' }).fill('健康');
  await expect(row).toBeVisible();
  await e2e.window.getByRole('searchbox', { name: '搜索记忆' }).fill('不存在的词');
  await expect(e2e.window.getByText(/没有匹配/)).toBeVisible();
  await e2e.window.getByRole('button', { name: '清除筛选' }).click();

  // 打开 → 编辑 → 保存为新版本。
  await row.click();
  const dialog = e2e.window.getByRole('dialog');
  await expect(dialog.getByText(/部署前必须检查健康端点/)).toBeVisible();
  await dialog.getByRole('button', { name: '编辑' }).click();
  const editForm = dialog.getByRole('form', { name: '编辑记忆' });
  await editForm.getByLabel('正文（Markdown）').fill('部署前必须检查健康端点、日志与回滚快照。');
  await editForm.getByRole('button', { name: '保存' }).click();
  await expect(dialog.getByText('已保存为新版本')).toBeVisible();
  await expect(dialog.getByText(/回滚快照/)).toBeVisible();

  // Esc 关闭。
  await e2e.window.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
  expectNoConsoleErrors();
});

test('G2/G3 Run 上下文记忆证据 + 归档后不再采用', async () => {
  await launchWithConsoleGuard();
  const projectId = await createProject();
  await e2e.rpc('memory.settingsUpdate', {
    projectId, settings: { enabled: true }, expectedRevision: 1, idempotencyKey: 'g2-enable',
  });
  const created = await e2e.rpc<{ memoryId: string }>('memory.create', {
    projectId, title: 'G2 记忆', kind: 'fact', body: '部署前检查快照。',
    idempotencyKey: 'g2-create',
  });
  const wi = await e2e.rpc<{ id: string }>('workitem.create', { projectId, title: 'G2 工作项', description: '' });

  const run1 = await e2e.rpc<{ runId: string }>('agent.start', {
    workItemId: wi.id, goal: '部署 检查', toolAllowlist: ['read_file'],
  });
  const g1 = await e2e.rpc<{ memory: { count: number; bytes: number; items: Array<{ memoryId: string; revisionId: string; bytes: number; revisionNo: number }> } }>(
    'agent.get', { runId: run1.runId });
  expect(g1.memory.count).toBe(1);
  expect(g1.memory.items[0].memoryId).toBe(created.memoryId);
  expect(g1.memory.items[0].revisionNo).toBe(1);
  expect(g1.memory.bytes).toBeGreaterThan(0);

  // 归档后新 Run 不再采用；旧 Run 证据保持冻结。
  const detail = await e2e.rpc<{ revision: number }>('memory.get', { projectId, memoryId: created.memoryId });
  await e2e.rpc('memory.archive', { projectId, memoryId: created.memoryId, expectedRevision: detail.revision, idempotencyKey: 'g3-archive' });
  const run2 = await e2e.rpc<{ runId: string }>('agent.start', {
    workItemId: wi.id, goal: '部署 检查', toolAllowlist: ['read_file'],
  });
  const g2 = await e2e.rpc<{ memory: { count: number } }>('agent.get', { runId: run2.runId });
  expect(g2.memory.count).toBe(0);
  const g1b = await e2e.rpc<{ memory: { count: number } }>('agent.get', { runId: run1.runId });
  expect(g1b.memory.count).toBe(1);
  expectNoConsoleErrors();
});

test('G4 导入/导出并经业务 ID reveal（非法 ID 拒绝）', async () => {
  await launchWithConsoleGuard();
  const projectId = await createProject();
  const md = '# G4 导入条目\n\n由 E2E 导入的正文。\n';
  const imported = await e2e.rpc<{ created: unknown[] }>('memory.import', {
    projectId,
    filename: 'g4.md',
    contentBase64: Buffer.from(md).toString('base64'),
    mode: 'active',
    idempotencyKey: 'g4-import',
  });
  expect(imported.created).toHaveLength(1);
  const exported = await e2e.rpc<{ exportId: string; count: number }>('memory.export', {
    projectId, includeArchived: false, format: 'markdown',
  });
  expect(exported.exportId).toMatch(/^memexp_[0-9a-f]{24}$/);
  expect(exported.count).toBeGreaterThanOrEqual(1);

  // 窄 IPC：合法 ID → true；未知/非法 ID → false（不暴露路径）。
  const ok = await e2e.window.evaluate(
    (id: string) => (window as unknown as { sixgates: { revealMemoryExport(id: string): Promise<boolean> } }).sixgates.revealMemoryExport(id),
    exported.exportId,
  );
  expect(ok).toBe(true);
  const bad = await e2e.window.evaluate(
    () => (window as unknown as { sixgates: { revealMemoryExport(id: string): Promise<boolean> } }).sixgates.revealMemoryExport('../../etc/passwd'),
  );
  expect(bad).toBe(false);
  const unknown = await e2e.window.evaluate(
    () => (window as unknown as { sixgates: { revealMemoryExport(id: string): Promise<boolean> } }).sixgates.revealMemoryExport('memexp_ffffffffffffffffffffffff'),
  );
  expect(unknown).toBe(false);
  expectNoConsoleErrors();
});

test('G5 Secret 拒绝且无副作用', async () => {
  await launchWithConsoleGuard();
  const projectId = await createProject();
  const errMsg = await e2e.window.evaluate(async (params: Record<string, unknown>) => {
    try {
      await (window as unknown as { sixgates: { rpc(m: string, p: Record<string, unknown>): Promise<unknown> } })
        .sixgates.rpc('memory.create', params);
      return null;
    } catch (e) {
      return String((e as Error)?.message ?? e);
    }
  }, { projectId, title: '含密', kind: 'fact', body: 'password = "supersecret123"', idempotencyKey: 'g5-1' });
  // IPC 包装（Error invoking remote method …）不影响稳定 token 的识别。
  expect(errMsg?.match(/memory_[a-z_]+/)?.[0]).toBe('memory_secret_detected');
  const list = await e2e.rpc<{ items: unknown[] }>('memory.list', { projectId });
  expect(list.items).toHaveLength(0);
  expectNoConsoleErrors();
});

test('G6 revision 冲突保留草稿', async () => {
  await launchWithConsoleGuard();
  const projectId = await createProject();
  const created = await e2e.rpc<{ memoryId: string }>('memory.create', {
    projectId, title: 'G6 冲突条目', kind: 'fact', body: '初版内容。', idempotencyKey: 'g6-1',
  });
  await e2e.gotoSettings('memory');
  const row = e2e.window.locator('#sg-memory-list-zone button', { hasText: 'G6 冲突条目' });
  await expect(row).toBeVisible({ timeout: 10000 });
  await row.click();
  const dialog = e2e.window.getByRole('dialog');
  await dialog.getByRole('button', { name: '编辑' }).click();
  const editForm = dialog.getByRole('form', { name: '编辑记忆' });
  await editForm.getByLabel('标题').fill('G6 本地草稿标题');

  // 幕后拨动 revision（模拟其他会话保存）。
  const detail = await e2e.rpc<{ revision: number }>('memory.get', { projectId, memoryId: created.memoryId });
  await e2e.rpc('memory.pin', {
    projectId, memoryId: created.memoryId, pinned: true,
    expectedRevision: detail.revision, idempotencyKey: 'g6-pin',
  });

  await editForm.getByRole('button', { name: '保存' }).click();
  // 冲突横幅出现，本地草稿仍在输入框中。
  await expect(dialog.getByText(/已被其他会话修改/)).toBeVisible();
  await expect(editForm.getByLabel('标题')).toHaveValue('G6 本地草稿标题');
  await dialog.getByRole('button', { name: '重新加载服务器版本' }).click();
  await expect(dialog.getByRole('heading', { name: 'G6 冲突条目' })).toBeVisible();
  expectNoConsoleErrors();
});

test('G8 purge 影响预览与墓碑', async () => {
  await launchWithConsoleGuard();
  const projectId = await createProject();
  await e2e.rpc<{ memoryId: string }>('memory.create', {
    projectId, title: 'G8 待清除', kind: 'fact', body: '即将清除的正文。', idempotencyKey: 'g8-1',
  });
  await e2e.gotoSettings('memory');
  const row = e2e.window.locator('#sg-memory-list-zone button', { hasText: 'G8 待清除' });
  await expect(row).toBeVisible({ timeout: 10000 });
  await row.click();
  const dialog = e2e.window.getByRole('dialog');
  await dialog.getByRole('button', { name: '清除内容…' }).click();
  // 影响预览：备份残留边界必须如实声明。
  await expect(dialog.getByText(/可能残留于/)).toBeVisible();
  await expect(dialog.getByText(/Context Manifest/)).toBeVisible();
  await dialog.getByRole('button', { name: /确认清除（两步确认）/ }).click();
  await expect(dialog.getByText(/正文已清除/).first()).toBeVisible();
  // 墓碑：正文位置显示已清除，状态为已清除。
  await expect(dialog.getByText('正文已清除', { exact: true })).toBeVisible();
  await expect(dialog.getByText('已清除').first()).toBeVisible();
  // 默认列表不再显示 purged 行。
  await e2e.window.keyboard.press('Escape');
  await expect(row).toHaveCount(0);
  expectNoConsoleErrors();
});

test('G9 1180×760/1440×900 × light/dark 无裁切 + 键盘路径 + AX 名称', async () => {
  await launchWithConsoleGuard();
  const projectId = await createProject();
  await e2e.rpc<{ memoryId: string }>('memory.create', {
    projectId, title: 'G9 布局条目', kind: 'lesson', body: '布局验证正文。', idempotencyKey: 'g9-1',
  });
  // 暗色主题（M5）：data-theme 由 AppShell 依据 app.appearance 应用；此处直接切 token 做视觉取证。
  for (const theme of ['light', 'dark']) {
    for (const viewport of [
      { width: 1180, height: 760 },
      { width: 1440, height: 900 },
    ]) {
      await e2e.window.setViewportSize(viewport);
      await e2e.gotoSettings('memory');
      await e2e.window.evaluate((t: string) => {
        document.documentElement.dataset.theme = t;
      }, theme);
      const overflow = await e2e.window.evaluate(
        () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
      );
      expect(overflow, `${theme} ${viewport.width}×${viewport.height} 不应出现横向滚动`).toBeLessThanOrEqual(0);
      await expect(e2e.window.getByRole('searchbox', { name: '搜索记忆' })).toBeVisible();
      await expect(e2e.window.getByRole('button', { name: '更多记忆操作' })).toBeVisible();
      const row = e2e.window.locator('#sg-memory-list-zone button', { hasText: 'G9 布局条目' });
      await expect(row).toBeVisible({ timeout: 10000 });
      await e2e.window.screenshot({
        path: `test-results/scenario-g-${theme}-${viewport.width}x${viewport.height}.png`,
        fullPage: false,
      });
      // 键盘路径与关闭按钮 AX 名称（每个主题组合验证一次即可，最后一组执行）。
      if (theme === 'dark' && viewport.width === 1180) {
        await row.focus();
        await e2e.window.keyboard.press('Enter');
        const dialog = e2e.window.getByRole('dialog');
        await expect(dialog.getByRole('button', { name: '关闭记忆详情' })).toBeVisible();
        await e2e.window.keyboard.press('Escape');
        await expect(dialog).toHaveCount(0);
      }
    }
  }
  // AX 快照：开关/搜索框/主操作按钮的 可访问名称 均不得为空（§3.7）。
  const ax = await e2e.window.accessibility.snapshot();
  const names: string[] = [];
  const walk = (node: { role?: string; name?: string; children?: unknown[] }): void => {
    if (['switch', 'searchbox', 'textbox', 'button'].includes(node.role ?? '') && !(node.name ?? '').trim()) {
      names.push(`${node.role}:（空名称）`);
    }
    for (const child of node.children ?? []) walk(child as typeof node);
  };
  if (ax) walk(ax);
  expect(names, `AX 空名称节点：${names.join(', ')}`).toHaveLength(0);
  expectNoConsoleErrors();
});

test('G7 候选沉淀：capture → 待确认区 → 接受（可编辑）激活', async () => {
  // 脚本模型：第 1 项驱动 Run 终态，第 2 项为候选抽取 JSON。
  const scriptPath = join(tmpdir(), `sg-g7-script-${Date.now()}.json`);
  writeFileSync(scriptPath, JSON.stringify([
    { content: '{"action":"final","summary":"G7 run 完成"}', tokensIn: 5, tokensOut: 5 },
    {
      content: JSON.stringify({
        title: '部署检查清单',
        kind: 'convention',
        summary: '部署前检查健康端点。',
        body: '部署流水线必须在发布前探测健康端点，失败即中止。',
      }),
      tokensIn: 30,
      tokensOut: 40,
    },
  ]));
  await launchApp({ env: { SIXGATES_FAKE_MODEL_SCRIPT: scriptPath } }).then((app) => {
    e2e = app;
  });
  consoleErrors = [];
  e2e.window.on('console', (msg) => {
    if (msg.type() === 'error') consoleErrors.push(msg.text());
  });
  e2e.window.on('pageerror', (err) => consoleErrors.push(String(err)));

  const projectId = await createProject();
  await e2e.rpc('memory.settingsUpdate', {
    projectId,
    settings: { enabled: true, captureMode: 'suggest' },
    expectedRevision: 1,
    idempotencyKey: 'g7-s1',
  });
  const wi = await e2e.rpc<{ id: string }>('workitem.create', { projectId, title: 'G7 工作项', description: '' });
  const run = await e2e.rpc<{ runId: string }>('agent.start', {
    workItemId: wi.id, goal: '部署 检查', toolAllowlist: ['read_file'],
  });
  for (let i = 0; i < 80; i++) {
    const r = await e2e.rpc<{ status: string }>('agent.get', { runId: run.runId });
    if (r.status === 'completed_execution') break;
    await new Promise((res) => setTimeout(res, 100));
  }
  const cap = await e2e.rpc<{ jobId: string }>('memory.captureStart', {
    projectId, runId: run.runId, idempotencyKey: 'g7-cap',
  });
  let state: { job: { status: string }; candidates: Array<{ candidateId: string }> } | null = null;
  for (let i = 0; i < 100; i++) {
    state = await e2e.rpc('memory.captureGet', { projectId, jobId: cap.jobId });
    if (['succeeded', 'failed', 'unknown'].includes(state.job.status)) break;
    await new Promise((res) => setTimeout(res, 100));
  }
  expect(state?.job.status).toBe('succeeded');
  expect(state?.candidates).toHaveLength(1);

  // UI：待确认区展示候选 → 接受并编辑 → 正式记忆出现在列表。
  await e2e.gotoSettings('memory');
  const candZone = e2e.window.getByLabel('待确认候选');
  await expect(candZone.getByText('部署检查清单')).toBeVisible({ timeout: 15000 });
  await candZone.getByRole('button', { name: '接受并编辑…' }).click();
  const dialog = e2e.window.getByRole('dialog');
  await expect(dialog.getByText(/接受后写入正式已确认记忆/)).toBeVisible();
  const editForm = dialog.getByRole('form', { name: '新建记忆' });
  await expect(editForm.getByLabel('标题')).toHaveValue('部署检查清单');
  await editForm.getByLabel('正文（Markdown）').fill('修订：发布前必须探测健康端点并记录结果。');
  await editForm.getByRole('button', { name: /接受（写入已确认记忆）/ }).click();
  const newRow = e2e.window.locator('#sg-memory-list-zone button', { hasText: '部署检查清单' });
  await expect(newRow).toBeVisible({ timeout: 10000 });
  // 待确认区清空。
  await expect(candZone).toHaveCount(0);
  expectNoConsoleErrors();
});
