// 工作台模型选择器：列出所有 Profile × 可用模型，选中即写入全局默认
// （modelRoute 主档 + 该 Profile 的 default_model），对后续 Agent 任务立即生效。
import { useCallback, useEffect, useRef, useState } from 'react';
import { rpc } from '../rpc/client';
import { IconCheck, IconChevronDown, IconCpu } from '../components/Icons';

interface ModelProfile {
  id: string;
  revision: number;
  name: string;
  provider_kind: string;
  default_model: string;
  models?: string[];
}

interface ModelRoute {
  scope: string;
  taskKind: string;
  primaryProfileId: string;
  revision: number;
}

export function ModelPicker({ onChange }: { onChange?: () => void }) {
  const [profiles, setProfiles] = useState<ModelProfile[]>([]);
  const [route, setRoute] = useState<ModelRoute | null>(null);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const root = useRef<HTMLDivElement>(null);

  const load = useCallback(async () => {
    try {
      const [profileRes, routeRes] = await Promise.all([
        rpc<{ items: ModelProfile[] }>('modelProfile.list', {}),
        rpc<ModelRoute[]>('modelRoute.get', {}).catch(() => []),
      ]);
      setProfiles(profileRes.items ?? []);
      setRoute(
        (Array.isArray(routeRes) ? routeRes : []).find((item) => item.taskKind === 'default') ?? null,
      );
    } catch {
      /* core 未就绪时静默：按钮仍渲染，选择时报错 */
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    window.addEventListener('mousedown', close);
    return () => window.removeEventListener('mousedown', close);
  }, [open]);

  const routeProfileId = route?.primaryProfileId ?? '';
  const current = profiles.find((profile) => profile.id === routeProfileId) ?? null;
  const label = current ? current.default_model || '未指定模型' : '选择模型';

  // 只列已同步的模型（profile.models）；未同步的供应商不出现，避免拿预设列表当真实能力。
  // 已配置的 default_model 始终保留为一项（它是当前真实生效的模型）。
  const groups = profiles
    .map((profile) => {
      const models = (profile.models ?? [])
        .concat(profile.default_model ? [profile.default_model] : [])
        .filter((model, index, all) => all.indexOf(model) === index);
      return { profile, models };
    })
    .filter((group) => group.models.length > 0);

  // 未同步过模型的供应商被 groups 过滤掉了：提示用户去设置页同步，而不是无声消失。
  const unsyncedNames = profiles
    .filter((p) => !groups.some((g) => g.profile.id === p.id))
    .map((p) => `“${p.name}”`)
    .join('、');

  const isRevisionConflict = (reason: unknown): boolean =>
    (reason as { code?: string } | null)?.code === 'conflict';

  const choose = async (profile: ModelProfile, model: string) => {
    setBusy(true);
    setError('');
    let ok = false;
    try {
      if (profile.id !== routeProfileId) {
        const updated = await rpc<ModelRoute[]>('modelRoute.update', {
          route: { scope: 'global', taskKind: 'default', primaryProfileId: profile.id },
          expectedRevision: route?.revision ?? 0,
        });
        setRoute(
          (Array.isArray(updated) ? updated : []).find((item) => item.taskKind === 'default') ?? null,
        );
      }
      if (model !== profile.default_model) {
        await rpc('modelProfile.update', {
          profileId: profile.id,
          defaultModel: model,
          expectedRevision: profile.revision,
        });
      }
      ok = true;
      setOpen(false);
    } catch (reason) {
      // 两步写（路由 + 默认模型）可能只成功一步：重读权威状态，
      // 界面绝不显示与库不一致的"已切换"；冲突翻译成可操作的文案。
      const conflict = isRevisionConflict(reason);
      setError(
        conflict
          ? '设置刚被其他窗口修改，已刷新当前状态，请重试'
          : reason instanceof Error
            ? `切换失败：${reason.message}`
            : '切换失败',
      );
    } finally {
      setBusy(false);
      await load();
      if (ok) onChange?.();
    }
  };

  return (
    <div className="sg-model-picker" ref={root}>
      <button
        className="sg-model-picker-btn"
        onClick={() => {
          // 打开菜单时重读：设置页同步/切换后回到工作台，组件可能未重挂载，
          // 挂载期快照会报 REVISION_CONFLICT 或列过期模型。
          if (!open) void load();
          setOpen((value) => !value);
        }}
        disabled={busy}
        title={current ? `全局默认模型：${current.name} / ${label}（点击切换）` : '选择全局默认模型'}
        aria-label="切换模型"
        aria-expanded={open}
      >
        <IconCpu size={13} />
        <span>{label}</span>
        <IconChevronDown size={12} />
      </button>
      {error ? <span className="sg-model-picker-error">{error}</span> : null}
      {open ? (
        <div className="sg-model-picker-menu" role="listbox" aria-label="选择全局默认模型">
          {groups.length === 0 ? (
            <div className="sg-model-picker-empty">还没有已同步的模型。请先到「设置 → 模型与路由」同步模型。</div>
          ) : (
            groups.map(({ profile, models }) => (
              <div key={profile.id} className="sg-model-picker-group">
                <div className="sg-model-picker-group-name">
                  {profile.name}
                  {profile.id === routeProfileId ? ' · 默认' : ''}
                </div>
                {models.map((model) => {
                  const active = profile.id === routeProfileId && model === profile.default_model;
                  return (
                    <button
                      key={`${profile.id}/${model}`}
                      className={active ? 'is-active' : ''}
                      disabled={busy}
                      onClick={() => void choose(profile, model)}
                    >
                      <span>{model}</span>
                      {active ? <IconCheck size={13} /> : null}
                    </button>
                  );
                })}
              </div>
            ))
          )}
          {groups.length > 0 && unsyncedNames.length > 0 ? (
            <div className="sg-model-picker-empty">
              {unsyncedNames}还未同步模型，可到「设置 → 模型与路由」点「同步模型」。
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
