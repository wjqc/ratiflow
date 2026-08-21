import { render, screen, waitFor } from '@testing-library/react';
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

describe('App', () => {
  beforeEach(() => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true, json: async () => report }));
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('renders required integrations and local health from the diagnostics API', async () => {
    render(<App />);

    expect(screen.getByRole('heading', { name: '本地运行诊断' })).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText('GitLab')).toBeInTheDocument());
    // 同一 detail 文本会同时出现在集成表格与事件日志中，必须接受多处匹配
    expect(screen.getAllByText('令牌未配置').length).toBeGreaterThan(0);
    expect(screen.getByText('Go 本地服务')).toBeInTheDocument();
  });
});
