import { useState } from 'react';
import { rpc, rpcErrorMessage } from '../rpc/client';

type Mode = 'text' | 'document' | 'issue' | 'image';

interface Props {
  projectId: string;
  onCreated: (workItemId: string) => void;
  onBack: () => void;
}

interface SelectedFile {
  filename: string;
  contentBase64: string;
  content?: string;
  size: number;
}

// 统一输入器：文字 / 本地文档 / GitLab Issue / 图片 四种来源（规范 §4.2）。
// 组件不推断上传成功；状态以服务端返回为准。
export default function NewTaskPage({ projectId, onCreated, onBack }: Props) {
  const [mode, setMode] = useState<Mode>('text');
  const [title, setTitle] = useState('');
  const [description, setDescription] = useState('');
  const [file, setFile] = useState<SelectedFile | null>(null);
  const [gitlabProjectId, setGitlabProjectId] = useState('');
  const [issueIid, setIssueIid] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  const submit = async () => {
    setBusy(true);
    setError('');
    try {
      if (mode === 'text') {
        if (!title.trim()) {
          throw new Error('标题不能为空');
        }
        const wi = await rpc<{ id: string }>('workitem.create', {
          projectId, title: title.trim(), description: description.trim(),
        });
        onCreated(wi.id);
      } else if (mode === 'document') {
        if (!file?.content) {
          throw new Error('请先选择文档');
        }
        const wi = await rpc<{ id: string }>('workitem.importDocument', {
          projectId, filename: file.filename, content: file.content,
        });
        onCreated(wi.id);
      } else if (mode === 'issue') {
        if (!gitlabProjectId.trim() || !issueIid.trim()) {
          throw new Error('GitLab 项目 ID 与 Issue IID 必填');
        }
        const wi = await rpc<{ id: string }>('workitem.importIssue', {
          projectId, gitlabProjectId: gitlabProjectId.trim(), issueIid: issueIid.trim(),
        });
        onCreated(wi.id);
      } else {
        if (!file) {
          throw new Error('请先选择图片');
        }
        // 图片：先创建任务，再作为附件导入（多模态解析由核心标记状态）。
        const wi = await rpc<{ id: string }>('workitem.create', {
          projectId, title: title.trim() || '图片需求', description: description.trim(),
        });
        await rpc('attachment.import', {
          workItemId: wi.id, filename: file.filename, contentBase64: file.contentBase64,
        });
        onCreated(wi.id);
      }
    } catch (reason) {
      setError(rpcErrorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const pickFile = async () => {
    const selected = await window.sixgates.selectFile();
    if (!selected) {
      return;
    }
    let content: string | undefined;
    if (selected.size < 2 << 20 && /\.(md|markdown|txt)$/i.test(selected.filename)) {
      content = atob(selected.contentBase64);
    }
    setFile({ ...selected, content });
  };

  return (
    <div className="sg-section" style={{ maxWidth: 720 }}>
      <div className="sg-row" style={{ justifyContent: 'space-between' }}>
        <h1 style={{ fontSize: 18, margin: 0 }}>新建任务</h1>
        <button className="sg-button" onClick={onBack}>← 返回</button>
      </div>
      <div className="sg-row" role="tablist" aria-label="需求来源" style={{ marginTop: 12 }}>
        {([['text', '✍️ 文字'], ['document', '📁 本地文档'], ['issue', '⑂ GitLab Issue'], ['image', '🖼 图片']] as Array<[Mode, string]>).map(([value, label]) => (
          <button
            key={value}
            role="tab"
            aria-selected={mode === value}
            className={`sg-button ${mode === value ? 'sg-button--primary' : ''}`}
            onClick={() => setMode(value)}
          >
            {label}
          </button>
        ))}
      </div>
      {error ? <div className="sg-banner sg-banner--error" role="alert" style={{ marginTop: 10 }}>{error}</div> : null}

      <div className="sg-stack" style={{ marginTop: 14 }}>
        {mode === 'text' || mode === 'image' ? (
          <>
            <label className="sg-field">
              <span>需求标题 *</span>
              <input className="sg-input" value={title} onChange={(e) => setTitle(e.target.value)} placeholder="例如：支持 SSO 登录" />
            </label>
            <label className="sg-field">
              <span>需求描述</span>
              <textarea className="sg-textarea" rows={5} value={description} onChange={(e) => setDescription(e.target.value)}
                placeholder="范围、非目标、验收标准……" />
            </label>
          </>
        ) : null}

        {mode === 'document' || mode === 'image' ? (
          <div className="sg-stack">
            <button className="sg-button" onClick={() => void pickFile()}>
              {file ? `已选择：${file.filename}（${file.size} 字节）—— 点击重选` : '选择文件（原生文件对话框）'}
            </button>
            {mode === 'image' && file ? (
              <p className="sg-muted">图片将作为附件导入并进入多模态解析；模型不支持视觉时会明确标记 vision_unsupported。</p>
            ) : null}
            {mode === 'document' && file && !file.content ? (
              <p className="sg-muted">仅支持文本类文档（.md/.txt）；二进制文档请改用文字模式描述。</p>
            ) : null}
          </div>
        ) : null}

        {mode === 'issue' ? (
          <div className="sg-row">
            <label className="sg-field" style={{ flex: 1 }}>
              <span>GitLab 项目 ID *</span>
              <input className="sg-input" value={gitlabProjectId} onChange={(e) => setGitlabProjectId(e.target.value)} placeholder="42" />
            </label>
            <label className="sg-field" style={{ flex: 1 }}>
              <span>Issue IID *</span>
              <input className="sg-input" value={issueIid} onChange={(e) => setIssueIid(e.target.value)} placeholder="108" />
            </label>
          </div>
        ) : null}

        <div>
          <button className="sg-button sg-button--primary" disabled={busy} onClick={() => void submit()}>
            {busy ? '创建中…' : '创建并进入需求关 →'}
          </button>
        </div>
      </div>
    </div>
  );
}
