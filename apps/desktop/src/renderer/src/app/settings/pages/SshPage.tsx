// S31 SSH 目标机：目标 CRUD + host key 首用确认/变更阻断 + 分步测试。
import { useCallback, useEffect, useState } from 'react';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { useConnectionTest } from '../hooks/useSettingsForm';
import { rpc, rpcErrorMessage } from '../../../rpc/client';

interface SshTarget {
  id: string; revision: number; name: string; host: string; port: number;
  username: string; remote_dir: string; credential_ref_id?: string | null;
  fingerprint: string; fingerprint_status: string; status: string;
}

export function SshPage() {
  const [targets, setTargets] = useState<SshTarget[]>([]);
  const [selected, setSelected] = useState('');
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [creating, setCreating] = useState(false);
  const [form, setForm] = useState({ name: '', host: '', port: 22, user: '', remoteDir: '', credentialRefId: '' });
  const test = useConnectionTest();
  const [pendingFingerprint, setPendingFingerprint] = useState('');

  const load = useCallback(async () => {
    setLoading(true); setError('');
    try {
      const result = await rpc<{ items: SshTarget[] }>('sshTarget.list', {});
      setTargets(result.items);
      if (!selected && result.items[0]) setSelected(result.items[0].id);
    } catch (reason) { setError(rpcErrorMessage(reason)); }
    finally { setLoading(false); }
  }, [selected]);
  useEffect(() => { void load(); }, [load]);

  const current = targets.find((t) => t.id === selected) ?? null;

  const create = async () => {
    setError('');
    try {
      await rpc('sshTarget.create', {
        name: form.name || form.host, host: form.host, port: form.port, user: form.user,
        remoteDir: form.remoteDir, credentialRefId: form.credentialRefId || undefined,
      });
      setCreating(false); setForm({ name: '', host: '', port: 22, user: '', remoteDir: '', credentialRefId: '' });
      await load();
    } catch (reason) { setError(rpcErrorMessage(reason)); }
  };

  const update = async (patch: Record<string, unknown>) => {
    if (!current) return;
    setError('');
    try {
      await rpc('sshTarget.update', { targetId: current.id, expectedRevision: current.revision, ...patch });
      await load();
    } catch (reason) { setError(rpcErrorMessage(reason)); }
  };

  const remove = async () => {
    if (!current || !window.confirm(`删除目标「${current.name}」？`)) return;
    try {
      await rpc('sshTarget.remove', { targetId: current.id, expectedRevision: current.revision });
      setSelected(''); await load();
    } catch (reason) { setError(rpcErrorMessage(reason)); }
  };

  const runTest = async () => {
    if (!current) return;
    setPendingFingerprint('');
    await test.run('sshTarget.test', { targetId: current.id });
    // 首用指纹：从步骤里取 HOST_KEY_FIRST_USE 的 fingerprint
    const hostKeyStep = test.steps.find((s) => s.errorCode === 'HOST_KEY_FIRST_USE');
    if (hostKeyStep) {
      const detail = hostKeyStep.detail as { fingerprint?: string } | null;
      setPendingFingerprint(detail?.fingerprint ?? '');
    }
  };

  const accept = async () => {
    if (!current || !pendingFingerprint) return;
    setError('');
    try {
      await rpc('sshTarget.acceptHostKey', { targetId: current.id, fingerprint: pendingFingerprint });
      setPendingFingerprint('');
      await load();
    } catch (reason) { setError(rpcErrorMessage(reason)); }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="SSH 目标机" scope="全局"
        status={targets.length === 0 ? <StatusPill kind="pending" label="未配置" /> : <StatusPill kind="ready" />}
        description="部署与验证的 SSH 目标。首次连接必须显式确认指纹；指纹变化默认阻断。"
        actions={!creating ? <button className="sg-btn sg-btn--primary" onClick={() => setCreating(true)}>新增目标</button> : null}
      />
      {error ? <div className="sg-banner sg-banner--error" role="alert">{error}</div> : null}

      {creating ? (
        <SettingsSection title="新增目标">
          <div className="sg-set-grid">
            <label className="sg-set-field"><span className="sg-set-label">名称</span>
              <input value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="部署机-测试" /></label>
            <label className="sg-set-field"><span className="sg-set-label">Host *</span>
              <input value={form.host} onChange={(e) => setForm({ ...form, host: e.target.value })} /></label>
            <label className="sg-set-field"><span className="sg-set-label">用户 *</span>
              <input value={form.user} onChange={(e) => setForm({ ...form, user: e.target.value })} placeholder="deploy" /></label>
            <label className="sg-set-field"><span className="sg-set-label">端口</span>
              <input type="number" value={form.port} onChange={(e) => setForm({ ...form, port: Number(e.target.value) })} /></label>
            <label className="sg-set-field"><span className="sg-set-label">远程目录</span>
              <input value={form.remoteDir} onChange={(e) => setForm({ ...form, remoteDir: e.target.value })} placeholder="/srv/app" /></label>
            <label className="sg-set-field"><span className="sg-set-label">凭据引用 ID</span>
              <input value={form.credentialRefId} onChange={(e) => setForm({ ...form, credentialRefId: e.target.value })} placeholder="cr_…" /></label>
          </div>
          <div className="sg-set-row">
            <button className="sg-btn sg-btn--primary" onClick={() => void create()}>创建</button>
            <button className="sg-btn" onClick={() => setCreating(false)}>取消</button>
          </div>
        </SettingsSection>
      ) : null}

      {targets.length > 0 && current ? (
        <div className="sg-set-cols">
          <div className="sg-set-list" role="list">
            {targets.map((t) => (
              <button key={t.id} role="listitem"
                className={`sg-set-item2 ${t.id === selected ? 'sg-set-item2--active' : ''}`}
                onClick={() => setSelected(t.id)}>
                <span>{t.name}</span>
                <StatusPill kind={t.fingerprint_status === 'accepted' ? 'ready' : t.fingerprint_status === 'changed' ? 'error' : 'pending'}
                  label={t.fingerprint_status === 'accepted' ? '已确认' : t.fingerprint_status === 'changed' ? '指纹变化' : '未确认'} />
              </button>
            ))}
          </div>
          <SettingsSection title={current.name} description={`${current.username}@${current.host}:${current.port} · revision ${current.revision}`}>
            <div className="sg-set-grid">
              <label className="sg-set-field"><span className="sg-set-label">远程目录</span>
                <input defaultValue={current.remote_dir} onBlur={(e) => { if (e.target.value !== current.remote_dir) void update({ remoteDir: e.target.value }); }} /></label>
              <label className="sg-set-field"><span className="sg-set-label">凭据引用 ID</span>
                <input defaultValue={current.credential_ref_id ?? ''} placeholder="未绑定" onBlur={(e) => { if (e.target.value !== (current.credential_ref_id ?? '')) void update({ credentialRefId: e.target.value }); }} /></label>
            </div>
            {current.fingerprint ? (
              <p className="sg-hint">已确认指纹：<code>{current.fingerprint}</code></p>
            ) : null}
            <div className="sg-set-row" aria-live="polite">
              <button className="sg-btn" disabled={test.testing} onClick={() => void runTest()}>
                {test.testing ? '测试中…' : '测试连接'}
              </button>
              <button className="sg-btn sg-btn--danger" onClick={() => void remove()}>删除</button>
              {test.overall !== 'idle' ? <StatusPill kind={test.overall === 'ready' ? 'ready' : test.overall === 'action_required' ? 'pending' : 'error'} label={test.overall} /> : null}
            </div>
            {pendingFingerprint ? (
              <div className="sg-banner sg-banner--error" role="alert">
                <p>首次连接——确认主机指纹：</p>
                <p><code>{pendingFingerprint}</code></p>
                <button className="sg-btn sg-btn--primary" onClick={() => void accept()}>确认并保存指纹</button>
              </div>
            ) : null}
            {test.steps.length > 0 ? (
              <ul className="sg-set-steps">
                {test.steps.map((s) => (
                  <li key={s.name}>
                    <span aria-hidden>{s.status === 'passed' ? '✓' : s.status === 'action_required' ? '?' : '✕'}</span>
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
