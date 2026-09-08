// 工具与审批：tool.list 策略表 + toolPolicy.update。
import { useCallback, useEffect, useState } from 'react';
import { rpc } from '../../../rpc/client';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { IconRefresh } from '../../../components/Icons';

interface ToolPolicy {
  tool_id: string;
  enabled: boolean;
  risk: 'low' | 'medium' | 'high';
  requires_approval: boolean;
  network: 'deny' | 'allow';
  revision: number;
  // F05/M0-③：注册表提供的限制字段（注册表外策略行可能缺省）。
  description?: string;
  max_result_bytes?: number;
  timeout_sec?: number;
}

const RISK_LABEL: Record<string, string> = { low: '低', medium: '中', high: '高' };

export function ToolsPage() {
  const [items, setItems] = useState<ToolPolicy[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true); setError(null);
    try {
      const res = await rpc<{ items: ToolPolicy[] }>('tool.list', {});
      setItems(res.items ?? []);
    } catch (e) {
      setError(e instanceof Error ? e.message : '工具策略加载失败');
    } finally { setLoading(false); }
  }, []);

  useEffect(() => { void load(); }, [load]);

  const update = async (tool: ToolPolicy, patch: Record<string, unknown>) => {
    setError(null); setNotice(null);
    try {
      await rpc('toolPolicy.update', { toolId: tool.tool_id, expectedRevision: tool.revision, ...patch });
      setNotice(`「${tool.tool_id}」已更新`);
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : '更新失败');
    }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="工具与审批"
        description="Agent 可用工具的启用、风险与审批策略。高风险工具默认需人工审批（绑定 ActionDigest）。"
        actions={
          <button className="sg-btn" onClick={() => void load()} disabled={loading}>
            <IconRefresh size={14} />
            刷新
          </button>
        }
      />

      {error ? <div className="sg-banner sg-banner--error" role="alert">操作失败：{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

      <SettingsSection title="工具策略" description="修改即时生效并写入审计">
        {loading ? (
          <div className="sg-skeleton-rows" aria-busy="true">
            <div className="sg-skeleton-row" />
            <div className="sg-skeleton-row" />
          </div>
        ) : items.length === 0 ? (
          <div className="sg-empty">
            <span>暂无工具注册</span>
            <span className="sg-hint">Agent Run 启动时按任务 allowlist 注入工具。</span>
          </div>
        ) : (
          <table className="sg-table" aria-label="工具策略表">
            <thead>
              <tr><th>工具</th><th>状态</th><th>风险</th><th>审批</th><th>限制</th><th>网络</th></tr>
            </thead>
            <tbody>
              {items.map((t) => (
                <tr key={t.tool_id}>
                  <td>
                    <code className="sg-code">{t.tool_id}</code>
                    {t.description ? <div className="sg-hint" style={{ margin: 0 }}>{t.description}</div> : null}
                  </td>
                  <td>
                    <label className="sg-row" style={{ gap: 6, cursor: 'pointer' }}>
                      <input type="checkbox" checked={t.enabled} onChange={(e) => void update(t, { enabled: e.target.checked })} />
                      <span className="sg-hint" style={{ margin: 0 }}>{t.enabled ? '启用' : '禁用'}</span>
                    </label>
                  </td>
                  <td>
                    <StatusPill kind={t.risk === 'high' ? 'error' : t.risk === 'medium' ? 'pending' : 'ready'} label={RISK_LABEL[t.risk]} />
                  </td>
                  <td>
                    <label className="sg-row" style={{ gap: 6, cursor: 'pointer' }}>
                      <input type="checkbox" checked={t.requires_approval} onChange={(e) => void update(t, { requiresApproval: e.target.checked })} />
                      <span className="sg-hint" style={{ margin: 0 }}>{t.requires_approval ? '每次审批' : '无需审批'}</span>
                    </label>
                  </td>
                  <td className="sg-muted">
                    {t.max_result_bytes ? `${Math.round(t.max_result_bytes / 1024)}KB` : '—'} · {t.timeout_sec ? `${t.timeout_sec}s` : '—'}
                  </td>
                  <td className="sg-muted">{t.network === 'allow' ? '允许' : '禁止'}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </SettingsSection>
    </div>
  );
}
