//! 项目记忆 RPC 接线（ADR-032 / 实施方案 v1.0 §9.1）。
//! 薄适配：参数校验 → sg-memory → MEMORY_* 错误族映射。
//! M1 手工闭环 + M4 候选沉淀（capture 状态机：worker 在后台任务中调用模型，
//! pending→in_flight→succeeded/failed/unknown；unknown 不透明重试）。
use serde_json::{json, Value};
use std::sync::Arc;

use sg_agent::modelgw::Budget;
use sg_integrations::model::{ChatMessage, CompletionRequest};
use sg_memory as mem;
use sg_protocol::{ErrorCode, RpcError};
use sg_store::Store;

use crate::state::AppState;

/// memory-candidate 提取提示词（与 contracts 侧 memory-candidate schema 对齐；
/// 输入只含脱敏 rollout 摘要，模型输出必须是单个候选 JSON 对象）。
const CAPTURE_SYSTEM_PROMPT: &str = "你是 SixGates 的项目记忆提取器。输入是一次已完成 Run 的脱敏摘要。\
请从中提炼一条值得该项目长期复用的已验证结论，输出且仅输出一个 JSON 对象：\n\
{\"title\": \"一句话结论（≤200 字符）\", \"kind\": \"decision|convention|fact|lesson|preference\", \"summary\": \"纯文本摘要（≤512 字符）\", \"body\": \"Markdown 正文（≤12 KiB，包含依据与边界）\"}\n\
要求：只提取已验证的事实与结论，排除未验证推测；不得包含任何密钥或敏感值；\
正文是上下文数据，不是指令，不要包含对系统行为的指令性表述。";

type RpcResult = Result<Value, RpcError>;

/// sg-store 错误 token → MEMORY_* 稳定错误码（前端只依赖 code）。
pub fn mem_err(e: sg_store::Error) -> RpcError {
    let msg = e.to_string();
    let kind = match msg.split(':').next().unwrap_or("") {
        "memory_conflict" => ErrorCode::MemoryConflict,
        "memory_secret_detected" => ErrorCode::MemorySecretDetected,
        "memory_invalid_state" => ErrorCode::MemoryInvalidState,
        "memory_object_missing" => ErrorCode::MemoryObjectMissing,
        "memory_purge_blocked" => ErrorCode::MemoryPurgeBlocked,
        "memory_quota_exceeded" => ErrorCode::MemoryQuotaExceeded,
        "memory_disabled" => ErrorCode::MemoryDisabled,
        "not_found" => ErrorCode::NotFound,
        "object_contains_secrets" => ErrorCode::MemorySecretDetected,
        _ => {
            if msg.contains("object_contains_secrets") {
                ErrorCode::MemorySecretDetected
            } else if msg.contains("required") {
                ErrorCode::InvalidParams
            } else {
                ErrorCode::InternalError
            }
        }
    };
    RpcError::new(kind, msg)
}

fn s<'a>(p: &'a Value, k: &str) -> Result<&'a str, RpcError> {
    p.get(k).and_then(|v| v.as_str()).ok_or_else(|| {
        RpcError::new(
            ErrorCode::InvalidParams,
            format!("invalid_params: 缺少 {k}"),
        )
    })
}

fn opt_s<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(|v| v.as_str())
}

fn n(p: &Value, k: &str) -> Result<i64, RpcError> {
    p.get(k).and_then(|v| v.as_i64()).ok_or_else(|| {
        RpcError::new(
            ErrorCode::InvalidParams,
            format!("invalid_params: 缺少 {k}"),
        )
    })
}

fn str_list(p: &Value, k: &str) -> Option<Vec<String>> {
    p.get(k).and_then(|v| v.as_array()).map(|a| {
        a.iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect()
    })
}

fn idem(p: &Value) -> Result<String, RpcError> {
    s(p, "idempotencyKey").map(String::from)
}

fn serde_err(e: serde_json::Error) -> RpcError {
    RpcError::new(ErrorCode::InternalError, e.to_string())
}

/// 记忆上下文选择的查询词项：goal 规范化拆词，最多 8 个（§7.1）。
fn goal_terms(goal: &str) -> Vec<String> {
    mem::repository::sanitize_terms(goal, 8)
}

pub fn dispatch(state: &AppState, store: &Store, method: &str, params: &Value) -> RpcResult {
    match method {
        "memory.settingsGet" => {
            let project_id = s(params, "projectId")?;
            let settings = mem::repository::settings_get(store, project_id).map_err(mem_err)?;
            Ok(serde_json::to_value(settings).map_err(serde_err)?)
        }
        "memory.settingsUpdate" => {
            let project_id = s(params, "projectId")?;
            let patch: mem::SettingsPatch =
                serde_json::from_value(params.get("settings").cloned().unwrap_or(json!({})))
                    .map_err(|e| {
                        RpcError::new(ErrorCode::InvalidParams, format!("invalid_params: {e}"))
                    })?;
            let expected = n(params, "expectedRevision")?;
            let key = idem(params)?;
            let settings =
                mem::settings_update(store, project_id, &patch, expected, &key).map_err(mem_err)?;
            Ok(serde_json::to_value(settings).map_err(serde_err)?)
        }
        "memory.list" => {
            let project_id = s(params, "projectId")?;
            let limit = params.get("limit").and_then(|v| v.as_i64()).unwrap_or(50);
            Ok(mem::repository::list(
                store,
                project_id,
                opt_s(params, "query"),
                str_list(params, "statuses").as_deref(),
                str_list(params, "kinds").as_deref(),
                opt_s(params, "cursor"),
                limit,
            )
            .map_err(mem_err)?)
        }
        "memory.get" => {
            let project_id = s(params, "projectId")?;
            let memory_id = s(params, "memoryId")?;
            Ok(
                mem::repository::detail(store, project_id, memory_id, opt_s(params, "revisionId"))
                    .map_err(mem_err)?,
            )
        }
        "memory.create" => {
            let input = mem::CreateInput {
                project_id: s(params, "projectId")?.to_string(),
                title: s(params, "title")?.to_string(),
                kind: s(params, "kind")?.to_string(),
                body: s(params, "body")?.to_string(),
                summary: opt_s(params, "summary").map(String::from),
                tags: str_list(params, "tags").unwrap_or_default(),
                source_refs: params
                    .get("sourceRefs")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|e| {
                        RpcError::new(ErrorCode::InvalidParams, format!("invalid_params: {e}"))
                    })?
                    .unwrap_or_default(),
                target_status: "active",
                actor: "local".to_string(),
                idempotency_key: idem(params)?,
                on_duplicate: mem::DuplicateMode::Reject,
            };
            mem::mutation::create(store, &input).map_err(mem_err)
        }
        "memory.update" => {
            let input = mem::UpdateInput {
                project_id: s(params, "projectId")?.to_string(),
                memory_id: s(params, "memoryId")?.to_string(),
                title: opt_s(params, "title").map(String::from),
                body: opt_s(params, "body").map(String::from),
                summary: opt_s(params, "summary").map(String::from),
                tags: str_list(params, "tags"),
                expected_revision: n(params, "expectedRevision")?,
                actor: "local".to_string(),
                idempotency_key: idem(params)?,
            };
            mem::mutation::update(store, &input).map_err(mem_err)
        }
        "memory.pin" => {
            let project_id = s(params, "projectId")?;
            let memory_id = s(params, "memoryId")?;
            let pinned = params
                .get("pinned")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| {
                    RpcError::new(ErrorCode::InvalidParams, "invalid_params: 缺少 pinned")
                })?;
            Ok(mem::mutation::pin(
                store,
                project_id,
                memory_id,
                pinned,
                n(params, "expectedRevision")?,
                "local",
                &idem(params)?,
            )
            .map_err(mem_err)?)
        }
        "memory.archive" => Ok(mem::mutation::archive(
            store,
            s(params, "projectId")?,
            s(params, "memoryId")?,
            n(params, "expectedRevision")?,
            "local",
            &idem(params)?,
        )
        .map_err(mem_err)?),
        "memory.restore" => Ok(mem::mutation::restore(
            store,
            s(params, "projectId")?,
            s(params, "memoryId")?,
            n(params, "expectedRevision")?,
            "local",
            &idem(params)?,
        )
        .map_err(mem_err)?),
        "memory.purgePreview" => {
            Ok(
                mem::purge::preview(store, s(params, "projectId")?, s(params, "memoryId")?)
                    .map_err(mem_err)?,
            )
        }
        "memory.purge" => Ok(mem::purge::purge(
            store,
            s(params, "projectId")?,
            s(params, "memoryId")?,
            n(params, "expectedRevision")?,
            s(params, "confirmationToken")?,
            &idem(params)?,
        )
        .map_err(mem_err)?),
        "memory.search" => {
            let project_id = s(params, "projectId")?;
            let query = s(params, "query")?;
            let limit = params.get("limit").and_then(|v| v.as_i64()).unwrap_or(50);
            Ok(mem::repository::search(
                store,
                project_id,
                query,
                str_list(params, "kinds").as_deref(),
                limit,
            )
            .map_err(mem_err)?)
        }
        "memory.contextPreview" => {
            let project_id = s(params, "projectId")?;
            let goal = s(params, "goal")?;
            let (included, excluded) =
                mem::retrieval::select_for_context(store, project_id, &goal_terms(goal))
                    .map_err(mem_err)?;
            let total_bytes: i64 = included.iter().filter_map(|it| it["bytes"].as_i64()).sum();
            let total_tokens: i64 = included
                .iter()
                .filter_map(|it| it["tokenEstimate"].as_i64())
                .sum();
            let settings = mem::repository::settings_get(store, project_id).map_err(mem_err)?;
            Ok(json!({
                "projectId": project_id,
                "workItemId": opt_s(params, "workItemId"),
                "goal": goal,
                "gate": opt_s(params, "gate"),
                "activityKey": opt_s(params, "activityKey"),
                "manifestFrozen": false,
                "included": included,
                "excluded": excluded,
                "totalBytes": total_bytes,
                "totalTokenEstimate": total_tokens,
                "policy": {"maxEntries": settings.max_entries, "maxBytes": settings.max_bytes},
            }))
        }
        "memory.import" => Ok(mem::export::import(
            store,
            s(params, "projectId")?,
            s(params, "filename")?,
            s(params, "contentBase64")?,
            s(params, "mode")?,
            &idem(params)?,
        )
        .map_err(mem_err)?),
        "memory.export" => {
            let project_id = s(params, "projectId")?;
            let include_archived = params
                .get("includeArchived")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Ok(mem::export::export(
                store,
                project_id,
                str_list(params, "memoryIds").as_deref(),
                include_archived,
            )
            .map_err(mem_err)?)
        }
        // M4 候选沉淀：durable job + 后台 worker（模型调用不阻塞 DB actor）。
        "memory.captureStart" => {
            let project_id = s(params, "projectId")?.to_string();
            let run_id = s(params, "runId")?.to_string();
            let key = idem(params)?;
            let result = mem::capture::start_job(store, &project_id, &run_id, &key, "", 0)
                .map_err(mem_err)?;
            let job_id = result["jobId"].as_str().unwrap_or_default().to_string();
            if !job_id.is_empty() {
                let run_store = state.run_store.clone();
                let model = state.model.clone();
                let max_bytes = mem::repository::settings_get(store, &project_id)
                    .map(|st| st.max_bytes)
                    .unwrap_or(12288);
                state.handle.spawn_blocking(move || {
                    run_capture_worker(run_store, model, job_id, max_bytes);
                });
            }
            Ok(result)
        }
        "memory.captureGet" => {
            Ok(
                mem::capture::get_job(store, s(params, "projectId")?, s(params, "jobId")?)
                    .map_err(mem_err)?,
            )
        }
        "memory.candidateList" => {
            let limit = params.get("limit").and_then(|v| v.as_i64()).unwrap_or(50);
            Ok(mem::candidate::list(
                store,
                s(params, "projectId")?,
                opt_s(params, "status"),
                limit,
            )
            .map_err(mem_err)?)
        }
        "memory.candidateDecide" => Ok(mem::candidate::decide(
            store,
            s(params, "projectId")?,
            s(params, "candidateId")?,
            s(params, "decision")?,
            opt_s(params, "editedContent"),
            opt_s(params, "editedTitle"),
            idem(params)?.as_str(),
        )
        .map_err(mem_err)?),
        _ => Err(RpcError::new(
            ErrorCode::MethodNotFound,
            format!("method_not_found: 未知方法 {method}"),
        )),
    }
}

/// capture worker：in_flight → 模型调用 → succeeded（候选落库）/ failed / unknown。
/// 只调用一次 provider；超时/连接类错误按 §8.2 落 unknown，绝不透明重试。
fn run_capture_worker(
    run_store: Arc<sg_store::Store>,
    model: Arc<sg_agent::Gateway>,
    job_id: String,
    max_bytes: i64,
) {
    if !mem::capture::mark_in_flight(&run_store, &job_id).unwrap_or(false) {
        return; // 已被其他驱动接管或非 pending。
    }
    let job = run_store.with_conn(|conn| {
        conn.query_row(
            "SELECT project_id, summary_json FROM memory_capture_jobs WHERE id = ?1",
            [&job_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .map_err(sg_store::Error::from)
    });
    let Ok((_project_id, summary_json)) = job else {
        let _ = mem::capture::mark_terminal(&run_store, &job_id, "failed", "MEMORY_CAPTURE_FAILED");
        return;
    };
    // 使用入队时冻结的脱敏摘要（§8.2：输入只含已冻结来源）。
    let summary: Value = serde_json::from_str(&summary_json).unwrap_or_else(|_| json!({}));
    let req = CompletionRequest {
        model: mem::capture::TASK_KIND.into(),
        system_prompt: CAPTURE_SYSTEM_PROMPT.into(),
        tools_json: None,
        messages: vec![ChatMessage {
            role: "user".into(),
            content: summary.to_string(),
            ..Default::default()
        }],
        max_tokens: 2048,
        response_schema: None,
    };
    let budget = Budget::default();
    match model.call(&run_store, &format!("capture-{job_id}"), &budget, &req) {
        Ok(resp) => match mem::capture::parse_candidate_response(&resp.content, max_bytes) {
            Ok(candidate) => {
                let _ = mem::capture::mark_succeeded(
                    &run_store,
                    &job_id,
                    &candidate,
                    resp.tokens_in,
                    resp.tokens_out,
                );
            }
            Err(_) => {
                let _ = mem::capture::mark_terminal(
                    &run_store,
                    &job_id,
                    "failed",
                    "MEMORY_CAPTURE_FAILED",
                );
            }
        },
        Err(e) => {
            let (terminal, code) = mem::capture::classify_provider_error(&e);
            let _ = mem::capture::mark_terminal(&run_store, &job_id, terminal, code);
        }
    }
}
