import { useCallback, useEffect, useState } from 'react';
import { rpc, rpcErrorMessage } from '../../../rpc/client';

/* ---------------- Agent 中心（ADR-030 M4 / 蓝图 §10.3） ----------------
 * AgentProfile 版本列表、阶段 activity 绑定矩阵、resolvePreview 路由预览。
 * 数据全部来自 agentProfile.* / agentBinding.* RPC。
 */

interface ProfileVersion {
  id: string;
  version_no: number;
  capabilities: string[];
  content_digest: string;
  created_at: string;
}

interface AgentProfile {
  id: string;
  project_id?: string;
  name: string;
  adapter_kind: string;
  enabled: boolean;
  versions: ProfileVersion[];
}

interface Binding {
  id: string;
  project_id?: string;
  gate: string;
  activity_key: string;
  profile_version_id: string;
  fallback_mode: string;
  priority: number;
  enabled: boolean;
}

const GATES: { gate: string; label: string; activities: string[] }[] = [
  { gate: 'requirements', label: '需求关', activities: ['requirement_analysis'] },
  { gate: 'design', label: '方案关', activities: ['prototype_design', 'technical_design'] },
  {
    gate: 'development',
    label: '开发关',
    activities: ['frontend', 'backend', 'code_analysis'],
  },
  { gate: 'testing', label: '测试关', activities: ['e2e_testing'] },
  { gate: 'deployment', label: '部署关', activities: ['release_planning', 'deployment_verification'] },
  { gate: 'verification', label: '验证关', activities: ['acceptance_verification'] },
];

export function AgentCenterPage() {
  const [profiles, setProfiles] = useState<AgentProfile[]>([]);
  const [bindings, setBindings] = useState<Binding[]>([]);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  // 新建 profile 表单。
  const [name, setName] = useState('');
  const [adapterKind, setAdapterKind] = useState('local_harness');
  const [persona, setPersona] = useState('');
  const [capabilities, setCapabilities] = useState('');
  // 绑定表单。
  const [bindGate, setBindGate] = useState('development');
  const [bindActivity, setBindActivity] = useState('frontend');
  const [bindVersion, setBindVersion] = useState('');
  const [fallbackMode, setFallbackMode] = useState('generic');
  // 路由预览。
  const [preview, setPreview] = useState<string>('');
  // 项目级绑定作用域（P0 审计修复：绑定与预览不再隐式取第一个项目）。
  const [projects, setProjects] = useState<{ id: string; name: string }[]>([]);
  const [projectId, setProjectId] = useState('');

  const reload = useCallback(async () => {
    try {
      const [p, b, pr] = await Promise.all([
        rpc<{ items: AgentProfile[] }>('agentProfile.list', {}),
        rpc<{ items: Binding[] }>('agentBinding.list', {}),
        rpc<{ items: { id: string; name: string }[] }>('project.list', {}),
      ]);
      setProfiles(p.items);
      setBindings(b.items);
      setProjects(pr.items);
      setProjectId((prev) => prev || pr.items[0]?.id || '');
    } catch (e) {
      setError(rpcErrorMessage(e));
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  const versionLabel = (versionId: string): string => {
    for (const p of profiles) {
      const v = p.versions.find((x) => x.id === versionId);
      if (v) return `${p.name} v${v.version_no}`;
    }
    return versionId.slice(0, 12);
  };

  const createProfile = async () => {
    setError('');
    setNotice('');
    try {
      const profile = await rpc<{ id: string }>('agentProfile.create', { name, adapterKind });
      await rpc('agentProfile.createVersion', {
        profileId: profile.id,
        persona,
        capabilities: capabilities
          .split(/[,，]/)
          .map((s) => s.trim())
          .filter(Boolean),
      });
      setNotice(`已创建 AgentProfile「${name}」v1（不可变版本冻结）。`);
      setName('');
      setPersona('');
      setCapabilities('');
      await reload();
    } catch (e) {
      setError(rpcErrorMessage(e));
    }
  };

  const bind = async () => {
    setError('');
    setNotice('');
    try {
      await rpc('agentBinding.set', {
        projectId: projectId || undefined,
        gate: bindGate,
        activityKey: bindActivity,
        profileVersionId: bindVersion,
        fallbackMode,
      });
      setNotice('绑定已保存；未绑定活动继续回落通用 Agent。');
      await reload();
    } catch (e) {
      setError(rpcErrorMessage(e));
    }
  };

  const unbind = async (bindingId: string) => {
    try {
      await rpc('agentBinding.remove', { bindingId });
      await reload();
    } catch (e) {
      setError(rpcErrorMessage(e));
    }
  };

  const runPreview = async (gate: string, activityKey: string) => {
    setError('');
    setPreview('');
    try {
      if (!projectId) throw new Error('无可用项目，无法预览');
      const view = await rpc<Record<string, unknown>>('agentBinding.resolvePreview', {
        projectId,
        gate,
        activityKey,
      });
      const scope = String(view.source_scope ?? view['source_scope'] ?? '（fail_closed 拒绝）');
      const fallback = Boolean(view.fallback_used ?? view['fallback_used']);
      setPreview(`${gate}/${activityKey} → ${scope}${fallback ? '（回退）' : ''}`);
    } catch (e) {
      setPreview(`预览失败：${rpcErrorMessage(e)}`);
    }
  };

  const gateLabel = (gate: string): string => GATES.find((g) => g.gate === gate)?.label ?? gate;

  return (
    <>
      <header className="sg-page-head">
        <span className="sg-page-head-title">Agent 中心</span>
        <span className="sg-page-head-status">本地运行</span>
      </header>

      {error ? (
        <div className="sg-banner sg-banner--error" role="alert">
          {error}
        </div>
      ) : null}
      {notice ? (
        <div className="sg-banner sg-banner--info" role="status">
          {notice}
        </div>
      ) : null}
      {preview ? (
        <div className="sg-banner sg-banner--info" role="status">
          路由预览：{preview}
        </div>
      ) : null}

      <section className="sg-settings-section">
        <h2 className="sg-section-title">AgentProfile（版本冻结）</h2>
        <p className="sg-muted" style={{ fontSize: 12.5 }}>
          专属 Agent 按版本冻结角色说明/SOP/能力；未绑定或不可用时回落内置通用 Agent，
          或在绑定上配置 fail_closed 直接失败关闭。
        </p>
        <div style={{ display: 'grid', gap: 8, marginTop: 10 }}>
          {profiles.map((p) => (
            <div key={p.id} className="sg-card" style={{ padding: '10px 14px' }}>
              <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                <span style={{ fontWeight: 600, fontSize: 13 }}>{p.name}</span>
                <span className="sg-chip">{p.adapter_kind === 'external_agent' ? '外部 Agent' : '本地'}</span>
                {!p.enabled ? <span className="sg-chip sg-chip--danger">已禁用</span> : null}
                {p.project_id ? <span className="sg-muted">项目级</span> : <span className="sg-muted">全局</span>}
              </div>
              <div style={{ display: 'grid', gap: 2, marginTop: 6 }}>
                {p.versions.map((v) => (
                  <div key={v.id} style={{ display: 'flex', gap: 8, fontSize: 12 }}>
                    <span className="sg-chip">v{v.version_no}</span>
                    <span className="sg-code">{v.content_digest.slice(0, 12)}</span>
                    {v.capabilities.length > 0 ? (
                      <span className="sg-muted">能力：{v.capabilities.join('、')}</span>
                    ) : (
                      <span className="sg-muted">通用能力</span>
                    )}
                  </div>
                ))}
              </div>
            </div>
          ))}
        </div>

        <div className="sg-row" style={{ marginTop: 12, flexWrap: 'wrap' }}>
          <input
            className="sg-input"
            style={{ width: 160 }}
            placeholder="名称，如 前端开发分身"
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
          <select className="sg-input" style={{ width: 140 }} value={adapterKind} onChange={(e) => setAdapterKind(e.target.value)}>
            <option value="local_harness">本地 harness</option>
            <option value="external_agent">外部 Agent</option>
          </select>
          <input
            className="sg-input"
            style={{ width: 200 }}
            placeholder="能力（逗号分隔，可空）"
            value={capabilities}
            onChange={(e) => setCapabilities(e.target.value)}
          />
          <input
            className="sg-input"
            style={{ width: 260 }}
            placeholder="角色说明 persona（进入冻结版本）"
            value={persona}
            onChange={(e) => setPersona(e.target.value)}
          />
          <button className="sg-button sg-button--primary" disabled={!name.trim() || !persona.trim()} onClick={() => void createProfile()}>
            创建 Profile（冻结 v1）
          </button>
        </div>
      </section>

      <section className="sg-settings-section" style={{ marginTop: 20 }}>
        <h2 className="sg-section-title">阶段绑定矩阵（六关 × activity）</h2>
        <div style={{ display: 'grid', gap: 4, marginTop: 10 }}>
          {bindings.length === 0 ? (
            <div className="sg-empty" style={{ padding: '14px 16px' }}>
              暂无绑定：所有活动使用内置通用 Agent（默认）。
            </div>
          ) : (
            bindings.map((b) => (
              <div key={b.id} style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 12.5 }}>
                <span className="sg-chip">{gateLabel(b.gate)}</span>
                <span className="sg-code">{b.activity_key}</span>
                <span style={{ fontWeight: 500 }}>{versionLabel(b.profile_version_id)}</span>
                <span className={`sg-chip ${b.fallback_mode === 'fail_closed' ? 'sg-chip--danger' : ''}`}>
                  {b.fallback_mode === 'fail_closed' ? 'fail_closed' : '可回退'}
                </span>
                <button className="sg-button" style={{ marginLeft: 'auto' }} onClick={() => void unbind(b.id)}>
                  解绑
                </button>
              </div>
            ))
          )}
        </div>

        <div className="sg-row" style={{ marginTop: 12, flexWrap: 'wrap' }}>
          <select
            className="sg-input"
            style={{ width: 150 }}
            value={projectId}
            onChange={(e) => setProjectId(e.target.value)}
            title="绑定作用域项目"
          >
            {projects.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name || p.id}
              </option>
            ))}
          </select>
          <select
            className="sg-input"
            style={{ width: 110 }}
            value={bindGate}
            onChange={(e) => {
              setBindGate(e.target.value);
              const first = GATES.find((g) => g.gate === e.target.value);
              if (first) setBindActivity(first.activities[0]);
            }}
          >
            {GATES.map((g) => (
              <option key={g.gate} value={g.gate}>
                {g.label}
              </option>
            ))}
          </select>
          <select className="sg-input" style={{ width: 180 }} value={bindActivity} onChange={(e) => setBindActivity(e.target.value)}>
            {(GATES.find((g) => g.gate === bindGate)?.activities ?? []).map((a) => (
              <option key={a} value={a}>
                {a}
              </option>
            ))}
          </select>
          <select className="sg-input" style={{ width: 220 }} value={bindVersion} onChange={(e) => setBindVersion(e.target.value)}>
            <option value="">选择 Profile 版本…</option>
            {profiles.flatMap((p) =>
              p.versions.map((v) => (
                <option key={v.id} value={v.id}>
                  {p.name} v{v.version_no}
                </option>
              )),
            )}
          </select>
          <select className="sg-input" style={{ width: 130 }} value={fallbackMode} onChange={(e) => setFallbackMode(e.target.value)}>
            <option value="generic">可回退通用</option>
            <option value="fail_closed">fail_closed</option>
          </select>
          <button className="sg-button sg-button--primary" disabled={!bindVersion} onClick={() => void bind()}>
            保存绑定
          </button>
          <button className="sg-button" onClick={() => void runPreview(bindGate, bindActivity)}>
            路由预览
          </button>
        </div>
      </section>
    </>
  );
}
