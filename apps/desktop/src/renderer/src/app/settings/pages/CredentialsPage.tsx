// 凭据引用：Keychain 引用管理（秘密 write-only，永不回显）。
import { useCallback, useEffect, useState, type FormEvent } from 'react';
import { rpc } from '../../../rpc/client';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { IconPlus, IconRefresh, IconShield } from '../../../components/Icons';

interface CredentialRef {
  id: string; revision: number; name: string; kind: string; provider: string;
  status: string; last_verified_at?: string | null; created_at: string;
}
interface CreateForm { name: string; kind: string; provider: string; secret: string }

const EMPTY: CreateForm = { name: '', kind: 'model_api_key', provider: '', secret: '' };
const KIND_LABEL: Record<string, string> = {
  gitlab_token: 'GitLab Token', model_api_key: '模型 API Key', ssh_key: 'SSH Key', generic_secret: '其他秘密',
};

export function CredentialsPage() {
  const [items, setItems] = useState<CredentialRef[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [form, setForm] = useState(EMPTY);
  const [rotating, setRotating] = useState('');
  const [rotateSecret, setRotateSecret] = useState('');

  const load = useCallback(async () => {
    setLoading(true); setError(null);
    try {
      const res = await rpc<{ items: CredentialRef[] }>('credentialRef.list', {});
      setItems(res.items ?? []);
    } catch (e) { setError(e instanceof Error ? e.message : '凭据列表加载失败'); }
    finally { setLoading(false); }
  }, []);
  useEffect(() => { void load(); }, [load]);

  const create = async (e: FormEvent) => {
    e.preventDefault(); setError(null); setNotice(null);
    try {
      const ref = await rpc<CredentialRef>('credentialRef.create', {
        name: form.name.trim(), kind: form.kind, provider: form.provider.trim() || undefined, secret: form.secret,
      });
      setNotice(`凭据「${ref.name}」已写入系统钥匙串（引用 ${ref.id}）`);
      setForm(EMPTY); setCreating(false);
      await load();
    } catch (err) { setError(err instanceof Error ? err.message : '创建失败'); }
  };

  const replace = async (ref: CredentialRef) => {
    if (!rotateSecret) return;
    setError(null);
    try {
      await rpc('credentialRef.replace', { ref_id: ref.id, secret: rotateSecret, expected_revision: ref.revision });
      setNotice(`「${ref.name}」已轮换`);
      setRotating(''); setRotateSecret('');
      await load();
    } catch (e) { setError(e instanceof Error ? e.message : '轮换失败'); }
  };

  const verify = async (ref: CredentialRef) => {
    setError(null); setNotice(null);
    try {
      const res = await rpc<CredentialRef>('credentialRef.verify', { ref_id: ref.id });
      setNotice(`「${ref.name}」验证：${res.status === 'active' ? '可用' : res.status}`);
    } catch (e) { setError(e instanceof Error ? e.message : '验证失败'); }
  };

  const remove = async (ref: CredentialRef) => {
    if (!window.confirm(`删除凭据「${ref.name}」？引用方将进入 degraded，不回退到其他秘密。`)) return;
    setError(null);
    try {
      await rpc('credentialRef.remove', { ref_id: ref.id, expected_revision: ref.revision, force: true });
      await load();
    } catch (e) { setError(e instanceof Error ? e.message : '删除失败'); }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="凭据引用"
        scope="全局"
        status={loading ? <StatusPill kind="checking" /> : <StatusPill kind="ready" />}
        description="秘密存于操作系统钥匙串；数据库仅保存引用。创建后永不回显。"
        actions={<button className="sg-btn" onClick={() => void load()} disabled={loading}><IconRefresh size={14} />刷新</button>}
      />

      {error ? <div className="sg-banner sg-banner--error" role="alert">操作失败：{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

      {!creating ? (
        <SettingsSection title="凭据列表" actions={
          <button className="sg-btn" onClick={() => setCreating(true)}><IconPlus size={14} />新增凭据</button>
        }>
          {loading ? (
            <div className="sg-skeleton-rows" aria-busy="true"><div className="sg-skeleton-row" /></div>
          ) : items.length === 0 ? (
            <div className="sg-empty">
              <IconShield size={28} style={{ color: 'var(--sg-border-strong)' }} />
              <span>暂无凭据引用</span>
              <span className="sg-hint">模型 / GitLab / SSH Profile 需绑定凭据引用后才能测试真实连接。</span>
            </div>
          ) : (
            <table className="sg-table" aria-label="凭据引用表">
              <thead><tr><th>名称</th><th>类型</th><th>状态</th><th style={{ width: 260 }}>操作</th></tr></thead>
              <tbody>
                {items.map((ref) => (
                  <tr key={ref.id}>
                    <td>
                      <div style={{ fontWeight: 500 }}>{ref.name}</div>
                      <div className="sg-path">{ref.id}</div>
                    </td>
                    <td>{KIND_LABEL[ref.kind] ?? ref.kind}</td>
                    <td>
                      <StatusPill kind={ref.status === 'active' ? 'ready' : 'error'} label={ref.status === 'active' ? '可用' : ref.status} />
                    </td>
                    <td>
                      <div className="sg-row" style={{ gap: 6 }}>
                        <button className="sg-btn sg-btn--sm" onClick={() => void verify(ref)}>验证</button>
                        <button className="sg-btn sg-btn--sm" onClick={() => { setRotating(rotating === ref.id ? '' : ref.id); setRotateSecret(''); }}>
                          <IconRefresh size={12} />轮换
                        </button>
                        <button className="sg-btn sg-btn--sm sg-btn--danger" onClick={() => void remove(ref)}>删除</button>
                      </div>
                      {rotating === ref.id ? (
                        <form className="sg-row" style={{ marginTop: 6, gap: 6 }} onSubmit={(e) => { e.preventDefault(); void replace(ref); }}>
                          <input className="sg-input" style={{ width: 200 }} type="password" autoComplete="new-password"
                            placeholder="新秘密值" value={rotateSecret} onChange={(e) => setRotateSecret(e.target.value)} />
                          <button type="submit" className="sg-btn sg-btn--primary sg-btn--sm" disabled={!rotateSecret}>确认</button>
                        </form>
                      ) : null}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          <p className="sg-hint">列表不显示秘密内容；「可用」仅表示钥匙串中该项可读。</p>
        </SettingsSection>
      ) : (
        <SettingsSection title="新增凭据" description="秘密只输入一次，保存后不回显">
          <form className="sg-card sg-set-form" onSubmit={create}>
            <div className="sg-form-grid--2">
              <div className="sg-field">
                <label htmlFor="cr-name">名称 *</label>
                <input id="cr-name" className="sg-input" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="OpenAI Key" />
              </div>
              <div className="sg-field">
                <label htmlFor="cr-kind">类型 *</label>
                <select id="cr-kind" className="sg-select" value={form.kind} onChange={(e) => setForm({ ...form, kind: e.target.value })}>
                  {Object.entries(KIND_LABEL).map(([v, l]) => <option key={v} value={v}>{l}</option>)}
                </select>
              </div>
              <div className="sg-field">
                <label htmlFor="cr-provider">提供方</label>
                <input id="cr-provider" className="sg-input" value={form.provider} onChange={(e) => setForm({ ...form, provider: e.target.value })} placeholder="openai" />
              </div>
              <div className="sg-field">
                <label htmlFor="cr-secret">秘密值 *</label>
                <input id="cr-secret" className="sg-input" type="password" autoComplete="new-password"
                  value={form.secret} onChange={(e) => setForm({ ...form, secret: e.target.value })} />
              </div>
            </div>
            <div className="sg-row">
              <button type="submit" className="sg-btn sg-btn--primary" disabled={!form.name.trim() || !form.secret}>
                <IconPlus size={14} />写入钥匙串
              </button>
              <button type="button" className="sg-btn" onClick={() => setCreating(false)}>取消</button>
            </div>
          </form>
        </SettingsSection>
      )}
    </div>
  );
}
