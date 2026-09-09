import { useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { rpc, rpcErrorMessage } from '../rpc/client';
import {
  IconChevronDown,
  IconCheck,
  IconDoc,
  IconImage,
  IconIssue,
  IconLogo,
  IconPaperclip,
  IconPlus,
  IconSend,
  IconText,
} from '../components/Icons';
import { ModelPicker } from './ModelPicker';
import { QuickPalette, TriggerHintMenu } from './QuickPalette';
import type { PaletteItem } from './QuickPalette';
import {
  friendlyAgentError,
  rememberAutomaticPrd,
  startPrdDraft,
} from './prdDraft';
import { WorkspacePicker } from './WorkspacePicker';
import type { Project } from './ProjectSidebar';

type Mode = 'text' | 'document' | 'issue' | 'image';

// "/" 选择的技能（冻结 active 版本；提交时经 workitem.create/updateRunDefaults 落任务默认面）。
interface SelectedSkill {
  skillId: string;
  name: string;
  versionId: string;
  versionNo: number;
}

// "@" 指派的 Agent（冻结当前最新版本；选路五级中的 workitem_default 档）。
interface SelectedAgent {
  profileId: string;
  name: string;
  versionId: string;
}

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
  const [flowOpen, setFlowOpen] = useState(false);
  const [templateOpen, setTemplateOpen] = useState(false);
  const templateMenuRef = useRef<HTMLDivElement>(null);
  const templateTriggerRef = useRef<HTMLButtonElement>(null);
  const modeMenuRef = useRef<HTMLDivElement>(null);
  // 关卡模板（配置化）：有 active 版本的模板可选；默认 six-gate-default。
  const [templates, setTemplates] = useState<{ key: string; name: string }[]>([]);
  const [templateKey, setTemplateKey] = useState('six-gate-default');
  // 所选模板的关卡流预览：默认模板用本地常量（免请求），自定义模板读激活版本定义。
  const [flowGates, setFlowGates] = useState<Array<{ name: string; sub: string }>>(GATE_FLOW);
  // "/" 与 "@"：快捷选择浮层（触发位置 start、过滤串、高亮序）与已选胶囊。
  const [picker, setPicker] = useState<{ kind: 'skill' | 'agent'; start: number } | null>(null);
  const [pickerQuery, setPickerQuery] = useState('');
  const [pickerIndex, setPickerIndex] = useState(0);
  // 聚焦空输入框时的触发提示菜单（附件 / @ Agent / / 技能）：点选直接唤起。
  const [hintOpen, setHintOpen] = useState(false);
  const [skillOptions, setSkillOptions] = useState<Array<PaletteItem & SelectedSkill>>([]);
  const [agentOptions, setAgentOptions] = useState<Array<PaletteItem & SelectedAgent>>([]);
  const [selectedSkills, setSelectedSkills] = useState<SelectedSkill[]>([]);
  const [selectedAgent, setSelectedAgent] = useState<SelectedAgent | null>(null);

  useEffect(() => {
    if (!templateOpen) return;
    const close = (event: MouseEvent) => {
      if (!templateMenuRef.current?.contains(event.target as Node)) setTemplateOpen(false);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        setTemplateOpen(false);
        templateTriggerRef.current?.focus();
      }
    };
    window.addEventListener('mousedown', close);
    window.addEventListener('keydown', escape);
    return () => {
      window.removeEventListener('mousedown', close);
      window.removeEventListener('keydown', escape);
    };
  }, [templateOpen]);

  useEffect(() => {
    let cancelled = false;
    rpc<{ items: { key: string; name: string; versions: { status: string }[] }[] }>(
      'workflowTemplate.list',
      {},
    )
      .then((r) => {
        if (cancelled) return;
        setTemplates(
          (r.items ?? [])
            .filter((t) => t.versions?.some((v) => v.status === 'active'))
            .map((t) => ({ key: t.key, name: t.name })),
        );
      })
      .catch(() => {
        // 模板域不可用（显式关闭/老库）→ 隐藏选择器，默认模板照常。
        if (!cancelled) setTemplates([]);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (templateKey === 'six-gate-default') {
      setFlowGates(GATE_FLOW);
      return;
    }
    let cancelled = false;
    rpc<{ activeVersion?: { gates?: { gate_id: string; title: string; purpose?: string }[] } }>(
      'workflowTemplate.get',
      { templateId: templateKey },
    )
      .then((r) => {
        if (cancelled) return;
        const gates = r.activeVersion?.gates ?? [];
        if (gates.length > 0) {
          setFlowGates(gates.map((g) => ({ name: g.title || g.gate_id, sub: g.purpose ?? '' })));
        }
      })
      .catch(() => {
        // 读取失败保持当前预览（创建仍会冻结该模板激活版本）。
      });
    return () => {
      cancelled = true;
    };
  }, [templateKey]);

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

  // ---------------- "/" 技能 与 "@" Agent 快捷选择 ----------------

  // 选项按需加载（首次触发时拉取，之后复用缓存；失败静默——浮层显示空态）。
  useEffect(() => {
    if (picker?.kind !== 'skill' || skillOptions.length > 0) return;
    let cancelled = false;
    rpc<{ items: Array<{ skillId: string; name: string; versionId: string; versionNo: number }> }>(
      'skill.activeList',
      {},
    )
      .then((r) => {
        if (cancelled) return;
        setSkillOptions(
          (r.items ?? []).map((s) => ({
            id: s.versionId,
            label: s.name,
            hint: `v${s.versionNo}`,
            skillId: s.skillId,
            name: s.name,
            versionId: s.versionId,
            versionNo: s.versionNo,
          })),
        );
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [picker?.kind, skillOptions.length]);

  useEffect(() => {
    if (picker?.kind !== 'agent' || agentOptions.length > 0) return;
    let cancelled = false;
    rpc<{
      items: Array<{
        id: string;
        name: string;
        enabled?: boolean;
        versions?: Array<{ id: string; versionNo: number }>;
      }>;
    }>('agentProfile.list', {})
      .then((r) => {
        if (cancelled) return;
        setAgentOptions(
          (r.items ?? [])
            .filter((p) => p.enabled !== false && (p.versions?.length ?? 0) > 0)
            .map((p) => {
              const latest = p.versions!.reduce((a, b) => (b.versionNo > a.versionNo ? b : a));
              return {
                id: p.id,
                label: p.name,
                hint: 'Agent',
                profileId: p.id,
                name: p.name,
                versionId: latest.id,
              };
            }),
        );
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [picker?.kind, agentOptions.length]);

  const closePicker = () => {
    setPicker(null);
    setPickerQuery('');
    setPickerIndex(0);
  };

  // 菜单行点选：把触发符插入正文末尾并直接唤起对应浮层（@ Agent / / 技能）。
  const insertTrigger = (trigger: '/' | '@') => {
    const base = description.trimEnd();
    const text = base ? `${base} ${trigger}` : trigger;
    setDescription(text);
    setHintOpen(false);
    setPicker({ kind: trigger === '/' ? 'skill' : 'agent', start: text.length - 1 });
    setPickerQuery('');
    setPickerIndex(0);
  };

  const hintRows = [
    {
      key: 'attach',
      symbol: '＋',
      label: '添加附件（图片或文档）',
      onPick: () => {
        setHintOpen(false);
        void attachFromMenu();
      },
    },
    {
      key: 'agent',
      symbol: '@',
      label: <>使用 <kbd className="sg-quick-kbd">@</kbd> 指派 Agent</>,
      onPick: () => insertTrigger('@'),
    },
    {
      key: 'skill',
      symbol: '/',
      label: <>使用 <kbd className="sg-quick-kbd">/</kbd> 选择技能</>,
      onPick: () => insertTrigger('/'),
    },
  ];

  // 触发检测：行首/空白后的 "/" 或 "@" 开启浮层；已开启时随输入过滤，遇空白/删除触发符关闭。
  const onDescriptionChange = (value: string, caret: number) => {
    setDescription(value);
    if (value) setHintOpen(false);
    if (picker) {
      if (caret <= picker.start) {
        closePicker();
        return;
      }
      const query = value.slice(picker.start + 1, caret);
      if (query.includes(' ') || query.includes('\n')) {
        closePicker();
        return;
      }
      setPickerQuery(query);
      setPickerIndex(0);
      return;
    }
    if (caret < 1) return;
    const trigger = value[caret - 1];
    if (trigger !== '/' && trigger !== '@') return;
    const prev = caret >= 2 ? value[caret - 2] : '';
    if (caret > 1 && prev !== ' ' && prev !== '\n') return;
    setHintOpen(false);
    setPicker({ kind: trigger === '/' ? 'skill' : 'agent', start: caret - 1 });
    setPickerQuery('');
    setPickerIndex(0);
  };

  const pickerItems = (picker?.kind === 'skill' ? skillOptions : agentOptions).filter((item) =>
    pickerQuery ? item.label.toLowerCase().includes(pickerQuery.toLowerCase()) : true,
  );

  const pickFromPalette = (item: PaletteItem) => {
    if (!picker) return;
    // 从正文摘除触发片段（"/xxx"），选择以胶囊呈现而非落正文。
    const caret = picker.start + 1 + pickerQuery.length;
    setDescription((prev) => prev.slice(0, picker.start) + prev.slice(caret));
    if (picker.kind === 'skill') {
      const skill = item as PaletteItem & SelectedSkill;
      setSelectedSkills((prev) =>
        prev.some((s) => s.versionId === skill.versionId) ? prev : [...prev, skill],
      );
    } else {
      const agent = item as PaletteItem & SelectedAgent;
      setSelectedAgent(agent);
    }
    closePicker();
  };

  const onDescriptionKeyDown = (event: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (hintOpen && event.key === 'Escape') {
      event.preventDefault();
      setHintOpen(false);
      return;
    }
    if (!picker) return;
    if (event.key === 'ArrowDown') {
      event.preventDefault();
      setPickerIndex((i) => (i + 1) % Math.max(pickerItems.length, 1));
    } else if (event.key === 'ArrowUp') {
      event.preventDefault();
      setPickerIndex((i) => (i - 1 + Math.max(pickerItems.length, 1)) % Math.max(pickerItems.length, 1));
    } else if (event.key === 'Enter' && pickerItems.length > 0) {
      event.preventDefault();
      pickFromPalette(pickerItems[pickerIndex % pickerItems.length]);
    } else if (event.key === 'Escape') {
      event.preventDefault();
      closePicker();
    }
  };

  // 任务级默认面参数（"/" 技能 + "@" Agent；提交时统一走 create 参数或事后 updateRunDefaults）。
  const runDefaultsParams = (): Record<string, unknown> => ({
    ...(selectedAgent ? { agentProfileVersionId: selectedAgent.versionId } : {}),
    ...(selectedSkills.length ? { skillVersionIds: selectedSkills.map((s) => s.versionId) } : {}),
  });

  // 文档/Issue 导入路径没有 create 参数位：创建后统一补一次任务级默认面。
  const applyRunDefaults = async (workItemId: string) => {
    const params = runDefaultsParams();
    if (Object.keys(params).length === 0) return;
    await rpc('workitem.updateRunDefaults', { workItemId, ...params });
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
          projectId: workspaceId, title, description: description.trim(), templateId: templateKey,
          ...runDefaultsParams(),
        });
        if (templateKey === 'six-gate-default') {
          await openWithAutomaticPrd(wi.id, description.trim());
        } else {
          // 自定义模板首关未必是需求关：不自动起草 PRD，直接进工作台按模板关卡推进。
          onCreated(wi.id, workspaceId);
        }
      } else if (mode === 'document') {
        if (!file?.content) {
          throw new Error('请先选择文档');
        }
        const wi = await rpc<{ id: string }>('workitem.importDocument', {
          projectId: workspaceId, filename: file.filename, content: file.content,
        });
        await applyRunDefaults(wi.id);
        await openWithAutomaticPrd(wi.id);
      } else if (mode === 'issue') {
        if (!gitlabProjectId.trim() || !issueIid.trim()) {
          throw new Error('GitLab 项目 ID 与 Issue IID 必填');
        }
        const wi = await rpc<{ id: string }>('workitem.importIssue', {
          projectId: workspaceId, gitlabProjectId: gitlabProjectId.trim(), issueIid: issueIid.trim(),
        });
        await applyRunDefaults(wi.id);
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
          templateId: templateKey,
          ...runDefaultsParams(),
        });
        await rpc('attachment.import', {
          workItemId: wi.id, filename: file.filename, contentBase64: file.contentBase64,
        });
        if (templateKey === 'six-gate-default') {
          await openWithAutomaticPrd(wi.id, description.trim());
        } else {
          onCreated(wi.id, workspaceId);
        }
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
    const selected = await window.ratiflow.selectFile();
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
    const selected = await window.ratiflow.selectFile();
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
      <div className="sg-scroll sg-nt-scroll">
        <div className="sg-nt-wrap sg-nt-wrap--centered">
          <div className="sg-nt-hero">
            <IconLogo size={42} className="sg-nt-hero-logo" />
            <h2 className="sg-hero-title">你想在 ratiflow 中完成什么？</h2>
          </div>

          <div className="sg-nt-composer-shell">
            <div className="sg-nt-context-pills">
              <div className="sg-nt-context-main">
                <div className="sg-nt-pill" title="PRD 将结合此工作区的代码与知识库起草">
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
                </div>
                <span className="sg-nt-context-divider" />
                <span className="sg-nt-local-context">
                  <span className="sg-nt-local-dot" />
                  本地
                </span>
                {templates.length > 0 && (mode === 'text' || mode === 'image') ? (
                  <>
                    <span className="sg-nt-context-divider" />
                    <div className="sg-nt-pill sg-nt-template-picker" ref={templateMenuRef}>
                      <button
                        type="button"
                        ref={templateTriggerRef}
                        className="sg-nt-template-trigger"
                        aria-label="关卡模板"
                        aria-haspopup="dialog"
                        aria-expanded={templateOpen}
                        onClick={() => setTemplateOpen((value) => !value)}
                      >
                        <span>{templates.find((t) => t.key === templateKey)?.name ?? '默认六关'}</span>
                        <IconChevronDown size={13} />
                      </button>
                      {templateOpen ? (
                        <div className="sg-nt-template-menu" role="dialog" aria-label="选择关卡模板">
                          <div className="sg-nt-template-menu-label">关卡模板</div>
                          {templates.map((t) => (
                            <button
                              type="button"
                              key={t.key}
                              aria-pressed={t.key === templateKey}
                              autoFocus={t.key === templateKey}
                              onClick={() => {
                                setTemplateKey(t.key);
                                setTemplateOpen(false);
                                templateTriggerRef.current?.focus();
                              }}
                            >
                              <span>{t.name}</span>
                              {t.key === templateKey ? <IconCheck size={14} /> : null}
                            </button>
                          ))}
                        </div>
                      ) : null}
                    </div>
                  </>
                ) : null}
              </div>
              <button
                type="button"
                className="sg-nt-flow-toggle"
                aria-expanded={flowOpen}
                onClick={() => setFlowOpen((value) => !value)}
              >
                查看流程
                <IconChevronDown size={13} className={flowOpen ? 'is-open' : ''} />
              </button>
            </div>

            <div className="sg-composer-main sg-nt-composer">
            {(selectedAgent || selectedSkills.length > 0) && (
              <div className="sg-composer-chips" style={{ marginBottom: 8 }}>
                {selectedAgent ? (
                  <span className="sg-composer-chip sg-composer-chip--active" title={`Agent @ ${selectedAgent.name}`}>
                    @ {selectedAgent.name}
                    <button
                      type="button"
                      aria-label={`移除指派 Agent ${selectedAgent.name}`}
                      className="sg-chip-remove"
                      onClick={() => setSelectedAgent(null)}
                    >×</button>
                  </span>
                ) : null}
                {selectedSkills.map((s) => (
                  <span key={s.versionId} className="sg-composer-chip" title={`技能 / ${s.name}（v${s.versionNo}）`}>
                    / {s.name}
                    <button
                      type="button"
                      aria-label={`移除技能 ${s.name}`}
                      className="sg-chip-remove"
                      onClick={() => setSelectedSkills((prev) => prev.filter((x) => x.versionId !== s.versionId))}
                    >×</button>
                  </span>
                ))}
              </div>
            )}
            <textarea
              className="sg-composer-input"
              placeholder={
                mode === 'issue'
                  ? '补充说明（可选）…'
                  : '描述你的需求…'
              }
              value={description}
              onFocus={() => {
                if (!description) setHintOpen(true);
              }}
              onBlur={() => setHintOpen(false)}
              onChange={(e) => {
                // jsdom 合成事件不携带光标位（selectionStart 恒 0）：非空文本回退按末位处理，
                // 真实浏览器走真实光标。
                const value = e.target.value;
                const caret = e.target.selectionStart || value.length;
                onDescriptionChange(value, caret);
              }}
              onKeyDown={onDescriptionKeyDown}
              aria-label="需求描述"
            />
            {hintOpen && !picker ? <TriggerHintMenu rows={hintRows} /> : null}
            {picker ? (
              <QuickPalette
                items={pickerItems}
                highlight={pickerIndex % Math.max(pickerItems.length, 1)}
                emptyText={picker.kind === 'skill' ? '无匹配技能（需先在设置中激活）' : '无匹配 Agent'}
                onPick={pickFromPalette}
              />
            ) : null}

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
          </div>

          {error ? (
            <div className="sg-banner sg-banner--error" role="alert" style={{ marginTop: 12 }}>
              {error}
            </div>
          ) : null}

          {flowOpen ? (
            <div className="sg-nt-flow-panel">
              <div className="sg-nt-flow-label">
                交付流程（{flowGates.length} 关{templateKey !== 'six-gate-default' ? ' · 按所选模板' : ''}）
              </div>
              <div className={`sg-nt-flow ${flowGates.length > 6 ? 'sg-nt-flow--dense' : ''}`}>
                {flowGates.map((g, i) => (
                  <div className="sg-nt-step" key={`${g.name}:${i}`}>
                    {i > 0 && <span className="sg-nt-step-sep">→</span>}
                    <span className={`sg-nt-step-num ${i === 0 ? '' : 'sg-nt-step-num--idle'}`}>
                      {i + 1}
                    </span>
                    <div style={{ minWidth: 0 }}>
                      <div className="sg-nt-step-name">{g.name}</div>
                      {g.sub ? <div className="sg-nt-step-sub">{g.sub}</div> : null}
                    </div>
                  </div>
                ))}
              </div>
            </div>
          ) : null}

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
