//! app_settings：scope 合并（managed > project > global）、revision 乐观锁。
pub mod summary;
use serde::Serialize;
use serde_json::Value;

use crate::{codes, store_err, SettingsError, SettingsResult};
use sg_store::{timefmt, Store};

#[derive(Debug, Clone, Serialize)]
pub struct SettingEntry {
    pub key: String,
    pub value: Value,
    pub source: String, // global | project | managed
    pub scope: String,
    pub revision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

pub fn get(
    store: &Store,
    scope: &str,
    project_id: Option<&str>,
    keys: Option<&[String]>,
) -> SettingsResult<Vec<SettingEntry>> {
    let project = project_id.unwrap_or("");
    let rows: Vec<(String, String, String, Value, i64)> = store
        .with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT key, scope, project_id, value_json, revision FROM app_settings
             WHERE (?1 = '' OR key = ?1) AND (?2 = '' OR key = ?2)
               AND scope = ?3 AND (?4 = '' OR project_id IN ('', ?4))
             ORDER BY key",
            )?;
            let key1 = keys.and_then(|k| k.first()).cloned().unwrap_or_default();
            let key2 = keys.and_then(|k| k.get(1)).cloned().unwrap_or_default();
            let rows = stmt.query_map(rusqlite::params![key1, key2, scope, project], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    serde_json::from_str::<Value>(&r.get::<_, String>(3)?).unwrap_or(Value::Null),
                    r.get::<_, i64>(4)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .map_err(store_err)?;
    Ok(rows
        .into_iter()
        .filter(|(key, _, _, _, _)| keys.map(|ks| ks.iter().any(|k| k == key)).unwrap_or(true))
        .map(
            |(key, row_scope, row_project, value, revision)| SettingEntry {
                key,
                value,
                source: if row_project.is_empty() {
                    "global".into()
                } else {
                    "project".into()
                },
                scope: row_scope,
                revision,
                project_id: if row_project.is_empty() {
                    None
                } else {
                    Some(row_project)
                },
            },
        )
        .collect())
}

pub struct Patch {
    pub key: String,
    pub value: Value,
    pub expected_revision: Option<i64>,
}

/// patch 非秘密设置；expectedRevision 不匹配返回 REVISION_CONFLICT（不覆盖）。
/// 更新成功：revision+1，写审计 + settings.changed 事件由调用方（dispatch）完成。
pub fn update(
    store: &Store,
    scope: &str,
    project_id: Option<&str>,
    patches: &[Patch],
    updated_by: &str,
) -> SettingsResult<Vec<SettingEntry>> {
    let project = project_id.unwrap_or("");
    let now = timefmt::now();
    for patch in patches {
        // managed 键以 settings_managed_imports 标记存在时只读。
        let managed: Option<String> = store.with_conn(|conn| {
            let result: rusqlite::Result<String> = conn.query_row(
                "SELECT resource_id FROM settings_managed_imports WHERE resource_type='setting' AND resource_id=?1",
                [&patch.key],
                |r| r.get(0),
            );
            Ok(result.ok())
        }).map_err(store_err)?;
        if managed.is_some() {
            return Err(Box::new(SettingsError::new(
                codes::MANAGED_READ_ONLY,
                format!("键 {} 由托管来源管理", patch.key),
            ));
        }

        let current: Option<(i64, Value)> = store.with_conn(|conn| {
            let result: rusqlite::Result<(i64, String)> = conn.query_row(
                "SELECT revision, value_json FROM app_settings WHERE scope=?1 AND project_id=?2 AND key=?3",
                rusqlite::params![scope, project, patch.key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            );
            Ok(result.ok().map(|(rev, body)| (rev, serde_json::from_str(&body).unwrap_or(Value::Null))))
        }).map_err(store_err)?;

        match current {
            None => {
                store.with_conn(|conn| {
                    conn.execute(
                        "INSERT INTO app_settings(key, scope, project_id, value_json, revision, updated_at, updated_by)
                         VALUES (?1,?2,?3,?4,1,?5,?6)",
                        rusqlite::params![patch.key, scope, project, patch.value.to_string(), now, updated_by],
                    )?;
                    Ok(())
                }).map_err(store_err)?;
            }
            Some((revision, _)) => {
                if let Some(expected) = patch.expected_revision {
                    if expected != revision {
                        return Err(Box::new(SettingsError::new(codes::REVISION_CONFLICT, format!("键 {} 期望 revision {} 实际 {}", patch.key, expected, revision))
                            .with_details(serde_json::json!({"key": patch.key, "expectedRevision": expected, "actualRevision": revision})));
                    }
                }
                store.with_conn(|conn| {
                    conn.execute(
                        "UPDATE app_settings SET value_json=?1, revision=revision+1, updated_at=?2, updated_by=?3
                         WHERE scope=?4 AND project_id=?5 AND key=?6",
                        rusqlite::params![patch.value.to_string(), now, updated_by, scope, project, patch.key],
                    )?;
                    Ok(())
                }).map_err(store_err)?;
            }
        }
    }
    let keys: Vec<String> = patches.iter().map(|p| p.key.clone()).collect();
    get(store, scope, project_id, Some(&keys))
}

/// 合成结果：project 覆盖 global；managed（env）标记 source=managed。
pub fn effective(
    store: &Store,
    project_id: Option<&str>,
    keys: Option<&[String]>,
) -> SettingsResult<Vec<SettingEntry>> {
    let mut merged: Vec<SettingEntry> = get(store, "global", None, keys)?;
    if let Some(pid) = project_id {
        if !pid.is_empty() {
            for entry in get(store, "global", Some(pid), keys)? {
                merged.retain(|m| m.key != entry.key);
                merged.push(entry);
            }
        }
    }
    Ok(merged)
}
