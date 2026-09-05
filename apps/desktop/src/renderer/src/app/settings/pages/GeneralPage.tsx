// 常规：应用偏好与外观设置在同一页面统一保存。
import { useCallback, useEffect, useState, type FormEvent } from 'react';
import { rpc } from '../../../rpc/client';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsRow, SettingsToggle } from '../components/SettingsRow';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { IconCheck } from '../../../components/Icons';

const GENERAL_KEY = 'app.general';
const APPEARANCE_KEY = 'app.appearance';

export function GeneralPage() {
  const [stored, setStored] = useState<Record<string, unknown>>({});
  const [draft, setDraft] = useState<Record<string, unknown>>({});
  const [revision, setRevision] = useState<number | null>(null);
  const [appearanceStored, setAppearanceStored] = useState<Record<string, unknown>>({});
  const [appearanceDraft, setAppearanceDraft] = useState<Record<string, unknown>>({});
  const [appearanceRevision, setAppearanceRevision] = useState<number | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await rpc<{
        items: Array<{ key: string; value: Record<string, unknown>; revision: number }>;
      }>('settings.get', { scope: 'global', keys: [GENERAL_KEY, APPEARANCE_KEY] });
      const general = result.items.find((item) => item.key === GENERAL_KEY);
      const appearance = result.items.find((item) => item.key === APPEARANCE_KEY);
      setStored(general?.value ?? {});
      setDraft(general?.value ?? {});
      setRevision(general?.revision ?? null);
      setAppearanceStored(appearance?.value ?? {});
      setAppearanceDraft(appearance?.value ?? {});
      setAppearanceRevision(appearance?.revision ?? null);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '设置加载失败');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const generalDirty = JSON.stringify(draft) !== JSON.stringify(stored);
  const appearanceDirty = JSON.stringify(appearanceDraft) !== JSON.stringify(appearanceStored);
  const dirty = generalDirty || appearanceDirty;
  const setGeneral = (key: string, value: unknown) => setDraft((current) => ({ ...current, [key]: value }));
  const setAppearance = (key: string, value: unknown) => setAppearanceDraft((current) => ({ ...current, [key]: value }));

  const save = async (event: FormEvent) => {
    event.preventDefault();
    if (!dirty) return;
    setSaving(true);
    setError(null);
    setNotice(null);
    try {
      const patches = [];
      if (generalDirty) {
        patches.push({ key: GENERAL_KEY, value: draft, expectedRevision: revision });
      }
      if (appearanceDirty) {
        patches.push({ key: APPEARANCE_KEY, value: appearanceDraft, expectedRevision: appearanceRevision });
      }
      await rpc('settings.update', { scope: 'global', patches });
      if (appearanceDirty) {
        window.dispatchEvent(new CustomEvent('sg:appearance-changed', {
          detail: { key: APPEARANCE_KEY, value: appearanceDraft },
        }));
      }
      setNotice('设置已保存');
      await load();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '保存失败');
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="常规"
        scope="全局"
        status={
          loading
            ? <StatusPill kind="checking" />
            : dirty
              ? <StatusPill kind="pending" label="有未保存更改" />
              : <StatusPill kind="ready" label="已保存" />
        }
        description="管理应用启动、显示方式与本地诊断偏好。"
        actions={
          <button
            type="submit"
            form="sg-general-settings-form"
            className="sg-btn sg-btn--primary"
            disabled={!dirty || saving}
          >
            <IconCheck size={14} />
            {saving ? '保存中…' : '保存更改'}
          </button>
        }
      />

      {error ? <div className="sg-banner sg-banner--error" role="alert">操作失败：{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

      <form id="sg-general-settings-form" className="sg-settings-form-page" onSubmit={save}>
        <SettingsSection title="启动与默认">
          <div className="sg-setting-list">
            <SettingsRow title="恢复工作现场" description="启动时回到上次打开的项目与任务">
              <SettingsToggle
                label="启动时恢复上次项目与任务"
                checked={draft.restoreLastProject !== false}
                onChange={(checked) => setGeneral('restoreLastProject', checked)}
              />
            </SettingsRow>
            <SettingsRow title="语言" description="设置应用界面的显示语言">
              <select className="sg-select" value={String(draft.language ?? 'zh-CN')} onChange={(event) => setGeneral('language', event.target.value)} aria-label="语言">
                <option value="zh-CN">简体中文</option>
                <option value="system">跟随系统</option>
              </select>
            </SettingsRow>
            <SettingsRow title="时间格式" description="选择界面时间的显示方式">
              <select className="sg-select" value={String(draft.timeFormat ?? 'system')} onChange={(event) => setGeneral('timeFormat', event.target.value)} aria-label="时间格式">
                <option value="system">系统</option>
                <option value="24h">24 小时</option>
              </select>
            </SettingsRow>
            <SettingsRow title="本地遥测" description="仅记录匿名诊断计数，不上传内容">
              <SettingsToggle
                label="启用本地遥测计数"
                checked={draft.telemetry === true}
                onChange={(checked) => setGeneral('telemetry', checked)}
              />
            </SettingsRow>
          </div>
        </SettingsSection>

        <SettingsSection title="外观" description="调整界面的主题、密度与动效。">
          <div className="sg-setting-list">
            <SettingsRow title="主题" description="选择应用使用的颜色主题">
              <select className="sg-select" value={String(appearanceDraft.theme ?? 'light')} onChange={(event) => setAppearance('theme', event.target.value)} aria-label="主题">
                <option value="light">浅色</option>
                <option value="dark" disabled>深色（实验，未开放）</option>
              </select>
            </SettingsRow>
            <SettingsRow title="界面密度" description="调整列表与控件的紧凑程度">
              <select className="sg-select" value={String(appearanceDraft.density ?? 'comfortable')} onChange={(event) => setAppearance('density', event.target.value)} aria-label="界面密度">
                <option value="comfortable">舒适</option>
                <option value="compact">紧凑</option>
              </select>
            </SettingsRow>
            <SettingsRow title="动效" description="控制界面动画与过渡效果">
              <select className="sg-select" value={String(appearanceDraft.motion ?? 'full')} onChange={(event) => setAppearance('motion', event.target.value)} aria-label="动效">
                <option value="full">完整</option>
                <option value="reduced">减少（跟随系统偏好）</option>
              </select>
            </SettingsRow>
          </div>
        </SettingsSection>
      </form>
    </div>
  );
}
