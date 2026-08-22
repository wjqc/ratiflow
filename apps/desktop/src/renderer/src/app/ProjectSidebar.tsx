import type { ProjectSummaryInfo, Route, WorkItemSummary } from './AppShell';
import { gateLabel } from './AppShell';

interface Props {
  projects: ProjectSummaryInfo[];
  activeProjectId: string;
  workItems: WorkItemSummary[];
  activeWorkItemId: string;
  coreReady: boolean | null;
  route: Route;
  onProjectChange: (projectId: string) => void;
  onTaskOpen: (workItemId: string) => void;
  onNavigate: (route: Route) => void;
}

export default function ProjectSidebar({
  projects, activeProjectId, workItems, activeWorkItemId, coreReady, route,
  onProjectChange, onTaskOpen, onNavigate,
}: Props) {
  return (
    <aside className="sg-sidebar" aria-label="项目导航">
      <div className="sg-sidebar-head">
        <span className="sg-brand">通关 SixGates</span>
        {coreReady === false ? <span className="sg-status sg-status--error" title="Rust core 不可用">⚠ core</span> : null}
      </div>

      <nav style={{ padding: '4px 8px' }}>
        <button
          className={`sg-nav-item ${route.page === 'home' ? 'sg-nav-item--active' : ''}`}
          onClick={() => onNavigate({ page: 'home' })}
        >
          🚪 需求入口
        </button>
        {activeProjectId ? (
          <button
            className={`sg-nav-item ${route.page === 'new' ? 'sg-nav-item--active' : ''}`}
            onClick={() => onNavigate({ page: 'new', projectId: activeProjectId })}
          >
            ✚ 新建任务
          </button>
        ) : null}
        <button
          className={`sg-nav-item ${route.page === 'approvals' ? 'sg-nav-item--active' : ''}`}
          onClick={() => onNavigate({ page: 'approvals' })}
        >
          🛡️ 审批中心
        </button>
        <button
          className={`sg-nav-item ${route.page === 'settings' ? 'sg-nav-item--active' : ''}`}
          onClick={() => onNavigate({ page: 'settings' })}
        >
          ⚙ 设置与诊断
        </button>
      </nav>

      <div style={{ padding: '8px 16px 4px' }}>
        <span className="sg-muted">项目</span>
      </div>
      <nav style={{ padding: '0 8px' }}>
        {projects.length === 0 ? (
          <div className="sg-muted" style={{ padding: '4px 8px' }}>无项目</div>
        ) : (
          projects.map((p) => (
            <button
              key={p.id}
              className={`sg-nav-item ${p.id === activeProjectId ? 'sg-nav-item--active' : ''}`}
              aria-current={p.id === activeProjectId ? 'true' : undefined}
              onClick={() => onProjectChange(p.id)}
            >
              <span>{p.name || p.project}</span>
              <small className="sg-muted">{p.status === 'ready' ? '✓' : '…'}</small>
            </button>
          ))
        )}
      </nav>

      {activeProjectId ? (
        <>
          <div style={{ padding: '12px 16px 4px', display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
            <span className="sg-muted">任务列表</span>
            <button
              className="sg-nav-item"
              style={{ width: 'auto', padding: '2px 8px' }}
              onClick={() => onNavigate({ page: 'knowledge', projectId: activeProjectId })}
              title="项目知识库"
            >
              📚
            </button>
          </div>
          <ul className="sg-task-list">
            {workItems.length === 0 ? (
              <li className="sg-muted" style={{ padding: '4px 10px' }}>暂无任务</li>
            ) : (
              workItems.map((task) => (
                <li key={task.id}>
                  <button
                    className={`sg-task-item ${task.id === activeWorkItemId ? 'sg-task-item--active' : ''}`}
                    onClick={() => onTaskOpen(task.id)}
                  >
                    {task.title}
                    <small>{gateLabel(task.currentGate)}</small>
                  </button>
                </li>
              ))
            )}
          </ul>
        </>
      ) : null}
    </aside>
  );
}
