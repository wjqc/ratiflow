import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  addReview,
  createArtifact,
  createDeployment,
  createDraft,
  deploy,
  documentContent,
  evaluateGate,
  freezeBaseline,
  getWorkItem,
  issuePassport,
  latestPassport,
  listArtifacts,
  listDocuments,
  listEvidence,
  listRevisions,
  recordEvidence,
  revisionContent,
  startAgentRunWithManifest,
  submitDeployment,
  updateDraft,
  verifyDeployment,
  verifyEvidence,
} from './api';
import type {
  Artifact,
  Evidence,
  GateName,
  GateResultInfo,
  PassportInfo,
  RevisionInfo,
  Stage,
  WorkItem,
} from './types';
import { gateLabels, gateOrder, stageLabels } from './types';

interface Props {
  workItemId: string;
  onBack: () => void;
}

// 闯关工作台：SixGates 主功能页。每一关是一个操作面板——
// 起草 → 评审 → 冻结 → 证据 → 门禁评估，通过后进入下一关。
export default function Workbench({ workItemId, onBack }: Props) {
  const [workItem, setWorkItem] = useState<WorkItem | null>(null);
  const [stages, setStages] = useState<Stage[]>([]);
  const [artifacts, setArtifacts] = useState<Artifact[]>([]);
  const [evidences, setEvidences] = useState<Evidence[]>([]);
  const [passport, setPassport] = useState<PassportInfo | null>(null);
  const [gateResult, setGateResult] = useState<GateResultInfo | null>(null);
  const [requirementDoc, setRequirementDoc] = useState<{ name: string; content: string } | null>(null);
  const [showDoc, setShowDoc] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    setError('');
    try {
      const detail = await getWorkItem(workItemId);
      setWorkItem(detail.workItem);
      setStages(detail.stages);
      const [artifactPage, evidencePage, docPage] = await Promise.all([
        listArtifacts(workItemId),
        listEvidence(workItemId),
        listDocuments(workItemId).catch(() => ({ items: [] as string[] })),
      ]);
      setArtifacts(artifactPage.items);
      setEvidences(evidencePage.items);
      if (docPage.items.length > 0 && !requirementDoc) {
        const content = await documentContent(workItemId, docPage.items[0]).catch(() => '');
        setRequirementDoc({ name: docPage.items[0], content });
      }
      setPassport(await latestPassport(workItemId).catch(() => null));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '加载失败');
    } finally {
      setLoading(false);
    }
    // requirementDoc 仅首次加载回读，避免覆盖展开状态。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [workItemId, requirementDoc === null]);

  useEffect(() => {
    setLoading(true);
    setGateResult(null);
    void refresh();
  }, [refresh]);

  const run = async (action: () => Promise<string>, options?: { silent?: boolean }) => {
    setBusy(true);
    setError('');
    if (!options?.silent) {
      setNotice('');
    }
    try {
      const message = await action();
      if (message) {
        setNotice(message);
      }
      await refresh();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '操作失败');
    } finally {
      setBusy(false);
    }
  };

  if (loading) {
    return <div className="panel loading-panel">正在读取六关状态…</div>;
  }
  if (!workItem) {
    return <div className="error-banner" role="alert">{error || '工作项不存在'}</div>;
  }

  const currentGate = workItem.currentGate;
  const allPassed = stages.length > 0 && stages.every((s) => s.state === 'passed');

  return (
    <div className="stack">
      <div className="page-heading">
        <div>
          <h1>{workItem.title}</h1>
          <p>
            {workItem.gitlabIssueIid ? `GitLab Issue #${workItem.gitlabIssueIid} · ` : ''}
            当前：{gateLabels[currentGate]}
          </p>
        </div>
        <div className="heading-actions">
          <button className="secondary-button" type="button" onClick={onBack}>← 返回需求列表</button>
        </div>
      </div>
      {error ? <div className="error-banner" role="alert">{error}</div> : null}
      {notice ? <div className="notice-banner" role="status">{notice}</div> : null}

      {requirementDoc ? (
        <section className="panel reqdoc-panel" aria-labelledby="reqdoc-title">
          <div className="reqdoc-header">
            <h2 id="reqdoc-title">📄 需求原文</h2>
            <span className="draft-meta">
              工作目录 <code>data/docs/{workItem.id.slice(0, 10)}…/{requirementDoc.name}</code>
            </span>
            <button
              className="row-action"
              type="button"
              aria-expanded={showDoc}
              onClick={() => setShowDoc((v) => !v)}
            >
              {showDoc ? '收起' : '展开'}
            </button>
          </div>
          {showDoc ? <pre className="reqdoc-body">{requirementDoc.content}</pre> : null}
        </section>
      ) : null}

      <ol className="gate-progress" aria-label="六关进度">
        {gateOrder.map((gate) => {
          const stage = stages.find((s) => s.gate === gate);
          const state = stage?.state ?? 'not_started';
          const isCurrent = gate === currentGate && !allPassed;
          return (
            <li
              key={gate}
              className={`gate-step gate-state-${state} ${isCurrent ? 'gate-current' : ''}`}
              aria-current={isCurrent ? 'step' : undefined}
            >
              <span className="gate-name">{gateLabels[gate]}</span>
              <span className={`stage-text stage-${state}`}>{isCurrent ? '进行中' : stageLabels[state]}</span>
            </li>
          );
        })}
      </ol>

      {allPassed ? (
        <PassportPanel
          passport={passport}
          busy={busy}
          onIssue={() =>
            run(async () => {
              await issuePassport(workItemId);
              return '通关文牒已签发 🎉 六关完成，共享摘要可提交 GitLab。';
            })
          }
        />
      ) : (
        <section className="panel workbench-panel" aria-labelledby="current-gate-title">
          <h2 id="current-gate-title">第 {gateOrder.indexOf(currentGate) + 1} 关 · {gateLabels[currentGate]}</h2>
          {currentGate === 'requirements' && (
            <DocGatePanel
              workItem={workItem}
              gate={currentGate}
              kind="prd"
              kindLabel="PRD"
              agentGoal={`根据以下需求起草 PRD（范围、非目标、用户故事、验收标准、风险）：\n标题：${workItem.title}\n描述：${workItem.description || '（未提供）'}`}
              artifacts={artifacts}
              busy={busy}
              setBusy={setBusy}
              run={run}
              onEvaluated={setGateResult}
            />
          )}
          {currentGate === 'design' && (
            <DocGatePanel
              workItem={workItem}
              gate={currentGate}
              kind="tech_design"
              kindLabel="技术方案"
              agentGoal={`为以下需求起草技术方案（架构、API、数据、错误处理、测试与回滚策略）：\n${workItem.title}\n${workItem.description || ''}`}
              artifacts={artifacts}
              busy={busy}
              setBusy={setBusy}
              run={run}
              onEvaluated={setGateResult}
            />
          )}
          {currentGate === 'development' && <DevGatePanel workItem={workItem} busy={busy} run={run} onEvaluated={setGateResult} />}
          {currentGate === 'testing' && (
            <DocGatePanel
              workItem={workItem}
              gate={currentGate}
              kind="test_plan"
              kindLabel="测试计划"
              agentGoal={`为以下需求起草测试计划（每条验收标准至少一个测试用例）：\n${workItem.title}`}
              artifacts={artifacts}
              busy={busy}
              setBusy={setBusy}
              run={run}
              onEvaluated={setGateResult}
            />
          )}
          {currentGate === 'deployment' && <DeployGatePanel workItem={workItem} busy={busy} run={run} onEvaluated={setGateResult} />}
          {currentGate === 'verification' && (
            <VerifyGatePanel workItem={workItem} busy={busy} run={run} onEvaluated={setGateResult} />
          )}
        </section>
      )}

      {gateResult ? (
        <section className="panel" aria-labelledby="gate-result-title">
          <h2 id="gate-result-title">门禁结论 · {gateLabels[gateResult.gate as GateName]}</h2>
          {gateResult.passed ? (
            <p className="gate-verdict verdict-pass" role="status">✅ 通过！已进入下一关。</p>
          ) : (
            <div className="gate-verdict verdict-fail" role="alert">
              <p>⛔ 未通过，还需要完成：</p>
              <ul>
                {gateResult.failedInputs.map((input) => (
                  <li key={input}>{gateInputLabels[input] ?? input}</li>
                ))}
              </ul>
            </div>
          )}
        </section>
      ) : null}

      <section className="panel" aria-labelledby="wb-evidence-title">
        <h2 id="wb-evidence-title">已积累的证据（{evidences.length}）</h2>
        {evidences.length === 0 ? (
          <div className="empty-state">尚无证据；每关完成后会在这里累积。</div>
        ) : (
          <div className="evidence-compact">
            {evidences.map((evidence) => (
              <span key={evidence.id} className={`evidence-chip ${evidence.verified ? 'verified' : ''}`}>
                {gateLabels[evidence.gate]} · {evidence.kind} {evidence.verified ? '✓' : '…'}
              </span>
            ))}
          </div>
        )}
      </section>
    </div>
  );
}

type RunFn = (action: () => Promise<string>, options?: { silent?: boolean }) => Promise<void>;

interface PanelProps {
  workItem: WorkItem;
  busy: boolean;
  run: RunFn;
  onEvaluated: (result: GateResultInfo) => void;
}

// ---------- 文档驱动关（需求 / 方案 / 测试） ----------

interface DocPanelProps extends PanelProps {
  gate: GateName;
  kind: string;
  kindLabel: string;
  agentGoal: string;
  artifacts: Artifact[];
  setBusy: (v: boolean) => void;
}

function DocGatePanel({ workItem, gate, kind, kindLabel, agentGoal, artifacts, busy, setBusy, run, onEvaluated }: DocPanelProps) {
  const artifact = artifacts.find((a) => a.kind === kind);
  const [draft, setDraft] = useState('');
  const [revision, setRevision] = useState<RevisionInfo | null>(null);
  const [etag, setEtag] = useState('');
  const [reviewer, setReviewer] = useState('local-user');
  const [loaded, setLoaded] = useState(false);

  // 回显已有修订：取该工件最新的非 superseded 修订，载入内容与状态。
  useEffect(() => {
    if (!artifact || loaded) {
      return;
    }
    setLoaded(true);
    void (async () => {
      try {
        const page = await listRevisions(artifact.id);
        const latest = page.items.find((r) => r.status !== 'superseded') ?? page.items[0];
        if (!latest) {
          return;
        }
        setRevision(latest);
        setEtag(latest.etag);
        const content = await revisionContent(latest.id);
        setDraft(content);
      } catch {
        setDraft('');
      }
    })();
  }, [artifact, loaded]);

  const steps = useMemo(() => {
    const hasArtifact = Boolean(artifact);
    const draftSaved = Boolean(revision);
    const reviewed = revision?.status === 'in_review';
    const frozen = revision?.status === 'frozen';
    return [
      { label: `创建${kindLabel}工件`, done: hasArtifact },
      { label: '完成草稿', done: draftSaved },
      { label: '评审通过', done: reviewed },
      { label: '冻结基线', done: frozen },
    ];
  }, [artifact, revision, kindLabel]);

  const saveDraft = () =>
    run(async () => {
      if (!artifact) {
        throw new Error('请先创建工件');
      }
      if (revision && etag) {
        const updated = await updateDraft(revision.id, etag, draft);
        setRevision(updated.revision);
        setEtag(updated.etag);
        return '草稿已保存（新修订）。';
      }
      const created = await createDraft(artifact.id, draft);
      setRevision(created.revision);
      setEtag(created.etag);
      return '草稿已保存。';
    });

  const agentDraft = () =>
    run(async () => {
      const result = await startAgentRunWithManifest(workItem.id, agentGoal, ['read_file']);
      if (result.status === 'completed_execution' && result.result) {
        setDraft((prev) => (prev ? `${prev}\n\n---\n${result.result}` : result.result));
        return 'Agent 已起草内容（见编辑框，确认后保存草稿）。';
      }
      throw new Error(`Agent 未完成：${result.status} ${result.result}`);
    });

  return (
    <div className="stack-sm">
      <StepChecklist steps={steps} />
      {!artifact ? (
        <div>
          <p className="panel-hint">第一步：为这个需求建立{kindLabel}工件。</p>
          <button
            className="primary-button"
            type="button"
            disabled={busy}
            onClick={() =>
              run(async () => {
                await createArtifact(workItem.id, kind, kindLabel);
                return `${kindLabel}工件已创建。`;
              })
            }
          >
            创建{kindLabel}工件
          </button>
        </div>
      ) : (
        <>
          <div className="draft-toolbar">
            <button className="secondary-button" type="button" disabled={busy} onClick={() => void agentDraft()}>
              🤖 让 Agent 起草
            </button>
            <button className="primary-button" type="button" disabled={busy || draft.trim() === ''} onClick={() => void saveDraft()}>
              保存草稿
            </button>
            {revision ? <span className="draft-meta">修订 r{revision.revNo} · {revisionStatusLabels[revision.status]}</span> : null}
          </div>
          <label className="field">
            <span>{kindLabel}草稿（Markdown）</span>
            <textarea
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              rows={12}
              placeholder={`# ${kindLabel}\n\n范围…\n非目标…\n验收标准…`}
            />
          </label>

          {revision && revision.status === 'draft' ? (
            <div className="inline-actions">
              <label className="field field-narrow">
                <span>评审人</span>
                <input value={reviewer} onChange={(e) => setReviewer(e.target.value)} />
              </label>
              <button
                className="primary-button"
                type="button"
                disabled={busy}
                onClick={() =>
                  run(async () => {
                    await addReview(revision.id, reviewer.trim() || 'local-user', 'approved');
                    return '评审通过；可冻结基线。';
                  })
                }
              >
                评审通过
              </button>
            </div>
          ) : null}

          {revision && revision.status === 'in_review' ? (
            <button
              className="primary-button"
              type="button"
              disabled={busy}
              onClick={() =>
                run(async () => {
                  await freezeBaseline(workItem.id, gate, [revision.id]);
                  return '基线已冻结；下游将以此为准。';
                })
              }
            >
              冻结{gateLabels[gate]}基线
            </button>
          ) : null}

          {revision?.status === 'frozen' ? (
            <div className="inline-actions">
              <button
                className="primary-button"
                type="button"
                disabled={busy}
                onClick={() =>
                  run(async () => {
                    const evidence = await recordEvidence(workItem.id, {
                      gate,
                      kind: 'review',
                      title: `${kindLabel}评审与冻结记录`,
                      source: 'local',
                    });
                    await verifyEvidence(evidence.id, reviewer);
                    return '证据已记录并复验；点击下方评估门禁。';
                  })
                }
              >
                记录{kindLabel}证据并复验
              </button>
              <EvaluateButton gate={gate} workItemId={workItem.id} busy={busy} run={run} onEvaluated={onEvaluated} />
            </div>
          ) : null}
        </>
      )}
    </div>
  );
}

// ---------- 开发关 ----------

function DevGatePanel({ workItem, busy, run, onEvaluated }: PanelProps) {
  const [goal, setGoal] = useState(`实现：${workItem.title}`);
  const [pipelineId, setPipelineId] = useState('');
  const [mrIid, setMrIid] = useState('');
  const [lastRun, setLastRun] = useState<{ status: string; result: string } | null>(null);

  return (
    <div className="stack-sm">
      <StepChecklist
        steps={[
          { label: 'Agent 执行开发任务', done: lastRun?.status === 'completed_execution' },
          { label: '提交 MR / 记录 CI 证据', done: pipelineId.trim() !== '' },
        ]}
      />
      <label className="field">
        <span>开发任务目标（Agent 只能提案工具调用，高风险需审批）</span>
        <textarea value={goal} onChange={(e) => setGoal(e.target.value)} rows={3} />
      </label>
      <button
        className="primary-button"
        type="button"
        disabled={busy}
        onClick={() =>
          run(async () => {
            const result = await startAgentRunWithManifest(workItem.id, goal, ['read_file', 'write_file']);
            setLastRun(result);
            if (result.status === 'completed_execution') {
              return `Agent 完成：${result.result}`;
            }
            throw new Error(`Agent 状态 ${result.status}：${result.result}`);
          })
        }
      >
        🤖 启动 Agent Run
      </button>
      {lastRun ? (
        <div className="agent-result">
          <strong>{lastRun.status}</strong>
          <p>{lastRun.result}</p>
        </div>
      ) : null}

      <div className="inline-actions">
        <label className="field field-narrow">
          <span>MR IID</span>
          <input value={mrIid} onChange={(e) => setMrIid(e.target.value)} placeholder="7" />
        </label>
        <label className="field field-narrow">
          <span>Pipeline ID</span>
          <input value={pipelineId} onChange={(e) => setPipelineId(e.target.value)} placeholder="1024" />
        </label>
        <button
          className="primary-button"
          type="button"
          disabled={busy || pipelineId.trim() === ''}
          onClick={() =>
            run(async () => {
              const evidence = await recordEvidence(workItem.id, {
                gate: 'development',
                kind: 'ci_pipeline',
                title: `MR !${mrIid || '—'} Pipeline ${pipelineId}`,
                source: 'gitlab',
              });
              await verifyEvidence(evidence.id, 'local-user');
              return 'CI 证据已记录；确认 MR head SHA 与 Pipeline 一致后评估门禁。';
            })
          }
        >
          记录 CI 证据并复验
        </button>
        <EvaluateButton gate="development" workItemId={workItem.id} busy={busy} run={run} onEvaluated={onEvaluated} />
      </div>
    </div>
  );
}

// ---------- 部署关 ----------

function DeployGatePanel({ workItem, busy, run, onEvaluated }: PanelProps) {
  const [host, setHost] = useState('deploy.example.internal');
  const [fingerprint, setFingerprint] = useState('SHA256:…');
  const [remoteDir, setRemoteDir] = useState('/srv/app');
  const [digest, setDigest] = useState('sha256:…');
  const [deployment, setDeployment] = useState<{ id: string; state: string } | null>(null);

  return (
    <div className="stack-sm">
      <StepChecklist
        steps={[
          { label: '制定部署计划', done: deployment?.state !== undefined && deployment?.state !== 'draft' },
          { label: '人工审批', done: Boolean(deployment && !['draft', 'awaiting_approval'].includes(deployment.state)) },
          { label: '部署并验证', done: deployment?.state === 'verified' },
        ]}
      />
      {!deployment || deployment.state === 'draft' ? (
        <>
          <div className="inline-actions">
            <label className="field">
              <span>SSH 主机</span>
              <input value={host} onChange={(e) => setHost(e.target.value)} />
            </label>
            <label className="field">
              <span>主机指纹</span>
              <input value={fingerprint} onChange={(e) => setFingerprint(e.target.value)} />
            </label>
            <label className="field field-narrow">
              <span>远程目录</span>
              <input value={remoteDir} onChange={(e) => setRemoteDir(e.target.value)} />
            </label>
          </div>
          <label className="field">
            <span>镜像 digest（必须是不可变 sha256 引用，tag 会被拒绝）</span>
            <input value={digest} onChange={(e) => setDigest(e.target.value)} placeholder="sha256:abc123…" />
          </label>
          <button
            className="primary-button"
            type="button"
            disabled={busy}
            onClick={() =>
              run(async () => {
                const created = await createDeployment(workItem.id, {
                  target: { host, port: 22, user: 'deploy', expectedFingerprint: fingerprint, remoteDir },
                  imageDigest: digest,
                  deploySteps: [
                    { seq: 0, name: 'compose_up', argv: ['docker', 'compose', 'up', '-d'], timeoutSec: 180 },
                  ],
                  verifyChecks: [
                    { name: 'health', argv: ['curl', '-f', 'http://localhost:8080/healthz'], required: true },
                  ],
                  rollbackSteps: [
                    { seq: 0, name: 'compose_down', argv: ['docker', 'compose', 'down'], timeoutSec: 120 },
                  ],
                });
                setDeployment(created);
                await submitDeployment(created.id);
                return '部署计划已提交审批；请到「审批中心」批准后回到本页执行部署。';
              })
            }
          >
            创建部署计划并提交审批
          </button>
        </>
      ) : (
        <>
          <p className="panel-hint">
            部署 <code>{deployment.id.slice(0, 14)}…</code> 当前状态：<strong>{deployment.state}</strong>
          </p>
          {deployment.state === 'awaiting_approval' ? (
            <p className="panel-hint">⏳ 等待审批：到「审批中心」批准本部署（批准绑定计划指纹，改参数需重新审批）。</p>
          ) : null}
          {deployment.state === 'approved' || deployment.state === 'awaiting_verification' ? (
            <div className="inline-actions">
              {deployment.state === 'approved' ? (
                <button
                  className="primary-button"
                  type="button"
                  disabled={busy}
                  onClick={() =>
                    run(async () => {
                      const updated = await deploy(deployment.id);
                      setDeployment(updated);
                      return `部署执行完成，进入等待验证（${updated.state}）。`;
                    })
                  }
                >
                  执行部署（SSH 预检 + Compose）
                </button>
              ) : null}
              {deployment.state === 'awaiting_verification' ? (
                <button
                  className="primary-button"
                  type="button"
                  disabled={busy}
                  onClick={() =>
                    run(async () => {
                      const updated = await verifyDeployment(deployment.id);
                      setDeployment(updated);
                      if (updated.state !== 'verified') {
                        throw new Error(`验证未通过：${updated.state}；Agent 不可覆盖，需回滚后重试。`);
                      }
                      const evidence = await recordEvidence(workItem.id, {
                        gate: 'deployment',
                        kind: 'deployment',
                        title: `部署 ${updated.target} 验证通过`,
                        source: 'local',
                      });
                      await verifyEvidence(evidence.id, 'local-user');
                      return '验证通过并已记录证据。';
                    })
                  }
                >
                  运行验证检查
                </button>
              ) : null}
            </div>
          ) : null}
          {deployment.state === 'verified' ? (
            <div className="inline-actions">
              <EvaluateButton gate="deployment" workItemId={workItem.id} busy={busy} run={run} onEvaluated={onEvaluated} />
            </div>
          ) : null}
          {['deploy_failed', 'verification_failed', 'rollback_failed'].includes(deployment.state) ? (
            <p className="error-banner" role="alert">
              部署状态 {deployment.state}：按 Runbook 处置（回滚或修正后重新制定计划）。
            </p>
          ) : null}
        </>
      )}
    </div>
  );
}

// ---------- 验证关 ----------

function VerifyGatePanel({ workItem, busy, run, onEvaluated }: PanelProps) {
  const [smoke, setSmoke] = useState('登录页可达、健康检查 200、无敏感日志');
  return (
    <div className="stack-sm">
      <StepChecklist
        steps={[
          { label: '运行冒烟验证', done: false },
          { label: '签发通关文牒', done: false },
        ]}
      />
      <label className="field">
        <span>冒烟验证结论（关键 API/UI、健康检查、日志扫描）</span>
        <textarea value={smoke} onChange={(e) => setSmoke(e.target.value)} rows={3} />
      </label>
      <div className="inline-actions">
        <button
          className="primary-button"
          type="button"
          disabled={busy}
          onClick={() =>
            run(async () => {
              const evidence = await recordEvidence(workItem.id, {
                gate: 'verification',
                kind: 'smoke',
                title: '线上冒烟验证',
                source: 'local',
              });
              await verifyEvidence(evidence.id, 'local-user');
              return '冒烟证据已记录并复验。';
            })
          }
        >
          记录冒烟证据并复验
        </button>
        <EvaluateButton gate="verification" workItemId={workItem.id} busy={busy} run={run} onEvaluated={onEvaluated} />
      </div>
      <p className="panel-hint">{smoke}</p>
    </div>
  );
}

// ---------- 文牒 ----------

function PassportPanel({
  passport,
  busy,
  onIssue,
}: {
  passport: PassportInfo | null;
  busy: boolean;
  onIssue: () => void;
}) {
  if (!passport) {
    return (
      <section className="panel workbench-panel" aria-labelledby="passport-title">
        <h2 id="passport-title">🏆 六关全部通过</h2>
        <p className="panel-hint">签发通关文牒：包含六关结论、证据索引与本地完整证据哈希。</p>
        <button className="primary-button" type="button" disabled={busy} onClick={onIssue}>
          签发通关文牒
        </button>
      </section>
    );
  }
  return (
    <section className="panel workbench-panel passport-panel" aria-labelledby="passport-title">
      <h2 id="passport-title">🏆 通关文牒</h2>
      <p>对象哈希：<code>{passport.objectSha256.slice(0, 32)}…</code></p>
      <ol className="passport-gates">
        {passport.gates.map((g) => (
          <li key={g.gate} className={g.passed ? 'pg-pass' : 'pg-fail'}>
            {gateLabels[g.gate as GateName]} {g.passed ? '✓' : '✗'}（{g.evidenceIds.length} 证据）
          </li>
        ))}
      </ol>
    </section>
  );
}

// ---------- 公共小组件 ----------

function EvaluateButton({
  gate,
  workItemId,
  busy,
  run,
  onEvaluated,
}: {
  gate: GateName;
  workItemId: string;
  busy: boolean;
  run: RunFn;
  onEvaluated: (result: GateResultInfo) => void;
}) {
  return (
    <button
      className="primary-button evaluate-btn"
      type="button"
      disabled={busy}
      onClick={() =>
        run(async () => {
          const result = await evaluateGate(workItemId, gate);
          onEvaluated(result);
          return result.passed ? '门禁通过 🎉' : '门禁未通过（见下方明细）。';
        })
      }
    >
      ⚖️ 评估{gateLabels[gate]}门禁
    </button>
  );
}

function StepChecklist({ steps }: { steps: Array<{ label: string; done: boolean }> }) {
  return (
    <ol className="step-checklist" aria-label="本关步骤">
      {steps.map((step) => (
        <li key={step.label} className={step.done ? 'step-done' : 'step-todo'}>
          <span aria-hidden="true">{step.done ? '✓' : '○'}</span> {step.label}
        </li>
      ))}
    </ol>
  );
}

const revisionStatusLabels: Record<string, string> = {
  draft: '草稿',
  in_review: '评审通过待冻结',
  frozen: '已冻结',
  superseded: '已被取代',
};

const gateInputLabels: Record<string, string> = {
  required_artifacts_frozen: '必需工件已冻结',
  required_checks_passed: '必需检查通过',
  approvals_valid: '审批有效',
  evidence_complete: '证据完备',
  no_blocking_risk: '无阻断风险',
  inputs_current: '输入为当前版本',
};
