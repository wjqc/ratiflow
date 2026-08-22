import { useMemo, useState } from 'react';
import { CheckIcon, AlertIcon, FolderIcon, RefreshIcon } from './Icons';
import ApprovalsPage from './ApprovalsPage';
import RequirementsEntry from './RequirementsEntry';
import Workbench from './Workbench';
import DiagnosticsView from './DiagnosticsView';
import type { Page } from './pageState';
import { navigate } from './pageState';

export default function App() {
  const [page, setPage] = useState<Page>('entry');
  const [workbenchId, setWorkbenchId] = useState('');

  const content = useMemo(() => {
    if (page === 'approvals') {
      return <ApprovalsPage />;
    }
    if (page === 'diagnostics') {
      return <DiagnosticsView />;
    }
    if (page === 'workbench' && workbenchId) {
      return <Workbench workItemId={workbenchId} onBack={() => setPage('entry')} />;
    }
    return <RequirementsEntry projectId="pj_demo" onOpenWorkbench={(id) => {
      setWorkbenchId(id);
      setPage('workbench');
    }} />;
  }, [page, workbenchId]);

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">通关 <strong>SixGates</strong></div>
        <nav aria-label="主导航">
          <ul className="nav-list">
            <li>
              <button
                type="button"
                className={`nav-item ${page === 'entry' || page === 'workbench' ? 'nav-active' : ''}`}
                aria-current={page === 'entry' || page === 'workbench' ? 'page' : undefined}
                onClick={() => navigate(setPage, 'entry', setWorkbenchId)}
              >
                🚪 需求入口 · 闯关
              </button>
            </li>
            <li>
              <button
                type="button"
                className={`nav-item ${page === 'approvals' ? 'nav-active' : ''}`}
                aria-current={page === 'approvals' ? 'page' : undefined}
                onClick={() => navigate(setPage, 'approvals', setWorkbenchId)}
              >
                🛡️ 审批中心
              </button>
            </li>
            <li>
              <button
                type="button"
                className={`nav-item ${page === 'diagnostics' ? 'nav-active' : ''}`}
                aria-current={page === 'diagnostics' ? 'page' : undefined}
                onClick={() => navigate(setPage, 'diagnostics', setWorkbenchId)}
              >
                🔧 本地诊断
              </button>
            </li>
          </ul>
        </nav>
        <div className="workspace-caption">六关：需求 → 方案 → 开发 → 测试 → 部署 → 验证</div>
      </aside>
      <main className="main-content">
        <header className="topbar"><span className="local-indicator" />本地运行 <span>· 仅本地连接</span></header>
        <div className="content-wrap">{content}</div>
      </main>
    </div>
  );
}
