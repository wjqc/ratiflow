#!/usr/bin/env node
// 结构化验收协议级 E2E（EvoFlow WP-7 / RDWS v1.4 B7，关 P2-11）：
// 双形态模板创建校验（未知 verifier 即拒）、digest v3 冻结、实例读模型携带结构化项、
// 结构化项驱动 gate.evaluate（acceptance:* failed_inputs；自由文本不判定）、
// manual_confirm 人工确认链（请求→审批→confirmed→放行解阻；replay 幂等；rejected 不放行）、
// kill switch（=0 结构化模板拒绝创建，自由文本不受影响）。
// 前置：cargo build --release -p ratiflow-core。
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
  if (!c) throw new Error(`gate-acceptance-e2e 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function expectErrorContains(call, token, label) {
  try {
    await call();
  } catch (e) {
    assert(String(e.message).includes(token), `${label}（实际 ${e.message.slice(0, 80)}）`);
    return;
  }
  throw new Error(`gate-acceptance-e2e 断言失败：${label} 期望错误含 ${token}，但调用成功`);
}

async function activeKeys(client, workItemId) {
  const cov = await client.call('trace.coverage', { workItemId });
  return (cov.items ?? []).filter((i) => i.status === 'active').map((i) => i.requirementKey);
}

const MANUAL_ELEMENT = {
  verifier: 'manual_confirm',
  confirmation_subject: 'gate_manual_confirm',
  confirm_role: 'user',
};
const EVIDENCE_ELEMENT = {
  verifier: 'evidence_verified',
  evidence_kind: 'test_report',
  min_count: 1,
};
const ACCEPTANCE_DEFS = (structured) => [
  {
    gateId: 'confirm',
    title: '确认关',
    purpose: '验收确认',
    deliverables: ['verification'],
    acceptance: structured
      ? ['自由文本：验收说明（仅展示，不判定）', EVIDENCE_ELEMENT, MANUAL_ELEMENT]
      : ['自由文本：验收说明（仅展示，不判定）'],
  },
];

async function main() {
  // ============ 场景一：kill switch 关闭（=0）============
  {
    const dataDir = mkdtempSync(join(tmpdir(), 'sg-acc-off-'));
    const c = new CoreClient(dataDir, {
      RATIFLOW_WORKFLOW_TEMPLATE_V2: '1',
      RATIFLOW_STRUCTURED_ACCEPTANCE: '0',
    });
    try {
      await c.call('project.create', { gitlabInstance: 'local', namespace: 'e2e', project: 'accoff', name: 'AccOff' });
      await expectErrorContains(
        () => c.call('workflowTemplate.create', { key: 'acc-off', name: '结构化关闭', gates: ACCEPTANCE_DEFS(true), idempotencyKey: 'acc-off-1' }),
        'feature_disabled',
        'kill switch=0：结构化模板创建拒绝',
      );
      // 自由文本模板不受影响（回退：字符串形态照常）。
      const ok = await c.call('workflowTemplate.create', { key: 'acc-off-plain', name: '纯文本', gates: ACCEPTANCE_DEFS(false), idempotencyKey: 'acc-off-2' });
      await c.call('workflowTemplate.activate', { versionId: ok.version.id, idempotencyKey: 'acc-off-3' });
      assert(ok.version.status === 'draft', 'kill switch=0：自由文本模板可创建');
      console.log('场景一（kill switch 回退）通过');
    } finally {
      c.kill();
      rmSync(dataDir, { recursive: true, force: true });
    }
  }

  // ============ 场景二：结构化全链（flag 开启）============
  {
    const dataDir = mkdtempSync(join(tmpdir(), 'sg-acc-on-'));
    const c = new CoreClient(dataDir, {
      RATIFLOW_WORKFLOW_TEMPLATE_V2: '1',
      RATIFLOW_STRUCTURED_ACCEPTANCE: '1',
    });
    try {
      await c.call('project.create', { gitlabInstance: 'local', namespace: 'e2e', project: 'acc', name: 'Acc' });
      const pj = (await c.call('project.list', {})).items[0].id;

      // 1) schema 校验在创建即拒：未知 verifier / 越界参数。
      await expectErrorContains(
        () => c.call('workflowTemplate.create', {
          key: 'acc-bad-1', name: '坏 verifier',
          gates: [{ gateId: 'g', title: 'g', deliverables: ['doc'], acceptance: [{ verifier: 'magic' }] }],
          idempotencyKey: 'acc-bad-1',
        }),
        'acceptance_schema_invalid',
        '未知 verifier 创建即拒',
      );
      await expectErrorContains(
        () => c.call('workflowTemplate.create', {
          key: 'acc-bad-2', name: '坏参数',
          gates: [{ gateId: 'g', title: 'g', deliverables: ['doc'], acceptance: [{ ...EVIDENCE_ELEMENT, min_count: 0 }] }],
          idempotencyKey: 'acc-bad-2',
        }),
        'acceptance_schema_invalid',
        'min_count 越界创建即拒',
      );

      // 2) 合法结构化模板：draft → activate → 实例读模型携带双形态。
      const created = await c.call('workflowTemplate.create', { key: 'acc-demo', name: '结构化验收', gates: ACCEPTANCE_DEFS(true), idempotencyKey: 'acc-1' });
      const activated = await c.call('workflowTemplate.activate', { versionId: created.version.id, idempotencyKey: 'acc-2' });
      assert(activated.status === 'active', '结构化模板激活');
      const wi = await c.call('workitem.create', { projectId: pj, title: '结构化验收任务', templateId: 'acc-demo' });
      const inst = await c.call('workflow.getInstance', { workItemId: wi.id });
      const acc = inst.gates[0].acceptance;
      assert(acc.length === 3, '实例读模型携带 3 条 acceptance');
      assert(typeof acc[0] === 'string', '字符串元素原样透传（仅展示）');
      assert(acc[1].verifier === 'evidence_verified' && acc[2].verifier === 'manual_confirm', '结构化元素对象透传');

      // 3) 六输入未备：evaluate 失败且 failed_inputs 含结构化 token；自由文本不产生 token。
      let ev = await c.call('gate.evaluate', { workItemId: wi.id, gate: 'confirm' });
      assert(ev.passed === false, '六输入未备 → 不通过');
      assert(ev.failed_inputs.some((t) => t.startsWith('acceptance:evidence_verified')), 'evidence_verified 结构化项记 failed_inputs');
      assert(ev.failed_inputs.some((t) => t.startsWith('acceptance:manual_confirm')), 'manual_confirm 结构化项记 failed_inputs');
      assert(!ev.failed_inputs.some((t) => t.includes('自由文本')), '自由文本不进判定');

      // 4) 备齐六输入（基线冻结 + 已核验证据）：仅剩 manual_confirm 解阻。
      const keys = await activeKeys(c, wi.id);
      const art = await c.call('artifact.create', { workItemId: wi.id, kind: 'verification', title: '验收产物' });
      const rev = await c.call('artifact.createDraft', { artifactId: art.id, content: `# 验收\n- [${keys[0]}] 覆盖`, requirementKeys: keys });
      await c.call('artifact.addReview', { revisionId: rev.id, reviewer: 't', verdict: 'approved' });
      await c.call('artifact.freezeBaseline', { workItemId: wi.id, gate: 'confirm', revisionIds: [rev.id] });
      const evid = await c.call('evidence.record', { workItemId: wi.id, gate: 'confirm', kind: 'test_report', title: '测试报告', source: 'local', requirementKeys: keys });
      await c.call('evidence.verify', { evidenceId: evid.id, verifiedBy: 'qa' });
      ev = await c.call('gate.evaluate', { workItemId: wi.id, gate: 'confirm' });
      assert(ev.passed === false, 'manual_confirm 未确认 → 整体不通过');
      assert(JSON.stringify(ev.failed_inputs) === JSON.stringify(['acceptance:manual_confirm(manual)']), `仅剩 manual_confirm 失败（实际 ${JSON.stringify(ev.failed_inputs)}）`);
      await expectErrorContains(
        () => c.call('gate.requestRelease', { workItemId: wi.id, gate: 'confirm' }),
        'gate_release_required',
        '结构化未过 → 放行拒绝',
      );

      // 5) manual_confirm 链：请求（审批挂起）→ 决定 confirmed → evaluate 解阻。
      const req1 = await c.call('gate.requestManualConfirmation', { workItemId: wi.id, gate: 'confirm', element: MANUAL_ELEMENT, requestedBy: 'agent', reason: '请人工复核', idempotencyKey: 'mc-1' });
      assert(req1.state === 'requested' && req1.approval_status === 'requested', '确认单+审批挂起');
      // P0-1：异 key 同 attempt 同元素 → 领域幂等返回既有确认单（同 key 则为 transport 重放）。
      const replay = await c.call('gate.requestManualConfirmation', { workItemId: wi.id, gate: 'confirm', element: MANUAL_ELEMENT, requestedBy: 'agent', reason: '', idempotencyKey: 'mc-2' });
      assert(replay.id === req1.id, '同 attempt 同元素请求幂等（返回既有确认单）');
      ev = await c.call('gate.evaluate', { workItemId: wi.id, gate: 'confirm' });
      assert(ev.passed === false, '仅请求未决定 → 仍不通过');
      const decided = await c.call('approval.decide', { approvalId: req1.approval_id, decision: 'approved', decidedBy: 'owner', reason: '人工复核通过' });
      assert(decided.confirmation.state === 'confirmed', '审批通过 → 确认事实落态');
      ev = await c.call('gate.evaluate', { workItemId: wi.id, gate: 'confirm' });
      assert(ev.passed === true && ev.failed_inputs.length === 0, '全部验收满足 → evaluate 通过');
      const list = await c.call('gate.manualConfirmations', { workItemId: wi.id });
      assert(list.items.length === 1 && list.items[0].state === 'confirmed', '确认单列表可查');
      await expectErrorContains(
        () => c.call('approval.decide', { approvalId: req1.approval_id, decision: 'rejected', decidedBy: 'owner', reason: '' }),
        'not in requested state',
        '确认单不可改判',
      );

      // 6) 拒绝路径：第二个工作项 rejected → manual_confirm 仍 fail。
      const wi2 = await c.call('workitem.create', { projectId: pj, title: '拒绝路径任务', templateId: 'acc-demo' });
      const req2 = await c.call('gate.requestManualConfirmation', { workItemId: wi2.id, gate: 'confirm', element: MANUAL_ELEMENT, requestedBy: 'agent', reason: '', idempotencyKey: 'mc-3' });
      await c.call('approval.decide', { approvalId: req2.approval_id, decision: 'rejected', decidedBy: 'owner', reason: '不同意' });
      const ev2 = await c.call('gate.evaluate', { workItemId: wi2.id, gate: 'confirm' });
      assert(ev2.passed === false && ev2.failed_inputs.some((t) => t.startsWith('acceptance:manual_confirm')), 'rejected 不放行');

      console.log('场景二（结构化全链）通过');
    } finally {
      c.kill();
      rmSync(dataDir, { recursive: true, force: true });
    }
  }

  console.log('结构化验收协议 E2E 通过。');
}

main().catch((e) => {
  console.error('gate-acceptance-e2e 失败：', e.message);
  process.exit(1);
});
