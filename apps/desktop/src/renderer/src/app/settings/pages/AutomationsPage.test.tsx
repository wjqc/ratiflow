import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import '@testing-library/jest-dom/vitest';
import { AutomationsPage } from './AutomationsPage';

const rpcMock = vi.fn();

describe('AutomationsPage', () => {
  beforeEach(() => {
    cleanup();
    rpcMock.mockReset();
    rpcMock.mockImplementation((method: string) => {
      if (method === 'automation.list') return Promise.resolve({ items: [{ id: 'auto_1', key: 'daily', workItemId: 'wi_1', intervalSecs: 3600, nextFireAt: '2026-09-09', status: 'active', revision: 2 }] });
      if (method === 'automation.observations') return Promise.resolve({ items: [], stats: { decided: 0 } });
      if (method === 'automation.history') return Promise.resolve({ items: [{ status: 'intent_created' }] });
      return Promise.resolve({});
    });
    (window as unknown as { ratiflow: unknown }).ratiflow = {
      rpc: (method: string, params: Record<string, unknown>) => rpcMock(method, params),
      onEvent: () => () => {},
    };
  });

  afterEach(() => delete (window as unknown as { ratiflow?: unknown }).ratiflow);

  it('暂停使用 revision CAS，立即执行携带调度时间', async () => {
    render(<AutomationsPage />);
    await waitFor(() => expect(screen.getByText(/daily · active/)).toBeInTheDocument());
    fireEvent.click(screen.getByRole('button', { name: '暂停' }));
    await waitFor(() => expect(rpcMock).toHaveBeenCalledWith('automation.pause', { automationId: 'auto_1', expectedRevision: 2 }));
    fireEvent.click(screen.getByRole('button', { name: '立即执行' }));
    await waitFor(() => expect(rpcMock).toHaveBeenCalledWith('automation.runNow', expect.objectContaining({ automationId: 'auto_1', scheduledFor: expect.any(String) })));
  });

  it('切换影子模式携带幂等键', async () => {
    render(<AutomationsPage />);
    await waitFor(() => expect(screen.getByRole('button', { name: '影子模式' })).toBeInTheDocument());
    fireEvent.click(screen.getByRole('button', { name: '影子模式' }));
    await waitFor(() => expect(rpcMock).toHaveBeenCalledWith('automation.setShadowMode', expect.objectContaining({ automationId: 'auto_1', shadowMode: true, expectedRevision: 2, idempotencyKey: expect.any(String) })));
  });
});
