// S02 外观：主题/密度/动效（MVP 浅色开放，深色标注实验状态）。
import { useCallback, useEffect, useState } from 'react';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { rpc, rpcErrorMessage } from '../../../rpc/client';

const KEY = 'app.appearance';

export function AppearancePage() {
  const [stored, setStored] = useState<Record<string, unknown>>({});
  const [draft, setDraft] = useState<Record<string, unknown>>({});
  const [revision, setRevision] = useState<number | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');

  const load = useCallback(async () => {
    setLoading(true); setError('');
    try {
      const result = await rpc<{ items: Array<{ key: string; value: Record<string, unknown>; revision: number }> }>('settings.get', { scope: 'global', keys: [KEY] });
      const entry = result.items.find((i) => i.key === KEY);
      setStored(entry?.value ?? {});
      setDraft(entry?.value ?? {});
      setRevision(entry?.revision ?? null);
    } catch (reason) { setError(rpcErrorMessage(reason)); }
    finally { setLoading(false); }
  }, []);
  useEffect(() => { void load(); }, [load]);

  const dirty = JSON.stringify(draft) !== JSON.stringify(stored);
  const set = (key: string, v: unknown) => setDraft({ ...draft, [key]: v });

  const save = async () => {
    setSaving(true); setError('');
    try {
      await rpc('settings.update', { scope: 'global', patches: [{ key: KEY, value: draft, expectedRevision: revision }] });
      await load();
    } catch (reason) { setError(rpcErrorMessage(reason)); }
    finally { setSaving(false); }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="外观" scope="全局"
        status={loading ? <StatusPill kind="checking" /> : dirty ? <StatusPill kind="pending" label="有未保存更改" /> : <StatusPill kind="ready" label="已保存" />}
        description="主题、密度与动效偏好。深色主题在 MVP 中标记为实验状态。"
        actions={<button className="sg-btn sg-btn--primary" disabled={!dirty || saving} onClick={() => void save()}>{saving ? '保存中…' : '保存更改'}</button>}
      />
      {error ? <div className="sg-banner sg-banner--error" role="alert">{error}</div> : null}
      <SettingsSection title="主题与密度">
        <label className="sg-set-field">
          <span className="sg-set-label">主题</span>
          <select value={String(draft.theme ?? 'light')} onChange={(e) => set('theme', e.target.value)}>
            <option value="light">浅色</option>
            <option value="dark" disabled>深色（实验，未开放）</option>
          </select>
        </label>
        <label className="sg-set-field">
          <span className="sg-set-label">界面密度</span>
          <select value={String(draft.density ?? 'comfortable')} onChange={(e) => set('density', e.target.value)}>
            <option value="comfortable">舒适</option>
            <option value="compact">紧凑</option>
          </select>
        </label>
        <label className="sg-set-field">
          <span className="sg-set-label">动效</span>
          <select value={String(draft.motion ?? 'full')} onChange={(e) => set('motion', e.target.value)}>
            <option value="full">完整</option>
            <option value="reduced">减少（尊重系统偏好）</option>
          </select>
        </label>
      </SettingsSection>
      <SettingsSection title="预览" description="使用真实组件预览，非静态图片">
        <div className="sg-set-preview" aria-label="组件预览">
          <button className="sg-btn sg-btn--primary">主操作</button>
          <button className="sg-btn">次操作</button>
          <StatusPill kind="ready" />
          <StatusPill kind="error" />
        </div>
      </SettingsSection>
    </div>
  );
}
