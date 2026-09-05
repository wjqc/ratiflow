//! F06 进度聚合：目标、六关状态、阻塞、证据数与待审批数（来自真实 Stage，禁止前端推断）。
use serde_json::{json, Value};

use sg_store::{Error, Store};

use crate::stages;

pub fn progress(store: &Store, workitem_id: &str) -> Result<Value, Error> {
    let wi = crate::get(store, workitem_id)?;
    let stages = stages(store, workitem_id)?;
    let evidence_count: i64 = store.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM evidences WHERE workitem_id=?1",
            [workitem_id],
            |r| r.get(0),
        )
        .map_err(Error::from)
    })?;
    let pending_approvals: i64 = store.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM approvals WHERE status='requested' AND subject_id IN (
                SELECT id FROM deployments WHERE workitem_id=?1
                UNION ALL SELECT id FROM tool_proposals WHERE agent_run_id IN (
                    SELECT id FROM agent_runs WHERE workitem_id=?1))",
            [workitem_id],
            |r| r.get(0),
        )
        .map_err(Error::from)
    })?;

    // M1-04：gateIndex 按实例顺序计算（自定义模板无六值假设）。
    let gate_index = crate::gate_refs(store, workitem_id).ok().and_then(|refs| {
        refs.iter()
            .position(|g| g.gate_id == wi.current_gate)
            .map(|i| i as i64)
    });
    let mut blocked_reason = Value::Null;
    for stage in &stages {
        if stage.gate == wi.current_gate {
            match stage.state.as_str() {
                "awaiting_approval" => blocked_reason = json!("等待人工审批"),
                "blocked" | "failed" => {
                    blocked_reason = json!(format!("当前关状态：{}", stage.state))
                }
                "stale" => blocked_reason = json!("输入基线已过期，需重新绑定"),
                _ => {}
            }
        }
    }

    Ok(json!({
        "workItemId": wi.id,
        "title": wi.title,
        "currentGate": wi.current_gate,
        "gateIndex": gate_index,
        "stages": stages,
        "evidenceCount": evidence_count,
        "pendingApprovals": pending_approvals,
        "blockedReason": blocked_reason,
        "updatedAt": wi.updated_at,
    }))
}
