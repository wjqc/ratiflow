//! 上下文选择（方案 §7.1）：FTS/LIKE + 确定性规则，不引入 Embedding。
//! 只查 project 一致、active、已确认、未过期、未 purge 的 current revision；
//! 预算（条数/字节）与状态门 fail-closed；同分按 updated_at DESC, memory_id ASC。
use serde_json::{json, Value};

use sg_store::Store;

use crate::repository;

/// 单条上下文评估结果（included 与 excluded 共用形状，§6.6）。
#[allow(clippy::too_many_arguments)]
fn item(
    memory_id: &str,
    revision_id: &str,
    revision_no: i64,
    title: &str,
    kind: &str,
    bytes: i64,
    score: f64,
    pinned: bool,
    reason: &str,
) -> Value {
    json!({
        "memoryId": memory_id,
        "revisionId": revision_id,
        "revisionNo": revision_no,
        "title": title,
        "kind": kind,
        "bytes": bytes,
        "tokenEstimate": bytes / 4,
        "score": score,
        "pinned": pinned,
        "reason": reason,
    })
}

struct Raw {
    id: String,
    revision_id: String,
    revision_no: i64,
    title: String,
    kind: String,
    object_sha: Option<String>,
    pinned: bool,
    updated_at: String,
    status: String,
}

struct Candidate {
    memory_id: String,
    revision_id: String,
    revision_no: i64,
    title: String,
    kind: String,
    bytes: i64,
    pinned: bool,
    updated_at: String,
    score: f64,
}

fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// 选择：返回 (included, excluded)。excluded 附带理由（§6.6 reason 枚举）。
pub fn select_for_context(
    store: &Store,
    project_id: &str,
    query_terms: &[String],
) -> Result<(Vec<Value>, Vec<Value>), sg_store::Error> {
    let settings = repository::settings_get(store, project_id)?;
    let feature = repository::feature_enabled(store)?;
    let now = sg_store::timefmt::now();

    // 收集候选与排除原因（单连接只读；正文读取在连接外逐条进行）。
    let (raws, mut excluded): (Vec<Raw>, Vec<Value>) = store.with_conn(|conn| {
        repository::require_project(conn, project_id)?;
        let mut stmt = conn.prepare(
            "SELECT e.id, r.id, r.revision_no, r.title, e.kind, r.object_sha256, e.pinned,
                    e.updated_at, e.status
             FROM memory_entries e
             JOIN memory_revisions r ON r.id = e.current_revision_id
             WHERE e.project_id = ?1 AND e.status IN ('active','conflicted')
               AND (e.valid_until IS NULL OR e.valid_until > ?2)",
        )?;
        let rows = stmt.query_map(rusqlite::params![project_id, now], |r| {
            Ok(Raw {
                id: r.get(0)?,
                revision_id: r.get(1)?,
                revision_no: r.get(2)?,
                title: r.get(3)?,
                kind: r.get(4)?,
                object_sha: r.get(5)?,
                pinned: r.get::<_, i64>(6)? != 0,
                updated_at: r.get(7)?,
                status: r.get(8)?,
            })
        })?;
        let mut raws = Vec::new();
        let mut conflicted = Vec::new();
        for row in rows {
            let r: Raw = row?;
            if r.status == "conflicted" {
                conflicted.push(item(
                    &r.id,
                    &r.revision_id,
                    r.revision_no,
                    &r.title,
                    &r.kind,
                    0,
                    0.0,
                    r.pinned,
                    "conflicted",
                ));
                continue;
            }
            raws.push(r);
        }
        Ok((raws, conflicted))
    })?;

    // feature/项目开关关闭：全部候选以 disabled 排除（§16.2：不报假成功）。
    if !feature || !settings.enabled {
        for r in &raws {
            excluded.push(item(
                &r.id,
                &r.revision_id,
                r.revision_no,
                &r.title,
                &r.kind,
                0,
                0.0,
                r.pinned,
                "disabled",
            ));
        }
        return Ok((vec![], excluded));
    }

    // stale 阈值：updated_at 距今超过 stale_after_days。
    let stale_cutoff = sg_store::timefmt::parse(&now)
        .map(|t| sg_store::timefmt::format_now(t - time::Duration::days(settings.stale_after_days)))
        .unwrap_or_default();

    let mut candidates: Vec<Candidate> = Vec::new();
    for r in raws {
        let Some(sha) = r.object_sha.clone() else {
            excluded.push(item(
                &r.id,
                &r.revision_id,
                r.revision_no,
                &r.title,
                &r.kind,
                0,
                0.0,
                r.pinned,
                "purged",
            ));
            continue;
        };
        // 正文不可读（缺失/哈希不符）→ fail-closed 排除（§7.4）。
        let Ok(body) = repository::read_body(store, &sha) else {
            excluded.push(item(
                &r.id,
                &r.revision_id,
                r.revision_no,
                &r.title,
                &r.kind,
                0,
                0.0,
                r.pinned,
                "purged",
            ));
            continue;
        };
        if !stale_cutoff.is_empty() && r.updated_at.as_str() < stale_cutoff.as_str() {
            excluded.push(item(
                &r.id,
                &r.revision_id,
                r.revision_no,
                &r.title,
                &r.kind,
                body.len() as i64,
                0.0,
                r.pinned,
                "stale",
            ));
            continue;
        }
        // 确定性打分：title +3 / kind +2 / body +1；pinned 加固定 boost（不绕过状态与预算）。
        let mut score = 0.0f64;
        let mut matched = false;
        for term in query_terms {
            let mut hit = false;
            if contains_ci(&r.title, term) {
                score += 3.0;
                hit = true;
            }
            if contains_ci(&r.kind, term) {
                score += 2.0;
                hit = true;
            }
            if contains_ci(&body, term) {
                score += 1.0;
                hit = true;
            }
            matched = matched || hit;
        }
        if r.pinned {
            score += 0.5;
        }
        if query_terms.is_empty() || matched {
            candidates.push(Candidate {
                memory_id: r.id.clone(),
                revision_id: r.revision_id.clone(),
                revision_no: r.revision_no,
                title: r.title.clone(),
                kind: r.kind.clone(),
                bytes: body.len() as i64,
                pinned: r.pinned,
                updated_at: r.updated_at.clone(),
                score,
            });
        }
        // 未命中且非空 query：不进入候选（reason 语义仅描述被评估后的排除原因）。
    }

    // 排序：score DESC, updated_at DESC, memory_id ASC（§7.1 确定性）。
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.memory_id.cmp(&b.memory_id))
    });

    let mut included = Vec::new();
    let mut used_bytes = 0i64;
    let mut used_entries = 0i64;
    for c in &candidates {
        let reason = if c.pinned && c.score <= 0.5 {
            "pinned"
        } else {
            "matched"
        };
        if used_entries >= settings.max_entries || used_bytes + c.bytes > settings.max_bytes {
            excluded.push(item(
                &c.memory_id,
                &c.revision_id,
                c.revision_no,
                &c.title,
                &c.kind,
                c.bytes,
                c.score,
                c.pinned,
                "over_budget",
            ));
            continue;
        }
        used_entries += 1;
        used_bytes += c.bytes;
        included.push(item(
            &c.memory_id,
            &c.revision_id,
            c.revision_no,
            &c.title,
            &c.kind,
            c.bytes,
            c.score,
            c.pinned,
            reason,
        ));
    }
    Ok((included, excluded))
}
