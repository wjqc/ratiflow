//! 长操作记录（operation.get 恢复 + operation.progress 事件）。
use serde::Serialize;
use serde_json::Value;

use crate::{store_err, SettingsResult};
use sg_store::{ids, outbox, timefmt, Store};

#[derive(Debug, Clone, Serialize)]
pub struct Operation {
    pub operation_id: String,
    pub kind: String,
    pub status: String,
    pub progress: Value,
    pub cancellable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    pub started_at: String,
    pub updated_at: String,
}

pub fn begin(store: &Store, kind: &str, cancellable: bool) -> SettingsResult<Operation> {
    let id = ids::new_id("op");
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO operations(id, kind, status, progress_json, cancellable, started_at, updated_at)
             VALUES (?1,?2,'running','{}',?3,?4,?4)",
            rusqlite::params![id, kind, cancellable as i64, now],
        )?;
        Ok(())
    }).map_err(crate::store_err)?;
    Ok(Operation {
        operation_id: id,
        kind: kind.into(),
        status: "running".into(),
        progress: serde_json::json!({"completed": 0, "total": 0}),
        cancellable,
        result: None,
        started_at: now.clone(),
        updated_at: now,
    })
}

pub fn progress(
    store: &Store,
    id: &str,
    completed: i64,
    total: i64,
    label_key: &str,
) -> SettingsResult<()> {
    let now = timefmt::now();
    let progress =
        serde_json::json!({"completed": completed, "total": total, "labelKey": label_key});
    store
        .with_conn(|conn| {
            conn.execute(
                "UPDATE operations SET progress_json=?1, updated_at=?2 WHERE id=?3",
                rusqlite::params![progress.to_string(), now, id],
            )?;
            Ok(())
        })
        .map_err(store_err)?;
    outbox::emit(store, "operation", id, "operation.progress",
        serde_json::json!({"operationId": id, "completed": completed, "total": total, "labelKey": label_key})).map_err(store_err)?;
    Ok(())
}

pub fn finish(store: &Store, id: &str, status: &str, result: Value) -> SettingsResult<()> {
    if !matches!(status, "succeeded" | "failed" | "cancelled") {
        return Err(crate::SettingsError::new("INVALID_PARAMS", "终态非法"));
    }
    let now = timefmt::now();
    store
        .with_conn(|conn| {
            conn.execute(
                "UPDATE operations SET status=?1, result_json=?2, updated_at=?3 WHERE id=?4",
                rusqlite::params![status, result.to_string(), now, id],
            )?;
            Ok(())
        })
        .map_err(store_err)?;
    Ok(())
}

pub fn get(store: &Store, id: &str) -> SettingsResult<Operation> {
    let row: Option<Operation> = store.with_conn(|conn| {
        Ok(conn.query_row(
            "SELECT id, kind, status, progress_json, cancellable, COALESCE(result_json,''), started_at, updated_at
             FROM operations WHERE id=?1",
            [id],
            |r| {
                Ok(Operation {
                    operation_id: r.get(0)?, kind: r.get(1)?, status: r.get(2)?,
                    progress: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
                    cancellable: r.get::<_, i64>(4)? == 1,
                    result: serde_json::from_str::<Value>(&r.get::<_, String>(5)?).ok(),
                    started_at: r.get(6)?, updated_at: r.get(7)?,
                })
            },
        ).ok())
    }).map_err(store_err)?;
    row.ok_or_else(|| crate::SettingsError::new("NOT_FOUND", format!("操作 {id} 不存在")))
}
