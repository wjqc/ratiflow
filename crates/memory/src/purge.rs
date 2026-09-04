//! 清除语义（方案 §6.3 / §10.3 / MEM-009）：
//! - purge 立即移除 FTS 与记忆正文引用，保留不含正文的审计墓碑；
//! - 共享对象零引用才物理删除；旧备份残留边界必须如实告知；
//! - 确认 token 一次性、绑定 projectId+memoryId+expectedRevision+impactDigest，5 分钟过期。
use rusqlite::Connection;
use serde_json::{json, Value};

use sg_store::Store;

use crate::model::{fingerprint, merr};
use crate::repository;

const TOKEN_TTL_SECS: i64 = 300;

/// 业务对象引用位点（跨领域共享 CAS；不含 events/audit 的哈希列）。
const OBJECT_REF_SITES: &[(&str, &str)] = &[
    ("knowledge_chunks", "object_sha256"),
    ("attachments", "object_sha256"),
    ("attachments", "extracted_object_sha256"),
    ("context_manifest_items", "object_sha256"),
    ("provenance_nodes", "object_sha256"),
    ("evidences", "object_sha256"),
];

/// 某对象在指定表列中的引用数（排除记忆自身指定 entry 的 revisions）。
fn count_refs(
    conn: &Connection,
    sum: &str,
    exclude_memory_id: Option<&str>,
) -> Result<i64, sg_store::Error> {
    let mut total = 0i64;
    for (table, column) in OBJECT_REF_SITES {
        let sql = format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1");
        let n: i64 = conn.query_row(&sql, [sum], |r| r.get(0)).unwrap_or(0);
        total += n;
    }
    // 其它记忆修订的引用（含本 entry 的历史 revision：purge 会全部置空，故仅排除非本条目）。
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memory_revisions WHERE object_sha256 = ?1
           AND (?2 = '' OR memory_id != ?2)",
        rusqlite::params![sum, exclude_memory_id.unwrap_or("")],
        |r| r.get(0),
    )?;
    total += n;
    Ok(total)
}

pub struct PurgeImpact {
    pub shared_with_knowledge: i64,
    pub shared_with_artifacts: i64,
    pub shared_with_attachments: i64,
    pub manifest_count: i64,
    pub latest_manifest: Option<(String, String)>,
    pub object_sums: Vec<String>,
    pub entry_status: String,
    pub entry_revision: i64,
}

/// 影响评估（只读）。
fn impact(
    conn: &Connection,
    project_id: &str,
    memory_id: &str,
) -> Result<PurgeImpact, sg_store::Error> {
    let entry = repository::entry_row(conn, project_id, memory_id)?;
    let mut sums = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT object_sha256 FROM memory_revisions
         WHERE memory_id = ?1 AND object_sha256 IS NOT NULL",
    )?;
    let rows = stmt.query_map([memory_id], |r| r.get::<_, String>(0))?;
    for row in rows {
        sums.push(row?);
    }

    let (shared_knowledge, shared_artifacts, shared_attachments) = {
        let mut k = 0i64;
        let mut a = 0i64;
        let mut at = 0i64;
        for sum in &sums {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM knowledge_chunks WHERE object_sha256 = ?1",
                    [sum],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            k += n;
            // context_manifest_items / provenance_nodes / evidences 归入 artifacts 侧。
            for (table, column) in [
                ("context_manifest_items", "object_sha256"),
                ("provenance_nodes", "object_sha256"),
                ("evidences", "object_sha256"),
            ] {
                let n: i64 = conn
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1"),
                        [sum],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                a += n;
            }
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM attachments WHERE object_sha256 = ?1 OR extracted_object_sha256 = ?1",
                    [sum],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            at += n;
        }
        (k, a, at)
    };
    // 旧 manifest 使用记录。
    let manifest_count: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT manifest_id) FROM context_manifest_memories WHERE memory_id = ?1",
        [memory_id],
        |r| r.get(0),
    )?;
    let latest_manifest = conn
        .query_row(
            "SELECT m.manifest_id, m.selected_at FROM context_manifest_memories m
             WHERE m.memory_id = ?1 ORDER BY m.selected_at DESC LIMIT 1",
            [memory_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .ok();

    Ok(PurgeImpact {
        shared_with_knowledge: shared_knowledge,
        shared_with_artifacts: shared_artifacts,
        shared_with_attachments: shared_attachments,
        manifest_count,
        latest_manifest,
        object_sums: sums,
        entry_status: entry.status,
        entry_revision: entry.revision,
    })
}

/// 影响摘要的 canonical digest：token 绑定影响范围，漂移即 MEMORY_PURGE_BLOCKED。
fn impact_digest(imp: &PurgeImpact) -> String {
    fingerprint(&json!({
        "attachments": imp.shared_with_attachments,
        "artifacts": imp.shared_with_artifacts,
        "knowledge": imp.shared_with_knowledge,
        "manifests": imp.manifest_count,
        "objectSums": imp.object_sums,
        "revision": imp.entry_revision,
        "status": imp.entry_status,
    }))
}

fn backup_count(store: &Store) -> i64 {
    let dir = store.data_dir.join("backups");
    std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map(|x| x == "db").unwrap_or(false))
                .count() as i64
        })
        .unwrap_or(0)
}

/// purgePreview：对象引用、manifest 使用、备份残留边界与一次性确认 token（§10.3）。
pub fn preview(store: &Store, project_id: &str, memory_id: &str) -> Result<Value, sg_store::Error> {
    let imp = store.with_conn(|conn| {
        repository::require_project(conn, project_id)?;
        impact(conn, project_id, memory_id)
    })?;
    let shared =
        imp.shared_with_knowledge + imp.shared_with_artifacts + imp.shared_with_attachments;
    let can_purge = shared == 0;
    let mut blockers = Vec::new();
    if imp.shared_with_knowledge > 0 {
        blockers.push(json!({
            "code": "SHARED_OBJECT_REFERENCED",
            "detail": format!("正文对象仍被 {} 个知识库分块引用，本次仅能解除记忆侧引用", imp.shared_with_knowledge),
        }));
    }
    if imp.shared_with_artifacts > 0 {
        blockers.push(json!({
            "code": "SHARED_OBJECT_REFERENCED",
            "detail": format!("正文对象仍被 {} 个工件/证据/谱系节点引用", imp.shared_with_artifacts),
        }));
    }
    if imp.shared_with_attachments > 0 {
        blockers.push(json!({
            "code": "SHARED_OBJECT_REFERENCED",
            "detail": format!("正文对象仍被 {} 个附件引用", imp.shared_with_attachments),
        }));
    }

    // 一次性 token：仅 canPurge 时发放，写 receipts 表（5 分钟过期，单次使用）。
    let (token, expires_at) = if can_purge {
        let token = sg_store::ids::new_id("mempt");
        let now = sg_store::timefmt::now();
        let digest = impact_digest(&imp);
        let expires_at = sg_store::timefmt::parse(&now)
            .map(|t| sg_store::timefmt::format_now(t + time::Duration::seconds(TOKEN_TTL_SECS)))
            .unwrap_or_else(|| now.clone());
        store.with_conn(|conn| {
            // 顺手清理过期 token。
            conn.execute(
                "DELETE FROM memory_mutation_receipts
                 WHERE operation = 'memory.purgeToken'
                   AND completed_at < datetime('now', '-1 hour')",
                [],
            )
            .ok();
            conn.execute(
                "INSERT INTO memory_mutation_receipts(
                    idempotency_key, project_id, operation, target_id, request_fingerprint, result_json, completed_at
                 ) VALUES (?1, ?2, 'memory.purgeToken', ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    token,
                    project_id,
                    memory_id,
                    digest,
                    json!({"expectedRevision": imp.entry_revision}).to_string(),
                    now
                ],
            )?;
            Ok(())
        })?;
        (Some(token), Some(expires_at))
    } else {
        (None, None)
    };

    Ok(json!({
        "memoryId": memory_id,
        "projectId": project_id,
        "entryStatus": imp.entry_status,
        "canPurge": can_purge,
        "blockers": blockers,
        "objectRefs": {
            "sharedWithKnowledge": imp.shared_with_knowledge,
            "sharedWithArtifacts": imp.shared_with_artifacts,
            "sharedWithAttachments": imp.shared_with_attachments,
            "orphansAfterPurge": if can_purge { imp.object_sums.len() as i64 } else { 0 },
        },
        "manifestRefs": {
            "count": imp.manifest_count,
            "latestRunId": imp.latest_manifest.as_ref().map(|(id, _)| id.clone()),
            "latestSelectedAt": imp.latest_manifest.as_ref().map(|(_, at)| at.clone()),
        },
        "backupRefs": {
            "likelyContained": backup_count(store),
            "note": "历史备份可能仍含原文；清除不等于从所有备份、共享对象或其他领域中抹除同一内容。",
        },
        "physicalDeletion": if can_purge { "unreferenced_objects" } else { "none" },
        "confirmationToken": token,
        "confirmationTokenExpiresAt": expires_at,
    }))
}

/// purge：墓碑化 + FTS 清除 + 零引用对象物理删除；audit/outbox/receipt 同事务。
pub fn purge(
    store: &Store,
    project_id: &str,
    memory_id: &str,
    expected_revision: i64,
    confirm_token: &str,
    idempotency_key: &str,
) -> Result<Value, sg_store::Error> {
    if confirm_token.trim().is_empty() {
        return Err(merr(
            crate::model::err_tokens::PURGE_BLOCKED,
            "缺少确认 token（两步清除）",
        ));
    }
    // token 校验 + 影响漂移检查（事务前只读评估）。
    let imp = store.with_conn(|conn| {
        repository::require_project(conn, project_id)?;
        impact(conn, project_id, memory_id)
    })?;
    let current_digest = impact_digest(&imp);
    let token_row: Option<(String, String, String, String)> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT project_id, target_id, request_fingerprint, completed_at
             FROM memory_mutation_receipts WHERE idempotency_key = ?1 AND operation = 'memory.purgeToken'",
        )?;
        let mut rows = stmt.query_map([confirm_token], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        rows.next().transpose().map_err(sg_store::Error::from)
    })?;
    let Some((token_project, token_target, token_digest, completed_at)) = token_row else {
        return Err(merr(
            crate::model::err_tokens::PURGE_BLOCKED,
            "确认 token 无效或已使用",
        ));
    };
    if token_project != project_id || token_target != memory_id {
        return Err(merr(
            crate::model::err_tokens::PURGE_BLOCKED,
            "确认 token 与目标不匹配",
        ));
    }
    if token_digest != current_digest {
        return Err(merr(
            crate::model::err_tokens::PURGE_BLOCKED,
            "清除影响范围已变化，请重新预览",
        ));
    }
    if token_expired(&completed_at)? {
        return Err(merr(
            crate::model::err_tokens::PURGE_BLOCKED,
            "确认已过期（5 分钟），请重新预览",
        ));
    }
    if imp.entry_revision != expected_revision {
        return Err(merr(
            crate::model::err_tokens::CONFLICT,
            format!(
                "revision 冲突：期望 {}，实际 {}",
                expected_revision, imp.entry_revision
            ),
        ));
    }

    let fp = fingerprint(&json!({
        "expectedRevision": expected_revision,
        "impactDigest": current_digest,
        "memoryId": memory_id,
        "operation": "memory.purge",
        "projectId": project_id,
    }));
    let released_sums = imp.object_sums.clone();
    let result = store.with_tx_immediate(|tx| {
        if let Some(previous) = crate::mutation::check_receipt(tx, idempotency_key, &fp)? {
            return Ok(previous);
        }
        let entry = repository::entry_row(tx, project_id, memory_id)?;
        if entry.status == "purged" {
            return Err(merr(
                crate::model::err_tokens::INVALID_STATE,
                "条目已处于清除状态",
            ));
        }
        let now = sg_store::timefmt::now();
        // 唯一的数据擦除例外：仅置空 object_sha256 并写 purged_at（§6.3）。
        tx.execute(
            "UPDATE memory_revisions SET object_sha256 = NULL, purged_at = ?2
             WHERE memory_id = ?1 AND object_sha256 IS NOT NULL",
            rusqlite::params![memory_id, now],
        )?;
        let changed = tx.execute(
            "UPDATE memory_entries SET status='purged', purged_at=?2, revision=revision+1, updated_at=?2
             WHERE id=?1 AND revision=?3",
            rusqlite::params![memory_id, now, expected_revision],
        )?;
        if changed == 0 {
            return Err(merr(crate::model::err_tokens::CONFLICT, "revision CAS 更新未命中"));
        }
        tx.execute("DELETE FROM memory_fts WHERE memory_id = ?1", [memory_id])?;
        // token 单次使用：立即删除。
        tx.execute(
            "DELETE FROM memory_mutation_receipts WHERE idempotency_key = ?1",
            [confirm_token],
        )?;
        sg_store::audit::append_at(
            tx,
            "local",
            "memory.purge",
            "memory",
            memory_id,
            json!({
                "from": entry.status,
                "to": "purged",
                "revision": expected_revision + 1,
                "releasedObjects": released_sums.len(),
            }),
        )?;
        sg_store::outbox::emit_at(
            tx,
            "memory",
            memory_id,
            "memory.purged",
            json!({"id": memory_id, "projectId": project_id, "status": "purged"}),
        )?;
        let result = json!({
            "memoryId": memory_id,
            "status": "purged",
            "revision": expected_revision + 1,
            "releasedObjects": released_sums.len(),
        });
        crate::mutation::receipt_put(
            tx,
            idempotency_key,
            project_id,
            "memory.purge",
            memory_id,
            &fp,
            &result,
        )?;
        Ok(result)
    })?;

    // 提交后：零引用对象物理删除（共享 CAS 中的同 hash 对象不受影响，§6.8）。
    for sum in &released_sums {
        let refs = store.with_conn(|conn| count_refs(conn, sum, None))?;
        if refs == 0 {
            let path = sg_store::objects::object_path(store, sum);
            if path.exists() {
                let _ = std::fs::remove_file(&path);
            }
            let _ = store.with_conn(|conn| {
                conn.execute("DELETE FROM objects WHERE sha256 = ?1", [sum])?;
                Ok(())
            });
        }
    }
    Ok(result)
}

fn token_expired(completed_at: &str) -> Result<bool, sg_store::Error> {
    let parsed = sg_store::timefmt::parse(completed_at).ok_or_else(|| {
        merr(
            crate::model::err_tokens::PURGE_BLOCKED,
            "token 时间戳不可解析",
        )
    })?;
    let age = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let ts = parsed.unix_timestamp();
    Ok(age - ts > TOKEN_TTL_SECS)
}
