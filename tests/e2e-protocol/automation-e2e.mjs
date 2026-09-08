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

// P0-1：decide/review/setShadowMode 为 receipt 门控 mutation（每次调用唯一 key）。
let keySeq = 0;
const idem = (prefix) => `${prefix}-${++keySeq}`;

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
  // P0-5：live 消费链的 agent run 需要 fake 模型（run_intent → agent_run → terminal）。
  const scriptPath = join(tmpdir(), `sg-auto-script-${Date.now()}.json`);
  writeFileSync(scriptPath, JSON.stringify(
    Array.from({ length: 8 }, (_, i) => ({
      content: `{"action":"final","summary":"自动化巡检 ${i} 完成"}`, tokensIn: 5, tokensOut: 5,
    })),
  ));
  const c = new CoreClient(dataDir, {
    RATIFLOW_AUTOMATIONS: '1',
    RATIFLOW_SKILL_REGISTRY_LOCAL: '1',
    RATIFLOW_FAKE_MODEL_SCRIPT: scriptPath,
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
        () => c.call('automation.decideSuggestion', { suggestionId: 'shs_ghost', decision: 'accepted', decidedBy: 'owner', note: '', idempotencyKey: idem('dec-miss') }),
        'shadow_suggestion_missing',
        '不存在建议决定拒绝',
      );
      // 非法 decision 枚举。
      await expectErrorContains(
        () => c.call('automation.decideSuggestion', { suggestionId: 'shs_ghost', decision: 'maybe', decidedBy: 'owner', note: '', idempotencyKey: idem('dec-bad') }),
        'shadow_decision_invalid',
        '非法 decision 枚举拒绝',
      );
      // 复核前置：建议须已决定（不存在 → shadow_suggestion_missing）。
      await expectErrorContains(
        () => c.call('automation.reviewSuggestion', { suggestionId: 'shs_ghost', falsePositive: true, reviewer: 'qa', note: '', idempotencyKey: idem('rev-miss') }),
        'shadow_suggestion_missing',
        '复核不存在建议拒绝',
      );
      // Flag 关闭：决定拒绝、观察面照常可读。
      {
        const cOff = new CoreClient(dataDir);
        try {
          await expectErrorContains(
            () => cOff.call('automation.decideSuggestion', { suggestionId: 'shs_x', decision: 'accepted', decidedBy: 'o', note: '', idempotencyKey: 'dec-off' }),
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

    // --- 8. WP-12/P0-5：shadow policy v2——误报率分母=已复核已决（coverage 门槛）、
    //     两窗迟滞自动回退（CAS）、durable run_intent 消费链（run → terminal）---
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
      const toDecide = obs.items.map((x) => x.id);
      const decide = (i, decision, note = '') =>
        c.call('automation.decideSuggestion', { suggestionId: toDecide[i], decision, decidedBy: 'owner', note, idempotencyKey: idem(`w12dec${i}`) });
      const review = (i, fp, note = '') =>
        c.call('automation.reviewSuggestion', { suggestionId: toDecide[i], falsePositive: fp, reviewer: 'qa', note, idempotencyKey: idem(`w12rev${i}`) });

      // 决定前：切 live 被拒（reviewed=0）。
      await expectErrorContains(
        () => c.call('automation.setShadowMode', { automationId: autoW12.id, shadowMode: false, expectedRevision: autoW12.revision, idempotencyKey: idem('lg0') }),
        'automation_shadow_gate',
        '零复核不可切 live',
      );

      // P0-5 coverage 门槛：30 决定只复核 14（coverage 46.7%<50%）→ 拒。
      // 口径：28 accepted + 2 rejected，复核前 14 条（fp 全 0）。
      for (let i = 0; i < 28; i++) await decide(i, 'accepted');
      await decide(28, 'rejected');
      await decide(29, 'rejected');
      for (let i = 0; i < 14; i++) await review(i, false);
      await expectErrorContains(
        () => c.call('automation.setShadowMode', { automationId: autoW12.id, shadowMode: false, expectedRevision: autoW12.revision, idempotencyKey: idem('lg1') }),
        'automation_shadow_gate',
        'coverage<50% 不可切 live（分母=已复核已决）',
      );
      // 补齐复核至全覆盖（fp=2/30=6.7%≤10%）→ 切 live 成功。
      for (let i = 14; i < 30; i++) await review(i, i >= 28);
      const live = await c.call('automation.setShadowMode', { automationId: autoW12.id, shadowMode: false, expectedRevision: autoW12.revision, idempotencyKey: idem('lg2') });
      assert(live.shadowMode === false, '全覆盖复核且误报率≤阈值 → 切 live 成功');

      // P0-5 durable 消费链：live tick → run_intent 入队 → consumer（5s tick）
      // 经 agent.start 创建 run → 回填 run_id → 终态。轮询 history 直到 runId 落位。
      const liveTick = await tick(40);
      assert(liveTick.status === 'intent_created', `live tick 入队 intent（实际 ${liveTick.status}）`);
      const intentId = liveTick.note;
      let runId = '';
      for (let i = 0; i < 60 && !runId; i++) {
        await new Promise((r) => setTimeout(r, 500));
        const hist = (await c.call('automation.history', { automationId: autoW12.id })).items;
        const row = hist.find((h) => h.scheduledFor === '2026-09-08T12:40:00.000Z');
        if (row?.runId) runId = row.runId;
      }
      assert(!!runId, 'run_intent 消费后回填 runId（durable 链）');
      let run = null;
      for (let i = 0; i < 60; i++) {
        run = await c.call('agent.get', { runId });
        if (['completed_execution', 'failed', 'cancelled'].includes(run.status)) break;
        await new Promise((r) => setTimeout(r, 300));
      }
      assert(run && run.status === 'completed_execution', `agent run 终态（实际 ${run?.status}）`);
      const histRow = (await c.call('automation.history', { automationId: autoW12.id })).items
        .find((h) => h.scheduledFor === '2026-09-08T12:40:00.000Z');
      assert(histRow.runIntentId === intentId && histRow.runIntentState === 'consumed',
        `history 携带 intent/run（实际 ${JSON.stringify(histRow)}）`);

      // P0-5 两窗迟滞（每个窗口需不同 metric digest——同 digest 重复 tick 幂等跳过）：
      // 窗一：决定 30/31 为 rejected+fp → 32 决定 32 复核 fp=4（12.5%>10%）
      // → streak=1，tick 仍 live（不回退）。
      for (const i of [30, 31]) {
        await decide(i, 'rejected');
        await review(i, true, '误报');
      }
      const t41 = await tick(41);
      assert(t41.status === 'intent_created', `第一窗超阈值不回退（streak=1，实际 ${t41.status}）`);
      // 同 digest 幂等：同窗口重复评估不叠加 streak（下一个不同 digest 窗口才推进）。
      // 窗二（新 digest）：决定最后一条 32 为 rejected+fp → 33/33 fp=5（15.2%）→ 回退。
      await decide(32, 'rejected');
      await review(32, true, '误报');
      const t42 = await tick(42);
      assert(t42.status === 'shadow_fallback', `第二窗超阈值 → 自动回 shadow（实际 ${t42.status}）`);
      const afterFb = await tick(43);
      assert(afterFb.status === 'shadowed', '回退后 tick 恢复 shadow 语义');
      // cooldown：自动回退后人工切 live 被冷静期拒绝（安全迟滞）。
      const autoAfter = await c.call('automation.list', {});
      const rec = autoAfter.items.find((a) => a.id === autoW12.id);
      await expectErrorContains(
        () => c.call('automation.setShadowMode', { automationId: autoW12.id, shadowMode: false, expectedRevision: rec.revision, idempotencyKey: idem('lg3') }),
        'automation_cooldown',
        '回退 cooldown 内不可人工切 live',
      );
      // 通知幂等：shadow_fallback 通知恰一条（幂等键唯一）。
      const notes = (await c.call('notification.list', {})).items;
      const fbNotes = notes.filter((n) => n.kind === 'automation_blocked' && JSON.stringify(n.payload ?? {}).includes('shadow_fallback'));
      assert(fbNotes.length === 1, `回退只发一条通知（实际 ${fbNotes.length}）`);
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
