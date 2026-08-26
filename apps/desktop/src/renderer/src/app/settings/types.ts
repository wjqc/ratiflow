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
