// MCP 服务器（S24，ADR-035 受控 ToolProvider）：注册→探针→候选→批准→活跃；
// 开关只摘/挂活跃集（enabled），撤销保留审计行、调用明确失败。行式布局基线见 SettingsRow。
import { useCallback, useEffect, useMemo, useState } from 'react';
import { rpc } from '../../../rpc/client';
import { SettingsRow, SettingsToggle } from '../components/SettingsRow';
import { useTwoStepConfirm } from '../components/useTwoStepConfirm';
import {
  IconArrowLeft,
  IconMore,
  IconPlus,
  IconRefresh,
  IconSearch,
  IconServer,
} from '../../../components/Icons';

type McpStatus = 'candidate' | 'active' | 'revoked' | 'probe_failed';

interface McpServer {
  serverId: string;
  name: string;
  transport: string;
  command: string;
  args: string[];
  status: McpStatus;
  probeError: string;
  enabled: boolean;
  toolCounts: { active: number; candidate: number };
}

/** 旧版内核（未重编 release 二进制）不带 enabled/toolCounts：补默认值，避免渲染崩溃白屏。 */
function normalizeServer(raw: McpServer): McpServer {
  return {
    ...raw,
    args: Array.isArray(raw.args) ? raw.args : [],
    enabled: raw.enabled ?? true,
    toolCounts: raw.toolCounts ?? { active: 0, candidate: 0 },
  };
}

const GROUPS: Array<{ key: McpStatus; title: string; description: string }> = [
  {
    key: 'active',
    title: '已安装',
    description: '已批准的受控服务器；开关停用后其工具不再进入 Agent 活跃集。',
  },
  {
    key: 'candidate',
    title: '待批准',
    description: '探针成功但尚未批准；批准前工具对 Agent 不可见。',
  },
  {
    key: 'probe_failed',
    title: '探针失败',
    description: 'initialize/tools/list 未通过；修复命令后可重试探针。',
  },
  {
    key: 'revoked',
    title: '已撤销',
    description: '撤销即禁用并保留审计行；相关工具调用明确失败，不换工具。',
  },
];

const NAME_RE = /^[A-Za-z0-9_-]+$/;

function stateLine(s: McpServer): string {
  switch (s.status) {
    case 'active':
      return s.enabled
        ? `已连接并可用 · ${s.toolCounts.active} 个工具`
        : `已停用 · ${s.toolCounts.active} 个工具暂不可用`;
    case 'candidate':
      return `探针成功 · ${s.toolCounts.candidate} 个候选工具待批准`;
    case 'probe_failed':
      return s.probeError || '探针失败';
    case 'revoked':
      return '已撤销：工具调用明确失败';
  }
}

export function McpPage() {
  const [items, setItems] = useState<McpServer[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState('');
  const [showAdd, setShowAdd] = useState(false);
  const [form, setForm] = useState({ name: '', command: '', args: '' });
  const [createMode, setCreateMode] = useState<'form' | 'json'>('form');
  const [jsonDraft, setJsonDraft] = useState('');
  const [busy, setBusy] = useState<string | null>(null);
  const [pendingRemove, requestRemove] = useTwoStepConfirm();
  // RDWS-006 Windows 产品负例：入口隐藏（平台事实来自 preload，非 UA 猜测）。
  const mcpUnsupported = window.ratiflow.platform?.() === 'win32';

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await rpc<{ items: McpServer[] }>('mcp.serverList', {});
      setItems((res.items ?? []).map(normalizeServer));
    } catch (e) {
      setError(e instanceof Error ? e.message : 'MCP 服务器列表加载失败');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return items;
    return items.filter(
      (s) =>
        s.name.toLowerCase().includes(q) ||
        `${s.command} ${s.args.join(' ')}`.toLowerCase().includes(q),
    );
  }, [items, query]);

  const grouped = useMemo(() => {
    return GROUPS.map((g) => ({
      ...g,
      servers: filtered.filter((s) => s.status === g.key),
    })).filter((g) => g.servers.length > 0);
  }, [filtered]);

  const act = async (id: string, run: () => Promise<unknown>) => {
    setError(null);
    setBusy(id);
    try {
      await run();
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : '操作失败');
    } finally {
      setBusy(null);
    }
  };

  const toggle = (s: McpServer, enabled: boolean) =>
    act(
      `${s.serverId}:toggle`,
      () => rpc('mcp.serverToggle', { serverId: s.serverId, enabled }),

    );

  const approve = (s: McpServer) =>
    act(
      `${s.serverId}:approve`,
      () => rpc('mcp.serverApprove', { serverId: s.serverId, decidedBy: 'local' }),

    );

  const reprobe = (s: McpServer) =>
    act(
      `${s.serverId}:refresh`,
      () => rpc('mcp.serverRefresh', { serverId: s.serverId }),

    );

  const remove = (s: McpServer) =>
    act(
      `${s.serverId}:remove`,
      () =>
        rpc('mcp.serverRemove', {
          serverId: s.serverId,
          decidedBy: 'local',
          reason: 'settings-ui',
        }),

    );

  const submitAdd = async () => {
    let next = { ...form };
    if (createMode === 'json') {
      try {
        const parsed = JSON.parse(jsonDraft) as Record<string, unknown>;
        const root = (parsed.mcpServers && typeof parsed.mcpServers === 'object')
          ? parsed.mcpServers as Record<string, unknown>
          : parsed;
        const first = Object.entries(root)[0];
        const config = first?.[1] as Record<string, unknown> | undefined;
        next = {
          name: first?.[0] ?? '',
          command: typeof config?.command === 'string' ? config.command : '',
          args: Array.isArray(config?.args) ? config.args.map(String).join(' ') : '',
        };
      } catch {
        setError('JSON 格式无效，请检查后重试');
        return;
      }
    }
    const name = next.name.trim();
    const command = next.command.trim();
    const args = next.args.trim().split(/\s+/).filter(Boolean);
    setError(null);
    if (!NAME_RE.test(name)) {
      setError('名称仅允许字母、数字、下划线与连字符');
      return;
    }
    if (!command) {
      setError('启动命令必填');
      return;
    }
    setBusy('add');
    try {
      const res = await rpc<McpServer>('mcp.serverAdd', { name, command, args });
      const probeFailed = res?.status === 'probe_failed';
      const probeError = probeFailed ? res.probeError : '';
      setForm({ name: '', command: '', args: '' });
      setShowAdd(false);
      // 先刷新再报结果：load() 会清 error，横幅必须放在其后。
      await load();
      if (probeFailed) {
        setError(`「${name}」注册成功但探针失败：${probeError || '未知原因'}`);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : '注册失败');
    } finally {
      setBusy(null);
    }
  };

  const openCreate = () => {
    const empty = { name: '', command: '', args: '' };
    setForm(empty);
    setCreateMode('form');
    setJsonDraft(JSON.stringify({
      'server-name': { type: 'stdio', command: '', args: [] },
    }, null, 2));
    setError(null);
    setShowAdd(true);
  };

  const switchCreateMode = (mode: 'form' | 'json') => {
    if (mode === 'json') {
      setJsonDraft(JSON.stringify({
        [form.name.trim() || 'server-name']: {
          type: 'stdio',
          command: form.command,
          args: form.args.trim().split(/\s+/).filter(Boolean),
        },
      }, null, 2));
    }
    setCreateMode(mode);
  };

  const renderRow = (s: McpServer) => {
    const removing = pendingRemove === s.serverId;
    return (
      <div className="sg-mcp-row" key={s.serverId} title={`${s.transport} · ${[s.command, ...s.args].join(' ')}`}>
        <span className="sg-mcp-server-icon">
          <IconServer size={16} />
          <i className={`sg-mcp-status-dot sg-mcp-status-dot--${s.status}`} />
        </span>
        <div className="sg-mcp-row-copy">
          <strong>{s.name}</strong>
          <span>{stateLine(s)}</span>
        </div>
        <div className="sg-mcp-row-actions">
          {s.status === 'active' ? (
            <SettingsToggle
              label={`启用 ${s.name}`}
              checked={s.enabled}
              onChange={(checked) => void toggle(s, checked)}
            />
          ) : null}
          {s.status === 'candidate' ? (
            <button
              className="sg-btn sg-btn--primary"
              disabled={busy !== null}
              onClick={() => void approve(s)}
            >
              批准
            </button>
          ) : null}
          {s.status === 'probe_failed' ? (
            <button
              className="sg-btn"
              disabled={busy !== null}
              onClick={() => void reprobe(s)}
            >
              重试探针
            </button>
          ) : null}
          {s.status === 'active' || s.status === 'candidate' || s.status === 'probe_failed' ? (
            <button
              className={`sg-btn${removing ? ' sg-btn--danger' : ''}`}
              disabled={busy !== null}
              onClick={() => requestRemove(s.serverId, () => void remove(s))}
            >
              {removing ? '确认撤销' : '撤销'}
            </button>
          ) : null}
        </div>
      </div>
    );
  };

  if (showAdd && !mcpUnsupported) {
    return (
      <div className="sg-set-page sg-reference-page sg-mcp-create-page">
        <button className="sg-reference-breadcrumb" type="button" onClick={() => setShowAdd(false)}>
          <IconArrowLeft size={13} /> MCP 服务器
        </button>
        <div className="sg-mcp-create-head">
          <div>
            <h1>新建 MCP 服务器</h1>
            <p>填写新的 MCP 配置，保存后返回列表并自动执行连接探针。</p>
          </div>
          <div className="sg-reference-segments" role="tablist" aria-label="配置编辑模式">
            <button type="button" role="tab" aria-selected={createMode === 'form'} className={createMode === 'form' ? 'is-active' : ''} onClick={() => switchCreateMode('form')}>表单</button>
            <button type="button" role="tab" aria-selected={createMode === 'json'} className={createMode === 'json' ? 'is-active' : ''} onClick={() => switchCreateMode('json')}>JSON</button>
          </div>
        </div>

        {error ? <div className="sg-banner sg-banner--error" role="alert">操作失败：{error}</div> : null}

        <section className="sg-mcp-create-card" aria-label="新建 MCP 服务器配置">
          <div className="sg-mcp-create-scope">
            <span>作用域</span>
            <select aria-label="MCP 作用域" value="global" disabled><option value="global">全局</option></select>
          </div>
          {createMode === 'form' ? (
            <div className="sg-setting-list sg-mcp-create-form">
              <SettingsRow title="名称" htmlFor="sg-mcp-name" description="仅字母、数字、下划线与连字符">
                <input id="sg-mcp-name" className="sg-input" value={form.name} onChange={(event) => setForm((current) => ({ ...current, name: event.target.value }))} placeholder="my-mcp-server" />
              </SettingsRow>
              <SettingsRow title="类型" description="当前版本支持本地 stdio 服务器">
                <select className="sg-select" aria-label="MCP 类型" value="stdio" disabled><option value="stdio">stdio（本地命令）</option></select>
              </SettingsRow>
              <SettingsRow title="超时时间" description="由系统按操作类型设置安全上限">
                <input className="sg-input" aria-label="MCP 超时时间" value="系统默认" readOnly />
              </SettingsRow>
              <SettingsRow title="协议版本" description="连接时与服务器自动协商">
                <select className="sg-select" aria-label="MCP 协议版本" value="auto" disabled><option value="auto">自动（推荐）</option></select>
              </SettingsRow>
              <SettingsRow title="启动命令" htmlFor="sg-mcp-command" description="用于拉起 MCP 服务器进程">
                <input id="sg-mcp-command" className="sg-input" value={form.command} onChange={(event) => setForm((current) => ({ ...current, command: event.target.value }))} placeholder="npx" />
              </SettingsRow>
              <SettingsRow title="启动参数" htmlFor="sg-mcp-args" description="以空格分隔">
                <input id="sg-mcp-args" className="sg-input" value={form.args} onChange={(event) => setForm((current) => ({ ...current, args: event.target.value }))} placeholder="-y @modelcontextprotocol/server-memory" />
              </SettingsRow>
            </div>
          ) : (
            <div className="sg-mcp-json-editor">
              <label htmlFor="sg-mcp-json">完整配置</label>
              <textarea id="sg-mcp-json" value={jsonDraft} onChange={(event) => setJsonDraft(event.target.value)} spellCheck={false} />
              <p>支持直接粘贴单个服务器对象，或使用 <code>mcpServers</code> 包装。</p>
            </div>
          )}
          <div className="sg-mcp-create-actions">
            <button className="sg-btn" type="button" onClick={() => setShowAdd(false)} disabled={busy !== null}>取消</button>
            <button className="sg-btn sg-mcp-save" type="button" onClick={() => void submitAdd()} disabled={busy !== null}>{busy === 'add' ? '保存中…' : '保存'}</button>
          </div>
        </section>
      </div>
    );
  }

  return (
    <div className="sg-set-page sg-reference-page sg-mcp-page">
      <header className="sg-reference-page-head"><h1>MCP 服务器</h1></header>

      {mcpUnsupported ? (
        <>
          <div className="sg-banner sg-banner--warn" role="alert" data-testid="mcp-unsupported-banner">
            当前平台（Windows）不支持 MCP 沙箱：入口已关闭，注册 RPC 也会被核心拒绝（fail-closed）。
          </div>
          <div className="sg-reference-empty">
            <IconServer size={24} />
            <strong>此平台不可用</strong>
            <span>MCP 服务器管理需要 macOS（Seatbelt）或 Linux（Landlock）沙箱支持。</span>
          </div>
        </>
      ) : (
        <>
          <div className="sg-reference-toolbar sg-mcp-toolbar">
            <div className="sg-reference-toolbar-start">
              <label className="sg-reference-scope">
                <select aria-label="MCP 作用域" value="global" disabled><option value="global">全局</option></select>
              </label>
              <span className="sg-reference-divider" aria-hidden />
              <span className="sg-reference-count">MCP {filtered.length}</span>
            </div>
            <div className="sg-reference-toolbar-end">
              <label className="sg-reference-search">
                <IconSearch size={14} />
                <input type="search" placeholder="搜索 MCP 服务器…" aria-label="搜索 MCP 服务器" value={query} onChange={(event) => setQuery(event.target.value)} />
              </label>
              <button className="sg-reference-icon-btn" type="button" aria-label="更多 MCP 操作"><IconMore size={16} /></button>
              <button className="sg-reference-icon-btn" type="button" aria-label="刷新 MCP 服务器" onClick={() => void load()} disabled={loading}><IconRefresh size={15} /></button>
              <button className="sg-mcp-new" type="button" onClick={openCreate}><IconPlus size={14} />新建</button>
            </div>
          </div>

      {error ? <div className="sg-banner sg-banner--error" role="alert">操作失败：{error}</div> : null}

      {loading && items.length === 0 ? (
        <div className="sg-mcp-list" aria-busy="true">
          <div className="sg-skeleton-rows" aria-busy="true">
            <div className="sg-skeleton-row" />
            <div className="sg-skeleton-row" />
          </div>
        </div>
      ) : grouped.length === 0 ? (
        <div className="sg-reference-empty"><IconServer size={24} /><strong>暂无 MCP 服务器</strong><span>点击“新建”注册第一个受控 MCP 服务器。</span></div>
      ) : (
        grouped.map((g) => (
          <section className="sg-mcp-group" key={g.key}>
            <h2>{g.title} <span>{g.servers.length}</span></h2>
            <div className="sg-mcp-list">{g.servers.map(renderRow)}</div>
          </section>
        ))
      )}
        </>
      )}
    </div>
  );
}
