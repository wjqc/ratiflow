// Electron main：窗口/菜单/sidecar 生命周期与 JSON-RPC 转发。
// 安全红线：renderer sandbox + contextIsolation，preload 只暴露确定方法；
// sg:rpc 只放行契约方法（rpcMethods.generated.ts 由 codegen 从 contracts 生成）。
import { app, BrowserWindow, ipcMain, dialog, shell, Menu } from 'electron';
import { ChildProcess, spawn } from 'node:child_process';
import { join, resolve, sep } from 'node:path';
import { homedir } from 'node:os';
import { readFileSync, existsSync, mkdirSync, appendFileSync, statSync, renameSync, realpathSync } from 'node:fs';
import * as readline from 'node:readline';
import { RPC_METHODS } from './rpcMethods.generated';

const RPC_METHOD_SET = new Set<string>(RPC_METHODS);
// 与 core 的 MAX_MESSAGE_BYTES 一致（attachment.import 等大载荷走同一上限）。
const MAX_RPC_PARAMS_BYTES = 8 * 1024 * 1024;

interface HelloInfo {
  protocolVersion: string;
  coreVersion: string;
  schemaVersion: number;
  capabilities: string[];
}

const LOG_MAX_BYTES = 5 * 1024 * 1024;
/// 追加 core 日志，超 5MB 轮转（缺陷审计：崩溃刷屏曾无上限且逐块同步写满盘）。
function appendLog(path: string, text: string): void {
  try {
    if (existsSync(path) && statSync(path).size > LOG_MAX_BYTES) {
      renameSync(path, `${path}.old`);
    }
  } catch {}
  try {
    appendFileSync(path, text);
  } catch {}
}

interface PendingRequest {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  timer: NodeJS.Timeout;
}

class CoreClient {
  private proc: ChildProcess | null = null;
  private nextId = 1;
  private pending = new Map<string, PendingRequest>();
  private eventListeners: Array<(event: unknown) => void> = [];
  private helloPromise: Promise<HelloInfo> | null = null;
  private restarts = 0;
  private restartWindowStart = Date.now();
  private shutdownRequested = false;
  private protocolIncompatibleFlag = false;
  coreUnavailable = false;

  constructor(private binaryPath: string, private dataDir: string, private logPath: string) {}

  async start(): Promise<void> {
    this.helloPromise = new Promise((resolve, reject) => {
      this.spawnCore(resolve, reject);
    });
    await this.helloPromise;
  }

  private spawnCore(
    onHello: (hello: HelloInfo) => void,
    onHelloError: (error: Error) => void,
  ): void {
    mkdirSync(this.dataDir, { recursive: true });
    mkdirSync(join(this.logPath, '..'), { recursive: true });
    this.proc = spawn(this.binaryPath, ['app-server', '--listen', 'stdio://', '--data-dir', this.dataDir], {
      stdio: ['pipe', 'pipe', 'pipe'],
    });

    const stdout = this.proc.stdout;
    if (!stdout || !this.proc.stdin || !this.proc.stderr) {
      onHelloError(new Error('无法启动 Rust core（stdio 不可用）'));
      return;
    }

    // spawn 异步失败（ENOENT 等）：无监听会以未捕获 error 击穿主进程。
    this.proc.on('error', (err) => {
      appendLog(this.logPath, `[main] core spawn error: ${err.message}\n`);
      if (!helloResolved) {
        onHelloError(new Error(`core 启动失败: ${err.message}`));
      }
    });

    const rl = readline.createInterface({ input: stdout });
    let helloResolved = false;
    let protocolIncompatible = false;
    rl.on('line', (line: string) => {
      if (!line.trim()) {
        return;
      }
      let message: any;
      try {
        message = JSON.parse(line);
      } catch {
        appendLog(this.logPath, `[stdout-nonjson] ${line}\n`);
        return;
      }
      if (message.jsonrpc === undefined && message.protocolVersion !== undefined) {
        // hello 握手。
        if (message.protocolVersion !== '1') {
          onHelloError(new Error(`core 协议 ${message.protocolVersion} 不兼容（需要 1）；请同时更新应用与 core`));
          helloResolved = true;
          protocolIncompatible = true;
          this.protocolIncompatibleFlag = true;
          return;
        }
        helloResolved = true;
        onHello(message as HelloInfo);
        return;
      }
      if (message.method === 'event' && message.params) {
        for (const listener of this.eventListeners) {
          listener(message.params);
        }
        return;
      }
      if (message.id !== undefined && this.pending.has(String(message.id))) {
        const pending = this.pending.get(String(message.id))!;
        this.pending.delete(String(message.id));
        clearTimeout(pending.timer);
        if (message.error) {
          pending.reject(Object.assign(new Error(message.error.data?.detail ?? message.error.message), {
            code: message.error.message,
            retryable: message.error.retryable,
          }));
        } else {
          pending.resolve(message.result);
        }
      }
    });

    this.proc.stderr.setEncoding('utf8');
    this.proc.stderr.on('data', (chunk: string) => {
      appendLog(this.logPath, chunk);
    });

    this.proc.on('exit', (code, signal) => {
      appendLog(this.logPath, `[main] core exited code=${code} signal=${signal}\n`);
      this.proc = null;
      // 缺陷审计：core 死亡时在途 RPC 曾挂满 120s 超时——立即以明确错误回收。
      for (const [id, p] of this.pending) {
        clearTimeout(p.timer);
        p.reject(Object.assign(new Error('core 已退出，请求未完成'), { code: 'core_exited' }));
        this.pending.delete(id);
      }
      // P0：requested shutdown 不重启；只有意外退出才进入退避重启。
      if (this.shutdownRequested) {
        appendLog(this.logPath, '[main] shutdown requested; no restart\n');
        return;
      }
      // 协议不兼容的 core：重启只会循环撞墙（缺陷审计），进入诊断模式。
      if (protocolIncompatible || this.protocolIncompatibleFlag) {
        this.coreUnavailable = true;
        appendLog(this.logPath, '[main] protocol incompatible; diagnostic mode\n');
        return;
      }
      if (!helloResolved) {
        onHelloError(new Error('core 启动后立即退出（详见日志）'));
        return;
      }
      // 30 秒窗口内最多重启 3 次；随后进入诊断模式。
      const now = Date.now();
      if (now - this.restartWindowStart > 30_000) {
        this.restarts = 0;
        this.restartWindowStart = now;
      }
      this.restarts += 1;
      if (this.restarts > 3) {
        this.coreUnavailable = true;
        appendLog(this.logPath, '[main] core restart limit reached; diagnostic mode\n');
        return;
      }
      // 指数退避：500ms * 2^(n-1)
      const backoff = 500 * Math.pow(2, this.restarts - 1);
      appendLog(this.logPath, `[main] restarting core (${this.restarts}/3) in ${backoff}ms\n`);
      setTimeout(() => {
        if (this.shutdownRequested) {
          return;
        }
        this.helloPromise = new Promise((resolve2, reject2) => {
          this.spawnCore(resolve2, reject2);
        });
        this.helloPromise.catch(() => undefined);
      }, backoff);
    });
  }

  onEvent(listener: (event: unknown) => void): () => void {
    this.eventListeners.push(listener);
    return () => {
      const index = this.eventListeners.indexOf(listener);
      if (index >= 0) {
        this.eventListeners.splice(index, 1);
      }
    };
  }

  get protocolIncompatible(): boolean {
    return this.protocolIncompatibleFlag;
  }

  async call(method: string, params: Record<string, unknown>): Promise<unknown> {
    if (this.coreUnavailable) {
      throw Object.assign(new Error('core 不可用：已达重启上限，请从设置页查看诊断'), { code: 'core_unavailable' });
    }
    await this.helloPromise?.catch(() => undefined);
    if (!this.proc?.stdin) {
      throw Object.assign(new Error('core 未运行'), { code: 'core_unavailable' });
    }
    const id = String(this.nextId++);
    const message = JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n';
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(Object.assign(new Error(`RPC ${method} 超时`), { code: 'timeout' }));
      }, 120_000);
      this.pending.set(id, { resolve, reject, timer });
      this.proc!.stdin!.write(message);
    });
  }

  async shutdown(): Promise<void> {
    // P0：标记请求退出 -> 停收新调用 -> graceful -> 有界等待 -> 强杀。
    this.shutdownRequested = true;
    this.proc?.stdin?.write(JSON.stringify({ jsonrpc: '2.0', method: 'shutdown' }) + '\n');
    this.proc?.stdin?.end();
    await new Promise((resolve) => setTimeout(resolve, 800));
    this.proc?.kill('SIGKILL');
  }
}

let mainWindow: BrowserWindow | null = null;
let client: CoreClient | null = null;

function resolveCoreBinary(): string {
  // 开发：target/release；打包：resources/。
  const devPath = join(__dirname, '..', '..', '..', '..', 'target', 'release', 'sixgates-core');
  if (existsSync(devPath)) {
    return devPath;
  }
  const packagedPath = join(process.resourcesPath ?? '', 'sixgates-core');
  if (existsSync(packagedPath)) {
    return packagedPath;
  }
  // PATH 回退（缺陷审计：资源缺失时静默执行 PATH 上任意同名程序，属劫持面）
  // 仅在显式环境开关下启用；否则明确失败并给可操作指引。
  if (process.env.SG_CORE_FROM_PATH === '1') {
    return 'sixgates-core';
  }
  throw new Error(
    'sixgates-core 二进制缺失（开发：cargo build --release -p sixgates-core；打包：resources/sixgates-core）。' +
      '确要用 PATH 回退请设 SG_CORE_FROM_PATH=1',
  );
}

function createWindow(): void {
  mainWindow = new BrowserWindow({
    width: 1536,
    height: 1024,
    minWidth: 1180,
    minHeight: 760,
    show: false,
    title: 'Ratiflow',
    webPreferences: {
      nodeIntegration: false,
      contextIsolation: true,
      sandbox: true,
      webSecurity: true,
      preload: join(__dirname, '..', 'preload', 'preload.js'),
    },
  });
  mainWindow.loadFile(join(__dirname, '..', 'renderer', 'index.html'));
  mainWindow.once('ready-to-show', () => {
    mainWindow?.show();
  });
  // 导航与窗口打开拦截（外部链接走系统浏览器 allowlist）。
  mainWindow.webContents.setWindowOpenHandler(({ url }) => {
    if (url.startsWith('http://') || url.startsWith('https://')) {
      void shell.openExternal(url);
    }
    return { action: 'deny' };
  });
  mainWindow.webContents.on('will-navigate', (event, url) => {
    if (!url.startsWith('file://')) {
      event.preventDefault();
    }
  });
  mainWindow.webContents.on('did-fail-load', (_e, code, desc, url) => {
    console.error(`[renderer] load failed ${code} ${desc} ${url}`);
  });
  mainWindow.webContents.on('console-message', (_e, level, message) => {
    if (level >= 2) {
      console.error(`[renderer console] ${message}`);
    }
  });
  mainWindow.webContents.on('render-process-gone', (_e, details) => {
    console.error(`[renderer] process gone: ${details.reason}`);
  });
  // 渲染诊断：确认 DOM/模块/bridge 状态（SG_DEBUG_RENDER=1 时输出）。
  if (process.env.SG_DEBUG_RENDER === '1') {
    mainWindow.webContents.on('did-finish-load', async () => {
      try {
        const state = await mainWindow!.webContents.executeJavaScript(`(() => ({
          readyState: document.readyState,
          rootChildren: document.getElementById('root')?.childElementCount ?? -1,
          hasBridge: typeof window.sixgates === 'object',
          scripts: Array.from(document.querySelectorAll('script')).map(s => ({ type: s.type, src: s.getAttribute('src') })),
          bodyText: document.body.innerText.slice(0, 80),
        }))()`);
        console.error('[sg-debug]', JSON.stringify(state, null, 2));
      } catch (error) {
        console.error('[sg-debug] executeJavaScript failed:', String(error));
      }
      // 延迟复检：等待异步 project.list/workitem.list 返回后再看真实 UI 状态。
      setTimeout(async () => {
        try {
          const later = await mainWindow!.webContents.executeJavaScript(`(() => ({
            bodyText: document.body.innerText.slice(0, 160),
          }))()`);
          console.error('[sg-debug+5s]', JSON.stringify(later));
        } catch {}
      }, 5000);
    });
  }
}

function registerIpc(): void {
  ipcMain.handle('sg:rpc', async (_event, method: string, params: Record<string, unknown>) => {
    // 白名单 + 结构校验：渲染层被攻破时也无法调用契约之外的方法或注入非对象载荷。
    if (typeof method !== 'string' || !RPC_METHOD_SET.has(method)) {
      throw new Error(`rpc 方法不在契约白名单内: ${String(method).slice(0, 100)}`);
    }
    if (params === null || typeof params !== 'object' || Array.isArray(params)) {
      throw new Error('rpc params 必须是对象');
    }
    if (JSON.stringify(params).length > MAX_RPC_PARAMS_BYTES) {
      throw new Error('rpc params 超过大小上限');
    }
    if (!client) {
      throw new Error('core 未初始化');
    }
    return client.call(method, params ?? {});
  });

  ipcMain.handle('sg:hello', async () => {
    if (!client) {
      return null;
    }
    try {
      await client.call('core.version', {});
      return { ok: true };
    } catch (error) {
      return { ok: false, error: String(error) };
    }
  });

  // 原生文件选择：路径进入核心前由 core 重新校验（FR-DESK-007）。
  ipcMain.handle('sg:selectFile', async () => {
    const result = await dialog.showOpenDialog(mainWindow!, {
      properties: ['openFile'],
      filters: [
        { name: '需求文档与图片', extensions: ['md', 'markdown', 'txt', 'png', 'jpg', 'jpeg', 'webp'] },
      ],
    });
    if (result.canceled || result.filePaths.length === 0) {
      return null;
    }
    const path = result.filePaths[0];
    const fs = await import('node:fs/promises');
    const buffer = await fs.readFile(path);
    return {
      path,
      filename: path.split('/').pop() ?? 'file',
      contentBase64: buffer.toString('base64'),
      size: buffer.length,
    };
  });

  ipcMain.handle('sg:openExternal', async (_event, url: string) => {
    if (url.startsWith('http://') || url.startsWith('https://')) {
      await shell.openExternal(url);
    }
  });

  // 设置中心窄 IPC（契约 §3.1）：目录选择 / 版本与目录信息 / 打开日志目录。
  ipcMain.handle('sg:selectDirectory', async () => {
    const result = await dialog.showOpenDialog(mainWindow!, { properties: ['openDirectory', 'createDirectory'] });
    if (result.canceled || result.filePaths.length === 0) {
      return null;
    }
    return result.filePaths[0];
  });

  ipcMain.handle('sg:appInfo', () => ({
    desktopVersion: app.getVersion(),
    logDir: join(homedir(), 'Library', 'Logs', 'sixgates'),
    userDataDir: app.getPath('userData'),
  }));

  ipcMain.handle('sg:openLogs', async () => {
    await shell.openPath(join(homedir(), 'Library', 'Logs', 'sixgates'));
  });

  // S12 项目记忆导出 reveal（ADR-032 §11.2）：只接受满足固定 ID 正则的 exportId，
  // 在已知 userData data 根下拼路径并做 containment + 存在性校验后 showItemInFolder；
  // 拒绝路径穿越/未知 ID/缺失 index，renderer 永远不能提交任意路径。
  ipcMain.handle('sg:revealMemoryExport', async (_event, exportId: unknown) => {
    if (typeof exportId !== 'string' || !/^memexp_[0-9a-f]{24}$/.test(exportId)) {
      return false;
    }
    // 与 bootstrap 同源的数据目录推导（E2E 显式覆盖优先）。
    const dataRoot = join(process.env.SIXGATES_E2E_DATA_DIR || app.getPath('userData'), 'data');
    const exportsRoot = join(dataRoot, 'exports', 'memory');
    const index = join(exportsRoot, exportId, '_index.json');
    const resolvedIndex = resolve(index);
    const resolvedRoot = resolve(exportsRoot);
    if (!resolvedIndex.startsWith(resolvedRoot + sep)) {
      return false;
    }
    if (!existsSync(resolvedIndex)) {
      return false;
    }
    // symlink 围栏（缺陷审计）：词法 resolve 不跟随符号链接，existsSync 跟随——
    // 目录被替换为指向外部的 symlink 时按真实路径复核。
    let realIndex: string;
    let realRoot: string;
    try {
      realIndex = realpathSync(resolvedIndex);
      realRoot = realpathSync(resolvedRoot);
    } catch {
      return false;
    }
    if (!realIndex.startsWith(realRoot + sep)) {
      return false;
    }
    shell.showItemInFolder(realIndex);
    return true;
  });
}

function buildMenu(): void {
  const template: Array<Electron.MenuItemConstructorOptions> = [
    {
      label: 'Ratiflow',
      submenu: [
        { label: '关于 Ratiflow', click: () => void shell.openExternal('https://sixgates.local') },
        { type: 'separator' },
        { role: 'quit', label: '退出' },
      ],
    },
    {
      label: '编辑',
      submenu: [
        { role: 'undo', label: '撤销' },
        { role: 'redo', label: '重做' },
        { type: 'separator' },
        { role: 'cut', label: '剪切' },
        { role: 'copy', label: '复制' },
        { role: 'paste', label: '粘贴' },
      ],
    },
    {
      label: '视图',
      submenu: [
        { role: 'reload', label: '重新加载' },
        { role: 'toggleDevTools', label: '开发者工具' },
        { type: 'separator' },
        { role: 'resetZoom', label: '重置缩放' },
        { role: 'zoomIn', label: '放大' },
        { role: 'zoomOut', label: '缩小' },
        { role: 'togglefullscreen', label: '全屏' },
      ],
    },
  ];
  Menu.setApplicationMenu(Menu.buildFromTemplate(template));
}

// D5 多实例防线：双开会各自拉起 core 进程打开同一 SQLite，
// 迁移窗口期的并发 ALTER 会 duplicate column 拒启——单实例锁 fail-closed。
// E2E 实例使用独立 SIXGATES_E2E_DATA_DIR（不共享库），豁免单实例锁，
// 否则与用户在用的桌面实例互斥，自动化无法启动。
const isE2eInstance = Boolean(process.env.SIXGATES_E2E_DATA_DIR);
if (isE2eInstance) {
  // localStorage（sg:lastRoute 等）也在 userData 下：不隔离会把在用实例的
  // 路由状态泄漏进 e2e（启动直接落在设置页，A0 首断言失败）。
  app.setPath('userData', process.env.SIXGATES_E2E_DATA_DIR as string);
}
if (!isE2eInstance && !app.requestSingleInstanceLock()) {
  app.quit();
} else {
  app.on('second-instance', () => {
    if (mainWindow) {
      if (mainWindow.isMinimized()) {
        mainWindow.restore();
      }
      mainWindow.focus();
    }
  });
  void bootstrap();
}

function bootstrap(): void {
  app.whenReady().then(async () => {
  buildMenu();
  // E2E 隔离：显式数据目录覆盖（发布构建不设此变量，不影响生产）。
  const userData = process.env.SIXGATES_E2E_DATA_DIR || app.getPath('userData');
  const logPath = join(userData, 'logs', 'core.log');
  mkdirSync(join(userData, 'logs'), { recursive: true });
  try {
    client = new CoreClient(resolveCoreBinary(), join(userData, 'data'), logPath);
  } catch (error) {
    dialog.showErrorBox('Ratiflow core 启动失败', `${error}\n\n日志：${logPath}`);
    registerIpc();
    createWindow();
    return;
  }
  // F02 事件通道端到端：core 通知转发到所有渲染窗口（renderer 经 preload onEvent 订阅）。
  client.onEvent((event) => {
    for (const win of BrowserWindow.getAllWindows()) {
      win.webContents.send('sg:event', event);
    }
  });
  try {
    await client.start();
  } catch (error) {
    dialog.showErrorBox('Ratiflow core 启动失败', `${error}\n\n日志：${logPath}`);
  }
  registerIpc();
  createWindow();

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) {
      createWindow();
    }
  });
});
}

app.on('window-all-closed', () => {
  // darwin：仅关窗口不杀 core（审计修复：此前无条件 shutdown 且 darwin 不 quit，
  // activate 重建窗口后 core 已死不可恢复，全应用瘫痪）。shutdown 交由 before-quit。
  if (process.platform !== 'darwin') {
    app.quit();
  }
});

app.on('before-quit', () => {
  void client?.shutdown();
});
