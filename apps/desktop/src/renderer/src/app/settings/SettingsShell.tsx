// 设置中心壳：左侧分组导航（208px，aria-current，状态点）+ 顶栏面包屑 + 内容出口。
import { useEffect, useState } from 'react';
import { rpc } from '../../rpc/client';
import type { DiagnosticsReport } from './types';
import {
  DEFAULT_SETTINGS_ROUTE,
  SETTINGS_NAV,
  isSettingsRouteId,
  settingsRouteMeta,
  type SettingsRouteId,
} from './settings-routes';
import { OverviewPage } from './OverviewPage';
import { ProjectsPage } from './ProjectsPage';
import { BackupPage } from './BackupPage';
import { AuditPage } from './AuditPage';
import { DiagnosticsPage } from './DiagnosticsPage';
import { UpdatesPage } from './UpdatesPage';
import { LogsPage } from './LogsPage';
import { PendingSettingsPage } from './PendingPages';
import { GeneralPage } from './pages/GeneralPage';
import { AppearancePage } from './pages/AppearancePage';
import { KnowledgeDefaultsPage } from './pages/KnowledgeDefaultsPage';
import { MemoryPage } from './pages/MemoryPage';
import { ModelsPage } from './pages/ModelsPage';
import { ToolsPage } from './pages/ToolsPage';
import { AgentCenterPage } from './pages/AgentCenterPage';
import { ExecutionPage } from './pages/ExecutionPage';
import { GitlabPage } from './pages/GitlabPage';
import { SshPage } from './pages/SshPage';
import { CredentialsPage } from './pages/CredentialsPage';

type DotTone = 'error' | 'warn';

/** 导航状态点：集成未就绪时在对应入口给出红/琥珀点（不只用颜色，配有 sr-only 文本）。 */
const DOT_TARGET: Record<string, SettingsRouteId> = {
  gitlab: 'gitlab',
  model: 'models',
  ssh: 'ssh',
};

export default function SettingsShell({ section }: { section?: string }) {
  const route: SettingsRouteId = isSettingsRouteId(section) ? section : DEFAULT_SETTINGS_ROUTE;
  const [diag, setDiag] = useState<DiagnosticsReport | null>(null);

  useEffect(() => {
    rpc<DiagnosticsReport>('diagnostics.check')
      .then(setDiag)
      .catch(() => setDiag(null));
  }, []);

  const dotFor = (id: SettingsRouteId): DotTone | null => {
    if (!diag) return null;
    const hit = diag.integrations.find((c) => DOT_TARGET[c.checkId] === id && c.status !== 'ready');
    if (!hit) return null;
    return hit.status === 'pending' ? 'warn' : 'error';
  };

  const meta = settingsRouteMeta(route);

  const content = (() => {
    switch (route) {
      case 'overview':
        return <OverviewPage onNavigate={go} />;
      case 'projects':
        return <ProjectsPage />;
      case 'backup':
        return <BackupPage />;
      case 'audit':
        return <AuditPage />;
      case 'diagnostics':
        return <DiagnosticsPage onNavigate={go} />;
      case 'updates':
        return <UpdatesPage />;
      case 'logs':
        return <LogsPage />;
      case 'app-general':
        return <GeneralPage />;
      case 'appearance':
        return <AppearancePage />;
      case 'knowledge-defaults':
        return <KnowledgeDefaultsPage />;
      case 'memory':
        return <MemoryPage />;
      case 'models':
        return <ModelsPage />;
      case 'tools':
        return <ToolsPage />;
      case 'agent-center':
        return <AgentCenterPage />;
      case 'execution':
        return <ExecutionPage />;
      case 'gitlab':
        return <GitlabPage />;
      case 'ssh':
        return <SshPage />;
      case 'credentials':
        return <CredentialsPage />;
      default:
        return <PendingSettingsPage id={route} />;
    }
  })();

  function go(next: SettingsRouteId) {
    window.dispatchEvent(new CustomEvent('sg:settings-navigate', { detail: next }));
  }

  return (
    <>
      <div className="sg-page-head">
        <span className="sg-page-head-title">设置与诊断 / {meta.name}</span>
      </div>
      <div className="sg-settings">
        <nav className="sg-settings-nav" aria-label="设置导航">
          {SETTINGS_NAV.map((group) => (
            <div key={group.label} role="group" aria-label={group.label}>
              <div className="sg-settings-group">{group.label}</div>
              {group.items.map((item) => {
                const Icon = item.icon;
                const active = item.id === route;
                const dot = dotFor(item.id);
                return (
                  <button
                    key={item.id}
                    className={`sg-settings-item ${active ? 'sg-settings-item--active' : ''}`}
                    aria-current={active ? 'page' : undefined}
                    onClick={() => go(item.id)}
                  >
                    <Icon size={14} />
                    <span style={{ flex: 1 }}>{item.name}</span>
                    {dot ? (
                      <>
                        <span className={`sg-dot sg-dot--${dot}`} aria-hidden />
                        <span className="sg-sr-only">{dot === 'error' ? '存在异常' : '待配置'}</span>
                      </>
                    ) : null}
                  </button>
                );
              })}
            </div>
          ))}
        </nav>
        <div className="sg-settings-body">{content}</div>
      </div>
    </>
  );
}
