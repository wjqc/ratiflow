import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import '@testing-library/jest-dom/vitest';
import RecoveryPanel from './RecoveryPanel';

// P1-5（RDWS 审计 §8）：skip/rework 恢复状态用户路径——UI 不拥有状态推进权。
const rpcMock = vi.fn();

function ok(body: unknown) {
  return Promise.resolve(body);
}

const REWORK_BLOCKED = {
  id: 'rwk_1',
  from_gate: 'testing',
  target_gate: 'design',
  state: 'blocked',
  progress: 'step_a_committed',
  blocked_reason: 'Step B 对象写中断，可恢复',
  reason_code: 'regression',
};
const REWORK_DONE = {
  id: 'rwk_2',
  from_gate: 'testing',
  target_gate: 'development',
  state: 'completed',
  progress: 'step_b_committed',
  blocked_reason: '',
  reason_code: 'other',
};
const SKIP_BLOCKED = {
  skipRequestId: 'skip_1',
  gate: 'review',
  state: 'blocked',
  progress: 'stage_advanced',
  blockedReason: 'Step B 快照写中断',
  approvalState: 'approved',
};
const SKIP_DONE = {
  skipRequestId: 'skip_2',
  gate: 'review2',
  state: 'completed',
  progress: 'completed',
  blockedReason: '',
};

function seedLists() {
  rpcMock.mockImplementation((method: string) => {
    if (method === 'rework.list') return ok({ items: [REWORK_BLOCKED, REWORK_DONE] });
    if (method === 'gate.skipRequests') return ok({ items: [SKIP_BLOCKED, SKIP_DONE] });
    return ok({});
  });
}

describe('RecoveryPanel 恢复状态（服务器权威）', () => {
  beforeEach(() => {
    cleanup();
    rpcMock.mockReset();
    seedLists();
    (window as unknown as { ratiflow: unknown }).ratiflow = {
      rpc: (method: string, params: Record<string, unknown>) => rpcMock(method, params),
      hello: () => Promise.resolve({ ok: true }),
      onEvent: () => () => {},
    };
  });

  afterEach(() => {
    delete (window as unknown as { ratiflow?: unknown }).ratiflow;
  });

  it('展示服务器读面的 state/progress/blocked，恢复入口只给 blocked/unknown', async () => {
    render(<RecoveryPanel workItemId="wi_1" />);
    await waitFor(() => expect(screen.getByTestId('rework-state-rwk_1')).toHaveTextContent('blocked'));
    expect(screen.getByTestId('rework-progress-rwk_1')).toHaveTextContent('Step A 已提交');
    expect(screen.getByTestId('rework-blocked-rwk_1')).toHaveTextContent('Step B 对象写中断，可恢复');
    expect(screen.getByTestId('skip-state-skip_1')).toHaveTextContent('blocked');
    expect(screen.getByTestId('skip-progress-skip_1')).toHaveTextContent('阶段已推进（Step A 后）');
    // 恢复按钮：blocked 有、completed 无。
    expect(screen.getByTestId('rework-row-rwk_1').querySelector('button')).not.toBeNull();
    expect(screen.getByTestId('skip-row-skip_1').querySelector('button')).not.toBeNull();
    expect(screen.getByTestId('rework-row-rwk_2').querySelector('button')).toBeNull();
    expect(screen.getByTestId('skip-row-skip_2').querySelector('button')).toBeNull();
  });

  it('恢复=发 intent（带幂等键与 resumedBy），随后展示以服务器重读为准', async () => {
    render(<RecoveryPanel workItemId="wi_1" />);
    await waitFor(() => expect(screen.getByTestId('rework-state-rwk_1')).toHaveTextContent('blocked'));
    // 服务器恢复成功后的读面：rwk_1 推进到 completed/step_b_committed。
    rpcMock.mockImplementation((method: string) => {
      if (method === 'rework.list') {
        return ok({ items: [{ ...REWORK_BLOCKED, state: 'completed', progress: 'step_b_committed', blocked_reason: '' }, REWORK_DONE] });
      }
      if (method === 'gate.skipRequests') return ok({ items: [SKIP_BLOCKED, SKIP_DONE] });
      if (method === 'rework.resume') return ok({ state: 'completed', progress: 'step_b_committed' });
      return ok({});
    });
    fireEvent.click(screen.getByTestId('rework-row-rwk_1').querySelector('button')!);
    const resumeCall = rpcMock.mock.calls.find(([m]) => m === 'rework.resume');
    expect(resumeCall).toBeDefined();
    expect(resumeCall![1]).toMatchObject({ operationId: 'rwk_1', resumedBy: 'local-user' });
    expect(typeof (resumeCall![1] as { idempotencyKey: string }).idempotencyKey).toBe('string');
    // 展示来自服务器重读，而非客户端本地推进。
    await waitFor(() => expect(screen.getByTestId('rework-state-rwk_1')).toHaveTextContent('completed'));
    expect(screen.getByTestId('rework-progress-rwk_1')).toHaveTextContent('Step B 已提交');
  });

  it('跳关恢复走 gate.resumeSkip intent；失败如实展示且状态不推进', async () => {
    render(<RecoveryPanel workItemId="wi_1" />);
    await waitFor(() => expect(screen.getByTestId('skip-state-skip_1')).toHaveTextContent('blocked'));
    rpcMock.mockImplementation((method: string, params: Record<string, unknown>) => {
      if (method === 'gate.resumeSkip') {
        return Promise.reject(new Error('recovery_conflict: 状态已变化'));
      }
      if (method === 'rework.list') return ok({ items: [REWORK_BLOCKED, REWORK_DONE] });
      if (method === 'gate.skipRequests') return ok({ items: [SKIP_BLOCKED, SKIP_DONE] });
      return ok({});
    });
    fireEvent.click(screen.getByTestId('skip-row-skip_1').querySelector('button')!);
    const resumeCall = rpcMock.mock.calls.find(([m]) => m === 'gate.resumeSkip');
    expect(resumeCall![1]).toMatchObject({ skipRequestId: 'skip_1' });
    expect(typeof (resumeCall![1] as { idempotencyKey: string }).idempotencyKey).toBe('string');
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('恢复失败'));
    // 服务器读面未变 → 展示保持 blocked。
    expect(screen.getByTestId('skip-state-skip_1')).toHaveTextContent('blocked');
  });

  it('无操作时面板不渲染（不占任务页空间）', async () => {
    rpcMock.mockImplementation((method: string) => {
      if (method === 'rework.list') return ok({ items: [] });
      if (method === 'gate.skipRequests') return ok({ items: [] });
      return ok({});
    });
    const { container } = render(<RecoveryPanel workItemId="wi_1" />);
    await waitFor(() => expect(rpcMock.mock.calls.filter(([m]) => m === 'rework.list').length).toBeGreaterThan(0));
    expect(container.querySelector('[data-testid="recovery-panel"]')).toBeNull();
  });
});
