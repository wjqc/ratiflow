import { useCallback, useEffect, useState } from 'react';
import { rpc, rpcErrorMessage } from '../rpc/client';
import { relativeTime } from '../lib/format';
import { renderMarkdown } from '../lib/markdown';
import { IconBook, IconDoc, IconFolder, IconIssue, IconPlus, IconSearch, IconX } from '../components/Icons';

interface ArtifactRow {
  id: string;
  workitemId: string;
  workitemTitle: string;
  kind: string;
  title: string;
  updated_at: string;
}

const ARTIFACT_KIND: Record<string, string> = {
  prd: 'PRD',
  tech_design: '技术方案',
  code: '代码产出',
  test: '测试产出',
  deployment: '部署产物',
  verification: '验收产出',
};

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

const GATE_OF_KIND: Record<string, string> = {
  prd: '需求关',
  tech_design: '方案关',
  code: '开发关',
  test: '测试关',
  deployment: '部署关',
  verification: '验证关',
};

type Selection = { kind: 'artifact' | 'source'; id: string } | null;

// 项目知识库：左目录（任务产物 + 知识来源）/ 右预览与编辑（规范 §4.3）。
export default function KnowledgePage({ projectId, projectName }: Props) {
  const [sources, setSources] = useState<SourceInfo[]>([]);
  const [showAdd, setShowAdd] = useState(false);
  const [kind, setKind] = useState('repo_path');
  const [name, setName] = useState('');
  const [locator, setLocator] = useState('');
  const [query, setQuery] = useState('');
  const [hits, setHits] = useState<
    Array<{ source_name?: string; title?: string; path?: string; snippet: string; score?: number }>
  >([]);
  const [contextPreview, setContextPreview] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  // 任务产物（各关工件，如 PRD/技术方案）：跟随项目。
  const [artifacts, setArtifacts] = useState<ArtifactRow[]>([]);
  const [artifactContent, setArtifactContent] = useState('');
  const [artifactLoading, setArtifactLoading] = useState(false);
  const [artifactRev, setArtifactRev] = useState<{ id: string; status: string; etag: string } | null>(null);
  const [artifactMode, setArtifactMode] = useState<'preview' | 'edit'>('preview');
  const [artifactEdit, setArtifactEdit] = useState('');
  const [artifactNotice, setArtifactNotice] = useState('');

  const [selection, setSelection] = useState<Selection>(null);

  const reloadSources = useCallback(async () => {
    try {
      const page = await rpc<{ items: SourceInfo[] }>('knowledge.list', { projectId });
      setSources(page.items);
    } catch (reason) {
      setError(rpcErrorMessage(reason));
    }
  }, [projectId]);

  const loadArtifacts = useCallback(async () => {
    try {
      const list = await rpc<{ items: Array<{ id: string; title: string }> }>('workitem.list', {
        projectId,
        limit: 50,
      });
      const rows: ArtifactRow[] = [];
      for (const wi of list.items ?? []) {
        try {
          const res = await rpc<{
            items: Array<{ id: string; kind: string; title: string; updated_at: string }>;
          }>('artifact.list', { workItemId: wi.id });
          for (const a of res.items ?? []) {
            rows.push({
              id: a.id,
              workitemId: wi.id,
              workitemTitle: wi.title,
              kind: a.kind,
              title: a.title,
              updated_at: a.updated_at,
            });
          }
        } catch {
          /* 单个工作项失败不影响整体 */
        }
      }
      setArtifacts(rows);
    } catch {
      setArtifacts([]);
    }
  }, [projectId]);

  const loadArtifactContent = useCallback(async (a: ArtifactRow) => {
    setArtifactLoading(true);
    setArtifactNotice('');
    try {
      const page = await rpc<{ items: Array<{ id: string; status: string; etag: string }> }>(
        'artifact.listRevisions',
        { artifactId: a.id },
      );
      const items = page.items ?? [];
      const current = items.find((r) => r.status !== 'superseded') ?? items[0] ?? null;
      setArtifactRev(current ? { id: current.id, status: current.status, etag: current.etag } : null);
      if (!current) {
        setArtifactContent('（该工件还没有修订内容）');
        return;
      }
      const res = await rpc<{ content: string }>('artifact.revisionContent', {
        revisionId: current.id,
      });
      setArtifactContent(res.content ?? '');
    } catch (reason) {
      setArtifactContent(`读取失败：${reason instanceof Error ? reason.message : String(reason)}`);
    } finally {
      setArtifactLoading(false);
    }
  }, []);

  useEffect(() => {
    void reloadSources();
    void loadArtifacts();
  }, [reloadSources, loadArtifacts]);

  const selected = selection
    ? selection.kind === 'artifact'
      ? artifacts.find((a) => a.id === selection.id) ?? null
      : sources.find((s) => s.id === selection.id) ?? null
    : null;
  const selectedArtifact =
    selection?.kind === 'artifact' ? (selected as ArtifactRow | null) : null;
  const selectedSource =
    selection?.kind === 'source' ? (selected as SourceInfo | null) : null;
  const enabledSources = sources.filter((s) => s.enabled);

  // 选中产物后自动加载内容。
  useEffect(() => {
    if (selection?.kind === 'artifact') {
      const a = artifacts.find((x) => x.id === selection.id);
      if (a) void loadArtifactContent(a);
    }
  }, [selection, artifacts, loadArtifactContent]);

  const addSource = () => {
    setBusy(true);
    void rpc('knowledge.create', { projectId, kind, name: name.trim(), locator: locator.trim() })
      .then(() => {
        setName('');
        setLocator('');
        setShowAdd(false);
        return reloadSources();
      })
      .catch((reason) => setError(rpcErrorMessage(reason)))
      .finally(() => setBusy(false));
  };

  const scan = (sourceId: string) => {
    setBusy(true);
    void rpc('knowledge.scan', { sourceId })
      .then(reloadSources)
      .catch((reason) => setError(rpcErrorMessage(reason)))
      .finally(() => setBusy(false));
  };

  const toggleSource = (source: SourceInfo) => {
    void rpc('knowledge.update', { sourceId: source.id, enabled: !source.enabled }).then(reloadSources);
  };

  const removeSource = (sourceId: string) => {
    void rpc('knowledge.remove', { sourceId }).then(() => {
      setSelection((cur) => (cur?.kind === 'source' && cur.id === sourceId ? null : cur));
      return reloadSources();
    });
  };

  // 团队共享：git pull 后把仓库 knowledge/ 清单对账进本地索引（manifest 平面）。
  const [syncingRepo, setSyncingRepo] = useState(false);
  const syncFromRepo = async () => {
    setSyncingRepo(true);
    try {
      await rpc('knowledge.syncFromRepo', { projectId });
      await reloadSources();
    } catch {
      /* 同步失败静默：来源列表仍是本地索引 */
    } finally {
      setSyncingRepo(false);
    }
  };

  const rescanAll = () => {
    setBusy(true);
    void Promise.all(sources.map((s) => rpc('knowledge.scan', { sourceId: s.id })))
      .then(reloadSources)
      .catch((reason) => setError(rpcErrorMessage(reason)))
      .finally(() => setBusy(false));
  };

  const runSearch = () => {
    if (!query.trim()) return;
    void rpc<{ items: typeof hits }>('knowledge.searchV2', { projectId, query: query.trim(), includeTests: false, limit: 20 })
      .then((result) => setHits(result.items))
      .catch((reason) => setError(rpcErrorMessage(reason)));
  };

  const previewContext = () => {
    if (!query.trim()) return;
    void rpc('context.preview', { projectId, query: query.trim(), maxBytes: 64 * 1024 })
      .then(setContextPreview)
      .catch((reason) => setError(rpcErrorMessage(reason)));
  };

  const saveArtifactEdit = async () => {
    if (!artifactRev || artifactRev.status !== 'draft') return;
    try {
      await rpc('artifact.updateDraft', {
        revisionId: artifactRev.id,
        etag: artifactRev.etag,
        content: artifactEdit,
      });
      setArtifactContent(artifactEdit);
      setArtifactNotice('已保存。');
      setArtifactMode('preview');
    } catch (reason) {
      setArtifactNotice(`保存失败：${reason instanceof Error ? reason.message : String(reason)}`);
    }
  };

  const selectArtifact = (a: ArtifactRow) => {
    setSelection({ kind: 'artifact', id: a.id });
    setArtifactMode('preview');
    void loadArtifactContent(a);
  };

  const artifactGroups = new Map<string, ArtifactRow[]>();
  for (const a of artifacts) {
    const list = artifactGroups.get(a.workitemTitle) ?? [];
    list.push(a);
    artifactGroups.set(a.workitemTitle, list);
  }

  return (
    <div className="sg-kbv2">
      <header className="sg-kbv2-head">
        <span className="sg-page-head-title">
          {projectName ? `${projectName} · 项目知识库` : '项目知识库'}
        </span>
        <span className="sg-kbv2-head-note">本地运行 · 产物默认跟随项目归档</span>
        <div className="sg-kbv2-head-actions">
          <button className="sg-btn sg-btn--sm" disabled={syncingRepo} onClick={() => void syncFromRepo()}>
            {syncingRepo ? '同步中…' : '从仓库同步'}
          </button>
          <button className="sg-btn sg-btn--sm" disabled={busy || sources.length === 0} onClick={rescanAll}>
            重建索引
          </button>
          <button className="sg-btn sg-btn--primary sg-btn--sm" onClick={() => setShowAdd((v) => !v)}>
            <IconPlus size={13} />
            添加来源
          </button>
        </div>
      </header>

      {error ? <div className="sg-banner sg-banner--error" style={{ margin: '0 16px' }} role="alert">{error}</div> : null}

      {showAdd ? (
        <div className="sg-card" style={{ margin: '10px 16px 0' }}>
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
              <select className="sg-select" value={kind} onChange={(e) => setKind(e.target.value)}>
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
            <button className="sg-btn sg-btn--primary" disabled={busy || !name.trim() || !locator.trim()} onClick={addSource}>
              添加
            </button>
          </div>
        </div>
      ) : null}

      <div className="sg-kbv2-layout">
        {/* 左：目录树 */}
        <aside className="sg-kbv2-side">
          <div className="sg-kbv2-side-title">任务产物</div>
          {artifactGroups.size === 0 ? (
            <div className="sg-kbv2-empty">本项目的任务还没有产出工件。</div>
          ) : (
            [...artifactGroups.entries()].map(([title, rows]) => (
              <div key={title} className="sg-kbv2-group">
                <div className="sg-kbv2-group-name">{title}</div>
                {rows.map((a) => {
                  const active = selection?.kind === 'artifact' && selection.id === a.id;
                  return (
                    <button
                      key={a.id}
                      className={`sg-kbv2-item${active ? ' sg-kbv2-item--active' : ''}`}
                      onClick={() => selectArtifact(a)}
                    >
                      <IconDoc size={12} />
                      {ARTIFACT_KIND[a.kind] ?? a.kind}
                    </button>
                  );
                })}
              </div>
            ))
          )}

          <div className="sg-kbv2-side-title" style={{ marginTop: 14 }}>
            知识来源（{sources.length}）
          </div>
          {sources.length === 0 ? (
            <div className="sg-kbv2-empty">点右上角「添加来源」接入仓库、文档或规则。</div>
          ) : (
            sources.map((s) => {
              const active = selection?.kind === 'source' && selection.id === s.id;
              return (
                <button
                  key={s.id}
                  className={`sg-kbv2-item${active ? ' sg-kbv2-item--active' : ''}`}
                  onClick={() => setSelection({ kind: 'source', id: s.id })}
                >
                  {kindIcon(s.kind)}
                  <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{s.name}</span>
                  {!s.enabled ? <span className="sg-kbv2-item-tag">停用</span> : null}
                </button>
              );
            })
          )}
        </aside>

        {/* 右：预览 / 编辑 / 管理 */}
        <section className="sg-kbv2-main">
          {showAdd && !selection ? (
            <div className="sg-kbv2-hint">来源添加表单在上方；添加后会出现在左侧目录。</div>
          ) : null}

          {!selected ? (
            <div className="sg-kbv2-hint" style={{ alignItems: 'center' }}>
              <IconBook size={22} />
              <div style={{ textAlign: 'center' }}>
                从左侧目录选择产物或知识来源
                <div className="sg-sub" style={{ marginTop: 6 }}>
                  产物支持 Markdown 预览与草稿编辑；知识来源可扫描、启停与检索试用。
                </div>
              </div>
            </div>
          ) : null}

          {/* 产物：Markdown 预览 / 草稿编辑 */}
          {selectedArtifact ? (
            <>
              <div className="sg-kbv2-main-head">
                <span className="sg-kbv2-crumb">
                  {selectedArtifact.workitemTitle} / {ARTIFACT_KIND[selectedArtifact.kind] ?? selectedArtifact.kind}
                </span>
                <div className="sg-kbv2-main-actions">
                  {artifactRev && artifactRev.status === 'draft' ? (
                    artifactMode === 'edit' ? (
                      <button className="sg-btn sg-btn--primary sg-btn--sm" onClick={() => void saveArtifactEdit()}>
                        保存
                      </button>
                    ) : (
                      <button
                        className="sg-btn sg-btn--sm"
                        onClick={() => {
                          setArtifactEdit(artifactContent);
                          setArtifactNotice('');
                          setArtifactMode('edit');
                        }}
                      >
                        编辑
                      </button>
                    )
                  ) : (
                    <span className="sg-kbv2-readonly">只读</span>
                  )}
                </div>
              </div>
              <div className="sg-kbv2-scroll">
                {artifactLoading ? (
                  <div className="sg-kbv2-loading">正在加载…</div>
                ) : artifactMode === 'edit' ? (
                  <textarea
                    className="sg-kbv2-edit"
                    value={artifactEdit}
                    onChange={(e) => setArtifactEdit(e.target.value)}
                    aria-label="编辑产物内容"
                  />
                ) : (
                  <div className="sg-kbv2-body sg-md-body">
                    <div dangerouslySetInnerHTML={{ __html: renderMarkdown(artifactContent) }} />
                  </div>
                )}
              </div>
              {artifactNotice ? <div className="sg-kbv2-note sg-kbv2-note--error">{artifactNotice}</div> : null}
              {artifactRev && artifactRev.status !== 'draft' && artifactMode !== 'edit' ? (
                <div className="sg-kbv2-note">
                  当前版本已定稿（{artifactRev.status}），如需修改请在门禁评审中发起新修订。
                </div>
              ) : null}
            </>
          ) : null}

          {/* 来源：管理详情 + 检索试用 */}
          {selectedSource ? (
            <>
              <div className="sg-kbv2-main-head">
                <span className="sg-kbv2-crumb">
                  {kindIcon(selectedSource.kind)}
                  {selectedSource.name}
                </span>
                <div className="sg-kbv2-main-actions">
                  <span className={`sg-status ${statusClass(selectedSource)}`}>{statusLabel(selectedSource)}</span>
                  <button className="sg-btn sg-btn--sm" disabled={busy} onClick={() => scan(selectedSource.id)}>
                    重新扫描
                  </button>
                  <button className="sg-btn sg-btn--sm" onClick={() => toggleSource(selectedSource)}>
                    {selectedSource.enabled ? '停用' : '启用'}
                  </button>
                  <button className="sg-btn sg-btn--danger sg-btn--sm" onClick={() => removeSource(selectedSource.id)}>
                    移除
                  </button>
                </div>
              </div>
              <div className="sg-kbv2-scroll">
              <div className="sg-kbv2-body">
                <div className="sg-card" style={{ margin: '0 0 14px' }}>
                  <div className="sg-card-head">来源信息</div>
                  <div style={{ padding: '10px 14px' }}>
                    <dl className="sg-kv">
                      <dt>类型</dt>
                      <dd>{KIND_TAGS[selectedSource.kind] ?? selectedSource.kind}</dd>
                      <dt>位置</dt>
                      <dd className="sg-code">{selectedSource.locator}</dd>
                      <dt>启用</dt>
                      <dd>{selectedSource.enabled ? '是（参与任务上下文）' : '否'}</dd>
                      <dt>最后索引</dt>
                      <dd>
                        {selectedSource.last_scanned_at
                          ? new Date(selectedSource.last_scanned_at).toLocaleString('zh-CN')
                          : '—'}
                      </dd>
                    </dl>
                    {selectedSource.error ? (
                      <div className="sg-banner sg-banner--error" style={{ marginTop: 10 }}>
                        {selectedSource.error}
                      </div>
                    ) : null}
                  </div>
                </div>

                <div className="sg-card">
                  <div className="sg-card-head">检索试用</div>
                  <div style={{ padding: '12px 14px', display: 'grid', gap: 10 }}>
                    <div className="sg-row">
                      <input
                        className="sg-input"
                        style={{ flex: 1 }}
                        value={query}
                        onChange={(e) => setQuery(e.target.value)}
                        placeholder="输入关键词（如：登录 认证）"
                        onKeyDown={(e) => {
                          if (e.key === 'Enter') runSearch();
                        }}
                      />
                      <button className="sg-btn" disabled={!query.trim()} onClick={runSearch}>
                        搜索
                      </button>
                      <button className="sg-btn" disabled={!query.trim()} onClick={previewContext}>
                        预览注入上下文
                      </button>
                    </div>
                    {hits.length > 0 ? (
                      <ul style={{ margin: 0, paddingLeft: 18 }}>
                        {hits.slice(0, 8).map((hit, index) => (
                          <li key={index} style={{ marginBottom: 8 }}>
                            <div style={{ display: 'flex', gap: 8, alignItems: 'baseline' }}>
                              <span className="sg-chip">{hit.source_name || '未知来源'}</span>
                              <span style={{ fontSize: 12.5 }}>{hit.title || hit.path || ''}</span>
                              {typeof hit.score === 'number' && hit.score > 0 ? (
                                <span className="sg-muted" style={{ marginLeft: 'auto', fontSize: 12 }}>
                                  相关度 {hit.score}
                                </span>
                              ) : null}
                            </div>
                            <div className="sg-muted">{hit.snippet.split('\n').slice(0, 3).join(' / ')}</div>
                          </li>
                        ))}
                      </ul>
                    ) : null}
                    {contextPreview ? (
                      <pre style={{ margin: 0, padding: 10, maxHeight: 260, overflow: 'auto', whiteSpace: 'pre-wrap' }}>
                        {JSON.stringify(contextPreview, null, 2)}
                      </pre>
                    ) : null}
                  </div>
                </div>
              </div>
              </div>
            </>
          ) : null}
        </section>
      </div>
    </div>
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
