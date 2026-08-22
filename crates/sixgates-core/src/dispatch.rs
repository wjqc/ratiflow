//! RPC 方法分发：~45 个方法覆盖项目/知识库/附件/工作项/工件/Agent/门禁/审批/证据/部署/时间线。
use serde_json::{json, Value};
use sg_protocol::{ErrorCode, RpcError};
use sg_store::{objects, outbox, Error, Store};

use crate::state::AppState;

type RpcResult = Result<Value, RpcError>;

fn err(kind: ErrorCode, msg: impl Into<String>) -> RpcError {
    RpcError::new(kind, msg)
}

fn store_err(e: Error) -> RpcError {
    let msg = e.to_string();
    for (needle, kind) in [
        ("etag_mismatch", ErrorCode::EtagMismatch),
        ("revision_frozen", ErrorCode::RevisionFrozen),
        (
            "invalid_stage_transition",
            ErrorCode::InvalidStageTransition,
        ),
        ("digest_drift", ErrorCode::Conflict),
        ("deployment", ErrorCode::Conflict),
        ("object_contains_secrets", ErrorCode::ObjectSecrets),
        ("not_found", ErrorCode::NotFound),
        ("path_outside_project", ErrorCode::PathOutsideProject),
        ("budget_exhausted", ErrorCode::BudgetExceeded),
        ("approval_invalid", ErrorCode::ApprovalInvalid),
        ("model_", ErrorCode::ModelUnavailable),
        ("preflight_failed", ErrorCode::Conflict),
        ("verification_failed", ErrorCode::Conflict),
        ("rollback", ErrorCode::Conflict),
        ("ssh_command_rejected", ErrorCode::ManifestRejected),
        ("approval_required", ErrorCode::ApprovalRequired),
        ("action_denied", ErrorCode::ActionDenied),
        ("required", ErrorCode::InvalidParams),
    ] {
        if msg.contains(needle) {
            return err(kind, msg);
        }
    }
    err(ErrorCode::InternalError, msg)
}

fn str_param(params: &Value, key: &str) -> Result<String, RpcError> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| err(ErrorCode::InvalidParams, format!("缺少参数 {key}")))
}

fn opt_str_param(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(|v| v.as_str()).map(String::from)
}

/// 分发一个 RPC 请求。返回 (结果, 待推送事件截止 sequence)。
pub fn dispatch(state: &AppState, method: &str, params: &Value) -> RpcResult {
    if let Some(result) = crate::settings_dispatch::dispatch(state, method, params) {
        return result;
    }
    match method {
        // --- 系统 ---
        "core.version" => Ok(
            json!({"version": state.core_version, "protocolVersion": sg_protocol::PROTOCOL_VERSION,
            "schemaVersion": state.store.schema_version().map_err(store_err)?}),
        ),
        "diagnostics.check" => diagnostics(state),

        // --- 项目 ---
        "project.list" => {
            let include_archived = params
                .get("includeArchived")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let items = sg_project::list(&state.store, include_archived).map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "project.get" => {
            let id = str_param(params, "projectId")?;
            sg_project::get(&state.store, &id)
                .map(|p| serde_json::to_value(p).unwrap_or_default())
                .map_err(store_err)
        }
        "project.create" => {
            let p = sg_project::register(
                &state.store,
                &str_param(params, "gitlabInstance")?,
                &str_param(params, "namespace")?,
                &str_param(params, "project")?,
                &opt_str_param(params, "defaultBranch").unwrap_or_default(),
                &opt_str_param(params, "name").unwrap_or_default(),
                &opt_str_param(params, "localRoot").unwrap_or_default(),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(p).unwrap_or_default())
        }
        "project.update" => {
            let id = str_param(params, "projectId")?;
            let p = sg_project::update(
                &state.store,
                &id,
                opt_str_param(params, "name").as_deref(),
                opt_str_param(params, "localRoot").as_deref(),
                opt_str_param(params, "defaultBranch").as_deref(),
                opt_str_param(params, "status").as_deref(),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(p).unwrap_or_default())
        }
        "project.archive" => {
            let id = str_param(params, "projectId")?;
            let archived = params
                .get("archived")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            sg_project::archive(&state.store, &id, archived).map_err(store_err)?;
            Ok(json!({"status": if archived { "archived" } else { "active" }}))
        }
        "project.summary" => {
            sg_project::summary(&state.store, &str_param(params, "projectId")?).map_err(store_err)
        }

        // --- 知识库 ---
        "knowledge.list" => {
            let items = sg_knowledge::list_sources(&state.store, &str_param(params, "projectId")?)
                .map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "knowledge.create" => {
            let src = sg_knowledge::create_source(
                &state.store,
                &str_param(params, "projectId")?,
                &str_param(params, "kind")?,
                &str_param(params, "name")?,
                &str_param(params, "locator")?,
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(src).unwrap_or_default())
        }
        "knowledge.update" => {
            sg_knowledge::update_source(
                &state.store,
                &str_param(params, "sourceId")?,
                params.get("enabled").and_then(|v| v.as_bool()),
                opt_str_param(params, "name").as_deref(),
            )
            .map_err(store_err)?;
            Ok(json!({"status": "updated"}))
        }
        "knowledge.remove" => {
            sg_knowledge::remove_source(&state.store, &str_param(params, "sourceId")?)
                .map_err(store_err)?;
            Ok(json!({"status": "removed"}))
        }
        "knowledge.scan" => {
            let src = sg_knowledge::scan_source(
                &state.store,
                &str_param(params, "sourceId")?,
                opt_str_param(params, "projectRoot")
                    .map(std::path::PathBuf::from)
                    .as_deref(),
                500,
                2 << 20,
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(src).unwrap_or_default())
        }
        "knowledge.search" => {
            let hits = sg_knowledge::search(
                &state.store,
                &str_param(params, "projectId")?,
                &str_param(params, "query")?,
                params.get("limit").and_then(|v| v.as_i64()).unwrap_or(20),
            )
            .map_err(store_err)?;
            Ok(json!({"items": hits}))
        }
        "context.preview" => sg_knowledge::context_preview(
            &state.store,
            &str_param(params, "projectId")?,
            &str_param(params, "query")?,
            params
                .get("maxBytes")
                .and_then(|v| v.as_i64())
                .unwrap_or(64 << 10),
        )
        .map_err(store_err),
        "context.create" => {
            let selected: Vec<String> = params
                .get("selectedSources")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            sg_knowledge::create_manifest(
                &state.store,
                &str_param(params, "projectId")?,
                &str_param(params, "workItemId")?,
                &str_param(params, "query")?,
                &selected,
            )
            .map_err(store_err)
        }

        // --- 附件 ---
        "attachment.import" => {
            let content_b64 = str_param(params, "contentBase64")?;
            let content =
                base64_decode(&content_b64).map_err(|e| err(ErrorCode::InvalidParams, e))?;
            let att = sg_attachment::import(
                &state.store,
                &str_param(params, "workItemId")?,
                &str_param(params, "filename")?,
                &content,
                objects::PutOptions::default(),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(att).unwrap_or_default())
        }
        "attachment.list" => {
            let items = sg_attachment::list(&state.store, &str_param(params, "workItemId")?)
                .map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "attachment.parse" => {
            sg_attachment::set_parse_result(
                &state.store,
                &str_param(params, "attachmentId")?,
                &str_param(params, "state")?,
                opt_str_param(params, "extractedText").as_deref(),
                &opt_str_param(params, "error").unwrap_or_default(),
            )
            .map_err(store_err)?;
            sg_attachment::get(&state.store, &str_param(params, "attachmentId")?)
                .map(|a| serde_json::to_value(a).unwrap_or_default())
                .map_err(store_err)
        }
        "attachment.remove" => {
            sg_attachment::remove(&state.store, &str_param(params, "attachmentId")?)
                .map_err(store_err)?;
            Ok(json!({"status": "removed"}))
        }

        // --- 工作项 ---
        "workitem.list" => {
            let (items, next) = sg_workitem::list(
                &state.store,
                &str_param(params, "projectId")?,
                &opt_str_param(params, "cursor").unwrap_or_default(),
                params.get("limit").and_then(|v| v.as_i64()).unwrap_or(20),
            )
            .map_err(store_err)?;
            Ok(json!({"items": items, "nextCursor": next}))
        }
        "workitem.get" => {
            let id = str_param(params, "workItemId")?;
            let wi = sg_workitem::get(&state.store, &id).map_err(store_err)?;
            let stages = sg_workitem::stages(&state.store, &id).map_err(store_err)?;
            Ok(json!({"workItem": wi, "stages": stages}))
        }
        "workitem.create" => {
            let labels: Vec<String> = params
                .get("labels")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|l| l.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let wi = sg_workitem::create(
                &state.store,
                &str_param(params, "projectId")?,
                &str_param(params, "title")?,
                &opt_str_param(params, "description").unwrap_or_default(),
                opt_str_param(params, "gitlabIssueIid").as_deref(),
                &labels,
            )
            .map_err(store_err)?;
            // 需求文档落盘（工作目录 data/docs/）。
            let doc = format!("# {}\n\n{}\n", wi.title, wi.description);
            let doc_path = sg_workitem::docs::save(&state.store, &wi.id, "requirement.md", &doc)
                .map_err(store_err)?;
            let mut value = serde_json::to_value(&wi).unwrap_or_default();
            value["requirementDoc"] = json!(doc_path);
            Ok(value)
        }
        "workitem.setStage" => {
            let gate = sg_workitem::Gate::parse(&str_param(params, "gate")?)
                .ok_or_else(|| err(ErrorCode::InvalidParams, "unknown gate"))?;
            let to = sg_workitem::StageState::parse(&str_param(params, "state")?)
                .ok_or_else(|| err(ErrorCode::InvalidParams, "unknown state"))?;
            sg_workitem::set_stage(
                &state.store,
                &str_param(params, "workItemId")?,
                gate,
                to,
                &opt_str_param(params, "inputBaselineSha").unwrap_or_default(),
            )
            .map_err(store_err)?;
            Ok(json!({"status": "updated"}))
        }
        "workitem.progress" => {
            sg_workitem::progress::progress(&state.store, &str_param(params, "workItemId")?)
                .map_err(store_err)
        }
        "workitem.documents" => {
            let names = sg_workitem::docs::list(&state.store, &str_param(params, "workItemId")?)
                .map_err(store_err)?;
            Ok(json!({"items": names}))
        }
        "workitem.getDocument" => {
            let content = sg_workitem::docs::read(
                &state.store,
                &str_param(params, "workItemId")?,
                &str_param(params, "name")?,
            )
            .map_err(store_err)?;
            Ok(json!({"content": content}))
        }
        "workitem.importDocument" => {
            let filename = str_param(params, "filename")?;
            let content = str_param(params, "content")?;
            let title = sg_workitem::docs::title_from_document(&filename, &content);
            let title = if title.is_empty() {
                "未命名需求".to_string()
            } else {
                title
            };
            let wi = sg_workitem::create(
                &state.store,
                &str_param(params, "projectId")?,
                &title,
                "",
                None,
                &[],
            )
            .map_err(store_err)?;
            let doc_path = sg_workitem::docs::save(&state.store, &wi.id, &filename, &content)
                .map_err(store_err)?;
            let mut value = serde_json::to_value(&wi).unwrap_or_default();
            value["requirementDoc"] = json!(doc_path);
            Ok(value)
        }
        "workitem.importIssue" => {
            let issue = state
                .gitlab
                .get_issue(
                    &str_param(params, "gitlabProjectId")?,
                    &str_param(params, "issueIid")?,
                )
                .map_err(|e| {
                    err(
                        if e.contains("forbidden") {
                            ErrorCode::GitlabUnconfigured
                        } else {
                            ErrorCode::GitlabUnreachable
                        },
                        e,
                    )
                })?;
            let wi = sg_workitem::create(
                &state.store,
                &str_param(params, "projectId")?,
                &issue.title,
                &issue.body,
                Some(&issue.iid),
                &issue.labels,
            )
            .map_err(store_err)?;
            let doc = format!("# {}\n\n{}\n", issue.title, issue.body);
            let doc_path = sg_workitem::docs::save(&state.store, &wi.id, "requirement.md", &doc)
                .unwrap_or_default();
            let mut value = serde_json::to_value(&wi).unwrap_or_default();
            value["requirementDoc"] = json!(doc_path);
            Ok(value)
        }

        // --- 工件 ---
        "artifact.list" => {
            let items =
                sg_artifact::list_artifacts(&state.store, &str_param(params, "workItemId")?)
                    .map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "artifact.create" => {
            let art = sg_artifact::create_artifact(
                &state.store,
                &str_param(params, "workItemId")?,
                &str_param(params, "kind")?,
                &str_param(params, "title")?,
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(art).unwrap_or_default())
        }
        "artifact.createDraft" => {
            let rev = sg_artifact::create_draft(
                &state.store,
                &str_param(params, "artifactId")?,
                &str_param(params, "content")?,
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(rev).unwrap_or_default())
        }
        "artifact.updateDraft" => {
            let rev = sg_artifact::update_draft(
                &state.store,
                &str_param(params, "revisionId")?,
                &str_param(params, "etag")?,
                &str_param(params, "content")?,
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(rev).unwrap_or_default())
        }
        "artifact.listRevisions" => {
            let items =
                sg_artifact::list_revisions(&state.store, &str_param(params, "artifactId")?)
                    .map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "artifact.revisionContent" => {
            let content =
                sg_artifact::revision_content(&state.store, &str_param(params, "revisionId")?)
                    .map_err(store_err)?;
            Ok(json!({"content": String::from_utf8_lossy(&content)}))
        }
        "artifact.addReview" => {
            sg_artifact::add_review(
                &state.store,
                &str_param(params, "revisionId")?,
                &str_param(params, "reviewer")?,
                &str_param(params, "verdict")?,
                &opt_str_param(params, "comment").unwrap_or_default(),
                opt_str_param(params, "gitlabMrIid").as_deref(),
            )
            .map_err(store_err)?;
            Ok(json!({"status": "reviewed"}))
        }
        "artifact.freezeBaseline" => {
            let revision_ids: Vec<String> = params
                .get("revisionIds")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .ok_or_else(|| err(ErrorCode::InvalidParams, "revisionIds required"))?;
            let base = sg_artifact::freeze(
                &state.store,
                &str_param(params, "workItemId")?,
                &str_param(params, "gate")?,
                &revision_ids,
                &opt_str_param(params, "gitlabCommitSha").unwrap_or_default(),
            )
            .map_err(store_err)?;
            // 新基线冻结 → 下游 stale 传播（core 编排）。
            if let Some(gate) =
                sg_workitem::Gate::parse(&str_param(params, "gate").unwrap_or_default())
            {
                let inputs = base.inputs_sha256.clone();
                let wi = str_param(params, "workItemId")?;
                let _ = sg_workitem::mark_stale_from(&state.store, &wi, gate, &inputs);
            }
            Ok(serde_json::to_value(base).unwrap_or_default())
        }

        // --- Agent ---
        "agent.run" => {
            let workitem_id = str_param(params, "workItemId")?;
            let goal = str_param(params, "goal")?;
            let manifest_id = str_param(params, "contextManifestId")?;
            // 校验清单属于该工作项。
            let manifest =
                sg_knowledge::get_manifest(&state.store, &manifest_id).map_err(store_err)?;
            if manifest["workitemId"].as_str() != Some(workitem_id.as_str()) {
                return Err(err(ErrorCode::InvalidParams, "上下文清单与工作项不匹配"));
            }
            let allowlist: Vec<String> = params
                .get("toolAllowlist")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_else(|| vec!["read_file".into()]);
            let budget: sg_agent::RunBudget = params
                .get("budget")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            let idem = opt_str_param(params, "idempotencyKey")
                .unwrap_or_else(|| format!("rpc-{}", sg_store::ids::new_id("idem")));
            // executor：映射到受约束执行（提案 → ExecutionManifest）。
            let mode = state.executor_mode;
            let executor = move |p: &sg_agent::Proposal| -> Result<String, String> {
                let manifest = sg_executor::ExecutionManifest {
                    argv: vec![p.tool.clone()],
                    work_dir: String::new(),
                    image: "alpine:3".into(),
                    network_off: true,
                    memory_mb: 256,
                    cpus: 0.5,
                    timeout_sec: 120,
                    writes_files: false,
                };
                match sg_executor::execute(mode, &manifest) {
                    Ok(result) => serde_json::to_string(&result).map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                }
            };
            let task_id = opt_str_param(params, "taskId").unwrap_or_default();
            let out = sg_agent::start(
                &state.store,
                &state.model,
                &state.policy,
                Some(&executor),
                &sg_agent::RunConfig {
                    workitem_id: &workitem_id,
                    task_id: &task_id,
                    goal: &goal,
                    manifest_id: &manifest_id,
                    tool_allowlist: &allowlist,
                    idempotency_key: &idem,
                    budget: &budget,
                    max_iterations: 20,
                },
            )
            .map_err(store_err)?;
            let proposals = sg_agent::proposals(&state.store, &out.run.id).unwrap_or_default();
            Ok(json!({"run": out.run, "output": out.output, "proposals": proposals}))
        }
        "agent.get" => {
            let run =
                sg_agent::get_run(&state.store, &str_param(params, "runId")?).map_err(store_err)?;
            Ok(serde_json::to_value(run).unwrap_or_default())
        }
        "agent.cancel" => {
            sg_agent::cancel(&state.store, &str_param(params, "runId")?).map_err(store_err)?;
            Ok(json!({"status": "cancelled"}))
        }
        "agent.proposals" => {
            let items = sg_agent::proposals(&state.store, &str_param(params, "runId")?)
                .map_err(store_err)?;
            Ok(json!({"items": items}))
        }

        // --- 门禁 ---
        "gate.evaluate" => {
            let workitem_id = str_param(params, "workItemId")?;
            let gate_name = str_param(params, "gate")?;
            sg_workitem::Gate::parse(&gate_name)
                .ok_or_else(|| err(ErrorCode::InvalidParams, "unknown gate"))?;
            let mut inputs = sg_workitem::EvaluateInputs {
                workitem_id: workitem_id.clone(),
                gate: gate_name.clone(),
                required_artifacts_frozen: sg_workitem::InputState::Unknown,
                required_checks_passed: sg_workitem::InputState::Unknown,
                approvals_valid: sg_workitem::InputState::Unknown,
                evidence_complete: sg_workitem::InputState::Unknown,
                no_blocking_risk: sg_workitem::InputState::Pass,
                inputs_current: sg_workitem::InputState::Unknown,
            };
            if let Some(base) =
                sg_artifact::latest_baseline(&state.store, &workitem_id).map_err(store_err)?
            {
                if sg_artifact::is_baseline_current(&state.store, &base.id).map_err(store_err)? {
                    inputs.required_artifacts_frozen = sg_workitem::InputState::Pass;
                    inputs.inputs_current = sg_workitem::InputState::Pass;
                } else {
                    inputs.required_artifacts_frozen = sg_workitem::InputState::Fail;
                    inputs.inputs_current = sg_workitem::InputState::Fail;
                }
            }
            let evidences = sg_evidence::list(&state.store, &workitem_id, Some(gate_name.as_str()))
                .map_err(store_err)?;
            if !evidences.is_empty() {
                inputs.evidence_complete = if evidences.iter().all(|e| e.verified) {
                    sg_workitem::InputState::Pass
                } else {
                    sg_workitem::InputState::Fail
                };
                inputs.required_checks_passed = sg_workitem::InputState::Pass;
            }
            let pending = sg_policy::pending(&state.store, 100).map_err(store_err)?;
            if pending.is_empty() {
                inputs.approvals_valid = sg_workitem::InputState::Pass;
            } else {
                inputs.no_blocking_risk = sg_workitem::InputState::Fail;
            }
            let result =
                sg_workitem::gate::evaluate_and_record(&state.store, &inputs).map_err(store_err)?;
            if result.passed {
                let gate = sg_workitem::Gate::parse(&gate_name).unwrap();
                sg_workitem::pass_gate(&state.store, &workitem_id, gate).map_err(store_err)?;
            }
            Ok(serde_json::to_value(result).unwrap_or_default())
        }

        // --- 审批 ---
        "approval.list" => {
            let items = sg_policy::pending(
                &state.store,
                params.get("limit").and_then(|v| v.as_i64()).unwrap_or(50),
            )
            .map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "approval.decide" => {
            let approval_id = str_param(params, "approvalId")?;
            let decision = str_param(params, "decision")?;
            // 批准前重算 digest 由领域层 validate_for 保证；前端只提交决定。
            let appr = sg_policy::decide(
                &state.store,
                &approval_id,
                &decision,
                &str_param(params, "decidedBy")?,
                &opt_str_param(params, "reason").unwrap_or_default(),
            )
            .map_err(store_err)?;
            sg_store::audit::append(
                &state.store,
                &str_param(params, "decidedBy")?,
                &format!("approval.{decision}"),
                "approval",
                &approval_id,
                json!({"subjectId": appr.subject_id}),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(appr).unwrap_or_default())
        }
        "approval.listByWorkItem" => {
            let workitem_id = str_param(params, "workItemId")?;
            let deployments =
                list_deployments_for(&state.store, &workitem_id).map_err(store_err)?;
            let mut items = Vec::new();
            for dep in &deployments {
                items.extend(
                    sg_policy::list_by_subject(&state.store, "deployment", dep)
                        .map_err(store_err)?,
                );
            }
            Ok(json!({"items": items}))
        }

        // --- 证据 / 文牒 ---
        "evidence.list" => {
            let items = sg_evidence::list(
                &state.store,
                &str_param(params, "workItemId")?,
                opt_str_param(params, "gate").as_deref(),
            )
            .map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "evidence.record" => {
            let title = opt_str_param(params, "title").unwrap_or_default();
            let content = opt_str_param(params, "content");
            let payload = opt_str_param(params, "payload").unwrap_or_else(|| "{}".into());
            let source = opt_str_param(params, "source").unwrap_or_default();
            let input = sg_evidence::RecordInput {
                workitem_id: &str_param(params, "workItemId")?,
                gate: &str_param(params, "gate")?,
                kind: &str_param(params, "kind")?,
                title: &title,
                content: content.as_deref(),
                payload: &payload,
                source: &source,
            };
            let ev = sg_evidence::record(&state.store, &input).map_err(store_err)?;
            Ok(serde_json::to_value(ev).unwrap_or_default())
        }
        "evidence.verify" => {
            sg_evidence::verify(
                &state.store,
                &str_param(params, "evidenceId")?,
                &str_param(params, "verifiedBy")?,
            )
            .map_err(store_err)?;
            Ok(json!({"status": "verified"}))
        }
        "passport.issue" => {
            let workitem_id = str_param(params, "workItemId")?;
            let mut gates = Vec::new();
            let mut ok = true;
            for gate in sg_workitem::Gate::ALL {
                match sg_workitem::gate::latest(&state.store, &workitem_id, gate.as_str())
                    .map_err(store_err)?
                {
                    Some(result) if result.passed => {
                        let evidences =
                            sg_evidence::list(&state.store, &workitem_id, Some(gate.as_str()))
                                .map_err(store_err)?;
                        gates.push(sg_evidence::GateSummary {
                            gate: gate.as_str().into(),
                            passed: true,
                            evidence_ids: evidences.iter().map(|e| e.id.clone()).collect(),
                            failed_inputs: vec![],
                        });
                    }
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                return Err(err(
                    ErrorCode::Conflict,
                    "passport_incomplete_gates: 六关尚未全部通过",
                ));
            }
            let passport = sg_evidence::issue_passport(
                &state.store,
                &workitem_id,
                &gates,
                &opt_str_param(params, "sharedSummary").unwrap_or_default(),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(passport).unwrap_or_default())
        }
        "passport.latest" => {
            match sg_evidence::latest_passport(&state.store, &str_param(params, "workItemId")?)
                .map_err(store_err)?
            {
                Some(p) => Ok(serde_json::to_value(p).unwrap_or_default()),
                None => Err(err(ErrorCode::NotFound, "尚无通关文牒")),
            }
        }

        // --- 部署 ---
        "deployment.create" => {
            let plan_value = params
                .get("plan")
                .cloned()
                .ok_or_else(|| err(ErrorCode::InvalidParams, "plan required"))?;
            let plan: sg_workflow::DeploymentPlan = serde_json::from_value(plan_value)
                .map_err(|e| err(ErrorCode::InvalidParams, format!("plan: {e}")))?;
            let dep =
                sg_workflow::create_plan(&state.store, &str_param(params, "workItemId")?, &plan)
                    .map_err(store_err)?;
            Ok(serde_json::to_value(dep).unwrap_or_default())
        }
        "deployment.get" => {
            let dep = sg_workflow::get(&state.store, &str_param(params, "deploymentId")?)
                .map_err(store_err)?;
            Ok(serde_json::to_value(dep).unwrap_or_default())
        }
        "deployment.submit" => {
            let id = str_param(params, "deploymentId")?;
            sg_workflow::submit_for_approval(&state.store, &id).map_err(store_err)?;
            Ok(json!({"status": "awaiting_approval"}))
        }
        "deployment.deploy" => {
            let dep = sg_workflow::approve_and_deploy(
                &state.store,
                &str_param(params, "deploymentId")?,
                state.ssh.as_ref(),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(dep).unwrap_or_default())
        }
        "deployment.verify" => {
            let dep = sg_workflow::verify(
                &state.store,
                &str_param(params, "deploymentId")?,
                state.ssh.as_ref(),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(dep).unwrap_or_default())
        }
        "deployment.rollback" => {
            let dep = sg_workflow::rollback(
                &state.store,
                &str_param(params, "deploymentId")?,
                state.ssh.as_ref(),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(dep).unwrap_or_default())
        }

        // --- 时间线 / 备份 / 审计 ---
        "timeline.snapshot" => {
            let wi = opt_str_param(params, "workItemId");
            let after = params.get("afterSeq").and_then(|v| v.as_i64()).unwrap_or(0);
            let events = sg_timeline::snapshot(&state.store, wi.as_deref(), after, 500)
                .map_err(store_err)?;
            Ok(
                json!({"events": events, "latest": outbox::latest_sequence(&state.store).map_err(store_err)?}),
            )
        }
        "backup.create" => {
            let snap = sg_store::backup::snapshot(&state.store).map_err(store_err)?;
            Ok(snap.manifest)
        }
        "audit.list" => {
            let items = sg_store::audit::list(
                &state.store,
                params.get("afterSeq").and_then(|v| v.as_i64()).unwrap_or(0),
                params.get("limit").and_then(|v| v.as_i64()).unwrap_or(50),
            )
            .map_err(store_err)?;
            Ok(json!({"items": items}))
        }

        _ => Err(err(ErrorCode::MethodNotFound, format!("未知方法 {method}"))),
    }
}

fn list_deployments_for(store: &Store, workitem_id: &str) -> Result<Vec<String>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT id FROM deployments WHERE workitem_id=?1")?;
        let rows = stmt.query_map([workitem_id], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

fn diagnostics(state: &AppState) -> RpcResult {
    let store_ok = state.store.quick_check().is_ok();
    let schema = state.store.schema_version().unwrap_or(0);
    let gitlab_configured = std::env::var("SIXGATES_GITLAB_URL")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let model_configured = std::env::var("SIXGATES_MODEL_API_KEY")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let ssh_configured = std::env::var("SIXGATES_SSH_HOST")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let integrations = json!([
        {"id": "gitlab", "label": "GitLab", "status": if gitlab_configured { "ready" } else { "needs_configuration" },
         "detail": if gitlab_configured { "已配置" } else { "SIXGATES_GITLAB_URL / SIXGATES_GITLAB_TOKEN 未配置" }, "required": true},
        {"id": "model", "label": "公有模型", "status": if model_configured { "ready" } else { "needs_configuration" },
         "detail": if model_configured { "已配置" } else { "SIXGATES_MODEL_BASE_URL / SIXGATES_MODEL_API_KEY 未配置" }, "required": true},
        {"id": "ssh", "label": "SSH 目标机", "status": if ssh_configured { "ready" } else { "needs_configuration" },
         "detail": if ssh_configured { "已配置" } else { "SIXGATES_SSH_HOST / SIXGATES_SSH_USER 未配置" }, "required": true},
    ]);
    Ok(json!({
        "generatedAt": sg_store::now(),
        "ready": gitlab_configured && model_configured && ssh_configured,
        "integrations": integrations,
        "local": [
            {"id": "core", "label": "Rust Core", "status": if store_ok { "ready" } else { "error" }, "detail": format!("schema v{schema}")},
            {"id": "executor", "label": "执行模式", "status": "ready", "detail": format!("{:?}", state.executor_mode)},
            {"id": "sqlite", "label": "SQLite WAL", "status": if store_ok { "ready" } else { "error" }, "detail": state.store.data_dir.display().to_string()},
        ],
    }))
}

fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    // 轻量 base64（无外部依赖）。
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [255u8; 256];
    for (i, c) in TABLE.iter().enumerate() {
        lookup[*c as usize] = i as u8;
    }
    let filtered: Vec<u8> = input
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();
    let mut out = Vec::with_capacity(filtered.len() * 3 / 4);
    for chunk in filtered.chunks(4) {
        let mut buf = [0u8; 4];
        for (i, c) in chunk.iter().enumerate() {
            buf[i] = lookup[*c as usize];
            if buf[i] == 255 {
                return Err(format!("invalid base64 byte {c}"));
            }
        }
        let combined: u32 = ((buf[0] as u32) << 18)
            | ((buf[1] as u32) << 12)
            | ((buf[2] as u32) << 6)
            | buf[3] as u32;
        out.push((combined >> 16) as u8);
        if chunk.len() > 2 {
            out.push((combined >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(combined as u8);
        }
    }
    Ok(out)
}
