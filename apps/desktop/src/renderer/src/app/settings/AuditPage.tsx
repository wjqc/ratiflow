// S42 审计日志：audit.list 真实（分页/筛选/详情检查器）；detailJson 渲染前统一脱敏（§15）。
import { useCallback, useEffect, useRef, useState } from 'react';
import { rpc } from '../../rpc/client';
import { formatDateTime, relativeTime } from '../../lib/format';
import { redactedJson } from '../../lib/redact';
import type { AuditEvent, AuditListResult } from './types';
import { SettingsPageHeader } from './components/SettingsPageHeader';
import { SettingsSection } from './components/SettingsSection';
import { IconRefresh, IconX } from '../../components/Icons';

const PAGE_SIZE = 50;

function parseDetail(json: string): unknown {
  try {
    return JSON.parse(json);
  } catch {
    return json;
  }
}

export function AuditPage() {
  const [items, setItems] = useState<AuditEvent[]>([]);
  const [keyword, setKeyword] = useState('');
  const [actionFilter, setActionFilter] = useState('');
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [hasMore, setHasMore] = useState(false);
  const [selected, setSelected] = useState<AuditEvent | null>(null);
  const lastFocusRef = useRef<HTMLElement | null>(null);
  const closeRef = useRef<HTMLButtonElement | null>(null);

  const load = useCallback(async (afterSeq?: number) => {
    setLoading(true);
    setError(null);
    try {
      const res = await rpc<AuditListResult>('audit.list', {
        limit: PAGE_SIZE,
        ...(afterSeq !== undefined ? { afterSeq } : {}),
      });
      const batch = res.items ?? [];
      setItems((prev) => (afterSeq === undefined ? batch : [...prev, ...batch]));
      setHasMore(batch.length === PAGE_SIZE);
    } catch (e) {
      setError(e instanceof Error ? e.message : '审计日志加载失败');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const openDetail = (e: AuditEvent, trigger: HTMLElement) => {
    lastFocusRef.current = trigger;
    setSelected(e);
  };

  const closeDetail = useCallback(() => {
    setSelected(null);
    lastFocusRef.current?.focus();
  }, []);

  useEffect(() => {
    if (!selected) return;
    closeRef.current?.focus();
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === 'Escape') closeDetail();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [selected, closeDetail]);

  const filtered = items.filter((e) => {
    if (actionFilter && !e.action.includes(actionFilter)) return false;
    if (!keyword) return true;
    const k = keyword.toLowerCase();
    return (
      e.actor.toLowerCase().includes(k) ||
      e.action.toLowerCase().includes(k) ||
      (e.targetId ?? '').toLowerCase().includes(k) ||
      e.targetType.toLowerCase().includes(k)
    );
  });

  const actionTypes = Array.from(new Set(items.map((e) => e.action.split('.')[0]))).sort();

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="审计日志"
        scope="本地"
        description="本地 append-only 事件链（每条含 prevHash）；秘密键已脱敏，导出契约待开放。"
        actions={
          <button className="sg-btn" onClick={() => void load()} disabled={loading}>
            <IconRefresh size={14} />
            刷新
          </button>
        }
      />

      {error ? <div className="sg-banner sg-banner--error" role="alert">加载失败：{error}</div> : null}

      <SettingsSection
        title="事件列表"
        actions={
          <div className="sg-row" style={{ gap: 8 }}>
            <input
              className="sg-input"
              style={{ height: 28, fontSize: 12, width: 180 }}
              placeholder="筛选 actor / 目标 / 动作"
              value={keyword}
              onChange={(e) => setKeyword(e.target.value)}
              aria-label="关键字筛选"
            />
            <select
              className="sg-select"
              style={{ height: 28, fontSize: 12 }}
              value={actionFilter}
              onChange={(e) => setActionFilter(e.target.value)}
              aria-label="按动作域筛选"
            >
              <option value="">全部动作域</option>
              {actionTypes.map((t) => (
                <option key={t} value={t}>
                  {t}.*
                </option>
              ))}
            </select>
          </div>
        }
      >
        {loading && items.length === 0 ? (
          <div className="sg-skeleton-rows" aria-busy="true">
            <div className="sg-skeleton-row" />
            <div className="sg-skeleton-row" />
            <div className="sg-skeleton-row" />
          </div>
        ) : filtered.length === 0 ? (
          <div className="sg-empty">
            <span>{items.length === 0 ? '暂无审计事件' : '无匹配结果'}</span>
            <span className="sg-hint">{items.length === 0 ? '执行项目/备份等操作后将在此留痕。' : '调整筛选条件后重试。'}</span>
          </div>
        ) : (
          <>
            <table className="sg-table">
              <thead>
                <tr>
                  <th style={{ width: 130 }}>时间</th>
                  <th style={{ width: 110 }}>Actor</th>
                  <th>动作</th>
                  <th style={{ width: 110 }}>目标类型</th>
                  <th>目标</th>
                </tr>
              </thead>
              <tbody>
                {filtered.map((e) => (
                  <tr key={e.seq} className={selected?.seq === e.seq ? 'is-selected' : ''}>
                    <td className="sg-muted" title={formatDateTime(e.time)}>
                      {relativeTime(e.time)}
                    </td>
                    <td>{e.actor}</td>
                    <td>
                      <button
                        className="sg-link-btn"
                        onClick={(ev) => openDetail(e, ev.currentTarget)}
                        aria-haspopup="dialog"
                      >
                        <code className="sg-code">{e.action}</code>
                      </button>
                    </td>
                    <td className="sg-muted">{e.targetType}</td>
                    <td className="sg-muted">{e.targetId ?? '—'}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            <div className="sg-row" style={{ marginTop: 10 }}>
              {hasMore ? (
                <button
                  className="sg-btn sg-btn--sm"
                  disabled={loading}
                  onClick={() => void load(filtered[filtered.length - 1]?.seq)}
                >
                  {loading ? '加载中…' : '加载更多'}
                </button>
              ) : (
                <span className="sg-hint">共 {items.length} 条（已加载全部）</span>
              )}
              <button className="sg-btn sg-btn--sm" disabled title="audit.export 契约待 ZCode 提交">
                导出
              </button>
            </div>
          </>
        )}
      </SettingsSection>

      {selected ? (
        <>
          <div className="sg-drawer-backdrop" onClick={closeDetail} />
          <aside className="sg-drawer" role="dialog" aria-modal="true" aria-label={`审计事件 #${selected.seq} 详情`}>
            <div className="sg-drawer-head">
              <span style={{ fontWeight: 600 }}>事件 #{selected.seq}</span>
              <button ref={closeRef} className="sg-icon-btn" onClick={closeDetail} aria-label="关闭详情">
                <IconX size={14} />
              </button>
            </div>
            <div className="sg-drawer-body">
              <div className="sg-kv">
                <span className="sg-kv-k">时间</span>
                <span>{formatDateTime(selected.time)}</span>
                <span className="sg-kv-k">actor</span>
                <span>{selected.actor}</span>
                <span className="sg-kv-k">动作</span>
                <span><code className="sg-code">{selected.action}</code></span>
                <span className="sg-kv-k">目标</span>
                <span>
                  {selected.targetType}
                  {selected.targetId ? ` / ${selected.targetId}` : ''}
                </span>
              </div>
              <h3 className="sg-set-pending-title" style={{ marginTop: 16 }}>detailJson（已脱敏）</h3>
              <pre className="sg-json-view">{redactedJson(parseDetail(selected.detailJson))}</pre>
            </div>
          </aside>
        </>
      ) : null}
    </div>
  );
}
