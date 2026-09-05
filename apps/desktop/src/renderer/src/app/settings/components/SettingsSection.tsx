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
        <div>
          <h2 id={headingId} className="sg-section-title">{title}</h2>
          {description ? <p className="sg-set-section-description">{description}</p> : null}
        </div>
        {actions}
      </div>
      <div className="sg-set-section-body">{children}</div>
    </section>
  );
}
