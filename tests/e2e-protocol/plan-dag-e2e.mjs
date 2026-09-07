#!/usr/bin/env node
// 结构化计划 Plan DAG 协议级 E2E（EvoFlow 方案 M2-10 / ADR-036/037）：
// Flag 关闭 feature_disabled；环计划拒绝（EV-005）；合法计划生命周期
// submit→decide→start（ready attempt 创建）；两个并行写任务隔离工作区（EV-007）；
// 只读任务无工作区；workspace finalize 落 retained + after digest。
// 前置：cargo build --release -p ratiflow-core。
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync, mkdirSync, writeFileSync, existsSync } from 'node:fs';
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
  if (!c) throw new Error(`plan-dag-e2e 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function expectErrorContains(call, token, label) {
  try {
    await call();
  } catch (e) {
    assert(String(e.message).includes(token), `${label}（实际 ${String(e.message).slice(0, 90)}）`);
    return;
  }
  throw new Error(`plan-dag-e2e 断言失败：${label} 期望错误含 ${token}，但调用成功`);
}

import { spawnSync } from 'node:child_process';

function git(dir, ...args) {
  const out = spawnSync('git', ['-C', dir, ...args], { encoding: 'utf8' });
  if (out.status !== 0) throw new Error(`git ${args[0]} 失败: ${out.stderr}`);
  return out.stdout.trim();
}

const WRITE_TASK = (key, deps = []) => ({
  taskKey: key, kind: 'local_write', title: `写-${key}`,
  inputs: [], expectedOutputs: [`file:${key}.txt`],
  acceptance: { machine: [], manual: [] },
  effectClass: 'local_write', deps,
});
const READ_TASK = (key, deps = []) => ({
  taskKey: key, kind: 'analysis', title: `析-${key}`,
  inputs: [], expectedOutputs: [], acceptance: { machine: [], manual: [] },
  effectClass: 'read', deps,
});

async function main() {
  // ============ 场景一：Flag 关闭（默认）============
  {
    const dataDir = mkdtempSync(join(tmpdir(), 'sg-plan-off-'));
    const c = new CoreClient(dataDir);
    try {
      await expectErrorContains(() => c.call('plan.list', { workItemId: 'x' }), 'feature_disabled', 'Flag 关闭：plan.* 拒绝');
      await expectErrorContains(() => c.call('taskWorkspace.get', { taskAttemptId: 'x' }), 'feature_disabled', 'Flag 关闭：taskWorkspace.* 拒绝');
      console.log('场景一（Flag 关闭）通过');
    } finally {
      c.kill();
      rmSync(dataDir, { recursive: true, force: true });
    }
  }

  // ============ 场景二：Flag 开启 ============
  {
    const dataDir = mkdtempSync(join(tmpdir(), 'sg-plan-on-'));
    const c = new CoreClient(dataDir, { RATIFLOW_PLAN_DAG: '1' });
    try {
      // git 主仓库（工作区 prepare 依赖）。
      const repoDir = join(tmpdir(), `sg-plan-repo-${Date.now()}`);
      mkdirSync(repoDir, { recursive: true });
      git(repoDir, 'init', '-b', 'main');
      git(repoDir, 'config', 'user.email', 'e2e@ratiflow.local');
      git(repoDir, 'config', 'user.name', 'e2e');
      writeFileSync(join(repoDir, 'README.md'), 'hello\n');
      git(repoDir, 'add', '.');
      git(repoDir, 'commit', '-m', 'init');

      const proj = await c.call('project.create', {
        gitlabInstance: 'https://gitlab.test', namespace: 'team', project: 'plan', localRoot: repoDir,
      });
      const wi = await c.call('workitem.create', { projectId: proj.id, title: '计划任务', description: 'desc' });
      // 需求关活跃 attempt（evaluate 通过后 ensure_active 建档）。
      const cov = await c.call('trace.coverage', { workItemId: wi.id });
      const keys = (cov.items ?? []).filter((i) => i.status === 'active').map((i) => i.requirementKey);
      const art = await c.call('artifact.create', { workItemId: wi.id, kind: 'prd', title: 'PRD' });
      const rev = await c.call('artifact.createDraft', { artifactId: art.id, content: `# PRD\n- [${keys[0]}] x`, requirementKeys: keys });
      await c.call('artifact.addReview', { revisionId: rev.id, reviewer: 't', verdict: 'approved' });
      await c.call('artifact.freezeBaseline', { workItemId: wi.id, gate: 'requirements', revisionIds: [rev.id] });
      const ev = await c.call('evidence.record', { workItemId: wi.id, gate: 'requirements', kind: 'manual', title: 'e', source: 'local', requirementKeys: keys });
      await c.call('evidence.verify', { evidenceId: ev.id, verifiedBy: 'q' });
      const evaluated = await c.call('gate.evaluate', { workItemId: wi.id, gate: 'requirements' });
      assert(evaluated.passed === true, '需求关评估通过（活跃 attempt 就绪）');

      // 1) EV-005：环计划拒绝创建。
      await expectErrorContains(
        () => c.call('plan.createDraft', {
          workItemId: wi.id, idempotencyKey: 'p-cycle',
          tasks: [WRITE_TASK('a', ['b']), WRITE_TASK('b', ['a'])],
        }),
        'plan_cycle_detected',
        '环计划拒绝（EV-005）',
      );
      // 缺引用依赖拒绝。
      await expectErrorContains(
        () => c.call('plan.createDraft', {
          workItemId: wi.id, idempotencyKey: 'p-ghost',
          tasks: [WRITE_TASK('a', ['ghost'])],
        }),
        'ghost',
        '引用不存在上游拒绝',
      );
      // 孤立写任务拒绝。
      await expectErrorContains(
        () => c.call('plan.createDraft', {
          workItemId: wi.id, idempotencyKey: 'p-orphan',
          tasks: [WRITE_TASK('stray')],
        }),
        '孤立写任务',
        '孤立写任务拒绝',
      );

      // 2) 合法计划：两并行写 + 汇合验证。
      const draft = await c.call('plan.createDraft', {
        workItemId: wi.id, idempotencyKey: 'p-ok',
        tasks: [
          WRITE_TASK('w1'),
          WRITE_TASK('w2'),
          READ_TASK('verify', ['w1', 'w2']),
        ],
      });
      const pid = draft.planRevisionId;
      assert(draft.revision.status === 'draft', 'draft 创建');

      // 3) 生命周期：未批准不可 start → submit → 审批链决定 → start。
      await expectErrorContains(() => c.call('plan.start', { planRevisionId: pid, idempotencyKey: 's0' }), 'plan_invalid_transition', '未批准 start 拒绝');
      await c.call('plan.submit', { planRevisionId: pid });
      const approvals = await c.call('approval.list', {});
      const planApproval = approvals.items.find((a) => a.subject_type === 'plan_revision');
      assert(!!planApproval, '审批中心出现计划待审批项');
      const decided = await c.call('plan.decide', { planRevisionId: pid, decision: 'approved', decidedBy: 'owner', reason: 'E2E' });
      assert(decided.status === 'approved', '计划批准（走既有审批链）');
      const started = await c.call('plan.start', { planRevisionId: pid, idempotencyKey: 's1' });
      assert(started.revision.status === 'executing', '计划开始执行');
      assert(started.readyAttempts.length === 2, `ready set = 两个无依赖写任务（实际 ${started.readyAttempts.length}）`);
      // 幂等：再次 start 不新建 attempt。
      const again = await c.call('plan.start', { planRevisionId: pid, idempotencyKey: 's2' });
      assert(again.readyAttempts.length === 2, 'start 幂等');

      // 4) EV-007：两个写任务隔离工作区。
      const [pa1, pa2] = started.readyAttempts;
      const w1 = (await c.call('taskWorkspace.prepare', { taskAttemptId: pa1.id })).workspace;
      const w2 = (await c.call('taskWorkspace.prepare', { taskAttemptId: pa2.id })).workspace;
      assert(w1.path !== w2.path, '两个写任务工作区路径不重叠（EV-007）');
      assert(w1.state === 'ready' && w1.base_head.length === 40, '工作区 ready + base HEAD 钉住');
      assert(existsSync(w1.path) && existsSync(w2.path), '工作区真实存在');
      // 幂等。
      const w1b = (await c.call('taskWorkspace.prepare', { taskAttemptId: pa1.id })).workspace;
      assert(w1b.id === w1.id, 'prepare 幂等');
      // 各自写入后 finalize：retained + after digest ≠ before。
      writeFileSync(join(w1.path, 'w1.txt'), 'one\n');
      writeFileSync(join(w2.path, 'w2.txt'), 'two\n');
      const f1 = (await c.call('taskWorkspace.finalize', { taskAttemptId: pa1.id, outcome: 'succeeded', idempotencyKey: 'f1' })).workspace;
      const f2 = (await c.call('taskWorkspace.finalize', { taskAttemptId: pa2.id, outcome: 'succeeded', idempotencyKey: 'f2' })).workspace;
      assert(f1.state === 'retained' && f2.state === 'retained', 'finalize 落 retained');
      assert(f1.workspace_digest_after !== f1.workspace_digest_before, 'after digest 记录变更');

      // 5) plan.get read model：拓扑序 + attempt 状态 + markdown 投影。
      const view = await c.call('plan.get', { planRevisionId: pid });
      assert(view.topologicalOrder.indexOf('w1') < view.topologicalOrder.indexOf('verify'), '拓扑序：写在验证前');
      assert(view.markdown.includes('# 计划'), 'Markdown 投影生成');
      assert(view.attempts.length === 2, 'attempt 列表');
      const listResult = await c.call('plan.list', { workItemId: wi.id });
      assert(listResult.items.length === 1, 'plan.list 按任务聚合');

      console.log('场景二（Flag 开启完整生命周期）通过');
    } finally {
      c.kill();
      rmSync(dataDir, { recursive: true, force: true });
    }
  }

  console.log('计划 DAG 协议 E2E 通过。');
}

main().catch((e) => {
  console.error('plan-dag-e2e 失败：', e.message);
  process.exit(1);
});
