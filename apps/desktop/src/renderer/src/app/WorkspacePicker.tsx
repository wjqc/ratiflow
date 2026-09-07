import { useEffect, useMemo, useRef, useState } from 'react';
import {
  IconCheck,
  IconChevronDown,
  IconCloud,
  IconFolder,
  IconPlus,
  IconSearch,
} from '../components/Icons';
import { rpc } from '../rpc/client';
import type { Project } from './ProjectSidebar';

interface Props {
  projects: Project[];
  projectId: string;
  onSelect: (project: Project) => void;
  onCreated: (project: Project) => void;
  onRemote: () => void;
}

export function WorkspacePicker({ projects, projectId, onSelect, onCreated, onRemote }: Props) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const root = useRef<HTMLDivElement>(null);
  const current = projects.find((project) => project.id === projectId);
  const visible = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    return normalized
      ? projects.filter((project) => project.name.toLowerCase().includes(normalized))
      : projects;
  }, [projects, query]);

  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    window.addEventListener('mousedown', close);
    return () => window.removeEventListener('mousedown', close);
  }, [open]);

  const openFolder = async () => {
    setBusy(true);
    setError('');
    try {
      const localRoot = await window.ratiflow.selectDirectory();
      if (!localRoot) return;
      const name = localRoot.split('/').filter(Boolean).at(-1) || '本地工作区';
      const project = await rpc<Project>('project.create', {
        gitlabInstance: 'local',
        namespace: 'workspace',
        project: `local-${stableHash(localRoot)}`,
        defaultBranch: 'main',
        name,
        localRoot,
      });
      onCreated(project);
      setOpen(false);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '无法打开该文件夹');
    } finally {
      setBusy(false);
    }
  };

  const useTemporaryWorkspace = async () => {
    setBusy(true);
    setError('');
    try {
      const project = await rpc<Project>('project.create', {
        gitlabInstance: 'local',
        namespace: 'workspace',
        project: 'no-project',
        defaultBranch: 'main',
        name: '不在项目中工作',
        localRoot: '',
      });
      onCreated(project);
      setOpen(false);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '无法创建临时工作区');
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="sg-workspace-picker" ref={root}>
      <button
        type="button"
        className="sg-workspace-trigger"
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        <IconFolder size={14} />
        <span>{current?.name || '选择工作区'}</span>
        <IconChevronDown size={13} />
      </button>

      {open ? (
        <div className="sg-workspace-popover" role="dialog" aria-label="选择工作区">
          <label className="sg-workspace-search">
            <IconSearch size={14} />
            <input
              autoFocus
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="搜索工作区"
              aria-label="搜索工作区"
            />
          </label>
          <div className="sg-workspace-list" role="listbox" aria-label="最近工作区">
            {visible.length === 0 ? (
              <div className="sg-workspace-empty">没有匹配的工作区</div>
            ) : (
              visible.map((project) => (
                <button
                  type="button"
                  role="option"
                  aria-selected={project.id === projectId}
                  className="sg-workspace-option"
                  key={project.id}
                  onClick={() => {
                    onSelect(project);
                    setOpen(false);
                  }}
                >
                  <IconFolder size={15} />
                  <span>{project.name}</span>
                  {project.id === projectId ? <IconCheck size={14} /> : null}
                </button>
              ))
            )}
          </div>
          <div className="sg-workspace-actions">
            <button type="button" onClick={() => void openFolder()} disabled={busy}>
              <IconPlus size={14} />
              {busy ? '正在打开…' : '打开文件夹'}
            </button>
            <button type="button" onClick={onRemote}>
              <IconCloud size={14} />
              远程连接
            </button>
            <button type="button" onClick={() => void useTemporaryWorkspace()} disabled={busy}>
              不在项目中工作
            </button>
          </div>
          {error ? <div className="sg-workspace-error" role="alert">{error}</div> : null}
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
