import { useCallback, useEffect, useMemo, useState } from 'react';
import { rpc, rpcErrorMessage } from '../../../rpc/client';
import {
  IconCheck,
  IconCpu,
  IconPlus,
  IconTarget,
  IconZap,
} from '../../../components/Icons';
import { SettingsPageHeader } from '../components/SettingsPageHeader';

interface ProfileVersion {
  id: string;
  version_no: number;
  capabilities: string[];
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
  enabled: boolean;
}

const GATES = [
  { gate: 'requirements', label: '需求关', purpose: '澄清需求并起草 PRD', activity: 'requirement_analysis' },
  { gate: 'design', label: '方案关', purpose: '形成产品与技术方案', activity: 'technical_design' },
  { gate: 'development', label: '开发关', purpose: '实现代码并完成自测', activity: 'frontend' },
  { gate: 'testing', label: '测试关', purpose: '执行端到端与质量验证', activity: 'e2e_testing' },
  { gate: 'deployment', label: '部署关', purpose: '准备发布并验证环境', activity: 'release_planning' },
  { gate: 'verification', label: '验证关', purpose: '完成验收与交付确认', activity: 'acceptance_verification' },
] as const;

const ROLE_TEMPLATES = [
  { name: '产品需求 Agent', persona: '你是一名资深产品经理，负责澄清需求、定义范围并输出可验收的 PRD。', capabilities: '需求分析,PRD,验收标准' },
  { name: '技术方案 Agent', persona: '你是一名软件架构师，负责把已确认需求转化为可实施、可测试、可回滚的技术方案。', capabilities: '架构设计,API,数据模型,回滚' },
  { name: '开发 Agent', persona: '你是一名资深开发工程师，遵循项目规范实现代码，并提供测试与变更说明。', capabilities: '代码实现,单元测试,代码审查' },
];

export function AgentCenterPage() {
  const [profiles, setProfiles] = useState<AgentProfile[]>([]);
  const [bindings, setBindings] = useState<Binding[]>([]);
  const [projects, setProjects] = useState<{ id: string; name: string }[]>([]);
  const [projectId, setProjectId] = useState('');
  const [assignments, setAssignments] = useState<Record<string, string>>({});
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState('');
  const [adapterKind, setAdapterKind] = useState('local_harness');
  const [persona, setPersona] = useState('');
  const [capabilities, setCapabilities] = useState('');
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [preview, setPreview] = useState('');
  const [savingGate, setSavingGate] = useState('');

  const reload = useCallback(async () => {
    try {
      const [profilePage, bindingPage, projectPage] = await Promise.all([
        rpc<{ items: AgentProfile[] }>('agentProfile.list', {}),
        rpc<{ items: Binding[] }>('agentBinding.list', {}),
        rpc<{ items: { id: string; name: string }[] }>('project.list', {}),
      ]);
      setProfiles(profilePage.items ?? []);
      setBindings(bindingPage.items ?? []);
      setProjects(projectPage.items ?? []);
      setProjectId((current) => current || projectPage.items?.[0]?.id || '');
    } catch (reason) {
      setError(rpcErrorMessage(reason));
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  useEffect(() => {
    const next: Record<string, string> = {};
    for (const gate of GATES) {
      const binding = bindings.find(
        (item) => item.gate === gate.gate && (!item.project_id || item.project_id === projectId),
      );
      if (binding) next[gate.gate] = binding.profile_version_id;
    }
    setAssignments(next);
  }, [bindings, projectId]);

  const versions = useMemo(
    () => profiles.flatMap((profile) => (profile.versions ?? []).map((version) => ({ profile, version }))),
    [profiles],
  );

  const versionLabel = (versionId: string) => {
    const found = versions.find((item) => item.version.id === versionId);
    return found ? found.profile.name : '内置通用 Agent';
  };

  const createProfile = async () => {
    setError('');
    setNotice('');
    try {
      const profile = await rpc<{ id: string }>('agentProfile.create', {
        name,
        adapterKind,
      });
      await rpc('agentProfile.createVersion', {
        profileId: profile.id,
        persona,
        capabilities: capabilities.split(/[,，]/).map((item) => item.trim()).filter(Boolean),
      });
      setNotice(`“${name}”已创建，可以分配给下面任一关。`);
      setName('');
      setPersona('');
      setCapabilities('');
      setCreating(false);
      await reload();
    } catch (reason) {
      setError(rpcErrorMessage(reason));
    }
  };

  const saveAssignment = async (gate: (typeof GATES)[number]) => {
    const versionId = assignments[gate.gate];
    setSavingGate(gate.gate);
    setError('');
    setNotice('');
    try {
      const existing = bindings.find(
        (item) => item.gate === gate.gate && (!item.project_id || item.project_id === projectId),
      );
      if (!versionId) {
        if (existing) await rpc('agentBinding.remove', { bindingId: existing.id });
        setNotice(`${gate.label}将使用内置通用 Agent。`);
      } else {
        await rpc('agentBinding.set', {
          projectId: projectId || undefined,
          gate: gate.gate,
          activityKey: gate.activity,
          profileVersionId: versionId,
          fallbackMode: 'generic',
        });
        setNotice(`${gate.label}已交给“${versionLabel(versionId)}”。`);
      }
      await reload();
    } catch (reason) {
      setError(rpcErrorMessage(reason));
    } finally {
      setSavingGate('');
    }
  };

  const runPreview = async (gate: (typeof GATES)[number]) => {
    setPreview('');
    try {
      if (!projectId) throw new Error('请先选择一个项目');
      const result = await rpc<Record<string, unknown>>('agentBinding.resolvePreview', {
        projectId,
        gate: gate.gate,
        activityKey: gate.activity,
      });
      const fallback = Boolean(result.fallback_used);
      const selectedId = String(result.resolved_profile_version_id ?? '');
      setPreview(
        `${gate.label}实际将使用“${selectedId ? versionLabel(selectedId) : '内置通用 Agent'}”${fallback ? '（自定义 Agent 不可用时自动回退）' : ''}。`,
      );
    } catch (reason) {
      setPreview(`无法预览：${rpcErrorMessage(reason)}`);
    }
  };

  const applyTemplate = (index: number) => {
    const template = ROLE_TEMPLATES[index];
    setName(template.name);
    setPersona(template.persona);
    setCapabilities(template.capabilities);
    setCreating(true);
  };

  return (
    <div className="sg-set-page sg-agent-page">
      <SettingsPageHeader
        title="Agent 中心"
        scope="本地"
        description="决定六关中的每一步由谁完成；没有特殊要求时使用内置通用 Agent。"
      />

      <div className="sg-agent-scroll">
        {error ? <div className="sg-banner sg-banner--error" role="alert">{error}</div> : null}
        {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}
        {preview ? <div className="sg-banner sg-banner--info" role="status">{preview}</div> : null}

        <section className="sg-agent-intro">
          <span className="sg-agent-avatar"><IconCpu size={22} /></span>
          <div>
            <h2>默认情况下，你不需要配置任何东西</h2>
            <p>内置通用 Agent 会完成所有阶段。只有当某一关需要固定角色、专门能力或外部 Agent 时，再创建并分配。</p>
          </div>
          <button className="sg-btn sg-btn--primary" onClick={() => setCreating((value) => !value)}>
            <IconPlus size={14} />新建 Agent
          </button>
        </section>

        {creating ? (
          <section className="sg-agent-create">
            <div className="sg-agent-create-head">
              <div><h2>新建 Agent</h2><p>先选一个模板，再按你的团队习惯调整职责和能力。</p></div>
              <div className="sg-agent-template-row">
                {ROLE_TEMPLATES.map((template, index) => <button key={template.name} onClick={() => applyTemplate(index)}>{template.name}</button>)}
              </div>
            </div>
            <div className="sg-agent-form-grid">
              <label><span>名称</span><input value={name} onChange={(event) => setName(event.target.value)} placeholder="例如：前端开发 Agent" /></label>
              <label><span>运行方式</span><select value={adapterKind} onChange={(event) => setAdapterKind(event.target.value)}><option value="local_harness">内置运行</option><option value="external_agent">外部 Agent</option></select></label>
              <label className="is-wide"><span>职责说明</span><textarea value={persona} onChange={(event) => setPersona(event.target.value)} placeholder="说明它负责什么、输出什么、哪些事情不要做" /></label>
              <label className="is-wide"><span>能力标签</span><input value={capabilities} onChange={(event) => setCapabilities(event.target.value)} placeholder="例如：React、接口设计、自动化测试" /><small>用逗号分隔，帮助团队理解用途。</small></label>
            </div>
            <div className="sg-row">
              <button className="sg-btn sg-btn--primary" disabled={!name.trim() || !persona.trim()} onClick={() => void createProfile()}><IconCheck size={14} />创建 Agent</button>
              <button className="sg-btn" onClick={() => setCreating(false)}>取消</button>
            </div>
          </section>
        ) : null}

        <section className="sg-agent-library">
          <div className="sg-agent-section-head">
            <div><h2>可用 Agent</h2><p>版本和运行细节由系统保存，这里只展示你需要选择的信息。</p></div>
          </div>
          <div className="sg-agent-cards">
            <article className="sg-agent-card sg-agent-card--builtin">
              <span className="sg-agent-avatar"><IconCpu size={20} /></span>
              <div><h3>内置通用 Agent</h3><p>覆盖六关的默认角色，未单独分配时自动使用。</p><span className="sg-chip">推荐默认</span></div>
            </article>
            {profiles.filter((profile) => profile.id !== 'built-in-generic' && profile.name !== 'builtin-generic').map((profile) => (
              <article className="sg-agent-card" key={profile.id}>
                <span className="sg-agent-avatar"><IconTarget size={20} /></span>
                <div>
                  <h3>{profile.name}</h3>
                  <p>{profile.versions.at(-1)?.capabilities.join('、') || '通用能力'}</p>
                  <span className="sg-chip">{profile.adapter_kind === 'external_agent' ? '外部运行' : '本地运行'}</span>
                  {!profile.enabled ? <span className="sg-chip sg-chip--danger">已停用</span> : null}
                </div>
              </article>
            ))}
          </div>
        </section>

        <section className="sg-agent-assignments">
          <div className="sg-agent-section-head">
            <div><h2>六关分工</h2><p>为项目的每一关选择负责人；保持“内置通用 Agent”即可零配置运行。</p></div>
            <label className="sg-agent-project"><span>当前项目</span><select value={projectId} onChange={(event) => setProjectId(event.target.value)}>{projects.map((project) => <option key={project.id} value={project.id}>{project.name}</option>)}</select></label>
          </div>
          <div className="sg-agent-gate-list">
            {GATES.map((gate, index) => (
              <div className="sg-agent-gate-row" key={gate.gate}>
                <span className="sg-agent-gate-number">{index + 1}</span>
                <div><h3>{gate.label}</h3><p>{gate.purpose}</p></div>
                <select value={assignments[gate.gate] ?? ''} onChange={(event) => setAssignments((current) => ({ ...current, [gate.gate]: event.target.value }))}>
                  <option value="">内置通用 Agent</option>
                  {versions.map(({ profile, version }) => <option key={version.id} value={version.id}>{profile.name}</option>)}
                </select>
                <button className="sg-btn sg-btn--sm" onClick={() => void saveAssignment(gate)} disabled={savingGate === gate.gate}>{savingGate === gate.gate ? '保存中…' : '保存'}</button>
                <button className="sg-icon-btn" title="预览实际选择" aria-label={`预览${gate.label}实际 Agent`} onClick={() => void runPreview(gate)}><IconZap size={14} /></button>
              </div>
            ))}
          </div>
        </section>
      </div>
    </div>
  );
}
