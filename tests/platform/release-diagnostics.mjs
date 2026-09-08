#!/usr/bin/env node
// R0 安全封锁诊断与证据清单（RDWS 审计 §10.1 R0 / runbook §2）：
// 在**剥离全部高风险 flag** 的干净环境下启动 core，逐一探针能力面必须处于关闭态
//（feature_disabled / manifest_v2_writer_disabled），并采集只读诊断计数器。
// 产出 JSON 证据（时间戳/版本/schema/flag 探针/计数器）——归档进发布记录。
// 任一探针返回"已启用"即失败（安全封锁被破坏）。前置：cargo build --release -p ratiflow-core。
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import * as readline from 'node:readline';

const CORE = process.env.CORE_BIN ?? join(process.cwd(), 'target', 'release', 'ratiflow-core');

// 高风险 flag 姿态（R0 安全封锁）：
// - 默认 OFF 的 flag：从子环境剥离（证明默认态，不是继承态）；
// - 默认 ON 的 kill switch（模板 v2 / 谱系写入）：显式置 0——R0 封锁不依赖"未设置"。
const STRIP_FLAGS = [
  'RATIFLOW_UNSAFE_EXEC', 'RATIFLOW_STRUCTURED_ACCEPTANCE',
  'RATIFLOW_REWORK', 'RATIFLOW_AUTOMATIONS', 'RATIFLOW_UNIFIED_RISK',
  'RATIFLOW_WORKITEM_FTS', 'RATIFLOW_KNOWLEDGE_MANIFEST_V2_WRITER', 'RATIFLOW_OBJECTS_GC_PRUNE',
  'RATIFLOW_CONTEXT_POLICY_V2', 'RATIFLOW_EXEC_MODE',
];
const LOCKDOWN_ZERO = ['RATIFLOW_WORKFLOW_TEMPLATE_V2', 'RATIFLOW_TRACE_WRITES'];

class CoreClient {
  constructor(dataDir) {
    const env = { ...process.env };
    for (const f of STRIP_FLAGS) delete env[f];
    for (const f of LOCKDOWN_ZERO) env[f] = '0';
    this.proc = spawn(CORE, ['app-server', '--data-dir', dataDir], { stdio: ['pipe', 'pipe', 'pipe'], env });
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

const report = {
  kind: 'ratiflow-r0-diagnostics',
  timestamp: new Date().toISOString(),
  coreVersion: null,
  schemaVersion: null,
  flagProbes: [],
  diagnostics: {},
};

function probe(name, token, fn) {
  return fn()
    .then(() => ({ name, ok: false, detail: '能力面意外可用（安全封锁被破坏）' }))
    .catch((e) => {
      const hit = String(e.message).includes(token);
      return { name, ok: hit, detail: hit ? '已封锁' : `非预期错误：${String(e.message).slice(0, 90)}` };
    });
}

async function main() {
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-r0diag-'));
  const projRoot = mkdtempSync(join(tmpdir(), 'sg-r0diag-proj-'));
  const c = new CoreClient(dataDir);
  try {
    const ver = await c.call('core.version');
    report.coreVersion = ver.version;
    report.schemaVersion = ver.schemaVersion;

    // localRoot 供 manifest 探针走到 v2-writer 门（root 解析在前）。
    const project = await c.call('project.create', { gitlabInstance: 'diag', namespace: 'r0', project: 'lockdown', name: 'R0 诊断', localRoot: projRoot });
    const wi = await c.call('workitem.create', { projectId: project.id, title: 'R0 flag 探针' });

    // ---- flag 负例探针（默认态必须全部关闭）----
    report.flagProbes.push(await probe('RATIFLOW_REWORK=0', 'feature_disabled',
      () => c.call('rework.preview', { workItemId: wi.id, targetGate: 'requirements', reasonCode: 'other' })));
    report.flagProbes.push(await probe('RATIFLOW_WORKFLOW_TEMPLATE_V2=0（kill switch 显式关）', 'feature_disabled',
      () => c.call('workitem.create', { projectId: project.id, title: 'x', templateId: 'nondefault-tpl' })));
    report.flagProbes.push(await probe('RATIFLOW_STRUCTURED_ACCEPTANCE=0', 'feature_disabled',
      () => c.call('workflowTemplate.create', {
        key: 'r0-structured', name: '结构化探针',
        gates: [{ gateId: 'g', title: 'g', deliverables: ['doc'], acceptance: [{ verifier: 'evidence_verified', evidence_kind: 'test_report', min_count: 1 }] }],
        idempotencyKey: 'r0-structured-1',
      })));
    report.flagProbes.push(await probe('RATIFLOW_WORKITEM_FTS=0', 'feature_disabled',
      () => c.call('workitem.searchRebuild', { idempotencyKey: 'r0-fts-1' })));
    report.flagProbes.push(await probe('RATIFLOW_KNOWLEDGE_MANIFEST_V2_WRITER=0', 'manifest_v2_writer_disabled',
      () => c.call('knowledge.manifestCreate', {
        projectId: project.id, opId: 'r0v2', kind: 'document', name: 'r0-v2-probe',
        body: '# R0 探针', expectedAbsent: true, contentOwner: 'alice',
        verificationPolicy: { intervalDays: 7, severity: 'warn' },
      })));

    // 执行模式：默认（无 flag）绝不落入 unsafe。
    const exec = await c.call('executor.settings.get', {});
    const mode = exec?.effective?.mode ?? exec?.mode ?? '';
    report.flagProbes.push({
      name: 'RATIFLOW_UNSAFE_EXEC=0',
      ok: mode !== 'unsafe',
      detail: `生效模式=${mode}${exec?.effective?.source ? '（来源 ' + exec.effective.source + '）' : ''}`,
    });

    // ---- 只读诊断计数器（R0 基线 + 持续监控输入）----
    const triage = await c.call('triage.list', {});
    report.diagnostics.triage = {
      unknownReconciliation: triage.unknownReconciliation ?? null,
      objectOrphans: triage.objectOrphans ?? null,
      knowledgeBlocked: (triage.knowledgeBlocked ?? []).length,
    };
    const metrics = await c.call('metrics.overview', { scope: 'global' });
    report.diagnostics.metrics = {
      orphanRate: metrics.orphanRate ?? null,
      loopRate: metrics.loopRate ?? null,
      aiSuggestionAdoption: metrics.aiSuggestionAdoption ?? null,
    };
    // dirty main workspace：探针恒 0 的执行面在 dirty-main-probe-e2e（此处引用计数约定）。
    report.diagnostics.dirtyMainWorkspaceCounter = 'per dirty-main-probe-e2e.mjs（必须恒 0）';
    // 数据目录哈希在归档时对 db 文件计算（本脚本用的是临时目录，无归档价值）。

    const failed = report.flagProbes.filter((p) => !p.ok);
    console.log(JSON.stringify(report, null, 2));
    if (failed.length > 0) {
      console.error(`✕ R0 诊断失败（${failed.length} 项探针未处封锁态）`);
      process.exit(1);
    }
    console.error('R0 安全封锁诊断通过（全部能力面关闭，证据清单如上）。');
  } finally {
    c.kill();
    rmSync(dataDir, { recursive: true, force: true });
  }
}

main().catch((e) => {
  console.error('release-diagnostics 失败：', e.message);
  process.exit(1);
});
