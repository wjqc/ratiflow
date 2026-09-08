// S13 工作流与关卡模板：模板版本化治理（draft → active → deprecated）。
// active 不可原地编辑——正确形态是「复制为新草稿 → 编辑 → 激活为新版本」；
// 已创建任务继续冻结使用创建时的版本，不受激活/弃用影响。
// 模板域受 RATIFLOW_WORKFLOW_TEMPLATE_V2 门控（显式 =0 关闭），关闭时整页降级提示。
import { useCallback, useEffect, useState } from 'react';
import { rpc, rpcErrorMessage } from '../../../rpc/client';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { StatusPill } from '../components/StatusPill';
import { useTwoStepConfirm } from '../components/useTwoStepConfirm';
import { IconCheck, IconPlus, IconRefresh } from '../../../components/Icons';

interface VersionRecord {
  id: string;
  version_no: number;
  status: 'draft' | 'active' | 'deprecated';
  content_digest: string;
  updated_at: string;
}

interface TemplateRecord {
  id: string;
  key: string;
  name: string;
  versions: VersionRecord[];
}

/** workflowTemplate.get 返回的关卡定义（存储形态，snake_case）。 */
interface GateDefinition {
  gate_id: string;
  title: string;
  purpose: string;
  deliverables: string[];
  acceptance: unknown[];
  context_policy_ref?: string | null;
  team_policy_ref?: string | null;
  workspace_policy_ref?: string | null;
  skip_policy?: unknown;
  fast_track_policy?: unknown;
}

/** 提交形态（workflowTemplate.create/updateDraft 的 gates 入参，camelCase）。策略引用透传不暴露编辑面。 */
interface GateDraft {
  gateId: string;
  title: string;
  purpose: string;
  /** 逗号分隔编辑态；提交时拆分。 */
  deliverables: string;
  /** 每行一条编辑态；提交时逐行还原（对象契约按原文保真，见 acceptanceOut）。 */
  acceptanceLines: string[];
  acceptanceRaw: unknown[];
  contextPolicyRef?: string;
  teamPolicyRef?: string;
  workspacePolicyRef?: string;
  skipPolicy?: unknown;
  fastTrackPolicy?: unknown;
}

const GATE_KEY_RE = /^[a-z][a-z0-9_-]{0,63}$/;

const STATUS_LABEL: Record<VersionRecord['status'], string> = {
  draft: '草稿',
  active: '使用中',
  deprecated: '已弃用',
};

function gatesToDraft(gates: GateDefinition[]): GateDraft[] {
  return gates.map((g) => ({
    gateId: g.gate_id,
    title: g.title,
    purpose: g.purpose ?? '',
    deliverables: (g.deliverables ?? []).join(', '),
    acceptanceLines: (g.acceptance ?? []).map((a) =>
      typeof a === 'string' ? a : JSON.stringify(a),
    ),
    acceptanceRaw: g.acceptance ?? [],
    contextPolicyRef: g.context_policy_ref ?? undefined,
    teamPolicyRef: g.team_policy_ref ?? undefined,
    workspacePolicyRef: g.workspace_policy_ref ?? undefined,
    skipPolicy: g.skip_policy ?? undefined,
    fastTrackPolicy: g.fast_track_policy ?? undefined,
  }));
}

/** 编辑态 → RPC gates 入参：行未改动的对象元素原样保真；改过的行尝试 JSON 还原，失败按纯文本。 */
function draftToGates(draft: GateDraft[]): Array<Record<string, unknown>> {
  return draft.map((d) => {
    const acceptance = d.acceptanceLines.map((line, i) => {
      const raw = d.acceptanceRaw[i];
      if (raw != null && typeof raw === 'object' && JSON.stringify(raw) === line) return raw;
      const trimmed = line.trim();
      if (trimmed.startsWith('{') || trimmed.startsWith('[')) {
        try {
          return JSON.parse(trimmed);
        } catch {
          return trimmed;
        }
      }
      return trimmed;
    });
    const out: Record<string, unknown> = {
      gateId: d.gateId.trim(),
      title: d.title.trim(),
      purpose: d.purpose.trim(),
      deliverables: d.deliverables.split(/[,，]/).map((s) => s.trim()).filter(Boolean),
      acceptance,
    };
    if (d.contextPolicyRef) out.contextPolicyRef = d.contextPolicyRef;
    if (d.teamPolicyRef) out.teamPolicyRef = d.teamPolicyRef;
    if (d.workspacePolicyRef) out.workspacePolicyRef = d.workspacePolicyRef;
    if (d.skipPolicy != null) out.skipPolicy = d.skipPolicy;
    if (d.fastTrackPolicy != null) out.fastTrackPolicy = d.fastTrackPolicy;
    return out;
  });
}

/** 前端预校验（后端 create/updateDraft/activate 三口仍会统一校验）。 */
function validateDraft(draft: GateDraft[]): string | null {
  if (draft.length === 0) return '至少需要一个关卡';
  const seen = new Set<string>();
  for (const d of draft) {
    if (!GATE_KEY_RE.test(d.gateId.trim())) {
      return `关卡标识非法（小写字母开头，仅限 a-z0-9_-，≤64 字符）：${d.gateId || '(空)'}`;
    }
    if (seen.has(d.gateId.trim())) return `关卡标识重复：${d.gateId.trim()}`;
    seen.add(d.gateId.trim());
    if (!d.title.trim()) return `${d.gateId.trim()} 缺少标题`;
    if (d.deliverables.split(/[,，]/).map((s) => s.trim()).filter(Boolean).length === 0) {
      return `${d.gateId.trim()} 至少声明一个交付物 kind（逗号分隔）`;
    }
  }
  return null;
}

const emptyGate = (): GateDraft => ({
  gateId: '',
  title: '',
  purpose: '',
  deliverables: '',
  acceptanceLines: [],
  acceptanceRaw: [],
});

export function WorkflowTemplatesPage() {
  const [templates, setTemplates] = useState<TemplateRecord[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [featureDisabled, setFeatureDisabled] = useState(false);
  const [busy, setBusy] = useState('');
  const [creating, setCreating] = useState(false);
  const [form, setForm] = useState({ key: '', name: '' });
  const [formError, setFormError] = useState<string | null>(null);
  // 展开草稿编辑器的版本（templateKey → versionId）。
  const [editingVersion, setEditingVersion] = useState<Record<string, string>>({});
  const [confirmId, requestConfirm] = useTwoStepConfirm();

  const load = useCallback(async (opts?: { keepNotice?: boolean }) => {
    if (!opts?.keepNotice) setNotice(null);
    setError(null);
    try {
      const r = await rpc<{ items: TemplateRecord[] }>('workflowTemplate.list', {});
      setTemplates(r.items ?? []);
      setFeatureDisabled(false);
    } catch (reason) {
      const msg = rpcErrorMessage(reason);
      if (msg.includes('feature_disabled')) {
        setFeatureDisabled(true);
        setTemplates([]);
      } else {
        setError(msg);
      }
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  /** 复制指定版本（默认激活版本）的关卡为新草稿。 */
  const forkDraft = async (template: TemplateRecord, fromVersionId?: string) => {
    setBusy(`fork:${template.key}`);
    setError(null);
    setNotice(null);
    try {
      const detail = await rpc<{
        activeVersion?: { version: VersionRecord; gates: GateDefinition[] };
        version?: { version: VersionRecord; gates: GateDefinition[] };
      }>('workflowTemplate.get', fromVersionId
        ? { templateId: template.id, versionId: fromVersionId }
        : { templateId: template.id });
      const source = detail.version ?? detail.activeVersion;
      if (!source) throw new Error('该模板没有可复制的版本');
      await rpc('workflowTemplate.create', {
        key: template.key,
        name: template.name,
        gates: draftToGates(gatesToDraft(source.gates)),
      });
      await load();
      // 展开最新草稿进入编辑。
      const r = await rpc<{ items: TemplateRecord[] }>('workflowTemplate.list', {});
      const latest = (r.items ?? []).find((t) => t.key === template.key);
      const draft = latest?.versions.find((v) => v.status === 'draft');
      if (draft) setEditingVersion((prev) => ({ ...prev, [template.key]: draft.id }));
      setNotice(`已从 v${source.version.version_no} 复制出新草稿 v${draft?.version_no ?? ''}，编辑保存后激活`);
    } catch (reason) {
      setError(rpcErrorMessage(reason));
    } finally {
      setBusy('');
    }
  };

  const createTemplate = async () => {
    setFormError(null);
    const key = form.key.trim();
    const name = form.name.trim();
    if (!GATE_KEY_RE.test(key)) {
      setFormError('模板标识非法：小写字母开头，仅限 a-z0-9_-，≤64 字符');
      return;
    }
    if (!name) {
      setFormError('模板名称必填');
      return;
    }
    setBusy('create');
    try {
      // 初始关卡复制默认六关（six-gate-default 的激活版本），用户随后在草稿里调整。
      const seed = await rpc<{ activeVersion?: { gates: GateDefinition[] } }>(
        'workflowTemplate.get',
        { templateId: 'six-gate-default' },
      );
      const gates = seed.activeVersion?.gates ?? [];
      if (gates.length === 0) throw new Error('默认模板读取失败，无法复制初始关卡');
      const r = await rpc<{ version: VersionRecord }>('workflowTemplate.create', {
        key, name, gates: draftToGates(gatesToDraft(gates)),
      });
      setForm({ key: '', name: '' });
      setCreating(false);
      await load({ keepNotice: true });
      setEditingVersion((prev) => ({ ...prev, [key]: r.version.id }));
      setNotice(`模板 ${name} 已创建（草稿 v${r.version.version_no}，初始关卡复制自默认六关）`);
    } catch (reason) {
      setFormError(rpcErrorMessage(reason));
    } finally {
      setBusy('');
    }
  };

  const deprecateActive = (template: TemplateRecord, version: VersionRecord) => {
    requestConfirm(`deprecate:${version.id}`, () => {
      void (async () => {
        setBusy(`deprecate:${version.id}`);
        setError(null);
        try {
          await rpc('workflowTemplate.deprecate', { versionId: version.id });
          await load({ keepNotice: true });
          setNotice('已弃用：新任务不再使用该模板（已创建任务不受影响）');
        } catch (reason) {
          setError(rpcErrorMessage(reason));
        } finally {
          setBusy('');
        }
      })();
    });
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="工作流与关卡模板"
        description="管理任务的关卡流程。激活版本不可直接修改：复制为新草稿、编辑后激活为新版本；已创建任务继续使用创建时冻结的版本。"
        actions={
          <>
            <button className="sg-btn" onClick={() => void load()} disabled={templates == null}>
              <IconRefresh size={14} />
              刷新
            </button>
            <button
              className="sg-btn sg-btn--primary"
              onClick={() => setCreating((v) => !v)}
              disabled={featureDisabled}
            >
              <IconPlus size={14} />
              新建模板
            </button>
          </>
        }
      />

      {featureDisabled ? (
        <div className="sg-banner sg-banner--info" role="status">
          关卡模板功能已关闭（RATIFLOW_WORKFLOW_TEMPLATE_V2=0）。开启后重启应用即可管理模板；默认六关流程不受影响。
        </div>
      ) : null}
      {error ? <div className="sg-banner sg-banner--error" role="alert">操作失败：{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

      {creating && !featureDisabled ? (
        <div className="sg-card" style={{ marginBottom: 16 }}>
          <div className="sg-card-head">新建关卡模板</div>
          <div className="sg-form-grid sg-form-grid--2" style={{ padding: '0 16px 16px' }}>
            <label className="sg-field">
              <span>模板标识 *</span>
              <input
                value={form.key}
                onChange={(e) => setForm((f) => ({ ...f, key: e.target.value }))}
                placeholder="hotfix-3"
              />
            </label>
            <label className="sg-field">
              <span>模板名称 *</span>
              <input
                value={form.name}
                onChange={(e) => setForm((f) => ({ ...f, name: e.target.value }))}
                placeholder="三关热修复"
              />
            </label>
          </div>
          <p className="sg-muted" style={{ margin: '0 16px 12px' }}>
            初始关卡会复制默认六关（six-gate-default），创建后可在草稿里增删关卡、改顺序与验收条件。
          </p>
          {formError ? (
            <div className="sg-banner sg-banner--error" role="alert" style={{ margin: '0 16px 12px' }}>
              {formError}
            </div>
          ) : null}
          <div style={{ padding: '0 16px 16px' }}>
            <button
              className="sg-btn sg-btn--primary"
              onClick={() => void createTemplate()}
              disabled={busy === 'create'}
            >
              <IconCheck size={14} />
              {busy === 'create' ? '创建中…' : '创建草稿'}
            </button>
          </div>
        </div>
      ) : null}

      {templates?.map((template) => {
        const active = template.versions.find((v) => v.status === 'active');
        const draft = template.versions.find((v) => v.status === 'draft');
        const editing = editingVersion[template.key];
        const isDefault = template.key === 'six-gate-default';
        return (
          <div className="sg-card" key={template.id} style={{ marginBottom: 16 }}>
            <div className="sg-card-head">
              {template.name}
              <span className="sg-muted" style={{ fontWeight: 400 }}>
                {template.key}
                {isDefault ? '（默认）' : ''}
              </span>
              <span className="sg-card-extra">
                <button
                  className="sg-btn sg-btn--sm"
                  onClick={() => void forkDraft(template)}
                  disabled={busy === `fork:${template.key}`}
                  title="以当前激活版本为底稿追加草稿版本"
                >
                  复制为新草稿
                </button>
                {active && !isDefault ? (
                  <button
                    className="sg-btn sg-btn--sm"
                    onClick={() => deprecateActive(template, active)}
                    disabled={busy === `deprecate:${active.id}`}
                  >
                    {confirmId === `deprecate:${active.id}` ? '再次点击确认弃用' : '弃用'}
                  </button>
                ) : null}
              </span>
            </div>

            <table className="sg-table" style={{ tableLayout: 'fixed' }}>
              <thead>
                <tr>
                  <th style={{ width: 64 }}>版本</th>
                  <th style={{ width: 90 }}>状态</th>
                  <th>内容摘要</th>
                  <th style={{ width: 130 }}>更新时间</th>
                  <th style={{ width: 90 }}></th>
                </tr>
              </thead>
              <tbody>
                {[...template.versions].reverse().map((v) => (
                  <tr key={v.id}>
                    <td>v{v.version_no}</td>
                    <td>
                      <StatusPill
                        kind={v.status === 'active' ? 'ready' : v.status === 'draft' ? 'pending' : 'readonly'}
                        label={STATUS_LABEL[v.status]}
                      />
                    </td>
                    <td className="sg-muted" style={{ overflow: 'hidden', textOverflow: 'ellipsis' }}>
                      {v.content_digest.slice(0, 12)}
                    </td>
                    <td className="sg-muted">{v.updated_at?.slice(0, 16).replace('T', ' ')}</td>
                    <td>
                      {v.status === 'draft' ? (
                        <button
                          className="sg-btn sg-btn--sm"
                          onClick={() =>
                            setEditingVersion((prev) => ({
                              ...prev,
                              [template.key]: prev[template.key] === v.id ? '' : v.id,
                            }))
                          }
                        >
                          {editing === v.id ? '收起' : '编辑'}
                        </button>
                      ) : null}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>

            {editing && template.versions.some((v) => v.id === editing) ? (
              <DraftEditor
                key={editing}
                templateId={template.id}
                versionId={editing}
                onSaved={async (msg) => {
                  await load({ keepNotice: true });
                  setNotice(msg);
                }}
              />
            ) : draft && !editing ? (
              <p className="sg-muted" style={{ margin: '8px 16px 12px' }}>
                有未激活的草稿 v{draft.version_no}，点击上方「编辑」继续调整。
              </p>
            ) : null}
          </div>
        );
      })}

      {templates != null && templates.length === 0 && !featureDisabled ? (
        <div className="sg-card">
          <div className="sg-empty" style={{ padding: '32px 24px' }}>
            暂无模板。默认六关由系统内置提供，可点「新建模板」基于它创建自定义流程。
          </div>
        </div>
      ) : null}
    </div>
  );
}

/** 草稿编辑器：加载指定版本定义 → 编辑关卡（标识/标题/目的/交付物/验收）→ 保存草稿 / 激活。 */
function DraftEditor({
  templateId,
  versionId,
  onSaved,
}: {
  templateId: string;
  versionId: string;
  onSaved: (message: string) => Promise<void>;
}) {
  const [gates, setGates] = useState<GateDraft[] | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [confirmId, requestConfirm] = useTwoStepConfirm();

  useEffect(() => {
    let cancelled = false;
    rpc<{ version?: { gates: GateDefinition[] }; activeVersion?: { gates: GateDefinition[] } }>(
      'workflowTemplate.get',
      { templateId, versionId },
    )
      .then((r) => {
        if (cancelled) return;
        const source = r.version ?? r.activeVersion;
        setGates(gatesToDraft(source?.gates ?? []));
      })
      .catch((reason) => {
        if (!cancelled) setError(rpcErrorMessage(reason));
      });
    return () => {
      cancelled = true;
    };
  }, [templateId, versionId]);

  const patch = (index: number, next: Partial<GateDraft>) => {
    setGates((current) =>
      current ? current.map((g, i) => (i === index ? { ...g, ...next } : g)) : current,
    );
  };
  const move = (index: number, delta: -1 | 1) => {
    setGates((current) => {
      if (!current) return current;
      const target = index + delta;
      if (target < 0 || target >= current.length) return current;
      const next = [...current];
      [next[index], next[target]] = [next[target], next[index]];
      return next;
    });
  };
  const remove = (index: number) => {
    setGates((current) => (current ? current.filter((_, i) => i !== index) : current));
  };

  const save = async (thenActivate: boolean) => {
    if (!gates) return;
    const invalid = validateDraft(gates);
    if (invalid) {
      setError(`无法保存：${invalid}`);
      return;
    }
    setBusy(true);
    setError('');
    try {
      await rpc('workflowTemplate.updateDraft', { versionId, gates: draftToGates(gates) });
      if (thenActivate) {
        await rpc('workflowTemplate.activate', { versionId });
        await onSaved('草稿已保存并激活：新任务将使用此版本，已创建任务继续使用原版本');
      } else {
        await onSaved('草稿已保存（尚未激活，新建任务仍使用当前激活版本）');
      }
    } catch (reason) {
      setError(rpcErrorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  if (gates == null) {
    return (
      <p className="sg-muted" style={{ margin: '8px 16px 12px' }}>
        {error ? `草稿加载失败：${error}` : '草稿加载中…'}
      </p>
    );
  }

  return (
    <div style={{ borderTop: '1px solid var(--sg-border)', padding: '12px 16px 16px' }}>
      {error ? (
        <div className="sg-banner sg-banner--error" role="alert" style={{ marginBottom: 12 }}>
          {error}
        </div>
      ) : null}
      {gates.map((gate, i) => (
        <div
          key={`${gate.gateId}-${i}`}
          className="sg-form-grid sg-form-grid--2"
          style={{
            padding: '12px 0',
            borderBottom: '1px solid var(--sg-border)',
            rowGap: 8,
          }}
        >
          <div className="sg-card-head" style={{ gridColumn: '1 / -1', padding: 0 }}>
            第 {i + 1} 关
            <span className="sg-card-extra">
              <button className="sg-btn sg-btn--sm" onClick={() => move(i, -1)} disabled={i === 0}>
                上移
              </button>
              <button
                className="sg-btn sg-btn--sm"
                onClick={() => move(i, 1)}
                disabled={i === gates.length - 1}
              >
                下移
              </button>
              <button
                className="sg-btn sg-btn--sm"
                onClick={() =>
                  requestConfirm(`remove:${i}`, () => {
                    remove(i);
                  })
                }
              >
                {confirmId === `remove:${i}` ? '确认删除' : '删除'}
              </button>
            </span>
          </div>
          <label className="sg-field">
            <span>关卡标识 *</span>
            <input
              value={gate.gateId}
              onChange={(e) => patch(i, { gateId: e.target.value })}
              placeholder="requirements"
            />
          </label>
          <label className="sg-field">
            <span>关卡标题 *</span>
            <input value={gate.title} onChange={(e) => patch(i, { title: e.target.value })} placeholder="需求关" />
          </label>
          <label className="sg-field" style={{ gridColumn: '1 / -1' }}>
            <span>关卡目的</span>
            <input value={gate.purpose} onChange={(e) => patch(i, { purpose: e.target.value })} placeholder="这一关要达成什么" />
          </label>
          <label className="sg-field">
            <span>交付物 kind *（逗号分隔）</span>
            <input value={gate.deliverables} onChange={(e) => patch(i, { deliverables: e.target.value })} placeholder="doc, prd" />
          </label>
          <label className="sg-field">
            <span>验收条件（每行一条）</span>
            <textarea
              rows={Math.max(2, gate.acceptanceLines.length)}
              value={gate.acceptanceLines.join('\n')}
              onChange={(e) => patch(i, { acceptanceLines: e.target.value.split('\n') })}
              placeholder={'PRD 已确认\n范围边界已记录'}
            />
          </label>
        </div>
      ))}

      <button className="sg-btn sg-btn--sm" style={{ marginTop: 12 }} onClick={() => setGates([...gates, emptyGate()])}>
        <IconPlus size={13} />
        添加关卡
      </button>

      <div style={{ marginTop: 16, display: 'flex', gap: 8, alignItems: 'center' }}>
        <button className="sg-btn" onClick={() => void save(false)} disabled={busy}>
          保存草稿
        </button>
        <button className="sg-btn sg-btn--primary" onClick={() => void save(true)} disabled={busy}>
          <IconCheck size={14} />
          {busy ? '处理中…' : '保存并激活'}
        </button>
        <span className="sg-muted" style={{ fontSize: 12 }}>
          激活后新任务冻结使用此版本；已创建任务不受影响。
        </span>
      </div>
    </div>
  );
}
