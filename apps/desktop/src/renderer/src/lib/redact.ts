// 秘密键脱敏：审计详情、诊断导出等渲染前必须经过此函数（安全要求 §15）。
// 命中键名的值替换为 "[redacted]"；不尝试解析值内容，避免误伤普通文本。

const SECRET_KEY = /secret|token|password|passwd|private[_-]?key|api[_-]?key|access[_-]?key|authorization|credential/i;

export const REDACTED = '[redacted]';

export function redactSecrets<T>(value: T): T {
  if (Array.isArray(value)) {
    return value.map((item) => redactSecrets(item)) as T;
  }
  if (value !== null && typeof value === 'object') {
    const out: Record<string, unknown> = {};
    for (const [key, child] of Object.entries(value as Record<string, unknown>)) {
      out[key] = SECRET_KEY.test(key) ? REDACTED : redactSecrets(child);
    }
    return out as T;
  }
  return value;
}

/** 渲染 JSON 前的统一入口：脱敏 + 稳定缩进。 */
export function redactedJson(value: unknown): string {
  return JSON.stringify(redactSecrets(value), null, 2);
}
