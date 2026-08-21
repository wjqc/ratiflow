import { useCallback, useEffect, useState } from 'react';
import { decideApproval, listApprovals } from './api';
import type { Approval } from './types';

export default function ApprovalsPage() {
  const [items, setItems] = useState<Approval[]>([]);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(true);
  const [actor, setActor] = useState('');

  const refresh = useCallback(async () => {
    setLoading(true);
    setError('');
    try {
      const page = await listApprovals();
      setItems(page.items);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '加载审批失败');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const decide = async (id: string, decision: 'approved' | 'rejected') => {
    if (!actor.trim()) {
      setError('请先填写审批人标识');
      return;
    }
    setError('');
    try {
      await decideApproval(id, decision, actor.trim());
      await refresh();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '审批操作失败');
    }
  };

  return (
    <div className="stack">
      <div className="page-heading">
        <div>
          <h1>审批中心</h1>
          <p>高风险动作（部署、高危工具）默认拒绝，直到本机用户确认。</p>
        </div>
      </div>
      {error ? <div className="error-banner" role="alert">{error}</div> : null}

      <form className="panel create-form" onSubmit={(e) => e.preventDefault()} aria-label="审批人">
        <label className="field field-narrow">
          <span>审批人</span>
          <input value={actor} onChange={(e) => setActor(e.target.value)} placeholder="例如：alice" />
        </label>
      </form>

      <section className="panel" aria-labelledby="approvals-title">
        <h2 id="approvals-title">待审批</h2>
        {loading ? (
          <div className="panel loading-panel">正在读取审批…</div>
        ) : items.length === 0 ? (
          <div className="empty-state">当前没有待审批事项。</div>
        ) : (
          <div className="integration-list">
            <div className="integration-header" aria-hidden="true">
              <span>对象</span><span>风险</span><span>理由</span><span>操作</span>
            </div>
            {items.map((approval) => (
              <div className="integration-row" key={approval.id}>
                <div className="service-name">
                  {subjectLabels[approval.subjectType] ?? approval.subjectType}
                  <code className="subject-id">{approval.subjectId.slice(0, 14)}…</code>
                </div>
                <div><span className={`risk-chip risk-${approval.risk}`}>{approval.risk}</span></div>
                <div className="detail">{approval.reason || '—'}</div>
                <div className="approval-actions">
                  <button className="row-action approve" type="button" onClick={() => void decide(approval.id, 'approved')}>批准</button>
                  <button className="row-action reject" type="button" onClick={() => void decide(approval.id, 'rejected')}>拒绝</button>
                </div>
              </div>
            ))}
          </div>
        )}
      </section>
    </div>
  );
}

const subjectLabels: Record<Approval['subjectType'], string> = {
  tool_proposal: '工具提案',
  deployment: '部署',
  baseline: '基线',
  risk: '风险',
};
