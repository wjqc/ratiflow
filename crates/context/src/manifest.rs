//! manifest 读取（自 sg-knowledge 迁移的 owner 职责；knowledge 保留检索）。
use serde_json::{json, Value};

use sg_store::{ids, timefmt, Error, Store};

use crate::builder::{self, BuildInput};

pub fn get_manifest(store: &Store, id: &str) -> Result<Value, Error> {
    let row: (String, String, String) = store.with_conn(|conn| {
        conn.query_row(
            "SELECT workitem_id, scope, COALESCE(data_policy,'standard') FROM context_manifests WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|_| Error::Message("manifest_not_found".into()))
    })?;
    Ok(json!({
        "id": id,
        "workitemId": row.0,
        "scope": serde_json::from_str::<Value>(&row.1).unwrap_or(Value::Null),
        "dataPolicy": row.2,
    }))
}

/// 兼容旧调用形状的纯知识清单创建（无记忆选入；context.create 之外不得使用）。
/// 新代码一律走 `builder::build_manifest`。
pub fn create_knowledge_only(
    store: &Store,
    project_id: &str,
    workitem_id: &str,
    query: &str,
    selected_sources: &[String],
) -> Result<Value, Error> {
    builder::build_manifest(
        store,
        &BuildInput {
            project_id,
            workitem_id,
            goal: query,
            selected_sources,
        },
    )
}

/// manifest 归属校验（agent.start 提供旧 contextManifestId 时必须验证并冻结）。
pub fn require_manifest_for_workitem(
    store: &Store,
    manifest_id: &str,
    workitem_id: &str,
) -> Result<Value, Error> {
    let manifest = get_manifest(store, manifest_id)?;
    if manifest["workitemId"].as_str() != Some(workitem_id) {
        return Err(Error::Message("manifest_workitem_mismatch".into()));
    }
    Ok(manifest)
}

/// scope JSON 构造（与迁移前形状兼容，附加 goal 供追溯）。
pub fn scope_json(query: &str, selected_sources: &[String]) -> Value {
    json!({
        "query": query,
        "selectedSources": selected_sources,
        "maxContextBytes": 262144,
    })
}

pub fn now_str() -> String {
    timefmt::now()
}

pub fn new_manifest_id() -> String {
    ids::new_id("ctx")
}
