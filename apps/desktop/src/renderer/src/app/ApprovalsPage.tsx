import { useCallback, useEffect, useMemo, useState } from 'react';
import { rpc, rpcErrorMessage } from '../rpc/client';
import { IconAlert, IconCheck, IconShield } from '../components/Icons';

interface ApprovalInfo {
  id: string; subject_type: string; subject_id: string; risk: string;
  status: string; reason: string; expires_at: string;
}

interface Props { onDecided: () => void }

type Tab = 'pending' | 'decided' | 'expired';

const TAB_LABELS: Record<Tab, string> = { pending: '待处理', decided: '已决定', expired: '已过期' };

// 审批中心（规范 §4.5，原型 05）：ActionDigest 绑定；批准/拒绝都要求可审计理由。
// 左列表 + 右详情；不虚构步骤/影响面等后端未返回的字段。
export default function ApprovalsPage({ onDecided }: Props) {
  const [items, setItems] = useState<ApprovalInfo[]>([]);
  const [tab, setTab] = useState<Tab>('pending');
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [actor, setActor] = useState('');
  const [reason, setReason] = useState('');
  const [confirmed, setConfirmed] = useState(false);
  const [error, setError] = useState('');

  const reload = useCallback(async () => {
    try {
      const page = await rpc<{ items: ApprovalInfo[] }>('approval.list', { limit: 50 });
      setItems(page.items);
    } catch (reason_) {
      setError(rpcErrorMessage(reason_));
    }
  }, []);

  // F02 事件驱动刷新：审批/任务事件即时拉取；30s 轮询仅作断线降级。
  useEffect(() => {
    const off = window.sixgates.onEvent((e) => {
      if (e.type.startsWith('approval.') || e.type.startsWith('run.')) void reload();
    });
    const t = setInterval(() => void reload(), 30000);
    return () => {
      off();
      clearInterval(t);
    };
  }, [reload]);

  const bucketOf = useCallback((a: ApprovalInfo): Tab => {
    if (a.status === 'approved' || a.status === 'rejected') return 'decided';
    if (a.status === 'expired' || (a.expires_at && new Date(a.expires_at).getTime() < Date.now())) return 'expired';
    return 'pending';
  }, []);

  const counts = useMemo(() => {
    const c: Record<Tab, number> = { pending: 0, decided: 0, expired: 0 };
    for (const a of items) c[bucketOf(a)] += 1;
    return c;
  }, [items, bucketOf]);

  const visible = items.filter((a) => bucketOf(a) === tab);
  const selected = visible.find((a) => a.id === selectedId) ?? visible[0] ?? null;

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
      setConfirmed(false);
      await reload();
      onDecided();
    } catch (reason_) {
      setError(rpcErrorMessage(reason_));
    }
  };

  return (
    <>
      <header className="sg-page-head">
        <span className="sg-page-head-title">审批中心</span>
        <span className="sg-page-head-status">本地运行</span>
      </header>

      <div className="sg-ap-layout">
        <div className="sg-ap-list">
          <div className="sg-ap-tabs" role="tablist">
            {(Object.keys(TAB_LABELS) as Tab[]).map((key) => (
              <button
                key={key}
                role="tab"
                aria-selected={tab === key}
                className={`sg-ap-tab ${tab === key ? 'sg-ap-tab--active' : ''}`}
                onClick={() => { setTab(key); setSelectedId(null); }}
              >
                {TAB_LABELS[key]}{key === 'pending' && counts.pending > 0 ? ` ${counts.pending}` : ''}
              </button>
            ))}
          </div>
          <div className="sg-ap-items">
            {visible.length === 0 ? (
              <div className="sg-empty" style={{ padding: '28px 16px' }}>
                {tab === 'pending' ? '当前没有待审批事项 ✓' : '暂无记录'}
              </div>
            ) : visible.map((approval) => (
              <button
                key={approval.id}
                className={`sg-ap-item ${selected?.id === approval.id ? 'sg-ap-item--active' : ''}`}
                onClick={() => setSelectedId(approval.id)}
              >
                <span className="sg-ap-item-title">
                  {approval.reason || subjectLabel(approval.subject_type)}
                  <span className={`sg-chip ${approval.risk === 'high' ? 'sg-chip--danger' : ''}`}>
                    {approval.risk === 'high' ? '高风险' : approval.risk || '中风险'}
                  </span>
                </span>
                <span className="sg-ap-item-sub">
                  {subjectLabel(approval.subject_type)} · {approval.subject_id.slice(0, 12)}…
                </span>
                <span className="sg-ap-item-sub">
                  {approval.expires_at
                    ? `有效期至 ${new Date(approval.expires_at).toLocaleTimeString('zh-CN', { hour12: false })}`
                    : '—'}
                </span>
              </button>
            ))}
          </div>
        </div>

        <div className="sg-ap-detail">
          {error ? <div className="sg-banner sg-banner--error" role="alert" style={{ marginBottom: 12 }}>{error}</div> : null}
          {!selected ? (
            <div className="sg-empty" style={{ padding: '60px 24px' }}>
              <IconShield size={32} style={{ color: 'var(--sg-border-strong)' }} />
              <span>选择左侧一条审批查看详情</span>
            </div>
          ) : (
            <>
              <h2 className="sg-ap-detail-title">{selected.reason || subjectLabel(selected.subject_type)}</h2>
              <div className="sg-ap-detail-meta">
                <span>
                  状态：
                  <span className={`sg-status ${selected.status === 'approved' ? 'sg-status--approved' : selected.status === 'rejected' ? 'sg-status--rejected' : 'sg-status--awaiting_approval'}`}>
                    {statusLabel(selected.status)}
                  </span>
                </span>
                <span>风险：{selected.risk === 'high' ? '高' : selected.risk || '中'}</span>
                <span>
                  有效期至：{selected.expires_at ? new Date(selected.expires_at).toLocaleString('zh-CN') : '—'}
                </span>
              </div>

              <div className="sg-ap-block">
                <div className="sg-ap-block-label">动作目标</div>
                <dl className="sg-kv">
                  <dt>类型</dt>
                  <dd>{subjectLabel(selected.subject_type)}</dd>
                  <dt>对象</dt>
                  <dd className="sg-code">{selected.subject_id}</dd>
                </dl>
              </div>

              <div className="sg-ap-block">
                <div className="sg-ap-block-label">ActionDigest</div>
                <div className="sg-code sg-muted" style={{ wordBreak: 'break-all' }}>
                  {selected.subject_id}
                </div>
              </div>

              {selected.reason ? (
                <div className="sg-ap-block">
                  <div className="sg-ap-block-label">申请理由</div>
                  <p style={{ margin: 0, fontSize: 13, lineHeight: 1.7 }}>{selected.reason}</p>
                </div>
              ) : null}

              <div className="sg-banner sg-banner--warning">
                <IconAlert size={14} style={{ flexShrink: 0, marginTop: 2 }} />
                <span>参数、目标或 digest 改变后，本次批准立即失效。批准仅对本机本次操作生效。</span>
              </div>

              {bucketOf(selected) === 'pending' && (
                <>
                  <div className="sg-row" style={{ marginTop: 16 }}>
                    <label className="sg-field" style={{ width: 180, marginBottom: 0 }}>
                      <span>审批人 *</span>
                      <input className="sg-input" value={actor} onChange={(e) => setActor(e.target.value)} placeholder="alice" />
                    </label>
                    <label className="sg-field" style={{ flex: 1, marginBottom: 0 }}>
                      <span>理由（必填，进入审计）*</span>
                      <input className="sg-input" value={reason} onChange={(e) => setReason(e.target.value)} placeholder="确认发布 / 参数有误…" />
                    </label>
                  </div>

                  <label className="sg-ap-confirm">
                    <input
                      type="checkbox"
                      checked={confirmed}
                      onChange={(e) => setConfirmed(e.target.checked)}
                    />
                    我已确认上述信息准确，且本次操作符合变更管理规范与风险控制要求。
                  </label>

                  <div className="sg-ap-actions">
                    <button
                      className="sg-btn sg-btn--danger"
                      disabled={!confirmed}
                      onClick={() => void decide(selected.id, 'rejected')}
                    >
                      拒绝
                    </button>
                    <button
                      className="sg-btn sg-btn--primary"
                      disabled={!confirmed}
                      onClick={() => void decide(selected.id, 'approved')}
                    >
                      <IconCheck size={13} />
                      批准
                    </button>
                  </div>
                </>
              )}
            </>
          )}
        </div>
      </div>
    </>
  );
}

function subjectLabel(type: string): string {
  if (type === 'deployment') return '部署';
  if (type === 'tool_proposal') return '工具提案';
  return type;
}

function statusLabel(status: string): string {
  if (status === 'approved') return '已批准';
  if (status === 'rejected') return '已拒绝';
  if (status === 'expired') return '已过期';
  return '等待审批';
}
