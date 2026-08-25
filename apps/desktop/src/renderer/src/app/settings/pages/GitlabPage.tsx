// S30 GitLab：实例 Profile CRUD + 分步测试 + currentUser。
import { useCallback, useEffect, useState } from 'react';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { useConnectionTest } from '../hooks/useSettingsForm';
import { rpc, rpcErrorMessage } from '../../../rpc/client';

interface GitlabProfile {
  id: string; revision: number; name: string; base_url: string;
  credential_ref_id?: string | null; managed_source?: string | null;
  status: string; last_tested_at?: string | null;
}

export function GitlabPage() {
  const [profiles, setProfiles] = useState<GitlabProfile[]>([]);
  const [selected, setSelected] = useState('');
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [creating, setCreating] = useState(false);
  const [form, setForm] = useState({ name: '', baseUrl: '', credentialRefId: '' });
  const test = useConnectionTest();
  const [user, setUser] = useState<string>('');

  const load = useCallback(async () => {
    setLoading(true); setError('');
    try {
      const result = await rpc<{ items: GitlabProfile[] }>('gitlabProfile.list', {});
      setProfiles(result.items);
      if (!selected && result.items[0]) setSelected(result.items[0].id);
    } catch (reason) { setError(rpcErrorMessage(reason)); }
    finally { setLoading(false); }
  }, [selected]);
  useEffect(() => { void load(); }, [load]);

  const current = profiles.find((p) => p.id === selected) ?? null;
  const readOnly = current?.managed_source != null;

  const create = async () => {
    setError('');
    try {
      await rpc('gitlabProfile.create', {
        name: form.name || 'GitLab', baseUrl: form.baseUrl,
        credentialRefId: form.credentialRefId || undefined,
      });
      setCreating(false); setForm({ name: '', baseUrl: '', credentialRefId: '' });
      await load();
    } catch (reason) { setError(rpcErrorMessage(reason)); }
  };

  const update = async (patch: Record<string, unknown>) => {
    if (!current) return;
    setError('');
    try {
      await rpc('gitlabProfile.update', { profileId: current.id, expectedRevision: current.revision, ...patch });
      await load();
    } catch (reason) { setError(rpcErrorMessage(reason)); }
  };

  const remove = async () => {
    if (!current || !window.confirm(`删除「${current.name}」？`)) return;
    try {
      await rpc('gitlabProfile.remove', { profileId: current.id, expectedRevision: current.revision });
      setSelected(''); await load();
    } catch (reason) { setError(rpcErrorMessage(reason)); }
  };

  const fetchUser = async () => {
    if (!current) return;
    setUser('');
    setError('');
    try {
      const u = await rpc<{ username?: string }>('gitlabProfile.currentUser', { profileId: current.id });
      setUser(u.username ?? '未知用户');
    } catch (reason) { setError(rpcErrorMessage(reason)); }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="GitLab" scope="全局"
        status={profiles.length === 0 ? <StatusPill kind="pending" label="未配置" /> : <StatusPill kind="ready" />}
        description="GitLab 实例与凭据绑定。令牌经凭据引用存 Keychain，此页不显示。"
        actions={!creating ? <button className="sg-btn sg-btn--primary" onClick={() => setCreating(true)}>新增实例</button> : null}
      />
      {error ? <div className="sg-banner sg-banner--error" role="alert">{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

      {creating ? (
        <SettingsSection title="新增实例" description="凭据先在「凭据引用」页创建 GitLab Token 引用">
          <div className="sg-set-grid">
            <label className="sg-set-field"><span className="sg-set-label">名称 *</span>
              <input value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} /></label>
            <label className="sg-set-field"><span className="sg-set-label">Base URL *</span>
              <input value={form.baseUrl} onChange={(e) => setForm({ ...form, baseUrl: e.target.value })} placeholder="https://gitlab.com" /></label>
            <label className="sg-set-field"><span className="sg-set-label">凭据引用 ID</span>
              <input value={form.credentialRefId} onChange={(e) => setForm({ ...form, credentialRefId: e.target.value })} placeholder="cr_…" /></label>
          </div>
          <div className="sg-set-row">
            <button className="sg-btn sg-btn--primary" onClick={() => void create()}>创建</button>
            <button className="sg-btn" onClick={() => setCreating(false)}>取消</button>
          </div>
        </SettingsSection>
      ) : null}

      {profiles.length > 0 && current ? (
        <div className="sg-set-cols">
          <div className="sg-set-list" role="list">
            {profiles.map((p) => (
              <button key={p.id} role="listitem"
                className={`sg-set-item2 ${p.id === selected ? 'sg-set-item2--active' : ''}`}
                onClick={() => setSelected(p.id)}>
                <span>{p.name}</span>
                <StatusPill kind={p.managed_source ? 'readonly' : p.status === 'ready' ? 'ready' : 'pending'} label={p.managed_source ? `托管(${p.managed_source})` : undefined} />
              </button>
            ))}
          </div>
          <SettingsSection title={current.name} description={`revision ${current.revision}${readOnly ? ' · 环境托管只读' : ''}`}>
            {readOnly ? <p className="sg-hint">此实例由环境变量导入，只读。</p> : null}
            <div className="sg-set-grid">
              <label className="sg-set-field"><span className="sg-set-label">Base URL</span>
                <input defaultValue={current.base_url} disabled={readOnly} onBlur={(e) => { if (e.target.value !== current.base_url) void update({ baseUrl: e.target.value }); }} /></label>
              <label className="sg-set-field"><span className="sg-set-label">凭据引用 ID</span>
                <input defaultValue={current.credential_ref_id ?? ''} disabled={readOnly} placeholder="未绑定" onBlur={(e) => { if (e.target.value !== (current.credential_ref_id ?? '')) void update({ credentialRefId: e.target.value }); }} /></label>
            </div>
            <div className="sg-set-row" aria-live="polite">
              <button className="sg-btn" disabled={test.testing} onClick={() => void test.run('gitlabProfile.test', { profileId: current.id })}>
                {test.testing ? '测试中…' : '测试连接'}
              </button>
              <button className="sg-btn" onClick={() => void fetchUser()}>读取当前用户</button>
              {!readOnly ? <button className="sg-btn sg-btn--danger" onClick={() => void remove()}>删除</button> : null}
              {user ? <code>{user}</code> : null}
              {test.overall !== 'idle' ? <StatusPill kind={test.overall === 'ready' ? 'ready' : 'error'} label={test.overall} /> : null}
            </div>
            {test.steps.length > 0 ? (
              <ul className="sg-set-steps">
                {test.steps.map((s) => (
                  <li key={s.name}>
                    <span aria-hidden>{s.status === 'passed' ? '✓' : '✕'}</span>
                    {s.name}{s.errorCode ? <code>{s.errorCode}</code> : null}
                  </li>
                ))}
              </ul>
            ) : null}
          </SettingsSection>
        </div>
      ) : null}
    </div>
  );
}
