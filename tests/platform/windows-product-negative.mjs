#!/usr/bin/env node
// Windows 产品负例（RDWS-006 / 审计 §9.2 windows-product-negative）：
// **Windows CI 专用**——验证 MCP 注册 RPC 在无内核沙箱后端的平台上 fail-closed
//（mcp_platform_unsupported），产品入口隐藏由 renderer 平台门承担
//（apps/desktop McpPage win32 门，组件测试覆盖）。非 Windows 平台直接失败退出
//（不允许无断言 skip——本脚本属 windows-product-negative job，不得在别处运行）。
// 前置：cargo build --release -p ratiflow-core。
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import * as readline from 'node:readline';

if (process.platform !== 'win32') {
  console.error('✕ windows-product-negative 仅允许在 Windows 运行（产品负例平台 job）');
  process.exit(1);
}

const CORE = process.env.CORE_BIN ?? join(process.cwd(), 'target', 'release', 'ratiflow-core.exe');

class CoreClient {
  constructor(dataDir) {
    this.proc = spawn(CORE, ['app-server', '--data-dir', dataDir], { stdio: ['pipe', 'pipe', 'pipe'] });
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

function assert(c, label) {
  if (!c) throw new Error(`windows-product-negative 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function main() {
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-win-neg-'));
  const c = new CoreClient(dataDir);
  try {
    const hello = await c.call('core.version');
    assert(!!hello.protocolVersion, 'hello/版本握手');

    // RPC fail-closed：无内核沙箱后端 → 注册探针拒绝，错误携带平台令牌。
    let err;
    try {
      await c.call('mcp.serverAdd', { name: 'neg', command: 'cmd', args: ['/c', 'echo hi'] });
    } catch (e) {
      err = e;
    }
    assert(!!err, 'serverAdd 必须 fail-closed（不允许成功注册）');
    assert(
      String(err.message).includes('mcp_platform_unsupported'),
      `错误含 mcp_platform_unsupported（实际 ${String(err.message).slice(0, 80)}）`,
    );

    // 读面不受影响：serverList 正常返回（治理关闭的是副作用面，不是只读面）。
    const list = await c.call('mcp.serverList', {});
    assert(Array.isArray(list.items), 'serverList 只读面正常');

    console.log('Windows 产品负例通过（RPC fail-closed + 只读面可用）。');
  } finally {
    c.kill();
    rmSync(dataDir, { recursive: true, force: true });
  }
}

main().catch((e) => {
  console.error('windows-product-negative 失败：', e.message);
  process.exit(1);
});
