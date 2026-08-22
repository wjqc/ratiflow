// S00 设置概览：聚合 diagnostics.check / core.version / audit.list，阻塞动作直达目标页。
import { useCallback, useEffect, useMemo, useState } from 'react';
import { rpc } from '../../rpc/client';
import { relativeTime } from '../../lib/format';
import type {
  AuditEvent,
  AuditListResult,
  CoreVersionInfo,
  DiagnosticsReport,
  IntegrationCheck,
} from './types';
import type { SettingsRouteId } from './settings-routes';
import { SettingsPageHeader } from './components/SettingsPageHeader';
import { SettingsSection } from './components/SettingsSection';
import { StatusPill } from './components/StatusPill';
import { IconChevronRight, IconRefresh } from '../../components/Icons';

export interface OverviewBlocker {
  key: string;
  title: string;
  impact: string;
  section: SettingsRouteId;
}

/** 从诊断集成检查推导待处理动作（纯函数，供单测）。 */
export function deriveBlockers(report: DiagnosticsReport | null): OverviewBlocker[] {
  if (!report) return [];
  const meta: Record<string, { title: string; impact: string; section: SettingsRouteId }> = {
    gitlab: { title: 'GitLab 未就绪', impact: '阻塞 Agent 运行与部署门禁', section: 'gitlab' },
    model: { title: '模型配置缺失', impact: '阻塞全部 Agent 阶段', section: 'models' },
    ssh: { title: 'SSH 目标未配置', impact: '阻塞目标机部署', section: 'ssh' },
  };
  return report.integrations
    .filter((c) => c.status !== 'ready' && meta[c.id])
    .map((c) => ({ key: c.id, ...meta[c.id] }));
}

type PillOf = 'ready' | 'pending' | 'error';

function pillOf(status: IntegrationCheck['status']): PillOf {
  if (status === 'ready') return 'ready';
  if (status === 'pending') return 'pending';
  return 'error';
}

function SummaryRow({ label, status, detail }: { label: string; status: React.ReactNode; detail?: string }) {
  return (
    <div className="sg-summary-row">
      <span className="sg-summary-label">{label}</span>
      <span className="sg-summary-status">{status}</span>
      <span className="sg-summary-detail sg-muted">{detail ?? ''}</span>
    </div>
  );
}

export function OverviewPage({ onNavigate }: { onNavigate: (section: SettingsRouteId) => void }) {
  const [diag, setDiag] = useState<DiagnosticsReport | null>(null);
  const [version, setVersion] = useState<CoreVersionInfo | null>(null);
  const [audit, setAudit] = useState<AuditEvent[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [d, v, a] = await Promise.all([
        rpc<DiagnosticsReport>('diagnostics.check'),
        rpc<CoreVersionInfo>('core.version'),
        rpc<AuditListResult>('audit.list', { limit: 50 }),
      ]);
      setDiag(d);
      setVersion(v);
      setAudit(a.items ?? []);
    } catch (e) {
      setError(e instanceof Error ? e.message : '加载失败');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const blockers = useMemo(() => deriveBlockers(diag), [diag]);
  const weekCount = useMemo(
    () => audit.filter((e) => Date.now() - new Date(e.time).getTime() < 7 * 86400_000).length,
    [audit],
  );

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="设置概览"
        scope="本地"
        description="聚合本地运行、外部集成、数据安全与待处理动作；有阻塞时先修复再继续交付。"
        actions={
          <button className="sg-btn" onClick={() => void load()} disabled={loading}>
            <IconRefresh size={14} />
            刷新
          </button>
        }
      />

      {error ? (
        <div className="sg-banner sg-banner--error" role="alert">
          <span>加载失败：{error}</span>
          <button className="sg-btn sg-btn--sm" onClick={() => void load()}>重试</button>
        </div>
      ) : null}

      <SettingsSection title="待处理动作" description={blockers.length ? '以下项阻塞交付链路，建议优先修复。' : undefined}>
        {loading ? (
          <div className="sg-skeleton-rows" aria-busy="true">
            <div className="sg-skeleton-row" />
            <div className="sg-skeleton-row" />
          </div>
        ) : blockers.length === 0 ? (
          <p className="sg-hint" style={{ margin: 0 }}>当前无阻塞项，可直接开始交付。</p>
        ) : (
          <div className="sg-stack" style={{ gap: 8 }}>
            {blockers.map((b) => (
              <div key={b.key} className="sg-blocker-row">
                <StatusPill kind="error" />
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ fontSize: 13, fontWeight: 500 }}>{b.title}</div>
                  <div className="sg-hint" style={{ margin: 0 }}>{b.impact}</div>
                </div>
                <button className="sg-btn sg-btn--primary sg-btn--sm" onClick={() => onNavigate(b.section)}>
                  去配置
                </button>
                <button className="sg-btn sg-btn--sm" onClick={() => onNavigate('diagnostics')}>
                  查看诊断
                </button>
              </div>
            ))}
          </div>
        )}
      </SettingsSection>

      <SettingsSection title="本地运行" description="Rust Core、执行器与数据库状态。">
        <div className="sg-summary-rows">
          <SummaryRow
            label="Rust Core"
            status={version ? <StatusPill kind="ready" label={`v${version.coreVersion}`} /> : <StatusPill kind="checking" />}
            detail={version ? `协议 v${version.protocolVersion} · schema v${version.schemaVersion}` : '读取中…'}
          />
          {diag?.local.map((c) => (
            <SummaryRow
              key={c.id}
              label={c.name}
              status={<StatusPill kind={pillOf(c.status)} />}
              detail={c.detail}
            />
          ))}
        </div>
      </SettingsSection>

      <SettingsSection title="外部集成" description="GitLab / 模型 / SSH 的启动装配状态；修复入口见诊断页。">
        <div className="sg-summary-rows">
          {diag?.integrations.map((c) => (
            <SummaryRow
              key={c.id}
              label={c.name}
              status={<StatusPill kind={pillOf(c.status)} />}
              detail={c.detail}
            />
          )) ?? <p className="sg-hint" style={{ margin: 0 }}>读取中…</p>}
        </div>
        <button className="sg-link-btn" onClick={() => onNavigate('diagnostics')}>
          打开运行与集成诊断 <IconChevronRight size={12} />
        </button>
      </SettingsSection>

      <SettingsSection title="数据安全" description="凭据只存引用，秘密值不入库；备份与审计为本地数据底座。">
        <div className="sg-summary-rows">
          <SummaryRow label="凭据引用" status={<StatusPill kind="dev" />} detail="credentialRef.list 契约待提交" />
          <SummaryRow label="最近备份" status={<StatusPill kind="dev" />} detail="backup.list 契约待提交" />
          <SummaryRow
            label="近 7 日审计事件"
            status={<StatusPill kind="ready" label={`${weekCount} 条`} />}
            detail={loading ? '读取中…' : undefined}
          />
        </div>
        <div className="sg-row">
          <button className="sg-link-btn" onClick={() => onNavigate('backup')}>
            备份与恢复 <IconChevronRight size={12} />
          </button>
          <button className="sg-link-btn" onClick={() => onNavigate('audit')}>
            审计日志 <IconChevronRight size={12} />
          </button>
        </div>
      </SettingsSection>

      <SettingsSection title="最近设置变化" description="来自本地审计事件（append-only），最新在前。">
        {audit.length === 0 && !loading ? (
          <p className="sg-hint" style={{ margin: 0 }}>暂无审计事件。</p>
        ) : (
          <div className="sg-stack" style={{ gap: 6 }}>
            {audit.slice(0, 5).map((e) => (
              <div key={e.seq} className="sg-row" style={{ fontSize: 12 }}>
                <span className="sg-muted" style={{ width: 90, flexShrink: 0 }}>{relativeTime(e.time)}</span>
                <code className="sg-code">{e.action}</code>
                <span className="sg-muted">
                  {e.actor} · {e.targetType}
                  {e.targetId ? `/${e.targetId}` : ''}
                </span>
              </div>
            ))}
          </div>
        )}
        <button className="sg-link-btn" onClick={() => onNavigate('audit')}>
          查看全部审计 <IconChevronRight size={12} />
        </button>
      </SettingsSection>
    </div>
  );
}
