//! 知识默认设置（global + 项目覆盖，revision 乐观锁）。
use serde_json::Value;

use crate::{codes, store_err, SettingsError, SettingsResult};
use sg_store::{timefmt, Store};

pub fn get(store: &Store, project_id: Option<&str>) -> SettingsResult<Value> {
    let project = project_id.unwrap_or("");
    let global: Option<String> = store.with_conn(|conn| {
        let result: rusqlite::Result<String> = conn.query_row(
            "SELECT settings_json FROM knowledge_default_settings WHERE scope='global' AND project_id=''",
            [],
            |r| r.get(0),
        );
        Ok(result.ok())
    }).map_err(store_err)?;
    let mut merged =
        serde_json::from_str::<Value>(&global.unwrap_or_else(|| "{}".into())).unwrap_or_default();
    if !project.is_empty() {
        let override_row: Option<String> = store.with_conn(|conn| {
            let result: rusqlite::Result<String> = conn.query_row(
                "SELECT settings_json FROM knowledge_default_settings WHERE scope='project' AND project_id=?1",
                [project],
                |r| r.get(0),
            );
            Ok(result.ok())
        }).map_err(store_err)?;
        if let Some(body) = override_row {
            if let (Some(base), Some(over)) = (
                merged.as_object_mut(),
                serde_json::from_str::<Value>(&body)
                    .unwrap_or_default()
                    .as_object(),
            ) {
                for (k, v) in over {
                    base.insert(k.clone(), v.clone());
                }
            }
        }
    }
    Ok(merged)
}

pub fn revision_of(store: &Store, scope: &str, project_id: &str) -> SettingsResult<i64> {
    let rev: Option<i64> = store
        .with_conn(|conn| {
            let result: rusqlite::Result<i64> = conn.query_row(
                "SELECT revision FROM knowledge_default_settings WHERE scope=?1 AND project_id=?2",
                [scope, project_id],
                |r| r.get(0),
            );
            Ok(result.ok())
        })
        .map_err(store_err)?;
    Ok(rev.unwrap_or(0))
}

pub fn update(
    store: &Store,
    project_id: Option<&str>,
    settings: &Value,
    expected_revision: i64,
) -> SettingsResult<i64> {
    let (scope, project) = match project_id.filter(|p| !p.is_empty()) {
        Some(p) => ("project", p),
        None => ("global", ""),
    };
    let current = revision_of(store, scope, project)?;
    let now = timefmt::now();
    if current == 0 {
        store.with_conn(|conn| {
            conn.execute(
                "INSERT INTO knowledge_default_settings(scope, project_id, settings_json, revision, updated_at)
                 VALUES (?1,?2,?3,1,?4)",
                rusqlite::params![scope, project, settings.to_string(), now],
            )?;
            Ok(())
        }).map_err(store_err)?;
        return Ok(1);
    }
    if current != expected_revision {
        return Err(SettingsError::new(
            codes::REVISION_CONFLICT,
            format!("知识设置期望 revision {expected_revision} 实际 {current}"),
        ));
    }
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE knowledge_default_settings SET settings_json=?1, revision=revision+1, updated_at=?2
             WHERE scope=?3 AND project_id=?4",
            rusqlite::params![settings.to_string(), now, scope, project],
        )?;
        Ok(())
    }).map_err(store_err)?;
    Ok(current + 1)
}
