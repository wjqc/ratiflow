//! P0-1（审计《Ratiflow_RDWS_v1.4实施审计与整改方案_v1.0》§5.1/§7 P0-1）：
//! RPC 方法注册表——read/mutation 分类、receipt 模式与领域幂等键的单一声明源。
//!
//! - `ReceiptMode::Required` 的 mutation：dispatch 入口先于一切 handler 强制
//!   `idempotencyKey`（缺 key → idempotency_key_required），handler 内经
//!   `with_rpc_receipt`/`with_rpc_receipt_tx` 执行（指纹门 + lease CAS 单 owner +
//!   completed envelope 重放）。
//! - `domain_key` 记录 transport receipt 之外由哪条 schema 约束/CAS 承载领域幂等
//!   （审计 §5.1：外部副作用不能依赖 transport receipt 防双花）。
//! - 注册表与 `contracts/rpc/ratiflow.json` 由 `packages/protocol/generate.mjs`
//!   做双向静态校验（方法集一致；Required ⇔ contract 必填 idempotencyKey），
//!   新增方法必须先入注册表再做契约，防止 mutation 漏声明。

/// 方法分类：纯读投影 / 产生领域写。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Read,
    Mutation,
}

/// 回执模式：无需 transport 回执 / 必须 idempotencyKey + rpc_receipts lease。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptMode {
    None,
    Required,
}

/// kind/domain_key 当前由本模块 #[cfg(test)] 静态自证与 generate.mjs 双向校验消费
/// （read/mutation 全量分类是审计 §5.1 的声明要求，运行时只消费 receipt 字段）。
#[allow(dead_code)]
pub struct Entry {
    pub method: &'static str,
    pub kind: Kind,
    pub receipt: ReceiptMode,
    /// 领域幂等键说明（schema 约束或 CAS；空 = 仅 transport receipt）。
    pub domain_key: &'static str,
}

pub const REGISTRY: &[Entry] = &[
    // BEGIN_REGISTRY_ENTRIES（generate.mjs 按标记双向校验；勿删标记行）
    Entry { method: "agent.cancel", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "agent.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "agent.proposals", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "agent.start", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "agent_runs 状态机（run_id 单权威）" },
    Entry { method: "agentBinding.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "agentBinding.remove", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "agentBinding.resolvePreview", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "agentBinding.set", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "agentProfile.create", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "agentProfile.createVersion", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "agentProfile.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "agentProfile.setEnabled", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "agentTeam.activate", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "agentTeam.create", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "agentTeam.createVersion", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "agentTeam.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "agentTeam.resolvePreview", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "approval.decide", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "approval.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "approval.listByWorkItem", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "artifact.addReview", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "artifact.create", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "artifact.createDraft", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "artifact.freezeBaseline", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "artifact.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "artifact.listRevisions", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "artifact.revisionContent", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "artifact.updateDraft", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "attachment.import", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "attachment.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "attachment.parse", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "attachment.remove", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "audit.export", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "audit.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "audit.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "audit.settings.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "audit.settings.update", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "automation.create", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "automation.decideSuggestion", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "shadow_decisions PK(suggestion_id)" },
    Entry { method: "automation.history", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "automation.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "automation.observations", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "automation.pause", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "automation.resume", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "automation.reviewSuggestion", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "shadow_reviews append-only（transport receipt）" },
    Entry { method: "automation.runNow", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "automation.setShadowMode", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "automations revision CAS" },
    Entry { method: "autonomy.createGrant", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "autonomy.revokeGrant", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "backup.create", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "backup.delete", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "backup.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "backup.restore", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "backup.revealInFolder", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "backup.verify", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "command.execute", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "command.preview", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "context.create", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "context.instructions", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "context.preview", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "contextPolicy.activate", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "contextPolicy.activeList", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "contextPolicy.createVersion", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "core.version", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "credentialRef.create", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "credentialRef.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "credentialRef.remove", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "credentialRef.replace", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "credentialRef.verify", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "deployment.create", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "deployment.deploy", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "deployment.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "deployment.rollback", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "deployment.submit", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "deployment.verify", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "diagnostics.check", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "diagnostics.run", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "evidence.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "evidence.record", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "evidence.verify", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "executionProfile.create", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "executionProfile.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "executionProfile.remove", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "executionProfile.update", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "executor.check", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "executor.settings.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "executor.settings.update", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "gate.applyWaiver", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "gate_fast_track_waivers UNIQUE(action_digest)（0052）" },
    Entry { method: "gate.decideRelease", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "gate.decideSkip", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "gate_skip_requests UNIQUE(approval_id)（0052）" },
    Entry { method: "gate.deliverableStatus", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "gate.evaluate", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "gate.evaluateFastTrack", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "shadow_suggestions UNIQUE(suggestion_digest)（0050）" },
    Entry { method: "gate.getRelease", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "gate.manualConfirmations", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "gate.requestManualConfirmation", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "manual_confirmations 服务端 id（0052 升级 operation）" },
    Entry { method: "gate.requestRelease", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "gate.requestSkip", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "gate_skip_requests UNIQUE(action_digest)（0052）" },
    Entry { method: "gate.resumeSkip", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "gate_skip_requests progress 游标幂等（0052）" },
    Entry { method: "gate.revokeWaiver", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "gate_fast_track_waivers status CAS active→revoked（0052）" },
    Entry { method: "gitlabProfile.capabilities", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "gitlabProfile.checkProjectPermissions", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "gitlabProfile.create", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "gitlabProfile.currentUser", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "gitlabProfile.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "gitlabProfile.remove", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "gitlabProfile.test", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "gitlabProfile.update", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "goal.autoReleaseCheck", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "impact.forProposal", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "knowledge.create", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "knowledge.freshnessOverview", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "knowledge.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "knowledge.manifestCreate", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "knowledge.manifestRemove", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "knowledge.manifestUpdate", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "knowledge.projectSettings.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "knowledge.projectSettings.update", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "knowledge.remove", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "knowledge.scan", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "knowledge.search", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "knowledge.searchV2", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "knowledge.settings.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "knowledge.settings.update", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "knowledge.syncFromRepo", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "knowledge.update", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "knowledge.verifySource", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "knowledge_verification_receipts UNIQUE(stable_id,verified_input_digest,outcome,verifier)" },
    Entry { method: "logs.exportDiagnosticBundle", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "logs.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "mcp.importAdd", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "mcp_import_sources UNIQUE(repo_url,pinned_sha,manifest_digest)" },
    Entry { method: "mcp.importDecide", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "import operation 状态机" },
    Entry { method: "mcp.importGet", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "mcp.importList", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "mcp.importResume", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "operation resume 幂等" },
    Entry { method: "mcp.importRevoke", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "revoke 单向" },
    Entry { method: "mcp.serverAdd", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "mcp.serverApprove", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "mcp.serverList", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "mcp.serverRefresh", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "mcp.serverRemove", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "mcp.serverToggle", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "mcp.toolsList", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "memory.archive", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "memory_entries status CAS" },
    Entry { method: "memory.candidateDecide", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "候选决定唯一" },
    Entry { method: "memory.candidateList", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "memory.captureGet", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "memory.captureStart", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "capture_jobs 状态机" },
    Entry { method: "memory.contextPreview", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "memory.create", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "memory_entries 服务端 id" },
    Entry { method: "memory.export", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "memory.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "memory.import", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "import batch id" },
    Entry { method: "memory.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "memory.pin", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "memory_entries pinned 状态" },
    Entry { method: "memory.purge", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "墓碑 + purge token" },
    Entry { method: "memory.purgePreview", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "memory.restore", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "memory_entries status CAS" },
    Entry { method: "memory.search", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "memory.settingsGet", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "memory.settingsUpdate", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "memory_settings revision CAS" },
    Entry { method: "memory.syncFromRepo", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "memory.update", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "memory_revisions CAS" },
    Entry { method: "metrics.overview", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "middlewareProfile.activate", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "middlewareProfile.createVersion", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "middlewareProfile.validate", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "model.usage", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "modelProfile.create", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "modelProfile.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "modelProfile.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "modelProfile.remove", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "modelProfile.syncModels", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "modelProfile.test", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "modelProfile.update", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "modelProvider.presets", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "modelRoute.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "modelRoute.update", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "notification.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "operation.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "passport.issue", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "passport.latest", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "plan.cancel", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "plan 状态机 CAS" },
    Entry { method: "plan.createDraft", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "plan drafts 追加" },
    Entry { method: "plan.decide", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "plan.dispatchReady", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "plan.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "plan.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "plan.replan", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "plan 状态机 CAS" },
    Entry { method: "plan.replanPreview", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "plan.start", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "plan 状态机 CAS" },
    Entry { method: "plan.startRunning", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "plan.submit", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "plan.updateDraft", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "draft revision CAS" },
    Entry { method: "planTask.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "planTask.prepare", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "planTask.reconcile", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "planTask.transition", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "attempt outcome 状态机" },
    Entry { method: "project.archive", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "project.create", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "project.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "project.gitStatus", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "project.inspectRoot", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "project.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "project.summary", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "project.update", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "requirement.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "requirement.importRevision", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "requirement.items", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "requirement.revisions", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "rework.decide", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "rework_operations UNIQUE(approval_id) 状态机" },
    Entry { method: "rework.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "rework.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "rework.preview", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "rework.request", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "rework_operations UNIQUE(action_digest)" },
    Entry { method: "rework.resume", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "rework_operations progress 游标 + 双 digest（0053）" },
    Entry { method: "rollback.decide", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "rollback.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "rollback.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "rollback.preview", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "rollback.request", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "settings.effective", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "settings.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "settings.summary", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "settings.update", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "skill.activateVersion", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "skill.activeList", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "skill.bindVersion", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "skill.create", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "skill.createVersion", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "skill.deprecateVersion", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "skill.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "skill.importFromRegistry", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "skill.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "skill.marketImport", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "skill.marketList", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "skill.marketPluginSkills", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "skill.marketSourceRemove", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "skill.marketSourceSave", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "skill.remove", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "skill.revokeVersion", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "skill.setEnabled", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "skill.update", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "skill.versionList", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "snapshot.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "snapshot.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "sshTarget.acceptHostKey", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "sshTarget.bindProject", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "sshTarget.create", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "sshTarget.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "sshTarget.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "sshTarget.remove", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "sshTarget.test", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "sshTarget.update", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "stage.attempts", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "stage.package", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "stage.startActivity", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "stage_attempts/activities 服务端装配" },
    Entry { method: "taskWorkspace.finalize", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "workspace 终态 CAS" },
    Entry { method: "taskWorkspace.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "taskWorkspace.prepare", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "timeline.snapshot", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "tool.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "tool.test", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "toolPolicy.effective", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "toolPolicy.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "toolPolicy.update", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "trace.coverage", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "trace.gaps", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "trace.graph", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "trace.lineage", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "trace.restoreCheckpoint", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "trace.taskReadModel", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "trace.usage", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "triage.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "update.check", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "update.status", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "workflow.getInstance", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "workflow.migrate", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "迁移 operation" },
    Entry { method: "workflow.migrationPreview", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "workflowTemplate.activate", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "模板 revision CAS" },
    Entry { method: "workflowTemplate.create", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "workflow_template_versions 追加式版本" },
    Entry { method: "workflowTemplate.deprecate", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "模板 revision CAS" },
    Entry { method: "workflowTemplate.get", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "workflowTemplate.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "workflowTemplate.updateDraft", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "draft 版本单权威" },
    Entry { method: "workitem.archive", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "workitem.create", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "workitem.documents", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "workitem.getDocument", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "workitem.importDocument", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "workitem.importIssue", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "workitem.list", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "workitem.progress", kind: Kind::Mutation, receipt: ReceiptMode::None, domain_key: "" },
    Entry { method: "workitem.searchRebuild", kind: Kind::Mutation, receipt: ReceiptMode::Required, domain_key: "（P1-3 升级 operation/receipt + 影子切换）" },
    Entry { method: "workitem.similar", kind: Kind::Read, receipt: ReceiptMode::None, domain_key: "" },
    // END_REGISTRY_ENTRIES
];

/// 查表：未声明方法返回 None（调用方按未知方法拒绝/透传既有逻辑）。
pub fn entry(method: &str) -> Option<&'static Entry> {
    REGISTRY.iter().find(|e| e.method == method)
}

/// receipt 门控 mutation：dispatch 入口据此强制 idempotencyKey。
pub fn requires_receipt(method: &str) -> bool {
    entry(method)
        .map(|e| e.receipt == ReceiptMode::Required)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dispatch_sources() -> String {
        // 静态自证：注册表方法字面量必须在 dispatch 实现面出现（含子分发器）。
        [
            include_str!("dispatch.rs"),
            include_str!("settings_dispatch.rs"),
            include_str!("workflow_dispatch.rs"),
            include_str!("plan_dispatch.rs"),
            include_str!("memory_dispatch.rs"),
            include_str!("trace_dispatch.rs"),
        ]
        .concat()
    }

    #[test]
    fn registry_is_sorted_and_unique() {
        let mut names: Vec<&str> = REGISTRY.iter().map(|e| e.method).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "注册表存在重复方法");
        let mut sorted: Vec<&str> = REGISTRY.iter().map(|e| e.method).collect();
        sorted.sort_unstable();
        assert_eq!(
            names, sorted,
            "注册表必须按方法名排序（新增方法插入正确位置）"
        );
    }

    #[test]
    fn every_registry_method_has_dispatch_literal() {
        let sources = dispatch_sources();
        let missing: Vec<&str> = REGISTRY
            .iter()
            .map(|e| e.method)
            .filter(|m| !sources.contains(&format!("\"{m}\"")))
            .collect();
        assert!(
            missing.is_empty(),
            "注册表方法未在 dispatch 实现面出现：{missing:?}"
        );
    }

    #[test]
    fn receipt_required_methods_document_domain_key() {
        let missing: Vec<&str> = REGISTRY
            .iter()
            .filter(|e| e.receipt == ReceiptMode::Required && e.domain_key.trim().is_empty())
            .map(|e| e.method)
            .collect();
        assert!(
            missing.is_empty(),
            "Required 方法必须声明 domain_key（可为'仅 transport receipt'说明）：{missing:?}"
        );
    }
}
