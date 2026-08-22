// S10 项目与目录：真实 project.list/create/update/archive；目录选择走主进程窄 IPC。
import { useCallback, useEffect, useState, type FormEvent } from 'react';
import { rpc } from '../../rpc/client';
import type { ProjectListResult, ProjectRow } from './types';
import { SettingsPageHeader } from './components/SettingsPageHeader';
import { SettingsSection } from './components/SettingsSection';
import { StatusPill } from './components/StatusPill';
import { IconFolder, IconPlus, IconRefresh } from '../../components/Icons';

const EMPTY_FORM = {
  name: '',
  localRoot: '',
  gitlabInstance: '',
  namespace: '',
  project: '',
  defaultBranch: 'main',
};

type ProjectForm = typeof EMPTY_FORM;

function ProjectFormFields({
  form,
  onChange,
  showGitlab,
}: {
  form: ProjectForm;
  onChange: (patch: Partial<ProjectForm>) => void;
  showGitlab: boolean;
}) {
  const pickDir = async () => {
    const dir = await window.sixgates.selectDirectory();
    if (dir) onChange({ localRoot: dir });
  };
  return (
    <>
      <div className="sg-field">
        <label htmlFor="pj-root">本地仓库目录</label>
        <div className="sg-row">
          <input
            id="pj-root"
            className="sg-input"
            style={{ flex: 1 }}
            value={form.localRoot}
            onChange={(e) => onChange({ localRoot: e.target.value })}
            placeholder="/Users/you/projects/demo"
          />
          <button
            type="button"
            className="sg-btn"
            onClick={() => void pickDir()}
            title="选择目录"
          >
            <IconFolder size={14} />
            选择目录
          </button>
        </div>
        <p className="sg-hint">目录存在性/嵌套检测待 project.inspectRoot 契约；当前创建前请先确认路径正确。</p>
      </div>
      <div className="sg-field">
        <label htmlFor="pj-name">显示名</label>
        <input
          id="pj-name"
          className="sg-input"
          value={form.name}
          onChange={(e) => onChange({ name: e.target.value })}
          placeholder="演示项目"
        />
      </div>
      {showGitlab ? (
        <div className="sg-set-grid-3">
          <div className="sg-field">
            <label htmlFor="pj-gi">GitLab 实例</label>
            <input
              id="pj-gi"
              className="sg-input"
              value={form.gitlabInstance}
              onChange={(e) => onChange({ gitlabInstance: e.target.value })}
              placeholder="default"
            />
          </div>
          <div className="sg-field">
            <label htmlFor="pj-ns">namespace</label>
            <input
              id="pj-ns"
              className="sg-input"
              value={form.namespace}
              onChange={(e) => onChange({ namespace: e.target.value })}
              placeholder="team"
            />
          </div>
          <div className="sg-field">
            <label htmlFor="pj-repo">project</label>
            <input
              id="pj-repo"
              className="sg-input"
              value={form.project}
              onChange={(e) => onChange({ project: e.target.value })}
              placeholder="demo"
            />
          </div>
        </div>
      ) : null}
      <div className="sg-field" style={{ maxWidth: 220 }}>
        <label htmlFor="pj-branch">默认分支</label>
        <input
          id="pj-branch"
          className="sg-input"
          value={form.defaultBranch}
          onChange={(e) => onChange({ defaultBranch: e.target.value })}
        />
      </div>
    </>
  );
}

export function ProjectsPage() {
  const [items, setItems] = useState<ProjectRow[]>([]);
  const [includeArchived, setIncludeArchived] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [form, setForm] = useState<ProjectForm>(EMPTY_FORM);
  const [creating, setCreating] = useState(false);
  const [editing, setEditing] = useState<ProjectRow | null>(null);
  const [editForm, setEditForm] = useState<ProjectForm>(EMPTY_FORM);
  const [saving, setSaving] = useState(false);

  const load = useCallback(async (archived: boolean) => {
    setLoading(true);
    setError(null);
    try {
      const res = await rpc<ProjectListResult>('project.list', { includeArchived: archived });
      setItems(res.items ?? []);
    } catch (e) {
      setError(e instanceof Error ? e.message : '项目列表加载失败');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load(includeArchived);
  }, [load, includeArchived]);

  const create = async (e: FormEvent) => {
    e.preventDefault();
    setCreating(true);
    setError(null);
    setNotice(null);
    try {
      await rpc('project.create', {
        gitlabInstance: form.gitlabInstance.trim(),
        namespace: form.namespace.trim(),
        project: form.project.trim(),
        defaultBranch: form.defaultBranch.trim(),
        name: form.name.trim(),
        localRoot: form.localRoot.trim(),
      });
      setNotice(`项目「${form.name.trim() || form.project.trim()}」已创建`);
      setForm(EMPTY_FORM);
      await load(includeArchived);
    } catch (err) {
      setError(err instanceof Error ? err.message : '创建失败');
    } finally {
      setCreating(false);
    }
  };

  const startEdit = (p: ProjectRow) => {
    setEditing(p);
    setEditForm({
      name: p.name,
      localRoot: p.localRoot,
      gitlabInstance: p.gitlabInstance,
      namespace: p.namespace,
      project: p.project,
      defaultBranch: p.defaultBranch,
    });
  };

  const save = async (e: FormEvent) => {
    e.preventDefault();
    if (!editing) return;
    setSaving(true);
    setError(null);
    try {
      await rpc('project.update', {
        projectId: editing.id,
        name: editForm.name.trim(),
        localRoot: editForm.localRoot.trim(),
        defaultBranch: editForm.defaultBranch.trim(),
      });
      setNotice(`项目「${editForm.name.trim()}」已保存`);
      setEditing(null);
      await load(includeArchived);
    } catch (err) {
      setError(err instanceof Error ? err.message : '保存失败');
    } finally {
      setSaving(false);
    }
  };

  const archive = async (p: ProjectRow, archived: boolean) => {
    setError(null);
    setNotice(null);
    try {
      await rpc('project.archive', { projectId: p.id, archived });
      setNotice(archived ? `项目「${p.name}」已归档` : `项目「${p.name}」已恢复`);
      await load(includeArchived);
    } catch (err) {
      setError(err instanceof Error ? err.message : '操作失败');
    }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="项目与目录"
        scope="本地"
        description="登记 GitLab 仓库与本地检出目录；归档不出现在主流程，可随时恢复。"
        actions={
          <button className="sg-btn" onClick={() => void load(includeArchived)} disabled={loading}>
            <IconRefresh size={14} />
            刷新
          </button>
        }
      />

      {error ? <div className="sg-banner sg-banner--error" role="alert">操作失败：{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

      <SettingsSection title="创建项目" description="当前契约要求 GitLab 关联（instance/namespace/project 必填）。">
        <form className="sg-card sg-set-form" onSubmit={create}>
          <ProjectFormFields form={form} onChange={(p) => setForm((f) => ({ ...f, ...p }))} showGitlab />
          <div className="sg-row">
            <button
              type="submit"
              className="sg-btn sg-btn--primary"
              disabled={
                creating ||
                !form.localRoot.trim() ||
                !form.gitlabInstance.trim() ||
                !form.namespace.trim() ||
                !form.project.trim()
              }
            >
              <IconPlus size={14} />
              {creating ? '创建中…' : '创建项目'}
            </button>
            <span className="sg-hint">创建后写入审计事件 project.create。</span>
          </div>
        </form>
      </SettingsSection>

      <SettingsSection
        title="项目列表"
        actions={
          <label className="sg-row sg-hint" style={{ gap: 6, cursor: 'pointer' }}>
            <input
              type="checkbox"
              checked={includeArchived}
              onChange={(e) => setIncludeArchived(e.target.checked)}
            />
            显示已归档
          </label>
        }
      >
        {loading ? (
          <div className="sg-skeleton-rows" aria-busy="true">
            <div className="sg-skeleton-row" />
            <div className="sg-skeleton-row" />
          </div>
        ) : items.length === 0 ? (
          <div className="sg-empty">
            <IconFolder size={28} style={{ color: 'var(--sg-border-strong)' }} />
            <span>{includeArchived ? '暂无项目记录' : '暂无启用中的项目'}</span>
            <span className="sg-hint">在上方表单登记第一个项目，或勾选「显示已归档」查看历史。</span>
          </div>
        ) : (
          <table className="sg-table">
            <thead>
              <tr>
                <th>名称</th>
                <th>GitLab</th>
                <th>状态</th>
                <th style={{ width: 200 }}>操作</th>
              </tr>
            </thead>
            <tbody>
              {items.map((p) => (
                <tr key={p.id}>
                  <td>
                    <div style={{ fontWeight: 500 }}>{p.name}</div>
                    <div className="sg-path">{p.localRoot || '未设置本地目录'}</div>
                  </td>
                  <td>
                    <span className="sg-code">{p.gitlabInstance}</span>{' '}
                    <span className="sg-muted">
                      {p.namespace}/{p.project}
                    </span>
                    <div className="sg-hint">分支 {p.defaultBranch}</div>
                  </td>
                  <td>
                    <StatusPill
                      kind={p.status === 'ready' ? 'ready' : 'readonly'}
                      label={p.status === 'ready' ? '已就绪' : '已归档'}
                    />
                  </td>
                  <td>
                    <div className="sg-row" style={{ gap: 6 }}>
                      <button className="sg-btn sg-btn--sm" onClick={() => startEdit(p)}>
                        编辑
                      </button>
                      {p.status === 'ready' ? (
                        <button className="sg-btn sg-btn--sm" onClick={() => void archive(p, true)}>
                          归档
                        </button>
                      ) : (
                        <button className="sg-btn sg-btn--sm" onClick={() => void archive(p, false)}>
                          恢复
                        </button>
                      )}
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </SettingsSection>

      {editing ? (
        <SettingsSection title={`编辑项目：${editing.name}`} description="GitLab 关联创建后不可修改（当前契约）。">
          <form className="sg-card sg-set-form" onSubmit={save}>
            <ProjectFormFields form={editForm} onChange={(p) => setEditForm((f) => ({ ...f, ...p }))} showGitlab={false} />
            <div className="sg-row">
              <button type="submit" className="sg-btn sg-btn--primary" disabled={saving || !editForm.name.trim()}>
                {saving ? '保存中…' : '保存修改'}
              </button>
              <button type="button" className="sg-btn" onClick={() => setEditing(null)} disabled={saving}>
                取消
              </button>
            </div>
          </form>
        </SettingsSection>
      ) : null}
    </div>
  );
}
