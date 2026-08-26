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

export interface AgentRunInfo {
  id: string;
  status: string;
  result: string;
}

export async function rpc<T = unknown>(method: string, params: Record<string, unknown> = {}): Promise<T> {
  return (await window.sixgates.rpc(method, params)) as T;
}

/** M0-②：agent.start 立即返回 runId，终态经轮询 agent.get 获得（事件订阅到达后同样触发刷新）。 */
export async function waitForRunTerminal(runId: string, timeoutMs = 600_000): Promise<AgentRunInfo> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const run = await rpc<AgentRunInfo>('agent.get', { runId });
    if (run.status === 'completed_execution' || run.status === 'failed' || run.status === 'cancelled') {
      return run;
    }
    if (Date.now() > deadline) {
      throw new Error(`等待任务结束超时（${runId}）`);
    }
    await new Promise((resolve) => setTimeout(resolve, 400));
  }
}

export function rpcErrorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }
  return String(error);
}
