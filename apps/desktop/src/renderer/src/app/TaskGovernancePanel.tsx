import { useCallback, useEffect, useState } from 'react';
import { rpc, rpcErrorMessage } from '../rpc/client';

type Stage = { gate: string; state: string };
type PlanRevision = { id: string; revision_no?: number; status: string; digest?: string };
type Snapshot = { id: string; kind: string; created_at: string; root_digest: string };

function idempotencyKey(action: string): string {
  return `ui-${action}-${crypto.randomUUID()}`;
}

export default function TaskGovernancePanel({
  workItemId,
  currentGate,
  stages,
  onChanged,
}: {
  workItemId: string;
  currentGate: string;
  stages: Stage[];
  onChanged: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [plans, setPlans] = useState<PlanRevision[]>([]);
  const [snapshots, setSnapshots] = useState<Snapshot[]>([]);
  const [attemptId, setAttemptId] = useState('');
  const [targetGate, setTargetGate] = useState(currentGate);
  const [reason, setReason] = useState('');
  const [taskJson, setTaskJson] = useState('[\n  {\n    "taskKey": "task-1",\n    "kind": "analysis",\n    "title": "分析并交付当前关产物",\n    "expectedOutputs": ["交付物"],\n    "acceptance": { "machine": [], "manual": ["人工复核"] },\n    "effectClass": "read",\n    "deps": []\n  }\n]');
  const [replanRoots, setReplanRoots] = useState('');
  const [manualElement, setManualElement] = useState('');
  const [substituteEvidenceId, setSubstituteEvidenceId] = useState('');
  const [waiverId, setWaiverId] = useState('');
  const [confirmations, setConfirmations] = useState<Array<{ id: string; state: string; approval_status: string }>>([]);
  const [busy, setBusy] = useState('');
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');

  const reload = useCallback(async () => {
    const [planResult, snapshotResult, attemptResult, confirmationResult] = await Promise.all([
      rpc<{ items: PlanRevision[] }>('plan.list', { workItemId }).catch(() => ({ items: [] })),
      rpc<{ items: Snapshot[] }>('snapshot.list', { workItemId }).catch(() => ({ items: [] })),
      rpc<{ items: Array<{ id: string; gate: string }> }>('stage.attempts', { workItemId }).catch(() => ({ items: [] })),
      rpc<{ items: Array<{ id: string; state: string; approval_status: string }> }>('gate.manualConfirmations', { workItemId, gate: currentGate }).catch(() => ({ items: [] })),
    ]);
    setPlans(planResult.items ?? []);
    setSnapshots(snapshotResult.items ?? []);
    const current = (attemptResult.items ?? []).find((item) => item.gate === currentGate);
    setAttemptId(current?.id ?? attemptResult.items?.[0]?.id ?? '');
    setConfirmations(confirmationResult.items ?? []);
  }, [currentGate, workItemId]);

  useEffect(() => {
    setTargetGate(currentGate);
    if (open) void reload();
  }, [currentGate, open, reload]);

  const run = async (key: string, action: () => Promise<unknown>, success: string) => {
    setBusy(key);
    setError('');
    setNotice('');
    try {
      await action();
      setNotice(success);
      await reload();
      onChanged();
    } catch (cause) {
      setError(rpcErrorMessage(cause));
    } finally {
      setBusy('');
    }
  };

  const requestRework = async () => {
    await rpc('rework.preview', {
      workItemId, targetGate, reasonCode: 'other', note: reason, requestedBy: 'local-user',
    });
    return rpc('rework.request', {
      workItemId, targetGate, reasonCode: 'other', note: reason, requestedBy: 'local-user',
      idempotencyKey: idempotencyKey('rework'),
    });
  };

  const createPlan = async () => {
    if (!attemptId) throw new Error('当前任务还没有阶段 attempt，先启动本关活动后再创建计划');
    const tasks = JSON.parse(taskJson) as unknown[];
    if (!Array.isArray(tasks) || tasks.length === 0) throw new Error('计划至少需要一个任务');
    return rpc('plan.createDraft', {
      workItemId, stageAttemptId: attemptId, tasks, idempotencyKey: idempotencyKey('plan-draft'),
    });
  };

  return (
    <div className="sg-card" data-testid="task-governance-panel" style={{ marginBottom: 12 }}>
      <div className="sg-card-head">
        治理操作
        <button type="button" className="sg-btn sg-btn--sm" style={{ marginLeft: 12 }} onClick={() => setOpen((value) => !value)}>
          {open ? '收起' : '展开'}
        </button>
      </div>
      {open ? (
        <div style={{ padding: 12, display: 'grid', gap: 14 }}>
          {error ? <div className="sg-banner sg-banner--error" role="alert">{error}</div> : null}
          {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

          <section style={{ display: 'grid', gap: 8 }} aria-label="关卡治理">
            <strong>关卡治理</strong>
            <div className="sg-row" style={{ flexWrap: 'wrap' }}>
              <select className="sg-input" aria-label="目标关卡" value={targetGate} onChange={(e) => setTargetGate(e.target.value)}>
                {stages.map((stage) => <option key={stage.gate} value={stage.gate}>{stage.gate}</option>)}
              </select>
              <input className="sg-input" aria-label="治理理由" value={reason} onChange={(e) => setReason(e.target.value)} placeholder="填写可审计理由" />
              <button className="sg-btn" disabled={!!busy || reason.trim().length < 2} onClick={() => void run('skip', () => rpc('gate.requestSkip', {
                workItemId, gateId: currentGate, waiver: reason.trim(), substituteEvidenceIds: [],
                idempotencyKey: idempotencyKey('skip'),
              }), '跳关申请已提交审批')}>申请跳过当前关</button>
              <button className="sg-btn" disabled={!!busy} onClick={() => void run('fast-track', () => rpc('gate.evaluateFastTrack', {
                workItemId, gate: currentGate, idempotencyKey: idempotencyKey('fast-track'),
              }), '快通道评估完成，建议已进入治理队列')}>评估快通道</button>
              <button className="sg-btn" disabled={!!busy || reason.trim().length < 2 || targetGate === currentGate} onClick={() => void run('rework', requestRework, '返工申请已提交审批')}>申请返工到所选关</button>
            </div>
            <div className="sg-row" style={{ flexWrap: 'wrap' }}>
              <input className="sg-input" aria-label="人工确认验收项 JSON" value={manualElement} onChange={(e) => setManualElement(e.target.value)} placeholder='人工验收项 JSON，例如 {"verifier":"manual_confirm",…}' />
              <button className="sg-btn" disabled={!!busy || !manualElement.trim() || reason.trim().length < 2} onClick={() => void run('manual-confirm', () => rpc('gate.requestManualConfirmation', { workItemId, gate: currentGate, element: JSON.parse(manualElement), requestedBy: 'local-user', reason: reason.trim(), idempotencyKey: idempotencyKey('manual-confirm') }), '人工确认已提交审批')}>申请人工确认</button>
              <span className="sg-muted">已有确认 {confirmations.length}</span>
            </div>
            <div className="sg-row" style={{ flexWrap: 'wrap' }}>
              <input className="sg-input" aria-label="替代证据 ID" value={substituteEvidenceId} onChange={(e) => setSubstituteEvidenceId(e.target.value)} placeholder="替代证据 ID" />
              <button className="sg-btn" disabled={!!busy || !substituteEvidenceId || reason.trim().length < 2} onClick={() => void run('apply-waiver', () => rpc('gate.applyWaiver', { workItemId, gate: currentGate, waivedKind: 'deliverable', substituteEvidenceId, rationale: reason.trim(), decidedBy: 'local-user', idempotencyKey: idempotencyKey('apply-waiver') }), '豁免已应用')}>应用交付物豁免</button>
              <input className="sg-input" aria-label="豁免 ID" value={waiverId} onChange={(e) => setWaiverId(e.target.value)} placeholder="待撤销豁免 ID" />
              <button className="sg-btn sg-btn--danger" disabled={!!busy || !waiverId || reason.trim().length < 2} onClick={() => void run('revoke-waiver', () => rpc('gate.revokeWaiver', { waiverId, reason: reason.trim(), revokedBy: 'local-user', idempotencyKey: idempotencyKey('revoke-waiver') }), '豁免已撤销')}>撤销豁免</button>
            </div>
            <small className="sg-muted">跳关、返工与快通道均由 Core 校验策略、状态与审批；特性未启用时会显示真实错误。</small>
          </section>

          <section style={{ display: 'grid', gap: 8 }} aria-label="回滚">
            <strong>快照与回滚</strong>
            {snapshots.length === 0 ? <span className="sg-muted">暂无可回滚快照。</span> : snapshots.map((snapshot) => (
              <div className="sg-row" key={snapshot.id} style={{ justifyContent: 'space-between' }}>
                <span>{snapshot.kind} · {snapshot.created_at} · <code>{snapshot.root_digest?.slice(0, 10)}</code></span>
                <button className="sg-btn sg-btn--sm" disabled={!!busy} onClick={() => void run(`rollback:${snapshot.id}`, async () => {
                  await rpc('rollback.preview', { workItemId, targetSnapshotId: snapshot.id });
                  return rpc('rollback.request', { workItemId, targetSnapshotId: snapshot.id, requestedBy: 'local-user' });
                }, '回滚影响已预览，申请已提交审批')}>预览并申请回滚</button>
              </div>
            ))}
          </section>

          <section style={{ display: 'grid', gap: 8 }} aria-label="结构化计划">
            <strong>结构化计划</strong>
            {plans.length === 0 ? <span className="sg-muted">暂无计划版本。</span> : plans.map((plan) => (
              <div className="sg-row" key={plan.id} style={{ justifyContent: 'space-between' }}>
                <span>v{plan.revision_no ?? '?'} · {plan.status} · <code>{plan.id}</code></span>
                <span className="sg-row">
                  {plan.status === 'draft' ? <button className="sg-btn sg-btn--sm" disabled={!!busy} onClick={() => void run(`submit:${plan.id}`, () => rpc('plan.submit', { planRevisionId: plan.id }), '计划已提交审批')}>提交审批</button> : null}
                  {plan.status === 'draft' ? <button className="sg-btn sg-btn--sm" disabled={!!busy} onClick={() => void run(`update:${plan.id}`, () => rpc('plan.updateDraft', { planRevisionId: plan.id, tasks: JSON.parse(taskJson), idempotencyKey: idempotencyKey('plan-update') }), '计划草稿已更新')}>保存任务</button> : null}
                  {plan.status === 'approved' ? <button className="sg-btn sg-btn--sm sg-btn--primary" disabled={!!busy} onClick={() => void run(`start:${plan.id}`, () => rpc('plan.start', { planRevisionId: plan.id, idempotencyKey: idempotencyKey('plan-start') }), '计划已启动')}>启动</button> : null}
                  {['draft', 'awaiting_approval', 'approved', 'running'].includes(plan.status) ? <button className="sg-btn sg-btn--sm" disabled={!!busy} onClick={() => void run(`cancel:${plan.id}`, () => rpc('plan.cancel', { planRevisionId: plan.id, idempotencyKey: idempotencyKey('plan-cancel') }), '计划已取消')}>取消</button> : null}
                </span>
              </div>
            ))}
            <textarea className="sg-textarea" rows={8} aria-label="计划任务 JSON" value={taskJson} onChange={(e) => setTaskJson(e.target.value)} />
            <div className="sg-row" style={{ flexWrap: 'wrap' }}>
              <button className="sg-btn sg-btn--primary" disabled={!!busy || !attemptId} onClick={() => void run('create-plan', createPlan, '计划草稿已创建')}>创建计划草稿</button>
              <input className="sg-input" aria-label="重规划根任务" value={replanRoots} onChange={(e) => setReplanRoots(e.target.value)} placeholder="重规划根任务，逗号分隔" />
              {plans[0] ? <button className="sg-btn" disabled={!!busy || !replanRoots.trim()} onClick={() => void run('replan', async () => {
                const roots = replanRoots.split(',').map((value) => value.trim()).filter(Boolean);
                await rpc('plan.replanPreview', { planRevisionId: plans[0].id, roots });
                return rpc('plan.replan', { planRevisionId: plans[0].id, roots, tasks: JSON.parse(taskJson), idempotencyKey: idempotencyKey('replan') });
              }, '局部重规划已创建新版本')}>预览并执行重规划</button> : null}
              {plans.find((plan) => plan.status === 'running') ? <button className="sg-btn" disabled={!!busy} onClick={() => { const running = plans.find((plan) => plan.status === 'running')!; void run('dispatch', () => rpc('plan.dispatchReady', { planRevisionId: running.id, maxParallel: 4 }), '就绪任务已派发'); }}>派发就绪任务</button> : null}
            </div>
          </section>
        </div>
      ) : null}
    </div>
  );
}
