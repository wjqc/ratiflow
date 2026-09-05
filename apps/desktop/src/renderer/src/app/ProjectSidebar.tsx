import { useEffect, useMemo, useRef, useState } from 'react';
import type { Route } from './AppShell';
import { relativeTime } from '../lib/format';
import {
  IconBook,
  IconChevronDown,
  IconChevronRight,
  IconFolder,
  IconGear,
  IconInfo,
  IconLogo,
  IconPlus,
  IconSearch,
  IconShield,
  IconX,
} from '../components/Icons';

export interface Project {
  id: string;
  name: string;
}

/** workitem.list 直接序列化 Rust WorkItem（snake_case）；兼容旧 mock 的 camelCase。 */
export interface WorkItemSummary {
  id: string;
  title: string;
  project_id?: string;
  current_gate?: string;
  currentGate?: string;
  updated_at?: string;
}

export function workItemGate(t: WorkItemSummary): string | undefined {
  return t.current_gate ?? t.currentGate;
}

export interface KnowledgeSourceInfo {
  id: string;
  name: string;
  kind: string;
  enabled?: boolean;
}

interface Props {
  projects: Project[];
  activeProjectId: string | null;
  /** 各项目任务缓存（展开时经 onExpandProject 懒加载）。 */
  tasksByProject: Record<string, WorkItemSummary[]>;
  activeWorkItemId: string | null;
  coreReady: boolean | null;
  route: Route;
  /** 活动项目知识来源（「本次上下文」卡片用）。 */
  knowledgeSources: KnowledgeSourceInfo[];
  /** 各项目知识来源数缓存（树节点计数；仅加载过的项目有值）。 */
  knowledgeCounts: Record<string, number>;
  onProjectChange: (projectId: string) => void;
  onExpandProject: (projectId: string) => void;
  onProjectRemove: (project: Project) => void;
  onTaskRemove: (task: WorkItemSummary) => void;
  /** 打开任务：第二参数是任务所属项目（树可多项目同时展开，不能假定是活动项目）。 */
  onTaskOpen: (workItemId: string, projectId?: string) => void;
  onNavigate: (route: Route) => void;
}

const GATE_LABELS: Record<string, string> = {
  requirements: '需求关',
  design: '方案关',
  development: '开发关',
  testing: '测试关',
  deployment: '部署关',
  verification: '验证关',
};

export function gateLabel(gate: string | undefined): string {
  if (!gate) return '未开始';
  return GATE_LABELS[gate] ?? gate;
}

const KIND_TAGS: Record<string, string> = {
  repo: '代码库',
  folder: '代码库',
  docs: '文档库',
  issues: '议题',
  gitlab: '议题',
  web: '网页',
};

function kindTag(kind: string): string {
  return KIND_TAGS[kind] ?? '来源';
}

export function ProjectSidebar({
  projects,
  activeProjectId,
  tasksByProject,
  activeWorkItemId,
  coreReady,
  route,
  knowledgeSources,
  knowledgeCounts,
  onProjectChange,
  onExpandProject,
  onProjectRemove,
  onTaskRemove,
  onTaskOpen,
  onNavigate,
}: Props) {
  const [query, setQuery] = useState('');
  const q = query.trim().toLowerCase();
  // 展开状态独立于选中：chevron 只切换展开，不切换工作区。
  const [expanded, setExpanded] = useState<Set<string>>(
    () => new Set(activeProjectId ? [activeProjectId] : []),
  );

  // 选中/切换项目时自动展开（收起后不再被强制展开）。
  useEffect(() => {
    if (!activeProjectId) return;
    setExpanded((prev) => {
      if (prev.has(activeProjectId)) return prev;
      const next = new Set(prev);
      next.add(activeProjectId);
      return next;
    });
  }, [activeProjectId]);

  const toggleExpanded = (projectId: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(projectId)) {
        next.delete(projectId);
      } else {
        next.add(projectId);
        onExpandProject(projectId);
      }
      return next;
    });
  };

  // 搜索时加载全部项目任务：只搜"已展开"的项目会漏掉未展开项目的任务。
  useEffect(() => {
    if (!q) return;
    for (const p of projects) {
      if (tasksByProject[p.id] === undefined) onExpandProject(p.id);
    }
  }, [q, projects, tasksByProject, onExpandProject]);

  // 移除采用行内两步确认（不依赖原生 confirm 对话框）：第一次点变「确认」，3 秒内再点执行。
  const [pendingRemove, setPendingRemove] = useState<{ kind: 'project' | 'task'; id: string } | null>(
    null,
  );
  const pendingTimer = useRef<number | null>(null);
  useEffect(() => {
    return () => {
      if (pendingTimer.current !== null) window.clearTimeout(pendingTimer.current);
    };
  }, []);
  const requestRemove = (kind: 'project' | 'task', id: string, execute: () => void) => {
    if (pendingRemove?.kind === kind && pendingRemove.id === id) {
      if (pendingTimer.current !== null) window.clearTimeout(pendingTimer.current);
      setPendingRemove(null);
      execute();
      return;
    }
    setPendingRemove({ kind, id });
    if (pendingTimer.current !== null) window.clearTimeout(pendingTimer.current);
    pendingTimer.current = window.setTimeout(() => setPendingRemove(null), 3000);
  };

  const filteredProjects = useMemo(
    () => (q ? projects.filter((p) => p.name.toLowerCase().includes(q)) : projects),
    [projects, q],
  );

  const filterTasks = (items: WorkItemSummary[] | undefined) =>
    q ? (items ?? []).filter((t) => t.title.toLowerCase().includes(q)) : (items ?? []);

  const activeProject = projects.find((p) => p.id === activeProjectId);
  const enabledSources = knowledgeSources.filter((s) => s.enabled !== false);

  return (
    <aside className="sg-sidebar" aria-label="项目导航">
      <button
        className="sg-brand sg-brand-button"
        onClick={() => onNavigate({ page: 'home' })}
        aria-label="返回任务首页"
      >
        <IconLogo size={20} className="sg-brand-logo" />
        <span>通关 SixGates</span>
        {coreReady === false ? (
          <span className="sg-status sg-status--error" title="Rust core 不可用">
            core 异常
          </span>
        ) : null}
      </button>

      <nav style={{ padding: '0 8px', display: 'flex', flexDirection: 'column', gap: 2 }}>
        <button
          className={`sg-nav-item ${route.page === 'new' ? 'sg-nav-item--active' : ''}`}
          onClick={() => activeProjectId && onNavigate({ page: 'new', projectId: activeProjectId })}
          disabled={!activeProjectId}
          title={activeProjectId ? '新建任务（⌘N）' : '请先在下方选择项目'}
        >
          <IconPlus size={15} />
          新建任务
          <span className="sg-kbd">⌘N</span>
        </button>
        <button
          className={`sg-nav-item ${route.page === 'approvals' ? 'sg-nav-item--active' : ''}`}
          onClick={() => onNavigate({ page: 'approvals' })}
        >
          <IconShield size={15} />
          审批中心
        </button>
      </nav>

      <div className="sg-search">
        <IconSearch size={13} />
        <input
          className="sg-input"
          placeholder="搜索"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          aria-label="搜索项目与任务"
        />
      </div>

      <div className="sg-sidebar-label">项目</div>
      <div style={{ paddingBottom: 4 }}>
        {filteredProjects.map((p) => {
          const active = p.id === activeProjectId;
          const isOpen = expanded.has(p.id);
          const tasks = filterTasks(tasksByProject[p.id]);
          const knowledgeCount = knowledgeCounts[p.id];
          return (
            <div key={p.id}>
              <div className={`sg-tree-row ${active ? 'sg-tree-row--active' : ''}`}>
                <button
                  className="sg-tree-toggle"
                  onClick={() => toggleExpanded(p.id)}
                  aria-expanded={isOpen}
                  aria-label={isOpen ? `收起 ${p.name}` : `展开 ${p.name}`}
                >
                  {isOpen ? (
                    <IconChevronDown size={12} style={{ color: 'var(--sg-text-secondary)' }} />
                  ) : (
                    <IconChevronRight size={12} style={{ color: 'var(--sg-text-secondary)' }} />
                  )}
                </button>
                <button
                  className="sg-tree-project"
                  onClick={() => onProjectChange(p.id)}
                  title={p.name}
                >
                  <IconFolder size={15} style={{ color: 'var(--sg-text-secondary)' }} />
                  <span className="sg-tree-name">{p.name}</span>
                </button>
                <button
                  className={`sg-tree-remove ${
                    pendingRemove?.kind === 'project' && pendingRemove.id === p.id
                      ? 'sg-tree-remove--confirm'
                      : ''
                  }`}
                  onClick={() => requestRemove('project', p.id, () => onProjectRemove(p))}
                  title={
                    pendingRemove?.kind === 'project' && pendingRemove.id === p.id
                      ? '再次点击确认移除'
                      : `移除「${p.name}」（归档，可在设置中恢复）`
                  }
                  aria-label={
                    pendingRemove?.kind === 'project' && pendingRemove.id === p.id
                      ? `确认移除项目 ${p.name}`
                      : `移除项目 ${p.name}`
                  }
                >
                  {pendingRemove?.kind === 'project' && pendingRemove.id === p.id ? (
                    '确认'
                  ) : (
                    <IconX size={12} />
                  )}
                </button>
              </div>
              {isOpen && (
                <div>
                  <button
                    className={`sg-tree-sub ${route.page === 'knowledge' && route.projectId === p.id ? 'sg-tree-sub--active' : ''}`}
                    onClick={() => onNavigate({ page: 'knowledge', projectId: p.id })}
                  >
                    <IconBook size={13} style={{ color: 'var(--sg-text-secondary)' }} />
                    <span className="sg-tree-name">知识库</span>
                    <span className="sg-tree-time">
                      {knowledgeCount !== undefined && knowledgeCount > 0
                        ? `${knowledgeCount} 个来源`
                        : ''}
                    </span>
                  </button>
                  {tasks.map((t) => {
                    const selected = route.page === 'task' && route.workItemId === t.id;
                    return (
                      <div
                        key={t.id}
                        className={`sg-tree-row sg-tree-row--sub ${selected ? 'sg-tree-row--active' : ''}`}
                      >
                        <button
                          className={`sg-tree-sub ${selected ? 'sg-tree-sub--active' : ''}`}
                          onClick={() => onTaskOpen(t.id, p.id)}
                          title={`${t.title} · ${gateLabel(workItemGate(t))}`}
                        >
                          <span
                            className={`sg-tree-dot ${selected ? 'sg-tree-dot--active' : ''}`}
                          />
                          <span className="sg-tree-name">{t.title}</span>
                          <span className="sg-tree-time">{relativeTime(t.updated_at)}</span>
                        </button>
                        <button
                          className={`sg-tree-remove ${
                            pendingRemove?.kind === 'task' && pendingRemove.id === t.id
                              ? 'sg-tree-remove--confirm'
                              : ''
                          }`}
                          onClick={() => requestRemove('task', t.id, () => onTaskRemove(t))}
                          title={
                            pendingRemove?.kind === 'task' && pendingRemove.id === t.id
                              ? '再次点击确认移除'
                              : `移除任务「${t.title}」（归档后从界面隐藏）`
                          }
                          aria-label={
                            pendingRemove?.kind === 'task' && pendingRemove.id === t.id
                              ? `确认移除任务 ${t.title}`
                              : `移除任务 ${t.title}`
                          }
                        >
                          {pendingRemove?.kind === 'task' && pendingRemove.id === t.id ? (
                            '确认'
                          ) : (
                            <IconX size={12} />
                          )}
                        </button>
                      </div>
                    );
                  })}
                  {tasks.length === 0 && tasksByProject[p.id] !== undefined && (
                    <div className="sg-sub" style={{ padding: '4px 12px 6px 30px' }}>
                      {q ? '无匹配任务' : '还没有任务，从「新建任务」开始'}
                    </div>
                  )}
                </div>
              )}
            </div>
          );
        })}
        {filteredProjects.length === 0 && (
          <div className="sg-sub" style={{ padding: '4px 14px' }}>
            {q ? '无匹配项目' : '暂无项目，请从左下角打开设置后添加'}
          </div>
        )}
      </div>

      {activeProject && enabledSources.length > 0 && (
        <div className="sg-context-card">
          <div className="sg-context-card-head">
            本次上下文
            <span
              className="sg-icon-btn"
              title="本次任务注入的知识来源，仅来自当前项目"
              style={{ cursor: 'default' }}
            >
              <IconInfo size={12} />
            </span>
          </div>
          <div className="sg-context-card-sub">基于当前项目知识库</div>
          <div style={{ marginTop: 4 }}>
            {enabledSources.slice(0, 3).map((s) => (
              <div className="sg-context-item" key={s.id}>
                <span className="sg-context-check">
                  <IconCheckMark />
                </span>
                <span className="sg-context-name">{s.name}</span>
                <span className="sg-context-tag">{kindTag(s.kind)}</span>
              </div>
            ))}
          </div>
          <div className="sg-context-card-foot">
            <span>
              已选 {Math.min(enabledSources.length, 3)}/{enabledSources.length} 个来源
            </span>
            <button
              className="sg-link-btn"
              onClick={() => onNavigate({ page: 'knowledge', projectId: activeProject.id })}
            >
              管理上下文
            </button>
          </div>
        </div>
      )}

      <div className="sg-sidebar-foot">
        <span className="sg-avatar">我</span>
        <span>本地用户</span>
        <button
          className="sg-icon-btn"
          onClick={() => onNavigate({ page: 'settings' })}
          title="设置"
          aria-label="设置"
        >
          <IconGear size={15} />
        </button>
      </div>
    </aside>
  );
}

/** 知识来源列表仅活动项目有缓存；本组件只用于「本次上下文」卡片。 */
function IconCheckMark() {
  return (
    <svg width="9" height="9" viewBox="0 0 10 10" fill="none" aria-hidden>
      <path d="M1.5 5.5l2.4 2.4L8.5 3" stroke="#fff" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}
