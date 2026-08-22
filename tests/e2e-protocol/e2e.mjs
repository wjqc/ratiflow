#!/usr/bin/env node
// 协议级 E2E（规范 §11.3 黄金流程的协议实现）：
// Node 客户端 spawn Rust core，经 JSON-RPC 走完：项目 → 工作项 → 需求关全步骤 →
// 六关证据与门禁 → 通关文牒 → 部署审批流 → 时间线/事件验证。
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import * as readline from 'node:readline';

const CORE = process.env.CORE_BIN ?? join(process.cwd(), 'target', 'release', 'sixgates-core');

class CoreClient {
  constructor(binary, dataDir) {
    this.proc = spawn(binary, ['app-server', '--data-dir', dataDir], { stdio: ['pipe', 'pipe', 'pipe'] });
    this.nextId = 1;
    this.pending = new Map();
    this.events = [];
    this.dead = false;
    this.rl = readline.createInterface({ input: this.proc.stdout });
    this.rl.on('line', (line) => this.onLine(line));
    this.proc.stderr.on('data', () => {});
    this.proc.on('exit', () => { this.dead = true; });
  }

  onLine(line) {
    if (!line.trim()) return;
    let message;
    try { message = JSON.parse(line); } catch { return; }
    if (message.method === 'event' && message.params) {
      this.events.push(message.params);
      return;
    }
    if (message.protocolVersion) {
      this.hello = message;
      return;
    }
    if (message.id && this.pending.has(message.id)) {
      const { resolve, reject } = this.pending.get(message.id);
      this.pending.delete(message.id);
      if (message.error) {
        reject(Object.assign(new Error(message.error.data?.detail ?? message.error.message), { code: message.error.message }));
      } else {
        resolve(message.result);
      }
    }
  }

  call(method, params = {}) {
    const id = String(this.nextId++);
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.proc.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
    });
  }

  kill() {
    this.proc.kill('SIGKILL');
  }
}

function assert(condition, label) {
  if (!condition) {
    throw new Error(`E2E 断言失败：${label}`);
  }
  console.log(`  ✓ ${label}`);
}

async function main() {
  const dataDir = await mkdtempSync(join(tmpdir(), 'sg-e2e-'));
  const client = new CoreClient(CORE, dataDir);
  const fail = (error) => {
    console.error(`E2E 失败：${error.message}`);
    client.kill();
    rmSync(dataDir, { recursive: true, force: true });
    process.exit(1);
  };

  try {
    // 等待 hello 握手。
    for (let i = 0; i < 50 && !client.hello; i++) {
      await new Promise((r) => setTimeout(r, 100));
    }
    assert(client.hello?.protocolVersion === '1', 'hello 握手 protocolVersion=1');

    // 1. 项目与知识库。
    const project = await client.call('project.create', {
      gitlabInstance: 'https://gitlab.test', namespace: 'team', project: 'demo', name: 'E2E 项目',
    });
    assert(!!project.id, '项目创建');

    // 2. 需求关全步骤（文档驱动）。
    const wi = await client.call('workitem.create', {
      projectId: project.id, title: '支持 SSO 登录', description: 'OIDC 接入公司 IdP',
    });
    assert(!!wi.id && !!wi.requirementDoc, '工作项创建 + 需求文档落盘');

    const artifact = await client.call('artifact.create', { workItemId: wi.id, kind: 'prd', title: 'PRD' });
    const draft = await client.call('artifact.createDraft', { artifactId: artifact.id, content: '# PRD\n范围：OIDC 登录。\n验收：回调建立会话。' });
    assert(draft.status === 'draft' && !!draft.etag, 'PRD 草稿 + ETag');

    const badUpdate = await client.call('artifact.updateDraft', { revisionId: draft.id, etag: '"wrong"', content: 'x' }).catch((e) => e);
    assert(badUpdate.code === 'etag_mismatch', 'ETag 冲突返回 etag_mismatch');

    await client.call('artifact.addReview', { revisionId: draft.id, reviewer: 'pm', verdict: 'approved', comment: 'LGTM' });
    const baseline = await client.call('artifact.freezeBaseline', { workItemId: wi.id, gate: 'requirements', revisionIds: [draft.id] });
    assert(!!baseline.inputs_sha256, '基线冻结（含 inputs SHA）');

    const evidence = await client.call('evidence.record', { workItemId: wi.id, gate: 'requirements', kind: 'review', title: 'PRD 评审', source: 'local' });
    await client.call('evidence.verify', { evidenceId: evidence.id, verifiedBy: 'pm' });
    assert(evidence.id.length > 0, '证据记录与复验');

    const gate1 = await client.call('gate.evaluate', { workItemId: wi.id, gate: 'requirements' });
    assert(gate1.passed === true, '需求关门禁通过');
    const afterGate1 = await client.call('workitem.get', { workItemId: wi.id });
    assert(afterGate1.workItem.current_gate === 'design', '过关后自动进入方案关');

    // 3. 其余五关：证据 + 门禁（部署关走完整审批流）。
    for (const gate of ['design', 'development', 'testing']) {
      const ev = await client.call('evidence.record', { workItemId: wi.id, gate, kind: 'manual', title: `${gate} 核验`, source: 'local' });
      await client.call('evidence.verify', { evidenceId: ev.id, verifiedBy: 'qa' });
      // 前序关需要基线；方案/测试关在未冻结工件时门禁应明确失败而非伪造通过。
      const result = await client.call('gate.evaluate', { workItemId: wi.id, gate });
      if (result.passed) {
        console.log(`  ✓ ${gate} 门禁通过（基线满足）`);
      } else {
        console.log(`  ○ ${gate} 门禁未过（${result.failed_inputs.join(',')}）— 基线未冻结时的正确行为`);
        // 冻结基线（复用最新 in_review 修订不可得 → 直接补齐工件）。
        const art = await client.call('artifact.create', { workItemId: wi.id, kind: gate === 'design' ? 'tech_design' : 'test_plan', title: gate });
        const rev = await client.call('artifact.createDraft', { artifactId: art.id, content: `# ${gate}\n验收映射…` });
        await client.call('artifact.addReview', { revisionId: rev.id, reviewer: 'tech', verdict: 'approved' });
        await client.call('artifact.freezeBaseline', { workItemId: wi.id, gate, revisionIds: [rev.id] });
        const retried = await client.call('gate.evaluate', { workItemId: wi.id, gate });
        assert(retried.passed === true, `${gate} 补齐基线后门禁通过`);
      }
    }

    // 4. 部署关：不可变 digest + 审批绑定 + 验证。
    const dep = await client.call('deployment.create', {
      workItemId: wi.id,
      plan: {
        target: { host: 'deploy.test', port: 22, user: 'deploy', expectedFingerprint: 'SHA256:x', remoteDir: '/srv' },
        imageDigest: 'sha256:e2eabc123',
        deploySteps: [{ seq: 0, name: 'up', argv: ['docker', 'compose', 'up', '-d'], timeoutSec: 60 }],
        verifyChecks: [{ name: 'health', argv: ['curl', '-f', 'http://localhost/h'], required: true }],
        rollbackSteps: [{ seq: 0, name: 'down', argv: ['docker', 'compose', 'down'], timeoutSec: 60 }],
      },
    });
    const badTag = await client.call('deployment.create', {
      workItemId: wi.id,
      plan: { target: { host: 'h', user: 'u', remoteDir: '/r' }, imageDigest: 'latest',
        deploySteps: [{ seq: 0, name: 'up', argv: ['docker', 'compose', 'up', '-d'] }],
        verifyChecks: [{ name: 'h', argv: ['true'] }] },
    }).catch((e) => e);
    assert(badTag.code === 'digest_drift' || /digest/.test(badTag.message), '可漂移 tag 被拒绝');

    await client.call('deployment.submit', { deploymentId: dep.id });
    const denied = await client.call('deployment.deploy', { deploymentId: dep.id }).catch((e) => e);
    assert(denied.code === 'approval_invalid' || /approval/.test(denied.message), '未批准部署被拒');

    const approvals = await client.call('approval.list', {});
    assert(approvals.items.length === 1, '审批中心出现待审批项');
    const approval = approvals.items[0];
    const decided = await client.call('approval.decide', { approvalId: approval.id, decision: 'approved', decidedBy: 'owner', reason: 'E2E 发布' });
    assert(decided.status === 'approved', '审批批准（审计留痕）');

    const deployed = await client.call('deployment.deploy', { deploymentId: dep.id });
    assert(deployed.state === 'awaiting_verification', '部署成功 → awaiting_verification（非 deployed）');
    const verifiedDep = await client.call('deployment.verify', { deploymentId: dep.id });
    assert(verifiedDep.state === 'verified', '验证通过');

    const depEvidence = await client.call('evidence.record', { workItemId: wi.id, gate: 'deployment', kind: 'deployment', title: '部署验证通过', source: 'local' });
    await client.call('evidence.verify', { evidenceId: depEvidence.id, verifiedBy: 'ops' });
    const depGate = await client.call('gate.evaluate', { workItemId: wi.id, gate: 'deployment' });
    assert(depGate.passed === true, '部署关门禁通过');

    // 5. 验证关 + 通关文牒。
    const verEvidence = await client.call('evidence.record', { workItemId: wi.id, gate: 'verification', kind: 'smoke', title: '冒烟通过', source: 'local' });
    await client.call('evidence.verify', { evidenceId: verEvidence.id, verifiedBy: 'qa' });
    const verGate = await client.call('gate.evaluate', { workItemId: wi.id, gate: 'verification' });
    assert(verGate.passed === true, '验证关门禁通过');

    const passport = await client.call('passport.issue', { workItemId: wi.id });
    assert(passport.gates.length === 6 && !!passport.object_sha256, '通关文牒签发（六关+证据哈希）');

    // 6. 时间线与事件通知。
    const timeline = await client.call('timeline.snapshot', { workItemId: wi.id });
    assert(timeline.events.length > 10, `时间线快照（${timeline.events.length} 事件）`);
    assert(client.events.length > 10, `事件 notification 推送（${client.events.length} 条）`);

    // 7. 门禁不通过路径（伪造通过不可能）。
    const wi2 = await client.call('workitem.create', { projectId: project.id, title: '空任务', description: '' });
    const gateEmpty = await client.call('gate.evaluate', { workItemId: wi2.id, gate: 'requirements' });
    assert(gateEmpty.passed === false, '无证据任务门禁必须失败');
    const rejectedPassport = await client.call('passport.issue', { workItemId: wi2.id }).catch((e) => e);
    assert(/incomplete/.test(rejectedPassport.message), '六关未全过时文牒签发被拒');

    console.log(`\nE2E 黄金流程通过：${client.events.length} 事件、${timeline.events.length} 时间线条目。`);
    client.kill();
    rmSync(dataDir, { recursive: true, force: true });
    process.exit(0);
  } catch (error) {
    fail(error);
  }
}

void main();
