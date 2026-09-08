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
                  <td>谱系孤儿率</td>
                  <td>
                    {pct(metrics.orphanRate.rate)}（孤儿 {metrics.orphanRate.orphanCount}/
                    {metrics.orphanRate.totalNodes} 节点）
                    {metrics.orphanRate.insufficientData ? (
                      <span className="sg-muted"> · 样本不足（&lt;5 节点）</span>
                    ) : null}
                  </td>
                </tr>
                <tr data-testid="metrics-loop">
                  <td>回环率</td>
                  <td>
                    平均返工次数 {num(metrics.loopRate.averageReworkCount)} · 返工任务占比{' '}
                    {pct(metrics.loopRate.reworkWorkitemRate)}
                    <span className="sg-muted">
                      {' '}
                      （{metrics.loopRate.completedReworks} 次返工 /{' '}
                      {metrics.loopRate.reworkedWorkitems} 个任务 /{' '}
                      {metrics.loopRate.workitemsWithReleases} 个放行任务）
                    </span>
                    {metrics.loopRate.insufficientData ? (
                      <span className="sg-muted"> · 样本不足（&lt;5 放行）</span>
                    ) : null}
                  </td>
                </tr>
                <tr data-testid="metrics-adoption">
                  <td>建议采纳率</td>
                  <td>
                    {pct(metrics.aiSuggestionAdoption.rate)}
                    <span className="sg-muted">
                      {' '}
                      （{metrics.aiSuggestionAdoption.accepted}/
                      {metrics.aiSuggestionAdoption.decided}）
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
                {metrics.approvalLayers.layers.map((l) => (
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
                ))}
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
                  <td>unknown / 对账</td>
                  <td>
                    run intent {triage.unknownReconciliation?.runIntents ?? 0} · 工具结果 unknown{' '}
                    {triage.unknownReconciliation?.toolOutcomesUnknown ?? 0} · 对账 pending{' '}
                    {triage.unknownReconciliation?.toolOutcomesReconciliationPending ?? 0}
                  </td>
                </tr>
                <tr data-testid="triage-orphans">
                  <td>对象库无引用</td>
                  <td>
                    {triage.objectOrphans?.count ?? 0} / {triage.objectOrphans?.total ?? 0}
                    {triage.objectOrphans?.pruneGated ? (
                      <span className="sg-muted"> · 清理受 GC PRUNE 门控（只计数）</span>
                    ) : null}
                  </td>
                </tr>
                <tr data-testid="triage-knowledge-blocked">
                  <td>知识 block 级未验证</td>
                  <td>
                    {triage.knowledgeBlocked.length === 0 ? (
                      '无'
                    ) : (
                      triage.knowledgeBlocked
                        .map((k) => `${k.stableId}（${k.state}）`)
                        .join('、')
                    )}
                  </td>
                </tr>
              </tbody>
            </table>
            {triage.items.length > 0 ? (
              <table className="sg-table">
                <thead>
                  <tr>
                    <th>任务</th>
                    <th>谱系缺口（孤儿/未验证/未覆盖）</th>
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
            <div className="sg-empty" style={{ padding: '24px' }}>
              该项目暂无声明验证策略的知识源
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
    </>
  );
}
