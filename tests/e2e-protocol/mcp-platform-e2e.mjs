#!/usr/bin/env node
// WP-3 平台矩阵负例 E2E（RDWS v1.4 / RDWS-006）：**Linux CI 专用**——
// Landlock ABI v1 无网络隔离位，MCP 沙箱暂 unsupported：serverAdd 探针必须
// fail-closed（mcp_platform_unsupported / mcp_sandbox_unavailable 前缀），
// 不允许成功链、也不允许无断言 skip（成功即失败——防「unsupported 声明」被静默突破）。
// 运行环境：.gitlab-ci.yml Linux 容器（node:26）。本脚本不得加入本地 Makefile
// （macOS 是 supported 平台，会反向失败）；UP-3a 真机链由专用 runner tag 门控。
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import * as readline from 'node:readline';

const CORE = process.env.CORE_BIN ?? join(process.cwd(), 'target', 'release', 'ratiflow-core');

if (process.platform !== 'linux') {
  console.error('✕ mcp-platform-e2e 仅允许在 Linux 运行（unsupported 负例）');
  process.exit(1);
}

class CoreClient {
  constructor(dataDir, env = {}) {
    this.proc = spawn(CORE, ['app-server', '--data-dir', dataDir], { stdio: ['pipe', 'pipe', 'ignore'], env: { ...process.env, ...env } });
    this.nextId = 1;
    this.pending = new Map();
    this.rl = readline.createInterface({ input: this.proc.stdout });
    this.rl.on('line', (l) => this.onLine(l));
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
  async hello_(timeoutMs = 5000) {
    const deadline = Date.now() + timeoutMs;
    while (!this.hello) {
      if (Date.now() > deadline) throw new Error('等待 hello 超时');
      await new Promise((r) => setTimeout(r, 25));
    }
    return this.hello;
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
  if (!c) throw new Error(`mcp-platform-e2e 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function main() {
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-mcp-platform-'));
  const c = new CoreClient(dataDir);
  try {
    await c.hello_();
    // Linux unsupported：探针 fail-closed（沙箱 spawn 拒绝），server 行 probe_failed，
    // 错误前缀必须是平台/沙箱声明——不得静默直启成功。
    let err = null;
    try {
      await c.call('mcp.serverAdd', {
        name: 'linuxneg', command: '/bin/cat', args: [],
      });
    } catch (e) {
      err = e;
    }
    assert(err !== null, 'Linux 上 serverAdd 不得成功');
    const detail = `${err?.code ?? ''} ${err?.message ?? ''}`;
    assert(
      /mcp_platform_unsupported|mcp_sandbox_unavailable|sandbox_denied/.test(detail),
      `失败前缀为平台/沙箱声明（实际 ${detail.slice(0, 90)}）`,
    );
    // disabled 模式：全部 MCP RPC 拒绝（kill switch）。
    const c2 = new CoreClient(dataDir, { RATIFLOW_MCP_MODE: 'disabled' });
    try {
      await c2.hello_();
      let err2 = null;
      try {
        await c2.call('mcp.serverAdd', { name: 'off', command: '/bin/cat', args: [] });
      } catch (e) { err2 = e; }
      assert(err2 !== null && /feature_disabled/.test(`${err2.code ?? ''} ${err2.message ?? ''}`),
        `disabled 模式 MCP RPC 拒绝（实际 ${err2?.code}）`);
    } finally {
      c2.kill();
    }
    console.log('mcp-platform-e2e（Linux unsupported 负例）全部通过');
  } finally {
    c.kill();
    rmSync(dataDir, { recursive: true, force: true });
  }
}

main().catch((e) => { console.error(`E2E 失败：${e.message}`); process.exit(1); });
