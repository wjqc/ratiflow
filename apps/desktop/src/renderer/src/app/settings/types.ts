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
    sixgatesVersion: string;
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

/** audit.export 返回：去敏后扁平 JSON 数组（camelCase）。 */
export interface AuditExportEntry {
  seq: number;
  actor: string;
  actorKind: string;
  action: string;
  targetType: string;
  targetId: string;
  result: string;
  correlationId?: string;
  metadataRedacted: boolean;
  createdAt: string;
}

/** logs.list 条目。 */
export interface LogEntry {
  name: string;
  sizeBytes: number;
  path: string;
}

export interface LogListResult {
  items: LogEntry[];
}

/** project.inspectRoot 结果。 */
export interface InspectRootResult {
  path: string;
  isGitRepo: boolean;
  readable: boolean;
  writable: boolean;
  stacks: string[];
  hasSixgatesDir: boolean;
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

/** update.check / update.status。 */
export interface UpdateCheckInfo {
  channel: string;
  autoCheck: boolean;
  autoDownload: boolean;
  currentVersion: string;
  latestVersion: string;
  updateAvailable: boolean;
  note: string;
}

export interface UpdateStatusInfo {
  desktop: string;
  core: string;
  protocol: number;
  schema: number;
  signatureVerified: boolean;
  note: string;
}
