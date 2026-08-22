//! outbox：领域事件持久化 + SSE/RPC notification 回放基础。
use serde_json::{json, Value};

use crate::{ids, timefmt, Error, Store};

/// 写入事件并返回全局 sequence。
pub fn emit(
    store: &Store,
    aggregate_type: &str,
    aggregate_id: &str,
    event_type: &str,
    payload: Value,
) -> Result<i64, Error> {
    let now = timefmt::now();
    let _ = ids::new_id("ev");
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO events_outbox(aggregate_type, aggregate_id, type, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                aggregate_type,
                aggregate_id,
                event_type,
                payload.to_string(),
                now
            ],
        )?;
        Ok(conn.last_insert_rowid())
    })
}

/// sequence > after_seq 的事件（Last-Event-ID / timeline 补发）。
pub fn replay(store: &Store, after_seq: i64, limit: i64) -> Result<Vec<Value>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT sequence, aggregate_type, aggregate_id, type, payload, created_at
             FROM events_outbox WHERE sequence > ?1 ORDER BY sequence LIMIT ?2",
        )?;
        let rows = stmt.query_map([after_seq, limit], |r| {
            Ok(json!({
                "sequence": r.get::<_, i64>(0)?,
                "aggregateType": r.get::<_, String>(1)?,
                "aggregateId": r.get::<_, String>(2)?,
                "type": r.get::<_, String>(3)?,
                "payload": serde_json::from_str::<Value>(&r.get::<_, String>(4)?).unwrap_or(Value::Null),
                "occurredAt": r.get::<_, String>(5)?,
            }))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

/// 最大 sequence。
pub fn latest_sequence(store: &Store) -> Result<i64, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT COALESCE(MAX(sequence),0) FROM events_outbox",
            [],
            |r| r.get(0),
        )
        .map_err(Error::from)
    })
}
