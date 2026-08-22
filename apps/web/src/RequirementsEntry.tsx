import { useCallback, useEffect, useRef, useState } from 'react';
import { createWorkItem, importDocument, importIssue, listWorkItems, loadDiagnostics } from './api';
import type { DiagnosticReport, WorkItem } from './types';
import { gateLabels } from './types';

interface Props {
  projectId: string;
  onOpenWorkbench: (id: string) => void;
}

// 需求入口：三种进入六关的方式——
// 1) 写入需求（落盘为工作目录文档）；2) 导入本地文档（复制进工作目录，主路径）；
// 3) GitLab Issue（可选，需先配置 GitLab）。
export default function RequirementsEntry({ projectId, onOpenWorkbench }: Props) {
  const [items, setItems] = useState<WorkItem[]>([]);
  const [diagnostics, setDiagnostics] = useState<DiagnosticReport | null>(null);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);

  // 模式一：写入需求
  const [title, setTitle] = useState('');
  const [description, setDescription] = useState('');
  // 模式二：导入本地文档
  const fileRef = useRef<HTMLInputElement>(null);
  const [pickedFile, setPickedFile] = useState<{ name: string; content: string } | null>(null);
  // 模式三：GitLab Issue
  const [issueIid, setIssueIid] = useState('');
  const [gitlabProjectId, setGitlabProjectId] = useState('');

  const refresh = useCallback(async () => {
    setLoading(true);
    setError('');
    try {
      const [page, report] = await Promise.all([
        listWorkItems(projectId),
        loadDiagnostics().catch(() => null),
      ]);
      setItems(page.items);
      setDiagnostics(report);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '加载失败');
    } finally {
      setLoading(false);
    }
  }, [projectId]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const gitlabReady = diagnostics?.integrations.some((i) => i.id === 'gitlab' && i.status === 'ready') ?? false;

  const submitText = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!title.trim()) {
      return;
    }
    setBusy(true);
    setError('');
    setNotice('');
    try {
      const wi = await createWorkItem({ projectId, title: title.trim(), description: description.trim() });
      setTitle('');
      setDescription('');
      setNotice(`已保存需求文档并创建「${wi.title}」。`);
      onOpenWorkbench(wi.id);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '创建失败');
    } finally {
      setBusy(false);
    }
  };

  const pickFile = (event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    if (!file) {
      return;
    }
    const reader = new FileReader();
    reader.onload = () => {
      setPickedFile({ name: file.name, content: String(reader.result ?? '') });
    };
    reader.readAsText(file);
  };

  const submitFile = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!pickedFile) {
      return;
    }
    setBusy(true);
    setError('');
    setNotice('');
    try {
      const wi = await importDocument({
        projectId,
        filename: pickedFile.name,
        content: pickedFile.content,
      });
      setPickedFile(null);
      if (fileRef.current) {
        fileRef.current.value = '';
      }
      setNotice(`已导入「${wi.title}」，文档已复制进工作目录。`);
      onOpenWorkbench(wi.id);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '导入失败');
    } finally {
      setBusy(false);
    }
  };

  const submitImport = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!issueIid.trim() || !gitlabProjectId.trim()) {
      return;
    }
    setBusy(true);
    setError('');
    setNotice('');
    try {
      const wi = await importIssue({
        projectId,
        issueIid: issueIid.trim(),
        gitlabProjectId: gitlabProjectId.trim(),
      });
      setIssueIid('');
      setNotice(`已从 GitLab Issue #${wi.gitlabIssueIid} 导入「${wi.title}」。`);
      onOpenWorkbench(wi.id);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '导入失败');
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="stack">
      <div className="page-heading">
        <div>
          <h1>写下你的需求，开始闯关</h1>
          <p>需求以 Markdown 文档形式保存在本地工作目录（data/docs/），随后依次通过需求 → 方案 → 开发 → 测试 → 部署 → 验证六关。</p>
        </div>
      </div>
      {error ? <div className="error-banner" role="alert">{error}</div> : null}
      {notice ? <div className="notice-banner" role="status">{notice}</div> : null}

      <div className="entry-grid">
        <form className="panel entry-card" onSubmit={submitFile} aria-labelledby="entry-file">
          <h2 id="entry-file">📁 导入本地文档</h2>
          <p className="entry-hint">选择磁盘上的需求文档（.md / .txt），SixGates 会把它复制进工作目录并创建工作项。</p>
          <input
            ref={fileRef}
            type="file"
            accept=".md,.markdown,.txt"
            onChange={pickFile}
            aria-label="选择需求文档文件"
          />
          {pickedFile ? (
            <p className="picked-file">
              已选择：<code>{pickedFile.name}</code>（{pickedFile.content.length} 字符，标题取首行 H1 或文件名）
            </p>
          ) : null}
          <button className="primary-button" type="submit" disabled={busy || !pickedFile}>
            复制到工作目录并开始闯关 →
          </button>
        </form>

        <form className="panel entry-card" onSubmit={submitText} aria-labelledby="entry-write">
          <h2 id="entry-write">✍️ 写入需求</h2>
          <p className="entry-hint">直接输入；保存后会生成 <code>data/docs/…/requirement.md</code> 文档。</p>
          <label className="field">
            <span>需求标题 *</span>
            <input value={title} onChange={(e) => setTitle(e.target.value)} placeholder="例如：支持 SSO 登录" required />
          </label>
          <label className="field">
            <span>需求描述</span>
            <textarea
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="范围、非目标、验收标准……（可留空，需求关中可让 Agent 起草 PRD）"
              rows={5}
            />
          </label>
          <button className="primary-button" type="submit" disabled={busy}>保存为文档并进入需求关 →</button>
        </form>

        <form className="panel entry-card entry-optional" onSubmit={submitImport} aria-labelledby="entry-import">
          <h2 id="entry-import">⑂ 从 GitLab Issue 导入<span className="optional-tag">可选</span></h2>
          <p className="entry-hint">
            {gitlabReady
              ? 'GitLab 已连接；Issue 标题、正文与标签会导入并落盘为文档。'
              : '需要先在「本地诊断」页配置 GitLab（SIXGATES_GITLAB_URL / SIXGATES_GITLAB_TOKEN）。'}
          </p>
          <label className="field">
            <span>GitLab 项目 ID *</span>
            <input value={gitlabProjectId} onChange={(e) => setGitlabProjectId(e.target.value)} placeholder="例如：42" inputMode="numeric" />
          </label>
          <label className="field">
            <span>Issue IID *</span>
            <input value={issueIid} onChange={(e) => setIssueIid(e.target.value)} placeholder="例如：108" inputMode="numeric" />
          </label>
          <button className="secondary-button" type="submit" disabled={busy || !gitlabReady}>
            {gitlabReady ? '导入 Issue →' : 'GitLab 未配置'}
          </button>
        </form>
      </div>

      <section className="panel" aria-labelledby="progress-list">
        <h2 id="progress-list">进行中的六关</h2>
        {loading ? (
          <div className="panel loading-panel">正在读取…</div>
        ) : items.length === 0 ? (
          <div className="empty-state">还没有需求；从上方开始第一关。</div>
        ) : (
          <div className="integration-list">
            <div className="integration-header" aria-hidden="true">
              <span>需求</span><span>当前关</span><span>来源</span><span>操作</span>
            </div>
            {items.map((item) => (
              <div className="integration-row" key={item.id}>
                <div className="service-name">{item.title}</div>
                <div><span className={`stage-chip stage-${item.currentGate}`}>{gateLabels[item.currentGate]}</span></div>
                <div className="detail">{item.gitlabIssueIid ? `GitLab #${item.gitlabIssueIid}` : '本地文档'}</div>
                <div>
                  <button className="row-action" type="button" onClick={() => onOpenWorkbench(item.id)}>继续闯关 →</button>
                </div>
              </div>
            ))}
          </div>
        )}
      </section>
    </div>
  );
}
