#!/usr/bin/env node
// 契约双端校验（TS 侧）：全部 fixture 经 decoders 解码；错误族与 errors.json 对齐；秘密探针。
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
  try {
    if (file.startsWith('settings-summary.')) decodeSettingsSummary(body);
    else if (file.startsWith('model-profile.')) check(Array.isArray(body.items) && body.items[0].revision >= 1, `${file} items.revision`);
    else if (/^ssh\.|^connection-test\./.test(file)) decodeTestReport(body);
    else if (file.startsWith('backup.running')) decodeOperation(body.operation);
    else if (file.startsWith('backup.corrupt')) check(body.verified === false && body.problems[0].code === 'BACKUP_CORRUPT', `${file} problems`);
    else if (file.startsWith('audit.')) check(body.entry.metadataRedacted === true, `${file} redacted 标记`);
    else if (file.startsWith('memory-settings.')) { const s = decodeMemorySettings(body); check(s.enabled === true && s.maxEntries >= 1 && s.maxEntries <= 32, `${file} 设置范围`); }
    else if (file.startsWith('memory-list.')) { const l = decodeMemoryList(body); check(l.items.some((i) => i.status === 'conflicted') && l.items.some((i) => i.status === 'proposed') && l.items.some((i) => i.status === 'archived'), `${file} 混合状态覆盖`); }
    else if (file.startsWith('memory-detail.active.')) { const d = decodeMemoryDetail(body); check(typeof d.body === 'string' && d.sources.length >= 1 && d.revisions.length >= 1, `${file} active 正文与来源`); }
    else if (file.startsWith('memory-detail.purged.')) { const d = decodeMemoryDetail(body); check(d.body === null && d.objectSha256 === null && d.contentSha256.length > 0, `${file} 墓碑语义`); }
    else if (file.startsWith('memory-context-preview.')) { const p = decodeMemoryContextPreview(body); check(p.included.length > 0 && p.excluded.length > 0 && p.manifestFrozen === false, `${file} included/excluded 双侧与预览非冻结`); }
    else if (file.startsWith('memory-capture.')) { const c = decodeMemoryCaptureJob(body); check(c.job.status === 'unknown' && c.candidates.length === 0, `${file} unknown 无候选`); }
    else if (file.startsWith('memory-purge-preview.')) { const pv = decodeMemoryPurgePreview(body); check(pv.canPurge === false && pv.blockers.length > 0 && pv.confirmationToken === null && pv.backupRefs.likelyContained >= 0, `${file} blocked 与备份边界`); }
    check(true, `${file} 解码通过`);
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

// 秘密探针：fixture 全文不得出现疑似真实秘密（占位形态允许）。
const banned = [/sk-[A-Za-z0-9]{20,}/, /ghp_[A-Za-z0-9]{30,}/, /glpat-[A-Za-z0-9\-]{20,}/, /AKIA[0-9A-Z]{16}/, /-----BEGIN [A-Z ]*PRIVATE KEY-----/];
for (const file of [...readdirSync(fixturesDir).filter((f) => f.endsWith('.json')).map((f) => join(fixturesDir, f)), ...errorFiles.map((f) => join(fixturesDir, 'errors', f))]) {
  const text = readFileSync(file, 'utf8');
  check(!banned.some((re) => re.test(text)), `${file} 无真实秘密形态`);
}

console.log(`contract(ts): ${pass} 通过，${fail} 失败`);
process.exit(fail === 0 ? 0 : 1);
