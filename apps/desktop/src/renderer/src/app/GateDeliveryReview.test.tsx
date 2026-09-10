import { beforeEach, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import '@testing-library/jest-dom/vitest';
import { rpc } from '../rpc/client';
import GateDeliveryReview, { acceptanceLine, decideGateDelivery, loadGateAcceptance, type ReviewDelivery } from './GateDeliveryReview';
vi.mock('../rpc/client', () => ({ rpc: vi.fn(), rpcErrorMessage: String }));
const mock = vi.mocked(rpc);
const reviewed: ReviewDelivery[] = [{ artifactId: 'a', kind: 'prd', title: 'PRD', revisionId: 'r1', etag: 'v1', status: 'draft', content: '# PRD' }];
let revisionId: string;
let passed: boolean;
let pending: boolean;
beforeEach(() => {
  mock.mockReset(); revisionId = 'r1'; passed = true; pending = false;
  mock.mockImplementation(async (method) => {
    switch (method) {
      case 'workitem.get': return { workItem: { current_gate: 'requirements' } };
      case 'gate.deliverableStatus': return { requiredKind: 'prd' };
      case 'artifact.list': return { items: [{ id: 'a', kind: 'prd', title: 'PRD' }] };
      case 'artifact.listRevisions': return { items: [{ id: revisionId, rev_no: 1, etag: 'v1', status: pending ? 'frozen' : 'draft' }] };
      case 'artifact.revisionContent': return { content: '# PRD' };
      case 'stage.package': return { releaseRequests: pending ? [{ state: 'pending', approval_id: 'approval' }] : [] };
      case 'evidence.list': return { items: [] };
      case 'trace.coverage': return { items: [{ requirementKey: 'REQ-1', status: 'active' }] };
      case 'evidence.record': return { id: 'e' };
      case 'gate.evaluate': return { passed, failed_inputs: ['required_checks_passed'] };
      case 'gate.requestRelease': return { approval_id: 'approval' };
      default: return {};
    }
  });
});
it('approves via the domain release decision only after review, freezing, evidence and checks', async () => {
  await decideGateDelivery('w', 'requirements', reviewed, 'approve', '内容符合要求');
  const methods = mock.mock.calls.map(([method]) => method);
  expect(methods.indexOf('artifact.freezeBaseline')).toBeLessThan(methods.indexOf('gate.evaluate'));
  expect(methods.indexOf('gate.evaluate')).toBeLessThan(methods.indexOf('gate.requestRelease'));
  expect(mock).toHaveBeenLastCalledWith('gate.decideRelease', { approvalId: 'approval', decision: 'approved', decidedBy: 'local-user', reason: '内容符合要求' });
});
it('stores rejection feedback without requesting approval or advancing the gate', async () => {
  await decideGateDelivery('w', 'requirements', reviewed, 'reject', '补充验收标准');
  expect(mock).toHaveBeenCalledWith('artifact.addReview', { revisionId: 'r1', reviewer: 'local-user', verdict: 'changes_requested', comment: '补充验收标准' });
  expect(mock.mock.calls.some(([method]) => method === 'gate.decideRelease' || method === 'gate.requestRelease')).toBe(false);
});
it('requires rejection feedback and refuses a revision that changed after preview', async () => {
  await expect(decideGateDelivery('w', 'requirements', reviewed, 'reject', '')).rejects.toThrow('评语');
  revisionId = 'new';
  await expect(decideGateDelivery('w', 'requirements', reviewed, 'approve', '')).rejects.toThrow('新版本');
  expect(mock.mock.calls.some(([method]) => method === 'artifact.addReview')).toBe(false);
});
it('keeps failed gate checks as blockers and resumes an existing release without duplicate preparation', async () => {
  passed = false;
  await expect(decideGateDelivery('w', 'requirements', reviewed, 'approve', '')).rejects.toThrow('检查未通过');
  expect(mock.mock.calls.some(([method]) => method === 'gate.decideRelease')).toBe(false);
  mock.mockClear(); pending = true;
  await decideGateDelivery('w', 'requirements', reviewed, 'approve', '确认通过');
  expect(mock.mock.calls.some(([method]) => method === 'artifact.freezeBaseline' || method === 'evidence.record')).toBe(false);
  expect(mock).toHaveBeenLastCalledWith('gate.decideRelease', expect.objectContaining({ decision: 'approved' }));
});
it('renders the acceptance checklist beside deliverables for item-by-item verification', async () => {
  mock.mockImplementation(async (method) => {
    switch (method) {
      case 'workflow.getInstance':
        return { gates: [{ gate_id: 'design', title: '设计关', purpose: '产品与技术方案', acceptance: ['完成技术方案设计', { verifier: 'text_nonempty', artifact_kind: 'tech_design' }] }] };
      case 'gate.deliverableStatus': return { requiredKind: 'tech_design' };
      case 'artifact.list': return { items: [{ id: 'a', kind: 'tech_design', title: '技术方案' }] };
      case 'artifact.listRevisions': return { items: [{ id: 'r1', rev_no: 1, etag: 'v1', status: 'draft' }] };
      case 'artifact.revisionContent': return { content: '# 方案' };
      default: return {};
    }
  });
  render(<GateDeliveryReview workItemId="w" gate="design" onDone={() => {}} onRevise={() => {}} />);
  expect(await screen.findByRole('heading', { name: /验收标准/ })).toBeVisible();
  expect(screen.getByText('完成技术方案设计')).toBeVisible();
  expect(screen.getByText('text_nonempty：交付物 tech_design 正文非空')).toBeVisible();
  cleanup();
});
it('falls back to the generic gate checklist when no workflow instance exists', async () => {
  mock.mockImplementation(async (method) => (method === 'workflow.getInstance' ? Promise.reject(new Error('无实例')) : {}));
  expect(await loadGateAcceptance('w', 'requirements')).toBeNull();
  expect(acceptanceLine('普通条目')).toBe('普通条目');
});
