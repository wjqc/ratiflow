import { useCallback, useEffect, useRef, useState } from 'react';
import { rpc, rpcErrorMessage } from '../rpc/client';
import { renderMarkdown } from '../lib/markdown';

export interface ReviewDelivery {
  artifactId: string;
  kind: string;
  title: string;
  revisionId: string;
  etag: string;
  status: string;
  content: string;
}
interface Revision { id: string; rev_no: number; etag: string; status: string }

export interface GateAcceptanceInfo {
  title: string;
  purpose: string;
  acceptance: (string | Record<string, unknown>)[];
}

/** 验收条目可读形态：字符串原样；结构化契约渲染 verifier + 关键参数（与服务端口径一致）。 */
export function acceptanceLine(item: string | Record<string, unknown>): string {
  if (typeof item === 'string') return item;
  const verifier = typeof item.verifier === 'string' ? item.verifier : 'unknown';
  const detail: Record<string, string> = {
    artifact_frozen: `交付物 ${item.artifact_kind ?? ''} 已冻结`,
    text_nonempty: `交付物 ${item.artifact_kind ?? ''} 正文非空`,
    evidence_verified: `证据 ${item.evidence_kind ?? ''} ≥ ${item.min_count ?? 1} 条已核验`,
    coverage_complete: '需求覆盖完整',
    digest_match: `摘要匹配（${item.expected_from ?? ''}）`,
    manual_confirm: `人工确认 ${item.confirmation_subject ?? ''}（角色 ${item.confirm_role ?? ''}）`,
  };
  return detail[verifier] ? `${verifier}：${detail[verifier]}` : verifier;
}

/** 本关验收标准（模板冻结定义；无实例的 legacy 任务回退 null，按通用门禁执行）。 */
export async function loadGateAcceptance(workItemId: string, gate: string): Promise<GateAcceptanceInfo | null> {
  try {
    const inst = await rpc<{ gates: { gate_id: string; title: string; purpose: string; acceptance?: (string | Record<string, unknown>)[] }[] }>('workflow.getInstance', { workItemId });
    const g = inst.gates?.find((item) => item.gate_id === gate);
    return g ? { title: g.title, purpose: g.purpose, acceptance: g.acceptance ?? [] } : null;
  } catch {
    return null;
  }
}

export async function loadGateDeliveries(workItemId: string, gate: string): Promise<ReviewDelivery[]> {
  const status = await rpc<{ requiredKind: string; requiredKinds?: string[] }>('gate.deliverableStatus', { workItemId, gate });
  const kinds = status.requiredKinds?.length ? status.requiredKinds : [status.requiredKind];
  const page = await rpc<{ items: { id: string; kind: string; title: string }[] }>('artifact.list', { workItemId });
  return Promise.all(kinds.filter(Boolean).map(async (kind) => {
    const artifact = page.items.find((item) => item.kind === kind);
    if (!artifact) return { artifactId: '', kind, title: kind, revisionId: '', etag: '', status: 'missing', content: '' };
    const revisions = await rpc<{ items: Revision[] }>('artifact.listRevisions', { artifactId: artifact.id });
    const revision = revisions.items.filter((r) => r.status !== 'superseded').sort((a, b) => b.rev_no - a.rev_no)[0];
    const content = revision ? await rpc<{ content: string }>('artifact.revisionContent', { revisionId: revision.id }) : { content: '' };
    return { artifactId: artifact.id, kind, title: artifact.title || kind, revisionId: revision?.id ?? '', etag: revision?.etag ?? '', status: revision?.status ?? 'missing', content: content.content };
  }));
}

async function assertReviewedVersion(workItemId: string, gate: string, reviewed: ReviewDelivery[]) {
  const task = await rpc<{ workItem: { current_gate: string } }>('workitem.get', { workItemId });
  if (task.workItem.current_gate !== gate) throw new Error('当前关卡已变化，请刷新后查看。');
  const current = await loadGateDeliveries(workItemId, gate);
  if (current.length !== reviewed.length || current.some((item, index) => item.revisionId !== reviewed[index].revisionId || item.etag !== reviewed[index].etag)) {
    throw new Error('交付物已有新版本，请重新打开并审阅后再决定。');
  }
  return current;
}

/** User intent is one decision; existing domain RPCs still enforce all release checks. */
export async function decideGateDelivery(workItemId: string, gate: string, reviewed: ReviewDelivery[], decision: 'approve' | 'reject', comment: string) {
  if (!reviewed.length || reviewed.some((item) => !item.revisionId || !item.content.trim())) throw new Error('本关交付物尚未完整生成，暂时无法评审。');
  if (decision === 'reject' && !comment.trim()) throw new Error('请填写需要修改的问题，作为打回评语。');
  const current = await assertReviewedVersion(workItemId, gate, reviewed);
  const reason = comment.trim() || '已审阅本关全部交付物，同意通过并进入下一关。';
  const reviewer = 'local-user';
  const pkg = await rpc<{ releaseRequests: { state: string; approval_id?: string }[] }>('stage.package', { workItemId, gate });
  const pending = pkg.releaseRequests.find((item) => item.state === 'pending' && item.approval_id);
  if (decision === 'reject') {
    // Cancel an already prepared release before changing its reviewed revisions.
    if (pending) await rpc('gate.decideRelease', { approvalId: pending.approval_id, decision: 'changes_requested', decidedBy: reviewer, reason });
    for (const item of current) {
      await rpc('artifact.addReview', { revisionId: item.revisionId, reviewer, verdict: 'changes_requested', comment: reason });
      // Frozen revisions remain immutable; create a draft for the requested rework.
      if (item.status === 'frozen') {
        const coverage = await rpc<{ items: { requirementKey: string; status: string }[] }>('trace.coverage', { workItemId });
        await rpc('artifact.createDraft', { artifactId: item.artifactId, content: item.content, requirementKeys: coverage.items.filter((entry) => entry.status === 'active').map((entry) => entry.requirementKey) });
      }
    }
    return;
  }
  if (!pending) {
    const unfrozen = current.filter((item) => item.status !== 'frozen');
    if (unfrozen.length && unfrozen.length !== current.length) throw new Error('本关交付物的冻结状态不一致，请先打回修改，统一新版本后重新审阅。');
    for (const item of unfrozen) {
      await rpc('artifact.addReview', { revisionId: item.revisionId, reviewer, verdict: 'approved', comment: reason });
    }
    if (unfrozen.length) await rpc('artifact.freezeBaseline', { workItemId, gate, revisionIds: unfrozen.map((item) => item.revisionId) });
    // Reuse an evidence record after a partially completed attempt instead of duplicating it.
    const evidenceTitle = `交付物审阅通过：${current.map((item) => item.revisionId).join(',')}`;
    const evidenceList = await rpc<{ items: { id: string; title: string }[] }>('evidence.list', { workItemId, gate });
    let evidence = evidenceList.items.find((item) => item.title === evidenceTitle);
    if (!evidence) {
      const coverage = await rpc<{ items: { requirementKey: string; status: string }[] }>('trace.coverage', { workItemId });
      evidence = await rpc<{ id: string; title: string }>('evidence.record', { workItemId, gate, kind: 'review', title: evidenceTitle, source: 'local', requirementKeys: coverage.items.filter((item) => item.status === 'active').map((item) => item.requirementKey) });
    }
    await rpc('evidence.verify', { evidenceId: evidence.id, verifiedBy: reviewer });
    const evaluation = await rpc<{ passed: boolean; failed_inputs: string[] }>('gate.evaluate', { workItemId, gate });
    if (!evaluation.passed) {
      const labels: Record<string, string> = {
        required_artifacts_frozen: '交付物尚未全部就绪', required_checks_passed: '自动检查未通过',
        approvals_valid: '存在未完成的必要授权', evidence_complete: '核验材料不完整',
        no_blocking_risk: '存在未解决的阻塞风险', inputs_current: '上游输入已变化',
      };
      throw new Error(`暂时无法进入下一关，仍有检查未通过：${evaluation.failed_inputs.map((key) => labels[key] || key).join('、')}`);
    }
  }
  const release = pending ?? await rpc<{ approval_id?: string }>('gate.requestRelease', { workItemId, gate });
  if (!release.approval_id) throw new Error('未取得有效的放行请求，请刷新后重试。');
  await assertReviewedVersion(workItemId, gate, reviewed);
  await rpc('gate.decideRelease', { approvalId: release.approval_id, decision: 'approved', decidedBy: reviewer, reason });
}

export default function GateDeliveryReview({ workItemId, gate, onDone, onRevise }: { workItemId: string; gate: string; onDone: () => void; onRevise: () => void }) {
  const [deliveries, setDeliveries] = useState<ReviewDelivery[]>([]);
  const [acceptance, setAcceptance] = useState<GateAcceptanceInfo | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [comment, setComment] = useState('');
  const [notice, setNotice] = useState('');
  const actionLock = useRef(false);
  const reload = useCallback(async () => {
    setLoading(true);
    try {
      const [items, gateAcceptance] = await Promise.all([
        loadGateDeliveries(workItemId, gate),
        loadGateAcceptance(workItemId, gate),
      ]);
      setDeliveries(items);
      setAcceptance(gateAcceptance);
      setError('');
    } catch (reason) { setError(rpcErrorMessage(reason)); }
    finally { setLoading(false); }
  }, [workItemId, gate]);
  useEffect(() => { void reload(); }, [reload]);
  const decide = async (decision: 'approve' | 'reject') => {
    if (actionLock.current) return;
    actionLock.current = true; setBusy(true); setError(''); setNotice('');
    try {
      await decideGateDelivery(workItemId, gate, deliveries, decision, comment);
      setNotice(decision === 'reject' ? '已保存评语并打回修改，任务保留在当前关。' : '已通过，正在进入下一关。');
      onDone();
      if (decision === 'reject') await reload();
    } catch (reason) { setError(rpcErrorMessage(reason)); }
    finally { actionLock.current = false; setBusy(false); }
  };
  const ready = !loading && deliveries.length > 0 && deliveries.every((item) => item.revisionId && item.content.trim());
  return <div className="sg-delivery-review">
    {acceptance ? <section aria-label="验收标准核对清单" className="sg-delivery-acceptance">
      <h3>验收标准（逐条核对{acceptance.title ? ` · ${acceptance.title}` : ''}）</h3>
      {acceptance.purpose ? <p className="sg-muted">本关目标：{acceptance.purpose}</p> : null}
      {acceptance.acceptance.length ? (
        <ol>
          {acceptance.acceptance.map((item, index) => <li key={index}>{acceptanceLine(item)}</li>)}
        </ol>
      ) : <p className="sg-muted">本关无模板级验收条目，按通用门禁六输入执行。</p>}
      <p className="sg-muted">通过前请逐条核对下方交付物；遗漏、冲突或偏离请在评语中明确列出。</p>
    </section> : null}
    {loading ? <p role="status">正在加载本关交付物…</p> : deliveries.map((item) => <section key={item.kind} aria-label={`交付物 ${item.title}`}>
      <h3>{item.title}</h3>
      {item.content ? <div className="sg-md-body" dangerouslySetInnerHTML={{ __html: renderMarkdown(item.content) }} /> : <p className="sg-muted">尚未生成可审阅的交付物，请先返回对话完成本关产出。</p>}
    </section>)}
    {!loading && !deliveries.length ? <p>本关尚无可审阅的交付物。</p> : null}
    <label className="sg-field"><span>评语（拒绝时必填）</span><textarea className="sg-textarea" rows={3} disabled={busy} value={comment} onChange={(event) => setComment(event.target.value)} placeholder="指出需要修改的问题、预期结果和验收要求…" /></label>
    {error ? <div className="sg-banner sg-banner--error" role="alert">{error}<button className="sg-link-btn" disabled={busy} onClick={() => void reload()}>重新加载交付物</button></div> : null}
    {notice ? <p role="status">{notice}</p> : null}
    <div className="sg-row">
      <button className="sg-btn" disabled={busy} onClick={onRevise}>返回对话修改</button>
      <button className="sg-btn sg-btn--danger" disabled={busy || !ready || !comment.trim()} onClick={() => void decide('reject')}>拒绝并打回</button>
      <button className="sg-btn sg-btn--primary" disabled={busy || !ready} onClick={() => void decide('approve')}>{busy ? '正在处理…' : '通过并进入下一关'}</button>
    </div>
  </div>;
}
