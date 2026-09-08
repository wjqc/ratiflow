// 常规：应用偏好与外观设置，改动即保存（无保存按钮）。
import { useCallback, useEffect, useRef, useState } from 'react';
import { rpc } from '../../../rpc/client';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsRow, SettingsToggle } from '../components/SettingsRow';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { useAutoSave } from '../hooks/useAutoSave';

const GENERAL_KEY = 'app.general';
const APPEARANCE_KEY = 'app.appearance';

export function GeneralPage() {
  const [general, setGeneral] = useState<Record<string, unknown>>({});
  const [appearance, setAppearance] = useState<Record<string, unknown>>({});
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const generalRef = useRef(general);
  generalRef.current = general;
  const appearanceRef = useRef(appearance);
  appearanceRef.current = appearance;
  const revisionRef = useRef<number | null>(null);
  const appearanceRevisionRef = useRef<number | null>(null);
  const lastSavedGeneral = useRef<Record<string, unknown> | null>(null);
  const lastSavedAppearance = useRef<Record<string, unknown> | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await rpc<{
        items: Array<{ key: string; value: Record<string, unknown>; revision: number }>;
      }>('settings.get', { scope: 'global', keys: [GENERAL_KEY, APPEARANCE_KEY] });
      const generalItem = result.items.find((item) => item.key === GENERAL_KEY);
      const appearanceItem = result.items.find((item) => item.key === APPEARANCE_KEY);
      const generalValue = generalItem?.value ?? {};
      const appearanceValue = appearanceItem?.value ?? {};
      revisionRef.current = generalItem?.revision ?? null;
      appearanceRevisionRef.current = appearanceItem?.revision ?? null;
      lastSavedGeneral.current = generalValue;
      lastSavedAppearance.current = appearanceValue;
      setGeneral(generalValue);
      setAppearance(appearanceValue);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '设置加载失败');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const persist = async () => {
    const currentGeneral = generalRef.current;
    const currentAppearance = appearanceRef.current;
    const patches: Array<{ key: string; value: Record<string, unknown>; expectedRevision: number | null }> = [];
    if (JSON.stringify(currentGeneral) !== JSON.stringify(lastSavedGeneral.current)) {
      patches.push({ key: GENERAL_KEY, value: currentGeneral, expectedRevision: revisionRef.current });
    }
    if (JSON.stringify(currentAppearance) !== JSON.stringify(lastSavedAppearance.current)) {
      patches.push({ key: APPEARANCE_KEY, value: currentAppearance, expectedRevision: appearanceRevisionRef.current });
    }
    if (patches.length === 0) return;
    try {
      const res = await rpc<{ items: Array<{ key: string; revision: number }> }>('settings.update', { scope: 'global', patches });
      for (const item of res.items ?? []) {
        if (item.key === GENERAL_KEY) revisionRef.current = item.revision;
        if (item.key === APPEARANCE_KEY) appearanceRevisionRef.current = item.revision;
      }
      if (patches.some((p) => p.key === GENERAL_KEY)) lastSavedGeneral.current = currentGeneral;
      if (patches.some((p) => p.key === APPEARANCE_KEY)) {
        lastSavedAppearance.current = currentAppearance;
        window.dispatchEvent(new CustomEvent('sg:appearance-changed', {
          detail: { key: APPEARANCE_KEY, value: currentAppearance },
        }));
      }
      setError(null);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '保存失败');
    }
  };

  const { pending, schedule } = useAutoSave(persist);

  const changeGeneral = (key: string, value: unknown) => {
    setGeneral((current) => ({ ...current, [key]: value }));
    schedule(0);
  };
  const changeAppearance = (key: string, value: unknown) => {
    setAppearance((current) => ({ ...current, [key]: value }));
    schedule(0);
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="常规"
        scope="全局"
        status={
          loading
            ? <StatusPill kind="checking" />
            : pending
              ? <StatusPill kind="pending" label="保存中…" />
              : <StatusPill kind="ready" label="已保存" />
        }
        description="管理应用启动、显示方式与本地诊断偏好。"
      />

      {error ? <div className="sg-banner sg-banner--error" role="alert">操作失败：{error}</div> : null}

      <SettingsSection title="启动与默认">
        <div className="sg-setting-list">
          <SettingsRow title="恢复工作现场" description="启动时回到上次打开的项目与工作页（任务需从列表重新打开，避免草稿误发送）">
            <SettingsToggle
              label="启动时恢复上次项目与工作页"
              checked={general.restoreLastProject !== false}
              onChange={(checked) => changeGeneral('restoreLastProject', checked)}
            />
          </SettingsRow>
          <SettingsRow title="语言" description="设置应用界面的显示语言">
            <select className="sg-select" value={String(general.language ?? 'zh-CN')} onChange={(event) => changeGeneral('language', event.target.value)} aria-label="语言">
              <option value="zh-CN">简体中文</option>
              <option value="system">跟随系统</option>
            </select>
          </SettingsRow>
          <SettingsRow title="时间格式" description="选择界面时间的显示方式">
            <select className="sg-select" value={String(general.timeFormat ?? 'system')} onChange={(event) => changeGeneral('timeFormat', event.target.value)} aria-label="时间格式">
              <option value="system">系统</option>
              <option value="24h">24 小时</option>
            </select>
          </SettingsRow>
          <SettingsRow title="本地遥测" description="仅记录匿名诊断计数，不上传内容">
            <SettingsToggle
              label="启用本地遥测计数"
              checked={general.telemetry === true}
              onChange={(checked) => changeGeneral('telemetry', checked)}
            />
          </SettingsRow>
        </div>
      </SettingsSection>

      <SettingsSection title="外观" description="调整界面的主题、密度与动效。">
        <div className="sg-setting-list">
          <SettingsRow title="主题" description="选择应用使用的颜色主题">
            <select className="sg-select" value={String(appearance.theme ?? 'light')} onChange={(event) => changeAppearance('theme', event.target.value)} aria-label="主题">
              <option value="light">浅色</option>
              <option value="dark" disabled>深色（实验，未开放）</option>
            </select>
          </SettingsRow>
          <SettingsRow title="界面密度" description="调整列表与控件的紧凑程度">
            <select className="sg-select" value={String(appearance.density ?? 'comfortable')} onChange={(event) => changeAppearance('density', event.target.value)} aria-label="界面密度">
              <option value="comfortable">舒适</option>
              <option value="compact">紧凑</option>
            </select>
          </SettingsRow>
          <SettingsRow title="动效" description="控制界面动画与过渡效果">
            <select className="sg-select" value={String(appearance.motion ?? 'full')} onChange={(event) => changeAppearance('motion', event.target.value)} aria-label="动效">
              <option value="full">完整</option>
              <option value="reduced">减少（跟随系统偏好）</option>
            </select>
          </SettingsRow>
        </div>
      </SettingsSection>
    </div>
  );
}
