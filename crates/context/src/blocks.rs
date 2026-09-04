//! manifest 内容块装载（Run 执行期）：知识块（自 sg-knowledge 迁移）+ 记忆块。
//! 记忆块按 manifest 冻结的 revision 读取（Run 内冻结，§7.4）；
//! 已 purge 返回墓碑、对象缺失/哈希不符 fail-closed 跳过并如实上报。
use serde_json::{json, Value};

use sg_memory as mem;
use sg_store::{objects, Error, Store};

/// 知识块装载（§8.1）：有冻结 blocks 行 → 按最终对象 SHA 装载（fail-closed）；
/// legacy_pending（兼容观察期）→ 旧按来源装配路径。
pub fn manifest_blocks(store: &Store, manifest_id: &str, max_bytes: i64) -> Result<Value, Error> {
    let frozen: i64 = store.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM context_manifest_blocks WHERE manifest_id=?1",
            [manifest_id],
            |r| r.get(0),
        )
        .map_err(Into::into)
    })?;
    if frozen > 0 {
        return load_frozen_blocks(store, manifest_id, max_bytes);
    }
    load_legacy_blocks(store, manifest_id, max_bytes)
}

/// 冻结装载：按 blocks.object_sha256 读 objects（对象缺失 fail-closed 报 missing）。
fn load_frozen_blocks(store: &Store, manifest_id: &str, max_bytes: i64) -> Result<Value, Error> {
    struct Row {
        role: String,
        object_sha256: String,
        bytes: i64,
    }
    let rows: Vec<Row> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT role, object_sha256, bytes FROM context_manifest_blocks
             WHERE manifest_id=?1 ORDER BY ordinal",
        )?;
        let mut out = Vec::new();
        let mut rows = stmt.query([manifest_id])?;
        while let Some(r) = rows.next()? {
            out.push(Row {
                role: r.get(0)?,
                object_sha256: r.get(1)?,
                bytes: r.get(2)?,
            });
        }
        Ok(out)
    })?;
    let mut blocks = Vec::new();
    let mut total = 0i64;
    let mut truncated = false;
    let mut missing = 0usize;
    for row in &rows {
        let mut text = match objects::open(store, &row.object_sha256) {
            Ok(body) => String::from_utf8_lossy(&body).to_string(),
            Err(_) => {
                // 对象缺失：fail-closed —— 绝不发送替代文本（§8.1）。
                missing += 1;
                blocks.push(json!({
                    "role": row.role,
                    "bytes": row.bytes,
                    "missing": true,
                    "reason": "object_missing",
                }));
                continue;
            }
        };
        if total + text.len() as i64 > max_bytes {
            truncated = true;
            text.truncate(max_bytes.saturating_sub(total) as usize);
        }
        total += text.len() as i64;
        blocks.push(json!({
            "role": row.role,
            "bytes": text.len(),
            "text": text,
        }));
        if total >= max_bytes {
            truncated = true;
            break;
        }
    }
    Ok(json!({
        "blocks": blocks,
        "totalBytes": total,
        "includedCount": blocks.len(),
        "missingCount": missing,
        "truncated": truncated,
    }))
}

/// 旧装配路径（legacy_pending 兼容观察期）。
fn load_legacy_blocks(store: &Store, manifest_id: &str, max_bytes: i64) -> Result<Value, Error> {
    struct Item {
        source_id: String,
    }
    let items: Vec<Item> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT source_id FROM context_manifest_items
             WHERE manifest_id=?1 AND included=1 ORDER BY ordinal",
        )?;
        let rows = stmt.query_map([manifest_id], |r| {
            Ok(Item {
                source_id: r.get(0)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;
    let mut blocks = Vec::new();
    let mut total = 0i64;
    let mut truncated = false;
    for item in &items {
        if total >= max_bytes {
            truncated = true;
            break;
        }
        let name: String = store.with_conn(|conn| {
            conn.query_row(
                "SELECT name FROM knowledge_sources WHERE id=?1",
                [&item.source_id],
                |r| r.get(0),
            )
            .map_err(|_| Error::Message("source_not_found".into()))
        })?;
        let chunk_shas: Vec<String> = store.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT object_sha256 FROM knowledge_chunks WHERE source_id=?1 ORDER BY ordinal",
            )?;
            let rows = stmt.query_map([&item.source_id], |r| r.get(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })?;
        let mut text = String::new();
        for sha in chunk_shas {
            if total + text.len() as i64 >= max_bytes {
                truncated = true;
                break;
            }
            if let Ok(body) = objects::open(store, &sha) {
                let part = String::from_utf8_lossy(&body);
                text.push_str(&part);
                text.push('\n');
            }
        }
        if text.is_empty() {
            continue;
        }
        total += text.len() as i64;
        blocks.push(json!({
            "sourceId": item.source_id,
            "name": name,
            "bytes": text.len(),
            "text": text,
        }));
    }
    let excluded: i64 = store.with_conn(|conn| {
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM context_manifest_items WHERE manifest_id=?1 AND included=0",
            [manifest_id],
            |r| r.get(0),
        )?)
    })?;
    Ok(json!({
        "blocks": blocks,
        "totalBytes": total,
        "includedCount": blocks.len(),
        "excludedCount": excluded,
        "truncated": truncated,
    }))
}

/// 记忆块装载：manifest 冻结的 included 记忆 → §7.2 格式文本块。
/// - revision 已 purge → missing（reason=purged），正文绝不返回；
/// - 对象缺失/哈希不符 → missing（reason=object_missing），fail-closed 不发送替代文本。
pub fn manifest_memory_blocks(
    store: &Store,
    manifest_id: &str,
    max_bytes: i64,
) -> Result<Value, Error> {
    struct Row {
        memory_id: String,
        revision_id: String,
        revision_no: i64,
        kind: String,
        title: String,
        object_sha: Option<String>,
        purged_at: Option<String>,
    }
    let rows: Vec<Row> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT m.memory_id, m.revision_id, r.revision_no, e.kind, r.title,
                    r.object_sha256, r.purged_at
             FROM context_manifest_memories m
             JOIN memory_revisions r ON r.id = m.revision_id
             JOIN memory_entries e ON e.id = m.memory_id
             WHERE m.manifest_id = ?1 AND m.included = 1
             ORDER BY m.ordinal",
        )?;
        let rows = stmt.query_map([manifest_id], |r| {
            Ok(Row {
                memory_id: r.get(0)?,
                revision_id: r.get(1)?,
                revision_no: r.get(2)?,
                kind: r.get(3)?,
                title: r.get(4)?,
                object_sha: r.get(5)?,
                purged_at: r.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;

    let mut blocks = Vec::new();
    let mut missing = Vec::new();
    let mut total = 0i64;
    for row in rows {
        if row.purged_at.is_some() {
            // 用户"忘记"意图优先：返回墓碑，不再发送正文（§7.4）。
            missing.push(json!({"memoryId": row.memory_id, "reason": "purged"}));
            continue;
        }
        let Some(sha) = row.object_sha.clone() else {
            missing.push(json!({"memoryId": row.memory_id, "reason": "object_missing"}));
            continue;
        };
        // 哈希复核（fail-closed；缺对象不发送空文本或错误正文）。
        let Ok(body) = mem::repository::read_body(store, &sha) else {
            missing.push(json!({"memoryId": row.memory_id, "reason": "object_missing"}));
            continue;
        };
        if total + body.len() as i64 >= max_bytes {
            missing.push(json!({"memoryId": row.memory_id, "reason": "over_budget"}));
            continue;
        }
        total += body.len() as i64;
        blocks.push(json!({
            "memoryId": row.memory_id,
            "revisionId": row.revision_id,
            "revisionNo": row.revision_no,
            "kind": row.kind,
            "title": row.title,
            "bytes": body.len(),
            "text": body,
        }));
    }
    Ok(json!({
        "blocks": blocks,
        "totalBytes": total,
        "missing": missing,
    }))
}

/// 记忆使用证据（agent.get / rollout 摘要）：只输出 count/bytes/IDs，不输出正文。
pub fn memory_evidence(store: &Store, manifest_id: &str) -> Result<Value, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT m.memory_id, m.revision_id, r.revision_no, m.bytes
             FROM context_manifest_memories m
             JOIN memory_revisions r ON r.id = m.revision_id
             WHERE m.manifest_id = ?1 AND m.included = 1
             ORDER BY m.ordinal",
        )?;
        let rows = stmt.query_map([manifest_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        let mut ids = Vec::new();
        let mut items = Vec::new();
        let mut bytes = 0i64;
        for row in rows {
            let (id, rev, rev_no, b) = row?;
            bytes += b;
            ids.push(id.clone());
            items.push(json!({
                "memoryId": id,
                "revisionId": rev,
                "revisionNo": rev_no,
                "bytes": b,
            }));
        }
        Ok(json!({"count": ids.len(), "bytes": bytes, "ids": ids, "items": items}))
    })
}
