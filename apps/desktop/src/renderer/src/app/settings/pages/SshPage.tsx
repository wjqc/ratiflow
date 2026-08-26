// SSH 目标机：目标列表（sg-table）+ host key 首用确认/变更阻断。
import { useCallback, useEffect, useState, type FormEvent } from 'react';
import { rpc } from '../../../rpc/client';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { IconPlus, IconRefresh, IconZap } from '../../../components/Icons';

interface SshTarget {
  id: string; revision: number; name: string; host: string; port: number;
  username: string; remote_dir: string; credential_ref_id?: string | null;
  fingerprint: string; fingerprint_status: string; status: string;
}
interface TestStep { name: string; status: string; error_code: string | null; detail?: { fingerprint?: string } | null }

const EMPTY = { name: '', host: '', port: 22, user: '', remoteDir: '', credentialRefId: '' };

export function SshPage() {
  const [items, setItems] = useState<SshTarget[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [form, setForm] = useState(EMPTY);
  const [testing, setTesting] = useState('');
  const [testResult, setTestResult] = useState<{ status: string; steps: TestStep[] } | null>(null);
  const [pendingFp, setPendingFp] = useState<{ targetId: string; fingerprint: string } | null>(null);

  const load = useCallback(async () => {
    setLoading(true); setError(null);
    try {
      const res = await rpc<{ items: SshTarget[] }>('sshTarget.list', {});
      setItems(res.items ?? []);
    } catch (e) { setError(e instanceof Error ? e.message : '目标列表加载失败'); }
    finally { setLoading(false); }
  }, []);
  useEffect(() => { void load(); }, [load]);

  const create = async (e: FormEvent) => {
    e.preventDefault(); setError(null); setNotice(null);
    try {
      await rpc('sshTarget.create', {
        name: form.name.trim() || form.host.trim(), host: form.host.trim(), port: form.port,
        user: form.user.trim(), remoteDir: form.remoteDir.trim(),
        credentialRefId: form.credentialRefId.trim() || undefined,
      });
      setNotice('SSH 目标已创建');
      setForm(EMPTY); setCreating(false);
      await load();
    } catch (err) { setError(err instanceof Error ? err.message : '创建失败'); }
  };

  const remove = async (t: SshTarget) => {
    if (!window.confirm(`删除目标「${t.name}」？不可撤销。`)) return;
    setError(null);
    try {
      await rpc('sshTarget.remove', { targetId: t.id, expectedRevision: t.revision });
      await load();
    } catch (e) { setError(e instanceof Error ? e.message : '删除失败'); }
  };

  const test = async (t: SshTarget) => {
    setTesting(t.id); setTestResult(null); setPendingFp(null); setError(null);
    try {
      const report = await rpc<{ status: string; steps: TestStep[] }>('sshTarget.test', { targetId: t.id });
      setTestResult(report);
      const hostKey = report.steps.find((s) => s.error_code === 'HOST_KEY_FIRST_USE');
      if (hostKey?.detail?.fingerprint) {
        setPendingFp({ targetId: t.id, fingerprint: hostKey.detail.fingerprint });
      }
    } catch (e) { setError(e instanceof Error ? e.message : '测试失败'); }
    finally { setTesting(''); }
  };

  const accept = async () => {
    if (!pendingFp) return;
    setError(null);
    try {
      await rpc('sshTarget.acceptHostKey', { targetId: pendingFp.targetId, fingerprint: pendingFp.fingerprint });
      setNotice('主机指纹已确认保存');
      setPendingFp(null);
      await load();
    } catch (e) { setError(e instanceof Error ? e.message : '确认失败'); }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="SSH 目标机"
        scope="全局"
        status={loading ? <StatusPill kind="checking" /> : items.length === 0 ? <StatusPill kind="pending" label="未配置" /> : <StatusPill kind="ready" />}
        description="部署与验证的 SSH 目标。首次连接必须显式确认指纹；指纹变化默认阻断。"
        actions={<button className="sg-btn" onClick={() => void load()} disabled={loading}><IconRefresh size={14} />刷新</button>}
      />

      {error ? <div className="sg-banner sg-banner--error" role="alert">操作失败：{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

      {!creating ? (
        <SettingsSection title="目标列表" actions={
          <button className="sg-btn" onClick={() => setCreating(true)}><IconPlus size={14} />新增目标</button>
        }>
          {loading ? (
            <div className="sg-skeleton-rows" aria-busy="true"><div className="sg-skeleton-row" /></div>
          ) : items.length === 0 ? (
            <div className="sg-empty">
              <span>暂无 SSH 目标</span>
              <span className="sg-hint">部署关依赖至少一个已确认指纹的目标。</span>
            </div>
          ) : (
            <table className="sg-table" aria-label="SSH 目标列表">
              <thead><tr><th>名称</th><th>地址</th><th>指纹</th><th style={{ width: 180 }}>操作</th></tr></thead>
              <tbody>
                {items.map((t) => (
                  <tr key={t.id}>
                    <td><div style={{ fontWeight: 500 }}>{t.name}</div>
                      <div className="sg-hint">{t.credential_ref_id ? `凭据 ${t.credential_ref_id.slice(0, 10)}…` : '未绑定凭据'}</div>
                    </td>
                    <td><span className="sg-code">{t.username}@{t.host}:{t.port}</span>
                      <div className="sg-path">{t.remote_dir || '未设远程目录'}</div>
                    </td>
                    <td>
                      <StatusPill kind={t.fingerprint_status === 'accepted' ? 'ready' : t.fingerprint_status === 'changed' ? 'error' : 'pending'}
                        label={t.fingerprint_status === 'accepted' ? '已确认' : t.fingerprint_status === 'changed' ? '指纹变化' : '未确认'} />
                      {t.fingerprint ? <div className="sg-path">{t.fingerprint.slice(0, 20)}…</div> : null}
                    </td>
                    <td>
                      <div className="sg-row" style={{ gap: 6 }}>
                        <button className="sg-btn sg-btn--sm" disabled={testing === t.id} onClick={() => void test(t)}>
                          <IconZap size={12} />{testing === t.id ? '测试中…' : '测试'}
                        </button>
                        <button className="sg-btn sg-btn--sm sg-btn--danger" onClick={() => void remove(t)}>删除</button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          {pendingFp ? (
            <div className="sg-banner sg-banner--warning" role="alert" style={{ marginTop: 10 }}>
              <div style={{ fontWeight: 500 }}>首次连接——确认主机指纹</div>
              <code className="sg-code">{pendingFp.fingerprint}</code>
              <div className="sg-row" style={{ marginTop: 6 }}>
                <button className="sg-btn sg-btn--primary sg-btn--sm" onClick={() => void accept()}>确认并保存</button>
                <button className="sg-btn sg-btn--sm" onClick={() => setPendingFp(null)}>取消</button>
              </div>
            </div>
          ) : null}
          {testResult ? (
            <table className="sg-table" style={{ marginTop: 10 }} aria-label="测试步骤" aria-live="polite">
              <thead><tr><th>步骤</th><th>状态</th><th>错误码</th></tr></thead>
              <tbody>
                {testResult.steps.map((s) => (
                  <tr key={s.name}>
                    <td>{s.name}</td>
                    <td><StatusPill kind={s.status === 'passed' ? 'ready' : s.status === 'action_required' ? 'pending' : 'error'} label={s.status} /></td>
                    <td className="sg-muted">{s.error_code ?? '—'}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          ) : null}
        </SettingsSection>
      ) : (
        <SettingsSection title="新增目标">
          <form className="sg-card sg-set-form" onSubmit={create}>
            <div className="sg-form-grid--2">
              <div className="sg-field">
                <label htmlFor="ssh-name">名称</label>
                <input id="ssh-name" className="sg-input" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="部署机-测试" />
              </div>
              <div className="sg-field">
                <label htmlFor="ssh-host">Host *</label>
                <input id="ssh-host" className="sg-input" value={form.host} onChange={(e) => setForm({ ...form, host: e.target.value })} />
              </div>
              <div className="sg-field">
                <label htmlFor="ssh-user">用户 *</label>
                <input id="ssh-user" className="sg-input" value={form.user} onChange={(e) => setForm({ ...form, user: e.target.value })} placeholder="deploy" />
              </div>
              <div className="sg-field">
                <label htmlFor="ssh-port">端口</label>
                <input id="ssh-port" className="sg-input" type="number" value={form.port} onChange={(e) => setForm({ ...form, port: Number(e.target.value) })} />
              </div>
              <div className="sg-field">
                <label htmlFor="ssh-dir">远程目录</label>
                <input id="ssh-dir" className="sg-input" value={form.remoteDir} onChange={(e) => setForm({ ...form, remoteDir: e.target.value })} placeholder="/srv/app" />
              </div>
              <div className="sg-field">
                <label htmlFor="ssh-cred">凭据引用 ID</label>
                <input id="ssh-cred" className="sg-input" value={form.credentialRefId} onChange={(e) => setForm({ ...form, credentialRefId: e.target.value })} placeholder="cr_…" />
              </div>
            </div>
            <div className="sg-row">
              <button type="submit" className="sg-btn sg-btn--primary" disabled={!form.host.trim() || !form.user.trim()}>
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
