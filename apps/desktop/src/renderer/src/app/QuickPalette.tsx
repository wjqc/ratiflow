// QuickPalette：任务输入器内的快捷选择浮层——"/" 列技能（skill.activeList），
// "@" 列 Agent（agentProfile.list）。触发、过滤、键盘导航由调用方（NewTaskPage）
// 管理；本组件只负责渲染列表与点击拾取（受控），a11y 用 combobox/listbox 语义。
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

export interface HintRow {
  key: string;
  symbol: React.ReactNode;
  label: React.ReactNode;
  onPick: () => void;
}

// TriggerHintMenu：聚焦空输入框时的触发提示菜单（参照 ZCode 首页输入器）：
// 附件 / @ Agent / / 技能 各一行，点选直接唤起对应面板或文件选择器。
interface HintProps {
  rows: HintRow[];
}

export function TriggerHintMenu({ rows }: HintProps) {
  return (
    <div className="sg-quick-palette sg-quick-hint" role="menu" aria-label="输入提示">
      {rows.map((row) => (
        <button
          key={row.key}
          type="button"
          role="menuitem"
          className="sg-quick-hint-row"
          onMouseDown={(e) => {
            // 先于 textarea blur 处理，避免菜单因失焦先关闭。
            e.preventDefault();
            row.onPick();
          }}
        >
          <span className="sg-quick-hint-icon" aria-hidden="true">{row.symbol}</span>
          {row.label}
        </button>
      ))}
    </div>
  );
}
