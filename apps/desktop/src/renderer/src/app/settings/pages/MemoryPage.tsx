// S12 项目记忆页（参考稿对齐版）：工作区记忆开关卡片 → 项目/计数 + 搜索 →
// 「文件」列表（slug.md + 相对时间 + 注入开关）；新建/导入/导出在列表工具行。
// 不区分类型；状态详情在抽屉内查看（§3.6 状态仍全覆盖）。
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { rpc } from '../../../rpc/client';
import { rpcErrText } from '../../../lib/rpcError';
import { StatusPill } from '../components/StatusPill';
import { SettingsToggle } from '../components/SettingsRow';
import { IconMore, IconPlus, IconRefresh, IconSearch } from '../../../components/Icons';
import { MemoryList } from '../components/MemoryList';
import { MemoryDrawer } from '../components/MemoryDrawer';
import { MemoryCandidates } from '../components/MemoryCandidates';
import type { MemoryListItem, MemoryListResult, MemorySettingsInfo } from '../types';

interface ProjectLite {
  id: string;
  name: string;
  archivedAt: string | null;
}

export function MemoryPage() {
  const [projects, setProjects] = useState<ProjectLite[]>([]);
  const [projectId, setProjectId] = useState<string | null>(null);
  const [settings, setSettings] = useState<MemorySettingsInfo | null>(null);
  const [settingsError, setSettingsError] = useState<string | null>(null);
  const [list, setList] = useState<MemoryListResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [listError, setListError] = useState<string | null>(null);
  const [queryInput, setQueryInput] = useState('');
  const [query, setQuery] = useState('');
  const [toggling, setToggling] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [syncing, setSyncing] = useState(false);
  const [syncResult, setSyncResult] = useState<string | null>(null);
  const [revealExportId, setRevealExportId] = useState<string | null>(null);
  const [drawer, setDrawer] = useState<{ id: string | null; create: boolean } | null>(null);
  const [actionsOpen, setActionsOpen] = useState(false);
  const [refreshKey, setRefreshKey] = useState(0);
  const requestSeq = useRef(0);
  const debounceRef = useRef<number | null>(null);

  // 项目已归档只影响写入路径；浏览/编辑/导入导出/归档/清除是数据控制路径，不受限（§3.3/§16.2/MEM-029）。
  const archivedProject = projects.find((p) => p.id === projectId)?.archivedAt != null;

  // 项目列表（首个未归档项目默认选中）。
  useEffect(() => {
    rpc<{ items?: Array<Record<string, unknown>> }>('project.list', {})
      .then((res) => {
        const lite: ProjectLite[] = (res.items ?? []).map((p) => ({
          id: String(p.id),
          name: String(p.name || p.id),
          archivedAt: (p.archivedAt as string | null | undefined) ?? null,
        }));
        setProjects(lite);
        setProjectId((cur) => cur ?? lite.find((p) => !p.archivedAt)?.id ?? lite[0]?.id ?? null);
      })
      .catch(() => setProjects([]));
  }, []);

  const loadSettings = useCallback(async (pid: string) => {
    setSettingsError(null);
    try {
      setSettings(await rpc<MemorySettingsInfo>('memory.settingsGet', { projectId: pid }));
    } catch (e) {
      setSettings(null);
      setSettingsError(rpcErrText(e) || '设置加载失败');
    }
  }, []);

  const loadList = useCallback(
    async (pid: string, q: string) => {
      const seq = ++requestSeq.current;
      setLoading(true);
      setListError(null);
      try {
        const res = await rpc<MemoryListResult>('memory.list', {
          projectId: pid,
          query: q.trim() || undefined,
          limit: 50,
        });
        // 旧请求结果不得覆盖新 query（§7.1）。
        if (seq === requestSeq.current) setList(res);
      } catch (e) {
        if (seq === requestSeq.current) setListError(rpcErrText(e) || '列表加载失败');
      } finally {
        if (seq === requestSeq.current) setLoading(false);
      }
    },
    [],
  );

  useEffect(() => {
    if (!projectId) return;
    void loadSettings(projectId);
  }, [projectId, loadSettings]);

  useEffect(() => {
    if (!projectId) return;
    void loadList(projectId, query);
  }, [projectId, query, refreshKey, loadList]);

  // 团队共享：以 <repo>/memory/*.md 为权威对账（git pull 后手动兜底；项目切换已自动触发）。
  const syncFromRepo = useCallback(async (pid: string) => {
    setSyncing(true);
    setSyncResult(null);
    try {
      const r = await rpc<{ synced?: boolean; created?: number; updated?: number; removed?: number; reason?: string }>(
        'memory.syncFromRepo', { projectId: pid });
      setSyncResult(
        r.synced === false
          ? `未同步：${r.reason === 'project_root_missing' ? '项目未登记本地目录' : (r.reason ?? '未知原因')}`
          : `已同步：新增 ${r.created ?? 0} · 更新 ${r.updated ?? 0} · 移除 ${r.removed ?? 0}`,
      );
      void loadList(pid, query);
    } catch (e) {
      setSyncResult(rpcErrText(e) || '同步失败');
    } finally {
      setSyncing(false);
    }
  }, [loadList, query]);

  // 搜索 250ms debounce。
  const onQueryChange = (v: string) => {
    setQueryInput(v);
    if (debounceRef.current !== null) window.clearTimeout(debounceRef.current);
    debounceRef.current = window.setTimeout(() => setQuery(v), 250);
  };

  const toggleEnabled = async () => {
    if (!projectId || !settings || toggling) return;
    setToggling(true);
    setNotice(null);
    try {
      const updated = await rpc<MemorySettingsInfo>('memory.settingsUpdate', {
        projectId,
        settings: { enabled: !settings.enabled },
        expectedRevision: settings.revision,
        idempotencyKey: crypto.randomUUID(),
      });
      setSettings(updated);
    } catch (e) {
      setNotice(rpcErrText(e) || '开关失败');
      void loadSettings(projectId);
    } finally {
      setToggling(false);
    }
  };

  // 行内注入开关：active ↔ archived（归档可恢复；其余状态在抽屉内裁决）。
  const toggleItemActive = async (item: MemoryListItem) => {
    if (!projectId) return;
    setNotice(null);
    try {
      if (item.status === 'active') {
        await rpc('memory.archive', {
          projectId,
          memoryId: item.id,
          expectedRevision: item.revision,
          idempotencyKey: crypto.randomUUID(),
        });
      } else {
        await rpc('memory.restore', {
          projectId,
          memoryId: item.id,
          expectedRevision: item.revision,
          idempotencyKey: crypto.randomUUID(),
        });
      }
      setRefreshKey((k) => k + 1);
      if (projectId) void loadList(projectId, query);
    } catch (e) {
      setNotice(rpcErrText(e) || '操作失败');
      if (projectId) void loadList(projectId, query);
    }
  };

  const doImport = async (mode: 'proposed' | 'active') => {
    if (!projectId) return;
    const picked = await window.ratiflow.selectFile();
    if (!picked) return;
    setNotice(null);
    try {
      const res = await rpc<{ created: unknown[]; duplicates: unknown[] }>('memory.import', {
        projectId,
        filename: picked.filename,
        contentBase64: picked.contentBase64,
        mode,
        idempotencyKey: crypto.randomUUID(),
      });
      setNotice(`导入完成：新建 ${res.created?.length ?? 0} 条，跳过/去重 ${res.duplicates?.length ?? 0} 条`);
      setRefreshKey((k) => k + 1);
      if (projectId) void loadList(projectId, query);
    } catch (e) {
      setNotice(rpcErrText(e) || '导入失败');
    }
  };

  const doExport = async (includeArchived: boolean) => {
    if (!projectId) return;
    setNotice(null);
    try {
      const res = await rpc<{ exportId: string; count: number }>('memory.export', {
        projectId,
        includeArchived,
        format: 'markdown',
      });
      setRevealExportId(res.exportId);
      setNotice(`导出完成（${res.count} 条），导出 ID ${res.exportId}`);
    } catch (e) {
      setNotice(rpcErrText(e) || '导出失败');
    }
  };

  const reveal = async () => {
    if (!revealExportId) return;
    const ok = await window.ratiflow.revealMemoryExport(revealExportId);
    setNotice(ok ? '已在访达中显示' : '导出目录不存在或已被清理');
  };

  const counts = list?.counts ?? {};
  const total =
    (counts.active ?? 0) + (counts.proposed ?? 0) + (counts.conflicted ?? 0) + (counts.archived ?? 0);

  const emptyState = useMemo(() => {
    if (queryInput.trim()) {
      return (
        <>
          没有匹配「{queryInput}」的记忆。
          <button
            type="button"
            className="sg-btn"
            onClick={() => {
              setQuery('');
              setQueryInput('');
            }}
          >
            清除筛选
          </button>
        </>
      );
    }
    if (settings?.enabled === false) return '当前项目未开启记忆。开启后新 Run 才会复用已确认记忆；也可以先新建或导入。';
    return '还没有记忆。新建一条，或从 Markdown 导入。';
  }, [queryInput, settings?.enabled]);

  return (
    <div className="sg-set-page sg-reference-page sg-memory-page">
      <header className="sg-reference-page-head">
        <h1>记忆</h1>
      </header>

      <section className="sg-memory-master" aria-label="工作区记忆">
        <div>
          <strong>工作区记忆</strong>
          <p>在工作区中保存并复用长期上下文，新会话生效。开启后可能增加模型调用和 Token 成本。</p>
        </div>
        <SettingsToggle
          label="启用记忆注入"
          checked={settings?.enabled ?? false}
          disabled={!projectId || !settings || toggling}
          onChange={() => void toggleEnabled()}
        />
      </section>

      {archivedProject ? (
        <div className="sg-memory-state-row">
          <StatusPill kind="readonly" label="项目已归档：只读浏览" />
        </div>
      ) : null}
      <div className="sg-memory-state-row">
        <span className="sg-hint">记忆随仓库同步（<code className="sg-code">memory/</code> 目录，git 提交后共享给团队）</span>
        <button
          type="button"
          className="sg-btn sg-btn--sm"
          disabled={!projectId || syncing}
          onClick={() => projectId && void syncFromRepo(projectId)}
        >
          <IconRefresh size={12} />
          {syncing ? '同步中…' : '从仓库同步'}
        </button>
        {syncResult ? <span className="sg-hint">{syncResult}</span> : null}
      </div>
      {settingsError ? (
        <div role="alert" className="sg-memory-banner sg-memory-banner--error">
          {settingsError}
          <button type="button" className="sg-btn" onClick={() => projectId && void loadSettings(projectId)}>重试</button>
        </div>
      ) : null}
      {notice ? (
        <div className="sg-memory-banner sg-memory-banner--ok">
          {notice}
          {revealExportId ? <button type="button" className="sg-btn" onClick={() => void reveal()}>在访达中显示</button> : null}
        </div>
      ) : null}

      <div className="sg-reference-toolbar">
        <div className="sg-reference-toolbar-start">
          <label className="sg-reference-scope">
            <select
              value={projectId ?? ''}
              aria-label="选择项目"
              onChange={(event) => {
                setDrawer(null);
                setProjectId(event.target.value || null);
              }}
            >
              {projects.length === 0 ? <option value="">暂无工作区</option> : null}
              {projects.map((project) => (
                <option key={project.id} value={project.id} disabled={project.archivedAt != null}>
                  {project.name}{project.archivedAt ? '（已归档）' : ''}
                </option>
              ))}
            </select>
          </label>
          <span className="sg-reference-divider" aria-hidden />
          <span className="sg-reference-count" aria-label="记忆总数">{total} 条记忆</span>
        </div>

        <div className="sg-reference-toolbar-end">
          <label className="sg-reference-search">
            <IconSearch size={14} />
            <input
              type="search"
              aria-label="搜索记忆文件"
              placeholder="搜索记忆文件…"
              value={queryInput}
              onChange={(event) => onQueryChange(event.target.value)}
            />
          </label>
          <div className="sg-reference-more">
            <button
              type="button"
              className="sg-reference-icon-btn"
              aria-label="更多记忆操作"
              aria-expanded={actionsOpen}
              onClick={() => setActionsOpen((open) => !open)}
            >
              <IconMore size={16} />
            </button>
            {actionsOpen ? (
              <div className="sg-reference-menu" role="menu">
                <button type="button" role="menuitem" onClick={() => { setActionsOpen(false); setDrawer({ id: null, create: true }); }} disabled={!projectId || archivedProject}>
                  <IconPlus size={14} />新建记忆
                </button>
                <button type="button" role="menuitem" onClick={() => { setActionsOpen(false); void doImport('proposed'); }} disabled={!projectId || archivedProject}>导入 Markdown</button>
                <button type="button" role="menuitem" onClick={() => { setActionsOpen(false); void doExport(false); }} disabled={!projectId}>导出</button>
              </div>
            ) : null}
          </div>
          <button type="button" className="sg-reference-icon-btn" aria-label="刷新记忆列表" onClick={() => projectId && void loadList(projectId, query)}>
            <IconRefresh size={15} />
          </button>
        </div>
      </div>

      <div className="sg-memory-files">
        {projectId ? (
          <MemoryCandidates
            projectId={projectId}
            refreshKey={refreshKey}
            onChanged={() => {
              setRefreshKey((key) => key + 1);
              if (projectId) void loadList(projectId, query);
            }}
          />
        ) : null}
        <div id="sg-memory-list-zone">
          <MemoryList
            items={list?.items ?? []}
            loading={loading}
            activeId={drawer?.id ?? null}
            onSelect={(id) => setDrawer({ id, create: false })}
            onToggleActive={(item) => void toggleItemActive(item)}
            empty={emptyState}
          />
        </div>
        {listError ? (
          <div role="alert" className="sg-memory-banner sg-memory-banner--error">
            {listError}
            <button type="button" className="sg-btn" onClick={() => projectId && void loadList(projectId, query)}>重试</button>
          </div>
        ) : null}
      </div>

      {drawer && projectId ? (
        <MemoryDrawer
          projectId={projectId}
          memoryId={drawer.id}
          createMode={drawer.create}
          readOnly={archivedProject}
          onClose={() => setDrawer(null)}
          onChanged={() => {
            setRefreshKey((k) => k + 1);
            if (projectId) {
              void loadList(projectId, query);
              void loadSettings(projectId);
            }
          }}
        />
      ) : null}
    </div>
  );
}
