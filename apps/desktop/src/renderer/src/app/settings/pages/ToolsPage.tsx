// S21 工具与审批：tool.list 策略表 + toolPolicy.update（含 revision 乐观锁）。
import { useCallback, useEffect, useState } from 'react';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { rpc, rpcErrorMessage } from '../../../rpc/client';

interface ToolPolicy {
  tool_id: string; enabled: boolean; risk: 'low' | 'medium' | 'high';
  requires_approval: boolean; network: 'deny' | 'allow'; revision: number;
}

const RISK_LABEL: Record<string, string> = { low: '低', medium: '中', high: '高' };

export function ToolsPage() {
  const [items, setItems] = useState<ToolPolicy[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');

  const load = useCallback(async () => {
    setLoading(true); setError('');
    try {
      const result = await rpc<{ items: ToolPolicy[] }>('tool.list', {});
      setItems(result.items);
    } catch (reason) { setError(rpcErrorMessage(reason)); }
    finally { setLoading(false); }
  }, []);
  useEffect(() => { void load(); }, [load]);

  const update = async (tool: ToolPolicy, patch: Record<string, unknown>) => {
    setNotice(''); setError('');
    try {
      await rpc('toolPolicy.update', { toolId: tool.tool_id, expectedRevision: tool.revision, ...patch });
      setNotice(`「${tool.tool_id}」已更新`);
      await load();
    } catch (reason) { setError(rpcErrorMessage(reason)); }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="工具与审批" scope="全局"
        status={loading ? <StatusPill kind="checking" /> : <StatusPill kind="ready" />}
        description="Agent 可用工具的启用、风险与审批策略。高风险工具默认需人工审批（ActionDigest 绑定）。"
      />
      {error ? <div className="sg-banner sg-banner--error" role="alert">{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}
      <SettingsSection title="工具策略" description="修改即时生效并写入审计">
        <table className="sg-table" aria-label="工具策略表">
          <thead>
            <tr><th>工具</th><th>状态</th><th>风险</th><th>审批</th><th>网络</th></tr>
          </thead>
          <tbody>
            {items.map((t) => (
              <tr key={t.tool_id}>
                <td><code>{t.tool_id}</code></td>
                <td>
                  <label className="sg-set-inline">
                    <input type="checkbox" checked={t.enabled} onChange={(e) => void update(t, { enabled: e.target.checked })} />
                    <span>{t.enabled ? '启用' : '禁用'}</span>
                  </label>
                </td>
                <td><StatusPill kind={t.risk === 'high' ? 'error' : t.risk === 'medium' ? 'pending' : 'ready'} label={RISK_LABEL[t.risk]} /></td>
                <td>
                  <label className="sg-set-inline">
                    <input type="checkbox" checked={t.requires_approval} onChange={(e) => void update(t, { requiresApproval: e.target.checked })} />
                    <span>{t.requires_approval ? '每次审批' : '无需审批'}</span>
                  </label>
                </td>
                <td>{t.network === 'allow' ? '允许' : '禁止'}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </SettingsSection>
    </div>
  );
}
