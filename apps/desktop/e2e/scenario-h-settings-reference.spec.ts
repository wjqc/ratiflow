// 参考图回归：项目记忆文件列表、MCP 分类列表、独立表单与 JSON 编辑状态。
import { expect, test } from '@playwright/test';
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { launchApp, type E2eApp } from './fixtures';

let e2e: E2eApp;

test.afterEach(async () => {
  if (e2e) {
    await e2e.close();
    e2e = undefined as unknown as E2eApp;
  }
});

test('H1 记忆与 MCP 页面匹配参考布局并保持交互可用', async ({}, testInfo) => {
  e2e = await launchApp();
  const errors: string[] = [];
  e2e.window.on('pageerror', (error) => errors.push(error.message));
  e2e.window.on('console', (message) => {
    if (message.type() === 'error') errors.push(message.text());
  });

  await e2e.window.setViewportSize({ width: 1920, height: 1050 });

  const project = await e2e.rpc<{ id: string }>('project.create', {
    gitlabInstance: 'local', namespace: 'e2e', project: 'reference', name: 'sixgates',
  });
  const memorySettings = await e2e.rpc<{ revision: number }>('memory.settingsGet', { projectId: project.id });
  await e2e.rpc('memory.settingsUpdate', {
    projectId: project.id,
    settings: { enabled: true },
    expectedRevision: memorySettings.revision,
    idempotencyKey: 'h1-memory-enable',
  });
  const memories = [
    ['github-ssh-push', 'GitHub SSH 推送使用独立凭据。'],
    ['github-upload-scope', '上传前确认仓库作用域。'],
    ['go-proxy-china', '国内环境使用可用代理。'],
    ['sixgates-design-review-bar', '设计走查必须保留截图证据。'],
    ['sixgates-electron-gotchas', 'Electron 退出后检查孤儿进程。'],
    ['sixgates-project-status', '项目状态以当前代码为准。'],
  ];
  for (const [title, body] of memories) {
    await e2e.rpc('memory.create', {
      projectId: project.id,
      title,
      kind: 'lesson',
      body,
      idempotencyKey: `h1-memory-${title}`,
    });
  }

  await e2e.gotoSettings('memory');
  await expect(e2e.window.getByRole('heading', { name: '记忆', exact: true })).toBeVisible();
  await expect(e2e.window.getByLabel('项目记忆列表').getByRole('listitem')).toHaveCount(memories.length);
  await e2e.window.screenshot({ path: testInfo.outputPath('reference-memory.png') });

  const serverScript = join(e2e.dataDir, 'fake-mcp.py');
  writeFileSync(serverScript, `import json, sys\nfor line in sys.stdin:\n req=json.loads(line)\n method=req.get("method")\n ident=req.get("id")\n if method=="initialize":\n  print(json.dumps({"jsonrpc":"2.0","id":ident,"result":{"serverInfo":{"name":"reference","version":"1"},"protocolVersion":"2024-11-05"}}), flush=True)\n elif method=="tools/list":\n  print(json.dumps({"jsonrpc":"2.0","id":ident,"result":{"tools":[{"name":"read_reference","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":True}}]}}), flush=True)\n`);
  const active = await e2e.rpc<{ serverId: string }>('mcp.serverAdd', {
    name: 'codegraph', command: 'python3', args: [serverScript],
  });
  await e2e.rpc('mcp.serverApprove', { serverId: active.serverId, decidedBy: 'local' });
  await e2e.rpc('mcp.serverAdd', { name: 'figma', command: 'python3', args: [serverScript] });

  await e2e.gotoSettings('mcp');
  await expect(e2e.window.getByRole('heading', { name: 'MCP 服务器', exact: true })).toBeVisible();
  await expect(e2e.window.getByText('codegraph')).toBeVisible();
  await expect(e2e.window.getByText('figma')).toBeVisible();
  await e2e.window.screenshot({ path: testInfo.outputPath('reference-mcp-list.png') });

  await e2e.window.getByRole('button', { name: '新建' }).click();
  await expect(e2e.window.getByRole('heading', { name: '新建 MCP 服务器' })).toBeVisible();
  await e2e.window.screenshot({ path: testInfo.outputPath('reference-mcp-form.png') });
  await e2e.window.getByRole('tab', { name: 'JSON' }).click();
  await expect(e2e.window.getByLabel('完整配置')).toBeVisible();
  await e2e.window.screenshot({ path: testInfo.outputPath('reference-mcp-json.png') });

  expect(errors).toEqual([]);
});
