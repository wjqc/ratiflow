#!/usr/bin/env node
// 局部重规划协议级 E2E（EvoFlow 方案 M3-08 / ADR-037 §6.4 / M3 退出标准）：
// 失败任务只使下游闭包重做；无关成功任务复用（reused_from_attempt_id 证明）；
// unknown 不自动重跑、对账后才可安全重试；effect 升级 → 计划升级重批；
// 免重批替换直接 approved；调度容量/串行档背压。
// 前置：cargo build --release -p sixgates-core。
import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, mkdirSync, writeFileSync } from 'node:fs';
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
  if (!c) throw new Error(`partial-replan-e2e 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function expectErrorContains(call, token, label) {
  try {
    await call();
  } catch (e) {
    assert(String(e.message).includes(token), `${label}（实际 ${String(e.message).slice(0, 90)}）`);
    return;
  }
  throw new Error(`partial-replan-e2e 断言失败：${label} 期望错误含 ${token}，但调用成功`);
}

function git(dir, ...args) {
  const out = spawnSync('git', ['-C', dir, ...args], { encoding: 'utf8' });
  if (out.status !== 0) throw new Error(`git ${args[0]} 失败: ${out.stderr}`);
  return out.stdout.trim();
}

const W = (key, deps = [], effect = 'local_write') => ({
  taskKey: key, kind: effect === 'read' ? 'analysis' : 'local_write', title: key,
  expectedOutputs: [`file:${key}.out`], acceptance: { machine: [], manual: [] },
  effectClass: effect, deps,
});

async function main() {
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-replan-e2e-'));
  const c = new CoreClient(dataDir, { SIXGATES_PLAN_DAG: '1' });
  try {
    const repoDir = join(tmpdir(), `sg-replan-repo-${Date.now()}`);
    mkdirSync(repoDir, { recursive: true });
    git(repoDir, 'init', '-b', 'main');
    git(repoDir, 'config', 'user.email', 'e2e@sixgates.local');
    git(repoDir, 'config', 'user.name', 'e2e');
    writeFileSync(join(repoDir, 'README.md'), 'hello\n');
    git(repoDir, 'add', '.');
    git(repoDir, 'commit', '-m', 'init');

    const proj = await c.call('project.create', {
      gitlabInstance: 'https://gitlab.test', namespace: 'team', project: 'rp', localRoot: repoDir,
    });
    const wi = await c.call('workitem.create', { projectId: proj.id, title: '重规划任务', description: 'desc' });
    // 需求关评估通过（活跃 attempt 就绪）。
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

    // 计划 v1：w1、w2 独立写 + v 汇合。
    const v1 = await c.call('plan.createDraft', {
      workItemId: wi.id, idempotencyKey: 'rp-1',
      tasks: [W('w1'), W('w2'), W('v', ['w1', 'w2'])],
    });
    await c.call('plan.submit', { planRevisionId: v1.planRevisionId });
    await c.call('plan.decide', { planRevisionId: v1.planRevisionId, decision: 'approved', decidedBy: 'owner', reason: 'E2E' });
    const started = await c.call('plan.start', { planRevisionId: v1.planRevisionId, idempotencyKey: 'rp-s1' });
    assert(started.readyAttempts.length === 2, 'v1 start：w1/w2 排队位就绪');
    // 调度闸：pending → ready。
    const disp1 = await c.call('plan.dispatchReady', { planRevisionId: v1.planRevisionId, maxParallel: 3 });
    assert(disp1.dispatched.length === 2, '调度闸放行两个写任务');
    const byKey = Object.fromEntries(disp1.dispatched.map((a) => [a.task_key, a]));

    // w1 成功（工作区准备→运行→回填），w2 失败。
    await c.call('planTask.prepare', { taskAttemptId: byKey.w1.id });
    await c.call('plan.startRunning', { taskAttemptId: byKey.w1.id });
    await c.call('planTask.transition', { taskAttemptId: byKey.w1.id, outcome: 'succeeded', outputDigest: 'od-w1', idempotencyKey: 't-w1' });
    await c.call('planTask.prepare', { taskAttemptId: byKey.w2.id });
    await c.call('plan.startRunning', { taskAttemptId: byKey.w2.id });
    await c.call('planTask.transition', { taskAttemptId: byKey.w2.id, outcome: 'failed', idempotencyKey: 't-w2' });

    // 重规划预览：root=w2 → 闭包 {w2, v}。
    const preview = await c.call('plan.replanPreview', { planRevisionId: v1.planRevisionId, roots: ['w2'] });
    assert(preview.closure.includes('w2') && preview.closure.includes('v'), `闭包含 w2/v（实际 ${JSON.stringify(preview.closure)}）`);
    assert(!preview.closure.includes('w1'), 'w1 不在闭包（可复用）');

    // 重规划：w2 同档替换，v 保留 → 免重批直接 approved。
    // replacements 仅闭包内任务：w2 同档替换，v 保留（闭包外 w1 自动携带）。
    const rp = await c.call('plan.replan', {
      planRevisionId: v1.planRevisionId, roots: ['w2'],
      tasks: [W('w2'), W('v', ['w1', 'w2'])],
      idempotencyKey: 'rp-2',
    });
    assert(rp.requires_reapproval === false, '同档替换免重批');
    assert(rp.revision.status === 'approved', '免重批直接 approved');
    assert(rp.reused.w1 && rp.reused.w1.length > 0, 'w1 复用有 attempt 证明（EV-010）');
    const v2Id = rp.revision.id;

    // v2 start：仅 w2 重做（w1 复用不建新 attempt）。
    const s2 = await c.call('plan.start', { planRevisionId: v2Id, idempotencyKey: 'rp-s2' });
    assert(s2.readyAttempts.length === 1 && s2.readyAttempts[0].task_key === 'w2', '仅 w2 重做');
    await c.call('plan.dispatchReady', { planRevisionId: v2Id, maxParallel: 3 });
    // w2 修复成功 → v 排队位出现。
    await c.call('planTask.prepare', { taskAttemptId: s2.readyAttempts[0].id });
    await c.call('plan.startRunning', { taskAttemptId: s2.readyAttempts[0].id });
    await c.call('planTask.transition', { taskAttemptId: s2.readyAttempts[0].id, outcome: 'succeeded', outputDigest: 'od-w2r', idempotencyKey: 't-w2r' });
    const vAttempts = await c.call('plan.dispatchReady', { planRevisionId: v2Id, maxParallel: 3 });
    assert(vAttempts.dispatched.length === 1 && vAttempts.dispatched[0].task_key === 'v', 'v 提升派发');
    await c.call('planTask.transition', { taskAttemptId: vAttempts.dispatched[0].id, outcome: 'succeeded', idempotencyKey: 't-v' });

    // unknown 路径：新计划 w1(external_write 升级) 重批 + unknown 先对账。
    const rp2 = await c.call('plan.replan', {
      planRevisionId: v2Id, roots: ['v'],
      tasks: [W('v', ['w1', 'w2'], 'external_write')],
      idempotencyKey: 'rp-3',
    });
    assert(rp2.requires_reapproval === true, 'effect 升级 → 必须重新审批');
    assert(rp2.revision.status === 'draft', '升级计划停留 draft');
    await c.call('plan.submit', { planRevisionId: rp2.revision.id });
    await c.call('plan.decide', { planRevisionId: rp2.revision.id, decision: 'approved', decidedBy: 'owner', reason: 'E2E 升级' });
    const s3 = await c.call('plan.start', { planRevisionId: rp2.revision.id, idempotencyKey: 'rp-s3' });
    // 升级只重做受影响任务：w1（升级）重做，w2（定义未变、已成功）继续复用不重跑。
    assert(s3.readyAttempts.length === 1 && s3.readyAttempts[0].task_key === 'w1', 'v3 仅升级任务 w1 重做（w2 复用不重跑）');
    const disp3 = await c.call('plan.dispatchReady', { planRevisionId: rp2.revision.id, maxParallel: 3 });
    assert(disp3.dispatched.length === 1, 'v3 调度闸放行 w1');
    const w1v3 = s3.readyAttempts.find((a) => a.task_key === 'w1');
    await c.call('planTask.prepare', { taskAttemptId: w1v3.id });
    await c.call('plan.startRunning', { taskAttemptId: w1v3.id });
    await c.call('planTask.transition', { taskAttemptId: w1v3.id, outcome: 'unknown', idempotencyKey: 't-unk' });
    // unknown 后：调度无新派发（不自动重跑）。
    const afterUnknown = await c.call('plan.dispatchReady', { planRevisionId: rp2.revision.id, maxParallel: 3 });
    assert(afterUnknown.dispatched.every((a) => a.task_key !== 'w1'), 'unknown 不自动重跑（M3 退出标准）');
    // 对账：查证未执行 → 新 attempt（attempt_no=2）。
    const reconciled = await c.call('planTask.reconcile', { taskAttemptId: w1v3.id, resolution: 'not_executed' });
    assert(reconciled.attempt_no === 2, `对账后安全重试 attempt_no=2（实际 ${reconciled.attempt_no}）`);
    // 直接对账非 unknown attempt 拒绝。
    await expectErrorContains(
      () => c.call('planTask.reconcile', { taskAttemptId: reconciled.id, resolution: 'executed_ok' }),
      'task_reconciliation_invalid',
      '非 unknown attempt 对账拒绝',
    );

    console.log('局部重规划协议 E2E 通过。');
  } finally {
    c.kill();
    rmSync(dataDir, { recursive: true, force: true });
  }
}

main().catch((e) => {
  console.error('partial-replan-e2e 失败：', e.message);
  process.exit(1);
});
