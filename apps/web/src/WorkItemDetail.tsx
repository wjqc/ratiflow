import { useCallback, useEffect, useState } from 'react';
import {
  evaluateGate,
  getWorkItem,
  issuePassport,
  listEvidence,
  recordEvidence,
  verifyEvidence,
} from './api';
import type { Evidence, GateResultInfo, PassportInfo, Stage, WorkItem } from './types';
import { gateLabels, gateOrder, stageLabels } from './types';

interface Props {
  workItemId: string;
  onBack: () => void;
}

export default function WorkItemDetail({ workItemId, onBack }: Props) {
  const [workItem, setWorkItem] = useState<WorkItem | null>(null);
  const [stages, setStages] = useState<Stage[]>([]);
  const [evidences, setEvidences] = useState<Evidence[]>([]);
  const [lastGate, setLastGate] = useState<GateResultInfo | null>(null);
  const [passport, setPassport] = useState<PassportInfo | null>(null);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(true);
  const [notice, setNotice] = useState('');

  const refresh = useCallback(async () => {
    setLoading(true);
    setError('');
    try {
      const detail = await getWorkItem(workItemId);
      setWorkItem(detail.workItem);
      setStages(detail.stages);
      const evidencePage = await listEvidence(workItemId);
      setEvidences(evidencePage.items);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '加载工作项失败');
    } finally {
      setLoading(false);
    }
  }, [workItemId]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const run = async (action: () => Promise<void>, successNote: string) => {
    setNotice('');
    setError('');
    try {
      await action();
      setNotice(successNote);
      await refresh();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '操作失败');
    }
  };

  if (loading) {
    return <div className="panel loading-panel">正在读取六关状态…</div>;
  }
  if (!workItem) {
    return <div className="error-banner" role="alert">{error || '工作项不存在'}</div>;
  }

  return (
    <div className="stack">
      <div className="page-heading">
        <div>
          <h1>{workItem.title}</h1>
          <p>{workItem.gitlabIssueIid ? `GitLab Issue #${workItem.gitlabIssueIid} · ` : ''}当前 {gateLabels[workItem.currentGate]}</p>
        </div>
        <div className="heading-actions">
          <button className="secondary-button" type="button" onClick={onBack}>返回列表</button>
          <button
            className="primary-button"
            type="button"
            onClick={() =>
              void run(async () => {
                const result = await evaluateGate(workItemId, workItem.currentGate);
                setLastGate(result);
                if (result.passed) {
                  await issuePassport(workItemId).then(setPassport).catch(() => undefined);
                }
              }, '门禁已评估')
            }
          >
            评估当前关
          </button>
        </div>
      </div>
      {error ? <div className="error-banner" role="alert">{error}</div> : null}
      {notice ? <div className="notice-banner" role="status">{notice}</div> : null}

      <section className="panel" aria-labelledby="stages-title">
        <h2 id="stages-title">六关状态</h2>
        <ol className="gate-progress">
          {gateOrder.map((gate) => {
            const stage = stages.find((s) => s.gate === gate);
            const state = stage?.state ?? 'not_started';
            return (
              <li key={gate} className={`gate-step gate-state-${state}`}>
                <span className="gate-name">{gateLabels[gate]}</span>
                <span className={`stage-text stage-${state}`}>{stageLabels[state]}</span>
              </li>
            );
          })}
        </ol>
      </section>

      {lastGate ? (
        <section className="panel" aria-labelledby="gate-result-title">
          <h2 id="gate-result-title">最近门禁结论 · {gateLabels[lastGate.gate]}</h2>
          {lastGate.passed ? (
            <p className="gate-verdict verdict-pass" role="status">通过：六项输入全部满足。</p>
          ) : (
            <div className="gate-verdict verdict-fail" role="alert">
              <p>未通过；未满足的输入：</p>
              <ul>
                {lastGate.failedInputs.map((input) => (
                  <li key={input}>{gateInputLabels[input] ?? input}</li>
                ))}
              </ul>
            </div>
          )}
        </section>
      ) : null}

      <section className="panel" aria-labelledby="evidence-title">
        <h2 id="evidence-title">证据</h2>
        {evidences.length === 0 ? (
          <div className="empty-state">尚无证据。为当前关补充证据并复验后才能过关。</div>
        ) : (
          <div className="integration-list">
            <div className="integration-header" aria-hidden="true">
              <span>关</span><span>类型</span><span>状态</span><span>操作</span>
            </div>
            {evidences.map((evidence) => (
              <div className="integration-row" key={evidence.id}>
                <div className="service-name">{gateLabels[evidence.gate]}</div>
                <div className="detail">{evidence.kind}{evidence.title ? ` · ${evidence.title}` : ''}</div>
                <div>
                  <span className={`stage-text ${evidence.verified ? 'stage-passed' : 'stage-running'}`}>
                    {evidence.verified ? '已复验' : '待复验'}
                  </span>
                </div>
                <div>
                  {evidence.verified ? null : (
                    <button
                      className="row-action"
                      type="button"
                      onClick={() => void run(() => verifyEvidence(evidence.id, 'web-user').then(() => undefined), '证据已复验')}
                    >
                      标记复验
                    </button>
                  )}
                </div>
              </div>
            ))}
          </div>
        )}
        <button
          className="secondary-button"
          type="button"
          onClick={() =>
            void run(
              () =>
                recordEvidence(workItemId, {
                  gate: workItem.currentGate,
                  kind: 'manual',
                  title: '人工核验记录',
                  source: 'local',
                }).then(() => undefined),
              '已补充人工证据',
            )
          }
        >
          为当前关补充人工证据
        </button>
      </section>

      {passport ? (
        <section className="panel" aria-labelledby="passport-title">
          <h2 id="passport-title">通关文牒</h2>
          <p>对象哈希：<code>{passport.objectSha256.slice(0, 24)}…</code></p>
        </section>
      ) : null}
    </div>
  );
}

const gateInputLabels: Record<string, string> = {
  required_artifacts_frozen: '必需工件已冻结',
  required_checks_passed: '必需检查通过',
  approvals_valid: '审批有效',
  evidence_complete: '证据完备',
  no_blocking_risk: '无阻断风险',
  inputs_current: '输入为当前版本',
};
