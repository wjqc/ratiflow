import { useState } from 'react';
import type { ReactNode } from 'react';
import { rpc, rpcErrorMessage } from '../rpc/client';
import { IconDoc, IconImage, IconIssue, IconSend, IconText } from '../components/Icons';

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

const MODE_CHIPS: Array<{ mode: Mode; label: string; icon: ReactNode }> = [
  { mode: 'text', label: '文字', icon: <IconText size={13} /> },
  { mode: 'document', label: '文档', icon: <IconDoc size={13} /> },
  { mode: 'issue', label: 'Issue', icon: <IconIssue size={13} /> },
  { mode: 'image', label: '图片', icon: <IconImage size={13} /> },
];

const GATE_FLOW: Array<{ name: string; sub: string }> = [
  { name: '需求关', sub: '需求澄清与范围确认' },
  { name: '方案关', sub: '整体方案与技术设计' },
  { name: '开发关', sub: '编码实现与单元测试' },
  { name: '测试关', sub: '集成测试与质量验证' },
  { name: '发布关', sub: '部署与环境准备' },
  { name: '验收关', sub: '验收与交付验证' },
];

// 统一输入器：文字 / 本地文档 / GitLab Issue / 图片 四种来源（规范 §4.2）。
// 组件不推断上传成功；状态以服务端返回为准。
export default function NewTaskPage({ projectId, onCreated }: Props) {
  const [mode, setMode] = useState<Mode>('text');
  const [description, setDescription] = useState('');
  const [file, setFile] = useState<SelectedFile | null>(null);
  const [gitlabProjectId, setGitlabProjectId] = useState('');
  const [issueIid, setIssueIid] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  // 标题取需求描述首行（原型只有一个大输入框）。
  const deriveTitle = (text: string): string => {
    const first = text.split('\n').map((l) => l.trim()).find(Boolean) ?? '';
    return first.length > 40 ? `${first.slice(0, 40)}…` : first;
  };

  const submit = async () => {
    setBusy(true);
    setError('');
    try {
      if (mode === 'text') {
        const title = deriveTitle(description);
        if (!title) {
          throw new Error('请先描述你的需求');
        }
        const wi = await rpc<{ id: string }>('workitem.create', {
          projectId, title, description: description.trim(),
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
          projectId,
          title: deriveTitle(description) || file.filename,
          description: description.trim(),
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
    <>
      <header className="sg-page-head">
        <span className="sg-page-head-title">新建任务</span>
        <span className="sg-page-head-status">本地运行</span>
      </header>
      <div className="sg-scroll">
        <div className="sg-nt-wrap">
          <h2 className="sg-hero-title">从需求开始，让 Agent 逐关推进</h2>
          <p className="sg-hero-sub">基于你选择的项目知识库，Agent 将按流程逐关完成交付。</p>

          <div className="sg-nt-card">
            <textarea
              className="sg-nt-textarea"
              placeholder={
                mode === 'issue'
                  ? '补充说明（可选）…'
                  : '请描述你的需求，例如：重构多项目 Agent 工作台，支持项目知识库和六关进度。'
              }
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              aria-label="需求描述"
            />

            {(mode === 'document' || mode === 'image' || mode === 'issue') && (
              <div className="sg-nt-extra">
                {mode === 'document' || mode === 'image' ? (
                  <>
                    <button className="sg-btn sg-btn--sm" onClick={() => void pickFile()}>
                      {file
                        ? `已选择：${file.filename}（${formatSize(file.size)}）—— 点击重选`
                        : mode === 'document'
                          ? '选择文档（原生文件对话框）'
                          : '选择图片（原生文件对话框）'}
                    </button>
                    {mode === 'image' && file ? (
                      <p className="sg-muted" style={{ margin: '8px 0 0' }}>
                        图片将作为附件导入并进入多模态解析；模型不支持视觉时会明确标记 vision_unsupported。
                      </p>
                    ) : null}
                    {mode === 'document' && file && !file.content ? (
                      <p className="sg-muted" style={{ margin: '8px 0 0' }}>
                        仅支持文本类文档（.md/.txt）；二进制文档请改用文字模式描述。
                      </p>
                    ) : null}
                  </>
                ) : null}
                {mode === 'issue' ? (
                  <div className="sg-form-grid sg-form-grid--2">
                    <label className="sg-field" style={{ marginBottom: 0 }}>
                      <span>GitLab 项目 ID *</span>
                      <input
                        value={gitlabProjectId}
                        onChange={(e) => setGitlabProjectId(e.target.value)}
                        placeholder="42"
                      />
                    </label>
                    <label className="sg-field" style={{ marginBottom: 0 }}>
                      <span>Issue IID *</span>
                      <input
                        value={issueIid}
                        onChange={(e) => setIssueIid(e.target.value)}
                        placeholder="108"
                      />
                    </label>
                  </div>
                ) : null}
              </div>
            )}

            <div className="sg-nt-bar">
              <div className="sg-nt-chips" role="tablist" aria-label="需求来源">
                {MODE_CHIPS.map(({ mode: m, label, icon }) => (
                  <button
                    key={m}
                    role="tab"
                    aria-selected={mode === m}
                    className={`sg-nt-chip ${mode === m ? 'sg-nt-chip--active' : ''}`}
                    onClick={() => setMode(m)}
                  >
                    {icon}
                    {label}
                  </button>
                ))}
              </div>
              <button
                className="sg-nt-send"
                disabled={busy}
                onClick={() => void submit()}
                title={busy ? '创建中…' : '创建并进入需求关'}
                aria-label="创建并进入需求关"
              >
                <IconSend size={15} />
              </button>
            </div>
          </div>

          {error ? (
            <div className="sg-banner sg-banner--error" role="alert" style={{ marginTop: 12 }}>
              {error}
            </div>
          ) : null}

          <div className="sg-nt-flow-label">交付流程（六关）</div>
          <div className="sg-nt-flow">
            {GATE_FLOW.map((g, i) => (
              <div className="sg-nt-step" key={g.name}>
                {i > 0 && <span className="sg-nt-step-sep">→</span>}
                <span className={`sg-nt-step-num ${i === 0 ? '' : 'sg-nt-step-num--idle'}`}>
                  {i + 1}
                </span>
                <div style={{ minWidth: 0 }}>
                  <div className="sg-nt-step-name">{g.name}</div>
                  <div className="sg-nt-step-sub">{g.sub}</div>
                </div>
              </div>
            ))}
          </div>

          <div className="sg-nt-flow-label">更多输入方式</div>
          <div className="sg-nt-alt-grid">
            <AltCard
              label="文档"
              active={mode === 'document'}
              onClick={() => setMode('document')}
              icon={<IconDoc size={18} />}
              body={mode === 'document' && file ? file.filename : '选择本地文档'}
              hint={mode === 'document' && file ? formatSize(file.size) : '支持 .md / .txt'}
            />
            <AltCard
              label="Issue（GitLab）"
              active={mode === 'issue'}
              onClick={() => setMode('issue')}
              icon={<IconIssue size={18} />}
              body={mode === 'issue' && issueIid ? `Issue #${issueIid}` : '填写项目与 Issue'}
              hint={mode === 'issue' && gitlabProjectId ? `项目 ${gitlabProjectId}` : '导入议题描述'}
            />
            <AltCard
              label="图片"
              active={mode === 'image'}
              onClick={() => setMode('image')}
              icon={<IconImage size={18} />}
              body={mode === 'image' && file ? file.filename : '选择界面截图'}
              hint={mode === 'image' && file ? formatSize(file.size) : '多模态解析'}
            />
          </div>
        </div>
      </div>
    </>
  );
}

function AltCard({
  label,
  active,
  onClick,
  icon,
  body,
  hint,
}: {
  label: string;
  active: boolean;
  onClick: () => void;
  icon: ReactNode;
  body: string;
  hint: string;
}) {
  return (
    <button
      className={`sg-nt-alt-card ${active ? 'sg-nt-alt-card--active' : ''}`}
      onClick={onClick}
    >
      <span className="sg-nt-alt-label">{label}</span>
      <span className="sg-nt-alt-body">
        {icon}
        <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
          {body}
        </span>
      </span>
      <span className="sg-nt-alt-hint">{hint}</span>
    </button>
  );
}

function formatSize(bytes: number): string {
  if (bytes >= 1 << 20) return `${(bytes / (1 << 20)).toFixed(1)} MB`;
  if (bytes >= 1 << 10) return `${Math.round(bytes / (1 << 10))} KB`;
  return `${bytes} 字节`;
}
