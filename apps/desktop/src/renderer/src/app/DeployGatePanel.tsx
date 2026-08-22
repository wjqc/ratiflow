import { useState } from 'react';
import { rpc, rpcErrorMessage } from '../rpc/client';
import { EvaluateButton } from './DocGatePanel';

interface Props {
  workItemId: string;
  onDone: () => void;
  onOpenApprovals: () => void;
}

interface DeploymentInfo { id: string; state: string; target: string; image_digest: string }

// 部署关：不可变 digest 计划 → 提交审批（绑定 ActionDigest）→ SSH 预检+部署 → 验证 → 门禁。
export default function DeployGatePanel({ workItemId, onDone, onOpenApprovals }: Props) {
  const [host, setHost] = useState('deploy.example.internal');
  const [fingerprint, setFingerprint] = useState('SHA256:…');
  const [remoteDir, setRemoteDir] = useState('/srv/app');
  const [digest, setDigest] = useState('sha256:…');
  const [deployment, setDeployment] = useState<DeploymentInfo | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');

  const run = async (action: () => Promise<string>) => {
    setBusy(true);
    setError('');
    setNotice('');
    try {
      setNotice(await action());
      onDone();
    } catch (reason) {
      setError(rpcErrorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="gate-workspace">
      <ol className="step-checklist">
        <li className={deployment && deployment.state !== 'draft' ? 'done' : ''}>制定部署计划</li>
        <li className={deployment && !['draft', 'awaiting_approval'].includes(deployment.state) ? 'done' : ''}>人工审批</li>
        <li className={deployment?.state === 'verified' ? 'done' : ''}>部署并验证</li>
      </ol>
      {error ? <div className="sg-banner sg-banner--error" role="alert">{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

      {!deployment || deployment.state === 'draft' ? (
        <>
          <div className="sg-row">
            <label className="sg-field" style={{ flex: 1 }}>
              <span>SSH 主机</span>
              <input className="sg-input" value={host} onChange={(e) => setHost(e.target.value)} />
            </label>
            <label className="sg-field" style={{ flex: 1 }}>
              <span>主机指纹</span>
              <input className="sg-input" value={fingerprint} onChange={(e) => setFingerprint(e.target.value)} />
            </label>
            <label className="sg-field" style={{ width: 140 }}>
              <span>远程目录</span>
              <input className="sg-input" value={remoteDir} onChange={(e) => setRemoteDir(e.target.value)} />
            </label>
          </div>
          <label className="sg-field">
            <span>镜像 digest（必须 sha256: 不可变引用；可漂移 tag 被拒绝）</span>
            <input className="sg-input" value={digest} onChange={(e) => setDigest(e.target.value)} placeholder="sha256:abc123…" />
          </label>
          <button
            className="sg-button sg-button--primary"
            disabled={busy}
            onClick={() => {
              void run(async () => {
                const created = await rpc<DeploymentInfo>('deployment.create', {
                  workItemId,
                  plan: {
                    target: { host, port: 22, user: 'deploy', expectedFingerprint: fingerprint, remoteDir },
                    imageDigest: digest,
                    deploySteps: [{ seq: 0, name: 'compose_up', argv: ['docker', 'compose', 'up', '-d'], timeoutSec: 180 }],
                    verifyChecks: [{ name: 'health', argv: ['curl', '-f', 'http://localhost:8080/healthz'], required: true }],
                    rollbackSteps: [{ seq: 0, name: 'compose_down', argv: ['docker', 'compose', 'down'], timeoutSec: 120 }],
                  },
                });
                setDeployment(created);
                await rpc('deployment.submit', { deploymentId: created.id });
                return '已提交审批：到「审批中心」批准后回到本页执行部署。';
              });
            }}
          >
            创建部署计划并提交审批
          </button>
        </>
      ) : (
        <>
          <p className="sg-muted">
            部署 <span className="sg-code">{deployment.id.slice(0, 14)}…</span> · 状态 <strong>{deployment.state}</strong>
          </p>
          {deployment.state === 'awaiting_approval' ? (
            <div className="sg-row">
              <p className="sg-muted" style={{ margin: 0 }}>⏳ 等待审批（批准绑定计划指纹，改参数需重新审批）。</p>
              <button className="sg-button" onClick={onOpenApprovals}>前往审批中心 →</button>
            </div>
          ) : null}
          {deployment.state === 'approved' ? (
            <button
              className="sg-button sg-button--primary"
              disabled={busy}
              onClick={() => {
                void run(async () => {
                  const updated = await rpc<DeploymentInfo>('deployment.deploy', { deploymentId: deployment.id });
                  setDeployment(updated);
                  return `部署执行完成（${updated.state}）。`;
                });
              }}
            >
              执行部署（SSH 预检 + Compose）
            </button>
          ) : null}
          {deployment.state === 'awaiting_verification' ? (
            <button
              className="sg-button sg-button--primary"
              disabled={busy}
              onClick={() => {
                void run(async () => {
                  const updated = await rpc<DeploymentInfo>('deployment.verify', { deploymentId: deployment.id });
                  setDeployment(updated);
                  if (updated.state !== 'verified') {
                    throw new Error(`验证未通过（${updated.state}）；Agent 不可覆盖，需回滚后重试。`);
                  }
                  const evidence = await rpc<{ id: string }>('evidence.record', {
                    workItemId, gate: 'deployment', kind: 'deployment',
                    title: `部署 ${updated.target} 验证通过`, source: 'local',
                  });
                  await rpc('evidence.verify', { evidenceId: evidence.id, verifiedBy: 'local-user' });
                  return '验证通过并已记录证据。';
                });
              }}
            >
              运行验证检查
            </button>
          ) : null}
          {deployment.state === 'verified' ? (
            <div className="sg-row">
              <EvaluateButton workItemId={workItemId} gate="deployment" busy={busy} onDone={onDone} />
            </div>
          ) : null}
          {['deploy_failed', 'verification_failed', 'rollback_failed'].includes(deployment.state) ? (
            <div className="sg-stack">
              <div className="sg-banner sg-banner--error" role="alert">
                部署状态 {deployment.state}：按 Runbook 处置（回滚或修正后重新制定计划）。
              </div>
              {deployment.state !== 'rollback_failed' ? (
                <button
                  className="sg-button sg-button--danger"
                  disabled={busy}
                  onClick={() => {
                    void run(async () => {
                      const updated = await rpc<DeploymentInfo>('deployment.rollback', { deploymentId: deployment.id });
                      setDeployment(updated);
                      return `回滚完成（${updated.state}）。`;
                    });
                  }}
                >
                  回滚到 previous digest
                </button>
              ) : null}
            </div>
          ) : null}
        </>
      )}
    </div>
  );
}
