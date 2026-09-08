#!/usr/bin/env node
// B9 发布说明模板实例协议级 E2E（RDWS v1.4 WP-13 / 审计整改 P1-5）：
// 真实链路 agent.start → write_file(受管工件草稿目录) → artifact revision
// → baseline 冻结 → acceptance（六输入 gate.evaluate）→ release（放行批准）。
// 断言要点：
// - release_notes 为模板可配置 deliverable kind：deployment 关只要求 release_notes
//   （不硬编码 Deployment 关的 'deployment' kind 联动）；
// - Agent 产物真实落盘在 data-dir 受管目录（非客户端伪造），artifact 修订内容
//   取自该文件字节；
// - 冻结前 deliverableStatus=not_frozen、冻结后 satisfied、evaluate 全过、
//   放行批准后 gate state=passed。
// 前置：cargo build --release -p ratiflow-core。
import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
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
  async call(method, params = {}) {
    for (let i = 0; i < 100 && !this.hello; i++) {
      await new Promise((r) => setTimeout(r, 50));
    }
    const id = String(this.nextId++);
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.proc.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
    });
  }
  kill() { this.proc.kill('SIGKILL'); }
}

function assert(c, label) {
  if (!c) throw new Error(`release-notes-e2e 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function waitTerminal(client, runId, capMs = 30000) {
  const deadline = Date.now() + capMs;
  for (;;) {
    const run = await client.call('agent.get', { runId });
    if (['completed_execution', 'failed', 'cancelled'].includes(run.status)) return run;
    if (Date.now() > deadline) throw new Error(`等待终态超时：${runId}（当前 ${run.status}）`);
    await new Promise((r) => setTimeout(r, 100));
  }
}

const NOTES_CONTENT = [
  '# 发布说明 v1.2.0',
  '',
  '## 新增',
  '- 门禁放行链路支持发布说明作为独立交付物（B9）',
  '',
  '## 修复',
  '- 基线冻结后新增草稿不再误判满足',
  '',
  '## 升级注意',
  '- 无数据库迁移',
].join('\n');

async function main() {
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-b9rn-'));
  // 脚本模型：write_file 产发布说明草稿 → final 收尾（无 run_command，全程无暂停）。
  const script = [
    { content: JSON.stringify({ action: 'write_file', arguments: { path: 'release-notes.md', content: NOTES_CONTENT }, summary: '生成发布说明草稿' }), tokensIn: 10, tokensOut: 5 },
    { content: JSON.stringify({ action: 'final', summary: '发布说明已生成' }), tokensIn: 10, tokensOut: 5 },
  ];
  const scriptPath = join(tmpdir(), `sg-b9rn-script-${Date.now()}.json`);
  writeFileSync(scriptPath, JSON.stringify(script));
  const c = new CoreClient(dataDir, {
    RATIFLOW_WORKFLOW_TEMPLATE_V2: '1',
    RATIFLOW_EXEC_MODE: 'safe_restricted',
    RATIFLOW_FAKE_MODEL_SCRIPT: scriptPath,
  });
  try {
    await c.call('project.create', { gitlabInstance: 'local', namespace: 'e2e', project: 'b9rn', name: 'B9 发布说明' });
    const pj = (await c.call('project.list', {})).items[0].id;

    // 1) 模板实例：deployment 关 deliverables 只声明 release_notes——
    //    证明 release_notes 是模板可配置 kind，且 Deployment 关不硬编码要求 'deployment'。
    const created = await c.call('workflowTemplate.create', {
      key: 'b9-release-notes', name: '发布说明发布关',
      gates: [{
        gateId: 'deployment', title: '发布关', purpose: '发布说明就绪后放行',
        deliverables: ['release_notes'], acceptance: ['发布说明内容完整、变更条目可追溯'],
      }],
      idempotencyKey: 'b9rn-tpl-1',
    });
    const activated = await c.call('workflowTemplate.activate', { versionId: created.version.id, idempotencyKey: 'b9rn-tpl-2' });
    assert(activated.status === 'active', '发布说明模板激活');

    const wi = await c.call('workitem.create', { projectId: pj, title: 'v1.2.0 发布', templateId: 'b9-release-notes' });
    const inst0 = await c.call('workflow.getInstance', { workItemId: wi.id });
    assert(inst0.gates.length === 1 && inst0.gates[0].gate_id === 'deployment', '实例读模型携带模板关');
    assert(JSON.stringify(inst0.gates[0].deliverables) === JSON.stringify(['release_notes']),
      `关交付物=release_notes（实际 ${JSON.stringify(inst0.gates[0].deliverables)}）`);

    // 2) agent.start：唯一执行入口，Agent 经 write_file 在受管目录生成发布说明草稿。
    const cov = await c.call('trace.coverage', { workItemId: wi.id });
    const keys = (cov.items ?? []).filter((i) => i.status === 'active').map((i) => i.requirementKey);
    assert(keys.length >= 1, '需求项就绪');
    const manifest = await c.call('context.create', { projectId: pj, workItemId: wi.id, query: '发布说明', selectedSources: [] });
    const started = await c.call('agent.start', {
      workItemId: wi.id, goal: '生成 v1.2.0 发布说明', contextManifestId: manifest.id,
      toolAllowlist: ['write_file'], idempotencyKey: 'b9rn-run-1',
    });
    const run = await waitTerminal(c, started.runId);
    assert(run.status === 'completed_execution', `Run 完成（实际 ${run.status}：${run.result}）`);

    // 3) 受管目录落盘证据：write_file 真实写入 data-dir artifacts/{runId}/。
    const props = (await c.call('agent.proposals', { runId: started.runId })).items;
    const wf = props.find((p) => p.tool === 'write_file');
    assert(wf && wf.decision === 'executed' && wf.result.includes('written'), `write_file 提案执行（${wf?.result}）`);
    const onDisk = readFileSync(join(dataDir, 'artifacts', started.runId, 'release-notes.md'), 'utf8');
    assert(onDisk === NOTES_CONTENT, '受管目录文件字节与模型输出一致');

    // 4) artifact revision：内容取自受管目录文件（非客户端另行编造）。
    const art = await c.call('artifact.create', { workItemId: wi.id, kind: 'release_notes', title: 'v1.2.0 发布说明' });
    const rev = await c.call('artifact.createDraft', { artifactId: art.id, content: onDisk, requirementKeys: keys });
    let st = await c.call('gate.deliverableStatus', { workItemId: wi.id, gate: 'deployment' });
    assert(st.satisfied === false && st.missing === 'not_frozen', `冻结前 not_frozen（实际 ${st.missing}）`);

    // 5) baseline：评审 → 冻结。
    await c.call('artifact.addReview', { revisionId: rev.id, reviewer: 'reviewer', verdict: 'approved' });
    await c.call('artifact.freezeBaseline', { workItemId: wi.id, gate: 'deployment', revisionIds: [rev.id] });
    st = await c.call('gate.deliverableStatus', { workItemId: wi.id, gate: 'deployment' });
    assert(st.satisfied === true && st.revisionId === rev.id, '冻结后交付物满足');

    // 6) acceptance：证据 + 六输入 evaluate。
    const evid = await c.call('evidence.record', { workItemId: wi.id, gate: 'deployment', kind: 'manual', title: '发布说明核验', source: 'local', requirementKeys: keys });
    await c.call('evidence.verify', { evidenceId: evid.id, verifiedBy: 'qa' });
    const ev = await c.call('gate.evaluate', { workItemId: wi.id, gate: 'deployment' });
    assert(ev.passed === true && ev.failed_inputs.length === 0, `acceptance 六输入全过（${JSON.stringify(ev.failed_inputs)}）`);

    // 7) release：放行请求 → 批准 → 关 passed。
    const rr = await c.call('gate.requestRelease', { workItemId: wi.id, gate: 'deployment' });
    const done = await c.call('gate.decideRelease', { approvalId: rr.approval_id, decision: 'approved', decidedBy: 'owner', reason: 'B9 E2E 放行' });
    assert(done.state === 'approved' && done.decided_at, `放行批准落态（${done.state}）`);
    const inst = await c.call('workflow.getInstance', { workItemId: wi.id });
    assert(inst.gates[0].state === 'passed', `关 state=passed（实际 ${inst.gates[0].state}）`);

    console.log('B9 发布说明模板实例 E2E 通过。');
  } finally {
    c.kill();
    rmSync(dataDir, { recursive: true, force: true });
    rmSync(scriptPath, { force: true });
  }
}

main().catch((e) => {
  console.error('release-notes-e2e 失败：', e.message);
  process.exit(1);
});
