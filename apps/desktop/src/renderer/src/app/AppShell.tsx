import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { rpc } from '../rpc/client';
import { IconPanelLeft } from '../components/Icons';
import { ProjectSidebar, gateLabel, workItemGate } from './ProjectSidebar';
import type { KnowledgeSourceInfo, Project, WorkItemSummary } from './ProjectSidebar';
import { Workbench } from './Workbench';
import NewTaskPage from './NewTaskPage';
import ApprovalsPage from './ApprovalsPage';
import KnowledgePage from './KnowledgePage';
import SettingsShell from './settings/SettingsShell';
import { SettingsSidebar } from './settings/SettingsSidebar';
import {
  DEFAULT_SETTINGS_ROUTE,
  isSettingsRouteId,
  type SettingsRouteId,
} from './settings/settings-routes';
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

// 刷新/重启只恢复工作页（当前仅审批中心）与最近项目，不恢复任务上下文
// （产品决策：避免草稿误发送，见 dev-notes/2026-08-22-settings-center.md；FR-DESK-008 是自动更新签名，与此无关）。
// 设置页是工具页不算"工作现场"：写入与恢复两侧都排除，启动永不落在设置页。
const LS_ROUTE = 'sg:lastRoute';
const LS_PROJECT = 'sg:lastProject';

/** 本地布尔标记读写；localStorage 不可用（隐私模式/测试环境）时静默降级。 */
function readFlag(key: string): boolean {
  try {
    return localStorage.getItem(key) === '1';
  } catch {
    return false;
  }
}

function writeFlag(key: string, value: boolean) {
  try {
    localStorage.setItem(key, value ? '1' : '0');
  } catch {
    /* 隐私模式等场景忽略 */
  }
}

function loadStoredRoute(): Route | null {
  try {
    const raw = localStorage.getItem(LS_ROUTE);
    if (!raw) return null;
    const r = JSON.parse(raw) as Route;
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
    // settings 不覆盖已记录的工作页：在设置里退出应用，重启应回到之前的工作页。
    if (route.page !== 'settings') localStorage.setItem(LS_ROUTE, JSON.stringify(route));
    if (projectId) localStorage.setItem(LS_PROJECT, projectId);
  } catch {
    /* 隐私模式等场景忽略 */
  }
}

export default function AppShell() {
  const [route, setRoute] = useState<Route>(() => loadStoredRoute() ?? { page: 'home' });
  // 左侧菜单栏整体收起（记忆在本地；设置中心有自己的侧栏不受影响）。
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => readFlag('sg:sidebarCollapsed'));
  const [projects, setProjects] = useState<Project[]>([]);
  const [activeProjectId, setActiveProjectId] = useState<string | null>(null);
  const [tasksByProject, setTasksByProject] = useState<Record<string, WorkItemSummary[]>>({});
  const [knowledgeSources, setKnowledgeSources] = useState<KnowledgeSourceInfo[]>([]);
  const [knowledgeCounts, setKnowledgeCounts] = useState<Record<string, number>>({});
  // 归档/移除后递增，驱动「最近任务」等派生列表重载。
  const [listVersion, setListVersion] = useState(0);
  const [coreReady, setCoreReady] = useState<boolean | null>(null);
  const lastWorkspaceRoute = useRef<Route>({ page: 'home' });

  useEffect(() => {
    writeFlag('sg:sidebarCollapsed', sidebarCollapsed);
  }, [sidebarCollapsed]);

  useEffect(() => {
    (async () => {
      try {
        const hello = await window.ratiflow.hello();
        if (hello && hello.ok === false) throw new Error(hello.error ?? 'core 未就绪');
        setCoreReady(true);
        // 「恢复工作现场」开关（app.general.restoreLastProject，默认开）：
        // 关闭时不恢复上次工作页路由与上次项目；读取失败按默认开处理。
        let restore = true;
        try {
          const settings = await rpc<{
            items: Array<{ key: string; value: Record<string, unknown> }>;
          }>('settings.get', { scope: 'global', keys: ['app.general'] });
          const general = settings.items?.find((item) => item.key === 'app.general');
          restore = general?.value?.restoreLastProject !== false;
        } catch {
          /* 设置域不可用时保持默认行为 */
        }
        if (!restore) setRoute({ page: 'home' });
        const result = await rpc<{ items: Project[] }>('project.list');
        setProjects(result.items ?? []);
        if (result.items?.length) {
          const stored = restore ? loadStoredProject() : null;
          const hit = result.items.find((p) => p.id === stored);
          setActiveProjectId((prev) => prev ?? hit?.id ?? result.items[0].id);
        }
      } catch {
        setCoreReady(false);
      }
    })();
  }, []);

  // 已加载过任务的项目集合：任务归档/恢复广播后按缓存精准重载。
  const cachedTaskProjects = useRef<Set<string>>(new Set());

  const loadTasks = useCallback(async (projectId: string) => {
    cachedTaskProjects.current.add(projectId);
    try {
      const r = await rpc<{ items: WorkItemSummary[] }>('workitem.list', { projectId, limit: 50 });
      setTasksByProject((prev) => ({ ...prev, [projectId]: r.items ?? [] }));
    } catch {
      setTasksByProject((prev) => ({ ...prev, [projectId]: [] }));
    }
  }, []);

  // 设置页恢复/归档任务后广播 sg:tasks-changed：重载所有已缓存项目的任务列表。
  useEffect(() => {
    const onTasksChanged = () => {
      for (const pid of cachedTaskProjects.current) void loadTasks(pid);
    };
    window.addEventListener('sg:tasks-changed', onTasksChanged);
    return () => window.removeEventListener('sg:tasks-changed', onTasksChanged);
  }, [loadTasks]);

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

  // 团队共享（知识库/记忆随仓库走）：项目切换后 fire-and-forget 对账一次，
  // git pull 回到应用即可见团队内容；失败静默（页面另有手动同步兜底）。
  useEffect(() => {
    if (!activeProjectId) return;
    void rpc('knowledge.syncFromRepo', { projectId: activeProjectId }).catch(() => {});
    void rpc('memory.syncFromRepo', { projectId: activeProjectId }).catch(() => {});
  }, [activeProjectId]);

  // 外观主题（M5：dark/light 落地）：app.appearance.theme → <html data-theme>。
  useEffect(() => {
    let disposed = false;
    rpc<{ items: Array<{ key: string; value: Record<string, unknown> }> }>('settings.get', {
      scope: 'global',
      keys: ['app.appearance'],
    })
      .then((res) => {
        if (disposed) return;
        const entry = res.items?.find((i) => i.key === 'app.appearance');
        const theme = entry?.value?.theme === 'dark' ? 'dark' : 'light';
        document.documentElement.dataset.theme = theme;
      })
      .catch(() => {
        if (!disposed) document.documentElement.dataset.theme = 'light';
      });
    const onAppearance = (event: Event) => {
      const detail = (event as CustomEvent<{ key?: string; value?: Record<string, unknown> }>).detail;
      if (detail?.key !== 'app.appearance') return;
      document.documentElement.dataset.theme = detail.value?.theme === 'dark' ? 'dark' : 'light';
    };
    window.addEventListener('sg:appearance-changed', onAppearance);
    return () => {
      disposed = true;
      window.removeEventListener('sg:appearance-changed', onAppearance);
    };
  }, []);

  // 设置内新建/归档项目后刷新侧栏列表（与 sg:settings-navigate 同一事件约定）。
  useEffect(() => {
    const onProjectsChanged = () => {
      (async () => {
        try {
          const result = await rpc<{ items: Project[] }>('project.list');
          const items = result.items ?? [];
          setProjects(items);
          setActiveProjectId((prev) =>
            prev && items.some((p) => p.id === prev) ? prev : items[0]?.id ?? null,
          );
        } catch {
          /* core 异常时保持现状 */
        }
      })();
    };
    window.addEventListener('sg:projects-changed', onProjectsChanged);
    return () => window.removeEventListener('sg:projects-changed', onProjectsChanged);
  }, []);

  const navigate = useCallback((next: Route) => {
    setRoute((prev) => {
      // 左下角齿轮始终进入常规；页面内深链仍显式携带 section。
      if (next.page === 'settings' && !next.section) {
        return { ...next, section: DEFAULT_SETTINGS_ROUTE };
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
    if (route.page !== 'settings') lastWorkspaceRoute.current = route;
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
      // 移除项目后，正看着该项目的任务/知识库/新建页时必须离开，
      // 否则主区渲染孤儿任务、面包屑/上下文挂到新活动项目上（错位）。
      if (
        (route.page === 'knowledge' || route.page === 'task' || route.page === 'new') &&
        route.projectId === project.id
      ) {
        setRoute({ page: 'home' });
      }
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
    <div
      className={`sg-shell${
        sidebarCollapsed && route.page !== 'settings' ? ' sg-shell--collapsed' : ''
      }`}
    >
      {route.page === 'settings' ? (
        <SettingsSidebar
          section={route.section}
          onNavigate={(section) => navigate({ page: 'settings', section })}
          onBack={() => navigate(lastWorkspaceRoute.current)}
        />
      ) : sidebarCollapsed ? (
        <div className="sg-sidebar-rail">
          <button
            className="sg-icon-btn"
            title="展开菜单栏"
            aria-label="展开菜单栏"
            onClick={() => setSidebarCollapsed(false)}
          >
            <IconPanelLeft size={15} />
          </button>
        </div>
      ) : (
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
          // 任务所属项目以树节点为准（可多项目同时展开），不能假定是当前活动项目。
          onTaskOpen={(workItemId, projectId) => {
            const pid = projectId ?? activeProjectId;
            if (pid) openTask(pid, workItemId);
          }}
          onNavigate={navigate}
          onCollapse={() => setSidebarCollapsed(true)}
        />
      )}
      <main className="sg-main">
        {coreReady === false ? (
          <CoreFailurePage />
        ) : (
          <>
            {route.page === 'home' && (
              <HomePage
                projects={projects}
                activeProjectId={activeProjectId}
                onNavigate={navigate}
                refreshKey={listVersion}
              />
            )}
            {route.page === 'new' && (
              <NewTaskPage
                projectId={route.projectId}
                projects={projects}
                onCreated={(workItemId, createdProjectId) => openTask(createdProjectId, workItemId)}
                onWorkspaceChanged={activateWorkspace}
                onOpenRemote={() => navigate({ page: 'settings', section: 'integrations' })}
                onBack={() => navigate({ page: 'home' })}
              />
            )}
            {route.page === 'task' && (
              <Workbench
                key={route.workItemId}
                projectId={route.projectId}
                projectName={
                  projects.find((p) => p.id === route.projectId)?.name ??
                  activeProject?.name ??
                  ''
                }
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
            {route.page === 'settings' && (
              <SettingsShell section={route.section} />
            )}
          </>
        )}
      </main>
    </div>
  );
}

/** core 未就绪/崩溃时的主区故障态：给出真实原因与出路，而不是误导性的「先去登记项目」。 */
function CoreFailurePage() {
  return (
    <>
      <header className="sg-page-head">
        <span className="sg-page-head-title">核心服务异常</span>
      </header>
      <div className="sg-scroll">
        <div className="sg-card">
          <div className="sg-empty" style={{ padding: '40px 24px' }}>
            <div>本地核心（core）未就绪</div>
            <div className="sg-sub">
              项目、任务与模型配置都保存在本地核心里；恢复前无法继续操作。恢复后可在「设置 → 高级设置」查看诊断与日志。
            </div>
            <button
              className="sg-btn sg-btn--primary"
              style={{ marginTop: 8 }}
              onClick={() => window.location.reload()}
            >
              重试连接
            </button>
          </div>
        </div>
      </div>
    </>
  );
}

/* ---------------- 首屏（需求入口） ---------------- */

function HomePage({
  projects,
  activeProjectId,
  onNavigate,
  refreshKey,
}: {
  projects: Project[];
  activeProjectId: string | null;
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
                  先在「设置 → 项目与目录」登记一个本地项目目录，再回来发起需求
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
          <RecentTasks
            projects={projects}
            activeProjectId={activeProjectId}
            onNavigate={onNavigate}
            refreshKey={refreshKey}
          />
        </div>
      </div>
    </>
  );
}

function RecentTasks({
  projects,
  activeProjectId,
  onNavigate,
  refreshKey,
}: {
  projects: Project[];
  activeProjectId: string | null;
  onNavigate: (r: Route) => void;
  refreshKey: number;
}) {
  const [rows, setRows] = useState<{ project: Project; task: WorkItemSummary }[]>([]);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const all: { project: Project; task: WorkItemSummary }[] = [];
      // 全量项目遍历（本地 RPC）：截断到前 5 个会让第 6 个项目的任务在首页永远不可见。
      for (const p of projects) {
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
          {(activeProjectId || projects[0]) && (
            <button
              className="sg-btn sg-btn--primary sg-btn--sm"
              onClick={() =>
                onNavigate({ page: 'new', projectId: activeProjectId ?? projects[0].id })
              }
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
