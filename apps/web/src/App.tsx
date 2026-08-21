import { useCallback, useEffect, useMemo, useState } from 'react';
import { loadDiagnostics, recheckDiagnostics } from './api';
import { AlertIcon, CheckIcon, FolderIcon, RefreshIcon } from './Icons';
import type { DiagnosticItem, DiagnosticReport, DiagnosticStatus } from './types';

const gates = ['需求关', '方案关', '开发关', '测试关', '部署关', '验证关'];

const statusCopy: Record<DiagnosticStatus, string> = {
  ready: '就绪',
  needs_configuration: '需配置',
  checking: '检测中',
  unavailable: '未检测',
};

function StatusMark({ status }: { status: DiagnosticStatus }) {
  const ready = status === 'ready';
  return (
    <span className={`status-mark status-${status}`} aria-label={statusCopy[status]}>
      {ready ? <CheckIcon /> : <AlertIcon />}
    </span>
  );
}

function Sidebar() {
  return (
    <aside className="sidebar">
      <div className="brand">通关 <strong>SixGates</strong></div>
      <nav aria-label="六关">
        <ol className="gate-list">
          {gates.map((gate, index) => (
            <li className={index === 0 ? 'gate-active' : ''} key={gate}>
              <span className="gate-index">{index + 1}</span>
              <span>{gate}</span>
            </li>
          ))}
        </ol>
      </nav>
      <div className="workspace-caption">工作区 · Epic 0</div>
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

function EventLog({ report }: { report: DiagnosticReport }) {
  const rows = useMemo(() => {
    const time = new Date(report.generatedAt).toLocaleTimeString('zh-CN', { hour12: false });
    return report.integrations.map((item) => ({ time, source: item.id, level: item.status === 'ready' ? 'INFO' : 'WARN', event: item.detail }));
  }, [report]);
  return (
    <section className="panel event-panel" aria-labelledby="event-title">
      <div className="panel-heading"><h2 id="event-title">事件日志</h2><span>最近一次诊断</span></div>
      <div className="event-table">
        <div className="event-row event-header"><span>时间</span><span>级别</span><span>来源</span><span>事件</span></div>
        {rows.map((row) => (
          <div className="event-row" key={`${row.source}-${row.event}`}><span>{row.time}</span><span className={`log-level level-${row.level.toLowerCase()}`}>{row.level}</span><span>{row.source}</span><span>{row.event}</span></div>
        ))}
      </div>
    </section>
  );
}

export default function App() {
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
    <div className="app-shell">
      <Sidebar />
      <main className="main-content">
        <header className="topbar"><span className="local-indicator" />本地运行 <span>· 仅本地连接</span></header>
        <div className="content-wrap">
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
              <EventLog report={report} />
            </>
          ) : <div className="panel loading-panel">正在读取本地诊断…</div>}
        </div>
      </main>
    </div>
  );
}
