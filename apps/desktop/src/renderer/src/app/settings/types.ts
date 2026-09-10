// 设置中心共享的 RPC 载荷类型（字段与 core 侧 dispatch 实际返回对齐）。

export interface IntegrationCheck {
  checkId: string;
  label: string;
  scope: string;
  status: string;
  severity: string;
  durationMs: number;
  detail: string;
  fixTarget: string;
}

export interface LocalCheck {
  checkId: string;
  label: string;
  scope: string;
  status: string;
  severity: string;
  durationMs: number;
  detail: string;
  fixTarget: string;
}

export interface DiagnosticsReport {
  generatedAt: string;
  local: LocalCheck[];
  integrations: IntegrationCheck[];
}

export interface CoreVersionInfo {
  coreVersion: string;
  protocolVersion: number;
  schemaVersion: number;
}

export interface ProjectRow {
  id: string;
  name: string;
  gitlab_instance: string;
  namespace: string;
  project: string;
  default_branch: string;
  local_root: string;
  status: string;
  archived_at?: string | null;
  created_at?: string;
  updated_at?: string;
}

export interface ProjectListResult {
  items: ProjectRow[];
}

export interface AuditEvent {
  seq: number;
  createdAt: string;
  actor: string;
  action: string;
  targetType: string;
  targetId?: string;
  detail: unknown;
}

export interface AuditListResult {
  items: AuditEvent[];
}

export interface BackupRecord {
  id: string;
  path: string;
  format_version: number;
  schema_version: number;
  size_bytes: number;
  digest: string;
  verified: boolean;
  problems: unknown;
  status: string;
  manifest: {
    schemaVersion: number;
    snapshotSha256: string;
    objectsCount: Record<string, number>;
    objectsRootHash: string;
    rolloutsCount: number;
    rolloutsRootHash: string;
    ratiflowVersion: string;
    createdAt: string;
  };
  created_at: string;
  updated_at: string;
}

export interface BackupListResult {
  items: BackupRecord[];
}

export interface BackupRestoreOutcome {
  restored: boolean;
  requiresRestart: boolean;
  safetySnapshot: string;
}

export interface BackupDeleteResult {
  status: string;
}

/** audit.get 返回：核心结构化审计（snake_case，含结果与前后摘要）。 */
export interface AuditEntryExt {
  seq: number;
  actor: string;
  actor_kind: string;
  action: string;
  target_type: string;
  target_id: string;
  result: string;
  correlation_id?: string;
  project_id?: string;
  before_summary?: string;
  after_summary?: string;
  metadata_redacted: boolean;
  created_at: string;
}

export interface AuditGetResult {
  entry: AuditEntryExt;
}

/** project.inspectRoot 结果。 */
export interface InspectRootResult {
  path: string;
  isGitRepo: boolean;
  readable: boolean;
  writable: boolean;
  stacks: string[];
  hasRatiflowDir: boolean;
  blockers: Array<{ id: string; severity: string; detail?: string }>;
}

/** settings.summary 聚合（S00）。 */
export interface SettingsSummary {
  overallStatus: string;
  checkedAt: string;
  blockers: Array<{
    id: string;
    severity: string;
    scope: string;
    capabilities: string[];
    titleKey: string;
    targetRoute: string;
  }>;
  components: Array<{
    id: string;
    status: string;
    profileCount?: number;
    managedOnly?: boolean;
    targetCount?: number;
    sourceCount?: number;
  }>;
  dataSafety: {
    credentialRefCount: number;
    lastBackup: { id: string; createdAt: string; verified: boolean } | null;
    auditEventsLast7Days: number;
  };
  recentChanges: Array<{ key: string; revision: number; updatedAt: string; updatedBy: string }>;
}

// --- S12 项目记忆（ADR-032 / 实施方案 v1.0 §3/§9.1；与 contracts fixtures 对应） ---

export type MemoryKind = 'decision' | 'convention' | 'fact' | 'lesson' | 'preference';
export type MemoryStatus = 'proposed' | 'active' | 'conflicted' | 'archived' | 'rejected' | 'purged';

export const MEMORY_KIND_LABEL: Record<MemoryKind, string> = {
  decision: '决策',
  convention: '约定',
  fact: '事实',
  lesson: '经验',
  preference: '偏好',
};

/** 状态不只靠颜色：中文标签随徽标渲染。 */
export const MEMORY_STATUS_LABEL: Record<MemoryStatus, string> = {
  proposed: '待确认',
  active: '已确认',
  conflicted: '冲突',
  archived: '已归档',
  rejected: '已拒绝',
  purged: '已清除',
};

export interface MemorySettingsInfo {
  projectId: string;
  enabled: boolean;
  captureMode: 'off' | 'suggest';
  maxEntries: number;
  maxBytes: number;
  staleAfterDays: number;
  revision: number;
  updatedAt: string;
  updatedBy: string;
}

export interface MemoryListItem {
  id: string;
  slug: string;
  kind: MemoryKind;
  status: MemoryStatus;
  pinned: boolean;
  title: string;
  summary: string;
  revision: number;
  revisionNo: number;
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

export interface MemorySourceRefView {
  sourceKind: string;
  sourceId: string;
  locator: string;
  sourceDigest: string;
  relation: string;
}

export interface MemoryRevisionView {
  revisionNo: number;
  title: string;
  contentSha256: string;
  createdAt: string;
  purgedAt: string | null;
}

export interface MemoryUsageView {
  manifestId: string;
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
  body: string | null;
  objectSha256?: string | null;
  contentSha256: string;
  tags: string[];
  sources: MemorySourceRefView[];
  revisions: MemoryRevisionView[];
  usage: MemoryUsageView[];
  purgedAt?: string | null;
}

export interface MemoryPurgePreviewInfo {
  memoryId: string;
  projectId: string;
  entryStatus: string;
  canPurge: boolean;
  blockers: Array<{ code: string; detail: string }>;
  objectRefs: Record<string, number>;
  manifestRefs: { count: number; latestRunId?: string | null; latestSelectedAt?: string | null };
  backupRefs: { likelyContained: number; note: string };
  physicalDeletion: string;
  confirmationToken: string | null;
}
