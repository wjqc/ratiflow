import { useCallback, useEffect, useState } from 'react';
import ProjectSidebar from './ProjectSidebar';
import Workbench from './Workbench';
import NewTaskPage from './NewTaskPage';
import KnowledgePage from './KnowledgePage';
import ApprovalsPage from './ApprovalsPage';
import SettingsPage from './SettingsPage';
import { rpc } from '../rpc/client';

export type Route =
  | { page: 'home' }
  | { page: 'new'; projectId: string }
  | { page: 'task'; projectId: string; workItemId: string }
  | { page: 'knowledge'; projectId: string }
  | { page: 'approvals' }
  | { page: 'settings' };

export interface ProjectSummaryInfo {
  id: string;
  name: string;
  namespace: string;
  project: string;
  status: string;
}

export interface WorkItemSummary {
  id: string;
  title: string;
  currentGate: string;
}

export default function AppShell() {
  const [route, setRoute] = useState<Route>({ page: 'home' });
  const [projects, setProjects] = useState<ProjectSummaryInfo[]>([]);
  const [activeProjectId, setActiveProjectId] = useState('');
  const [workItems, setWorkItems] = useState<WorkItemSummary[]>([]);
  const [activeWorkItemId, setActiveWorkItemId] = useState('');
  const [coreReady, setCoreReady] = useState<boolean | null>(null);

  const refreshProjects = useCallback(async () => {
    try {
      const result = await rpc<{ items: ProjectSummaryInfo[] }>('project.list', { includeArchived: false });
      setProjects(result.items);
      if (!activeProjectId && result.items.length > 0) {
        setActiveProjectId(result.items[0].id);
      }
    } catch {
      setCoreReady(false);
    }
  }, [activeProjectId]);

  const refreshWorkItems = useCallback(async () => {
    if (!activeProjectId) {
      setWorkItems([]);
      return;
    }
    try {
      const result = await rpc<{ items: WorkItemSummary[] }>('workitem.list', { projectId: activeProjectId, limit: 30 });
      setWorkItems(result.items);
    } catch {
      setWorkItems([]);
    }
  }, [activeProjectId]);

  useEffect(() => {
    void (async () => {
      const hello = await window.sixgates.hello();
      setCoreReady(hello?.ok ?? false);
    })();
  }, []);

  useEffect(() => {
    void refreshProjects();
  }, [refreshProjects]);

  useEffect(() => {
    void refreshWorkItems();
  }, [refreshWorkItems]);

  const openTask = (projectId: string, workItemId: string) => {
    setActiveProjectId(projectId);
    setActiveWorkItemId(workItemId);
    setRoute({ page: 'task', projectId, workItemId });
  };

  const switchProject = (projectId: string) => {
    // 项目切换必须清空上一个项目的任务选择与上下文（规范 §4.4）。
    setActiveProjectId(projectId);
    setActiveWorkItemId('');
    setRoute({ page: 'home' });
    void refreshWorkItems();
  };

  const navigate = (next: Route) => {
    setRoute(next);
    if (next.page === 'task') {
      setActiveProjectId(next.projectId);
      setActiveWorkItemId(next.workItemId);
    }
  };

  return (
    <div className="sg-shell">
      <ProjectSidebar
        projects={projects}
        activeProjectId={activeProjectId}
        workItems={workItems}
        activeWorkItemId={activeWorkItemId}
        coreReady={coreReady}
        route={route}
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
            key={route.workItemId}
            projectId={route.projectId}
            workItemId={route.workItemId}
            onOpenApprovals={() => navigate({ page: 'approvals' })}
          />
        )}
        {route.page === 'knowledge' && <KnowledgePage projectId={route.projectId} />}
        {route.page === 'approvals' && <ApprovalsPage onDecided={() => void refreshWorkItems()} />}
        {route.page === 'settings' && <SettingsPage />}
      </main>
    </div>
  );
}

function HomePage({ projects, onNavigate }: { projects: ProjectSummaryInfo[]; onNavigate: (route: Route) => void }) {
  return (
    <div className="sg-section" style={{ maxWidth: 860 }}>
      <h1 style={{ fontSize: 20, fontWeight: 700 }}>写下你的需求，开始闯关</h1>
      <p className="sg-muted" style={{ marginTop: 0 }}>
        需求以文档形式保存在本地工作目录，Agent 按需求关 → 方案关 → 开发关 → 测试关 → 部署关 → 验证关逐关推进；所有结论由门禁引擎基于证据计算。
      </p>
      {projects.length === 0 ? (
        <div className="sg-banner sg-banner--info">
          还没有项目。到 <strong>设置 → 项目</strong> 登记 GitLab 项目与本地仓库目录后开始。
        </div>
      ) : (
        <div className="sg-row" style={{ marginTop: 12 }}>
          {projects.map((p) => (
            <button
              key={p.id}
              className="sg-button sg-button--primary"
              onClick={() => onNavigate({ page: 'new', projectId: p.id })}
            >
              在「{p.name || p.project}」新建任务
            </button>
          ))}
        </div>
      )}
      <section style={{ marginTop: 24 }}>
        <h2 className="sg-section-title">最近任务</h2>
        <RecentTasks onOpen={(projectId, workItemId) => onNavigate({ page: 'task', projectId, workItemId })} />
      </section>
    </div>
  );
}

function RecentTasks({ onOpen }: { onOpen: (projectId: string, workItemId: string) => void }) {
  const [projects, setProjects] = useState<ProjectSummaryInfo[]>([]);
  const [tasks, setTasks] = useState<Array<WorkItemSummary & { projectId: string }>>([]);
  useEffect(() => {
    void (async () => {
      try {
        const projectList = (await rpc<{ items: ProjectSummaryInfo[] }>('project.list', { includeArchived: false })).items;
        setProjects(projectList);
        const all: Array<WorkItemSummary & { projectId: string }> = [];
        for (const p of projectList.slice(0, 5)) {
          const result = await rpc<{ items: WorkItemSummary[] }>('workitem.list', { projectId: p.id, limit: 5 });
          all.push(...result.items.map((t) => ({ ...t, projectId: p.id })));
        }
        setTasks(all);
      } catch {
        setTasks([]);
      }
    })();
  }, []);
  if (tasks.length === 0) {
    return <div className="sg-empty">暂无任务</div>;
  }
  const name = (id: string) => projects.find((p) => p.id === id)?.name ?? id.slice(0, 8);
  return (
    <table className="sg-table">
      <thead>
        <tr><th>任务</th><th>项目</th><th>当前关</th><th></th></tr>
      </thead>
      <tbody>
        {tasks.map((task) => (
          <tr key={task.id}>
            <td>{task.title}</td>
            <td className="sg-muted">{name(task.projectId)}</td>
            <td><span className="sg-status sg-status--running">{gateLabel(task.currentGate)}</span></td>
            <td><button className="sg-button" onClick={() => onOpen(task.projectId, task.id)}>继续闯关 →</button></td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

export function gateLabel(gate: string): string {
  const labels: Record<string, string> = {
    requirements: '需求关', design: '方案关', development: '开发关',
    testing: '测试关', deployment: '部署关', verification: '验证关',
  };
  return labels[gate] ?? gate;
}
