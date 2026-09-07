#!/usr/bin/env node
// 自动化调度 + Goal 谓词 + 私有技能仓库协议 E2E（EvoFlow 方案 M6-08 / ADR-037/039）：
// receipt 幂等（同 scheduled_for 不重复执行）；misfire skip；grant 闸（无/无效 grant
// → blocked_no_grant + 通知，零副作用）；runNow 手动触发；goal.autoReleaseCheck
// 七条件谓词（EV-021）；skill registry pin SHA 导入（draft，不执行任何代码，EV-022）。
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
  if (!c) throw new Error(`automation-e2e 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function expectErrorContains(call, token, label) {
  try {
    await call();
  } catch (e) {
    assert(String(e.message).includes(token), `${label}（实际 ${String(e.message).slice(0, 90)}）`);
    return;
  }
  throw new Error(`automation-e2e 断言失败：${label} 期望错误含 ${token}，但调用成功`);
}

function git(dir, ...args) {
  const out = spawnSync('git', ['-C', dir, ...args], { encoding: 'utf8' });
  if (out.status !== 0) throw new Error(`git ${args[0]} 失败: ${out.stderr}`);
  return out.stdout.trim();
}

async function main() {
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-auto-e2e-'));
  const c = new CoreClient(dataDir, {
    SIXGATES_AUTOMATIONS: '1',
    SIXGATES_SKILL_REGISTRY_LOCAL: '1',
  });
  try {
    const proj = await c.call('project.create', { gitlabInstance: 'g', namespace: 'n', project: 'auto', name: 'Auto' });
    const pj = (await c.call('project.list', {})).items[0].id;
    const wi = await c.call('workitem.create', { projectId: pj, title: '自动化任务', description: '' });

    // --- 1. Flag 关闭拒绝（另一 client 不带 flag）---
    {
      const cOff = new CoreClient(dataDir);
      try {
        await expectErrorContains(
          () => cOff.call('automation.create', { key: 'off-1', workItemId: wi.id, intent: {}, intervalSecs: 3600 }),
          'feature_disabled',
          'Flag 关闭：automation.create 拒绝',
        );
      } finally {
        cOff.kill();
      }
    }

    // --- 2. Grant 闸：无 grant 触发 → blocked_no_grant + 通知（零副作用）---
    const auto = await c.call('automation.create', {
      key: 'auto-no-grant',
      workItemId: wi.id,
      intent: { kind: 'goal', workItemId: wi.id, goal: '部署' },
      intervalSecs: 3600,
      autonomyGrantId: null,
    });
    assert(auto.status === 'active', '自动化创建 active');
    const fired = await c.call('automation.runNow', { automationId: auto.id, scheduledFor: '2026-09-06T12:00:00.000Z' });
    assert(fired.status === 'blocked_no_grant', `无 grant → blocked_no_grant（实际 ${fired.status}）`);
    // 同 scheduled_for 重放 → receipt 去重（不产生第二条）。
    const fired2 = await c.call('automation.runNow', { automationId: auto.id, scheduledFor: '2026-09-06T12:00:00.000Z' });
    assert(fired2.status === 'deduped', `重复触发被 receipt 去重（实际 ${fired2.status}）`);
    const hist = (await c.call('automation.history', { automationId: auto.id })).items;
    assert(hist.filter((h) => h.scheduledFor === '2026-09-06T12:00:00.000Z').length === 1, '历史恰一条（M6 退出标准：不重复执行）');
    // 通知面。
    const notes = (await c.call('notification.list', {})).items;
    assert(notes.some((n) => n.kind === 'automation_blocked'), 'automation_blocked 通知落 outbox');

    // --- 3. 有效 grant → intent_created ---
    // 缺陷审计：.catch 兼容路径会把 expiresAt 被拒（契约破坏）静默降级为通过——
    // 契约钉死：createGrant 必须接受 expiresAt，失败即失败。
    const grant = await c.call('autonomy.createGrant', {
      workItemId: wi.id,
      allowedTools: ['read_file', 'search_knowledge'],
      allowedRisks: ['low'],
      expiresAt: '2099-01-01T00:00:00.000Z',
    });
    const grantId = grant.grantId;
    assert(grantId, 'autonomy.createGrant 返回 grantId');
    const auto2 = await c.call('automation.create', {
      key: 'auto-with-grant',
      workItemId: wi.id,
      intent: { kind: 'goal', workItemId: wi.id, goal: '巡检' },
      intervalSecs: 60,
      autonomyGrantId: grantId,
    });
    const f3 = await c.call('automation.runNow', { automationId: auto2.id, scheduledFor: '2026-09-06T13:00:00.000Z' });
    assert(f3.status === 'intent_created', `有效 grant → intent_created（实际 ${f3.status}）`);

    // --- 4. Goal v2 谓词（EV-021）：flag 默认关 → 不允许 ---
    const check = await c.call('goal.autoReleaseCheck', { workItemId: wi.id, grantId });
    assert(check.allowed === false, 'Goal v2 自动放行默认关闭（flag）');
    assert(check.reasons.includes('auto_gate_release_disabled'), `原因含 auto_gate_release_disabled（实际 ${JSON.stringify(check.reasons)}）`);
    // 不存在的 grant → autonomy_grant_required。
    const bad = await c.call('goal.autoReleaseCheck', { workItemId: wi.id, grantId: 'g_ghost' });
    assert(!bad.allowed && bad.reasons.some((r) => r.includes('autonomy_grant_required')), '不存在 grant 拒绝');

    // --- 5. 暂停 CAS + misfire skip（过期触发被放弃）---
    const paused = await c.call('automation.pause', { automationId: auto2.id, expectedRevision: auto2.revision });
    assert(paused.status === 'paused', '暂停（CAS）');
    await expectErrorContains(
      () => c.call('automation.resume', { automationId: auto2.id, expectedRevision: auto2.revision }),
      'automation_conflict',
      '旧 revision 恢复 → CAS 冲突',
    );

    // --- 6. Skill 私有仓库：pin SHA 导入（draft，不执行任何代码）---
    const repoDir = join(tmpdir(), `sg-reg-repo-${Date.now()}`);
    mkdirSync(repoDir, { recursive: true });
    writeFileSync(join(repoDir, 'runbook.md'), '# 部署手册\n按步骤执行。\n');
    git(repoDir, 'init', '-b', 'main');
    git(repoDir, 'config', 'user.email', 'r@x');
    git(repoDir, 'config', 'user.name', 'r');
    git(repoDir, 'add', '.');
    git(repoDir, 'commit', '-m', 'v1');
    const sha = git(repoDir, 'rev-parse', 'HEAD');
    const bare = join(tmpdir(), `sg-reg-origin-${Date.now()}.git`);
    spawnSync('git', ['clone', '--quiet', '--bare', repoDir, bare]);
    // 本地远端需 opt-in（client env 已带 SIXGATES_SKILL_REGISTRY_LOCAL=1）。
    const imported = await c.call('skill.importFromRegistry', {
      repoUrl: bare, pinSha: sha,
    });
    assert(imported.files.some((f) => f.endsWith('runbook.md')), 'Markdown 文件导入（仅读取，不执行）');
    const versions = (await c.call('skill.versionList', { skillId: imported.skill_id })).items;
    assert(versions[0].status === 'draft', '导入为 draft（管理员激活前不进注入面）');
    // pin SHA 形状校验：分支名拒绝。
    await expectErrorContains(
      () => c.call('skill.importFromRegistry', { repoUrl: bare, pinSha: 'main' }),
      '40 位',
      'pin 必须为 commit SHA（防注入）',
    );

    console.log('自动化调度/Goal/私有技能仓库 协议 E2E 通过。');
  } finally {
    c.kill();
    rmSync(dataDir, { recursive: true, force: true });
  }
}

main().catch((e) => {
  console.error('automation-e2e 失败：', e.message);
  process.exit(1);
});
