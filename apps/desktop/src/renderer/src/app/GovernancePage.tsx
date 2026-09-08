import { useCallback, useEffect, useState } from 'react';
import { rpc, rpcErrorMessage } from '../rpc/client';

// P1-5（RDWS 审计 §8）：治理总览页——metrics 唯一口径（WP-10 A5）+ Triage 聚合
// （WP-11/P1-3）+ knowledge freshness（WP-13 B10）。
// 治理边界：纯读展示，无任何状态推进入口；unknown/blocked/insufficient 如实呈现，
// 不虚构数值（服务端 null → 显示「样本不足/未知」而非 0）。

interface MetricsOverview {
  windowDays: number;
  orphanRate: {
    totalNodes: number;
    orphanCount: number;
    rate: number | null;
    insufficientData: boolean;
  };
  loopRate: {
    completedReworks: number;
    reworkedWorkitems: number;
    workitemsWithReleases: number;
    averageReworkCount: number | null;
    reworkWorkitemRate: number | null;
    insufficientData: boolean;
  };
  approvalLayers: {
    dimensions: string[];
    layers: Array<{
      subjectType: string;
      risk: string;
      approved: number;
      rejected: number;
      pending: number;
      expired: number;
      changesRequested: number;
      passRate: number | null;
      sample: number;
      insufficientData: boolean;
      rubberStampSuspect: boolean;
    }>;
  };
  aiSuggestionAdoption: {
    decided: number;
    accepted: number;
    rate: number | null;
    insufficientData: boolean;
  };
}

interface TriageList {
  items: Array<{ workitemId: string; orphanCount: number; unverifiedCount: number; uncoveredCount: number }>;
  knowledgeBlocked: Array<{ projectId: string; stableId: string; state: string }>;
  objectOrphans?: { count: number; total: number; pruneGated: boolean };
  unknownReconciliation?: {
    runIntents: number;
    toolOutcomesUnknown: number;
    toolOutcomesReconciliationPending: number;
  };
}

interface FreshnessItem {
  stableId: string;
  state: string;
  nextDueAt: string | null;
  inputRevisionMode: string;
  contentOwner?: string | null;
}

const FRESHNESS_LABELS: Record<string, string> = {
  verified: '已验证',
  unverified: '未验证',
  unverified_content_changed: '内容已变更（待重验）',
  failed: '验证失败',
  unknown: '验证结果未知',
  expired: '已过期',
};

function pct(v: number | null | undefined): string {
  if (v === null || v === undefined) return '样本不足';
  return `${(v * 100).toFixed(1)}%`;
}

function num(v: number | null | undefined): string {
  if (v === null || v === undefined) return '未知';
  return String(v);
}

/** 键值行标签列：固定宽度 + 次级色，保证数值列起始对齐（页内约定，不动全局样式）。 */
const LABEL_TD: React.CSSProperties = { width: 132, color: 'var(--sg-text-secondary)' };
/** 值内明细行：块级小字，主值与样本明细分行不再挤一行。 */
const DETAIL: React.CSSProperties = {
  display: 'block',
  fontSize: 12,
  color: 'var(--sg-text-secondary)',
  marginTop: 2,
};

export default function GovernancePage() {
  const [metrics, setMetrics] = useState<MetricsOverview | null>(null);
  const [triage, setTriage] = useState<TriageList | null>(null);
  const [projects, setProjects] = useState<Array<{ id: string; name: string }>>([]);
  const [projectId, setProjectId] = useState('');
  const [freshness, setFreshness] = useState<Array<FreshnessItem>>([]);
  const [error, setError] = useState('');

  useEffect(() => {
    void rpc<{ items: Array<{ id: string; name: string }> }>('project.list')
      .then((r) => {
        setProjects(r.items ?? []);
        setProjectId((prev) => prev || r.items?.[0]?.id || '');
      })
      .catch((e) => setError(rpcErrorMessage(e)));
  }, []);

  const reload = useCallback(async () => {
    try {
      const [ov, tri] = await Promise.all([
        rpc<MetricsOverview>('metrics.overview', { scope: 'global' }),
        rpc<TriageList>('triage.list'),
      ]);
      setMetrics(ov);
      setTriage(tri);
      setError('');
    } catch (e) {
      setError(rpcErrorMessage(e));
    }
  }, []);

  useEffect(() => {
    void reload();
    const t = setInterval(() => void reload(), 30000);
    return () => clearInterval(t);
  }, [reload]);

  // 知识新鲜度按项目读取（服务端解析 revision，客户端不参与计算）。
  useEffect(() => {
    if (!projectId) return;
    void rpc<{ items: FreshnessItem[] }>('knowledge.freshnessOverview', { projectId })
      .then((r) => setFreshness(r.items ?? []))
      .catch((e) => setError(rpcErrorMessage(e)));
  }, [projectId]);

  return (
    <>
      <header className="sg-page-head">
        <span className="sg-page-head-title">治理总览</span>
        <span className="sg-page-head-status">纯读投影 · 服务器权威</span>
      </header>
      <div className="sg-scroll">
        <div style={{ display: 'flex', flexDirection: 'column', gap: 12, paddingBottom: 24 }}>
        {error ? (
          <div className="sg-banner sg-banner--error" role="alert">
            {error}
          </div>
        ) : null}

        {metrics ? (
          <div className="sg-card" data-testid="metrics-card">
            <div className="sg-card-head">
              流程指标（{metrics.windowDays} 天窗口）
              <span className="sg-card-extra sg-muted">
                回环双口径：平均返工次数 × 发生返工任务占比
              </span>
            </div>
            <table className="sg-table">
              <tbody>
                <tr data-testid="metrics-orphan">
                  <td style={LABEL_TD}>谱系孤儿率</td>
                  <td>
                    {pct(metrics.orphanRate.rate)}
                    <span style={DETAIL}>
                      孤儿 {metrics.orphanRate.orphanCount}/{metrics.orphanRate.totalNodes} 节点
                      {metrics.orphanRate.insufficientData ? ' · 样本不足（<5 节点）' : ''}
                    </span>
                  </td>
                </tr>
                <tr data-testid="metrics-loop">
                  <td style={LABEL_TD}>回环率</td>
                  <td>
                    平均返工次数 {num(metrics.loopRate.averageReworkCount)} · 返工任务占比{' '}
                    {pct(metrics.loopRate.reworkWorkitemRate)}
                    <span style={DETAIL}>
                      {metrics.loopRate.completedReworks} 次返工 · {metrics.loopRate.reworkedWorkitems}{' '}
                      个任务发生返工 · {metrics.loopRate.workitemsWithReleases} 个放行任务
                      {metrics.loopRate.insufficientData ? ' · 样本不足（<5 放行）' : ''}
                    </span>
                  </td>
                </tr>
                <tr data-testid="metrics-adoption">
                  <td style={LABEL_TD}>建议采纳率</td>
                  <td>
                    {pct(metrics.aiSuggestionAdoption.rate)}
                    <span style={DETAIL}>
                      采纳 {metrics.aiSuggestionAdoption.accepted}/{metrics.aiSuggestionAdoption.decided}
                    </span>
                  </td>
                </tr>
              </tbody>
            </table>
            <div className="sg-card-head" style={{ borderTop: '1px solid var(--sg-border)' }}>
              审批分层（subjectType × risk）
            </div>
            <table className="sg-table">
              <thead>
                <tr>
                  <th>主体</th>
                  <th>风险</th>
                  <th>通过</th>
                  <th>拒绝</th>
                  <th>待处理</th>
                  <th>过期</th>
                  <th>要求修改</th>
                  <th>通过率</th>
                </tr>
              </thead>
              <tbody>
                {metrics.approvalLayers.layers.length === 0 ? (
                  <tr>
                    <td
                      colSpan={8}
                      style={{ textAlign: 'center', color: 'var(--sg-text-secondary)', padding: '14px 12px' }}
                    >
                      暂无审批数据（窗口期内无审批记录）
                    </td>
                  </tr>
                ) : (
                  metrics.approvalLayers.layers.map((l) => (
                    <tr key={`${l.subjectType}:${l.risk}`}>
                      <td>{l.subjectType}</td>
                      <td>{l.risk}</td>
                      <td>{l.approved}</td>
                      <td>{l.rejected}</td>
                      <td>{l.pending}</td>
                      <td>{l.expired}</td>
                      <td>{l.changesRequested}</td>
                      <td data-testid={`approval-pass-rate-${l.subjectType}-${l.risk}`}>
                        {pct(l.passRate)}
                        {l.rubberStampSuspect ? (
                          <span className="sg-muted"> · 疑橡皮章</span>
                        ) : null}
                      </td>
                    </tr>
                  ))
                )}
              </tbody>
            </table>
          </div>
        ) : null}

        {triage ? (
          <div className="sg-card" data-testid="triage-card">
            <div className="sg-card-head">
              Triage 聚合
              <span className="sg-card-extra sg-muted">只提示不自动处置</span>
            </div>
            <table className="sg-table">
              <tbody>
                <tr data-testid="triage-unknown">
                  <td style={LABEL_TD}>unknown / 对账</td>
                  <td>
                    <span style={DETAIL}>run intent {triage.unknownReconciliation?.runIntents ?? 0}</span>
                    <span style={DETAIL}>工具结果 unknown {triage.unknownReconciliation?.toolOutcomesUnknown ?? 0}</span>
                    <span style={DETAIL}>对账 pending {triage.unknownReconciliation?.toolOutcomesReconciliationPending ?? 0}</span>
                  </td>
                </tr>
                <tr data-testid="triage-orphans">
                  <td style={LABEL_TD}>对象库无引用</td>
                  <td>
                    {triage.objectOrphans?.count ?? 0} / {triage.objectOrphans?.total ?? 0}
                    {triage.objectOrphans?.pruneGated ? (
                      <span style={DETAIL}>清理受 GC PRUNE 门控（只计数）</span>
                    ) : null}
                  </td>
                </tr>
                <tr data-testid="triage-knowledge-blocked">
                  <td style={LABEL_TD}>知识 block 级未验证</td>
                  <td>
                    {triage.knowledgeBlocked.length === 0 ? (
                      '无'
                    ) : (
                      triage.knowledgeBlocked.map((k, i) => (
                        <span key={`${k.projectId}:${k.stableId}`} style={DETAIL}>
                          {k.stableId}（{k.state}）
                        </span>
                      ))
                    )}
                  </td>
                </tr>
              </tbody>
            </table>
            {triage.items.length > 0 ? (
              <table className="sg-table" style={{ marginTop: -1 }}>
                <thead>
                  <tr>
                    <th style={{ width: '55%' }}>任务</th>
                    <th>谱系缺口（孤儿 / 未验证 / 未覆盖）</th>
                  </tr>
                </thead>
                <tbody>
                  {triage.items.slice(0, 10).map((it) => (
                    <tr key={it.workitemId}>
                      <td className="sg-muted">{it.workitemId}</td>
                      <td>
                        {it.orphanCount} / {it.unverifiedCount} / {it.uncoveredCount}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            ) : null}
          </div>
        ) : null}

        <div className="sg-card" data-testid="freshness-card">
          <div className="sg-card-head">
            知识新鲜度
            <span className="sg-card-extra">
              {projects.length > 0 ? (
                <select
                  className="sg-input"
                  style={{ width: 200 }}
                  value={projectId}
                  onChange={(e) => setProjectId(e.target.value)}
                  aria-label="选择项目"
                >
                  {projects.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.name}
                    </option>
                  ))}
                </select>
              ) : null}
            </span>
          </div>
          {freshness.length === 0 ? (
            <div style={{ padding: '14px 16px', color: 'var(--sg-text-secondary)', fontSize: 13 }}>
              该项目暂无声明验证策略的知识源（在知识库页为知识源配置验证策略后，这里显示验证状态与到期）
            </div>
          ) : (
            <table className="sg-table">
              <thead>
                <tr>
                  <th>知识源</th>
                  <th>状态</th>
                  <th>版本锚点</th>
                  <th>下次到期</th>
                </tr>
              </thead>
              <tbody>
                {freshness.map((f) => (
                  <tr key={f.stableId}>
                    <td>{f.stableId}</td>
                    <td data-testid={`freshness-state-${f.stableId}`}>
                      {FRESHNESS_LABELS[f.state] ?? f.state}
                    </td>
                    <td className="sg-muted">{f.inputRevisionMode}</td>
                    <td className="sg-muted">{f.nextDueAt ?? '—'}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
        </div>
      </div>
    </>
  );
}
