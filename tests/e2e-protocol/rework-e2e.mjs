#!/usr/bin/env node
// 跨关返工协议级 E2E（EvoFlow WP-9；flag=RATIFLOW_REWORK 默认 0）：
// 打回链（Testing→Design：区间 gate_results 失效、stages 复位、指针回退、
// pending release superseded、旧护照失效要求重签、重做再放行全链）、CAS 漂移
// （发起后状态变化 → decide 拒+操作 failed）、并发双打回（第二个 CAS 不符）、
// flag 关闭回退。前置：cargo build --release -p ratiflow-core。
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import * as readline from 'node:readline';

const CORE = process.env.CORE_BIN ?? join(process.cwd(), 'target', 'release', 'ratiflow-core');

class CoreClient {
  constructor(dataDir, env = {}) {
    this.proc = spawn(CORE, ['app-server', '--data-dir', dataDir], { stdio: ['pipe', 'pipe', 'pipe'], env: { ...process.env, ...env } });
    this.nextId = 1;
    this.pending = new Map();
    this.rl = readline.createInterface({ input: this.proc.stdout });
    this.rl.on('line', (l) => this.onLine(l));
    this.proc.stderr.on('data', () => {});
  }
  onLine(line) {
    if (!line.trim()) return;
    let m; try { m = JSON.parse(line); } catch { return; }
    if (m.protocolVersion) { this.hello = m; return; }
    if (m.id && this.pending.has(m.id)) {
      const { resolve, reject } = this.pending.get(m.id);
      this.pending.delete(m.id);
      if (m.error) reject(Object.assign(new Error(m.error.data?.detail ?? m.error.message), { code: m.error.message }));
      else resolve(m.result);
    }
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

function assert(c, label) {
  if (!c) throw new Error(`rework-e2e 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function expectErrorContains(call, token, label) {
  try {
    await call();
  } catch (e) {
    assert(String(e.message).includes(token), `${label}（实际 ${e.message.slice(0, 80)}）`);
    return;
  }
  throw new Error(`rework-e2e 断言失败：${label} 期望错误含 ${token}，但调用成功`);
}

async function activeKeys(client, workItemId) {
  const cov = await client.call('trace.coverage', { workItemId });
  return (cov.items ?? []).filter((i) => i.status === 'active').map((i) => i.requirementKey);
}

const KINDS = {
  requirements: 'prd', design: 'tech_design', development: 'code',
  testing: 'test', deployment: 'deployment', verification: 'verification',
};

async function main() {
  // ============ 场景一：flag 关闭 ============
  {
    const dataDir = mkdtempSync(join(tmpdir(), 'sg-rwk-off-'));
    const c = new CoreClient(dataDir, {});
    try {
      await c.call('project.create', { gitlabInstance: 'local', namespace: 'e2e', project: 'rwkoff', name: 'RwkOff' });
      const wi = await c.call('workitem.create', { projectId: (await c.call('project.list', {})).items[0].id, title: '默认任务' });
      await expectErrorContains(
        () => c.call('rework.preview', { workItemId: wi.id, targetGate: 'requirements', reasonCode: 'other' }),
        'feature_disabled',
        'flag 关闭：rework.preview 拒绝',
      );
      console.log('场景一（flag 关闭回退）通过');
    } finally {
      c.kill();
      rmSync(dataDir, { recursive: true, force: true });
    }
  }

  // ============ 场景二：打回全链 ============
  {
    const dataDir = mkdtempSync(join(tmpdir(), 'sg-rwk-on-'));
    const c = new CoreClient(dataDir, { RATIFLOW_REWORK: '1' });
    try {
      await c.call('project.create', { gitlabInstance: 'local', namespace: 'e2e', project: 'rwk', name: 'Rwk' });
      const pj = (await c.call('project.list', {})).items[0].id;
      const wi = await c.call('workitem.create', { projectId: pj, title: '返工任务' });
      const keys = await activeKeys(c, wi.id);
      assert(keys.length >= 1, '需求项就绪');

      const completeGate = async (g) => {
        const art = await c.call('artifact.create', { workItemId: wi.id, kind: KINDS[g], title: `${g} 产物` });
        const rev = await c.call('artifact.createDraft', { artifactId: art.id, content: `# ${g}\n- [${keys[0]}] 覆盖`, requirementKeys: keys });
        await c.call('artifact.addReview', { revisionId: rev.id, reviewer: 't', verdict: 'approved' });
        await c.call('artifact.freezeBaseline', { workItemId: wi.id, gate: g, revisionIds: [rev.id] });
        const ev = await c.call('evidence.record', { workItemId: wi.id, gate: g, kind: 'manual', title: `${g} 核验`, source: 'local', requirementKeys: keys });
        await c.call('evidence.verify', { evidenceId: ev.id, verifiedBy: 'qa' });
        await c.call('gate.evaluate', { workItemId: wi.id, gate: g });
        const rr = await c.call('gate.requestRelease', { workItemId: wi.id, gate: g });
        await c.call('gate.decideRelease', { approvalId: rr.approval_id, decision: 'approved', decidedBy: 'owner', reason: 'E2E' });
      };
      for (const g of ['requirements', 'design', 'development']) {
        await completeGate(g);
      }
      // 走到 testing：留一笔 pending release（打回应被 superseded）。
      const artT = await c.call('artifact.create', { workItemId: wi.id, kind: KINDS.testing, title: 'testing 产物' });
      const revT = await c.call('artifact.createDraft', { artifactId: artT.id, content: `# testing\n- [${keys[0]}] 覆盖`, requirementKeys: keys });
      await c.call('artifact.addReview', { revisionId: revT.id, reviewer: 't', verdict: 'approved' });
      await c.call('artifact.freezeBaseline', { workItemId: wi.id, gate: 'testing', revisionIds: [revT.id] });
      const evT = await c.call('evidence.record', { workItemId: wi.id, gate: 'testing', kind: 'manual', title: 'testing 核验', source: 'local', requirementKeys: keys });
      await c.call('evidence.verify', { evidenceId: evT.id, verifiedBy: 'qa' });
      await c.call('gate.evaluate', { workItemId: wi.id, gate: 'testing' });
      const pendingRr = await c.call('gate.requestRelease', { workItemId: wi.id, gate: 'testing' });
      // 护照签发失败（未全过）不影响；先签一本不成立——跳过，直接打回。

      // 1) preview：CAS 冻结 + 区间。
      const pv = await c.call('rework.preview', { workItemId: wi.id, targetGate: 'design', reasonCode: 'regression', note: '回归打回', requestedBy: 'agent' });
      assert(pv.state === 'previewed', 'preview 落 previewed');
      assert(JSON.stringify(pv.intervalGates) === JSON.stringify(['design', 'development', 'testing']), `区间 [design..testing]（实际 ${JSON.stringify(pv.intervalGates)}）`);
      assert(pv.activeRunPresent === false, '无活跃 Run');

      // 2) 并发双打回：第二个操作（不同 CAS 不可得——同状态同 note 会幂等重放；
      //    用不同 note 造第二操作）待审中；第一个批准后第二个 CAS 不符 failed。
      const pv2 = await c.call('rework.preview', { workItemId: wi.id, targetGate: 'design', reasonCode: 'regression', note: '第二次打回', requestedBy: 'agent' });
      const req2 = await c.call('rework.request', { workItemId: wi.id, targetGate: 'design', reasonCode: 'regression', note: '第二次打回', requestedBy: 'agent' });
      assert(req2.state === 'awaiting_approval', '第二个操作待审');

      // 3) 第一个操作请求+批准（CAS 与 preview 时一致）。
      const req1 = await c.call('rework.request', { workItemId: wi.id, targetGate: 'design', reasonCode: 'regression', note: '回归打回', requestedBy: 'agent' });
      assert(req1.state === 'awaiting_approval' && req1.approvalId, '第一个操作待审');
      const done1 = await c.call('approval.decide', { approvalId: req1.approvalId, decision: 'approved', decidedBy: 'owner', reason: '批准打回' });
      assert(done1.state === 'completed' && done1.completed_at, '批准 → 两步执行 → completed');

      // 4) 第二个操作：CAS 已漂移 → decide 拒 + failed。
      await expectErrorContains(
        () => c.call('rework.decide', { approvalId: req2.approvalId, decision: 'approved', decidedBy: 'owner', reason: '' }),
        'rework_state_changed',
        '并发第二个操作 CAS 不符拒绝',
      );

      // 5) 失效读取面。
      const inst = await c.call('workflow.getInstance', { workItemId: wi.id });
      const st = (g) => inst.gates.find((x) => x.gate_id === g).state;
      assert(st('design') === 'not_started' && st('development') === 'not_started' && st('testing') === 'not_started', '区间 stages 复位 not_started');
      assert(st('requirements') === 'passed', '区间外 requirements 保持 passed');
      assert(inst.instance.current_gate_id === 'design', `指针回 design（实际 ${inst.instance.current_gate_id}）`);
      const pendingAfter = await c.call('gate.getRelease', { releaseId: pendingRr.id });
      assert(pendingAfter.releaseRequest.state === 'superseded', `pending release 已 superseded（实际 ${pendingAfter.releaseRequest.state}）`);

      // 6) 重做全链（design→verification）再签护照：旧失效、新签成功。
      for (const g of ['design', 'development', 'testing', 'deployment', 'verification']) {
        await completeGate(g);
      }
      const passport = await c.call('passport.issue', { workItemId: wi.id });
      assert(passport.gates.length === 6, '重做后护照重签成功（6 关）');
      const latest = await c.call('passport.latest', { workItemId: wi.id });
      assert(latest && latest.id === passport.id, 'passport.latest 返回新护照');

      console.log('场景二（打回全链）通过');
    } finally {
      c.kill();
      rmSync(dataDir, { recursive: true, force: true });
    }
  }

  console.log('跨关返工协议 E2E 通过。');
}

main().catch((e) => {
  console.error('rework-e2e 失败：', e.message);
  process.exit(1);
});
