// 外观：主题/密度/动效（深色 MVP 标注实验状态）。
import { useCallback, useEffect, useState, type FormEvent } from 'react';
import { rpc } from '../../../rpc/client';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { IconCheck } from '../../../components/Icons';

const KEY = 'app.appearance';

export function AppearancePage() {
  const [stored, setStored] = useState<Record<string, unknown>>({});
  const [draft, setDraft] = useState<Record<string, unknown>>({});
  const [revision, setRevision] = useState<number | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true); setError(null);
    try {
      const res = await rpc<{ items: Array<{ key: string; value: Record<string, unknown>; revision: number }> }>(
        'settings.get', { scope: 'global', keys: [KEY] });
      const entry = res.items.find((i) => i.key === KEY);
      setStored(entry?.value ?? {});
      setDraft(entry?.value ?? {});
      setRevision(entry?.revision ?? null);
    } catch (e) {
      setError(e instanceof Error ? e.message : '外观设置加载失败');
    } finally { setLoading(false); }
  }, []);
  useEffect(() => { void load(); }, [load]);

  const dirty = JSON.stringify(draft) !== JSON.stringify(stored);
  const set = (key: string, v: unknown) => setDraft({ ...draft, [key]: v });

  const save = async (e: FormEvent) => {
    e.preventDefault();
    setSaving(true); setError(null); setNotice(null);
    try {
      await rpc('settings.update', { scope: 'global', patches: [{ key: KEY, value: draft, expectedRevision: revision }] });
      setNotice('外观设置已保存');
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : '保存失败');
    } finally { setSaving(false); }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="外观"
        scope="全局"
        status={loading ? <StatusPill kind="checking" /> : dirty ? <StatusPill kind="pending" label="有未保存更改" /> : <StatusPill kind="ready" label="已保存" />}
        description="主题、密度与动效偏好。"
      />

      {error ? <div className="sg-banner sg-banner--error" role="alert">操作失败：{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

      <SettingsSection title="主题与密度">
        <form className="sg-card sg-set-form" onSubmit={save}>
          <div className="sg-form-grid--2">
            <div className="sg-field">
              <label htmlFor="ap-theme">主题</label>
              <select id="ap-theme" className="sg-select" value={String(draft.theme ?? 'light')} onChange={(e) => set('theme', e.target.value)}>
                <option value="light">浅色</option>
                <option value="dark" disabled>深色（实验，未开放）</option>
              </select>
            </div>
            <div className="sg-field">
              <label htmlFor="ap-density">界面密度</label>
              <select id="ap-density" className="sg-select" value={String(draft.density ?? 'comfortable')} onChange={(e) => set('density', e.target.value)}>
                <option value="comfortable">舒适</option>
                <option value="compact">紧凑</option>
              </select>
            </div>
          </div>
          <div className="sg-field" style={{ maxWidth: 240 }}>
            <label htmlFor="ap-motion">动效</label>
            <select id="ap-motion" className="sg-select" value={String(draft.motion ?? 'full')} onChange={(e) => set('motion', e.target.value)}>
              <option value="full">完整</option>
              <option value="reduced">减少（尊重系统偏好）</option>
            </select>
          </div>
          <div className="sg-row">
            <button type="submit" className="sg-btn sg-btn--primary" disabled={!dirty || saving}>
              <IconCheck size={14} />
              {saving ? '保存中…' : '保存更改'}
            </button>
          </div>
        </form>
      </SettingsSection>
    </div>
  );
}
