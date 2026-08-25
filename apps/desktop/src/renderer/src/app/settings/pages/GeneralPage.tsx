// S01 应用常规：app.general 键值表单（settings.get/update，非秘密）。
import { useCallback, useEffect, useState } from 'react';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { rpc, rpcErrorMessage } from '../../../rpc/client';

const KEY = 'app.general';

export function GeneralPage() {
  const [stored, setStored] = useState<Record<string, unknown>>({});
  const [draft, setDraft] = useState<Record<string, unknown>>({});
  const [revision, setRevision] = useState<number | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [conflict, setConflict] = useState(false);
  const [savedAt, setSavedAt] = useState('');

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
    setSaving(true); setError(''); setConflict(false);
    try {
      await rpc('settings.update', {
        scope: 'global',
        patches: [{ key: KEY, value: draft, expectedRevision: revision }],
      });
      setSavedAt(new Date().toLocaleTimeString('zh-CN', { hour12: false }));
      await load();
    } catch (reason) {
      const msg = rpcErrorMessage(reason);
      setError(msg);
      if (/revision|冲突|conflict/i.test(msg)) setConflict(true);
    } finally { setSaving(false); }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="常规" scope="全局"
        status={loading ? <StatusPill kind="checking" /> : dirty ? <StatusPill kind="pending" label="有未保存更改" /> : <StatusPill kind="ready" label={savedAt ? `已保存 ${savedAt}` : '已保存'} />}
        description="应用级偏好。此处不登记 GitLab 项目、不显示密钥或环境变量。"
        actions={<button className="sg-btn sg-btn--primary" disabled={!dirty || saving} onClick={() => void save()}>{saving ? '保存中…' : '保存更改'}</button>}
      />
      {error ? <div className="sg-banner sg-banner--error" role="alert">{error}</div> : null}
      {conflict ? <div className="sg-banner sg-banner--error" role="alert">设置已被其他会话修改。刷新后重试，不覆盖对方更改。</div> : null}
      <SettingsSection title="启动与默认" description="影响应用启动行为">
        <label className="sg-set-field">
          <input type="checkbox" checked={draft.restoreLastProject !== false} onChange={(e) => set('restoreLastProject', e.target.checked)} />
          <span>启动时恢复上次项目/任务</span>
        </label>
        <label className="sg-set-field">
          <span className="sg-set-label">语言</span>
          <select value={String(draft.language ?? 'zh-CN')} onChange={(e) => set('language', e.target.value)}>
            <option value="zh-CN">简体中文</option>
            <option value="system">跟随系统</option>
          </select>
        </label>
        <label className="sg-set-field">
          <span className="sg-set-label">时间格式</span>
          <select value={String(draft.timeFormat ?? 'system')} onChange={(e) => set('timeFormat', e.target.value)}>
            <option value="system">系统</option>
            <option value="24h">24 小时</option>
          </select>
        </label>
      </SettingsSection>
      <SettingsSection title="遥测" description="默认关闭；仅本地匿名诊断计数，不上传内容">
        <label className="sg-set-field">
          <input type="checkbox" checked={draft.telemetry === true} onChange={(e) => set('telemetry', e.target.checked)} />
          <span>启用本地遥测计数</span>
        </label>
      </SettingsSection>
    </div>
  );
}
