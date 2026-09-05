//! Rollout flags（RFC v1.0 §19.2/§19.5）：manifest 域总开关与 GC 暂停。
//! 环境变量显式覆盖（E2E/开发用），否则读 app_settings（默认关闭/未暂停）。
use sg_store::{Error, Store};

/// knowledge.manifestEnabled（默认 false）：关闭时 manifest RPC 返回 feature_disabled。
pub fn manifest_enabled(store: &Store) -> Result<bool, Error> {
    if let Ok(v) = std::env::var("SIXGATES_KNOWLEDGE_MANIFEST") {
        return Ok(v == "1" || v.eq_ignore_ascii_case("true"));
    }
    flag(store, "knowledge.manifestEnabled")
}

/// knowledge.gcPaused（默认 false）：暂停 GC 第二步（排查/回退窗口）。
pub fn gc_paused(store: &Store) -> Result<bool, Error> {
    if let Ok(v) = std::env::var("SIXGATES_KNOWLEDGE_GC_PAUSE") {
        return Ok(v == "1" || v.eq_ignore_ascii_case("true"));
    }
    flag(store, "knowledge.gcPaused")
}

fn flag(store: &Store, key: &str) -> Result<bool, Error> {
    store.with_conn(|conn| {
        let v: Option<String> = conn
            .query_row(
                "SELECT value_json FROM app_settings WHERE scope='global' AND project_id='' AND key=?1",
                [key],
                |r| r.get(0),
            )
            .ok();
        Ok(v.map(|s| s.contains("true")).unwrap_or(false))
    })
}
