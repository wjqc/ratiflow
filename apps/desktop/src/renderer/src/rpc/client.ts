// renderer → preload bridge → main → Rust core。renderer 不接触 Node/文件系统/密钥。
declare global {
  interface Window {
    sixgates: {
      rpc(method: string, params?: Record<string, unknown>): Promise<unknown>;
      hello(): Promise<{ ok: boolean; error?: string } | null>;
      selectFile(): Promise<{ path: string; filename: string; contentBase64: string; size: number } | null>;
      openExternal(url: string): Promise<void>;
      selectDirectory(): Promise<string | null>;
      appInfo(): Promise<{ desktopVersion: string; logDir: string; userDataDir: string }>;
      openLogs(): Promise<void>;
      onEvent(callback: (event: TimelineEvent) => void): () => void;
    };
  }
}

export interface TimelineEvent {
  sequence: number;
  type: string;
  workItemId?: string;
  occurredAt: string;
  summary: string;
  detail?: unknown;
}

export async function rpc<T = unknown>(method: string, params: Record<string, unknown> = {}): Promise<T> {
  return (await window.sixgates.rpc(method, params)) as T;
}

export function rpcErrorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }
  return String(error);
}
