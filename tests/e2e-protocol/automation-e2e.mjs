#!/usr/bin/env node
// 自动化调度 + Goal 谓词 + 私有技能仓库协议 E2E（EvoFlow 方案 M6-08 / ADR-037/039）：
// receipt 幂等（同 scheduled_for 不重复执行）；misfire skip；grant 闸（无/无效 grant
// → blocked_no_grant + 通知，零副作用）；runNow 手动触发；goal.autoReleaseCheck
// 七条件谓词（EV-021）；skill registry pin SHA 导入（draft，不执行任何代码，EV-022）。
// 前置：cargo build --release -p ratiflow-core。
import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, mkdirSync, writeFileSync } from 'node:fs';
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
    RATIFLOW_AUTOMATIONS: '1',
    RATIFLOW_SKILL_REGISTRY_LOCAL: '1',
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
    assert(fired.status === 'shadowed', `WP-12 默认 shadow：无 grant tick 也只产建议（实际 ${fired.status}）`);
    // 同 scheduled_for 重放 → receipt 去重（不产生第二条）。
    const fired2 = await c.call('automation.runNow', { automationId: auto.id, scheduledFor: '2026-09-06T12:00:00.000Z' });
    assert(fired2.status === 'deduped', `重复触发被 receipt 去重（实际 ${fired2.status}）`);
    const hist = (await c.call('automation.history', { automationId: auto.id })).items;
    assert(hist.filter((h) => h.scheduledFor === '2026-09-06T12:00:00.000Z').length === 1, '历史恰一条（M6 退出标准：不重复执行）');
    // 通知面：automation_blocked 断言移至 WP-12 场景（shadow_fallback 通知）。
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
      assert(f3.status === 'shadowed', `WP-12 默认 shadow：有效 grant 也只产建议（实际 ${f3.status}）`);

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
    // 本地远端需 opt-in（client env 已带 RATIFLOW_SKILL_REGISTRY_LOCAL=1）。
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

    // --- 7. WP-8a：suggestion/observation 基础设施（RPC 面；写侧随 WP-8 fast-track /
    //     WP-12 automation tick 领域路径落地，其全链 e2e 由 WP-8 的 gate-skip-e2e 承接）---
    {
      // 空观察面（fast_track 源此刻必空——automation 源已由 WP-12 场景之前的 shadow tick 产数）。
      const empty = await c.call('automation.observations', { source: 'fast_track' });
      assert(Array.isArray(empty.items) && empty.items.length === 0, '空观察面返回空 items');
      assert(empty.stats && empty.stats.total === 0 && empty.stats.decided === 0, 'stats 零值形状');
      const bySource = await c.call('automation.observations', { source: 'automation' });
      assert(Array.isArray(bySource.items) && bySource.items.length >= 2, 'automation 源含既有 shadow tick 建议');
      await expectErrorContains(
        () => c.call('automation.observations', { source: 'magic' }),
        'shadow_suggestion_invalid',
        '非法 source 拒绝',
      );
      // 不存在建议 → NotFound 族。
      await expectErrorContains(
        () => c.call('automation.decideSuggestion', { suggestionId: 'shs_ghost', decision: 'accepted', decidedBy: 'owner', note: '' }),
        'shadow_suggestion_missing',
        '不存在建议决定拒绝',
      );
      // 非法 decision 枚举。
      await expectErrorContains(
        () => c.call('automation.decideSuggestion', { suggestionId: 'shs_ghost', decision: 'maybe', decidedBy: 'owner', note: '' }),
        'shadow_decision_invalid',
        '非法 decision 枚举拒绝',
      );
      // 复核前置：建议须已决定（不存在 → shadow_suggestion_missing）。
      await expectErrorContains(
        () => c.call('automation.reviewSuggestion', { suggestionId: 'shs_ghost', falsePositive: true, reviewer: 'qa', note: '' }),
        'shadow_suggestion_missing',
        '复核不存在建议拒绝',
      );
      // Flag 关闭：决定拒绝、观察面照常可读。
      {
        const cOff = new CoreClient(dataDir);
        try {
          await expectErrorContains(
            () => cOff.call('automation.decideSuggestion', { suggestionId: 'shs_x', decision: 'accepted', decidedBy: 'o', note: '' }),
            'feature_disabled',
            'Flag 关闭：decideSuggestion 拒绝',
          );
          const obsOff = await cOff.call('automation.observations', {});
          assert(Array.isArray(obsOff.items), 'Flag 关闭：observations 读面可用');
        } finally {
          cOff.kill();
        }
      }
    }

    // --- 8. WP-12：shadow policy——shadow tick 只产建议不执行；人工切 live 过
    //     观察门槛（29/30 边界）；误报率超阈值自动回 shadow + 通知 ---
    {
      const grant3 = await c.call('autonomy.createGrant', {
        workItemId: wi.id,
        allowedTools: ['read_file', 'search_knowledge'],
        allowedRisks: ['low'],
        expiresAt: '2099-01-01T00:00:00.000Z',
      });
      const autoW12 = await c.call('automation.create', {
        key: 'auto-wp12',
        workItemId: wi.id,
        intent: { kind: 'goal', workItemId: wi.id, goal: '巡检' },
        intervalSecs: 60,
        autonomyGrantId: grant3.grantId,
      });
      assert(autoW12.shadow_mode === true, '新建自动化默认 shadow_mode=1');
      // 33 个 shadow tick（不同 scheduledFor）：只产建议不执行。
      const tick = (i) => c.call('automation.runNow', { automationId: autoW12.id, scheduledFor: `2026-09-08T12:${String(i).padStart(2, '0')}:00.000Z` });
      for (let i = 0; i < 33; i++) {
        const r = await tick(i);
        if (r.status !== 'shadowed') throw new Error(`tick ${i} 状态 ${r.status}，期望 shadowed`);
      }
      assert(true, '33 次 shadow tick 全部 shadowed（无副作用）');
      const obs = await c.call('automation.observations', { source: 'automation', automationId: autoW12.id });
      assert(obs.items.length === 33, `33 条 shadow 建议在册（实际 ${obs.items.length}）`);
      // 决定前：切 live 被门槛拒（decided=0<30）。
      await expectErrorContains(
        () => c.call('automation.setShadowMode', { automationId: autoW12.id, shadowMode: false, expectedRevision: autoW12.revision }),
        'automation_shadow_gate',
        '未达观察门槛不可切 live',
      );
      // 决定 29 条：27 accepted + 2 rejected（1 条 fp=1 复核）→ 29<30 仍拒。
      const toDecide = obs.items.map((x) => x.id);
      for (let i = 0; i < 29; i++) {
        const decision = i < 27 ? 'accepted' : 'rejected';
        await c.call('automation.decideSuggestion', { suggestionId: toDecide[i], decision, decidedBy: 'owner', note: '' });
        if (i === 28) {
          await c.call('automation.reviewSuggestion', { suggestionId: toDecide[i], falsePositive: true, reviewer: 'qa', note: '误报' });
        }
      }
      await expectErrorContains(
        () => c.call('automation.setShadowMode', { automationId: autoW12.id, shadowMode: false, expectedRevision: autoW12.revision }),
        'automation_shadow_gate',
        '29 条决定 <min_sample 仍拒（边界 29/30）',
      );
      // 第 30 条：rejected + fp=1 → decided=30、fp=2（6.7%≤10%）→ 切 live 成功。
      await c.call('automation.decideSuggestion', { suggestionId: toDecide[29], decision: 'rejected', decidedBy: 'owner', note: '' });
      await c.call('automation.reviewSuggestion', { suggestionId: toDecide[29], falsePositive: true, reviewer: 'qa', note: '误报' });
      const live = await c.call('automation.setShadowMode', { automationId: autoW12.id, shadowMode: false, expectedRevision: autoW12.revision });
      assert(live.shadowMode === false, '30 条决定且误报率≤阈值 → 切 live 成功');
      // live tick：恢复真实执行（intent_created）。
      const liveTick = await tick(40);
      assert(liveTick.status === 'intent_created', `live tick 恢复执行（实际 ${liveTick.status}）`);
      // 误报率抬升：剩余 3 条全 rejected + fp=1 → 5/33>10% → 下次 tick 自动回 shadow。
      for (let i = 30; i < 33; i++) {
        await c.call('automation.decideSuggestion', { suggestionId: toDecide[i], decision: 'rejected', decidedBy: 'owner', note: '' });
        await c.call('automation.reviewSuggestion', { suggestionId: toDecide[i], falsePositive: true, reviewer: 'qa', note: '误报' });
      }
      const fb = await tick(41);
      assert(fb.status === 'shadow_fallback', `误报率超阈值 → 自动回 shadow（实际 ${fb.status}）`);
      const afterFb = await tick(42);
      assert(afterFb.status === 'shadowed', '回退后 tick 恢复 shadow 语义');
      // （通知断言移至 WP-12 场景）
      const notes = (await c.call('notification.list', {})).items;
      assert(notes.some((n) => n.kind === 'automation_blocked' && JSON.stringify(n.payload ?? {}).includes('shadow_fallback')), '回退落 automation_blocked 通知');
      console.log('场景三（WP-12 shadow policy）通过');
    }

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
