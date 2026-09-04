// S12 项目记忆页（ADR-032 / 实施方案 §3）：项目开关 + 计数 + 搜索/筛选 + 文件式列表
// + Drawer 详情；新建/导入/导出/归档恢复/两步清除全流程；全状态覆盖（§3.6）。
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { rpc } from '../../../rpc/client';
import { rpcErrText } from '../../../lib/rpcError';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { MemoryList } from '../components/MemoryList';
import { MemoryDrawer } from '../components/MemoryDrawer';
import { MemoryCandidates } from '../components/MemoryCandidates';
import type { MemoryListResult, MemorySettingsInfo } from '../types';

interface ProjectLite {
  id: string;
  name: string;
  archivedAt: string | null;
}

const FILTERS: Array<{ id: string; label: string; kinds?: string[]; statuses?: string[] }> = [
  { id: 'all', label: '全部' },
  { id: 'decision', label: '决策', kinds: ['decision'] },
  { id: 'convention', label: '约定', kinds: ['convention'] },
  { id: 'fact', label: '事实', kinds: ['fact'] },
  { id: 'lesson', label: '经验', kinds: ['lesson'] },
  { id: 'preference', label: '偏好', kinds: ['preference'] },
  { id: 'proposed', label: '待确认', statuses: ['proposed'] },
  { id: 'archived', label: '已归档', statuses: ['archived'] },
];

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
  const [filter, setFilter] = useState('all');
  const [toggling, setToggling] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [revealExportId, setRevealExportId] = useState<string | null>(null);
  const [drawer, setDrawer] = useState<{ id: string | null; create: boolean } | null>(null);
  const [refreshKey, setRefreshKey] = useState(0);
  const requestSeq = useRef(0);
  const debounceRef = useRef<number | null>(null);

  const activeFilter = FILTERS.find((f) => f.id === filter) ?? FILTERS[0];
  const featureOff = settings ? !settings.featureEnabled : false;
  // 全局 flag 只锁注入开关与捕获；浏览/编辑/导入导出/归档/清除是数据控制路径，不受限（§3.3/§16.2/MEM-029）。
  const readOnly = false;

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
    async (pid: string, q: string, f: (typeof FILTERS)[number]) => {
      const seq = ++requestSeq.current;
      setLoading(true);
      setListError(null);
      try {
        const res = await rpc<MemoryListResult>('memory.list', {
          projectId: pid,
          query: q.trim() || undefined,
          statuses: f.statuses,
          kinds: f.kinds,
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
    void loadList(projectId, query, activeFilter);
  }, [projectId, query, activeFilter, loadList]);

  // 搜索 250ms debounce。
  const onQueryChange = (v: string) => {
    setQueryInput(v);
    if (debounceRef.current !== null) window.clearTimeout(debounceRef.current);
    debounceRef.current = window.setTimeout(() => setQuery(v), 250);
  };

  const toggleEnabled = async () => {
    if (!projectId || !settings || toggling || readOnly) return;
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
      setNotice(updated.enabled ? '已开启：新 Run 将在预算内复用已确认记忆' : '已关闭：保留全部条目，仅停止注入');
    } catch (e) {
      setNotice(rpcErrText(e) || '开关失败');
      void loadSettings(projectId);
    } finally {
      setToggling(false);
    }
  };

  const doImport = async (mode: 'proposed' | 'active') => {
    if (!projectId) return;
    const picked = await window.sixgates.selectFile();
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
      if (projectId) void loadList(projectId, query, activeFilter);
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
    const ok = await window.sixgates.revealMemoryExport(revealExportId);
    setNotice(ok ? '已在访达中显示' : '导出目录不存在或已被清理');
  };

  const counts = list?.counts ?? {};
  const archivedProject = projects.find((p) => p.id === projectId)?.archivedAt != null;

  const emptyState = useMemo(() => {
    if (queryInput.trim()) return <>没有匹配「{queryInput}」的记忆。<button type="button" className="sg-btn" onClick={() => { setQuery(''); setQueryInput(''); }}>清除筛选</button></>;
    if (featureOff) return '项目记忆功能未开启：可浏览与编辑，注入由管理员/版本开关控制。';
    if (settings?.enabled === false) return '当前项目未开启记忆。开启后新 Run 才会复用已确认记忆；也可以先新建或导入。';
    return '还没有记忆。新建一条，或从 Markdown 导入。';
  }, [queryInput, featureOff, settings?.enabled]);

  return (
    <>
      <SettingsPageHeader
        title="项目记忆"
        scope="项目"
        description="该项目以后持续记住什么，由你确认与管理；记忆是上下文数据，不会改变审批、关卡或项目指令。"
      />
      <div className="sg-set-page">
        <SettingsSection
          title="在该项目中使用记忆"
          description="新启动的 Agent Run 会在预算内复用已确认的项目记忆。开启后可能增加模型输入 Token；不会自动改变审批、关卡或项目指令。"
        >
          <div className="sg-memory-switch-row">
            <label className="sg-memory-switch">
              <input
                type="checkbox"
                role="switch"
                aria-checked={settings?.enabled ?? false}
                checked={settings?.enabled ?? false}
                disabled={!projectId || !settings || toggling || featureOff}
                onChange={() => void toggleEnabled()}
              />
              <span>启用记忆注入（每 Run 最多 {settings?.maxEntries ?? 8} 条 / {Math.round((settings?.maxBytes ?? 12288) / 1024)} KiB）</span>
            </label>
            {featureOff ? <StatusPill kind="readonly" label="功能由当前版本/管理员关闭" /> : null}
            {archivedProject ? <StatusPill kind="readonly" label="项目已归档：只读浏览" /> : null}
          </div>
          <div className="sg-memory-project-row">
            <label className="sg-field">
              <span>项目</span>
              <select value={projectId ?? ''} onChange={(e) => { setDrawer(null); setProjectId(e.target.value || null); }}>
                {projects.length === 0 ? <option value="">（暂无项目）</option> : null}
                {projects.map((p) => (
                  <option key={p.id} value={p.id} disabled={p.archivedAt != null}>
                    {p.name}{p.archivedAt ? '（已归档）' : ''}
                  </option>
                ))}
              </select>
            </label>
            <span className="sg-memory-counts" aria-label="记忆计数">
              已确认 {counts.active ?? 0} · 待确认 {counts.proposed ?? 0} · 冲突 {counts.conflicted ?? 0} · 已归档 {counts.archived ?? 0}
            </span>
            <button type="button" className="sg-btn" onClick={() => projectId && void loadList(projectId, query, activeFilter)}>
              刷新
            </button>
          </div>
          {settingsError ? (
            <div role="alert" className="sg-memory-banner sg-memory-banner--error">
              {settingsError}
              <button type="button" className="sg-btn" onClick={() => projectId && void loadSettings(projectId)}>
                重试
              </button>
            </div>
          ) : null}
          {notice ? (
            <div className="sg-memory-banner sg-memory-banner--ok">
              {notice}
              {revealExportId ? (
                <button type="button" className="sg-btn" onClick={() => void reveal()}>
                  在访达中显示
                </button>
              ) : null}
            </div>
          ) : null}
        </SettingsSection>

        <SettingsSection
          title="记忆列表"
          actions={
            <div className="sg-memory-toolbar">
              <input
                type="search"
                className="sg-memory-search"
                aria-label="搜索记忆"
                placeholder="搜索标题与正文…"
                value={queryInput}
                onChange={(e) => onQueryChange(e.target.value)}
              />
              <button type="button" className="sg-btn" onClick={() => setDrawer({ id: null, create: true })} disabled={!projectId || readOnly || archivedProject}>
                新建记忆
              </button>
              <button type="button" className="sg-btn" onClick={() => void doImport('proposed')} disabled={!projectId || archivedProject}>
                导入 Markdown
              </button>
              <button type="button" className="sg-btn" onClick={() => void doExport(false)} disabled={!projectId}>
                导出
              </button>
            </div>
          }
        >
          <div className="sg-memory-filters" role="group" aria-label="筛选">
            {FILTERS.map((f) => (
              <button
                key={f.id}
                type="button"
                className={`sg-memory-filter ${filter === f.id ? 'sg-memory-filter--active' : ''}`}
                aria-pressed={filter === f.id}
                onClick={() => setFilter(f.id)}
              >
                {f.label}
              </button>
            ))}
          </div>

          {projectId ? (
            <MemoryCandidates
              projectId={projectId}
              refreshKey={refreshKey}
              onChanged={() => {
                if (projectId) void loadList(projectId, query, activeFilter);
              }}
            />
          ) : null}

          <div id="sg-memory-list-zone">
            <MemoryList
              items={list?.items ?? []}
              loading={loading}
              activeId={drawer?.id ?? null}
              onSelect={(id) => setDrawer({ id, create: false })}
              empty={emptyState}
            />
          </div>

          {listError ? (
            <div role="alert" className="sg-memory-banner sg-memory-banner--error">
              {listError}
              <button
                type="button"
                className="sg-btn"
                onClick={() => projectId && void loadList(projectId, query, activeFilter)}
              >
                重试
              </button>
            </div>
          ) : null}
        </SettingsSection>
      </div>

      {drawer && projectId ? (
        <MemoryDrawer
          projectId={projectId}
          memoryId={drawer.id}
          createMode={drawer.create}
          readOnly={readOnly || archivedProject}
          onClose={() => setDrawer(null)}
          onChanged={() => {
            setRefreshKey((k) => k + 1);
            if (projectId) {
              void loadList(projectId, query, activeFilter);
              void loadSettings(projectId);
            }
          }}
        />
      ) : null}
    </>
  );
}
