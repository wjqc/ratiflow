// Electron E2E 公共设施：fake GitLab 服务器 + 应用启动/驱动/清理。
// 场景定义见 docs/current/SixGates_跨端契约与验收手册_v1.0.md §14 A/D/E/F。
import { _electron as electron, type ElectronApplication, type Page } from '@playwright/test';
import { mkdtempSync, rmSync } from 'node:fs';
import { createServer, type Server } from 'node:http';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

export class FakeGitLab {
  private server: Server | null = null;
  private mode: 'ok' | 'unauthorized' = 'ok';
  port = 0;
  tokenSeen: string[] = [];

  async start(): Promise<void> {
    this.server = createServer((req, res) => {
      const token = req.headers['private-token'] as string | undefined;
      if (token) { this.tokenSeen.push(token); }
      if (this.mode === 'unauthorized') {
        res.writeHead(401, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ message: '401 Unauthorized' }));
        return;
      }
      if (req.url?.includes('/api/v4/user')) {
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ id: 1, username: 'e2e-user', name: 'E2E' }));
        return;
      }
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify([]));
    });
    await new Promise<void>((resolve) => this.server!.listen(0, '127.0.0.1', resolve));
    this.port = (this.server.address() as { port: number }).port;
  }

  setMode(mode: 'ok' | 'unauthorized'): void { this.mode = mode; }
  url(): string { return `http://127.0.0.1:${this.port}`; }

  async stop(): Promise<void> {
    if (this.server) { await new Promise((r) => this.server!.close(r)); this.server = null; }
  }
}

export interface E2eApp {
  app: ElectronApplication;
  window: Page;
  dataDir: string;

  /** 通过 preload bridge 调用 RPC（与 renderer 完全同路径）。 */
  rpc<T = unknown>(method: string, params?: Record<string, unknown>): Promise<T>;

  /** 导航到设置 section（走主壳路由）。 */
  gotoSettings(section: string): Promise<void>;

  close(): Promise<void>;
}

export async function launchApp(options: { env?: Record<string, string> } = {}): Promise<E2eApp> {
  const dataDir = mkdtempSync(join(tmpdir(), 'sg-e2e-'));
  const app = await electron.launch({
    args: ['.'],
    cwd: join(process.cwd()),
    env: {
      ...process.env,
      SIXGATES_E2E_DATA_DIR: dataDir,
      ...options.env,
    },
  });
  const window = await app.firstWindow();
  await window.waitForLoadState('domcontentloaded');
  // 等待 core hello 完成（bridge 可用且 RPC 有响应）。
  const deadline = Date.now() + 20000;
  while (Date.now() < deadline) {
    try {
      await window.evaluate(() =>
        (window as unknown as { sixgates: { rpc(m: string): Promise<unknown> } })
          .sixgates.rpc('core.version'));
      break;
    } catch {
      await new Promise((r) => setTimeout(r, 250));
    }
  }

  const rpc = <T,>(method: string, params: Record<string, unknown> = {}): Promise<T> =>
    window.evaluate(([m, p]: [string, Record<string, unknown>]) =>
      (window as unknown as { sixgates: { rpc(m: string, p: Record<string, unknown>): Promise<unknown> } })
        .sixgates.rpc(m, p), [method, params] as [string, Record<string, unknown>]) as Promise<T>;

  const gotoSettings = async (section: string): Promise<void> => {
    await window.evaluate((s: string) => {
      window.dispatchEvent(new CustomEvent('sg:settings-navigate', { detail: s }));
    }, section);
    await window.waitForTimeout(300);
  };

  return {
    app, window, dataDir, rpc, gotoSettings,
    close: async () => {
      try { await app.close(); } catch { /* already closed */ }
      rmSync(dataDir, { recursive: true, force: true });
    },
  };
}

/** 找到属于指定数据目录的 core 进程 PID（pgrep -f dataDir）。 */
export async function findCorePid(dataDir: string): Promise<number[]> {
  const { execSync } = await import('node:child_process');
  try {
    const out = execSync(`pgrep -f "sixgates-core.*${dataDir}"`).toString().trim();
    return out.split('\n').filter(Boolean).map(Number);
  } catch { return []; }
}

/** 等待直到条件为真或超时。 */
export async function waitFor(cond: () => Promise<boolean> | boolean, timeoutMs = 15000, label = ''): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await cond()) { return; }
    await new Promise((r) => setTimeout(r, 300));
  }
  throw new Error(`waitFor 超时 ${timeoutMs}ms${label ? `：${label}` : ''}`);
}
