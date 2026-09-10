import { useCallback, useEffect, useState } from 'react';
import { rpc, rpcErrorMessage, waitForRunTerminal } from '../rpc/client';
import { gateLabel } from './AppShell';
import { IconShield, IconZap } from '../components/Icons';
import { draftPrd, friendlyAgentError } from './prdDraft';

interface Props {
  workItemId: string;
  gate: string;
  onDone: () => void;
  onOpenApprovals?: () => void;
}

// kind 与后端交付物门禁映射对齐（workitem/deliverable.rs required_kind），
// 不对齐会导致 request_release 前置 deliverable_missing 永不满足。
const DOC_GATES: Record<string, { kind: string; label: string }> = {
  requirements: { kind: 'prd', label: 'PRD' },
  design: { kind: 'tech_design', label: '技术方案' },
  development: { kind: 'code', label: '代码交付' },
  testing: { kind: 'test', label: '测试计划' },
  // M2 per-gate 基线：部署/验证关也各自产出并冻结基线。
  deployment: { kind: 'deployment', label: '发布说明' },
  verification: { kind: 'verification', label: '验收说明' },
};

interface ArtifactInfo { id: string; kind: string; title: string }
interface RevisionInfo { id: string; rev_no: number; status: string; etag: string }

/** 修订状态中文展示（工程状态值不直接暴露给用户）。 */
const REVISION_STATUS: Record<string, string> = {
  draft: '草稿',
  in_review: '评审中',
  frozen: '已冻结',
  superseded: '已废弃',
};

// 文档驱动关（需求/方案/测试）：创建工件 → 草稿（可让 Agent 起草）→ 评审 → 冻结 → 证据 → 门禁。
export default function DocGatePanel({ workItemId, gate, onDone, onOpenApprovals }: Props) {
  const config = DOC_GATES[gate];
  const [artifact, setArtifact] = useState<ArtifactInfo | null>(null);
  const [revision, setRevision] = useState<RevisionInfo | null>(null);
  const [draft, setDraft] = useState('');
  const [savedDraft, setSavedDraft] = useState('');
  const [reviewer, setReviewer] = useState('local-user');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');

  const run = async (action: () => Promise<string>) => {
    setBusy(true);
    setError('');
    setNotice('');
    try {
      setNotice(await action());
      await reload();
      onDone();
    } catch (reason) {
      setError(friendlyAgentError(reason));
    } finally {
      setBusy(false);
    }
  };

  const reload = useCallback(async () => {
    if (!config) return;
    try {
      const page = await rpc<{ items: ArtifactInfo[] }>('artifact.list', { workItemId });
      const found = page.items.find((a) => a.kind === config.kind) ?? null;
      setArtifact(found);
      if (found) {
        const revisions = await rpc<{ items: RevisionInfo[] }>('artifact.listRevisions', { artifactId: found.id });
        const latest = [...revisions.items].filter((r) => r.status !== 'superseded').sort((a, b) => b.rev_no - a.rev_no)[0] ?? null;
        setRevision(latest);
        if (latest) {
          const content = await rpc<{ content: string }>('artifact.revisionContent', { revisionId: latest.id });
          setDraft(content.content);
          setSavedDraft(content.content);
        }
      }
    } catch (reason) {
      setError(rpcErrorMessage(reason));
    }
  }, [workItemId, config?.kind]);

  useEffect(() => {
    void reload();
  }, [reload]);

  if (!config) {
    return <p className="sg-muted">此关无文档面板。</p>;
  }

  const steps = [
    { label: `创建${config.label}工件`, done: artifact !== null },
    { label: '完成草稿', done: revision !== null },
    { label: '评审通过', done: revision?.status === 'in_review' || revision?.status === 'frozen' },
    { label: '冻结基线', done: revision?.status === 'frozen' },
  ];

  return (
    <div className="gate-workspace">
      <ol className="step-checklist">
        {steps.map((step) => (
          <li key={step.label} className={step.done ? 'done' : ''}>
            {step.done ? '✓' : '○'} {step.label}
          </li>
        ))}
      </ol>
      {error ? <div className="sg-banner sg-banner--error" role="alert">{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

      {!artifact ? (
        <div>
          <p className="sg-muted">第一步：为这个任务建立{config.label}工件。</p>
          <button className="sg-button sg-button--primary" disabled={busy} onClick={() => {
            void run(async () => {
              await rpc('artifact.create', { workItemId, kind: config.kind, title: config.label });
              return `${config.label}工件已创建。`;
            });
          }}>
            创建{config.label}工件
          </button>
        </div>
      ) : (
        <>
          <div className="sg-row">
            <button
              className="sg-button"
              disabled={busy || (revision !== null && revision.status !== 'draft')}
              onClick={() => {
                void run(async () => {
                  if (gate === 'requirements') {
                    await draftPrd(
                      workItemId,
                      draft,
                      `retry-prd-${workItemId}-${Date.now()}`,
                    );
                    return 'PRD 已重新起草并保存为当前草稿。';
                  }
                  // M4：唯一关卡执行入口——服务端装配选路/快照/清单（客户端不自报绑定）。
                  const started = await rpc<{ runId: string; selection: { source_scope: string; fallback_used: boolean } }>('stage.startActivity', {
                    workItemId,
                    gate,
                    goal: `${draftGoal(gate, draft)}`,
                    toolAllowlist: ['read_file'],
                    idempotencyKey: `draft-${Date.now()}`,
                  }).catch((reason) => {
                    throw new Error(rpcErrorMessage(reason));
                  });
                  const run = await waitForRunTerminal(started.runId);
                  if (run.status !== 'completed_execution') {
                    throw new Error(`Agent 状态 ${run.status}：${run.result}`);
                  }
                  setDraft((prev) => (prev ? `${prev}\n\n---\n${run.result}` : run.result));
                  return 'Agent 已起草（见编辑框，确认后保存草稿）。';
                });
              }}
            >
              <IconZap size={14} />
              {gate === 'requirements' ? (revision ? '重新起草 PRD' : '起草 PRD') : '让 Agent 起草'}
            </button>
            <button
              className="sg-button sg-button--primary"
              disabled={busy || draft.trim() === '' || (revision !== null && revision.status !== 'draft')}
              onClick={() => {
                void run(async () => {
                  const keys = await activeRequirementKeys(workItemId);
                  if (revision) {
                    const updated = await rpc<RevisionInfo>('artifact.updateDraft', {
                      revisionId: revision.id, etag: revision.etag, content: draft,
                    });
                    setRevision(updated);
                  } else {
                    await rpc('artifact.createDraft', { artifactId: artifact.id, content: draft, requirementKeys: keys });
                  }
                  return '草稿已保存（不可变修订）。';
                });
              }}
            >
              保存草稿
            </button>
            {revision ? <span className="sg-muted">r{revision.rev_no} · {REVISION_STATUS[revision.status] ?? revision.status}</span> : null}
          </div>
          <textarea className="sg-textarea sg-doc-editor" aria-label={`${config.label}草稿`} readOnly={busy || (revision !== null && revision.status !== 'draft')} rows={12} value={draft} onChange={(e) => setDraft(e.target.value)}
            placeholder={`# ${config.label}\n\n范围…\n非目标…\n验收标准…`} />

          {revision?.status === 'draft' ? (
            <div className="sg-row">
              <label className="sg-field" style={{ minWidth: 140 }}>
                <span>评审人</span>
                <input className="sg-input" value={reviewer} onChange={(e) => setReviewer(e.target.value)} />
              </label>
              {draft !== savedDraft ? <span className="sg-muted">有未保存的修改，请先保存草稿再评审。</span> : null}
              <button className="sg-button sg-button--primary" disabled={busy || draft !== savedDraft || !draft.trim()} onClick={() => {
                void run(async () => {
                  await rpc('artifact.addReview', { revisionId: revision.id, reviewer: reviewer || 'local-user', verdict: 'approved', comment: '工作台评审' });
                  return '评审通过；可冻结基线。';
                });
              }}>
                评审通过
              </button>
            </div>
          ) : null}

          {revision?.status === 'in_review' ? (
            <button className="sg-button sg-button--primary" disabled={busy} onClick={() => {
              void run(async () => {
                await rpc('artifact.freezeBaseline', { workItemId, gate, revisionIds: [revision.id] });
                return '基线已冻结；下游以此为准。';
              });
            }}>
              冻结{gateLabel(gate)}基线
            </button>
          ) : null}

          {revision?.status === 'frozen' ? (
            <div className="sg-row">
              <button className="sg-button sg-button--primary" disabled={busy} onClick={() => {
                void run(async () => {
                  const keys = await activeRequirementKeys(workItemId);
                  const evidence = await rpc<{ id: string }>('evidence.record', {
                    workItemId, gate, kind: 'review', title: `${config.label}评审与冻结记录`, source: 'local',
                    requirementKeys: keys,
                  });
                  await rpc('evidence.verify', { evidenceId: evidence.id, verifiedBy: reviewer });
                  return '证据已记录并复验；点击评估门禁。';
                });
              }}>
                记录证据并复验
              </button>
              <EvaluateButton workItemId={workItemId} gate={gate} busy={busy} onDone={onDone} onOpenApprovals={onOpenApprovals} />
            </div>
          ) : null}
        </>
      )}
    </div>
  );
}

export function EvaluateButton({ workItemId, gate, busy, onDone, onOpenApprovals }: { workItemId: string; gate: string; busy: boolean; onDone: () => void; onOpenApprovals?: () => void }) {
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [pendingRelease, setPendingRelease] = useState<{ id: string } | null>(null);
  // 自持评估中态：父层 busy 恒为 false 时也不能双击重复评估。
  const [evaluating, setEvaluating] = useState(false);

  // M2：该关有待审批放行时不再重复评估——用户决定在审批中心完成。
  const loadPending = useCallback(async () => {
    try {
      const pkg = await rpc<{ releaseRequests: { id: string; state: string }[] }>('stage.package', { workItemId, gate });
      setPendingRelease(pkg.releaseRequests.find((r) => r.state === 'pending') ?? null);
    } catch {
      // 只读状态增强失败不阻塞主流程。
    }
  }, [workItemId, gate]);

  useEffect(() => {
    void loadPending();
  }, [loadPending]);

  const evaluateAndRequest = () => {
    if (evaluating) return;
    setError('');
    setNotice('');
    setEvaluating(true);
    void (async () => {
      try {
        const result = await rpc<{ passed: boolean; failed_inputs: string[] }>('gate.evaluate', { workItemId, gate });
        if (!result.passed) {
          // 键名→中文问题+下一步：门禁拒绝是流程最关键的反馈时刻，不能甩内部键名。
          setError(`门禁未通过：${result.failed_inputs.map(gateInputText).join('；')}`);
          return;
        }
        // evaluate 只计算；通过后冻结输出包并提交用户放行审批（AC-SW-02：current_gate 不变）。
        await rpc('gate.requestRelease', { workItemId, gate });
        setNotice('已冻结输出包并提交放行审批；请在审批中心「批准并进入下一关」。');
        await loadPending();
        onDone();
      } catch (reason) {
        setError(rpcErrorMessage(reason));
      } finally {
        setEvaluating(false);
      }
    })();
  };

  return (
    <>
      {error ? <div className="sg-banner sg-banner--error">{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}
      {pendingRelease ? (
        <>
        <span className="sg-chip" title="在审批中心完成批准 / 要求修改 / 拒绝">
          放行审批等待用户决定
        </span>
        {onOpenApprovals ? <button className="sg-button sg-button--primary" onClick={onOpenApprovals}>前往审批中心，批准后进入下一关</button> : null}
        </>
      ) : (
        <button className="sg-button sg-button--primary" disabled={busy || evaluating} onClick={evaluateAndRequest}>
          <IconShield size={14} />
          {evaluating ? '评估中…' : '评估'}
          {gateLabel(gate)}门禁并提交放行
        </button>
      )}
    </>
  );
}

/** 当前修订全部 active 需求 key（P0-1：正常用户路径必须建立覆盖链）。 */
export async function activeRequirementKeys(workItemId: string): Promise<string[]> {
  try {
    const cov = await rpc<{ items: { requirementKey: string; status: string }[] }>('trace.coverage', { workItemId });
    return cov.items.filter((i) => i.status === 'active').map((i) => i.requirementKey);
  } catch {
    return [];
  }
}

/** 门禁内部输入键 → 用户能看懂的问题 + 下一步动作（P6：键名不外露）。 */
const GATE_INPUT_GUIDANCE: Record<string, string> = {
  required_artifacts_frozen: '本关要求的文档产物还未全部冻结——先在各面板完成「冻结基线」步骤',
  required_checks_passed: '本关的自动检查未全部通过——查看检查详情并修复失败项',
  approvals_valid: '所需审批缺失或已过期——补齐审批后再试',
  evidence_complete: '证据链不完整——先「记录证据并复验」，补齐每步证据',
  no_blocking_risk: '存在未关闭的阻塞风险——先处理风险项或降低风险等级',
  inputs_current: '输入基线已过期——上游文档有更新，需要重新冻结并同步',
};

function gateInputText(key: string): string {
  return GATE_INPUT_GUIDANCE[key] ?? key;
}

function draftGoal(gate: string, draft: string): string {
  // 六关各有正确的起草指令：否则部署/验证关会把"测试计划"贴进发布/验收说明。
  const instructions: Record<string, string> = {
    requirements: '根据以下需求起草 PRD（范围、非目标、用户故事、验收标准、风险）',
    design: '为以下需求起草技术方案（架构、API、数据、错误、测试与回滚）',
    development: '为以下需求与方案撰写开发说明（实现要点、改动清单、自测记录、遗留问题）',
    testing: '为以下需求起草测试计划（每条验收标准至少一个用例）',
    deployment: '为以下任务撰写发布说明（发布步骤、配置变更、验证方式、回滚步骤）',
    verification: '为以下任务撰写验收说明（逐条验收标准的核验结果、结论与遗留风险）',
  };
  const instruction = instructions[gate] ?? '根据以下内容撰写本关文档';
  const base = draft.slice(0, 400);
  // 静默截断会让人以为模型看到了全文：截断必须显式告知。
  const suffix = draft.length > 400 ? '（注：已有内容过长，仅截取前 400 字作为上下文）' : '';
  return `${instruction}${suffix}：\n${base}`;
}
