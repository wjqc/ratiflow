//! 结构化计划 RPC（EvoFlow 方案 M2-09 / ADR-036 §6.2 / §8.1）。
//! 全域受 `SIXGATES_PLAN_DAG` 门控（默认 feature_disabled = 零行为变化）。
//! 计划批准走既有 approvals 链（subject_type=plan_revision，0034 扩作用域），不旁路。

use serde_json::{json, Value};

use crate::dispatch::RpcResult;
use crate::state::AppState;
use rusqlite::OptionalExtension;
use sg_protocol::{ErrorCode, RpcError};
use sg_store::Store;
use sg_workflow::plan::{self, PlanAcceptance, PlanTaskInput};

fn plan_dag_enabled() -> bool {
    std::env::var("SIXGATES_PLAN_DAG").ok().as_deref() == Some("1")
}

fn disabled() -> RpcError {
    RpcError::new(
        ErrorCode::InvalidRequest,
        "feature_disabled: SIXGATES_PLAN_DAG 未开启",
    )
}

fn invalid(msg: impl Into<String>) -> RpcError {
    let m: String = msg.into();
    RpcError::new(ErrorCode::InvalidParams, m.as_str())
}

fn store_err(e: sg_store::Error) -> RpcError {
    RpcError::new(ErrorCode::InternalError, e.to_string().as_str())
}

fn str_param(params: &Value, key: &str) -> Result<String, RpcError> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| invalid(format!("missing param: {key}")))
}

fn parse_tasks(params: &Value) -> Result<Vec<PlanTaskInput>, RpcError> {
    let arr = params
        .get("tasks")
        .and_then(|v| v.as_array())
        .ok_or_else(|| invalid("missing param: tasks"))?;
    let mut out = Vec::new();
    for t in arr {
        let acceptance = t.get("acceptance").cloned().unwrap_or(json!({}));
        out.push(PlanTaskInput {
            task_key: t
                .get("taskKey")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid("tasks[].taskKey required"))?
                .to_string(),
            kind: t
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("analysis")
                .to_string(),
            title: t
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            inputs: t
                .get("inputs")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            expected_outputs: t
                .get("expectedOutputs")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            acceptance: serde_json::from_value(acceptance).unwrap_or(PlanAcceptance {
                machine: vec![],
                manual: vec![],
            }),
            effect_class: t
                .get("effectClass")
                .and_then(|v| v.as_str())
                .unwrap_or("read")
                .to_string(),
            deps: t
                .get("deps")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            team_role_key: t
                .get("teamRoleKey")
                .and_then(|v| v.as_str())
                .map(String::from),
        });
    }
    Ok(out)
}

fn revision_view(store: &Store, id: &str) -> Result<Value, RpcError> {
    let rev = plan::revision_by_id(store, id).map_err(store_err)?;
    let tasks = plan::tasks_of(store, id).map_err(store_err)?;
    let attempts = plan::attempts_of(store, id).map_err(store_err)?;
    let dag_tasks: Vec<sg_workflow::dag::DagTask> = tasks
        .iter()
        .map(|t| {
            sg_workflow::dag::DagTask::from_strings(
                t.task_key.clone(),
                t.deps.clone(),
                matches!(
                    t.effect_class.as_str(),
                    "local_write" | "external_write" | "irreversible"
                ),
            )
        })
        .collect();
    let order = sg_workflow::dag::validate_and_order(&dag_tasks)
        .map_err(|e| invalid(format!("{}: {}", e.token, e.message)))?;
    let markdown = plan::render_markdown(
        &tasks
            .iter()
            .map(|t| PlanTaskInput {
                task_key: t.task_key.clone(),
                kind: t.kind.clone(),
                title: t.title.clone(),
                inputs: t.inputs.clone(),
                expected_outputs: t.expected_outputs.clone(),
                acceptance: t.acceptance.clone(),
                effect_class: t.effect_class.clone(),
                deps: t.deps.clone(),
                team_role_key: t.team_role_key.clone(),
            })
            .collect::<Vec<_>>(),
        &order,
        rev.revision_no,
    );
    Ok(json!({
        "revision": serde_json::to_value(&rev).unwrap_or_default(),
        "tasks": tasks,
        "attempts": attempts,
        "topologicalOrder": order,
        "markdown": markdown,
    }))
}

/// 解析 stage_attempt：显式传入优先；否则取 WorkItem 当前关活跃 attempt。
fn resolve_stage_attempt(
    store: &Store,
    workitem_id: &str,
    explicit: Option<&str>,
) -> Result<String, RpcError> {
    if let Some(id) = explicit.filter(|s| !s.is_empty()) {
        return Ok(id.to_string());
    }
    let wi = sg_workitem::get(store, workitem_id).map_err(store_err)?;
    sg_workitem::attempt::active_for_gate(store, workitem_id, &wi.current_gate)
        .map_err(store_err)?
        .map(|a| a.id)
        .ok_or_else(|| {
            invalid(
                "task_dependency_blocked: 当前关无活跃 attempt，先启动活动或显式传 stageAttemptId",
            )
        })
}

pub fn dispatch(_state: &AppState, store: &Store, method: &str, params: &Value) -> RpcResult {
    if !plan_dag_enabled() {
        return Err(disabled());
    }
    match method {
        "plan.createDraft" => {
            let workitem_id = str_param(params, "workItemId")?;
            let stage_attempt_id = resolve_stage_attempt(
                store,
                &workitem_id,
                params.get("stageAttemptId").and_then(|v| v.as_str()),
            )?;
            let tasks = parse_tasks(params)?;
            let _ = str_param(params, "idempotencyKey")?;
            let rev = plan::create_draft(
                store,
                &workitem_id,
                &stage_attempt_id,
                &tasks,
                "local",
                None,
            )
            .map_err(store_err)?;
            Ok(
                json!({"planRevisionId": rev.id, "revision": serde_json::to_value(&rev).unwrap_or_default()}),
            )
        }
        "plan.updateDraft" => {
            let revision_id = str_param(params, "planRevisionId")?;
            let tasks = parse_tasks(params)?;
            let _ = str_param(params, "idempotencyKey")?;
            let rev = plan::update_draft(store, &revision_id, &tasks).map_err(store_err)?;
            Ok(serde_json::to_value(&rev).unwrap_or_default())
        }
        "plan.get" => {
            if let Some(id) = params.get("planRevisionId").and_then(|v| v.as_str()) {
                return revision_view(store, id);
            }
            let workitem_id = str_param(params, "workItemId")?;
            // workItem 维度：取全部 revision 摘要 + 最新详情。
            let list = revision_list(store, &workitem_id)?;
            let latest: Option<String> = list
                .first()
                .and_then(|v| v["id"].as_str())
                .map(String::from);
            let mut out = json!({"revisions": list});
            if let Some(id) = latest {
                out["latest"] = revision_view(store, &id)?;
            }
            Ok(out)
        }
        "plan.list" => {
            let workitem_id = str_param(params, "workItemId")?;
            Ok(json!({"items": revision_list(store, &workitem_id)?}))
        }
        "plan.submit" => {
            let revision_id = str_param(params, "planRevisionId")?;
            let rev = plan::submit(store, &revision_id).map_err(store_err)?;
            // 审批链：subject=plan_revision（0034 扩作用域），走既有 approvals。
            sg_policy::request_approval(
                store,
                "plan_revision",
                &revision_id,
                &rev.digest,
                sg_policy::Risk::High,
                &format!("计划批准 v{}", rev.revision_no),
                3600,
                Some(&rev.workitem_id),
                None,
            )
            .map_err(|e| RpcError::new(ErrorCode::InternalError, e.to_string().as_str()))?;
            Ok(serde_json::to_value(&rev).unwrap_or_default())
        }
        "plan.decide" => {
            let revision_id = str_param(params, "planRevisionId")?;
            let decision = str_param(params, "decision")?;
            let decided_by = str_param(params, "decidedBy")?;
            let reason = params.get("reason").and_then(|v| v.as_str()).unwrap_or("");
            if !matches!(decision.as_str(), "approved" | "rejected") {
                return Err(invalid("decision 须为 approved|rejected"));
            }
            // 解析 pending 审批 → 既有链裁决 → 计划状态迁移。
            let approval_id: Option<String> = store
                .with_conn(|conn| {
                    let row: Option<String> = conn
                        .query_row(
                            "SELECT id FROM approvals WHERE subject_type='plan_revision'
                             AND subject_id=?1 AND status='requested'
                             ORDER BY created_at DESC LIMIT 1",
                            [&revision_id],
                            |r| r.get(0),
                        )
                        .optional()
                        .map_err(sg_store::Error::from)?;
                    Ok(row)
                })
                .map_err(store_err)?;
            let Some(approval_id) = approval_id else {
                return Err(invalid("approval_invalid: 无待决计划审批"));
            };
            sg_policy::decide(store, &approval_id, &decision, &decided_by, reason)
                .map_err(|e| RpcError::new(ErrorCode::InternalError, e.to_string().as_str()))?;
            let rev = if decision == "approved" {
                plan::approve(store, &revision_id, &decided_by)
            } else {
                plan::reject(store, &revision_id, &decided_by)
            }
            .map_err(store_err)?;
            Ok(serde_json::to_value(&rev).unwrap_or_default())
        }
        "plan.start" => {
            let revision_id = str_param(params, "planRevisionId")?;
            let _ = str_param(params, "idempotencyKey")?;
            let (rev, attempts) = plan::start(store, &revision_id).map_err(store_err)?;
            Ok(json!({
                "revision": serde_json::to_value(&rev).unwrap_or_default(),
                "readyAttempts": attempts,
            }))
        }
        "plan.cancel" => {
            let revision_id = str_param(params, "planRevisionId")?;
            let _ = str_param(params, "idempotencyKey")?;
            let rev = plan::cancel(store, &revision_id).map_err(store_err)?;
            Ok(serde_json::to_value(&rev).unwrap_or_default())
        }
        "taskWorkspace.prepare" => {
            let attempt_id = str_param(params, "taskAttemptId")?;
            let rec = sg_executor::workspace::prepare(store, &attempt_id).map_err(store_err)?;
            Ok(json!({"workspace": rec}))
        }
        "taskWorkspace.get" => {
            let attempt_id = str_param(params, "taskAttemptId")?;
            let rec = sg_executor::workspace::get(store, &attempt_id).map_err(store_err)?;
            Ok(json!({"workspace": rec}))
        }
        "taskWorkspace.finalize" => {
            let attempt_id = str_param(params, "taskAttemptId")?;
            let outcome = str_param(params, "outcome")?;
            let _ = str_param(params, "idempotencyKey")?;
            let rec = sg_executor::workspace::finalize(store, &attempt_id, &outcome)
                .map_err(store_err)?;
            Ok(json!({"workspace": rec}))
        }
        _ => Err(invalid(format!("unknown plan method: {method}"))),
    }
}

fn revision_list(store: &Store, workitem_id: &str) -> Result<Vec<Value>, RpcError> {
    store
        .with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, revision_no, status, digest, supersedes_id, approved_by, created_at
                 FROM plan_revisions WHERE workitem_id=?1 ORDER BY revision_no DESC",
            )?;
            let rows = stmt.query_map([workitem_id], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "revision_no": r.get::<_, i64>(1)?,
                    "status": r.get::<_, String>(2)?,
                    "digest": r.get::<_, String>(3)?,
                    "supersedes_id": r.get::<_, Option<String>>(4)?,
                    "approved_by": r.get::<_, Option<String>>(5)?,
                    "created_at": r.get::<_, String>(6)?,
                }))
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .map_err(store_err)
}
