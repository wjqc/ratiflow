// IPC 错误还原：ipcMain.handle 的 rejection 经 Electron 序列化后只剩 message
// （自定义属性如 code 丢失），且带 "Error invoking remote method" 包装前缀。
// 稳定 token（Rust ErrorCode::name 与 MEMORY_* 前缀）从 message 提取，供逻辑分支。
const TOKEN_RE = /\b(memory_[a-z_]+|not_found|revision_conflict|invalid_params|method_not_found|revision_frozen|conflict)\b/;

export function rpcErrToken(e: unknown): string | null {
  return String((e as Error)?.message ?? '').match(TOKEN_RE)?.[0] ?? null;
}

export function rpcErrText(e: unknown): string {
  return String((e as Error)?.message ?? '')
    .replace(/^Error invoking remote method 'sg:rpc':\s*/i, '')
    .replace(/^(Error:\s*)+/i, '');
}
