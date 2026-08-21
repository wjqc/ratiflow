import * as vscode from 'vscode';

function getLocalUrl(): URL {
  const raw = vscode.workspace.getConfiguration('sixgates').get<string>('localUrl', 'http://127.0.0.1:7666');
  const url = new URL(raw);
  if (url.protocol !== 'http:' || !['127.0.0.1', 'localhost', '[::1]'].includes(url.hostname)) {
    throw new Error('SixGates 本地地址必须使用 http 和 loopback 主机。');
  }
  return url;
}

async function sendHeartbeat(context: vscode.ExtensionContext): Promise<void> {
  const baseUrl = getLocalUrl();
  const workspace = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? '';
  const response = await fetch(new URL('/api/v1/integrations/vscode/heartbeat', baseUrl), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ version: context.extension.packageJSON.version, workspace }),
  });
  if (!response.ok) {
    throw new Error(`SixGates heartbeat 失败（HTTP ${response.status}）`);
  }
}

async function openDiagnostics(): Promise<void> {
  const baseUrl = getLocalUrl();
  await vscode.env.openExternal(vscode.Uri.parse(baseUrl.toString()));
}

async function openProject(): Promise<void> {
  const folder = await vscode.window.showOpenDialog({ canSelectFolders: true, canSelectFiles: false, canSelectMany: false, openLabel: '在 SixGates 中打开' });
  if (folder?.[0]) {
    await vscode.commands.executeCommand('vscode.openFolder', folder[0], false);
  }
}

export function activate(context: vscode.ExtensionContext): void {
  context.subscriptions.push(
    vscode.commands.registerCommand('sixgates.openDiagnostics', openDiagnostics),
    vscode.commands.registerCommand('sixgates.openProject', openProject),
  );

  void sendHeartbeat(context).catch((error: unknown) => {
    const message = error instanceof Error ? error.message : String(error);
    void vscode.window.showWarningMessage(`无法连接本机 SixGates：${message}`, '打开诊断').then((choice) => {
      if (choice === '打开诊断') {
        void openDiagnostics();
      }
    });
  });
}

export function deactivate(): void {}
