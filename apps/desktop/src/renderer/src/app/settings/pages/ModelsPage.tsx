// 模型设置：围绕“选择供应商 → 配好凭据与模型 → 连接测试通过”组织。
// 诊断步骤默认折叠，避免把内部探测字段当成主界面。
import { useCallback, useEffect, useMemo, useState, type FormEvent } from 'react';
import { rpc } from '../../../rpc/client';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { StatusPill } from '../components/StatusPill';
import {
  IconCheck,
  IconChevronDown,
  IconChevronRight,
  IconPlus,
  IconRefresh,
  IconZap,
} from '../../../components/Icons';

interface ModelProfile {
  id: string;
  revision: number;
  name: string;
  provider_kind: string;
  base_url: string;
  credential_ref_id?: string | null;
  default_model: string;
  managed_source?: string | null;
  status: string;
  last_tested_at?: string | null;
}

interface TestStep {
  name: string;
  status: string;
  errorCode?: string | null;
}

interface ModelPreset {
  id: string;
  label: string;
  providerKind: string;
  baseUrl: string;
  defaultModel: string;
  models: { id: string; note: string }[];
}

interface ModelRoute {
  id: string;
  scope: string;
  taskKind: string;
  primaryProfileId: string;
  revision: number;
}

const KIND_LABEL: Record<string, string> = {
  zhipu: '智谱',
  deepseek: 'DeepSeek',
  openai_compatible: '自定义',
  fake: '测试模型',
};

const STEP_LABEL: Record<string, string> = {
  resolve: '网络地址',
  auth: '身份验证',
  generation: '文本生成',
  tool: '工具调用',
  vision: '图片理解',
};

const EMPTY_FORM = {
  name: '',
  presetId: 'custom',
  baseUrl: '',
  apiKey: '',
  credentialRefId: '',
  defaultModel: '',
};

export function ModelsPage() {
  const [items, setItems] = useState<ModelProfile[]>([]);
  const [presets, setPresets] = useState<ModelPreset[]>([]);
  const [routes, setRoutes] = useState<ModelRoute[]>([]);
  const [selectedId, setSelectedId] = useState('');
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [form, setForm] = useState(EMPTY_FORM);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{ status: string; steps: TestStep[] } | null>(null);
  const [showDiagnostics, setShowDiagnostics] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [modelsByProfile, setModelsByProfile] = useState<Record<string, string[]>>({});

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [list, presetRes, routeRes] = await Promise.all([
        rpc<{ items: ModelProfile[] }>('modelProfile.list', {}),
        rpc<{ items: ModelPreset[] }>('modelProvider.presets', {}).catch(() => ({ items: [] })),
        rpc<ModelRoute[]>('modelRoute.get', {}).catch(() => []),
      ]);
      const next = list.items ?? [];
      setItems(next);
      setPresets(presetRes.items ?? []);
      setRoutes(Array.isArray(routeRes) ? routeRes : []);
      setSelectedId((current) =>
        next.some((profile) => profile.id === current) ? current : next[0]?.id ?? '',
      );
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '模型列表加载失败');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const selected = items.find((profile) => profile.id === selectedId) ?? null;
  const activePreset = presets.find((preset) => preset.id === form.presetId);
  const route = routes.find((item) => item.taskKind === 'default') ?? routes[0] ?? null;
  const selectedModels = useMemo(() => {
    if (!selected) return [];
    const synced = modelsByProfile[selected.id] ?? [];
    return synced.length > 0 ? synced : selected.default_model ? [selected.default_model] : [];
  }, [modelsByProfile, selected]);

  const pickPreset = (id: string) => {
    const preset = presets.find((item) => item.id === id);
    setForm((current) => ({
      ...current,
      presetId: id,
      name: preset?.label ?? current.name,
      baseUrl: preset?.baseUrl ?? '',
      defaultModel: preset?.defaultModel ?? '',
    }));
  };

  const create = async (event: FormEvent) => {
    event.preventDefault();
    setError(null);
    setNotice(null);
    const preset = presets.find((item) => item.id === form.presetId);
    try {
      const created = await rpc<ModelProfile>('modelProfile.create', {
        name: form.name.trim(),
        providerKind: preset?.providerKind ?? 'openai_compatible',
        baseUrl: form.baseUrl.trim(),
        apiKey: form.apiKey.trim() || undefined,
        credentialRefId: form.apiKey.trim() ? undefined : form.credentialRefId.trim() || undefined,
        defaultModel: form.defaultModel.trim(),
      });
      setNotice(`“${created.name}”已添加。请测试连接后设为默认模型。`);
      setForm(EMPTY_FORM);
      setCreating(false);
      await load();
      setSelectedId(created.id);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '创建失败');
    }
  };

  const test = async () => {
    if (!selected) return;
    setTesting(true);
    setTestResult(null);
    setError(null);
    setNotice(null);
    try {
      const result = await rpc<{ status: string; steps: TestStep[] }>('modelProfile.test', {
        profileId: selected.id,
      });
      setTestResult(result);
      setShowDiagnostics(result.status !== 'ready');
      setNotice(
        result.status === 'ready'
          ? '连接与文本生成均已通过，这个模型可以用于 Agent。'
          : '连接测试未通过。展开详情可查看失败步骤。',
      );
      await load();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '连接测试失败');
    } finally {
      setTesting(false);
    }
  };

  const syncModels = async () => {
    if (!selected) return;
    setSyncing(true);
    setError(null);
    try {
      const result = await rpc<{ models: string[] }>('modelProfile.syncModels', {
        profileId: selected.id,
      });
      setModelsByProfile((current) => ({ ...current, [selected.id]: result.models ?? [] }));
      setNotice(`已读取 ${result.models?.length ?? 0} 个可用模型。`);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '模型列表同步失败');
    } finally {
      setSyncing(false);
    }
  };

  const setDefault = async () => {
    if (!selected) return;
    setError(null);
    try {
      const result = await rpc<ModelRoute[]>('modelRoute.update', {
        route: { scope: 'global', taskKind: 'default', primaryProfileId: selected.id },
        expectedRevision: route?.revision ?? 0,
      });
      setRoutes(result);
      setNotice(`“${selected.name}”已设为默认模型。`);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '默认模型设置失败');
    }
  };

  const remove = async () => {
    if (!selected || !window.confirm(`删除“${selected.name}”？此操作不可撤销。`)) return;
    setError(null);
    try {
      await rpc('modelProfile.remove', {
        profileId: selected.id,
        expectedRevision: selected.revision,
      });
      setSelectedId('');
      await load();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '删除失败');
    }
  };

  return (
    <div className="sg-set-page sg-model-page">
      <SettingsPageHeader
        title="模型设置"
        scope="全局"
        description="管理 Agent 使用的模型供应商。API Key 只保存到 OS Keychain，界面不会回显。"
        actions={
          <button className="sg-btn sg-btn--quiet" onClick={() => void load()} disabled={loading}>
            <IconRefresh size={14} />刷新
          </button>
        }
      />
      {error ? <div className="sg-banner sg-banner--error" role="alert">{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

      <div className="sg-model-layout">
        <aside className="sg-model-providers" aria-label="模型供应商">
          <div className="sg-model-providers-head">供应商</div>
          <div className="sg-model-provider-list">
            {loading ? <div className="sg-model-provider-empty">正在加载…</div> : null}
            {!loading && items.length === 0 ? <div className="sg-model-provider-empty">还没有模型供应商</div> : null}
            {items.map((profile) => (
              <button
                key={profile.id}
                className={`sg-model-provider ${profile.id === selectedId ? 'sg-model-provider--active' : ''}`}
                onClick={() => {
                  setSelectedId(profile.id);
                  setCreating(false);
                  setTestResult(null);
                  setShowDiagnostics(false);
                }}
              >
                <span className="sg-model-provider-mark">{profile.name.slice(0, 1).toUpperCase()}</span>
                <span><strong>{profile.name}</strong><small>{KIND_LABEL[profile.provider_kind] ?? profile.provider_kind}</small></span>
                <span className={`sg-model-provider-dot ${profile.status === 'ready' ? 'is-ready' : ''}`} />
              </button>
            ))}
          </div>
          <button className="sg-model-add" onClick={() => { setCreating(true); setForm(EMPTY_FORM); }}>
            <IconPlus size={14} />添加供应商
          </button>
        </aside>

        <section className="sg-model-detail">
          {creating ? (
            <ProviderForm form={form} presets={presets} activePreset={activePreset} onForm={setForm} onPreset={pickPreset} onSubmit={create} onCancel={() => setCreating(false)} />
          ) : selected ? (
            <>
              <div className="sg-model-detail-head">
                <div>
                  <div className="sg-model-title-row">
                    <span className="sg-model-provider-mark sg-model-provider-mark--large">{selected.name.slice(0, 1).toUpperCase()}</span>
                    <h2>{selected.name}</h2>
                    <StatusPill kind={selected.status === 'ready' ? 'ready' : selected.status === 'error' ? 'error' : 'pending'} label={selected.status === 'ready' ? '可用' : selected.status === 'error' ? '需检查' : '待测试'} />
                    {route?.primaryProfileId === selected.id ? <span className="sg-chip">默认</span> : null}
                  </div>
                  <p>用于需求分析、PRD 起草和后续各关 Agent 任务。</p>
                </div>
                <div className="sg-row">
                  {route?.primaryProfileId !== selected.id ? <button className="sg-btn" onClick={() => void setDefault()} disabled={selected.status !== 'ready'} title={selected.status === 'ready' ? '设为默认模型' : '请先通过连接测试'}><IconCheck size={14} />设为默认</button> : null}
                  <button className="sg-btn sg-btn--primary" onClick={() => void test()} disabled={testing}><IconZap size={14} />{testing ? '测试中…' : '测试连接'}</button>
                </div>
              </div>

              <div className="sg-model-fields">
                <label><span>Base URL</span><input value={selected.base_url} readOnly /></label>
                <label>
                  <span>API Key</span>
                  <input value={selected.credential_ref_id ? '••••••••••••••••••••••••' : ''} placeholder="未绑定凭据" readOnly />
                  <small>{selected.credential_ref_id ? '已安全存储在 Keychain' : '需要添加凭据后才能生成内容'}</small>
                </label>
              </div>

              <div className="sg-model-list-head">
                <div><h3>模型列表</h3><p>当前 Agent 默认使用 {selected.default_model || '未指定模型'}。</p></div>
                <button className="sg-btn sg-btn--quiet" onClick={() => void syncModels()} disabled={syncing}><IconRefresh size={13} />{syncing ? '同步中…' : '同步模型'}</button>
              </div>
              <div className="sg-model-list">
                {selectedModels.length === 0 ? <div className="sg-empty">同步供应商后会在这里显示可用模型。</div> : selectedModels.map((model) => (
                  <div className="sg-model-row" key={model}><span>{model}</span>{model === selected.default_model ? <span className="sg-chip">默认</span> : null}</div>
                ))}
              </div>

              {testResult ? (
                <div className="sg-model-diagnostics">
                  <button onClick={() => setShowDiagnostics((value) => !value)}>
                    {showDiagnostics ? <IconChevronDown size={14} /> : <IconChevronRight size={14} />}
                    连接测试详情
                    <StatusPill kind={testResult.status === 'ready' ? 'ready' : 'error'} label={testResult.status === 'ready' ? '全部通过' : '需要处理'} />
                  </button>
                  {showDiagnostics ? <div className="sg-model-diagnostic-list">{testResult.steps.map((step) => (
                    <div key={step.name}>
                      <span>{STEP_LABEL[step.name] ?? step.name}</span>
                      <StatusPill kind={step.status === 'passed' ? 'ready' : step.status === 'skipped' ? 'readonly' : 'error'} label={step.status === 'passed' ? '通过' : step.status === 'skipped' ? '未检测' : '失败'} />
                      <small>{step.errorCode ?? ''}</small>
                    </div>
                  ))}</div> : null}
                </div>
              ) : null}

              {!selected.managed_source ? <div className="sg-model-danger-zone"><button className="sg-link-btn sg-link-btn--danger" onClick={() => void remove()}>删除此供应商</button></div> : null}
            </>
          ) : (
            <div className="sg-model-welcome">
              <h2>添加第一个模型</h2>
              <p>配置完成并通过文本生成测试后，需求提交才会自动起草 PRD。</p>
              <button className="sg-btn sg-btn--primary" onClick={() => setCreating(true)}><IconPlus size={14} />添加供应商</button>
            </div>
          )}
        </section>
      </div>
    </div>
  );
}

function ProviderForm({ form, presets, activePreset, onForm, onPreset, onSubmit, onCancel }: {
  form: typeof EMPTY_FORM;
  presets: ModelPreset[];
  activePreset?: ModelPreset;
  onForm: (next: typeof EMPTY_FORM) => void;
  onPreset: (id: string) => void;
  onSubmit: (event: FormEvent) => void;
  onCancel: () => void;
}) {
  return (
    <form className="sg-model-create" onSubmit={onSubmit}>
      <div><h2>添加模型供应商</h2><p>选择预设会自动填入推荐端点和模型。</p></div>
      <div className="sg-model-preset-row">
        {presets.map((preset) => <button key={preset.id} type="button" className={form.presetId === preset.id ? 'is-active' : ''} onClick={() => onPreset(preset.id)}>{preset.label}</button>)}
        <button type="button" className={form.presetId === 'custom' ? 'is-active' : ''} onClick={() => onPreset('custom')}>自定义</button>
      </div>
      <div className="sg-model-fields">
        <label><span>名称 *</span><input value={form.name} onChange={(event) => onForm({ ...form, name: event.target.value })} placeholder="主力模型" /></label>
        <label><span>Base URL *</span><input value={form.baseUrl} onChange={(event) => onForm({ ...form, baseUrl: event.target.value })} placeholder={activePreset?.baseUrl || 'https://api.example.com/v1'} /></label>
        <label><span>API Key</span><input type="password" autoComplete="off" value={form.apiKey} onChange={(event) => onForm({ ...form, apiKey: event.target.value })} placeholder="只写入 Keychain，不回显" /></label>
        <label><span>已有凭据引用</span><input value={form.credentialRefId} onChange={(event) => onForm({ ...form, credentialRefId: event.target.value })} placeholder="与 API Key 二选一" disabled={Boolean(form.apiKey.trim())} /></label>
        <label><span>默认模型 *</span><input value={form.defaultModel} onChange={(event) => onForm({ ...form, defaultModel: event.target.value })} list="model-options" />
          <datalist id="model-options">{(activePreset?.models ?? []).map((model) => <option key={model.id} value={model.id}>{model.note}</option>)}</datalist>
        </label>
      </div>
      <div className="sg-row">
        <button className="sg-btn sg-btn--primary" type="submit" disabled={!form.name.trim() || !form.baseUrl.trim() || !form.defaultModel.trim()}>添加并继续</button>
        <button className="sg-btn" type="button" onClick={onCancel}>取消</button>
      </div>
    </form>
  );
}
