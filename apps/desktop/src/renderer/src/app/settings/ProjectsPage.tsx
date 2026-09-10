import { useCallback, useEffect, useState, type FormEvent } from 'react';
import { rpc } from '../../rpc/client';
import type { ProjectListResult, ProjectRow } from './types';
import { SettingsPageHeader } from './components/SettingsPageHeader';
import {
  IconCheck,
  IconCloud,
  IconFolder,
  IconPlus,
  IconRefresh,
} from '../../components/Icons';

const EMPTY_REMOTE = {
  name: '',
  gitlabInstance: '',
  namespace: '',
  project: '',
  defaultBranch: 'main',
  localRoot: '',
};

export function ProjectsPage() {
  const [items, setItems] = useState<ProjectRow[]>([]);
  const [includeArchived, setIncludeArchived] = useState(false);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [showRemote, setShowRemote] = useState(false);
  const [remote, setRemote] = useState(EMPTY_REMOTE);
  // 已归档任务（侧栏移除即归档，原本从界面"永久消失"）；这里提供唯一的找回入口。
  const [archivedTasks, setArchivedTasks] = useState<
    Array<{ id: string; title: string; projectId: string; projectName: string }>
  >([]);
  const [restoringTask, setRestoringTask] = useState('');
  const [detail, setDetail] = useState<{ project: ProjectRow; summary: unknown } | null>(null);
  const [detailForm, setDetailForm] = useState({ name: '', localRoot: '', defaultBranch: 'main' });

  const load = useCallback(async (archived: boolean) => {
    setLoading(true);
    setError(null);
    try {
      const result = await rpc<ProjectListResult>('project.list', { includeArchived: archived });
      setItems(result.items ?? []);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '工作区列表加载失败');
    } finally {
      setLoading(false);
    }
  }, []);

  const loadArchivedTasks = useCallback(async () => {
    try {
      const projects = await rpc<{ items: ProjectRow[] }>('project.list', { includeArchived: false });
      const out: Array<{ id: string; title: string; projectId: string; projectName: string }> = [];
      for (const p of projects.items ?? []) {
        const r = await rpc<{ items: Array<{ id: string; title: string; archived_at?: string }> }>(
          'workitem.list',
          { projectId: p.id, limit: 50, includeArchived: true },
        );
        for (const t of r.items ?? []) {
          if (t.archived_at) out.push({ id: t.id, title: t.title, projectId: p.id, projectName: p.name });
        }
      }
      setArchivedTasks(out);
    } catch {
      // 只读增强：失败不打断项目列表。
    }
  }, []);

  useEffect(() => {
    void load(includeArchived);
    void loadArchivedTasks();
  }, [includeArchived, load, loadArchivedTasks]);

  // 项目列表是 AppShell 的侧栏数据源：设置内的增删改必须广播，否则侧栏不刷新。
  const notifyProjectsChanged = () => window.dispatchEvent(new CustomEvent('sg:projects-changed'));
  // 任务归档/恢复后广播，侧栏与首页「最近任务」按缓存重载。
  const notifyTasksChanged = () => window.dispatchEvent(new CustomEvent('sg:tasks-changed'));

  const openFolder = async () => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const localRoot = await window.ratiflow.selectDirectory();
      if (!localRoot) return;
      const name = localRoot.split('/').filter(Boolean).at(-1) || '本地工作区';
      const project = await rpc<ProjectRow>('project.create', {
        gitlabInstance: 'local',
        namespace: 'workspace',
        project: `local-${stableHash(localRoot)}`,
        defaultBranch: 'main',
        name,
        localRoot,
      });
      setNotice(null);
      await load(includeArchived);
      notifyProjectsChanged();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '无法打开该文件夹');
    } finally {
      setBusy(false);
    }
  };

  const connectRemote = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const created = await rpc<ProjectRow>('project.create', {
        gitlabInstance: remote.gitlabInstance.trim(),
        namespace: remote.namespace.trim(),
        project: remote.project.trim(),
        defaultBranch: remote.defaultBranch.trim(),
        name: remote.name.trim() || remote.project.trim(),
        localRoot: remote.localRoot.trim(),
      });
      setNotice(`“${created.name}”已连接。`);
      setRemote(EMPTY_REMOTE);
      setShowRemote(false);
      await load(includeArchived);
      notifyProjectsChanged();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '远程项目连接失败');
    } finally {
      setBusy(false);
    }
  };

  const restoreTask = async (task: { id: string; title: string }) => {
    setRestoringTask(task.id);
    setError(null);
    try {
      await rpc('workitem.archive', { workItemId: task.id, archived: false });
      setNotice(`任务「${task.title}」已恢复，重新打开项目可见。`);
      await loadArchivedTasks();
      notifyTasksChanged();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '恢复失败');
    } finally {
      setRestoringTask('');
    }
  };

  const archive = async (project: ProjectRow, archived: boolean) => {
    setError(null);
    try {
      await rpc('project.archive', { projectId: project.id, archived });
      setNotice(archived ? `“${project.name}”已移到归档。` : `“${project.name}”已恢复。`);
      await load(includeArchived);
      notifyProjectsChanged();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '操作失败');
    }
  };

  const openDetail = async (projectId: string) => {
    setBusy(true);
    setError(null);
    try {
      const [project, summary] = await Promise.all([
        rpc<ProjectRow>('project.get', { projectId }),
        rpc('project.summary', { projectId }),
      ]);
      setDetail({ project, summary });
      setDetailForm({ name: project.name, localRoot: project.local_root ?? '', defaultBranch: project.default_branch ?? 'main' });
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '项目详情加载失败');
    } finally {
      setBusy(false);
    }
  };

  const saveDetail = async () => {
    if (!detail) return;
    setBusy(true);
    try {
      await rpc('project.update', {
        projectId: detail.project.id,
        name: detailForm.name.trim(),
        localRoot: detailForm.localRoot.trim(),
        defaultBranch: detailForm.defaultBranch.trim(),
      });
      setDetail(null);
      setNotice('项目设置已更新。');
      await load(includeArchived);
      notifyProjectsChanged();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '项目更新失败');
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="sg-set-page sg-projects-page">
      <SettingsPageHeader
        title="项目工作区"
        description="一个工作区对应一个本地代码目录。GitLab 和远程环境都是可选连接，不再阻塞本地工作。"
        actions={<button className="sg-btn sg-btn--quiet" onClick={() => void load(includeArchived)} disabled={loading}><IconRefresh size={14} />刷新</button>}
      />

      {error ? <div className="sg-banner sg-banner--error" role="alert">{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status"><IconCheck size={14} />{notice}</div> : null}

      <section className="sg-project-actions">
        <button className="sg-project-action sg-project-action--primary" onClick={() => void openFolder()} disabled={busy}>
          <span><IconFolder size={20} /></span>
          <div><strong>{busy ? '正在打开…' : '打开文件夹'}</strong><small>选择本机已有代码目录</small></div>
          <IconPlus size={15} />
        </button>
        <button className="sg-project-action" onClick={() => setShowRemote((value) => !value)}>
          <span><IconCloud size={20} /></span>
          <div><strong>连接 GitLab 项目</strong><small>可选：关联 Issue、分支和合并请求</small></div>
          <IconPlus size={15} />
        </button>
      </section>

      {showRemote ? (
        <form className="sg-project-remote-form" onSubmit={connectRemote}>
          <div><h2>连接 GitLab 项目</h2><p>本地目录可以稍后再选，远程连接不会替代本地工作区。</p></div>
          <div className="sg-project-remote-grid">
            <label><span>显示名</span><input value={remote.name} onChange={(event) => setRemote({ ...remote, name: event.target.value })} placeholder="我的项目" /></label>
            <label><span>GitLab 实例 *</span><input value={remote.gitlabInstance} onChange={(event) => setRemote({ ...remote, gitlabInstance: event.target.value })} placeholder="default" /></label>
            <label><span>命名空间 *</span><input value={remote.namespace} onChange={(event) => setRemote({ ...remote, namespace: event.target.value })} placeholder="team" /></label>
            <label><span>项目 *</span><input value={remote.project} onChange={(event) => setRemote({ ...remote, project: event.target.value })} placeholder="demo" /></label>
            <label><span>默认分支</span><input value={remote.defaultBranch} onChange={(event) => setRemote({ ...remote, defaultBranch: event.target.value })} /></label>
            <label><span>本地目录（可选）</span><input value={remote.localRoot} onChange={(event) => setRemote({ ...remote, localRoot: event.target.value })} placeholder="/Users/you/project" /></label>
          </div>
          <div className="sg-row"><button className="sg-btn sg-btn--primary" type="submit" disabled={busy || !remote.gitlabInstance.trim() || !remote.namespace.trim() || !remote.project.trim()}>连接项目</button><button className="sg-btn" type="button" onClick={() => setShowRemote(false)}>取消</button></div>
        </form>
      ) : null}

      <section className="sg-project-list-section">
        <div className="sg-project-list-head">
          <div><h2>最近工作区</h2><p>这些工作区会出现在新建任务上方的选择器里。</p></div>
          <label><input type="checkbox" checked={includeArchived} onChange={(event) => setIncludeArchived(event.target.checked)} />显示已归档</label>
        </div>
        {loading ? <div className="sg-empty">正在加载工作区…</div> : items.length === 0 ? (
          <div className="sg-project-empty"><IconFolder size={28} /><h3>还没有工作区</h3><p>打开一个本地文件夹即可开始，不需要先配置 GitLab。</p><button className="sg-btn sg-btn--primary" onClick={() => void openFolder()}><IconPlus size={14} />打开文件夹</button></div>
        ) : (
          <div className="sg-project-rows">
            {items.map((project) => (
              <article className="sg-project-row" key={project.id}>
                <span className="sg-project-icon"><IconFolder size={18} /></span>
                <div><h3>{project.name}</h3><p>{project.local_root || '未绑定本地目录'}</p></div>
                <div className="sg-project-row-actions">
                  <button className="sg-btn sg-btn--sm" onClick={() => void openDetail(project.id)}>详情</button>
                  <button className="sg-btn sg-btn--sm" onClick={() => void archive(project, !project.archived_at)}>{project.archived_at ? '恢复' : '归档'}</button>
                </div>
              </article>
            ))}
          </div>
        )}
      </section>

      {archivedTasks.length > 0 ? (
        <section className="sg-project-list-section">
          <div className="sg-project-list-head">
            <div><h2>已归档任务</h2><p>侧栏移除的任务在这里，可恢复回项目。</p></div>
          </div>
          <div className="sg-project-rows">
            {archivedTasks.map((task) => (
              <article className="sg-project-row" key={task.id}>
                <span className="sg-project-icon"><IconFolder size={18} /></span>
                <div><h3>{task.title}</h3><p>{task.projectName}</p></div>
                <button className="sg-btn sg-btn--sm" disabled={restoringTask !== ''} onClick={() => void restoreTask(task)}>
                  {restoringTask === task.id ? '恢复中…' : '恢复'}
                </button>
              </article>
            ))}
          </div>
        </section>
      ) : null}
      {detail ? (
        <div className="sg-drawer-backdrop" onClick={() => setDetail(null)}>
          <div className="sg-drawer" role="dialog" aria-label="项目详情" onClick={(event) => event.stopPropagation()}>
            <div className="sg-drawer-head"><strong>项目详情</strong><button className="sg-icon-btn" aria-label="关闭" onClick={() => setDetail(null)}>✕</button></div>
            <div className="sg-drawer-body sg-project-detail">
              <section className="sg-pd-section">
                <h4>基本信息</h4>
                <div className="sg-project-remote-grid sg-pd-form">
                  <label><span>名称</span><input value={detailForm.name} onChange={(event) => setDetailForm({ ...detailForm, name: event.target.value })} /></label>
                  <label><span>本地目录</span><input value={detailForm.localRoot} onChange={(event) => setDetailForm({ ...detailForm, localRoot: event.target.value })} /></label>
                  <label><span>默认分支</span><input value={detailForm.defaultBranch} onChange={(event) => setDetailForm({ ...detailForm, defaultBranch: event.target.value })} /></label>
                </div>
              </section>
              {(() => {
                const summary = (detail.summary ?? {}) as Record<string, unknown>;
                const num = (key: string) => (typeof summary[key] === 'number' ? String(summary[key]) : '0');
                const statusRaw = typeof summary.status === 'string' ? summary.status : '';
                const statusLabel = statusRaw === 'not_ready' ? '未就绪' : statusRaw === 'ready' ? '就绪' : statusRaw || '—';
                return (
                  <section className="sg-pd-section">
                    <h4>概览</h4>
                    <dl className="sg-pd-kv">
                      <div><dt>状态</dt><dd>{statusLabel}</dd></div>
                      <div><dt>归档</dt><dd>{summary.archived === true ? '已归档' : '未归档'}</dd></div>
                      <div><dt>任务</dt><dd>{num('workItemCount')}</dd></div>
                      <div><dt>阻塞任务</dt><dd>{num('blockedCount')}</dd></div>
                      <div><dt>知识源</dt><dd>{num('knowledgeSourceCount')}</dd></div>
                      <div><dt>项目 ID</dt><dd className="sg-pd-mono">{typeof summary.id === 'string' ? summary.id : detail.project.id}</dd></div>
                    </dl>
                  </section>
                );
              })()}
            </div>
            <div className="sg-drawer-foot">
              <button className="sg-btn sg-btn--primary" disabled={busy || !detailForm.name.trim()} onClick={() => void saveDetail()}>保存</button>
              <button className="sg-btn" onClick={() => setDetail(null)}>取消</button>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}

function stableHash(value: string): string {
  let hash = 2166136261;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return (hash >>> 0).toString(36);
}
