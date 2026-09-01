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
  | 'context.instructions'
  | 'context.preview'
  | 'context.create'
  | 'attachment.import'
  | 'attachment.list'
  | 'attachment.parse'
  | 'attachment.remove'
  | 'workitem.list'
  | 'workitem.get'
  | 'workitem.create'
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
  | 'agent.start'
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
  | 'audit.list'
  | 'settings.summary'
  | 'settings.get'
  | 'settings.update'
  | 'settings.effective'
  | 'modelProfile.list'
  | 'modelProfile.get'
  | 'modelProfile.create'
  | 'modelProfile.update'
  | 'modelProfile.remove'
  | 'modelProfile.test'
  | 'modelProfile.syncModels'
  | 'modelRoute.get'
  | 'modelRoute.update'
  | 'toolPolicy.list'
  | 'toolPolicy.update'
  | 'toolPolicy.effective'
  | 'executionProfile.list'
  | 'executionProfile.create'
  | 'executionProfile.update'
  | 'executionProfile.remove'
  | 'gitlabProfile.list'
  | 'gitlabProfile.create'
  | 'gitlabProfile.update'
  | 'gitlabProfile.remove'
  | 'gitlabProfile.test'
  | 'gitlabProfile.capabilities'
  | 'sshTarget.list'
  | 'sshTarget.get'
  | 'sshTarget.create'
  | 'sshTarget.update'
  | 'sshTarget.remove'
  | 'sshTarget.test'
  | 'sshTarget.acceptHostKey'
  | 'credentialRef.list'
  | 'credentialRef.create'
  | 'credentialRef.replace'
  | 'credentialRef.remove'
  | 'credentialRef.verify'
  | 'backup.list'
  | 'backup.verify'
  | 'backup.restore'
  | 'backup.delete'
  | 'audit.get'
  | 'audit.export'
  | 'logs.list'
  | 'logs.exportDiagnosticBundle'
  | 'operation.get'
  | 'knowledge.settings.get'
  | 'knowledge.settings.update'
  | 'knowledge.searchV2'
  | 'project.inspectRoot'
  | 'update.check'
  | 'update.status'
  | 'executor.settings.get'
  | 'executor.settings.update'
  | 'executor.check'
  | 'diagnostics.run'
  | 'gitlabProfile.currentUser'
  | 'gitlabProfile.checkProjectPermissions'
  | 'sshTarget.bindProject'
  | 'knowledge.projectSettings.get'
  | 'knowledge.projectSettings.update'
  | 'backup.revealInFolder'
  | 'audit.settings.get'
  | 'audit.settings.update'
  | 'tool.list'
  | 'tool.test'
  | 'requirement.importRevision'
  | 'requirement.revisions'
  | 'requirement.items'
  | 'requirement.get'
  | 'trace.lineage'
  | 'trace.coverage'
  | 'trace.gaps'
  | 'gate.requestRelease'
  | 'gate.decideRelease'
  | 'gate.getRelease'
  | 'stage.attempts'
  | 'stage.package'
  | 'snapshot.get'
  | 'snapshot.list'
  | 'rollback.preview'
  | 'rollback.request'
  | 'rollback.decide'
  | 'rollback.get'
  | 'rollback.list'
  | 'stage.startActivity'
  | 'agentProfile.list'
  | 'agentProfile.create'
  | 'agentProfile.createVersion'
  | 'agentProfile.setEnabled'
  | 'agentBinding.list'
  | 'agentBinding.set'
  | 'agentBinding.remove'
  | 'agentBinding.resolvePreview';

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
  'context.instructions',
  'context.preview',
  'context.create',
  'attachment.import',
  'attachment.list',
  'attachment.parse',
  'attachment.remove',
  'workitem.list',
  'workitem.get',
  'workitem.create',
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
  'agent.start',
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
  'settings.summary',
  'settings.get',
  'settings.update',
  'settings.effective',
  'modelProfile.list',
  'modelProfile.get',
  'modelProfile.create',
  'modelProfile.update',
  'modelProfile.remove',
  'modelProfile.test',
  'modelProfile.syncModels',
  'modelRoute.get',
  'modelRoute.update',
  'toolPolicy.list',
  'toolPolicy.update',
  'toolPolicy.effective',
  'executionProfile.list',
  'executionProfile.create',
  'executionProfile.update',
  'executionProfile.remove',
  'gitlabProfile.list',
  'gitlabProfile.create',
  'gitlabProfile.update',
  'gitlabProfile.remove',
  'gitlabProfile.test',
  'gitlabProfile.capabilities',
  'sshTarget.list',
  'sshTarget.get',
  'sshTarget.create',
  'sshTarget.update',
  'sshTarget.remove',
  'sshTarget.test',
  'sshTarget.acceptHostKey',
  'credentialRef.list',
  'credentialRef.create',
  'credentialRef.replace',
  'credentialRef.remove',
  'credentialRef.verify',
  'backup.list',
  'backup.verify',
  'backup.restore',
  'backup.delete',
  'audit.get',
  'audit.export',
  'logs.list',
  'logs.exportDiagnosticBundle',
  'operation.get',
  'knowledge.settings.get',
  'knowledge.settings.update',
  'knowledge.searchV2',
  'project.inspectRoot',
  'update.check',
  'update.status',
  'executor.settings.get',
  'executor.settings.update',
  'executor.check',
  'diagnostics.run',
  'gitlabProfile.currentUser',
  'gitlabProfile.checkProjectPermissions',
  'sshTarget.bindProject',
  'knowledge.projectSettings.get',
  'knowledge.projectSettings.update',
  'backup.revealInFolder',
  'audit.settings.get',
  'audit.settings.update',
  'tool.list',
  'tool.test',
  'requirement.importRevision',
  'requirement.revisions',
  'requirement.items',
  'requirement.get',
  'trace.lineage',
  'trace.coverage',
  'trace.gaps',
  'gate.requestRelease',
  'gate.decideRelease',
  'gate.getRelease',
  'stage.attempts',
  'stage.package',
  'snapshot.get',
  'snapshot.list',
  'rollback.preview',
  'rollback.request',
  'rollback.decide',
  'rollback.get',
  'rollback.list',
  'stage.startActivity',
  'agentProfile.list',
  'agentProfile.create',
  'agentProfile.createVersion',
  'agentProfile.setEnabled',
  'agentBinding.list',
  'agentBinding.set',
  'agentBinding.remove',
  'agentBinding.resolvePreview',
] as const;

export const EVENT_TYPES: readonly string[] = [
  'agent.fallback_used',
  'agent.selection_resolved',
  'approval.approved',
  'approval.changes_requested',
  'approval.rejected',
  'approval.requested',
  'artifact.reviewed',
  'attachment.imported',
  'backup.changed',
  'baseline.frozen',
  'credentialRef.changed',
  'deployment.approved',
  'deployment.awaiting_approval',
  'deployment.awaiting_verification',
  'deployment.deploy_failed',
  'deployment.deploying',
  'deployment.draft',
  'deployment.rollback_failed',
  'deployment.rolled_back',
  'deployment.rolling_back',
  'deployment.verification_failed',
  'deployment.verified',
  'diagnostics.completed',
  'evidence.recorded',
  'executionProfile.changed',
  'gate.changes_requested',
  'gate.evaluated',
  'gate.release_approved',
  'gate.release_rejected',
  'gate.release_requested',
  'gitlabProfile.changed',
  'knowledge.scanned',
  'knowledge.settingsChanged',
  'modelProfile.changed',
  'modelRoute.changed',
  'operation.progress',
  'passport.issued',
  'policy.changed',
  'project.changed',
  'project.created',
  'requirement.revision_imported',
  'rollback.blocked',
  'rollback.cancelled',
  'rollback.completed',
  'rollback.failed',
  'rollback.previewed',
  'rollback.requested',
  'rollback.started',
  'run.cancelled',
  'run.compacted',
  'run.completed_execution',
  'run.failed',
  'run.resumed',
  'run.started',
  'run.waiting_approval',
  'settings.changed',
  'snapshot.created',
  'sshTarget.changed',
  'stage.attempt_approved',
  'stage.attempt_awaiting_user_approval',
  'stage.attempt_cancelled',
  'stage.attempt_changes_requested',
  'stage.attempt_failed',
  'stage.attempt_prepared',
  'stage.attempt_rejected',
  'stage.attempt_review_ready',
  'stage.attempt_rolled_back',
  'stage.attempt_running',
  'stage.attempt_superseded',
  'stage.passed',
  'stage.running',
  'stage.stale',
  'tool.completed',
  'tool.proposed',
  'tool.started',
  'trace.edge_created',
  'workitem.created',
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

export interface ContextInstructionsParams {
  projectId: string;
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
  requirementKeys?: unknown[];
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

export interface AgentStartParams {
  workItemId: string;
  goal: string;
  contextManifestId: string;
  toolAllowlist?: unknown[];
  budget?: unknown;
  taskId?: string;
  idempotencyKey?: string;
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
  requirementKeys?: unknown[];
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

export type SettingsSummaryParams = Record<string, never>;

export interface SettingsGetParams {
  scope: string;
  keys?: unknown[];
  projectId?: string;
}

export interface SettingsUpdateParams {
  scope: string;
  patches: unknown[];
  projectId?: string;
  expectedRevisions?: unknown;
}

export interface SettingsEffectiveParams {
  projectId?: string;
  keys?: unknown[];
}

export type ModelProfileListParams = Record<string, never>;

export interface ModelProfileGetParams {
  profileId: string;
}

export interface ModelProfileCreateParams {
  name: string;
  providerKind: string;
  baseUrl?: string;
  credentialRefId?: string;
  defaultModel?: string;
  capabilities?: unknown;
  limits?: unknown;
  dataPolicy?: unknown;
}

export interface ModelProfileUpdateParams {
  profileId: string;
  expectedRevision: number;
  name?: string;
  baseUrl?: string;
  credentialRefId?: string;
  defaultModel?: string;
  limits?: unknown;
  dataPolicy?: unknown;
}

export interface ModelProfileRemoveParams {
  profileId: string;
  expectedRevision: number;
}

export interface ModelProfileTestParams {
  profileId: string;
  credentialRefId?: string;
}

export interface ModelProfileSyncModelsParams {
  profileId: string;
}

export type ModelRouteGetParams = Record<string, never>;

export interface ModelRouteUpdateParams {
  route: unknown;
  expectedRevision: number;
}

export type ToolPolicyListParams = Record<string, never>;

export interface ToolPolicyUpdateParams {
  toolId: string;
  expectedRevision: number;
  enabled?: boolean;
  risk?: string;
  requiresApproval?: boolean;
  network?: string;
  projectId?: string;
}

export interface ToolPolicyEffectiveParams {
  projectId?: string;
}

export type ExecutionProfileListParams = Record<string, never>;

export interface ExecutionProfileCreateParams {
  name: string;
  mode: string;
  limits?: unknown;
}

export interface ExecutionProfileUpdateParams {
  profileId: string;
  expectedRevision: number;
  name?: string;
  mode?: string;
  limits?: unknown;
}

export interface ExecutionProfileRemoveParams {
  profileId: string;
  expectedRevision: number;
}

export type GitlabProfileListParams = Record<string, never>;

export interface GitlabProfileCreateParams {
  name: string;
  baseUrl: string;
  credentialRefId?: string;
}

export interface GitlabProfileUpdateParams {
  profileId: string;
  expectedRevision: number;
  name?: string;
  baseUrl?: string;
  credentialRefId?: string;
}

export interface GitlabProfileRemoveParams {
  profileId: string;
  expectedRevision: number;
}

export interface GitlabProfileTestParams {
  profileId: string;
  credentialRefId?: string;
}

export interface GitlabProfileCapabilitiesParams {
  profileId: string;
}

export type SshTargetListParams = Record<string, never>;

export interface SshTargetGetParams {
  targetId: string;
}

export interface SshTargetCreateParams {
  name: string;
  host: string;
  user: string;
  port?: number;
  remoteDir?: string;
  credentialRefId?: string;
  jumpHost?: string;
}

export interface SshTargetUpdateParams {
  targetId: string;
  expectedRevision: number;
  name?: string;
  host?: string;
  port?: number;
  user?: string;
  remoteDir?: string;
  credentialRefId?: string;
  jumpHost?: string;
}

export interface SshTargetRemoveParams {
  targetId: string;
  expectedRevision: number;
}

export interface SshTargetTestParams {
  targetId: string;
}

export interface SshTargetAcceptHostKeyParams {
  targetId: string;
  fingerprint: string;
  expectedRevision?: number;
}

export type CredentialRefListParams = Record<string, never>;

export interface CredentialRefCreateParams {
  name: string;
  kind: string;
  secret: string;
  provider?: string;
  scope?: string;
  projectId?: string;
}

export interface CredentialRefReplaceParams {
  refId: string;
  secret: string;
  expectedRevision: number;
}

export interface CredentialRefRemoveParams {
  refId: string;
  expectedRevision: number;
  force?: boolean;
}

export interface CredentialRefVerifyParams {
  refId: string;
}

export type BackupListParams = Record<string, never>;

export interface BackupVerifyParams {
  backupId: string;
}

export interface BackupRestoreParams {
  backupId: string;
}

export interface BackupDeleteParams {
  backupId: string;
}

export interface AuditGetParams {
  entryId: string;
}

export interface AuditExportParams {
  filters?: unknown;
  afterSeq?: number;
  limit?: number;
}

export interface LogsListParams {
  limit?: number;
}

export type LogsExportDiagnosticBundleParams = Record<string, never>;

export interface OperationGetParams {
  operationId: string;
}

export interface KnowledgeSettingsGetParams {
  projectId?: string;
}

export interface KnowledgeSettingsUpdateParams {
  settings: unknown;
  expectedRevision: number;
  projectId?: string;
}

export interface KnowledgeSearchV2Params {
  projectId: string;
  query: string;
  includeTests?: boolean;
  limit?: number;
}

export interface ProjectInspectRootParams {
  path: string;
}

export type UpdateCheckParams = Record<string, never>;

export type UpdateStatusParams = Record<string, never>;

export type ExecutorSettingsGetParams = Record<string, never>;

export interface ExecutorSettingsUpdateParams {
  settings: unknown;
  expectedRevision: number;
}

export type ExecutorCheckParams = Record<string, never>;

export interface DiagnosticsRunParams {
  checkId: string;
}

export interface GitlabProfileCurrentUserParams {
  profileId: string;
}

export interface GitlabProfileCheckProjectPermissionsParams {
  profileId: string;
  namespace: string;
  project: string;
}

export interface SshTargetBindProjectParams {
  targetId: string;
  projectId: string;
  bind?: boolean;
  allowAutoDeploy?: boolean;
}

export interface KnowledgeProjectSettingsGetParams {
  projectId: string;
}

export interface KnowledgeProjectSettingsUpdateParams {
  projectId: string;
  settings: unknown;
  expectedRevision: number;
}

export interface BackupRevealInFolderParams {
  backupId: string;
}

export type AuditSettingsGetParams = Record<string, never>;

export interface AuditSettingsUpdateParams {
  settings: unknown;
  expectedRevision: number;
}

export type ToolListParams = Record<string, never>;

export interface ToolTestParams {
  toolId: string;
}

export interface RequirementImportRevisionParams {
  workItemId: string;
  filename: string;
  content: string;
  sourceKind?: string;
  createdBy?: string;
}

export interface RequirementRevisionsParams {
  workItemId: string;
}

export interface RequirementItemsParams {
  revisionId: string;
}

export interface RequirementGetParams {
  revisionId: string;
}

export interface TraceLineageParams {
  nodeId: string;
  direction?: string;
  depth?: number;
}

export interface TraceCoverageParams {
  workItemId: string;
  revisionId?: string;
}

export interface TraceGapsParams {
  workItemId: string;
}

export interface GateRequestReleaseParams {
  workItemId: string;
  gate: string;
}

export interface GateDecideReleaseParams {
  approvalId: string;
  decision: string;
  decidedBy: string;
  reason?: string;
}

export interface GateGetReleaseParams {
  releaseId: string;
}

export interface StageAttemptsParams {
  workItemId: string;
}

export interface StagePackageParams {
  workItemId: string;
  gate: string;
}

export interface SnapshotGetParams {
  snapshotId: string;
}

export interface SnapshotListParams {
  workItemId: string;
}

export interface RollbackPreviewParams {
  workItemId: string;
  targetSnapshotId: string;
}

export interface RollbackRequestParams {
  workItemId: string;
  targetSnapshotId: string;
  requestedBy?: string;
}

export interface RollbackDecideParams {
  approvalId: string;
  decision: string;
  decidedBy: string;
  reason?: string;
}

export interface RollbackGetParams {
  operationId: string;
}

export interface RollbackListParams {
  workItemId: string;
}

export interface StageStartActivityParams {
  workItemId: string;
  gate: string;
  goal: string;
  activityKey?: string;
  profileVersionId?: string;
  requiredCapabilities?: unknown[];
  toolAllowlist?: unknown[];
  idempotencyKey?: string;
}

export interface AgentProfileListParams {
  projectId?: string;
}

export interface AgentProfileCreateParams {
  name: string;
  adapterKind: string;
  projectId?: string;
}

export interface AgentProfileCreateVersionParams {
  profileId: string;
  persona?: string;
  sop?: string;
  capabilities?: unknown[];
  outputSchema?: string;
  modelRoute?: string;
  budget?: string;
}

export interface AgentProfileSetEnabledParams {
  profileId: string;
  enabled: boolean;
}

export interface AgentBindingListParams {
  projectId?: string;
}

export interface AgentBindingSetParams {
  gate: string;
  activityKey: string;
  profileVersionId: string;
  projectId?: string;
  fallbackMode?: string;
  priority?: number;
}

export interface AgentBindingRemoveParams {
  bindingId: string;
}

export interface AgentBindingResolvePreviewParams {
  projectId: string;
  gate: string;
  activityKey: string;
  requiredCapabilities?: unknown[];
}
