import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import '@testing-library/jest-dom/vitest';
import AppShell from './AppShell';

// renderer 组件测试：mock preload bridge（不依赖 Electron/Rust core）。
const rpcMock = vi.fn();

function ok(body: unknown) {
  return Promise.resolve(body);
}

describe('AppShell', () => {
  beforeEach(() => {
    cleanup();
    rpcMock.mockReset();
    rpcMock.mockImplementation((method: string) => {
      switch (method) {
        case 'project.list':
          return ok({ items: [{ id: 'pj_1', name: '演示项目', namespace: 'team', project: 'demo', status: 'ready' }] });
        case 'workitem.list':
          return ok({ items: [{ id: 'wi_1', title: '支持 SSO 登录', currentGate: 'requirements' }] });
        case 'diagnostics.check':
          return ok({ generatedAt: '2026-09-05T00:00:00Z', local: [], integrations: [] });
        case 'settings.summary':
          return ok({
            overallStatus: 'ready',
            checkedAt: '2026-09-05T00:00:00Z',
            blockers: [],
            components: [],
            dataSafety: { credentialRefCount: 0, lastBackup: null, auditEventsLast7Days: 0 },
            recentChanges: [],
          });
        default:
          return ok({});
      }
    });
    // 只注入 ratiflow bridge，不替换 window 本体（testing-library 依赖 document 绑定）。
    (window as unknown as { ratiflow: unknown }).ratiflow = {
      rpc: (method: string, params: Record<string, unknown>) => rpcMock(method, params),
      hello: () => Promise.resolve({ ok: true }),
      selectFile: () => Promise.resolve(null),
      openExternal: () => Promise.resolve(),
    };
  });

  afterEach(() => {
    delete (window as unknown as { ratiflow?: unknown }).ratiflow;
  });

  it('显示需求入口与最近任务（来自真实 core 数据）', async () => {
    render(<AppShell />);
    expect(screen.getByText('写下你的需求，开始闯关')).toBeInTheDocument();
    await waitFor(() => expect(screen.getAllByText('支持 SSO 登录').length).toBeGreaterThan(0));
    expect(screen.getAllByText('需求关').length).toBeGreaterThan(0);
  });

  it('主侧栏只保留任务与审批入口，设置从底部齿轮进入', async () => {
    render(<AppShell />);
    await waitFor(() => expect(screen.getAllByText('演示项目').length).toBeGreaterThan(0));
    const projectNav = within(screen.getByRole('complementary', { name: '项目导航' }));
    expect(projectNav.getByRole('button', { name: /审批中心/ })).toBeInTheDocument();
    expect(projectNav.getByRole('button', { name: /新建任务/ })).toBeInTheDocument();
    expect(projectNav.queryByRole('button', { name: '需求入口' })).not.toBeInTheDocument();
    expect(projectNav.queryByRole('button', { name: '设置与诊断' })).not.toBeInTheDocument();
    expect(projectNav.getByRole('button', { name: '设置' })).toBeInTheDocument();
  });

  it('进入设置后替换项目侧栏，并按需展开高级设置后返回原工作区', async () => {
    render(<AppShell />);
    await waitFor(() => expect(screen.getByRole('complementary', { name: '项目导航' })).toBeInTheDocument());

    fireEvent.click(screen.getByRole('button', { name: '设置' }));
    await waitFor(() => expect(screen.getByRole('complementary', { name: '设置导航' })).toBeInTheDocument());
    await waitFor(() => expect(screen.getByRole('heading', { name: /^常规$/ })).toBeInTheDocument());
    expect(screen.queryByRole('complementary', { name: '项目导航' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^使用统计$/ })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: '外部集成' })).not.toBeInTheDocument();
    expect(screen.getAllByRole('complementary')).toHaveLength(1);

    fireEvent.click(screen.getByRole('button', { name: '高级设置' }));
    expect(screen.getByRole('button', { name: /^使用统计$/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: '外部集成' })).toBeInTheDocument();

    // 外部集成是普通页：GitLab / SSH 目标机为页内区块，不再是独立菜单项。
    fireEvent.click(screen.getByRole('button', { name: '外部集成' }));
    await waitFor(() => expect(screen.getByRole('heading', { name: '外部集成' })).toBeInTheDocument());
    expect(screen.getByRole('heading', { name: 'GitLab' })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'SSH 目标机' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'GitLab' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'SSH 目标机' })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: '返回工作区' }));
    await waitFor(() => expect(screen.getByRole('complementary', { name: '项目导航' })).toBeInTheDocument());
  });

  it('core 不可用时显示错误标记', async () => {
    (window as unknown as { ratiflow: unknown }).ratiflow = {
      rpc: () => Promise.reject(new Error('core 未运行')),
      hello: () => Promise.resolve({ ok: false, error: 'down' }),
      selectFile: () => Promise.resolve(null),
      openExternal: () => Promise.resolve(),
    };
    render(<AppShell />);
    await waitFor(() => expect(screen.getByTitle('Rust core 不可用')).toBeInTheDocument());
  });

  it('项目树可独立展开/收起（chevron 不切换项目）', async () => {
    rpcMock.mockImplementation((method: string, params: Record<string, unknown>) => {
      switch (method) {
        case 'project.list':
          return ok({
            items: [
              { id: 'pj_1', name: '演示项目', status: 'ready' },
              { id: 'pj_2', name: '第二项目', status: 'ready' },
            ],
          });
        case 'workitem.list':
          return ok({
            items:
              params.projectId === 'pj_2'
                ? [{ id: 'wi_2', title: '第二项目任务', currentGate: 'requirements' }]
                : [{ id: 'wi_1', title: '支持 SSO 登录', currentGate: 'requirements' }],
          });
        default:
          return ok({});
      }
    });
    render(<AppShell />);
    await waitFor(() => expect(screen.getAllByText('演示项目').length).toBeGreaterThan(0));
    // 初始：活动项目自动展开（effect 异步提交，需等待），第二项目收起。
    await waitFor(() =>
      expect(screen.getByRole('button', { name: '收起 演示项目' })).toBeInTheDocument(),
    );
    expect(screen.getByRole('button', { name: '展开 第二项目' })).toBeInTheDocument();
    // 展开第二项目：懒加载其任务，但不切换活动项目。
    fireEvent.click(screen.getByRole('button', { name: '展开 第二项目' }));
    await waitFor(() =>
      expect(rpcMock).toHaveBeenCalledWith(
        'workitem.list',
        expect.objectContaining({ projectId: 'pj_2' }),
      ),
    );
    await waitFor(() => expect(screen.getAllByText('第二项目任务').length).toBe(2));
    expect(screen.getByRole('button', { name: '收起 第二项目' })).toBeInTheDocument();
    // 收起后树节点隐藏（仅剩首页「最近任务」表格一处）。
    fireEvent.click(screen.getByRole('button', { name: '收起 第二项目' }));
    expect(screen.getAllByText('第二项目任务').length).toBe(1);
  });

  it('移除项目两步确认后走归档并从侧栏消失', async () => {
    render(<AppShell />);
    await waitFor(() => expect(screen.getAllByText('演示项目').length).toBeGreaterThan(0));
    // 第一步：进入确认态（按钮变「确认移除」）。
    fireEvent.click(screen.getByRole('button', { name: '移除项目 演示项目' }));
    // 第二步：3 秒内再点执行归档。
    fireEvent.click(screen.getByRole('button', { name: '确认移除项目 演示项目' }));
    await waitFor(() =>
      expect(rpcMock).toHaveBeenCalledWith('project.archive', {
        projectId: 'pj_1',
        archived: true,
      }),
    );
    await waitFor(() => expect(screen.queryByText('演示项目')).not.toBeInTheDocument());
  });

  it('移除任务两步确认后走归档并刷新列表', async () => {
    render(<AppShell />);
    await waitFor(() => expect(screen.getAllByText('支持 SSO 登录').length).toBeGreaterThan(0));
    fireEvent.click(screen.getByRole('button', { name: '移除任务 支持 SSO 登录' }));
    fireEvent.click(screen.getByRole('button', { name: '确认移除任务 支持 SSO 登录' }));
    await waitFor(() =>
      expect(rpcMock).toHaveBeenCalledWith('workitem.archive', {
        workItemId: 'wi_1',
        archived: true,
      }),
    );
    await waitFor(() =>
      expect(rpcMock).toHaveBeenCalledWith(
        'workitem.list',
        expect.objectContaining({ projectId: 'pj_1' }),
      ),
    );
  });
});
