#!/usr/bin/env node
// 关卡放行竞态与不变量 E2E（ADR-030 M2 / 蓝图 §13.2 gate-release-race-e2e）：
// ① 双击批准（并发 decideRelease 只推进一次）；② 并发改输出 → 旧审批失效（AC-SW-03）；
// ③ 放行审批过期（SIXGATES_APPROVAL_TTL_SECS 钩子）；④ 跨任务待审批互不阻塞（AC-SW-05）。
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import * as readline from 'node:readline';

const CORE = process.env.CORE_BIN ?? join(process.cwd(), 'target', 'release', 'sixgates-core');

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

async function activeKeys(client, workItemId) {
  const cov = await client.call('trace.coverage', { workItemId });
  return (cov.items ?? []).filter((i) => i.status === 'active').map((i) => i.requirementKey);
}

async function prepareReleasableWorkitem(client, project, title) {
  const wi = await client.call('workitem.create', { projectId: project.id, title, description: `${title} 描述` });
  const art = await client.call('artifact.create', { workItemId: wi.id, kind: 'prd', title: 'PRD' });
  const rev = await client.call('artifact.createDraft', { artifactId: art.id, content: `# ${title}\n验收：可放行。`, requirementKeys: await activeKeys(client, wi.id) });
  await client.call('artifact.addReview', { revisionId: rev.id, reviewer: 'pm', verdict: 'approved' });
  await client.call('artifact.freezeBaseline', { workItemId: wi.id, gate: 'requirements', revisionIds: [rev.id] });
  const ev = await client.call('evidence.record', { workItemId: wi.id, gate: 'requirements', kind: 'review', title: '评审', source: 'local', requirementKeys: await activeKeys(client, wi.id) });
  await client.call('evidence.verify', { evidenceId: ev.id, verifiedBy: 'pm' });
  const result = await client.call('gate.evaluate', { workItemId: wi.id, gate: 'requirements' });
  assert(result.passed === true, `${title} 技术门禁通过`);
  return wi;
}

async function main() {
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-release-race-'));
  let client = new CoreClient(dataDir, { SIXGATES_APPROVAL_TTL_SECS: '3600' });
  const fail = (error) => {
    console.error(`E2E 失败：${error.message}`);
    client?.kill();
    rmSync(dataDir, { recursive: true, force: true });
    process.exit(1);
  };

  try {
    await client.hello_();
    const project = await client.call('project.create', {
      gitlabInstance: 'https://gitlab.test', namespace: 'team', project: 'race',
    });

    // ① 双击批准：并发两次 decideRelease，只推进一次。
    {
      const wi = await prepareReleasableWorkitem(client, project, '竞态任务');
      const rr = await client.call('gate.requestRelease', { workItemId: wi.id, gate: 'requirements' });
      const results = await Promise.allSettled([
        client.call('gate.decideRelease', { approvalId: rr.approval_id, decision: 'approved', decidedBy: 'owner', reason: '双击-1' }),
        client.call('gate.decideRelease', { approvalId: rr.approval_id, decision: 'approved', decidedBy: 'owner', reason: '双击-2' }),
      ]);
      const okCount = results.filter((r) => r.status === 'fulfilled').length;
      assert(okCount >= 1, `并发批准至少一次成功（${okCount}/2）`);
      const wiNow = await client.call('workitem.get', { workItemId: wi.id });
      assert(wiNow.workItem.current_gate === 'design', '并发批准后关卡恰好推进一次（design）');
      const attempts = await client.call('stage.attempts', { workItemId: wi.id });
      const reqAttempts = attempts.items.filter((a) => a.gate === 'requirements' && a.state === 'approved');
      assert(reqAttempts.length === 1, '需求关 approved attempt 恰好一个');
      // 幂等重放第三次。
      const replay = await client.call('gate.decideRelease', { approvalId: rr.approval_id, decision: 'approved', decidedBy: 'owner', reason: '重放' });
      assert(replay.releaseRequest?.state === 'approved' || replay.state === 'approved' || replay.attempt, '重放返回幂等视图');
      assert((await client.call('workitem.get', { workItemId: wi.id })).workItem.current_gate === 'design', '重放不重复推进');
    }

    // ② 并发改输出：请求放行后追加证据 → decide 被拒（output_digest_changed），旧审批失效。
    {
      const wi = await prepareReleasableWorkitem(client, project, '漂移任务');
      const rr = await client.call('gate.requestRelease', { workItemId: wi.id, gate: 'requirements' });
      const ev2 = await client.call('evidence.record', { workItemId: wi.id, gate: 'requirements', kind: 'manual', title: '迟到证据', source: 'local' });
      await client.call('evidence.verify', { evidenceId: ev2.id, verifiedBy: 'qa' });
      const err = await client.call('gate.decideRelease', { approvalId: rr.approval_id, decision: 'approved', decidedBy: 'owner', reason: '漂移后批准' }).catch((e) => e);
      // 两条失效路径都符合 AC-SW-03：eager（证据落库即作废 → approval_invalid）或
      // decide 时重算 digest（→ output_digest_changed）。核心不变量：推进被拒。
      assert(
        /output_digest_changed|approval_invalid/.test(err.code ?? err.message),
        '漂移后批准被拒（AC-SW-03：eager 作废或 decide 重算拒绝）',
      );
      const approvals = await client.call('approval.list', {});
      assert(!approvals.items.some((a) => a.id === rr.approval_id), '旧放行审批已不在待审批列表');
      const wiNow = await client.call('workitem.get', { workItemId: wi.id });
      assert(wiNow.workItem.current_gate === 'requirements', '漂移后关卡不推进');
      // 重新评估 → 重新放行 → 批准成功。
      await client.call('gate.evaluate', { workItemId: wi.id, gate: 'requirements' });
      const rr2 = await client.call('gate.requestRelease', { workItemId: wi.id, gate: 'requirements' });
      await client.call('gate.decideRelease', { approvalId: rr2.approval_id, decision: 'approved', decidedBy: 'owner', reason: '新包批准' });
      assert((await client.call('workitem.get', { workItemId: wi.id })).workItem.current_gate === 'design', '新输出包批准后推进');
    }

    // P0-2 负例：评估通过后新增未核验证据 → 旧评估过期，requestRelease 被拒。
    {
      const wi = await prepareReleasableWorkitem(client, project, '过期评估');
      const ev2 = await client.call('evidence.record', {
        workItemId: wi.id, gate: 'requirements', kind: 'manual', title: '迟到的未核验证据', source: 'local',
      });
      assert(!!ev2.id, '新增未核验证据落库');
      const err = await client.call('gate.requestRelease', { workItemId: wi.id, gate: 'requirements' }).catch((e) => e);
      assert(
        /gate_release_required/.test(err.message ?? '') && /过期/.test(err.message ?? ''),
        `P0-2：评估过期后放行申请被拒（${err.message}）`,
      );
      // 重新评估（仍未核验 → 不通过）→ 补核验 → 再评估 → 放行成功。
      const r1 = await client.call('gate.evaluate', { workItemId: wi.id, gate: 'requirements' });
      assert(r1.passed === false && r1.failed_inputs.includes('evidence_complete'), '重新评估正确失败（evidence_complete）');
      await client.call('evidence.verify', { evidenceId: ev2.id, verifiedBy: 'qa' });
      const r2 = await client.call('gate.evaluate', { workItemId: wi.id, gate: 'requirements' });
      assert(r2.passed === true, '补核验后重新评估通过');
      const rr = await client.call('gate.requestRelease', { workItemId: wi.id, gate: 'requirements' });
      assert(rr.state === 'pending', '重新评估后放行申请成功');
    }

    // P0-1 负例：零覆盖（产物不关联需求项）→ 放行被拒。
    {
      const wi = await client.call('workitem.create', { projectId: project.id, title: '零覆盖', description: '零覆盖 描述' });
      const art = await client.call('artifact.create', { workItemId: wi.id, kind: 'prd', title: 'PRD' });
      const rev = await client.call('artifact.createDraft', { artifactId: art.id, content: '# 零覆盖 PRD（不传 keys）' });
      await client.call('artifact.addReview', { revisionId: rev.id, reviewer: 'pm', verdict: 'approved' });
      await client.call('artifact.freezeBaseline', { workItemId: wi.id, gate: 'requirements', revisionIds: [rev.id] });
      const ev = await client.call('evidence.record', { workItemId: wi.id, gate: 'requirements', kind: 'review', title: '评审', source: 'local' });
      await client.call('evidence.verify', { evidenceId: ev.id, verifiedBy: 'pm' });
      const r = await client.call('gate.evaluate', { workItemId: wi.id, gate: 'requirements' });
      assert(r.passed === true, '零覆盖任务技术评估通过');
      const err = await client.call('gate.requestRelease', { workItemId: wi.id, gate: 'requirements' }).catch((e) => e);
      assert(/trace_incomplete/.test(err.code ?? err.message), 'P0-1：零覆盖放行被拒（trace_incomplete）');
    }

    // ④ 跨任务待审批互不阻塞（AC-SW-05）。
    {
      const wiA = await prepareReleasableWorkitem(client, project, '任务A');
      const wiB = await prepareReleasableWorkitem(client, project, '任务B');
      const rrB = await client.call('gate.requestRelease', { workItemId: wiB.id, gate: 'requirements' });
      // 任务 B 有 pending 放行，不阻塞任务 A 的技术评估。
      const evalA = await client.call('gate.evaluate', { workItemId: wiA.id, gate: 'requirements' });
      assert(evalA.passed === true, '任务 B 待审批不阻塞任务 A 门禁（AC-SW-05）');
      const rrA = await client.call('gate.requestRelease', { workItemId: wiA.id, gate: 'requirements' });
      await client.call('gate.decideRelease', { approvalId: rrA.approval_id, decision: 'approved', decidedBy: 'owner', reason: 'A 先走' });
      assert((await client.call('workitem.get', { workItemId: wiA.id })).workItem.current_gate === 'design', '任务 A 独立放行推进');
      await client.call('gate.decideRelease', { approvalId: rrB.approval_id, decision: 'changes_requested', decidedBy: 'owner', reason: 'B 要求修改' });
      const bAfter = await client.call('workitem.get', { workItemId: wiB.id });
      assert(bAfter.workItem.current_gate === 'requirements', '任务 B 要求修改不推进');
    }

    client.kill();

    // ③ 放行审批过期（TTL=1s 钩子）。
    client = new CoreClient(dataDir, { SIXGATES_APPROVAL_TTL_SECS: '1' });
    await client.hello_();
    {
      const wi = await prepareReleasableWorkitem(client, project, '过期任务');
      const rr = await client.call('gate.requestRelease', { workItemId: wi.id, gate: 'requirements' });
      await new Promise((r) => setTimeout(r, 1600));
      const err = await client.call('gate.decideRelease', { approvalId: rr.approval_id, decision: 'approved', decidedBy: 'owner', reason: '迟到批准' }).catch((e) => e);
      assert(err.code === 'approval_expired' || /approval_expired/.test(err.message), '过期放行审批被拒');
      assert((await client.call('workitem.get', { workItemId: wi.id })).workItem.current_gate === 'requirements', '过期后关卡不推进');
    }

    console.log('\ngate-release-race-e2e：全部断言通过 ✓');
    client.kill();
    rmSync(dataDir, { recursive: true, force: true });
  } catch (error) {
    fail(error);
  }
}

main();
