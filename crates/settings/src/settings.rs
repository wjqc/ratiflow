//! 简单应用偏好（settings.get/update RPC 的后端）。
//! 存储：`data_dir/settings.json` 单文件（sg_store::prefstore，2026-09-08 起不再落表）；
//! scope 合并（managed > project > global）与 revision 乐观锁语义不变。
pub mod summary;
use serde::Serialize;
use serde_json::Value;

use crate::{codes, store_err, SettingsError, SettingsResult};
use sg_store::prefstore::{self, PrefEntry};
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
    let doc = prefstore::load(store).map_err(store_err)?;
    let mut rows: Vec<SettingEntry> = doc
        .iter()
        .filter_map(|(id, entry)| {
            let (row_scope, row_project, key) = prefstore::split_composite(id);
            // project 为空 = 不过滤（global 与全部项目覆盖都返回），与旧 SQL `project_id IN ('', ?)` 一致。
            if row_scope != scope {
                return None;
            }
            if !project.is_empty() && !row_project.is_empty() && row_project != project {
                return None;
            }
            if let Some(ks) = keys {
                if !ks.iter().any(|k| k == &key) {
                    return None;
                }
            }
            Some(SettingEntry {
                key,
                value: entry.value.clone(),
                source: if row_project.is_empty() {
                    "global".into()
                } else {
                    "project".into()
                },
                scope: row_scope,
                revision: entry.revision,
                project_id: if row_project.is_empty() {
                    None
                } else {
                    Some(row_project)
                },
            })
        })
        .collect();
    rows.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(rows)
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
        // managed 键以 settings_managed_imports 标记存在时只读（台账留在 SQLite）。
        let managed: Option<String> = store.with_conn(|conn| {
            let result: rusqlite::Result<String> = conn.query_row(
                "SELECT resource_id FROM settings_managed_imports WHERE resource_type='setting' AND resource_id=?1",
                [&patch.key],
                |r| r.get(0),
            );
            Ok(result.ok())
        }).map_err(store_err)?;
        if managed.is_some() {
            return Err(SettingsError::new(
                codes::MANAGED_READ_ONLY,
                format!("键 {} 由托管来源管理", patch.key),
            ));
        }

        let id = prefstore::composite(scope, project, &patch.key);
        prefstore::write(store, |doc| {
            let revision = match doc.get(&id) {
                None => 1,
                Some(entry) => {
                    if let Some(expected) = patch.expected_revision {
                        if expected != entry.revision {
                            return Err(SettingsError::new(
                                codes::REVISION_CONFLICT,
                                format!("键 {} 期望 revision {} 实际 {}", patch.key, expected, entry.revision),
                            ).with_details(serde_json::json!({"key": patch.key, "expectedRevision": expected, "actualRevision": entry.revision})));
                        }
                    }
                    entry.revision + 1
                }
            };
            doc.insert(
                id.clone(),
                PrefEntry {
                    value: patch.value.clone(),
                    revision,
                    updated_at: now.clone(),
                    updated_by: updated_by.to_string(),
                },
            );
            Ok(())
        })?;
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
