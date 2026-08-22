// 设置中心共享的 RPC 载荷类型（字段与 core 侧 dispatch 实际返回对齐）。

export interface IntegrationCheck {
  id: 'gitlab' | 'model' | 'ssh';
  name: string;
  status: 'ready' | 'pending' | 'error' | 'disabled';
  detail: string;
}

export interface LocalCheck {
  id: 'core' | 'executor' | 'sqlite';
  name: string;
  status: 'ready' | 'error';
  detail: string;
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
  gitlabInstance: string;
  namespace: string;
  project: string;
  defaultBranch: string;
  localRoot: string;
  status: 'ready' | 'archived';
  createdAt?: string;
  updatedAt?: string;
}

export interface ProjectListResult {
  items: ProjectRow[];
}

export interface AuditEvent {
  seq: number;
  time: string;
  actor: string;
  action: string;
  targetType: string;
  targetId?: string;
  detailJson: string;
}

export interface AuditListResult {
  items: AuditEvent[];
}

export interface BackupManifest {
  path: string;
  createdAt: string;
  coreVersion: string;
  schemaVersion: number;
  sha256: string;
  objectsCount: Record<string, number>;
}
