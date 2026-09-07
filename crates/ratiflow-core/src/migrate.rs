//! v2 → v3 数据迁移器（技术方案 §10）：
//! 备份原目录 → 只读复制 v2 SQLite + objects → 在新目录应用 v3 迁移 → 校验行数。
use std::path::Path;

use serde_json::json;
use sg_store::{Error, Store};

pub fn migrate_v2(from: &str, to: &str, version: &str) -> Result<serde_json::Value, Error> {
    let from_dir = Path::new(from);
    let to_dir = Path::new(to);
    if !from_dir.join("ratiflow.db").exists() {
        return Err(Error::Message(format!(
            "v2 数据库不存在：{}/ratiflow.db",
            from_dir.display()
        )));
    }
    if to_dir.exists() {
        return Err(Error::Message(
            "目标目录已存在（保持可回退原则，不覆盖）".into(),
        ));
    }
    std::fs::create_dir_all(to_dir)?;

    // 只读复制（原目录保持不动 → 可回退）。
    std::fs::copy(from_dir.join("ratiflow.db"), to_dir.join("ratiflow.db"))?;
    if from_dir.join("objects").exists() {
        copy_dir(&from_dir.join("objects"), &to_dir.join("objects"))?;
    }
    if from_dir.join("docs").exists() {
        copy_dir(&from_dir.join("docs"), &to_dir.join("docs"))?;
    }

    // v2 行数（迁移前）。
    let before = count_rows(from_dir)?;

    // 打开新目录：迁移器自动接管 v2 schema 并推进到 v3。
    let store = Store::open(to_dir, version)?;
    store.quick_check()?;
    let after = count_rows(to_dir)?;

    Ok(json!({
        "from": from,
        "to": to,
        "schemaVersion": store.schema_version()?,
        "rowCounts": {"before": before, "after": after},
        "verified": before == after,
        "rollback": "原目录未改动；删除新目录即可回退",
    }))
}

fn count_rows(dir: &Path) -> Result<serde_json::Value, Error> {
    let conn = rusqlite::Connection::open(dir.join("ratiflow.db"))?;
    let count = |table: &str| -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap_or(0)
    };
    Ok(json!({
        "projects": count("projects"),
        "workitems": count("workitems"),
        "artifacts": count("artifacts"),
        "revisions": count("revisions"),
        "evidences": count("evidences"),
        "passports": count("passports"),
    }))
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
