import { useCallback, useEffect, useState, type FormEvent } from 'react';
import { rpc } from '../../rpc/client';
import type { ProjectListResult, ProjectRow } from './types';
import { SettingsPageHeader } from './components/SettingsPageHeader';
import { StatusPill } from './components/StatusPill';
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

  useEffect(() => {
    void load(includeArchived);
  }, [includeArchived, load]);

  const openFolder = async () => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const localRoot = await window.sixgates.selectDirectory();
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
      setNotice(`“${project.name}”已添加，可以直接在新建任务中使用。`);
      await load(includeArchived);
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
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '远程项目连接失败');
    } finally {
      setBusy(false);
    }
  };

  const archive = async (project: ProjectRow, archived: boolean) => {
    setError(null);
    try {
      await rpc('project.archive', { projectId: project.id, archived });
      setNotice(archived ? `“${project.name}”已移到归档。` : `“${project.name}”已恢复。`);
      await load(includeArchived);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '操作失败');
    }
  };

  return (
    <div className="sg-set-page sg-projects-page">
      <SettingsPageHeader
        title="项目工作区"
        scope="本地"
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
                <div className="sg-project-meta">
                  {project.gitlab_instance && project.gitlab_instance !== 'local' ? <span>{project.namespace}/{project.project}</span> : <span>仅本地</span>}
                  <StatusPill kind={project.status === 'archived' ? 'readonly' : 'ready'} label={project.status === 'archived' ? '已归档' : '可用'} />
                </div>
                <button className="sg-btn sg-btn--sm" onClick={() => void archive(project, project.status !== 'archived')}>{project.status === 'archived' ? '恢复' : '归档'}</button>
              </article>
            ))}
          </div>
        )}
      </section>
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
