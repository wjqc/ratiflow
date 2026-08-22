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
    let name = format!("sixgates-{}.db", crate::timefmt::now().replace(':', ""));
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
    let version = store.schema_version()?;
    let manifest = json!({
        "schemaVersion": version,
        "snapshotSha256": snapshot_hash,
        "objectsCount": objects_count,
        "objectsRootHash": objects_root,
        "sixgatesVersion": store.version,
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
