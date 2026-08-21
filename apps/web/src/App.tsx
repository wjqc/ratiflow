import { useCallback, useEffect, useMemo, useState } from 'react';
import { loadDiagnostics, recheckDiagnostics } from './api';
import { AlertIcon, CheckIcon, FolderIcon, RefreshIcon } from './Icons';
import ApprovalsPage from './ApprovalsPage';
import WorkItemDetail from './WorkItemDetail';
import WorkItemsPage from './WorkItemsPage';
import type { DiagnosticItem, DiagnosticReport, DiagnosticStatus } from './types';
import { gateOrder, gateLabels } from './types';

const statusCopy: Record<DiagnosticStatus, string> = {
  ready: '就绪',
  needs_configuration: '需配置',
  checking: '检测中',
  unavailable: '未检测',
};

type Page = 'diagnostics' | 'workitems' | 'approvals';

function StatusMark({ status }: { status: DiagnosticStatus }) {
  const ready = status === 'ready';
  return (
    <span className={`status-mark status-${status}`} aria-label={statusCopy[status]}>
      {ready ? <CheckIcon /> : <AlertIcon />}
    </span>
  );
}

function Sidebar({ page, onNavigate }: { page: Page; onNavigate: (page: Page) => void }) {
  const navItems: Array<{ key: Page; label: string }> = [
    { key: 'diagnostics', label: '本地诊断' },
    { key: 'workitems', label: '工作项' },
    { key: 'approvals', label: '审批中心' },
  ];
  return (
    <aside className="sidebar">
      <div className="brand">通关 <strong>SixGates</strong></div>
      <nav aria-label="主导航">
        <ul className="nav-list">
          {navItems.map((item) => (
            <li key={item.key}>
              <button
                type="button"
                className={`nav-item ${page === item.key ? 'nav-active' : ''}`}
                aria-current={page === item.key ? 'page' : undefined}
                onClick={() => onNavigate(item.key)}
              >
                {item.label}
              </button>
            </li>
          ))}
        </ul>
      </nav>
      <nav aria-label="六关">
        <ol className="gate-list">
          {gateOrder.map((gate) => (
            <li key={gate}>
              <span>{gateLabels[gate]}</span>
            </li>
          ))}
        </ol>
      </nav>
      <div className="workspace-caption">本地运行 · 仅 loopback</div>
    </aside>
  );
}

function IntegrationTable({ items, onRecheck }: { items: DiagnosticItem[]; onRecheck: () => void }) {
  return (
    <section className="panel integration-panel" aria-labelledby="integration-title">
      <h2 id="integration-title">集成依赖诊断</h2>
      <div className="integration-header" aria-hidden="true">
        <span>服务</span><span>状态</span><span>诊断详情</span><span>操作</span>
      </div>
      <div className="integration-list">
        {items.map((item) => (
          <div className="integration-row" key={item.id}>
            <div className="service-name"><span className="service-code">{item.id.slice(0, 2).toUpperCase()}</span>{item.label}</div>
            <div className={`status-text status-${item.status}`}><span className="status-dot" />{statusCopy[item.status]}</div>
            <div className="detail"><StatusMark status={item.status} /><span>{item.detail}</span></div>
            <button className="row-action" type="button" onClick={onRecheck}>{item.status === 'needs_configuration' ? '配置' : '重新检测'}</button>
          </div>
        ))}
      </div>
    </section>
  );
}

function LocalHealth({ items }: { items: DiagnosticItem[] }) {
  return (
    <section className="panel health-panel" aria-labelledby="health-title">
      <h2 id="health-title">本地服务健康</h2>
      <div className="health-grid">
        {items.map((item) => (
          <div className="health-item" key={item.id}>
            <StatusMark status={item.status} />
            <div><strong>{item.label}</strong><span>{item.detail}</span></div>
          </div>
        ))}
      </div>
    </section>
  );
}

function DiagnosticsView() {
  const [report, setReport] = useState<DiagnosticReport | null>(null);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(true);

  const refresh = useCallback(async (recheck: boolean) => {
    setLoading(true);
    setError('');
    try {
      setReport(await (recheck ? recheckDiagnostics() : loadDiagnostics()));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '诊断请求失败');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh(false);
  }, [refresh]);

  const openProject = () => {
    window.location.href = 'vscode://sixgates.sixgates/openProject';
  };

  return (
    <>
      <div className="page-heading">
        <div><h1>本地运行诊断</h1><p>检测本地集成依赖与服务健康状况，确保 AI 软件交付流水线可用。</p></div>
        <div className="heading-actions">
          <button className="secondary-button" type="button" onClick={() => void refresh(true)} disabled={loading}><RefreshIcon />{loading ? '检测中' : '重新检测'}</button>
          <button className="primary-button" type="button" onClick={openProject}><FolderIcon />打开项目</button>
        </div>
      </div>
      {error ? <div className="error-banner" role="alert">{error}</div> : null}
      {report ? (
        <>
          <IntegrationTable items={report.integrations} onRecheck={() => void refresh(true)} />
          <LocalHealth items={report.local} />
        </>
      ) : <div className="panel loading-panel">正在读取本地诊断…</div>}
    </>
  );
}

export default function App() {
  const [page, setPage] = useState<Page>('diagnostics');
  const [detailId, setDetailId] = useState('');
  // MVP：单项目演示（pj_demo）；多项目切换在后续版本接入项目 API。
  const projectId = 'pj_demo';

  const content = useMemo(() => {
    if (page === 'approvals') {
      return <ApprovalsPage />;
    }
    if (page === 'workitems') {
      if (detailId) {
        return <WorkItemDetail workItemId={detailId} onBack={() => setDetailId('')} />;
      }
      return <WorkItemsPage projectId={projectId} onOpenDetail={setDetailId} />;
    }
    return <DiagnosticsView />;
  }, [page, detailId, projectId]);

  const navigate = (next: Page) => {
    setDetailId('');
    setPage(next);
  };

  return (
    <div className="app-shell">
      <Sidebar page={page} onNavigate={navigate} />
      <main className="main-content">
        <header className="topbar"><span className="local-indicator" />本地运行 <span>· 仅本地连接</span></header>
        <div className="content-wrap">{content}</div>
      </main>
    </div>
  );
}
