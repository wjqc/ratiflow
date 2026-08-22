// 契约待提交面板：F2/F3 页面统一结构（功能范围 + 等待契约 + 替代路径）。
// 交付要求 #7：不伪造"已配置"，明示等待的 RPC 方法名。
import { IconInfo } from '../../../components/Icons';
import { StatusPill } from './StatusPill';

export function ContractPending({
  features,
  awaiting,
  fallback,
}: {
  /** 页面设计文档给出的功能范围。 */
  features: string[];
  /** 等待 ZCode 提交的 RPC 契约方法名。 */
  awaiting: string[];
  /** 当前替代路径（env 装配 / 项目页 / 无）。 */
  fallback: string;
}) {
  return (
    <div className="sg-set-pending" aria-live="polite">
      <div className="sg-row" style={{ gap: 10 }}>
        <IconInfo size={16} style={{ color: 'var(--sg-action-primary)', flexShrink: 0 }} />
        <StatusPill kind="dev" />
        <span className="sg-hint">接口契约待 ZCode 提交；到位前本页展示结构与等待项，不产生任何写入。</span>
      </div>
      <div className="sg-set-pending-grid">
        <div>
          <h3 className="sg-set-pending-title">功能范围（规划）</h3>
          <ul className="sg-set-list">
            {features.map((f) => (
              <li key={f}>{f}</li>
            ))}
          </ul>
        </div>
        <div>
          <h3 className="sg-set-pending-title">等待契约</h3>
          <div className="sg-row" style={{ flexWrap: 'wrap', gap: 6 }}>
            {awaiting.map((m) => (
              <code key={m} className="sg-code">{m}</code>
            ))}
          </div>
          <h3 className="sg-set-pending-title" style={{ marginTop: 14 }}>当前替代路径</h3>
          <p className="sg-hint" style={{ margin: 0 }}>{fallback}</p>
        </div>
      </div>
    </div>
  );
}
