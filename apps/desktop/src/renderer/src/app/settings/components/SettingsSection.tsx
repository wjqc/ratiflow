// 设置页分组区块：<section aria-labelledby> + h2，保证可访问性与结构一致。
import { useId, type ReactNode } from 'react';

export function SettingsSection({
  title,
  description,
  children,
  actions,
}: {
  title: string;
  description?: string;
  children: ReactNode;
  actions?: ReactNode;
}) {
  const headingId = useId();
  return (
    <section className="sg-set-section" aria-labelledby={headingId}>
      <div className="sg-set-section-head">
        <h2 id={headingId} className="sg-section-title">{title}</h2>
        {actions}
      </div>
      {description ? <p className="sg-hint" style={{ marginTop: -4 }}>{description}</p> : null}
      {children}
    </section>
  );
}
