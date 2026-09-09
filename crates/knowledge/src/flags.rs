//! Rollout flags（RFC v1.0 §19.5）：GC 暂停。
//! manifest 域总开关已随「无版本开关」决策移除（交付物入库依赖该路径，常开）。
//! 环境变量显式覆盖（E2E/开发用），否则读 settings.json 单文件（sg_store::prefstore）。
use sg_store::{prefstore, Error, Store};

/// knowledge.gcPaused（默认 false）：暂停 GC 第二步（排查/回退窗口）。
pub fn gc_paused(store: &Store) -> Result<bool, Error> {
    if let Ok(v) = std::env::var("RATIFLOW_KNOWLEDGE_GC_PAUSE") {
        return Ok(v == "1" || v.eq_ignore_ascii_case("true"));
    }
    flag(store, "knowledge.gcPaused")
}

fn flag(store: &Store, key: &str) -> Result<bool, Error> {
    // 旧实现按 value_json 原文 contains("true") 判定；文件形态下保持同语义。
    Ok(prefstore::get(store, "global", "", key)?
        .map(|e| e.value.to_string().contains("true"))
        .unwrap_or(false))
}
