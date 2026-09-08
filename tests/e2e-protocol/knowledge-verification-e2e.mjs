#!/usr/bin/env node
// 知识验证 v2 协议级 E2E（P0-6 / 0054；审计 RDWS-019「语义反向」修复）：
// ① 旧参数名 stableId 已废：sourceId + 服务端 revision 解析；
// ② pass → verified；最新 fail / unknown 立即覆盖旧 pass（不遮蔽）；
// ③ 操作幂等：同 verificationOpId 重放返回同一 receipt；
// ④ 重新 pass 恢复 verified（事件语义——改判可翻面）；
// ⑤ 内容变更（manifest contentSha256）→ unverified_content_changed；
// ⑥ 跨项目相同 stableId 不串（作用域隔离）；
// ⑦ receipt 携带 inputRevisionMode（服务端按 kind 解析，非 git 根 → content_hash）。
// 前置：cargo build --release -p ratiflow-core。
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { createHash } from 'node:crypto';
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
  if (!c) throw new Error(`knowledge-verification-e2e 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function expectErrorContains(call, token, label) {
  try {
    await call();
  } catch (e) {
    assert(String(e.message).includes(token), `${label}（实际 ${e.message.slice(0, 80)}）`);
    return;
  }
  throw new Error(`knowledge-verification-e2e 断言失败：${label} 期望错误含 ${token}，但调用成功`);
}

// 与 sg-knowledge repo_path_identity 同口径：
// digest = sha256("v1|repo_path|<normalized locator>")；stableId = "repopath-<digest>"；
// fileSlug = "repopath-<name_slug>-<digest[:12]>"（name="指南" → name_slug 按 ASCII 折叠为 src）。
function identitiesOf(locator, name = '指南') {
  const digest = createHash('sha256').update(`v1|repo_path|${locator}`).digest('hex');
  return {
    stableId: `repopath-${digest}`,
    slug: `repopath-src-${digest.slice(0, 12)}`,
  };
}

function writeProjectRoot(root, contentSha) {
  mkdirSync(join(root, 'knowledge/sources'), { recursive: true });
  const locator = 'docs/guide.md';
  const { stableId, slug } = identitiesOf(locator);
  const manifest = {
    contentSha256: contentSha,
    enabled: true,
    kind: 'repo_path',
    locator,
    name: '指南',
    schemaVersion: 2,
    stableId,
    contentOwner: 'doc-team',
    verificationPolicy: { intervalDays: 7, severity: 'block' },
  };
  writeFileSync(join(root, 'knowledge/sources', `${slug}.json`), JSON.stringify(manifest));
  return stableId;
}

let keySeq = 0;
const idem = (p) => `${p}-${++keySeq}`;

async function main() {
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-kv2-'));
  const c = new CoreClient(dataDir, {});
  try {
    const root1 = mkdtempSync(join(tmpdir(), 'sg-kv2-root1-'));
    const root2 = mkdtempSync(join(tmpdir(), 'sg-kv2-root2-'));
    await c.call('project.create', { gitlabInstance: 'local', namespace: 'e2e', project: 'kv2', name: 'KV2', localRoot: root1 });
    await c.call('project.create', { gitlabInstance: 'local', namespace: 'e2e', project: 'kv2b', name: 'KV2B', localRoot: root2 });
    const projects = (await c.call('project.list', {})).items;
    const pj1 = projects.find((p) => p.project === 'kv2');
    const pj2 = projects.find((p) => p.project === 'kv2b');

    const stableId = writeProjectRoot(root1, 'sha_v1');
    writeProjectRoot(root2, 'sha_v1'); // 跨项目同 stableId 同内容

    // ① 未验证 → unverified；receipt 携带服务端解析的 mode。
    let ov = await c.call('knowledge.freshnessOverview', { projectId: pj1.id });
    assert(ov.items.length === 1 && ov.items[0].state === 'unverified', '未验证 → unverified');
    assert(ov.items[0].inputRevisionMode === 'content_hash', '非 git 根 → content_hash 模式');

    // ② 旧参数名已废（stableId → sourceId）。
    await expectErrorContains(
      () => c.call('knowledge.verifySource', { projectId: pj1.id, stableId, outcome: 'pass', verifier: 'o', idempotencyKey: idem('bad') }),
      'sourceId',
      '旧参数 stableId 拒绝（missing sourceId）',
    );

    // ③ pass → verified；op 幂等。
    const r1 = await c.call('knowledge.verifySource', { projectId: pj1.id, sourceId: stableId, outcome: 'pass', verifier: 'owner', verificationOpId: 'kvop-1', idempotencyKey: idem('v1') });
    assert(r1.outcome === 'pass' && r1.inputRevisionMode === 'content_hash', 'pass receipt 落 v2（含 mode）');
    const r1r = await c.call('knowledge.verifySource', { projectId: pj1.id, sourceId: stableId, outcome: 'pass', verifier: 'owner', verificationOpId: 'kvop-1', idempotencyKey: idem('v1r') });
    assert(r1r.id === r1.id && r1r.verificationOpId === 'kvop-1', '同 verificationOpId 重放幂等');
    ov = await c.call('knowledge.freshnessOverview', { projectId: pj1.id });
    assert(ov.items[0].state === 'verified' && typeof ov.items[0].nextDueAt === 'string', 'pass → verified + nextDue');

    // ④ 最新 fail 覆盖旧 pass（v1 会遮蔽——RDWS-019 语义反向修复）。
    await c.call('knowledge.verifySource', { projectId: pj1.id, sourceId: stableId, outcome: 'fail', verifier: 'auditor', idempotencyKey: idem('v2') });
    ov = await c.call('knowledge.freshnessOverview', { projectId: pj1.id });
    assert(ov.items[0].state === 'failed', '最新 fail 覆盖旧 pass');

    // ⑤ unknown 同样覆盖；重新 pass 恢复。
    await c.call('knowledge.verifySource', { projectId: pj1.id, sourceId: stableId, outcome: 'unknown', verifier: 'auditor', idempotencyKey: idem('v3') });
    ov = await c.call('knowledge.freshnessOverview', { projectId: pj1.id });
    assert(ov.items[0].state === 'unknown', '最新 unknown 覆盖');
    await c.call('knowledge.verifySource', { projectId: pj1.id, sourceId: stableId, outcome: 'pass', verifier: 'owner', idempotencyKey: idem('v4') });
    ov = await c.call('knowledge.freshnessOverview', { projectId: pj1.id });
    assert(ov.items[0].state === 'verified', '重新 pass 恢复 verified（事件可翻面）');

    // ⑥ 跨项目同 stableId：pj1 的事实不影响 pj2（v1 会串成 verified）。
    ov = await c.call('knowledge.freshnessOverview', { projectId: pj2.id });
    assert(ov.items.length === 1 && ov.items[0].state === 'unverified', '跨项目同 stableId 不串');
    const r2 = await c.call('knowledge.verifySource', { projectId: pj2.id, sourceId: stableId, outcome: 'fail', verifier: 'qa2', idempotencyKey: idem('v5') });
    assert(r2.projectId === pj2.id && r2.id !== r1.id, 'pj2 独立 receipt');
    ov = await c.call('knowledge.freshnessOverview', { projectId: pj1.id });
    assert(ov.items[0].state === 'verified', 'pj2 的 fail 不影响 pj1');

    // ⑦ 内容变更 → unverified_content_changed；重验恢复。
    writeProjectRoot(root1, 'sha_v2');
    ov = await c.call('knowledge.freshnessOverview', { projectId: pj1.id });
    assert(ov.items[0].state === 'unverified_content_changed', '内容变更 → unverified_content_changed');
    await c.call('knowledge.verifySource', { projectId: pj1.id, sourceId: stableId, outcome: 'pass', verifier: 'owner', idempotencyKey: idem('v6') });
    ov = await c.call('knowledge.freshnessOverview', { projectId: pj1.id });
    assert(ov.items[0].state === 'verified', '新内容重验恢复 verified');

    console.log('knowledge-verification-e2e 全部通过');
  } finally {
    c.kill();
    rmSync(dataDir, { recursive: true, force: true });
  }
}

main().catch((e) => {
  console.error('knowledge-verification-e2e 失败：', e.message);
  process.exit(1);
});
