import { useCallback, useEffect, useRef, useState } from 'react';
import type { MouseEvent as ReactMouseEvent, ReactNode } from 'react';
import { rpc, waitForRunTerminal } from '../rpc/client';
import type { TimelineEvent } from '../rpc/client';
import { gateLabel } from './ProjectSidebar';
import { formatDateTime, relativeTime } from '../lib/format';
import {
  IconAlert,
  IconArrowLeft,
  IconChevronDown,
  IconBook,
  IconCheck,
  IconCpu,
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
  IconX,
} from '../components/Icons';
import { ModelPicker } from './ModelPicker';
import RecoveryPanel from './RecoveryPanel';
import TaskGovernancePanel from './TaskGovernancePanel';
import TracePanel from './TracePanel';
import { renderMarkdown } from '../lib/markdown';
import { isDeltaEvent, streamBuffer, type RunStreamSnapshot } from '../lib/streamBuffer';
import { useTwoStepConfirm } from './settings/components/useTwoStepConfirm';
import {
  clearAutomaticPrd,
  finishPrdDraft,
  friendlyAgentError,
  readAutomaticPrd,
  rememberAutomaticPrd,
  startPrdDraft,
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

// M1 门禁数据化（评审 P0-4）：Gate 放宽为 string——自定义模板关 id 原样贯穿 UI，
// 不再静默回退 requirements；六关映射仅作 legacy 展示兜底。
type Gate = string;

const GATE_SUBS: Record<string, string> = {
  requirements: '需求澄清与 PRD',
  design: '产品与技术方案',
  development: '编码实现与自测',
  testing: '集成测试与质量验证',
  deployment: '发布与环境准备',
  verification: '验收与交付确认',
};

const GATE_STATE_LABELS: Record<string, string> = {
  pending: '未开始',
  not_started: '未开始',
  in_progress: '进行中',
  running: '进行中',
  prepared: '已准备',
  awaiting_approval: '等待放行',
  blocked: '被阻塞',
  stale: '基线过期',
  passed: '已通过',
  failed: '未通过',
  cancelled: '已取消',
};

interface AgentRunInfo {
  id: string;
  goal: string;
  status: string;
  result: string;
  created_at: string;
  updated_at: string;
}

interface TraceStep {
  kind: string;
  seq: number;
  ts: string;
  name: string;
  summary?: string;
  proposeTs?: string;
  preview?: string;
}

interface RunTrace {
  steps: TraceStep[];
  checkpoints: { seq: number; createdAt: string }[];
}

/** 各关的本关要求（展示用清单，与门禁就绪检查互补）。 */
const GATE_REQUIREMENTS: Record<string, string[]> = {
  requirements: ['完成需求澄清与确认', '完成 PRD 并存储到知识库', '通过评审并放行'],
  design: ['完成技术方案设计', '方案评审通过并冻结基线', '通过评审并放行'],
  development: ['完成编码实现与自测', '通过代码评审', '通过评审并放行'],
  testing: ['完成集成测试', '缺陷清零或达成豁免', '通过评审并放行'],
  deployment: ['完成发布与环境准备', '部署到目标环境', '通过评审并放行'],
  verification: ['完成验收确认', '交付物归档', '通过验收并放行'],
};

/** 各关默认执行 Agent 名称（展示用）。 */
const GATE_AGENT: Record<string, string> = {
  requirements: '需求分析 Agent',
  design: '方案设计 Agent',
  development: '开发实施 Agent',
  testing: '质量校验 Agent',
  deployment: '部署执行 Agent',
  verification: '验收确认 Agent',
};

const RUN_STATUS: Record<string, { label: string; cls: string }> = {
  queued: { label: '排队中', cls: 'pending' },
  running: { label: '进行中', cls: 'active' },
  paused: { label: '等待审批', cls: 'blocked' },
  completed_execution: { label: '已完成', cls: 'done' },
  failed: { label: '失败', cls: 'failed' },
  cancelled: { label: '已取消', cls: 'pending' },
};

type StageVisual = 'done' | 'active' | 'blocked' | 'failed' | 'idle';

function stageVisual(state?: string): StageVisual {
  switch (state) {
    case 'passed':
      return 'done';
    case 'in_progress':
    case 'running':
    case 'awaiting_approval':
    case 'prepared':
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
  const [runs, setRuns] = useState<AgentRunInfo[]>([]);
  const [docs, setDocs] = useState<string[]>([]);
  const [trace, setTrace] = useState<RunTrace | null>(null);
  const [error, setError] = useState('');
  const [view, setView] = useState<'timeline' | 'gate'>('timeline');
  // 右下角悬浮详情卡（跟随当前关）：胶囊化状态。
  const [detailMinimized, setDetailMinimized] = useState(false);
  const [prdState, setPrdState] = useState<'idle' | 'drafting' | 'ready' | 'failed'>('idle');
  const [prdMessage, setPrdMessage] = useState('');

  const loadAll = useCallback(async () => {
    try {
      const [detail, prog, atts, tl, evs, runList, docList] = await Promise.all([
        rpc<{ workItem: WorkItemDetail; stages: StageInfo[] }>('workitem.get', { workItemId }),
        rpc<ProgressInfo>('workitem.progress', { workItemId }),
        rpc<{ items: AttachmentInfo[] }>('attachment.list', { workItemId }),
        rpc<{ events: TimelineEvent[]; latest: number }>('timeline.snapshot', { workItemId, afterSeq: 0 }),
        rpc<{ items: EvidenceInfo[] }>('evidence.list', { workItemId }),
        rpc<{ items: AgentRunInfo[] }>('agent.list', { workItemId, limit: 6 }).catch(() => ({ items: [] })),
        rpc<{ items: string[] }>('workitem.documents', { workItemId }).catch(() => ({ items: [] })),
      ]);
      setWorkItem(detail.workItem);
      setProgress(prog);
      setAttachments(atts.items ?? []);
      setEvents(tl.events ?? []);
      setEvidences(evs.items ?? []);
      setRuns(runList.items ?? []);
      setDocs(docList.items ?? []);
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
      // 失败是终态：展示一次后即清除残留记录，避免每次进入任务都重复报“模型不可用”
      // （后续 run 可能早已成功）。失败详情仍可在任务进展时间线中追溯。
      clearAutomaticPrd(workItemId);
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
  // M2：流式 delta 是易失高频事件，进 streamBuffer 增量渲染，不触发全量刷新。
  useEffect(() => {
    const off = window.ratiflow.onEvent((event) => {
      if (isDeltaEvent(event)) {
        streamBuffer.ingest(event);
        return;
      }
      void loadAll();
    });
    const t = setInterval(loadAll, 30000);
    return () => {
      off();
      clearInterval(t);
    };
  }, [loadAll]);

  // 最新 Run 的执行轨迹：运行中每 1.5s 轮询（对话区实时展示推理/工具步骤），终态后拉一次。
  useEffect(() => {
    const latest = runs[0];
    if (!latest) {
      setTrace(null);
      return;
    }
    let cancelled = false;
    const fetchOnce = () => {
      rpc<RunTrace>('agent.trace', { runId: latest.id })
        .then((t) => {
          if (!cancelled) setTrace(t);
        })
        .catch(() => {});
    };
    void fetchOnce();
    const active = latest.status === 'running' || latest.status === 'queued';
    if (!active) return () => { cancelled = true; };
    const timer = setInterval(fetchOnce, 1500);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [runs]);

  const stagesByGate = new Map((progress?.stages ?? []).map((s) => [s.gate, s]));
  // 评审 P0-4：自定义关 id 原样使用（gateLabel 原样展示），仅在缺失时回退首关。
  const currentGate: Gate = progress?.currentGate || 'requirements';
  const currentStage = stagesByGate.get(currentGate);
  const gateEvidences = evidences.filter((e) => e.gate === currentGate);

  // 产出查看器：点对话区变更卡的“打开”后，工作台左右分栏展示内容；分割线可拖拽。
  const [openedDoc, setOpenedDoc] = useState<{ revisionId: string; title: string; meta: string } | null>(null);
  const [docContent, setDocContent] = useState('');
  const [splitRatio, setSplitRatio] = useState(0.55);
  const splitRef = useRef<HTMLDivElement>(null);

  const openRevision = (revisionId: string, title: string, meta: string) => {
    setOpenedDoc({ revisionId, title, meta });
    setDocContent('');
    rpc<{ content: string }>('artifact.revisionContent', { revisionId })
      .then((res) => setDocContent(res.content))
      .catch(() => setDocContent('（内容读取失败）'));
  };

  const startSplitDrag = (event: ReactMouseEvent) => {
    event.preventDefault();
    const onMove = (move: MouseEvent) => {
      const rect = splitRef.current?.getBoundingClientRect();
      if (!rect || rect.width === 0) return;
      const ratio = (move.clientX - rect.left) / rect.width;
      setSplitRatio(Math.min(0.8, Math.max(0.25, ratio)));
    };
    const onUp = () => {
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
  };

  if (error && !workItem) {
    return (
      <div className="sg-scroll">
        <div className="sg-banner sg-banner--error">无法打开任务：{error}</div>
      </div>
    );
  }

  const centerBody = (
    <>
      <div className="sg-workbench-scroll">
        {error && <div className="sg-banner sg-banner--error">{error}</div>}
        {prdState !== 'idle' ? (
          <div
            className={`sg-banner ${prdState === 'failed' ? 'sg-banner--error' : 'sg-banner--info'}`}
            role={prdState === 'failed' ? 'alert' : 'status'}
          >
            <span>{prdMessage}</span>
            {prdState === 'failed' ? (
              <>
                <button
                  className="sg-link-btn"
                  onClick={() => onNavigate({ page: 'settings', section: 'models' })}
                >
                  检查模型
                </button>
                <button
                  className="sg-link-btn"
                  onClick={() => {
                    setPrdState('idle');
                    setPrdMessage('');
                  }}
                >
                  关闭
                </button>
              </>
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
        <RecoveryPanel workItemId={workItemId} />
        <TaskGovernancePanel
          workItemId={workItemId}
          currentGate={currentGate}
          stages={progress?.stages ?? []}
          onChanged={loadAll}
        />
        <TracePanel workItemId={workItemId} />
        <Conversation runs={runs} trace={trace} workItemId={workItemId} onOpenRevision={openRevision} />
      </div>

      <Composer
        workItemId={workItemId}
        projectId={projectId}
        currentGate={currentGate}
        requirementText={[workItem?.title, workItem?.description].filter(Boolean).join('\n')}
        knowledgeCount={knowledgeCount}
        onChanged={loadAll}
        onOpenGate={() => setView('gate')}
        onOpenModels={() => onNavigate({ page: 'settings', section: 'models' })}
      />
    </>
  );

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

          {openedDoc ? (
            <div className="sg-split" ref={splitRef}>
              <div className="sg-split-left" style={{ flexBasis: `${splitRatio * 100}%` }}>
                {centerBody}
              </div>
              <div
                className="sg-split-divider"
                role="separator"
                aria-orientation="vertical"
                aria-label="拖拽调整布局"
                onMouseDown={startSplitDrag}
              />
              <div className="sg-split-viewer">
                <div className="sg-split-viewer-head">
                  <IconDoc size={14} />
                  <strong>{openedDoc.title}</strong>
                  <span className="sg-muted">{openedDoc.meta}</span>
                  <button
                    className="sg-icon-btn sg-split-close"
                    aria-label="关闭查看器"
                    title="关闭"
                    onClick={() => setOpenedDoc(null)}
                  >
                    <IconX size={14} />
                  </button>
                </div>
                <div className="sg-split-viewer-body">
                  <div
                    className="sg-md-body"
                    dangerouslySetInnerHTML={{ __html: renderMarkdown(docContent || '加载中…') }}
                  />
                </div>
              </div>
            </div>
          ) : (
            centerBody
          )}
        </div>
      ) : (
        <GateWorkspace
          gate={currentGate}
          stage={currentStage}
          progress={progress}
          evidences={gateEvidences}
          workItemId={workItemId}
          projectId={projectId}
          events={events}
          runs={runs}
          docs={docs}
          trace={trace}
          knowledgeCount={knowledgeCount}
          onChanged={loadAll}
          onBack={() => setView('timeline')}
          onOpenApprovals={() => onNavigate({ page: 'approvals' })}
        />
      )}

      <GateDetailFloat
        gate={currentGate}
        stagesByGate={stagesByGate}
        gateOpen={view === 'gate'}
        minimized={detailMinimized}
        onToggleMinimized={() => setDetailMinimized((v) => !v)}
        onOpenGate={() => setView('gate')}
      />
    </div>
  );
}

/* ---------------- Run 执行轨迹（推理摘要 + 工具调用 + 检查点） ---------------- */

/* ---------------- 执行过程（双栏：左时间线 / 右当前步骤面板） ---------------- */

function formatElapsed(fromIso: string, toIso?: string): string {
  const from = Date.parse(fromIso);
  const to = toIso ? Date.parse(toIso) : Date.now();
  if (Number.isNaN(from)) return '—';
  const total = Math.max(0, Math.floor((to - from) / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const pad = (n: number) => String(n).padStart(2, '0');
  return h > 0 ? `${pad(h)}:${pad(m)}:${pad(s)}` : `${pad(m)}:${pad(s)}`;
}

function ExecutionProcessView({
  workItemId,
  gate,
  stage,
  progress,
  events,
  runs,
  trace,
  docs,
  knowledgeCount,
}: {
  workItemId: string;
  gate: Gate;
  stage?: StageInfo;
  progress: ProgressInfo | null;
  events: TimelineEvent[];
  runs: AgentRunInfo[];
  trace: RunTrace | null;
  docs: string[];
  knowledgeCount: number;
}) {
  const [showReasoning, setShowReasoning] = useState(true);
  const [showFinal, setShowFinal] = useState(false);
  const steps = trace?.steps ?? [];
  const toolSteps = steps.filter((s) => s.kind === 'tool');
  const reasonSteps = steps.filter((s) => s.kind === 'reasoning');
  const latestReason = reasonSteps[reasonSteps.length - 1] ?? null;
  const failedRun = runs.find((r) => r.status === 'failed');
  const finishedRun = runs.find((r) => r.status === 'completed_execution');
  const activeRun = runs.find((r) => r.status === 'running' || r.status === 'queued') ?? runs[0] ?? null;

  // 进度：以迭代步数估算（预算 40 次工具调用上限对齐 max_iterations 语义），完成即 100%。
  const finished = activeRun?.status === 'completed_execution';
  const totalSteps = Math.max(7, steps.length + 1);
  const stepIndex = finished ? totalSteps : Math.min(steps.length, totalSteps - 1);
  const percent = finished ? 100 : Math.round((stepIndex / totalSteps) * 100);
  const elapsedFrom = activeRun?.created_at ?? stage?.updated_at;
  const elapsedTo = finished ? activeRun?.updated_at : undefined;
  const elapsed = elapsedFrom ? formatElapsed(elapsedFrom, elapsedTo) : '—';
  const remaining =
    !finished && percent > 0 && percent < 90 && elapsedFrom
      ? formatElapsed(elapsedFrom, new Date().toISOString())
      : null;

  const inputDocs = docs.filter((d) => d.startsWith('requirement'));
  const outputDocs = docs.filter((d) => !d.startsWith('requirement'));

  return (
    <div className="sg-process-grid">
      <InstanceGatesCard workItemId={workItemId} />
      <PlanCard workItemId={workItemId} />
      <CockpitCard workItemId={workItemId} />
      {/* 左：执行时间线 */}
      <div className="sg-card sg-process-left">
        <div className="sg-card-head">执行时间线</div>
        {steps.length === 0 ? (
          <div className="sg-empty" style={{ padding: '20px 16px' }}>
            Agent 尚未开始执行。在下方输入补充说明即可启动。
          </div>
        ) : (
          <div className="sg-process-timeline">
            {steps.map((step, index) => {
              const isLast = index === steps.length - 1;
              const done = step.kind === 'tool' || index < steps.length - 1 || finished;
              return (
                <div className="sg-process-tl-row" key={`${step.seq}-${index}`}>
                  <span className="sg-process-tl-time">{timelineTime(step.ts)}</span>
                  <span
                    className={`sg-process-tl-dot ${
                      isLast && !finished ? 'sg-process-tl-dot--active' : done ? 'sg-process-tl-dot--done' : ''
                    }`}
                  />
                  <div className="sg-process-tl-body">
                    <div className="sg-process-tl-name">
                      {index + 1}. {step.name === 'final' ? '生成最终结果' : step.name}
                    </div>
                    <div className={`sg-process-tl-state ${done ? '' : 'sg-process-tl-state--pending'}`}>
                      {isLast && !finished ? '进行中' : '已完成'}
                    </div>
                  </div>
                </div>
              );
            })}
          </div>
        )}
        {steps.length === 0 ? <Timeline events={events} /> : null}
      </div>

      {/* 右：当前步骤面板 */}
      <div className="sg-process-right">
        <div className="sg-card sg-process-current">
          <div className="sg-process-current-head">
            当前步骤 {stepIndex}/{totalSteps}：
            {latestReason ? (latestReason.name === 'final' ? '生成最终结果' : latestReason.name) : '等待启动'}
          </div>
          <div className="sg-process-progress">
            <div className="sg-process-progress-bar">
              <div className="sg-process-progress-fill" style={{ width: `${percent}%` }} />
            </div>
            <span className="sg-process-progress-pct">{finished ? 100 : percent}%</span>
          </div>
          <div className="sg-process-progress-meta">
            <span>⏱ 已用时 {elapsed}</span>
            {remaining ? <span>↑ 预计剩余 {remaining}</span> : null}
          </div>

          {latestReason?.summary ? (
            <div className="sg-process-section">
              <button className="sg-process-collapse" onClick={() => setShowReasoning((v) => !v)}>
                <span className={`sg-caret${showReasoning ? ' sg-caret--open' : ''}`}>⌄</span>
                模型推理摘要
              </button>
              {showReasoning ? (
                <div className="sg-process-reasoning">{latestReason.summary}</div>
              ) : null}
            </div>
          ) : null}

          <div className="sg-process-cards">
            <div className="sg-process-info">
              <div className="sg-process-info-head">工具调用</div>
              {toolSteps.length === 0 ? (
                <div className="sg-process-info-empty">暂无工具调用</div>
              ) : (
                <>
                  {toolSteps.map((s, i) => {
                    const ms = s.proposeTs && s.ts ? Math.max(0, Date.parse(s.ts) - Date.parse(s.proposeTs)) : null;
                    return (
                      <div className="sg-process-tool" key={`${s.seq}-${i}`}>
                        <span className="sg-process-tool-name">{s.name}</span>
                        <span className="sg-process-tool-dur">
                          {ms !== null ? (ms >= 1000 ? `${(ms / 1000).toFixed(2)}s` : `${ms}ms`) : ''}
                        </span>
                      </div>
                    );
                  })}
                  {toolSteps.length > 3 ? (
                    <div className="sg-process-info-more">查看全部工具调用（{toolSteps.length}）</div>
                  ) : null}
                </>
              )}
            </div>
            <div className="sg-process-info">
              <div className="sg-process-info-head">输入（Input）</div>
              {(inputDocs.length ? inputDocs : ['requirement.md']).map((d) => (
                <div className="sg-process-file" key={d}>
                  <IconDoc size={12} /> {d}
                </div>
              ))}
              <div className="sg-process-file">
                <IconBook size={12} /> 知识库（{knowledgeCount} 个来源）
              </div>
            </div>
            <div className="sg-process-info">
              <div className="sg-process-info-head">输出（Output）</div>
              {outputDocs.length === 0 ? (
                <div className="sg-process-info-empty">暂无产出文件</div>
              ) : (
                <>
                  {outputDocs.slice(0, 3).map((d) => (
                    <div className="sg-process-file" key={d}>
                      <IconDoc size={12} /> {d}
                    </div>
                  ))}
                  {outputDocs.length > 3 ? (
                    <div className="sg-process-info-more">+ {outputDocs.length - 3} 个文件</div>
                  ) : null}
                </>
              )}
            </div>
            <div className="sg-process-info">
              <div className="sg-process-info-head">检查点（Checkpoint）</div>
              {(trace?.checkpoints ?? []).length === 0 ? (
                <div className="sg-process-info-empty">暂无检查点</div>
              ) : (
                <>
                  {(trace?.checkpoints ?? []).slice(0, 2).map((c) => (
                    <div className="sg-process-file" key={c.seq}>
                      已保存中间结果
                      <span className="sg-process-tool-dur">{timelineTime(c.createdAt)}</span>
                    </div>
                  ))}
                  {(trace?.checkpoints ?? []).length > 2 ? (
                    <div className="sg-process-info-more">
                      查看全部检查点（{trace?.checkpoints.length}）
                    </div>
                  ) : null}
                </>
              )}
            </div>
          </div>

          <div className="sg-process-section sg-process-section--row">
            <span className="sg-process-ok-icon">
              <IconCheck size={11} />
            </span>
            错误与警告
            <span className="sg-process-section-note">
              {failedRun ? failedRun.result : '当前无错误或警告'}
            </span>
          </div>

          <div className="sg-process-section">
            <button className="sg-process-collapse" onClick={() => setShowFinal((v) => !v)}>
              <span className={`sg-caret${showFinal ? ' sg-caret--open' : ''}`}>⌄</span>
              最终结果
              <span className="sg-process-section-note">
                {finishedRun ? '' : '（当前运行尚未完成）'}
              </span>
            </button>
            {showFinal ? (
              <div className="sg-process-reasoning">
                {finishedRun?.result || '完成所有步骤后将生成最终结果并供审批。'}
              </div>
            ) : null}
          </div>
        </div>
      </div>
    </div>
  );
}

function RunTraceView({ trace }: { trace: RunTrace | null }) {
  if (!trace || trace.steps.length === 0) return null;
  return (
    <>
      <div className="sg-card">
        <div className="sg-card-head">
          Agent 执行轨迹
          <span className="sg-card-extra">{trace.steps.length} 步</span>
        </div>
        <div className="sg-trace-list">
          {trace.steps.map((step, index) => {
            const durationMs =
              step.kind === 'tool' && step.proposeTs && step.ts
                ? Math.max(0, Date.parse(step.ts) - Date.parse(step.proposeTs))
                : null;
            return (
              <div className="sg-trace-step" key={`${step.seq}-${index}`}>
                <span className={`sg-trace-kind sg-trace-kind--${step.kind}`}>
                  {step.kind === 'tool' ? '工具' : '推理'}
                </span>
                <div className="sg-trace-body">
                  <div className="sg-trace-name">
                    {step.name === 'final' ? '生成最终结果' : step.name}
                    {durationMs !== null ? (
                      <span className="sg-trace-duration">
                        {durationMs >= 1000 ? `${(durationMs / 1000).toFixed(1)}s` : `${durationMs}ms`}
                      </span>
                    ) : null}
                  </div>
                  {step.summary ? <div className="sg-trace-summary">{step.summary}</div> : null}
                  {step.preview ? <div className="sg-trace-summary sg-trace-preview">{step.preview}</div> : null}
                </div>
                <span className="sg-trace-time">{timelineTime(step.ts)}</span>
              </div>
            );
          })}
        </div>
      </div>
      {trace.checkpoints.length > 0 ? (
        <div className="sg-card">
          <div className="sg-card-head">
            检查点
            <span className="sg-card-extra">{trace.checkpoints.length} 个</span>
          </div>
          <div style={{ padding: '8px 14px 12px', display: 'grid', gap: 6 }}>
            {trace.checkpoints.map((c) => (
              <div key={c.seq} style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 12.5 }}>
                <span className="sg-chip">seq {c.seq}</span>
                <span>已保存中间结果</span>
                <span className="sg-muted" style={{ marginLeft: 'auto' }}>
                  {timelineTime(c.createdAt)}
                </span>
              </div>
            ))}
          </div>
        </div>
      ) : null}
    </>
  );
}

/* ---------------- 选中关详情悬浮框（可缩为胶囊） ---------------- */

function GateDetailFloat({
  gate,
  stagesByGate,
  gateOpen,
  minimized,
  onToggleMinimized,
  onOpenGate,
}: {
  gate: Gate;
  stagesByGate: Map<string, StageInfo>;
  gateOpen: boolean;
  minimized: boolean;
  onToggleMinimized: () => void;
  onOpenGate: () => void;
}) {
  const state = stagesByGate.get(gate)?.state;
  const started = state !== undefined && state !== 'not_started' && state !== 'pending';
  const steps: { name: string; done: boolean | null }[] = [
    { name: '完成本关产物', done: state === 'passed' ? true : started ? false : null },
    { name: '通过就绪检查', done: state === 'passed' ? true : started ? false : null },
    { name: '评审通过并放行', done: state === 'passed' ? true : null },
  ];
  const nextAction =
    state === 'passed'
      ? '本关已通过，等待进入下一关。'
      : state === 'in_progress'
        ? '完成本关产物并通过就绪检查后提交放行。'
        : state === 'blocked'
          ? '先处理阻塞原因，再继续执行本关。'
          : state === 'failed'
            ? '修正失败原因后重新执行本关。'
            : '完成前置关卡后进入本关。';
  const chipCls =
    state === 'passed'
      ? 'sg-chip--ok'
      : state === 'in_progress'
        ? 'sg-chip--info'
        : state === 'blocked' || state === 'failed'
          ? 'sg-chip--danger'
          : '';
  const stateText = GATE_STATE_LABELS[state ?? 'pending'] ?? state ?? '';

  if (minimized) {
    return (
      <button className="sg-detail-capsule" onClick={onToggleMinimized} title="展开关卡详情">
        <IconTarget size={13} />
        <span>
          {gateLabel(gate)}
          {stateText ? ` · ${stateText}` : ''}
        </span>
        <span className="sg-detail-capsule-caret">⌃</span>
      </button>
    );
  }

  return (
    <div className="sg-detail-float">
      <div className="sg-detail-float-head">
        <strong>{gateLabel(gate)}</strong>
        {stateText ? <span className={`sg-chip ${chipCls}`}>{stateText}</span> : null}
        <button
          className="sg-detail-collapse"
          onClick={onToggleMinimized}
          title="收起为胶囊"
          aria-label="收起关卡详情"
        >
          <IconChevronDown size={14} />
        </button>
      </div>
      <div className="sg-gate-detail-label">执行步骤</div>
      {steps.map((s) => (
        <div className="sg-gate-detail-step" key={s.name}>
          <span className={`sg-check-icon ${s.done === true ? 'sg-check-icon--ok' : ''}`}>
            {s.done === true ? <IconCheck size={10} /> : null}
          </span>
          <span className={s.done === null ? 'sg-muted' : ''}>{s.name}</span>
          <span className="sg-gate-detail-state">
            {s.done === true ? '已完成' : s.done === false ? '进行中' : '待执行'}
          </span>
        </div>
      ))}
      <div className="sg-gate-detail-label">执行 Agent</div>
      <div className="sg-gate-detail-agent">{GATE_AGENT[gate] ?? '执行 Agent'}</div>
      <div className="sg-gate-detail-label">下一步行动</div>
      <div className="sg-gate-detail-next">{nextAction}</div>
      <div className={`sg-detail-float-note ${gateOpen ? '' : 'sg-detail-float-note--action'}`}>
        {gateOpen ? (
          `正在查看${gateLabel(gate)}，完成草稿与检查后再提交放行。`
        ) : (
          <button className="sg-link-btn" onClick={onOpenGate}>
            进入当前关：{gateLabel(gate)}
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

/* ---------------- 对话视图：我发送的 → 模型思考 → 完整回复 → 产出变更 ---------------- */

/** 从 run.goal 提取用户真正输入的文本（起草模板把需求/补充拼进了 goal）。 */
function extractUserMessage(goal: string): string {
  const supplement = goal.split(/用户补充：\s*/)[1];
  if (supplement) return supplement.trim();
  const requirement = goal.split(/用户需求：\s*/)[1];
  if (requirement) return requirement.trim();
  return goal.trim();
}

function Conversation({
  runs,
  trace,
  workItemId,
  onOpenRevision,
}: {
  runs: AgentRunInfo[];
  trace: RunTrace | null;
  workItemId: string;
  onOpenRevision: (revisionId: string, title: string, meta: string) => void;
}) {
  const ordered = [...runs].reverse();
  if (ordered.length === 0) {
    return (
      <div className="sg-empty" style={{ padding: '32px 24px' }}>
        还没有执行记录。在下方输入框发送消息，Agent 将开始处理并在此展示完整过程。
      </div>
    );
  }
  return (
    <div className="sg-conv">
      {ordered.map((run, index) => (
        <ConversationTurn
          key={run.id}
          run={run}
          isLatest={index === ordered.length - 1}
          latestTrace={trace}
          workItemId={workItemId}
          onOpenRevision={onOpenRevision}
        />
      ))}
    </div>
  );
}

function ConversationTurn({
  run,
  isLatest,
  latestTrace,
  workItemId,
  onOpenRevision,
}: {
  run: AgentRunInfo;
  isLatest: boolean;
  latestTrace: RunTrace | null;
  workItemId: string;
  onOpenRevision: (revisionId: string, title: string, meta: string) => void;
}) {
  const [showReasoning, setShowReasoning] = useState(isLatest);
  const [turnTrace, setTurnTrace] = useState<RunTrace | null>(isLatest ? latestTrace : null);
  const running = run.status === 'running' || run.status === 'queued';

  // M2 增量渲染：运行中订阅流式 delta 视图；终态后清缓冲（以 run.result 为准）。
  const [stream, setStream] = useState<RunStreamSnapshot>(() => streamBuffer.snapshot(run.id));
  useEffect(() => {
    if (!running) {
      streamBuffer.clear(run.id);
      return;
    }
    return streamBuffer.subscribe(run.id, setStream);
  }, [running, run.id]);

  useEffect(() => {
    if (isLatest) {
      setTurnTrace(latestTrace);
      return;
    }
  }, [isLatest, latestTrace]);

  // 历史 run 的思考轨迹按需拉取（展开“模型思考”时）。
  useEffect(() => {
    if (isLatest || !showReasoning || turnTrace) return;
    let cancelled = false;
    rpc<RunTrace>('agent.trace', { runId: run.id })
      .then((t) => {
        if (!cancelled) setTurnTrace(t);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [isLatest, showReasoning, turnTrace, run.id]);

  const reasonSteps = (turnTrace?.steps ?? []).filter((s) => s.kind === 'reasoning');
  const toolSteps = (turnTrace?.steps ?? []).filter((s) => s.kind === 'tool');
  const liveSteps = [...reasonSteps, ...toolSteps].sort((a, b) => a.seq - b.seq);

  return (
    <div className="sg-conv-turn">
      <div className="sg-conv-row sg-conv-row--user">
        <div className="sg-conv-bubble sg-conv-bubble--user">{extractUserMessage(run.goal)}</div>
      </div>
      <div className="sg-conv-row sg-conv-row--agent">
        <div className="sg-conv-bubble sg-conv-bubble--agent">
          <div className="sg-conv-agent-head">
            <IconCpu size={12} />
            <span>需求分析 Agent</span>
            <span className="sg-muted">
              {run.status === 'completed_execution'
                ? '已完成'
                : running
                  ? '思考与执行中…'
                  : run.status === 'failed'
                    ? '执行失败'
                    : '已取消'}
            </span>
          </div>

          {running ? (
            <div className="sg-conv-thinking">
              {stream.text ? (
                <div className="sg-conv-streaming" data-testid="sg-streaming-output">
                  {stream.text}
                  <span className="sg-conv-streaming-caret">▍</span>
                </div>
              ) : null}
              {liveSteps.length > 0 ? (
                liveSteps.slice(-4).map((s) => (
                  <div key={s.seq} className="sg-conv-step">
                    <span className={`sg-chip ${s.kind === 'tool' ? '' : 'sg-chip--ok'}`}>
                      {s.kind === 'tool' ? '工具' : '推理'}
                    </span>
                    <span className="sg-conv-step-name">{s.name}</span>
                    {s.summary ? <span className="sg-muted sg-conv-step-summary">{s.summary}</span> : null}
                  </div>
                ))
              ) : (
                <span className="sg-muted">模型推理中，首次产出可能需要一到两分钟…</span>
              )}
            </div>
          ) : null}

          {run.status === 'failed' ? (
            <div className="sg-banner sg-banner--error">
              {friendlyAgentError(`Agent 状态 ${run.status}：${run.result}`)}
            </div>
          ) : null}
          {run.status === 'cancelled' ? (
            <div className="sg-muted">已取消本次执行；你输入的内容已保留，可修改后重发。</div>
          ) : null}

          {reasonSteps.length > 0 ? (
            <div className="sg-conv-reason">
              <button className="sg-conv-collapse" onClick={() => setShowReasoning((v) => !v)}>
                {showReasoning ? '⌄' : '›'} 模型思考（{reasonSteps.length}）
              </button>
              {showReasoning ? (
                <div className="sg-conv-reason-body">
                  {reasonSteps.map((s) => (
                    <div key={s.seq} className="sg-conv-reason-item">
                      {s.summary || s.name}
                    </div>
                  ))}
                </div>
              ) : null}
            </div>
          ) : null}

          {run.status === 'completed_execution' && run.result ? (
            <div className="sg-md-body sg-conv-result" dangerouslySetInnerHTML={{ __html: renderMarkdown(run.result) }} />
          ) : null}

          {run.status === 'completed_execution' ? (
            <ChangedArtifacts workItemId={workItemId} since={run.created_at} onOpen={onOpenRevision} />
          ) : null}
        </div>
      </div>
    </div>
  );
}

interface RevisionRow {
  id: string;
  rev_no: number;
  status: string;
  size: number;
  content_sha256?: string;
  created_at: string;
}

/** 行级差异估算：按行多重集合比较（新增=新文本多出的行，删除=旧文本多出的行）。 */
function diffLineCounts(oldText: string, newText: string): { added: number; deleted: number } {
  const tally = (text: string) => {
    const map = new Map<string, number>();
    for (const line of text.split('\n')) map.set(line, (map.get(line) ?? 0) + 1);
    return map;
  };
  const oldLines = tally(oldText);
  const newLines = tally(newText);
  let added = 0;
  let deleted = 0;
  for (const [line, n] of newLines) {
    const base = oldLines.get(line) ?? 0;
    if (n > base) added += n - base;
  }
  for (const [line, n] of oldLines) {
    const base = newLines.get(line) ?? 0;
    if (n > base) deleted += n - base;
  }
  return { added, deleted };
}

/** 本次执行更新的产出卡片：默认收起只显示“N 个产出已更新 +A -D”，展开后逐修订展示路径与行级变更。 */
function ChangedArtifacts({
  workItemId,
  since,
  onOpen,
}: {
  workItemId: string;
  since: string;
  onOpen: (revisionId: string, title: string, meta: string) => void;
}) {
  const [entries, setEntries] = useState<
    { key: string; title: string; path: string; revLabel: string; added: number; deleted: number; revisionId: string; meta: string }[] | null
  >(null);
  const [expanded, setExpanded] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const page = await rpc<{ items: { id: string; kind: string; title: string }[] }>(
          'artifact.list',
          { workItemId },
        );
        const out: {
          key: string;
          title: string;
          path: string;
          revLabel: string;
          added: number;
          deleted: number;
          revisionId: string;
          meta: string;
        }[] = [];
        for (const art of page.items ?? []) {
          const revs = await rpc<{ items: RevisionRow[] }>('artifact.listRevisions', {
            artifactId: art.id,
          }).catch(() => ({ items: [] as RevisionRow[] }));
          const sorted = [...(revs.items ?? [])].sort((a, b) => a.rev_no - b.rev_no);
          for (const rev of sorted.filter((r) => r.created_at >= since)) {
            const prev = sorted.find((r) => r.rev_no === rev.rev_no - 1) ?? null;
            const [current, previous] = await Promise.all([
              rpc<{ content: string }>('artifact.revisionContent', { revisionId: rev.id }).catch(() => ({ content: '' })),
              prev
                ? rpc<{ content: string }>('artifact.revisionContent', { revisionId: prev.id }).catch(() => ({ content: '' }))
                : Promise.resolve({ content: '' }),
            ]);
            const { added, deleted } = diffLineCounts(previous.content, current.content);
            const sha = rev.content_sha256 ?? '';
            out.push({
              key: `${art.id}/${rev.id}`,
              title: art.title || art.kind,
              path: sha ? `objects/${sha.slice(0, 2)}/${sha}` : `artifacts/${art.kind}`,
              revLabel: `rev${rev.rev_no} · ${rev.status}`,
              added,
              deleted,
              revisionId: rev.id,
              meta: `rev${rev.rev_no} · ${rev.status}`,
            });
          }
        }
        if (!cancelled) setEntries(out);
      } catch {
        if (!cancelled) setEntries([]);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [workItemId, since]);

  if (entries === null || entries.length === 0) return null;
  const totalAdded = entries.reduce((n, e) => n + e.added, 0);
  const totalDeleted = entries.reduce((n, e) => n + e.deleted, 0);

  return (
    <div className="sg-conv-changes">
      <button
        className="sg-conv-changes-head sg-conv-changes-toggle"
        onClick={() => setExpanded((v) => !v)}
        aria-expanded={expanded}
      >
        <span className="sg-conv-chevron">{expanded ? '⌄' : '›'}</span>
        <IconDoc size={13} />
        <span>
          {entries.length} 个产出已更新
        </span>
        <span className="sg-conv-diff">+{totalAdded}</span>
        <span className="sg-conv-diff-del">-{totalDeleted}</span>
      </button>
      {expanded
        ? entries.map((e) => (
            <div key={e.key} className="sg-conv-change">
              <div className="sg-conv-change-row">
                <IconDoc size={13} />
                <span className="sg-conv-change-name">{e.title}</span>
                <span className="sg-conv-change-path">{e.path}</span>
                <span className="sg-conv-diff">+{e.added}</span>
                <span className="sg-conv-diff-del">-{e.deleted}</span>
                <button
                  className="sg-conv-change-open"
                  onClick={() => onOpen(e.revisionId, e.title, e.meta)}
                >
                  打开
                </button>
              </div>
            </div>
          ))
        : null}
    </div>
  );
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
  projectId,
  currentGate,
  requirementText,
  knowledgeCount,
  onChanged,
  onOpenGate,
  onOpenModels,
}: {
  workItemId: string;
  projectId: string;
  currentGate: Gate;
  requirementText: string;
  knowledgeCount: number;
  onChanged: () => Promise<void>;
  onOpenGate: () => void;
  onOpenModels: () => void;
}) {
  const [text, setText] = useState('');
  const [busy, setBusy] = useState(false);
  const [runId, setRunId] = useState('');
  const [notice, setNotice] = useState('');
  const [error, setError] = useState('');
  // “+”添加菜单：附件导入与 @ 知识来源引用（均接真实 RPC）。
  const [addMenuOpen, setAddMenuOpen] = useState(false);
  const [addMenuView, setAddMenuView] = useState<'root' | 'context'>('root');
  const [contextSources, setContextSources] = useState<{ id: string; name: string }[]>([]);
  const [addStatus, setAddStatus] = useState('');
  const addMenuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!addMenuOpen) return;
    const close = (event: MouseEvent) => {
      if (!addMenuRef.current?.contains(event.target as Node)) setAddMenuOpen(false);
    };
    window.addEventListener('mousedown', close);
    return () => window.removeEventListener('mousedown', close);
  }, [addMenuOpen]);

  // 附件：原生文件对话框 → attachment.import（对象存储 + 附件记录）。
  const importAttachment = async () => {
    setAddMenuOpen(false);
    const file = await window.ratiflow.selectFile();
    if (!file) return;
    try {
      await rpc('attachment.import', {
        workItemId,
        filename: file.filename,
        contentBase64: file.contentBase64,
      });
      setAddStatus(`附件已添加：${file.filename}`);
      await onChanged();
    } catch (reason) {
      setAddStatus(`附件添加失败：${reason instanceof Error ? reason.message : String(reason)}`);
    }
  };

  // @ 上下文：列出项目知识来源，点选后以 @名称 写入输入，随消息一起交给 Agent 检索。
  const openContextMenu = async () => {
    setAddMenuView('context');
    try {
      const res = await rpc<{ items: { id: string; name: string }[] }>('knowledge.list', { projectId });
      setContextSources(res.items ?? []);
    } catch {
      setContextSources([]);
    }
  };

  const mentionSource = (name: string) => {
    setAddMenuOpen(false);
    setText((prev) => `${prev}${prev && !prev.endsWith(' ') ? ' ' : ''}@${name} `);
  };

  // 长任务可取消（推理型模型一次生成可达数分钟）：调 agent.cancel，
  // 轮询会在下一拍见到 cancelled 终态。已终态时取消是 no-op。
  const cancelRun = async () => {
    if (!runId) return;
    try {
      await rpc('agent.cancel', { runId });
    } catch {
      /* run 已终态或 core 繁忙：忽略，轮询会收尾 */
    }
  };

  const submit = async () => {
    const message = text.trim();
    if (!message || busy) return;
    setBusy(true);
    setError('');
    setNotice(currentGate === 'requirements' ? '正在根据补充说明起草 PRD…' : 'Agent 正在处理…');
    try {
      if (currentGate === 'requirements') {
        const id = await startPrdDraft(
          workItemId,
          `${requirementText}\n\n用户补充：\n${message}`,
          `composer-prd-${workItemId}-${Date.now()}`,
        );
        setRunId(id);
        const run = await waitForRunTerminal(id);
        setRunId('');
        if (run.status === 'cancelled') {
          setNotice('已取消本次起草；你输入的内容已保留，可修改后重发。');
          return;
        }
        if (run.status !== 'completed_execution') {
          throw new Error(friendlyAgentError(`Agent 状态 ${run.status}：${run.result}`));
        }
        await finishPrdDraft(workItemId, id);
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
        setRunId(started.runId);
        const run = await waitForRunTerminal(started.runId);
        setRunId('');
        if (run.status === 'cancelled') {
          setNotice('已取消本次处理；你输入的内容已保留，可修改后重发。');
          return;
        }
        if (run.status !== 'completed_execution') {
          throw new Error(`Agent 状态 ${run.status}：${run.result}`);
        }
        setNotice('Agent 已完成处理。结果如下，可进入当前关查看产出。');
        setText('');
        await onChanged();
      }
    } catch (reason) {
      setNotice('');
      setError(friendlyAgentError(reason));
    } finally {
      setRunId('');
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
      {notice ? (
        <div className="sg-composer-status" role="status">
          <span>{notice}</span>
          {busy && runId ? (
            <button className="sg-link-btn" style={{ marginLeft: 8 }} onClick={() => void cancelRun()}>
              取消
            </button>
          ) : null}
        </div>
      ) : null}
      <div className="sg-composer-main">
        {addStatus ? (
          <div className="sg-compose-add-status" role="status">
            <span>{addStatus}</span>
            <button className="sg-link-btn" onClick={() => setAddStatus('')}>
              关闭
            </button>
          </div>
        ) : null}
        <textarea
          className="sg-composer-input"
          placeholder="提出后续修改要求"
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
        <div className="sg-composer-bar">
          <div className="sg-composer-bar-left">
            <div className="sg-compose-add" ref={addMenuRef}>
              <button
                className="sg-compose-add-btn"
                title="添加"
                aria-label="添加"
                aria-expanded={addMenuOpen}
                onClick={() => {
                  setAddMenuOpen((v) => {
                    if (!v) setAddMenuView('root');
                    return !v;
                  });
                }}
              >
                <IconPlus size={15} />
              </button>
              {addMenuOpen ? (
                <div className="sg-compose-add-menu" role="menu">
                  {addMenuView === 'root' ? (
                    <>
                      <button
                        className="sg-compose-add-item"
                        role="menuitem"
                        onClick={() => void importAttachment()}
                      >
                        <IconPaperclip size={14} />
                        <span>添加附件</span>
                      </button>
                      <button
                        className="sg-compose-add-item"
                        role="menuitem"
                        onClick={() => void openContextMenu()}
                      >
                        <IconText size={14} />
                        <span>使用 @ 添加上下文</span>
                      </button>
                    </>
                  ) : (
                    <>
                      <div className="sg-compose-add-title">引用知识来源</div>
                      {contextSources.length === 0 ? (
                        <div className="sg-compose-add-empty">
                          项目暂无知识来源，可到「知识库」添加后引用。
                        </div>
                      ) : (
                        contextSources.map((source) => (
                          <button
                            key={source.id}
                            className="sg-compose-add-item"
                            role="menuitem"
                            onClick={() => mentionSource(source.name)}
                          >
                            <IconBook size={14} />
                            <span>@{source.name}</span>
                          </button>
                        ))
                      )}
                    </>
                  )}
                </div>
              ) : null}
            </div>
            <span className="sg-composer-chip sg-composer-chip--flat">
              <IconBook size={12} />
              本次上下文：知识库（{knowledgeCount} 个来源）
            </span>
          </div>
          <div className="sg-composer-bar-right">
            <ModelPicker />
            <button
              className="sg-compose-send"
              title={busy ? '处理中…' : '发送（⌘↵）'}
              aria-label={busy ? '处理中' : '发送'}
              disabled={busy || text.trim() === ''}
              onClick={() => void submit()}
            >
              <IconSend size={15} />
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

/* ---------------- 右侧目标面板（六关总览） ---------------- */

function gateIconContent(v: StageVisual): ReactNode {
  if (v === 'done') return <IconCheck size={11} />;
  if (v === 'active') return <span className="sg-stage-pulse" />;
  if (v === 'blocked' || v === 'failed') return <IconAlert size={10} />;
  return null;
}

/* ---------------- 当前关工作区 ---------------- */

/** 交付物门禁状态：本关要求的所有 deliverable kind 均已冻结基线才放行（与 core request_release 前置一致）。 */
function DeliverableChip({
  gate,
  workItemId,
  onChanged,
}: {
  gate: Gate;
  workItemId: string;
  onChanged: () => void;
}) {
  const [status, setStatus] = useState<{
    requiredKind: string;
    requiredKinds?: string[];
    satisfied: boolean;
    missing: string | null;
    entries?: { kind: string; satisfied: boolean; missing: string | null }[];
  } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  const reload = useCallback(async () => {
    try {
      const s = await rpc<{
        requiredKind: string;
        requiredKinds?: string[];
        satisfied: boolean;
        missing: string | null;
        entries?: { kind: string; satisfied: boolean; missing: string | null }[];
      }>('gate.deliverableStatus', { workItemId, gate });
      setStatus(s);
    } catch {
      setStatus(null);
    }
  }, [gate, workItemId]);

  useEffect(() => {
    void reload();
  }, [reload, onChanged]);

  const importFile = async () => {
    const picked = await window.ratiflow.selectFile();
    if (!picked) return;
    setBusy(true);
    setError('');
    try {
      const content = new TextDecoder().decode(Uint8Array.from(atob(picked.contentBase64), (c) => c.charCodeAt(0)));
      // 导入目标 = 第一个未满足的 kind（多交付物按缺口顺序补齐）。
      const requiredKind =
        status?.entries?.find((e) => !e.satisfied)?.kind ?? status?.requiredKind ?? '';
      const existing = await rpc<{ items: Array<{ id: string; kind: string; title: string }> }>(
        'artifact.list',
        { workItemId },
      );
      const found = existing.items?.find((a) => a.kind === requiredKind) ?? null;
      const artifactId = found?.id
        ? found.id
        : (
            await rpc<{ id: string }>('artifact.create', {
              workItemId,
              kind: requiredKind,
              title: picked.filename.replace(/\.[^.]+$/, ''),
            })
          ).id;
      const draft = await rpc<{ id: string }>('artifact.createDraft', {
        artifactId,
        content,
      });
      await rpc('artifact.addReview', {
        revisionId: draft.id,
        reviewer: 'local-import',
        verdict: 'approved',
        comment: '外部导入',
      });
      await reload();
      onChanged();
    } catch (err) {
      setError(err instanceof Error ? err.message : '导入失败');
    } finally {
      setBusy(false);
    }
  };

  if (!status) return null;
  const MISSING_LABELS: Record<string, string> = {
    artifact_absent: '缺工件',
    revision_absent: '无修订',
    not_frozen: '未冻结',
    empty_content: '空内容',
  };
  const entries =
    status.entries && status.entries.length > 0
      ? status.entries
      : [
          {
            kind: status.requiredKind,
            satisfied: status.satisfied,
            missing: status.missing,
          },
        ];
  const allSatisfied = entries.every((e) => e.satisfied);
  return (
    <>
      {entries.map((e) => (
        <div
          key={e.kind}
          className={`sg-gate-req${e.satisfied ? '' : ' sg-gate-req--missing'}`}
          title={
            e.satisfied
              ? '交付物已冻结基线，可进入审批'
              : '该交付物未就绪：无冻结交付物不可进入审批'
          }
        >
          {e.satisfied ? <IconCheck size={12} /> : '✗'} 交付物（{e.kind}）
          {e.satisfied ? '已冻结' : MISSING_LABELS[e.missing ?? ''] ?? '未就绪'}
        </div>
      ))}
      {!allSatisfied ? (
        <div>
          <button className="sg-button" disabled={busy} onClick={() => void importFile()}>
            {busy ? '导入中…' : '导入交付物文件'}
          </button>
          {error ? <div className="sg-banner sg-banner--error" role="alert">{error}</div> : null}
        </div>
      ) : null}
    </>
  );
}

/** 代码团队共享状态：主仓库分支 + 未提交数（pull/commit/push 仍由开发者 git 工作流完成）。 */
function GitStatusChip({ projectId }: { projectId: string }) {
  const [info, setInfo] = useState<{ available: boolean; branch?: string; dirty?: number } | null>(null);
  useEffect(() => {
    let alive = true;
    void rpc<{ available: boolean; branch?: string; dirty?: number }>('project.gitStatus', { projectId })
      .then((r) => { if (alive) setInfo(r); })
      .catch(() => { if (alive) setInfo(null); });
    return () => { alive = false; };
  }, [projectId]);
  if (!info?.available || !info.branch) return null;
  return (
    <span className="sg-chip" title="代码仓库分支与未提交变更（团队共享经 git push/pull）">
      分支 {info.branch} · {info.dirty ?? 0} 处未提交
    </span>
  );
}

/// 实例关信息（模板配置面）：acceptance 验收策略 + deliverables kind 清单。
/// workflow.getInstance 失败（老库/无实例）→ null，UI 回退 legacy 展示。
function useInstanceGateInfo(workItemId: string, gate: string) {
  const [info, setInfo] = useState<{ acceptance: string[]; deliverables: string[] } | null>(null);
  useEffect(() => {
    let cancelled = false;
    rpc<{
      gates: { gate_id: string; acceptance?: string[]; deliverables: string[] }[];
    }>('workflow.getInstance', { workItemId })
      .then((r) => {
        if (cancelled) return;
        const g = (r.gates ?? []).find((x) => x.gate_id === gate);
        setInfo(
          g
            ? { acceptance: g.acceptance ?? [], deliverables: g.deliverables ?? [] }
            : null,
        );
      })
      .catch(() => {
        if (!cancelled) setInfo(null);
      });
    return () => {
      cancelled = true;
    };
  }, [workItemId, gate]);
  return info;
}

function GateWorkspace({
  gate,
  stage,
  progress,
  evidences,
  workItemId,
  projectId,
  events,
  runs,
  docs,
  trace,
  knowledgeCount,
  onChanged,
  onBack,
  onOpenApprovals,
}: {
  gate: Gate;
  stage?: StageInfo;
  progress: ProgressInfo | null;
  evidences: EvidenceInfo[];
  workItemId: string;
  projectId: string;
  events: TimelineEvent[];
  runs: AgentRunInfo[];
  docs: string[];
  trace: RunTrace | null;
  knowledgeCount: number;
  onChanged: () => void;
  onBack: () => void;
  onOpenApprovals: () => void;
}) {
  // 仅当最新一次 Run 失败时才显示错误卡（历史失败不追溯展示）。
  const failedRun = runs[0]?.status === 'failed' ? runs[0] : undefined;
  // 本关要求：模板声明的 acceptance 优先（配置化），无则回退 legacy 文案。
  const instanceGate = useInstanceGateInfo(workItemId, gate);
  const gateReqs = instanceGate?.acceptance?.length
    ? instanceGate.acceptance
    : (GATE_REQUIREMENTS[gate] ?? ['完成本关交付物并通过放行审批']);
  return (
    <div className="sg-workbench-center">
      <div className="sg-gate-head">
        <div className="sg-workbench-crumb">
          <button className="sg-icon-btn" title="返回任务进展" aria-label="返回任务进展" onClick={onBack}>
            <IconArrowLeft size={15} />
          </button>
          <span>
            当前关：{gateLabel(gate)}{GATE_SUBS[gate] ? ` · ${GATE_SUBS[gate]}` : ''}
          </span>
          <GitStatusChip projectId={projectId} />
        </div>
      </div>

      {/* 门禁头卡：图标 + 本关要求 + 本关状态 */}
      <div className="sg-gate-summary">
        <span className="sg-gate-summary-icon">
          <IconTarget size={20} />
        </span>
        <div className="sg-gate-summary-name">
          <strong>{gateLabel(gate)}</strong>
          <span>{GATE_SUBS[gate] ?? '自定义关卡'}</span>
        </div>
        <div className="sg-gate-summary-reqs">
          <div className="sg-gate-summary-label">本关要求</div>
          {gateReqs.map((req) => (
            <div className="sg-gate-req" key={req}>
              <IconCheck size={12} />
              {req}
            </div>
          ))}
          <DeliverableChip gate={gate} workItemId={workItemId} onChanged={onChanged} />
        </div>
        <div className="sg-gate-summary-status">
          <div className="sg-gate-summary-label">本关状态</div>
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
          {stage?.updated_at ? (
            <div className="sg-gate-summary-elapsed">最近推进 {relativeTime(stage.updated_at)}</div>
          ) : null}
        </div>
      </div>

      {/* Agent 执行详情横条：只展示最新一次 Run；历史次数收进标题行小字 */}
      <div className="sg-agent-strip-wrap">
        <div className="sg-agent-strip-title">
          Agent 执行详情
          {runs.length > 1 ? (
            <span className="sg-agent-strip-meta">共 {runs.length} 次执行（最新一次如下）</span>
          ) : null}
        </div>
        <div className="sg-agent-strip">
          {runs.length === 0 ? (
            <div className="sg-agent-card sg-agent-card--current">
              <span className="sg-agent-avatar">
                <IconCpu size={14} />
              </span>
              <span className="sg-agent-card-name">{GATE_AGENT[gate] ?? "执行 Agent"}</span>
              <span className="sg-agent-card-status">尚未启动</span>
              <span className="sg-agent-card-goal">在下方输入补充说明即可启动本关 Agent</span>
            </div>
          ) : (
            (() => {
              const run = runs[0];
              const st = RUN_STATUS[run.status] ?? { label: run.status, cls: 'pending' };
              const durationMin =
                run.created_at && run.updated_at
                  ? Math.max(
                      1,
                      Math.round((Date.parse(run.updated_at) - Date.parse(run.created_at)) / 60000),
                    )
                  : null;
              const failedCount = runs.slice(1).filter((r) => r.status === 'failed').length;
              return (
                <div className="sg-agent-card sg-agent-card--current" title={run.goal}>
                  <span className="sg-agent-avatar">
                    <IconCpu size={14} />
                  </span>
                  <span className="sg-agent-card-name">{GATE_AGENT[gate] ?? "执行 Agent"}</span>
                  <span className={`sg-agent-card-status sg-agent-status--${st.cls}`}>{st.label}</span>
                  {durationMin !== null ? (
                    <span className="sg-agent-card-goal">{durationMin} 分钟</span>
                  ) : null}
                  <span className="sg-agent-card-time">{relativeTime(run.created_at)}</span>
                  {failedCount > 0 ? (
                    <span className="sg-agent-card-goal">此前失败 {failedCount} 次</span>
                  ) : null}
                </div>
              );
            })()
          )}
        </div>
      </div>

      {/* 执行过程（输入 → Agent 执行 → 输出 → 证据） */}
      <div className="sg-gate-workspace">
        <ExecutionProcessView
          workItemId={workItemId}
          gate={gate}
          stage={stage}
          progress={progress}
          events={events}
          runs={runs}
          trace={trace}
          docs={docs}
          knowledgeCount={knowledgeCount}
        />
        {failedRun ? (
          <div className="sg-card">
            <div className="sg-card-head">错误与警告</div>
            <div className="sg-banner sg-banner--error" style={{ margin: '0 14px 12px' }}>
              {failedRun.result || '最近一次执行失败'}
            </div>
          </div>
        ) : null}
      </div>

      {progress && progress.pendingApprovals > 0 ? (
        <div className="sg-approval-banner">
          <div className="sg-approval-banner-text">
            <strong>等待你的放行审批</strong>
            <span>请前往审批中心查看并放行本关产物，批准后将进入下一关。</span>
          </div>
          <div className="sg-approval-banner-actions">
            <span className="sg-chip">待审批 {progress.pendingApprovals} 项</span>
            <button className="sg-btn sg-btn--primary" onClick={onOpenApprovals}>
              前往审批中心
            </button>
          </div>
        </div>
      ) : null}
    </div>
  );
}

/// M5-05（ADR-039 只读骨架）：驾驶舱卡——taskReadModel 真实进度（无估算百分比），
/// 下一步动作 + 阻塞原因；事实变化由 checkpoint sha 可回查。
function CockpitCard({ workItemId }: { workItemId: string }) {
  const [data, setData] = useState<{
    model: {
      current_gate_id: string;
      gates: { gate_id: string; progress: string }[];
      tasks: { task_key: string; phase: string }[];
      next_action: string;
      blocked_reason: string | null;
    };
    factsSha256: string;
  } | null>(null);
  useEffect(() => {
    let cancelled = false;
    rpc<{
      model: {
        current_gate_id: string;
        gates: { gate_id: string; progress: string }[];
        tasks: { task_key: string; phase: string }[];
        next_action: string;
        blocked_reason: string | null;
      };
      factsSha256: string;
    }>('trace.taskReadModel', { workItemId })
      .then((r) => {
        if (!cancelled) setData(r);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [workItemId]);
  if (!data) return null;
  const { model, factsSha256 } = data;
  return (
    <div className="sg-card" style={{ marginBottom: 12 }}>
      <div className="sg-card-head">
        驾驶舱 · 当前关 {model.current_gate_id}
        <span style={{ marginLeft: 'auto', opacity: 0.5, fontSize: 11 }} title={factsSha256}>
          facts {factsSha256.slice(0, 8)}
        </span>
      </div>
      <div style={{ padding: '8px 16px', fontSize: 13 }}>
        <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', marginBottom: 6 }}>
          {model.gates.map((g) => (
            <span key={g.gate_id} style={{ opacity: g.progress === 'done' ? 0.45 : 1 }}>
              {g.gate_id}:{g.progress}
            </span>
          ))}
        </div>
        <div>下一步：{model.next_action}</div>
        {model.blocked_reason && (
          <div style={{ color: '#b45309' }}>阻塞：{model.blocked_reason}</div>
        )}
      </div>
    </div>
  );
}

/// M2-09（ADR-036/037 只读骨架）：结构化计划卡。
/// RATIFLOW_PLAN_DAG 关闭时 RPC 返回 feature_disabled → 静默不渲染（零行为变化）。
function PlanCard({ workItemId }: { workItemId: string }) {
  const [data, setData] = useState<{
    latest?: {
      revision: { revision_no: number; status: string };
      tasks: { task_key: string; kind: string; effect_class: string; deps: string[] }[];
      attempts: { task_key: string; state: string }[];
    };
  } | null>(null);
  useEffect(() => {
    let cancelled = false;
    rpc<{ latest?: { revision: { revision_no: number; status: string }; tasks: { task_key: string; kind: string; effect_class: string; deps: string[] }[]; attempts: { task_key: string; state: string }[] } }>(
      'plan.get',
      { workItemId },
    )
      .then((r) => {
        if (!cancelled) setData(r);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [workItemId]);
  if (!data?.latest) return null;
  const { revision, tasks, attempts } = data.latest;
  const stateOf = (key: string) => attempts.find((a) => a.task_key === key)?.state ?? '未开始';
  return (
    <div className="sg-card" style={{ marginBottom: 12 }}>
      <div className="sg-card-head">
        计划 v{revision.revision_no} · {revision.status}
      </div>
      <div style={{ padding: '8px 16px', fontSize: 13 }}>
        {tasks.map((t) => (
          <div key={t.task_key} style={{ display: 'flex', gap: 8, padding: '2px 0' }}>
            <span style={{ opacity: 0.6 }}>{t.effect_class}</span>
            <span>{t.task_key}</span>
            <span style={{ marginLeft: 'auto', opacity: 0.75 }}>{stateOf(t.task_key)}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

/// M1-09（ADR-036 只读骨架）：模板实例关卡条。
/// Flag（RATIFLOW_WORKFLOW_TEMPLATE_V2）关闭时 RPC 返回 feature_disabled → 静默不渲染，
/// 默认六关界面行为不变；开启后按实例定义展示关卡标题与状态（自定义模板可见 3/2 关）。
function InstanceGatesCard({ workItemId }: { workItemId: string }) {
  const [data, setData] = useState<{
    instance: { template_version_id: string; current_gate_id: string };
    gates: { gate_id: string; title: string; state: string }[];
  } | null>(null);
  useEffect(() => {
    let cancelled = false;
    rpc<{ instance: { template_version_id: string; current_gate_id: string }; gates: { gate_id: string; title: string; state: string }[] }>(
      'workflow.getInstance',
      { workItemId },
    )
      .then((r) => {
        if (!cancelled) setData(r);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [workItemId]);
  if (!data || data.gates.length === 0) return null;
  return (
    <div className="sg-card" style={{ marginBottom: 12 }}>
      <div className="sg-card-head">实例关卡</div>
      <div className="sg-gate-stepper">
        {data.gates.map((g) => (
          <div
            key={g.gate_id}
            className={`sg-gate-step${
              g.state === 'passed'
                ? ' sg-gate-step--done'
                : g.gate_id === data.instance.current_gate_id
                  ? ' sg-gate-step--active'
                  : ''
            }`}
          >
            <span className="sg-gate-step-dot" />
            {g.title || g.gate_id}
          </div>
        ))}
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
