import { useCallback, useEffect, useState } from 'react';
import { rpc, rpcErrorMessage } from '../rpc/client';

interface ApprovalInfo {
  id: string; subject_type: string; subject_id: string; risk: string;
  status: string; reason: string; expires_at: string;
}

interface Props { onDecided: () => void }

// 审批中心（规范 §4.5）：ActionDigest 绑定；批准/拒绝都要求可审计理由。
export default function ApprovalsPage({ onDecided }: Props) {
  const [items, setItems] = useState<ApprovalInfo[]>([]);
  const [actor, setActor] = useState('');
  const [reason, setReason] = useState('');
  const [error, setError] = useState('');

  const reload = useCallback(async () => {
    try {
      const page = await rpc<{ items: ApprovalInfo[] }>('approval.list', { limit: 50 });
      setItems(page.items);
    } catch (reason_) {
      setError(rpcErrorMessage(reason_));
    }
  }, []);

  useEffect(() => { void reload(); const t = setInterval(() => void reload(), 4000); return () => clearInterval(t); }, [reload]);

  const decide = async (approvalId: string, decision: 'approved' | 'rejected') => {
    if (!actor.trim()) {
      setError('请先填写审批人标识');
      return;
    }
    if (reason.trim().length < 2) {
      setError('批准与拒绝都必须填写可审计理由');
      return;
    }
    setError('');
    try {
      await rpc('approval.decide', { approvalId, decision, decidedBy: actor.trim(), reason: reason.trim() });
      setReason('');
      await reload();
      onDecided();
    } catch (reason_) {
      setError(rpcErrorMessage(reason_));
    }
  };

  return (
    <div className="sg-section" style={{ maxWidth: 860 }}>
      <h1 style={{ fontSize: 18, margin: 0 }}>审批中心</h1>
      <p className="sg-muted">高风险动作默认拒绝直到本机确认；批准绑定 ActionDigest，参数变化即失效。</p>
      {error ? <div className="sg-banner sg-banner--error" style={{ marginTop: 8 }}>{error}</div> : null}

      <div className="sg-row" style={{ marginTop: 12 }}>
        <label className="sg-field" style={{ width: 180 }}>
          <span>审批人</span>
          <input className="sg-input" value={actor} onChange={(e) => setActor(e.target.value)} placeholder="alice" />
        </label>
        <label className="sg-field" style={{ flex: 1 }}>
          <span>理由（必填，进入审计）</span>
          <input className="sg-input" value={reason} onChange={(e) => setReason(e.target.value)} placeholder="确认发布 / 参数有误…" />
        </label>
      </div>

      <table className="sg-table" style={{ marginTop: 14 }}>
        <thead>
          <tr><th>对象</th><th>风险</th><th>理由</th><th>有效期至</th><th>操作</th></tr>
        </thead>
        <tbody>
          {items.length === 0 ? (
            <tr><td colSpan={5} className="sg-muted" style={{ textAlign: 'center', padding: 20 }}>当前没有待审批事项 ✓</td></tr>
          ) : items.map((approval) => (
            <tr key={approval.id}>
              <td>
                {subjectLabel(approval.subject_type)}
                <br /><span className="sg-code">{approval.subject_id.slice(0, 16)}…</span>
              </td>
              <td>
                <span className={`sg-status sg-status--${approval.risk === 'high' ? 'error' : 'running'}`}>
                  {approval.risk === 'high' ? '⚠ 高风险' : approval.risk}
                </span>
              </td>
              <td className="sg-muted">{approval.reason || '—'}</td>
              <td className="sg-muted">{new Date(approval.expires_at).toLocaleTimeString('zh-CN', { hour12: false })}</td>
              <td>
                <div className="sg-row">
                  <button className="sg-button sg-button--primary" onClick={() => void decide(approval.id, 'approved')}>批准</button>
                  <button className="sg-button sg-button--danger" onClick={() => void decide(approval.id, 'rejected')}>拒绝</button>
                </div>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function subjectLabel(type: string): string {
  if (type === 'deployment') return '部署';
  if (type === 'tool_proposal') return '工具提案';
  return type;
}
