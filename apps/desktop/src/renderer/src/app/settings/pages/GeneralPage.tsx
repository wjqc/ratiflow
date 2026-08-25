// 常规：应用偏好（settings.get/update，非秘密）。
import { useCallback, useEffect, useState, type FormEvent } from 'react';
import { rpc } from '../../../rpc/client';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { IconCheck } from '../../../components/Icons';

const KEY = 'app.general';

export function GeneralPage() {
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
      setError(e instanceof Error ? e.message : '设置加载失败');
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
      setNotice('设置已保存');
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : '保存失败');
    } finally { setSaving(false); }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="常规"
        scope="全局"
        status={loading ? <StatusPill kind="checking" /> : dirty ? <StatusPill kind="pending" label="有未保存更改" /> : <StatusPill kind="ready" label="已保存" />}
        description="应用级偏好；此处不登记 GitLab 项目，不显示密钥。"
      />

      {error ? <div className="sg-banner sg-banner--error" role="alert">操作失败：{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

      <SettingsSection title="启动与默认" description="影响应用启动行为">
        <form className="sg-card sg-set-form" onSubmit={save}>
          <div className="sg-field">
            <label htmlFor="gen-restore">
              <input
                id="gen-restore"
                type="checkbox"
                checked={draft.restoreLastProject !== false}
                onChange={(e) => set('restoreLastProject', e.target.checked)}
              />
              启动时恢复上次项目与任务
            </label>
          </div>
          <div className="sg-form-grid--2">
            <div className="sg-field">
              <label htmlFor="gen-lang">语言</label>
              <select id="gen-lang" className="sg-select" value={String(draft.language ?? 'zh-CN')} onChange={(e) => set('language', e.target.value)}>
                <option value="zh-CN">简体中文</option>
                <option value="system">跟随系统</option>
              </select>
            </div>
            <div className="sg-field">
              <label htmlFor="gen-time">时间格式</label>
              <select id="gen-time" className="sg-select" value={String(draft.timeFormat ?? 'system')} onChange={(e) => set('timeFormat', e.target.value)}>
                <option value="system">系统</option>
                <option value="24h">24 小时</option>
              </select>
            </div>
          </div>
          <div className="sg-row">
            <button type="submit" className="sg-btn sg-btn--primary" disabled={!dirty || saving}>
              <IconCheck size={14} />
              {saving ? '保存中…' : '保存更改'}
            </button>
            <span className="sg-hint">保存后立即生效并写入审计。</span>
          </div>
        </form>
      </SettingsSection>

      <SettingsSection title="遥测" description="默认关闭；仅本地匿名诊断计数，不上传内容">
        <form className="sg-card sg-set-form" onSubmit={save}>
          <div className="sg-field">
            <label htmlFor="gen-telemetry">
              <input
                id="gen-telemetry"
                type="checkbox"
                checked={draft.telemetry === true}
                onChange={(e) => set('telemetry', e.target.checked)}
              />
              启用本地遥测计数
            </label>
            <p className="sg-hint"><IconCheck size={12} /> 关闭时不产生任何网络请求。</p>
          </div>
        </form>
      </SettingsSection>
    </div>
  );
}
