import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import App from './App';
import type { DiagnosticReport } from './types';

const report: DiagnosticReport = {
  generatedAt: '2026-08-21T10:00:00Z',
  ready: false,
  integrations: [
    { id: 'vscode', label: 'VS Code', status: 'ready', detail: '扩展已连接', required: true },
    { id: 'gitlab', label: 'GitLab', status: 'needs_configuration', detail: '令牌未配置', required: true },
  ],
  local: [
    { id: 'go', label: 'Go 本地服务', status: 'ready', detail: '运行中', required: false },
  ],
};

function ok(body: unknown): Response {
  return { ok: true, json: async () => body } as unknown as Response;
}

describe('App', () => {
  beforeEach(() => {
    window.localStorage?.clear?.();
    const calls: Array<{ url: string; init?: RequestInit }> = [];
    vi.stubGlobal('fetch', vi.fn(async (input: string | URL, init?: RequestInit) => {
      const url = String(input);
      calls.push({ url, init });
      if (url.endsWith('/api/v1/auth/sessions')) {
        return ok({ token: 'test-token', session: { id: 's1' } });
      }
      if (url.startsWith('/api/v1/workitems?')) {
        return ok({ items: [], nextCursor: '' });
      }
      if (url.startsWith('/api/v1/diagnostics')) {
        return ok(report);
      }
      return ok({});
    }));
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it('opens on the requirements entry page with create and import modes', async () => {
    render(<App />);

    expect(screen.getByRole('heading', { name: '写下你的需求，开始闯关' })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: /写入需求/ })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: /从 GitLab Issue 导入/ })).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText('还没有需求；从上方开始第一关。')).toBeInTheDocument());
  });

  it('exposes approvals and diagnostics in the main navigation', () => {
    render(<App />);
    expect(screen.getByRole('button', { name: /审批中心/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /本地诊断/ })).toBeInTheDocument();
  });
});
