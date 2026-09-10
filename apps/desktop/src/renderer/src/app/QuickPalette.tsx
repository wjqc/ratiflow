// QuickPalette：任务输入器内的快捷选择浮层——"/" 列技能（skill.activeList），
// "@" 列 Agent（agentProfile.list）。触发、过滤、键盘导航由 useComposerAssignments
// （TaskComposer 公共输入器）管理；本组件只负责渲染列表与点击拾取（受控），
// a11y 用 combobox/listbox 语义。
import { useEffect, useRef } from 'react';

export interface PaletteItem {
  id: string;
  label: string;
  hint: string;
}

interface Props {
  items: PaletteItem[];
  highlight: number;
  emptyText: string;
  onPick: (item: PaletteItem) => void;
}

export function QuickPalette({ items, highlight, emptyText, onPick }: Props) {
  const listRef = useRef<HTMLUListElement>(null);

  // 高亮项滚入可视区（键盘连续下移时不丢焦点上下文）。
  useEffect(() => {
    listRef.current
      ?.querySelectorAll('li')
      .item(highlight)
      ?.scrollIntoView({ block: 'nearest' });
  }, [highlight]);

  return (
    <div className="sg-quick-palette" role="listbox" aria-label="快捷选择">
      {items.length === 0 ? (
        <div className="sg-quick-palette-empty">{emptyText}</div>
      ) : (
        <ul ref={listRef} className="sg-quick-palette-list">
          {items.map((item, index) => (
            <li
              key={item.id}
              role="option"
              aria-selected={index === highlight}
              className={index === highlight ? 'is-active' : ''}
              onMouseDown={(e) => {
                // mousedown 拾取：先于 textarea blur，避免浮层因失焦先关闭。
                e.preventDefault();
                onPick(item);
              }}
            >
              <span className="sg-quick-palette-label">{item.label}</span>
              <span className="sg-quick-palette-hint">{item.hint}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
