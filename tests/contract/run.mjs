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
const { decodeSettingsSummary, decodeTestReport, decodeOperation, decodeError } =
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
