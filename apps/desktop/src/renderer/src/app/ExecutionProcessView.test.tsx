import '@testing-library/jest-dom/vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { ExecutionProcessView } from './Workbench';
import { rpc } from '../rpc/client';
vi.mock('../rpc/client', () => ({ rpc: vi.fn(), rpcErrorMessage: String }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });
const ts = '2026-09-10T01:00:00Z';
it('shows saved PRD outside the file directory, opens its content, expands checkpoints and excludes past failures', async () => {
  vi.mocked(rpc).mockImplementation(async (method) => {
    if (method === 'artifact.list') return { items: [{ id: 'a', kind: 'prd', title: 'PRD' }] };
    if (method === 'artifact.listRevisions') return { items: [{ id: 'r', rev_no: 1, status: 'draft' }] };
    if (method === 'artifact.revisionContent') return { content: '# Saved PRD\nFull persisted content' };
    throw new Error('feature_disabled');
  });
  render(<ExecutionProcessView workItemId="w" gate="requirements" progress={null} events={[]} docs={['requirement.md']} knowledgeCount={0}
    runs={[{ id: 'new', goal: 'write', result: 'Final document', status: 'completed_execution', created_at: ts, updated_at: ts }, { id: 'old', goal: 'write', result: 'old model failure', status: 'failed', created_at: ts, updated_at: ts }]}
    trace={{ steps: [{ kind: 'reasoning', seq: 1, ts, name: 'final', summary: 'Truncated final should not be reasoning' }], checkpoints: [1, 2, 3].map((seq) => ({ seq, createdAt: ts })) }} />);
  fireEvent.click(await screen.findByRole('button', { name: 'PRD · 草稿' }));
  expect(await screen.findByRole('heading', { name: 'Saved PRD' })).toBeVisible();
  expect(screen.queryByText('暂无已保存产物')).not.toBeInTheDocument();
  expect(screen.queryByText('检查点 #3')).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: '查看全部检查点（3）' }));
  expect(screen.getByText('检查点 #3')).toBeVisible();
  fireEvent.click(screen.getByRole('button', { name: '收起检查点' }));
  expect(screen.queryByText('检查点 #3')).not.toBeInTheDocument();
  expect(screen.queryByText('old model failure')).not.toBeInTheDocument();
  expect(screen.queryByText('模型推理摘要')).not.toBeInTheDocument();
});
it('distinguishes a failed artifact lookup from a genuinely empty output list', async () => {
  vi.mocked(rpc).mockRejectedValue(new Error('offline'));
  render(<ExecutionProcessView workItemId="w" gate="requirements" progress={null} events={[]} docs={[]} knowledgeCount={0} runs={[]} trace={null} />);
  expect(await screen.findByRole('alert')).toHaveTextContent('产物加载失败');
  expect(screen.queryByText('暂无已保存产物')).not.toBeInTheDocument();
});
it('lists the upstream deliverables and versions bound to the latest run', async () => {
  vi.mocked(rpc).mockImplementation(async (method) => {
    if (method === 'artifact.list') return { items: [] };
    if (method === 'agent.gateContext') {
      return { upstream: [{ gate: 'requirements', gateTitle: '需求关', kind: 'prd', title: 'PRD', revisionId: 'rev_1', revNo: 3, etag: '"abc123"', baselineId: 'base_9' }] };
    }
    throw new Error('unexpected ' + method);
  });
  render(<ExecutionProcessView workItemId="w" gate="design" progress={null} events={[]} docs={['requirement.md']} knowledgeCount={2}
    runs={[{ id: 'run-1', goal: '设计', result: '', status: 'running', created_at: ts, updated_at: ts }]} trace={null} />);
  expect(await screen.findByText(/需求关《PRD》 · rev 3 · "abc123"/)).toBeVisible();
  expect(screen.getByText(/以上为本次运行绑定的上游已批准版本/)).toBeVisible();
});
