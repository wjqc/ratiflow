//! 备份记录 + verify（digest/schema/version）+ restore（安全快照→替换→quick_check→要求重启）。
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};

use crate::{codes, store_err, SettingsError, SettingsResult};
use sg_store::{backup, Store};

#[derive(Debug, Clone, Serialize)]
pub struct BackupRecord {
    pub id: String,
    pub path: String,
    pub format_version: i64,
    pub schema_version: i64,
    pub size_bytes: i64,
    pub digest: String,
    pub verified: bool,
    pub problems: Value,
    pub status: String,
    pub manifest: Value,
    pub created_at: String,
    pub updated_at: String,
}

pub fn record(store: &Store, id: &str) -> SettingsResult<BackupRecord> {
    let row: Option<BackupRecord> = store.with_conn(|conn| {
        Ok(conn.query_row(
            "SELECT id, path, format_version, schema_version, size_bytes, digest, verified, problems_json,
                    status, manifest_json, created_at, updated_at FROM backup_records WHERE id=?1",
            [id],
            |r| {
                Ok(BackupRecord {
                    id: r.get(0)?, path: r.get(1)?, format_version: r.get(2)?, schema_version: r.get(3)?,
                    size_bytes: r.get(4)?, digest: r.get(5)?, verified: r.get::<_, i64>(6)? == 1,
                    problems: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default(),
                    status: r.get(8)?,
                    manifest: serde_json::from_str(&r.get::<_, String>(9)?).unwrap_or_default(),
                    created_at: r.get(10)?, updated_at: r.get(11)?,
                })
            },
        ).ok())
    }).map_err(store_err)?;
    row.ok_or_else(|| Box::new(SettingsError::new("NOT_FOUND", format!("备份 {id} 不存在"))))
}

pub fn list(store: &Store) -> SettingsResult<Vec<BackupRecord>> {
    let ids: Vec<String> = store
        .with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT id FROM backup_records ORDER BY created_at DESC")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .map_err(store_err)?;
    ids.iter().map(|id| record(store, id)).collect()
}

/// 登记 backup::snapshot 产物为记录。
pub fn register(
    store: &Store,
    snap: &backup::Snapshot,
    format_version: i64,
) -> SettingsResult<BackupRecord> {
    let id = sg_store::ids::new_id("bk");
    let meta = std::fs::metadata(&snap.path)
        .map_err(|e| store_err(sg_store::Error::Message(e.to_string())))?;
    let schema = store.schema_version().map_err(store_err)?;
    let digest = snap.manifest["snapshotSha256"]
        .as_str()
        .unwrap_or("")
        .to_string();
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO backup_records(id, path, format_version, schema_version, size_bytes, digest, verified,
                problems_json, status, manifest_json, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,0,'[]','created',?7,?8,?8)",
            rusqlite::params![id, snap.path, format_version, schema, meta.len() as i64, digest,
                snap.manifest.to_string(), sg_store::timefmt::now()],
        )?;
        Ok(())
    }).map_err(store_err)?;
    record(store, &id)
}

/// verify：digest 重算 + schema/version 兼容检查（不兼容禁止恢复）。
pub fn verify(store: &Store, id: &str) -> SettingsResult<BackupRecord> {
    let rec = record(store, id)?;
    store
        .with_conn(|conn| {
            conn.execute(
                "UPDATE backup_records SET status='verifying', updated_at=?1 WHERE id=?2",
                rusqlite::params![sg_store::timefmt::now(), id],
            )?;
            Ok(())
        })
        .map_err(store_err)?;

    let mut problems: Vec<Value> = Vec::new();
    let body = match std::fs::read(&rec.path) {
        Ok(b) => b,
        Err(e) => {
            problems.push(json!({"code": codes::BACKUP_CORRUPT, "detail": e.to_string()}));
            Vec::new()
        }
    };
    let actual = hex(&Sha256::digest(&body));
    let mut verified = true;
    if !body.is_empty() {
        if actual != rec.digest {
            problems.push(json!({"code": codes::BACKUP_CORRUPT, "detail": "快照 sha256 不匹配"}));
            verified = false;
        }
        let current_schema = store.schema_version().map_err(store_err)?;
        if rec.schema_version > current_schema {
            problems.push(json!({"code": codes::BACKUP_INCOMPATIBLE,
                "detail": format!("备份 schema {} 高于当前 {}", rec.schema_version, current_schema)}));
            verified = false;
        }
    } else {
        verified = false;
    }

    let status = if verified {
        "verified"
    } else if problems
        .iter()
        .any(|p| p["code"] == json!(codes::BACKUP_INCOMPATIBLE))
    {
        "incompatible"
    } else {
        "corrupt"
    };
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE backup_records SET verified=?1, problems_json=?2, status=?3, updated_at=?4 WHERE id=?5",
            rusqlite::params![verified as i64, serde_json::to_string(&problems).unwrap_or_else(|_| "[]".into()),
                status, sg_store::timefmt::now(), id],
        )?;
        Ok(())
    }).map_err(store_err)?;
    record(store, id)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug)]
pub struct RestoreOutcome {
    pub restored: bool,
    pub requires_restart: bool,
    pub safety_snapshot: String,
}

/// restore：verified 才可执行；先安全快照当前库 → 复制快照覆盖 db（objects 不动，按清单核对）→
/// quick_check → 返回 requiresRestart。失败时从安全快照回滚。
/// 注意：调用方应在恢复后要求应用重启（core 持有旧连接）。
pub fn restore(store: &Store, id: &str) -> SettingsResult<RestoreOutcome> {
    let rec = record(store, id)?;
    if rec.status != "verified" {
        return Err(Box::new(SettingsError::new(
            codes::BACKUP_CORRUPT,
            "仅 verified 备份可恢复；先执行 backup.verify",
        )));
    }
    // 安全快照（当前状态可回滚）。
    let safety = backup::snapshot(store).map_err(store_err)?;

    store
        .with_conn(|conn| {
            conn.execute(
                "UPDATE backup_records SET status='restoring', updated_at=?1 WHERE id=?2",
                rusqlite::params![sg_store::timefmt::now(), id],
            )?;
            Ok(())
        })
        .map_err(store_err)?;

    let db_path = store.data_dir.join("sixgates.db");
    // 先 checkpoint 并截断 WAL，保证磁盘快照自洽。
    let _ = store.with_conn(|conn| Ok(conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?));
    // WAL 文件一并替换，避免旧 WAL 污染。
    for wal in ["sixgates.db-wal", "sixgates.db-shm"] {
        let _ = std::fs::remove_file(store.data_dir.join(wal));
    }
    if let Err(e) = std::fs::copy(&rec.path, &db_path) {
        // 回滚：用安全快照恢复。
        let _ = std::fs::copy(&safety.path, &db_path);
        return Err(Box::new(SettingsError::new("INTERNAL", format!("恢复失败已回滚：{e}"))
            .with_details(json!({"safetySnapshot": safety.path}))));
    }
    // 完整性检查：裸连接 quick_check（不走 Store::open，避免迁移备份再触发写锁）。
    let check_ok = (|| -> Option<bool> {
        let conn = rusqlite::Connection::open(&db_path).ok()?;
        conn.pragma_update(None, "busy_timeout", 3000).ok()?;
        let result: rusqlite::Result<String> =
            conn.query_row("PRAGMA quick_check", [], |r| r.get(0));
        Some(matches!(result, Ok(v) if v.eq_ignore_ascii_case("ok")))
    })()
    .unwrap_or(false);
    if !check_ok {
        let _ = std::fs::copy(&safety.path, &db_path);
        return Err(Box::new(SettingsError::new(
            codes::BACKUP_CORRUPT,
            "恢复后完整性检查失败，已回滚安全快照",
        ).with_details(json!({"safetySnapshot": safety.path}))));
    }

    store
        .with_conn(|conn| {
            conn.execute(
                "UPDATE backup_records SET status='restored', updated_at=?1 WHERE id=?2",
                rusqlite::params![sg_store::timefmt::now(), id],
            )?;
            Ok(())
        })
        .map_err(store_err)?;
    Ok(RestoreOutcome {
        restored: true,
        requires_restart: true,
        safety_snapshot: safety.path,
    })
}

pub fn delete(store: &Store, id: &str) -> SettingsResult<()> {
    let rec = record(store, id)?;
    store
        .with_conn(|conn| {
            conn.execute("DELETE FROM backup_records WHERE id=?1", [id])?;
            Ok(())
        })
        .map_err(store_err)?;
    let _ = std::fs::remove_file(&rec.path);
    let _ = std::fs::remove_file(format!("{}.manifest.json", rec.path));
    Ok(())
}

/// 日志列表 + 诊断 bundle（先脱敏报告再打包引用，不含秘密）。
pub fn logs_list(limit: i64) -> SettingsResult<Value> {
    let log_dir = dirs_log_dir();
    let mut items = Vec::new();
    if let Some(dir) = &log_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let meta = entry.metadata().ok();
                items.push(json!({
                    "name": entry.file_name().to_string_lossy(),
                    "sizeBytes": meta.as_ref().map(|m| m.len()).unwrap_or(0),
                    "path": entry.path().to_string_lossy(),
                }));
            }
        }
    }
    items.truncate(limit.max(1) as usize);
    Ok(json!({"items": items}))
}

pub fn export_diagnostic_bundle(store: &Store) -> SettingsResult<Value> {
    let logs = logs_list(100)?;
    let recent_audit = crate::audit_ext::list(store, &json!({}), 0, 50).unwrap_or_default();
    let summary = json!({
        "schemaVersion": store.schema_version().map_err(store_err)?,
        "generatedAt": sg_store::timefmt::now(),
        "coreVersion": store.version,
        "logs": logs["items"],
        "recentAudit": recent_audit.iter().map(|e| json!({
            "seq": e.seq, "action": e.action, "result": e.result, "createdAt": e.created_at,
        })).collect::<Vec<_>>(),
        "redactionReport": {"applied": true, "note": "审计与日志引用已脱敏；明文秘密不入 bundle"},
    });
    // 全文脱敏一遍（含日志文件名等不可控字段）。
    let body = summary.to_string();
    let (masked, _) = sg_store::scan::mask(body.as_bytes());
    serde_json::from_str::<Value>(&masked)
        .map_err(|e| Box::new(SettingsError::new("INTERNAL", e.to_string())))
}

fn dirs_log_dir() -> Option<std::path::PathBuf> {
    std::env::var("SIXGATES_LOG_DIR")
        .ok()
        .map(std::path::PathBuf::from)
}
