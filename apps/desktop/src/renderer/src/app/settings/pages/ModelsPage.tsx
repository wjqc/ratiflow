// S20 模型与路由：Provider 列表 + 详情编辑 + 分步测试 + 路由规则。
import { useCallback, useEffect, useState } from 'react';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { useConnectionTest } from '../hooks/useSettingsForm';
import { rpc, rpcErrorMessage } from '../../../rpc/client';

interface ModelProfile {
  id: string; revision: number; name: string; providerKind: string; baseUrl: string;
  credentialRefId?: string | null; defaultModel: string;
  capabilities?: Record<string, boolean>; limits?: Record<string, number>;
  managedSource?: string | null; status: string; lastTestedAt?: string | null;
}

export function ModelsPage() {
  const [profiles, setProfiles] = useState<ModelProfile[]>([]);
  const [selected, setSelected] = useState<string>('');
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [creating, setCreating] = useState(false);
  const [form, setForm] = useState({ name: '', baseUrl: '', credentialRefId: '', defaultModel: '' });
  const test = useConnectionTest();

  const load = useCallback(async () => {
    setLoading(true); setError('');
    try {
      const result = await rpc<{ items: ModelProfile[] }>('modelProfile.list', {});
      setProfiles(result.items);
      if (!selected && result.items[0]) setSelected(result.items[0].id);
    } catch (reason) { setError(rpcErrorMessage(reason)); }
    finally { setLoading(false); }
  }, [selected]);

  useEffect(() => { void load(); }, [load]);

  const current = profiles.find((p) => p.id === selected) ?? null;
  const readOnly = current?.managedSource != null;

  const create = async () => {
    setNotice(''); setError('');
    try {
      await rpc('modelProfile.create', {
        name: form.name || '新 Provider', providerKind: 'openai_compatible',
        baseUrl: form.baseUrl, credentialRefId: form.credentialRefId || undefined,
        defaultModel: form.defaultModel,
      });
      setCreating(false); setForm({ name: '', baseUrl: '', credentialRefId: '', defaultModel: '' });
      await load();
    } catch (reason) { setError(rpcErrorMessage(reason)); }
  };

  const update = async (patch: Record<string, unknown>) => {
    if (!current) return;
    setNotice(''); setError('');
    try {
      await rpc('modelProfile.update', { profileId: current.id, expectedRevision: current.revision, ...patch });
      await load();
    } catch (reason) { setError(rpcErrorMessage(reason)); }
  };

  const remove = async () => {
    if (!current) return;
    if (!window.confirm(`删除 Provider「${current.name}」？此操作不可撤销。`)) return;
    try {
      await rpc('modelProfile.remove', { profileId: current.id, expectedRevision: current.revision });
      setSelected(''); await load();
    } catch (reason) { setError(rpcErrorMessage(reason)); }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="模型与路由" scope="全局"
        status={profiles.length === 0 ? <StatusPill kind="pending" label="未配置" /> : <StatusPill kind="ready" />}
        description="公有模型 Provider 与任务路由。密钥经凭据引用绑定，此页不输入/回显密钥。"
        actions={!creating ? <button className="sg-btn sg-btn--primary" onClick={() => setCreating(true)}>新增 Provider</button> : null}
      />
      {error ? <div className="sg-banner sg-banner--error" role="alert">{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

      {profiles.length === 0 && !creating && !loading ? (
        <SettingsSection title="尚无 Provider">
          <p className="sg-hint">未配置模型时 Agent Run 被阻塞。新增 Provider 后即可在应用内测试与路由。</p>
        </SettingsSection>
      ) : null}

      {creating ? (
        <SettingsSection title="新增 Provider" description="OpenAI 兼容端点；凭据先在「凭据引用」页创建">
          <div className="sg-set-grid">
            <label className="sg-set-field"><span className="sg-set-label">名称 *</span>
              <input value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="主力模型" /></label>
            <label className="sg-set-field"><span className="sg-set-label">Base URL</span>
              <input value={form.baseUrl} onChange={(e) => setForm({ ...form, baseUrl: e.target.value })} placeholder="https://api.example.com/v1" /></label>
            <label className="sg-set-field"><span className="sg-set-label">凭据引用 ID</span>
              <input value={form.credentialRefId} onChange={(e) => setForm({ ...form, credentialRefId: e.target.value })} placeholder="cr_…" /></label>
            <label className="sg-set-field"><span className="sg-set-label">默认模型</span>
              <input value={form.defaultModel} onChange={(e) => setForm({ ...form, defaultModel: e.target.value })} /></label>
          </div>
          <div className="sg-set-row">
            <button className="sg-btn sg-btn--primary" onClick={() => void create()}>创建</button>
            <button className="sg-btn" onClick={() => setCreating(false)}>取消</button>
          </div>
        </SettingsSection>
      ) : null}

      {profiles.length > 0 ? (
        <div className="sg-set-cols">
          <div className="sg-set-list" role="list" aria-label="Provider 列表">
            {profiles.map((p) => (
              <button key={p.id} role="listitem"
                className={`sg-set-item2 ${p.id === selected ? 'sg-set-item2--active' : ''}`}
                onClick={() => setSelected(p.id)}>
                <span>{p.name}</span>
                <StatusPill kind={p.managedSource ? 'readonly' : p.status === 'ready' ? 'ready' : 'pending'} label={p.managedSource ? `托管(${p.managedSource})` : undefined} />
              </button>
            ))}
          </div>
          {current ? (
            <SettingsSection title={current.name} description={`revision ${current.revision} · ${current.providerKind}${readOnly ? ' · 环境托管只读；替代路径：创建新 Provider' : ''}`}>
              {readOnly ? <p className="sg-hint">此 Profile 由环境变量导入，只读。要修改：创建新 Provider 并更新路由。</p> : null}
              <div className="sg-set-grid">
                <label className="sg-set-field"><span className="sg-set-label">Base URL</span>
                  <input defaultValue={current.baseUrl} disabled={readOnly} onBlur={(e) => { if (e.target.value !== current.baseUrl) void update({ baseUrl: e.target.value }); }} /></label>
                <label className="sg-set-field"><span className="sg-set-label">默认模型</span>
                  <input defaultValue={current.defaultModel} disabled={readOnly} onBlur={(e) => { if (e.target.value !== current.defaultModel) void update({ defaultModel: e.target.value }); }} /></label>
                <label className="sg-set-field"><span className="sg-set-label">凭据引用 ID</span>
                  <input defaultValue={current.credentialRefId ?? ''} disabled={readOnly} placeholder="未绑定" onBlur={(e) => { if (e.target.value !== (current.credentialRefId ?? '')) void update({ credentialRefId: e.target.value }); }} /></label>
              </div>
              <div className="sg-set-row" aria-live="polite">
                <button className="sg-btn" disabled={test.testing} onClick={() => void test.run('modelProfile.test', { profileId: current.id })}>
                  {test.testing ? '测试中…' : '测试连接'}
                </button>
                {!readOnly ? <button className="sg-btn sg-btn--danger" onClick={() => void remove()}>删除</button> : null}
                {test.overall !== 'idle' ? <StatusPill kind={test.overall === 'ready' ? 'ready' : test.overall === 'degraded' ? 'partial' : test.overall === 'action_required' ? 'pending' : 'error'} label={test.overall} /> : null}
              </div>
              {test.steps.length > 0 ? (
                <ul className="sg-set-steps">
                  {test.steps.map((s) => (
                    <li key={s.name}>
                      <span aria-hidden>{s.status === 'passed' ? '✓' : s.status === 'skipped' ? '—' : '✕'}</span>
                      {s.name}
                      {s.errorCode && s.status !== 'skipped' ? <code>{s.errorCode}</code> : null}
                    </li>
                  ))}
                </ul>
              ) : null}
            </SettingsSection>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
