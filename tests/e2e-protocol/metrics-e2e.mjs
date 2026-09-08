#!/usr/bin/env node
// 指标投影协议级 E2E（EvoFlow WP-10；纯读）：
// metrics.overview 六指标形状与最小样本 insufficient_data、审批分层（gate_release
// 窗内计数）、scope 非法负例；triage.list 聚合 gaps 概要。前置：cargo build --release -p ratiflow-core。
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
  if (!c) throw new Error(`metrics-e2e 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function expectErrorContains(call, token, label) {
  try {
    await call();
  } catch (e) {
    assert(String(e.message).includes(token), `${label}（实际 ${e.message.slice(0, 80)}）`);
    return;
  }
  throw new Error(`metrics-e2e 断言失败：${label} 期望错误含 ${token}，但调用成功`);
}

async function main() {
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-metrics-'));
  const c = new CoreClient(dataDir, {});
  try {
    await c.call('project.create', { gitlabInstance: 'local', namespace: 'e2e', project: 'metrics', name: 'Metrics' });
    const pj = (await c.call('project.list', {})).items[0].id;

    // 造数：两个 workitem，各完成 requirements 门（2 笔 approved gate_release 审批）。
    for (const n of ['m1', 'm2']) {
      const wi = await c.call('workitem.create', { projectId: pj, title: `指标任务 ${n}` });
      const keys = (await c.call('trace.coverage', { workItemId: wi.id })).items
        .filter((i) => i.status === 'active').map((i) => i.requirementKey);
      const art = await c.call('artifact.create', { workItemId: wi.id, kind: 'prd', title: 'PRD' });
      const rev = await c.call('artifact.createDraft', { artifactId: art.id, content: `# PRD\n- [${keys[0]}] 覆盖`, requirementKeys: keys });
      await c.call('artifact.addReview', { revisionId: rev.id, reviewer: 't', verdict: 'approved' });
      await c.call('artifact.freezeBaseline', { workItemId: wi.id, gate: 'requirements', revisionIds: [rev.id] });
      const ev = await c.call('evidence.record', { workItemId: wi.id, gate: 'requirements', kind: 'manual', title: '核验', source: 'local', requirementKeys: keys });
      await c.call('evidence.verify', { evidenceId: ev.id, verifiedBy: 'qa' });
      await c.call('gate.evaluate', { workItemId: wi.id, gate: 'requirements' });
      const rr = await c.call('gate.requestRelease', { workItemId: wi.id, gate: 'requirements' });
      await c.call('gate.decideRelease', { approvalId: rr.approval_id, decision: 'approved', decidedBy: 'owner', reason: 'E2E' });
    }

    // 1) overview：形状与 gate_release 分层计数（P1-2：subject_type × risk 二维）。
    const ov = await c.call('metrics.overview', { scope: 'global' });
    assert(ov.scope === 'global' && ov.windowDays === 30, 'scope/window 形状');
    assert(JSON.stringify(ov.approvalLayers.dimensions) === JSON.stringify(['subjectType', 'risk']),
      `approvalLayers 维度声明（实际 ${JSON.stringify(ov.approvalLayers.dimensions)}）`);
    const layers = ov.approvalLayers.layers;
    const gr = layers.find((l) => l.subjectType === 'gate_release');
    assert(gr && gr.approved === 2 && gr.rejected === 0, `gate_release approved=2（实际 ${JSON.stringify(gr)}）`);
    assert(gr.passRate === 1, `通过率 1.0（实际 ${gr.passRate}）`);
    assert(gr.insufficientData === true, 'n<10 → insufficient_data');
    assert(typeof gr.latencyMedianSecs === 'number', '延迟 median 落值');
    assert(gr.rubberStampSuspect === false, '人工评审不构成橡皮图章');
    // 回环双口径：无 rework → 次数/占比均 0（非 null：分母在窗），insufficient。
    assert(ov.loopRate.insufficientData === true && ov.loopRate.completedReworks === 0, '回环率 insufficient（workitem<5）');
    assert(ov.loopRate.averageReworkCount === 0 && ov.loopRate.reworkWorkitemRate === 0, '双口径字段在位');
    assert(!('rate' in ov.loopRate), '旧混用口径 rate 字段已废');
    assert(ov.aiSuggestionAdoption.insufficientData === true, '采纳率 insufficient（<30）');
    assert(Array.isArray(ov.orphanRate.perWorkitem) && ov.orphanRate.perWorkitem.length === 2, '孤儿率按 workitem 透出');

    // 2) 负例：scope 非法。
    await expectErrorContains(
      () => c.call('metrics.overview', { scope: 'per-workitem' }),
      'metrics_invalid',
      'scope 非法拒绝',
    );

    // 3) triage.list：聚合 gaps 概要。
    const triage = await c.call('triage.list', {});
    assert(Array.isArray(triage.items) && triage.items.length === 2, 'triage 聚合两个 workitem');
    assert(triage.items.every((x) => typeof x.orphanCount === 'number' && typeof x.uncoveredCount === 'number'), 'triage 条目含 orphan/uncovered 计数');

    console.log('指标投影协议 E2E 通过。');
  } finally {
    c.kill();
    rmSync(dataDir, { recursive: true, force: true });
  }
}

main().catch((e) => {
  console.error('metrics-e2e 失败：', e.message);
  process.exit(1);
});
