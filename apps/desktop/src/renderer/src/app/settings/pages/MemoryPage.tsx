// S12 项目记忆页（参考稿对齐版）：工作区记忆开关卡片 → 项目/计数 + 搜索 →
// 「文件」列表（slug.md + 相对时间 + 注入开关）；新建/导入/导出在列表工具行。
// 不区分类型；状态详情在抽屉内查看（§3.6 状态仍全覆盖）。
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { rpc } from '../../../rpc/client';
import { rpcErrText } from '../../../lib/rpcError';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { IconRefresh } from '../../../components/Icons';
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
  const [revealExportId, setRevealExportId] = useState<string | null>(null);
  const [drawer, setDrawer] = useState<{ id: string | null; create: boolean } | null>(null);
  const [refreshKey, setRefreshKey] = useState(0);
  const requestSeq = useRef(0);
  const debounceRef = useRef<number | null>(null);

  const featureOff = settings ? !settings.featureEnabled : false;
  // 全局 flag 只锁注入开关与捕获；浏览/编辑/导入导出/归档/清除是数据控制路径，不受限（§3.3/§16.2/MEM-029）。
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

  // 搜索 250ms debounce。
  const onQueryChange = (v: string) => {
    setQueryInput(v);
    if (debounceRef.current !== null) window.clearTimeout(debounceRef.current);
    debounceRef.current = window.setTimeout(() => setQuery(v), 250);
  };

  const toggleEnabled = async () => {
    if (!projectId || !settings || toggling || featureOff) return;
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
    const ok = await window.sixgates.revealMemoryExport(revealExportId);
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
              <span>
                启用记忆注入（每 Run 最多 {settings?.maxEntries ?? 8} 条 / {Math.round((settings?.maxBytes ?? 12288) / 1024)} KiB）
              </span>
            </label>
            {featureOff ? <StatusPill kind="readonly" label="功能由当前版本/管理员关闭" /> : null}
            {archivedProject ? <StatusPill kind="readonly" label="项目已归档：只读浏览" /> : null}
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

        {/* 参考稿布局：[项目▾ | N 条记忆] …… [搜索记忆文件…] */}
        <div className="sg-memory-projectbar">
          <label className="sg-memory-project">
            <select
              value={projectId ?? ''}
              aria-label="选择项目"
              onChange={(e) => {
                setDrawer(null);
                setProjectId(e.target.value || null);
              }}
            >
              {projects.length === 0 ? <option value="">（暂无项目）</option> : null}
              {projects.map((p) => (
                <option key={p.id} value={p.id} disabled={p.archivedAt != null}>
                  {p.name}
                  {p.archivedAt ? '（已归档）' : ''}
                </option>
              ))}
            </select>
          </label>
          <span className="sg-memory-total" aria-label="记忆总数">
            {total} 条记忆
          </span>
          <input
            type="search"
            className="sg-memory-search"
            aria-label="搜索记忆文件"
            placeholder="搜索记忆文件…"
            value={queryInput}
            onChange={(e) => onQueryChange(e.target.value)}
          />
        </div>

        <SettingsSection
          title="文件"
          actions={
            <div className="sg-memory-toolbar">
              <button
                type="button"
                className="sg-btn"
                aria-label="新建记忆"
                onClick={() => setDrawer({ id: null, create: true })}
                disabled={!projectId || archivedProject}
              >
                新建记忆
              </button>
              <button
                type="button"
                className="sg-btn"
                onClick={() => void doImport('proposed')}
                disabled={!projectId || archivedProject}
              >
                导入 Markdown
              </button>
              <button type="button" className="sg-btn" onClick={() => void doExport(false)} disabled={!projectId}>
                导出
              </button>
              <button
                type="button"
                className="sg-btn"
                aria-label="刷新记忆列表"
                onClick={() => projectId && void loadList(projectId, query)}
              >
                <IconRefresh size={14} />
              </button>
            </div>
          }
        >
          {projectId ? (
            <MemoryCandidates
              projectId={projectId}
              refreshKey={refreshKey}
              onChanged={() => {
                setRefreshKey((k) => k + 1);
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
              <button
                type="button"
                className="sg-btn"
                onClick={() => projectId && void loadList(projectId, query)}
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
    </>
  );
}
