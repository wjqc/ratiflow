import { useCallback, useEffect, useState } from 'react';
import { rpc, rpcErrorMessage } from '../rpc/client';

interface DiagnosticItem { id: string; label: string; status: string; detail: string; required?: boolean }
interface ProjectInfo { id: string; name: string; namespace: string; project: string; status: string }

// 设置与诊断（规范 §4.6）：分组展示而非统计首页；秘密只显示 configured + 引用名。
export default function SettingsPage() {
  const [diagnostics, setDiagnostics] = useState<{ ready: boolean; integrations: DiagnosticItem[]; local: DiagnosticItem[] } | null>(null);
  const [projects, setProjects] = useState<ProjectInfo[]>([]);
  const [instance, setInstance] = useState('https://gitlab.example.com');
  const [namespace, setNamespace] = useState('');
  const [project, setProject] = useState('');
  const [name, setName] = useState('');
  const [localRoot, setLocalRoot] = useState('');
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');

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

  return (
    <div className="sg-section" style={{ maxWidth: 860 }}>
      <h1 style={{ fontSize: 18, margin: 0 }}>设置与诊断</h1>
      {error ? <div className="sg-banner sg-banner--error" style={{ marginTop: 8 }}>{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" style={{ marginTop: 8 }}>{notice}</div> : null}

      <section style={{ marginTop: 16 }}>
        <h2 className="sg-section-title">桌面运行</h2>
        <table className="sg-table">
          <tbody>
            {(diagnostics?.local ?? []).map((item) => (
              <tr key={item.id}>
                <td style={{ width: 160 }}>{item.label}</td>
                <td><StatusMark status={item.status} /></td>
                <td className="sg-muted sg-code">{item.detail}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>

      <section style={{ marginTop: 20 }}>
        <h2 className="sg-section-title">必需集成（环境变量配置后重启生效）</h2>
        <table className="sg-table">
          <tbody>
            {(diagnostics?.integrations ?? []).map((item) => (
              <tr key={item.id}>
                <td style={{ width: 160 }}>{item.label}{item.required ? ' *' : ''}</td>
                <td><StatusMark status={item.status} /></td>
                <td className="sg-muted">{item.detail}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>

      <section style={{ marginTop: 20 }}>
        <h2 className="sg-section-title">项目</h2>
        <div className="sg-row">
          <label className="sg-field" style={{ flex: 2 }}>
            <span>GitLab 实例</span>
            <input className="sg-input" value={instance} onChange={(e) => setInstance(e.target.value)} />
          </label>
          <label className="sg-field" style={{ flex: 1 }}>
            <span>namespace *</span>
            <input className="sg-input" value={namespace} onChange={(e) => setNamespace(e.target.value)} placeholder="team" />
          </label>
          <label className="sg-field" style={{ flex: 1 }}>
            <span>project *</span>
            <input className="sg-input" value={project} onChange={(e) => setProject(e.target.value)} placeholder="demo" />
          </label>
          <label className="sg-field" style={{ flex: 1 }}>
            <span>显示名</span>
            <input className="sg-input" value={name} onChange={(e) => setName(e.target.value)} placeholder="演示项目" />
          </label>
          <label className="sg-field" style={{ flex: 2 }}>
            <span>本地仓库目录（可选）</span>
            <input className="sg-input" value={localRoot} onChange={(e) => setLocalRoot(e.target.value)} placeholder="/Users/you/project" />
          </label>
          <button
            className="sg-button sg-button--primary"
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
        <table className="sg-table" style={{ marginTop: 12 }}>
          <thead><tr><th>名称</th><th>GitLab</th><th>状态</th><th>操作</th></tr></thead>
          <tbody>
            {projects.map((p) => (
              <tr key={p.id}>
                <td>{p.name || p.project}</td>
                <td className="sg-muted">{p.namespace}/{p.project}</td>
                <td><StatusMark status={p.status === 'ready' ? 'ready' : 'needs_configuration'} /></td>
                <td>
                  <button className="sg-button" onClick={() => {
                    void rpc('project.archive', { projectId: p.id, archived: true }).then(reload);
                  }}>归档</button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>
    </div>
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
