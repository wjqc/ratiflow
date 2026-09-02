import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import '@testing-library/jest-dom/vitest';
import NewTaskPage from './NewTaskPage';

describe('新建任务', () => {
  const rpcMock = vi.fn();
  const onCreated = vi.fn();

  beforeEach(() => {
    cleanup();
    rpcMock.mockReset();
    onCreated.mockReset();
    rpcMock.mockImplementation((method: string) => {
      switch (method) {
        case 'workitem.create': return Promise.resolve({ id: 'wi_new' });
        case 'artifact.list': return Promise.resolve({ items: [] });
        case 'artifact.create': return Promise.resolve({ id: 'ar_prd', kind: 'prd' });
        case 'stage.startActivity': return Promise.resolve({ runId: 'run_prd' });
        default: return Promise.resolve({});
      }
    });
    (window as unknown as { sixgates: unknown }).sixgates = {
      rpc: (method: string, params: Record<string, unknown>) => rpcMock(method, params),
      hello: () => Promise.resolve({ ok: true }),
      selectFile: () => Promise.resolve(null),
      selectDirectory: () => Promise.resolve(null),
      openExternal: () => Promise.resolve(),
      appInfo: () => Promise.resolve({ desktopVersion: 'test', logDir: '', userDataDir: '' }),
      openLogs: () => Promise.resolve(),
      onEvent: () => () => undefined,
    };
  });

  afterEach(() => {
    delete (window as unknown as { sixgates?: unknown }).sixgates;
  });

  it('提交文字需求后立即启动 PRD 起草并进入任务', async () => {
    render(
      <NewTaskPage
        projectId="pj_1"
        projects={[{ id: 'pj_1', name: 'SixGates' }]}
        onCreated={onCreated}
        onWorkspaceChanged={() => undefined}
        onOpenRemote={() => undefined}
        onBack={() => undefined}
      />,
    );

    fireEvent.change(screen.getByLabelText('需求描述'), {
      target: { value: '支持可搜索的项目工作区选择器' },
    });
    fireEvent.click(screen.getByRole('button', { name: '创建并进入需求关' }));

    await waitFor(() => expect(onCreated).toHaveBeenCalledWith('wi_new', 'pj_1'));
    expect(rpcMock).toHaveBeenCalledWith(
      'stage.startActivity',
      expect.objectContaining({
        workItemId: 'wi_new',
        gate: 'requirements',
        idempotencyKey: 'auto-prd-wi_new',
      }),
    );
  });
});
