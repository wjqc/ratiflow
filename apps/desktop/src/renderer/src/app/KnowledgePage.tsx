import { useCallback, useEffect, useState } from 'react';
import { rpc, rpcErrorMessage } from '../rpc/client';

interface SourceInfo {
  id: string; kind: string; name: string; locator: string; enabled: boolean;
  scan_state: string; last_scanned_at: string | null; error: string;
}

interface Props { projectId: string }

// 项目知识库（规范 §4.3）：来源管理、扫描状态、检索试用与隔离提示。
export default function KnowledgePage({ projectId }: Props) {
  const [sources, setSources] = useState<SourceInfo[]>([]);
  const [kind, setKind] = useState('repo_path');
  const [name, setName] = useState('');
  const [locator, setLocator] = useState('');
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

  return (
    <div className="sg-section" style={{ maxWidth: 960 }}>
      <h1 style={{ fontSize: 18, margin: 0 }}>项目知识库</h1>
      <p className="sg-muted">来源属于且仅属于本项目；检索、Manifest 与任务上下文不跨项目泄漏。</p>
      {error ? <div className="sg-banner sg-banner--error">{error}</div> : null}

      <div className="sg-row" style={{ marginTop: 12 }}>
        <label className="sg-field" style={{ width: 130 }}>
          <span>类型</span>
          <select className="sg-input" value={kind} onChange={(e) => setKind(e.target.value)}>
            <option value="repo_path">仓库目录</option>
            <option value="document">文档</option>
            <option value="openapi">OpenAPI</option>
            <option value="rule">规则</option>
          </select>
        </label>
        <label className="sg-field" style={{ width: 200 }}>
          <span>名称 *</span>
          <input className="sg-input" value={name} onChange={(e) => setName(e.target.value)} placeholder="主仓库" />
        </label>
        <label className="sg-field" style={{ flex: 1 }}>
          <span>位置（绝对路径）*</span>
          <input className="sg-input" value={locator} onChange={(e) => setLocator(e.target.value)} placeholder="/Users/you/project" />
        </label>
        <button
          className="sg-button sg-button--primary"
          disabled={busy || !name.trim() || !locator.trim()}
          onClick={() => {
            setBusy(true);
            void rpc('knowledge.create', { projectId, kind, name: name.trim(), locator: locator.trim() })
              .then(reload)
              .catch((reason) => setError(rpcErrorMessage(reason)))
              .finally(() => setBusy(false));
          }}
        >
          添加来源
        </button>
      </div>

      <table className="sg-table" style={{ marginTop: 16 }}>
        <thead>
          <tr><th>名称</th><th>类型</th><th>扫描状态</th><th>最近扫描</th><th>操作</th></tr>
        </thead>
        <tbody>
          {sources.length === 0 ? (
            <tr><td colSpan={5} className="sg-muted" style={{ textAlign: 'center', padding: 20 }}>尚无来源</td></tr>
          ) : sources.map((source) => (
            <tr key={source.id}>
              <td>{source.name}<br /><span className="sg-muted sg-code">{source.locator}</span></td>
              <td>{source.kind}</td>
              <td>
                <span className={`sg-status ${source.scan_state === 'indexed' ? 'sg-status--passed' : source.scan_state === 'failed' ? 'sg-status--error' : 'sg-status--running'}`}>
                  {source.scan_state === 'indexed' ? '✓ 已索引' : source.scan_state === 'failed' ? `✕ ${source.error.slice(0, 30)}` : `◐ ${source.scan_state}`}
                </span>
              </td>
              <td className="sg-muted">{source.last_scanned_at ? new Date(source.last_scanned_at).toLocaleString('zh-CN') : '—'}</td>
              <td>
                <div className="sg-row">
                  <button className="sg-button" disabled={busy} onClick={() => {
                    setBusy(true);
                    void rpc('knowledge.scan', { sourceId: source.id })
                      .then(reload)
                      .catch((reason) => setError(rpcErrorMessage(reason)))
                      .finally(() => setBusy(false));
                  }}>扫描</button>
                  <button className="sg-button" onClick={() => {
                    void rpc('knowledge.update', { sourceId: source.id, enabled: !source.enabled }).then(reload);
                  }}>{source.enabled ? '停用' : '启用'}</button>
                  <button className="sg-button sg-button--danger" onClick={() => {
                    void rpc('knowledge.remove', { sourceId: source.id }).then(reload);
                  }}>删除</button>
                </div>
              </td>
            </tr>
          ))}
        </tbody>
      </table>

      <div className="sg-stack" style={{ marginTop: 20 }}>
        <strong>检索试用</strong>
        <div className="sg-row">
          <input className="sg-input" style={{ flex: 1 }} value={query} onChange={(e) => setQuery(e.target.value)}
            placeholder="输入关键词（如：登录 认证）" onKeyDown={(e) => {
              if (e.key === 'Enter' && query.trim()) {
                void rpc<{ items: typeof hits }>('knowledge.search', { projectId, query: query.trim() })
                  .then((result) => setHits(result.items))
                  .catch((reason) => setError(rpcErrorMessage(reason)));
              }
            }} />
          <button className="sg-button" disabled={!query.trim()} onClick={() => {
            void rpc<{ items: typeof hits }>('knowledge.search', { projectId, query: query.trim() })
              .then((result) => setHits(result.items))
              .catch((reason) => setError(rpcErrorMessage(reason)));
          }}>搜索</button>
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
  );
}
