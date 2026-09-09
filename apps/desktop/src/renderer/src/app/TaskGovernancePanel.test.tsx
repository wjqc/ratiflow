import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import '@testing-library/jest-dom/vitest';
import TaskGovernancePanel from './TaskGovernancePanel';

const rpcMock = vi.fn();

describe('TaskGovernancePanel', () => {
  beforeEach(() => {
    cleanup();
    rpcMock.mockReset();
    rpcMock.mockImplementation((method: string) => {
      if (method === 'plan.list') return Promise.resolve({ items: [{ id: 'plan_1', revision_no: 1, status: 'draft' }] });
      if (method === 'snapshot.list') return Promise.resolve({ items: [{ id: 'snap_1', kind: 'pre_gate', created_at: '2026-09-08', root_digest: 'abcdef123456' }] });
      if (method === 'stage.attempts') return Promise.resolve({ items: [{ id: 'attempt_1', gate: 'testing' }] });
      return Promise.resolve({});
    });
    (window as unknown as { ratiflow: unknown }).ratiflow = {
      rpc: (method: string, params: Record<string, unknown>) => rpcMock(method, params),
      onEvent: () => () => {},
    };
  });

  afterEach(() => delete (window as unknown as { ratiflow?: unknown }).ratiflow);

  it('展开后读取计划、快照和 attempt，并通过领域入口提交治理 intent', async () => {
    render(<TaskGovernancePanel workItemId="wi_1" currentGate="testing" stages={[{ gate: 'design', state: 'passed' }, { gate: 'testing', state: 'running' }]} onChanged={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: '展开' }));
    await waitFor(() => expect(screen.getByText(/v1 · draft/)).toBeInTheDocument());
    expect(screen.getByText(/pre_gate/)).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText('治理理由'), { target: { value: '测试结果需要重新设计' } });
    fireEvent.change(screen.getByLabelText('目标关卡'), { target: { value: 'design' } });
    fireEvent.click(screen.getByRole('button', { name: '申请返工到所选关' }));
    await waitFor(() => expect(rpcMock).toHaveBeenCalledWith('rework.preview', expect.objectContaining({ workItemId: 'wi_1', targetGate: 'design' })));
    expect(rpcMock).toHaveBeenCalledWith('rework.request', expect.objectContaining({ idempotencyKey: expect.any(String) }));
  });

  it('计划提交使用 plan.submit，回滚先 preview 再 request', async () => {
    render(<TaskGovernancePanel workItemId="wi_1" currentGate="testing" stages={[{ gate: 'testing', state: 'running' }]} onChanged={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: '展开' }));
    await waitFor(() => expect(screen.getByRole('button', { name: '提交审批' })).toBeInTheDocument());
    fireEvent.click(screen.getByRole('button', { name: '提交审批' }));
    await waitFor(() => expect(rpcMock).toHaveBeenCalledWith('plan.submit', { planRevisionId: 'plan_1' }));
    fireEvent.click(screen.getByRole('button', { name: '预览并申请回滚' }));
    await waitFor(() => expect(rpcMock).toHaveBeenCalledWith('rollback.preview', { workItemId: 'wi_1', targetSnapshotId: 'snap_1' }));
    expect(rpcMock).toHaveBeenCalledWith('rollback.request', { workItemId: 'wi_1', targetSnapshotId: 'snap_1', requestedBy: 'local-user' });
  });
});
