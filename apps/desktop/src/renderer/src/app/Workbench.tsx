import { useCallback, useEffect, useRef, useState } from 'react';
import { rpc, rpcErrorMessage, type TimelineEvent } from '../rpc/client';
import { gateLabel } from './AppShell';
import DocGatePanel from './DocGatePanel';
import DeployGatePanel from './DeployGatePanel';
import DevGatePanel from './DevGatePanel';

interface Props {
  projectId: string;
  workItemId: string;
  onOpenApprovals: () => void;
}

const GATE_ORDER = ['requirements', 'design', 'development', 'testing', 'deployment', 'verification'];

interface StageInfo {
  gate: string;
  state: string;
  updated_at: string;
}

interface ProgressInfo {
  workItemId: string;
  title: string;
  currentGate: string;
  stages: StageInfo[];
  evidenceCount: number;
  pendingApprovals: number;
  blockedReason: string | null;
}

interface PassportInfo {
  id: string;
  object_sha256: string;
  gates: Array<{ gate: string; passed: boolean; evidence_ids: string[] }>;
}

// 闯关工作台：左时间线 + 中当前关操作 + 右六关进度（规范 §4.1）。
export default function Workbench({ workItemId, onOpenApprovals }: Props) {
  const [progress, setProgress] = useState<ProgressInfo | null>(null);
  const [events, setEvents] = useState<TimelineEvent[]>([]);
  const [passport, setPassport] = useState<PassportInfo | null>(null);
  const [error, setError] = useState('');
  const seenSeq = useRef(new Set<number>());

  const absorbEvents = useCallback((incoming: TimelineEvent[]) => {
    setEvents((prev) => {
      const fresh = incoming.filter((e) => !seenSeq.current.has(e.sequence));
      for (const e of fresh) {
        seenSeq.current.add(e.sequence);
      }
      if (fresh.length === 0) {
        return prev;
      }
      const merged = [...prev, ...fresh];
      merged.sort((a, b) => a.sequence - b.sequence);
      return merged.slice(-500);
    });
  }, []);

  const refresh = useCallback(async () => {
    setError('');
    try {
      const [p, snapshot] = await Promise.all([
        rpc<ProgressInfo>('workitem.progress', { workItemId }),
        rpc<{ events: TimelineEvent[] }>('timeline.snapshot', { workItemId }),
      ]);
      setProgress(p);
      absorbEvents(snapshot.events);
      setPassport(await rpc<PassportInfo>('passport.latest', { workItemId }).catch(() => null));
    } catch (reason) {
      setError(rpcErrorMessage(reason));
    }
  }, [workItemId, absorbEvents]);

  useEffect(() => {
    seenSeq.current = new Set();
    setEvents([]);
    void refresh();
    const timer = setInterval(() => void refresh(), 3000);
    return () => clearInterval(timer);
  }, [refresh]);

  const allPassed = progress?.stages.length === 6 && progress.stages.every((s) => s.state === 'passed');
  const currentGate = progress?.currentGate ?? 'requirements';

  return (
    <>
      <header className="sg-section" style={{ borderBottom: '1px solid var(--sg-border-default)', paddingBottom: 10 }}>
        <h1 style={{ fontSize: 18, margin: 0, fontWeight: 600 }}>{progress?.title ?? '…'}</h1>
        <div className="sg-row" style={{ marginTop: 4 }}>
          <span className="sg-status sg-status--running">本地运行</span>
          <span className="sg-muted">
            当前：{gateLabel(currentGate)} · 证据 {progress?.evidenceCount ?? 0}
          </span>
          {progress && progress.pendingApprovals > 0 ? (
            <button className="sg-button sg-button--danger" onClick={onOpenApprovals}>
              ⚠ {progress.pendingApprovals} 项待审批
            </button>
          ) : null}
        </div>
      </header>
      {error ? <div className="sg-banner sg-banner--error" role="alert" style={{ margin: '10px 20px' }}>{error}</div> : null}

      <div style={{ display: 'grid', gridTemplateColumns: '1fr 1.2fr', flex: 1, minHeight: 0 }}>
        <section aria-label="Agent 执行时间线" style={{ overflowY: 'auto', borderRight: '1px solid var(--sg-bg-subtle)' }}>
          <div className="sg-section" style={{ paddingBottom: 4 }}><strong>执行时间线</strong></div>
          {events.length === 0 ? (
            <div className="sg-empty">暂无事件；每一步操作都会在这里留下可追溯的轨迹。</div>
          ) : (
            <ol className="sg-timeline">
              {events.map((event) => (
                <li key={event.sequence} className="sg-timeline-item">
                  <span className={`sg-timeline-dot ${dotClass(event.type)}`} />
                  <div>
                    <span className="sg-timeline-time">
                      {new Date(event.occurredAt).toLocaleTimeString('zh-CN', { hour12: false })}
                    </span>
                    {event.summary}
                  </div>
                </li>
              ))}
            </ol>
          )}
        </section>

        <section className="sg-section" style={{ overflowY: 'auto' }} aria-label="当前关操作">
          {allPassed ? (
            passport ? <PassportView passport={passport} /> : <IssuePassport workItemId={workItemId} onIssued={refresh} />
          ) : (
            <>
              <h2 className="sg-section-title">第 {GATE_ORDER.indexOf(currentGate) + 1} 关 · {gateLabel(currentGate)}</h2>
              {currentGate === 'development' ? (
                <DevGatePanel workItemId={workItemId} onDone={refresh} />
              ) : currentGate === 'deployment' ? (
                <DeployGatePanel workItemId={workItemId} onDone={refresh} onOpenApprovals={onOpenApprovals} />
              ) : (
                <DocGatePanel workItemId={workItemId} gate={currentGate} onDone={refresh} />
              )}
            </>
          )}
        </section>
      </div>

      <aside className="sg-inspector" aria-label="六关进度">
        <div className="sg-section">
          <h2 className="sg-section-title">六关进度</h2>
          <ol className="sg-gates">
            {(progress?.stages ?? []).map((stage) => (
              <li
                key={stage.gate}
                className={`sg-gate ${stage.gate === currentGate && !allPassed ? 'sg-gate--current' : ''} ${stage.state === 'passed' ? 'sg-gate--passed' : ''}`}
              >
                <span>{gateLabel(stage.gate)}</span>
                <StageMark state={stage.state} />
              </li>
            ))}
          </ol>
          {progress?.blockedReason ? (
            <div className="sg-banner sg-banner--error" style={{ marginTop: 12 }}>{progress.blockedReason}</div>
          ) : null}
          <p className="sg-muted" style={{ marginTop: 12 }}>
            状态由 Rust 门禁引擎基于真实 Stage 计算；renderer 不推断过关。
          </p>
        </div>
      </aside>
    </>
  );
}

export function StageMark({ state }: { state: string }) {
  if (state === 'passed') {
    return <span className="sg-status sg-status--passed">✓ 已通过</span>;
  }
  if (state === 'stale') {
    return <span className="sg-status sg-status--error">↻ 已过期</span>;
  }
  if (state === 'running') {
    return <span className="sg-status sg-status--running">◐ 进行中</span>;
  }
  if (state === 'awaiting_approval') {
    return <span className="sg-status sg-status--error">⏸ 待审批</span>;
  }
  if (state === 'failed') {
    return <span className="sg-status sg-status--error">✕ 失败</span>;
  }
  return <span className="sg-status">○ 未开始</span>;
}

function dotClass(type: string): string {
  if (type.includes('passed') || type.includes('verified') || type.includes('issued')) {
    return 'sg-timeline-dot--passed';
  }
  if (type.includes('failed') || type.includes('rejected')) {
    return 'sg-timeline-dot--error';
  }
  if (type.includes('running') || type.includes('deploying')) {
    return 'sg-timeline-dot--running';
  }
  return '';
}

function IssuePassport({ workItemId, onIssued }: { workItemId: string; onIssued: () => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  return (
    <div className="sg-stack">
      <h2 className="sg-section-title">🏆 六关全部通过</h2>
      <p className="sg-muted">签发通关文牒：包含六关结论、共享证据索引与本地完整证据哈希。</p>
      {error ? <div className="sg-banner sg-banner--error">{error}</div> : null}
      <button
        className="sg-button sg-button--primary"
        disabled={busy}
        onClick={() => {
          setBusy(true);
          void rpc('passport.issue', { workItemId })
            .then(onIssued)
            .catch((reason) => setError(rpcErrorMessage(reason)))
            .finally(() => setBusy(false));
        }}
      >
        签发通关文牒
      </button>
    </div>
  );
}

function PassportView({ passport }: { passport: PassportInfo }) {
  return (
    <div className="sg-stack">
      <h2 className="sg-section-title">🏆 通关文牒</h2>
      <p className="sg-muted">对象哈希：<span className="sg-code">{passport.object_sha256.slice(0, 32)}…</span></p>
      <ol className="sg-gates">
        {passport.gates.map((gate) => (
          <li key={gate.gate} className="sg-gate sg-gate--passed">
            <span>{gateLabel(gate.gate)}</span>
            <span className="sg-status sg-status--passed">✓ {gate.evidence_ids.length} 证据</span>
          </li>
        ))}
      </ol>
    </div>
  );
}
