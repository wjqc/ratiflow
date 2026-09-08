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
  /** M2 流式增量事件（run.output_delta 等）专用字段：易失、不落库。 */
  aggregateId?: string;
  payload?: { workItemId?: string; text?: string; droppedBytes?: number };
  volatile?: boolean;
}

const api = {
  rpc(method: string, params: Record<string, unknown> = {}): Promise<unknown> {
    return ipcRenderer.invoke('sg:rpc', method, params);
  },
  hello(): Promise<{ ok: boolean; error?: string } | null> {
    return ipcRenderer.invoke('sg:hello');
  },
  // RDWS-006 Windows 产品负例：renderer 需知宿主平台以隐藏不受支持的 MCP 入口
  //（RPC 侧另有 fail-closed 探针；UI 门只是产品面，不承担治理）。
  platform(): NodeJS.Platform {
    return process.platform;
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
  // S12 项目记忆：仅接受业务导出 ID（memexp_ 前缀），main 侧校验后经 shell 打开访达；
  // renderer 永远不能提交任意路径（ADR-032 §11.2）。
  revealMemoryExport(exportId: string): Promise<boolean> {
    return ipcRenderer.invoke('sg:revealMemoryExport', exportId);
  },
  // F02 只读事件订阅：main 转发的 sg:event 通知，返回退订函数。
  onEvent(callback: (event: CoreEvent) => void): () => void {
    const listener = (_e: unknown, event: CoreEvent): void => callback(event);
    ipcRenderer.on('sg:event', listener);
    return () => {
      ipcRenderer.removeListener('sg:event', listener);
    };
  },
};

export type RatiflowBridge = typeof api;

contextBridge.exposeInMainWorld('ratiflow', api);
