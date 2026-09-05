//! RPC 方法分发：~45 个方法覆盖项目/知识库/附件/工作项/工件/Agent/门禁/审批/证据/部署/时间线。
use std::sync::Arc;

use serde_json::{json, Value};
use sg_protocol::{ErrorCode, RpcError};
use sg_store::{objects, outbox, Error, Store};

use crate::settings_dispatch::serr;
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
        ("trace_incomplete", ErrorCode::TraceIncomplete),
        ("trace_cycle", ErrorCode::Conflict),
        ("output_digest_changed", ErrorCode::Conflict),
        ("snapshot_failed", ErrorCode::SnapshotFailed),
        ("rollback_drift", ErrorCode::RollbackDrift),
        (
            "rollback_manual_action_required",
            ErrorCode::RollbackManualActionRequired,
        ),
        (
            "agent_profile_unavailable",
            ErrorCode::AgentProfileUnavailable,
        ),
        ("capability_mismatch", ErrorCode::AgentCapabilityMismatch),
        ("approval_expired", ErrorCode::ApprovalExpired),
        ("attempt_active_exists", ErrorCode::Conflict),
        ("digest_drift", ErrorCode::Conflict),
        ("deployment", ErrorCode::Conflict),
        ("object_contains_secrets", ErrorCode::ObjectSecrets),
        ("manifest_workitem_mismatch", ErrorCode::InvalidParams),
        ("not_found", ErrorCode::NotFound),
        ("path_outside_project", ErrorCode::PathOutsideProject),
        ("budget_exhausted", ErrorCode::BudgetExceeded),
        ("context_too_large", ErrorCode::ContextTooLarge),
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

/// 放行 digest 的 policy version 分量：当前权限快照的 canonical digest。
fn release_policy_version(store: &Store) -> String {
    let (snapshot, _) = assemble_policy_snapshot(store);
    sg_policy::action_digest(&serde_json::to_value(&snapshot).unwrap_or_default())
}

/// M1 谱系新写开关（可回退点）：SIXGATES_TRACE_WRITES=0 关闭全部谱系写入/回填，保留表结构。
pub(crate) fn trace_writes_enabled() -> bool {
    std::env::var("SIXGATES_TRACE_WRITES")
        .map(|v| v != "0")
        .unwrap_or(true)
}

fn workitem_gate(store: &Store, workitem_id: &str) -> String {
    store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT current_gate FROM workitems WHERE id=?1",
                [workitem_id],
                |r| r.get::<_, String>(0),
            )
            .map_err(sg_store::Error::from)
        })
        .unwrap_or_default()
}

fn str_list_param(params: &Value, key: &str) -> Vec<String> {
    params
        .get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// 产出入口的谱系接线（蓝图 §6.1 最小父边纪律，fail-closed）：
/// 先解析需求 key → item id（缺修订/缺条目即 trace_incomplete），调用方确认无副作用后再建边。
fn resolve_requirement_items(
    store: &Store,
    workitem_id: &str,
    requirement_keys: &[String],
) -> Result<Vec<String>, Error> {
    if requirement_keys.is_empty() {
        return Ok(vec![]);
    }
    let Some(revision_id) = sg_workitem::requirements::latest_revision_id(store, workitem_id)?
    else {
        return Err(Error::Message(format!(
            "trace_incomplete: 工作项 {workitem_id} 尚无需求修订，无法建立谱系边"
        )));
    };
    let mut item_ids = Vec::with_capacity(requirement_keys.len());
    for key in requirement_keys {
        item_ids.push(
            sg_workitem::requirements::item_id_by_key(store, &revision_id, key)?.ok_or_else(
                || {
                    Error::Message(format!(
                        "trace_incomplete: 修订 {revision_id} 中不存在需求项 {key}"
                    ))
                },
            )?,
        );
    }
    Ok(item_ids)
}

fn trace_link_items(
    store: &Store,
    workitem_id: &str,
    from_node_type: &str,
    from_entity_id: &str,
    relation: &str,
    item_ids: &[String],
) -> Result<(), Error> {
    for item_id in item_ids {
        sg_provenance::add_edge(
            store,
            &sg_provenance::EdgeInput {
                workitem_id,
                from_node_type,
                from_entity_id,
                relation,
                to_node_type: sg_provenance::node_type::REQUIREMENT_ITEM,
                to_entity_id: item_id,
                stage_attempt_id: "",
                created_by_run_id: "",
            },
        )?;
    }
    Ok(())
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
        "knowledge.syncFromRepo" => {
            if !sg_knowledge::flags::manifest_enabled(store).map_err(store_err)? {
                return Ok(json!({"status": "feature_disabled"}));
            }
            let project_id = str_param(params, "projectId")?;
            let root =
                sg_knowledge::reconcile::project_root(store, &project_id).map_err(store_err)?;
            let reconcile = sg_knowledge::reconcile::sync_from_repo(store, &project_id, &root)
                .map_err(store_err)?;
            let (generation_id, generation_status) =
                sg_knowledge::reconcile::build_and_activate_generation(store, &project_id, &root)
                    .map_err(store_err)?;
            Ok(json!({
                "reconcile": reconcile,
                "generationId": generation_id,
                "generationStatus": generation_status,
            }))
        }
        "knowledge.manifestCreate" => {
            sg_knowledge::manifest::manifest_create(store, params).map_err(store_err)
        }
        "knowledge.manifestUpdate" => {
            sg_knowledge::manifest::manifest_update(store, params).map_err(store_err)
        }
        "knowledge.manifestRemove" => {
            sg_knowledge::manifest::manifest_remove(store, params).map_err(store_err)
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
                requires_approval_tools: vec!["run_command".into()],
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
            sg_context::build_manifest(
                store,
                &sg_context::BuildInput {
                    project_id: &str_param(params, "projectId")?,
                    workitem_id: &str_param(params, "workItemId")?,
                    goal: &str_param(params, "query")?,
                    selected_sources: &selected,
                },
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
                params
                    .get("includeArchived")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
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
            // 需求文档落盘（工作目录 data/docs/）+ 需求修订/条目/谱系节点（M1）。
            let doc = format!("# {}\n\n{}\n", wi.title, wi.description);
            let doc_path = sg_workitem::docs::save(store, &wi.id, "requirement.md", &doc)
                .map_err(store_err)?;
            if trace_writes_enabled() {
                sg_workitem::requirements::import_revision(
                    store,
                    &wi.id,
                    "requirement.md",
                    &doc,
                    "inline",
                    "local-user",
                    "verified",
                )
                .map_err(store_err)?;
            }
            let mut value = serde_json::to_value(&wi).unwrap_or_default();
            value["requirementDoc"] = json!(doc_path);
            Ok(value)
        }
        "workitem.progress" => {
            sg_workitem::progress::progress(store, &str_param(params, "workItemId")?)
                .map_err(store_err)
        }
        "workitem.archive" => {
            let id = str_param(params, "workItemId")?;
            let archived = params
                .get("archived")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            sg_workitem::archive(store, &id, archived).map_err(store_err)?;
            Ok(json!({"status": if archived { "archived" } else { "active" }}))
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
            if trace_writes_enabled() {
                sg_workitem::requirements::import_revision(
                    store,
                    &wi.id,
                    &filename,
                    &content,
                    "document",
                    "local-user",
                    "verified",
                )
                .map_err(store_err)?;
            }
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
            if trace_writes_enabled() {
                sg_workitem::requirements::import_revision(
                    store,
                    &wi.id,
                    "requirement.md",
                    &doc,
                    "issue",
                    "local-user",
                    "verified",
                )
                .map_err(store_err)?;
            }
            let mut value = serde_json::to_value(&wi).unwrap_or_default();
            value["requirementDoc"] = json!(doc_path);
            Ok(value)
        }

        // --- 需求版本 / 追溯（ADR-030 M1）---
        "requirement.importRevision" => {
            let result = sg_workitem::requirements::import_revision(
                store,
                &str_param(params, "workItemId")?,
                &str_param(params, "filename")?,
                &str_param(params, "content")?,
                &opt_str_param(params, "sourceKind").unwrap_or_else(|| "document".into()),
                &opt_str_param(params, "createdBy").unwrap_or_else(|| "local-user".into()),
                "verified",
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(result).unwrap_or_default())
        }
        "requirement.revisions" => {
            let grouped =
                sg_workitem::requirements::revisions(store, &str_param(params, "workItemId")?)
                    .map_err(store_err)?;
            Ok(json!({"items": grouped.iter().map(|(doc, revs)| json!({
                "document": doc,
                "revisions": revs,
            })).collect::<Vec<_>>()}))
        }
        "requirement.items" => {
            let items = sg_workitem::requirements::items(store, &str_param(params, "revisionId")?)
                .map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "requirement.get" => {
            let revision_id = str_param(params, "revisionId")?;
            let items = sg_workitem::requirements::items(store, &revision_id).map_err(store_err)?;
            // 修订本体在 revisions 聚合中返回过；此处携带条目与谱系覆盖。
            let workitem_id = store
                .with_conn(|conn| {
                    conn.query_row(
                        "SELECT d.workitem_id FROM requirement_revisions r
                         JOIN requirement_documents d ON d.id = r.document_id WHERE r.id=?1",
                        [&revision_id],
                        |r| r.get::<_, String>(0),
                    )
                    .map_err(|_| Error::Message(format!("not_found: 修订 {revision_id}")))
                })
                .map_err(store_err)?;
            let coverage =
                sg_provenance::coverage(store, &workitem_id, &revision_id).map_err(store_err)?;
            Ok(
                json!({"revisionId": revision_id, "workItemId": workitem_id, "items": items, "coverage": coverage}),
            )
        }

        // --- 谱系查询（只读）---
        "trace.lineage" => {
            let direction = opt_str_param(params, "direction").unwrap_or_else(|| "both".into());
            if !matches!(direction.as_str(), "up" | "down" | "both") {
                return Err(err(ErrorCode::InvalidParams, "direction 须为 up/down/both"));
            }
            let depth = params.get("depth").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
            let result =
                sg_provenance::lineage(store, &str_param(params, "nodeId")?, &direction, depth)
                    .map_err(store_err)?;
            Ok(serde_json::to_value(result).unwrap_or_default())
        }
        "trace.coverage" => {
            let workitem_id = str_param(params, "workItemId")?;
            let revision_id = match opt_str_param(params, "revisionId") {
                Some(id) => id,
                None => sg_workitem::requirements::latest_revision_id(store, &workitem_id)
                    .map_err(store_err)?
                    .ok_or_else(|| {
                        err(
                            ErrorCode::NotFound,
                            format!("工作项 {workitem_id} 尚无需求修订"),
                        )
                    })?,
            };
            sg_provenance::coverage(store, &workitem_id, &revision_id).map_err(store_err)
        }
        "trace.gaps" => {
            sg_provenance::gaps(store, &str_param(params, "workItemId")?).map_err(store_err)
        }

        // --- 快照 / 回滚（ADR-030 M3）---
        "snapshot.get" => {
            let snap = sg_workitem::snapshot::get(store, &str_param(params, "snapshotId")?)
                .map_err(store_err)?
                .ok_or_else(|| err(ErrorCode::NotFound, "快照不存在"))?;
            let mut v = serde_json::to_value(&snap).unwrap_or_default();
            v["resources"] = serde_json::to_value(
                sg_workitem::snapshot::resources(store, &snap.id).map_err(store_err)?,
            )
            .unwrap_or_default();
            Ok(v)
        }
        "snapshot.list" => {
            let items = sg_workitem::snapshot::list(store, &str_param(params, "workItemId")?)
                .map_err(store_err)?;
            Ok(json!({ "items": items }))
        }
        "rollback.preview" => {
            let result = sg_workitem::rollback::preview(
                store,
                &str_param(params, "workItemId")?,
                &str_param(params, "targetSnapshotId")?,
                &release_policy_version(store),
            )
            .map_err(store_err)?;
            Ok(result)
        }
        "rollback.request" => {
            // E2E 钩子（仅显式设置生效）：SIXGATES_APPROVAL_TTL_SECS 覆盖回滚审批有效期。
            let ttl = std::env::var("SIXGATES_APPROVAL_TTL_SECS")
                .ok()
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or_else(|| assemble_policy_snapshot(store).0.approval_ttl_secs);
            let result = sg_workitem::rollback::request(
                store,
                &str_param(params, "workItemId")?,
                &str_param(params, "targetSnapshotId")?,
                &opt_str_param(params, "requestedBy").unwrap_or_else(|| "local-user".into()),
                &release_policy_version(store),
                ttl,
            )
            .map_err(store_err)?;
            Ok(result)
        }
        "rollback.decide" => {
            let approval_id = str_param(params, "approvalId")?;
            let decided_by = str_param(params, "decidedBy")?;
            let result = sg_workitem::rollback::decide(
                store,
                &approval_id,
                &str_param(params, "decision")?,
                &decided_by,
                &opt_str_param(params, "reason").unwrap_or_default(),
                &release_policy_version(store),
            )
            .map_err(store_err)?;
            sg_store::audit::append(
                store,
                &decided_by,
                &format!("rollback.{}", str_param(params, "decision")?),
                "approval",
                &approval_id,
                json!({}),
            )
            .map_err(store_err)?;
            Ok(result)
        }
        "rollback.get" => {
            sg_workitem::rollback::get(store, &str_param(params, "operationId")?).map_err(store_err)
        }
        "rollback.list" => {
            let items = sg_workitem::rollback::list(store, &str_param(params, "workItemId")?)
                .map_err(store_err)?;
            Ok(json!({ "items": items }))
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
            let artifact_id = str_param(params, "artifactId")?;
            let requirement_keys = str_list_param(params, "requirementKeys");
            // fail-closed：先解析需求 key 与工件归属，再落修订。
            let workitem_id: String = store
                .with_conn(|conn| {
                    conn.query_row(
                        "SELECT workitem_id FROM artifacts WHERE id=?1",
                        [&artifact_id],
                        |r| r.get::<_, String>(0),
                    )
                    .map_err(|_| Error::Message(format!("not_found: 工件 {artifact_id}")))
                })
                .map_err(store_err)?;
            let resolved_items = resolve_requirement_items(store, &workitem_id, &requirement_keys)
                .map_err(store_err)?;
            let rev =
                sg_artifact::create_draft(store, &artifact_id, &str_param(params, "content")?)
                    .map_err(store_err)?;
            if trace_writes_enabled() {
                sg_provenance::register_node(
                    store,
                    &sg_provenance::NodeInput {
                        project_id: "",
                        workitem_id: &workitem_id,
                        node_type: sg_provenance::node_type::ARTIFACT_REVISION,
                        entity_id: &rev.id,
                        content_digest: &rev.content_sha256,
                        verification_state: "verified",
                    },
                )
                .map_err(store_err)?;
                trace_link_items(
                    store,
                    &workitem_id,
                    sg_provenance::node_type::ARTIFACT_REVISION,
                    &rev.id,
                    sg_provenance::relation::SATISFIES,
                    &resolved_items,
                )
                .map_err(store_err)?;
            }
            // M2：修订即输出变化 → 该关 pending 放行失效（AC-SW-03 前置）。
            sg_workitem::release::invalidate_pending_if_drift(
                store,
                &workitem_id,
                workitem_gate(store, &workitem_id).as_str(),
                &release_policy_version(store),
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
            let workitem_id = str_param(params, "workItemId")?;
            let gate_name = str_param(params, "gate")?;
            let gate = sg_workitem::Gate::parse(&gate_name)
                .ok_or_else(|| err(ErrorCode::InvalidParams, "unknown gate"))?;
            let revision_ids: Vec<String> = params
                .get("revisionIds")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .ok_or_else(|| err(ErrorCode::InvalidParams, "revisionIds required"))?;
            // M2：基线绑定到当前关活跃 attempt（冻结即执行中工作）。
            let mut attempt = sg_workitem::attempt::ensure_active(store, &workitem_id, gate)
                .map_err(store_err)?;
            if attempt.state == "prepared" {
                attempt = sg_workitem::attempt::transition(store, &attempt.id, "running")
                    .map_err(store_err)?;
            }
            let base = sg_artifact::freeze(
                store,
                &workitem_id,
                &gate_name,
                &revision_ids,
                &opt_str_param(params, "gitlabCommitSha").unwrap_or_default(),
                &attempt.id,
            )
            .map_err(store_err)?;
            // 新基线冻结 → 放行请求漂移失效（AC-SW-03）+ 下游 stale 传播（core 编排）。
            sg_workitem::release::invalidate_pending_if_drift(
                store,
                &workitem_id,
                &gate_name,
                &release_policy_version(store),
            )
            .map_err(store_err)?;
            let inputs = base.inputs_sha256.clone();
            let _ = sg_workitem::mark_stale_from(store, &workitem_id, gate, &inputs);
            Ok(serde_json::to_value(base).unwrap_or_default())
        }

        // --- Agent ---
        // 唯一入口（ADR-028）：建行（幂等）→ 立即返回 runId，循环在独立任务/连接上执行。
        "agent.start" => {
            let workitem_id = str_param(params, "workItemId")?;
            let goal = str_param(params, "goal")?;
            // ADR-032 M2：服务端创建/验证 manifest。旧客户端可暂传 contextManifestId，
            // 但必须通过归属验证并原样冻结；下一协议版本移除该参数（实施方案 §13 M2）。
            let manifest_id = match opt_str_param(params, "contextManifestId") {
                Some(id) => {
                    sg_context::manifest::require_manifest_for_workitem(store, &id, &workitem_id)
                        .map_err(store_err)?;
                    id
                }
                None => {
                    let wi_project: String = store
                        .with_conn(|conn| {
                            conn.query_row(
                                "SELECT project_id FROM workitems WHERE id=?1",
                                [&workitem_id],
                                |r| r.get::<_, String>(0),
                            )
                            .map_err(|_| Error::Message("not_found: workitem".into()))
                        })
                        .map_err(store_err)?;
                    let built = sg_context::build_manifest(
                        store,
                        &sg_context::BuildInput {
                            project_id: &wi_project,
                            workitem_id: &workitem_id,
                            goal: &goal,
                            selected_sources: &[],
                        },
                    )
                    .map_err(store_err)?;
                    built["id"].as_str().unwrap_or_default().to_string()
                }
            };
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
                if trace_writes_enabled() {
                    sg_provenance::register_node(
                        store,
                        &sg_provenance::NodeInput {
                            project_id: "",
                            workitem_id: &workitem_id,
                            node_type: sg_provenance::node_type::AGENT_RUN,
                            entity_id: &run.id,
                            content_digest: "",
                            verification_state: "verified",
                        },
                    )
                    .map_err(store_err)?;
                }
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
            // F10：真实权限快照（'default' 为旧占位）。
            let snap_raw: String = store
                .with_conn(|conn| {
                    Ok(conn
                        .query_row(
                            "SELECT policy_snapshot FROM agent_runs WHERE id=?1",
                            [&run_id],
                            |r| r.get::<_, String>(0),
                        )
                        .unwrap_or_default())
                })
                .unwrap_or_default();
            v["policySnapshot"] = json!(snap_raw);
            // M4：关卡绑定与选路记录透出（AC-SW-08 运行详情可见）。
            let bindings: (String, String, String, String) = store
                .with_conn(|conn| {
                    conn.query_row(
                        "SELECT stage_attempt_id, stage_activity_id, agent_selection_id, input_snapshot_id FROM agent_runs WHERE id=?1",
                        [&run_id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .map_err(Error::from)
                })
                .unwrap_or_default();
            v["stageAttemptId"] = json!(bindings.0);
            v["stageActivityId"] = json!(bindings.1);
            v["agentSelectionId"] = json!(bindings.2);
            v["inputSnapshotId"] = json!(bindings.3);
            if !bindings.2.is_empty() {
                if let Ok(sel) = sg_agent::router::get(store, &bindings.2) {
                    if let Ok(ver) =
                        sg_agent::profile::get_version(store, &sel.resolved_profile_version_id)
                    {
                        v["agentSelection"] = json!({
                            "selectionId": sel.id,
                            "resolvedProfileVersionId": sel.resolved_profile_version_id,
                            "profileId": ver.profile_id,
                            "versionNo": ver.version_no,
                            "contentDigest": ver.content_digest,
                            "sourceScope": sel.source_scope,
                            "fallbackUsed": sel.fallback_used,
                            "reasonCode": sel.reason_code,
                            "candidates": sel.candidate_report,
                        });
                    }
                }
            }
            // ADR-032 M2：本次 Run 实际采用的记忆证据（ID/bytes；正文永不出协议）。
            let run_manifest_id: String = store
                .with_conn(|conn| {
                    Ok(conn
                        .query_row(
                            "SELECT context_manifest_id FROM agent_runs WHERE id=?1",
                            [&run_id],
                            |r| r.get::<_, String>(0),
                        )
                        .unwrap_or_default())
                })
                .unwrap_or_default();
            if !run_manifest_id.is_empty() {
                if let Ok(ev) = sg_context::blocks::memory_evidence(store, &run_manifest_id) {
                    v["memory"] = ev;
                }
            }
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
        "agent.list" => {
            let items = sg_agent::list_recent(
                store,
                &str_param(params, "workItemId")?,
                params.get("limit").and_then(|v| v.as_i64()).unwrap_or(8),
            )
            .map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "agent.trace" => {
            let run_id = str_param(params, "runId")?;
            sg_agent::trace(store, &run_id).map_err(store_err)
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
            if let Some(token) = state.runs.get(&run_id) {
                // 活跃任务：置位取消令牌——阻塞中的模型 HTTP/SSE 在 select 点即时
                // 中止（放弃 future 即关闭 socket，目标 <1s），Run 循环以 cancelled
                // 终态收尾并发恰好一条 run.cancelled。
                token.cancel();
                Ok(json!({"runId": run_id, "status": "cancelling"}))
            } else {
                // 无活跃任务（遗留 running/paused 行，如进程崩溃后）：直接置库收尾。
                sg_agent::cancel(store, &run_id).map_err(store_err)?;
                Ok(json!({"runId": run_id, "status": "cancelled"}))
            }
        }
        // --- M6：受控 MCP ToolProvider（ADR-035）---
        "mcp.serverAdd" => {
            let args_list = str_list_param(params, "args");
            sg_settings::mcp_ext::server_add(
                store,
                &str_param(params, "name")?,
                &str_param(params, "command")?,
                &args_list,
            )
            .map_err(serr)
        }
        "mcp.serverApprove" => sg_settings::mcp_ext::server_approve(
            store,
            &str_param(params, "serverId")?,
            &str_param(params, "decidedBy")?,
        )
        .map_err(serr),
        "mcp.serverList" => sg_settings::mcp_ext::server_list(store).map_err(serr),
        "mcp.serverRemove" => sg_settings::mcp_ext::server_revoke(
            store,
            &str_param(params, "serverId")?,
            &str_param(params, "decidedBy")?,
            &opt_str_param(params, "reason").unwrap_or_default(),
        )
        .map_err(serr),
        "mcp.serverRefresh" => {
            sg_settings::mcp_ext::server_refresh(store, &str_param(params, "serverId")?)
                .map_err(serr)
        }
        "mcp.toolsList" => mcp_tools_list(store, opt_str_param(params, "serverId").as_deref()),

        // M4：模型缓存与压缩观测（不含任何 reasoning 正文）。
        "model.usage" => {
            let run_id = opt_str_param(params, "runId");
            model_usage(state, store, run_id.as_deref())
        }
        "agent.proposals" => {
            let items =
                sg_agent::proposals(store, &str_param(params, "runId")?).map_err(store_err)?;
            Ok(json!({"items": items}))
        }

        // --- 门禁（M2/ADR-030：evaluate 只计算，绝不推进；放行走 gate.decideRelease）---
        "gate.evaluate" => {
            let workitem_id = str_param(params, "workItemId")?;
            let gate_name = str_param(params, "gate")?;
            let gate = sg_workitem::Gate::parse(&gate_name)
                .ok_or_else(|| err(ErrorCode::InvalidParams, "unknown gate"))?;
            // 评估输入单一事实源（与放行的新鲜度重查共用 build_inputs，P0-2）。
            let inputs = sg_workitem::gate::build_inputs(store, &workitem_id, &gate_name)
                .map_err(store_err)?;
            let result =
                sg_workitem::gate::evaluate_and_record(store, &inputs).map_err(store_err)?;
            if result.passed {
                // attempt 投影推进（不碰 current_gate）。
                sg_workitem::attempt::advance_to_review_ready(store, &workitem_id, gate)
                    .map_err(store_err)?;
            }
            Ok(serde_json::to_value(result).unwrap_or_default())
        }
        "gate.requestRelease" => {
            // 关卡放行审批不限时（人工评审无期限）；E2E 需要限时行为时显式设置
            // SIXGATES_APPROVAL_TTL_SECS（秒）即可恢复过期语义。
            let ttl = std::env::var("SIXGATES_APPROVAL_TTL_SECS")
                .ok()
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0);
            let result = sg_workitem::release::request_release(
                store,
                &str_param(params, "workItemId")?,
                &str_param(params, "gate")?,
                &release_policy_version(store),
                ttl,
            )
            .map_err(store_err)?;
            Ok(result)
        }
        "gate.decideRelease" => {
            let mut result = sg_workitem::release::decide_release(
                store,
                &str_param(params, "approvalId")?,
                &str_param(params, "decision")?,
                &str_param(params, "decidedBy")?,
                &opt_str_param(params, "reason").unwrap_or_default(),
                &release_policy_version(store),
            )
            .map_err(store_err)?;
            sg_store::audit::append(
                store,
                &str_param(params, "decidedBy")?,
                &format!("gate.release.{}", str_param(params, "decision")?),
                "approval",
                &str_param(params, "approvalId")?,
                json!({}),
            )
            .map_err(store_err)?;
            // ADR-031 C2：放行成功 → 镜像 ReleaseDecided 事件（事件权威，DB 为投影）。
            // 事件写入失败不回滚已成的放行，但如实记审计并在响应标注（事件先行的
            // 严格顺序属投影化改造里程碑，见 ADR-031 实施状态）。
            match sg_workitem::release_events::mirror_release_decided(
                store,
                result["attempt"]["workitem_id"]
                    .as_str()
                    .unwrap_or_default(),
                result["attempt"]["gate"].as_str().unwrap_or_default(),
                result["attempt"]["id"].as_str().unwrap_or_default(),
                result["release_digest"].as_str().unwrap_or_default(),
                &str_param(params, "decision")?,
                &str_param(params, "decidedBy")?,
            ) {
                Ok(Some(event_id)) => {
                    result["releaseEventId"] = json!(event_id);
                }
                Ok(None) => {
                    result["releaseEventId"] = json!(null);
                }
                Err(e) => {
                    let _ = sg_store::audit::append(
                        store,
                        "system",
                        "gate.release.event_mirror_failed",
                        "approval",
                        &str_param(params, "approvalId")?,
                        json!({ "error": e.to_string() }),
                    );
                    result["releaseEventMirrorError"] = json!(e.to_string());
                }
            }
            Ok(result)
        }
        "gate.getRelease" => {
            sg_workitem::release::get_release(store, &str_param(params, "releaseId")?)
                .map_err(store_err)
        }
        "stage.attempts" => {
            let workitem_id = str_param(params, "workItemId")?;
            let attempts = sg_workitem::attempt::list(store, &workitem_id).map_err(store_err)?;
            let items: Vec<Value> = attempts
                .iter()
                .map(|a| {
                    let mut v = serde_json::to_value(a).unwrap_or_default();
                    v["activities"] = serde_json::to_value(
                        sg_workitem::attempt::activities(store, &a.id).unwrap_or_default(),
                    )
                    .unwrap_or_default();
                    v
                })
                .collect();
            Ok(json!({"items": items}))
        }
        "stage.startActivity" => {
            // M4 唯一关卡执行入口（SG-AGT-007）：attempt/快照/活动/选路/清单全部服务端装配。
            let workitem_id = str_param(params, "workItemId")?;
            let gate_name = str_param(params, "gate")?;
            let gate = sg_workitem::Gate::parse(&gate_name)
                .ok_or_else(|| err(ErrorCode::InvalidParams, "unknown gate"))?;
            let goal = str_param(params, "goal")?;
            let required_caps = str_list_param(params, "requiredCapabilities");
            let task_override = opt_str_param(params, "profileVersionId");
            let idem = opt_str_param(params, "idempotencyKey")
                .unwrap_or_else(|| format!("sa-{}", sg_store::ids::new_id("idem")));
            // 只能在当前关启动活动（其他关的活跃 attempt 会被单活跃约束拒绝）。
            let wi_now = sg_workitem::get(store, &workitem_id).map_err(store_err)?;
            if wi_now.current_gate != gate_name {
                return Err(err(
                    ErrorCode::Conflict,
                    format!(
                        "attempt_active_exists: 工作项当前关为 {}，不能在 {gate_name} 启动活动",
                        wi_now.current_gate
                    ),
                ));
            }
            let attempt = sg_workitem::attempt::ensure_active(store, &workitem_id, gate)
                .map_err(store_err)?;
            let activities =
                sg_workitem::attempt::activities(store, &attempt.id).map_err(store_err)?;
            let activity_key = match opt_str_param(params, "activityKey") {
                Some(key) => activities
                    .iter()
                    .find(|a| a.activity_key == key)
                    .ok_or_else(|| err(ErrorCode::InvalidParams, format!("未知活动 {key}")))?
                    .activity_key
                    .clone(),
                None => activities
                    .iter()
                    .find(|a| a.state == "pending")
                    .or_else(|| activities.first())
                    .ok_or_else(|| err(ErrorCode::Conflict, "attempt 无可用活动"))?
                    .activity_key
                    .clone(),
            };
            let activity_id = activities
                .iter()
                .find(|a| a.activity_key == activity_key)
                .map(|a| a.id.clone())
                .unwrap_or_default();
            let project_id: String = store
                .with_conn(|conn| {
                    conn.query_row(
                        "SELECT project_id FROM workitems WHERE id=?1",
                        [&workitem_id],
                        |r| r.get::<_, String>(0),
                    )
                    .map_err(|_| Error::Message("not_found: workitem".into()))
                })
                .map_err(store_err)?;
            // 选路（四级优先 + 回退语义），先于运行创建（Run 必须绑定 selection）。
            let selection = sg_agent::router::resolve(
                store,
                &sg_agent::router::ResolveContext {
                    project_id: &project_id,
                    gate: gate.as_str(),
                    activity_key: &activity_key,
                    stage_activity_id: &activity_id,
                    task_override_version_id: task_override.as_deref(),
                    required_capabilities: &required_caps,
                    persist: true,
                },
            )
            .map_err(store_err)?;
            sg_workitem::attempt::set_activity_state(store, &attempt.id, &activity_key, "running")
                .map_err(store_err)?;
            // 服务端装配上下文清单（客户端不再自报关键绑定）。
            let manifest = sg_context::build_manifest(
                store,
                &sg_context::BuildInput {
                    project_id: &project_id,
                    workitem_id: &workitem_id,
                    goal: &goal,
                    selected_sources: &[],
                },
            )
            .map_err(store_err)?;
            let manifest_id = manifest["id"].as_str().unwrap_or_default().to_string();
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
            let config = sg_agent::RunConfig {
                workitem_id: &workitem_id,
                task_id: &activity_id,
                goal: &goal,
                manifest_id: &manifest_id,
                tool_allowlist: &allowlist,
                idempotency_key: &idem,
                budget: &budget,
                max_iterations: 20,
            };
            let (run, created) = sg_agent::create_run(store, &config).map_err(store_err)?;
            if created {
                store
                    .with_conn(|conn| {
                        conn.execute(
                            "UPDATE agent_runs SET stage_attempt_id=?1, stage_activity_id=?2, agent_selection_id=?3, input_snapshot_id=?4 WHERE id=?5",
                            rusqlite::params![attempt.id, activity_id, selection.id, attempt.entry_snapshot_id, run.id],
                        )?;
                        Ok(())
                    })
                    .map_err(store_err)?;
                if trace_writes_enabled() {
                    sg_provenance::register_node(
                        store,
                        &sg_provenance::NodeInput {
                            project_id: "",
                            workitem_id: &workitem_id,
                            node_type: sg_provenance::node_type::AGENT_RUN,
                            entity_id: &run.id,
                            content_digest: "",
                            verification_state: "verified",
                        },
                    )
                    .map_err(store_err)?;
                }
                let instructions = spawn_run_task(state, store, &run.id)?;
                return Ok(json!({
                    "runId": run.id, "status": run.status, "instructions": instructions,
                    "attemptId": attempt.id, "activityKey": activity_key,
                    "selection": selection,
                }));
            }
            Ok(json!({
                "runId": run.id, "status": run.status,
                "attemptId": attempt.id, "activityKey": activity_key,
                "selection": serde_json::to_value(&selection).unwrap_or_default(),
            }))
        }
        "agentProfile.list" => {
            let project = opt_str_param(params, "projectId");
            let profiles =
                sg_agent::profile::list_profiles(store, project.as_deref()).map_err(store_err)?;
            let items: Vec<Value> = profiles
                .iter()
                .map(|p| {
                    let mut v = serde_json::to_value(p).unwrap_or_default();
                    v["versions"] = serde_json::to_value(
                        sg_agent::profile::versions(store, &p.id).unwrap_or_default(),
                    )
                    .unwrap_or_default();
                    v
                })
                .collect();
            Ok(json!({ "items": items }))
        }
        "agentProfile.create" => {
            let profile = sg_agent::profile::create_profile(
                store,
                opt_str_param(params, "projectId").as_deref(),
                &str_param(params, "name")?,
                &str_param(params, "adapterKind")?,
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(profile).unwrap_or_default())
        }
        "agentProfile.createVersion" => {
            let capabilities = str_list_param(params, "capabilities");
            let version = sg_agent::profile::create_version(
                store,
                &str_param(params, "profileId")?,
                &opt_str_param(params, "persona").unwrap_or_default(),
                &opt_str_param(params, "sop").unwrap_or_default(),
                &capabilities,
                &opt_str_param(params, "outputSchema").unwrap_or_default(),
                &opt_str_param(params, "modelRoute").unwrap_or_else(|| "{}".into()),
                &opt_str_param(params, "budget").unwrap_or_else(|| "{}".into()),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(version).unwrap_or_default())
        }
        "agentProfile.setEnabled" => {
            sg_agent::profile::set_profile_enabled(
                store,
                &str_param(params, "profileId")?,
                params
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
            )
            .map_err(store_err)?;
            Ok(json!({ "status": "updated" }))
        }
        "agentBinding.list" => {
            let bindings = sg_agent::profile::list_bindings(
                store,
                opt_str_param(params, "projectId").as_deref(),
            )
            .map_err(store_err)?;
            Ok(json!({ "items": bindings }))
        }
        "agentBinding.set" => {
            sg_agent::profile::set_binding(
                store,
                opt_str_param(params, "projectId").as_deref(),
                &str_param(params, "gate")?,
                &str_param(params, "activityKey")?,
                &str_param(params, "profileVersionId")?,
                &opt_str_param(params, "fallbackMode").unwrap_or_else(|| "generic".into()),
                params.get("priority").and_then(|v| v.as_i64()).unwrap_or(0),
            )
            .map_err(store_err)?;
            Ok(json!({ "status": "bound" }))
        }
        "agentBinding.remove" => {
            sg_agent::profile::remove_binding(store, &str_param(params, "bindingId")?)
                .map_err(store_err)?;
            Ok(json!({ "status": "removed" }))
        }
        "agentBinding.resolvePreview" => {
            let preview = sg_agent::router::resolve_preview(
                store,
                &str_param(params, "projectId")?,
                &str_param(params, "gate")?,
                &str_param(params, "activityKey")?,
                &str_list_param(params, "requiredCapabilities"),
            )
            .map_err(store_err)?;
            Ok(preview)
        }
        "stage.package" => {
            let workitem_id = str_param(params, "workItemId")?;
            let gate_name = str_param(params, "gate")?;
            let gate = sg_workitem::Gate::parse(&gate_name)
                .ok_or_else(|| err(ErrorCode::InvalidParams, "unknown gate"))?;
            let latest = sg_workitem::attempt::latest_for_gate(store, &workitem_id, gate)
                .map_err(store_err)?;
            Ok(json!({
                "workItemId": workitem_id,
                "gate": gate_name,
                "latestAttempt": latest,
                "activeAttempt": sg_workitem::attempt::active_for_gate(store, &workitem_id, gate)
                    .map_err(store_err)?,
                "releaseRequests": sg_workitem::release::list_for_gate(store, &workitem_id, gate.as_str())
                    .map_err(store_err)?,
            }))
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
            let workitem_id = str_param(params, "workItemId")?;
            let requirement_keys = str_list_param(params, "requirementKeys");
            // fail-closed：需求 key 先解析（不命中即 trace_incomplete，不落任何事实）。
            let resolved_items = resolve_requirement_items(store, &workitem_id, &requirement_keys)
                .map_err(store_err)?;
            let gate_param = str_param(params, "gate")?;
            let input = sg_evidence::RecordInput {
                workitem_id: &workitem_id,
                gate: &gate_param,
                kind: &str_param(params, "kind")?,
                title: &title,
                content: content.as_deref(),
                payload: &payload,
                source: &source,
            };
            let ev = sg_evidence::record(store, &input).map_err(store_err)?;
            if trace_writes_enabled() {
                sg_provenance::register_node(
                    store,
                    &sg_provenance::NodeInput {
                        project_id: "",
                        workitem_id: &workitem_id,
                        node_type: sg_provenance::node_type::EVIDENCE,
                        entity_id: &ev.id,
                        content_digest: &ev.object_sha256,
                        verification_state: "unverified",
                    },
                )
                .map_err(store_err)?;
                trace_link_items(
                    store,
                    &workitem_id,
                    sg_provenance::node_type::EVIDENCE,
                    &ev.id,
                    sg_provenance::relation::VERIFIES,
                    &resolved_items,
                )
                .map_err(store_err)?;
            }
            // M2：证据/核验态是放行包的一部分 → 漂移失效（AC-SW-03 前置）。
            sg_workitem::release::invalidate_pending_if_drift(
                store,
                &workitem_id,
                gate_param.as_str(),
                &release_policy_version(store),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(ev).unwrap_or_default())
        }
        "evidence.verify" => {
            let evidence_id = str_param(params, "evidenceId")?;
            sg_evidence::verify(store, &evidence_id, &str_param(params, "verifiedBy")?)
                .map_err(store_err)?;
            if trace_writes_enabled() {
                sg_provenance::set_verification(
                    store,
                    sg_provenance::node_type::EVIDENCE,
                    &evidence_id,
                    "verified",
                )
                .map_err(store_err)?;
            }
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

        // --- 项目记忆（ADR-032 / 实施方案 v1.0）：dispatch 仅做分流，
        // 全部逻辑在 sg-memory crate + memory_dispatch 适配层（§5.1 依赖方向）。 ---
        "memory.settingsGet"
        | "memory.settingsUpdate"
        | "memory.list"
        | "memory.get"
        | "memory.create"
        | "memory.update"
        | "memory.pin"
        | "memory.archive"
        | "memory.restore"
        | "memory.purgePreview"
        | "memory.purge"
        | "memory.search"
        | "memory.contextPreview"
        | "memory.import"
        | "memory.export"
        | "memory.captureStart"
        | "memory.captureGet"
        | "memory.candidateList"
        | "memory.candidateDecide" => {
            crate::memory_dispatch::dispatch(state, store, method, params)
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

/// F10/M3：装配运行时权限快照——注册表为工具权威（risk/data_level/max_result_bytes/timeout），
/// toolPolicy 全局行覆盖 enabled/requiresApproval/risk；enabled=false 即从规则集中移除（策略拒绝）。
fn assemble_policy_snapshot(store: &Store) -> (sg_policy::Snapshot, Value) {
    let overrides = sg_settings::policy_ext::tool_list(store).unwrap_or_default();
    let mut rules = Vec::new();
    let mut sources = Vec::new();
    for def in sg_agent::tools::registry() {
        let o = overrides
            .iter()
            .find(|o| o.tool_id == def.name && o.project_id.is_empty());
        if let Some(o) = o {
            if !o.enabled {
                sources.push(json!({"tool": def.name, "source": "settings", "enabled": false}));
                continue;
            }
        }
        let risk = match o.map(|o| o.risk.as_str()) {
            Some("low") => sg_policy::Risk::Low,
            Some("medium") => sg_policy::Risk::Medium,
            Some("high") => sg_policy::Risk::High,
            _ => def.risk,
        };
        let requires_approval = o
            .map(|o| o.requires_approval)
            .unwrap_or(def.risk == sg_policy::Risk::High);
        rules.push(sg_policy::ToolRule {
            tool: def.name.to_string(),
            risk,
            requires_approval,
            data_level: def.data_level.into(),
            max_result_bytes: def.max_result_bytes as i64,
            timeout_sec: def.timeout_sec,
        });
        sources.push(json!({
            "tool": def.name,
            "source": if o.is_some() { "settings" } else { "registry" },
            "requiresApproval": requires_approval,
        }));
    }
    // M6（ADR-035）：活跃 MCP 工具纳入策略快照——
    // read_only → Low/免审批；写 → High/必须审批；data_level=external；
    // 沙箱标注：本地 stdio 进程直启（沙箱包裹为后续硬化项），远端不受本机沙箱保护。
    for t in sg_settings::mcp_ext::active_tools(store) {
        let model_name = format!("mcp__{}__{}", t.server_name, t.tool_name);
        rules.push(sg_policy::ToolRule {
            tool: model_name.clone(),
            risk: if t.read_only {
                sg_policy::Risk::Low
            } else {
                sg_policy::Risk::High
            },
            requires_approval: !t.read_only,
            data_level: "external".into(),
            max_result_bytes: 64 * 1024,
            timeout_sec: 60,
        });
        sources.push(json!({
            "tool": model_name,
            "source": "mcp",
            "server": t.server_name,
            "schemaDigest": t.schema_digest,
            "readOnly": t.read_only,
            "sandboxed": false,
        }));
    }
    (
        sg_policy::Snapshot {
            tool_rules: rules,
            approval_ttl_secs: 3600,
        },
        json!({"sources": sources}),
    )
}

/// F10/M3：执行模式生效来源——env 覆盖（CI 优先）> 设置域 executionProfile（unsafe 需双确认）> 探测。
pub(crate) fn mode_str(m: sg_executor::Mode) -> &'static str {
    match m {
        sg_executor::Mode::Docker => "docker",
        sg_executor::Mode::KernelRestricted => "kernel_restricted",
        sg_executor::Mode::SafeRestricted => "safe_restricted",
        sg_executor::Mode::UnsafeExplicit => "unsafe_explicit",
        sg_executor::Mode::Disabled => "disabled",
    }
}

pub(crate) fn effective_executor_mode(
    state: &AppState,
    store: &Store,
) -> (sg_executor::Mode, &'static str) {
    if let Some(m) = std::env::var("SIXGATES_EXEC_MODE")
        .ok()
        .and_then(|m| match m.as_str() {
            "docker" => Some(sg_executor::Mode::Docker),
            "kernel_restricted" => Some(sg_executor::Mode::KernelRestricted),
            "safe_restricted" => Some(sg_executor::Mode::SafeRestricted),
            "unsafe_explicit" => Some(sg_executor::Mode::UnsafeExplicit),
            "disabled" => Some(sg_executor::Mode::Disabled),
            _ => None,
        })
    {
        return (m, "env");
    }
    let prof = sg_settings::executor_ext::get(store).unwrap_or_else(|_| json!({}));
    let confirmed = prof["unsafeConfirmed"].as_bool() == Some(true);
    match prof["mode"].as_str() {
        Some("docker") if sg_executor::docker_available() => {
            (sg_executor::Mode::Docker, "settings")
        }
        // kernel_restricted 要求内核沙箱实际可用（不可用 → 探测回落，fail-closed）。
        Some("kernel_restricted") if sg_executor::sandbox::probe().backend != "unavailable" => {
            (sg_executor::Mode::KernelRestricted, "settings")
        }
        Some("safe_restricted") => (sg_executor::Mode::SafeRestricted, "settings"),
        Some("unsafe_explicit") if confirmed => (sg_executor::Mode::UnsafeExplicit, "settings"),
        Some("disabled") => (sg_executor::Mode::Disabled, "settings"),
        _ => (state.executor_mode, "detected"),
    }
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
    // M3+：Agent 工具执行在 SixGates 隔离 worktree（SG-RBK-005）；不可用时诚实回退 local_root。
    let worktree_info = sg_workitem::worktree::ensure(store, &workitem_id).ok();
    let work_dir = match &worktree_info {
        Some(info) => Some(std::path::PathBuf::from(&info.path)),
        None if !local_root.is_empty() => Some(std::path::PathBuf::from(&local_root)),
        _ => None,
    };
    // F10：权限快照与生效执行模式——已存快照（暂停恢复）原样复用（运行中不受设置变更影响），
    // 否则装配（注册表+toolPolicy 覆盖）并落库 policy_snapshot 列。
    let parse_mode = |v: &str| match v {
        "docker" => Some(sg_executor::Mode::Docker),
        "kernel_restricted" => Some(sg_executor::Mode::KernelRestricted),
        // 旧值兼容读取（ADR-034）：safe_restricted 仅 argv 只读白名单，无内核强制。
        "safe_restricted" => Some(sg_executor::Mode::SafeRestricted),
        "unsafe_explicit" => Some(sg_executor::Mode::UnsafeExplicit),
        "disabled" => Some(sg_executor::Mode::Disabled),
        _ => None,
    };
    let existing_snap: String = store
        .with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT policy_snapshot FROM agent_runs WHERE id=?1",
                    [run_id],
                    |r| r.get::<_, String>(0),
                )
                .unwrap_or_default())
        })
        .unwrap_or_default();
    let (policy_snapshot, snapshot_envelope, mode, mode_source) = if existing_snap.starts_with('{')
    {
        match serde_json::from_str::<Value>(&existing_snap)
            .ok()
            .and_then(|v| {
                Some((
                    serde_json::from_value::<sg_policy::Snapshot>(v["snapshot"].clone()).ok()?,
                    v,
                ))
            }) {
            Some((snap, v)) => {
                let m = v["mode"]
                    .as_str()
                    .and_then(parse_mode)
                    .unwrap_or(state.executor_mode);
                (snap, v, m, "stored")
            }
            None => {
                let (snap, _) = assemble_policy_snapshot(store);
                let (m, src) = effective_executor_mode(state, store);
                (snap, json!(null), m, src)
            }
        }
    } else {
        let (snap, sources) = assemble_policy_snapshot(store);
        let (m, src) = effective_executor_mode(state, store);
        // ADR-034 M3：内核沙箱能力与本次 Run 的策略 digest 固化进执行快照
        // （backend/version/digest；审计与放行对账可回查）。
        let sandbox_probe = sg_executor::sandbox::probe();
        let sandbox_policy = sg_executor::sandbox::SandboxPolicy {
            read_paths: work_dir
                .as_ref()
                .map(|p| vec![p.to_string_lossy().to_string()])
                .unwrap_or_default(),
            write_paths: vec![
                work_dir
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
                store
                    .data_dir
                    .join("artifacts")
                    .join(run_id)
                    .to_string_lossy()
                    .to_string(),
            ],
            network_off: true,
        };
        let envelope = json!({
            "snapshot": serde_json::to_value(&snap).unwrap_or_default(),
            "sources": sources["sources"],
            "mode": mode_str(m),
            "modeSource": src,
            "sandbox": {
                "backend": sandbox_probe.backend,
                "version": sandbox_probe.version,
                "policyDigest": sandbox_policy.digest(),
                "blockedReason": sandbox_probe.blocked_reason,
            },
        });
        let _ = store.with_conn(|conn| {
            conn.execute(
                "UPDATE agent_runs SET policy_snapshot=?1 WHERE id=?2",
                rusqlite::params![envelope.to_string(), run_id],
            )?;
            Ok(())
        });
        (snap, envelope, m, src)
    };
    let ctx = sg_agent::tools::ToolCtx {
        mode,
        work_dir: work_dir.clone(),
        artifacts_dir: store.data_dir.join("artifacts").join(run_id),
        // P0-4：回退到主工作区 = 只读模式（run_command 被拒绝），可写执行必须发生在受管 worktree。
        read_only: worktree_info.is_none() && !local_root.is_empty(),
    };
    let executor =
        crate::tool_exec::make_executor(ctx, state.run_store.clone(), project_id.clone());

    // F06/F07/F08 上下文装配：分层指令文件（全局→项目根→docs/）+ manifest 内容块 → 知识层。
    let knowledge_settings = sg_settings::knowledge_defaults::get(store, Some(&project_id))
        .unwrap_or_else(|_| json!({}));
    let instr_settings = sg_agent::instructions::settings_from_json(&knowledge_settings);
    let (instr_text, layers, instr_warnings) =
        sg_agent::instructions::aggregate(&store.data_dir, work_dir.as_deref(), &instr_settings);
    let blocks = sg_context::blocks::manifest_blocks(store, &manifest_id, 64 << 10)
        .unwrap_or_else(|_| json!({"blocks": [], "totalBytes": 0, "includedCount": 0, "excludedCount": 0, "truncated": false}));
    // ADR-032 M2：记忆块按 manifest 冻结 revision 装载；purge/对象缺失 fail-closed 上报（§7.4）。
    let memory_blocks = sg_context::blocks::manifest_memory_blocks(store, &manifest_id, 32 << 10)
        .unwrap_or_else(|_| json!({"blocks": [], "totalBytes": 0, "missing": []}));
    let memory_blocks_text = memory_blocks["blocks"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|b| {
                    format!(
                        "### [memory:{}@{}] {}\n{}",
                        b["memoryId"].as_str().unwrap_or(""),
                        b["revisionNo"].as_i64().unwrap_or(0),
                        b["kind"].as_str().unwrap_or(""),
                        b["text"].as_str().unwrap_or(""),
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default();
    let memory_segment = sg_agent::prompt::memory_text(&memory_blocks_text);
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
    let compact_policy = sg_agent::CompactPolicy {
        threshold_tokens: knowledge_settings
            .get("autoCompactThresholdTokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(24000) as usize,
        keep_turns: knowledge_settings
            .get("compactionKeepTurns")
            .and_then(|x| x.as_u64())
            .unwrap_or(2) as usize,
    };
    let knowledge = sg_agent::prompt::knowledge_text(&instr_text, &blocks_text);
    let env = sg_agent::prompt::PromptEnv {
        mode: Some(mode),
        work_dir_label: work_dir.as_ref().map(|p| p.to_string_lossy().to_string()),
        requires_approval_tools: policy_snapshot
            .tool_rules
            .iter()
            .filter(|r| r.requires_approval)
            .map(|r| r.tool.clone())
            .collect(),
    };
    // M4：AgentProfile developer 层（persona/SOP/输出契约；无 selection 时为 None）。
    let profile_text: Option<String> = {
        let sel_id: String = store
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT agent_selection_id FROM agent_runs WHERE id=?1",
                    [run_id],
                    |r| r.get::<_, String>(0),
                )
                .map_err(Error::from)
            })
            .unwrap_or_default();
        if sel_id.is_empty() {
            None
        } else {
            sg_agent::router::get(store, &sel_id).ok().and_then(|sel| {
                sg_agent::profile::get_version(store, &sel.resolved_profile_version_id)
                    .ok()
                    .and_then(|ver| {
                        sg_agent::profile::version_texts(store, &ver)
                            .ok()
                            .map(|(persona, sop)| {
                                format!(
                                    "【Agent 角色档案】\n角色说明：{}\n标准作业流程：{}\n（profile v{} · digest {}）",
                                    if persona.is_empty() { "（未配置）" } else { persona.trim() },
                                    if sop.is_empty() { "（未配置）" } else { sop.trim() },
                                    ver.version_no,
                                    ver.content_digest
                                )
                            })
                    })
            })
        }
    };
    let initial = sg_agent::prompt::assemble_with_profile(
        &env,
        &allowlist,
        &knowledge,
        &memory_segment,
        &goal,
        profile_text.as_deref(),
    );
    let summary = json!({
        "instructionLayers": sg_agent::instructions::layers_json(&layers),
        "instructionBytes": instr_text.len(),
        "warnings": instr_warnings,
        "knowledgeItems": blocks["includedCount"].clone(),
        "knowledgeBytes": blocks["totalBytes"].clone(),
        "knowledgeExcluded": blocks["excludedCount"].clone(),
        "memoryItems": memory_blocks["blocks"].as_array().map(|a| a.len()).unwrap_or(0),
        "memoryBytes": memory_blocks["totalBytes"].clone(),
        "memoryMissing": memory_blocks["missing"].clone(),
        "promptBytes": sg_agent::prompt::segment_bytes(&initial),
        "modeSource": mode_source,
        "policyRuleCount": policy_snapshot.tool_rules.len(),
        "policySources": if snapshot_envelope.is_null() {
            json!(null)
        } else {
            snapshot_envelope["sources"].clone()
        },
    });
    // rollout（F04）：打开失败不阻断 Run（观测数据可用性优先），记 stderr 继续。
    let mut rollout = sg_agent::rollout::Rollout::open(&store.data_dir, run_id).ok();
    if rollout.is_none() {
        eprintln!("{{\"level\":\"warn\",\"msg\":\"rollout open failed for {run_id}\"}}");
    }
    // ADR-032 §7.4：记忆块缺失（已清除/对象损坏）如实入 rollout；默认 memory optional 不阻断 Run。
    for missing in memory_blocks["missing"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        if let Some(r) = rollout.as_mut() {
            let _ = r.append("memory_block_missing", missing);
        }
    }
    let token = Arc::new(sg_integrations::CancelToken::new());
    state.runs.register(run_id, token.clone());
    // M2：高频 UI delta 转发器（易失通道；不落库）。
    let forwarder: Arc<dyn sg_agent::modelgw::TurnDeltaForwarder> = Arc::new(
        crate::deltas::HubForwarder::new(state.deltas.clone(), run_id, &workitem_id),
    );
    let run_store = state.run_store.clone();
    let audit_store = state.run_store.clone();
    let gateway = state.model.clone();
    let policy = policy_snapshot.clone();
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
    let compact_policy = std::sync::Arc::new(compact_policy);
    let compact_for_task = compact_policy.clone();
    let summary = std::sync::Arc::new(summary);
    let summary_for_task = summary.clone();
    state.handle.spawn(async move {
        let run_id_inner = run_id_owned.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let mut rollout = rollout.take();
            let initial = initial_for_task;
            let compact_policy = compact_for_task;
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
                Some(&token),
                rollout.take(),
                &initial,
                &compact_policy,
                Some(forwarder),
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
        // P0-3：活动终态回写——Run 结束时其绑定的 stage activity 不得停留在 running。
        if let Ok(run_row) = sg_agent::get_run(&audit_store, &run_id_owned) {
            let activity_state = match run_row.status.as_str() {
                "completed_execution" => Some("done"),
                "failed" | "cancelled" => Some("failed"),
                _ => None,
            };
            if let Some(state) = activity_state {
                let activity_id: String = audit_store
                    .with_conn(|conn| {
                        conn.query_row(
                            "SELECT stage_activity_id FROM agent_runs WHERE id=?1",
                            [&run_id_owned],
                            |r| r.get::<_, String>(0),
                        )
                        .map_err(sg_store::Error::from)
                    })
                    .unwrap_or_default();
                if !activity_id.is_empty() {
                    let _ = audit_store.with_conn(|conn| {
                        conn.execute(
                            "UPDATE stage_activities SET state=?1, updated_at=?2 WHERE id=?3 AND state='running'",
                            rusqlite::params![state, sg_store::timefmt::now(), activity_id],
                        )?;
                        Ok(())
                    });
                }
            }
        }
        registry.unregister(&run_id_owned);
    });
    Ok(summary.as_ref().clone())
}

/// M4：model.usage 聚合——model_turns 的 token/缓存/延迟观测 + 压缩次数与前后估算。
fn model_usage(_state: &AppState, store: &Store, run_id: Option<&str>) -> RpcResult {
    let (calls, tokens_in, tokens_out, cached, reasoning, ttft_avg, ttft_p95, total_ms, compactions, compact_before, compact_after) =
        store
            .with_conn(|conn| {
                let turns: (i64, i64, i64, i64, i64, Option<f64>, Option<i64>, i64) = conn
                    .query_row(
                        "SELECT COUNT(*), COALESCE(SUM(tokens_in),0), COALESCE(SUM(tokens_out),0), COALESCE(SUM(cached_tokens),0), COALESCE(SUM(reasoning_tokens),0), AVG(ttft_ms), MAX(ttft_ms), COALESCE(SUM(total_ms),0) FROM model_turns WHERE (?1 IS NULL OR agent_run_id=?1)",
                        rusqlite::params![run_id],
                        |r| {
                            Ok((
                                r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?,
                                r.get(5)?, r.get(6)?, r.get(7)?,
                            ))
                        },
                    )
                    .map_err(Error::from)?;
                let comp: (i64, i64, i64) = conn
                    .query_row(
                        "SELECT COUNT(*), COALESCE(MAX(json_extract(payload,'$.beforeEst')),-1), COALESCE(MAX(json_extract(payload,'$.afterEst')),-1) FROM events_outbox WHERE type='run.compacted' AND (?1 IS NULL OR aggregate_id=?1)",
                        rusqlite::params![run_id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .map_err(Error::from)?;
                Ok((
                    turns.0, turns.1, turns.2, turns.3, turns.4,
                    turns.5, turns.6, turns.7,
                    comp.0, comp.1, comp.2,
                ))
            })
            .map_err(store_err)?;
    let hit_ratio = if tokens_in > 0 {
        Some((cached as f64 / tokens_in as f64 * 1000.0).round() / 1000.0)
    } else {
        None
    };
    Ok(json!({
        "scope": run_id,
        "calls": calls,
        "tokensIn": tokens_in,
        "tokensOut": tokens_out,
        "cachedTokens": cached,
        "cacheHitRatio": hit_ratio,
        "reasoningTokens": reasoning,
        "ttftAvgMs": ttft_avg.map(|v| (v * 10.0).round() / 10.0),
        "ttftP95Ms": ttft_p95,
        "totalMs": total_ms,
        "compactions": compactions,
        "compactionBeforeEst": if compact_before >= 0 { json!(compact_before) } else { json!(null) },
        "compactionAfterEst": if compact_after >= 0 { json!(compact_after) } else { json!(null) },
        "costMicros": 0,
        "costNote": "未接价格表；成本恒 0（诚实口径）",
    }))
}

/// M6：工具清单（候选/活跃/撤销状态 + schema digest）。
fn mcp_tools_list(store: &Store, server_id: Option<&str>) -> RpcResult {
    let items: Vec<Value> = store
        .with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT t.id, t.server_id, s.name, t.tool_name, t.description, t.schema_json, t.schema_digest, t.read_only_hint, t.status
                 FROM mcp_server_tools t JOIN mcp_servers s ON s.id=t.server_id
                 WHERE (?1 IS NULL OR t.server_id=?1)
                 ORDER BY s.name, t.tool_name",
            )?;
            let rows = stmt.query_map(rusqlite::params![server_id], |r| {
                Ok(json!({
                    "toolId": r.get::<_, String>(0)?,
                    "serverId": r.get::<_, String>(1)?,
                    "serverName": r.get::<_, String>(2)?,
                    "toolName": r.get::<_, String>(3)?,
                    "description": r.get::<_, String>(4)?,
                    "schema": serde_json::from_str::<Value>(&r.get::<_, String>(5)?).unwrap_or(Value::Null),
                    "schemaDigest": r.get::<_, String>(6)?,
                    "readOnlyHint": r.get::<_, i64>(7)? == 1,
                    "status": r.get::<_, String>(8)?,
                    "modelName": format!("mcp__{}__{}", r.get::<_, String>(2)?, r.get::<_, String>(3)?),
                }))
            })?;
            let out = rows.flatten().collect::<Vec<_>>();
            Ok(out)
        })
        .map_err(store_err)?;
    Ok(json!({"items": items}))
}

#[cfg(test)]
mod m6_zero_change_tests {
    use super::*;

    /// M6 退出标准：默认无 MCP server 时零行为变化——
    /// 策略快照不含任何 mcp 来源；注册+批准后出现（写工具 High/必审批）。
    #[test]
    fn policy_snapshot_unchanged_without_mcp_servers() {
        let dir = std::env::temp_dir().join(format!("sg-mcp-zero-{}", sg_store::ids::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        let (snap, envelope) = assemble_policy_snapshot(&store);
        let any_mcp = envelope["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["source"] == "mcp");
        assert!(!any_mcp, "无 MCP server 时策略快照不得出现 mcp 来源");
        let before_count = snap.tool_rules.len();

        // 注册+批准（需要 python3 跑 fake server；缺失时跳过该半段）。
        let has_python = std::process::Command::new("python3")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if has_python {
            let v = sg_settings::mcp_ext::server_add(
                &store,
                "srvzero",
                "python3",
                &["/tmp/sg-mcp-e2e/fake_server.py".into(), "ok".into()],
            )
            .unwrap();
            sg_settings::mcp_ext::server_approve(&store, v["serverId"].as_str().unwrap(), "admin")
                .unwrap();
            let (snap2, envelope2) = assemble_policy_snapshot(&store);
            let mcp_rules: Vec<_> = snap2
                .tool_rules
                .iter()
                .filter(|r| r.tool.starts_with("mcp__"))
                .collect();
            assert_eq!(mcp_rules.len(), 2, "活跃 MCP 工具进策略快照");
            let write_rule = mcp_rules
                .iter()
                .find(|r| r.tool.ends_with("send_thing"))
                .unwrap();
            assert!(write_rule.requires_approval, "写工具必须审批");
            assert_eq!(write_rule.data_level, "external");
            assert!(envelope2["sources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["source"] == "mcp" && s["sandboxed"] == json!(false)));
            assert_eq!(snap.tool_rules.len(), before_count, "基线规则集不变");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
