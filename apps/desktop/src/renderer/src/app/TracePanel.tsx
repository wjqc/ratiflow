import { useEffect, useState } from 'react';
import { rpc, rpcErrorMessage } from '../rpc/client';
import { relativeTime } from '../lib/format';

/* ---------------- 只读追溯面板（ADR-030 M1 谱系底座） ----------------
 * 需求修订（不可变版本）→ 需求项覆盖 → 断链扫描。
 * 数据全部来自 requirement.* / trace.* 只读 RPC；本面板不提供任何写操作。
 */

interface RequirementDoc {
  id: string;
  source_kind: string;
  source_ref: string;
  title: string;
  created_at: string;
}

interface RequirementRevision {
  id: string;
  revision_no: number;
  content_sha256: string;
  created_by: string;
  created_at: string;
  supersedes_revision_id?: string;
}

interface RevisionGroup {
  document: RequirementDoc;
  revisions: RequirementRevision[];
}

interface CoverageItem {
  requirementKey: string;
  title: string;
  status: string;
  satisfies: number;
  implements: number;
  verifies: number;
  covered: boolean;
  verified: boolean;
}

interface Coverage {
  revisionId: string;
  totalItems: number;
  coveredCount: number;
  verifiedCount: number;
  items: CoverageItem[];
}

interface Gaps {
  orphanCount: number;
  unverifiedCount: number;
  uncoveredItemCount: number;
}

interface TraceSpan {
  id?: string;
  name?: string;
  kind?: string;
  parent_span_id?: string | null;
  status?: string;
}

interface UsageTurn {
  provider: string;
  model: string;
  promptTokens: number;
  completionTokens: number;
  cacheRead: number | null;
  cacheWrite: number | null;
  usageEstimated: boolean;
  costState: string;
}

const KIND_LABELS: Record<string, string> = {
  inline: '文字',
  document: '文档',
  issue: 'Issue',
  legacy_import: '历史导入',
};

export default function TracePanel({ workItemId }: { workItemId: string }) {
  const [groups, setGroups] = useState<RevisionGroup[] | null>(null);
  const [coverage, setCoverage] = useState<Coverage | null>(null);
  const [gaps, setGaps] = useState<Gaps | null>(null);
  const [spans, setSpans] = useState<TraceSpan[]>([]);
  const [usage, setUsage] = useState<UsageTurn[]>([]);
  const [error, setError] = useState('');
  const [restoreState, setRestoreState] = useState('');
  const [lineageNodeId, setLineageNodeId] = useState('');
  const [lineage, setLineage] = useState<unknown>(null);

  useEffect(() => {
    let alive = true;
    setGroups(null);
    setCoverage(null);
    setGaps(null);
    setError('');
    (async () => {
      try {
        const [revs, cov, gapList, graph, usageResult] = await Promise.all([
          rpc<{ items: RevisionGroup[] }>('requirement.revisions', { workItemId }).catch(() => ({ items: [] })),
          rpc<Coverage>('trace.coverage', { workItemId }).catch(() => null),
          rpc<Gaps>('trace.gaps', { workItemId }).catch(() => null),
          rpc<{ spans: TraceSpan[] }>('trace.graph', { workItemId }).catch(() => ({ spans: [] })),
          rpc<{ turns: UsageTurn[] }>('trace.usage', { workItemId }).catch(() => ({ turns: [] })),
        ]);
        if (!alive) return;
        setGroups(revs.items);
        setCoverage(cov);
        setGaps(gapList);
        setSpans(graph.spans ?? []);
        setUsage(usageResult.turns ?? []);
      } catch (e) {
        if (alive) setError(e instanceof Error ? e.message : String(e));
      }
    })();
    return () => {
      alive = false;
    };
  }, [workItemId]);

  const clean = !error && gaps && gaps.orphanCount + gaps.unverifiedCount + gaps.uncoveredItemCount === 0;

  return (
    <div className="sg-card">
      <div className="sg-card-head">
        追溯（需求 → 产物 → 证据）
        <span className="sg-card-extra">
          {clean ? (
            <span className="sg-chip sg-chip--ok">链路完整</span>
          ) : gaps ? (
            <span className="sg-chip">
              断链 {gaps.orphanCount + gaps.uncoveredItemCount} · 未验证 {gaps.unverifiedCount}
            </span>
          ) : null}
          <button
            type="button"
            className="sg-btn sg-btn--sm"
            style={{ marginLeft: 8 }}
            onClick={() => {
              setRestoreState('恢复中…');
              void rpc<{ restored: boolean; source?: string }>('trace.restoreCheckpoint', { workItemId })
                .then((result) => setRestoreState(result.restored ? `已从 ${result.source ?? '事实'} 恢复` : '没有可恢复状态'))
                .catch((cause) => setRestoreState(`恢复失败：${rpcErrorMessage(cause)}`));
            }}
          >
            恢复检查点
          </button>
        </span>
      </div>
      {restoreState ? <div className="sg-muted" role="status" style={{ padding: '8px 14px 0' }}>{restoreState}</div> : null}

      {error && (
        <div className="sg-empty" style={{ padding: '14px 24px' }} role="alert">
          追溯数据加载失败：{error}
        </div>
      )}

      {!error && groups && groups.length === 0 && (
        <div className="sg-empty" style={{ padding: '14px 24px' }}>
          本任务尚无需求修订。需求关产物冻结后会自动生成可追溯版本。
        </div>
      )}

      {!error && groups && groups.length > 0 && (
        <div style={{ padding: '8px 14px 12px', display: 'grid', gap: 10 }}>
          {groups.map(({ document, revisions }) => (
            <div key={document.id} style={{ display: 'grid', gap: 4 }}>
              <div style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 12.5 }}>
                <span className="sg-chip">{KIND_LABELS[document.source_kind] ?? document.source_kind}</span>
                <span style={{ fontWeight: 500 }}>{document.title || document.source_ref}</span>
                <span className="sg-muted">{revisions.length} 个修订</span>
              </div>
              {revisions.map((rev) => (
                <div
                  key={rev.id}
                  style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 12, paddingLeft: 16 }}
                >
                  <span className="sg-muted">v{rev.revision_no}</span>
                  <span className="sg-muted" style={{ fontFamily: 'monospace', fontSize: 11 }}>
                    {rev.content_sha256.slice(0, 12)}
                  </span>
                  <span className="sg-muted">{relativeTime(rev.created_at)}</span>
                  {rev.supersedes_revision_id && <span className="sg-muted">取代上一版</span>}
                </div>
              ))}
            </div>
          ))}

          {coverage && coverage.totalItems > 0 && (
            <div style={{ display: 'grid', gap: 4 }}>
              <div style={{ fontSize: 12.5, fontWeight: 500 }}>
                需求覆盖（最新修订 {coverage.coveredCount}/{coverage.totalItems} 已实现 ·{' '}
                {coverage.verifiedCount}/{coverage.totalItems} 有测试证据）
              </div>
              {coverage.items.map((item) => (
                <div
                  key={item.requirementKey}
                  style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 12, paddingLeft: 16 }}
                >
                  <span className={`sg-chip ${item.verified ? 'sg-chip--ok' : item.covered ? '' : ''}`}>
                    {item.verified ? '已验证' : item.covered ? '已实现' : '未覆盖'}
                  </span>
                  <span style={{ fontFamily: 'monospace', fontSize: 11 }}>{item.requirementKey}</span>
                  <span className="sg-muted" style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                    {item.title}
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
      {!error ? (
        <div style={{ padding: '8px 14px 14px', display: 'grid', gap: 10 }}>
          <div className="sg-row" style={{ flexWrap: 'wrap' }}>
            <button type="button" className="sg-btn sg-btn--sm" onClick={() => {
              void window.ratiflow.selectFile().then((picked) => {
                if (!picked) return;
                const content = new TextDecoder().decode(Uint8Array.from(atob(picked.contentBase64), (char) => char.charCodeAt(0)));
                return rpc('requirement.importRevision', { workItemId, filename: picked.filename, content, sourceKind: 'document', createdBy: 'local-user' });
              }).then(() => window.location.reload()).catch((cause) => setError(rpcErrorMessage(cause)));
            }}>导入需求修订</button>
            <input className="sg-input" aria-label="谱系节点 ID" value={lineageNodeId} onChange={(event) => setLineageNodeId(event.target.value)} placeholder="谱系节点 ID" />
            <button type="button" className="sg-btn sg-btn--sm" disabled={!lineageNodeId.trim()} onClick={() => void rpc('trace.lineage', { nodeId: lineageNodeId.trim(), direction: 'both', depth: 5 }).then(setLineage).catch((cause) => setError(rpcErrorMessage(cause)))}>查看上下游谱系</button>
          </div>
          {lineage ? <pre style={{ overflow: 'auto', whiteSpace: 'pre-wrap' }}>{JSON.stringify(lineage, null, 2)}</pre> : null}
          <div>
            <strong style={{ fontSize: 12.5 }}>执行图</strong>
            <span className="sg-muted" style={{ marginLeft: 8 }}>由 {spans.length} 个持久化 span 重建</span>
            {spans.slice(0, 20).map((span, index) => (
              <div key={span.id ?? index} className="sg-muted" style={{ paddingLeft: span.parent_span_id ? 24 : 8, fontSize: 12 }}>
                {span.kind ?? 'span'} · {span.name ?? span.id ?? '未命名'} · {span.status ?? 'unknown'}
              </div>
            ))}
          </div>
          <div>
            <strong style={{ fontSize: 12.5 }}>模型用量</strong>
            {usage.length === 0 ? <span className="sg-muted" style={{ marginLeft: 8 }}>暂无用量记录</span> : usage.slice(0, 20).map((turn, index) => (
              <div key={`${turn.provider}:${turn.model}:${index}`} className="sg-muted" style={{ paddingLeft: 8, fontSize: 12 }}>
                {turn.provider}/{turn.model} · 输入 {turn.promptTokens} · 输出 {turn.completionTokens} · 缓存读取 {turn.cacheRead ?? 'unknown'} · 成本 {turn.costState || 'unknown'}{turn.usageEstimated ? ' · 估算' : ''}
              </div>
            ))}
          </div>
        </div>
      ) : null}
    </div>
  );
}
