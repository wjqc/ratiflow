import { useCallback, useEffect, useState } from 'react';
import { rpc, rpcErrorMessage } from '../rpc/client';
import { IconAlert, IconBook, IconCode, IconCpu, IconGear, IconInfo, IconLink, IconRefresh, IconServer, IconShield } from '../components/Icons';
import type { ReactNode } from 'react';

interface DiagnosticItem { id: string; label: string; status: string; detail: string; required?: boolean }
interface ProjectInfo { id: string; name: string; namespace: string; project: string; status: string }

interface NavGroup {
  label: string;
  items: Array<{ id: string; name: string; icon: ReactNode }>;
}

const NAV: NavGroup[] = [
  {
    label: '基础设置',
    items: [{ id: 'general', name: '常规', icon: <IconGear size={14} /> }],
  },
  {
    label: 'Agent 能力',
    items: [
      { id: 'model', name: '模型', icon: <IconCpu size={14} /> },
      { id: 'knowledge', name: '知识库', icon: <IconBook size={14} /> },
      { id: 'tools', name: '工具', icon: <IconCode size={14} /> },
    ],
  },
  {
    label: '集成',
    items: [
      { id: 'gitlab', name: 'GitLab', icon: <IconLink size={14} /> },
      { id: 'ssh', name: 'SSH 目标机', icon: <IconServer size={14} /> },
    ],
  },
  {
    label: '数据与安全',
    items: [
      { id: 'secrets', name: '凭据引用', icon: <IconShield size={14} /> },
      { id: 'backup', name: '备份与恢复', icon: <IconInfo size={14} /> },
      { id: 'audit', name: '审计日志', icon: <IconInfo size={14} /> },
    ],
  },
];

// 设置与诊断（规范 §4.6，原型 06-v2）：左导航 + 内容区。
// 集成诊断与项目登记有真实 RPC；其余导航项为规划中占位，不虚构配置表单。
export default function SettingsPage() {
  const [section, setSection] = useState('diagnostics');
  const [diagnostics, setDiagnostics] = useState<{ ready: boolean; integrations: DiagnosticItem[]; local: DiagnosticItem[] } | null>(null);
  const [projects, setProjects] = useState<ProjectInfo[]>([]);
  const [instance, setInstance] = useState('https://gitlab.example.com');
  const [namespace, setNamespace] = useState('');
  const [project, setProject] = useState('');
  const [name, setName] = useState('');
  const [localRoot, setLocalRoot] = useState('');
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [checking, setChecking] = useState(false);

  const reload = useCallback(async () => {
    try {
      const [diag, list] = await Promise.all([
        rpc<typeof diagnostics>('diagnostics.check', {}),
        rpc<{ items: ProjectInfo[] }>('project.list', { includeArchived: true }),
      ]);
      setDiagnostics(diag);
      setProjects(list.items);
    } catch (reason) {
      setError(rpcErrorMessage(reason));
    }
  }, []);

  useEffect(() => { void reload(); }, [reload]);

  const recheck = async () => {
    setChecking(true);
    await reload();
    setChecking(false);
  };

  const notReady = (diagnostics?.integrations ?? []).filter(
    (i) => i.required !== false && i.status !== 'ready' && i.status !== 'indexed',
  );

  return (
    <>
      <header className="sg-page-head">
        <span className="sg-page-head-title">设置与诊断</span>
        <span className="sg-page-head-status">本地运行</span>
      </header>

      <div className="sg-set-layout">
        <nav className="sg-set-nav" aria-label="设置导航">
          {NAV.map((group) => (
            <div key={group.label}>
              <div className="sg-set-nav-label">{group.label}</div>
              {group.items.map((item) => (
                <button
                  key={item.id}
                  className={`sg-set-nav-item ${section === item.id ? 'sg-set-nav-item--active' : ''}`}
                  onClick={() => setSection(item.id)}
                >
                  {item.icon}
                  {item.name}
                </button>
              ))}
            </div>
          ))}
          <div className="sg-set-nav-label">诊断</div>
          <button
            className={`sg-set-nav-item ${section === 'diagnostics' ? 'sg-set-nav-item--active' : ''}`}
            onClick={() => setSection('diagnostics')}
          >
            <IconRefresh size={14} />
            集成诊断
          </button>
        </nav>

        <div className="sg-set-main">
          {error ? <div className="sg-banner sg-banner--error" role="alert" style={{ marginBottom: 12 }}>{error}</div> : null}
          {notice ? <div className="sg-banner sg-banner--info" style={{ marginBottom: 12 }}>{notice}</div> : null}

          {section === 'diagnostics' && (
            <>
              <h2 className="sg-set-title">运行与集成诊断</h2>
              <p className="sg-set-sub">
                完整六关闭环需要 GitLab、模型与 SSH Linux 目标机；缺失的集成会在对应关卡拦截。
              </p>

              <DiagTable
                title="外部集成"
                items={diagnostics?.integrations ?? []}
                onRecheck={recheck}
                checking={checking}
              />
              <DiagTable
                title="本地运行"
                items={diagnostics?.local ?? []}
                onRecheck={recheck}
                checking={checking}
              />

              {notReady.length > 0 && (
                <div className="sg-banner sg-banner--warning" style={{ marginTop: 16 }}>
                  <IconAlert size={14} style={{ flexShrink: 0, marginTop: 2 }} />
                  <span>
                    {notReady.map((i) => i.label).join('、')} 未就绪：相关关卡会被拦截，其余能力不受影响。
                  </span>
                </div>
              )}
            </>
          )}

          {section === 'general' && (
            <>
              <h2 className="sg-set-title">常规</h2>
              <p className="sg-set-sub">登记 GitLab 项目为受管项目；归档后不再出现在侧栏。</p>

              <div className="sg-row" style={{ marginTop: 14, flexWrap: 'wrap' }}>
                <label className="sg-field" style={{ flex: 2, minWidth: 180 }}>
                  <span>GitLab 实例</span>
                  <input className="sg-input" value={instance} onChange={(e) => setInstance(e.target.value)} />
                </label>
                <label className="sg-field" style={{ flex: 1, minWidth: 120 }}>
                  <span>namespace *</span>
                  <input className="sg-input" value={namespace} onChange={(e) => setNamespace(e.target.value)} placeholder="team" />
                </label>
                <label className="sg-field" style={{ flex: 1, minWidth: 120 }}>
                  <span>project *</span>
                  <input className="sg-input" value={project} onChange={(e) => setProject(e.target.value)} placeholder="demo" />
                </label>
                <label className="sg-field" style={{ flex: 1, minWidth: 120 }}>
                  <span>显示名</span>
                  <input className="sg-input" value={name} onChange={(e) => setName(e.target.value)} placeholder="演示项目" />
                </label>
                <label className="sg-field" style={{ flex: 2, minWidth: 180 }}>
                  <span>本地仓库目录（可选）</span>
                  <input className="sg-input" value={localRoot} onChange={(e) => setLocalRoot(e.target.value)} placeholder="/Users/you/project" />
                </label>
                <button
                  className="sg-btn sg-btn--primary"
                  disabled={!namespace.trim() || !project.trim()}
                  onClick={() => {
                    setNotice('');
                    void rpc('project.create', {
                      gitlabInstance: instance, namespace: namespace.trim(), project: project.trim(),
                      name: name.trim(), localRoot: localRoot.trim(),
                    })
                      .then(async () => { setNotice('项目已登记'); await reload(); })
                      .catch((reason) => setError(rpcErrorMessage(reason)));
                  }}
                >
                  登记项目
                </button>
              </div>

              <table className="sg-table" style={{ marginTop: 14 }}>
                <thead><tr><th>名称</th><th>GitLab</th><th>状态</th><th>操作</th></tr></thead>
                <tbody>
                  {projects.length === 0 ? (
                    <tr><td colSpan={4} className="sg-muted" style={{ textAlign: 'center', padding: 18 }}>尚未登记项目</td></tr>
                  ) : projects.map((p) => (
                    <tr key={p.id}>
                      <td>{p.name || p.project}</td>
                      <td className="sg-muted">{p.namespace}/{p.project}</td>
                      <td><StatusMark status={p.status === 'ready' ? 'ready' : 'needs_configuration'} /></td>
                      <td>
                        <button className="sg-btn" onClick={() => {
                          void rpc('project.archive', { projectId: p.id, archived: true }).then(reload);
                        }}>归档</button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </>
          )}

          {section !== 'diagnostics' && section !== 'general' && (
            <div className="sg-empty" style={{ padding: '60px 24px' }}>
              <IconInfo size={28} style={{ color: 'var(--sg-border-strong)' }} />
              <span>该配置项规划中，暂未开放</span>
            </div>
          )}
        </div>
      </div>
    </>
  );
}

function DiagTable({
  title, items, onRecheck, checking,
}: {
  title: string;
  items: DiagnosticItem[];
  onRecheck: () => void;
  checking: boolean;
}) {
  return (
    <section style={{ marginTop: 18 }}>
      <div style={{ display: 'flex', alignItems: 'center', marginBottom: 8 }}>
        <h3 style={{ margin: 0, fontSize: 14, fontWeight: 600 }}>{title}</h3>
        <button
          className="sg-btn sg-btn--sm"
          style={{ marginLeft: 'auto' }}
          disabled={checking}
          onClick={onRecheck}
        >
          <IconRefresh size={12} />
          {checking ? '检查中…' : '重新检查'}
        </button>
      </div>
      <table className="sg-table">
        <thead><tr><th style={{ width: 180 }}>集成</th><th style={{ width: 110 }}>状态</th><th>详情</th></tr></thead>
        <tbody>
          {items.length === 0 ? (
            <tr><td colSpan={3} className="sg-muted" style={{ textAlign: 'center', padding: 16 }}>检查中…</td></tr>
          ) : items.map((item) => (
            <tr key={item.id}>
              <td>{item.label}{item.required ? ' *' : ''}</td>
              <td><StatusMark status={item.status} /></td>
              <td className="sg-muted">{item.detail}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}

function StatusMark({ status }: { status: string }) {
  if (status === 'ready' || status === 'indexed') {
    return <span className="sg-status sg-status--passed">✓ 就绪</span>;
  }
  if (status === 'error' || status === 'failed') {
    return <span className="sg-status sg-status--error">✕ 异常</span>;
  }
  return <span className="sg-status sg-status--running">◐ 待配置</span>;
}
