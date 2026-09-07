#!/usr/bin/env node
// 契约双端校验（TS 侧）：全部 fixture 经 decoders 解码；坏 payload 必须被拒（负例矩阵）；
// fixture 命名必须命中解码分支（防死 fixture）；错误族与 errors.json 对齐；秘密探针。
import { readFileSync, readdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { tmpdir } from 'node:os';
import { buildSync } from 'esbuild';

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');

// 现场把 decoders.ts 打包为临时 ESM 再 import（避免 TS 直跑）。
const outDir = mkdtempSync(join(tmpdir(), 'sg-contract-'));
writeFileSync(join(outDir, 'package.json'), '{"type":"module"}');
buildSync({
  entryPoints: [join(root, 'packages/protocol/src/decoders.ts')],
  bundle: true, format: 'esm', target: 'node18',
  outfile: join(outDir, 'decoders.mjs'),
});
const { decodeSettingsSummary, decodeTestReport, decodeOperation, decodeError,
  decodeMemorySettings, decodeMemoryList, decodeMemoryDetail, decodeMemoryContextPreview,
  decodeMemoryCaptureJob, decodeMemoryPurgePreview } =
  await import(join(outDir, 'decoders.mjs'));

let pass = 0;
let fail = 0;
function check(cond, label) {
  if (cond) { pass++; }
  else { fail++; console.error(`  ✕ ${label}`); }
}

const fixturesDir = join(root, 'contracts/fixtures');
for (const file of readdirSync(fixturesDir)) {
  if (!file.endsWith('.json')) continue;
  const body = JSON.parse(readFileSync(join(fixturesDir, file), 'utf8'));
  let matched = false;
  try {
    if (file.startsWith('settings-summary.')) { matched = true; decodeSettingsSummary(body); }
    else if (file.startsWith('model-profile.')) { matched = true; check(Array.isArray(body.items) && body.items[0].revision >= 1, `${file} items.revision`); }
    else if (/^ssh\.|^connection-test\./.test(file)) { matched = true; decodeTestReport(body); }
    else if (file.startsWith('backup.running')) { matched = true; decodeOperation(body.operation); }
    else if (file.startsWith('backup.corrupt')) { matched = true; check(body.verified === false && body.problems[0].code === 'BACKUP_CORRUPT', `${file} problems`); }
    else if (file.startsWith('audit.')) { matched = true; check(body.entry.metadataRedacted === true, `${file} redacted 标记`); }
    else if (file.startsWith('memory-settings.')) { matched = true; const s = decodeMemorySettings(body); check(s.enabled === true && s.maxEntries >= 1 && s.maxEntries <= 32, `${file} 设置范围`); }
    else if (file.startsWith('memory-list.')) { matched = true; const l = decodeMemoryList(body); check(l.items.some((i) => i.status === 'conflicted') && l.items.some((i) => i.status === 'proposed') && l.items.some((i) => i.status === 'archived'), `${file} 混合状态覆盖`); }
    else if (file.startsWith('memory-detail.active.')) { matched = true; const d = decodeMemoryDetail(body); check(typeof d.body === 'string' && d.sources.length >= 1 && d.revisions.length >= 1, `${file} active 正文与来源`); }
    else if (file.startsWith('memory-detail.purged.')) { matched = true; const d = decodeMemoryDetail(body); check(d.body === null && d.objectSha256 === null && d.contentSha256.length > 0, `${file} 墓碑语义`); }
    else if (file.startsWith('memory-context-preview.')) { matched = true; const p = decodeMemoryContextPreview(body); check(p.included.length > 0 && p.excluded.length > 0 && p.manifestFrozen === false, `${file} included/excluded 双侧与预览非冻结`); }
    else if (file.startsWith('memory-capture.')) { matched = true; const c = decodeMemoryCaptureJob(body); check(c.job.status === 'unknown' && c.candidates.length === 0, `${file} unknown 无候选`); }
    else if (file.startsWith('memory-purge-preview.')) { matched = true; const pv = decodeMemoryPurgePreview(body); check(pv.canPurge === false && pv.blockers.length > 0 && pv.confirmationToken === null && pv.backupRefs.likelyContained >= 0, `${file} blocked 与备份边界`); }
    if (!matched) {
      check(false, `${file} 无解码器分支匹配（fixture 命名漂移 → 死 fixture，恒绿假象）`);
    } else {
      check(true, `${file} 解码通过`);
    }
  } catch (error) {
    check(false, `${file} 解码失败: ${error.message}`);
  }
}

const errorFiles = readdirSync(join(fixturesDir, 'errors'));
const families = JSON.parse(readFileSync(join(root, 'contracts/rpc/errors.json'), 'utf8')).families;
const allCodes = new Set(Object.values(families).flatMap((f) => f.codes));
for (const file of errorFiles) {
  const body = JSON.parse(readFileSync(join(fixturesDir, 'errors', file), 'utf8'));
  try {
    decodeError(body);
    check(allCodes.has(body.code), `${file} 错误码 ${body.code} 在 errors.json 族内`);
  } catch (error) {
    check(false, `${file}: ${error.message}`);
  }
}

// --- 负例矩阵：坏 payload 必须被拒（解码器一旦放宽，这里立即报警）。 ---
function firstFixture(prefix) {
  const f = readdirSync(fixturesDir).filter((n) => n.startsWith(prefix) && n.endsWith('.json')).sort()[0];
  if (!f) throw new Error(`找不到 ${prefix} 正向 fixture`);
  return JSON.parse(readFileSync(join(fixturesDir, f), 'utf8'));
}
function expectReject(decoder, makeBad, label) {
  let threw = false;
  try { decoder(makeBad()); } catch { threw = true; }
  check(threw, `负例必须被拒: ${label}`);
}

// ErrorEnvelope
expectReject(decodeError, () => ({ code: 'E_X', message: 'm' }), 'ErrorEnvelope 缺 retryable');
expectReject(decodeError, () => ({ code: 'E_X', message: 'm', retryable: 'yes' }), 'ErrorEnvelope.retryable 非布尔');
expectReject(decodeError, () => ({ code: 42, message: 'm', retryable: true }), 'ErrorEnvelope.code 非字符串');
expectReject(decodeError, () => ({ code: 'E_X', message: 'm', retryable: true, apiKey: 'sk-x' }), 'ErrorEnvelope 携带 apiKey');
expectReject(decodeError, () => ({ code: 'E_X', message: 'm', retryable: true, token: 't' }), 'ErrorEnvelope 携带 token');

// TestReport / StepResult
expectReject(decodeTestReport, () => ({ status: 'green', durationMs: 1, steps: [] }), 'TestReport.status 非法枚举');
expectReject(decodeTestReport, () => ({ status: 'ready', durationMs: '1', steps: [] }), 'TestReport.durationMs 非数字');
expectReject(decodeTestReport, () => ({ status: 'ready', durationMs: 1, steps: [{ name: 'a', status: 'unknown', durationMs: 1, errorCode: null }] }), 'Step.status 非法枚举');
expectReject(decodeTestReport, () => ({ status: 'ready', durationMs: 1, steps: [{ name: 'a', status: 'passed', durationMs: 1 }] }), 'Step.errorCode 必须显式存在');

// OperationInfo
expectReject(decodeOperation, () => ({ operationId: 'op_1', kind: 'backup', status: 'pending', cancellable: false, startedAt: 't' }), 'Operation.status 非法枚举');
expectReject(decodeOperation, () => ({ operationId: 'op_1', kind: 'backup', status: 'running', cancellable: 1, startedAt: 't' }), 'Operation.cancellable 非布尔');
expectReject(decodeOperation, () => ({ kind: 'backup', status: 'running', cancellable: false, startedAt: 't' }), 'Operation 缺 operationId');

// SettingsSummary
expectReject(decodeSettingsSummary, () => ({ ...firstFixture('settings-summary.'), overallStatus: 'ok' }), 'SettingsSummary.overallStatus 非法枚举');
expectReject(decodeSettingsSummary, () => { const o = firstFixture('settings-summary.'); delete o.dataSafety; return o; }, 'SettingsSummary 缺 dataSafety');
expectReject(decodeSettingsSummary, () => { const o = firstFixture('settings-summary.'); delete o.dataSafety.lastBackup; return o; }, 'SettingsSummary.lastBackup 必须显式存在');
expectReject(decodeSettingsSummary, () => { const o = firstFixture('settings-summary.'); o.blockers = [{ id: 'b', severity: 'fatal', titleKey: 'k', targetRoute: 'r' }]; return o; }, 'blocker.severity 非法枚举');

// MemorySettings
expectReject(decodeMemorySettings, () => ({ ...firstFixture('memory-settings.'), maxEntries: 0 }), 'MemorySettings.maxEntries 下界');
expectReject(decodeMemorySettings, () => ({ ...firstFixture('memory-settings.'), maxEntries: 33 }), 'MemorySettings.maxEntries 上界');
expectReject(decodeMemorySettings, () => ({ ...firstFixture('memory-settings.'), maxEntries: 2.5 }), 'MemorySettings.maxEntries 非整数');
expectReject(decodeMemorySettings, () => ({ ...firstFixture('memory-settings.'), maxBytes: 512 }), 'MemorySettings.maxBytes 下界');
expectReject(decodeMemorySettings, () => ({ ...firstFixture('memory-settings.'), staleAfterDays: 0 }), 'MemorySettings.staleAfterDays 下界');
expectReject(decodeMemorySettings, () => ({ ...firstFixture('memory-settings.'), captureMode: 'auto' }), 'MemorySettings.captureMode 非法枚举');
expectReject(decodeMemorySettings, () => ({ ...firstFixture('memory-settings.'), revision: 0 }), 'MemorySettings.revision 下界');

// MemoryList
expectReject(decodeMemoryList, () => { const o = firstFixture('memory-list.'); o.items[0].id = 'abc'; return o; }, 'MemoryList.id 缺 mem_ 前缀');
expectReject(decodeMemoryList, () => { const o = firstFixture('memory-list.'); o.items[0].kind = 'note'; return o; }, 'MemoryList.kind 非法枚举');
expectReject(decodeMemoryList, () => { const o = firstFixture('memory-list.'); o.items[0].status = 'deleted'; return o; }, 'MemoryList.status 非法枚举');
expectReject(decodeMemoryList, () => { const o = firstFixture('memory-list.'); o.items[0].revisionNo = 0; return o; }, 'MemoryList.revisionNo 下界');

// MemoryDetail
expectReject(decodeMemoryDetail, () => { const o = firstFixture('memory-detail.active.'); o.status = 'purged'; return o; }, 'MemoryDetail purged 必须墓碑化');
expectReject(decodeMemoryDetail, () => { const o = firstFixture('memory-detail.active.'); delete o.sources; return o; }, 'MemoryDetail 缺 sources');
expectReject(decodeMemoryDetail, () => { const o = firstFixture('memory-detail.active.'); o.sources = []; return o; }, 'MemoryDetail active 至少一条来源(MEM-018)');
expectReject(decodeMemoryDetail, () => { const o = firstFixture('memory-detail.active.'); o.body = null; return o; }, 'MemoryDetail 非 purged 不得空正文');
expectReject(decodeMemoryDetail, () => { const o = firstFixture('memory-detail.purged.'); delete o.objectSha256; return o; }, 'MemoryDetail purged 必须显式 objectSha256');
expectReject(decodeMemoryDetail, () => { const o = firstFixture('memory-detail.active.'); o.id = 'xyz'; return o; }, 'MemoryDetail.id 缺 mem_ 前缀');

// MemoryContextPreview
expectReject(decodeMemoryContextPreview, () => { const o = firstFixture('memory-context-preview.'); o.included[0].reason = 'because'; return o; }, 'ContextPreview.reason 非法枚举');
expectReject(decodeMemoryContextPreview, () => { const o = firstFixture('memory-context-preview.'); o.included[0].bytes = -1; return o; }, 'ContextPreview.bytes 下界');
expectReject(decodeMemoryContextPreview, () => { const o = firstFixture('memory-context-preview.'); delete o.policy; return o; }, 'ContextPreview 缺 policy');
expectReject(decodeMemoryContextPreview, () => { const o = firstFixture('memory-context-preview.'); o.included = 'all'; return o; }, 'ContextPreview.included 非数组');

// MemoryCaptureJob
expectReject(decodeMemoryCaptureJob, () => { const o = firstFixture('memory-capture.'); o.candidates = [{ id: 'c' }]; return o; }, 'Capture unknown 不得携带候选(MEM-021)');
expectReject(decodeMemoryCaptureJob, () => { const o = firstFixture('memory-capture.'); o.job.errorCode = 'TOOL_DENIED'; return o; }, 'Capture unknown 必须 MEMORY_* errorCode');
expectReject(decodeMemoryCaptureJob, () => { const o = firstFixture('memory-capture.'); o.job.jobId = 'job_1'; return o; }, 'Capture.jobId 缺 memjob_ 前缀');
expectReject(decodeMemoryCaptureJob, () => { const o = firstFixture('memory-capture.'); o.job.status = 'queued'; return o; }, 'Capture.status 非法枚举');

// MemoryPurgePreview
expectReject(decodeMemoryPurgePreview, () => { const o = firstFixture('memory-purge-preview.'); o.blockers = []; return o; }, 'Purge 不可清除必须给 blockers');
expectReject(decodeMemoryPurgePreview, () => { const o = firstFixture('memory-purge-preview.'); o.confirmationToken = 'tok'; return o; }, 'Purge 不可清除不得发放 token');
expectReject(decodeMemoryPurgePreview, () => { const o = firstFixture('memory-purge-preview.'); delete o.backupRefs; return o; }, 'Purge 必须声明备份边界');
expectReject(decodeMemoryPurgePreview, () => { const o = firstFixture('memory-purge-preview.'); o.memoryId = 'm1'; return o; }, 'Purge.memoryId 缺 mem_ 前缀');

// 秘密探针：fixture 全文不得出现疑似真实秘密（占位形态允许）。
const banned = [/sk-[A-Za-z0-9]{20,}/, /ghp_[A-Za-z0-9]{30,}/, /glpat-[A-Za-z0-9\-]{20,}/, /AKIA[0-9A-Z]{16}/, /-----BEGIN [A-Z ]*PRIVATE KEY-----/];
for (const file of [...readdirSync(fixturesDir).filter((f) => f.endsWith('.json')).map((f) => join(fixturesDir, f)), ...errorFiles.map((f) => join(fixturesDir, 'errors', f))]) {
  const text = readFileSync(file, 'utf8');
  check(!banned.some((re) => re.test(text)), `${file} 无真实秘密形态`);
}

console.log(`contract(ts): ${pass} 通过，${fail} 失败`);
process.exit(fail === 0 ? 0 : 1);
