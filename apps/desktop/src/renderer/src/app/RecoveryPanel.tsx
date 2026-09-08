import { useCallback, useEffect, useState } from 'react';
import { rpc, rpcErrorMessage } from '../rpc/client';
import { IconAlert } from '../components/Icons';

// P1-5（RDWS 审计 §8）：跳关/打回恢复状态面板。
// 治理边界：UI 不拥有状态推进权——只读服务器读面（rework.list / gate.skipRequests），
// 恢复入口仅发 intent（rework.resume / gate.resumeSkip，带 idempotencyKey），
// 展示的 state/progress 一律来自服务器响应与重读，unknown/blocked 如实呈现。

interface ReworkOp {
  id: string;
  from_gate: string;
  target_gate: string;
  state: string;
  progress: string;
  blocked_reason: string;
  reason_code: string;
}

interface SkipRequestView {
  skipRequestId: string;
  gate: string;
  state: string;
  progress: string;
  blockedReason: string;
  approvalState?: string;
}

/** 两步执行进度游标（skip/rework 共用语义，0052/0053）。 */
const PROGRESS_LABELS: Record<string, string> = {
  prepared: '已准备',
  stage_advanced: '阶段已推进（Step A 后）',
  step_a_committed: 'Step A 已提交',
  next_attempt_ready: '下一 attempt 就绪',
  step_b_committed: 'Step B 已提交',
  completed: '已完成',
};

function progressLabel(progress: string): string {
  return PROGRESS_LABELS[progress] ?? progress;
}

/** 需要人工恢复入口的状态：中断/阻塞/未知（服务器判定，不由 UI 推测）。 */
function needsRecovery(state: string): boolean {
  return state === 'blocked' || state === 'unknown';
}

export default function RecoveryPanel({ workItemId }: { workItemId: string }) {
  const [reworkOps, setReworkOps] = useState<ReworkOp[]>([]);
  const [skipReqs, setSkipReqs] = useState<SkipRequestView[]>([]);
  const [error, setError] = useState('');
  const [busyId, setBusyId] = useState<string | null>(null);

  const reload = useCallback(async () => {
    const [ops, skips] = await Promise.all([
      rpc<{ items: ReworkOp[] }>('rework.list', { workItemId }).catch(() => ({ items: [] })),
      rpc<{ items: SkipRequestView[] }>('gate.skipRequests', { workItemId }).catch(() => ({ items: [] })),
    ]);
    setReworkOps(ops.items ?? []);
    setSkipReqs(skips.items ?? []);
  }, [workItemId]);

  useEffect(() => {
    void reload().catch((e) => setError(rpcErrorMessage(e)));
  }, [reload]);

  /** 恢复入口：发 intent 后以服务器响应/重读刷新展示——不本地推进状态。 */
  const resume = useCallback(
    async (kind: 'rework' | 'skip', id: string) => {
      setBusyId(`${kind}:${id}`);
      setError('');
      try {
        if (kind === 'rework') {
          await rpc('rework.resume', {
            operationId: id,
            resumedBy: 'local-user',
            idempotencyKey: `ui-rework-resume-${id}-${Date.now()}`,
          });
        } else {
          await rpc('gate.resumeSkip', {
            skipRequestId: id,
            idempotencyKey: `ui-skip-resume-${id}-${Date.now()}`,
          });
        }
        await reload();
      } catch (e) {
        setError(rpcErrorMessage(e));
      } finally {
        setBusyId(null);
      }
    },
    [reload],
  );

  if (reworkOps.length === 0 && skipReqs.length === 0) return null;

  return (
    <div className="sg-card" data-testid="recovery-panel" style={{ marginBottom: 12 }}>
      <div className="sg-card-head">
        恢复状态（服务器权威）
        <span className="sg-card-extra sg-muted">跳关 / 打回操作与两步执行进度</span>
      </div>
      {error ? (
        <div className="sg-banner sg-banner--error" role="alert" style={{ margin: 8 }}>
          恢复失败：{error}
        </div>
      ) : null}
      <table className="sg-table">
        <thead>
          <tr>
            <th style={{ width: 90 }}>类型</th>
            <th>范围</th>
            <th style={{ width: 130 }}>状态</th>
            <th style={{ width: 170 }}>进度</th>
            <th style={{ width: 110 }}></th>
          </tr>
        </thead>
        <tbody>
          {reworkOps.map((op) => (
            <tr key={op.id} data-testid={`rework-row-${op.id}`}>
              <td>打回</td>
              <td>
                {op.from_gate} → {op.target_gate}
                {op.reason_code ? (
                  <span className="sg-muted" style={{ marginLeft: 6 }}>
                    （{op.reason_code}）
                  </span>
                ) : null}
                {op.blocked_reason ? (
                  <div className="sg-muted" data-testid={`rework-blocked-${op.id}`}>
                    <IconAlert size={12} /> {op.blocked_reason}
                  </div>
                ) : null}
              </td>
              <td data-testid={`rework-state-${op.id}`}>{op.state}</td>
              <td className="sg-muted" data-testid={`rework-progress-${op.id}`}>
                {progressLabel(op.progress)}
              </td>
              <td>
                {needsRecovery(op.state) ? (
                  <button
                    className="sg-btn sg-btn--sm"
                    disabled={busyId === `rework:${op.id}`}
                    onClick={() => void resume('rework', op.id)}
                  >
                    {busyId === `rework:${op.id}` ? '恢复中…' : '恢复执行'}
                  </button>
                ) : null}
              </td>
            </tr>
          ))}
          {skipReqs.map((req) => (
            <tr key={req.skipRequestId} data-testid={`skip-row-${req.skipRequestId}`}>
              <td>跳关</td>
              <td>
                关 {req.gate}
                {req.blockedReason ? (
                  <div className="sg-muted" data-testid={`skip-blocked-${req.skipRequestId}`}>
                    <IconAlert size={12} /> {req.blockedReason}
                  </div>
                ) : null}
              </td>
              <td data-testid={`skip-state-${req.skipRequestId}`}>{req.state}</td>
              <td className="sg-muted" data-testid={`skip-progress-${req.skipRequestId}`}>
                {progressLabel(req.progress)}
              </td>
              <td>
                {needsRecovery(req.state) ? (
                  <button
                    className="sg-btn sg-btn--sm"
                    disabled={busyId === `skip:${req.skipRequestId}`}
                    onClick={() => void resume('skip', req.skipRequestId)}
                  >
                    {busyId === `skip:${req.skipRequestId}` ? '恢复中…' : '恢复执行'}
                  </button>
                ) : null}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
