// 执行与沙箱：executor.settings + executor.check 运行自检。
import { useCallback, useEffect, useState, type FormEvent } from 'react';
import { rpc } from '../../../rpc/client';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { IconCheck, IconZap } from '../../../components/Icons';

interface ExecutorSettings {
  mode?: string;
  memoryMB?: number;
  cpus?: number;
  timeoutSec?: number;
  networkOff?: boolean;
  revision: number;
}
interface CheckStep { name: string; status: string; detail: string }

const MODE_HINT: Record<string, string> = {
  docker: '推荐。一次性容器、禁网、资源限制。',
  safe_restricted: '只读命令白名单；自动写/执行禁用。',
  disabled: '全部执行禁用。',
  unsafe_explicit: '不提供容器隔离；所有执行进入日志与证据标注。',
};

export function ExecutionPage() {
  const [value, setValue] = useState<ExecutorSettings | null>(null);
  const [draft, setDraft] = useState<ExecutorSettings>({ revision: 0 });
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const [check, setCheck] = useState<{ status: string; steps: CheckStep[] } | null>(null);

  const load = useCallback(async () => {
    setLoading(true); setError(null);
    try {
      const res = await rpc<ExecutorSettings>('executor.settings.get', {});
      setValue(res); setDraft(res);
    } catch (e) {
      setError(e instanceof Error ? e.message : '执行设置加载失败');
    } finally { setLoading(false); }
  }, []);
  useEffect(() => { void load(); }, [load]);

  const dirty = JSON.stringify(draft) !== JSON.stringify(value);
  const set = (key: keyof ExecutorSettings, v: unknown) => setDraft((d) => ({ ...d, [key]: v }));

  const save = async (e: FormEvent) => {
    e.preventDefault();
    setSaving(true); setError(null); setNotice(null);
    try {
      await rpc('executor.settings.update', {
        settings: draft,
        expectedRevision: value?.revision ?? 0,
        unsafeConfirmed: draft.mode === 'unsafe_explicit',
      });
      setNotice('执行设置已保存');
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : '保存失败');
    } finally { setSaving(false); }
  };

  const runCheck = async () => {
    setChecking(true); setError(null);
    try {
      setCheck(await rpc<{ status: string; steps: CheckStep[] }>('executor.check', {}));
    } catch (e) {
      setError(e instanceof Error ? e.message : '自检失败');
    } finally { setChecking(false); }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="执行与沙箱"
        scope="本地"
        status={loading ? <StatusPill kind="checking" /> : dirty ? <StatusPill kind="pending" label="有未保存更改" /> : <StatusPill kind="ready" label="已保存" />}
        description="Agent 命令执行模式与资源限制。"
      />

      {error ? <div className="sg-banner sg-banner--error" role="alert">操作失败：{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

      <SettingsSection title="模式与资源">
        <form className="sg-card sg-set-form" onSubmit={save}>
          <div className="sg-field">
            <label htmlFor="ex-mode">执行模式</label>
            <select id="ex-mode" className="sg-select" value={draft.mode ?? 'safe_restricted'} onChange={(e) => set('mode', e.target.value)}>
              <option value="docker">Docker（推荐）</option>
              <option value="safe_restricted">安全受限（只读白名单）</option>
              <option value="disabled">禁用</option>
              <option value="unsafe_explicit">显式不安全（需确认）</option>
            </select>
            <p className="sg-hint">{MODE_HINT[draft.mode ?? 'safe_restricted']}</p>
            {draft.mode === 'unsafe_explicit' ? (
              <div className="sg-banner sg-banner--warning" role="alert" style={{ marginTop: 4 }}>
                不安全模式不提供容器级隔离；保存即视为二次确认。
              </div>
            ) : null}
          </div>
          <div className="sg-form-grid--2">
            <div className="sg-field">
              <label htmlFor="ex-mem">内存（MB）</label>
              <input id="ex-mem" className="sg-input" type="number" min={128} max={8192}
                value={draft.memoryMB ?? 256} onChange={(e) => set('memoryMB', Number(e.target.value))} />
            </div>
            <div className="sg-field">
              <label htmlFor="ex-cpu">CPU</label>
              <input id="ex-cpu" className="sg-input" type="number" min={1} max={8} step={0.5}
                value={draft.cpus ?? 1} onChange={(e) => set('cpus', Number(e.target.value))} />
            </div>
            <div className="sg-field">
              <label htmlFor="ex-timeout">超时（秒）</label>
              <input id="ex-timeout" className="sg-input" type="number" min={10} max={3600}
                value={draft.timeoutSec ?? 120} onChange={(e) => set('timeoutSec', Number(e.target.value))} />
            </div>
            <div className="sg-field">
              <label htmlFor="ex-net" style={{ cursor: 'pointer' }}>
                <input id="ex-net" type="checkbox" checked={draft.networkOff !== false}
                  onChange={(e) => set('networkOff', e.target.checked)} />
                默认禁网
              </label>
            </div>
          </div>
          <div className="sg-row">
            <button type="submit" className="sg-btn sg-btn--primary" disabled={!dirty || saving}>
              <IconCheck size={14} />
              {saving ? '保存中…' : '保存更改'}
            </button>
          </div>
        </form>
      </SettingsSection>

      <SettingsSection title="运行自检" description="创建一次性容器（禁网）→ 写入临时文件 → 销毁并验证清理">
        <div className="sg-card sg-set-form">
          <button className="sg-btn" disabled={checking} onClick={() => void runCheck()} aria-live="polite">
            <IconZap size={14} />
            {checking ? '自检运行中…' : '运行自检'}
          </button>
          {check ? (
            <table className="sg-table" style={{ marginTop: 8 }} aria-label="自检步骤">
              <thead><tr><th>步骤</th><th>状态</th><th>详情</th></tr></thead>
              <tbody>
                {check.steps.map((s) => (
                  <tr key={s.name}>
                    <td>{s.name}</td>
                    <td><StatusPill kind={s.status === 'passed' ? 'ready' : 'error'} label={s.status} /></td>
                    <td className="sg-muted">{s.detail.slice(0, 80)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          ) : null}
        </div>
      </SettingsSection>
    </div>
  );
}
