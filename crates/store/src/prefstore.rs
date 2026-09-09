//! 简单应用偏好的单文件存储（`data_dir/settings.json`）。
//! 设计立场（用户 2026-09-08 拍板）：应用偏好不是治理事实——不配拥有表。
//! 整取整存、低频写、本地单进程：一个 JSON 文件 + 原子替换（temp+rename）足够；
//! 手工可编辑、可 diff。**范围**：generic settings（app.general 等）、
//! executor.settings、知识默认策略、knowledge GC flag。
//! **不含** memory.settings——其写入与 audit/event/mutation receipt 同事务
//! （mutation_registry ReceiptMode::Required），属受治理状态，留在 SQLite。
//! 首次访问从旧表（app_settings / knowledge_default_settings）种子导入一次，
//! 此后文件是唯一事实源，旧表不再写入（保留作迁移前快照，不删）。

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::{Error, Store};

pub const DOC_FILE: &str = "settings.json";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PrefEntry {
    pub value: Value,
    pub revision: i64,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    #[serde(rename = "updatedBy")]
    pub updated_by: String,
}

/// 复合键 → 条目。BTreeMap 迭代有序，文件 diff 稳定。
pub type PrefDoc = BTreeMap<String, PrefEntry>;

pub fn doc_path(store: &Store) -> PathBuf {
    store.data_dir.join(DOC_FILE)
}

/// 条目键：`scope:project_id:key`（scope 为 global/project；project_id 可空）。
/// 三段均不含冒号（key 是 app.general 等点分名，项目 ID 带前缀），splitn(3) 安全。
pub fn composite(scope: &str, project_id: &str, key: &str) -> String {
    format!("{scope}:{project_id}:{key}")
}

pub fn split_composite(id: &str) -> (String, String, String) {
    let mut it = id.splitn(3, ':');
    (
        it.next().unwrap_or_default().to_string(),
        it.next().unwrap_or_default().to_string(),
        it.next().unwrap_or_default().to_string(),
    )
}

static PREF_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> Result<std::sync::MutexGuard<'static, ()>, Error> {
    PREF_LOCK
        .lock()
        .map_err(|_| Error::Message("settings.json 锁中毒".into()))
}

fn read_file(store: &Store) -> Result<Option<PrefDoc>, Error> {
    let raw = match std::fs::read_to_string(doc_path(store)) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::Message(format!("settings.json 读取失败：{e}"))),
    };
    #[derive(serde::Deserialize)]
    struct Doc {
        entries: PrefDoc,
    }
    // 解析失败如实报错（手工编辑改坏 JSON 不允许被静默清空）。
    serde_json::from_str::<Doc>(&raw)
        .map(|d| Some(d.entries))
        .map_err(|e| {
            Error::Message(format!(
                "settings.json 解析失败（请检查手工编辑的 JSON）：{e}"
            ))
        })
}

fn persist(store: &Store, doc: &PrefDoc) -> Result<(), Error> {
    let path = doc_path(store);
    let body = serde_json::to_string_pretty(&serde_json::json!({ "version": 1, "entries": doc }))
        .map_err(|e| Error::Message(format!("settings.json 序列化失败：{e}")))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body)
        .map_err(|e| Error::Message(format!("settings.json 写入失败：{e}")))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| Error::Message(format!("settings.json 原子替换失败：{e}")))
}

/// 旧表种子导入（文件缺失时执行一次；结果确定，双检下并发只跑一份）。
fn seed_from_legacy(store: &Store) -> Result<PrefDoc, Error> {
    let mut doc = PrefDoc::new();
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT scope, project_id, key, value_json, revision, updated_at, updated_by FROM app_settings",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?;
        for row in rows {
            let (scope, project, key, value_json, revision, updated_at, updated_by) = row?;
            doc.insert(
                composite(&scope, &project, &key),
                PrefEntry {
                    value: serde_json::from_str(&value_json).unwrap_or(Value::Null),
                    revision,
                    updated_at,
                    updated_by,
                },
            );
        }
        let mut stmt = conn.prepare(
            "SELECT scope, project_id, settings_json, revision, updated_at FROM knowledge_default_settings",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (scope, project, body, revision, updated_at) = row?;
            doc.insert(
                composite(&scope, &project, "knowledge.defaults"),
                PrefEntry {
                    value: serde_json::from_str(&body).unwrap_or(Value::Null),
                    revision,
                    updated_at,
                    updated_by: "local".into(),
                },
            );
        }
        Ok(())
    })?;
    Ok(doc)
}

fn load_under_lock(store: &Store) -> Result<PrefDoc, Error> {
    if let Some(doc) = read_file(store)? {
        return Ok(doc);
    }
    let doc = seed_from_legacy(store)?;
    persist(store, &doc)?;
    Ok(doc)
}

/// 读取整份文档。文件缺失时从旧表种子导入（单次）；损坏报错不静默。
pub fn load(store: &Store) -> Result<PrefDoc, Error> {
    if let Some(doc) = read_file(store)? {
        return Ok(doc);
    }
    let _guard = lock()?;
    load_under_lock(store)
}

/// 读取单个条目。
pub fn get(
    store: &Store,
    scope: &str,
    project_id: &str,
    key: &str,
) -> Result<Option<PrefEntry>, Error> {
    Ok(load(store)?.remove(&composite(scope, project_id, key)))
}

/// 事务式修改：持锁加载 → f 应用 → 成功才原子落盘（f 出错不写文件）。
/// E 泛型让领域层保持自己的错误码（如 REVISION_CONFLICT）不丢结构。
pub fn write<T, E: From<Error>>(
    store: &Store,
    f: impl FnOnce(&mut PrefDoc) -> Result<T, E>,
) -> Result<T, E> {
    let _guard = lock().map_err(E::from)?;
    let mut doc = load_under_lock(store).map_err(E::from)?;
    let out = f(&mut doc)?;
    persist(store, &doc).map_err(E::from)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-pref-{}-{}",
            std::process::id(),
            crate::ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Store::open(&dir, "test").unwrap()
    }

    #[test]
    fn write_read_roundtrip_and_atomic_persist() {
        let store = setup();
        write(&store, |doc: &mut PrefDoc| -> Result<(), Error> {
            doc.insert(
                composite("global", "", "app.general"),
                PrefEntry {
                    value: json!({"theme": "dark"}),
                    revision: 1,
                    updated_at: crate::timefmt::now(),
                    updated_by: "local".into(),
                },
            );
            Ok(())
        })
        .unwrap();
        // 新实例从文件读回（无进程内缓存，验证落盘真实生效）。
        let entry = get(&store, "global", "", "app.general").unwrap().unwrap();
        assert_eq!(entry.value, json!({"theme": "dark"}));
        assert!(doc_path(&store).exists());
        // 原子替换无残留 tmp。
        assert!(!doc_path(&store).with_extension("json.tmp").exists());
    }

    #[test]
    fn f_error_aborts_persist() {
        let store = setup();
        write(&store, |doc: &mut PrefDoc| -> Result<(), Error> {
            doc.insert(
                composite("global", "", "k"),
                PrefEntry {
                    value: json!(1),
                    revision: 1,
                    updated_at: crate::timefmt::now(),
                    updated_by: "local".into(),
                },
            );
            Ok(())
        })
        .unwrap();
        // f 失败：加载阶段允许种子落盘，但 f 的改动不得持久化、已有内容不得受损。
        let _: Result<(), Error> = write(&store, |_doc| Err(Error::Message("boom".into())));
        let doc = load(&store).unwrap();
        assert_eq!(doc.len(), 1);
        assert_eq!(doc.get(&composite("global", "", "k")).unwrap().revision, 1);
    }

    #[test]
    fn seed_imports_legacy_tables_once() {
        let store = setup();
        store
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO app_settings(key, scope, project_id, value_json, revision, updated_at, updated_by)
                     VALUES ('ui.density','global','','\"compact\"',3,'2026-09-08T00:00:00.000Z','local')",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO knowledge_default_settings(scope, project_id, settings_json, revision, updated_at)
                     VALUES ('global','','{\"maxChunkChars\":2000}',2,'2026-09-08T00:00:00.000Z')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let doc = load(&store).unwrap();
        let density = doc.get(&composite("global", "", "ui.density")).unwrap();
        assert_eq!(density.value, json!("compact"));
        assert_eq!(density.revision, 3);
        let knowledge = doc
            .get(&composite("global", "", "knowledge.defaults"))
            .unwrap();
        assert_eq!(knowledge.value, json!({"maxChunkChars": 2000}));
        assert_eq!(knowledge.revision, 2);
        // 种子只导一次：旧表后续变化不再进入文件。
        store
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO app_settings(key, scope, project_id, value_json, revision, updated_at, updated_by)
                     VALUES ('late.key','global','','1',1,'2026-09-08T01:00:00.000Z','local')",
                    [],
                )
                .map_err(crate::Error::from)
            })
            .unwrap();
        let doc = load(&store).unwrap();
        assert!(!doc.contains_key(&composite("global", "", "late.key")));
    }

    #[test]
    fn corrupt_file_is_error_not_silent_reset() {
        let store = setup();
        std::fs::write(doc_path(&store), "{ not json").unwrap();
        assert!(load(&store).is_err());
    }

    #[test]
    fn composite_split_roundtrip() {
        let id = composite("project", "pj_1", "knowledge.defaults");
        assert_eq!(
            split_composite(&id),
            ("project".into(), "pj_1".into(), "knowledge.defaults".into())
        );
        let id = composite("global", "", "app.general");
        assert_eq!(
            split_composite(&id),
            ("global".into(), String::new(), "app.general".into())
        );
    }
}
