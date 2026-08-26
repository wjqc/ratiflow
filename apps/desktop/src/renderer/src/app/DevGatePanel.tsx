import { useState } from 'react';
import { rpc, rpcErrorMessage, waitForRunTerminal } from '../rpc/client';
import { EvaluateButton } from './DocGatePanel';

interface Props {
  workItemId: string;
  onDone: () => void;
}

interface RunInfo { status: string; result: string }

// 开发关：Agent Run（工具提案制）→ 记录 MR/CI 证据 → 门禁评估。
export default function DevGatePanel({ workItemId, onDone }: Props) {
  const [goal, setGoal] = useState('');
  const [mrIid, setMrIid] = useState('');
  const [pipelineId, setPipelineId] = useState('');
  const [lastRun, setLastRun] = useState<RunInfo | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');

  const run = async (action: () => Promise<string>) => {
    setBusy(true);
    setError('');
    setNotice('');
    try {
      setNotice(await action());
      onDone();
    } catch (reason) {
      setError(rpcErrorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="gate-workspace">
      <ol className="step-checklist">
        <li className={lastRun?.status === 'completed_execution' ? 'done' : ''}>
          {lastRun?.status === 'completed_execution' ? '✓' : '○'} Agent 执行开发任务
        </li>
        <li className={pipelineId ? 'done' : ''}>{pipelineId ? '✓' : '○'} 记录 MR / CI 证据</li>
      </ol>
      {error ? <div className="sg-banner sg-banner--error" role="alert">{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

      <label className="sg-field">
        <span>开发任务目标（Agent 只能提案工具调用；高风险动作进入审批中心）</span>
        <textarea className="sg-textarea" rows={3} value={goal} onChange={(e) => setGoal(e.target.value)}
          placeholder="例如：实现 OIDC 登录回调并补充单测" />
      </label>
      <div className="sg-row">
        <button
          className="sg-button sg-button--primary"
          disabled={busy || goal.trim() === ''}
          onClick={() => {
            void run(async () => {
              const manifest = await rpc<{ id: string }>('context.create', {
                projectId: '', workItemId, query: goal.slice(0, 100), selectedSources: [],
              });
              const started = await rpc<{ runId: string }>('agent.start', {
                workItemId, goal, contextManifestId: manifest.id,
                toolAllowlist: ['read_file', 'write_file'],
                idempotencyKey: `dev-${Date.now()}`,
              });
              const run = await waitForRunTerminal(started.runId);
              setLastRun(run);
              if (run.status !== 'completed_execution') {
                throw new Error(`Agent 状态 ${run.status}：${run.result}`);
              }
              return `Agent 完成：${run.result}`;
            });
          }}
        >
          🤖 启动 Agent Run
        </button>
        {lastRun ? <span className="sg-muted">{lastRun.status} · {lastRun.result.slice(0, 80)}</span> : null}
      </div>

      <div className="sg-row">
        <label className="sg-field" style={{ minWidth: 120 }}>
          <span>MR IID</span>
          <input className="sg-input" value={mrIid} onChange={(e) => setMrIid(e.target.value)} placeholder="7" />
        </label>
        <label className="sg-field" style={{ minWidth: 140 }}>
          <span>Pipeline ID</span>
          <input className="sg-input" value={pipelineId} onChange={(e) => setPipelineId(e.target.value)} placeholder="1024" />
        </label>
        <button
          className="sg-button sg-button--primary"
          disabled={busy || pipelineId.trim() === ''}
          onClick={() => {
            void run(async () => {
              const evidence = await rpc<{ id: string }>('evidence.record', {
                workItemId, gate: 'development', kind: 'ci_pipeline',
                title: `MR !${mrIid || '—'} Pipeline ${pipelineId}`, source: 'gitlab',
              });
              await rpc('evidence.verify', { evidenceId: evidence.id, verifiedBy: 'local-user' });
              return 'CI 证据已记录；开发关只接受当前 MR head SHA 的证据。';
            });
          }}
        >
          记录 CI 证据并复验
        </button>
        <EvaluateButton workItemId={workItemId} gate="development" busy={busy} onDone={onDone} />
      </div>
    </div>
  );
}
