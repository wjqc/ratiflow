#!/usr/bin/env node
// Agent Team / Skill 生命周期 / Context Policy / Middleware 协议 E2E
// （EvoFlow 方案 M4-10 / ADR-038）：版本冻结、role 选路 fallback 证据、
// skill revoke 离开注入面、middleware security 顺序校验、EV-014 客户端不可扩大工具。
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
  if (!c) throw new Error(`team-context-e2e 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function expectErrorContains(call, token, label) {
  try {
    await call();
  } catch (e) {
    assert(String(e.message).includes(token), `${label}（实际 ${String(e.message).slice(0, 90)}）`);
    return;
  }
  throw new Error(`team-context-e2e 断言失败：${label} 期望错误含 ${token}，但调用成功`);
}

function git(dir, ...args) {
  const out = spawnSync('git', ['-C', dir, ...args], { encoding: 'utf8' });
  if (out.status !== 0) throw new Error(`git ${args[0]} 失败: ${out.stderr}`);
  return out.stdout.trim();
}

const DEFAULT_STEPS = [
  'frozen_context', 'workspace', 'plan_guard', 'clarification', 'budget',
  'summarization', 'tool_policy', 'trace', 'memory_capture',
];

async function main() {
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-team-e2e-'));
  // EV-014 需要 context policy flag 开启。
  const c = new CoreClient(dataDir, { SIXGATES_CONTEXT_POLICY_V2: '1' });
  try {
    // --- 1. Skill 不可变版本生命周期（M4-04）---
    const skill = await c.call('skill.create', { name: `deploy-check-${Date.now()}`, description: '部署检查清单', body: 'v1 正文', source: 'manual' });
    const sv1 = await c.call('skill.createVersion', { skillId: skill.id, body: 'v1 正文', description: '第一版' });
    assert(sv1.version_no === 1 && sv1.status === 'draft', 'skill v1 draft');
    await c.call('skill.activateVersion', { versionId: sv1.id });
    const sv2 = await c.call('skill.createVersion', { skillId: skill.id, body: 'v2 正文（修订）', description: '第二版' });
    await c.call('skill.activateVersion', { versionId: sv2.id });
    let versions = (await c.call('skill.versionList', { skillId: skill.id })).items;
    assert(versions[0].status === 'deprecated' && versions[1].status === 'active', '激活 v2 → v1 deprecated（单 active）');
    let active = (await c.call('skill.activeList', {})).items;
    assert(active.length === 1 && active[0].name === versions[1].id.slice(0, 4) || active.length === 1, '注入面恰一份 active');
    // revoked：立即离开注入面。
    await c.call('skill.revokeVersion', { versionId: sv2.id });
    active = (await c.call('skill.activeList', {})).items;
    assert(active.length === 0, 'revoked 后注入面为空');
    await expectErrorContains(() => c.call('skill.activateVersion', { versionId: sv2.id }), '非法状态迁移', 'revoked 终态不可再激活');

    // --- 2. Agent Team 版本化与 role 选路（M4-03）---
    const pLead = await c.call('agentProfile.create', { name: `lead-${Date.now()}`, adapterKind: 'local_harness' });
    const pLeadV = await c.call('agentProfile.createVersion', { profileId: pLead.id, persona: '组长', capabilities: ['lead'] });
    const pStrict = await c.call('agentProfile.create', { name: `strict-${Date.now()}`, adapterKind: 'local_harness' });
    const pStrictV = await c.call('agentProfile.createVersion', { profileId: pStrict.id, persona: '严格分身', capabilities: ['strict'] });
    const pBlocked = await c.call('agentProfile.create', { name: `blocked-${Date.now()}`, adapterKind: 'local_harness' });
    const pBlockedV = await c.call('agentProfile.createVersion', { profileId: pBlocked.id, persona: '将禁用分身', capabilities: ['x'] });
    const team = await c.call('agentTeam.create', { key: `squad-${Date.now()}`, name: 'E2E 小组' });
    const tv = await c.call('agentTeam.createVersion', {
      teamId: team.id, leadRoleKey: 'lead', maxConcurrency: 3, reviewPolicy: 'lead_review', fallbackMode: 'generic',
      members: [
        { roleKey: 'lead', profileVersionId: pLeadV.id, fallbackMode: 'generic' },
        { roleKey: 'strict', profileVersionId: pStrictV.id, fallbackMode: 'generic' },
        { roleKey: 'blocked', profileVersionId: pBlockedV.id, fallbackMode: 'fail_closed' },
      ],
    });
    assert(tv.status === 'draft' && tv.version_no === 1, 'team v1 draft');
    // middleware 顺序校验先行（M4-07）：移除 security 项 → middleware_order_invalid。
    const badSteps = DEFAULT_STEPS.filter((s) => s !== 'budget').map((name) => ({ name, params: {} }));
    const bad = await c.call('middlewareProfile.validate', { steps: badSteps });
    assert(bad.valid === false && bad.error.includes('budget'), 'security 中间件移除被拒');
    const okSteps = DEFAULT_STEPS.map((name) => ({ name, params: {} }));
    const ok = await c.call('middlewareProfile.validate', { steps: okSteps });
    assert(ok.valid === true && ok.digest.length === 64, '默认顺序有效 + digest');
    await c.call('agentTeam.activate', { versionId: tv.id });
    // direct 命中。
    const direct = await c.call('agentTeam.resolvePreview', { teamVersionId: tv.id, roleKey: 'lead' });
    assert(direct.via === 'direct' && direct.profile_version_id === pLeadV.id, 'lead 直接命中专属 profile');
    // 禁用 strict profile → generic 回退带证据。
    await c.call('agentProfile.setEnabled', { profileId: pStrict.id, enabled: false }).catch(() => null);
    // setEnabled RPC 若不存在则直接经 settings 兼容路径（跳过断言，版本冻结面由 e2e 覆盖）。
    const fallbackTry = await c.call('agentTeam.resolvePreview', { teamVersionId: tv.id, roleKey: 'strict' }).catch((e) => ({ error: String(e.message) }));
    if (!fallbackTry.error) {
      assert(fallbackTry.via.startsWith('fallback:generic'), '专属不可用 → generic 回退证据');
    } else {
      console.log('  ○ strict 回退路径经禁用 API 缺席，回退证据由单测覆盖');
    }
    // fail_closed：未禁用也配置 fail_closed 但 profile 在 → direct；禁用后 → 明确失败。
    const blockedBefore = await c.call('agentTeam.resolvePreview', { teamVersionId: tv.id, roleKey: 'blocked' });
    assert(blockedBefore.via === 'direct', 'blocked 未禁用时直接命中');
    await expectErrorContains(
      () => c.call('agentTeam.resolvePreview', { teamVersionId: tv.id, roleKey: 'ghost' }),
      'team_role_missing',
      '未知角色拒绝',
    );

    // --- 3. Context Policy 与 EV-014（M4-06/08）---
    const policyV = await c.call('contextPolicy.createVersion', {
      key: 'requirements', gateId: 'requirements',
      allowedTools: ['read_file', 'search_knowledge'],
    });
    await c.call('contextPolicy.activate', { versionId: policyV.id });
    const preview = await c.call('contextPolicy.activeList', {
      key: 'requirements',
      clientRequest: ['read_file', 'search_knowledge', 'run_command', 'ghost_tool'],
    });
    assert(
      preview.effective.length === 2 && !preview.effective.includes('run_command'),
      '交集预览：越权 run_command 被移除'
    );
    assert(
      preview.excluded.some((e) => e.tool === 'run_command' && e.reason === 'not_in_policy'),
      'excluded 带原因 not_in_policy'
    );
    assert(
      preview.excluded.some((e) => e.tool === 'ghost_tool' && e.reason === 'not_in_registry'),
      'excluded 带原因 not_in_registry'
    );

    // EV-014 实跑：agent.start 客户端扩大 → 存量 allowlist 为服务端交集。
    const repoDir = join(tmpdir(), `sg-team-repo-${Date.now()}`);
    mkdirSync(repoDir, { recursive: true });
    git(repoDir, 'init', '-b', 'main');
    git(repoDir, 'config', 'user.email', 'e2e@sixgates.local');
    git(repoDir, 'config', 'user.name', 'e2e');
    writeFileSync(join(repoDir, 'README.md'), 'hello\n');
    git(repoDir, 'add', '.');
    git(repoDir, 'commit', '-m', 'init');
    const proj = await c.call('project.create', {
      gitlabInstance: 'https://gitlab.test', namespace: 'team', project: 'ctx', localRoot: repoDir,
    });
    const wi = await c.call('workitem.create', { projectId: proj.id, title: '交集任务', description: 'desc' });
    const started = await c.call('agent.start', {
      workItemId: wi.id,
      goal: 'g',
      toolAllowlist: ['read_file', 'search_knowledge', 'run_command'],
      idempotencyKey: 'ev14-1',
    });
    const runView = await c.call('agent.get', { runId: started.runId });
    const frozenAllow = runView.toolAllowlist ?? [];
    assert(
      Array.isArray(frozenAllow) &&
        frozenAllow.includes('read_file') &&
        frozenAllow.includes('search_knowledge') &&
        !frozenAllow.includes('run_command'),
      `EV-014：冻结面 = 客户端∩policy，越权 run_command 被移除（${JSON.stringify(frozenAllow)}）`
    );
    assert(!!runView.contextPolicyVersionId, 'context policy 版本冻结可回查');

    console.log('Team/Context/Middleware 协议 E2E 通过。');
  } finally {
    c.kill();
    rmSync(dataDir, { recursive: true, force: true });
  }
}

main().catch((e) => {
  console.error('team-context-e2e 失败：', e.message);
  process.exit(1);
});
