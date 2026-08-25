// 新接线的 9 个设置页：mock bridge 下渲染不崩溃 + 空/加载状态展示。
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import '@testing-library/jest-dom/vitest';
import { GeneralPage } from './GeneralPage';
import { AppearancePage } from './AppearancePage';
import { KnowledgeDefaultsPage } from './KnowledgeDefaultsPage';
import { ModelsPage } from './ModelsPage';
import { ToolsPage } from './ToolsPage';
import { ExecutionPage } from './ExecutionPage';
import { GitlabPage } from './GitlabPage';
import { SshPage } from './SshPage';
import { CredentialsPage } from './CredentialsPage';

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
          return ok({ items: [] });
        case 'executor.settings.get': return ok({ revision: 0 });
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
    ['外观', AppearancePage, '深色主题在 MVP 中标记为实验状态'],
    ['知识默认', KnowledgeDefaultsPage, '此处不添加项目来源'],
    ['模型', ModelsPage, '此页不输入/回显密钥'],
    ['工具', ToolsPage, 'ActionDigest 绑定'],
    ['执行', ExecutionPage, '显式不安全模式需二次确认'],
    ['GitLab', GitlabPage, '令牌经凭据引用存 Keychain'],
    ['SSH', SshPage, '首次连接必须显式确认指纹'],
    ['凭据', CredentialsPage, '创建后永不回显'],
  ] as Array<[string, () => JSX.Element, string]>)('%s 页渲染且含安全提示', async (_label, Page, hint) => {
    render(<Page />);
    await waitFor(() => expect(screen.getAllByText(new RegExp(hint.slice(0, 6))).length).toBeGreaterThan(0));
  });

  it('凭据页空状态显示引导', async () => {
    render(<CredentialsPage />);
    await waitFor(() => expect(screen.getByText(/尚无凭据/)).toBeInTheDocument());
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
