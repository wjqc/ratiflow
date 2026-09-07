//! 数据化工作流模板与实例 RPC（EvoFlow 方案 M1-07 / ADR-036 / §8.1）。
//! 全域受 `SIXGATES_WORKFLOW_TEMPLATE_V2` 门控：默认关闭返回 feature_disabled
//! （kill switch 语义，方案 §11.1）；关闭不删除新事实，实例投影继续 shadow 双写。

use serde_json::{json, Value};

use crate::dispatch::RpcResult;
use crate::state::AppState;
use sg_protocol::{ErrorCode, RpcError};
use sg_store::Store;

fn disabled() -> RpcError {
    RpcError::new(
        ErrorCode::InvalidRequest,
        "feature_disabled: SIXGATES_WORKFLOW_TEMPLATE_V2 未开启",
    )
}

fn err_invalid(msg: impl Into<String>) -> RpcError {
    RpcError::new(ErrorCode::InvalidParams, msg.into().as_str())
}

fn store_err(e: sg_store::Error) -> RpcError {
    RpcError::new(ErrorCode::InternalError, e.to_string().as_str())
}

fn str_param(params: &Value, key: &str) -> Result<String, RpcError> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| err_invalid(format!("missing param: {key}")))
}

fn opt_str(params: &Value, key: &str) -> String {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// 契约 gates 数组 → 模板定义输入（camelCase → GateDefInput）。
/// acceptance 与 policyRefs = 门禁数据化输入面（评审 P0-4）。
fn parse_gates(params: &Value) -> Result<Vec<sg_workflow::template::GateDefInput>, RpcError> {
    let arr = params
        .get("gates")
        .and_then(|v| v.as_array())
        .ok_or_else(|| err_invalid("missing param: gates"))?;
    let mut out = Vec::new();
    for g in arr {
        let str_list = |key: &str| -> Vec<String> {
            g.get(key)
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        };
        let opt_ref = |key: &str| -> Option<String> {
            g.get(key)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from)
        };
        out.push(sg_workflow::template::GateDefInput {
            gate_id: g
                .get("gateId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| err_invalid("gates[].gateId required"))?
                .to_string(),
            title: g
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            purpose: g
                .get("purpose")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            deliverables: str_list("deliverables"),
            acceptance: str_list("acceptance"),
            context_policy_ref: opt_ref("contextPolicyRef"),
            team_policy_ref: opt_ref("teamPolicyRef"),
            workspace_policy_ref: opt_ref("workspacePolicyRef"),
        });
    }
    Ok(out)
}

fn version_json(v: &sg_workflow::template::VersionRecord) -> Value {
    serde_json::to_value(v).unwrap_or_default()
}

pub fn dispatch(_state: &AppState, store: &Store, method: &str, params: &Value) -> RpcResult {
    if !sg_workflow::template::template_v2_enabled() {
        return Err(disabled());
    }
    match method {
        "workflowTemplate.list" => {
            let templates = sg_workflow::template::list_templates(store).map_err(store_err)?;
            let items: Vec<Value> = templates
                .into_iter()
                .map(|(t, versions)| {
                    json!({
                        "id": t.id, "key": t.key, "name": t.name,
                        "createdAt": t.created_at, "updatedAt": t.updated_at,
                        "versions": versions.iter().map(version_json).collect::<Vec<_>>(),
                    })
                })
                .collect();
            Ok(json!({ "items": items }))
        }
        "workflowTemplate.get" => {
            let template_id = str_param(params, "templateId")?;
            let templates = sg_workflow::template::list_templates(store).map_err(store_err)?;
            let found = templates
                .into_iter()
                .find(|(t, _)| t.id == template_id || t.key == template_id);
            let Some((t, versions)) = found else {
                return Err(err_invalid(format!(
                    "workflow_template_invalid: 模板 {template_id} 不存在"
                )));
            };
            let mut out = json!({
                "id": t.id, "key": t.key, "name": t.name,
                "createdAt": t.created_at, "updatedAt": t.updated_at,
                "versions": versions.iter().map(version_json).collect::<Vec<_>>(),
            });
            // 激活版本的关卡定义内联返回（read model 一次取全）。
            if let Some(active) = versions.iter().find(|v| v.status == "active") {
                let defs = sg_workflow::template::definitions_via_store(store, &active.id)
                    .map_err(store_err)?;
                out["activeVersion"] = json!({
                    "version": version_json(active),
                    "gates": defs,
                });
            }
            Ok(out)
        }
        "workflowTemplate.create" => {
            let key = str_param(params, "key")?;
            let name = str_param(params, "name")?;
            let gates = parse_gates(params)?;
            let idem = opt_str(params, "idempotencyKey");
            // 幂等回执（评审 P1）：同 key 重放返回首次响应，不追加新版本。
            crate::dispatch::with_rpc_receipt(store, &idem, "workflowTemplate.create", || {
                // 语义：key 不存在 → 建模板 + draft v1；已存在 → 追加下一个 draft 版本
                // （版本只追加，active 不可原地编辑——ADR-036 决策 3）。
                let existing = sg_workflow::template::list_templates(store)
                    .map_err(store_err)?
                    .into_iter()
                    .find(|(t, _)| t.key == key);
                match existing {
                    None => {
                        let t = sg_workflow::template::create_template(store, &key, &name)
                            .map_err(store_err)?;
                        let version =
                            sg_workflow::template::create_version(store, &t.id, &gates, "local")
                                .map_err(store_err)?;
                        Ok(
                            json!({"template": serde_json::to_value(t).unwrap_or_default(),
                                  "version": version_json(&version)}),
                        )
                    }
                    Some((t, _)) => {
                        let version =
                            sg_workflow::template::create_version(store, &t.id, &gates, "local")
                                .map_err(store_err)?;
                        Ok(
                            json!({"template": serde_json::to_value(t).unwrap_or_default(),
                                  "version": version_json(&version)}),
                        )
                    }
                }
            })
        }
        "workflowTemplate.updateDraft" => {
            let version_id = str_param(params, "versionId")?;
            let gates = parse_gates(params)?;
            let idem = opt_str(params, "idempotencyKey");
            // 幂等回执：同 key 重放返回首次响应，不重复覆盖草稿。
            crate::dispatch::with_rpc_receipt(store, &idem, "workflowTemplate.updateDraft", || {
                let version = sg_workflow::template::update_draft(store, &version_id, &gates)
                    .map_err(store_err)?;
                Ok(version_json(&version))
            })
        }
        "workflowTemplate.activate" => {
            let version_id = str_param(params, "versionId")?;
            let idem = opt_str(params, "idempotencyKey");
            // 幂等回执：同 key 重放返回首次响应，不重复换 active/发事件。
            crate::dispatch::with_rpc_receipt(store, &idem, "workflowTemplate.activate", || {
                let version =
                    sg_workflow::template::activate(store, &version_id).map_err(store_err)?;
                sg_store::outbox::emit(
                    store,
                    "workflow",
                    &version.template_id,
                    "workflow.template_activated",
                    json!({"templateId": version.template_id, "versionId": version.id,
                           "versionNo": version.version_no, "digest": version.content_digest}),
                )
                .map_err(store_err)?;
                Ok(version_json(&version))
            })
        }
        "workflowTemplate.deprecate" => {
            let version_id = str_param(params, "versionId")?;
            let idem = opt_str(params, "idempotencyKey");
            // 幂等回执：同 key 重放返回首次响应，不重复迁移状态。
            crate::dispatch::with_rpc_receipt(store, &idem, "workflowTemplate.deprecate", || {
                let version =
                    sg_workflow::template::deprecate(store, &version_id).map_err(store_err)?;
                Ok(version_json(&version))
            })
        }
        "workflow.getInstance" => {
            let workitem_id = str_param(params, "workItemId")?;
            let Some(instance) =
                sg_workflow::instance::for_workitem(store, &workitem_id).map_err(store_err)?
            else {
                return Err(err_invalid(format!(
                    "workflow_template_invalid: {workitem_id} 无实例"
                )));
            };
            let gates = sg_workflow::instance::gates_for_workitem(store, &workitem_id)
                .map_err(store_err)?
                .unwrap_or_default();
            Ok(json!({
                "instance": serde_json::to_value(&instance).unwrap_or_default(),
                "gates": gates,
            }))
        }
        "workflow.migrationPreview" => {
            let workitem_id = str_param(params, "workItemId")?;
            let target = str_param(params, "targetVersionId")?;
            sg_workflow::instance::migration_preview(store, &workitem_id, &target)
                .map_err(store_err)
        }
        "workflow.migrate" => {
            let workitem_id = str_param(params, "workItemId")?;
            let target = str_param(params, "targetVersionId")?;
            let idem = opt_str(params, "idempotencyKey");
            // 幂等回执：同 key 重放返回首次实例投影，不重复 DELETE/INSERT 投影。
            crate::dispatch::with_rpc_receipt(store, &idem, "workflow.migrate", || {
                let instance = sg_workflow::instance::migrate(store, &workitem_id, &target)
                    .map_err(store_err)?;
                sg_store::outbox::emit(
                    store,
                    "workflow",
                    &workitem_id,
                    "workflow.instance_migrated",
                    json!({"workItemId": workitem_id, "toVersionId": target,
                           "currentGateId": instance.current_gate_id}),
                )
                .map_err(store_err)?;
                Ok(serde_json::to_value(&instance).unwrap_or_default())
            })
        }
        _ => Err(err_invalid(format!("unknown workflow method: {method}"))),
    }
}
