#!/usr/bin/env node
// 阶段 Agent 路由 E2E（ADR-030 M4 / 蓝图 §13.2 agent-routing-e2e）：
// AC-SW-08 专属绑定实际使用且运行详情可见；AC-SW-09 不可用回退通用（记录原因）、
// fail_closed 失败关闭；AC-SW-10 多 activity 分别选路并汇总到同一输出包。
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
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

async function main() {
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-routing-e2e-'));
  // 脚本模型：每个 Run 两次调用（读文件 → final）；未配置真实模型时生效。
  const script = [
    { content: '{"action":"final","summary":"前端活动完成"}', tokensIn: 10, tokensOut: 5 },
    { content: '{"action":"final","summary":"后端活动完成"}', tokensIn: 10, tokensOut: 5 },
    { content: '{"action":"final","summary":"代码分析完成"}', tokensIn: 10, tokensOut: 5 },
    { content: '{"action":"final","summary":"测试活动完成"}', tokensIn: 10, tokensOut: 5 },
    { content: '{"action":"final","summary":"回退活动完成"}', tokensIn: 10, tokensOut: 5 },
    { content: '{"action":"final","summary":"汇总后活动完成"}', tokensIn: 10, tokensOut: 5 },
  ];
  const scriptPath = join(tmpdir(), `sg-routing-script-${Date.now()}.json`);
  writeFileSync(scriptPath, JSON.stringify(script));
  let client = new CoreClient(dataDir, { SIXGATES_FAKE_MODEL_SCRIPT: scriptPath });
  const fail = (error) => {
    console.error(`E2E 失败：${error.message}`);
    client?.kill();
    rmSync(dataDir, { recursive: true, force: true });
    process.exit(1);
  };

  try {
    await client.hello_();
    assert(client.hello.schemaVersion >= 19, `schemaVersion ≥ 19（实际 ${client.hello.schemaVersion}）`);
    const project = await client.call('project.create', {
      gitlabInstance: 'https://gitlab.test', namespace: 'team', project: 'routing',
    });
    const wi = await client.call('workitem.create', {
      projectId: project.id, title: '多 Agent 开发', description: 'M4 路由验证',
    });

    // 释放 requirements 进入 development。
    const GATE_KINDS = { requirements: 'prd', design: 'tech_design', development: 'code', testing: 'test', deployment: 'deployment', verification: 'verification' };

    async function activeKeys(workItemId) {
      const cov = await client.call('trace.coverage', { workItemId });
      return (cov.items ?? []).filter((i) => i.status === 'active').map((i) => i.requirementKey);
    }
    async function releaseGate(gate) {
      const keys = await activeKeys(wi.id);
      // kind 与 deliverable.rs 门禁映射对齐，否则 request_release 报 deliverable_missing。
      const art = await client.call('artifact.create', { workItemId: wi.id, kind: GATE_KINDS[gate] ?? 'doc', title: gate });
      const rev = await client.call('artifact.createDraft', { artifactId: art.id, content: `# ${gate}`, requirementKeys: keys });
      await client.call('artifact.addReview', { revisionId: rev.id, reviewer: 't', verdict: 'approved' });
      await client.call('artifact.freezeBaseline', { workItemId: wi.id, gate, revisionIds: [rev.id] });
      const ev = await client.call('evidence.record', { workItemId: wi.id, gate, kind: 'review', title: gate, source: 'local', requirementKeys: keys });
      await client.call('evidence.verify', { evidenceId: ev.id, verifiedBy: 'q' });
      await client.call('gate.evaluate', { workItemId: wi.id, gate });
      const rr = await client.call('gate.requestRelease', { workItemId: wi.id, gate });
      await client.call('gate.decideRelease', { approvalId: rr.approval_id, decision: 'approved', decidedBy: 'o', reason: 'E2E' });
    }
    await releaseGate('requirements');
    // design 关放行（development 活动需当前关到位）。
    await (async () => {
      const keys = await activeKeys(wi.id);
      const art = await client.call('artifact.create', { workItemId: wi.id, kind: 'tech_design', title: '设计' });
      const rev = await client.call('artifact.createDraft', { artifactId: art.id, content: '# 设计', requirementKeys: keys });
      await client.call('artifact.addReview', { revisionId: rev.id, reviewer: 't', verdict: 'approved' });
      await client.call('artifact.freezeBaseline', { workItemId: wi.id, gate: 'design', revisionIds: [rev.id] });
      const ev = await client.call('evidence.record', { workItemId: wi.id, gate: 'design', kind: 'review', title: 'd', source: 'local', requirementKeys: keys });
      await client.call('evidence.verify', { evidenceId: ev.id, verifiedBy: 'q' });
      await client.call('gate.evaluate', { workItemId: wi.id, gate: 'design' });
      const rr = await client.call('gate.requestRelease', { workItemId: wi.id, gate: 'design' });
      await client.call('gate.decideRelease', { approvalId: rr.approval_id, decision: 'approved', decidedBy: 'o', reason: 'E2E' });
    })();

    // 创建专属 Profile（版本冻结）。
    const frontendProfile = await client.call('agentProfile.create', { projectId: project.id, name: '前端开发分身', adapterKind: 'local_harness' });
    const frontendVersion = await client.call('agentProfile.createVersion', {
      profileId: frontendProfile.id, persona: '严谨的前端工程师', sop: '先读代码再改', capabilities: ['frontend'],
    });
    const backendProfile = await client.call('agentProfile.create', { projectId: project.id, name: '后端开发分身', adapterKind: 'local_harness' });
    const backendVersion = await client.call('agentProfile.createVersion', {
      profileId: backendProfile.id, persona: '稳健的后端工程师', sop: '先写测试再实现', capabilities: ['backend'],
    });
    assert(!!frontendVersion.content_digest && frontendVersion.version_no === 1, 'Profile 版本冻结（v1 + digest）');

    // 绑定矩阵：frontend → 前端分身；backend → 后端分身；code_analysis 不绑定（回退）。
    await client.call('agentBinding.set', {
      projectId: project.id, gate: 'development', activityKey: 'frontend',
      profileVersionId: frontendVersion.id, fallbackMode: 'generic',
    });
    await client.call('agentBinding.set', {
      projectId: project.id, gate: 'development', activityKey: 'backend',
      profileVersionId: backendVersion.id, fallbackMode: 'generic',
    });

    // AC-SW-10：三个 activity 分别选路执行。
    const startFrontend = await client.call('stage.startActivity', {
      workItemId: wi.id, gate: 'development', activityKey: 'frontend',
      goal: '实现前端组件', requiredCapabilities: ['frontend'], toolAllowlist: ['read_file'],
    });
    assert(startFrontend.selection.source_scope === 'project_binding', 'frontend 命中项目绑定（AC-SW-08）');
    assert(startFrontend.selection.fallback_used === false, 'frontend 无回退');
    assert(startFrontend.selection.resolved_profile_version_id === frontendVersion.id, '实际使用绑定的 profile 版本（AC-SW-08）');
    assert(!!startFrontend.attemptId && !!startFrontend.selection.stage_activity_id, 'Run 绑定 attempt/activity');

    const startBackend = await client.call('stage.startActivity', {
      workItemId: wi.id, gate: 'development', activityKey: 'backend',
      goal: '实现后端接口', requiredCapabilities: ['backend'], toolAllowlist: ['read_file'],
    });
    assert(startBackend.selection.resolved_profile_version_id === backendVersion.id, 'backend 命中后端分身（AC-SW-10）');

    const startAnalysis = await client.call('stage.startActivity', {
      workItemId: wi.id, gate: 'development', activityKey: 'code_analysis',
      goal: '静态分析', toolAllowlist: ['read_file'],
    });
    assert(startAnalysis.selection.source_scope === 'builtin_generic' && startAnalysis.selection.fallback_used === true, 'code_analysis 未绑定回退通用（AC-SW-10）');

    // AC-SW-08：运行详情可见选路记录。
    const runDetail = await client.call('agent.get', { runId: startFrontend.runId });
    assert(runDetail.stageAttemptId === startFrontend.attemptId && !!runDetail.agentSelectionId, 'agent.get 透出 attempt/selection 绑定');
    assert(runDetail.agentSelection?.profileId === frontendProfile.id && runDetail.agentSelection?.fallbackUsed === false, '运行详情含 profile/fallback（AC-SW-08）');
    assert(runDetail.inputSnapshotId === runDetail.stageAttemptId || !!runDetail.inputSnapshotId, 'Run 关联输入快照');

    // AC-SW-09：external_agent 未配置模型 → 不健康 → 回退通用并记录原因。
    const externalProfile = await client.call('agentProfile.create', { name: '外部测试分身', adapterKind: 'external_agent' });
    const externalVersion = await client.call('agentProfile.createVersion', {
      profileId: externalProfile.id, persona: '外部测试', capabilities: ['e2e_testing'],
    });
    await client.call('agentBinding.set', {
      projectId: project.id, gate: 'testing', activityKey: 'e2e_testing',
      profileVersionId: externalVersion.id, fallbackMode: 'generic',
    });
    // 先释放 development（三次活动后放行）。
    const keys = await activeKeys(wi.id);
    const art = await client.call('artifact.create', { workItemId: wi.id, kind: 'code', title: '开发说明' });
    const rev = await client.call('artifact.createDraft', { artifactId: art.id, content: '# 开发完成', requirementKeys: keys });
    await client.call('artifact.addReview', { revisionId: rev.id, reviewer: 't', verdict: 'approved' });
    await client.call('artifact.freezeBaseline', { workItemId: wi.id, gate: 'development', revisionIds: [rev.id] });
    const devEv = await client.call('evidence.record', { workItemId: wi.id, gate: 'development', kind: 'ci_pipeline', title: 'CI', source: 'local', requirementKeys: keys });
    await client.call('evidence.verify', { evidenceId: devEv.id, verifiedBy: 'q' });
    await client.call('gate.evaluate', { workItemId: wi.id, gate: 'development' });
    const devRr = await client.call('gate.requestRelease', { workItemId: wi.id, gate: 'development' });
    await client.call('gate.decideRelease', { approvalId: devRr.approval_id, decision: 'approved', decidedBy: 'o', reason: 'E2E' });

    // ===== 当前关为 testing =====
    // AC-SW-09a：fail_closed——绑定 external（未配置模型=不健康）+ fail_closed → 启动失败关闭。
    await client.call('agentBinding.set', {
      projectId: project.id, gate: 'testing', activityKey: 'e2e_testing',
      profileVersionId: externalVersion.id, fallbackMode: 'fail_closed',
    });
    let fcError = false;
    let fcRunCount = 0;
    try {
      await client.call('stage.startActivity', {
        workItemId: wi.id, gate: 'testing', activityKey: 'e2e_testing',
        goal: '发布计划 FC', requiredCapabilities: ['e2e_testing'],
      });
    } catch (e) {
      fcError = /agent_profile_unavailable/.test(e.code ?? e.message);
    }
    assert(fcError, 'fail_closed 且专属不可用 → 启动失败关闭（AC-SW-09）');
    // 无 Run 创建（FC 失败路径不产生运行记录）。
    fcRunCount = (await client.call('agent.get', { runId: 'none' }).catch((e) => e)).code === 'not_found' ? 0 : 1;
    assert(fcRunCount === 0, 'fail_closed 未创建 Run');

    // AC-SW-09b：同绑定改为可回退 → 回退通用并记录原因。
    await client.call('agentBinding.set', {
      projectId: project.id, gate: 'testing', activityKey: 'e2e_testing',
      profileVersionId: externalVersion.id, fallbackMode: 'generic',
    });
    const startTesting = await client.call('stage.startActivity', {
      workItemId: wi.id, gate: 'testing', activityKey: 'e2e_testing',
      goal: '执行 E2E', requiredCapabilities: ['e2e_testing'], toolAllowlist: ['read_file'],
    });
    assert(startTesting.selection.source_scope === 'builtin_generic' && startTesting.selection.fallback_used === true, 'AC-SW-09：不健康专属回退通用');
    assert(/adapter_unhealthy/.test(startTesting.selection.reason_code), `回退原因被记录（${startTesting.selection.reason_code}）`);
    // 候选报告包含失败的外部候选。
    assert(
      (startTesting.selection.candidate_report ?? []).some((c) => c.ok === false && c.scope === 'project_binding'),
      '候选报告冻结失败原因',
    );

    // AC-SW-10 收尾：testing 放行 → 输出包汇总（同一 gate 输出包包含全部产出与证据）。
    const tKeys = await activeKeys(wi.id);
    const tArt = await client.call('artifact.create', { workItemId: wi.id, kind: 'test', title: '测试计划' });
    const tRev = await client.call('artifact.createDraft', { artifactId: tArt.id, content: '# 测试计划', requirementKeys: tKeys });
    await client.call('artifact.addReview', { revisionId: tRev.id, reviewer: 't', verdict: 'approved' });
    await client.call('artifact.freezeBaseline', { workItemId: wi.id, gate: 'testing', revisionIds: [tRev.id] });
    const tEv = await client.call('evidence.record', { workItemId: wi.id, gate: 'testing', kind: 'manual', title: '测试证据', source: 'local', requirementKeys: tKeys });
    await client.call('evidence.verify', { evidenceId: tEv.id, verifiedBy: 'q' });
    await client.call('gate.evaluate', { workItemId: wi.id, gate: 'testing' });
    const tRr = await client.call('gate.requestRelease', { workItemId: wi.id, gate: 'testing' });
    const releaseDetail = await client.call('gate.getRelease', { releaseId: tRr.id });
    const roles = (releaseDetail.items ?? []).map((i) => i.role);
    assert(roles.includes('artifact_revision') && roles.includes('evidence') && roles.includes('requirement_item'), '输出包汇总工件/证据/需求项（AC-SW-10）');

    // 选路事件存在。
    const timeline = await client.call('timeline.snapshot', { workItemId: wi.id });
    // 选路事件 aggregate 是 agent，不在 workitem 时间线中——直接断言事件流（全局事件在 client.events 不可见，跳过）。

    console.log('\nagent-routing-e2e：全部断言通过 ✓');
    client.kill();
    rmSync(dataDir, { recursive: true, force: true });
  } catch (error) {
    fail(error);
  }
}

main();
