// 设置页统一页头（页面设计 §2 骨架）：唯一 h1 + 生效范围 + 状态徽标 + 说明 + 主操作。
import type { ReactNode } from 'react';

export function SettingsPageHeader({
  title,
  scope,
  status,
  description,
  actions,
}: {
  title: string;
  scope: '全局' | '项目' | '环境' | '本地';
  status?: ReactNode;
  description: string;
  actions?: ReactNode;
}) {
  return (
    <header className="sg-set-header">
      <div className="sg-set-header-row">
        <div className="sg-set-heading">
          <h1 className="sg-set-title">{title}</h1>
          <span className="sg-set-scope">{scope}</span>
          {status}
        </div>
        {actions ? <div className="sg-set-header-actions">{actions}</div> : null}
      </div>
      <p className="sg-set-description">{description}</p>
    </header>
  );
}
