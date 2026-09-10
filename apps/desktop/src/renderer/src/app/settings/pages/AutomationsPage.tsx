import { useCallback, useEffect, useState } from 'react';
import { rpc, rpcErrorMessage } from '../../../rpc/client';
import { SettingsPageHeader } from '../components/SettingsPageHeader';

interface Automation {
  id: string;
  key: string;
  workItemId: string;
  intervalSecs: number;
  nextFireAt: string;
  status: string;
  revision: number;
}

interface AutomationRun {
  id?: string;
  status?: string;
  scheduled_for?: string;
  created_at?: string;
  [key: string]: unknown;
}

interface Observation {
  id: string;
  source: string;
  automation_id?: string;
  suggestion_type: string;
  content: unknown;
  legacy: boolean;
  decision?: { decision: string };
  reviews: Array<{ false_positive: boolean }>;
}

const DECISION_LABEL: Record<string, string> = {
  accepted: '已采纳',
  rejected: '已拒绝',
  ignored: '已忽略',
  expired: '已过期',
};

/** 建议内容摘要：优先取 goal 字段，否则原样 JSON（行内截断交给样式）。 */
function contentText(content: unknown): string {
  if (content && typeof content === 'object') {
    const goal = (content as { goal?: unknown }).goal;
    if (typeof goal === 'string' && goal.trim()) return goal;
  }
  return JSON.stringify(content);
}

function pct(v: unknown): string {
  return `${Math.round((Number(v) || 0) * 100)}%`;
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="sg-token-metric">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

export function AutomationsPage() {
  const [items, setItems] = useState<Automation[]>([]);
  const [key, setKey] = useState('');
  const [workItemId, setWorkItemId] = useState('');
  const [intervalSecs, setIntervalSecs] = useState(3600);
  const [intent, setIntent] = useState('{\n  "goal": "检查任务状态并生成建议"\n}');
  const [history, setHistory] = useState<{ title: string; items: AutomationRun[] } | null>(null);
  const [observations, setObservations] = useState<Observation[]>([]);
  const [stats, setStats] = useState<Record<string, unknown>>({});
  const [busy, setBusy] = useState('');
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');

  const load = useCallback(async () => {
    try {
      const [result, observed] = await Promise.all([
        rpc<{ items: Automation[] }>('automation.list'),
        rpc<{ items: Observation[]; stats: Record<string, unknown> }>('automation.observations', {}).catch(() => ({ items: [], stats: {} })),
      ]);
      setItems(result.items ?? []);
      setObservations(observed.items ?? []);
      setStats(observed.stats ?? {});
      setError('');
    } catch (cause) {
      setError(rpcErrorMessage(cause));
    }
  }, []);

  useEffect(() => { void load(); }, [load]);

  const act = async (id: string, action: () => Promise<unknown>, message: string) => {
    setBusy(id); setError(''); setNotice('');
    try {
      await action();
      setNotice(message);
      await load();
    } catch (cause) {
      setError(rpcErrorMessage(cause));
    } finally {
      setBusy('');
    }
  };

  const create = () => act('create', () => {
    const parsed = JSON.parse(intent) as Record<string, unknown>;
    return rpc('automation.create', {
      key: key.trim(), workItemId: workItemId.trim() || undefined, intent: parsed,
      intervalSecs, misfirePolicy: 'skip', overlapPolicy: 'skip',
    });
  }, '自动化规则已创建');

  return (
    <div className="sg-set-page sg-reference-page">
      <SettingsPageHeader title="自动化" description="定时生成受治理的 Run intent；执行、审批和工具策略仍由 Core 控制。" />
      {error ? <div className="sg-banner sg-banner--error" role="alert">{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}
      <div className="sg-setting-list">
        <div className="sg-set-item">
          <div className="sg-set-item-copy"><label className="sg-set-item-title" htmlFor="automation-key">规则标识</label></div>
          <div className="sg-set-item-control"><input id="automation-key" className="sg-input" value={key} onChange={(e) => setKey(e.target.value)} placeholder="daily-review" /></div>
        </div>
        <div className="sg-set-item">
          <div className="sg-set-item-copy"><label className="sg-set-item-title" htmlFor="automation-workitem">关联任务</label><small className="sg-set-item-desc">可留空创建全局规则。</small></div>
          <div className="sg-set-item-control"><input id="automation-workitem" className="sg-input" value={workItemId} onChange={(e) => setWorkItemId(e.target.value)} placeholder="wi_…" /></div>
        </div>
        <div className="sg-set-item">
          <div className="sg-set-item-copy"><label className="sg-set-item-title" htmlFor="automation-interval">执行间隔（秒）</label></div>
          <div className="sg-set-item-control"><input id="automation-interval" className="sg-input" type="number" min={1} value={intervalSecs} onChange={(e) => setIntervalSecs(Number(e.target.value))} /></div>
        </div>
        <div className="sg-set-item" style={{ gridTemplateColumns: '1fr' }}>
          <label className="sg-set-item-title" htmlFor="automation-intent">Intent JSON</label>
          <textarea id="automation-intent" className="sg-textarea" rows={6} value={intent} onChange={(e) => setIntent(e.target.value)} />
        </div>
        <div className="sg-set-form-actions"><button className="sg-btn sg-btn--primary" disabled={!!busy || !key.trim() || intervalSecs < 1} onClick={() => void create()}>创建规则</button></div>
      </div>

      <div className="sg-setting-list" role="list" aria-label="自动化规则">
        {items.length === 0 ? <div className="sg-empty"><span>暂无自动化规则。</span></div> : items.map((item) => (
          <div className="sg-set-item" role="listitem" key={item.id}>
            <div className="sg-set-item-copy">
              <span className="sg-set-item-title">{item.key} · {item.status}</span>
              <small className="sg-set-item-desc">每 {item.intervalSecs} 秒 · 下次 {item.nextFireAt}{item.workItemId ? ` · ${item.workItemId}` : ''}</small>
            </div>
            <div className="sg-set-item-control" style={{ gap: 8 }}>
              {item.status === 'active' ? <button className="sg-btn sg-btn--sm" disabled={!!busy} onClick={() => void act(item.id, () => rpc('automation.pause', { automationId: item.id, expectedRevision: item.revision }), '规则已暂停')}>暂停</button> : <button className="sg-btn sg-btn--sm" disabled={!!busy} onClick={() => void act(item.id, () => rpc('automation.resume', { automationId: item.id, expectedRevision: item.revision }), '规则已恢复')}>恢复</button>}
              <button className="sg-btn sg-btn--sm sg-btn--primary" disabled={!!busy} onClick={() => void act(`${item.id}:run`, () => rpc('automation.runNow', { automationId: item.id, scheduledFor: new Date().toISOString() }), '已请求立即执行')}>立即执行</button>
              <button className="sg-btn sg-btn--sm" disabled={!!busy} onClick={() => void act(`${item.id}:shadow`, () => rpc('automation.setShadowMode', { automationId: item.id, shadowMode: true, expectedRevision: item.revision, idempotencyKey: `ui-automation-shadow-${crypto.randomUUID()}` }), '规则已切换到影子模式')}>影子模式</button>
              <button className="sg-btn sg-btn--sm" disabled={!!busy} onClick={() => void act(`${item.id}:live`, () => rpc('automation.setShadowMode', { automationId: item.id, shadowMode: false, expectedRevision: item.revision, idempotencyKey: `ui-automation-live-${crypto.randomUUID()}` }), '规则已通过观察门槛并切换到实时模式')}>实时模式</button>
              <button className="sg-btn sg-btn--sm" disabled={!!busy} onClick={() => void rpc<{ items: AutomationRun[] }>('automation.history', { automationId: item.id }).then((result) => setHistory({ title: item.key, items: result.items ?? [] })).catch((cause) => setError(rpcErrorMessage(cause)))}>历史</button>
            </div>
          </div>
        ))}
      </div>
      {history ? (
        <div className="sg-card" style={{ marginTop: 12 }}>
          <div className="sg-card-head">
            {history.title} 执行历史
            <span className="sg-card-extra"><button className="sg-btn sg-btn--sm" onClick={() => setHistory(null)}>关闭</button></span>
          </div>
          <div className="sg-auto-runs" role="list" aria-label="执行历史">
            {history.items.length === 0 ? (
              <div className="sg-inline-note">暂无执行记录。</div>
            ) : history.items.map((run, index) => (
              <div className="sg-auto-run" role="listitem" key={run.id ?? index}>
                <span className="sg-auto-run-status">{run.status || '未知'}</span>
                <small className="sg-auto-run-time">计划 {run.scheduled_for || '—'} · 创建 {run.created_at || '—'}</small>
              </div>
            ))}
          </div>
        </div>
      ) : null}
      <div className="sg-card" style={{ marginTop: 12 }}>
        <div className="sg-card-head">
          影子建议与观测
          <span className="sg-card-extra sg-muted">影子模式规则的试运行产出与质量观测</span>
        </div>
        <div className="sg-auto-metrics" aria-label="影子建议观测统计">
          <Metric label="累计建议" value={Number(stats.total ?? 0).toLocaleString()} />
          <Metric label="已裁决" value={Number(stats.decided ?? 0).toLocaleString()} />
          <Metric label="已复核" value={Number(stats.reviewed ?? 0).toLocaleString()} />
          <Metric label="误报" value={Number(stats.false_positives ?? 0).toLocaleString()} />
          <Metric label="误报率" value={pct(stats.false_positive_rate)} />
          <Metric label="复核覆盖" value={pct(stats.review_coverage)} />
        </div>
        {observations.length === 0 ? (
          <div className="sg-inline-note">暂无影子建议。</div>
        ) : observations.map((item) => {
          const review = item.reviews[0];
          const reviewLabel = review ? (review.false_positive ? '复核：误报' : '复核：有效') : '';
          const decisionLabel = item.decision ? (DECISION_LABEL[item.decision.decision] ?? item.decision.decision) : '待裁决';
          return (
            <div className="sg-set-item" key={item.id}>
              <div className="sg-set-item-copy">
                <span className="sg-set-item-title">
                  {item.suggestion_type} · {decisionLabel}
                  {reviewLabel ? ` · ${reviewLabel}` : ''}
                  {item.legacy ? ' · legacy 只读' : ''}
                </span>
                <small className="sg-set-item-desc">{contentText(item.content)}</small>
              </div>
              <div className="sg-set-item-control" style={{ gap: 8 }}>
                {!item.decision && !item.legacy ? <>
                  <button className="sg-btn sg-btn--sm sg-btn--primary" disabled={!!busy} onClick={() => void act(`suggestion:${item.id}:accept`, () => rpc('automation.decideSuggestion', { suggestionId: item.id, decision: 'accepted', decidedBy: 'local-user', note: 'settings-ui accepted', idempotencyKey: `ui-suggestion-${crypto.randomUUID()}` }), '建议已采纳；涉及豁免时仍须单独应用')}>采纳</button>
                  <button className="sg-btn sg-btn--sm" disabled={!!busy} onClick={() => void act(`suggestion:${item.id}:reject`, () => rpc('automation.decideSuggestion', { suggestionId: item.id, decision: 'rejected', decidedBy: 'local-user', note: 'settings-ui rejected', idempotencyKey: `ui-suggestion-${crypto.randomUUID()}` }), '建议已拒绝')}>拒绝</button>
                </> : null}
                {item.decision && item.reviews.length === 0 && !item.legacy ? <>
                  <button className="sg-btn sg-btn--sm" disabled={!!busy} onClick={() => void act(`review:${item.id}:ok`, () => rpc('automation.reviewSuggestion', { suggestionId: item.id, falsePositive: false, reviewer: 'local-user', note: 'settings-ui review', idempotencyKey: `ui-review-${crypto.randomUUID()}` }), '建议已复核为有效')}>有效建议</button>
                  <button className="sg-btn sg-btn--sm" disabled={!!busy} onClick={() => void act(`review:${item.id}:fp`, () => rpc('automation.reviewSuggestion', { suggestionId: item.id, falsePositive: true, reviewer: 'local-user', note: 'settings-ui false positive', idempotencyKey: `ui-review-${crypto.randomUUID()}` }), '建议已标记为误报')}>标记误报</button>
                </> : null}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
