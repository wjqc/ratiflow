// 模型与路由：Provider 列表（sg-table 与项目页一致）+ 编辑 + 分步测试。
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

const EMPTY = { name: '', baseUrl: '', credentialRefId: '', defaultModel: '' };

export function ModelsPage() {
  const [items, setItems] = useState<ModelProfile[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [form, setForm] = useState(EMPTY);
  const [testing, setTesting] = useState('');
  const [testResult, setTestResult] = useState<{ status: string; steps: TestStep[] } | null>(null);

  const load = useCallback(async () => {
    setLoading(true); setError(null);
    try {
      const res = await rpc<{ items: ModelProfile[] }>('modelProfile.list', {});
      setItems(res.items ?? []);
    } catch (e) {
      setError(e instanceof Error ? e.message : '模型列表加载失败');
    } finally { setLoading(false); }
  }, []);
  useEffect(() => { void load(); }, [load]);

  const create = async (e: FormEvent) => {
    e.preventDefault();
    setError(null); setNotice(null);
    try {
      await rpc('modelProfile.create', {
        name: form.name.trim(), providerKind: 'openai_compatible',
        baseUrl: form.baseUrl.trim(), credentialRefId: form.credentialRefId.trim() || undefined,
        defaultModel: form.defaultModel.trim(),
      });
      setNotice(`Provider「${form.name.trim()}」已创建`);
      setForm(EMPTY); setCreating(false);
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

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="模型与路由"
        scope="全局"
        status={loading ? <StatusPill kind="checking" /> : items.length === 0 ? <StatusPill kind="pending" label="未配置" /> : <StatusPill kind="ready" />}
        description="公有模型 Provider。密钥经凭据引用存 Keychain，此页不输入也不回显。"
        actions={
          <button className="sg-btn" onClick={() => void load()} disabled={loading}><IconRefresh size={14} />刷新</button>
        }
      />

      {error ? <div className="sg-banner sg-banner--error" role="alert">操作失败：{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

      {!creating ? (
        <SettingsSection title="Provider 列表" actions={
          <button className="sg-btn" onClick={() => setCreating(true)}><IconPlus size={14} />新增</button>
        }>
          {loading ? (
            <div className="sg-skeleton-rows" aria-busy="true"><div className="sg-skeleton-row" /></div>
          ) : items.length === 0 ? (
            <div className="sg-empty">
              <span>暂无模型 Provider</span>
              <span className="sg-hint">未配置模型时 Agent Run 被阻塞（概览有对应提示）。</span>
            </div>
          ) : (
            <table className="sg-table" aria-label="Provider 列表">
              <thead><tr><th>名称</th><th>端点</th><th>状态</th><th style={{ width: 220 }}>操作</th></tr></thead>
              <tbody>
                {items.map((p) => (
                  <tr key={p.id}>
                    <td>
                      <div style={{ fontWeight: 500 }}>{p.name}</div>
                      <div className="sg-path">{p.default_model || '未设默认模型'}</div>
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
        <SettingsSection title="新增 Provider" description="凭据先在「凭据引用」页创建">
          <form className="sg-card sg-set-form" onSubmit={create}>
            <div className="sg-form-grid--2">
              <div className="sg-field">
                <label htmlFor="mp-name">名称 *</label>
                <input id="mp-name" className="sg-input" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="主力模型" />
              </div>
              <div className="sg-field">
                <label htmlFor="mp-url">Base URL</label>
                <input id="mp-url" className="sg-input" value={form.baseUrl} onChange={(e) => setForm({ ...form, baseUrl: e.target.value })} placeholder="https://api.example.com/v1" />
              </div>
              <div className="sg-field">
                <label htmlFor="mp-cred">凭据引用 ID</label>
                <input id="mp-cred" className="sg-input" value={form.credentialRefId} onChange={(e) => setForm({ ...form, credentialRefId: e.target.value })} placeholder="cr_…" />
              </div>
              <div className="sg-field">
                <label htmlFor="mp-model">默认模型</label>
                <input id="mp-model" className="sg-input" value={form.defaultModel} onChange={(e) => setForm({ ...form, defaultModel: e.target.value })} />
              </div>
            </div>
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
