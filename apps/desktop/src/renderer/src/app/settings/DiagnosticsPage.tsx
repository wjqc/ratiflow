// S50 运行与集成诊断：diagnostics.check 真实数据 + 每项修复入口直达目标设置页（§5.14）。
import { useCallback, useEffect, useState } from 'react';
import { rpc } from '../../rpc/client';
import { formatDateTime } from '../../lib/format';
import type { DiagnosticsReport, IntegrationCheck, LocalCheck } from './types';
import type { SettingsRouteId } from './settings-routes';
import { SettingsPageHeader } from './components/SettingsPageHeader';
import { SettingsSection } from './components/SettingsSection';
import { StatusPill } from './components/StatusPill';
import { IconChevronRight, IconRefresh } from '../../components/Icons';

/** 检查项 → 修复目标页（纯映射，供单测）。 */
export const FIX_TARGET: Record<string, SettingsRouteId> = {
  gitlab: 'gitlab',
  model: 'models',
  ssh: 'ssh',
  sqlite: 'backup',
  core: 'logs',
  executor: 'execution',
};

function pillKind(status: string) {
  if (status === 'ready') return 'ready' as const;
  if (status === 'pending') return 'pending' as const;
  if (status === 'disabled' || status === 'needs_configuration') return 'readonly' as const;
  return 'error' as const;
}

function CheckTable({
  checks,
  onNavigate,
}: {
  checks: Array<IntegrationCheck | LocalCheck>;
  onNavigate: (section: SettingsRouteId) => void;
}) {
  return (
    <table className="sg-table">
      <thead>
        <tr>
          <th style={{ width: 110 }}>检查项</th>
          <th style={{ width: 100 }}>状态</th>
          <th>详情（含环境变量名）</th>
          <th style={{ width: 110 }}>修复入口</th>
        </tr>
      </thead>
      <tbody>
        {checks.map((c) => (
          <tr key={c.checkId}>
            <td>{c.label}</td>
            <td><StatusPill kind={pillKind(c.status)} /></td>
            <td className="sg-muted" style={{ wordBreak: 'break-all' }}>{c.detail}</td>
            <td>
              {FIX_TARGET[c.checkId] ? (
                <button className="sg-link-btn" onClick={() => onNavigate(FIX_TARGET[c.checkId])}>
                  去处理 <IconChevronRight size={12} />
                </button>
              ) : (
                <span className="sg-hint">—</span>
              )}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

export function DiagnosticsPage({ onNavigate }: { onNavigate: (section: SettingsRouteId) => void }) {
  const [report, setReport] = useState<DiagnosticsReport | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const runChecks = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setReport(await rpc<DiagnosticsReport>('diagnostics.check'));
    } catch (e) {
      setError(e instanceof Error ? e.message : '诊断执行失败');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void runChecks();
  }, [runChecks]);

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="运行与集成诊断"
        scope="本地"
        description="逐项列出 core / 执行器 / SQLite 与外部集成就绪状态；异常项给出修复入口。"
        actions={
          <button className="sg-btn sg-btn--primary" onClick={() => void runChecks()} disabled={loading}>
            <IconRefresh size={14} />
            {loading ? '检查中…' : '运行全部检查'}
          </button>
        }
      />

      <div aria-live="polite">
        {error ? <div className="sg-banner sg-banner--error" role="alert">诊断失败：{error}</div> : null}
        {report ? (
          <p className="sg-hint">上次检查时间：{formatDateTime(report.generatedAt)}</p>
        ) : null}
      </div>

      <SettingsSection title="本地运行" description="core 进程、命令执行器、SQLite 迁移状态。">
        {report ? (
          <CheckTable checks={report.local} onNavigate={onNavigate} />
        ) : (
          <div className="sg-skeleton-rows" aria-busy="true">
            <div className="sg-skeleton-row" />
            <div className="sg-skeleton-row" />
          </div>
        )}
      </SettingsSection>

      <SettingsSection
        title="外部集成"
        description="详情中出现的环境变量名（如 SIXGATES_GITLAB_BASE_URL）为启动时装配来源；gitlabProfile/modelProfile 契约到位后迁移为应用内配置。"
      >
        {report ? (
          <CheckTable checks={report.integrations} onNavigate={onNavigate} />
        ) : (
          <div className="sg-skeleton-rows" aria-busy="true">
            <div className="sg-skeleton-row" />
            <div className="sg-skeleton-row" />
          </div>
        )}
      </SettingsSection>
    </div>
  );
}
