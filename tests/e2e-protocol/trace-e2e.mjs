#!/usr/bin/env node
// 谱系底座协议级 E2E（ADR-030 M1 / 蓝图 §13.2 trace-e2e）：
// 需求修订/条目导入与幂等 → 证据/工件经 requirementKeys 建边 → coverage/lineage/gaps →
// trace_incomplete fail-closed → legacy 回填（unverified）→ 新写开关。
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
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
  if (!condition) {
    throw new Error(`E2E 断言失败：${label}`);
  }
  console.log(`  ✓ ${label}`);
}

async function main() {
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-trace-e2e-'));
  let client = new CoreClient(dataDir);
  const fail = (error) => {
    console.error(`E2E 失败：${error.message}`);
    client.kill();
    rmSync(dataDir, { recursive: true, force: true });
    process.exit(1);
  };

  try {
    await client.hello_();
    assert(client.hello.schemaVersion >= 16, `schemaVersion ≥ 16（实际 ${client.hello.schemaVersion}）`);

    const project = await client.call('project.create', {
      gitlabInstance: 'https://gitlab.example.com', namespace: 'demo', project: 'trace',
    });

    // 1. workitem.create 自动生成需求修订 v1（verified）+ 条目 + 谱系节点。
    const wi = await client.call('workitem.create', {
      projectId: project.id,
      title: '对账需求',
      description: '商户对账闭环',
    });
    let revs = await client.call('requirement.revisions', { workItemId: wi.id });
    assert(revs.items.length === 1, '创建任务后自动生成一个需求文档');
    assert(revs.items[0].document.source_kind === 'inline', '文档来源为 inline');
    assert(revs.items[0].revisions.length === 1, '初始修订恰好 1 个');
    const rev1 = revs.items[0].revisions[0];
    let items = await client.call('requirement.items', { revisionId: rev1.id });
    assert(items.items.length === 1, 'description 无列表时回退单条目');
    assert(items.items[0].requirement_key === 'REQ-001', '回退条目 key 为 REQ-001');

    // 2. 显式导入修订：列表解析为多条目；同内容幂等；新内容产生 v2 + supersedes。
    const contentV2 = '# 对账需求\n\n- 日终自动对账 `REQ-ACC-01`\n- 差异报告导出\n';
    const imported = await client.call('requirement.importRevision', {
      workItemId: wi.id, filename: 'requirement.md', content: contentV2,
    });
    assert(imported.revision.revision_no === 2, '显式导入产生 v2');
    assert(imported.items.length === 2, 'v2 解析出 2 个需求项');
    assert(imported.items[0].requirement_key === 'REQ-ACC-01', '反引号 REQ key 被采用');
    const dup = await client.call('requirement.importRevision', {
      workItemId: wi.id, filename: 'requirement.md', content: contentV2,
    });
    assert(dup.deduplicated === true && dup.revision.id === imported.revision.id, '同内容导入幂等（不追加修订）');

    // 3. 证据 + requirementKeys → verifies 边；coverage 可见。
    await client.call('artifact.create', { workItemId: wi.id, kind: 'plan', title: '对账方案' });
    const arts = await client.call('artifact.list', { workItemId: wi.id });
    const draft = await client.call('artifact.createDraft', {
      artifactId: arts.items[0].id,
      content: '# 对账方案\n覆盖 REQ-ACC-01 与差异导出。',
      requirementKeys: ['REQ-ACC-01'],
    });
    assert(!!draft.id, '工件修订创建成功（satisfies 边随 createDraft 写入）');
    const evidence = await client.call('evidence.record', {
      workItemId: wi.id, gate: 'requirements', kind: 'test_report',
      title: '对账测试报告', content: '全部通过', requirementKeys: ['REQ-ACC-01'],
    });
    const cov = await client.call('trace.coverage', { workItemId: wi.id });
    assert(cov.revisionId === imported.revision.id, 'coverage 默认取最新修订');
    const acc = cov.items.find((i) => i.requirementKey === 'REQ-ACC-01');
    const exp = cov.items.find((i) => i.requirement_key === 'REQ-002' || i.requirementKey === 'REQ-002');
    assert(acc && acc.satisfies === 1 && acc.verifies === 1, 'REQ-ACC-01 有 satisfies+verifies 入边');
    assert(exp && exp.covered === false, '另一条目未覆盖');
    assert(cov.coveredCount === 1 && cov.totalItems === 2, '覆盖计数 1/2');

    // 4. lineage：从需求项 up 能到证据/工件，down 到修订。
    const lineage = await client.call('trace.lineage', { nodeId: acc.nodeId, direction: 'both', depth: 3 });
    const types = lineage.nodes.map((n) => n.node_type);
    assert(types.includes('requirement_revision'), 'lineage down 到需求修订');
    assert(types.includes('artifact_revision') && types.includes('evidence'), 'lineage up 到工件修订与证据');

    // 5. 证据核验 → 节点 verified；unverified 计数下降。
    const gapsBefore = await client.call('trace.gaps', { workItemId: wi.id });
    assert(gapsBefore.unverifiedCount === 1, '证据初始 unverified 计数 1');
    await client.call('evidence.verify', { evidenceId: evidence.id, verifiedBy: 'local-user' });
    const gapsAfter = await client.call('trace.gaps', { workItemId: wi.id });
    assert(gapsAfter.unverifiedCount === 0, '核验后 unverified 计数归零');
    assert(gapsAfter.uncoveredItemCount === 1, '断链扫描发现 1 个未覆盖条目');

    // 6. fail-closed：未知需求 key → trace_incomplete，且不落证据。
    const beforeCount = (await client.call('evidence.list', { workItemId: wi.id })).items.length;
    let rejected = false;
    try {
      await client.call('evidence.record', {
        workItemId: wi.id, gate: 'requirements', kind: 'test_report',
        title: '坏链证据', requirementKeys: ['REQ-404'],
      });
    } catch (e) {
      rejected = e.code === 'trace_incomplete';
    }
    assert(rejected, '未知需求 key 返回 trace_incomplete');
    const afterCount = (await client.call('evidence.list', { workItemId: wi.id })).items.length;
    assert(afterCount === beforeCount, 'trace_incomplete 时证据未落库（无副作用）');

    // 7. requirement.get 携带条目与 coverage。
    const reqGet = await client.call('requirement.get', { revisionId: imported.revision.id });
    assert(reqGet.items.length === 2 && reqGet.coverage.totalItems === 2, 'requirement.get 返回条目与覆盖');

    client.kill();

    // 8. legacy 回填：关新写开关建任务 → 重启默认开关 → synthetic 修订（unverified）。
    client = new CoreClient(dataDir, { SIXGATES_TRACE_WRITES: '0' });
    await client.hello_();
    const legacyWi = await client.call('workitem.create', {
      projectId: project.id, title: '历史任务', description: '旧描述',
    });
    let legacyRevs = await client.call('requirement.revisions', { workItemId: legacyWi.id });
    assert(legacyRevs.items.length === 0, '关闭新写时不生成需求修订');
    client.kill();

    client = new CoreClient(dataDir);
    await client.hello_();
    legacyRevs = await client.call('requirement.revisions', { workItemId: legacyWi.id });
    assert(legacyRevs.items.length === 1, '重启回填生成 synthetic 需求文档');
    assert(legacyRevs.items[0].document.source_kind === 'legacy_import', '来源标记 legacy_import');
    assert(legacyRevs.items[0].revisions.length === 1, 'synthetic 修订恰好 1 个');
    const legacyCov = await client.call('trace.coverage', { workItemId: legacyWi.id });
    assert(legacyCov.totalItems === 1, 'synthetic 修订有回退条目');
    const legacyGaps = await client.call('trace.gaps', { workItemId: legacyWi.id });
    assert(legacyGaps.unverifiedCount >= 1, 'legacy 节点 unverified（诚实标记）');
    // 幂等：再次重启不重复回填。
    client.kill();
    client = new CoreClient(dataDir);
    await client.hello_();
    const legacyRevs2 = await client.call('requirement.revisions', { workItemId: legacyWi.id });
    assert(legacyRevs2.items[0].revisions.length === 1, '回填幂等：重启不重复生成修订');

    // 9. 时间线包含谱系事件。
    const timeline = await client.call('timeline.snapshot', { workItemId: legacyWi.id });
    const revEvents = timeline.events.filter((e) => e.type === 'requirement.revision_imported');
    assert(revEvents.length >= 1, '时间线包含需求修订导入事件（中文摘要）');
    assert(revEvents[0].summary.includes('需求修订导入'), `摘要为中文（${revEvents[0].summary}）`);

    console.log('\ntrace-e2e：全部断言通过 ✓');
    client.kill();
    rmSync(dataDir, { recursive: true, force: true });
  } catch (error) {
    fail(error);
  }
}

main();
