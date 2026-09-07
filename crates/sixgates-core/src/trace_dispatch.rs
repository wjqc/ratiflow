//! Trace 与驾驶舱 RPC（EvoFlow 方案 M5-04 / ADR-039）：
//! trace.graph（durable spans 重建）、trace.usage（诚实语义：Provider 未提供
//! 的字段为 unknown，不填 0）、trace.taskReadModel（真实进度 + checkpoint）。
//! 只读面——不新增副作用。

use serde_json::{json, Value};

use crate::dispatch::RpcResult;
use crate::state::AppState;
use sg_protocol::{ErrorCode, RpcError};
use sg_store::Store;

fn store_err(e: sg_store::Error) -> RpcError {
    RpcError::new(ErrorCode::InternalError, e.to_string().as_str())
}

fn invalid(msg: impl Into<String>) -> RpcError {
    let m: String = msg.into();
    RpcError::new(ErrorCode::InvalidParams, m.as_str())
}

fn str_param(params: &Value, key: &str) -> Result<String, RpcError> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| invalid(format!("missing param: {key}")))
}

pub fn dispatch(_state: &AppState, store: &Store, method: &str, params: &Value) -> RpcResult {
    match method {
        "trace.graph" => {
            let workitem_id = str_param(params, "workItemId")?;
            let spans =
                sg_workflow::trace::spans_for_workitem(store, &workitem_id).map_err(store_err)?;
            Ok(json!({
                "workItemId": workitem_id,
                "spans": spans,
                "rebuiltFrom": "durable_facts",
            }))
        }
        "trace.usage" => {
            // 诚实语义（§6.12）：Provider 未提供的字段为 NULL → JSON null（unknown），
            // 不填 0 冒充；成本在价格表接入前恒 unknown。
            let workitem_id = str_param(params, "workItemId")?;
            let rows: Vec<Value> = store
                .with_conn(|conn| {
                    let mut stmt = conn.prepare(
                        "SELECT mt.provider, mt.model, mt.tokens_in, mt.tokens_out,
                                COALESCE(mt.cache_read_tokens, mt.cached_tokens),
                                COALESCE(mt.cache_write_tokens, -1),
                                mt.usage_estimated, mt.cost_state, mt.created_at
                         FROM model_turns mt
                         JOIN agent_runs ar ON ar.id = mt.agent_run_id
                         WHERE ar.workitem_id=?1
                         ORDER BY mt.created_at DESC LIMIT 50",
                    )?;
                    let rows = stmt.query_map([&workitem_id], |r| {
                        let cached: i64 = r.get(4)?;
                        let cache_write: i64 = r.get(5)?;
                        let estimated: i64 = r.get(6)?;
                        // M5-01 语义 + 评审 P1：新列 cache_read 优先，legacy cached_tokens 兜底；
                        // >0 才是已知的缓存命中，否则 unknown（null）。cache write 无新列数据时为 null。
                        Ok(json!({
                            "provider": r.get::<_, String>(0)?,
                            "model": r.get::<_, String>(1)?,
                            "promptTokens": r.get::<_, i64>(2)?,
                            "completionTokens": r.get::<_, i64>(3)?,
                            "cacheRead": if cached > 0 { serde_json::Value::from(cached) } else { serde_json::Value::Null },
                            "cacheWrite": if cache_write > 0 { serde_json::Value::from(cache_write) } else { serde_json::Value::Null },
                            "usageEstimated": estimated != 0,
                            "costState": r.get::<_, String>(7)?,
                            "createdAt": r.get::<_, String>(8)?,
                        }))
                    })?;
                    let mut out = Vec::new();
                    for row in rows {
                        out.push(row?);
                    }
                    Ok(out)
                })
                .map_err(store_err)?;
            let cache_known = rows.iter().any(|r| !r["cacheRead"].is_null());
            Ok(json!({
                "workItemId": workitem_id,
                "turns": rows,
                "cacheRead": if cache_known { Value::Null } else { json!("unknown") },
                "cost": "unknown",
                "note": "cache/cost 字段缺失时显示 unknown，不填 0（EV-016）",
            }))
        }
        "trace.taskReadModel" => {
            let workitem_id = str_param(params, "workItemId")?;
            let model = sg_workflow::read_model::build(store, &workitem_id).map_err(store_err)?;
            let facts_sha =
                sg_workflow::read_model::save_checkpoint(store, &workitem_id).map_err(store_err)?;
            Ok(json!({
                "model": serde_json::to_value(&model).unwrap_or_default(),
                "factsSha256": facts_sha,
                "source": "durable_facts",
            }))
        }
        "trace.restoreCheckpoint" => {
            // UI 断线恢复：优先 durable checkpoint；checkpoint 缺失或事实已漂移
            // （load 返回 None，评审 P1 修复的新鲜度校验）→ 从真实事实即时重建，
            // 绝不把陈旧驾驶舱当作当前状态。
            let workitem_id = str_param(params, "workItemId")?;
            if let Some(model) =
                sg_workflow::read_model::load_checkpoint(store, &workitem_id).map_err(store_err)?
            {
                return Ok(json!({
                    "restored": true,
                    "source": "checkpoint",
                    "model": serde_json::to_value(&model).unwrap_or_default(),
                }));
            }
            match sg_workflow::read_model::build(store, &workitem_id) {
                Ok(model) => {
                    let sha = sg_workflow::read_model::save_checkpoint(store, &workitem_id)
                        .map_err(store_err)?;
                    Ok(json!({
                        "restored": true,
                        "source": "rebuilt_from_facts",
                        "factsSha256": sha,
                        "model": serde_json::to_value(&model).unwrap_or_default(),
                    }))
                }
                Err(_) => Ok(json!({"restored": false})),
            }
        }
        _ => Err(invalid(format!("unknown trace method: {method}"))),
    }
}
