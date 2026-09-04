//! 统一 manifest 构建（方案 §7.3）：验证归属 → effective 策略 → 检索知识+记忆 →
//! included/excluded 与理由 → 单事务写 context_manifests/items/memories。
use serde_json::{json, Value};

use sg_knowledge as kb;
use sg_memory as mem;
use sg_store::{Error, Store};

use crate::manifest;

pub struct BuildInput<'a> {
    pub project_id: &'a str,
    pub workitem_id: &'a str,
    pub goal: &'a str,
    pub selected_sources: &'a [String],
}

fn workitem_project(store: &Store, workitem_id: &str) -> Result<String, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT project_id FROM workitems WHERE id=?1",
            [workitem_id],
            |r| r.get::<_, String>(0),
        )
        .map_err(|_| Error::Message("not_found: workitem".into()))
    })
}

/// 统一入口：两条 Run 入口与 context.create 共用；返回 manifest 元数据 +
/// memory included/excluded 摘要（不返回正文）。
pub fn build_manifest(store: &Store, input: &BuildInput) -> Result<Value, Error> {
    // 1. 归属验证：workitem 必须属于该项目（项目隔离，MEM-010）。
    let actual_project = workitem_project(store, input.workitem_id)?;
    if !actual_project.is_empty() && actual_project != input.project_id {
        return Err(Error::Message("manifest_workitem_mismatch".into()));
    }
    let project_id = if actual_project.is_empty() {
        input.project_id
    } else {
        &actual_project
    };

    // 2. 检索：知识（来源级）+ 记忆（确定性选择，reason 枚举 §6.6）。
    let query: String = input.goal.chars().take(80).collect();
    let preview = kb::context_preview(store, project_id, &query, 64 << 10)?;
    let terms = mem::repository::sanitize_terms(input.goal, 8);
    let (mem_included, mem_excluded) =
        mem::retrieval::select_for_context(store, project_id, &terms)?;

    // 3. 单事务落库：manifest + 知识 items + 记忆 included/excluded 证据。
    let id = manifest::new_manifest_id();
    let now = manifest::now_str();
    let scope = manifest::scope_json(&query, input.selected_sources);
    let project_id = project_id.to_string();
    let mut mem_bytes = 0i64;
    store.with_tx(|tx| {
        tx.execute(
            "INSERT INTO context_manifests(id, workitem_id, scope, data_policy, created_at)
             VALUES (?1,?2,?3,'standard',?4)",
            rusqlite::params![id, input.workitem_id, scope.to_string(), now],
        )?;
        let items = preview["items"].as_array().cloned().unwrap_or_default();
        for (ordinal, item) in items
            .iter()
            .filter(|i| i["included"].as_bool() == Some(true))
            .enumerate()
        {
            tx.execute(
                "INSERT INTO context_manifest_items(manifest_id, source_id, object_sha256, purpose, included, reason, ordinal)
                 VALUES (?1,?2,'',?3,1,?4,?5)",
                rusqlite::params![
                    id,
                    item["sourceId"].as_str().unwrap_or_default(),
                    "retrieval",
                    item["reason"].as_str().unwrap_or_default(),
                    ordinal as i64
                ],
            )?;
        }
        for (ordinal, item) in mem_included.iter().enumerate() {
            mem_bytes += item["bytes"].as_i64().unwrap_or(0);
            tx.execute(
                "INSERT INTO context_manifest_memories(
                    manifest_id, ordinal, project_id, memory_id, revision_id,
                    included, reason, score, bytes, token_estimate, selected_at
                 ) VALUES (?1,?2,?3,?4,?5,1,?6,?7,?8,?9,?10)",
                rusqlite::params![
                    id,
                    ordinal as i64,
                    project_id,
                    item["memoryId"].as_str().unwrap_or_default(),
                    item["revisionId"].as_str().unwrap_or_default(),
                    item["reason"].as_str().unwrap_or_default(),
                    item["score"].as_f64().unwrap_or(0.0),
                    item["bytes"].as_i64().unwrap_or(0),
                    item["tokenEstimate"].as_i64().unwrap_or(0),
                    now
                ],
            )?;
        }
        for (ordinal, item) in mem_excluded.iter().enumerate() {
            tx.execute(
                "INSERT INTO context_manifest_memories(
                    manifest_id, ordinal, project_id, memory_id, revision_id,
                    included, reason, score, bytes, token_estimate, selected_at
                 ) VALUES (?1,?2,?3,?4,?5,0,?6,?7,?8,?9,?10)",
                rusqlite::params![
                    id,
                    (mem_included.len() + ordinal) as i64,
                    project_id,
                    item["memoryId"].as_str().unwrap_or_default(),
                    item["revisionId"].as_str().unwrap_or_default(),
                    item["reason"].as_str().unwrap_or_default(),
                    item["score"].as_f64().unwrap_or(0.0),
                    item["bytes"].as_i64().unwrap_or(0),
                    item["tokenEstimate"].as_i64().unwrap_or(0),
                    now
                ],
            )?;
        }
        Ok(())
    })?;

    Ok(json!({
        "id": id,
        "workitemId": input.workitem_id,
        "scope": scope,
        "createdAt": now,
        "memory": {
            "included": mem_included.len(),
            "excluded": mem_excluded.len(),
            "bytes": mem_bytes,
        },
    }))
}
