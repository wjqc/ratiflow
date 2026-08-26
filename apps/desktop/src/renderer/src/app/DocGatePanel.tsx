import { useCallback, useEffect, useState } from 'react';
import { rpc, rpcErrorMessage, waitForRunTerminal } from '../rpc/client';
import { gateLabel } from './AppShell';

interface Props {
  workItemId: string;
  gate: string;
  onDone: () => void;
}

const DOC_GATES: Record<string, { kind: string; label: string }> = {
  requirements: { kind: 'prd', label: 'PRD' },
  design: { kind: 'tech_design', label: '技术方案' },
  testing: { kind: 'test_plan', label: '测试计划' },
};

interface ArtifactInfo { id: string; kind: string; title: string }
interface RevisionInfo { id: string; rev_no: number; status: string; etag: string }

// 文档驱动关（需求/方案/测试）：创建工件 → 草稿（可让 Agent 起草）→ 评审 → 冻结 → 证据 → 门禁。
export default function DocGatePanel({ workItemId, gate, onDone }: Props) {
  const config = DOC_GATES[gate];
  const [artifact, setArtifact] = useState<ArtifactInfo | null>(null);
  const [revision, setRevision] = useState<RevisionInfo | null>(null);
  const [draft, setDraft] = useState('');
  const [reviewer, setReviewer] = useState('local-user');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');

  const run = useCallback(async (action: () => Promise<string>) => {
    setBusy(true);
    setError('');
    setNotice('');
    try {
      setNotice(await action());
      await reload();
    } catch (reason) {
      setError(rpcErrorMessage(reason));
    } finally {
      setBusy(false);
    }
  }, []);

  const reload = useCallback(async () => {
    try {
      const page = await rpc<{ items: ArtifactInfo[] }>('artifact.list', { workItemId });
      const found = page.items.find((a) => a.kind === config.kind) ?? null;
      setArtifact(found);
      if (found) {
        const revisions = await rpc<{ items: RevisionInfo[] }>('artifact.listRevisions', { artifactId: found.id });
        const latest = revisions.items.find((r) => r.status !== 'superseded') ?? revisions.items[0] ?? null;
        setRevision(latest);
        if (latest) {
          const content = await rpc<{ content: string }>('artifact.revisionContent', { revisionId: latest.id });
          setDraft(content.content);
        }
      }
    } catch (reason) {
      setError(rpcErrorMessage(reason));
    }
  }, [workItemId, config.kind]);

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
              disabled={busy}
              onClick={() => {
                void run(async () => {
                  const manifest = await rpc<{ id: string }>('context.create', {
                    projectId: '', workItemId, query: draft.slice(0, 100) || workItemId, selectedSources: [],
                  }).catch(() => null);
                  const started = await rpc<{ runId: string }>('agent.start', {
                    workItemId,
                    goal: `${draftGoal(gate, draft)}`,
                    contextManifestId: manifest?.id ?? 'ctx-any',
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
              🤖 让 Agent 起草
            </button>
            <button
              className="sg-button sg-button--primary"
              disabled={busy || draft.trim() === ''}
              onClick={() => {
                void run(async () => {
                  if (revision) {
                    const updated = await rpc<RevisionInfo>('artifact.updateDraft', {
                      revisionId: revision.id, etag: revision.etag, content: draft,
                    });
                    setRevision(updated);
                  } else {
                    await rpc('artifact.createDraft', { artifactId: artifact.id, content: draft });
                  }
                  return '草稿已保存（不可变修订）。';
                });
              }}
            >
              保存草稿
            </button>
            {revision ? <span className="sg-muted">r{revision.rev_no} · {revision.status}</span> : null}
          </div>
          <textarea className="sg-textarea" rows={12} value={draft} onChange={(e) => setDraft(e.target.value)}
            placeholder={`# ${config.label}\n\n范围…\n非目标…\n验收标准…`} />

          {revision?.status === 'draft' ? (
            <div className="sg-row">
              <label className="sg-field" style={{ minWidth: 140 }}>
                <span>评审人</span>
                <input className="sg-input" value={reviewer} onChange={(e) => setReviewer(e.target.value)} />
              </label>
              <button className="sg-button sg-button--primary" disabled={busy} onClick={() => {
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
                  const evidence = await rpc<{ id: string }>('evidence.record', {
                    workItemId, gate, kind: 'review', title: `${config.label}评审与冻结记录`, source: 'local',
                  });
                  await rpc('evidence.verify', { evidenceId: evidence.id, verifiedBy: reviewer });
                  return '证据已记录并复验；点击评估门禁。';
                });
              }}>
                记录证据并复验
              </button>
              <EvaluateButton workItemId={workItemId} gate={gate} busy={busy} onDone={onDone} />
            </div>
          ) : null}
        </>
      )}
    </div>
  );
}

export function EvaluateButton({ workItemId, gate, busy, onDone }: { workItemId: string; gate: string; busy: boolean; onDone: () => void }) {
  const [error, setError] = useState('');
  return (
    <>
      {error ? <div className="sg-banner sg-banner--error">{error}</div> : null}
      <button
        className="sg-button sg-button--primary"
        disabled={busy}
        onClick={() => {
          setError('');
          void rpc<{ passed: boolean; failed_inputs: string[] }>('gate.evaluate', { workItemId, gate })
            .then((result) => {
              onDone();
              if (!result.passed) {
                setError(`门禁未通过：${result.failed_inputs.join('、')}`);
              }
            })
            .catch((reason) => setError(rpcErrorMessage(reason)));
        }}
      >
        ⚖️ 评估{gateLabel(gate)}门禁
      </button>
    </>
  );
}

function draftGoal(gate: string, draft: string): string {
  const base = draft.slice(0, 400);
  if (gate === 'requirements') {
    return `根据以下需求起草 PRD（范围、非目标、用户故事、验收标准、风险）：\n${base}`;
  }
  if (gate === 'design') {
    return `为以下需求起草技术方案（架构、API、数据、错误、测试与回滚）：\n${base}`;
  }
  return `为以下需求起草测试计划（每条验收标准至少一个用例）：\n${base}`;
}
