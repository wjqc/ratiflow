//! 在线备份：VACUUM INTO 一致快照 + 清单（schema 版本、快照哈希、objects 根哈希）。
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{Error, Store};

pub struct Snapshot {
    pub path: String,
    pub manifest: serde_json::Value,
}

pub fn snapshot(store: &Store) -> Result<Snapshot, Error> {
    let backups = store.data_dir.join("backups");
    std::fs::create_dir_all(&backups)?;
    let name = format!("ratiflow-{}.db", crate::timefmt::now().replace(':', ""));
    let target = backups.join(&name);
    if target.exists() {
        return Err(Error::Message("backup target exists".into()));
    }
    store.with_conn(|conn| {
        conn.execute("VACUUM INTO ?1", [target.to_string_lossy().as_ref()])?;
        Ok(())
    })?;

    let body = std::fs::read(&target)?;
    let snapshot_hash = crate::ids::hex(&Sha256::digest(&body));
    let (objects_root, objects_count) = objects_root_hash(store)?;
    // M1/F04：rollout 会话日志随备份走——拷贝 logs/runs/*.jsonl 到 <name>.rollouts/ 并记根哈希。
    let (rollouts_count, rollouts_root) = bundle_rollouts(store, &target)?;
    // 项目记忆（ADR-032）：manifest 记录记忆条目数与带正文对象数，供恢复后对账。
    let (memory_entries, memory_objects) = memory_stats(store)?;
    let version = store.schema_version()?;
    let manifest = json!({
        "schemaVersion": version,
        "snapshotSha256": snapshot_hash,
        "objectsCount": objects_count,
        "objectsRootHash": objects_root,
        "rolloutsCount": rollouts_count,
        "rolloutsRootHash": rollouts_root,
        "memoryEntries": memory_entries,
        "memoryObjects": memory_objects,
        "ratiflowVersion": store.version,
        "createdAt": crate::timefmt::now(),
    });
    let manifest_path = backups.join(format!("{name}.manifest.json"));
    let manifest_body =
        serde_json::to_vec_pretty(&manifest).map_err(|e| Error::Message(e.to_string()))?;
    std::fs::write(&manifest_path, manifest_body)?;
    Ok(Snapshot {
        path: target.to_string_lossy().to_string(),
        manifest,
    })
}

/// objects 表规范化根哈希（排序后逐行拼接再哈希）。
fn objects_root_hash(store: &Store) -> Result<(String, i64), Error> {
    store.with_conn(|conn| {
        let mut stmt =
            conn.prepare("SELECT sha256, size, content_type FROM objects ORDER BY sha256")?;
        let rows = stmt.query_map([], |r| {
            Ok(format!(
                "{} {} {}\n",
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?
            ))
        })?;
        let mut hasher = Sha256::new();
        let mut count = 0i64;
        for row in rows {
            hasher.update(row?.as_bytes());
            count += 1;
        }
        Ok((crate::ids::hex(&hasher.finalize()), count))
    })
}

/// 项目记忆统计：条目数与带正文对象的修订数（表不存在时按 0 计，兼容旧库）。
fn memory_stats(store: &Store) -> Result<(i64, i64), Error> {
    store.with_conn(|conn| {
        let has_table: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='memory_entries'",
            [],
            |r| r.get(0),
        )?;
        if has_table == 0 {
            return Ok((0, 0));
        }
        let entries: i64 =
            conn.query_row("SELECT COUNT(*) FROM memory_entries", [], |r| r.get(0))?;
        let objects: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_revisions WHERE object_sha256 IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        Ok((entries, objects))
    })
}

/// rollout 捆绑：拷贝 `logs/runs/*.jsonl` 到 `<db 快照路径去 .db>.rollouts/`，
/// 返回 (文件数, 规范化根哈希)——按文件名排序，逐行拼接 "{name} {sha256} {size}\n" 再哈希（与 objects 同风格）。
pub fn bundle_rollouts(store: &Store, db_target: &std::path::Path) -> Result<(i64, String), Error> {
    let stem = db_target
        .file_stem()
        .ok_or_else(|| Error::Message("backup target invalid".into()))?
        .to_string_lossy()
        .to_string();
    let bundle_dir = db_target
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(format!("{stem}.rollouts"));
    std::fs::create_dir_all(&bundle_dir)?;
    let runs_dir = store.data_dir.join("logs").join("runs");
    let mut names: Vec<String> = std::fs::read_dir(&runs_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().map(|x| x == "jsonl").unwrap_or(false))
                .filter_map(|e| e.file_name().to_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    let mut hasher = Sha256::new();
    for name in &names {
        let body = std::fs::read(runs_dir.join(name))?;
        hasher.update(
            format!(
                "{} {} {}\n",
                name,
                crate::ids::hex(&Sha256::digest(&body)),
                body.len()
            )
            .as_bytes(),
        );
        std::fs::write(bundle_dir.join(name), &body)?;
    }
    Ok((names.len() as i64, crate::ids::hex(&hasher.finalize())))
}

/// 校验备份的 rollouts 捆绑（verify 用）：重算根哈希与 manifest 比对。
pub fn verify_rollouts(
    db_target: &std::path::Path,
    expected_count: i64,
    expected_root: &str,
) -> Result<bool, Error> {
    let stem = db_target
        .file_stem()
        .ok_or_else(|| Error::Message("backup target invalid".into()))?
        .to_string_lossy()
        .to_string();
    let bundle_dir = db_target
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(format!("{stem}.rollouts"));
    let mut names: Vec<String> = std::fs::read_dir(&bundle_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().map(|x| x == "jsonl").unwrap_or(false))
                .filter_map(|e| e.file_name().to_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    if names.len() as i64 != expected_count {
        return Ok(false);
    }
    let mut hasher = Sha256::new();
    for name in &names {
        let body = std::fs::read(bundle_dir.join(name))?;
        hasher.update(
            format!(
                "{} {} {}\n",
                name,
                crate::ids::hex(&Sha256::digest(&body)),
                body.len()
            )
            .as_bytes(),
        );
    }
    Ok(crate::ids::hex(&hasher.finalize()) == expected_root)
}

/// 恢复：把备份的 rollouts 拷回 `logs/runs/`（restore 用；文件存在则覆盖）。
pub fn restore_rollouts(store: &Store, db_target: &std::path::Path) -> Result<i64, Error> {
    let stem = db_target
        .file_stem()
        .ok_or_else(|| Error::Message("backup target invalid".into()))?
        .to_string_lossy()
        .to_string();
    let bundle_dir = db_target
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(format!("{stem}.rollouts"));
    if !bundle_dir.is_dir() {
        return Ok(0);
    }
    let runs_dir = store.data_dir.join("logs").join("runs");
    std::fs::create_dir_all(&runs_dir)?;
    let mut count = 0i64;
    for entry in std::fs::read_dir(&bundle_dir)?.flatten() {
        let path = entry.path();
        if path.extension().map(|x| x == "jsonl").unwrap_or(false) {
            std::fs::copy(&path, runs_dir.join(entry.file_name()))?;
            count += 1;
        }
    }
    Ok(count)
}
