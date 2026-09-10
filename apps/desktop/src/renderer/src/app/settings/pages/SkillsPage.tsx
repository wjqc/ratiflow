// S25 技能页：用户级 Agent 指令包——启用即注入 Run 提示词。
// 列表（名称+描述在左、启停开关+删除在右）+ 搜索 + 新建/导入 + 抽屉编辑正文。
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { rpc, rpcErrorMessage } from '../../../rpc/client';
import { SettingsToggle } from '../components/SettingsRow';
import { IconMore, IconPlus, IconRefresh, IconSearch } from '../../../components/Icons';

interface SkillInfo {
  id: string;
  name: string;
  description: string;
  bodyBytes: number;
  enabled: boolean;
  source: string;
  agentProfileId: string | null;
  agentName: string | null;
  revision: number;
  createdAt: string;
  updatedAt: string;
}

interface AgentLite {
  id: string;
  name: string;
  enabled: boolean;
  versions?: Array<{ id: string; version_no: number; status?: string }>;
}

interface SkillVersion {
  id: string;
  skill_id: string;
  version_no: number;
  status: string;
  body_bytes: number;
  description: string;
  content_digest: string;
  created_at: string;
}

interface MarketSkillEntry {
  name: string;
  dirName: string;
  description: string;
  plugin: string;
  version: string;
}

// 市场源（可配置）：zcode_local = 本机插件市场目录；remote_git = 远程 https 市场仓库；
// remote_url = 远程 https 清单地址（marketplace.json，插件按 zip+sha256 下载校验）。
interface MarketPluginEntry {
  name: string;
  description: string;
  version: string;
  category: string;
}

interface MarketSourceBrowse {
  id: string;
  name: string;
  kind: string;
  rootPath: string;
  marketplaceId: string;
  enabled: boolean;
  revision: number;
  resolvedRoot: string;
  marketplaceName: string;
  description: string;
  pluginCount: number;
  skills: MarketSkillEntry[];
  plugins: MarketPluginEntry[];
  error: string;
}

// 市场技能本地缓存（localStorage，仅存本设备）：打开 tab 立即出上次的技能列表，
// 后台刷新完成后回写。存扁平条目 + 来源 id/名（安装开关按 sourceId 走 marketImport）。
interface MarketCacheEntry {
  name: string;
  dirName: string;
  description: string;
  plugin: string;
  version: string;
  sourceId: string;
  sourceName: string;
}

interface MarketCache {
  cachedAt: number;
  items: MarketCacheEntry[];
}

const MARKET_CACHE_KEY = 'ratiflow.skill.market.v1';
const MARKET_CACHE_MAX = 500;

function loadMarketCache(): MarketCache {
  try {
    const raw = localStorage.getItem(MARKET_CACHE_KEY);
    if (!raw) return { cachedAt: 0, items: [] };
    const parsed = JSON.parse(raw) as MarketCache;
    if (!Array.isArray(parsed.items)) return { cachedAt: 0, items: [] };
    const items = parsed.items.filter(
      (it) =>
        it &&
        typeof it.name === 'string' &&
        typeof it.dirName === 'string' &&
        typeof it.sourceId === 'string',
    );
    return { cachedAt: typeof parsed.cachedAt === 'number' ? parsed.cachedAt : 0, items };
  } catch {
    return { cachedAt: 0, items: [] };
  }
}

/** 缓存回填用的源占位（安装只消费 id/name，其余字段不参与）。 */
function cachedSourceStub(sourceId: string, sourceName: string): MarketSourceBrowse {
  return {
    id: sourceId,
    name: sourceName,
    kind: 'remote_url',
    rootPath: '',
    marketplaceId: '',
    enabled: true,
    revision: 0,
    resolvedRoot: '',
    marketplaceName: '',
    description: '',
    pluginCount: 0,
    skills: [],
    plugins: [],
    error: '',
  };
}

const NAME_RE = /^[A-Za-z0-9_-]+$/;

/** 从导入的 Markdown 提取描述：frontmatter description 优先，否则取首个非空段落。 */
function extractDescription(md: string): string {
  const fm = md.match(/^---\n([\s\S]*?)\n---/);
  if (fm) {
    const m = fm[1].match(/^description:\s*(.+)$/m);
    if (m) return m[1].trim().slice(0, 200);
  }
  const para = md
    .split(/\n{2,}/)
    .map((s) => s.replace(/^#.*\n?/, '').trim())
    .find((s) => s.length > 0);
  return (para ?? '').slice(0, 200);
}

export function SkillsPage() {
  const [items, setItems] = useState<SkillInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [agents, setAgents] = useState<AgentLite[]>([]);
  const [queryInput, setQueryInput] = useState('');
  const [query, setQuery] = useState('');
  const [busyId, setBusyId] = useState('');
  const [actionsOpen, setActionsOpen] = useState(false);
  const [showCreate, setShowCreate] = useState(false);
  // 从目录导入（开发规范技能化）：path 必填；subdirs 逗号分隔可选过滤。
  const [dirImport, setDirImport] = useState<{ open: boolean; path: string; subdirs: string; busy: boolean }>({ open: false, path: '', subdirs: '', busy: false });
  const [form, setForm] = useState<{ name: string; description: string; body: string; agentProfileId: string | null }>({ name: '', description: '', body: '', agentProfileId: null });
  const [editing, setEditing] = useState<SkillInfo | null>(null);
  const [editBody, setEditBody] = useState('');
  // 已安装 / 市场 两个分区。
  const [tab, setTab] = useState<'installed' | 'versions' | 'market'>('installed');
  const [versionSkillId, setVersionSkillId] = useState('');
  const [versions, setVersions] = useState<SkillVersion[]>([]);
  const [versionBody, setVersionBody] = useState('');
  const [versionDescription, setVersionDescription] = useState('');
  const [profileVersionId, setProfileVersionId] = useState('');
  const [marketOpen, setMarketOpen] = useState(false);
  const [markets, setMarkets] = useState<MarketSourceBrowse[] | null>(null);
  const [marketErr, setMarketErr] = useState('');
  const [marketNotice, setMarketNotice] = useState('');
  const [marketBusy, setMarketBusy] = useState('');
  const [srcForm, setSrcForm] = useState<{ id: string | null; kind: string; name: string; rootPath: string; marketplaceId: string; revision: number | null } | null>(null);
  // 远程清单插件按需拉取的技能：key = `${sourceId}/${plugin}`。
  const [pluginSkills, setPluginSkills] = useState<Record<string, { loading: boolean; error: string; items: MarketSkillEntry[] }>>({});
  const pluginSkillsRef = useRef(pluginSkills);
  pluginSkillsRef.current = pluginSkills;
  // 进行中的插件拉取 key（防并发重复拉取）。
  const pendingPluginRef = useRef<Set<string>>(new Set());
  // 市场扁平清单：搜索词 + 分页（默认 10 条，「更多」每次加 20）。
  const [marketQuery, setMarketQuery] = useState('');
  const [marketLimit, setMarketLimit] = useState(10);
  // 上次拉取成功的本地缓存（打开 tab 即显示，后台刷新后更新）。
  const [marketCache] = useState<MarketCache>(loadMarketCache);
  const debounceRef = useRef<number | null>(null);

  const load = useCallback(async () => {
    setError(null);
    try {
      const res = await rpc<{ items: SkillInfo[] }>('skill.list', {});
      setItems(res.items ?? []);
    } catch (e) {
      setError(rpcErrorMessage(e) || '技能列表加载失败');
    }
  }, []);
  useEffect(() => { void load(); }, [load]);

  useEffect(() => {
    if (!versionSkillId && items?.[0]) setVersionSkillId(items[0].id);
  }, [items, versionSkillId]);

  const loadVersions = useCallback(async (skillId: string) => {
    if (!skillId) return setVersions([]);
    try {
      const result = await rpc<{ items: SkillVersion[] }>('skill.versionList', { skillId });
      setVersions(result.items ?? []);
      setError(null);
    } catch (cause) {
      setVersions([]);
      setError(rpcErrorMessage(cause) || '版本列表加载失败');
    }
  }, []);

  useEffect(() => {
    if (tab === 'versions') void loadVersions(versionSkillId);
  }, [loadVersions, tab, versionSkillId]);

  // Agent 列表（绑定选择器数据源；全局 + 各 Agent）。
  useEffect(() => {
    rpc<{ items: AgentLite[] }>('agentProfile.list', {})
      .then((res) => setAgents((res.items ?? []).filter((a) => a.enabled)))
      .catch(() => setAgents([]));
  }, []);

  const onQueryChange = (v: string) => {
    setQueryInput(v);
    if (debounceRef.current !== null) window.clearTimeout(debounceRef.current);
    debounceRef.current = window.setTimeout(() => setQuery(v), 250);
  };

  const filtered = useMemo(() => {
    const list = items ?? [];
    const q = query.trim().toLowerCase();
    if (!q) return list;
    return list.filter(
      (s) => s.name.toLowerCase().includes(q) || s.description.toLowerCase().includes(q),
    );
  }, [items, query]);

  const toggle = async (s: SkillInfo) => {
    setBusyId(s.id);
    setError(null);
    try {
      const updated = await rpc<SkillInfo>('skill.setEnabled', {
        skillId: s.id,
        enabled: !s.enabled,
        expectedRevision: s.revision,
      });
      setItems((cur) => (cur ?? []).map((x) => (x.id === updated.id ? updated : x)));
    } catch (e) {
      setError(rpcErrorMessage(e) || '操作失败');
      void load();
    } finally {
      setBusyId('');
    }
  };

  // 改绑范围：'' = 全局，否则 Agent id。显式传 agentProfileId（null=全局）。
  const bind = async (s: SkillInfo, agentId: string) => {
    setBusyId(s.id);
    setError(null);
    try {
      const updated = await rpc<SkillInfo>('skill.update', {
        skillId: s.id,
        agentProfileId: agentId === '' ? null : agentId,
        expectedRevision: s.revision,
      });
      setItems((cur) => (cur ?? []).map((x) => (x.id === updated.id ? updated : x)));
    } catch (e) {
      setError(rpcErrorMessage(e) || '绑定失败');
      void load();
    } finally {
      setBusyId('');
    }
  };

  const remove = async (s: SkillInfo) => {
    setBusyId(s.id);
    setError(null);
    try {
      // 两步确认：第一次点击仅提示。
      if (!window.confirm(`删除技能「${s.name}」？（不可撤销）`)) {
        setBusyId('');
        return;
      }
      await rpc('skill.remove', { skillId: s.id, expectedRevision: s.revision });
      setItems((cur) => (cur ?? []).filter((x) => x.id !== s.id));
    } catch (e) {
      setError(rpcErrorMessage(e) || '删除失败');
    } finally {
      setBusyId('');
    }
  };

  const openEdit = async (s: SkillInfo) => {
    setEditing(s);
    try {
      const res = await rpc<{ body: string }>('skill.body', { skillId: s.id });
      setEditBody(res.body ?? '');
    } catch {
      setEditBody('');
    }
  };

  const saveEdit = async () => {
    if (!editing) return;
    setBusyId(editing.id);
    setError(null);
    try {
      await rpc('skill.update', {
        skillId: editing.id,
        body: editBody,
        expectedRevision: editing.revision,
      });
      setEditing(null);
      await load();
    } catch (e) {
      setError(rpcErrorMessage(e) || '保存失败');
    } finally {
      setBusyId('');
    }
  };

  const submitCreate = async () => {
    setError(null);
    const name = form.name.trim();
    if (!NAME_RE.test(name)) {
      setError('名称仅允许字母、数字、下划线与连字符');
      return;
    }
    try {
      await rpc('skill.create', {
        name,
        description: form.description.trim(),
        body: form.body,
        source: 'manual',
        ...(form.agentProfileId ? { agentProfileId: form.agentProfileId } : {}),
      });
      setNotice(`技能「${name}」已创建并启用`);
      setForm({ name: '', description: '', body: '', agentProfileId: null });
      setShowCreate(false);
      await load();
    } catch (e) {
      setError(rpcErrorMessage(e) || '创建失败');
    }
  };

  const importFile = async () => {
    const picked = await window.ratiflow.selectFile();
    if (!picked) return;
    setError(null);
    try {
      const md = new TextDecoder().decode(
        Uint8Array.from(atob(picked.contentBase64), (c) => c.charCodeAt(0)),
      );
      const base = picked.filename.replace(/\.[^.]+$/, '').replace(/[^A-Za-z0-9_-]/g, '-') || 'imported-skill';
      const created = await rpc<SkillInfo>('skill.create', {
        name: base,
        description: extractDescription(md),
        body: md,
        source: 'import',
      });
      setNotice(`技能「${created.name}」已导入${created.enabled ? '并启用' : ''}`);
      await load();
    } catch (e) {
      setError(rpcErrorMessage(e) || '导入失败');
    }
  };

  // 从目录导入（开发规范技能化接入）：目录下每个 .md 一个 draft 技能；
  // 三态清单如实展示，失败文件不阻断其余。
  const importDirectory = async () => {
    const path = dirImport.path.trim();
    if (!path) return;
    setDirImport((s) => ({ ...s, busy: true }));
    setError(null);
    setNotice(null);
    try {
      const subdirs = dirImport.subdirs
        .split(/[,，]/)
        .map((s) => s.trim())
        .filter(Boolean);
      const out = await rpc<{
        imported: string[];
        updated: string[];
        skipped: string[];
        failed: Array<{ file: string; reason: string }>;
      }>('skill.importDirectory', { path, ...(subdirs.length ? { subdirs } : {}) });
      const summary = `目录导入完成：新增 ${out.imported.length} / 更新 ${out.updated.length} / 未变 ${out.skipped.length} / 失败 ${out.failed.length}`;
      setNotice(
        out.failed.length
          ? `${summary}——${out.failed.slice(0, 3).map((f) => `${f.file}（${f.reason}）`).join('；')}${out.failed.length > 3 ? ' 等' : ''}`
          : summary,
      );
      setDirImport((s) => ({ ...s, open: false, path: '', subdirs: '' }));
      await load();
    } catch (e) {
      setError(rpcErrorMessage(e) || '目录导入失败');
    } finally {
      setDirImport((s) => ({ ...s, busy: false }));
    }
  };

  // ---------------- 市场源（可配置：远程 Git 仓库 / 本机市场目录；只读 SKILL.md） ----------------

  const loadMarkets = useCallback(async () => {
    setMarketErr('');
    try {
      const res = await rpc<{ items: MarketSourceBrowse[] }>('skill.marketList', {});
      setMarkets(res.items ?? []);
      autoLoadPluginSkills(res.items ?? []);
    } catch (e) {
      setMarketErr(rpcErrorMessage(e) || '市场源加载失败');
      setMarkets([]);
    }
  }, []);

  // 「管理源」配置对话框（源的增删改/启停）。
  const openManager = () => {
    setMarketOpen(true);
    setSrcForm(null);
    setMarketNotice('');
    if (markets === null) void loadMarkets();
  };

  // 切到市场分区：首次进入拉取市场源（之后用手动刷新）。
  const openMarketTab = () => {
    setTab('market');
    setMarketNotice('');
    if (markets === null) void loadMarkets();
  };

  const versionAction = async (label: string, action: () => Promise<unknown>) => {
    setBusyId('version-action');
    setError(null);
    try {
      await action();
      setNotice(label);
      await loadVersions(versionSkillId);
    } catch (cause) {
      setError(rpcErrorMessage(cause) || '版本操作失败');
    } finally {
      setBusyId('');
    }
  };

  const saveSource = async () => {
    if (!srcForm) return;
    setMarketBusy('form');
    setMarketErr('');
    try {
      await rpc('skill.marketSourceSave', {
        ...(srcForm.id ? { sourceId: srcForm.id, expectedRevision: srcForm.revision } : {}),
        kind: srcForm.kind,
        name: srcForm.name.trim(),
        rootPath: srcForm.rootPath.trim(),
        marketplaceId: srcForm.marketplaceId.trim(),
        enabled: true,
      });
      setSrcForm(null);
      await loadMarkets();
    } catch (e) {
      setMarketErr(rpcErrorMessage(e) || '市场源保存失败');
    } finally {
      setMarketBusy('');
    }
  };

  // 远程清单插件：点开时拉取其技能列表（git pin 检出 / zip 下载校验，仅扫 SKILL.md）。
  const loadPluginSkills = async (sourceId: string, plugin: string) => {
    const key = `${sourceId}/${plugin}`;
    pendingPluginRef.current.add(key);
    setPluginSkills((cur) => ({ ...cur, [key]: { loading: true, error: '', items: [] } }));
    try {
      const res = await rpc<{ items: MarketSkillEntry[]; resolvedSha: string }>('skill.marketPluginSkills', { sourceId, plugin });
      setPluginSkills((cur) => ({ ...cur, [key]: { loading: false, error: '', items: res.items ?? [] } }));
    } catch (e) {
      setPluginSkills((cur) => ({ ...cur, [key]: { loading: false, error: rpcErrorMessage(e) || '拉取失败', items: [] } }));
    } finally {
      pendingPluginRef.current.delete(key);
    }
  };

  // 市场源加载后自动逐插件拉取技能列表（并发 3，渐进渲染）——
  // 让市场页默认直接展示全部可安装技能，不必逐个点「查看技能」。
  const autoLoadPluginSkills = (sources: MarketSourceBrowse[]) => {
    const tasks: Array<() => Promise<void>> = [];
    for (const src of sources) {
      if (!src.enabled || src.error || src.plugins.length === 0) continue;
      for (const p of src.plugins) {
        const key = `${src.id}/${p.name}`;
        if (pluginSkillsRef.current[key] || pendingPluginRef.current.has(key)) continue;
        pendingPluginRef.current.add(key);
        tasks.push(async () => {
          try {
            await loadPluginSkills(src.id, p.name);
          } finally {
            pendingPluginRef.current.delete(key);
          }
        });
      }
    }
    let idx = 0;
    void Promise.all(
      Array.from({ length: Math.min(3, tasks.length) }, async () => {
        while (idx < tasks.length) {
          const t = tasks[idx++];
          await t();
        }
      }),
    );
  };

  // 全源扁平技能清单：合并所有启用源的技能（本地直读 + 各插件已拉取列表），
  // 按技能名去重（同名技能安装状态本就按名判定），排序供搜索与分页。
  // 实时数据未到齐的部分用本地缓存补位：打开 tab 即有内容，刷新逐插件就位。
  const allMarketSkills = useMemo(() => {
    const map = new Map<string, { src: MarketSourceBrowse; entry: MarketSkillEntry }>();
    for (const src of markets ?? []) {
      if (!src.enabled || src.error) continue;
      const entries =
        src.plugins.length > 0
          ? src.plugins.flatMap((p) => pluginSkills[`${src.id}/${p.name}`]?.items ?? [])
          : src.skills;
      for (const entry of entries) {
        if (!map.has(entry.name)) map.set(entry.name, { src, entry });
      }
    }
    for (const it of marketCache.items) {
      if (!map.has(it.name)) {
        map.set(it.name, {
          src: cachedSourceStub(it.sourceId, it.sourceName),
          entry: { name: it.name, dirName: it.dirName, description: it.description, plugin: it.plugin, version: it.version },
        });
      }
    }
    return [...map.values()].sort((a, b) => a.entry.name.localeCompare(b.entry.name));
  }, [markets, pluginSkills, marketCache]);

  const filteredMarketSkills = useMemo(() => {
    const q = marketQuery.trim().toLowerCase();
    if (!q) return allMarketSkills;
    return allMarketSkills.filter(
      ({ entry }) =>
        entry.name.toLowerCase().includes(q) || entry.description.toLowerCase().includes(q),
    );
  }, [allMarketSkills, marketQuery]);

  // 各插件拉取状态汇总：加载中 / 失败清单（供整页状态行与一键重试）。
  const marketLoading = useMemo(
    () =>
      (markets ?? []).some((src) =>
        src.enabled && !src.error
          ? src.plugins.some((p) => pluginSkills[`${src.id}/${p.name}`]?.loading)
          : false,
      ),
    [markets, pluginSkills],
  );
  const marketFailed = useMemo(() => {
    const out: Array<{ srcId: string; plugin: string }> = [];
    for (const src of markets ?? []) {
      if (!src.enabled || src.error) continue;
      for (const p of src.plugins) {
        if (pluginSkills[`${src.id}/${p.name}`]?.error) out.push({ srcId: src.id, plugin: p.name });
      }
    }
    return out;
  }, [markets, pluginSkills]);

  // 刷新全部完成后回写本地缓存：下次打开即见列表（来源变更/下线技能随刷新自然更替）。
  useEffect(() => {
    if (markets === null || markets.length === 0 || marketLoading || allMarketSkills.length === 0) return;
    const items = allMarketSkills.slice(0, MARKET_CACHE_MAX).map(({ src, entry }) => ({
      name: entry.name,
      dirName: entry.dirName,
      description: entry.description,
      plugin: entry.plugin,
      version: entry.version,
      sourceId: src.id,
      sourceName: src.name,
    }));
    try {
      localStorage.setItem(MARKET_CACHE_KEY, JSON.stringify({ cachedAt: Date.now(), items }));
    } catch {
      // 存储配额/隐私模式失败不影响功能，仅无缓存可用。
    }
  }, [markets, marketLoading, allMarketSkills]);

  const toggleSource = async (src: MarketSourceBrowse) => {
    setMarketBusy(src.id);
    setMarketErr('');
    try {
      await rpc('skill.marketSourceSave', {
        sourceId: src.id,
        name: src.name,
        rootPath: src.rootPath,
        marketplaceId: src.marketplaceId,
        enabled: !src.enabled,
        expectedRevision: src.revision,
      });
      await loadMarkets();
    } catch (e) {
      setMarketErr(rpcErrorMessage(e) || '市场源更新失败');
    } finally {
      setMarketBusy('');
    }
  };

  const removeSource = async (src: MarketSourceBrowse) => {
    setMarketBusy(src.id);
    setMarketErr('');
    try {
      // 两步确认：第一次点击仅提示。
      if (!window.confirm(`删除市场源「${src.name}」？（不影响已导入的技能）`)) {
        setMarketBusy('');
        return;
      }
      await rpc('skill.marketSourceRemove', { sourceId: src.id, expectedRevision: src.revision });
      await loadMarkets();
    } catch (e) {
      setMarketErr(rpcErrorMessage(e) || '市场源删除失败');
    } finally {
      setMarketBusy('');
    }
  };

  // 市场技能开关：开 = 安装（marketImport），关 = 卸载（删除对应已安装技能，需确认）。
  const toggleMarketSkill = async (
    src: MarketSourceBrowse,
    entry: MarketSkillEntry,
    installedSkill: SkillInfo | null,
  ) => {
    setMarketBusy(`${src.id}/${entry.dirName}`);
    setMarketErr('');
    setMarketNotice('');
    try {
      if (!installedSkill) {
        const out = await rpc<{ skill: SkillInfo; versionId: string }>('skill.marketImport', {
          sourceId: src.id,
          plugin: entry.plugin,
          version: entry.version,
          skillName: entry.dirName,
        });
        setMarketNotice(`技能「${out.skill.name}」已安装（来源：${src.name}；版本 draft，待激活）`);
      } else {
        if (!window.confirm(`卸载技能「${installedSkill.name}」？（不可撤销）`)) {
          setMarketBusy('');
          return;
        }
        await rpc('skill.remove', {
          skillId: installedSkill.id,
          expectedRevision: installedSkill.revision,
        });
        setMarketNotice(`技能「${installedSkill.name}」已卸载`);
      }
      await load();
    } catch (e) {
      setMarketErr(rpcErrorMessage(e) || '操作失败');
      void load();
    } finally {
      setMarketBusy('');
    }
  };

  // 市场技能行（本地源技能列表与远程插件技能列表共用）：名称 | 描述 | 安装开关。
  const renderMarketSkillRow = (src: MarketSourceBrowse, entry: MarketSkillEntry) => {
    const installedSkill = (items ?? []).find((s) => s.name === entry.name) ?? null;
    const busy = marketBusy === `${src.id}/${entry.dirName}`;
    const key = `${src.id}/${entry.plugin}@${entry.version}/${entry.dirName}`;
    return (
      <div className="sg-mkt-skill" role="listitem" key={key}>
        <span className="sg-mkt-skill-name">{entry.name}</span>
        <small className="sg-mkt-skill-desc">{entry.description || '（无描述）'}</small>
        <div className="sg-mkt-skill-control">
          <SettingsToggle
            label={`安装技能 ${entry.name}`}
            checked={!!installedSkill}
            disabled={busy}
            onChange={() => void toggleMarketSkill(src, entry, installedSkill)}
          />
        </div>
      </div>
    );
  };

  return (
    <div className="sg-set-page sg-reference-page sg-memory-page">
      <header className="sg-reference-page-head">
        <h1>技能</h1>
      </header>

      <div className="sg-tabs" role="tablist" aria-label="技能分区">
        <button
          type="button"
          role="tab"
          aria-selected={tab === 'installed'}
          className={`sg-tab${tab === 'installed' ? ' sg-tab--active' : ''}`}
          onClick={() => setTab('installed')}
        >
          已安装<span className="sg-tab-count">{items?.length ?? 0}</span>
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={tab === 'versions'}
          className={`sg-tab${tab === 'versions' ? ' sg-tab--active' : ''}`}
          onClick={() => setTab('versions')}
        >
          版本治理
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={tab === 'market'}
          className={`sg-tab${tab === 'market' ? ' sg-tab--active' : ''}`}
          onClick={openMarketTab}
        >
          市场
        </button>
      </div>

      {tab === 'installed' ? (
        <>
      {error ? <div role="alert" className="sg-memory-banner sg-memory-banner--error">{error}</div> : null}
      {notice ? <div role="status" className="sg-memory-banner sg-memory-banner--ok">{notice}</div> : null}

      <div className="sg-reference-toolbar">
        <div className="sg-reference-toolbar-start">
          <span className="sg-reference-count" aria-label="技能总数">{filtered.length} 个技能</span>
        </div>
        <div className="sg-reference-toolbar-end">
          <label className="sg-reference-search">
            <IconSearch size={14} />
            <input
              type="search"
              aria-label="搜索技能"
              placeholder="搜索技能…"
              value={queryInput}
              onChange={(e) => onQueryChange(e.target.value)}
            />
          </label>
          <div className="sg-reference-more">
            <button
              type="button"
              className="sg-reference-icon-btn"
              aria-label="更多技能操作"
              aria-expanded={actionsOpen}
              onClick={() => setActionsOpen((open) => !open)}
            >
              <IconMore size={16} />
            </button>
            {actionsOpen ? (
              <div className="sg-reference-menu" role="menu">
                <button type="button" role="menuitem" onClick={() => { setActionsOpen(false); setShowCreate(true); }}>
                  <IconPlus size={14} />新建技能
                </button>
                <button type="button" role="menuitem" onClick={() => { setActionsOpen(false); void importFile(); }}>
                  导入 Markdown
                </button>
                <button type="button" role="menuitem" onClick={() => { setActionsOpen(false); setDirImport((s) => ({ ...s, open: true })); }}>
                  从目录导入
                </button>
                <button type="button" role="menuitem" onClick={() => { setActionsOpen(false); openManager(); }}>
                  管理市场源
                </button>
              </div>
            ) : null}
          </div>
          <button type="button" className="sg-reference-icon-btn" aria-label="刷新技能列表" onClick={() => void load()}>
            <IconRefresh size={15} />
          </button>
        </div>
      </div>

      {showCreate ? (
        <form
          className="sg-setting-list"
          onSubmit={(e) => {
            e.preventDefault();
            void submitCreate();
          }}
        >
          <div className="sg-set-item">
            <div className="sg-set-item-copy">
              <label htmlFor="skill-name" className="sg-set-item-title">名称 *（字母数字_-）</label>
            </div>
            <div className="sg-set-item-control">
              <input id="skill-name" className="sg-input" value={form.name}
                onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="deploy-check" />
            </div>
          </div>
          <div className="sg-set-item">
            <div className="sg-set-item-copy">
              <label htmlFor="skill-desc" className="sg-set-item-title">描述</label>
            </div>
            <div className="sg-set-item-control">
              <input id="skill-desc" className="sg-input" value={form.description}
                onChange={(e) => setForm({ ...form, description: e.target.value })} />
            </div>
          </div>
          <div className="sg-set-item">
            <div className="sg-set-item-copy">
              <label htmlFor="skill-agent" className="sg-set-item-title">生效范围</label>
              <small className="sg-set-item-desc">全局 = 注入所有 Run；绑定 Agent = 仅该 Agent 的 Run 注入</small>
            </div>
            <div className="sg-set-item-control">
              <select id="skill-agent" className="sg-select"
                value={form.agentProfileId ?? ''}
                onChange={(e) => setForm({ ...form, agentProfileId: e.target.value === '' ? null : e.target.value })}>
                <option value="">全局生效</option>
                {agents.map((a) => (
                  <option key={a.id} value={a.id}>绑定：{a.name}</option>
                ))}
              </select>
            </div>
          </div>
          <div className="sg-set-item" style={{ gridTemplateColumns: '1fr' }}>
            <div className="sg-set-item-copy">
              <label htmlFor="skill-body" className="sg-set-item-title">正文（Markdown）</label>
            </div>
            <textarea id="skill-body" className="sg-textarea" rows={8} value={form.body}
              onChange={(e) => setForm({ ...form, body: e.target.value })} />
          </div>
          <div className="sg-set-form-actions">
            <button type="submit" className="sg-btn sg-btn--primary" disabled={!form.name.trim() || !form.body.trim()}>
              创建并启用
            </button>
            <button type="button" className="sg-btn" onClick={() => setShowCreate(false)}>取消</button>
          </div>
        </form>
      ) : null}

      {dirImport.open ? (
        <form
          className="sg-setting-list"
          onSubmit={(e) => {
            e.preventDefault();
            void importDirectory();
          }}
        >
          <div className="sg-set-item">
            <div className="sg-set-item-copy">
              <label htmlFor="skill-dir-path" className="sg-set-item-title">目录绝对路径 *</label>
              <small className="sg-set-item-desc">目录下每个 .md 导入为一个 draft 技能（名称 = 相对路径转连字符）；激活后进注入面</small>
            </div>
            <div className="sg-set-item-control">
              <input id="skill-dir-path" className="sg-input" value={dirImport.path}
                onChange={(e) => setDirImport((s) => ({ ...s, path: e.target.value }))}
                placeholder="/Users/you/knowledge/agent-dev-spec" />
            </div>
          </div>
          <div className="sg-set-item">
            <div className="sg-set-item-copy">
              <label htmlFor="skill-dir-subdirs" className="sg-set-item-title">子目录过滤（可选）</label>
              <small className="sg-set-item-desc">逗号分隔的首级目录名；留空导入全部</small>
            </div>
            <div className="sg-set-item-control">
              <input id="skill-dir-subdirs" className="sg-input" value={dirImport.subdirs}
                onChange={(e) => setDirImport((s) => ({ ...s, subdirs: e.target.value }))}
                placeholder="standards, workflow, lessons" />
            </div>
          </div>
          <div className="sg-set-form-actions">
            <button type="submit" className="sg-btn sg-btn--primary" disabled={!dirImport.path.trim() || dirImport.busy}>
              {dirImport.busy ? '导入中…' : '导入目录'}
            </button>
            <button type="button" className="sg-btn" onClick={() => setDirImport({ open: false, path: '', subdirs: '', busy: false })}>取消</button>
          </div>
        </form>
      ) : null}

      <div className="sg-memory-files">
        {items === null ? (
          <div className="sg-skeleton-rows" aria-busy="true"><div className="sg-skeleton-row" /></div>
        ) : filtered.length === 0 ? (
          <div className="sg-empty">
            <span>{query ? '没有匹配的技能。' : '还没有技能。新建一条，或从 Markdown 导入。'}</span>
          </div>
        ) : (
          <div className="sg-setting-list" role="list" aria-label="技能列表">
            {filtered.map((s) => (
              <div className="sg-set-item" role="listitem" key={s.id}>
                <div className="sg-set-item-copy">
                  <button
                    type="button"
                    className="sg-set-item-title"
                    style={{ textAlign: 'left', background: 'none', border: 0, cursor: 'pointer', padding: 0 }}
                    onClick={() => void openEdit(s)}
                  >
                    {s.name}
                  </button>
                  <small className="sg-set-item-desc">
                    {s.description || '（无描述）'} · {s.source === 'import' ? '导入' : s.source === 'market' ? '市场' : '手动'}
                    {s.agentName ? ` · 仅 ${s.agentName} 生效` : ' · 全局生效'}
                  </small>
                </div>
                <div className="sg-set-item-control">
                  <select
                    className="sg-select"
                    style={{ marginRight: 10 }}
                    aria-label={`技能 ${s.name} 生效范围`}
                    value={s.agentProfileId ?? ''}
                    disabled={busyId === s.id}
                    onChange={(e) => void bind(s, e.target.value)}
                  >
                    <option value="">全局生效</option>
                    {agents.map((a) => (
                      <option key={a.id} value={a.id}>绑定：{a.name}</option>
                    ))}
                  </select>
                  <SettingsToggle
                    label={`启用技能 ${s.name}`}
                    checked={s.enabled}
                    disabled={busyId === s.id}
                    onChange={() => void toggle(s)}
                  />
                  <button
                    type="button"
                    className="sg-btn sg-btn--sm sg-btn--danger"
                    style={{ marginLeft: 10 }}
                    disabled={busyId === s.id}
                    onClick={() => void remove(s)}
                  >
                    删除
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
        </>
      ) : tab === 'versions' ? (
        <div style={{ display: 'grid', gap: 14, paddingTop: 12 }}>
          {error ? <div role="alert" className="sg-memory-banner sg-memory-banner--error">{error}</div> : null}
          {notice ? <div role="status" className="sg-memory-banner sg-memory-banner--ok">{notice}</div> : null}
          <div className="sg-setting-list">
            <div className="sg-set-item">
              <div className="sg-set-item-copy">
                <label className="sg-set-item-title" htmlFor="version-skill">技能</label>
                <small className="sg-set-item-desc">版本不可变；激活新版本会弃用同技能的旧活动版本。</small>
              </div>
              <div className="sg-set-item-control">
                <select id="version-skill" className="sg-select" value={versionSkillId} onChange={(e) => setVersionSkillId(e.target.value)}>
                  {(items ?? []).map((skill) => <option key={skill.id} value={skill.id}>{skill.name}</option>)}
                </select>
              </div>
            </div>
          </div>
          <div className="sg-setting-list" role="list" aria-label="技能版本列表">
            {versions.length === 0 ? <div className="sg-empty"><span>暂无不可变版本，可从当前正文创建第一版。</span></div> : versions.map((version) => (
              <div className="sg-set-item" role="listitem" key={version.id}>
                <div className="sg-set-item-copy">
                  <span className="sg-set-item-title">v{version.version_no} · {version.status}</span>
                  <small className="sg-set-item-desc">{version.description || '（无版本说明）'} · {version.body_bytes} bytes · {version.content_digest.slice(0, 12)}</small>
                </div>
                <div className="sg-set-item-control">
                  {version.status === 'draft' || version.status === 'deprecated' ? <button className="sg-btn sg-btn--sm sg-btn--primary" disabled={!!busyId} onClick={() => void versionAction('版本已激活', () => rpc('skill.activateVersion', { versionId: version.id }))}>激活</button> : null}
                  {version.status === 'active' ? <button className="sg-btn sg-btn--sm" disabled={!!busyId} onClick={() => void versionAction('版本已弃用', () => rpc('skill.deprecateVersion', { versionId: version.id }))}>弃用</button> : null}
                  {version.status !== 'revoked' ? <button className="sg-btn sg-btn--sm sg-btn--danger" style={{ marginLeft: 8 }} disabled={!!busyId} onClick={() => void versionAction('版本已撤销', () => rpc('skill.revokeVersion', { versionId: version.id }))}>撤销</button> : null}
                  <button className="sg-btn sg-btn--sm" style={{ marginLeft: 8 }} disabled={!!busyId || !profileVersionId} onClick={() => void versionAction('技能版本已绑定到 Agent 版本', () => rpc('skill.bindVersion', { skillVersionId: version.id, profileVersionId }))}>绑定</button>
                </div>
              </div>
            ))}
          </div>
          <div className="sg-setting-list">
            <div className="sg-set-item">
              <div className="sg-set-item-copy"><label className="sg-set-item-title" htmlFor="profile-version">绑定目标</label></div>
              <div className="sg-set-item-control">
                <select id="profile-version" className="sg-select" value={profileVersionId} onChange={(e) => setProfileVersionId(e.target.value)}>
                  <option value="">选择 Agent Profile 版本</option>
                  {agents.flatMap((agent) => (agent.versions ?? []).map((version) => <option key={version.id} value={version.id}>{agent.name} v{version.version_no}{version.status ? ` · ${version.status}` : ''}</option>))}
                </select>
              </div>
            </div>
            <div className="sg-set-item" style={{ gridTemplateColumns: '1fr' }}>
              <input className="sg-input" aria-label="版本说明" placeholder="版本说明" value={versionDescription} onChange={(e) => setVersionDescription(e.target.value)} />
              <textarea className="sg-textarea" rows={12} aria-label="版本正文" placeholder="不可变版本正文（Markdown）" value={versionBody} onChange={(e) => setVersionBody(e.target.value)} />
            </div>
            <div className="sg-set-form-actions">
              <button className="sg-btn sg-btn--primary" disabled={!!busyId || !versionSkillId || !versionBody.trim()} onClick={() => void versionAction('新版本草稿已创建', async () => {
                await rpc('skill.createVersion', { skillId: versionSkillId, body: versionBody, description: versionDescription.trim() });
                setVersionBody(''); setVersionDescription('');
              })}>创建版本草稿</button>
            </div>
          </div>
        </div>
      ) : (
        // ---------------- 市场 tab：市场源浏览 + 每技能安装开关 ----------------
        <>
          {marketErr ? <div role="alert" className="sg-memory-banner sg-memory-banner--error">{marketErr}</div> : null}
          {marketNotice ? <div role="status" className="sg-memory-banner sg-memory-banner--ok">{marketNotice}</div> : null}

          <div className="sg-reference-toolbar">
            <div className="sg-reference-toolbar-start">
              <label className="sg-reference-search">
                <IconSearch size={14} />
                <input
                  type="search"
                  aria-label="搜索市场技能"
                  placeholder="搜索技能名称或描述…"
                  value={marketQuery}
                  onChange={(e) => setMarketQuery(e.target.value)}
                />
              </label>
            </div>
            <div className="sg-reference-toolbar-end">
              <button
                type="button"
                className="sg-btn sg-btn--sm"
                style={{ marginRight: 8 }}
                onClick={() => setMarketOpen(true)}
              >
                管理源
              </button>
              <button type="button" className="sg-reference-icon-btn" aria-label="刷新市场源" onClick={() => void loadMarkets()}>
                <IconRefresh size={15} />
              </button>
            </div>
          </div>

          <div className="sg-memory-files">
            {markets === null && marketCache.items.length === 0 ? (
              <div className="sg-skeleton-rows" aria-busy="true"><div className="sg-skeleton-row" /></div>
            ) : markets !== null && markets.length === 0 && marketCache.items.length === 0 ? (
              <div className="sg-empty">
                <span>还没有市场源。点右上角「管理源」添加一个（远程清单、远程 Git 仓库或本机市场目录）。</span>
              </div>
            ) : (
              <>
                {filteredMarketSkills.length > 0 ? (
                  <div className="sg-mkt-skills" role="list" aria-label="可安装技能">
                    {filteredMarketSkills.slice(0, marketLimit).map(({ src, entry }) => renderMarketSkillRow(src, entry))}
                  </div>
                ) : null}
                {(markets === null || marketLoading) && marketCache.items.length > 0 ? (
                  <div className="sg-mkt-note">正在后台刷新…</div>
                ) : null}
                {marketLoading && marketCache.items.length === 0 ? (
                  <div className="sg-mkt-note">正在拉取技能列表…</div>
                ) : null}
                {!marketLoading && marketFailed.length > 0 ? (
                  <div className="sg-mkt-note">
                    {marketFailed.map((f) => f.plugin).join('、')} 拉取失败
                    <button
                      type="button"
                      className="sg-btn sg-btn--sm"
                      style={{ marginLeft: 12 }}
                      onClick={() => { for (const f of marketFailed) void loadPluginSkills(f.srcId, f.plugin); }}
                    >
                      重试
                    </button>
                  </div>
                ) : null}
                {!marketLoading && marketFailed.length === 0 && filteredMarketSkills.length === 0 ? (
                  <div className="sg-mkt-note">
                    {marketQuery.trim() ? `没有匹配「${marketQuery.trim()}」的技能。` : '暂无可安装技能。'}
                  </div>
                ) : null}
                {filteredMarketSkills.length > marketLimit ? (
                  <div className="sg-mkt-more">
                    <button type="button" className="sg-btn" onClick={() => setMarketLimit((n) => n + 20)}>
                      加载更多（还有 {filteredMarketSkills.length - marketLimit} 个）
                    </button>
                  </div>
                ) : null}
              </>
            )}
          </div>

        </>
      )}

      {editing ? (
        <div className="sg-drawer-backdrop" onClick={() => setEditing(null)}>
          <div
            className="sg-drawer"
            role="dialog"
            aria-label={`编辑技能 ${editing.name}`}
            onClick={(e) => e.stopPropagation()}
          >
            <div className="sg-drawer-head">
              <strong>编辑技能：{editing.name}</strong>
              <button className="sg-icon-btn" onClick={() => setEditing(null)} aria-label="关闭">✕</button>
            </div>
            <textarea className="sg-textarea" rows={16} value={editBody}
              onChange={(e) => setEditBody(e.target.value)} />
            <div className="sg-row" style={{ marginTop: 10 }}>
              <button className="sg-btn sg-btn--primary" disabled={busyId === editing.id} onClick={() => void saveEdit()}>
                保存
              </button>
              <button className="sg-btn" onClick={() => setEditing(null)}>取消</button>
            </div>
          </div>
        </div>
      ) : null}

      {marketOpen ? (
        <div className="sg-drawer-backdrop" onClick={() => setMarketOpen(false)}>
          <div
            className="sg-modal-card"
            role="dialog"
            aria-modal="true"
            aria-label="管理市场源"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="sg-drawer-head">
              <strong>管理市场源</strong>
              <button className="sg-icon-btn" onClick={() => setMarketOpen(false)} aria-label="关闭">✕</button>
            </div>

            <div className="sg-modal-body">
            {marketErr ? <div role="alert" className="sg-memory-banner sg-memory-banner--error">{marketErr}</div> : null}
            {marketNotice ? <div role="status" className="sg-memory-banner sg-memory-banner--ok">{marketNotice}</div> : null}

            {(markets ?? []).length === 0 ? (
              <div className="sg-empty"><span>还没有市场源。点下方「新增市场源」添加。</span></div>
            ) : (
              <div className="sg-setting-list" role="list" aria-label="市场源列表">
                {(markets ?? []).map((src) => (
                  <div className="sg-set-item" role="listitem" key={src.id}>
                    <div className="sg-set-item-copy">
                      <span className="sg-set-item-title">{src.name}</span>
                      <small className="sg-set-item-desc">
                        {src.kind === 'remote_git' ? '远程 Git' : src.kind === 'remote_url' ? '远程清单' : '本机目录'} · {src.rootPath}
                        {src.error ? ` · 不可用：${src.error}` : ''}
                      </small>
                    </div>
                    <div className="sg-set-item-control">
                      <button
                        type="button"
                        className="sg-btn sg-btn--sm"
                        style={{ marginRight: 8 }}
                        onClick={() => setSrcForm({ id: src.id, kind: src.kind, name: src.name, rootPath: src.rootPath, marketplaceId: src.marketplaceId, revision: src.revision })}
                      >
                        编辑
                      </button>
                      <button
                        type="button"
                        className="sg-btn sg-btn--sm sg-btn--danger"
                        style={{ marginRight: 8 }}
                        disabled={marketBusy === src.id}
                        onClick={() => void removeSource(src)}
                      >
                        删除
                      </button>
                      <SettingsToggle
                        label={`启用市场源 ${src.name}`}
                        checked={src.enabled}
                        disabled={marketBusy === src.id}
                        onChange={() => void toggleSource(src)}
                      />
                    </div>
                  </div>
                ))}
              </div>
            )}

            {srcForm ? (
                <form
                  className="sg-setting-list"
                  style={{ marginTop: 10 }}
                  onSubmit={(e) => {
                    e.preventDefault();
                    void saveSource();
                  }}
                >
                  <div className="sg-set-item">
                    <div className="sg-set-item-copy">
                      <label htmlFor="mkt-kind" className="sg-set-item-title">类型</label>
                      <small className="sg-set-item-desc">远程清单/远程 Git = 从公网在线拉取；本机市场目录 = 读取本机已安装插件</small>
                    </div>
                    <div className="sg-set-item-control">
                      <select id="mkt-kind" className="sg-select" value={srcForm.kind}
                        onChange={(e) => setSrcForm({ ...srcForm, kind: e.target.value })}>
                        <option value="remote_url">远程清单（https）</option>
                        <option value="remote_git">远程 Git 仓库</option>
                        <option value="zcode_local">本机市场目录</option>
                      </select>
                    </div>
                  </div>
                  <div className="sg-set-item">
                    <div className="sg-set-item-copy">
                      <label htmlFor="mkt-name" className="sg-set-item-title">名称 *</label>
                    </div>
                    <div className="sg-set-item-control">
                      <input id="mkt-name" className="sg-input" value={srcForm.name}
                        onChange={(e) => setSrcForm({ ...srcForm, name: e.target.value })} placeholder="官方插件市场" />
                    </div>
                  </div>
                  {srcForm.kind === 'remote_url' ? (
                    <div className="sg-set-item">
                      <div className="sg-set-item-copy">
                        <label htmlFor="mkt-root" className="sg-set-item-title">市场清单 URL *</label>
                        <small className="sg-set-item-desc">仅支持 https。指向 marketplace.json；插件按 zip 下载并做 sha256 校验，只提取其中 SKILL.md</small>
                      </div>
                      <div className="sg-set-item-control">
                        <input id="mkt-root" className="sg-input" value={srcForm.rootPath}
                          onChange={(e) => setSrcForm({ ...srcForm, rootPath: e.target.value })}
                          placeholder="https://cdn-zcode.z.ai/zcode/official-plugin/marketplace.json" />
                      </div>
                    </div>
                  ) : srcForm.kind === 'remote_git' ? (
                    <div className="sg-set-item">
                      <div className="sg-set-item-copy">
                        <label htmlFor="mkt-root" className="sg-set-item-title">市场仓库 URL *</label>
                        <small className="sg-set-item-desc">仅支持 https。含 marketplace.json 清单则按插件导入；否则扫描仓库内全部 SKILL.md</small>
                      </div>
                      <div className="sg-set-item-control">
                        <input id="mkt-root" className="sg-input" value={srcForm.rootPath}
                          onChange={(e) => setSrcForm({ ...srcForm, rootPath: e.target.value })}
                          placeholder="https://github.com/anthropics/claude-plugins-official" />
                      </div>
                    </div>
                  ) : (
                    <div className="sg-set-item">
                      <div className="sg-set-item-copy">
                        <label htmlFor="mkt-root" className="sg-set-item-title">根目录 *</label>
                        <small className="sg-set-item-desc">本机插件市场目录（支持 ~ 展开），须含 marketplaces/、installed_plugins.json 或 cache/</small>
                      </div>
                      <div className="sg-set-item-control">
                        <input id="mkt-root" className="sg-input" value={srcForm.rootPath}
                          onChange={(e) => setSrcForm({ ...srcForm, rootPath: e.target.value })} placeholder="~/.zcode/cli/plugins" />
                      </div>
                    </div>
                  )}
                  {srcForm.kind === 'zcode_local' ? (
                    <div className="sg-set-item">
                      <div className="sg-set-item-copy">
                        <label htmlFor="mkt-id" className="sg-set-item-title">市场 ID</label>
                        <small className="sg-set-item-desc">留空 = 取该目录市场清单中的第一个</small>
                      </div>
                      <div className="sg-set-item-control">
                        <input id="mkt-id" className="sg-input" value={srcForm.marketplaceId}
                          onChange={(e) => setSrcForm({ ...srcForm, marketplaceId: e.target.value })} placeholder="official-plugins" />
                      </div>
                    </div>
                  ) : null}
                  <div className="sg-set-form-actions">
                    <button type="submit" className="sg-btn sg-btn--primary" disabled={!srcForm.name.trim() || !srcForm.rootPath.trim() || marketBusy === 'form'}>
                      保存
                    </button>
                    <button type="button" className="sg-btn" onClick={() => setSrcForm(null)}>取消</button>
                  </div>
                </form>
              ) : (
                <div style={{ marginTop: 10 }}>
                  <button type="button" className="sg-btn" onClick={() => setSrcForm({ id: null, kind: 'remote_url', name: '', rootPath: 'https://cdn-zcode.z.ai/zcode/official-plugin/marketplace.json', marketplaceId: '', revision: null })}>
                    <IconPlus size={14} /> 新增市场源
                  </button>
                </div>
              )}

            <p className="sg-set-item-desc" style={{ marginTop: 12 }}>
              市场源支持三种形态：远程清单（https 指向 marketplace.json，插件按 zip 下载并做 sha256 校验）、
              远程 Git 仓库（仅 https，保存时预检可达，导入按清单 pin SHA/引用拉取）与本机插件市场目录
              （须含 marketplaces/、installed_plugins.json 或 cache/ 之一）。
              三者都只提取其中 SKILL.md 文本（经秘密扫描），绝不执行市场内任何文件；
              导入后为 draft 版本，可在版本管理中激活。
            </p>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
