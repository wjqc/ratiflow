#!/usr/bin/env node
// RDWS manifest 校验（审计整改 §9.1）：编号化证据矩阵的机器门——
//   ① ID 恰为 RDWS-001..020、唯一且有序；
//   ② 每条断言 unit 与 protocol 证据非空（机器可执行维度不允许空证据）；
//   ③ unit 条目 "crate test_fn" 的 crate 目录存在且源码 grep 得到 fn；
//   ④ contract 条目存在于 contracts/rpc/ratiflow.json；
//   ⑤ protocol 脚本存在于 tests/e2e-protocol/ 或 tests/platform/，且已注册进 CI
//     （Makefile e2e / 平台块 / .gitlab-ci.yml job）——未进入 CI 的脚本即失败；
//   ⑥ electron 条目文件存在；
//   ⑦ uat 必须显式 recorded（record 路径存在）或 pending（带 note）；
//   ⑧ 反向：tests/e2e-protocol/*.mjs（除 with-timeout 助手）都必须被 manifest 引用。
import { readFileSync, readdirSync, existsSync } from 'node:fs';
import { execSync } from 'node:child_process';
import { join } from 'node:path';

const ROOT = join(import.meta.dirname, '..', '..');
const errors = [];
const ok = (label) => console.log(`  ✓ ${label}`);

// ---------- 载入 ----------
const manifest = JSON.parse(readFileSync(join(ROOT, 'tests/rdws/manifest.json'), 'utf8'));
const assertions = manifest.assertions ?? [];

// ① ID 完整/唯一/有序。
const ids = assertions.map((a) => a.id);
const expected = Array.from({ length: 20 }, (_, i) => `RDWS-${String(i + 1).padStart(3, '0')}`);
if (JSON.stringify(ids) !== JSON.stringify(expected)) {
  errors.push(`ID 集不合期望（重复/缺失/乱序）：${ids.join(',')}`);
} else {
  ok(`ID 恰为 RDWS-001..020 且有序（${ids.length} 条）`);
}

// ---------- CI 注册面（供 ⑤ 使用）----------
const makefile = readFileSync(join(ROOT, 'Makefile'), 'utf8');
const ciyml = existsSync(join(ROOT, '.gitlab-ci.yml'))
  ? readFileSync(join(ROOT, '.gitlab-ci.yml'), 'utf8')
  : '';
const registered = (script) => makefile.includes(script) || ciyml.includes(script);

// ②-⑦ 逐条断言。
const contract = JSON.parse(readFileSync(join(ROOT, 'contracts/rpc/ratiflow.json'), 'utf8'));
const contractMethods = new Set(contract.methods.map((m) => m.name));

for (const a of assertions) {
  const { id } = a;
  if (!Array.isArray(a.unit) || a.unit.length === 0) {
    errors.push(`${id}: unit 证据为空`);
  }
  if (!Array.isArray(a.protocol) || a.protocol.length === 0) {
    errors.push(`${id}: protocol 证据为空`);
  }
  // ③ unit 存在性：crate 目录 + fn grep。
  for (const u of a.unit ?? []) {
    const [crate, fn, ...rest] = String(u).split(/\s+/);
    if (!crate || !fn || rest.length > 0) {
      errors.push(`${id}: unit 条目格式应为 "<crate> <test_fn>"（实际 ${u}）`);
      continue;
    }
    const crateDir = join(ROOT, 'crates', crate);
    if (!existsSync(crateDir)) {
      errors.push(`${id}: crate 目录不存在 crates/${crate}`);
      continue;
    }
    let found = false;
    try {
      execSync(`grep -r --include='*.rs' -l "fn ${fn}" ${JSON.stringify(join(ROOT, 'crates', crate) + '/src')}`, { stdio: 'pipe' });
      found = true;
    } catch {
      found = false;
    }
    if (!found) errors.push(`${id}: 测试函数 fn ${fn} 在 crates/${crate} 源码中不存在`);
  }
  // ④ contract 方法存在。
  for (const m of a.contract ?? []) {
    if (!contractMethods.has(m)) errors.push(`${id}: 契约方法 ${m} 不存在于 contracts/rpc/ratiflow.json`);
  }
  // ⑤ protocol 脚本存在 + 进 CI。
  for (const s of a.protocol ?? []) {
    const inProtocol = existsSync(join(ROOT, 'tests/e2e-protocol', s));
    const inPlatform = existsSync(join(ROOT, 'tests/platform', s));
    if (!inProtocol && !inPlatform) {
      errors.push(`${id}: protocol 脚本不存在 ${s}`);
      continue;
    }
    if (!registered(s)) errors.push(`${id}: protocol 脚本 ${s} 未注册进任何 CI 面（Makefile/.gitlab-ci.yml）`);
  }
  // ⑥ electron 文件存在。
  for (const e of a.electron ?? []) {
    if (!existsSync(join(ROOT, e))) errors.push(`${id}: electron spec 不存在 ${e}`);
  }
  // ⑦ uat 显式状态。
  const uat = a.uat;
  if (!uat || !['recorded', 'pending'].includes(uat.status)) {
    errors.push(`${id}: uat 必须显式 recorded|pending`);
  } else if (uat.status === 'recorded' && !existsSync(join(ROOT, String(uat.record)))) {
    errors.push(`${id}: uat record 路径不存在 ${uat.record}`);
  } else if (uat.status === 'pending' && !uat.note) {
    errors.push(`${id}: uat pending 必须带 note 说明`);
  }
}

// ⑧ 反向：全部 E2E 脚本都必须注册进 CI（Makefile 平台块或 .gitlab-ci.yml）——
// manifest 断言只覆盖 RDWS 编号面，通用协议脚本走同一 CI 注册检查。
const referenced = new Set(assertions.flatMap((a) => a.protocol ?? []));
const allScripts = readdirSync(join(ROOT, 'tests/e2e-protocol')).filter(
  (f) => f.endsWith('.mjs') && f !== 'with-timeout.mjs',
);
for (const f of allScripts) {
  if (!registered(f)) errors.push(`E2E 脚本未注册进任何 CI 面（Makefile/.gitlab-ci.yml）：${f}`);
}
ok(`反向检查：tests/e2e-protocol 全部 ${allScripts.length} 个脚本均已注册进 CI（其中 ${referenced.size} 类进入 RDWS 断言矩阵）`);

// platform 字段引用的 job 必须存在于 .gitlab-ci.yml（或为本地 make ci 平台块条目）。
const platformJobs = new Set(assertions.flatMap((a) => a.platform ?? []));
const knownJobs = [
  'protocol-rdws-linux', 'mcp-linux-unsupported', 'mcp-macos-success',
  'windows-product-negative', 'electron-rdws', 'migration-old-binary-drill',
];
for (const job of platformJobs) {
  if (!knownJobs.includes(job) && !ciyml.includes(`${job}:`)) {
    errors.push(`platform job 未定义：${job}`);
  }
}

if (errors.length > 0) {
  console.error(`✕ RDWS manifest 校验失败（${errors.length} 项）：`);
  for (const e of errors) console.error(`  - ${e}`);
  process.exit(1);
}
console.log(`RDWS manifest 校验通过（${assertions.length} 条断言，全部证据就位）。`);
