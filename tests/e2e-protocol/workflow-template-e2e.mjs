#!/usr/bin/env node
// 数据化工作流模板协议级 E2E（EvoFlow 方案 M1-10 / ADR-036）：
// 默认六关 parity、Flag 门控、3 关模板创建/激活/冻结实例、模板升级不改既有实例（EV-002）、
// 非法模板拒绝激活（EV-004）、未开工实例迁移与运行事实拒绝迁移、护照按实例序列签发。
// 前置：cargo build --release -p ratiflow-core。
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
  if (!c) throw new Error(`workflow-template-e2e 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function expectErrorContains(call, token, label) {
  try {
    await call();
  } catch (e) {
    assert(String(e.message).includes(token), `${label}（实际 ${e.message.slice(0, 80)}）`);
    return;
  }
  throw new Error(`workflow-template-e2e 断言失败：${label} 期望错误含 ${token}，但调用成功`);
}

async function activeKeys(client, workItemId) {
  const cov = await client.call('trace.coverage', { workItemId });
  return (cov.items ?? []).filter((i) => i.status === 'active').map((i) => i.requirementKey);
}

const THREE_GATES = (a, b, c) => [
  { gateId: a, title: `关-${a}`, purpose: '第一关', deliverables: ['doc'] },
  { gateId: b, title: `关-${b}`, purpose: '第二关', deliverables: ['code'] },
  { gateId: c, title: `关-${c}`, purpose: '第三关', deliverables: ['verification'] },
];

async function main() {
  // ============ 场景一：Flag 显式关闭（默认开，=0 为 kill switch）============
  {
    const dataDir = mkdtempSync(join(tmpdir(), 'sg-wftpl-off-'));
    const c = new CoreClient(dataDir, { RATIFLOW_WORKFLOW_TEMPLATE_V2: '0' });
    try {
      await c.call('project.create', { gitlabInstance: 'local', namespace: 'e2e', project: 'flagoff', name: 'FlagOff' });
      const wi = await c.call('workitem.create', { projectId: (await c.call('project.list', {})).items[0].id, title: '默认六关' });
      assert(c.hello.schemaVersion >= 32, `Flag 关闭：schemaVersion ≥ 32（实际 ${c.hello.schemaVersion}）`);
      assert(wi.current_gate === 'requirements', 'Flag 关闭：默认六关行为不变');
      await expectErrorContains(() => c.call('workflowTemplate.list', {}), 'feature_disabled', 'Flag 关闭：模板 RPC 拒绝');
      const pjOff = (await c.call('project.list', {})).items[0].id;
      await expectErrorContains(
        () => c.call('workitem.create', { projectId: pjOff, title: 'x', templateId: 'hotfix-3' }),
        'feature_disabled',
        'Flag 关闭：非默认模板创建拒绝',
      );
      console.log('场景一（Flag 关闭 parity）通过');
    } finally {
      c.kill();
      rmSync(dataDir, { recursive: true, force: true });
    }
  }

  // ============ 场景二：Flag 开启（完整生命周期）============
  {
    const dataDir = mkdtempSync(join(tmpdir(), 'sg-wftpl-on-'));
    const c = new CoreClient(dataDir, { RATIFLOW_WORKFLOW_TEMPLATE_V2: '1' });
    try {
      await c.call('project.create', { gitlabInstance: 'local', namespace: 'e2e', project: 'tpl', name: 'Tpl' });
      const pj = (await c.call('project.list', {})).items[0].id;

      // 1) 内置默认模板 parity。
      const list = await c.call('workflowTemplate.list', {});
      const builtin = list.items.find((t) => t.key === 'six-gate-default');
      assert(!!builtin, '内置 six-gate-default 存在');
      assert(builtin.versions.some((v) => v.version_no === 1 && v.status === 'active'), '内置模板 v1 active');
      const builtinDetail = await c.call('workflowTemplate.get', { templateId: 'six-gate-default' });
      assert(builtinDetail.activeVersion.gates.length === 6, '默认模板 6 关定义');
      assert(builtinDetail.activeVersion.gates[5].gate_id === 'verification', '默认模板顺序以 verification 收尾');

      // 2) 3 关热修复模板：draft → activate → 实例冻结。
      const created = await c.call('workflowTemplate.create', {
        key: 'hotfix-3',
        name: '三关热修复',
        gates: THREE_GATES('triage', 'fix', 'confirm'),
        idempotencyKey: 'e2e-3g-1',
      });
      assert(created.version.status === 'draft', '新建版本为 draft');
      const activated = await c.call('workflowTemplate.activate', { versionId: created.version.id, idempotencyKey: 'e2e-act-1' });
      assert(activated.status === 'active', '激活成功');
      // EV-004：非法模板（重复 gateId）拒绝。
      await expectErrorContains(
        () => c.call('workflowTemplate.create', { key: 'bad-tpl', name: 'bad', gates: [{ gateId: 'dup', title: 'a', deliverables: ['doc'] }, { gateId: 'dup', title: 'b', deliverables: ['doc'] }], idempotencyKey: 'e2e-bad' }),
        'workflow_template_invalid',
        '重复 gate_id 拒绝（EV-004）',
      );

      const wi3 = await c.call('workitem.create', { projectId: pj, title: '热修复任务', templateId: 'hotfix-3' });
      const inst = await c.call('workflow.getInstance', { workItemId: wi3.id });
      assert(inst.gates.length === 3, '实例冻结 3 关');
      assert(inst.instance.current_gate_id === 'triage', '当前关为第一关');
      assert(inst.gates[1].deliverables[0] === 'code', '实例定义携带交付物 kind');

      // 3) 3 关全流程：交付物门禁 + 评估 + 放行推进 + 护照按实例序列。
      const keys = await activeKeys(c, wi3.id);
      assert(keys.length >= 1, '默认需求文档落盘产生 REQ 项');
      for (const g of inst.gates) {
        const kind = g.deliverables[0];
        const art = await c.call('artifact.create', { workItemId: wi3.id, kind, title: g.title });
        const rev = await c.call('artifact.createDraft', { artifactId: art.id, content: `# ${g.title} 产物\n- [${keys[0]}] 覆盖`, requirementKeys: keys });
        await c.call('artifact.addReview', { revisionId: rev.id, reviewer: 't', verdict: 'approved' });
        await c.call('artifact.freezeBaseline', { workItemId: wi3.id, gate: g.gate_id, revisionIds: [rev.id] });
        const ev = await c.call('evidence.record', { workItemId: wi3.id, gate: g.gate_id, kind: 'manual', title: `${g.title} 核验`, source: 'local', requirementKeys: keys });
        await c.call('evidence.verify', { evidenceId: ev.id, verifiedBy: 'qa' });
        await c.call('gate.evaluate', { workItemId: wi3.id, gate: g.gate_id });
        const rr = await c.call('gate.requestRelease', { workItemId: wi3.id, gate: g.gate_id });
        await c.call('gate.decideRelease', { approvalId: rr.approval_id, decision: 'approved', decidedBy: 'owner', reason: 'E2E' });
      }
      const wi3After = (await c.call('workflow.getInstance', { workItemId: wi3.id })).instance;
      assert(wi3After.current_gate_id === 'confirm', '前两关放行后推进至最后一关');
      const passport = await c.call('passport.issue', { workItemId: wi3.id });
      assert(passport.gates.length === 3, `护照按实例序列签发 3 关（实际 ${passport.gates.length}）`);

      // 4) 模板升级不改既有实例（EV-002）：同 key 追加 draft v2 → 激活 → 旧 active deprecated。
      const v2b = await c.call('workflowTemplate.create', {
        key: 'hotfix-3',
        name: '三关热修复',
        gates: THREE_GATES('triage', 'fix2', 'confirm'),
        idempotencyKey: 'e2e-3g-2',
      });
      assert(v2b.version.version_no >= 2, `同 key 追加为后续 draft 版本（实际 v${v2b.version.version_no}）`);
      await c.call('workflowTemplate.activate', { versionId: v2b.version.id, idempotencyKey: 'e2e-act-2' });
      const instAfterUpgrade = await c.call('workflow.getInstance', { workItemId: wi3.id });
      assert(
        instAfterUpgrade.instance.template_version_id === inst.instance.template_version_id,
        '模板升级后既有实例仍冻结原版本（EV-002）',
      );
      const wi3b = await c.call('workitem.create', { projectId: pj, title: 'v2 任务', templateId: 'hotfix-3' });
      const inst3b = await c.call('workflow.getInstance', { workItemId: wi3b.id });
      assert(inst3b.instance.template_version_id === v2b.version.id, '新实例冻结新版本');
      assert(inst3b.gates.some((g) => g.gate_id === 'fix2'), '新实例使用新关定义');

      // 5) 实例迁移（2 关热修复模板为目标）：未开工可迁移；已有运行事实拒绝。
      const twoGate = await c.call('workflowTemplate.create', {
        key: 'hotfix-2',
        name: '两关热修复',
        gates: [
          { gateId: 'fix', title: '修复关', purpose: '', deliverables: ['code'] },
          { gateId: 'confirm', title: '确认关', purpose: '', deliverables: ['verification'] },
        ],
        idempotencyKey: 'e2e-2g-1',
      });
      await c.call('workflowTemplate.activate', { versionId: twoGate.version.id, idempotencyKey: 'e2e-act-3' });
      const wi4 = await c.call('workitem.create', { projectId: pj, title: '待迁移任务', templateId: 'hotfix-3' });
      const wi4Inst = await c.call('workflow.getInstance', { workItemId: wi4.id });
      const preview = await c.call('workflow.migrationPreview', { workItemId: wi4.id, targetVersionId: twoGate.version.id });
      assert(preview.blocked === false, '未开工任务迁移预览放行');
      const migrated = await c.call('workflow.migrate', { workItemId: wi4.id, targetVersionId: twoGate.version.id, idempotencyKey: 'e2e-mig-1' });
      assert(migrated.state === 'migrated' && migrated.current_gate_id === 'fix', '迁移成功且指向新版本首关');
      const wi4After = await c.call('workflow.getInstance', { workItemId: wi4.id });
      assert(wi4After.gates.length === 2 && wi4After.instance.template_version_id === twoGate.version.id, '迁移后实例按 2 关定义运行');
      void wi4Inst;
      const wi3Preview = await c.call('workflow.migrationPreview', { workItemId: wi3.id, targetVersionId: twoGate.version.id });
      assert(wi3Preview.blocked === true, '已有运行事实 → 预览标注 blocked');
      await expectErrorContains(
        () => c.call('workflow.migrate', { workItemId: wi3.id, targetVersionId: twoGate.version.id, idempotencyKey: 'e2e-mig-2' }),
        'workflow_migration_blocked',
        '已有运行事实 → 迁移拒绝',
      );

      // 6) 默认模板 parity：Flag 开启下默认六关任务照常走放行推进。
      const wiDefault = await c.call('workitem.create', { projectId: pj, title: '默认模板任务' });
      const instDefault = await c.call('workflow.getInstance', { workItemId: wiDefault.id });
      assert(instDefault.gates.length === 6, '未指定模板 → 默认六关实例');
      assert(instDefault.gates.map((g) => g.gate_id).join(',') === 'requirements,design,development,testing,deployment,verification', '默认顺序逐字一致');

      // 7) 多交付物配置化：一关声明两个 kind，缺一不放行、全冻才过（deliverableStatus 全量透出）。
      const multi = await c.call('workflowTemplate.create', {
        key: 'multi-dlv',
        name: '多交付物模板',
        gates: [{ gateId: 'build', title: '构建关', purpose: '', deliverables: ['spec', 'patch'] }],
        idempotencyKey: 'e2e-md-1',
      });
      await c.call('workflowTemplate.activate', { versionId: multi.version.id, idempotencyKey: 'e2e-act-4' });
      const wiM = await c.call('workitem.create', { projectId: pj, title: '多交付物任务', templateId: 'multi-dlv' });
      const keysM = await activeKeys(c, wiM.id);
      // deliverableStatus：requiredKinds 全量 + entries 逐 kind。
      const st0 = await c.call('gate.deliverableStatus', { workItemId: wiM.id, gate: 'build' });
      assert(JSON.stringify(st0.requiredKinds) === JSON.stringify(['spec', 'patch']), 'requiredKinds 全量透出');
      assert(st0.entries.length === 2 && st0.entries.every((e) => e.missing === 'artifact_absent'), '逐 kind 明细：均缺工件');
      // 只备 spec → 放行拒绝并指名 patch。
      const mkFrozen = async (kind, content) => {
        const art = await c.call('artifact.create', { workItemId: wiM.id, kind, title: kind });
        const rev = await c.call('artifact.createDraft', { artifactId: art.id, content, requirementKeys: keysM });
        await c.call('artifact.addReview', { revisionId: rev.id, reviewer: 't', verdict: 'approved' });
        return rev;
      };
      const specRev = await mkFrozen('spec', `# spec\n- [${keysM[0]}] x`);
      await c.call('artifact.freezeBaseline', { workItemId: wiM.id, gate: 'build', revisionIds: [specRev.id] });
      const st1 = await c.call('gate.deliverableStatus', { workItemId: wiM.id, gate: 'build' });
      assert(st1.satisfied === false, 'patch 缺失 → 整体不满足');
      const patchEntry = st1.entries.find((e) => e.kind === 'patch');
      assert(patchEntry.missing === 'artifact_absent', '缺失指向 patch（按名指明）');
      // 技术评估先过（放行前置顺序：评估 → 交付物），再验证交付物缺一不放行。
      const evM = await c.call('evidence.record', { workItemId: wiM.id, gate: 'build', kind: 'manual', title: '构建核验', source: 'local', requirementKeys: keysM });
      await c.call('evidence.verify', { evidenceId: evM.id, verifiedBy: 'qa' });
      await c.call('gate.evaluate', { workItemId: wiM.id, gate: 'build' });
      await expectErrorContains(
        () => c.call('gate.requestRelease', { workItemId: wiM.id, gate: 'build' }),
        'patch',
        '多交付物缺一 → 放行拒绝（指名缺失 kind）',
      );
      // patch 补齐；spec 旧修订已冻结不可复用 → spec 出 v2 新修订，两 kind 同基线冻结 → 放行。
      const patchRev = await mkFrozen('patch', `# patch\n- [${keysM[0]}] x`);
      const specRev2 = await mkFrozen('spec', `# spec v2\n- [${keysM[0]}] x`);
      await c.call('artifact.freezeBaseline', { workItemId: wiM.id, gate: 'build', revisionIds: [specRev2.id, patchRev.id] });
      const st2 = await c.call('gate.deliverableStatus', { workItemId: wiM.id, gate: 'build' });
      assert(st2.satisfied === true, '全部 kind 冻结 → 满足');

      console.log('场景二（Flag 开启完整生命周期）通过');
    } finally {
      c.kill();
      rmSync(dataDir, { recursive: true, force: true });
    }
  }

  console.log('工作流模板协议 E2E 通过。');
}

main().catch((e) => {
  console.error('workflow-template-e2e 失败：', e.message);
  process.exit(1);
});
