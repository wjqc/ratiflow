// 新接线的 9 个设置页：mock bridge 下渲染不崩溃 + 空/加载状态展示。
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import '@testing-library/jest-dom/vitest';
import type React from 'react';
import { GeneralPage } from './GeneralPage';
import { AppearancePage } from './AppearancePage';
import { KnowledgeDefaultsPage } from './KnowledgeDefaultsPage';
import { ModelsPage } from './ModelsPage';
import { ToolsPage } from './ToolsPage';
import { ExecutionPage } from './ExecutionPage';
import { GitlabPage } from './GitlabPage';
import { SshPage } from './SshPage';
import { CredentialsPage } from './CredentialsPage';
import { MemoryPage } from './MemoryPage';

const rpcMock = vi.fn();
function ok(body: unknown) { return Promise.resolve(body); }

describe('设置页接线', () => {
  beforeEach(() => {
    cleanup();
    rpcMock.mockReset();
    rpcMock.mockImplementation((method: string) => {
      switch (method) {
        case 'settings.get': return ok({ items: [] });
        case 'knowledge.settings.get': return ok({ revision: 0 });
        case 'modelProfile.list': case 'gitlabProfile.list': case 'sshTarget.list':
        case 'credentialRef.list': case 'tool.list': case 'executionProfile.list':
        case 'modelProvider.presets':
          return ok({ items: [] });
        case 'executor.settings.get': return ok({ revision: 0 });
        case 'project.list': return ok({ items: [{ id: 'prj_1', name: '示例项目', archivedAt: null }] });
        case 'memory.settingsGet':
          return ok({
            projectId: 'prj_1', featureEnabled: true, enabled: false, captureMode: 'off',
            maxEntries: 8, maxBytes: 12288, staleAfterDays: 180, revision: 1,
            updatedAt: '2026-09-04T09:30:00.000Z', updatedBy: 'local',
          });
        case 'memory.list':
          return ok({ projectId: 'prj_1', items: [], counts: { active: 0 }, cursor: null });
        default: return ok({});
      }
    });
    (window as unknown as { sixgates: unknown }).sixgates = {
      rpc: (m: string, p: Record<string, unknown>) => rpcMock(m, p),
      hello: () => Promise.resolve({ ok: true }),
      selectFile: () => Promise.resolve(null),
      openExternal: () => Promise.resolve(),
    };
  });
  afterEach(() => { delete (window as unknown as { sixgates?: unknown }).sixgates; });

  it.each([
    ['常规', GeneralPage, '此处不登记 GitLab 项目'],
    ['外观', AppearancePage, '深色'],
    ['知识默认', KnowledgeDefaultsPage, '不添加项目来源'],
    ['模型', ModelsPage, 'Keychain'],
    ['工具', ToolsPage, 'ActionDigest 绑定'],
    ['执行', ExecutionPage, '显式不安全'],
    ['GitLab', GitlabPage, '令牌经凭据引用'],
    ['SSH', SshPage, '首次连接必须显式确认'],
    ['凭据', CredentialsPage, '不回显'],
  ] as Array<[string, () => React.ReactElement, string]>)('%s 页渲染且含安全提示', async (_label, Page, hint) => {
    render(<Page />);
    await waitFor(() => expect(screen.getAllByText(new RegExp(hint.slice(0, 4))).length).toBeGreaterThan(0));
  });

  it('凭据页空状态显示引导', async () => {
    render(<CredentialsPage />);
    await waitFor(() => expect(screen.getByText(/暂无凭据引用/)).toBeInTheDocument());
  });

  it('S12 记忆页：开关/计数/空状态与禁用态', async () => {
    render(<MemoryPage />);
    await waitFor(() => expect(screen.getByRole('switch', { name: /启用记忆注入/ })).toBeInTheDocument());
    await waitFor(() => expect(screen.getByText(/0 条记忆/)).toBeInTheDocument());
    // 默认项目未开启 → 开关未勾选（enabled=false 来自 settingsGet）。
    const sw = screen.getByRole('switch', { name: /启用记忆注入/ }) as HTMLInputElement;
    expect(sw.checked).toBe(false);
    await waitFor(() => expect(screen.getByText(/当前项目未开启记忆/)).toBeInTheDocument());
  });

  it('S12 记忆页：全局关闭时开关只读并给出说明', async () => {
    rpcMock.mockImplementation((method: string) => {
      if (method === 'project.list') return ok({ items: [{ id: 'prj_1', name: '示例项目', archivedAt: null }] });
      if (method === 'memory.settingsGet')
        return ok({
          projectId: 'prj_1', featureEnabled: false, enabled: false, captureMode: 'off',
          maxEntries: 8, maxBytes: 12288, staleAfterDays: 180, revision: 1,
          updatedAt: '2026-09-04T09:30:00.000Z', updatedBy: 'local',
        });
      if (method === 'memory.list') return ok({ projectId: 'prj_1', items: [], counts: {}, cursor: null });
      return ok({});
    });
    render(<MemoryPage />);
    await waitFor(() => expect(screen.getByText('功能由当前版本/管理员关闭')).toBeInTheDocument());
    const sw = screen.getByRole('switch', { name: /启用记忆注入/ }) as HTMLInputElement;
    expect(sw.disabled).toBe(true);
  });

  it('工具页策略行来自 tool.list', async () => {
    rpcMock.mockImplementation((method: string) => {
      if (method === 'tool.list') {
        return ok({ items: [{ tool_id: 'read_file', enabled: true, risk: 'low', requires_approval: false, network: 'deny', revision: 1 }] });
      }
      return ok({});
    });
    render(<ToolsPage />);
    await waitFor(() => expect(screen.getByText('read_file')).toBeInTheDocument());
  });
});
