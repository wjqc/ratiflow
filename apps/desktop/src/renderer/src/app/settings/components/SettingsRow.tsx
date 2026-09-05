// 设置行（统一行式布局，对齐设置中心参考稿）：标题+描述在左、控件右对齐；
// 同一 sg-setting-list 内多行自动加分隔线。列表页（表格）不使用本组件。
import type { ReactNode } from 'react';

export function SettingsRow({ title, htmlFor, description, children, narrow }: {
  title: string;
  /** 传入时标题以 <label htmlFor> 与控件关联（表单输入）；否则仅展示文本（开关用 aria-label 关联）。 */
  htmlFor?: string;
  description?: ReactNode;
  children: ReactNode;
  /** 窄控件（数字输入等） */
  narrow?: boolean;
}) {
  return (
    <div className="sg-set-item">
      <div className="sg-set-item-copy">
        {htmlFor
          ? <label htmlFor={htmlFor} className="sg-set-item-title">{title}</label>
          : <span className="sg-set-item-title">{title}</span>}
        {description != null ? <small className="sg-set-item-desc">{description}</small> : null}
      </div>
      <div className={`sg-set-item-control${narrow ? ' sg-set-item-control--narrow' : ''}`}>{children}</div>
    </div>
  );
}

/** 胶囊开关（sg-setting-toggle）：label 经 aria-label 关联。 */
export function SettingsToggle({ label, checked, onChange, disabled = false }: {
  label: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <label className="sg-setting-toggle">
      <input
        type="checkbox"
        role="switch"
        aria-checked={checked}
        checked={checked}
        disabled={disabled}
        onChange={(event) => onChange(event.target.checked)}
        aria-label={label}
      />
      <span aria-hidden />
    </label>
  );
}
