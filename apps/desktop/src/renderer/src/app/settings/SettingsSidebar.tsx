import { useEffect, useState } from 'react';
import { rpc } from '../../rpc/client';
import { IconArrowLeft, IconChevronDown, IconChevronRight } from '../../components/Icons';
import type { DiagnosticsReport } from './types';
import {
  DEFAULT_SETTINGS_ROUTE,
  SETTINGS_ADVANCED_ITEMS,
  SETTINGS_PRIMARY_ITEMS,
  isSettingsRouteId,
  type SettingsRouteMeta,
  type SettingsRouteId,
} from './settings-routes';

type DotTone = 'error' | 'warn';

/** 诊断检查项 → 导航项：GitLab/SSH 检查都归到「外部集成」页。 */
const DOT_TARGET: Record<string, SettingsRouteId> = {
  gitlab: 'integrations',
  model: 'models',
  ssh: 'integrations',
};

const ADVANCED_ROUTE_IDS = new Set<SettingsRouteId>(
  SETTINGS_ADVANCED_ITEMS.map((item) => item.id),
);

interface SettingsSidebarProps {
  section?: string;
  onNavigate: (section: SettingsRouteId) => void;
  onBack: () => void;
}

export function SettingsSidebar({ section, onNavigate, onBack }: SettingsSidebarProps) {
  const route = isSettingsRouteId(section) ? section : DEFAULT_SETTINGS_ROUTE;
  const [advancedOpen, setAdvancedOpen] = useState(() => ADVANCED_ROUTE_IDS.has(route));
  const [diag, setDiag] = useState<DiagnosticsReport | null>(null);

  useEffect(() => {
    if (ADVANCED_ROUTE_IDS.has(route)) setAdvancedOpen(true);
  }, [route]);

  useEffect(() => {
    rpc<DiagnosticsReport>('diagnostics.check')
      .then((report) => setDiag(Array.isArray(report.integrations) ? report : null))
      .catch(() => setDiag(null));
  }, []);

  const dotFor = (id: SettingsRouteId): DotTone | null => {
    if (!diag) return null;
    const hit = diag.integrations.find(
      (check) => DOT_TARGET[check.checkId] === id && check.status !== 'ready',
    );
    if (!hit) return null;
    return hit.status === 'pending' ? 'warn' : 'error';
  };

  const renderItem = (item: SettingsRouteMeta) => {
    const Icon = item.icon;
    const active = item.id === route;
    const dot = dotFor(item.id);
    return (
      <button
        key={item.id}
        className={`sg-settings-item ${active ? 'sg-settings-item--active' : ''}`}
        aria-current={active ? 'page' : undefined}
        onClick={() => onNavigate(item.id)}
      >
        <Icon size={15} />
        <span>{item.name}</span>
        {dot ? (
          <>
            <span className={`sg-dot sg-dot--${dot}`} aria-hidden />
            <span className="sg-sr-only">{dot === 'error' ? '存在异常' : '待配置'}</span>
          </>
        ) : null}
      </button>
    );
  };

  return (
    <aside className="sg-sidebar sg-settings-sidebar" aria-label="设置导航">
      <div className="sg-settings-sidebar-head">
        <button className="sg-icon-btn" onClick={onBack} title="返回工作区" aria-label="返回工作区">
          <IconArrowLeft size={17} />
        </button>
        <span>设置</span>
      </div>

      <nav className="sg-settings-sidebar-nav">
        <div className="sg-settings-group">常用</div>
        {SETTINGS_PRIMARY_ITEMS.map(renderItem)}

        <button
          className="sg-settings-menu-toggle"
          aria-expanded={advancedOpen}
          onClick={() => setAdvancedOpen((current) => !current)}
        >
          {advancedOpen ? <IconChevronDown size={14} /> : <IconChevronRight size={14} />}
          <span>高级设置</span>
        </button>
        {advancedOpen ? (
          <div className="sg-settings-menu-list">
            {SETTINGS_ADVANCED_ITEMS.map(renderItem)}
          </div>
        ) : null}
      </nav>

      <div className="sg-settings-sidebar-foot">设置仅保存在此设备</div>
    </aside>
  );
}
