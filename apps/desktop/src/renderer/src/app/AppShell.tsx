import { useCallback, useEffect, useMemo, useState } from 'react';
import { rpc } from '../rpc/client';
import { ProjectSidebar, gateLabel, workItemGate } from './ProjectSidebar';
import type { KnowledgeSourceInfo, Project, WorkItemSummary } from './ProjectSidebar';
import { Workbench } from './Workbench';
import NewTaskPage from './NewTaskPage';
import ApprovalsPage from './ApprovalsPage';
import KnowledgePage from './KnowledgePage';
import SettingsShell from './settings/SettingsShell';
import { isSettingsRouteId, type SettingsRouteId } from './settings/settings-routes';
import { relativeTime } from '../lib/format';
import { IconInbox, IconSend } from '../components/Icons';

// 各关面板从 './AppShell' 导入 gateLabel，这里统一再导出。
export { gateLabel };

export type Route =
  | { page: 'home' }
  | { page: 'new'; projectId: string }
  | { page: 'task'; projectId: string; workItemId: string }
  | { page: 'approvals' }
  | { page: 'knowledge'; projectId: string }
  | { page: 'settings'; section?: SettingsRouteId };

// 刷新/重启恢复（契约 FR-DESK-008）：最近页面与最近项目；任务上下文仅恢复 section 级页面。
const LS_ROUTE = 'sg:lastRoute';
const LS_PROJECT = 'sg:lastProject';

function loadStoredRoute(): Route | null {
  try {
    const raw = localStorage.getItem(LS_ROUTE);
    if (!raw) return null;
    const r = JSON.parse(raw) as Route;
    if (r.page === 'settings') {
      return { page: 'settings', section: isSettingsRouteId(r.section) ? r.section : undefined };
    }
    if (r.page === 'approvals') return { page: 'approvals' };
    return null;
  } catch {
    return null;
  }
}

function loadStoredProject(): string | null {
  try {
    return localStorage.getItem(LS_PROJECT);
  } catch {
    return null;
  }
}

function storeLast(route: Route, projectId: string | null): void {
  try {
    localStorage.setItem(LS_ROUTE, JSON.stringify(route));
    if (projectId) localStorage.setItem(LS_PROJECT, projectId);
  } catch {
    /* 隐私模式等场景忽略 */
  }
}

export default function AppShell() {
  const [route, setRoute] = useState<Route>(() => loadStoredRoute() ?? { page: 'home' });
  const [projects, setProjects] = useState<Project[]>([]);
  const [activeProjectId, setActiveProjectId] = useState<string | null>(null);
  const [tasksByProject, setTasksByProject] = useState<Record<string, WorkItemSummary[]>>({});
  const [knowledgeSources, setKnowledgeSources] = useState<KnowledgeSourceInfo[]>([]);
  const [knowledgeCounts, setKnowledgeCounts] = useState<Record<string, number>>({});
  // 归档/移除后递增，驱动「最近任务」等派生列表重载。
  const [listVersion, setListVersion] = useState(0);
  const [coreReady, setCoreReady] = useState<boolean | null>(null);

  useEffect(() => {
    (async () => {
      try {
        const hello = await window.sixgates.hello();
        if (hello && hello.ok === false) throw new Error(hello.error ?? 'core 未就绪');
        setCoreReady(true);
        const result = await rpc<{ items: Project[] }>('project.list');
        setProjects(result.items ?? []);
        if (result.items?.length) {
          const stored = loadStoredProject();
          const hit = result.items.find((p) => p.id === stored);
          setActiveProjectId((prev) => prev ?? hit?.id ?? result.items[0].id);
        }
      } catch {
        setCoreReady(false);
      }
    })();
  }, []);

  const loadTasks = useCallback(async (projectId: string) => {
    try {
      const r = await rpc<{ items: WorkItemSummary[] }>('workitem.list', { projectId, limit: 50 });
      setTasksByProject((prev) => ({ ...prev, [projectId]: r.items ?? [] }));
    } catch {
      setTasksByProject((prev) => ({ ...prev, [projectId]: [] }));
    }
  }, []);

  const loadProjectData = useCallback(
    async (projectId: string) => {
      void loadTasks(projectId);
      try {
        const r = await rpc<{ items: KnowledgeSourceInfo[] }>('knowledge.list', { projectId });
        setKnowledgeSources(r.items ?? []);
        setKnowledgeCounts((prev) => ({ ...prev, [projectId]: (r.items ?? []).length }));
      } catch {
        setKnowledgeSources([]);
      }
    },
    [loadTasks],
  );

  // 树节点展开时懒加载该项目任务（已缓存则跳过）。
  const expandProject = useCallback(
    (projectId: string) => {
      setTasksByProject((prev) => {
        if (prev[projectId] === undefined) void loadTasks(projectId);
        return prev;
      });
    },
    [loadTasks],
  );

  useEffect(() => {
    if (activeProjectId) void loadProjectData(activeProjectId);
  }, [activeProjectId, loadProjectData]);

  const navigate = useCallback((next: Route) => {
    setRoute((prev) => {
      // 进入设置中心未指定 section 时，沿用当前/最近的 section。
      if (next.page === 'settings' && !next.section) {
        const stored = loadStoredRoute();
        const fallback =
          prev.page === 'settings' ? prev.section : stored?.page === 'settings' ? stored.section : undefined;
        return fallback ? { ...next, section: fallback } : next;
      }
      return next;
    });
  }, []);

  // 设置中心内部导航（SettingsShell 通过事件上抛，保持路由单一事实源）。
  useEffect(() => {
    const onSettingsNav = (e: Event) => {
      const detail = (e as CustomEvent<string>).detail;
      if (isSettingsRouteId(detail)) navigate({ page: 'settings', section: detail });
    };
    window.addEventListener('sg:settings-navigate', onSettingsNav);
    return () => window.removeEventListener('sg:settings-navigate', onSettingsNav);
  }, [navigate]);

  // 路由/项目变化时持久化，供刷新与重启恢复。
  useEffect(() => {
    storeLast(route, activeProjectId);
  }, [route, activeProjectId]);

  const openTask = useCallback(
    (projectId: string, workItemId: string) => {
      navigate({ page: 'task', projectId, workItemId });
    },
    [navigate],
  );

  const switchProject = useCallback((projectId: string) => {
    setActiveProjectId(projectId);
    setRoute({ page: 'home' });
  }, []);

  const activateWorkspace = useCallback((project: Project) => {
    setProjects((current) =>
      current.some((item) => item.id === project.id) ? current : [...current, project],
    );
    setActiveProjectId(project.id);
  }, []);

  // 移除 = 归档（可恢复）：侧栏已做行内两步确认，这里直接执行；
  // 移除的是活动项目时，切到剩余第一个项目（无则回首页）。
  const removeProject = useCallback(
    async (project: Project) => {
      try {
        await rpc('project.archive', { projectId: project.id, archived: true });
      } catch (reason) {
        window.alert(`移除失败：${reason instanceof Error ? reason.message : String(reason)}`);
        return;
      }
      setProjects((prev) => prev.filter((p) => p.id !== project.id));
      setListVersion((v) => v + 1);
      if (activeProjectId === project.id) {
        const rest = projects.filter((p) => p.id !== project.id);
        if (rest.length > 0) {
          setActiveProjectId(rest[0].id);
        } else {
          setActiveProjectId(null);
          setRoute({ page: 'home' });
        }
      }
      if (route.page === 'knowledge' && route.projectId === project.id) setRoute({ page: 'home' });
    },
    [activeProjectId, projects, route],
  );

  const removeTask = useCallback(
    async (task: WorkItemSummary) => {
      try {
        await rpc('workitem.archive', { workItemId: task.id, archived: true });
      } catch (reason) {
        window.alert(`移除失败：${reason instanceof Error ? reason.message : String(reason)}`);
        return;
      }
      if (activeProjectId) void loadTasks(activeProjectId);
      // 被移除的任务可能属于非活动项目（树可多开展开），按其所属项目精准刷新。
      if (task.project_id && task.project_id !== activeProjectId) {
        void loadTasks(task.project_id);
      }
      setListVersion((v) => v + 1);
      if (route.page === 'task' && route.workItemId === task.id) setRoute({ page: 'home' });
    },
    [activeProjectId, loadTasks, route],
  );

  const activeProject = useMemo(
    () => projects.find((p) => p.id === activeProjectId) ?? null,
    [projects, activeProjectId],
  );

  // ⌘N / Ctrl+N：有活动项目时直接进入新建任务（对齐原型快捷键提示）。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'n') {
        e.preventDefault();
        if (activeProjectId) navigate({ page: 'new', projectId: activeProjectId });
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [activeProjectId, navigate]);

  return (
    <div className="sg-shell">
      <ProjectSidebar
        projects={projects}
        activeProjectId={activeProjectId}
        tasksByProject={tasksByProject}
        activeWorkItemId={route.page === 'task' ? route.workItemId : null}
        coreReady={coreReady}
        route={route}
        knowledgeSources={knowledgeSources}
        knowledgeCounts={knowledgeCounts}
        onProjectChange={switchProject}
        onExpandProject={expandProject}
        onProjectRemove={(project) => void removeProject(project)}
        onTaskRemove={(task) => void removeTask(task)}
        onTaskOpen={(workItemId) => activeProjectId && openTask(activeProjectId, workItemId)}
        onNavigate={navigate}
      />
      <main className="sg-main">
        {route.page === 'home' && (
          <HomePage projects={projects} onNavigate={navigate} refreshKey={listVersion} />
        )}
        {route.page === 'new' && (
          <NewTaskPage
            projectId={route.projectId}
            projects={projects}
            onCreated={(workItemId, createdProjectId) => openTask(createdProjectId, workItemId)}
            onWorkspaceChanged={activateWorkspace}
            onOpenRemote={() => navigate({ page: 'settings', section: 'ssh' })}
            onBack={() => navigate({ page: 'home' })}
          />
        )}
        {route.page === 'task' && (
          <Workbench
            projectId={route.projectId}
            projectName={activeProject?.name ?? ''}
            workItemId={route.workItemId}
            knowledgeCount={knowledgeSources.length}
            onNavigate={navigate}
          />
        )}
        {route.page === 'approvals' && (
          <ApprovalsPage
            onDecided={() => activeProjectId && void loadProjectData(activeProjectId)}
          />
        )}
        {route.page === 'knowledge' && (
          <KnowledgePage projectId={route.projectId} projectName={activeProject?.name} />
        )}
        {route.page === 'settings' && <SettingsShell section={route.section} />}
      </main>
    </div>
  );
}

/* ---------------- 首屏（需求入口） ---------------- */

function HomePage({
  projects,
  onNavigate,
  refreshKey,
}: {
  projects: Project[];
  onNavigate: (r: Route) => void;
  refreshKey: number;
}) {
  if (projects.length === 0) {
    return (
      <>
        <header className="sg-page-head">
          <span className="sg-page-head-title">需求入口</span>
        </header>
        <div className="sg-scroll">
          <div className="sg-hero">
            <h2 className="sg-hero-title">写下你的需求，开始闯关</h2>
            <p className="sg-hero-sub">
              文字描述、需求文档、GitLab Issue 或界面截图，选择一种方式开始。Agent
              将带着任务依次通过需求、方案、开发、测试、发布、验收六关。
            </p>
            <div className="sg-card" style={{ marginTop: 20 }}>
              <div className="sg-empty" style={{ padding: '40px 24px' }}>
                <IconInbox size={32} style={{ color: 'var(--sg-border-strong)' }} />
                <div>还没有项目</div>
                <div className="sg-sub">
                  先在「设置与诊断 → 项目与目录」登记一个本地项目目录，再回来发起需求
                </div>
                <button
                  className="sg-btn sg-btn--primary"
                  style={{ marginTop: 8 }}
                  onClick={() => onNavigate({ page: 'settings', section: 'projects' })}
                >
                  去登记项目
                </button>
              </div>
            </div>
          </div>
        </div>
      </>
    );
  }
  return (
    <>
      <header className="sg-page-head">
        <span className="sg-page-head-title">需求入口</span>
        <span className="sg-page-head-status">本地运行</span>
      </header>
      <div className="sg-scroll">
        <div className="sg-hero">
          <h2 className="sg-hero-title">写下你的需求，开始闯关</h2>
          <p className="sg-hero-sub">
            文字描述、需求文档、GitLab Issue 或界面截图，选择一种方式开始。Agent
            将带着任务依次通过需求、方案、开发、测试、发布、验收六关。
          </p>
          <RecentTasks projects={projects} onNavigate={onNavigate} refreshKey={refreshKey} />
        </div>
      </div>
    </>
  );
}

function RecentTasks({
  projects,
  onNavigate,
  refreshKey,
}: {
  projects: Project[];
  onNavigate: (r: Route) => void;
  refreshKey: number;
}) {
  const [rows, setRows] = useState<{ project: Project; task: WorkItemSummary }[]>([]);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const all: { project: Project; task: WorkItemSummary }[] = [];
      for (const p of projects.slice(0, 5)) {
        try {
          const r = await rpc<{ items: WorkItemSummary[] }>('workitem.list', {
            projectId: p.id,
            limit: 3,
          });
          for (const task of r.items ?? []) all.push({ project: p, task });
        } catch {
          /* 忽略单项目失败 */
        }
      }
      if (!cancelled) setRows(all);
    })();
    return () => {
      cancelled = true;
    };
  }, [projects, refreshKey]);

  return (
    <div className="sg-card" style={{ marginTop: 20 }}>
      <div className="sg-card-head">
        最近任务
        <span className="sg-card-extra">
          {projects[0] && (
            <button
              className="sg-btn sg-btn--primary sg-btn--sm"
              onClick={() => onNavigate({ page: 'new', projectId: projects[0].id })}
            >
              <IconSend size={13} />
              新建任务
            </button>
          )}
        </span>
      </div>
      {rows.length === 0 ? (
        <div className="sg-empty" style={{ padding: '32px 24px' }}>
          暂无任务，从「新建任务」发起第一个需求
        </div>
      ) : (
        <table className="sg-table">
          <thead>
            <tr>
              <th>任务</th>
              <th style={{ width: 140 }}>项目</th>
              <th style={{ width: 90 }}>当前关</th>
              <th style={{ width: 110 }}>更新于</th>
              <th style={{ width: 90 }}></th>
            </tr>
          </thead>
          <tbody>
            {rows.map(({ project, task }) => (
              <tr key={task.id}>
                <td>{task.title}</td>
                <td className="sg-muted">{project.name}</td>
                <td className="sg-muted">{gateLabel(workItemGate(task))}</td>
                <td className="sg-muted">{relativeTime(task.updated_at)}</td>
                <td>
                  <button
                    className="sg-btn sg-btn--sm"
                    onClick={() =>
                      onNavigate({ page: 'task', projectId: project.id, workItemId: task.id })
                    }
                  >
                    继续闯关
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
