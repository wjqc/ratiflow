//! 知识默认设置（global + 项目覆盖，revision 乐观锁）。
//! 存储：`data_dir/settings.json` 单文件（sg_store::prefstore），键 `knowledge.defaults`。
use serde_json::{json, Value};

use crate::{codes, store_err, SettingsError, SettingsResult};
use sg_store::prefstore::{self, PrefEntry};
use sg_store::{timefmt, Store};

const KEY: &str = "knowledge.defaults";

pub fn get(store: &Store, project_id: Option<&str>) -> SettingsResult<Value> {
    let doc = prefstore::load(store).map_err(store_err)?;
    let mut merged = doc
        .get(&prefstore::composite("global", "", KEY))
        .map(|e| e.value.clone())
        .unwrap_or_else(|| json!({}));
    if let Some(project) = project_id.filter(|p| !p.is_empty()) {
        if let Some(over) = doc
            .get(&prefstore::composite("project", project, KEY))
            .and_then(|e| e.value.as_object())
        {
            if let Some(base) = merged.as_object_mut() {
                for (k, v) in over {
                    base.insert(k.clone(), v.clone());
                }
            }
        }
    }
    Ok(merged)
}

pub fn revision_of(store: &Store, scope: &str, project_id: &str) -> SettingsResult<i64> {
    Ok(prefstore::get(store, scope, project_id, KEY)
        .map_err(store_err)?
        .map(|e| e.revision)
        .unwrap_or(0))
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
    let id = prefstore::composite(scope, project, KEY);
    let now = timefmt::now();
    prefstore::write(store, |doc| -> Result<i64, SettingsError> {
        // 首次创建不 CAS（旧表行为：缺行即建 revision=1，忽略 expected）。
        let revision = match doc.get(&id) {
            None => 1,
            Some(entry) => {
                if entry.revision != expected_revision {
                    return Err(SettingsError::new(
                        codes::REVISION_CONFLICT,
                        format!(
                            "知识设置期望 revision {expected_revision} 实际 {}",
                            entry.revision
                        ),
                    ));
                }
                entry.revision + 1
            }
        };
        doc.insert(
            id.clone(),
            PrefEntry {
                value: settings.clone(),
                revision,
                updated_at: now.clone(),
                updated_by: "local".into(),
            },
        );
        Ok(revision)
    })
}
