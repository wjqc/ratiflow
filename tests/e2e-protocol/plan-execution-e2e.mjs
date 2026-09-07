#!/usr/bin/env node
// 计划任务真实执行链 E2E（EvoFlow 评审 P0-2/P0-3 修复的验收面）：
// dispatchReady → TaskWorkspace.prepare → agent.start(planTaskAttemptId) 绑定
// → 模型驱动 write_file 落到任务工作区 → Run 终态 completed_execution
// → planTask.transition(succeeded) 仅凭绑定 Run 终态证明放行。
// 反向：无绑定 Run 自报 succeeded 被拒（task_execution_proof_required）；
// 非法 autonomyGrantId 在 agent.start 即拒绝。
// 前置：cargo build --release -p ratiflow-core。
import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, mkdirSync, writeFileSync, existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import * as readline from 'node:readline';
import { createHash } from 'node:crypto';

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
  if (!c) throw new Error(`plan-execution-e2e 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function expectErrorContains(call, token, label) {
  try {
    await call();
  } catch (e) {
    assert(String(e.message).includes(token), `${label}（实际 ${String(e.message).slice(0, 90)}）`);
    return;
  }
  throw new Error(`plan-execution-e2e 断言失败：${label} 期望错误含 ${token}，但调用成功`);
}

function git(dir, ...args) {
  const out = spawnSync('git', ['-C', dir, ...args], { encoding: 'utf8' });
  if (out.status !== 0) throw new Error(`git ${args[0]} 失败: ${out.stderr}`);
  return out.stdout.trim();
}

async function waitTerminal(c, runId, capMs = 30000) {
  const deadline = Date.now() + capMs;
  for (;;) {
    const run = await c.call('agent.get', { runId });
    if (['completed_execution', 'failed', 'cancelled'].includes(run.status)) return run;
    if (Date.now() > deadline) throw new Error(`等待 Run 终态超时：${run.status}`);
    await new Promise((r) => setTimeout(r, 100));
  }
}

async function main() {
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-plan-exec-'));
  // 模型脚本：每个 Run 消费两条/三条（读任务工作区标记 → final）；顺序消费 = Run 串行驱动。
  // 标记文件只存在于对应 TaskWorkspace（测试在 prepare 后写入）——读到内容即证明
  // Agent 工作目录 = TaskWorkspace（评审 P0-3 接线），其他目录均无此文件。
  const fakeScript = dataDir + '-fake.json';
  const readAction = (file) => JSON.stringify({ action: 'read_file', arguments: { path: file }, summary: `读 ${file}` });
  writeFileSync(fakeScript, JSON.stringify([
    { content: readAction('only-in-task-ws-1.txt'), tokensIn: 3, tokensOut: 2 },
    { content: JSON.stringify({ action: 'final', summary: '完成' }), tokensIn: 2, tokensOut: 1 },
    { content: readAction('only-in-task-ws-2.txt'), tokensIn: 3, tokensOut: 2 },
    { content: JSON.stringify({ action: 'final', summary: '完成' }), tokensIn: 2, tokensOut: 1 },
    { content: readAction('only-in-task-ws-1.txt'), tokensIn: 2, tokensOut: 1 },
    { content: readAction('only-in-task-ws-2.txt'), tokensIn: 2, tokensOut: 1 },
    { content: JSON.stringify({ action: 'final', summary: '验证完成' }), tokensIn: 2, tokensOut: 1 },
  ]));
  const c = new CoreClient(dataDir, {
    RATIFLOW_PLAN_DAG: '1',
    RATIFLOW_FAKE_MODEL_SCRIPT: fakeScript,
    // 本机 Docker daemon 不可用时执行器回落需要确定性：safe_restricted
    // （argv 只读白名单）足够承接 read_file 的真实执行。
    RATIFLOW_EXEC_MODE: 'safe_restricted',
  });
  try {
    // 主仓库（worktree/工作区 prepare 依赖）。
    const repoDir = join(tmpdir(), `sg-plan-exec-repo-${Date.now()}`);
    mkdirSync(repoDir, { recursive: true });
    git(repoDir, 'init', '-b', 'main');
    git(repoDir, 'config', 'user.email', 'e2e@ratiflow.local');
    git(repoDir, 'config', 'user.name', 'e2e');
    writeFileSync(join(repoDir, 'README.md'), 'hello\n');
    git(repoDir, 'add', '.');
    git(repoDir, 'commit', '-m', 'init');
    const sha1 = (s) => createHash('sha256').update(s).digest('hex');

    const proj = await c.call('project.create', {
      gitlabInstance: 'https://gitlab.test', namespace: 'team', project: 'planexec', localRoot: repoDir,
    });
    const wi = await c.call('workitem.create', { projectId: proj.id, title: '真实执行链', description: 'desc' });
    // 需求关活跃 attempt（门禁基线流，同 plan-dag-e2e）。
    const cov = await c.call('trace.coverage', { workItemId: wi.id });
    const keys = (cov.items ?? []).filter((i) => i.status === 'active').map((i) => i.requirementKey);
    const art = await c.call('artifact.create', { workItemId: wi.id, kind: 'prd', title: 'PRD' });
    const rev = await c.call('artifact.createDraft', { artifactId: art.id, content: `# PRD\n- [${keys[0]}] x`, requirementKeys: keys });
    await c.call('artifact.addReview', { revisionId: rev.id, reviewer: 't', verdict: 'approved' });
    await c.call('artifact.freezeBaseline', { workItemId: wi.id, gate: 'requirements', revisionIds: [rev.id] });
    const ev = await c.call('evidence.record', { workItemId: wi.id, gate: 'requirements', kind: 'manual', title: 'e', source: 'local', requirementKeys: keys });
    await c.call('evidence.verify', { evidenceId: ev.id, verifiedBy: 'q' });
    await c.call('gate.evaluate', { workItemId: wi.id, gate: 'requirements' });

    // 计划：两并行写 + 汇合验证 → submit/decide/start → dispatchReady。
    const mkTask = (key, deps) => ({
      taskKey: key, kind: deps.length ? 'analysis' : 'local_write', title: `任务-${key}`,
      inputs: [], expectedOutputs: deps.length ? [] : [`file:${key}.txt`],
      acceptance: { machine: [], manual: [] },
      effectClass: deps.length ? 'read' : 'local_write', deps,
    });
    const draft = await c.call('plan.createDraft', {
      workItemId: wi.id, idempotencyKey: 'pe-1',
      tasks: [mkTask('w1', []), mkTask('w2', []), mkTask('verify', ['w1', 'w2'])],
    });
    const pid = draft.planRevisionId;
    await c.call('plan.submit', { planRevisionId: pid });
    await c.call('plan.decide', { planRevisionId: pid, decision: 'approved', decidedBy: 'owner', reason: 'E2E' });
    await c.call('plan.start', { planRevisionId: pid, idempotencyKey: 'pe-s1' });
    const ready = (await c.call('plan.dispatchReady', { planRevisionId: pid, maxParallel: 3 })).dispatched;
    assert(ready.length === 2, `派发闸：两个写任务进入 ready（实际 ${ready.length}）`);
    const attemptId = ready[0].id;

    // 任务工作区（TaskAttempt → TaskWorkspace 落点）；标记文件仅存在于该工作区。
    const ws = (await c.call('taskWorkspace.prepare', { taskAttemptId: attemptId })).workspace;
    assert(ws.state === 'ready', 'TaskWorkspace ready');
    writeFileSync(join(ws.path, 'only-in-task-ws-1.txt'), 'task-ws-marker-1');

    // 反向 1：无绑定 Run 自报 succeeded → 拒绝。
    await expectErrorContains(
      () => c.call('planTask.transition', { taskAttemptId: attemptId, outcome: 'succeeded', outputDigest: 'x', idempotencyKey: 'pe-t0' }),
      'task_execution_proof_required',
      '无绑定 Run 自报 succeeded 被拒（评审 P0-2）',
    );

    // 反向 2：非法 autonomyGrantId 在 agent.start 即拒绝（fail-closed）。
    await expectErrorContains(
      () => c.call('agent.start', { workItemId: wi.id, goal: 'g', autonomyGrantId: 'g-nonexistent' }),
      'autonomyGrant',
      '非法 autonomyGrantId 拒绝启动',
    );

    // 绑定执行：agent.start(planTaskAttemptId) → 模型驱动 read_file 读任务工作区。
    const started = await c.call('agent.start', {
      workItemId: wi.id, goal: '完成 w1', planTaskAttemptId: attemptId,
      toolAllowlist: ['read_file'], idempotencyKey: 'pe-run1',
    });
    assert(!!started.runId, `agent.start 绑定 attempt 返回 runId`);
    const run = await waitTerminal(c, started.runId);
    assert(run.status === 'completed_execution', `Run 真实执行完成（${run.status}）`);
    const tr1 = await c.call('agent.trace', { runId: started.runId });
    const readStep = (tr1.steps ?? []).find((s) => s.kind === 'tool' && s.name === 'read_file');
    assert(!!readStep && String(readStep.preview ?? '').includes('task-ws-marker-1'),
      'Agent 读到 TaskWorkspace 独有标记（work_dir=TaskWorkspace 接线，评审 P0-3）');

    // 正向：绑定 Run 终态证明 → succeeded 放行 + 工作区 finalize retained。
    const info = await c.call('planTask.transition', { taskAttemptId: attemptId, outcome: 'succeeded', outputDigest: sha1('task-ws-marker-1'), idempotencyKey: 'pe-t1' });
    assert((info.attempt?.state ?? info.state) === 'succeeded', '绑定 Run 终态后 succeeded 放行');
    const wsAfter = (await c.call('taskWorkspace.get', { taskAttemptId: attemptId })).workspace;
    assert(wsAfter.state === 'retained', 'finalize 落 retained');
    assert(wsAfter.workspace_digest_after !== wsAfter.workspace_digest_before, 'after digest 记录变更');

    // 第二个写任务走同一真实链；完成后 verify（只读）派发并绑定执行。
    const [a2] = ready.slice(1);
    const ws2 = (await c.call('taskWorkspace.prepare', { taskAttemptId: a2.id })).workspace;
    writeFileSync(join(ws2.path, 'only-in-task-ws-2.txt'), 'task-ws-marker-2');
    const s2 = await c.call('agent.start', {
      workItemId: wi.id, goal: '完成 w2', planTaskAttemptId: a2.id,
      toolAllowlist: ['read_file'], idempotencyKey: 'pe-run2',
    });
    const run2 = await waitTerminal(c, s2.runId);
    assert(run2.status === 'completed_execution', '第二个写任务 Run 完成');
    const tr2 = await c.call('agent.trace', { runId: s2.runId });
    assert((tr2.steps ?? []).some((s) => s.kind === 'tool' && String(s.preview ?? '').includes('task-ws-marker-2')),
      '第二个任务同样读到其工作区独有标记');
    await c.call('planTask.transition', { taskAttemptId: a2.id, outcome: 'succeeded', outputDigest: sha1('task-ws-marker-2'), idempotencyKey: 'pe-t2' });
    const vReady = (await c.call('plan.dispatchReady', { planRevisionId: pid, maxParallel: 3 })).dispatched;
    assert(vReady.length === 1 && vReady[0].task_key === 'verify', '下游 verify 派发');
    const s3 = await c.call('agent.start', {
      workItemId: wi.id, goal: '验证', planTaskAttemptId: vReady[0].id,
      toolAllowlist: ['read_file'], idempotencyKey: 'pe-run3',
    });
    const run3 = await waitTerminal(c, s3.runId);
    assert(run3.status === 'completed_execution', 'verify Run 完成');
    await c.call('planTask.transition', { taskAttemptId: vReady[0].id, outcome: 'succeeded', outputDigest: sha1('verify'), idempotencyKey: 'pe-t3' });

    // 计划视角：全部任务真实完成。
    const view = await c.call('plan.get', { planRevisionId: pid });
    assert(view.attempts.length === 3 && view.attempts.every((a) => a.state === 'succeeded'), '计划任务全部真实完成');

    console.log('计划任务真实执行链 E2E 通过。');
  } finally {
    c.kill();
    rmSync(dataDir, { recursive: true, force: true });
    rmSync(fakeScript, { force: true });
  }
}

main().catch((e) => {
  console.error('plan-execution-e2e 失败：', e.message);
  process.exit(1);
});
