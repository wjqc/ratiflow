// S40 凭据引用：Keychain 引用管理（秘密 write-only，永不回显；点状占位仅表示"已存在"）。
import { useCallback, useEffect, useState } from 'react';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { rpc, rpcErrorMessage } from '../../../rpc/client';

interface CredentialRef {
  id: string; revision: number; name: string; kind: string; provider: string;
  status: string; last_verified_at?: string | null; created_at: string;
}

const KIND_LABEL: Record<string, string> = {
  gitlab_token: 'GitLab Token', model_api_key: '模型 API Key', ssh_key: 'SSH Key', generic_secret: '其他秘密',
};

export function CredentialsPage() {
  const [items, setItems] = useState<CredentialRef[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [creating, setCreating] = useState(false);
  const [rotating, setRotating] = useState<string>('');
  const [form, setForm] = useState({ name: '', kind: 'model_api_key', provider: '', secret: '' });
  const [rotateSecret, setRotateSecret] = useState('');

  const load = useCallback(async () => {
    setLoading(true); setError('');
    try {
      const result = await rpc<{ items: CredentialRef[] }>('credentialRef.list', {});
      setItems(result.items);
    } catch (reason) { setError(rpcErrorMessage(reason)); }
    finally { setLoading(false); }
  }, []);
  useEffect(() => { void load(); }, [load]);

  const create = async () => {
    setError(''); setNotice('');
    try {
      const ref = await rpc<CredentialRef>('credentialRef.create', {
        name: form.name, kind: form.kind, provider: form.provider || undefined, secret: form.secret,
      });
      setNotice(`凭据「${ref.name}」已写入系统钥匙串（引用 ${ref.id}）。`);
      setCreating(false); setForm({ name: '', kind: 'model_api_key', provider: '', secret: '' });
      await load();
    } catch (reason) { setError(rpcErrorMessage(reason)); }
  };

  const replace = async (ref: CredentialRef) => {
    if (!rotateSecret) return;
    setError('');
    try {
      await rpc('credentialRef.replace', { refId: ref.id, secret: rotateSecret, expectedRevision: ref.revision });
      setNotice(`「${ref.name}」已轮换。`); setRotating(''); setRotateSecret('');
      await load();
    } catch (reason) { setError(rpcErrorMessage(reason)); }
  };

  const verify = async (ref: CredentialRef) => {
    setError(''); setNotice('');
    try {
      const result = await rpc<CredentialRef>('credentialRef.verify', { refId: ref.id });
      setNotice(`「${ref.name}」验证：${result.status === 'active' ? '可用 ✓' : result.status}`);
    } catch (reason) { setError(rpcErrorMessage(reason)); }
  };

  const remove = async (ref: CredentialRef) => {
    const dependents = items.length; // 真实依赖检查在 core；此处直接确认
    if (!window.confirm(`删除凭据「${ref.name}」？引用它的 Profile 将进入 degraded（不会回退到其他秘密）。`)) return;
    void dependents;
    setError('');
    try {
      await rpc('credentialRef.remove', { refId: ref.id, expectedRevision: ref.revision, force: true });
      await load();
    } catch (reason) {
      const msg = rpcErrorMessage(reason);
      if (/引用/.test(msg) && window.confirm(`${msg}\n强制删除？`)) {
        try {
          await rpc('credentialRef.remove', { refId: ref.id, expectedRevision: ref.revision, force: true });
          await load();
        } catch (e2) { setError(rpcErrorMessage(e2)); }
      } else { setError(msg); }
    }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="凭据引用" scope="全局"
        status={loading ? <StatusPill kind="checking" /> : <StatusPill kind="ready" />}
        description="秘密存储在操作系统钥匙串中；数据库仅保存引用。创建后永不回显，轮换需输入新值。"
        actions={!creating ? <button className="sg-btn sg-btn--primary" onClick={() => setCreating(true)}>新增凭据</button> : null}
      />
      {error ? <div className="sg-banner sg-banner--error" role="alert">{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

      {creating ? (
        <SettingsSection title="新增凭据" description="秘密只输入一次，保存后不回显">
          <div className="sg-set-grid">
            <label className="sg-set-field"><span className="sg-set-label">名称 *</span>
              <input value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="OpenAI Key" /></label>
            <label className="sg-set-field"><span className="sg-set-label">类型 *</span>
              <select value={form.kind} onChange={(e) => setForm({ ...form, kind: e.target.value })}>
                {Object.entries(KIND_LABEL).map(([v, l]) => <option key={v} value={v}>{l}</option>)}
              </select></label>
            <label className="sg-set-field"><span className="sg-set-label">提供方</span>
              <input value={form.provider} onChange={(e) => setForm({ ...form, provider: e.target.value })} placeholder="openai" /></label>
            <label className="sg-set-field"><span className="sg-set-label">秘密值 *</span>
              <input type="password" autoComplete="new-password" value={form.secret} onChange={(e) => setForm({ ...form, secret: e.target.value })} /></label>
          </div>
          <div className="sg-set-row">
            <button className="sg-btn sg-btn--primary" disabled={!form.name || !form.secret} onClick={() => void create()}>写入钥匙串</button>
            <button className="sg-btn" onClick={() => setCreating(false)}>取消</button>
          </div>
        </SettingsSection>
      ) : null}

      <SettingsSection title="凭据列表" description="点状占位（•••）仅表示秘密已存在，不代表读取过">
        {items.length === 0 && !creating ? (
          <p className="sg-hint">尚无凭据。模型/GitLab/SSH Profile 绑定凭据引用后才能测试真实连接。</p>
        ) : (
          <table className="sg-table" aria-label="凭据引用表">
            <thead><tr><th>名称</th><th>类型</th><th>存储</th><th>状态</th><th>操作</th></tr></thead>
            <tbody>
              {items.map((ref) => (
                <tr key={ref.id}>
                  <td>{ref.name}<br /><code className="sg-hint">{ref.id}</code></td>
                  <td>{KIND_LABEL[ref.kind] ?? ref.kind}</td>
                  <td aria-label="秘密已存在">•••</td>
                  <td><StatusPill kind={ref.status === 'active' ? 'ready' : 'error'} label={ref.status === 'active' ? '可用' : ref.status} /></td>
                  <td>
                    <div className="sg-set-row">
                      <button className="sg-btn" onClick={() => void verify(ref)}>验证</button>
                      <button className="sg-btn" onClick={() => { setRotating(rotating === ref.id ? '' : ref.id); setRotateSecret(''); }}>轮换</button>
                      <button className="sg-btn sg-btn--danger" onClick={() => void remove(ref)}>删除</button>
                    </div>
                    {rotating === ref.id ? (
                      <div className="sg-set-row" style={{ marginTop: 6 }}>
                        <input type="password" autoComplete="new-password" placeholder="新秘密值"
                          value={rotateSecret} onChange={(e) => setRotateSecret(e.target.value)} />
                        <button className="sg-btn sg-btn--primary" disabled={!rotateSecret} onClick={() => void replace(ref)}>确认轮换</button>
                      </div>
                    ) : null}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </SettingsSection>
    </div>
  );
}
