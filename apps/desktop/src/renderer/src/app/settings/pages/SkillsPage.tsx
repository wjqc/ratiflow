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
  const [form, setForm] = useState<{ name: string; description: string; body: string; agentProfileId: string | null }>({ name: '', description: '', body: '', agentProfileId: null });
  const [editing, setEditing] = useState<SkillInfo | null>(null);
  const [editBody, setEditBody] = useState('');
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
    const picked = await window.sixgates.selectFile();
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

  return (
    <div className="sg-set-page sg-reference-page sg-memory-page">
      <header className="sg-reference-page-head">
        <h1>技能</h1>
      </header>

      <section className="sg-memory-master" aria-label="技能说明">
        <div>
          <strong>Agent 技能</strong>
          <p>启用的技能会在 Agent 执行时注入提示词，用于固化工作方式与检查清单。总注入预算 32KB。</p>
        </div>
      </section>

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
              <select id="skill-agent" className="sg-select" style={{ width: 'auto' }}
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
                    {s.description || '（无描述）'} · {s.source === 'import' ? '导入' : '手动'}
                    {s.agentName ? ` · 仅 ${s.agentName} 生效` : ' · 全局生效'}
                  </small>
                </div>
                <div className="sg-set-item-control">
                  <select
                    className="sg-select"
                    style={{ marginRight: 10, width: 'auto' }}
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
    </div>
  );
}
