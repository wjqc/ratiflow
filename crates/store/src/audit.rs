//! 追加式审计：只允许插入与查询。
use serde_json::{json, Value};

use crate::{Error, Store};

pub fn append(
    store: &Store,
    actor: &str,
    action: &str,
    target_type: &str,
    target_id: &str,
    detail: Value,
) -> Result<i64, Error> {
    if actor.is_empty() || action.is_empty() || target_type.is_empty() || target_id.is_empty() {
        return Err(Error::Message(
            "audit entry requires actor/action/target".into(),
        ));
    }
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO audit_log(actor, action, target_type, target_id, detail, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                actor,
                action,
                target_type,
                target_id,
                detail.to_string(),
                crate::timefmt::now()
            ],
        )?;
        Ok(conn.last_insert_rowid())
    })
}

/// seq 降序分页（after_seq 为上一页最小 seq，0 表示第一页）。
pub fn list(store: &Store, after_seq: i64, limit: i64) -> Result<Vec<Value>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT seq, actor, action, target_type, target_id, detail, created_at
             FROM audit_log WHERE (?1 = 0 OR seq < ?1) ORDER BY seq DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map([after_seq, limit], |r| {
            Ok(json!({
                "seq": r.get::<_, i64>(0)?,
                "actor": r.get::<_, String>(1)?,
                "action": r.get::<_, String>(2)?,
                "targetType": r.get::<_, String>(3)?,
                "targetId": r.get::<_, String>(4)?,
                "detail": serde_json::from_str::<Value>(&r.get::<_, String>(5)?).unwrap_or(Value::Null),
                "createdAt": r.get::<_, String>(6)?,
            }))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}
