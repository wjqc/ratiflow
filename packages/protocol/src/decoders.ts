// 手写运行时解码器：供契约校验脚本与 renderer 在信任边界使用。
// 与 contracts/rpc/errors.json envelope、fixtures 一一对应；禁止从数据库行直接复用。
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Any = any;

export interface StepResult {
  name: string;
  status: 'passed' | 'failed' | 'skipped' | 'action_required';
  durationMs: number;
  errorCode: string | null;
  detail: unknown;
}

export interface TestReport {
  status: 'ready' | 'degraded' | 'error' | 'action_required';
  durationMs: number;
  steps: StepResult[];
  requiresAccept?: boolean;
}

export interface OperationInfo {
  operationId: string;
  kind: string;
  status: 'running' | 'succeeded' | 'failed' | 'cancelled';
  progress: { completed: number; total: number; labelKey: string } | null;
  cancellable: boolean;
  startedAt: string;
}

export interface ErrorEnvelope {
  code: string;
  message: string;
  retryable: boolean;
  fieldErrors?: Record<string, string> | null;
  correlationId?: string | null;
  details?: Record<string, unknown> | null;
}

export interface SettingsBlocker {
  id: string;
  severity: 'blocking' | 'degraded';
  scope: string;
  capabilities: string[];
  titleKey: string;
  targetRoute: string;
}

export interface SettingsSummary {
  overallStatus: 'ready' | 'degraded' | 'action_required';
  checkedAt: string;
  blockers: SettingsBlocker[];
  components: Array<{ id: string; status: string; managedOnly?: boolean } & Record<string, unknown>>;
  dataSafety: {
    credentialRefCount: number;
    lastBackup: { id: string; createdAt: string; verified: boolean } | null;
    auditEventsLast7Days: number;
  };
  recentChanges: Array<{ key: string; revision: number; updatedAt: string; updatedBy: string }>;
}

function req<T>(v: unknown, check: (x: Any) => boolean, what: string): T {
  if (!check(v)) {
    throw new TypeError(`decode ${what}: 形状不合法`);
  }
  return v as T;
}
const isStr = (x: Any) => typeof x === 'string';
const isNum = (x: Any) => typeof x === 'number';
const isBool = (x: Any) => typeof x === 'boolean';
const isObj = (x: Any) => x !== null && typeof x === 'object';
const isArr = Array.isArray;

export function decodeStep(v: unknown): StepResult {
  const o = req<Record<string, Any>>(v, isObj, 'StepResult');
  req(o.name, isStr, 'StepResult.name');
  req(o.status, (x) => ['passed', 'failed', 'skipped', 'action_required'].includes(x), 'StepResult.status');
  req(o.durationMs, isNum, 'StepResult.durationMs');
  if (!('errorCode' in o)) throw new TypeError('StepResult.errorCode 必须显式存在（可为 null）');
  return { name: o.name, status: o.status, durationMs: o.durationMs, errorCode: o.errorCode, detail: o.detail ?? null };
}

export function decodeTestReport(v: unknown): TestReport {
  const o = req<Record<string, Any>>(v, isObj, 'TestReport');
  req(o.status, (x) => ['ready', 'degraded', 'error', 'action_required'].includes(x), 'TestReport.status');
  req(o.durationMs, isNum, 'TestReport.durationMs');
  return { status: o.status, durationMs: o.durationMs, steps: isArr(o.steps) ? o.steps.map(decodeStep) : [], requiresAccept: o.requiresAccept };
}

export function decodeOperation(v: unknown): OperationInfo {
  const o = req<Record<string, Any>>(v, isObj, 'OperationInfo');
  req(o.operationId, isStr, 'OperationInfo.operationId');
  req(o.kind, isStr, 'OperationInfo.kind');
  req(o.status, (x) => ['running', 'succeeded', 'failed', 'cancelled'].includes(x), 'OperationInfo.status');
  req(o.cancellable, isBool, 'OperationInfo.cancellable');
  req(o.startedAt, isStr, 'OperationInfo.startedAt');
  return { operationId: o.operationId, kind: o.kind, status: o.status, progress: o.progress ?? null, cancellable: o.cancellable, startedAt: o.startedAt };
}

export function decodeError(v: unknown): ErrorEnvelope {
  const o = req<Record<string, Any>>(v, isObj, 'ErrorEnvelope');
  req(o.code, isStr, 'ErrorEnvelope.code');
  req(o.message, isStr, 'ErrorEnvelope.message');
  req(o.retryable, isBool, 'ErrorEnvelope.retryable');
  // envelope 约束：秘密相关字段绝不允许出现。
  for (const banned of ['secret', 'maskedSecret', 'token', 'apiKey']) {
    if (banned in o) throw new TypeError(`ErrorEnvelope 不允许携带 ${banned}`);
  }
  return { code: o.code, message: o.message, retryable: o.retryable, fieldErrors: o.fieldErrors ?? null, correlationId: o.correlationId ?? null, details: o.details ?? null };
}

export function decodeSettingsSummary(v: unknown): SettingsSummary {
  const o = req<Record<string, Any>>(v, isObj, 'SettingsSummary');
  req(o.overallStatus, (x) => ['ready', 'degraded', 'action_required'].includes(x), 'SettingsSummary.overallStatus');
  req(o.checkedAt, isStr, 'SettingsSummary.checkedAt');
  req(o.blockers, isArr, 'SettingsSummary.blockers');
  for (const b of o.blockers) {
    req(b.id, isStr, 'blocker.id');
    req(b.severity, (x) => ['blocking', 'degraded'].includes(x), 'blocker.severity');
    req(b.titleKey, isStr, 'blocker.titleKey');
    req(b.targetRoute, isStr, 'blocker.targetRoute');
  }
  const ds = req<Record<string, Any>>(o.dataSafety, isObj, 'SettingsSummary.dataSafety');
  req(ds.credentialRefCount, isNum, 'dataSafety.credentialRefCount');
  req(ds.auditEventsLast7Days, isNum, 'dataSafety.auditEventsLast7Days');
  if (!('lastBackup' in ds)) throw new TypeError('dataSafety.lastBackup 必须显式存在（可为 null）');
  return {
    overallStatus: o.overallStatus, checkedAt: o.checkedAt,
    blockers: o.blockers, components: o.components ?? [],
    dataSafety: { credentialRefCount: ds.credentialRefCount, lastBackup: ds.lastBackup, auditEventsLast7Days: ds.auditEventsLast7Days },
    recentChanges: o.recentChanges ?? [],
  };
}
