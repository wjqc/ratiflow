import { useMemo, useState } from 'react';
import type { Route } from './AppShell';
import { relativeTime } from '../lib/format';
import {
  IconBook,
  IconChevronDown,
  IconChevronRight,
  IconFolder,
  IconGear,
  IconHome,
  IconInfo,
  IconLogo,
  IconPlus,
  IconSearch,
  IconShield,
} from '../components/Icons';

export interface Project {
  id: string;
  name: string;
}

/** workitem.list 直接序列化 Rust WorkItem（snake_case）；兼容旧 mock 的 camelCase。 */
export interface WorkItemSummary {
  id: string;
  title: string;
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
  workItems: WorkItemSummary[];
  activeWorkItemId: string | null;
  coreReady: boolean | null;
  route: Route;
  knowledgeSources: KnowledgeSourceInfo[];
  onProjectChange: (projectId: string) => void;
  onTaskOpen: (workItemId: string) => void;
  onNavigate: (route: Route) => void;
}

const GATE_LABELS: Record<string, string> = {
  requirements: '需求关',
  design: '方案关',
  development: '开发关',
  testing: '测试关',
  deployment: '发布关',
  verification: '验收关',
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
  workItems,
  activeWorkItemId,
  coreReady,
  route,
  knowledgeSources,
  onProjectChange,
  onTaskOpen,
  onNavigate,
}: Props) {
  const [query, setQuery] = useState('');
  const q = query.trim().toLowerCase();

  const filteredProjects = useMemo(
    () => (q ? projects.filter((p) => p.name.toLowerCase().includes(q)) : projects),
    [projects, q],
  );
  const filteredTasks = useMemo(
    () => (q ? workItems.filter((t) => t.title.toLowerCase().includes(q)) : workItems),
    [workItems, q],
  );

  const activeProject = projects.find((p) => p.id === activeProjectId);
  const enabledSources = knowledgeSources.filter((s) => s.enabled !== false);

  return (
    <aside className="sg-sidebar" aria-label="项目导航">
      <div className="sg-brand">
        <IconLogo size={20} className="sg-brand-logo" />
        <span>通关 SixGates</span>
        {coreReady === false ? (
          <span className="sg-status sg-status--error" title="Rust core 不可用">
            core 异常
          </span>
        ) : null}
      </div>

      <nav style={{ padding: '0 8px', display: 'flex', flexDirection: 'column', gap: 2 }}>
        <button
          className={`sg-nav-item ${route.page === 'home' ? 'sg-nav-item--active' : ''}`}
          onClick={() => onNavigate({ page: 'home' })}
        >
          <IconHome size={15} />
          需求入口
        </button>
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
        <button
          className={`sg-nav-item ${route.page === 'settings' ? 'sg-nav-item--active' : ''}`}
          onClick={() => onNavigate({ page: 'settings' })}
        >
          <IconGear size={15} />
          设置与诊断
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
          return (
            <div key={p.id}>
              <button
                className="sg-tree-project"
                onClick={() => onProjectChange(p.id)}
                aria-expanded={active}
              >
                {active ? (
                  <IconChevronDown size={12} style={{ color: 'var(--sg-text-secondary)' }} />
                ) : (
                  <IconChevronRight size={12} style={{ color: 'var(--sg-text-secondary)' }} />
                )}
                <IconFolder size={15} style={{ color: 'var(--sg-text-secondary)' }} />
                <span className="sg-tree-name">{p.name}</span>
              </button>
              {active && (
                <div>
                  <button
                    className={`sg-tree-sub ${route.page === 'knowledge' ? 'sg-tree-sub--active' : ''}`}
                    onClick={() => onNavigate({ page: 'knowledge', projectId: p.id })}
                  >
                    <IconBook size={13} style={{ color: 'var(--sg-text-secondary)' }} />
                    <span className="sg-tree-name">知识库</span>
                    <span className="sg-tree-time">
                      {knowledgeSources.length > 0 ? `${knowledgeSources.length} 个来源` : ''}
                    </span>
                  </button>
                  {filteredTasks.map((t) => (
                    <button
                      key={t.id}
                      className={`sg-tree-sub ${
                        route.page === 'task' && route.workItemId === t.id ? 'sg-tree-sub--active' : ''
                      }`}
                      onClick={() => onTaskOpen(t.id)}
                      title={`${t.title} · ${gateLabel(workItemGate(t))}`}
                    >
                      <span
                        className={`sg-tree-dot ${
                          route.page === 'task' && route.workItemId === t.id
                            ? 'sg-tree-dot--active'
                            : workItemGate(t) === 'verification'
                              ? 'sg-tree-dot--done'
                              : ''
                        }`}
                      />
                      <span className="sg-tree-name">{t.title}</span>
                      <span className="sg-tree-time">{relativeTime(t.updated_at)}</span>
                    </button>
                  ))}
                  {filteredTasks.length === 0 && (
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
            {q ? '无匹配项目' : '暂无项目，请到「设置 → 常规」登记'}
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

function IconCheckMark() {
  return (
    <svg width="9" height="9" viewBox="0 0 10 10" fill="none" aria-hidden>
      <path d="M1.5 5.5l2.4 2.4L8.5 3" stroke="#fff" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}
