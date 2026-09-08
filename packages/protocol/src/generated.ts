// 本文件由 generate.mjs 从 contracts/rpc/ratiflow.json 生成；不要手写修改。
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
  | 'workitem.archive'
  | 'workitem.archive'
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
  | 'modelProvider.presets'
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
  | 'gate.requestManualConfirmation'
  | 'gate.manualConfirmations'
  | 'gate.requestSkip'
  | 'gate.evaluateFastTrack'
  | 'rework.preview'
  | 'rework.request'
  | 'rework.decide'
  | 'rework.get'
  | 'rework.list'
  | 'metrics.overview'
  | 'triage.list'
  | 'workitem.searchRebuild'
  | 'automation.setShadowMode'
  | 'workitem.similar'
  | 'knowledge.verifySource'
  | 'knowledge.freshnessOverview'
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
  | 'agentBinding.resolvePreview'
  | 'memory.settingsGet'
  | 'memory.settingsUpdate'
  | 'memory.list'
  | 'memory.get'
  | 'memory.create'
  | 'memory.update'
  | 'memory.pin'
  | 'memory.archive'
  | 'memory.restore'
  | 'memory.purgePreview'
  | 'memory.purge'
  | 'memory.search'
  | 'memory.contextPreview'
  | 'memory.import'
  | 'memory.export'
  | 'memory.captureStart'
  | 'memory.captureGet'
  | 'memory.candidateList'
  | 'memory.candidateDecide'
  | 'knowledge.manifestCreate'
  | 'knowledge.manifestUpdate'
  | 'knowledge.manifestRemove'
  | 'knowledge.syncFromRepo'
  | 'model.usage'
  | 'mcp.serverAdd'
  | 'mcp.serverApprove'
  | 'mcp.serverList'
  | 'mcp.serverRemove'
  | 'mcp.serverRefresh'
  | 'mcp.serverToggle'
  | 'mcp.toolsList'
  | 'gate.deliverableStatus'
  | 'skill.list'
  | 'skill.get'
  | 'skill.create'
  | 'skill.update'
  | 'skill.setEnabled'
  | 'skill.remove'
  | 'memory.syncFromRepo'
  | 'project.gitStatus'
  | 'workflowTemplate.list'
  | 'workflowTemplate.get'
  | 'workflowTemplate.create'
  | 'workflowTemplate.updateDraft'
  | 'workflowTemplate.activate'
  | 'workflowTemplate.deprecate'
  | 'workflow.getInstance'
  | 'workflow.migrationPreview'
  | 'workflow.migrate'
  | 'plan.createDraft'
  | 'plan.updateDraft'
  | 'plan.get'
  | 'plan.list'
  | 'plan.submit'
  | 'plan.decide'
  | 'plan.start'
  | 'plan.cancel'
  | 'taskWorkspace.prepare'
  | 'taskWorkspace.get'
  | 'taskWorkspace.finalize'
  | 'plan.replanPreview'
  | 'plan.replan'
  | 'planTask.list'
  | 'planTask.prepare'
  | 'planTask.transition'
  | 'planTask.reconcile'
  | 'plan.startRunning'
  | 'plan.dispatchReady'
  | 'agentTeam.list'
  | 'agentTeam.create'
  | 'agentTeam.createVersion'
  | 'agentTeam.activate'
  | 'agentTeam.resolvePreview'
  | 'contextPolicy.createVersion'
  | 'contextPolicy.activate'
  | 'contextPolicy.activeList'
  | 'middlewareProfile.createVersion'
  | 'middlewareProfile.activate'
  | 'middlewareProfile.validate'
  | 'skill.versionList'
  | 'skill.createVersion'
  | 'skill.activateVersion'
  | 'skill.deprecateVersion'
  | 'skill.revokeVersion'
  | 'skill.bindVersion'
  | 'skill.activeList'
  | 'trace.graph'
  | 'trace.usage'
  | 'trace.taskReadModel'
  | 'trace.restoreCheckpoint'
  | 'command.preview'
  | 'command.execute'
  | 'automation.create'
  | 'automation.list'
  | 'automation.pause'
  | 'automation.resume'
  | 'automation.runNow'
  | 'automation.history'
  | 'automation.decideSuggestion'
  | 'automation.reviewSuggestion'
  | 'automation.observations'
  | 'goal.autoReleaseCheck'
  | 'notification.list'
  | 'skill.importFromRegistry'
  | 'skill.marketList'
  | 'skill.marketImport'
  | 'skill.marketSourceSave'
  | 'skill.marketSourceRemove'
  | 'skill.marketPluginSkills'
  | 'autonomy.createGrant'
  | 'autonomy.revokeGrant'
  | 'mcp.importAdd'
  | 'mcp.importDecide'
  | 'mcp.importResume'
  | 'mcp.importRevoke'
  | 'mcp.importList'
  | 'mcp.importGet'
  | 'impact.forProposal';

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
  'workitem.archive',
  'workitem.archive',
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
  'modelProvider.presets',
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
  'gate.requestManualConfirmation',
  'gate.manualConfirmations',
  'gate.requestSkip',
  'gate.evaluateFastTrack',
  'rework.preview',
  'rework.request',
  'rework.decide',
  'rework.get',
  'rework.list',
  'metrics.overview',
  'triage.list',
  'workitem.searchRebuild',
  'automation.setShadowMode',
  'workitem.similar',
  'knowledge.verifySource',
  'knowledge.freshnessOverview',
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
  'memory.settingsGet',
  'memory.settingsUpdate',
  'memory.list',
  'memory.get',
  'memory.create',
  'memory.update',
  'memory.pin',
  'memory.archive',
  'memory.restore',
  'memory.purgePreview',
  'memory.purge',
  'memory.search',
  'memory.contextPreview',
  'memory.import',
  'memory.export',
  'memory.captureStart',
  'memory.captureGet',
  'memory.candidateList',
  'memory.candidateDecide',
  'knowledge.manifestCreate',
  'knowledge.manifestUpdate',
  'knowledge.manifestRemove',
  'knowledge.syncFromRepo',
  'model.usage',
  'mcp.serverAdd',
  'mcp.serverApprove',
  'mcp.serverList',
  'mcp.serverRemove',
  'mcp.serverRefresh',
  'mcp.serverToggle',
  'mcp.toolsList',
  'gate.deliverableStatus',
  'skill.list',
  'skill.get',
  'skill.create',
  'skill.update',
  'skill.setEnabled',
  'skill.remove',
  'memory.syncFromRepo',
  'project.gitStatus',
  'workflowTemplate.list',
  'workflowTemplate.get',
  'workflowTemplate.create',
  'workflowTemplate.updateDraft',
  'workflowTemplate.activate',
  'workflowTemplate.deprecate',
  'workflow.getInstance',
  'workflow.migrationPreview',
  'workflow.migrate',
  'plan.createDraft',
  'plan.updateDraft',
  'plan.get',
  'plan.list',
  'plan.submit',
  'plan.decide',
  'plan.start',
  'plan.cancel',
  'taskWorkspace.prepare',
  'taskWorkspace.get',
  'taskWorkspace.finalize',
  'plan.replanPreview',
  'plan.replan',
  'planTask.list',
  'planTask.prepare',
  'planTask.transition',
  'planTask.reconcile',
  'plan.startRunning',
  'plan.dispatchReady',
  'agentTeam.list',
  'agentTeam.create',
  'agentTeam.createVersion',
  'agentTeam.activate',
  'agentTeam.resolvePreview',
  'contextPolicy.createVersion',
  'contextPolicy.activate',
  'contextPolicy.activeList',
  'middlewareProfile.createVersion',
  'middlewareProfile.activate',
  'middlewareProfile.validate',
  'skill.versionList',
  'skill.createVersion',
  'skill.activateVersion',
  'skill.deprecateVersion',
  'skill.revokeVersion',
  'skill.bindVersion',
  'skill.activeList',
  'trace.graph',
  'trace.usage',
  'trace.taskReadModel',
  'trace.restoreCheckpoint',
  'command.preview',
  'command.execute',
  'automation.create',
  'automation.list',
  'automation.pause',
  'automation.resume',
  'automation.runNow',
  'automation.history',
  'automation.decideSuggestion',
  'automation.reviewSuggestion',
  'automation.observations',
  'goal.autoReleaseCheck',
  'notification.list',
  'skill.importFromRegistry',
  'skill.marketList',
  'skill.marketImport',
  'skill.marketSourceSave',
  'skill.marketSourceRemove',
  'skill.marketPluginSkills',
  'autonomy.createGrant',
  'autonomy.revokeGrant',
  'mcp.importAdd',
  'mcp.importDecide',
  'mcp.importResume',
  'mcp.importRevoke',
  'mcp.importList',
  'mcp.importGet',
  'impact.forProposal',
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
  'memory.archived',
  'memory.candidate_accepted',
  'memory.candidate_created',
  'memory.candidate_rejected',
  'memory.capture_failed',
  'memory.capture_started',
  'memory.capture_succeeded',
  'memory.capture_unknown',
  'memory.created',
  'memory.pinned',
  'memory.purged',
  'memory.restored',
  'memory.settings_changed',
  'memory.updated',
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
  'workflow.template_activated',
  'workflow.instance_created',
  'workflow.instance_migrated',
  'plan.draft_created',
  'plan.approval_requested',
  'plan.approved',
  'plan.rejected',
  'plan.started',
  'plan.superseded',
  'plan.replan_required',
  'plan.replanned',
  'task.succeeded',
  'task.failed',
  'task.unknown',
  'task.cancelled',
  'trace.span_completed',
  'command.previewed',
  'command.executed',
  'context.tools_excluded',
  'automation.triggered',
  'automation.skipped',
  'automation.paused',
  'automation.failed',
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
  includeArchived?: boolean;
}

export interface WorkitemArchiveParams {
  workItemId: string;
}

export interface WorkitemArchiveParams {
  workItemId: string;
  archived?: boolean;
}

export interface WorkitemCreateParams {
  projectId: string;
  title: string;
  description?: string;
  gitlabIssueIid?: string;
  labels?: unknown[];
  templateId?: string;
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

export type ModelProviderPresetsParams = Record<string, never>;

export type ModelProfileListParams = Record<string, never>;

export interface ModelProfileGetParams {
  profileId: string;
}

export interface ModelProfileCreateParams {
  name: string;
  providerKind: string;
  baseUrl?: string;
  credentialRefId?: string;
  apiKey?: string;
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

export interface GateRequestManualConfirmationParams {
  workItemId: string;
  gate: string;
  element: unknown;
  requestedBy: string;
  reason?: string;
}

export interface GateManualConfirmationsParams {
  workItemId: string;
  gate?: string;
}

export interface GateRequestSkipParams {
  workItemId: string;
  gateId: string;
  waiver: string;
  substituteEvidenceIds: unknown[];
}

export interface GateEvaluateFastTrackParams {
  workItemId: string;
  gate: string;
  factors: unknown;
}

export interface ReworkPreviewParams {
  workItemId: string;
  targetGate: string;
  reasonCode: string;
  note?: string;
  requestedBy?: string;
}

export interface ReworkRequestParams {
  workItemId: string;
  targetGate: string;
  reasonCode: string;
  note?: string;
  requestedBy?: string;
}

export interface ReworkDecideParams {
  approvalId: string;
  decision: string;
  decidedBy: string;
  reason?: string;
}

export interface ReworkGetParams {
  operationId: string;
}

export interface ReworkListParams {
  workItemId: string;
}

export interface MetricsOverviewParams {
  scope: string;
}

export type TriageListParams = Record<string, never>;

export type WorkitemSearchRebuildParams = Record<string, never>;

export interface AutomationSetShadowModeParams {
  automationId: string;
  shadowMode: boolean;
  expectedRevision: unknown;
}

export interface WorkitemSimilarParams {
  workItemId: string;
}

export interface KnowledgeVerifySourceParams {
  projectId: string;
  stableId: string;
  outcome: string;
  verifier: string;
  evidenceRef?: string;
}

export interface KnowledgeFreshnessOverviewParams {
  projectId: string;
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

export interface MemorySettingsGetParams {
  projectId: string;
}

export interface MemorySettingsUpdateParams {
  projectId: string;
  settings: unknown;
  expectedRevision: number;
  idempotencyKey: string;
}

export interface MemoryListParams {
  projectId: string;
  query?: string;
  statuses?: unknown[];
  kinds?: unknown[];
  cursor?: string;
  limit?: number;
}

export interface MemoryGetParams {
  projectId: string;
  memoryId: string;
  revisionId?: string;
}

export interface MemoryCreateParams {
  projectId: string;
  title: string;
  kind: string;
  body: string;
  idempotencyKey: string;
  tags?: unknown[];
  sourceRefs?: unknown[];
}

export interface MemoryUpdateParams {
  projectId: string;
  memoryId: string;
  expectedRevision: number;
  idempotencyKey: string;
  title?: string;
  body?: string;
  tags?: unknown[];
}

export interface MemoryPinParams {
  projectId: string;
  memoryId: string;
  pinned: boolean;
  expectedRevision: number;
  idempotencyKey: string;
}

export interface MemoryArchiveParams {
  projectId: string;
  memoryId: string;
  expectedRevision: number;
  idempotencyKey: string;
}

export interface MemoryRestoreParams {
  projectId: string;
  memoryId: string;
  expectedRevision: number;
  idempotencyKey: string;
}

export interface MemoryPurgePreviewParams {
  projectId: string;
  memoryId: string;
}

export interface MemoryPurgeParams {
  projectId: string;
  memoryId: string;
  expectedRevision: number;
  confirmationToken: string;
  idempotencyKey: string;
}

export interface MemorySearchParams {
  projectId: string;
  query: string;
  kinds?: unknown[];
  limit?: number;
}

export interface MemoryContextPreviewParams {
  projectId: string;
  goal: string;
  workItemId?: string;
  gate?: string;
  activityKey?: string;
  maxBytes?: number;
}

export interface MemoryImportParams {
  projectId: string;
  filename: string;
  contentBase64: string;
  mode: string;
  idempotencyKey: string;
}

export interface MemoryExportParams {
  projectId: string;
  format: string;
  memoryIds?: unknown[];
  includeArchived?: boolean;
}

export interface MemoryCaptureStartParams {
  projectId: string;
  runId: string;
  idempotencyKey: string;
}

export interface MemoryCaptureGetParams {
  projectId: string;
  jobId: string;
}

export interface MemoryCandidateListParams {
  projectId: string;
  status?: string;
  cursor?: string;
  limit?: number;
}

export interface MemoryCandidateDecideParams {
  projectId: string;
  candidateId: string;
  decision: string;
  idempotencyKey: string;
  editedContent?: string;
}

export interface KnowledgeManifestCreateParams {
  projectId: string;
  opId: string;
  kind: string;
  name: string;
  expectedAbsent: boolean;
  locator?: string;
  body?: string;
  enabled?: boolean;
}

export interface KnowledgeManifestUpdateParams {
  projectId: string;
  opId: string;
  stableId: string;
  expectedManifestSha256: string;
  name?: string;
  enabled?: boolean;
}

export interface KnowledgeManifestRemoveParams {
  projectId: string;
  opId: string;
  stableId: string;
  expectedManifestSha256: string;
}

export interface KnowledgeSyncFromRepoParams {
  projectId: string;
}

export interface ModelUsageParams {
  runId?: string;
}

export interface McpServerAddParams {
  name: string;
  command: string;
  args?: unknown[];
}

export interface McpServerApproveParams {
  serverId: string;
  decidedBy: string;
}

export type McpServerListParams = Record<string, never>;

export interface McpServerRemoveParams {
  serverId: string;
  decidedBy: string;
  reason?: string;
}

export interface McpServerRefreshParams {
  serverId: string;
}

export interface McpServerToggleParams {
  serverId: string;
  enabled: boolean;
}

export interface McpToolsListParams {
  serverId?: string;
}

export interface GateDeliverableStatusParams {
  workItemId: string;
  gate: string;
}

export type SkillListParams = Record<string, never>;

export interface SkillGetParams {
  skillId: string;
}

export interface SkillCreateParams {
  name: string;
  body: string;
  description?: string;
  source?: string;
  agentProfileId?: string;
}

export interface SkillUpdateParams {
  skillId: string;
  expectedRevision: unknown;
  description?: string;
  body?: string;
  agentProfileId?: string;
}

export interface SkillSetEnabledParams {
  skillId: string;
  enabled: boolean;
  expectedRevision: unknown;
}

export interface SkillRemoveParams {
  skillId: string;
  expectedRevision: unknown;
}

export interface MemorySyncFromRepoParams {
  projectId: string;
}

export interface ProjectGitStatusParams {
  projectId: string;
}

export type WorkflowTemplateListParams = Record<string, never>;

export interface WorkflowTemplateGetParams {
  templateId: string;
}

export interface WorkflowTemplateCreateParams {
  key: string;
  name: string;
  gates: unknown[];
  idempotencyKey: string;
}

export interface WorkflowTemplateUpdateDraftParams {
  versionId: string;
  gates: unknown[];
  idempotencyKey: string;
}

export interface WorkflowTemplateActivateParams {
  versionId: string;
  idempotencyKey: string;
}

export interface WorkflowTemplateDeprecateParams {
  versionId: string;
  idempotencyKey: string;
}

export interface WorkflowGetInstanceParams {
  workItemId: string;
}

export interface WorkflowMigrationPreviewParams {
  workItemId: string;
  targetVersionId: string;
}

export interface WorkflowMigrateParams {
  workItemId: string;
  targetVersionId: string;
  idempotencyKey: string;
}

export interface PlanCreateDraftParams {
  workItemId: string;
  tasks: unknown[];
  idempotencyKey: string;
  stageAttemptId?: string;
}

export interface PlanUpdateDraftParams {
  planRevisionId: string;
  tasks: unknown[];
  idempotencyKey: string;
}

export interface PlanGetParams {
  planRevisionId?: string;
  workItemId?: string;
}

export interface PlanListParams {
  workItemId: string;
}

export interface PlanSubmitParams {
  planRevisionId: string;
}

export interface PlanDecideParams {
  planRevisionId: string;
  decision: string;
  decidedBy: string;
  reason?: string;
}

export interface PlanStartParams {
  planRevisionId: string;
  idempotencyKey: string;
}

export interface PlanCancelParams {
  planRevisionId: string;
  idempotencyKey: string;
}

export interface TaskWorkspacePrepareParams {
  taskAttemptId: string;
}

export interface TaskWorkspaceGetParams {
  taskAttemptId: string;
}

export interface TaskWorkspaceFinalizeParams {
  taskAttemptId: string;
  outcome: string;
  idempotencyKey: string;
}

export interface PlanReplanPreviewParams {
  planRevisionId: string;
  roots: unknown[];
}

export interface PlanReplanParams {
  planRevisionId: string;
  roots: unknown[];
  tasks: unknown[];
  idempotencyKey: string;
}

export interface PlanTaskListParams {
  planRevisionId: string;
}

export interface PlanTaskPrepareParams {
  taskAttemptId: string;
}

export interface PlanTaskTransitionParams {
  taskAttemptId: string;
  outcome: string;
  idempotencyKey: string;
  outputDigest?: string;
}

export interface PlanTaskReconcileParams {
  taskAttemptId: string;
  resolution: string;
  outputDigest?: string;
}

export interface PlanStartRunningParams {
  taskAttemptId: string;
}

export interface PlanDispatchReadyParams {
  planRevisionId: string;
  maxParallel?: number;
}

export type AgentTeamListParams = Record<string, never>;

export interface AgentTeamCreateParams {
  key: string;
  name: string;
}

export interface AgentTeamCreateVersionParams {
  teamId: string;
  leadRoleKey: string;
  members: unknown[];
  maxConcurrency?: number;
  reviewPolicy?: string;
  fallbackMode?: string;
}

export interface AgentTeamActivateParams {
  versionId: string;
}

export interface AgentTeamResolvePreviewParams {
  teamVersionId: string;
  roleKey: string;
}

export interface ContextPolicyCreateVersionParams {
  key: string;
  allowedTools: unknown[];
  gateId?: string;
  sources?: unknown[];
}

export interface ContextPolicyActivateParams {
  versionId: string;
}

export interface ContextPolicyActiveListParams {
  key: string;
  clientRequest?: unknown[];
}

export interface MiddlewareProfileCreateVersionParams {
  key: string;
  steps: unknown[];
}

export interface MiddlewareProfileActivateParams {
  versionId: string;
}

export interface MiddlewareProfileValidateParams {
  steps: unknown[];
}

export interface SkillVersionListParams {
  skillId: string;
}

export interface SkillCreateVersionParams {
  skillId: string;
  body: string;
  description?: string;
}

export interface SkillActivateVersionParams {
  versionId: string;
}

export interface SkillDeprecateVersionParams {
  versionId: string;
}

export interface SkillRevokeVersionParams {
  versionId: string;
}

export interface SkillBindVersionParams {
  skillVersionId: string;
  profileVersionId?: string;
}

export interface SkillActiveListParams {
  profileVersionId?: string;
}

export interface TraceGraphParams {
  workItemId: string;
}

export interface TraceUsageParams {
  workItemId: string;
}

export interface TraceTaskReadModelParams {
  workItemId: string;
}

export interface TraceRestoreCheckpointParams {
  workItemId: string;
}

export interface CommandPreviewParams {
  workItemId: string;
  text: string;
}

export interface CommandExecuteParams {
  workItemId: string;
  text: string;
  previewToken: string;
}

export interface AutomationCreateParams {
  key: string;
  intent: unknown;
  intervalSecs: number;
  workItemId?: string;
  misfirePolicy?: string;
  overlapPolicy?: string;
  autonomyGrantId?: string;
}

export type AutomationListParams = Record<string, never>;

export interface AutomationPauseParams {
  automationId: string;
  expectedRevision: number;
}

export interface AutomationResumeParams {
  automationId: string;
  expectedRevision: number;
}

export interface AutomationRunNowParams {
  automationId: string;
  scheduledFor?: string;
}

export interface AutomationHistoryParams {
  automationId: string;
}

export interface AutomationDecideSuggestionParams {
  suggestionId: string;
  decision: string;
  decidedBy: string;
  note: string;
}

export interface AutomationReviewSuggestionParams {
  suggestionId: string;
  falsePositive: boolean;
  reviewer: string;
  note: string;
}

export interface AutomationObservationsParams {
  source?: string;
  automationId?: string;
}

export interface GoalAutoReleaseCheckParams {
  workItemId: string;
  grantId: string;
}

export type NotificationListParams = Record<string, never>;

export interface SkillImportFromRegistryParams {
  repoUrl: string;
  pinSha: string;
}

export type SkillMarketListParams = Record<string, never>;

export interface SkillMarketImportParams {
  sourceId: string;
  plugin: string;
  version: string;
  skillName: string;
}

export interface SkillMarketSourceSaveParams {
  kind: string;
  name: string;
  rootPath: string;
  sourceId?: string;
  marketplaceId?: string;
  enabled?: boolean;
  expectedRevision?: number;
}

export interface SkillMarketSourceRemoveParams {
  sourceId: string;
  expectedRevision: number;
}

export interface SkillMarketPluginSkillsParams {
  sourceId: string;
  plugin: string;
}

export interface AutonomyCreateGrantParams {
  workItemId: string;
  allowedTools?: unknown[];
  allowedRisks?: unknown[];
  expiresAt?: string;
  limits?: unknown;
}

export interface AutonomyRevokeGrantParams {
  grantId: string;
  reason?: string;
}

export interface McpImportAddParams {
  repoUrl: string;
  ref: string;
  idempotencyKey: string;
  createdBy?: string;
}

export interface McpImportDecideParams {
  importId: string;
  decision: string;
  decidedBy: string;
  idempotencyKey: string;
  reason?: string;
}

export interface McpImportResumeParams {
  importId: string;
  idempotencyKey: string;
}

export interface McpImportRevokeParams {
  importId: string;
  decidedBy: string;
  idempotencyKey: string;
  reason?: string;
}

export type McpImportListParams = Record<string, never>;

export interface McpImportGetParams {
  importId: string;
}

export interface ImpactForProposalParams {
  proposalId: string;
}
