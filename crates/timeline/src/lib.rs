//! F05 时间线投影：outbox → 稳定 UI 事件（sequence 去重/补发）。
use serde_json::{json, Value};

use sg_store::{outbox, Error, Store};

/// 快照：workitem 相关事件 + 全局非 workitem 事件（审批/项目）按 sequence 升序。
pub fn snapshot(
    store: &Store,
    workitem_id: Option<&str>,
    after_seq: i64,
    limit: i64,
) -> Result<Vec<Value>, Error> {
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

fn attempt_summary(row: &Value, state: &str) -> String {
    let gate = row["payload"]["gate"].as_str().unwrap_or("");
    let no = row["payload"]["attemptNo"].as_i64().unwrap_or(0);
    format!("{gate}关第 {no} 次尝试：{state}")
}

fn summarize(row: &Value) -> String {
    let kind = row["type"].as_str().unwrap_or("event");
    match kind {
        "workitem.created" => "创建工作项".into(),
        "stage.passed" => format!(
            "{}关通过",
            row["payload"]["gate"].as_str().unwrap_or("当前")
        ),
        "stage.running" => format!(
            "{}关开始",
            row["payload"]["gate"].as_str().unwrap_or("当前")
        ),
        "stage.stale" => format!(
            "{}关输入过期",
            row["payload"]["gate"].as_str().unwrap_or("当前")
        ),
        "gate.evaluated" => {
            if row["payload"]["passed"].as_bool() == Some(true) {
                "门禁通过".into()
            } else {
                "门禁未通过".into()
            }
        }
        "baseline.frozen" => "基线冻结".into(),
        "artifact.reviewed" => {
            format!("评审：{}", row["payload"]["verdict"].as_str().unwrap_or(""))
        }
        "approval.requested" => "高风险动作请求审批".into(),
        "approval.approved" => "审批通过".into(),
        "approval.rejected" => "审批拒绝".into(),
        "run.started" => "任务开始执行".into(),
        "run.waiting_approval" => "任务挂起等待审批".into(),
        "run.resumed" => "审批通过，任务恢复".into(),
        "run.compacted" => "上下文已自动压缩".into(),
        "tool.started" => format!(
            "工具开始执行：{}",
            row["payload"]["tool"].as_str().unwrap_or("")
        ),
        "tool.completed" => format!(
            "工具执行完成：{}",
            row["payload"]["tool"].as_str().unwrap_or("")
        ),
        "evidence.recorded" => format!(
            "记录证据：{}",
            row["payload"]["kind"].as_str().unwrap_or("")
        ),
        "passport.issued" => "通关文牒签发".into(),
        "attachment.imported" => format!(
            "导入附件：{}",
            row["payload"]["filename"].as_str().unwrap_or("")
        ),
        "requirement.revision_imported" => format!(
            "需求修订导入：v{}（{} 条需求项）",
            row["payload"]["revisionNo"].as_i64().unwrap_or(0),
            row["payload"]["itemCount"].as_i64().unwrap_or(0)
        ),
        "trace.edge_created" => format!(
            "追溯记录建立：{}",
            row["payload"]["relation"].as_str().unwrap_or("")
        ),
        "stage.attempt_prepared" => attempt_summary(row, "已准备"),
        "stage.attempt_running" => attempt_summary(row, "执行中"),
        "stage.attempt_review_ready" => attempt_summary(row, "产出就绪"),
        "stage.attempt_awaiting_user_approval" => attempt_summary(row, "等待用户放行审批"),
        "stage.attempt_approved" => attempt_summary(row, "已批准"),
        "stage.attempt_rejected" => attempt_summary(row, "放行被拒绝"),
        "stage.attempt_changes_requested" => attempt_summary(row, "放行要求修改"),
        "stage.attempt_superseded" => attempt_summary(row, "已被上游变化取代"),
        "stage.attempt_rolled_back" => attempt_summary(row, "已回滚"),
        "stage.attempt_failed" => attempt_summary(row, "失败"),
        "stage.attempt_cancelled" => attempt_summary(row, "已取消"),
        "gate.release_requested" => format!(
            "放行审批已提交：{}关（等待用户决定）",
            row["payload"]["gate"].as_str().unwrap_or("")
        ),
        "gate.release_approved" => format!(
            "{}关放行批准，进入下一关",
            row["payload"]["gate"].as_str().unwrap_or("")
        ),
        "gate.release_rejected" => format!(
            "{}关放行被拒绝",
            row["payload"]["gate"].as_str().unwrap_or("")
        ),
        "gate.changes_requested" => format!(
            "{}关要求修改，可继续当前尝试",
            row["payload"]["gate"].as_str().unwrap_or("")
        ),
        "approval.changes_requested" => "审批决定：要求修改".into(),
        "snapshot.created" => format!(
            "快照已创建（{}）",
            row["payload"]["kind"].as_str().unwrap_or("stage_entry")
        ),
        "rollback.previewed" => "回滚影响预览已生成".into(),
        "rollback.requested" => "回滚请求已提交审批".into(),
        "rollback.started" => "回滚开始执行".into(),
        "rollback.completed" => "回滚完成：控制面已恢复，历史保留".into(),
        "rollback.blocked" => "回滚被阻塞：存在需人工处置的外部副作用".into(),
        "rollback.failed" => format!(
            "回滚失败：{}",
            row["payload"]["reason"].as_str().unwrap_or("")
        ),
        "rollback.cancelled" => "回滚已取消".into(),
        other => other.to_string(),
    }
}

/// 重连恢复：sequence 之后的事件补发。
pub fn events_after(
    store: &Store,
    workitem_id: Option<&str>,
    after_seq: i64,
) -> Result<Vec<Value>, Error> {
    snapshot(store, workitem_id, after_seq, 1000)
}
