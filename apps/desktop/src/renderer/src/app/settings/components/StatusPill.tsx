// 设置页统一状态徽标（页面设计 §4.2：图标+文字，不只用颜色）。
type PillKind = 'ready' | 'pending' | 'checking' | 'error' | 'readonly' | 'partial' | 'dev';

const PILL_META: Record<PillKind, { cls: string; icon: string; label: string }> = {
  ready: { cls: 'sg-status--passed', icon: '✓', label: '已就绪' },
  pending: { cls: 'sg-status--pending', icon: '○', label: '待配置' },
  checking: { cls: 'sg-status--running', icon: '…', label: '检查中' },
  error: { cls: 'sg-status--error', icon: '!', label: '异常' },
  readonly: { cls: 'sg-status--disabled', icon: '—', label: '只读' },
  partial: { cls: 'sg-status--pending', icon: '◐', label: '部分可用' },
  dev: { cls: 'sg-status--dev', icon: '…', label: '开发中' },
};

export function StatusPill({ kind, label }: { kind: PillKind; label?: string }) {
  const meta = PILL_META[kind];
  return (
    <span className={`sg-status ${meta.cls}`}>
      <span aria-hidden>{meta.icon}</span>
      {label ?? meta.label}
    </span>
  );
}
