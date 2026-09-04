// S12 记忆列表：文件式行（slug/标题 + 类型·状态·置顶·来源·更新时间）。
// loading 出 5 行 skeleton；语义化 list/button；状态有中文文字不只靠颜色（§3.7）。
import type { MemoryListItem } from '../types';
import { MEMORY_KIND_LABEL, MEMORY_STATUS_LABEL } from '../types';

const STATUS_TONE: Record<string, string> = {
  proposed: 'sg-status--pending',
  active: 'sg-status--passed',
  conflicted: 'sg-status--error',
  archived: 'sg-status--disabled',
  rejected: 'sg-status--disabled',
  purged: 'sg-status--disabled',
};

export function MemoryList({
  items,
  loading,
  activeId,
  onSelect,
  empty,
}: {
  items: MemoryListItem[];
  loading: boolean;
  activeId: string | null;
  onSelect: (id: string) => void;
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
      {items.map((item) => (
        <li key={item.id}>
          <button
            type="button"
            id={`sg-memory-row-${item.id}`}
            className={`sg-memory-row ${item.id === activeId ? 'sg-memory-row--active' : ''}`}
            aria-current={item.id === activeId ? 'true' : undefined}
            onClick={() => onSelect(item.id)}
          >
            <span className="sg-memory-row-main">
              <span className="sg-memory-row-title">
                {item.pinned ? (
                  <>
                    <span aria-hidden>📌</span>
                    <span className="sg-sr-only">（置顶）</span>{' '}
                  </>
                ) : null}
                {item.title || item.slug}
              </span>
              <span className="sg-memory-row-meta">
                <span className="sg-memory-chip">{MEMORY_KIND_LABEL[item.kind] ?? item.kind}</span>
                <span className={`sg-status ${STATUS_TONE[item.status] ?? ''}`}>
                  <span aria-hidden>•</span>
                  {MEMORY_STATUS_LABEL[item.status] ?? item.status}
                </span>
                <span>来源 {item.sourceCount}</span>
                <span>· {formatTime(item.updatedAt)}</span>
              </span>
            </span>
          </button>
        </li>
      ))}
    </ul>
  );
}

export function formatTime(iso: string): string {
  const t = new Date(iso);
  if (Number.isNaN(t.getTime())) return iso;
  return `${t.getMonth() + 1} 月 ${t.getDate()} 日 ${String(t.getHours()).padStart(2, '0')}:${String(t.getMinutes()).padStart(2, '0')}`;
}
