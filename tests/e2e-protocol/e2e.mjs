#!/usr/bin/env node
// 协议级 E2E（规范 §11.3 黄金流程的协议实现，M2/ADR-030 语义重写）：
// 项目 → 工作项 → 六关各自"产出 → 证据 → evaluate（只计算，不推进）→ requestRelease →
// 用户批准（gate.decideRelease）→ 原子进入下一关" → 通关文牒 → 部署审批流 → 时间线/事件验证。
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
    assert(client.hello.schemaVersion >= 17, `schemaVersion ≥ 17（实际 ${client.hello.schemaVersion}）`);

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

    // setStage 已从公开协议移除（ADR-030 §9.1）：放行只经 gate.decideRelease。
    const setStageGone = await client.call('workitem.setStage', { workItemId: wi.id, gate: 'requirements', state: 'passed' }).catch((e) => e);
    assert(setStageGone.code === 'method_not_found', 'workitem.setStage 已移除（method_not_found）');

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

    // 3. M2 人工放行：evaluate 只计算；批准前绝不进入下一关（AC-SW-02）。
    async function passGate(gate, nextGate, prep) {
      if (prep) await prep();
      const result = await client.call('gate.evaluate', { workItemId: wi.id, gate });
      assert(result.passed === true, `${gate} 门禁技术评估通过`);
      const before = await client.call('workitem.get', { workItemId: wi.id });
      assert(before.workItem.current_gate === gate, `${gate} 评估通过后 current_gate 不变（AC-SW-02）`);
      const rr = await client.call('gate.requestRelease', { workItemId: wi.id, gate });
      assert(rr.state === 'pending' && !!rr.approval_id, `${gate} 放行请求冻结输出包并创建审批`);
      // 同内容重复请求幂等。
      const again = await client.call('gate.requestRelease', { workItemId: wi.id, gate });
      assert(again.id === rr.id, `${gate} 重复放行请求幂等`);
      const detail = await client.call('gate.getRelease', { releaseId: rr.id });
      assert(!!detail.package?.digest && detail.items.length > 0, `${gate} 放行详情含输出包 digest 与条目`);
      const decided = await client.call('gate.decideRelease', {
        approvalId: rr.approval_id, decision: 'approved', decidedBy: 'owner', reason: 'E2E 放行',
      });
      const after = await client.call('workitem.get', { workItemId: wi.id });
      assert(after.workItem.current_gate === nextGate, `${gate} 批准后进入 ${nextGate}`);
    }

    await passGate('requirements', 'design');

    // 4. 其余五关（部署关走完整部署审批流）。
    for (const gate of ['design', 'development', 'testing']) {
      await passGate(gate, gate === 'testing' ? 'deployment' : gate === 'development' ? 'testing' : 'development', async () => {
        const kind = gate === 'design' ? 'tech_design' : gate === 'testing' ? 'test_plan' : 'dev_notes';
        const art = await client.call('artifact.create', { workItemId: wi.id, kind, title: gate });
        const rev = await client.call('artifact.createDraft', { artifactId: art.id, content: `# ${gate}\n验收映射…` });
        await client.call('artifact.addReview', { revisionId: rev.id, reviewer: 'tech', verdict: 'approved' });
        await client.call('artifact.freezeBaseline', { workItemId: wi.id, gate, revisionIds: [rev.id] });
        const ev = await client.call('evidence.record', { workItemId: wi.id, gate, kind: 'manual', title: `${gate} 核验`, source: 'local' });
        await client.call('evidence.verify', { evidenceId: ev.id, verifiedBy: 'qa' });
      });
    }

    // 部署关：不可变 digest + 审批绑定 + 验证，再走关卡放行。
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
    await client.call('deployment.submit', { deploymentId: dep.id });
    const denied = await client.call('deployment.deploy', { deploymentId: dep.id }).catch((e) => e);
    assert(denied.code === 'approval_invalid' || /approval/.test(denied.message), '未批准部署被拒');

    const approvals = await client.call('approval.list', {});
    const depApproval = approvals.items.find((a) => a.subject_type === 'deployment');
    assert(!!depApproval, '审批中心出现部署待审批项');
    const decidedDep = await client.call('approval.decide', { approvalId: depApproval.id, decision: 'approved', decidedBy: 'owner', reason: 'E2E 发布' });
    assert(decidedDep.status === 'approved', '审批批准（审计留痕）');

    const deployed = await client.call('deployment.deploy', { deploymentId: dep.id });
    assert(deployed.state === 'awaiting_verification', '部署成功 → awaiting_verification（非 deployed）');
    const verifiedDep = await client.call('deployment.verify', { deploymentId: dep.id });
    assert(verifiedDep.state === 'verified', '验证通过');

    await passGate('deployment', 'verification', async () => {
      // per-gate 基线（蓝图 §5.3）：每关独立产出并冻结自己的基线。
      const art = await client.call('artifact.create', { workItemId: wi.id, kind: 'release_notes', title: '发布说明' });
      const rev = await client.call('artifact.createDraft', { artifactId: art.id, content: '# 发布说明\n镜像 sha256:e2eabc123 → deploy.test' });
      await client.call('artifact.addReview', { revisionId: rev.id, reviewer: 'ops', verdict: 'approved' });
      await client.call('artifact.freezeBaseline', { workItemId: wi.id, gate: 'deployment', revisionIds: [rev.id] });
      const ev = await client.call('evidence.record', { workItemId: wi.id, gate: 'deployment', kind: 'deployment', title: '部署验证通过', source: 'local' });
      await client.call('evidence.verify', { evidenceId: ev.id, verifiedBy: 'ops' });
    });

    // 验证关。
    await passGate('verification', 'verification', async () => {
      const art = await client.call('artifact.create', { workItemId: wi.id, kind: 'acceptance_notes', title: '验收说明' });
      const rev = await client.call('artifact.createDraft', { artifactId: art.id, content: '# 验收说明\n冒烟与验收全部通过。' });
      await client.call('artifact.addReview', { revisionId: rev.id, reviewer: 'qa', verdict: 'approved' });
      await client.call('artifact.freezeBaseline', { workItemId: wi.id, gate: 'verification', revisionIds: [rev.id] });
      const ev = await client.call('evidence.record', { workItemId: wi.id, gate: 'verification', kind: 'smoke', title: '冒烟通过', source: 'local' });
      await client.call('evidence.verify', { evidenceId: ev.id, verifiedBy: 'qa' });
    });

    const passport = await client.call('passport.issue', { workItemId: wi.id });
    assert(passport.gates.length === 6 && !!passport.object_sha256, '通关文牒签发（六关+证据哈希）');

    // attempt 历史：六关各有 attempt，需求关恰好一次批准。
    const attempts = await client.call('stage.attempts', { workItemId: wi.id });
    assert(attempts.items.length >= 6, `attempt 历史完整（${attempts.items.length} 条）`);
    const reqAttempts = attempts.items.filter((a) => a.gate === 'requirements');
    assert(reqAttempts.length === 1 && reqAttempts[0].state === 'approved', '需求关 attempt 恰一次且已批准');
    assert(Array.isArray(reqAttempts[0].activities) && reqAttempts[0].activities.length > 0, 'attempt 携带阶段活动模板');

    // 6. 时间线与事件通知。
    const timeline = await client.call('timeline.snapshot', { workItemId: wi.id });
    assert(timeline.events.length > 10, `时间线快照（${timeline.events.length} 事件）`);
    assert(client.events.length > 10, `事件 notification 推送（${client.events.length} 条）`);
    assert(timeline.events.some((e) => e.type === 'gate.release_approved'), '时间线包含放行批准事件');

    // 7. 门禁不通过路径（伪造通过不可能）。
    const wi2 = await client.call('workitem.create', { projectId: project.id, title: '空任务', description: '' });
    const gateEmpty = await client.call('gate.evaluate', { workItemId: wi2.id, gate: 'requirements' });
    assert(gateEmpty.passed === false, '无证据任务门禁必须失败');
    const releaseEmpty = await client.call('gate.requestRelease', { workItemId: wi2.id, gate: 'requirements' }).catch((e) => e);
    assert(/gate_release_required/.test(releaseEmpty.message), '未通过评估不可请求放行');
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
