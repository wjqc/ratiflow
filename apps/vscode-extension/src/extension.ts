import * as vscode from 'vscode';

const PROTOCOL_VERSION = '1';
const HEARTBEAT_INTERVAL_MS = 30_000;
const SECRET_KEY = 'sixgates.session-token';

function getLocalUrl(): URL {
  const raw = vscode.workspace.getConfiguration('sixgates').get<string>('localUrl', 'http://127.0.0.1:7666');
  const url = new URL(raw);
  if (url.protocol !== 'http:' || !['127.0.0.1', 'localhost', '[::1]'].includes(url.hostname)) {
    throw new Error('SixGates 本地地址必须使用 http 和 loopback 主机。');
  }
  return url;
}

// 会话令牌只存 SecretStorage（手册 §12.2），绝不写 Workspace Settings。
async function sessionToken(context: vscode.ExtensionContext): Promise<string> {
  const existing = await context.secrets.get(SECRET_KEY);
  if (existing) {
    return existing;
  }
  const baseUrl = getLocalUrl();
  const response = await fetch(new URL('/api/v1/auth/sessions', baseUrl), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ label: 'vscode-extension' }),
  });
  if (!response.ok) {
    throw new Error(`创建会话失败（HTTP ${response.status}）`);
  }
  const body = (await response.json()) as { token: string };
  await context.secrets.store(SECRET_KEY, body.token);
  return body.token;
}

interface HeartbeatResponse {
  status: string;
  serverVersion: string;
  protocolVersion: string;
  compatible: boolean;
  upgradeHint?: string;
}

async function sendHeartbeat(context: vscode.ExtensionContext): Promise<HeartbeatResponse> {
  const baseUrl = getLocalUrl();
  const workspace = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? '';
  const response = await fetch(new URL('/api/v1/integrations/vscode/heartbeat', baseUrl), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      version: context.extension.packageJSON.version,
      workspace,
      vscodeVersion: vscode.version,
      protocolVersion: PROTOCOL_VERSION,
    }),
  });
  if (!response.ok) {
    throw new Error(`SixGates heartbeat 失败（HTTP ${response.status}）`);
  }
  return (await response.json()) as HeartbeatResponse;
}

interface WorkItemSummary {
  id: string;
  title: string;
  currentGate: string;
}

async function fetchWorkItems(context: vscode.ExtensionContext): Promise<WorkItemSummary[]> {
  const baseUrl = getLocalUrl();
  const token = await sessionToken(context);
  const response = await fetch(new URL('/api/v1/workitems?limit=20', baseUrl), {
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!response.ok) {
    throw new Error(`读取工作项失败（HTTP ${response.status}）`);
  }
  const page = (await response.json()) as { items: WorkItemSummary[] };
  return page.items;
}

const gateLabels: Record<string, string> = {
  requirements: '需求关',
  design: '方案关',
  development: '开发关',
  testing: '测试关',
  deployment: '部署关',
  verification: '验证关',
};

class WorkItemTree implements vscode.TreeDataProvider<WorkItemNode> {
  private emitter = new vscode.EventEmitter<WorkItemNode | undefined | null>();
  readonly onDidChangeTreeData = this.emitter.event;
  items: WorkItemSummary[] = [];

  refresh(): void {
    this.emitter.fire(undefined);
  }

  getTreeItem(element: WorkItemNode): vscode.TreeItem {
    return element;
  }

  getChildren(): WorkItemNode[] {
    if (this.items.length === 0) {
      return [new WorkItemNode('尚无工作项；在 Web 控制台创建', true)];
    }
    return this.items.map(
      (item) =>
        new WorkItemNode(`${item.title} · ${gateLabels[item.currentGate] ?? item.currentGate}`),
    );
  }
}

class WorkItemNode extends vscode.TreeItem {
  constructor(label: string, isPlaceholder = false) {
    super(label, vscode.TreeItemCollapsibleState.None);
    this.contextValue = isPlaceholder ? 'placeholder' : 'workitem';
    this.iconPath = new vscode.ThemeIcon(isPlaceholder ? 'info' : 'pass');
  }
}

async function openDiagnostics(): Promise<void> {
  const baseUrl = getLocalUrl();
  await vscode.env.openExternal(vscode.Uri.parse(baseUrl.toString()));
}

async function openProject(): Promise<void> {
  const folder = await vscode.window.showOpenDialog({
    canSelectFolders: true,
    canSelectFiles: false,
    canSelectMany: false,
    openLabel: '在 SixGates 中打开',
  });
  if (folder?.[0]) {
    await vscode.commands.executeCommand('vscode.openFolder', folder[0], false);
  }
}

export function activate(context: vscode.ExtensionContext): void {
  const tree = new WorkItemTree();
  const refreshWorkItems = async () => {
    try {
      tree.items = await fetchWorkItems(context);
    } catch {
      tree.items = [];
    }
    tree.refresh();
  };

  context.subscriptions.push(
    vscode.commands.registerCommand('sixgates.openDiagnostics', openDiagnostics),
    vscode.commands.registerCommand('sixgates.openProject', openProject),
    vscode.commands.registerCommand('sixgates.refreshWorkItems', () => void refreshWorkItems()),
    vscode.window.registerTreeDataProvider('sixgatesWorkItems', tree),
  );

  let compatibilityWarned = false;
  const beat = async () => {
    try {
      const response = await sendHeartbeat(context);
      if (!response.compatible && !compatibilityWarned) {
        compatibilityWarned = true;
        void vscode.window.showWarningMessage(response.upgradeHint || '扩展与本地 SixGates 协议不兼容。');
      } else if (response.compatible) {
        compatibilityWarned = false;
      }
      await refreshWorkItems();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      void vscode.window.showWarningMessage(`无法连接本机 SixGates：${message}`, '打开诊断').then((choice) => {
        if (choice === '打开诊断') {
          void openDiagnostics();
        }
      });
    }
  };

  void beat();
  const timer = setInterval(() => void beat(), HEARTBEAT_INTERVAL_MS);
  context.subscriptions.push({ dispose: () => clearInterval(timer) });
}

export function deactivate(): void {}
