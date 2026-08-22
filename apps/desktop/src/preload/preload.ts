// preload：contextBridge 暴露最小 typed API（禁止通用 invoke 通道）。
import { contextBridge, ipcRenderer } from 'electron';

export interface SelectedFile {
  path: string;
  filename: string;
  contentBase64: string;
  size: number;
}

export interface CoreEvent {
  sequence: number;
  type: string;
  workItemId?: string;
  occurredAt: string;
  summary: string;
  detail?: unknown;
}

const api = {
  rpc(method: string, params: Record<string, unknown> = {}): Promise<unknown> {
    return ipcRenderer.invoke('sg:rpc', method, params);
  },
  hello(): Promise<{ ok: boolean; error?: string } | null> {
    return ipcRenderer.invoke('sg:hello');
  },
  selectFile(): Promise<SelectedFile | null> {
    return ipcRenderer.invoke('sg:selectFile');
  },
  openExternal(url: string): Promise<void> {
    return ipcRenderer.invoke('sg:openExternal', url);
  },
  // 设置中心窄 IPC：目录选择 / 应用信息 / 打开日志目录。
  selectDirectory(): Promise<string | null> {
    return ipcRenderer.invoke('sg:selectDirectory');
  },
  appInfo(): Promise<{ desktopVersion: string; logDir: string; userDataDir: string }> {
    return ipcRenderer.invoke('sg:appInfo');
  },
  openLogs(): Promise<void> {
    return ipcRenderer.invoke('sg:openLogs');
  },
};

export type SixGatesBridge = typeof api;

contextBridge.exposeInMainWorld('sixgates', api);
