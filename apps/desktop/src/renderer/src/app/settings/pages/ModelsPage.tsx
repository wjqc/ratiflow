// 模型与路由：内置供应商预设（智谱 GLM / DeepSeek）快速接入 + 自定义 OpenAI 兼容 Provider。
// API Key 直填后经 modelProfile.create 落 OS Keychain（DB 只存凭据引用 ID），此页不回显。
import { useCallback, useEffect, useState, type FormEvent } from 'react';
import { rpc } from '../../../rpc/client';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { IconPlus, IconRefresh, IconZap } from '../../../components/Icons';

interface ModelProfile {
  id: string; revision: number; name: string; provider_kind: string; base_url: string;
  credential_ref_id?: string | null; default_model: string;
  managed_source?: string | null; status: string; last_tested_at?: string | null;
}
interface TestStep { name: string; status: string; error_code: string | null }
interface ModelPreset {
  id: string; label: string; providerKind: string; baseUrl: string;
  defaultModel: string; models: { id: string; note: string }[];
}

const KIND_LABEL: Record<string, string> = {
  zhipu: '智谱 GLM', deepseek: 'DeepSeek', openai_compatible: 'OpenAI 兼容', fake: 'fake',
};

const EMPTY_FORM = { name: '', presetId: 'custom', baseUrl: '', apiKey: '', credentialRefId: '', defaultModel: '' };

export function ModelsPage() {
  const [items, setItems] = useState<ModelProfile[]>([]);
  const [presets, setPresets] = useState<ModelPreset[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [form, setForm] = useState(EMPTY_FORM);
  const [testing, setTesting] = useState('');
  const [testResult, setTestResult] = useState<{ status: string; steps: TestStep[] } | null>(null);
  const [syncing, setSyncing] = useState('');
  const [synced, setSynced] = useState<{ name: string; models: string[] } | null>(null);

  const load = useCallback(async () => {
    setLoading(true); setError(null);
    try {
      const [list, presetRes] = await Promise.all([
        rpc<{ items: ModelProfile[] }>('modelProfile.list', {}),
        rpc<{ items: ModelPreset[] }>('modelProvider.presets', {}).catch(() => ({ items: [] })),
      ]);
      setItems(list.items ?? []);
      setPresets(presetRes.items ?? []);
    } catch (e) {
      setError(e instanceof Error ? e.message : '模型列表加载失败');
    } finally { setLoading(false); }
  }, []);
  useEffect(() => { void load(); }, [load]);

  const activePreset = presets.find((p) => p.id === form.presetId);

  const pickPreset = (id: string) => {
    const preset = presets.find((p) => p.id === id);
    setForm((f) => ({
      ...f,
      presetId: id,
      name: preset ? preset.label : f.name,
      baseUrl: preset ? preset.baseUrl : '',
      defaultModel: preset ? preset.defaultModel : '',
    }));
  };

  const create = async (e: FormEvent) => {
    e.preventDefault();
    setError(null); setNotice(null);
    const preset = presets.find((p) => p.id === form.presetId);
    try {
      await rpc('modelProfile.create', {
        name: form.name.trim(),
        providerKind: preset?.providerKind ?? 'openai_compatible',
        baseUrl: form.baseUrl.trim(),
        apiKey: form.apiKey.trim() || undefined,
        credentialRefId: form.apiKey.trim() ? undefined : (form.credentialRefId.trim() || undefined),
        defaultModel: form.defaultModel.trim(),
      });
      setNotice(`Provider「${form.name.trim()}」已创建`);
      setForm(EMPTY_FORM); setCreating(false);
      await load();
    } catch (err) { setError(err instanceof Error ? err.message : '创建失败'); }
  };

  const remove = async (p: ModelProfile) => {
    if (!window.confirm(`删除 Provider「${p.name}」？不可撤销。`)) return;
    setError(null);
    try {
      await rpc('modelProfile.remove', { profileId: p.id, expectedRevision: p.revision });
      await load();
    } catch (e) { setError(e instanceof Error ? e.message : '删除失败'); }
  };

  const test = async (p: ModelProfile) => {
    setTesting(p.id); setTestResult(null); setError(null);
    try {
      setTestResult(await rpc<{ status: string; steps: TestStep[] }>('modelProfile.test', { profileId: p.id }));
    } catch (e) { setError(e instanceof Error ? e.message : '测试失败'); }
    finally { setTesting(''); }
  };

  const syncModels = async (p: ModelProfile) => {
    setSyncing(p.id); setSynced(null); setError(null);
    try {
      const res = await rpc<{ models: string[] }>('modelProfile.syncModels', { profileId: p.id });
      setSynced({ name: p.name, models: res.models ?? [] });
    } catch (e) { setError(e instanceof Error ? e.message : '模型列表同步失败'); }
    finally { setSyncing(''); }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="模型与路由"
        scope="全局"
        status={loading ? <StatusPill kind="checking" /> : items.length === 0 ? <StatusPill kind="pending" label="未配置" /> : <StatusPill kind="ready" />}
        description="接入智谱 GLM、DeepSeek 或任意 OpenAI 兼容模型。API Key 直填后只写入 OS Keychain，此页不回显。"
        actions={
          <button className="sg-btn" onClick={() => void load()} disabled={loading}><IconRefresh size={14} />刷新</button>
        }
      />

      {error ? <div className="sg-banner sg-banner--error" role="alert">操作失败：{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

      {!creating ? (
        <SettingsSection title="Provider 列表" actions={
          <button className="sg-btn" onClick={() => { setForm(EMPTY_FORM); setCreating(true); }}><IconPlus size={14} />新增</button>
        }>
          {loading ? (
            <div className="sg-skeleton-rows" aria-busy="true"><div className="sg-skeleton-row" /></div>
          ) : items.length === 0 ? (
            <div className="sg-empty">
              <span>暂无模型 Provider</span>
              <span className="sg-hint">未配置模型时 Agent Run 被阻塞（概览有对应提示）。点「新增」用预设一键接入 GLM / DeepSeek。</span>
            </div>
          ) : (
            <table className="sg-table" aria-label="Provider 列表">
              <thead><tr><th>名称</th><th>端点</th><th>状态</th><th style={{ width: 300 }}>操作</th></tr></thead>
              <tbody>
                {items.map((p) => (
                  <tr key={p.id}>
                    <td>
                      <div style={{ fontWeight: 500 }}>{p.name}</div>
                      <div className="sg-hint">{KIND_LABEL[p.provider_kind] ?? p.provider_kind}{p.default_model ? ` · ${p.default_model}` : ''}</div>
                    </td>
                    <td><span className="sg-code">{p.base_url || '—'}</span>
                      <div className="sg-hint">{p.credential_ref_id ? `凭据 ${p.credential_ref_id.slice(0, 10)}…` : '未绑定凭据'}</div>
                    </td>
                    <td>
                      <StatusPill kind={p.managed_source ? 'readonly' : p.status === 'ready' ? 'ready' : 'pending'}
                        label={p.managed_source ? `托管(${p.managed_source})` : undefined} />
                    </td>
                    <td>
                      <div className="sg-row" style={{ gap: 6 }}>
                        <button className="sg-btn sg-btn--sm" disabled={testing === p.id} onClick={() => void test(p)}>
                          <IconZap size={12} />{testing === p.id ? '测试中…' : '测试'}
                        </button>
                        <button className="sg-btn sg-btn--sm" disabled={syncing === p.id} onClick={() => void syncModels(p)}>
                          {syncing === p.id ? '同步中…' : '同步模型'}
                        </button>
                        {!p.managed_source ? (
                          <button className="sg-btn sg-btn--sm sg-btn--danger" onClick={() => void remove(p)}>删除</button>
                        ) : null}
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          {synced ? (
            <div className="sg-banner sg-banner--info" role="status" style={{ marginTop: 10 }}>
              「{synced.name}」可用模型（{synced.models.length}）：{synced.models.join('、')}
            </div>
          ) : null}
          {testResult ? (
            <table className="sg-table" style={{ marginTop: 10 }} aria-label="测试步骤" aria-live="polite">
              <thead><tr><th>步骤</th><th>状态</th><th>错误码</th></tr></thead>
              <tbody>
                {testResult.steps.map((s) => (
                  <tr key={s.name}>
                    <td>{s.name}</td>
                    <td><StatusPill kind={s.status === 'passed' ? 'ready' : s.status === 'skipped' ? 'readonly' : 'error'} label={s.status} /></td>
                    <td className="sg-muted">{s.error_code ?? '—'}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          ) : null}
        </SettingsSection>
      ) : (
        <SettingsSection title="新增 Provider" description="选择预设自动填端点与模型；API Key 只写 Keychain，也可改用已有凭据引用 ID">
          <div className="sg-row" style={{ gap: 8, marginBottom: 14 }} role="tablist" aria-label="供应商预设">
            {presets.map((p) => (
              <button key={p.id} type="button"
                className={`sg-btn${form.presetId === p.id ? ' sg-btn--primary' : ''}`}
                aria-pressed={form.presetId === p.id}
                onClick={() => pickPreset(p.id)}>
                {p.label}
              </button>
            ))}
            <button type="button"
              className={`sg-btn${form.presetId === 'custom' ? ' sg-btn--primary' : ''}`}
              aria-pressed={form.presetId === 'custom'}
              onClick={() => pickPreset('custom')}>
              自定义（OpenAI 兼容）
            </button>
          </div>
          <form className="sg-card sg-set-form" onSubmit={create}>
            <div className="sg-form-grid--2">
              <div className="sg-field">
                <label htmlFor="mp-name">名称 *</label>
                <input id="mp-name" className="sg-input" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="主力模型" />
              </div>
              <div className="sg-field">
                <label htmlFor="mp-url">Base URL</label>
                <input id="mp-url" className="sg-input" value={form.baseUrl}
                  onChange={(e) => setForm({ ...form, baseUrl: e.target.value })}
                  placeholder={activePreset ? activePreset.baseUrl : 'https://api.example.com/v1'} />
              </div>
              <div className="sg-field">
                <label htmlFor="mp-key">API Key</label>
                <input id="mp-key" className="sg-input" type="password" autoComplete="off" value={form.apiKey}
                  onChange={(e) => setForm({ ...form, apiKey: e.target.value })}
                  placeholder="直填（只写 Keychain，不回显）" />
              </div>
              <div className="sg-field">
                <label htmlFor="mp-cred">或凭据引用 ID</label>
                <input id="mp-cred" className="sg-input" value={form.credentialRefId}
                  onChange={(e) => setForm({ ...form, credentialRefId: e.target.value })}
                  placeholder="cr_…（与 API Key 二选一）" disabled={!!form.apiKey.trim()} />
              </div>
              <div className="sg-field">
                <label htmlFor="mp-model">默认模型</label>
                <input id="mp-model" className="sg-input" value={form.defaultModel}
                  onChange={(e) => setForm({ ...form, defaultModel: e.target.value })}
                  list="mp-model-options" />
                <datalist id="mp-model-options">
                  {(activePreset?.models ?? []).map((m) => <option key={m.id} value={m.id}>{m.note}</option>)}
                </datalist>
              </div>
            </div>
            {activePreset ? (
              <p className="sg-hint" style={{ margin: '0 0 12px' }}>
                {activePreset.label} 推荐模型：{activePreset.models.map((m) => m.id).join('、')}
              </p>
            ) : null}
            <div className="sg-row">
              <button type="submit" className="sg-btn sg-btn--primary" disabled={!form.name.trim()}>
                <IconPlus size={14} />创建
              </button>
              <button type="button" className="sg-btn" onClick={() => setCreating(false)}>取消</button>
            </div>
          </form>
        </SettingsSection>
      )}
    </div>
  );
}
