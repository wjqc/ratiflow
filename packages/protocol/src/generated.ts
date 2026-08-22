// 本文件由 generate.mjs 从 contracts/rpc/sixgates.json 生成；不要手写修改。
export const PROTOCOL_VERSION = '1';
export const MAX_MESSAGE_BYTES = 8 * 1024 * 1024;

export type RpcMethodName =
  | 'core.version'
  | 'diagnostics.check'
  | 'project.list'
  | 'project.get'
  | 'project.create'
  | 'project.update'
  | 'project.archive'
  | 'project.summary'
  | 'knowledge.list'
  | 'knowledge.create'
  | 'knowledge.update'
  | 'knowledge.remove'
  | 'knowledge.scan'
  | 'knowledge.search'
  | 'context.preview'
  | 'context.create'
  | 'attachment.import'
  | 'attachment.list'
  | 'attachment.parse'
  | 'attachment.remove'
  | 'workitem.list'
  | 'workitem.get'
  | 'workitem.create'
  | 'workitem.setStage'
  | 'workitem.progress'
  | 'workitem.documents'
  | 'workitem.getDocument'
  | 'workitem.importDocument'
  | 'workitem.importIssue'
  | 'artifact.list'
  | 'artifact.create'
  | 'artifact.createDraft'
  | 'artifact.updateDraft'
  | 'artifact.listRevisions'
  | 'artifact.revisionContent'
  | 'artifact.addReview'
  | 'artifact.freezeBaseline'
  | 'agent.run'
  | 'agent.get'
  | 'agent.cancel'
  | 'agent.proposals'
  | 'gate.evaluate'
  | 'approval.list'
  | 'approval.decide'
  | 'approval.listByWorkItem'
  | 'evidence.list'
  | 'evidence.record'
  | 'evidence.verify'
  | 'passport.issue'
  | 'passport.latest'
  | 'deployment.create'
  | 'deployment.get'
  | 'deployment.submit'
  | 'deployment.deploy'
  | 'deployment.verify'
  | 'deployment.rollback'
  | 'timeline.snapshot'
  | 'backup.create'
  | 'audit.list';

export const RPC_METHODS: readonly RpcMethodName[] = [
  'core.version',
  'diagnostics.check',
  'project.list',
  'project.get',
  'project.create',
  'project.update',
  'project.archive',
  'project.summary',
  'knowledge.list',
  'knowledge.create',
  'knowledge.update',
  'knowledge.remove',
  'knowledge.scan',
  'knowledge.search',
  'context.preview',
  'context.create',
  'attachment.import',
  'attachment.list',
  'attachment.parse',
  'attachment.remove',
  'workitem.list',
  'workitem.get',
  'workitem.create',
  'workitem.setStage',
  'workitem.progress',
  'workitem.documents',
  'workitem.getDocument',
  'workitem.importDocument',
  'workitem.importIssue',
  'artifact.list',
  'artifact.create',
  'artifact.createDraft',
  'artifact.updateDraft',
  'artifact.listRevisions',
  'artifact.revisionContent',
  'artifact.addReview',
  'artifact.freezeBaseline',
  'agent.run',
  'agent.get',
  'agent.cancel',
  'agent.proposals',
  'gate.evaluate',
  'approval.list',
  'approval.decide',
  'approval.listByWorkItem',
  'evidence.list',
  'evidence.record',
  'evidence.verify',
  'passport.issue',
  'passport.latest',
  'deployment.create',
  'deployment.get',
  'deployment.submit',
  'deployment.deploy',
  'deployment.verify',
  'deployment.rollback',
  'timeline.snapshot',
  'backup.create',
  'audit.list',
] as const;

export const EVENT_TYPES: readonly string[] = [
  'project.created',
  'workitem.created',
  'stage.passed',
  'stage.running',
  'stage.stale',
  'gate.evaluated',
  'baseline.frozen',
  'artifact.reviewed',
  'approval.requested',
  'approval.approved',
  'approval.rejected',
  'evidence.recorded',
  'passport.issued',
  'attachment.imported',
  'knowledge.scanned',
  'tool.proposed',
  'run.completed_execution',
  'run.failed',
  'run.cancelled',
  'deployment.draft',
  'deployment.awaiting_approval',
  'deployment.approved',
  'deployment.deploying',
  'deployment.awaiting_verification',
  'deployment.verified',
  'deployment.verification_failed',
  'deployment.deploy_failed',
  'deployment.rolling_back',
  'deployment.rolled_back',
  'deployment.rollback_failed',
] as const;

export interface TimelineEvent {
  sequence: number;
  type: string;
  workItemId?: string;
  occurredAt: string;
  summary: string;
  detail?: unknown;
}

export type CoreVersionParams = Record<string, never>;

export type DiagnosticsCheckParams = Record<string, never>;

export interface ProjectListParams {
  includeArchived?: boolean;
}

export interface ProjectGetParams {
  projectId: string;
}

export interface ProjectCreateParams {
  gitlabInstance: string;
  namespace: string;
  project: string;
  defaultBranch?: string;
  name?: string;
  localRoot?: string;
}

export interface ProjectUpdateParams {
  projectId: string;
  name?: string;
  localRoot?: string;
  defaultBranch?: string;
  status?: string;
}

export interface ProjectArchiveParams {
  projectId: string;
  archived?: boolean;
}

export interface ProjectSummaryParams {
  projectId: string;
}

export interface KnowledgeListParams {
  projectId: string;
}

export interface KnowledgeCreateParams {
  projectId: string;
  kind: string;
  name: string;
  locator: string;
}

export interface KnowledgeUpdateParams {
  sourceId: string;
  enabled?: boolean;
  name?: string;
}

export interface KnowledgeRemoveParams {
  sourceId: string;
}

export interface KnowledgeScanParams {
  sourceId: string;
  projectRoot?: string;
}

export interface KnowledgeSearchParams {
  projectId: string;
  query: string;
  limit?: number;
}

export interface ContextPreviewParams {
  projectId: string;
  query: string;
  maxBytes?: number;
}

export interface ContextCreateParams {
  projectId: string;
  workItemId: string;
  query: string;
  selectedSources?: unknown[];
}

export interface AttachmentImportParams {
  workItemId: string;
  filename: string;
  contentBase64: string;
}

export interface AttachmentListParams {
  workItemId: string;
}

export interface AttachmentParseParams {
  attachmentId: string;
  state: string;
  extractedText?: string;
  error?: string;
}

export interface AttachmentRemoveParams {
  attachmentId: string;
}

export interface WorkitemListParams {
  projectId: string;
  cursor?: string;
  limit?: number;
}

export interface WorkitemGetParams {
  workItemId: string;
}

export interface WorkitemCreateParams {
  projectId: string;
  title: string;
  description?: string;
  gitlabIssueIid?: string;
  labels?: unknown[];
}

export interface WorkitemSetStageParams {
  workItemId: string;
  gate: string;
  state: string;
  inputBaselineSha?: string;
}

export interface WorkitemProgressParams {
  workItemId: string;
}

export interface WorkitemDocumentsParams {
  workItemId: string;
}

export interface WorkitemGetDocumentParams {
  workItemId: string;
  name: string;
}

export interface WorkitemImportDocumentParams {
  projectId: string;
  filename: string;
  content: string;
}

export interface WorkitemImportIssueParams {
  projectId: string;
  gitlabProjectId: string;
  issueIid: string;
}

export interface ArtifactListParams {
  workItemId: string;
}

export interface ArtifactCreateParams {
  workItemId: string;
  kind: string;
  title: string;
}

export interface ArtifactCreateDraftParams {
  artifactId: string;
  content: string;
}

export interface ArtifactUpdateDraftParams {
  revisionId: string;
  etag: string;
  content: string;
}

export interface ArtifactListRevisionsParams {
  artifactId: string;
}

export interface ArtifactRevisionContentParams {
  revisionId: string;
}

export interface ArtifactAddReviewParams {
  revisionId: string;
  reviewer: string;
  verdict: string;
  comment?: string;
  gitlabMrIid?: string;
}

export interface ArtifactFreezeBaselineParams {
  workItemId: string;
  gate: string;
  revisionIds: unknown[];
  gitlabCommitSha?: string;
}

export interface AgentRunParams {
  workItemId: string;
  goal: string;
  contextManifestId: string;
  toolAllowlist?: unknown[];
  idempotencyKey?: string;
  budget?: unknown;
}

export interface AgentGetParams {
  runId: string;
}

export interface AgentCancelParams {
  runId: string;
}

export interface AgentProposalsParams {
  runId: string;
}

export interface GateEvaluateParams {
  workItemId: string;
  gate: string;
}

export interface ApprovalListParams {
  limit?: number;
}

export interface ApprovalDecideParams {
  approvalId: string;
  decision: string;
  decidedBy: string;
  reason?: string;
}

export interface ApprovalListByWorkItemParams {
  workItemId: string;
}

export interface EvidenceListParams {
  workItemId: string;
  gate?: string;
}

export interface EvidenceRecordParams {
  workItemId: string;
  gate: string;
  kind: string;
  title?: string;
  content?: string;
  payload?: string;
  source?: string;
}

export interface EvidenceVerifyParams {
  evidenceId: string;
  verifiedBy: string;
}

export interface PassportIssueParams {
  workItemId: string;
  sharedSummary?: string;
}

export interface PassportLatestParams {
  workItemId: string;
}

export interface DeploymentCreateParams {
  workItemId: string;
  plan: unknown;
}

export interface DeploymentGetParams {
  deploymentId: string;
}

export interface DeploymentSubmitParams {
  deploymentId: string;
}

export interface DeploymentDeployParams {
  deploymentId: string;
}

export interface DeploymentVerifyParams {
  deploymentId: string;
}

export interface DeploymentRollbackParams {
  deploymentId: string;
}

export interface TimelineSnapshotParams {
  workItemId?: string;
  afterSeq?: number;
}

export type BackupCreateParams = Record<string, never>;

export interface AuditListParams {
  afterSeq?: number;
  limit?: number;
}
