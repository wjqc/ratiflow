import { useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { rpc, rpcErrorMessage } from '../rpc/client';
import { IconDoc, IconImage, IconIssue, IconPaperclip, IconPlus, IconSend, IconText } from '../components/Icons';
import { ModelPicker } from './ModelPicker';
import {
  friendlyAgentError,
  rememberAutomaticPrd,
  startPrdDraft,
} from './prdDraft';
import { WorkspacePicker } from './WorkspacePicker';
import type { Project } from './ProjectSidebar';

type Mode = 'text' | 'document' | 'issue' | 'image';

interface Props {
  projectId: string;
  projects: Project[];
  onCreated: (workItemId: string, projectId: string) => void;
  onWorkspaceChanged: (project: Project) => void;
  onOpenRemote: () => void;
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
  { name: '部署关', sub: '部署与环境准备' },
  { name: '验证关', sub: '验收与交付验证' },
];

// 统一输入器：文字 / 本地文档 / GitLab Issue / 图片 四种来源（规范 §4.2）。
// 组件不推断上传成功；状态以服务端返回为准。
export default function NewTaskPage({
  projectId,
  projects,
  onCreated,
  onWorkspaceChanged,
  onOpenRemote,
}: Props) {
  const [workspaceId, setWorkspaceId] = useState(projectId);
  const [mode, setMode] = useState<Mode>('text');
  const [description, setDescription] = useState('');
  const [file, setFile] = useState<SelectedFile | null>(null);
  const [gitlabProjectId, setGitlabProjectId] = useState('');
  const [issueIid, setIssueIid] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [modeMenuOpen, setModeMenuOpen] = useState(false);
  const modeMenuRef = useRef<HTMLDivElement>(null);

  // 点外部收起“+”来源菜单（与工作台输入器同一交互）。
  useEffect(() => {
    if (!modeMenuOpen) return;
    const close = (event: MouseEvent) => {
      if (!modeMenuRef.current?.contains(event.target as Node)) setModeMenuOpen(false);
    };
    window.addEventListener('mousedown', close);
    return () => window.removeEventListener('mousedown', close);
  }, [modeMenuOpen]);

  useEffect(() => setWorkspaceId(projectId), [projectId]);

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
          projectId: workspaceId, title, description: description.trim(),
        });
        await openWithAutomaticPrd(wi.id, description.trim());
      } else if (mode === 'document') {
        if (!file?.content) {
          throw new Error('请先选择文档');
        }
        const wi = await rpc<{ id: string }>('workitem.importDocument', {
          projectId: workspaceId, filename: file.filename, content: file.content,
        });
        await openWithAutomaticPrd(wi.id);
      } else if (mode === 'issue') {
        if (!gitlabProjectId.trim() || !issueIid.trim()) {
          throw new Error('GitLab 项目 ID 与 Issue IID 必填');
        }
        const wi = await rpc<{ id: string }>('workitem.importIssue', {
          projectId: workspaceId, gitlabProjectId: gitlabProjectId.trim(), issueIid: issueIid.trim(),
        });
        await openWithAutomaticPrd(wi.id, description.trim());
      } else {
        if (!file) {
          throw new Error('请先选择图片');
        }
        // 图片：先创建任务，再作为附件导入（多模态解析由核心标记状态）。
        const wi = await rpc<{ id: string }>('workitem.create', {
          projectId: workspaceId,
          title: deriveTitle(description) || file.filename,
          description: description.trim(),
        });
        await rpc('attachment.import', {
          workItemId: wi.id, filename: file.filename, contentBase64: file.contentBase64,
        });
        await openWithAutomaticPrd(wi.id, description.trim());
      }
    } catch (reason) {
      setError(rpcErrorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const openWithAutomaticPrd = async (workItemId: string, suppliedText = '') => {
    let requirementText = suppliedText;
    if (!requirementText) {
      try {
        const detail = await rpc<{ workItem: { title: string; description: string } }>(
          'workitem.get',
          { workItemId },
        );
        requirementText = [detail.workItem.title, detail.workItem.description]
          .filter(Boolean)
          .join('\n');
      } catch {
        // WorkItem 已创建，自动起草仍可仅依赖其需求修订与项目知识库。
      }
    }

    try {
      const runId = await startPrdDraft(
        workItemId,
        requirementText,
        `auto-prd-${workItemId}`,
      );
      rememberAutomaticPrd(workItemId, { state: 'running', runId });
    } catch (reason) {
      rememberAutomaticPrd(workItemId, {
        state: 'failed',
        error: friendlyAgentError(reason),
      });
    }
    onCreated(workItemId, workspaceId);
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

  // “+”菜单的添加附件：按文件类型自动落到 图片（多模态附件）或 文档 模式。
  const attachFromMenu = async () => {
    setModeMenuOpen(false);
    const selected = await window.sixgates.selectFile();
    if (!selected) {
      return;
    }
    let content: string | undefined;
    if (selected.size < 2 << 20 && /\.(md|markdown|txt)$/i.test(selected.filename)) {
      content = atob(selected.contentBase64);
    }
    setFile({ ...selected, content });
    const isImage = /\.(png|jpe?g|gif|webp|bmp)$/i.test(selected.filename);
    setMode(isImage ? 'image' : 'document');
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
          <p className="sg-hero-sub">提交后会自动创建需求版本并起草 PRD，你只需要审阅和确认。</p>

          <div className="sg-nt-context-row">
            <span>工作区</span>
            <WorkspacePicker
              projects={projects}
              projectId={workspaceId}
              onSelect={(project) => {
                setWorkspaceId(project.id);
                onWorkspaceChanged(project);
              }}
              onCreated={(project) => {
                setWorkspaceId(project.id);
                onWorkspaceChanged(project);
              }}
              onRemote={onOpenRemote}
            />
            <span className="sg-muted">PRD 将结合此工作区的代码与知识库起草</span>
          </div>

          <div className="sg-composer-main sg-nt-composer">
            <textarea
              className="sg-composer-input"
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
                        图片将作为附件导入并进入多模态解析；若当前模型不支持看图，会在解析结果中明确提示。
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

            <div className="sg-composer-bar">
              <div className="sg-composer-bar-left">
                <div className="sg-compose-add" ref={modeMenuRef}>
                  <button
                    className="sg-compose-add-btn"
                    title="添加"
                    aria-label="添加"
                    aria-expanded={modeMenuOpen}
                    onClick={() => setModeMenuOpen((v) => !v)}
                  >
                    <IconPlus size={15} />
                  </button>
                  {modeMenuOpen ? (
                    <div className="sg-compose-add-menu" role="menu">
                      <button
                        className="sg-compose-add-item"
                        role="menuitem"
                        onClick={() => void attachFromMenu()}
                      >
                        <IconPaperclip size={14} />
                        <span>添加附件</span>
                      </button>
                      <button
                        className="sg-compose-add-item"
                        role="menuitem"
                        onClick={() => {
                          setMode('issue');
                          setModeMenuOpen(false);
                        }}
                      >
                        <IconIssue size={14} />
                        <span>导入 GitLab Issue</span>
                      </button>
                    </div>
                  ) : null}
                </div>
                <span className="sg-composer-chip sg-composer-chip--flat">
                  {MODE_CHIPS.find(({ mode: m }) => m === mode)?.icon}
                  来源：{MODE_CHIPS.find(({ mode: m }) => m === mode)?.label}
                </span>
              </div>
              <div className="sg-composer-bar-right">
                <ModelPicker />
                <button
                  className="sg-compose-send"
                  disabled={busy}
                  onClick={() => void submit()}
                  title={busy ? '创建中…' : '创建并进入需求关'}
                  aria-label="创建并进入需求关"
                >
                  <IconSend size={15} />
                </button>
              </div>
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

        </div>
      </div>
    </>
  );
}

function formatSize(bytes: number): string {
  if (bytes >= 1 << 20) return `${(bytes / (1 << 20)).toFixed(1)} MB`;
  if (bytes >= 1 << 10) return `${Math.round(bytes / (1 << 10))} KB`;
  return `${bytes} 字节`;
}
