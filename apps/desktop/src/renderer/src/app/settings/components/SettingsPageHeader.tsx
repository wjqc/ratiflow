// 设置页统一页头（页面设计 §2 骨架）：唯一 h1 + 说明 + 主操作。即存模式无保存状态徽章；失败反馈由页内错误横幅承担。
import type { ReactNode } from 'react';

export function SettingsPageHeader({
  title,
  description,
  actions,
}: {
  title: string;
  description: string;
  actions?: ReactNode;
}) {
  return (
    <header className="sg-set-header">
      <div className="sg-set-header-row">
        <div className="sg-set-heading">
          <h1 className="sg-set-title">{title}</h1>
        </div>
        {actions ? <div className="sg-set-header-actions">{actions}</div> : null}
      </div>
      <p className="sg-set-description">{description}</p>
    </header>
  );
}
