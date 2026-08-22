import { cleanup, render, screen, waitFor } from '@testing-library/react';
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
        default:
          return ok({});
      }
    });
    // 只注入 sixgates bridge，不替换 window 本体（testing-library 依赖 document 绑定）。
    (window as unknown as { sixgates: unknown }).sixgates = {
      rpc: (method: string, params: Record<string, unknown>) => rpcMock(method, params),
      hello: () => Promise.resolve({ ok: true }),
      selectFile: () => Promise.resolve(null),
      openExternal: () => Promise.resolve(),
    };
  });

  afterEach(() => {
    delete (window as unknown as { sixgates?: unknown }).sixgates;
  });

  it('显示需求入口与最近任务（来自真实 core 数据）', async () => {
    render(<AppShell />);
    expect(screen.getByText('写下你的需求，开始闯关')).toBeInTheDocument();
    await waitFor(() => expect(screen.getAllByText('支持 SSO 登录').length).toBeGreaterThan(0));
    expect(screen.getByText('需求关')).toBeInTheDocument();
  });

  it('侧栏展示项目与六关导航', async () => {
    render(<AppShell />);
    await waitFor(() => expect(screen.getAllByText('演示项目').length).toBeGreaterThan(0));
    expect(screen.getByRole('button', { name: /审批中心/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /设置与诊断/ })).toBeInTheDocument();
  });

  it('core 不可用时显示错误标记', async () => {
    (window as unknown as { sixgates: unknown }).sixgates = {
      rpc: () => Promise.reject(new Error('core 未运行')),
      hello: () => Promise.resolve({ ok: false, error: 'down' }),
      selectFile: () => Promise.resolve(null),
      openExternal: () => Promise.resolve(),
    };
    render(<AppShell />);
    await waitFor(() => expect(screen.getByTitle('Rust core 不可用')).toBeInTheDocument());
  });
});
