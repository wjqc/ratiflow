#!/usr/bin/env node
// 跳关协议级 E2E（EvoFlow WP-8 skip 半边；fast-track 半边随其流程落地追加）：
// 部署/迁移类关 skip 策略创建即拒、forbidden 关拒绝跳过、替代证据必填且须在案、
// 幂等重放返回既有审批、批准 → 阶段 skipped + 指针推进、护照 outcome=
// skipped_with_waiver 且 passed:false、flag 关闭拒绝新建（回退读法不退化）。
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
  if (!c) throw new Error(`gate-skip-e2e 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function expectErrorContains(call, token, label) {
  try {
    await call();
  } catch (e) {
    assert(String(e.message).includes(token), `${label}（实际 ${e.message.slice(0, 80)}）`);
    return;
  }
  throw new Error(`gate-skip-e2e 断言失败：${label} 期望错误含 ${token}，但调用成功`);
}

async function activeKeys(client, workItemId) {
  const cov = await client.call('trace.coverage', { workItemId });
  return (cov.items ?? []).filter((i) => i.status === 'active').map((i) => i.requirementKey);
}

const GATES = [
  { gateId: 'triage', title: '分诊关', purpose: '', deliverables: ['doc'] },
  {
    gateId: 'review', title: '评审关', purpose: '', deliverables: ['doc'],
    skipPolicy: { mode: 'manual_approval' },
  },
  { gateId: 'build', title: '构建关', purpose: '', deliverables: ['code'] },
];

async function main() {
  // ============ 场景一：flag 关闭（默认 0）============
  {
    const dataDir = mkdtempSync(join(tmpdir(), 'sg-skip-off-'));
    const c = new CoreClient(dataDir, { RATIFLOW_WORKFLOW_TEMPLATE_V2: '1' });
    try {
      await c.call('project.create', { gitlabInstance: 'local', namespace: 'e2e', project: 'skipoff', name: 'SkipOff' });
      const pj = (await c.call('project.list', {})).items[0].id;
      const wi = await c.call('workitem.create', { projectId: pj, title: '默认任务' });
      await expectErrorContains(
        () => c.call('gate.requestSkip', { workItemId: wi.id, gateId: 'design', waiver: 'x', substituteEvidenceIds: ['e1'] }),
        'feature_disabled',
        'flag 关闭：requestSkip 拒绝',
      );
      console.log('场景一（flag 关闭回退）通过');
    } finally {
      c.kill();
      rmSync(dataDir, { recursive: true, force: true });
    }
  }

  // ============ 场景二：skip 全链（flag 开启）============
  {
    const dataDir = mkdtempSync(join(tmpdir(), 'sg-skip-on-'));
    const c = new CoreClient(dataDir, {
      RATIFLOW_WORKFLOW_TEMPLATE_V2: '1',
      RATIFLOW_GATE_SKIP: '1',
    });
    try {
      await c.call('project.create', { gitlabInstance: 'local', namespace: 'e2e', project: 'skip', name: 'Skip' });
      const pj = (await c.call('project.list', {})).items[0].id;

      // 1) 部署/迁移类关恒 forbidden：manual_approval 声明创建即拒。
      await expectErrorContains(
        () => c.call('workflowTemplate.create', {
          key: 'skip-deploy', name: '部署跳关',
          gates: [{ gateId: 'deploy', title: '部署关', deliverables: ['deployment'], skipPolicy: { mode: 'manual_approval' } }],
          idempotencyKey: 'skip-deploy-1',
        }),
        '恒 forbidden',
        '部署类关 manual_approval 创建即拒',
      );

      // 2) 合法模板：review 关声明 manual_approval，其余缺省 forbidden。
      const created = await c.call('workflowTemplate.create', { key: 'skippable', name: '可跳关模板', gates: GATES, idempotencyKey: 'skip-1' });
      await c.call('workflowTemplate.activate', { versionId: created.version.id, idempotencyKey: 'skip-2' });
      const wi = await c.call('workitem.create', { projectId: pj, title: '跳关任务', templateId: 'skippable' });

      // 3) forbidden 关（triage 无声明）拒绝。
      await expectErrorContains(
        () => c.call('gate.requestSkip', { workItemId: wi.id, gateId: 'triage', waiver: 'w', substituteEvidenceIds: ['e1'] }),
        'gate_skip_forbidden',
        'forbidden 关拒绝跳过',
      );

      // 4) review 关：替代证据必填且须在案。
      await expectErrorContains(
        () => c.call('gate.requestSkip', { workItemId: wi.id, gateId: 'review', waiver: 'w', substituteEvidenceIds: [] }),
        'substitute_evidence_missing',
        '替代证据为空拒绝',
      );
      await expectErrorContains(
        () => c.call('gate.requestSkip', { workItemId: wi.id, gateId: 'review', waiver: 'w', substituteEvidenceIds: ['ev_ghost'] }),
        'substitute_evidence_missing',
        '不存在证据拒绝',
      );
      const keys = await activeKeys(c, wi.id);
      const subst = await c.call('evidence.record', { workItemId: wi.id, gate: 'review', kind: 'manual', title: '替代核验记录', source: 'local', requirementKeys: keys });

      // 5) 完成 triage（正常评估+放行）→ 推进至 review（当前关、未开工）。
      const completeGate = async (g, kind) => {
        const art = await c.call('artifact.create', { workItemId: wi.id, kind, title: `${g} 产物` });
        const rev = await c.call('artifact.createDraft', { artifactId: art.id, content: `# ${g}\n- [${keys[0]}] 覆盖`, requirementKeys: keys });
        await c.call('artifact.addReview', { revisionId: rev.id, reviewer: 't', verdict: 'approved' });
        await c.call('artifact.freezeBaseline', { workItemId: wi.id, gate: g, revisionIds: [rev.id] });
        const ev = await c.call('evidence.record', { workItemId: wi.id, gate: g, kind: 'manual', title: `${g} 核验`, source: 'local', requirementKeys: keys });
        await c.call('evidence.verify', { evidenceId: ev.id, verifiedBy: 'qa' });
        await c.call('gate.evaluate', { workItemId: wi.id, gate: g });
        const rr = await c.call('gate.requestRelease', { workItemId: wi.id, gate: g });
        await c.call('gate.decideRelease', { approvalId: rr.approval_id, decision: 'approved', decidedBy: 'owner', reason: 'E2E' });
      };
      await completeGate('triage', 'doc');

      // 6) 请求跳关 → 审批挂起；同参重放幂等返回既有审批。
      const req = await c.call('gate.requestSkip', { workItemId: wi.id, gateId: 'review', waiver: '评审会线下完成', substituteEvidenceIds: [subst.id] });
      assert(req.state === 'requested', '跳关审批挂起');
      const replay = await c.call('gate.requestSkip', { workItemId: wi.id, gateId: 'review', waiver: '评审会线下完成', substituteEvidenceIds: [subst.id] });
      assert(replay.idempotentReplay === true && replay.approvalId === req.approvalId, '同 digest 重放幂等返回既有审批');

      // 6) 批准 → 阶段 skipped、指针推进到 build。
      const decided = await c.call('approval.decide', { approvalId: req.approvalId, decision: 'approved', decidedBy: 'owner', reason: '批准跳过' });
      assert(decided.skip && decided.skip.outcome === 'skipped_with_waiver', '批准 → skipped_with_waiver');
      const inst = await c.call('workflow.getInstance', { workItemId: wi.id });
      const reviewGate = inst.gates.find((g) => g.gate_id === 'review');
      assert(reviewGate.state === 'skipped', `评审关阶段为 skipped（实际 ${reviewGate.state}）`);
      assert(inst.instance.current_gate_id === 'build', `指针推进至 build（实际 ${inst.instance.current_gate_id}）`);

      // 7) 已 skipped 关重复请求 → 状态已变（非幂等重放路径）。
      await expectErrorContains(
        () => c.call('gate.requestSkip', { workItemId: wi.id, gateId: 'review', waiver: '换个理由再跳', substituteEvidenceIds: [subst.id] }),
        'gate_skip_state_changed',
        '已跳过关再请求 → 状态已变',
      );

      // 8) 完成 build（正常评估+放行）→ 护照签发。
      await completeGate('build', 'code');
      const passport = await c.call('passport.issue', { workItemId: wi.id });
      const pg = passport.gates.find((x) => x.gate === 'review');
      assert(pg.passed === false && pg.outcome === 'skipped_with_waiver', `护照 skipped 关：passed:false + outcome（实际 ${JSON.stringify(pg)}）`);
      assert(pg.waiver_approval_id === req.approvalId, '护照携带豁免审批 id');
      const triage = passport.gates.find((x) => x.gate === 'triage');
      assert(triage.passed === true && triage.outcome === 'passed', '正常关 outcome=passed');

      console.log('场景二（skip 全链）通过');
    } finally {
      c.kill();
      rmSync(dataDir, { recursive: true, force: true });
    }
  }

  // ============ 场景三：fast-track 全链（flag 开启，独立工作项）============
  {
    const dataDir = mkdtempSync(join(tmpdir(), 'sg-ft-on-'));
    const c = new CoreClient(dataDir, {
      RATIFLOW_WORKFLOW_TEMPLATE_V2: '1',
      RATIFLOW_GATE_SKIP: '1',
      RATIFLOW_AUTOMATIONS: '1',
    });
    try {
      await c.call('project.create', { gitlabInstance: 'local', namespace: 'e2e', project: 'ft', name: 'FT' });
      const pj = (await c.call('project.list', {})).items[0].id;
      const FT_GATES = [
        {
          gateId: 'build', title: '构建关', purpose: '', deliverables: ['code'],
          fastTrackPolicy: {
            skippable_activities: ['execution'],
            waived_deliverables: [{ kind: 'code', substitute_evidence_kind: 'manual' }],
            reduced_approval: true,
          },
        },
      ];
      const created = await c.call('workflowTemplate.create', { key: 'ft-tpl', name: '快通道模板', gates: FT_GATES, idempotencyKey: 'ft-1' });
      await c.call('workflowTemplate.activate', { versionId: created.version.id, idempotencyKey: 'ft-2' });
      const wi = await c.call('workitem.create', { projectId: pj, title: '快通道任务', templateId: 'ft-tpl' });

      // 1) 六因素非全真 → 拒（不产生建议）。
      const FACTORS = {
        no_protected_path: true, api_schema_unchanged: true, effect_class_read_only: true,
        provenance_complete: true, test_evidence_present: true,
      };
      await expectErrorContains(
        () => c.call('gate.evaluateFastTrack', { workItemId: wi.id, gate: 'build', factors: { ...FACTORS, api_schema_unchanged: false } }),
        'fast_track_factors_not_all_true',
        '六因素非全真拒绝',
      );
      // 2) 全真 → 建议落 shadow；重放幂等返回同一建议。
      const s1 = await c.call('gate.evaluateFastTrack', { workItemId: wi.id, gate: 'build', factors: FACTORS });
      assert(s1.source === 'fast_track' && s1.id, '建议落 shadow（source=fast_track）');
      const s2 = await c.call('gate.evaluateFastTrack', { workItemId: wi.id, gate: 'build', factors: FACTORS });
      assert(s2.id === s1.id, '同因素重放幂等返回同一建议');
      const obs = await c.call('automation.observations', { source: 'fast_track' });
      assert(obs.items.some((x) => x.id === s1.id), '观察面可见 fast_track 建议');

      // 3) 未采纳：交付物豁免不生效（code kind 照常要求工件冻结）。
      const st0 = await c.call('gate.deliverableStatus', { workItemId: wi.id, gate: 'build' });
      assert(st0.satisfied === false && st0.entries[0].waived === undefined, '未采纳：无豁免');

      // 4) 采纳 → 应用缩减；交付物豁免生效但替代证据强制。
      const decided = await c.call('automation.decideSuggestion', { suggestionId: s1.id, decision: 'accepted', decidedBy: 'owner', note: '采纳快通道' });
      assert(decided.fastTrack && decided.fastTrack.applied === true, `采纳后应用缩减（实际 ${JSON.stringify(decided.fastTrack)}）`);
      const st1 = await c.call('gate.deliverableStatus', { workItemId: wi.id, gate: 'build' });
      assert(st1.entries[0].waived === true && st1.entries[0].satisfied === false, '豁免生效但替代证据缺失 → 不满足');
      assert(st1.entries[0].missing === 'substitute_evidence_missing', '缺失指明 substitute_evidence_missing');
      // 替代证据补齐（verified）→ 满足，且无需创建 code 工件。
      const keys = await activeKeys(c, wi.id);
      const subst = await c.call('evidence.record', { workItemId: wi.id, gate: 'build', kind: 'manual', title: '替代核验', source: 'local', requirementKeys: keys });
      await c.call('evidence.verify', { evidenceId: subst.id, verifiedBy: 'qa' });
      const st2 = await c.call('gate.deliverableStatus', { workItemId: wi.id, gate: 'build' });
      assert(st2.satisfied === true && st2.entries[0].waived === true, '替代证据在案 → 豁免满足');

      // 5) Policy 审批不被豁免：放行仍须走 gate_release 审批链（快通道不自动放行）。
      const keys2 = await activeKeys(c, wi.id);
      const art = await c.call('artifact.create', { workItemId: wi.id, kind: 'code', title: '构建产物' });
      const rev = await c.call('artifact.createDraft', {
        artifactId: art.id,
        content: `# build\n- [${keys2[0]}] 覆盖`, requirementKeys: keys2,
      });
      await c.call('artifact.addReview', { revisionId: rev.id, reviewer: 't', verdict: 'approved' });
      await c.call('artifact.freezeBaseline', { workItemId: wi.id, gate: 'build', revisionIds: [rev.id] });
      const ev = await c.call('evidence.record', { workItemId: wi.id, gate: 'build', kind: 'test_report', title: '测试报告', source: 'local', requirementKeys: keys2 });
      await c.call('evidence.verify', { evidenceId: ev.id, verifiedBy: 'qa' });
      await c.call('gate.evaluate', { workItemId: wi.id, gate: 'build' });
      const rr = await c.call('gate.requestRelease', { workItemId: wi.id, gate: 'build' });
      assert(rr.approval_id, '放行审批照常创建（审批不被豁免）');
      await c.call('gate.decideRelease', { approvalId: rr.approval_id, decision: 'approved', decidedBy: 'owner', reason: 'E2E' });

      console.log('场景三（fast-track 全链）通过');
    } finally {
      c.kill();
      rmSync(dataDir, { recursive: true, force: true });
    }
  }

  console.log('跳关协议 E2E 通过。');
}

main().catch((e) => {
  console.error('gate-skip-e2e 失败：', e.message);
  process.exit(1);
});
