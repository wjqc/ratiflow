//! 风险分级、ActionDigest 与审批（v2 ADR-022/023 行为等价）。
//! 审批绑定 ActionDigest：参数变化使既有批准立即失效。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use sg_store::{ids, timefmt, Error, Store};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Low,
    Medium,
    High,
}

impl Risk {
    pub fn as_str(&self) -> &'static str {
        match self {
            Risk::Low => "low",
            Risk::Medium => "medium",
            Risk::High => "high",
        }
    }
}

/// 规范化动作 → SHA-256。键排序 JSON 保证参数变化必然改变 digest。
pub fn action_digest(action: &Value) -> String {
    let canonical = canonicalize(action);
    ids::hex(&Sha256::digest(canonical.as_bytes()))
}

fn canonicalize(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(&sort_json(other)).unwrap_or_default(),
    }
}

fn sort_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sorted = serde_json::Map::new();
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for key in keys {
                sorted.insert(key.clone(), sort_json(&map[key]));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(sort_json).collect()),
        other => other.clone(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolRule {
    pub tool: String,
    pub risk: Risk,
    #[serde(default)]
    pub requires_approval: bool,
    #[serde(default)]
    pub data_level: String,
    #[serde(default = "default_max_result")]
    pub max_result_bytes: i64,
    #[serde(default = "default_timeout")]
    pub timeout_sec: i64,
}

fn default_max_result() -> i64 {
    1 << 20
}
fn default_timeout() -> i64 {
    120
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub tool_rules: Vec<ToolRule>,
    pub approval_ttl_secs: i64,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            tool_rules: vec![],
            approval_ttl_secs: 3600,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("action_denied: tool {0} not in allowlist")]
    ActionDenied(String),
    #[error("action_denied: empty action digest")]
    EmptyDigest,
    #[error("approval_required")]
    ApprovalRequired,
    #[error("approval_invalid: approved digest != current")]
    ApprovalInvalid,
    #[error("approval_expired")]
    ApprovalExpired,
}

/// 评估工具提案：allowlist 外拒绝；高风险需审批。
pub fn evaluate(snapshot: &Snapshot, tool: &str, digest: &str) -> Result<ToolRule, PolicyError> {
    let rule = snapshot
        .tool_rules
        .iter()
        .find(|r| r.tool == tool)
        .ok_or_else(|| PolicyError::ActionDenied(tool.into()))?;
    if digest.is_empty() {
        return Err(PolicyError::EmptyDigest);
    }
    if rule.requires_approval || matches!(rule.risk, Risk::High) {
        return Err(PolicyError::ApprovalRequired);
    }
    Ok(rule.clone())
}

// --- 审批持久化 ---

#[derive(Debug, Clone, Serialize)]
pub struct Approval {
    pub id: String,
    pub subject_type: String,
    pub subject_id: String,
    /// M2 作用域列（AC-SW-05：跨任务待审批互不阻塞）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workitem_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage_attempt_id: Option<String>,
    pub action_digest: String,
    pub risk: String,
    pub status: String,
    pub requested_by: String,
    pub expires_at: String,
    pub reason: String,
    pub created_at: String,
}

#[allow(clippy::too_many_arguments)]
pub fn request_approval(
    store: &Store,
    subject_type: &str,
    subject_id: &str,
    digest: &str,
    risk: Risk,
    reason: &str,
    ttl_secs: i64,
    workitem_id: Option<&str>,
    stage_attempt_id: Option<&str>,
) -> Result<Approval, Error> {
    let id = ids::new_id("appr");
    let now = timefmt::now();
    // ttl_secs <= 0 表示不限时（如关卡放行的人工评审）；expires_at 留空即永不失效。
    let expires = if ttl_secs > 0 {
        (timefmt::parse(&now).unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
            + time::Duration::seconds(ttl_secs))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or(now.clone())
    } else {
        String::new()
    };
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO approvals(id, subject_type, subject_id, workitem_id, stage_attempt_id, action_digest, risk, status,
                requested_by, expires_at, reason, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,'requested','local',?8,?9,?10)",
            rusqlite::params![
                id,
                subject_type,
                subject_id,
                workitem_id,
                stage_attempt_id,
                digest,
                risk.as_str(),
                expires,
                reason,
                now
            ],
        )?;
        Ok(())
    })?;
    sg_store::outbox::emit(
        store,
        "approval",
        &id,
        "approval.requested",
        serde_json::json!({"subjectType": subject_type, "subjectId": subject_id, "risk": risk.as_str()}),
    )?;
    Ok(Approval {
        id,
        subject_type: subject_type.into(),
        subject_id: subject_id.into(),
        workitem_id: workitem_id.map(String::from),
        stage_attempt_id: stage_attempt_id.map(String::from),
        action_digest: digest.into(),
        risk: risk.as_str().into(),
        status: "requested".into(),
        requested_by: "local".into(),
        expires_at: expires,
        reason: reason.into(),
        created_at: now,
    })
}

/// 决定审批；只能在 requested 状态。
pub fn decide(
    store: &Store,
    approval_id: &str,
    decision: &str,
    decided_by: &str,
    reason: &str,
) -> Result<Approval, Error> {
    if !matches!(decision, "approved" | "rejected" | "changes_requested") {
        return Err(Error::Message(
            "decision must be approved|rejected|changes_requested".into(),
        ));
    }
    let now = timefmt::now();
    let updated = store.with_conn(|conn| {
        conn.execute(
            "UPDATE approvals SET status=?1, decided_by=?2, decided_at=?3, reason=?4
             WHERE id=?5 AND status='requested'",
            rusqlite::params![decision, decided_by, now, reason, approval_id],
        )?;
        Ok(conn.changes())
    })?;
    if updated == 0 {
        return Err(Error::Message(format!(
            "approval {approval_id} not in requested state"
        )));
    }
    sg_store::outbox::emit(
        store,
        "approval",
        approval_id,
        &format!("approval.{decision}"),
        serde_json::json!({"by": decided_by}),
    )?;
    get(store, approval_id)
}

const APPROVAL_COLUMNS: &str = "id, subject_type, subject_id, workitem_id, stage_attempt_id, action_digest, risk, status, requested_by, expires_at, reason, created_at";

fn row_approval(r: &rusqlite::Row<'_>) -> rusqlite::Result<Approval> {
    Ok(Approval {
        id: r.get(0)?,
        subject_type: r.get(1)?,
        subject_id: r.get(2)?,
        workitem_id: r.get(3)?,
        stage_attempt_id: r.get(4)?,
        action_digest: r.get(5)?,
        risk: r.get(6)?,
        status: r.get(7)?,
        requested_by: r.get(8)?,
        expires_at: r.get(9)?,
        reason: r.get(10)?,
        created_at: r.get(11)?,
    })
}

pub fn get(store: &Store, approval_id: &str) -> Result<Approval, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            &format!("SELECT {APPROVAL_COLUMNS} FROM approvals WHERE id = ?1"),
            [approval_id],
            row_approval,
        )
        .map_err(|_| Error::Message("approval_not_found".into()))
    })
}

/// 单条审批主动作废（放行 digest 漂移等）。
pub fn expire(store: &Store, approval_id: &str, reason: &str) -> Result<(), Error> {
    let updated = store.with_conn(|conn| {
        conn.execute(
            "UPDATE approvals SET status='expired', reason=?1 WHERE id=?2 AND status='requested'",
            rusqlite::params![reason, approval_id],
        )?;
        Ok(conn.changes())
    })?;
    if updated == 0 {
        return Err(Error::Message(format!(
            "approval {approval_id} not in requested state"
        )));
    }
    Ok(())
}

/// 指定任务的未决"风险类"审批计数（AC-SW-05）：gate_release/rollback 不阻塞门禁评估；
/// legacy 行 workitem_id 为 NULL 视为全局（保守 fail-closed）。
pub fn pending_blocking_count(store: &Store, workitem_id: &str) -> Result<i64, Error> {
    store.with_conn(|conn| {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM approvals
             WHERE status='requested'
               AND (workitem_id = ?1 OR workitem_id IS NULL)
               AND subject_type NOT IN ('gate_release','rollback')",
                [workitem_id],
                |r| r.get(0),
            )
            .map_err(Error::from)?;
        Ok(count)
    })
}

/// 过期未决自动失效。
pub fn expire_stale(store: &Store) -> Result<(), Error> {
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE approvals SET status='expired' WHERE status='requested' AND expires_at != '' AND expires_at < ?1",
            [timefmt::now()],
        )?;
        Ok(())
    })
}

/// 校验动作当前是否有绑定同一 digest 的有效批准。
pub fn validate_for(
    store: &Store,
    subject_type: &str,
    subject_id: &str,
    digest: &str,
) -> Result<(), PolicyError> {
    expire_stale(store).map_err(|e| PolicyError::ActionDenied(e.to_string()))?;
    let row: Option<(String, String)> = store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT action_digest, expires_at FROM approvals
                 WHERE subject_type=?1 AND subject_id=?2 AND status='approved'
                 ORDER BY decided_at DESC LIMIT 1",
                [subject_type, subject_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| Error::Message("none".into()))
        })
        .ok();
    match row {
        None => Err(PolicyError::ApprovalRequired),
        Some((approved_digest, expires_at)) => {
            if approved_digest != digest {
                return Err(PolicyError::ApprovalInvalid);
            }
            if !expires_at.is_empty() && expires_at < timefmt::now() {
                return Err(PolicyError::ApprovalExpired);
            }
            Ok(())
        }
    }
}

pub fn pending(store: &Store, limit: i64) -> Result<Vec<Approval>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {APPROVAL_COLUMNS}
             FROM approvals WHERE status='requested' ORDER BY created_at LIMIT ?1"
        ))?;
        let rows = stmt.query_map([limit], row_approval)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

pub fn list_by_subject(
    store: &Store,
    subject_type: &str,
    subject_id: &str,
) -> Result<Vec<Approval>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {APPROVAL_COLUMNS}
             FROM approvals WHERE subject_type=?1 AND subject_id=?2 ORDER BY created_at DESC"
        ))?;
        let rows = stmt.query_map([subject_type, subject_id], row_approval)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> sg_store::Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-policy-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        sg_store::Store::open(&dir, "test").unwrap()
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            tool_rules: vec![
                ToolRule {
                    tool: "read_file".into(),
                    risk: Risk::Low,
                    requires_approval: false,
                    data_level: "internal".into(),
                    max_result_bytes: 1024,
                    timeout_sec: 30,
                },
                ToolRule {
                    tool: "run_command".into(),
                    risk: Risk::High,
                    requires_approval: true,
                    data_level: "internal".into(),
                    max_result_bytes: 1024,
                    timeout_sec: 30,
                },
            ],
            approval_ttl_secs: 3600,
        }
    }

    #[test]
    fn evaluate_allowlist_and_risk() {
        let snap = snapshot();
        assert!(evaluate(&snap, "rm_rf", "d").is_err());
        assert!(evaluate(&snap, "read_file", "d").is_ok());
        assert_eq!(
            evaluate(&snap, "run_command", "d").unwrap_err(),
            PolicyError::ApprovalRequired
        );
        assert!(evaluate(&snap, "read_file", "").is_err());
    }

    #[test]
    fn digest_changes_with_arguments() {
        let d1 = action_digest(&json!({"tool": "deploy", "target": "a"}));
        let d2 = action_digest(&json!({"target": "a", "tool": "deploy"}));
        assert_eq!(d1, d2, "键排序保证规范化稳定");
        let d3 = action_digest(&json!({"tool": "deploy", "target": "b"}));
        assert_ne!(d1, d3);
    }

    #[test]
    fn approval_lifecycle_digest_binding() {
        let s = store();
        let digest = action_digest(&json!({"tool": "deploy"}));
        let appr = request_approval(
            &s,
            "deployment",
            "dp_1",
            &digest,
            Risk::High,
            "首次",
            60,
            None,
            None,
        )
        .unwrap();
        assert!(matches!(
            validate_for(&s, "deployment", "dp_1", &digest),
            Err(PolicyError::ApprovalRequired)
        ));
        decide(&s, &appr.id, "approved", "owner", "ok").unwrap();
        validate_for(&s, "deployment", "dp_1", &digest).unwrap();
        let other = action_digest(&json!({"tool": "deploy", "x": 1}));
        assert_eq!(
            validate_for(&s, "deployment", "dp_1", &other).unwrap_err(),
            PolicyError::ApprovalInvalid
        );
        assert!(
            decide(&s, &appr.id, "rejected", "o", "").is_err(),
            "双重决定必须拒绝"
        );
    }
}
