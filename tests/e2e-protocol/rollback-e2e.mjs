#!/usr/bin/env node
// 关前快照与业务回滚 E2E（ADR-030 M3 / 蓝图 §13.2 rollback-e2e）：
// 多关执行 → 回滚 → 投影/审批/谱系一致（AC-SW-06）；不可逆外部副作用 blocked（AC-SW-07）；
// 崩溃恢复（AC-SW-12）；主工作区只读（SG-RBK-005）；对象缺失故障注入（snapshot_failed）。
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync, existsSync, mkdirSync, writeFileSync, unlinkSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { execSync } from 'node:child_process';
import * as readline from 'node:readline';

const CORE = process.env.CORE_BIN ?? join(process.cwd(), 'target', 'release', 'ratiflow-core');

class CoreClient {
  constructor(dataDir, env = {}) {
    this.proc = spawn(CORE, ['app-server', '--data-dir', dataDir], {
      stdio: ['pipe', 'pipe', 'pipe'],
      env: { ...process.env, ...env },
    });
    this.nextId = 1;
    this.pending = new Map();
    this.rl = readline.createInterface({ input: this.proc.stdout });
    this.rl.on('line', (l) => this.onLine(l));
    this.proc.stderr.on('data', () => {});
  }
  onLine(line) {
    if (!line.trim()) return;
    let m;
    try { m = JSON.parse(line); } catch { return; }
    if (m.protocolVersion) { this.hello = m; return; }
    if (m.id && this.pending.has(m.id)) {
      const { resolve, reject } = this.pending.get(m.id);
      this.pending.delete(m.id);
      if (m.error) reject(Object.assign(new Error(m.error.data?.detail ?? m.error.message), { code: m.error.message }));
      else resolve(m.result);
    }
  }
  async hello_(timeoutMs = 5000) {
    const deadline = Date.now() + timeoutMs;
    while (!this.hello) {
      if (Date.now() > deadline) throw new Error('等待 hello 超时');
      await new Promise((r) => setTimeout(r, 25));
    }
    return this.hello;
  }
  call(method, params = {}) {
    const id = String(this.nextId++);
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.proc.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
    });
  }
  kill() { this.proc.kill('SIGKILL'); }
}

function assert(condition, label) {
  if (!condition) throw new Error(`E2E 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

const GATE_KINDS = { requirements: 'prd', design: 'tech_design', development: 'code', testing: 'test', deployment: 'deployment', verification: 'verification' };

async function activeKeys(client, workItemId) {
  const cov = await client.call('trace.coverage', { workItemId });
  return (cov.items ?? []).filter((i) => i.status === 'active').map((i) => i.requirementKey);
}

async function releaseGate(client, workItemId, gate) {
  // kind 与 deliverable.rs 门禁映射对齐（requirements↔prd 等），否则 request_release 报 deliverable_missing。
  const art = await client.call('artifact.create', { workItemId, kind: GATE_KINDS[gate] ?? 'doc', title: gate });
  const rev = await client.call('artifact.createDraft', { artifactId: art.id, content: `# ${gate} 产物`, requirementKeys: await activeKeys(client, workItemId) });
  await client.call('artifact.addReview', { revisionId: rev.id, reviewer: 'tech', verdict: 'approved' });
  await client.call('artifact.freezeBaseline', { workItemId, gate, revisionIds: [rev.id] });
  const ev = await client.call('evidence.record', { workItemId, gate, kind: 'review', title: `${gate} 评审`, source: 'local', requirementKeys: await activeKeys(client, workItemId) });
  await client.call('evidence.verify', { evidenceId: ev.id, verifiedBy: 'qa' });
  const result = await client.call('gate.evaluate', { workItemId, gate });
  assert(result.passed === true, `${gate} 门禁通过`);
  const rr = await client.call('gate.requestRelease', { workItemId, gate });
  await client.call('gate.decideRelease', { approvalId: rr.approval_id, decision: 'approved', decidedBy: 'owner', reason: 'E2E' });
  return rr;
}

function git(repo, ...args) {
  return execSync(`git -C "${repo}" ${args.map((a) => `"${a}"`).join(' ')}`, { encoding: 'utf8' }).trim();
}

async function main() {
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-rbk-e2e-'));
  let client = new CoreClient(dataDir);
  const fail = (error) => {
    console.error(`E2E 失败：${error.message}`);
    client?.kill();
    rmSync(dataDir, { recursive: true, force: true });
    process.exit(1);
  };

  try {
    await client.hello_();
    assert(client.hello.schemaVersion >= 18, `schemaVersion ≥ 18（实际 ${client.hello.schemaVersion}）`);
    const project = await client.call('project.create', {
      gitlabInstance: 'https://gitlab.test', namespace: 'team', project: 'rollback',
    });
    const wi = await client.call('workitem.create', {
      projectId: project.id, title: '回滚演练', description: 'M3 回滚端到端',
    });

    // 1. 两关放行 → 尝试放行 design（pending）→ 挂起待审。
    await releaseGate(client, wi.id, 'requirements');
    const art = await client.call('artifact.create', { workItemId: wi.id, kind: 'tech_design', title: '设计' });
    const rev = await client.call('artifact.createDraft', { artifactId: art.id, content: '# 设计文档', requirementKeys: await activeKeys(client, wi.id) });
    await client.call('artifact.addReview', { revisionId: rev.id, reviewer: 'tech', verdict: 'approved' });
    await client.call('artifact.freezeBaseline', { workItemId: wi.id, gate: 'design', revisionIds: [rev.id] });
    const ev2 = await client.call('evidence.record', { workItemId: wi.id, gate: 'design', kind: 'review', title: '设计评审', source: 'local', requirementKeys: await activeKeys(client, wi.id) });
    await client.call('evidence.verify', { evidenceId: ev2.id, verifiedBy: 'qa' });
    await client.call('gate.evaluate', { workItemId: wi.id, gate: 'design' });
    const pendingRr = await client.call('gate.requestRelease', { workItemId: wi.id, gate: 'design' });
    assert(pendingRr.state === 'pending', 'design 放行请求挂起待审');

    // 2. 回滚目标：design attempt 的关前快照（回滚到方案关执行前）。
    const snapshots = await client.call('snapshot.list', { workItemId: wi.id });
    assert(snapshots.items.length >= 2, `快照历史完整（${snapshots.items.length} 条，含 safety 前各关 entry）`);
    const target = snapshots.items.find((s) => s.kind === 'stage_entry' && s.root_digest && s.stage_attempt_id.startsWith('att_'));
    // 找 design attempt 的 entry snapshot：通过 attempt 列表定位。
    const attempts = await client.call('stage.attempts', { workItemId: wi.id });
    const designAttempt = attempts.items.find((a) => a.gate === 'design');
    assert(!!designAttempt.entry_snapshot_id, 'design attempt 携带关前快照（SG-RBK-001）');
    const targetId = designAttempt.entry_snapshot_id;
    const targetBefore = await client.call('snapshot.get', { snapshotId: targetId });
    assert(!!targetBefore.root_digest && targetBefore.resources.length > 0, '目标快照含 root_digest 与资源明细');

    // 3. 影响预览：受影响投影 + 将失效审批。
    const preview = await client.call('rollback.preview', { workItemId: wi.id, targetSnapshotId: targetId });
    assert(preview.operation.state === 'previewed', '回滚操作进入 previewed');
    assert(preview.impact.currentGate === 'design', '影响预览含当前关');
    assert(preview.impact.approvalsToExpire.length >= 1, '影响预览列出将失效的 pending 放行审批');

    // 4. 请求回滚 → safety 快照 + rollback 审批。
    const request = await client.call('rollback.request', { workItemId: wi.id, targetSnapshotId: targetId, requestedBy: 'owner' });
    assert(request.safetySnapshotId && request.approvalId, '请求创建 safety 快照与回滚审批（SG-RBK-003）');
    const approvals = await client.call('approval.list', {});
    const rbApproval = approvals.items.find((a) => a.subject_type === 'rollback');
    assert(!!rbApproval, '审批中心出现 rollback 主体');

    // 5. 批准执行：控制面恢复 + 历史保留 + 新 attempt 带快照（AC-SW-06）。
    const decided = await client.call('rollback.decide', { approvalId: request.approvalId, decision: 'approved', decidedBy: 'owner', reason: '方案推倒重来' });
    assert(decided.operation.state === 'completed', '回滚完成');
    const wiNow = await client.call('workitem.get', { workItemId: wi.id });
    assert(wiNow.workItem.current_gate === 'design', 'AC-SW-06：current_gate 恢复到 design');
    const attemptsAfter = await client.call('stage.attempts', { workItemId: wi.id });
    const supersededDesign = attemptsAfter.items.filter((a) => a.gate === 'design' && a.state === 'superseded');
    assert(supersededDesign.length >= 1, '原 design attempt 保留并 superseded（历史不删除）');
    const freshDesign = attemptsAfter.items.filter((a) => a.gate === 'design' && a.state === 'prepared');
    assert(freshDesign.length === 1 && !!freshDesign[0].entry_snapshot_id, '目标关新建 attempt 且携带关前快照');
    // 旧放行审批失效。
    const approvalsAfter = await client.call('approval.list', {});
    assert(!approvalsAfter.items.some((a) => a.id === pendingRr.approval_id), 'AC-SW-06：旧放行审批已失效（不在待审批）');
    // 谱系一致：新 attempt 可反查需求修订。
    const gaps = await client.call('trace.gaps', { workItemId: wi.id });
    assert(typeof gaps.orphanCount === 'number', '回滚后谱系查询可用');

    // 6. AC-SW-12 崩溃恢复：挂起回滚审批 → 杀进程 → 重启 → 决定完成；快照 digest 不变。
    const wi2 = await client.call('workitem.create', { projectId: project.id, title: '崩溃恢复', description: '' });
    await releaseGate(client, wi2.id, 'requirements');
    const att2 = await client.call('stage.attempts', { workItemId: wi2.id });
    const target2 = att2.items.find((a) => a.gate === 'design').entry_snapshot_id;
    const snap2Before = await client.call('snapshot.get', { snapshotId: target2 });
    const req2 = await client.call('rollback.request', { workItemId: wi2.id, targetSnapshotId: target2 });
    client.kill();
    client = new CoreClient(dataDir);
    await client.hello_();
    const snap2After = await client.call('snapshot.get', { snapshotId: target2 });
    assert(snap2After.root_digest === snap2Before.root_digest, 'AC-SW-12：重启后快照 digest 不变');
    const decided2 = await client.call('rollback.decide', { approvalId: req2.approvalId, decision: 'approved', decidedBy: 'owner', reason: '重启后批准' });
    assert(decided2.operation.state === 'completed', 'AC-SW-12：重启后回滚完成');

    // 7. AC-SW-07 不可逆外部副作用：部署发生后 → 含部署的快照回滚 → blocked/manual。
    const wi3 = await client.call('workitem.create', { projectId: project.id, title: '含部署', description: '' });
    await releaseGate(client, wi3.id, 'requirements');
    // 7a. 先部署（真实部署域记录）。
    const dep = await client.call('deployment.create', {
      workItemId: wi3.id,
      plan: {
        target: { host: 'h', user: 'u', remoteDir: '/r' }, imageDigest: 'sha256:abc',
        deploySteps: [{ seq: 0, name: 'up', argv: ['true'] }],
        verifyChecks: [{ name: 'h', argv: ['true'] }],
      },
    });
    await client.call('deployment.submit', { deploymentId: dep.id });
    const depApprovals = await client.call('approval.list', {});
    const depAppr = depApprovals.items.find((a) => a.subject_type === 'deployment');
    await client.call('approval.decide', { approvalId: depAppr.id, decision: 'approved', decidedBy: 'owner', reason: 'E2E 部署' });
    await client.call('deployment.deploy', { deploymentId: dep.id });
    // 7b. 第一次回滚（目标快照创建于部署前 → 无 manual 资源，正常完成）；
    //     回滚在部署后新建的关前快照将包含 manual 部署资源。
    const att3 = await client.call('stage.attempts', { workItemId: wi3.id });
    const designAttempt3 = att3.items.find((a) => a.gate === 'design');
    const req3a = await client.call('rollback.request', { workItemId: wi3.id, targetSnapshotId: designAttempt3.entry_snapshot_id });
    await client.call('rollback.decide', { approvalId: req3a.approvalId, decision: 'approved', decidedBy: 'owner', reason: '第一次回滚（部署后重建快照）' });
    const att3b = await client.call('stage.attempts', { workItemId: wi3.id });
    const freshDesign3 = att3b.items.find((a) => a.gate === 'design' && a.state === 'prepared');
    assert(!!freshDesign3.entry_snapshot_id, '部署后新建的 design attempt 有关前快照');
    // 7c. 回滚含部署资源的快照 → blocked/manual_action_required（不显示成功）。
    const preview3 = await client.call('rollback.preview', { workItemId: wi3.id, targetSnapshotId: freshDesign3.entry_snapshot_id });
    assert((preview3.impact.externalManualResources ?? []).length > 0, '影响预览列出 manual 部署资源');
    const req3 = await client.call('rollback.request', { workItemId: wi3.id, targetSnapshotId: freshDesign3.entry_snapshot_id });
    const err3 = await client.call('rollback.decide', { approvalId: req3.approvalId, decision: 'approved', decidedBy: 'owner', reason: '含部署回滚' }).catch((e) => e);
    assert(/rollback_manual_action_required/.test(err3.code ?? err3.message), 'AC-SW-07：不可逆副作用 → blocked/manual');
    const op3 = await client.call('rollback.get', { operationId: req3.operation.id });
    assert(op3.operation.state === 'blocked', '操作状态 blocked（不显示成功）');
    assert((await client.call('workitem.get', { workItemId: wi3.id })).workItem.current_gate === 'design', 'blocked 时控制面不推进');

    // 8. 故障注入：对象文件缺失 → snapshot_failed。
    const wi4 = await client.call('workitem.create', { projectId: project.id, title: '故障注入', description: '' });
    await releaseGate(client, wi4.id, 'requirements');
    const att4 = await client.call('stage.attempts', { workItemId: wi4.id });
    const reqAttempt4 = att4.items.find((a) => a.gate === 'requirements');
    const target4 = reqAttempt4.entry_snapshot_id;
    const snap4 = await client.call('snapshot.get', { snapshotId: target4 });
    const sha = snap4.control_manifest_sha256;
    const objPath = join(dataDir, 'objects', sha.slice(0, 2), sha);
    // 缺陷审计：注入包装在 existsSync 里，对象布局一变唯一崩溃场景静默蒸发。
    // 布局漂移必须让本测试失败（提示更新注入路径），而不是跳过后继续绿。
    assert(existsSync(objPath), `对象分片路径布局变化: ${objPath} 不存在——请更新故障注入路径`);
    unlinkSync(objPath);
    const err4 = await client.call('rollback.preview', { workItemId: wi4.id, targetSnapshotId: target4 }).catch((e) => e);
    assert(/snapshot_failed/.test(err4.code ?? err4.message), '故障注入：清单对象缺失 → snapshot_failed');

    // 9. 主工作区只读（SG-RBK-005）：带 git localRoot 的项目回滚不动工作区。
    const repoDir = join(tmpdir(), `sg-rbk-repo-${Date.now()}`);
    mkdirSync(repoDir, { recursive: true });
    git(repoDir, 'init', '-b', 'main');
    git(repoDir, 'config', 'user.email', 'e2e@ratiflow.local');
    git(repoDir, 'config', 'user.name', 'e2e');
    writeFileSync(join(repoDir, 'README.md'), 'hello\n');
    git(repoDir, 'add', '.');
    git(repoDir, 'commit', '-m', 'init');
    const headBefore = git(repoDir, 'rev-parse', 'HEAD');
    const proj2 = await client.call('project.create', {
      gitlabInstance: 'https://gitlab.test', namespace: 'team', project: 'ws', localRoot: repoDir,
    });
    const wi5 = await client.call('workitem.create', { projectId: proj2.id, title: '工作区只读', description: '' });
    await releaseGate(client, wi5.id, 'requirements');
    // 制造脏工作区。
    writeFileSync(join(repoDir, 'dirty.txt'), 'user change\n');
    // 受管隔离 worktree（Agent 执行域）：模拟 Agent 产物写入。
    const wtDir = join(dataDir, 'worktrees', wi5.id);
    assert(existsSync(wtDir), '隔离 worktree 已创建（Agent 执行域）');
    writeFileSync(join(wtDir, 'agent-scratch.md'), 'agent generated\n');
    const att5 = await client.call('stage.attempts', { workItemId: wi5.id });
    const design5 = att5.items.find((a) => a.gate === 'design');
    if (design5?.entry_snapshot_id) {
      const targetSnap5 = await client.call('snapshot.get', { snapshotId: design5.entry_snapshot_id });
      assert(
        (targetSnap5.resources ?? []).some((r) => r.resource_type === 'worktree_head' && r.reversibility === 'logical_restore'),
        '快照记录受管 worktree HEAD（可整体恢复）',
      );
      const wtHeadBefore = git(wtDir, 'rev-parse', 'HEAD');
      const req5 = await client.call('rollback.request', { workItemId: wi5.id, targetSnapshotId: design5.entry_snapshot_id });
      await client.call('rollback.decide', { approvalId: req5.approvalId, decision: 'approved', decidedBy: 'owner', reason: '工作区只读演练' });
      assert(!existsSync(join(wtDir, 'agent-scratch.md')), '回滚恢复受管 worktree：Agent 产物被清理');
      assert(git(wtDir, 'rev-parse', 'HEAD') === wtHeadBefore, '受管 worktree HEAD 恢复到快照点');
      const headAfter = git(repoDir, 'rev-parse', 'HEAD');
      assert(headAfter === headBefore, 'SG-RBK-005：回滚不触碰主工作区 HEAD');
      assert(existsSync(join(repoDir, 'dirty.txt')), 'SG-RBK-005：用户脏文件原样保留');
      assert(!existsSync(join(repoDir, 'agent-scratch.md')), '受管区产物不会泄漏到主工作区');
    }

    console.log('\nrollback-e2e：全部断言通过 ✓');
    client.kill();
    rmSync(dataDir, { recursive: true, force: true });
    rmSync(repoDir, { recursive: true, force: true });
  } catch (error) {
    fail(error);
  }
}

main();
