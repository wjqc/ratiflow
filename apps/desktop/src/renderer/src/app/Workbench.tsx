import { useCallback, useEffect, useState } from 'react';
import type { ReactNode } from 'react';
import { rpc, waitForRunTerminal } from '../rpc/client';
import type { TimelineEvent } from '../rpc/client';
import { gateLabel } from './ProjectSidebar';
import { relativeTime } from '../lib/format';
import {
  IconAlert,
  IconArrowLeft,
  IconBook,
  IconCheck,
  IconDoc,
  IconEdit,
  IconImage,
  IconMore,
  IconPaperclip,
  IconPlay,
  IconPlus,
  IconSearch,
  IconSend,
  IconShield,
  IconTarget,
  IconText,
} from '../components/Icons';
import DocGatePanel, { EvaluateButton } from './DocGatePanel';
import DevGatePanel from './DevGatePanel';
import DeployGatePanel from './DeployGatePanel';
import TracePanel from './TracePanel';
import {
  clearAutomaticPrd,
  draftPrd,
  finishPrdDraft,
  friendlyAgentError,
  readAutomaticPrd,
  rememberAutomaticPrd,
} from './prdDraft';

/* ---------------- 类型：与 Rust 序列化结构一一对应 ---------------- */

interface WorkItemDetail {
  id: string;
  project_id: string;
  title: string;
  description: string;
  current_gate: string;
  created_at: string;
  updated_at: string;
}

interface AttachmentInfo {
  id: string;
  kind: string;
  filename: string;
  content_type: string;
  parse_state: string;
  created_at: string;
}

interface StageInfo {
  gate: string;
  state: string;
  input_baseline_sha?: string;
  updated_at: string;
}

interface ProgressInfo {
  workItemId: string;
  title: string;
  currentGate: string;
  gateIndex: number;
  stages: StageInfo[];
  evidenceCount: number;
  pendingApprovals: number;
  blockedReason: string | null;
  updatedAt: string;
}

interface EvidenceInfo {
  id: string;
  gate: string;
  kind: string;
  title: string;
  verified: boolean;
  created_at: string;
}

/* ---------------- 常量 ---------------- */

const GATES = ['requirements', 'design', 'development', 'testing', 'deployment', 'verification'] as const;
type Gate = (typeof GATES)[number];

const GATE_SUBS: Record<Gate, string> = {
  requirements: '需求澄清与 PRD',
  design: '产品与技术方案',
  development: '编码实现与自测',
  testing: '集成测试与质量验证',
  deployment: '发布与环境准备',
  verification: '验收与交付确认',
};

const GATE_STATE_LABELS: Record<string, string> = {
  pending: '未开始',
  in_progress: '进行中',
  blocked: '被阻塞',
  passed: '已通过',
  failed: '未通过',
  cancelled: '已取消',
};

type StageVisual = 'done' | 'active' | 'blocked' | 'failed' | 'idle';

function stageVisual(state?: string): StageVisual {
  switch (state) {
    case 'passed':
      return 'done';
    case 'in_progress':
      return 'active';
    case 'blocked':
      return 'blocked';
    case 'failed':
      return 'failed';
    default:
      return 'idle';
  }
}

/* ---------------- Workbench ---------------- */

export function Workbench({
  projectId,
  projectName,
  workItemId,
  knowledgeCount,
  onNavigate,
}: {
  projectId: string;
  projectName: string;
  workItemId: string;
  knowledgeCount: number;
  onNavigate: (
    r: { page: 'home' } | { page: 'approvals' } | { page: 'settings'; section: 'models' },
  ) => void;
}) {
  const [workItem, setWorkItem] = useState<WorkItemDetail | null>(null);
  const [progress, setProgress] = useState<ProgressInfo | null>(null);
  const [attachments, setAttachments] = useState<AttachmentInfo[]>([]);
  const [events, setEvents] = useState<TimelineEvent[]>([]);
  const [evidences, setEvidences] = useState<EvidenceInfo[]>([]);
  const [error, setError] = useState('');
  const [view, setView] = useState<'timeline' | 'gate'>('timeline');
  const [prdState, setPrdState] = useState<'idle' | 'drafting' | 'ready' | 'failed'>('idle');
  const [prdMessage, setPrdMessage] = useState('');

  const loadAll = useCallback(async () => {
    try {
      const [detail, prog, atts, tl, evs] = await Promise.all([
        rpc<{ workItem: WorkItemDetail; stages: StageInfo[] }>('workitem.get', { workItemId }),
        rpc<ProgressInfo>('workitem.progress', { workItemId }),
        rpc<{ items: AttachmentInfo[] }>('attachment.list', { workItemId }),
        rpc<{ events: TimelineEvent[]; latest: number }>('timeline.snapshot', { workItemId, afterSeq: 0 }),
        rpc<{ items: EvidenceInfo[] }>('evidence.list', { workItemId }),
      ]);
      setWorkItem(detail.workItem);
      setProgress(prog);
      setAttachments(atts.items ?? []);
      setEvents(tl.events ?? []);
      setEvidences(evs.items ?? []);
      setError('');
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [workItemId]);

  useEffect(() => {
    void loadAll();
  }, [loadAll]);

  // 新建任务会立即启动 PRD Agent；工作台负责恢复运行、保存结果并打开需求关。
  useEffect(() => {
    const pending = readAutomaticPrd(workItemId);
    if (!pending) return;
    if (pending.state === 'failed' || !pending.runId) {
      setPrdState('failed');
      setPrdMessage(pending.error || '自动起草未能启动，请检查模型后重试。');
      return;
    }

    let cancelled = false;
    setPrdState('drafting');
    setPrdMessage('需求已保存，Agent 正在结合项目知识库起草 PRD…');
    void finishPrdDraft(workItemId, pending.runId)
      .then(async () => {
        if (cancelled) return;
        clearAutomaticPrd(workItemId);
        setPrdState('ready');
        setPrdMessage('PRD 草稿已生成并保存，等待你的审阅。');
        await loadAll();
        setView('gate');
      })
      .catch((reason) => {
        if (cancelled) return;
        const message = friendlyAgentError(reason);
        rememberAutomaticPrd(workItemId, { state: 'failed', error: message });
        setPrdState('failed');
        setPrdMessage(message);
      });
    return () => {
      cancelled = true;
    };
  }, [loadAll, workItemId]);

  // F02 事件驱动刷新：sg:event 推送触发对账拉取；30s 轮询仅作断线降级。
  useEffect(() => {
    const off = window.sixgates.onEvent(() => {
      void loadAll();
    });
    const t = setInterval(loadAll, 30000);
    return () => {
      off();
      clearInterval(t);
    };
  }, [loadAll]);

  const stagesByGate = new Map((progress?.stages ?? []).map((s) => [s.gate, s]));
  const currentGate: Gate = GATES.includes(progress?.currentGate as Gate)
    ? (progress?.currentGate as Gate)
    : 'requirements';
  const currentStage = stagesByGate.get(currentGate);
  const gateEvidences = evidences.filter((e) => e.gate === currentGate);

  if (error && !workItem) {
    return (
      <div className="sg-scroll">
        <div className="sg-banner sg-banner--error">无法打开任务：{error}</div>
      </div>
    );
  }

  return (
    <div className="sg-workbench">
      {view === 'timeline' ? (
        <div className="sg-workbench-center">
          <header className="sg-workbench-head">
            <div className="sg-workbench-crumb">
              <button
                className="sg-icon-btn"
                title="返回需求入口"
                aria-label="返回需求入口"
                onClick={() => onNavigate({ page: 'home' })}
              >
                <IconArrowLeft size={15} />
              </button>
              <span>{projectName || '项目'}</span>
              <span className="sg-workbench-crumb-sep">›</span>
              <span>{gateLabel(currentGate)}</span>
            </div>
            <div className="sg-workbench-actions">
              <button className="sg-icon-btn" title="搜索（规划中）" aria-label="搜索" disabled>
                <IconSearch size={15} />
              </button>
              <button className="sg-icon-btn" title="更多操作（规划中）" aria-label="更多操作" disabled>
                <IconMore size={15} />
              </button>
            </div>
          </header>

          <div className="sg-workbench-scroll">
            {error && <div className="sg-banner sg-banner--error">{error}</div>}
            {prdState !== 'idle' ? (
              <div
                className={`sg-banner ${prdState === 'failed' ? 'sg-banner--error' : 'sg-banner--info'}`}
                role={prdState === 'failed' ? 'alert' : 'status'}
              >
                <span>{prdMessage}</span>
                {prdState === 'failed' ? (
                  <button
                    className="sg-link-btn"
                    onClick={() => onNavigate({ page: 'settings', section: 'models' })}
                  >
                    检查模型
                  </button>
                ) : null}
              </div>
            ) : null}
            {progress?.blockedReason && (
              <div className="sg-banner sg-banner--warn">
                <IconAlert size={14} />
                <span>
                  任务被阻塞：{progress.blockedReason}
                  {progress.pendingApprovals > 0 && (
                    <button
                      className="sg-link-btn"
                      style={{ marginLeft: 8 }}
                      onClick={() => onNavigate({ page: 'approvals' })}
                    >
                      去审批中心处理（{progress.pendingApprovals}）
                    </button>
                  )}
                </span>
              </div>
            )}
            {workItem && <RequirementCard workItem={workItem} attachments={attachments} />}
            <Timeline events={events} />
          </div>

          <Composer
            workItemId={workItemId}
            currentGate={currentGate}
            requirementText={[workItem?.title, workItem?.description].filter(Boolean).join('\n')}
            knowledgeCount={knowledgeCount}
            onChanged={loadAll}
            onOpenGate={() => setView('gate')}
            onOpenModels={() => onNavigate({ page: 'settings', section: 'models' })}
          />
        </div>
      ) : (
        <GateWorkspace
          gate={currentGate}
          stage={currentStage}
          progress={progress}
          evidences={gateEvidences}
          workItemId={workItemId}
          onChanged={loadAll}
          onBack={() => setView('timeline')}
          onOpenApprovals={() => onNavigate({ page: 'approvals' })}
        />
      )}

      <GoalPanel
        progress={progress}
        workItem={workItem}
        stagesByGate={stagesByGate}
        currentGate={currentGate}
        gateOpen={view === 'gate'}
        onOpenGate={() => setView('gate')}
      />
    </div>
  );
}

/* ---------------- 用户需求卡 ---------------- */

function RequirementCard({
  workItem,
  attachments,
}: {
  workItem: WorkItemDetail;
  attachments: AttachmentInfo[];
}) {
  const [expanded, setExpanded] = useState(false);
  return (
    <div className="sg-req-card">
      <div className="sg-req-card-head">
        <span className="sg-req-card-title">{workItem.title}</span>
        <span className="sg-req-card-meta">{relativeTime(workItem.created_at)} 创建</span>
      </div>
      {workItem.description && (
        <p className={`sg-req-card-body ${expanded ? 'sg-req-card-body--expanded' : ''}`}>
          {workItem.description}
        </p>
      )}
      {attachments.length > 0 && (
        <div className="sg-req-card-attachments">
          {attachments.map((a) => (
            <span className="sg-attach-chip" key={a.id} title={a.filename}>
              {a.content_type.startsWith('image/') ? <IconImage size={13} /> : <IconDoc size={13} />}
              {a.filename}
            </span>
          ))}
        </div>
      )}
      <div className="sg-req-card-foot">
        {workItem.description && (
          <button className="sg-link-btn" onClick={() => setExpanded((v) => !v)}>
            {expanded ? '收起' : '查看完整需求'}
          </button>
        )}
      </div>
    </div>
  );
}

/* ---------------- 时间线 ---------------- */

function Timeline({ events }: { events: TimelineEvent[] }) {
  if (events.length === 0) {
    return (
      <div className="sg-empty" style={{ padding: '32px 24px' }}>
        还没有进展。任务从需求关开始，先在底部说明期望，或切换到右侧「当前关」完成关卡步骤。
      </div>
    );
  }
  return (
    <div className="sg-timeline">
      <div className="sg-timeline-label">任务进展</div>
      {events.map((ev) => {
        const [cls, icon] = eventVisual(ev.type);
        return (
          <div className="sg-timeline-item" key={ev.sequence}>
            <div className="sg-timeline-rail">
              <div className={`sg-timeline-icon ${cls}`}>{icon}</div>
              <div className="sg-timeline-line" />
            </div>
            <div>
              <div className="sg-timeline-row">
                <span className="sg-timeline-summary">{ev.summary}</span>
                <span className="sg-timeline-time">{timelineTime(ev.occurredAt)}</span>
              </div>
            </div>
          </div>
        );
      })}
    </div>
  );
}

function timelineTime(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

function eventVisual(type: string): [string, ReactNode] {
  if (type.includes('create') || type.includes('create_with_source')) return ['', <IconPlus size={13} />];
  if (type.includes('context')) return ['sg-timeline-icon--info', <IconSearch size={13} />];
  if (type.includes('note') || type.includes('attachment') || type.includes('import'))
    return ['sg-timeline-icon--info', <IconEdit size={13} />];
  if (type.includes('artifact') || type.includes('freeze') || type.includes('diff'))
    return ['sg-timeline-icon--info', <IconDoc size={13} />];
  if (type.includes('knowledge') || type.includes('retrieval'))
    return ['sg-timeline-icon--info', <IconBook size={13} />];
  if (type.includes('approve') || type.includes('grant') || type.includes('approval'))
    return ['sg-timeline-icon--warn', <IconShield size={13} />];
  if (type.includes('stage') || type.includes('evaluate')) return ['sg-timeline-icon--warn', <IconPlay size={13} />];
  if (type.includes('block')) return ['sg-timeline-icon--danger', <IconAlert size={13} />];
  if (type.includes('pass') || type.includes('issue') || type.includes('verify'))
    return ['sg-timeline-icon--ok', <IconCheck size={13} />];
  return ['', <IconDoc size={13} />];
}

/* ---------------- 底部输入器 ---------------- */

function Composer({
  workItemId,
  currentGate,
  requirementText,
  knowledgeCount,
  onChanged,
  onOpenGate,
  onOpenModels,
}: {
  workItemId: string;
  currentGate: Gate;
  requirementText: string;
  knowledgeCount: number;
  onChanged: () => Promise<void>;
  onOpenGate: () => void;
  onOpenModels: () => void;
}) {
  const [text, setText] = useState('');
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState('');
  const [error, setError] = useState('');

  const submit = async () => {
    const message = text.trim();
    if (!message || busy) return;
    setBusy(true);
    setError('');
    setNotice(currentGate === 'requirements' ? '正在根据补充说明起草 PRD…' : 'Agent 正在处理…');
    try {
      if (currentGate === 'requirements') {
        await draftPrd(
          workItemId,
          `${requirementText}\n\n用户补充：\n${message}`,
          `composer-prd-${workItemId}-${Date.now()}`,
        );
        setNotice('PRD 草稿已生成并保存，等待你的审阅。');
        setText('');
        await onChanged();
        onOpenGate();
      } else {
        const started = await rpc<{ runId: string }>('stage.startActivity', {
          workItemId,
          gate: currentGate,
          goal: message,
          toolAllowlist: ['read_file', 'search_knowledge'],
          idempotencyKey: `composer-${workItemId}-${currentGate}-${Date.now()}`,
        });
        const run = await waitForRunTerminal(started.runId);
        if (run.status !== 'completed_execution') {
          throw new Error(`Agent 状态 ${run.status}：${run.result}`);
        }
        setNotice('Agent 已完成处理。请进入当前关查看并确认产出。');
        setText('');
        await onChanged();
        onOpenGate();
      }
    } catch (reason) {
      setNotice('');
      setError(friendlyAgentError(reason));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="sg-composer">
      {error ? (
        <div className="sg-banner sg-banner--error" role="alert">
          <span>{error}</span>
          <button className="sg-link-btn" onClick={onOpenModels}>检查模型</button>
        </div>
      ) : null}
      {notice ? <div className="sg-composer-status" role="status">{notice}</div> : null}
      <div className="sg-composer-chips">
        <span className="sg-composer-chip">
          <IconBook size={12} />
          本次上下文：知识库（{knowledgeCount} 个来源）
        </span>
      </div>
      <div className="sg-composer-box">
        <textarea
          className="sg-composer-input"
          placeholder="对当前任务补充说明，或提出修改意见…"
          rows={2}
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
              e.preventDefault();
              void submit();
            }
          }}
          aria-label="补充说明"
        />
        <div className="sg-composer-tools">
          <div className="sg-composer-tool-group">
            <button className="sg-composer-tool" title="粘贴附件（规划中）" aria-label="附件" disabled>
              <IconPaperclip size={15} />
            </button>
            <button className="sg-composer-tool" title="插入文本（规划中）" aria-label="文本" disabled>
              <IconText size={15} />
            </button>
            <button className="sg-composer-tool" title="插入截图（规划中）" aria-label="截图" disabled>
              <IconImage size={15} />
            </button>
            <button className="sg-composer-tool" title="插入文档（规划中）" aria-label="文档" disabled>
              <IconDoc size={15} />
            </button>
            <button className="sg-composer-tool" title="引用知识库（规划中）" aria-label="知识库" disabled>
              <IconBook size={15} />
            </button>
          </div>
          <button
            className="sg-btn sg-btn--primary sg-btn--sm"
            title={busy ? '处理中…' : '发送（⌘↵）'}
            disabled={busy || text.trim() === ''}
            onClick={() => void submit()}
          >
            <IconSend size={13} />
            {busy ? '处理中…' : '发送'}
          </button>
        </div>
      </div>
    </div>
  );
}

/* ---------------- 右侧目标面板（六关总览） ---------------- */

function GoalPanel({
  progress,
  workItem,
  stagesByGate,
  currentGate,
  gateOpen,
  onOpenGate,
}: {
  progress: ProgressInfo | null;
  workItem: WorkItemDetail | null;
  stagesByGate: Map<string, StageInfo>;
  currentGate: Gate;
  gateOpen: boolean;
  onOpenGate: () => void;
}) {
  const passedCount = GATES.filter((g) => stagesByGate.get(g)?.state === 'passed').length;
  const allPassed = passedCount === GATES.length;
  return (
    <aside className="sg-inspector">
      <div className="sg-inspector-block">
        <div className="sg-inspector-title">
          <IconTarget size={14} />
          本次目标
        </div>
        <div className="sg-inspector-goal">{workItem?.title ?? '加载中…'}</div>
        <div className="sg-inspector-progress">{progress ? `${passedCount}/6 关已通过` : '—'}</div>
      </div>

      <div className="sg-inspector-block">
        <div className="sg-inspector-label">六关总览</div>
        {GATES.map((g) => {
          const state = stagesByGate.get(g)?.state;
          const v = stageVisual(state);
          const cls = v === 'idle' ? '' : ` sg-stage-item--${v}`;
          return (
            <div
              className={`sg-stage-item${cls}${g === currentGate ? ' sg-stage-item--current' : ''}`}
              key={g}
            >
              <span className={`sg-stage-icon sg-stage-icon--${v}`}>{gateIconContent(v)}</span>
              <div>
                <div className="sg-stage-name">{gateLabel(g)}</div>
                <div className="sg-stage-sub">{GATE_SUBS[g]}</div>
              </div>
              <span className="sg-stage-state">{GATE_STATE_LABELS[state ?? 'pending']}</span>
            </div>
          );
        })}
      </div>

      <div style={{ padding: '0 16px 16px' }}>
        {allPassed ? (
          <div className="sg-banner sg-banner--ok">
            <IconCheck size={14} />
            <span>六关全部通过，本任务可进入归档。</span>
          </div>
        ) : gateOpen ? (
          <div className="sg-inspector-current">
            正在查看{gateLabel(currentGate)}，完成草稿与检查后再提交放行。
          </div>
        ) : (
          <button className="sg-btn sg-btn--primary" style={{ width: '100%' }} onClick={onOpenGate}>
            进入当前关：{gateLabel(currentGate)}
          </button>
        )}
      </div>
    </aside>
  );
}

function gateIconContent(v: StageVisual): ReactNode {
  if (v === 'done') return <IconCheck size={11} />;
  if (v === 'active') return <span className="sg-stage-pulse" />;
  if (v === 'blocked' || v === 'failed') return <IconAlert size={10} />;
  return null;
}

/* ---------------- 当前关工作区 ---------------- */

function GateWorkspace({
  gate,
  stage,
  progress,
  evidences,
  workItemId,
  onChanged,
  onBack,
  onOpenApprovals,
}: {
  gate: Gate;
  stage?: StageInfo;
  progress: ProgressInfo | null;
  evidences: EvidenceInfo[];
  workItemId: string;
  onChanged: () => void;
  onBack: () => void;
  onOpenApprovals: () => void;
}) {
  return (
    <div className="sg-workbench-center">
      <div className="sg-gate-head">
        <div className="sg-workbench-crumb">
          <button className="sg-icon-btn" title="返回任务进展" aria-label="返回任务进展" onClick={onBack}>
            <IconArrowLeft size={15} />
          </button>
          <span>
            当前关：{gateLabel(gate)} · {GATE_SUBS[gate]}
          </span>
        </div>
        <span
          className={`sg-chip ${
            stage?.state === 'passed'
              ? 'sg-chip--ok'
              : stage?.state === 'in_progress'
                ? 'sg-chip--info'
                : stage?.state === 'blocked' || stage?.state === 'failed'
                  ? 'sg-chip--danger'
                  : ''
          }`}
        >
          {GATE_STATE_LABELS[stage?.state ?? 'pending']}
        </span>
      </div>

      <div className="sg-gate-workspace">
        <GateStepper stage={stage} />
        <GateChecksCard stage={stage} progress={progress} />

        {gate === 'development' && <DevGatePanel workItemId={workItemId} onDone={onChanged} />}
        {gate === 'deployment' && (
          <DeployGatePanel workItemId={workItemId} onDone={onChanged} onOpenApprovals={onOpenApprovals} />
        )}
        <DocGatePanel workItemId={workItemId} gate={gate} onDone={onChanged} />
        {gate === 'verification' && <AcceptancePanel workItemId={workItemId} progress={progress} />}

        <TracePanel workItemId={workItemId} />

        <div className="sg-card">
          <div className="sg-card-head">
            本关证据
            <span className="sg-card-extra">{evidences.length} 条</span>
          </div>
          {evidences.length === 0 ? (
            <div className="sg-empty" style={{ padding: '20px 24px' }}>
              暂无证据。完成上方步骤后会自动记录。
            </div>
          ) : (
            <div style={{ padding: '8px 14px 12px', display: 'grid', gap: 6 }}>
              {evidences.map((e) => (
                <div
                  key={e.id}
                  style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 12.5 }}
                >
                  <span className={`sg-chip ${e.verified ? 'sg-chip--ok' : ''}`}>
                    {e.verified ? '已核验' : '待核验'}
                  </span>
                  <span style={{ fontWeight: 500 }}>{e.title}</span>
                  <span className="sg-muted">{e.kind}</span>
                  <span className="sg-muted" style={{ marginLeft: 'auto' }}>
                    {relativeTime(e.created_at)}
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>

      <div className="sg-gate-foot">
        <span className="sg-sub">
          就绪检查全部通过后可评估本关；评估依据为冻结产物与已核验证据。
        </span>
        <EvaluateButton workItemId={workItemId} gate={gate} busy={false} onDone={onChanged} />
      </div>
    </div>
  );
}

function GateStepper({ stage }: { stage?: StageInfo }) {
  const v = stageVisual(stage?.state);
  return (
    <div className="sg-card" style={{ marginBottom: 12 }}>
      <div className="sg-gate-stepper">
        <div className={`sg-gate-step${v !== 'idle' ? ' sg-gate-step--done' : ''}`}>
          <span className="sg-gate-step-dot" />
          准备输入
        </div>
        <div className="sg-gate-step-sep" />
        <div
          className={`sg-gate-step${
            v === 'done' ? ' sg-gate-step--done' : v === 'active' ? ' sg-gate-step--active' : ''
          }`}
        >
          <span className="sg-gate-step-dot" />
          就绪检查
        </div>
        <div className="sg-gate-step-sep" />
        <div className={`sg-gate-step${v === 'done' ? ' sg-gate-step--done' : ''}`}>
          <span className="sg-gate-step-dot" />
          评估与放行
        </div>
      </div>
    </div>
  );
}

function GateChecksCard({ stage, progress }: { stage?: StageInfo; progress: ProgressInfo | null }) {
  const checks: { name: string; ok: boolean | null }[] = [
    { name: '输入基线已冻结', ok: stage?.input_baseline_sha ? true : null },
    { name: '产物已生成并通过校验', ok: null },
    {
      name: '证据已核验',
      ok: progress && progress.evidenceCount > 0 ? true : null,
    },
    { name: '无未决高风险审批', ok: progress ? progress.pendingApprovals === 0 : null },
  ];
  return (
    <div className="sg-card">
      <div className="sg-card-head">就绪检查</div>
      <div className="sg-checks">
        {checks.map((c) => (
          <div className="sg-check" key={c.name}>
            <span
              className={`sg-check-icon ${
                c.ok === true ? 'sg-check-icon--ok' : c.ok === false ? 'sg-check-icon--fail' : ''
              }`}
            >
              {c.ok === true && <IconCheck size={10} />}
            </span>
            {c.name}
          </div>
        ))}
      </div>
    </div>
  );
}

/* ---------------- 验证关 ---------------- */

interface PassportInfo {
  id: string;
  workitem_id: string;
  object_sha256: string;
  created_at: string;
  gates: { gate: string; passed: boolean; evidence_ids: string[]; failed_inputs: string[] }[];
}

function AcceptancePanel({
  workItemId,
  progress,
}: {
  workItemId: string;
  progress: ProgressInfo | null;
}) {
  const [passport, setPassport] = useState<PassportInfo | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const stagesByGate = new Map((progress?.stages ?? []).map((s) => [s.gate, s]));
  const allPassed = GATES.every((g) => stagesByGate.get(g)?.state === 'passed');

  const issue = async () => {
    setBusy(true);
    setError('');
    try {
      const p = await rpc<PassportInfo>('passport.issue', { workItemId });
      setPassport(p);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="sg-card">
      <div className="sg-card-head">验收与通关凭证</div>
      <div style={{ padding: '12px 14px', display: 'grid', gap: 10 }}>
        <div className="sg-checks">
          {GATES.map((g) => {
            const passed = stagesByGate.get(g)?.state === 'passed';
            return (
              <div className="sg-check" key={g}>
                <span className={`sg-check-icon ${passed ? 'sg-check-icon--ok' : ''}`}>
                  {passed && <IconCheck size={10} />}
                </span>
                {gateLabel(g)} {GATE_STATE_LABELS[stagesByGate.get(g)?.state ?? 'pending']}
              </div>
            );
          })}
        </div>
        {error && <div className="sg-banner sg-banner--error">{error}</div>}
        {passport ? (
          <div className="sg-banner sg-banner--ok">
            <IconCheck size={14} />
            <span>
              通关凭证已签发：{passport.id}（{passport.created_at}）
            </span>
          </div>
        ) : (
          <button
            className="sg-btn sg-btn--primary"
            disabled={!allPassed || busy}
            title={allPassed ? '签发通关凭证' : '六关全部通过后可签发'}
            onClick={() => void issue()}
          >
            {busy ? '签发中…' : '签发通关凭证'}
          </button>
        )}
      </div>
    </div>
  );
}
