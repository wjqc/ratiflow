export type DiagnosticStatus = 'ready' | 'needs_configuration' | 'checking' | 'unavailable';

export interface DiagnosticItem {
  id: string;
  label: string;
  status: DiagnosticStatus;
  detail: string;
  required: boolean;
}

export interface DiagnosticReport {
  generatedAt: string;
  ready: boolean;
  integrations: DiagnosticItem[];
  local: DiagnosticItem[];
}

export type GateName = 'requirements' | 'design' | 'development' | 'testing' | 'deployment' | 'verification';

export type StageState =
  | 'not_started'
  | 'running'
  | 'blocked'
  | 'awaiting_approval'
  | 'passed'
  | 'failed'
  | 'cancelled'
  | 'stale';

export interface WorkItem {
  id: string;
  projectId: string;
  gitlabIssueIid?: string;
  title: string;
  description: string;
  labels: string[];
  currentGate: GateName;
  createdAt: string;
  updatedAt: string;
}

export interface Stage {
  gate: GateName;
  state: StageState;
  inputBaselineSha: string;
  updatedAt: string;
}

export interface Artifact {
  id: string;
  workItemId: string;
  kind: string;
  title: string;
  createdAt: string;
}

export interface Evidence {
  id: string;
  workItemId: string;
  gate: GateName;
  kind: string;
  title: string;
  verified: boolean;
  source: 'local' | 'gitlab';
  createdAt: string;
}

export interface Approval {
  id: string;
  subjectType: 'tool_proposal' | 'deployment' | 'baseline' | 'risk';
  subjectId: string;
  risk: 'low' | 'medium' | 'high';
  status: 'requested' | 'approved' | 'rejected' | 'expired';
  reason: string;
  expiresAt: string;
  createdAt: string;
}

export interface GateResultInfo {
  gate: GateName;
  passed: boolean;
  failedInputs: string[];
  computedAt: string;
}

export interface PassportInfo {
  id: string;
  workItemId: string;
  objectSha256: string;
  createdAt: string;
  gates: Array<{ gate: GateName; passed: boolean; evidenceIds: string[] }>;
}

export interface DeploymentInfo {
  id: string;
  workItemId: string;
  target: string;
  imageDigest: string;
  state: string;
}

export interface RevisionInfo {
  id: string;
  artifactId: string;
  revNo: number;
  contentSha256: string;
  size: number;
  status: 'draft' | 'in_review' | 'frozen' | 'superseded';
  etag: string;
  createdAt: string;
}

export interface AgentRunInfo {
  id: string;
  goal: string;
  status: 'queued' | 'running' | 'paused' | 'completed_execution' | 'failed' | 'cancelled';
  result: string;
}

export const riskLabels: Record<string, string> = {
  low: '低风险',
  medium: '中风险',
  high: '高风险',
};

export const gateLabels: Record<GateName, string> = {
  requirements: '需求关',
  design: '方案关',
  development: '开发关',
  testing: '测试关',
  deployment: '部署关',
  verification: '验证关',
};

export const stageLabels: Record<StageState, string> = {
  not_started: '未开始',
  running: '进行中',
  blocked: '受阻',
  awaiting_approval: '待审批',
  passed: '已通过',
  failed: '失败',
  cancelled: '已取消',
  stale: '已过期',
};

export const gateOrder: GateName[] = [
  'requirements',
  'design',
  'development',
  'testing',
  'deployment',
  'verification',
];
