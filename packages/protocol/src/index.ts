// JSON-RPC 2.0 客户端协议层（Node/Electron main 共用；renderer 经 preload bridge 访问）。
import type { TimelineEvent, RpcMethodName } from './generated';

export interface RpcRequest {
  jsonrpc: '2.0';
  id: number | string;
  method: RpcMethodName;
  params: Record<string, unknown>;
}

export interface RpcError {
  code: number;
  message: string;
  retryable: boolean;
  data?: { detail?: string };
}

export interface RpcResponse {
  jsonrpc: '2.0';
  id: number | string | null;
  result?: unknown;
  error?: RpcError;
}

export interface Hello {
  protocolVersion: string;
  coreVersion: string;
  schemaVersion: number;
  capabilities: string[];
}

export type { TimelineEvent, RpcMethodName };
export * from './generated';
