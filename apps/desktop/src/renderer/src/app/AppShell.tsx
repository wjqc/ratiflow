import { useCallback, useEffect, useMemo, useState } from 'react';
import { rpc } from '../rpc/client';
import { ProjectSidebar, gateLabel, workItemGate } from './ProjectSidebar';
import type { KnowledgeSourceInfo, Project, WorkItemSummary } from './ProjectSidebar';
import { Workbench } from './Workbench';
import NewTaskPage from './NewTaskPage';
import ApprovalsPage from './ApprovalsPage';
import KnowledgePage from './KnowledgePage';
import SettingsPage from './SettingsPage';
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
  | { page: 'settings' };

export default function AppShell() {
  const [route, setRoute] = useState<Route>({ page: 'home' });
  const [projects, setProjects] = useState<Project[]>([]);
  const [activeProjectId, setActiveProjectId] = useState<string | null>(null);
  const [workItems, setWorkItems] = useState<WorkItemSummary[]>([]);
  const [knowledgeSources, setKnowledgeSources] = useState<KnowledgeSourceInfo[]>([]);
  const [coreReady, setCoreReady] = useState<boolean | null>(null);

  useEffect(() => {
    (async () => {
      try {
        const hello = await window.sixgates.hello();
        if (hello && hello.ok === false) throw new Error(hello.error ?? 'core 未就绪');
        setCoreReady(true);
        const result = await rpc<{ items: Project[] }>('project.list');
        setProjects(result.items ?? []);
        if (result.items?.length) setActiveProjectId((prev) => prev ?? result.items[0].id);
      } catch {
        setCoreReady(false);
      }
    })();
  }, []);

  const loadProjectData = useCallback(async (projectId: string) => {
    try {
      const r = await rpc<{ items: WorkItemSummary[] }>('workitem.list', { projectId, limit: 50 });
      setWorkItems(r.items ?? []);
    } catch {
      setWorkItems([]);
    }
    try {
      const r = await rpc<{ items: KnowledgeSourceInfo[] }>('knowledge.list', { projectId });
      setKnowledgeSources(r.items ?? []);
    } catch {
      setKnowledgeSources([]);
    }
  }, []);

  useEffect(() => {
    if (activeProjectId) void loadProjectData(activeProjectId);
  }, [activeProjectId, loadProjectData]);

  const navigate = useCallback((next: Route) => setRoute(next), []);

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
        workItems={workItems}
        activeWorkItemId={route.page === 'task' ? route.workItemId : null}
        coreReady={coreReady}
        route={route}
        knowledgeSources={knowledgeSources}
        onProjectChange={switchProject}
        onTaskOpen={(workItemId) => activeProjectId && openTask(activeProjectId, workItemId)}
        onNavigate={navigate}
      />
      <main className="sg-main">
        {route.page === 'home' && <HomePage projects={projects} onNavigate={navigate} />}
        {route.page === 'new' && (
          <NewTaskPage
            projectId={route.projectId}
            onCreated={(workItemId) => openTask(route.projectId, workItemId)}
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
        {route.page === 'settings' && <SettingsPage />}
      </main>
    </div>
  );
}

/* ---------------- 首屏（需求入口） ---------------- */

function HomePage({
  projects,
  onNavigate,
}: {
  projects: Project[];
  onNavigate: (r: Route) => void;
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
                  先在「设置与诊断 → 常规」登记一个本地项目目录，再回来发起需求
                </div>
                <button
                  className="sg-btn sg-btn--primary"
                  style={{ marginTop: 8 }}
                  onClick={() => onNavigate({ page: 'settings' })}
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
          <RecentTasks projects={projects} onNavigate={onNavigate} />
        </div>
      </div>
    </>
  );
}

function RecentTasks({
  projects,
  onNavigate,
}: {
  projects: Project[];
  onNavigate: (r: Route) => void;
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
  }, [projects]);

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
