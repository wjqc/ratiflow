// S00 设置概览：settings.summary 单调用聚合阻塞/组件/数据安全/最近变更，阻塞动作直达目标页。
import { useCallback, useEffect, useMemo, useState } from 'react';
import { rpc } from '../../rpc/client';
import { relativeTime } from '../../lib/format';
import type { DiagnosticsReport, SettingsSummary } from './types';
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

/** 从诊断集成检查推导待处理动作（纯函数，供单测，S50 兼容）。 */
export function deriveBlockers(report: DiagnosticsReport | null): OverviewBlocker[] {
  if (!report) return [];
  const meta: Record<string, { title: string; impact: string; section: SettingsRouteId }> = {
    gitlab: { title: 'GitLab 未就绪', impact: '阻塞 Agent 运行与部署门禁', section: 'gitlab' },
    model: { title: '模型配置缺失', impact: '阻塞全部 Agent 阶段', section: 'models' },
    ssh: { title: 'SSH 目标未配置', impact: '阻塞目标机部署', section: 'ssh' },
  };
  return report.integrations
    .filter((c) => c.status !== 'ready' && meta[c.checkId])
    .map((c) => ({ key: c.checkId, ...meta[c.checkId] }));
}

/** settings.summary targetRoute（如 "/settings/models"）→ SettingsRouteId 白名单映射。 */
const ROUTE_BY_TARGET: Record<string, SettingsRouteId> = {
  '/settings/models': 'models',
  '/settings/gitlab': 'gitlab',
  '/settings/ssh': 'ssh',
  '/settings/credentials': 'credentials',
  '/settings/backup': 'backup',
  '/settings/knowledge': 'knowledge-defaults',
  '/settings/tools': 'tools',
  '/settings/execution': 'execution',
};

/** blocker.id → 中文标题/影响（titleKey 由核心侧给 i18n key，前端按 id 直出）。 */
const BLOCKER_META: Record<string, { title: string; impact: string }> = {
  model_not_configured: { title: '模型配置缺失', impact: '阻塞全部 Agent 阶段' },
  gitlab_not_configured: { title: 'GitLab 未就绪', impact: '阻塞问题导入与 MR 门禁' },
  ssh_not_configured: { title: 'SSH 目标未配置', impact: '阻塞目标机部署' },
};

/** 从 settings.summary.blockers 推导待处理动作（targetRoute 映射，纯函数）。 */
export function summaryBlockers(summary: SettingsSummary | null): OverviewBlocker[] {
  if (!summary) return [];
  return summary.blockers
    .filter((b) => ROUTE_BY_TARGET[b.targetRoute])
    .map((b) => ({
      key: b.id,
      ...(BLOCKER_META[b.id] ?? { title: b.id, impact: b.capabilities.join('、') }),
      section: ROUTE_BY_TARGET[b.targetRoute],
    }));
}

type PillOf = 'ready' | 'pending' | 'error';

function pillOf(status: string): PillOf {
  if (status === 'ready') return 'ready';
  if (status === 'error') return 'error';
  return 'pending';
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

const COMPONENT_LABEL: Record<string, string> = {
  model: '模型与路由',
  gitlab: 'GitLab',
  ssh: 'SSH 目标机',
  knowledge: '知识来源',
};

export function OverviewPage({ onNavigate }: { onNavigate: (section: SettingsRouteId) => void }) {
  const [summary, setSummary] = useState<SettingsSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setSummary(await rpc<SettingsSummary>('settings.summary'));
    } catch (e) {
      setError(e instanceof Error ? e.message : '加载失败');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const blockers = useMemo(() => summaryBlockers(summary), [summary]);
  const lastBackup = summary?.dataSafety.lastBackup ?? null;

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="设置概览"
        scope="本地"
        description="聚合外部集成、数据安全与待处理动作；有阻塞时先修复再继续交付。"
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

      <SettingsSection title="组件与集成" description="模型、GitLab、SSH 与知识来源的当前装配状态。">
        <div className="sg-summary-rows">
          {summary?.components.map((c) => (
            <SummaryRow
              key={c.id}
              label={COMPONENT_LABEL[c.id] ?? c.id}
              status={<StatusPill kind={pillOf(c.status)} />}
              detail={componentDetail(c)}
            />
          )) ?? <p className="sg-hint" style={{ margin: 0 }}>读取中…</p>}
        </div>
      </SettingsSection>

      <SettingsSection title="数据安全" description="凭据只存引用，秘密值不入库；备份与审计为本地数据底座。">
        <div className="sg-summary-rows">
          <SummaryRow
            label="凭据引用"
            status={summary ? <StatusPill kind="ready" label={`${summary.dataSafety.credentialRefCount} 个`} /> : <StatusPill kind="checking" />}
            detail={summary ? '仅存引用，不存秘密值' : undefined}
          />
          <SummaryRow
            label="最近备份"
            status={
              summary
                ? lastBackup
                  ? <StatusPill kind={lastBackup.verified ? 'ready' : 'pending'} label={lastBackup.verified ? '已校验' : '未校验'} />
                  : <StatusPill kind="pending" label="无备份" />
                : <StatusPill kind="checking" />
            }
            detail={lastBackup ? `${relativeTime(lastBackup.createdAt)} · ${lastBackup.id}` : undefined}
          />
          <SummaryRow
            label="近 7 日审计事件"
            status={summary ? <StatusPill kind="ready" label={`${summary.dataSafety.auditEventsLast7Days} 条`} /> : <StatusPill kind="checking" />}
            detail={undefined}
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

      <SettingsSection title="最近设置变化" description="来自设置项修订记录（append-only），最新在前。">
        {!summary ? (
          <p className="sg-hint" style={{ margin: 0 }}>读取中…</p>
        ) : summary.recentChanges.length === 0 ? (
          <p className="sg-hint" style={{ margin: 0 }}>暂无设置变更记录。</p>
        ) : (
          <div className="sg-stack" style={{ gap: 6 }}>
            {summary.recentChanges.map((c) => (
              <div key={`${c.key}-${c.revision}`} className="sg-row" style={{ fontSize: 12 }}>
                <span className="sg-muted" style={{ width: 90, flexShrink: 0 }}>{relativeTime(c.updatedAt)}</span>
                <code className="sg-code">{c.key}</code>
                <span className="sg-muted">
                  r{c.revision} · {c.updatedBy}
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

/** components 条目的补充详情文案。 */
function componentDetail(c: NonNullable<SettingsSummary['components']>[number]): string | undefined {
  if (c.id === 'model' && typeof c.profileCount === 'number') {
    return `${c.profileCount} 个 Profile${c.managedOnly ? ' · 仅托管' : ''}`;
  }
  if (c.id === 'gitlab' && typeof c.profileCount === 'number') return `${c.profileCount} 个实例`;
  if (c.id === 'ssh' && typeof c.targetCount === 'number') return `${c.targetCount} 个目标`;
  if (c.id === 'knowledge' && typeof c.sourceCount === 'number') return `${c.sourceCount} 个来源`;
  return undefined;
}
