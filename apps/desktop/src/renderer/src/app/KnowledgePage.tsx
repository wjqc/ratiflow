import { useCallback, useEffect, useState } from 'react';
import { rpc, rpcErrorMessage } from '../rpc/client';
import { IconBook, IconDoc, IconFolder, IconIssue, IconPlus, IconSearch, IconX } from '../components/Icons';

interface SourceInfo {
  id: string; kind: string; name: string; locator: string; enabled: boolean;
  scan_state: string; last_scanned_at: string | null; error: string;
}

interface Props { projectId: string; projectName?: string }

const KIND_TAGS: Record<string, string> = {
  repo_path: '仓库',
  document: '文档',
  openapi: 'OpenAPI',
  rule: '规则',
};

// 项目知识库（规范 §4.3）：来源管理、扫描状态、检索试用与隔离提示。
// 布局对齐原型 03：工具条 + 来源表 + 右侧详情抽屉 + 本次上下文卡。
export default function KnowledgePage({ projectId, projectName }: Props) {
  const [sources, setSources] = useState<SourceInfo[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [showAdd, setShowAdd] = useState(false);
  const [kind, setKind] = useState('repo_path');
  const [name, setName] = useState('');
  const [locator, setLocator] = useState('');
  const [filter, setFilter] = useState('');
  const [query, setQuery] = useState('');
  const [hits, setHits] = useState<Array<{ chunkId: string; sourceId: string; snippet: string }>>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  const reload = useCallback(async () => {
    try {
      const page = await rpc<{ items: SourceInfo[] }>('knowledge.list', { projectId });
      setSources(page.items);
    } catch (reason) {
      setError(rpcErrorMessage(reason));
    }
  }, [projectId]);

  useEffect(() => { void reload(); }, [reload]);

  const selected = sources.find((s) => s.id === selectedId) ?? null;
  const enabledSources = sources.filter((s) => s.enabled);
  const filtered = filter.trim()
    ? sources.filter((s) => `${s.name} ${s.locator} ${s.kind}`.toLowerCase().includes(filter.trim().toLowerCase()))
    : sources;

  const addSource = () => {
    setBusy(true);
    void rpc('knowledge.create', { projectId, kind, name: name.trim(), locator: locator.trim() })
      .then(() => {
        setName('');
        setLocator('');
        setShowAdd(false);
        return reload();
      })
      .catch((reason) => setError(rpcErrorMessage(reason)))
      .finally(() => setBusy(false));
  };

  const scan = (sourceId: string) => {
    setBusy(true);
    void rpc('knowledge.scan', { sourceId })
      .then(reload)
      .catch((reason) => setError(rpcErrorMessage(reason)))
      .finally(() => setBusy(false));
  };

  const toggle = (source: SourceInfo) => {
    void rpc('knowledge.update', { sourceId: source.id, enabled: !source.enabled }).then(reload);
  };

  const remove = (sourceId: string) => {
    void rpc('knowledge.remove', { sourceId }).then(() => {
      setSelectedId((cur) => (cur === sourceId ? null : cur));
      return reload();
    });
  };

  const rescanAll = () => {
    setBusy(true);
    void Promise.all(sources.map((s) => rpc('knowledge.scan', { sourceId: s.id })))
      .then(reload)
      .catch((reason) => setError(rpcErrorMessage(reason)))
      .finally(() => setBusy(false));
  };

  const runSearch = () => {
    if (!query.trim()) return;
    void rpc<{ items: typeof hits }>('knowledge.search', { projectId, query: query.trim() })
      .then((result) => setHits(result.items))
      .catch((reason) => setError(rpcErrorMessage(reason)));
  };

  return (
    <>
      <header className="sg-page-head">
        <span className="sg-page-head-title">{projectName ? `${projectName} · 项目知识库` : '项目知识库'}</span>
        <span className="sg-page-head-status">本地运行</span>
      </header>

      <div className="sg-toolbar">
        <IconSearch size={13} style={{ color: 'var(--sg-text-secondary)', flexShrink: 0 }} />
        <input
          className="sg-input"
          placeholder="搜索来源 / 类型 / 位置…"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          aria-label="过滤来源"
        />
        <div className="sg-toolbar-actions">
          <button
            className="sg-btn sg-btn--sm"
            disabled={busy || sources.length === 0}
            onClick={rescanAll}
            title="对全部来源重新扫描"
          >
            重建索引
          </button>
          <button className="sg-btn sg-btn--primary sg-btn--sm" onClick={() => setShowAdd((v) => !v)}>
            <IconPlus size={13} />
            添加来源
          </button>
        </div>
      </div>

      <div className="sg-kb-layout">
        <div className="sg-kb-main">
          {error ? (
            <div className="sg-banner sg-banner--error" style={{ margin: '12px 16px 0' }} role="alert">
              {error}
            </div>
          ) : null}

          <div className="sg-banner sg-banner--info" style={{ margin: '12px 16px 0' }}>
            <IconBook size={14} style={{ flexShrink: 0, marginTop: 2 }} />
            <span>
              项目边界：{projectName || '本项目'} · 不会检索其他项目。来源只在当前项目内检索，任务仅使用已确认的上下文。
            </span>
          </div>

          {showAdd && (
            <div className="sg-card" style={{ margin: '12px 16px 0' }}>
              <div className="sg-card-head">
                添加知识来源
                <span className="sg-card-extra">
                  <button className="sg-icon-btn" aria-label="关闭" onClick={() => setShowAdd(false)}>
                    <IconX size={13} />
                  </button>
                </span>
              </div>
              <div style={{ padding: '12px 14px', display: 'flex', gap: 10, flexWrap: 'wrap', alignItems: 'flex-end' }}>
                <label className="sg-field" style={{ width: 130, marginBottom: 0 }}>
                  <span>类型</span>
                  <select className="sg-input" value={kind} onChange={(e) => setKind(e.target.value)}>
                    <option value="repo_path">仓库目录</option>
                    <option value="document">文档</option>
                    <option value="openapi">OpenAPI</option>
                    <option value="rule">规则</option>
                  </select>
                </label>
                <label className="sg-field" style={{ width: 200, marginBottom: 0 }}>
                  <span>名称 *</span>
                  <input className="sg-input" value={name} onChange={(e) => setName(e.target.value)} placeholder="主仓库" />
                </label>
                <label className="sg-field" style={{ flex: 1, minWidth: 220, marginBottom: 0 }}>
                  <span>位置（绝对路径）*</span>
                  <input className="sg-input" value={locator} onChange={(e) => setLocator(e.target.value)} placeholder="/Users/you/project" />
                </label>
                <button
                  className="sg-btn sg-btn--primary"
                  disabled={busy || !name.trim() || !locator.trim()}
                  onClick={addSource}
                >
                  添加
                </button>
              </div>
            </div>
          )}

          <div className="sg-card" style={{ margin: '12px 16px 0' }}>
            <table className="sg-table">
              <thead>
                <tr><th>来源</th><th>类型</th><th>位置</th><th>索引状态</th><th>最后扫描</th><th /></tr>
              </thead>
              <tbody>
                {filtered.length === 0 ? (
                  <tr>
                    <td colSpan={6}>
                      <div className="sg-empty" style={{ padding: '28px 24px' }}>
                        {sources.length === 0 ? '尚无来源。点击右上角「添加来源」接入仓库、文档或规则。' : '没有匹配的来源。'}
                      </div>
                    </td>
                  </tr>
                ) : filtered.map((source) => (
                  <tr
                    key={source.id}
                    className={selectedId === source.id ? 'is-selected' : ''}
                    onClick={() => setSelectedId(source.id)}
                    style={{ cursor: 'pointer' }}
                  >
                    <td>
                      <span style={{ display: 'inline-flex', alignItems: 'center', gap: 8, fontWeight: 500 }}>
                        {kindIcon(source.kind)}
                        {source.name}
                      </span>
                    </td>
                    <td><span className="sg-chip">{KIND_TAGS[source.kind] ?? source.kind}</span></td>
                    <td><span className="sg-muted sg-code" style={{ fontSize: 11 }}>{source.locator}</span></td>
                    <td>
                      <span className={`sg-status ${statusClass(source)}`}>
                        {statusLabel(source)}
                      </span>
                    </td>
                    <td className="sg-muted">
                      {source.last_scanned_at ? new Date(source.last_scanned_at).toLocaleString('zh-CN') : '—'}
                    </td>
                    <td onClick={(e) => e.stopPropagation()}>
                      <button className="sg-btn sg-btn--sm" disabled={busy} onClick={() => scan(source.id)}>
                        扫描
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>

          <div className="sg-card" style={{ margin: '12px 16px 0' }}>
            <div className="sg-card-head">
              本次任务已选来源
              <span className="sg-card-extra">{enabledSources.length} 个启用</span>
            </div>
            {enabledSources.length === 0 ? (
              <div className="sg-empty" style={{ padding: '20px 24px' }}>
                暂无启用的来源；任务上下文将只包含需求描述。
              </div>
            ) : (
              <div style={{ padding: '10px 14px 12px', display: 'flex', flexWrap: 'wrap', gap: 6 }}>
                {enabledSources.map((s) => (
                  <span className="sg-composer-chip" key={s.id}>
                    {kindIcon(s.kind)}
                    {s.name}
                    <span className="sg-muted">{KIND_TAGS[s.kind] ?? s.kind}</span>
                    <button
                      className="sg-icon-btn"
                      style={{ width: 16, height: 16 }}
                      aria-label={`停用 ${s.name}`}
                      title="从本次上下文移除（停用来源）"
                      onClick={() => toggle(s)}
                    >
                      <IconX size={10} />
                    </button>
                  </span>
                ))}
              </div>
            )}
          </div>

          <div className="sg-card" style={{ margin: '12px 16px 24px' }}>
            <div className="sg-card-head">检索试用</div>
            <div style={{ padding: '12px 14px', display: 'grid', gap: 10 }}>
              <div className="sg-row">
                <input
                  className="sg-input"
                  style={{ flex: 1 }}
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  placeholder="输入关键词（如：登录 认证）"
                  onKeyDown={(e) => { if (e.key === 'Enter') runSearch(); }}
                />
                <button className="sg-btn" disabled={!query.trim()} onClick={runSearch}>搜索</button>
              </div>
              {hits.length > 0 ? (
                <ul style={{ margin: 0, paddingLeft: 18 }}>
                  {hits.slice(0, 8).map((hit) => (
                    <li key={hit.chunkId} className="sg-muted" style={{ marginBottom: 6 }}>
                      {hit.snippet.split('\n').slice(0, 3).join(' / ')}
                    </li>
                  ))}
                </ul>
              ) : null}
            </div>
          </div>
        </div>

        {selected && (
          <aside className="sg-kb-drawer">
            <div className="sg-section" style={{ borderBottom: '1px solid var(--sg-border-default)' }}>
              <div className="sg-row" style={{ justifyContent: 'space-between' }}>
                <strong style={{ fontSize: 14, display: 'inline-flex', alignItems: 'center', gap: 8 }}>
                  {kindIcon(selected.kind)}
                  {selected.name}
                </strong>
                <button className="sg-icon-btn" aria-label="关闭详情" onClick={() => setSelectedId(null)}>
                  <IconX size={14} />
                </button>
              </div>
              <div style={{ marginTop: 8 }}>
                <span className={`sg-status ${statusClass(selected)}`}>{statusLabel(selected)}</span>
              </div>
            </div>

            <div className="sg-section">
              <dl className="sg-kv">
                <dt>类型</dt>
                <dd>{KIND_TAGS[selected.kind] ?? selected.kind}</dd>
                <dt>位置</dt>
                <dd className="sg-code">{selected.locator}</dd>
                <dt>启用</dt>
                <dd>{selected.enabled ? '是' : '否'}</dd>
                <dt>最后索引</dt>
                <dd>{selected.last_scanned_at ? new Date(selected.last_scanned_at).toLocaleString('zh-CN') : '—'}</dd>
              </dl>
              {selected.error ? (
                <div className="sg-banner sg-banner--error" style={{ marginTop: 12 }}>
                  {selected.error}
                </div>
              ) : null}
            </div>

            <div className="sg-section" style={{ display: 'grid', gap: 8 }}>
              <button className="sg-btn sg-btn--primary" disabled={busy} onClick={() => scan(selected.id)}>
                重新扫描
              </button>
              <button className="sg-btn" onClick={() => toggle(selected)}>
                {selected.enabled ? '停用（移出任务上下文）' : '启用（加入任务上下文）'}
              </button>
              <button className="sg-btn sg-btn--danger" onClick={() => remove(selected.id)}>
                移除来源
              </button>
            </div>
          </aside>
        )}
      </div>
    </>
  );
}

function kindIcon(kind: string) {
  if (kind === 'repo_path') return <IconFolder size={15} style={{ color: 'var(--sg-text-secondary)' }} />;
  if (kind === 'openapi') return <IconIssue size={15} style={{ color: 'var(--sg-text-secondary)' }} />;
  return <IconDoc size={15} style={{ color: 'var(--sg-text-secondary)' }} />;
}

function statusClass(source: SourceInfo): string {
  if (!source.enabled) return 'sg-status--disabled';
  if (source.scan_state === 'indexed') return 'sg-status--passed';
  if (source.scan_state === 'failed') return 'sg-status--error';
  return 'sg-status--running';
}

function statusLabel(source: SourceInfo): string {
  if (!source.enabled) return '已停用';
  if (source.scan_state === 'indexed') return '✓ 已就绪';
  if (source.scan_state === 'failed') return `✕ 失败`;
  return `◐ ${source.scan_state}`;
}
