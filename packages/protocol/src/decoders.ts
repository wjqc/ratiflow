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

// --- 项目记忆（ADR-032 / 实施方案 v1.0 §14.2）---

export const MEMORY_KINDS = ['decision', 'convention', 'fact', 'lesson', 'preference'] as const;
export type MemoryKind = (typeof MEMORY_KINDS)[number];

export const MEMORY_STATUSES = ['proposed', 'active', 'conflicted', 'archived', 'rejected', 'purged'] as const;
export type MemoryStatus = (typeof MEMORY_STATUSES)[number];

/** §6.6 context_manifest_memories.reason 固定枚举。 */
export const MEMORY_CONTEXT_REASONS = ['matched', 'pinned', 'over_budget', 'stale', 'conflicted', 'disabled', 'purged'] as const;

const isMemoryKind = (x: Any): x is MemoryKind => MEMORY_KINDS.includes(x);
const isMemoryStatus = (x: Any): x is MemoryStatus => MEMORY_STATUSES.includes(x);

export interface MemorySettingsInfo {
  projectId: string;
  featureEnabled: boolean;
  enabled: boolean;
  captureMode: 'off' | 'suggest';
  maxEntries: number;
  maxBytes: number;
  staleAfterDays: number;
  revision: number;
  updatedAt: string;
  updatedBy: string;
}

export function decodeMemorySettings(v: unknown): MemorySettingsInfo {
  const o = req<Record<string, Any>>(v, isObj, 'MemorySettings');
  req(o.projectId, isStr, 'MemorySettings.projectId');
  req(o.featureEnabled, isBool, 'MemorySettings.featureEnabled');
  req(o.enabled, isBool, 'MemorySettings.enabled');
  req(o.captureMode, (x) => ['off', 'suggest'].includes(x), 'MemorySettings.captureMode');
  req(o.maxEntries, (x) => isNum(x) && Number.isInteger(x) && x >= 1 && x <= 32, 'MemorySettings.maxEntries(1..32)');
  req(o.maxBytes, (x) => isNum(x) && Number.isInteger(x) && x >= 1024 && x <= 65536, 'MemorySettings.maxBytes(1024..65536)');
  req(o.staleAfterDays, (x) => isNum(x) && Number.isInteger(x) && x >= 1 && x <= 3650, 'MemorySettings.staleAfterDays(1..3650)');
  req(o.revision, (x) => isNum(x) && x >= 1, 'MemorySettings.revision');
  req(o.updatedAt, isStr, 'MemorySettings.updatedAt');
  req(o.updatedBy, isStr, 'MemorySettings.updatedBy');
  return { ...o } as MemorySettingsInfo;
}

export interface MemoryListItem {
  id: string;
  slug: string;
  kind: MemoryKind;
  status: MemoryStatus;
  pinned: boolean;
  title: string;
  summary: string;
  revisionNo: number;
  revision: number;
  tags: string[];
  updatedAt: string;
  sourceCount: number;
}

export interface MemoryListResult {
  projectId: string;
  items: MemoryListItem[];
  counts: Record<string, number>;
  cursor: string | null;
}

export function decodeMemoryList(v: unknown): MemoryListResult {
  const o = req<Record<string, Any>>(v, isObj, 'MemoryList');
  req(o.projectId, isStr, 'MemoryList.projectId');
  req(o.items, isArr, 'MemoryList.items');
  const items = o.items.map((it: Any) => {
    req(it.id, (x) => isStr(x) && x.startsWith('mem_'), 'MemoryListItem.id(mem_ 前缀)');
    req(it.slug, isStr, 'MemoryListItem.slug');
    req(it.kind, isMemoryKind, 'MemoryListItem.kind');
    req(it.status, isMemoryStatus, 'MemoryListItem.status');
    req(it.pinned, isBool, 'MemoryListItem.pinned');
    req(it.title, isStr, 'MemoryListItem.title');
    req(it.revisionNo, (x) => isNum(x) && x >= 1, 'MemoryListItem.revisionNo');
    req(it.revision, (x) => isNum(x) && x >= 1, 'MemoryListItem.revision');
    return { ...it } as MemoryListItem;
  });
  return { projectId: o.projectId, items, counts: isObj(o.counts) ? o.counts : {}, cursor: o.cursor ?? null };
}

export interface MemorySourceRef {
  sourceKind: string;
  sourceId: string;
  locator: string;
  sourceDigest: string;
  relation: string;
}

export interface MemoryRevisionInfo {
  revisionNo: number;
  title: string;
  contentSha256: string;
  createdAt: string;
  purgedAt: string | null;
}

export interface MemoryUsageInfo {
  manifestId: string;
  runId: string;
  revisionNo: number;
  selectedAt: string;
}

export interface MemoryDetail {
  id: string;
  projectId: string;
  slug: string;
  kind: MemoryKind;
  status: MemoryStatus;
  pinned: boolean;
  revisionNo: number;
  revision: number;
  title: string;
  summary: string;
  bodyState: 'available' | 'purged';
  /** 正文可空仅限 purged（墓碑）；其余状态必须是字符串。 */
  body: string | null;
  contentSha256: string;
  tags: string[];
  sources: MemorySourceRef[];
  revisions: MemoryRevisionInfo[];
  usage: MemoryUsageInfo[];
}

export function decodeMemoryDetail(v: unknown): MemoryDetail {
  const o = req<Record<string, Any>>(v, isObj, 'MemoryDetail');
  req(o.id, (x) => isStr(x) && x.startsWith('mem_'), 'MemoryDetail.id(mem_ 前缀)');
  req(o.projectId, isStr, 'MemoryDetail.projectId');
  req(o.slug, isStr, 'MemoryDetail.slug');
  req(o.kind, isMemoryKind, 'MemoryDetail.kind');
  req(o.status, isMemoryStatus, 'MemoryDetail.status');
  req(o.revisionNo, (x) => isNum(x) && x >= 1, 'MemoryDetail.revisionNo');
  req(o.revision, (x) => isNum(x) && x >= 1, 'MemoryDetail.revision');
  req(o.title, isStr, 'MemoryDetail.title');
  req(o.bodyState, (x) => ['available', 'purged'].includes(x), 'MemoryDetail.bodyState');
  const purged = o.status === 'purged';
  if (purged) {
    if (o.body !== null || o.bodyState !== 'purged') {
      throw new TypeError('MemoryDetail: purged 状态必须返回墓碑（body=null 且 bodyState=purged）');
    }
  } else if (!isStr(o.body) || o.bodyState !== 'available') {
    throw new TypeError('MemoryDetail: 非 purged 状态正文可空仅限 purged，必须返回正文');
  }
  req(o.contentSha256, isStr, 'MemoryDetail.contentSha256');
  if (!('objectSha256' in o) && purged) throw new TypeError('MemoryDetail: purged 必须显式携带 objectSha256=null');
  req(o.sources, isArr, 'MemoryDetail.sources');
  if (o.status === 'active' && o.sources.length < 1) {
    throw new TypeError('MemoryDetail: active 记忆至少需要一条来源引用（MEM-018）');
  }
  req(o.revisions, isArr, 'MemoryDetail.revisions');
  return { ...o } as MemoryDetail;
}

export interface MemoryContextItem {
  memoryId: string;
  revisionId: string;
  revisionNo: number;
  reason: (typeof MEMORY_CONTEXT_REASONS)[number];
  bytes: number;
  tokenEstimate: number;
}

export interface MemoryContextPreview {
  projectId: string;
  manifestFrozen: boolean;
  included: MemoryContextItem[];
  excluded: MemoryContextItem[];
  totalBytes: number;
  totalTokenEstimate: number;
  policy: { maxEntries: number; maxBytes: number };
}

export function decodeMemoryContextPreview(v: unknown): MemoryContextPreview {
  const o = req<Record<string, Any>>(v, isObj, 'MemoryContextPreview');
  req(o.projectId, isStr, 'MemoryContextPreview.projectId');
  req(o.manifestFrozen, isBool, 'MemoryContextPreview.manifestFrozen');
  const pick = (list: Any, what: string): MemoryContextItem[] =>
    req<Any[]>(list, isArr, what).map((it: Any) => {
      req(it.memoryId, isStr, `${what}.memoryId`);
      req(it.revisionId, isStr, `${what}.revisionId`);
      req(it.revisionNo, (x) => isNum(x) && x >= 1, `${what}.revisionNo`);
      req(it.reason, (x) => MEMORY_CONTEXT_REASONS.includes(x), `${what}.reason`);
      req(it.bytes, (x) => isNum(x) && x >= 0, `${what}.bytes`);
      req(it.tokenEstimate, (x) => isNum(x) && x >= 0, `${what}.tokenEstimate`);
      return { ...it } as MemoryContextItem;
    });
  const included = pick(o.included, 'included');
  const excluded = pick(o.excluded, 'excluded');
  const policy = req<Record<string, Any>>(o.policy, isObj, 'MemoryContextPreview.policy');
  return {
    projectId: o.projectId, manifestFrozen: o.manifestFrozen, included, excluded,
    totalBytes: o.totalBytes, totalTokenEstimate: o.totalTokenEstimate,
    policy: { maxEntries: policy.maxEntries, maxBytes: policy.maxBytes },
  };
}

export interface MemoryCaptureJobResult {
  job: {
    jobId: string;
    projectId: string;
    runId: string;
    status: 'pending' | 'in_flight' | 'succeeded' | 'failed' | 'unknown' | 'cancelled';
    attemptCount: number;
    errorCode: string | null;
    providerRequestId: string | null;
    sourceDigest: string;
  };
  candidates: Array<Record<string, unknown>>;
}

export function decodeMemoryCaptureJob(v: unknown): MemoryCaptureJobResult {
  const o = req<Record<string, Any>>(v, isObj, 'MemoryCaptureJob');
  const j = req<Record<string, Any>>(o.job, isObj, 'MemoryCaptureJob.job');
  req(j.jobId, (x) => isStr(x) && x.startsWith('memjob_'), 'capture.jobId(memjob_ 前缀)');
  req(j.projectId, isStr, 'capture.projectId');
  req(j.runId, isStr, 'capture.runId');
  req(j.status, (x) => ['pending', 'in_flight', 'succeeded', 'failed', 'unknown', 'cancelled'].includes(x), 'capture.status');
  req(j.attemptCount, (x) => isNum(x) && x >= 1, 'capture.attemptCount');
  const candidates = req<Any[]>(o.candidates, isArr, 'MemoryCaptureJob.candidates');
  if (j.status === 'unknown') {
    if (candidates.length > 0) throw new TypeError('capture unknown 状态不得携带候选（MEM-021）');
    if (!isStr(j.errorCode) || !j.errorCode.startsWith('MEMORY_')) {
      throw new TypeError('capture unknown 必须携带 MEMORY_* errorCode');
    }
  }
  return { job: { ...j }, candidates } as MemoryCaptureJobResult;
}

export interface MemoryPurgePreview {
  memoryId: string;
  projectId: string;
  canPurge: boolean;
  blockers: Array<{ code: string; detail: string }>;
  objectRefs: Record<string, number>;
  manifestRefs: Record<string, unknown>;
  backupRefs: { likelyContained: number; note: string };
  physicalDeletion: string;
  confirmationToken: string | null;
}

export function decodeMemoryPurgePreview(v: unknown): MemoryPurgePreview {
  const o = req<Record<string, Any>>(v, isObj, 'MemoryPurgePreview');
  req(o.memoryId, (x) => isStr(x) && x.startsWith('mem_'), 'MemoryPurgePreview.memoryId');
  req(o.projectId, isStr, 'MemoryPurgePreview.projectId');
  req(o.canPurge, isBool, 'MemoryPurgePreview.canPurge');
  const blockers = req<Any[]>(o.blockers, isArr, 'MemoryPurgePreview.blockers');
  if (!o.canPurge && blockers.length === 0) throw new TypeError('不可清除时必须给出 blockers');
  if (!isObj(o.backupRefs) || !isStr(o.backupRefs.note)) {
    throw new TypeError('purgePreview 必须声明备份残留边界（§10.3）');
  }
  if (!o.canPurge && o.confirmationToken != null) {
    throw new TypeError('不可清除时不得发放 confirmationToken');
  }
  return { ...o } as MemoryPurgePreview;
}
