// TaskComposer：新建任务页与工作台共用的任务输入器公共组件。
// 视觉取自新建任务页输入器（sg-composer-card 圆角大卡）；交互取自工作台：
// @ / / 触发与失焦收起走 useComposerAssignments，"+" 菜单点外部收起，
// 菜单条目与底栏"来源"胶囊由页面注入（两页的附件/引用域不同）。
import { useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { IconPlus, IconSend } from '../components/Icons';
import { ModelPicker } from './ModelPicker';
import { QuickPalette } from './QuickPalette';
import type { ComposerAssignments } from './useComposerAssignments';

interface Props {
  /** 受控正文（页面持有；浮层拾取经 assignments 改写）。 */
  text: string;
  assignments: ComposerAssignments;
  placeholder: string;
  ariaLabel: string;
  busy?: boolean;
  sendDisabled?: boolean;
  sendTitle: string;
  sendAriaLabel: string;
  onSend: () => void;
  /** "+" 菜单内容；close 收起菜单，是否收起由条目自行决定（如二级视图保持打开）。 */
  renderAddMenu: (close: () => void) => ReactNode;
  /** 打开 "+" 菜单前的复位钩子（工作台用它把二级视图重置回根视图）。 */
  onAddMenuOpen?: () => void;
  /** 底栏左侧“来源/上下文”胶囊。 */
  contextChip: ReactNode;
  /** 卡片内正文上方插槽（工作台的附件状态行）。 */
  aboveInput?: ReactNode;
  /** 正文与底栏之间插槽（新建任务页的文档/Issue 补充表单）。 */
  belowInput?: ReactNode;
}

export default function TaskComposer({
  text,
  assignments,
  placeholder,
  ariaLabel,
  busy,
  sendDisabled,
  sendTitle,
  sendAriaLabel,
  onSend,
  renderAddMenu,
  onAddMenuOpen,
  contextChip,
  aboveInput,
  belowInput,
}: Props) {
  const [addMenuOpen, setAddMenuOpen] = useState(false);
  const addMenuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!addMenuOpen) return;
    const close = (event: MouseEvent) => {
      if (!addMenuRef.current?.contains(event.target as Node)) setAddMenuOpen(false);
    };
    window.addEventListener('mousedown', close);
    return () => window.removeEventListener('mousedown', close);
  }, [addMenuOpen]);

  return (
    <div className="sg-composer-main sg-composer-card">
      {assignments.agent || assignments.skills.length ? (
        <div className="sg-composer-chips">
          {assignments.agent ? (
            <span className="sg-composer-chip sg-composer-chip--active" title={`Agent @ ${assignments.agent.label}`}>
              @ {assignments.agent.label}
              <button
                type="button"
                aria-label={`移除指派 Agent ${assignments.agent.label}`}
                className="sg-chip-remove"
                disabled={busy}
                onClick={assignments.removeAgent}
              >×</button>
            </span>
          ) : null}
          {assignments.skills.map((skill) => (
            <span key={skill.versionId} className="sg-composer-chip" title={`技能 / ${skill.label}${skill.hint ? `（${skill.hint}）` : ''}`}>
              / {skill.label}
              <button
                type="button"
                aria-label={`移除技能 ${skill.label}`}
                className="sg-chip-remove"
                disabled={busy}
                onClick={() => assignments.removeSkill(skill.versionId)}
              >×</button>
            </span>
          ))}
        </div>
      ) : null}
      {aboveInput}
      <textarea
        className="sg-composer-input"
        ref={assignments.inputRef}
        readOnly={busy}
        placeholder={placeholder}
        rows={2}
        value={text}
        onChange={(e) => {
          setAddMenuOpen(false);
          // jsdom 合成事件不携带光标位（selectionStart 恒 0）：非空文本回退按末位处理，
          // 真实浏览器走真实光标。
          assignments.change(e.target.value, e.target.selectionStart || e.target.value.length);
        }}
        onBlur={assignments.close}
        onKeyDown={(e) => {
          if (assignments.onKeyDown(e)) return;
          if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            onSend();
          }
        }}
        aria-label={ariaLabel}
      />
      {assignments.picker ? (
        <QuickPalette
          items={assignments.items}
          highlight={assignments.highlight}
          emptyText={assignments.emptyText}
          onPick={assignments.pick}
        />
      ) : null}
      {belowInput}
      <div className="sg-composer-bar">
        <div className="sg-composer-bar-left">
          <div className="sg-compose-add" ref={addMenuRef}>
            <button
              type="button"
              className="sg-compose-add-btn"
              title="添加"
              aria-label="添加"
              aria-expanded={addMenuOpen}
              disabled={busy}
              onClick={() => {
                assignments.close();
                setAddMenuOpen((v) => {
                  if (!v) onAddMenuOpen?.();
                  return !v;
                });
              }}
            >
              <IconPlus size={15} />
            </button>
            {addMenuOpen ? (
              <div className="sg-compose-add-menu" role="menu" aria-label="添加">
                {renderAddMenu(() => setAddMenuOpen(false))}
              </div>
            ) : null}
          </div>
          {contextChip}
        </div>
        <div className="sg-composer-bar-right">
          <ModelPicker />
          <button
            type="button"
            className="sg-compose-send"
            title={sendTitle}
            aria-label={sendAriaLabel}
            disabled={sendDisabled}
            onClick={onSend}
          >
            <IconSend size={15} />
          </button>
        </div>
      </div>
    </div>
  );
}

/** "+" 菜单标准条目行（图标/符号 + 文案），两页共用保证菜单观感一致。 */
export function ComposerMenuItem({ icon, label, onSelect }: { icon?: ReactNode; label: string; onSelect: () => void }) {
  return (
    <button type="button" className="sg-compose-add-item" role="menuitem" onClick={onSelect}>
      {icon}
      <span>{label}</span>
    </button>
  );
}
