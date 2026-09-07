import { useCallback, useEffect, useMemo, useState } from 'react';
import { rpc, rpcErrorMessage } from '../rpc/client';
import { renderMarkdown } from '../lib/markdown';
import { IconAlert, IconCheck, IconShield } from '../components/Icons';

interface ApprovalInfo {
  id: string; subject_type: string; subject_id: string; risk: string;
  status: string; reason: string; expires_at: string; action_digest?: string;
  // WP-6 三要素（tool_proposal 审批才有；缺省如实留空）。
  rationale?: string;
  confidence?: number | null;
  impactCompleteness?: 'complete' | 'incomplete' | 'unknown' | string;
  impactNodeCount?: number;
}

interface ReleaseDetail {
  releaseRequest: { id: string; release_digest: string; state: string };
  attempt?: { gate: string; attempt_no: number; state: string; workitem_id?: string };
  package?: { digest: string; packageNo: number; coverage?: { totalItems: number; coveredCount: number; verifiedCount: number } };
  items?: { role: string; nodeType: string; entityId: string }[];
}

interface Props { onDecided: () => void }

type Tab = 'pending' | 'decided' | 'expired';

const TAB_LABELS: Record<Tab, string> = { pending: '待处理', decided: '已决定', expired: '已过期' };

// 审批中心（规范 §4.5，原型 05）：ActionDigest 绑定；批准/拒绝都要求可审计理由。
// 左列表 + 右详情；工具提案展示 WP-6 三要素（影响面 completeness 服务端权威 /
// 模型理由与置信度=模型自报 untrusted_display，不参与判定）。
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

  // F02 事件驱动刷新：挂载首拉 + 审批/任务事件即时拉取；30s 轮询仅作断线降级。
  useEffect(() => {
    void reload();
    const off = window.ratiflow.onEvent((e) => {
      if (e.type.startsWith('approval.') || e.type.startsWith('run.')) void reload();
    });
    const t = setInterval(() => void reload(), 30000);
    return () => {
      off();
      clearInterval(t);
    };
  }, [reload]);

  const bucketOf = useCallback((a: ApprovalInfo): Tab => {
    if (a.status === 'approved' || a.status === 'rejected' || a.status === 'changes_requested') return 'decided';
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

  const [releaseDetail, setReleaseDetail] = useState<ReleaseDetail | null>(null);
  // 审批人要看的交付内容：该任务各工件（PRD/技术方案…）的当前修订全文。
  const [deliverables, setDeliverables] = useState<
    Array<{ id: string; label: string; content: string }>
  >([]);
  useEffect(() => {
    let alive = true;
    setReleaseDetail(null);
    if (selected?.subject_type !== 'gate_release') {
      if (alive) setDeliverables([]);
      return;
    }
    void rpc<ReleaseDetail>('gate.getRelease', { releaseId: selected.subject_id })
      .then(async (d) => {
        if (alive) setReleaseDetail(d);
        if (!alive) return;
        // 拉取该任务各工件的当前修订全文，作为审批人要看的交付内容。
        const workitemId = d.attempt?.workitem_id;
        if (!workitemId) return;
        try {
          const arts = await rpc<{
            items: Array<{ id: string; kind: string; title: string }>;
          }>('artifact.list', { workItemId: workitemId });
          const KINDS: Record<string, string> = {
            prd: 'PRD', tech_design: '技术方案', code: '代码产出',
            test: '测试产出', deployment: '部署产物', verification: '验收产出',
          };
          const docs: Array<{ id: string; label: string; content: string }> = [];
          for (const a of arts.items ?? []) {
            try {
              const revs = await rpc<{ items: Array<{ id: string; status: string }> }>(
                'artifact.listRevisions',
                { artifactId: a.id },
              );
              const cur = (revs.items ?? []).find((r) => r.status !== 'superseded') ?? (revs.items ?? [])[0];
              if (!cur) continue;
              const body = await rpc<{ content: string }>('artifact.revisionContent', {
                revisionId: cur.id,
              });
              docs.push({
                id: a.id,
                label: KINDS[a.kind] ?? a.title ?? a.kind,
                content: body.content ?? '',
              });
            } catch {
              /* 单个工件读取失败跳过 */
            }
          }
          if (alive) setDeliverables(docs);
        } catch {
          if (alive) setDeliverables([]);
        }
      })
      .catch(() => {});
    return () => { alive = false; };
  }, [selected]);

  const decide = async (approvalId: string, decision: 'approved' | 'rejected' | 'changes_requested') => {
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
      if (selected?.subject_type === 'gate_release') {
        // M2：关卡放行经独立放行事务（evaluate 只计算，推进只在这里原子发生）。
        await rpc('gate.decideRelease', { approvalId, decision, decidedBy: actor.trim(), reason: reason.trim() });
      } else {
        await rpc('approval.decide', { approvalId, decision, decidedBy: actor.trim(), reason: reason.trim() });
      }
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

              {selected.subject_type === 'gate_release' && releaseDetail ? (
                <div className="sg-ap-block">
                  <div className="sg-ap-block-label">放行详情</div>
                  <dl className="sg-kv">
                    <dt>关卡</dt>
                    <dd>{releaseDetail.attempt?.gate ?? '—'}（第 {releaseDetail.attempt?.attempt_no ?? '—'} 次尝试）</dd>
                    <dt>需求覆盖</dt>
                    <dd>
                      {releaseDetail.package?.coverage
                        ? `${releaseDetail.package.coverage.coveredCount}/${releaseDetail.package.coverage.totalItems} 已实现 · ${releaseDetail.package.coverage.verifiedCount}/${releaseDetail.package.coverage.totalItems} 有测试证据`
                        : '—'}
                    </dd>
                  </dl>
                  <p className="sg-muted" style={{ margin: '6px 0 0', fontSize: 12 }}>
                    批准后当前关标记通过并进入下一关；输出在此期间变化会使本审批自动失效。
                  </p>
                </div>
              ) : null}

              {selected.subject_type === 'gate_release' && deliverables.length > 0 ? (
                <div className="sg-ap-block">
                  <div className="sg-ap-block-label">交付内容（进入下一阶段的文档）</div>
                  {deliverables.map((d) => (
                    <div key={d.id} style={{ marginTop: 10 }}>
                      <div style={{ fontSize: 12.5, fontWeight: 600, marginBottom: 6 }}>{d.label}</div>
                      <div
                        className="sg-md-body"
                        style={{
                          border: '1px solid var(--sg-border-default)',
                          borderRadius: 8,
                          padding: '12px 14px',
                          maxHeight: 320,
                          overflowY: 'auto',
                        }}
                        dangerouslySetInnerHTML={{ __html: renderMarkdown(d.content) }}
                      />
                    </div>
                  ))}
                </div>
              ) : null}

              {selected.subject_type === 'tool_proposal' ? (
                <div className="sg-ap-block">
                  <div className="sg-ap-block-label">工具提案依据（服务端权威）</div>
                  <dl className="sg-kv">
                    <dt>影响面</dt>
                    <dd>
                      {selected.impactCompleteness ? (
                        <>
                          <span
                            className={`sg-chip ${selected.impactCompleteness === 'complete' ? '' : 'sg-chip--danger'}`}
                            data-testid="impact-completeness"
                          >
                            {impactLabel(selected.impactCompleteness)}
                          </span>
                          {typeof selected.impactNodeCount === 'number'
                            ? ` · ${selected.impactNodeCount} 个关联节点`
                            : ''}
                          <span className="sg-muted" style={{ marginLeft: 8, fontSize: 12 }}>
                            审批等待期间影响面漂移将使本审批自动失效
                          </span>
                        </>
                      ) : (
                        <span className="sg-muted">未绑定（旧版审批）</span>
                      )}
                    </dd>
                    {selected.rationale ? (
                      <>
                        <dt>模型理由</dt>
                        <dd data-testid="model-rationale">
                          {selected.rationale}
                          <span className="sg-muted" style={{ marginLeft: 6, fontSize: 12 }}>（模型自报）</span>
                        </dd>
                      </>
                    ) : null}
                    {selected.confidence != null ? (
                      <>
                        <dt>置信度</dt>
                        <dd data-testid="model-confidence">
                          {formatConfidence(selected.confidence)}
                          <span className="sg-muted" style={{ marginLeft: 6, fontSize: 12 }}>
                            （模型自报，仅供参考——不参与自动判定）
                          </span>
                        </dd>
                      </>
                    ) : null}
                  </dl>
                </div>
              ) : null}

              <div className="sg-ap-block">
                <div className="sg-ap-block-label">申请理由</div>
                <p style={{ margin: 0, fontSize: 13, lineHeight: 1.7 }}>{selected.reason}</p>
              </div>

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
                    {selected.subject_type === 'gate_release' ? (
                      <button
                        className="sg-btn"
                        disabled={!confirmed}
                        onClick={() => void decide(selected.id, 'changes_requested')}
                      >
                        要求修改
                      </button>
                    ) : null}
                    <button
                      className="sg-btn sg-btn--primary"
                      disabled={!confirmed}
                      onClick={() => void decide(selected.id, 'approved')}
                    >
                      <IconCheck size={13} />
                      {selected.subject_type === 'gate_release' ? '批准并进入下一关' : '批准'}
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
  if (type === 'gate_release') return '关卡放行';
  if (type === 'rollback') return '回滚';
  if (type === 'baseline') return '基线';
  if (type === 'risk') return '风险';
  return type;
}

function impactLabel(completeness: string): string {
  if (completeness === 'complete') return '全图可达';
  if (completeness === 'incomplete') return '影响面过大（已截断）';
  return '无谱系数据';
}

function formatConfidence(v: number): string {
  if (v >= 0 && v <= 1) return `${Math.round(v * 100)}%`;
  return String(v);
}

function statusLabel(status: string): string {
  if (status === 'approved') return '已批准';
  if (status === 'rejected') return '已拒绝';
  if (status === 'expired') return '已过期';
  if (status === 'changes_requested') return '已要求修改';
  return '等待审批';
}
