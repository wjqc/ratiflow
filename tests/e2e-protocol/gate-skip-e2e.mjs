#!/usr/bin/env node
// 跳关协议级 E2E（EvoFlow WP-8 skip 半边；fast-track 半边随其流程落地追加）：
// 部署/迁移类关 skip 策略创建即拒、forbidden 关拒绝跳过、替代证据必填且须在案、
// 幂等重放返回既有审批、批准 → 阶段 skipped + 指针推进、护照 outcome=
// skipped_with_waiver 且 passed:false、flag 关闭拒绝新建（回退读法不退化）。
// 前置：cargo build --release -p ratiflow-core。
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import * as readline from 'node:readline';

const CORE = process.env.CORE_BIN ?? join(process.cwd(), 'target', 'release', 'ratiflow-core');

// P0-1：requestSkip/evaluateFastTrack/decideSuggestion 均为 receipt 门控 mutation。
// 重放断言分两层：同 key = transport receipt 重放（返回首次响应）；异 key 同参 =
// 领域 digest 幂等（idempotentReplay）。场景里按需选 key。
let keySeq = 0;
const idem = (prefix) => `${prefix}-${++keySeq}`;

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
      // P0-1：入口门控先于 feature flag——缺 key 一律 idempotency_key_required。
      await expectErrorContains(
        () => c.call('gate.requestSkip', { workItemId: wi.id, gateId: 'design', waiver: 'x', substituteEvidenceIds: ['e1'] }),
        '必须携带 idempotencyKey',
        '缺 idempotencyKey：先于 flag 拒绝',
      );
      await expectErrorContains(
        () => c.call('gate.requestSkip', { workItemId: wi.id, gateId: 'design', waiver: 'x', substituteEvidenceIds: ['e1'], idempotencyKey: idem('skipoff') }),
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
        () => c.call('gate.requestSkip', { workItemId: wi.id, gateId: 'triage', waiver: 'w', substituteEvidenceIds: ['e1'], idempotencyKey: idem('forbid') }),
        'gate_skip_forbidden',
        'forbidden 关拒绝跳过',
      );

      // 4) review 关：替代证据必填且须在案。
      await expectErrorContains(
        () => c.call('gate.requestSkip', { workItemId: wi.id, gateId: 'review', waiver: 'w', substituteEvidenceIds: [], idempotencyKey: idem('evempty') }),
        'substitute_evidence_missing',
        '替代证据为空拒绝',
      );
      await expectErrorContains(
        () => c.call('gate.requestSkip', { workItemId: wi.id, gateId: 'review', waiver: 'w', substituteEvidenceIds: ['ev_ghost'], idempotencyKey: idem('evghost') }),
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

      // 6) 请求跳关 → operation 行 + 审批挂起；异 key 同参重放走领域 digest 幂等
      //    返回既有请求（P0-3：gate_skip_requests 权威，含 skipRequestId）。
      const req = await c.call('gate.requestSkip', { workItemId: wi.id, gateId: 'review', waiver: '评审会线下完成', substituteEvidenceIds: [subst.id], idempotencyKey: idem('skipreq') });
      assert(req.state === 'requested' && req.skipRequestId && req.approvalId, `跳关请求挂起（实际 ${JSON.stringify(req)}）`);
      const replay = await c.call('gate.requestSkip', { workItemId: wi.id, gateId: 'review', waiver: '评审会线下完成', substituteEvidenceIds: [subst.id], idempotencyKey: idem('skipreq2') });
      assert(replay.skipRequestId === req.skipRequestId && replay.approvalId === req.approvalId, '同 digest 重放幂等返回既有请求');

      // 6) 批准（gate.decideSkip 唯一决定入口；两步执行完成）→ 阶段 skipped、
      //    指针推进到 build、单一下一关 prepared attempt；resume 幂等。
      const decided = await c.call('gate.decideSkip', { approvalId: req.approvalId, decision: 'approved', decidedBy: 'owner', reason: '批准跳过', idempotencyKey: idem('skipdec') });
      assert(decided.state === 'completed' && decided.progress === 'next_attempt_ready', `批准 → 两步执行完成（实际 ${JSON.stringify(decided)}）`);
      assert(decided.nextAttemptId, '产生下一关 attempt');
      const resumed = await c.call('gate.resumeSkip', { skipRequestId: req.skipRequestId, idempotencyKey: idem('skipres') });
      assert(resumed.state === 'completed' && resumed.nextAttemptId === decided.nextAttemptId, 'resume 幂等（不重复创建 attempt）');
      const inst = await c.call('workflow.getInstance', { workItemId: wi.id });
      const reviewGate = inst.gates.find((g) => g.gate_id === 'review');
      assert(reviewGate.state === 'skipped', `评审关阶段为 skipped（实际 ${reviewGate.state}）`);
      assert(inst.instance.current_gate_id === 'build', `指针推进至 build（实际 ${inst.instance.current_gate_id}）`);

      // 7) 已 skipped 关重复请求 → 状态已变（非幂等重放路径）。
      await expectErrorContains(
        () => c.call('gate.requestSkip', { workItemId: wi.id, gateId: 'review', waiver: '换个理由再跳', substituteEvidenceIds: [subst.id], idempotencyKey: idem('skipagain') }),
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

  // ============ 场景三：fast-track 服务端事实链（P0-2；flag 开启）============
  {
    // 脚本模型：read_file 工具调用 → final（wi 运行）；final（wi2 运行）。
    const scriptPath = join(tmpdir(), `sg-ft-script-${Date.now()}.json`);
    writeFileSync(scriptPath, JSON.stringify([
      { content: '{"action":"read_file","arguments":{"path":"README.md"},"summary":"读取参考"}', tokensIn: 5, tokensOut: 5 },
      { content: '{"action":"final","summary":"已读取相关文件"}', tokensIn: 5, tokensOut: 5 },
      { content: '{"action":"read_file","arguments":{"path":"docs/spec.md"},"summary":"第二工作项读取"}', tokensIn: 5, tokensOut: 5 },
      { content: '{"action":"final","summary":"第二工作项运行完成"}', tokensIn: 5, tokensOut: 5 },
    ]));
    const dataDir = mkdtempSync(join(tmpdir(), 'sg-ft-on-'));
    const c = new CoreClient(dataDir, {
      RATIFLOW_WORKFLOW_TEMPLATE_V2: '1',
      RATIFLOW_GATE_SKIP: '1',
      RATIFLOW_AUTOMATIONS: '1',
      RATIFLOW_FAKE_MODEL_SCRIPT: scriptPath,
    });
    const waitForRunTerminal = async (runId) => {
      for (let i = 0; i < 200; i++) {
        const run = await c.call('agent.get', { runId });
        if (['completed_execution', 'failed', 'cancelled'].includes(run.status)) return run;
        await new Promise((r) => setTimeout(r, 100));
      }
      throw new Error(`run ${runId} 等待终态超时`);
    };
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

      // 1) P0-2：客户端 factors 一律拒绝（伪造面关闭，先于事实判定）。
      await expectErrorContains(
        () => c.call('gate.evaluateFastTrack', {
          workItemId: wi.id, gate: 'build',
          factors: { no_protected_path: true, api_schema_unchanged: true, effect_class_read_only: true, reversibility_confirmed: true, provenance_complete: true, test_evidence_present: true },
          idempotencyKey: idem('ftforged'),
        }),
        'fast_track_client_factors_rejected',
        '携带客户端 factors 拒绝',
      );
      // 2) expectedStateDigest 漂移 → 冲突（状态 CAS）。
      await expectErrorContains(
        () => c.call('gate.evaluateFastTrack', { workItemId: wi.id, gate: 'build', expectedStateDigest: 'sha256:garbage', idempotencyKey: idem('ftcas') }),
        'fast_track_state_changed',
        'expectedStateDigest 漂移冲突',
      );
      // 3) 无服务端事实 → 六因素非全真拒绝（错误信息携带证据引用）。
      await expectErrorContains(
        () => c.call('gate.evaluateFastTrack', { workItemId: wi.id, gate: 'build', idempotencyKey: idem('ftbare') }),
        'fast_track_factors_not_all_true',
        '无事实：六因素非全真拒绝',
      );

      // 4) 构造服务端事实：attempt 内只读工具 run + verified test_report 证据
      //    （evidence.record 同时建立谱系节点 → provenance complete）。
      const started = await c.call('stage.startActivity', {
        workItemId: wi.id, gate: 'build', goal: '阅读相关文件', toolAllowlist: ['read_file'],
        idempotencyKey: idem('ftrun'),
      });
      const run = await waitForRunTerminal(started.runId);
      assert(run.status === 'completed_execution', `只读 run 终态完成（实际 ${run.status}）`);
      const keys = await activeKeys(c, wi.id);
      const tev = await c.call('evidence.record', { workItemId: wi.id, gate: 'build', kind: 'test_report', title: '测试报告', source: 'local', requirementKeys: keys });
      await c.call('evidence.verify', { evidenceId: tev.id, verifiedBy: 'qa' });

      // 5) 全真 → 建议落 shadow（六因素服务端派生全 true）；异 key 同状态重放
      //    走领域 digest 幂等返回同一建议。
      const s1 = await c.call('gate.evaluateFastTrack', { workItemId: wi.id, gate: 'build', idempotencyKey: idem('ft1') });
      assert(s1.source === 'fast_track' && s1.id, '建议落 shadow（source=fast_track）');
      const factors = s1.content?.factors ?? s1.factors;
      assert(factors && Object.values(factors).every((v) => v === true), `六因素服务端派生全真（实际 ${JSON.stringify(factors)}）`);
      assert(typeof s1.input_state_digest === 'string' && s1.input_state_digest.length > 0, '建议冻结 input_state_digest');
      const s2 = await c.call('gate.evaluateFastTrack', { workItemId: wi.id, gate: 'build', idempotencyKey: idem('ft2') });
      assert(s2.id === s1.id, '同输入状态重放幂等返回同一建议');
      const obs = await c.call('automation.observations', { source: 'fast_track' });
      assert(obs.items.some((x) => x.id === s1.id), '观察面可见 fast_track 建议');

      // 6) 输入漂移使旧建议过期：requirement 新修订（attempt 建立后）→
      //    api_schema_unchanged=false → 拒 + s1 过期（不可再采纳）。
      await c.call('requirement.importRevision', { workItemId: wi.id, filename: 'spec-v2.md', content: '# 需求 v2\n- [R1] 更新', sourceKind: 'inline', createdBy: 'owner' });
      await expectErrorContains(
        () => c.call('gate.evaluateFastTrack', { workItemId: wi.id, gate: 'build', idempotencyKey: idem('ftdrift') }),
        'fast_track_factors_not_all_true',
        'schema 漂移后拒绝',
      );
      const obs2 = await c.call('automation.observations', { source: 'fast_track' });
      const s1After = obs2.items.find((x) => x.id === s1.id);
      assert(s1After?.decision?.decision === 'expired', `漂移使旧建议过期（实际 ${JSON.stringify(s1After?.decision)}）`);
      await expectErrorContains(
        () => c.call('automation.decideSuggestion', { suggestionId: s1.id, decision: 'accepted', decidedBy: 'owner', note: '', idempotencyKey: idem('ftexpired') }),
        '不可改判',
        '过期建议不可采纳',
      );

      // 7) 第二工作项（干净状态）走完采纳链：应用缩减 + 豁免 + Policy 审批不豁免。
      const wi2 = await c.call('workitem.create', { projectId: pj, title: '快通道任务二', templateId: 'ft-tpl' });
      const started2 = await c.call('stage.startActivity', {
        workItemId: wi2.id, gate: 'build', goal: '阅读相关文件', toolAllowlist: ['read_file'],
        idempotencyKey: idem('ftrun2'),
      });
      const run2 = await waitForRunTerminal(started2.runId);
      assert(run2.status === 'completed_execution', `wi2 只读 run 完成（实际 ${run2.status}）`);
      const keys2 = await activeKeys(c, wi2.id);
      const tev2 = await c.call('evidence.record', { workItemId: wi2.id, gate: 'build', kind: 'test_report', title: '测试报告', source: 'local', requirementKeys: keys2 });
      await c.call('evidence.verify', { evidenceId: tev2.id, verifiedBy: 'qa' });

      const st0 = await c.call('gate.deliverableStatus', { workItemId: wi2.id, gate: 'build' });
      assert(st0.satisfied === false && st0.entries[0].waived === undefined, '未采纳：无豁免');

      const s3 = await c.call('gate.evaluateFastTrack', { workItemId: wi2.id, gate: 'build', idempotencyKey: idem('ft3') });
      // P0-3 两动作分离：采纳建议不自动应用——决定后仍无豁免。
      const decided = await c.call('automation.decideSuggestion', { suggestionId: s3.id, decision: 'accepted', decidedBy: 'owner', note: '采纳快通道', idempotencyKey: idem('ftdecide') });
      assert(decided.fastTrack === undefined, '采纳不自动应用豁免（无 fastTrack 字段）');
      let st1 = await c.call('gate.deliverableStatus', { workItemId: wi2.id, gate: 'build' });
      assert(st1.entries[0].waived === undefined, '仅采纳：豁免面仍空（须显式 applyWaiver）');

      // 替代证据（manual，匹配策略 substitute_evidence_kind）→ applyWaiver 显式应用。
      const subst = await c.call('evidence.record', { workItemId: wi2.id, gate: 'build', kind: 'manual', title: '替代核验', source: 'local', requirementKeys: keys2 });
      await c.call('evidence.verify', { evidenceId: subst.id, verifiedBy: 'qa' });
      const w1 = await c.call('gate.applyWaiver', {
        workItemId: wi2.id, gate: 'build', waivedKind: 'code', substituteEvidenceId: subst.id,
        rationale: '线下核验替代', suggestionId: s3.id, decidedBy: 'owner', idempotencyKey: idem('w1'),
      });
      assert(w1.status === 'active' && w1.waiverId, `豁免应用（实际 ${JSON.stringify(w1)}）`);
      const w1r = await c.call('gate.applyWaiver', {
        workItemId: wi2.id, gate: 'build', waivedKind: 'code', substituteEvidenceId: subst.id,
        rationale: '线下核验替代', suggestionId: s3.id, decidedBy: 'owner', idempotencyKey: idem('w1r'),
      });
      assert(w1r.idempotentReplay === true && w1r.waiverId === w1.waiverId, '同参豁免幂等');
      st1 = await c.call('gate.deliverableStatus', { workItemId: wi2.id, gate: 'build' });
      assert(st1.entries[0].waived === true && st1.entries[0].satisfied === true, '豁免生效（替代证据在案）');

      // revokeWaiver：pending 放行失效 + 豁免面清空 + 幂等（安全收紧通道）。
      // 放行前置：工件冻结 + 评估通过（豁免只免工件要求时评估链照常）。
      const art = await c.call('artifact.create', { workItemId: wi2.id, kind: 'code', title: '构建产物' });
      const rev = await c.call('artifact.createDraft', {
        artifactId: art.id,
        content: `# build\n- [${keys2[0]}] 覆盖`, requirementKeys: keys2,
      });
      await c.call('artifact.addReview', { revisionId: rev.id, reviewer: 't', verdict: 'approved' });
      await c.call('artifact.freezeBaseline', { workItemId: wi2.id, gate: 'build', revisionIds: [rev.id] });
      await c.call('gate.evaluate', { workItemId: wi2.id, gate: 'build' });
      const rr = await c.call('gate.requestRelease', { workItemId: wi2.id, gate: 'build' });
      assert(rr.approval_id, '放行审批照常创建（审批不被豁免）');
      const revoked = await c.call('gate.revokeWaiver', { waiverId: w1.waiverId, reason: '复核不通过', revokedBy: 'qa', idempotencyKey: idem('rw1') });
      assert(revoked.status === 'revoked', `豁免撤销（实际 ${JSON.stringify(revoked)}）`);
      const revoked2 = await c.call('gate.revokeWaiver', { waiverId: w1.waiverId, reason: '再撤', revokedBy: 'qa', idempotencyKey: idem('rw2') });
      assert(revoked2.idempotentReplay === true, '撤销幂等');
      const rrAfter = await c.call('gate.getRelease', { releaseId: rr.id });
      assert(rrAfter.releaseRequest.state === 'superseded', `撤销使 pending 放行失效（实际 ${rrAfter.releaseRequest.state}）`);
      const stR = await c.call('gate.deliverableStatus', { workItemId: wi2.id, gate: 'build' });
      assert(stR.entries[0].waived === undefined, '撤销后豁免面清空');

      // Policy 审批不被豁免：撤销后重新放行仍须走 gate_release 审批链。
      const rr2 = await c.call('gate.requestRelease', { workItemId: wi2.id, gate: 'build' });
      assert(rr2.approval_id, '重新放行审批照常创建');
      await c.call('gate.decideRelease', { approvalId: rr2.approval_id, decision: 'approved', decidedBy: 'owner', reason: 'E2E' });

      console.log('场景三（fast-track 服务端事实链）通过');
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
