//! 结构化审计（0015 扩列）+ 脱敏导出。
use serde::Serialize;
use serde_json::{json, Value};

use crate::{store_err, SettingsResult};
use sg_store::{scan, Store};

#[derive(Debug, Clone, Serialize)]
pub struct AuditEntryExt {
    pub seq: i64,
    pub actor: String,
    pub actor_kind: String,
    pub action: String,
    pub target_type: String,
    pub target_id: String,
    pub result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_summary: Option<String>,
    pub metadata_redacted: bool,
    pub created_at: String,
}

/// 结构化审计事件（append 的输入）。
pub struct AuditEvent<'a> {
    pub actor: &'a str,
    pub actor_kind: &'a str,
    pub action: &'a str,
    pub target_type: &'a str,
    pub target_id: &'a str,
    pub result: &'a str,
    pub correlation_id: Option<&'a str>,
    pub project_id: Option<&'a str>,
    pub before_summary: Option<&'a Value>,
    pub after_summary: Option<&'a Value>,
}

/// 追加写（metadata 自动脱敏；审计不可经普通 API 修改/删除）。
pub fn append(store: &Store, event: &AuditEvent<'_>) -> SettingsResult<i64> {
    let AuditEvent {
        actor,
        actor_kind,
        action,
        target_type,
        target_id,
        result,
        correlation_id,
        project_id,
        before_summary,
        after_summary,
    } = *event;
    let redact = |v: Option<&Value>| -> Option<String> {
        v.map(|x| {
            let body = x.to_string();
            let (masked, _) = scan::mask(body.as_bytes());
            masked
        })
    };
    let before = redact(before_summary);
    let after = redact(after_summary);
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO audit_log(actor, actor_kind, action, target_type, target_id, detail, result,
                correlation_id, project_id, before_summary, after_summary, metadata_redacted, created_at)
             VALUES (?1,?2,?3,?4,?5,'{}',?6,?7,?8,?9,?10,1,?11)",
            rusqlite::params![actor, actor_kind, action, target_type, target_id, result,
                correlation_id, project_id, before, after, sg_store::timefmt::now()],
        )?;
        Ok(conn.last_insert_rowid())
    })
    .map_err(store_err)
}

fn row_to_entry(r: &rusqlite::Row<'_>) -> rusqlite::Result<AuditEntryExt> {
    let correlation: Option<String> = r.get(7)?;
    let project: Option<String> = r.get(8)?;
    let before: Option<String> = r.get(9)?;
    let after: Option<String> = r.get(10)?;
    Ok(AuditEntryExt {
        seq: r.get(0)?,
        actor: r.get(1)?,
        actor_kind: r.get(2)?,
        action: r.get(3)?,
        target_type: r.get(4)?,
        target_id: r.get(5)?,
        result: r.get(6)?,
        correlation_id: correlation,
        project_id: project,
        before_summary: before,
        after_summary: after,
        metadata_redacted: r.get::<_, i64>(11)? == 1,
        created_at: r.get(12)?,
    })
}

const SELECT: &str = "SELECT seq, actor, actor_kind, action, target_type, target_id, result,
       correlation_id, project_id, before_summary, after_summary, metadata_redacted, created_at
       FROM audit_log";

pub fn get(store: &Store, seq: i64) -> SettingsResult<AuditEntryExt> {
    let sql = format!("{SELECT} WHERE seq=?1");
    let row: Option<AuditEntryExt> = store
        .with_conn(|conn| Ok(conn.query_row(&sql, [seq], row_to_entry).ok()))
        .map_err(store_err)?;
    row.ok_or_else(|| crate::SettingsError::new("NOT_FOUND", format!("审计 #{seq} 不存在")))
}

pub fn list(
    store: &Store,
    filters: &Value,
    after_seq: i64,
    limit: i64,
) -> SettingsResult<Vec<AuditEntryExt>> {
    let action = filters.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let target_type = filters
        .get("targetType")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let sql = format!("{SELECT} WHERE (?1=0 OR seq<?1) AND (?2='' OR action=?2) AND (?3='' OR target_type=?3) ORDER BY seq DESC LIMIT ?4");
    store
        .with_conn(|conn| {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params![after_seq, action, target_type, limit],
                row_to_entry,
            )?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .map_err(store_err)
}

/// 导出（默认脱敏已由 append 保证；导出再扫一遍防旧数据）。
pub fn export(store: &Store, filters: &Value, limit: i64) -> SettingsResult<Value> {
    let entries = list(store, filters, 0, limit)?;
    let redacted: Vec<Value> = entries
        .iter()
        .map(|e| {
            json!({
                "seq": e.seq, "actor": e.actor, "actorKind": e.actor_kind, "action": e.action,
                "targetType": e.target_type, "targetId": e.target_id, "result": e.result,
                "correlationId": e.correlation_id, "metadataRedacted": true,
                "createdAt": e.created_at,
            })
        })
        .collect();
    let body = serde_json::to_string(&redacted).unwrap_or_default();
    let (masked, _) = scan::mask(body.as_bytes());
    serde_json::from_str::<Value>(&masked)
        .map_err(|e| crate::SettingsError::new("INTERNAL", e.to_string()))
}
