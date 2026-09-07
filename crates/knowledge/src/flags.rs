//! Rollout flags（RFC v1.0 §19.5）：GC 暂停。
//! manifest 域总开关已随「无版本开关」决策移除（交付物入库依赖该路径，常开）。
//! 环境变量显式覆盖（E2E/开发用），否则读 app_settings。
use sg_store::{Error, Store};

/// knowledge.gcPaused（默认 false）：暂停 GC 第二步（排查/回退窗口）。
pub fn gc_paused(store: &Store) -> Result<bool, Error> {
    if let Ok(v) = std::env::var("RATIFLOW_KNOWLEDGE_GC_PAUSE") {
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
