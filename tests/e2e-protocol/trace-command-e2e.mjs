#!/usr/bin/env node
// Trace / 驾驶舱 / Slash 协议 E2E（EvoFlow 方案 M5-08 / ADR-039）：
// durable spans 从事实重建调用链；taskReadModel 只读真实状态（无估算假值）；
// checkpoint 断线恢复；usage cache/cost 未知显示 unknown（EV-016）；
// Slash preview→execute 命中同一放行链（EV-018 不旁路审批），token 不一致拒绝。
// 前置：cargo build --release -p sixgates-core。
import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import * as readline from 'node:readline';

const CORE = process.env.CORE_BIN ?? join(process.cwd(), 'target', 'release', 'sixgates-core');

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
  if (!c) throw new Error(`trace-command-e2e 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function expectErrorContains(call, token, label) {
  try {
    await call();
  } catch (e) {
    assert(String(e.message).includes(token), `${label}（实际 ${String(e.message).slice(0, 90)}）`);
    return;
  }
  throw new Error(`trace-command-e2e 断言失败：${label} 期望错误含 ${token}，但调用成功`);
}

function git(dir, ...args) {
  const out = spawnSync('git', ['-C', dir, ...args], { encoding: 'utf8' });
  if (out.status !== 0) throw new Error(`git ${args[0]} 失败: ${out.stderr}`);
  return out.stdout.trim();
}

const W = (key, deps = []) => ({
  taskKey: key, kind: 'local_write', title: key,
  expectedOutputs: [`file:${key}.out`], acceptance: { machine: [], manual: [] },
  effectClass: 'local_write', deps,
});

async function main() {
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-tcmd-e2e-'));
  const c = new CoreClient(dataDir, { SIXGATES_PLAN_DAG: '1' });
  try {
    const repoDir = join(tmpdir(), `sg-tcmd-repo-${Date.now()}`);
    import('node:fs').then((fs) => {
      fs.mkdirSync(repoDir, { recursive: true });
    });
    // 同步建仓（import 顶层不可用）。
    const fs = await import('node:fs');
    fs.mkdirSync(repoDir, { recursive: true });
    git(repoDir, 'init', '-b', 'main');
    git(repoDir, 'config', 'user.email', 'e2e@sixgates.local');
    git(repoDir, 'config', 'user.name', 'e2e');
    fs.writeFileSync(join(repoDir, 'README.md'), 'hello\n');
    git(repoDir, 'add', '.');
    git(repoDir, 'commit', '-m', 'init');

    const proj = await c.call('project.create', {
      gitlabInstance: 'https://gitlab.test', namespace: 'team', project: 'tc', localRoot: repoDir,
    });
    const wi = await c.call('workitem.create', { projectId: proj.id, title: 'Trace 任务', description: 'desc' });
    // 需求关产物/证据/评估（放行链前置）。
    const cov = await c.call('trace.coverage', { workItemId: wi.id });
    const keys = (cov.items ?? []).filter((i) => i.status === 'active').map((i) => i.requirementKey);
    const art = await c.call('artifact.create', { workItemId: wi.id, kind: 'prd', title: 'PRD' });
    const rev = await c.call('artifact.createDraft', { artifactId: art.id, content: `# PRD\n- [${keys[0]}] x`, requirementKeys: keys });
    await c.call('artifact.addReview', { revisionId: rev.id, reviewer: 't', verdict: 'approved' });
    await c.call('artifact.freezeBaseline', { workItemId: wi.id, gate: 'requirements', revisionIds: [rev.id] });
    const ev = await c.call('evidence.record', { workItemId: wi.id, gate: 'requirements', kind: 'manual', title: 'e', source: 'local', requirementKeys: keys });
    await c.call('evidence.verify', { evidenceId: ev.id, verifiedBy: 'q' });
    const evaluated = await c.call('gate.evaluate', { workItemId: wi.id, gate: 'requirements' });
    assert(evaluated.passed === true, '需求关评估通过');

    // 计划执行产生 task 事实（w1 → v）。
    const plan = await c.call('plan.createDraft', {
      workItemId: wi.id, idempotencyKey: 'tc-1',
      tasks: [W('w1'), W('v', ['w1'])],
    });
    await c.call('plan.submit', { planRevisionId: plan.planRevisionId });
    await c.call('plan.decide', { planRevisionId: plan.planRevisionId, decision: 'approved', decidedBy: 'owner', reason: 'E2E' });
    await c.call('plan.start', { planRevisionId: plan.planRevisionId, idempotencyKey: 'tc-s1' });
    const disp = await c.call('plan.dispatchReady', { planRevisionId: plan.planRevisionId, maxParallel: 3 });
    await c.call('planTask.prepare', { taskAttemptId: disp.dispatched[0].id });
    await c.call('plan.startRunning', { taskAttemptId: disp.dispatched[0].id });
    await c.call('planTask.transition', { taskAttemptId: disp.dispatched[0].id, outcome: 'succeeded', outputDigest: 'od', idempotencyKey: 'tc-t1' });

    // --- 1. trace.taskReadModel：真实状态 + checkpoint ---
    const rm = await c.call('trace.taskReadModel', { workItemId: wi.id });
    assert(rm.source === 'durable_facts', 'read model 来自 durable facts');
    const w1t = rm.model.tasks.find((t) => t.task_key === 'w1');
    assert(!!w1t && w1t.phase === 'done' && w1t.state === 'succeeded', 'w1 真实完成态');
    const vt = rm.model.tasks.find((t) => t.task_key === 'v');
    assert(!!vt && vt.phase === 'waiting', 'v 排队位真实等待态（advance 已提升）');
    assert(typeof rm.factsSha256 === 'string' && rm.factsSha256.length === 64, 'checkpoint sha');
    // 断线恢复：restoreCheckpoint。
    const restored = await c.call('trace.restoreCheckpoint', { workItemId: wi.id });
    assert(restored.restored === true && restored.model.tasks.length === 2, '断线后 checkpoint 重建');
    // 进度无估算假值：tasks 只含已建 attempt 的任务（v 未派发不出现）。
    assert(rm.model.tasks.every((t) => !t.phase.startsWith('est')), '无估算进度值');

    // --- 2. trace.graph / usage 诚实语义 ---
    const usage = await c.call('trace.usage', { workItemId: wi.id });
    assert(usage.cost === 'unknown', '成本恒 unknown（价格表未接入，EV-016）');
    assert(usage.cacheRead === 'unknown', 'cache 无数据 → unknown（不填 0）');

    // --- 3. Slash：preview → token 校验 → execute 命中同一放行链（EV-018）---
    const pv = await c.call('command.preview', { workItemId: wi.id, text: '/放行' });
    assert(pv.intent === '放行' && pv.requiresApproval === true, '放行 preview 标注需审批');
    assert(pv.gate === 'requirements', 'preview 目标为当前关');
    // token 不一致拒绝。
    await expectErrorContains(
      () => c.call('command.execute', { workItemId: wi.id, text: '/放行', previewToken: 'bad-token' }),
      'command_token_mismatch',
      'token 不一致拒绝',
    );
    // execute → 走 gate.requestRelease（审批链），不直接推进关卡。
    const executed = await c.call('command.execute', {
      workItemId: wi.id, text: '/放行', previewToken: pv.previewToken,
    });
    assert(executed.executed === 'gate.requestRelease', 'execute 命中 requestRelease');
    const wiNow = await c.call('workitem.progress', { workItemId: wi.id });
    assert(wiNow.currentGate === 'requirements', '放行请求后 current_gate 不变（审批链未被旁路）');
    const approvals = await c.call('approval.list', {});
    const relApproval = approvals.items.find((a) => a.subject_type === 'gate_release');
    assert(!!relApproval, '放行审批出现在审批中心（同链证据）');
    await c.call('gate.decideRelease', { approvalId: relApproval.id, decision: 'approved', decidedBy: 'owner', reason: 'E2E' });
    const wiAfter = await c.call('workitem.progress', { workItemId: wi.id });
    assert(wiAfter.currentGate === 'design', '人工批准后推进 design');

    // /打回：design 关打回路径（无 pending 审批 → 明确缺失）。
    await expectErrorContains(
      async () => {
        const p2 = await c.call('command.preview', { workItemId: wi.id, text: '/打回 design' });
        await c.call('command.execute', { workItemId: wi.id, text: '/打回 design', previewToken: p2.previewToken });
      },
      'command_target_missing',
      '/打回 无待决审批 → 明确缺失',
    );

    console.log('Trace/Slash 协议 E2E 通过。');
  } finally {
    c.kill();
    rmSync(dataDir, { recursive: true, force: true });
  }
}

main().catch((e) => {
  console.error('trace-command-e2e 失败：', e.message);
  process.exit(1);
});
