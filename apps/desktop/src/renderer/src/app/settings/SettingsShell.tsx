// 设置内容壳：设置导航由 AppShell 在主侧栏原位置渲染，此处只负责页面内容。
import {
  DEFAULT_SETTINGS_ROUTE,
  isSettingsRouteId,
  type SettingsRouteId,
} from './settings-routes';
import { ProjectsPage } from './ProjectsPage';
import { BackupPage } from './BackupPage';
import { DiagnosticsPage } from './DiagnosticsPage';
import { UpdatesPage } from './UpdatesPage';
import { PendingSettingsPage } from './PendingPages';
import { GeneralPage } from './pages/GeneralPage';
import { KnowledgeDefaultsPage } from './pages/KnowledgeDefaultsPage';
import { MemoryPage } from './pages/MemoryPage';
import { ModelsPage } from './pages/ModelsPage';
import { ToolsPage } from './pages/ToolsPage';
import { McpPage } from './pages/McpPage';
import { SkillsPage } from './pages/SkillsPage';
import { AgentCenterPage } from './pages/AgentCenterPage';
import { ExecutionPage } from './pages/ExecutionPage';
import { IntegrationsPage } from './pages/IntegrationsPage';

export default function SettingsShell({ section }: { section?: string }) {
  const route: SettingsRouteId = isSettingsRouteId(section) ? section : DEFAULT_SETTINGS_ROUTE;

  const content = (() => {
    switch (route) {
      case 'projects':
        return <ProjectsPage />;
      case 'backup':
        return <BackupPage />;
      case 'diagnostics':
        return <DiagnosticsPage />;
      case 'updates':
        return <UpdatesPage />;
      case 'app-general':
        return <GeneralPage />;
      case 'knowledge-defaults':
        return <KnowledgeDefaultsPage />;
      case 'memory':
        return <MemoryPage />;
      case 'models':
        return <ModelsPage />;
      case 'tools':
        return <ToolsPage />;
      case 'mcp':
        return <McpPage />;
      case 'skills':
        return <SkillsPage />;
      case 'agent-center':
        return <AgentCenterPage />;
      case 'execution':
        return <ExecutionPage />;
      case 'integrations':
        return <IntegrationsPage />;
      default:
        return <PendingSettingsPage id={route} />;
    }
  })();

  return (
    <div className="sg-settings-body">{content}</div>
  );
}
