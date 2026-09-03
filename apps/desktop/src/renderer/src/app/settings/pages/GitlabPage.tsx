// GitLab：实例列表（sg-table）+ 新增/删除 + 分步测试 + currentUser。
import { useCallback, useEffect, useState, type FormEvent } from 'react';
import { rpc } from '../../../rpc/client';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { useTwoStepConfirm } from '../components/useTwoStepConfirm';
import { IconPlus, IconRefresh, IconUser, IconZap } from '../../../components/Icons';

interface GitlabProfile {
  id: string; revision: number; name: string; base_url: string;
  credential_ref_id?: string | null; managed_source?: string | null;
  status: string; last_tested_at?: string | null;
}
interface TestStep { name: string; status: string; error_code: string | null }

const EMPTY = { name: '', baseUrl: '', credentialRefId: '' };

export function GitlabPage() {
  const [items, setItems] = useState<GitlabProfile[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [form, setForm] = useState(EMPTY);
  const [testing, setTesting] = useState('');
  const [testResult, setTestResult] = useState<{ status: string; steps: TestStep[] } | null>(null);
  const [currentUser, setCurrentUser] = useState('');

  const load = useCallback(async () => {
    setLoading(true); setError(null);
    try {
      const res = await rpc<{ items: GitlabProfile[] }>('gitlabProfile.list', {});
      setItems(res.items ?? []);
    } catch (e) { setError(e instanceof Error ? e.message : '实例列表加载失败'); }
    finally { setLoading(false); }
  }, []);
  useEffect(() => { void load(); }, [load]);

  const create = async (e: FormEvent) => {
    e.preventDefault(); setError(null); setNotice(null);
    try {
      await rpc('gitlabProfile.create', {
        name: form.name.trim(), baseUrl: form.baseUrl.trim(),
        credentialRefId: form.credentialRefId.trim() || undefined,
      });
      setNotice(`实例「${form.name.trim()}」已创建`);
      setForm(EMPTY); setCreating(false);
      await load();
    } catch (err) { setError(err instanceof Error ? err.message : '创建失败'); }
  };

  const [pendingRemove, requestRemove] = useTwoStepConfirm();
  const remove = (p: GitlabProfile) =>
    requestRemove(p.id, () => {
      void (async () => {
        setError(null);
        try {
          await rpc('gitlabProfile.remove', { profileId: p.id, expectedRevision: p.revision });
          await load();
        } catch (e) { setError(e instanceof Error ? e.message : '删除失败'); }
      })();
    });

  const test = async (p: GitlabProfile) => {
    setTesting(p.id); setTestResult(null); setError(null);
    try { setTestResult(await rpc<{ status: string; steps: TestStep[] }>('gitlabProfile.test', { profileId: p.id })); }
    catch (e) { setError(e instanceof Error ? e.message : '测试失败'); }
    finally { setTesting(''); }
  };

  const fetchUser = async (p: GitlabProfile) => {
    setCurrentUser(''); setError(null);
    try {
      const u = await rpc<{ username?: string }>('gitlabProfile.currentUser', { profileId: p.id });
      setCurrentUser(u.username ?? '未知用户');
      setNotice(`当前用户：${u.username ?? '未知'}`);
    } catch (e) { setError(e instanceof Error ? e.message : '读取用户失败'); }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="GitLab"
        scope="全局"
        status={loading ? <StatusPill kind="checking" /> : items.length === 0 ? <StatusPill kind="pending" label="未配置" /> : <StatusPill kind="ready" />}
        description="GitLab 实例与凭据绑定。令牌经凭据引用存 Keychain，此页不显示。"
        actions={<button className="sg-btn" onClick={() => void load()} disabled={loading}><IconRefresh size={14} />刷新</button>}
      />

      {error ? <div className="sg-banner sg-banner--error" role="alert">操作失败：{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

      {!creating ? (
        <SettingsSection title="实例列表" actions={
          <button className="sg-btn" onClick={() => setCreating(true)}><IconPlus size={14} />新增实例</button>
        }>
          {loading ? (
            <div className="sg-skeleton-rows" aria-busy="true"><div className="sg-skeleton-row" /></div>
          ) : items.length === 0 ? (
            <div className="sg-empty">
              <span>暂无 GitLab 实例</span>
              <span className="sg-hint">Issue 导入、MR 创建依赖 GitLab 配置。</span>
            </div>
          ) : (
            <table className="sg-table" aria-label="GitLab 实例列表">
              <thead><tr><th>名称</th><th>端点</th><th>状态</th><th style={{ width: 260 }}>操作</th></tr></thead>
              <tbody>
                {items.map((p) => (
                  <tr key={p.id}>
                    <td><div style={{ fontWeight: 500 }}>{p.name}</div>
                      <div className="sg-hint">{p.credential_ref_id ? `凭据 ${p.credential_ref_id.slice(0, 10)}…` : '未绑定凭据'}</div>
                    </td>
                    <td><span className="sg-code">{p.base_url}</span></td>
                    <td><StatusPill kind={p.managed_source ? 'readonly' : p.status === 'ready' ? 'ready' : 'pending'}
                      label={p.managed_source ? `托管(${p.managed_source})` : undefined} /></td>
                    <td>
                      <div className="sg-row" style={{ gap: 6 }}>
                        <button className="sg-btn sg-btn--sm" disabled={testing === p.id} onClick={() => void test(p)}>
                          <IconZap size={12} />{testing === p.id ? '测试中…' : '测试'}
                        </button>
                        <button className="sg-btn sg-btn--sm" onClick={() => void fetchUser(p)}>
                          <IconUser size={12} />用户
                        </button>
                        {!p.managed_source ? (
                          <button
                          className="sg-btn sg-btn--sm sg-btn--danger"
                          onClick={() => remove(p)}
                          title={pendingRemove === p.id ? '再次点击确认删除' : `删除实例「${p.name}」（不可撤销）`}
                        >
                          {pendingRemove === p.id ? '确认删除？' : '删除'}
                        </button>
                        ) : null}
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          {currentUser ? <p className="sg-hint">当前用户：<code className="sg-code">{currentUser}</code></p> : null}
          {testResult ? (
            <table className="sg-table" style={{ marginTop: 10 }} aria-label="测试步骤" aria-live="polite">
              <thead><tr><th>步骤</th><th>状态</th><th>错误码</th></tr></thead>
              <tbody>
                {testResult.steps.map((s) => (
                  <tr key={s.name}>
                    <td>{s.name}</td>
                    <td><StatusPill kind={s.status === 'passed' ? 'ready' : 'error'} label={s.status} /></td>
                    <td className="sg-muted">{s.error_code ?? '—'}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          ) : null}
        </SettingsSection>
      ) : (
        <SettingsSection title="新增实例" description="凭据先在「凭据引用」页创建 GitLab Token 引用">
          <form className="sg-card sg-set-form" onSubmit={create}>
            <div className="sg-form-grid--2">
              <div className="sg-field">
                <label htmlFor="gl-name">名称 *</label>
                <input id="gl-name" className="sg-input" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} />
              </div>
              <div className="sg-field">
                <label htmlFor="gl-url">Base URL *</label>
                <input id="gl-url" className="sg-input" value={form.baseUrl} onChange={(e) => setForm({ ...form, baseUrl: e.target.value })} placeholder="https://gitlab.com" />
              </div>
              <div className="sg-field">
                <label htmlFor="gl-cred">凭据引用 ID</label>
                <input id="gl-cred" className="sg-input" value={form.credentialRefId} onChange={(e) => setForm({ ...form, credentialRefId: e.target.value })} placeholder="cr_…" />
              </div>
            </div>
            <div className="sg-row">
              <button type="submit" className="sg-btn sg-btn--primary" disabled={!form.name.trim() || !form.baseUrl.trim()}>
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
