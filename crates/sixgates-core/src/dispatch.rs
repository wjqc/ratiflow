//! RPC 方法分发：~45 个方法覆盖项目/知识库/附件/工作项/工件/Agent/门禁/审批/证据/部署/时间线。
use std::sync::Arc;

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

/// 分发一个 RPC 请求（在 DB actor 线程上执行；store 由 actor 提供）。
pub fn dispatch(state: &AppState, store: &Store, method: &str, params: &Value) -> RpcResult {
    if let Some(result) = crate::settings_dispatch::dispatch(state, store, method, params) {
        return result;
    }
    match method {
        // --- 系统 ---
        "core.version" => Ok(
            json!({"version": state.core_version, "protocolVersion": sg_protocol::PROTOCOL_VERSION,
            "schemaVersion": store.schema_version().map_err(store_err)?}),
        ),
        "diagnostics.check" => diagnostics(state, store),

        // --- 项目 ---
        "project.list" => {
            let include_archived = params
                .get("includeArchived")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let items = sg_project::list(store, include_archived).map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "project.get" => {
            let id = str_param(params, "projectId")?;
            sg_project::get(store, &id)
                .map(|p| serde_json::to_value(p).unwrap_or_default())
                .map_err(store_err)
        }
        "project.create" => {
            let p = sg_project::register(
                store,
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
                store,
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
            sg_project::archive(store, &id, archived).map_err(store_err)?;
            Ok(json!({"status": if archived { "archived" } else { "active" }}))
        }
        "project.summary" => {
            sg_project::summary(store, &str_param(params, "projectId")?).map_err(store_err)
        }

        // --- 知识库 ---
        "knowledge.list" => {
            let items = sg_knowledge::list_sources(store, &str_param(params, "projectId")?)
                .map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "knowledge.create" => {
            let src = sg_knowledge::create_source(
                store,
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
                store,
                &str_param(params, "sourceId")?,
                params.get("enabled").and_then(|v| v.as_bool()),
                opt_str_param(params, "name").as_deref(),
            )
            .map_err(store_err)?;
            Ok(json!({"status": "updated"}))
        }
        "knowledge.remove" => {
            sg_knowledge::remove_source(store, &str_param(params, "sourceId")?)
                .map_err(store_err)?;
            Ok(json!({"status": "removed"}))
        }
        "knowledge.scan" => {
            let src = sg_knowledge::scan_source(
                store,
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
                store,
                &str_param(params, "projectId")?,
                &str_param(params, "query")?,
                params.get("limit").and_then(|v| v.as_i64()).unwrap_or(20),
            )
            .map_err(store_err)?;
            Ok(json!({"items": hits}))
        }
        "context.preview" => sg_knowledge::context_preview(
            store,
            &str_param(params, "projectId")?,
            &str_param(params, "query")?,
            params
                .get("maxBytes")
                .and_then(|v| v.as_i64())
                .unwrap_or(64 << 10),
        )
        .map_err(store_err),
        "context.instructions" => {
            // F07/M2：分层指令文件预览（全局→项目根→docs/，含装配字节占比）。
            let project_id = str_param(params, "projectId")?;
            let local_root: Option<String> = store
                .with_conn(|conn| {
                    Ok(conn
                        .query_row(
                            "SELECT COALESCE(local_root,'') FROM projects WHERE id=?1",
                            [&project_id],
                            |r| r.get::<_, String>(0),
                        )
                        .ok())
                })
                .map_err(store_err)?;
            let root = local_root
                .filter(|p| !p.is_empty())
                .map(std::path::PathBuf::from);
            let knowledge_settings = sg_settings::knowledge_defaults::get(store, Some(&project_id))
                .unwrap_or_else(|_| json!({}));
            let instr = sg_agent::instructions::settings_from_json(&knowledge_settings);
            let (text, layers, warnings) =
                sg_agent::instructions::aggregate(&store.data_dir, root.as_deref(), &instr);
            let knowledge = sg_agent::prompt::knowledge_text(&text, "");
            let env = sg_agent::prompt::PromptEnv {
                mode: Some(state.executor_mode),
                work_dir_label: root.as_ref().map(|p| p.to_string_lossy().to_string()),
            };
            let initial =
                sg_agent::prompt::assemble(&env, &["read_file".to_string()], &knowledge, "");
            Ok(json!({
                "layers": sg_agent::instructions::layers_json(&layers),
                "totalBytes": text.len(),
                "warnings": warnings,
                "promptBytes": sg_agent::prompt::segment_bytes(&initial),
            }))
        }
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
                store,
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
                store,
                &str_param(params, "workItemId")?,
                &str_param(params, "filename")?,
                &content,
                objects::PutOptions::default(),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(att).unwrap_or_default())
        }
        "attachment.list" => {
            let items =
                sg_attachment::list(store, &str_param(params, "workItemId")?).map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "attachment.parse" => {
            sg_attachment::set_parse_result(
                store,
                &str_param(params, "attachmentId")?,
                &str_param(params, "state")?,
                opt_str_param(params, "extractedText").as_deref(),
                &opt_str_param(params, "error").unwrap_or_default(),
            )
            .map_err(store_err)?;
            sg_attachment::get(store, &str_param(params, "attachmentId")?)
                .map(|a| serde_json::to_value(a).unwrap_or_default())
                .map_err(store_err)
        }
        "attachment.remove" => {
            sg_attachment::remove(store, &str_param(params, "attachmentId")?).map_err(store_err)?;
            Ok(json!({"status": "removed"}))
        }

        // --- 工作项 ---
        "workitem.list" => {
            let (items, next) = sg_workitem::list(
                store,
                &str_param(params, "projectId")?,
                &opt_str_param(params, "cursor").unwrap_or_default(),
                params.get("limit").and_then(|v| v.as_i64()).unwrap_or(20),
            )
            .map_err(store_err)?;
            Ok(json!({"items": items, "nextCursor": next}))
        }
        "workitem.get" => {
            let id = str_param(params, "workItemId")?;
            let wi = sg_workitem::get(store, &id).map_err(store_err)?;
            let stages = sg_workitem::stages(store, &id).map_err(store_err)?;
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
                store,
                &str_param(params, "projectId")?,
                &str_param(params, "title")?,
                &opt_str_param(params, "description").unwrap_or_default(),
                opt_str_param(params, "gitlabIssueIid").as_deref(),
                &labels,
            )
            .map_err(store_err)?;
            // 需求文档落盘（工作目录 data/docs/）。
            let doc = format!("# {}\n\n{}\n", wi.title, wi.description);
            let doc_path = sg_workitem::docs::save(store, &wi.id, "requirement.md", &doc)
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
                store,
                &str_param(params, "workItemId")?,
                gate,
                to,
                &opt_str_param(params, "inputBaselineSha").unwrap_or_default(),
            )
            .map_err(store_err)?;
            Ok(json!({"status": "updated"}))
        }
        "workitem.progress" => {
            sg_workitem::progress::progress(store, &str_param(params, "workItemId")?)
                .map_err(store_err)
        }
        "workitem.documents" => {
            let names = sg_workitem::docs::list(store, &str_param(params, "workItemId")?)
                .map_err(store_err)?;
            Ok(json!({"items": names}))
        }
        "workitem.getDocument" => {
            let content = sg_workitem::docs::read(
                store,
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
                store,
                &str_param(params, "projectId")?,
                &title,
                "",
                None,
                &[],
            )
            .map_err(store_err)?;
            let doc_path =
                sg_workitem::docs::save(store, &wi.id, &filename, &content).map_err(store_err)?;
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
                store,
                &str_param(params, "projectId")?,
                &issue.title,
                &issue.body,
                Some(&issue.iid),
                &issue.labels,
            )
            .map_err(store_err)?;
            let doc = format!("# {}\n\n{}\n", issue.title, issue.body);
            let doc_path =
                sg_workitem::docs::save(store, &wi.id, "requirement.md", &doc).unwrap_or_default();
            let mut value = serde_json::to_value(&wi).unwrap_or_default();
            value["requirementDoc"] = json!(doc_path);
            Ok(value)
        }

        // --- 工件 ---
        "artifact.list" => {
            let items = sg_artifact::list_artifacts(store, &str_param(params, "workItemId")?)
                .map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "artifact.create" => {
            let art = sg_artifact::create_artifact(
                store,
                &str_param(params, "workItemId")?,
                &str_param(params, "kind")?,
                &str_param(params, "title")?,
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(art).unwrap_or_default())
        }
        "artifact.createDraft" => {
            let rev = sg_artifact::create_draft(
                store,
                &str_param(params, "artifactId")?,
                &str_param(params, "content")?,
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(rev).unwrap_or_default())
        }
        "artifact.updateDraft" => {
            let rev = sg_artifact::update_draft(
                store,
                &str_param(params, "revisionId")?,
                &str_param(params, "etag")?,
                &str_param(params, "content")?,
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(rev).unwrap_or_default())
        }
        "artifact.listRevisions" => {
            let items = sg_artifact::list_revisions(store, &str_param(params, "artifactId")?)
                .map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "artifact.revisionContent" => {
            let content = sg_artifact::revision_content(store, &str_param(params, "revisionId")?)
                .map_err(store_err)?;
            Ok(json!({"content": String::from_utf8_lossy(&content)}))
        }
        "artifact.addReview" => {
            sg_artifact::add_review(
                store,
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
                store,
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
                let _ = sg_workitem::mark_stale_from(store, &wi, gate, &inputs);
            }
            Ok(serde_json::to_value(base).unwrap_or_default())
        }

        // --- Agent ---
        // 唯一入口（ADR-028）：建行（幂等）→ 立即返回 runId，循环在独立任务/连接上执行。
        "agent.start" => {
            let workitem_id = str_param(params, "workItemId")?;
            let goal = str_param(params, "goal")?;
            let manifest_id = str_param(params, "contextManifestId")?;
            // 校验清单属于该工作项。
            let manifest = sg_knowledge::get_manifest(store, &manifest_id).map_err(store_err)?;
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
            let task_id = opt_str_param(params, "taskId").unwrap_or_default();
            let config = sg_agent::RunConfig {
                workitem_id: &workitem_id,
                task_id: &task_id,
                goal: &goal,
                manifest_id: &manifest_id,
                tool_allowlist: &allowlist,
                idempotency_key: &idem,
                budget: &budget,
                max_iterations: 20,
            };
            let (run, created) = sg_agent::create_run(store, &config).map_err(store_err)?;
            if created {
                let instructions = spawn_run_task(state, store, &run.id)?;
                return Ok(
                    json!({"runId": run.id, "status": run.status, "instructions": instructions}),
                );
            }
            Ok(json!({"runId": run.id, "status": run.status}))
        }
        "agent.get" => {
            let run_id = str_param(params, "runId")?;
            let run = sg_agent::get_run(store, &run_id).map_err(store_err)?;
            let mut v = serde_json::to_value(&run).unwrap_or_default();
            v["modelCalls"] = json!(sg_agent::count_model_calls(store, &run_id).unwrap_or(0));
            // rollout 摘要（F04）：行数/字节/路径；全文查看走 logs.* 既有域。
            let path = sg_agent::rollout::Rollout::path_for(&store.data_dir, &run_id);
            v["rollout"] = match std::fs::read_to_string(&path) {
                Ok(body) => json!({
                    "lines": body.lines().count(),
                    "bytes": body.len(),
                    "path": path.to_string_lossy(),
                }),
                Err(_) => Value::Null,
            };
            Ok(v)
        }
        "agent.cancel" => {
            let run_id = str_param(params, "runId")?;
            let run = sg_agent::get_run(store, &run_id).map_err(store_err)?;
            let terminal = matches!(
                run.status.as_str(),
                "completed_execution" | "failed" | "cancelled"
            );
            if terminal {
                return Ok(json!({"runId": run_id, "status": run.status}));
            }
            if let Some(flag) = state.runs.get(&run_id) {
                // 活跃任务：置位取消旗标；循环在下一检查点收尾并发 run.cancelled。
                // 阻塞中的模型调用不可中断（诚实语义），终态经事件/agent.get 可见。
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(json!({"runId": run_id, "status": "cancelling"}))
            } else {
                // 无活跃任务（遗留 running/paused 行，如进程崩溃后）：直接置库收尾。
                sg_agent::cancel(store, &run_id).map_err(store_err)?;
                Ok(json!({"runId": run_id, "status": "cancelled"}))
            }
        }
        "agent.proposals" => {
            let items =
                sg_agent::proposals(store, &str_param(params, "runId")?).map_err(store_err)?;
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
                sg_artifact::latest_baseline(store, &workitem_id).map_err(store_err)?
            {
                if sg_artifact::is_baseline_current(store, &base.id).map_err(store_err)? {
                    inputs.required_artifacts_frozen = sg_workitem::InputState::Pass;
                    inputs.inputs_current = sg_workitem::InputState::Pass;
                } else {
                    inputs.required_artifacts_frozen = sg_workitem::InputState::Fail;
                    inputs.inputs_current = sg_workitem::InputState::Fail;
                }
            }
            let evidences = sg_evidence::list(store, &workitem_id, Some(gate_name.as_str()))
                .map_err(store_err)?;
            if !evidences.is_empty() {
                inputs.evidence_complete = if evidences.iter().all(|e| e.verified) {
                    sg_workitem::InputState::Pass
                } else {
                    sg_workitem::InputState::Fail
                };
                inputs.required_checks_passed = sg_workitem::InputState::Pass;
            }
            let pending = sg_policy::pending(store, 100).map_err(store_err)?;
            if pending.is_empty() {
                inputs.approvals_valid = sg_workitem::InputState::Pass;
            } else {
                inputs.no_blocking_risk = sg_workitem::InputState::Fail;
            }
            let result =
                sg_workitem::gate::evaluate_and_record(store, &inputs).map_err(store_err)?;
            if result.passed {
                let gate = sg_workitem::Gate::parse(&gate_name).unwrap();
                sg_workitem::pass_gate(store, &workitem_id, gate).map_err(store_err)?;
            }
            Ok(serde_json::to_value(result).unwrap_or_default())
        }

        // --- 审批 ---
        "approval.list" => {
            let items = sg_policy::pending(
                store,
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
                store,
                &approval_id,
                &decision,
                &str_param(params, "decidedBy")?,
                &opt_str_param(params, "reason").unwrap_or_default(),
            )
            .map_err(store_err)?;
            let decided_by = str_param(params, "decidedBy")?;
            let reason = opt_str_param(params, "reason").unwrap_or_default();
            sg_store::audit::append(
                store,
                &decided_by,
                &format!("approval.{decision}"),
                "approval",
                &approval_id,
                json!({"subjectId": appr.subject_id}),
            )
            .map_err(store_err)?;
            // M1/F03 联动：审批主体是工具提案且其 Run 挂起 → 批准拉起恢复任务/拒绝收尾 failed。
            let mut run_link = Value::Null;
            if appr.subject_type == "tool_proposal" {
                let proposal =
                    sg_agent::get_proposal(store, &appr.subject_id).map_err(store_err)?;
                if let Ok(linked) = sg_agent::get_run(store, &proposal.run_id) {
                    if linked.status == "paused" {
                        match decision.as_str() {
                            "approved" => {
                                spawn_run_task(state, store, &linked.id)?;
                                run_link = json!({"runId": linked.id, "status": "resuming"});
                            }
                            _ => {
                                let message = format!(
                                    "审批拒绝：{}（{decided_by}）",
                                    if reason.is_empty() {
                                        "未提供理由"
                                    } else {
                                        reason.as_str()
                                    }
                                );
                                sg_agent::fail_paused_run(store, &linked.id, &message)
                                    .map_err(store_err)?;
                                run_link = json!({"runId": linked.id, "status": "failed"});
                            }
                        }
                    }
                }
            }
            let mut out = serde_json::to_value(appr).unwrap_or_default();
            out["run"] = run_link;
            Ok(out)
        }
        "approval.listByWorkItem" => {
            let workitem_id = str_param(params, "workItemId")?;
            let deployments = list_deployments_for(store, &workitem_id).map_err(store_err)?;
            let mut items = Vec::new();
            for dep in &deployments {
                items.extend(
                    sg_policy::list_by_subject(store, "deployment", dep).map_err(store_err)?,
                );
            }
            Ok(json!({"items": items}))
        }

        // --- 证据 / 文牒 ---
        "evidence.list" => {
            let items = sg_evidence::list(
                store,
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
            let ev = sg_evidence::record(store, &input).map_err(store_err)?;
            Ok(serde_json::to_value(ev).unwrap_or_default())
        }
        "evidence.verify" => {
            sg_evidence::verify(
                store,
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
                match sg_workitem::gate::latest(store, &workitem_id, gate.as_str())
                    .map_err(store_err)?
                {
                    Some(result) if result.passed => {
                        let evidences = sg_evidence::list(store, &workitem_id, Some(gate.as_str()))
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
                store,
                &workitem_id,
                &gates,
                &opt_str_param(params, "sharedSummary").unwrap_or_default(),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(passport).unwrap_or_default())
        }
        "passport.latest" => {
            match sg_evidence::latest_passport(store, &str_param(params, "workItemId")?)
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
            let dep = sg_workflow::create_plan(store, &str_param(params, "workItemId")?, &plan)
                .map_err(store_err)?;
            Ok(serde_json::to_value(dep).unwrap_or_default())
        }
        "deployment.get" => {
            let dep =
                sg_workflow::get(store, &str_param(params, "deploymentId")?).map_err(store_err)?;
            Ok(serde_json::to_value(dep).unwrap_or_default())
        }
        "deployment.submit" => {
            let id = str_param(params, "deploymentId")?;
            sg_workflow::submit_for_approval(store, &id).map_err(store_err)?;
            Ok(json!({"status": "awaiting_approval"}))
        }
        "deployment.deploy" => {
            let dep = sg_workflow::approve_and_deploy(
                store,
                &str_param(params, "deploymentId")?,
                state.ssh.as_ref(),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(dep).unwrap_or_default())
        }
        "deployment.verify" => {
            let dep = sg_workflow::verify(
                store,
                &str_param(params, "deploymentId")?,
                state.ssh.as_ref(),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(dep).unwrap_or_default())
        }
        "deployment.rollback" => {
            let dep = sg_workflow::rollback(
                store,
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
            let events =
                sg_timeline::snapshot(store, wi.as_deref(), after, 500).map_err(store_err)?;
            Ok(
                json!({"events": events, "latest": outbox::latest_sequence(store).map_err(store_err)?}),
            )
        }
        "backup.create" => {
            let snap = sg_store::backup::snapshot(store).map_err(store_err)?;
            Ok(snap.manifest)
        }
        "audit.list" => {
            let items = sg_store::audit::list(
                store,
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

fn diagnostics(state: &AppState, store: &Store) -> RpcResult {
    let checks = diagnostics_checks(state, store);
    let ready = checks
        .iter()
        .all(|c| c["status"] != json!("error") && c["status"] != json!("needs_configuration"));
    Ok(json!({
        "generatedAt": sg_store::now(),
        "ready": ready,
        "checks": checks,
        // 兼容字段（旧 UI 消费）：由 checks 派生。
        "integrations": diagnostics_checks(state, store).iter().filter(|c| c["scope"] == json!("integration")).cloned().collect::<Vec<_>>(),
        "local": diagnostics_checks(state, store).iter().filter(|c| c["scope"] == json!("local")).cloned().collect::<Vec<_>>(),
    }))
}

/// S50：每个检查项含 checkId/scope/severity/status/durationMs/fixTarget（一键跳转配置页）。
fn diagnostics_checks(state: &AppState, store: &Store) -> Vec<Value> {
    let start = std::time::Instant::now();
    let store_ok = store.quick_check().is_ok();
    let schema = store.schema_version().unwrap_or(0);
    let dur = |s: std::time::Instant| s.elapsed().as_millis() as i64;

    let gl_profiles: i64 = store
        .with_conn(|conn| {
            Ok(conn
                .query_row("SELECT COUNT(*) FROM gitlab_profiles", [], |r| r.get(0))
                .unwrap_or(0))
        })
        .unwrap_or(0);
    let gl_env = std::env::var("SIXGATES_GITLAB_URL")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let mp_profiles: i64 = store
        .with_conn(|conn| {
            Ok(conn
                .query_row("SELECT COUNT(*) FROM model_profiles", [], |r| r.get(0))
                .unwrap_or(0))
        })
        .unwrap_or(0);
    let mp_env = std::env::var("SIXGATES_MODEL_API_KEY")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let ssh_targets: i64 = store
        .with_conn(|conn| {
            Ok(conn
                .query_row("SELECT COUNT(*) FROM ssh_targets", [], |r| r.get(0))
                .unwrap_or(0))
        })
        .unwrap_or(0);

    let mk = |check_id: &str,
              label: &str,
              scope: &str,
              status: &str,
              severity: &str,
              detail: String,
              fix: &str| {
        json!({
            "checkId": check_id, "label": label, "scope": scope, "status": status, "severity": severity,
            "detail": detail, "durationMs": dur(start), "fixTarget": fix, "required": true,
        })
    };

    vec![
        mk(
            "gitlab",
            "GitLab",
            "integration",
            if gl_profiles > 0 || gl_env {
                "ready"
            } else {
                "needs_configuration"
            },
            "blocking",
            if gl_profiles > 0 {
                format!("{gl_profiles} 个 Profile")
            } else if gl_env {
                "env 托管".into()
            } else {
                "未配置".into()
            },
            "/settings/gitlab",
        ),
        mk(
            "model",
            "公有模型",
            "integration",
            if mp_profiles > 0 || mp_env {
                "ready"
            } else {
                "needs_configuration"
            },
            "blocking",
            if mp_profiles > 0 {
                format!("{mp_profiles} 个 Profile")
            } else if mp_env {
                "env 托管".into()
            } else {
                "未配置".into()
            },
            "/settings/models",
        ),
        mk(
            "ssh",
            "SSH 目标机",
            "integration",
            if ssh_targets > 0 {
                "ready"
            } else {
                "needs_configuration"
            },
            "degraded",
            if ssh_targets > 0 {
                format!("{ssh_targets} 个目标")
            } else {
                "未配置".into()
            },
            "/settings/ssh",
        ),
        mk(
            "core",
            "Rust Core",
            "local",
            if store_ok { "ready" } else { "error" },
            "blocking",
            format!("schema v{schema}"),
            "",
        ),
        mk(
            "executor",
            "执行模式",
            "local",
            "ready",
            "info",
            format!("{:?}", state.executor_mode),
            "/settings/execution",
        ),
        mk(
            "sqlite",
            "SQLite WAL",
            "local",
            if store_ok { "ready" } else { "error" },
            "blocking",
            store.data_dir.display().to_string(),
            "/settings/backup",
        ),
    ]
}

pub fn diagnostics_run_pub(state: &AppState, store: &Store, check_id: &str) -> RpcResult {
    diagnostics_run(state, store, check_id)
}

/// diagnostics.run(checkId)：单项重查（S50）。
fn diagnostics_run(state: &AppState, store: &Store, check_id: &str) -> RpcResult {
    let checks = diagnostics_checks(state, store);
    let found = checks
        .into_iter()
        .find(|c| c["checkId"] == json!(check_id))
        .ok_or_else(|| err(ErrorCode::InvalidParams, format!("未知检查项 {check_id}")))?;
    Ok(found)
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

/// 派发 Run 任务（agent.start 与审批恢复共用，M1/F03）：
/// 行配置重建 → ToolCtx/executor → rollout → tokio 任务（spawn_blocking 驱动循环，run_store 专属连接）。
fn spawn_run_task(state: &AppState, store: &Store, run_id: &str) -> Result<Value, RpcError> {
    let run = sg_agent::get_run(store, run_id).map_err(store_err)?;
    let (workitem_id, goal, manifest_id, allowlist, budget) =
        sg_agent::row_config(store, run_id).map_err(store_err)?;
    // 工具上下文（F05/M0-③）：项目根来自工作项所属项目的 local_root；
    // 工件草稿区 <dataDir>/artifacts/<runId>/。
    let (project_id, local_root): (String, String) = store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT p.id, COALESCE(p.local_root,'') FROM projects p
                 JOIN workitems w ON w.project_id = p.id WHERE w.id=?1",
                [&workitem_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| sg_store::Error::Message("workitem_not_found".into()))
        })
        .map_err(store_err)?;
    let work_dir = if local_root.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(&local_root))
    };
    let ctx = sg_agent::tools::ToolCtx {
        mode: state.executor_mode,
        work_dir: work_dir.clone(),
        artifacts_dir: store.data_dir.join("artifacts").join(run_id),
    };
    let executor =
        crate::tool_exec::make_executor(ctx, state.run_store.clone(), project_id.clone());

    // F06/F07/F08 上下文装配：分层指令文件（全局→项目根→docs/）+ manifest 内容块 → 知识层。
    let knowledge_settings = sg_settings::knowledge_defaults::get(store, Some(&project_id))
        .unwrap_or_else(|_| json!({}));
    let instr_settings = sg_agent::instructions::settings_from_json(&knowledge_settings);
    let (instr_text, layers, instr_warnings) =
        sg_agent::instructions::aggregate(&store.data_dir, work_dir.as_deref(), &instr_settings);
    let blocks = sg_knowledge::manifest_blocks(store, &manifest_id, 64 << 10)
        .unwrap_or_else(|_| json!({"blocks": [], "totalBytes": 0, "includedCount": 0, "excludedCount": 0, "truncated": false}));
    let blocks_text = blocks["blocks"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|b| {
                    format!(
                        "### {}\n{}",
                        b["name"].as_str().unwrap_or(""),
                        sg_agent::tools::truncate_output(
                            b["text"].as_str().unwrap_or(""),
                            16 << 10
                        )
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default();
    let knowledge = sg_agent::prompt::knowledge_text(&instr_text, &blocks_text);
    let env = sg_agent::prompt::PromptEnv {
        mode: Some(state.executor_mode),
        work_dir_label: work_dir.as_ref().map(|p| p.to_string_lossy().to_string()),
    };
    let initial = sg_agent::prompt::assemble(&env, &allowlist, &knowledge, &goal);
    let summary = json!({
        "instructionLayers": sg_agent::instructions::layers_json(&layers),
        "instructionBytes": instr_text.len(),
        "warnings": instr_warnings,
        "knowledgeItems": blocks["includedCount"].clone(),
        "knowledgeBytes": blocks["totalBytes"].clone(),
        "knowledgeExcluded": blocks["excludedCount"].clone(),
        "promptBytes": sg_agent::prompt::segment_bytes(&initial),
    });
    // rollout（F04）：打开失败不阻断 Run（观测数据可用性优先），记 stderr 继续。
    let mut rollout = sg_agent::rollout::Rollout::open(&store.data_dir, run_id).ok();
    if rollout.is_none() {
        eprintln!("{{\"level\":\"warn\",\"msg\":\"rollout open failed for {run_id}\"}}");
    }
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    state.runs.register(run_id, flag.clone());
    let run_store = state.run_store.clone();
    let audit_store = state.run_store.clone();
    let gateway = state.model.clone();
    let policy = state.policy.clone();
    let registry = state.runs.clone();
    let run_id_owned = run_id.to_string();
    let _ = run;
    let t_workitem = workitem_id;
    let t_goal = goal;
    let t_manifest = manifest_id;
    let t_allow = allowlist;
    let t_budget = budget;
    let initial = std::sync::Arc::new(initial);
    let initial_for_task = initial.clone();
    let summary = std::sync::Arc::new(summary);
    let summary_for_task = summary.clone();
    state.handle.spawn(async move {
        let run_id_inner = run_id_owned.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let mut rollout = rollout.take();
            let initial = initial_for_task;
            let summary = summary_for_task;
            let config = sg_agent::RunConfig {
                workitem_id: &t_workitem,
                task_id: "",
                goal: &t_goal,
                manifest_id: &t_manifest,
                tool_allowlist: &t_allow,
                idempotency_key: "",
                budget: &t_budget,
                max_iterations: 20,
            };
            if let Some(ro) = rollout.as_mut() {
                // F06/F07/F08：装配摘要（层数/知识项/告警）入 rollout。
                let _ = ro.append("instructions_summary", summary.as_ref().clone());
            }
            let out = sg_agent::execute_run(
                &run_store,
                &gateway,
                &policy,
                Some(executor.as_ref()),
                &config,
                &run_id_inner,
                Some(&flag),
                rollout.take(),
                &initial,
            );
            (out, rollout)
        })
        .await;
        let (result, rollout) = joined.unwrap_or_else(|_| {
            (
                Err(sg_store::Error::Message("run task panicked".into())),
                None,
            )
        });
        // rollout 收尾：fsync + sha256 → 审计行（路径/行数/哈希；合规证据链仍是 audit+evidence）。
        if let Some(ro) = rollout.and_then(|r| r.finish().ok()) {
            let (lines, sha256) = ro;
            let path = sg_agent::rollout::Rollout::path_for(&audit_store.data_dir, &run_id_owned);
            let _ = sg_store::audit::append(
                &audit_store,
                "system",
                "agent.rollout",
                "agent_run",
                &run_id_owned,
                json!({"path": path.to_string_lossy(), "lines": lines, "sha256": sha256}),
            );
        }
        // 终态/事件已由 execute_run 写库（失败也含在 result 中）；此处只做注册表清理。
        let _ = result;
        registry.unregister(&run_id_owned);
    });
    Ok(summary.as_ref().clone())
}
