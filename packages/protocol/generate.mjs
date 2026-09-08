#!/usr/bin/env node
// 从 contracts/rpc/ratiflow.json 生成 TypeScript 方法清单与类型（契约先行，禁止手写漂移）。
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';

const contract = JSON.parse(readFileSync('contracts/rpc/ratiflow.json', 'utf8'));

function tsType(type) {
  switch (type) {
    case 'string': return 'string';
    case 'number': return 'number';
    case 'boolean': return 'boolean';
    case 'array': return 'unknown[]';
    default: return 'unknown';
  }
}

let code = `// 本文件由 generate.mjs 从 contracts/rpc/ratiflow.json 生成；不要手写修改。
export const PROTOCOL_VERSION = '1';
export const MAX_MESSAGE_BYTES = 8 * 1024 * 1024;

export type RpcMethodName =
${contract.methods.map((m) => `  | '${m.name}'`).join('\n')};

export const RPC_METHODS: readonly RpcMethodName[] = [
${contract.methods.map((m) => `  '${m.name}',`).join('\n')}
] as const;

export const EVENT_TYPES: readonly string[] = [
${contract.events.map((e) => `  '${e}',`).join('\n')}
] as const;

export interface TimelineEvent {
  sequence: number;
  type: string;
  workItemId?: string;
  occurredAt: string;
  summary: string;
  detail?: unknown;
}
`;

for (const method of contract.methods) {
  const params = method.params.filter((p) => p.required);
  const optional = method.params.filter((p) => !p.required);
  if (params.length + optional.length === 0) {
    code += `\nexport type ${ifaceName(method.name)}Params = Record<string, never>;\n`;
    continue;
  }
  code += `\nexport interface ${ifaceName(method.name)}Params {\n`;
  for (const p of params) {
    code += `  ${p.name}: ${tsType(p.type)};\n`;
  }
  for (const p of optional) {
    code += `  ${p.name}?: ${tsType(p.type)};\n`;
  }
  code += `}\n`;
}

function ifaceName(method) {
  return method.split('.').map((part) => part[0].toUpperCase() + part.slice(1)).join('');
}

mkdirSync('packages/protocol/src', { recursive: true });
writeFileSync('packages/protocol/src/generated.ts', code);

// Electron 主进程的 RPC 白名单（渲染层透传面收口，随契约自动同步）。
mkdirSync('apps/desktop/src/main', { recursive: true });
writeFileSync(
  'apps/desktop/src/main/rpcMethods.generated.ts',
  `// 本文件由 generate.mjs 从 contracts/rpc/ratiflow.json 生成；不要手写修改。
export const RPC_METHODS: readonly string[] = [
${contract.methods.map((m) => `  '${m.name}',`).join('\n')}
];
`,
);

// 方法 ↔ dispatch 实现对齐检查（契约测试数据）。
const rustDispatch = readFileSync('crates/ratiflow-core/src/dispatch.rs', 'utf8') + readFileSync('crates/ratiflow-core/src/settings_dispatch.rs', 'utf8');
const missing = contract.methods.filter((m) => !rustDispatch.includes(`"${m.name}"`));
if (missing.length > 0) {
  console.error('契约中声明但 Rust 未实现的方法：', missing.map((m) => m.name));
  process.exit(1);
}

// --- P0-1 mutation registry 双向静态校验（审计 §7 P0-1 退出标准）---
// 注册表（crates/ratiflow-core/src/mutation_registry.rs）是 read/mutation、receipt
// 模式的单一声明源：方法集与契约严格一致；Required ⇔ 契约必填 idempotencyKey。
const registrySource = readFileSync('crates/ratiflow-core/src/mutation_registry.rs', 'utf8');
const markerBegin = '// BEGIN_REGISTRY_ENTRIES';
const markerEnd = '// END_REGISTRY_ENTRIES';
const beginIdx = registrySource.indexOf(markerBegin);
const endIdx = registrySource.indexOf(markerEnd);
if (beginIdx < 0 || endIdx < 0 || endIdx < beginIdx) {
  console.error('mutation registry 缺少 BEGIN/END_REGISTRY_ENTRIES 标记（勿删标记行）');
  process.exit(1);
}
const registryBlock = registrySource.slice(beginIdx + markerBegin.length, endIdx);
const entryRe = /Entry\s*\{\s*method:\s*"([^"]+)",\s*kind:\s*Kind::(Read|Mutation),\s*receipt:\s*ReceiptMode::(None|Required)/g;
const registry = new Map();
for (const [, method, kind, receipt] of registryBlock.matchAll(entryRe)) {
  if (registry.has(method)) {
    console.error(`mutation registry 重复方法：${method}`);
    process.exit(1);
  }
  registry.set(method, { kind, receipt });
}
const contractNames = contract.methods.map((m) => m.name);
const contractSet = new Set(contractNames);
if (contractNames.length !== contractSet.size) {
  console.error('契约存在重复方法声明：', contractNames.filter((n, i) => contractNames.indexOf(n) !== i));
  process.exit(1);
}
const registryOnly = [...registry.keys()].filter((m) => !contractSet.has(m));
if (registryOnly.length > 0) {
  console.error('注册表有而契约没有的方法（先改契约或删条目）：', registryOnly);
  process.exit(1);
}
const contractOnly = contractNames.filter((m) => !registry.has(m));
if (contractOnly.length > 0) {
  console.error('契约有而注册表未声明的方法（新增方法必须先入注册表）：', contractOnly);
  process.exit(1);
}
const keyMismatch = contract.methods
  .filter((m) => {
    const declared = m.params.some((p) => p.name === 'idempotencyKey' && p.required);
    const registered = registry.get(m.name)?.receipt === 'Required';
    return declared !== registered;
  })
  .map((m) => m.name);
if (keyMismatch.length > 0) {
  console.error('receipt 模式与契约 idempotencyKey 声明不一致的方法：', keyMismatch);
  process.exit(1);
}
const receiptCount = [...registry.values()].filter((e) => e.receipt === 'Required').length;
console.log(
  `生成 ${contract.methods.length} 个方法类型；Rust 实现对齐检查通过；` +
  `mutation registry 双向一致（${registry.size} 方法，${receiptCount} 个 receipt 门控 mutation）。`,
);
