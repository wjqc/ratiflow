// S12 记忆列表（参考稿对齐版）：文件式行——图标 + `slug.md` + 相对时间，
// 右侧为该条记忆的注入开关（active=开，归档=关）。不区分类型、不展示状态标签
// （详情/状态在抽屉内查看）。loading 出 5 行 skeleton；语义化 list/button。
import type { MemoryListItem } from '../types';
import { SettingsToggle } from './SettingsRow';

const STATUS_TONE: Record<string, string> = {
  proposed: 'sg-status--pending',
  conflicted: 'sg-status--error',
  rejected: 'sg-status--disabled',
  purged: 'sg-status--disabled',
};

export function MemoryList({
  items,
  loading,
  activeId,
  onSelect,
  onToggleActive,
  empty,
}: {
  items: MemoryListItem[];
  loading: boolean;
  activeId: string | null;
  onSelect: (id: string) => void;
  onToggleActive: (item: MemoryListItem) => void;
  empty?: React.ReactNode;
}) {
  if (loading) {
    return (
      <ul className="sg-memory-list" aria-label="项目记忆列表加载中" aria-busy="true">
        {[1, 2, 3, 4, 5].map((i) => (
          <li key={i} className="sg-memory-row sg-memory-row--skeleton" aria-hidden="true">
            <span className="sg-memory-skel-line" />
            <span className="sg-memory-skel-line sg-memory-skel-line--short" />
          </li>
        ))}
      </ul>
    );
  }
  if (items.length === 0) {
    return <div className="sg-memory-empty">{empty ?? '暂无记忆'}</div>;
  }
  return (
    <ul className="sg-memory-list" aria-label="项目记忆列表">
      {items.map((item) => {
        const active = item.status === 'active';
        // 开关仅对 active/archived 有意义；其余状态（待确认/冲突）锁定，去抽屉裁决。
        const switchable = item.status === 'active' || item.status === 'archived';
        return (
          <li
            key={item.id}
            className={`sg-memory-row ${item.id === activeId ? 'sg-memory-row--active' : ''}`}
            aria-current={item.id === activeId ? 'true' : undefined}
          >
            <span className="sg-memory-fileicon" aria-hidden="true">
              <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
                <path
                  d="M4 1.5h5.2L13 5.3V14a.9.9 0 0 1-.9.9H4a.9.9 0 0 1-.9-.9V2.4c0-.5.4-.9.9-.9Z"
                  stroke="currentColor"
                  strokeWidth="1.2"
                />
                <path d="M9 1.8V5h3.2" stroke="currentColor" strokeWidth="1.2" />
              </svg>
            </span>
            <button
              type="button"
              id={`sg-memory-row-${item.id}`}
              className="sg-memory-row-main"
              onClick={() => onSelect(item.id)}
              aria-label={`打开记忆 ${item.slug}.md`}
            >
              <span className="sg-memory-row-title">{item.slug}.md</span>
              <span className="sg-memory-row-meta">
                {item.title && item.title !== item.slug ? `${item.title} · ` : ''}
                {formatTime(item.updatedAt)}
                {item.status !== 'active' && item.status !== 'archived' ? (
                  <span className={`sg-status ${STATUS_TONE[item.status] ?? ''}`}>
                    {item.status === 'proposed' ? '待确认' : item.status === 'conflicted' ? '冲突' : item.status}
                  </span>
                ) : null}
              </span>
            </button>
            <SettingsToggle
              label={`启用注入：${item.slug}.md`}
              checked={active}
              disabled={!switchable}
              onChange={() => onToggleActive(item)}
            />
          </li>
        );
      })}
    </ul>
  );
}

export function formatTime(iso: string): string {
  const t = new Date(iso);
  if (Number.isNaN(t.getTime())) return iso;
  const diffMs = Date.now() - t.getTime();
  const min = Math.floor(diffMs / 60000);
  if (min < 1) return '刚刚';
  if (min < 60) return `${min} 分钟前`;
  const hour = Math.floor(min / 60);
  if (hour < 24) return `${hour} 小时前`;
  const hhmm = `${String(t.getHours()).padStart(2, '0')}:${String(t.getMinutes()).padStart(2, '0')}`;
  const week = ['周日', '周一', '周二', '周三', '周四', '周五', '周六'][t.getDay()];
  if (hour < 24 * 7) return `${week} ${hhmm}`;
  return `${t.getMonth() + 1} 月 ${t.getDate()} 日 ${hhmm}`;
}
