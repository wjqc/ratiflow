import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import '@testing-library/jest-dom/vitest';
import GovernancePage from './GovernancePage';

// P1-5（RDWS 审计 §8）：metrics/Triage + knowledge freshness 页面路径——纯读投影，
// unknown/样本不足如实呈现（不虚构 0），无状态推进入口。
const rpcMock = vi.fn();

function ok(body: unknown) {
  return Promise.resolve(body);
}

const METRICS = {
  scope: 'global',
  windowDays: 30,
  orphanRate: { totalNodes: 3, orphanCount: 0, rate: null, insufficientData: true },
  loopRate: {
    completedReworks: 4,
    reworkedWorkitems: 2,
    workitemsWithReleases: 8,
    averageReworkCount: 0.5,
    reworkWorkitemRate: 0.25,
    insufficientData: false,
  },
  approvalLayers: {
    dimensions: ['subjectType', 'risk'],
    layers: [
      {
        subjectType: 'tool_proposal',
        risk: 'high',
        approved: 10,
        rejected: 2,
        pending: 1,
        expired: 0,
        changesRequested: 1,
        passRate: 0.833,
        sample: 14,
        insufficientData: false,
        rubberStampSuspect: false,
      },
    ],
  },
  aiSuggestionAdoption: { decided: 2, accepted: 1, rate: null, insufficientData: true },
};

const TRIAGE = {
  items: [{ workitemId: 'wi_9', orphanCount: 1, unverifiedCount: 2, uncoveredCount: 3 }],
  knowledgeBlocked: [{ projectId: 'pj_1', stableId: 'repo-docs', state: 'unverified' }],
  objectOrphans: { count: 5, total: 40, pruneGated: true },
  unknownReconciliation: { runIntents: 2, toolOutcomesUnknown: 1, toolOutcomesReconciliationPending: 3 },
};

const FRESHNESS = {
  projectId: 'pj_1',
  items: [
    { stableId: 'api-spec', state: 'verified', nextDueAt: '2026-09-15T00:00:00Z', inputRevisionMode: 'committed', contentOwner: null },
    { stableId: 'repo-docs', state: 'unverified_content_changed', nextDueAt: null, inputRevisionMode: 'worktree', contentOwner: 'alice' },
    { stableId: 'runbook', state: 'failed', nextDueAt: null, inputRevisionMode: 'content_hash', contentOwner: null },
  ],
};

describe('GovernancePage 治理总览（纯读）', () => {
  beforeEach(() => {
    cleanup();
    rpcMock.mockReset();
    rpcMock.mockImplementation((method: string) => {
      if (method === 'project.list') return ok({ items: [{ id: 'pj_1', name: '主项目' }] });
      if (method === 'metrics.overview') return ok(METRICS);
      if (method === 'triage.list') return ok(TRIAGE);
      if (method === 'knowledge.freshnessOverview') return ok(FRESHNESS);
      return ok({});
    });
    (window as unknown as { ratiflow: unknown }).ratiflow = {
      rpc: (method: string, params: Record<string, unknown>) => rpcMock(method, params),
      hello: () => Promise.resolve({ ok: true }),
      onEvent: () => () => {},
    };
  });

  afterEach(() => {
    delete (window as unknown as { ratiflow?: unknown }).ratiflow;
  });

  it('metrics 唯一口径：回环双指标 + 审批 subjectType×risk 分层；null 如实显示样本不足', async () => {
    render(<GovernancePage />);
    await waitFor(() => expect(screen.getByTestId('metrics-card')).toBeInTheDocument());
    // 孤儿率 null（样本不足）→ 不虚构 0%。
    expect(screen.getByTestId('metrics-orphan')).toHaveTextContent('样本不足');
    // 回环双口径同屏。
    const loop = screen.getByTestId('metrics-loop');
    expect(loop).toHaveTextContent('平均返工次数 0.5');
    expect(loop).toHaveTextContent('返工任务占比 25.0%');
    // 审批分层按 subject×risk 展示通过率。
    expect(screen.getByTestId('approval-pass-rate-tool_proposal-high')).toHaveTextContent('83.3%');
    // 建议采纳样本不足 → 如实。
    expect(screen.getByTestId('metrics-adoption')).toHaveTextContent('样本不足');
  });

  it('Triage 聚合：unknown 对账 / 对象库无引用（PRUNE 门控提示）/ knowledge block', async () => {
    render(<GovernancePage />);
    await waitFor(() => expect(screen.getByTestId('triage-card')).toBeInTheDocument());
    expect(screen.getByTestId('triage-unknown')).toHaveTextContent('run intent 2');
    expect(screen.getByTestId('triage-unknown')).toHaveTextContent('工具结果 unknown 1');
    expect(screen.getByTestId('triage-unknown')).toHaveTextContent('对账 pending 3');
    expect(screen.getByTestId('triage-orphans')).toHaveTextContent('5 / 40');
    expect(screen.getByTestId('triage-orphans')).toHaveTextContent('清理受 GC PRUNE 门控');
    expect(screen.getByTestId('triage-knowledge-blocked')).toHaveTextContent('repo-docs（unverified）');
  });

  it('knowledge freshness：状态字典如实（已验证/内容已变更/验证失败）', async () => {
    render(<GovernancePage />);
    await waitFor(() =>
      expect(screen.getByTestId('freshness-state-api-spec')).toHaveTextContent('已验证'),
    );
    expect(screen.getByTestId('freshness-state-repo-docs')).toHaveTextContent('内容已变更（待重验）');
    expect(screen.getByTestId('freshness-state-runbook')).toHaveTextContent('验证失败');
  });

  it('纯读页面：无任何按钮（不拥有状态推进权）', async () => {
    render(<GovernancePage />);
    await waitFor(() => expect(screen.getByTestId('freshness-card')).toBeInTheDocument());
    expect(document.querySelectorAll('button').length).toBe(0);
  });
});
