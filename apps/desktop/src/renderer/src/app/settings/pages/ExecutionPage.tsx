// S22 执行与沙箱：executor.settings + executor.check 运行自检（一次性容器写入/清理验证）。
import { useCallback, useEffect, useState } from 'react';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { rpc, rpcErrorMessage } from '../../../rpc/client';

interface ExecutorSettings {
  mode?: string; memoryMB?: number; cpus?: number; timeoutSec?: number;
  networkOff?: boolean; revision: number;
}
interface CheckStep { name: string; status: string; detail: string }

export function ExecutionPage() {
  const [value, setValue] = useState<ExecutorSettings | null>(null);
  const [draft, setDraft] = useState<ExecutorSettings>({ revision: 0 });
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [checking, setChecking] = useState(false);
  const [checkResult, setCheckResult] = useState<{ status: string; steps: CheckStep[] } | null>(null);

  const load = useCallback(async () => {
    setLoading(true); setError('');
    try {
      const result = await rpc<ExecutorSettings>('executor.settings.get', {});
      setValue(result); setDraft(result);
    } catch (reason) { setError(rpcErrorMessage(reason)); }
    finally { setLoading(false); }
  }, []);
  useEffect(() => { void load(); }, [load]);

  const dirty = JSON.stringify(draft) !== JSON.stringify(value);
  const set = (key: keyof ExecutorSettings, v: unknown) => setDraft({ ...draft, [key]: v });

  const save = async () => {
    setSaving(true); setError(''); setNotice('');
    try {
      await rpc('executor.settings.update', {
        settings: draft,
        expectedRevision: value?.revision ?? 0,
        unsafeConfirmed: draft.mode === 'unsafe_explicit',
      });
      setNotice('执行设置已保存');
      await load();
    } catch (reason) { setError(rpcErrorMessage(reason)); }
    finally { setSaving(false); }
  };

  const runCheck = async () => {
    setChecking(true); setError('');
    try {
      const result = await rpc<{ status: string; steps: CheckStep[] }>('executor.check', {});
      setCheckResult(result);
    } catch (reason) { setError(rpcErrorMessage(reason)); }
    finally { setChecking(false); }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="执行与沙箱" scope="本地"
        status={loading ? <StatusPill kind="checking" /> : dirty ? <StatusPill kind="pending" label="有未保存更改" /> : <StatusPill kind="ready" label="已保存" />}
        description="Agent 命令执行模式与资源限制。显式不安全模式需二次确认且计入证据标注。"
        actions={<button className="sg-btn sg-btn--primary" disabled={!dirty || saving} onClick={() => void save()}>{saving ? '保存中…' : '保存更改'}</button>}
      />
      {error ? <div className="sg-banner sg-banner--error" role="alert">{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}
      <SettingsSection title="模式">
        <label className="sg-set-field">
          <span className="sg-set-label">执行模式</span>
          <select value={draft.mode ?? 'safe_restricted'} onChange={(e) => set('mode', e.target.value)}>
            <option value="docker">Docker（推荐）</option>
            <option value="safe_restricted">安全受限（只读白名单）</option>
            <option value="disabled">禁用</option>
            <option value="unsafe_explicit">显式不安全（需确认）</option>
          </select>
        </label>
        {draft.mode === 'unsafe_explicit' ? (
          <p className="sg-hint" role="alert">⚠ 不安全模式不提供容器级隔离；所有执行进入日志与证据标注。</p>
        ) : null}
      </SettingsSection>
      <SettingsSection title="资源限制">
        <div className="sg-set-grid">
          <label className="sg-set-field"><span className="sg-set-label">内存（MB）</span>
            <input type="number" min={128} max={8192} value={draft.memoryMB ?? 256} onChange={(e) => set('memoryMB', Number(e.target.value))} /></label>
          <label className="sg-set-field"><span className="sg-set-label">CPU</span>
            <input type="number" min={1} max={8} step={0.5} value={draft.cpus ?? 1} onChange={(e) => set('cpus', Number(e.target.value))} /></label>
          <label className="sg-set-field"><span className="sg-set-label">超时（秒）</span>
            <input type="number" min={10} max={3600} value={draft.timeoutSec ?? 120} onChange={(e) => set('timeoutSec', Number(e.target.value))} /></label>
        </div>
        <label className="sg-set-field">
          <input type="checkbox" checked={draft.networkOff !== false} onChange={(e) => set('networkOff', e.target.checked)} />
          <span>默认禁网</span>
        </label>
      </SettingsSection>
      <SettingsSection title="运行自检" description="创建一次性容器（禁网）→ 写入临时文件 → 销毁并验证清理">
        <button className="sg-btn" disabled={checking} onClick={() => void runCheck()} aria-live="polite">
          {checking ? '自检运行中…' : '运行自检'}
        </button>
        {checkResult ? (
          <>
            <p><StatusPill kind={checkResult.status === 'ready' ? 'ready' : 'error'} label={checkResult.status} /></p>
            <ul className="sg-set-steps">
              {checkResult.steps.map((s) => (
                <li key={s.name}>
                  <span aria-hidden>{s.status === 'passed' ? '✓' : '✕'}</span>
                  {s.name} — {s.detail.slice(0, 80)}
                </li>
              ))}
            </ul>
          </>
        ) : null}
      </SettingsSection>
    </div>
  );
}
