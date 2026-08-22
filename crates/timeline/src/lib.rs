//! F05 时间线投影：outbox → 稳定 UI 事件（sequence 去重/补发）。
use serde_json::{json, Value};

use sg_store::{outbox, Error, Store};

/// 快照：workitem 相关事件 + 全局非 workitem 事件（审批/项目）按 sequence 升序。
pub fn snapshot(store: &Store, workitem_id: Option<&str>, after_seq: i64, limit: i64) -> Result<Vec<Value>, Error> {
    let rows = outbox::replay(store, after_seq, limit)?;
    let mut events: Vec<Value> = rows
        .into_iter()
        .filter(|row| match workitem_id {
            Some(wi) => {
                row.get("aggregateType").and_then(|t| t.as_str()) == Some("workitem")
                    && (row.get("payload").and_then(|p| p.get("workitemId")).and_then(|w| w.as_str()) == Some(wi)
                        || row.get("aggregateId").and_then(|a| a.as_str()) == Some(wi))
            }
            None => true,
        })
        .map(|row| {
            json!({
                "sequence": row["sequence"],
                "type": row["type"],
                "workItemId": row["payload"].get("workitemId").cloned().unwrap_or_else(|| row["aggregateId"].clone()),
                "occurredAt": row["occurredAt"],
                "summary": summarize(&row),
                "detail": row["payload"],
            })
        })
        .collect();
    events.sort_by_key(|e| e["sequence"].as_i64().unwrap_or(0));
    Ok(events)
}

fn summarize(row: &Value) -> String {
    let kind = row["type"].as_str().unwrap_or("event");
    match kind {
        "workitem.created" => "创建工作项".into(),
        "stage.passed" => format!("{}关通过", row["payload"]["gate"].as_str().unwrap_or("当前")),
        "stage.running" => format!("{}关开始", row["payload"]["gate"].as_str().unwrap_or("当前")),
        "stage.stale" => format!("{}关输入过期", row["payload"]["gate"].as_str().unwrap_or("当前")),
        "gate.evaluated" => {
            if row["payload"]["passed"].as_bool() == Some(true) { "门禁通过".into() } else { "门禁未通过".into() }
        }
        "baseline.frozen" => "基线冻结".into(),
        "artifact.reviewed" => format!("评审：{}", row["payload"]["verdict"].as_str().unwrap_or("")),
        "approval.requested" => "高风险动作请求审批".into(),
        "approval.approved" => "审批通过".into(),
        "approval.rejected" => "审批拒绝".into(),
        "evidence.recorded" => format!("记录证据：{}", row["payload"]["kind"].as_str().unwrap_or("")),
        "passport.issued" => "通关文牒签发".into(),
        "attachment.imported" => format!("导入附件：{}", row["payload"]["filename"].as_str().unwrap_or("")),
        other => other.to_string(),
    }
}

/// 重连恢复：sequence 之后的事件补发。
pub fn events_after(store: &Store, workitem_id: Option<&str>, after_seq: i64) -> Result<Vec<Value>, Error> {
    snapshot(store, workitem_id, after_seq, 1000)
}
