// 使用统计：只展示真实的模型缓存观测（用量卡片 + 每日趋势）。
import { useCallback, useEffect, useState } from 'react';
import { rpc } from '../../rpc/client';
import { IconRefresh } from '../../components/Icons';
import { SettingsPageHeader } from './components/SettingsPageHeader';
import { SettingsSection } from './components/SettingsSection';

interface DailyPoint {
  day: string;
  totalTokens: number;
  cachedTokens: number;
  cacheHitRatio: number | null;
}

interface ModelUsage {
  tokensIn: number;
  tokensOut: number;
  cachedTokens: number;
  cacheHitRatio: number | null;
  daily?: DailyPoint[];
}

function TokenMetric({ label, value, tone }: { label: string; value: string; tone?: 'accent' | 'success' }) {
  return (
    <div className={`sg-token-metric ${tone ? `sg-token-metric--${tone}` : ''}`}>
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

/// 纵轴取整到 1/2/2.5/5×10^k，避免刻度出现零碎数字。
function niceMax(v: number): number {
  if (v <= 0) return 1;
  const base = 10 ** Math.floor(Math.log10(v));
  for (const m of [1, 2, 2.5, 5, 10]) {
    if (v <= m * base) return m * base;
  }
  return 10 * base;
}

function fmtCompact(n: number): string {
  if (n >= 1_000_000) return `${Number((n / 1_000_000).toPrecision(3))}M`;
  if (n >= 1_000) return `${Number((n / 1_000).toPrecision(3))}k`;
  return String(n);
}

const CHART_W = 760;
const CHART_H = 240;
const PAD = { top: 14, right: 46, bottom: 30, left: 58 };
const PLOT_W = CHART_W - PAD.left - PAD.right;
const PLOT_H = CHART_H - PAD.top - PAD.bottom;
const RATIO_MAX = 100;

function xAt(i: number, n: number): number {
  return n <= 1 ? PAD.left + PLOT_W / 2 : PAD.left + (i * PLOT_W) / (n - 1);
}

function yTokens(v: number, max: number): number {
  return PAD.top + PLOT_H * (1 - v / max);
}

function yRatio(pct: number): number {
  return PAD.top + PLOT_H * (1 - pct / RATIO_MAX);
}

function UsageLine({ values, getX, getY, stroke, dashed }: {
  values: Array<number | null>;
  getX: (i: number) => number;
  getY: (v: number) => number;
  stroke: string;
  dashed?: string;
}) {
  // 只连接相邻有效点：中间出现空洞（null）时断线，不跨天硬连；孤立点不生成线，只留数据圆点。
  const segments: string[] = [];
  let current: string[] = [];
  const flush = () => {
    if (current.length > 1) segments.push(current.join(' '));
    current = [];
  };
  values.forEach((v, i) => {
    if (v == null) {
      flush();
      return;
    }
    current.push(`${getX(i).toFixed(1)},${getY(v).toFixed(1)}`);
    if (i === values.length - 1) flush();
  });
  return (
    <>
      {segments.map((pts) => (
        <polyline key={`${stroke}-${pts.slice(0, 24)}`} points={pts} fill="none" stroke={stroke} strokeWidth={2} strokeDasharray={dashed} strokeLinejoin="round" strokeLinecap="round" />
      ))}
    </>
  );
}

function DailyUsageChart({ points }: { points: DailyPoint[] }) {
  const maxTokens = niceMax(Math.max(...points.map((p) => Math.max(p.totalTokens, p.cachedTokens)), 0));
  const gridRatios = [0, 0.25, 0.5, 0.75, 1];
  const hasMultiple = points.length > 1;
  const xLabels = hasMultiple
    ? [points[0], points[Math.floor((points.length - 1) / 2)], points[points.length - 1]]
    : points;
  const pointTitle = (p: DailyPoint) =>
    `${p.day}：总 ${p.totalTokens.toLocaleString()} / 命中 ${p.cachedTokens.toLocaleString()}` +
    (p.cacheHitRatio == null ? '' : ` / 命中率 ${Math.round(p.cacheHitRatio * 100)}%`);

  return (
    <svg viewBox={`0 0 ${CHART_W} ${CHART_H}`} role="img" aria-label="近 30 天模型每日 Token 用量折线图" className="sg-daily-chart-svg">
      {gridRatios.map((r) => {
        const y = PAD.top + PLOT_H * (1 - r);
        return (
          <g key={r}>
            <line x1={PAD.left} y1={y} x2={PAD.left + PLOT_W} y2={y} stroke="var(--sg-border-default)" strokeWidth={r === 0 ? 1.2 : 1} />
            <text x={PAD.left - 8} y={y + 3.5} textAnchor="end" fontSize={10.5} fill="var(--sg-text-secondary)">{fmtCompact(maxTokens * r)}</text>
            <text x={PAD.left + PLOT_W + 8} y={y + 3.5} textAnchor="start" fontSize={10.5} fill="var(--sg-text-secondary)">{Math.round(r * RATIO_MAX)}%</text>
          </g>
        );
      })}
      {xLabels.map((p) => (
        <text key={`x-${p.day}`} x={xAt(points.indexOf(p), points.length)} y={CHART_H - 10} textAnchor="middle" fontSize={10.5} fill="var(--sg-text-secondary)">{p.day.slice(5)}</text>
      ))}

      <UsageLine values={points.map((p) => p.totalTokens)} getX={(i) => xAt(i, points.length)} getY={(v) => yTokens(v, maxTokens)} stroke="var(--sg-action-primary)" />
      <UsageLine values={points.map((p) => p.cachedTokens)} getX={(i) => xAt(i, points.length)} getY={(v) => yTokens(v, maxTokens)} stroke="var(--sg-status-passed)" />
      <UsageLine values={points.map((p) => (p.cacheHitRatio == null ? null : p.cacheHitRatio * 100))} getX={(i) => xAt(i, points.length)} getY={yRatio} stroke="var(--sg-text-secondary)" dashed="4 3" />

      {points.map((p, i) => (
        <g key={`pt-${p.day}`}>
          <circle cx={xAt(i, points.length)} cy={yTokens(p.totalTokens, maxTokens)} r={2.6} fill="var(--sg-action-primary)">
            <title>{pointTitle(p)}</title>
          </circle>
          <circle cx={xAt(i, points.length)} cy={yTokens(p.cachedTokens, maxTokens)} r={2.6} fill="var(--sg-status-passed)">
            <title>{pointTitle(p)}</title>
          </circle>
          {p.cacheHitRatio == null ? null : (
            <circle cx={xAt(i, points.length)} cy={yRatio(p.cacheHitRatio * 100)} r={2} fill="var(--sg-text-secondary)">
              <title>{pointTitle(p)}</title>
            </circle>
          )}
        </g>
      ))}
    </svg>
  );
}

export function DiagnosticsPage() {
  const [usage, setUsage] = useState<ModelUsage | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [systemData, setSystemData] = useState<Record<string, unknown>>({});
  const [checkId, setCheckId] = useState('');
  const [operationId, setOperationId] = useState('');

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setUsage(await rpc<ModelUsage>('model.usage', {}));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '观测数据加载失败');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const hitRatio = usage?.cacheHitRatio == null
    ? '—'
    : `${Math.round(usage.cacheHitRatio * 100)}%`;
  const daily = usage?.daily ?? [];
  const hasDailyVolume = daily.some((p) => p.totalTokens > 0);

  const inspect = async (key: string, method: string, params: Record<string, unknown> = {}) => {
    try {
      const value = await rpc(method, params);
      setSystemData((current) => ({ ...current, [key]: value }));
      setError(null);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '诊断操作失败');
    }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="使用统计"
        description="查看模型请求的缓存使用情况。"
        actions={
          <button className="sg-btn" onClick={() => void load()} disabled={loading}>
            <IconRefresh size={14} />
            {loading ? '刷新中…' : '刷新'}
          </button>
        }
      />

      {error ? (
        <div className="sg-banner sg-banner--error" role="alert">
          加载失败：{error}
        </div>
      ) : null}

      <SettingsSection title="模型 Token 用量" description="统计全部模型调用的总 Token、缓存命中与每日趋势。">
        {loading && !usage ? (
          <div className="sg-skeleton-rows" aria-busy="true">
            <div className="sg-skeleton-row" />
            <div className="sg-skeleton-row" />
          </div>
        ) : usage ? (
          <>
            <div className="sg-token-metrics" aria-label="模型 Token 用量统计">
              <TokenMetric label="总 Token" value={(usage.tokensIn + usage.tokensOut).toLocaleString()} tone="accent" />
              <TokenMetric label="缓存命中 Token" value={usage.cachedTokens.toLocaleString()} tone="success" />
              <TokenMetric label="缓存命中率" value={hitRatio} tone="success" />
            </div>
            <div className="sg-daily-chart">
              <div className="sg-daily-chart-head">
                <span>每日趋势<small>近 30 天（UTC 日聚合）</small></span>
                <div className="sg-daily-chart-legend" aria-hidden>
                  <span><i style={{ background: 'var(--sg-action-primary)' }} />总 Token</span>
                  <span><i style={{ background: 'var(--sg-status-passed)' }} />缓存命中 Token</span>
                  <span><i className="sg-daily-chart-legend-dash" />缓存命中率</span>
                </div>
              </div>
              {hasDailyVolume ? (
                <DailyUsageChart points={daily} />
              ) : (
                <div className="sg-empty" style={{ padding: '24px 20px' }}>
                  近 30 天暂无每日模型调用量。
                </div>
              )}
            </div>
          </>
        ) : (
          <div className="sg-empty" style={{ padding: '28px 20px' }}>
            暂无模型缓存观测数据。
          </div>
        )}
      </SettingsSection>
      <SettingsSection title="诊断与审计" description="读取本地日志、通知、审计和不确定操作状态；导出内容由 Core 去敏。">
        <div className="sg-row" style={{ flexWrap: 'wrap' }}>
          <button className="sg-btn" onClick={() => void inspect('logs', 'logs.list', { limit: 100 })}>查看日志文件</button>
          <button className="sg-btn" onClick={() => void inspect('diagnosticBundle', 'logs.exportDiagnosticBundle')}>导出诊断包</button>
          <button className="sg-btn" onClick={() => void inspect('audit', 'audit.list', { limit: 100 })}>查看审计</button>
          <button className="sg-btn" onClick={() => void inspect('auditExport', 'audit.export', { limit: 100 })}>导出去敏审计</button>
          <button className="sg-btn" onClick={() => void inspect('notifications', 'notification.list')}>查看通知</button>
        </div>
        <div className="sg-row" style={{ marginTop: 10, flexWrap: 'wrap' }}>
          <input className="sg-input" aria-label="诊断检查 ID" value={checkId} onChange={(event) => setCheckId(event.target.value)} placeholder="检查 ID" />
          <button className="sg-btn" disabled={!checkId.trim()} onClick={() => void inspect('diagnosticRun', 'diagnostics.run', { checkId: checkId.trim() })}>运行检查</button>
          <input className="sg-input" aria-label="操作 ID" value={operationId} onChange={(event) => setOperationId(event.target.value)} placeholder="unknown / pending 操作 ID" />
          <button className="sg-btn" disabled={!operationId.trim()} onClick={() => void inspect('operation', 'operation.get', { operationId: operationId.trim() })}>查询操作状态</button>
        </div>
        {Object.keys(systemData).length > 0 ? <pre style={{ maxHeight: 380, overflow: 'auto', whiteSpace: 'pre-wrap' }}>{JSON.stringify(systemData, null, 2)}</pre> : null}
      </SettingsSection>
    </div>
  );
}
