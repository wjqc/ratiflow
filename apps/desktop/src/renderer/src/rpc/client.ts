// renderer → preload bridge → main → Rust core。renderer 不接触 Node/文件系统/密钥。
declare global {
  interface Window {
    ratiflow: {
      rpc(method: string, params?: Record<string, unknown>): Promise<unknown>;
      hello(): Promise<{ ok: boolean; error?: string } | null>;
      selectFile(): Promise<{ path: string; filename: string; contentBase64: string; size: number } | null>;
      openExternal(url: string): Promise<void>;
      selectDirectory(): Promise<string | null>;
      appInfo(): Promise<{ desktopVersion: string; logDir: string; userDataDir: string }>;
      openLogs(): Promise<void>;
      revealMemoryExport(exportId: string): Promise<boolean>;
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
  /** M2 流式增量事件（run.output_delta 等）专用字段：易失、不落库。 */
  aggregateId?: string;
  payload?: { workItemId?: string; text?: string; droppedBytes?: number };
  volatile?: boolean;
}

export interface AgentRunInfo {
  id: string;
  status: string;
  result: string;
}

export async function rpc<T = unknown>(method: string, params: Record<string, unknown> = {}): Promise<T> {
  return (await window.ratiflow.rpc(method, params)) as T;
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
      throw new Error('等待任务结束超时。任务可能仍在后台运行，请稍后刷新查看；也可重新发起。');
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
