//! 候选捕获：durable job 状态机（方案 §8.2 / §6.7）。
//! pending → in_flight → succeeded | failed | unknown（| cancelled）。
//! 规则：timeout/连接中断/进程死亡不可确认 → unknown，不透明重试、不生成候选、不激活记忆；
//! 状态迁移与候选写入在同一 SQLite 事务；输入只使用脱敏 rollout 摘要 + 已冻结来源。
use rusqlite::Connection;
use serde_json::{json, Value};

use sg_store::{ids, scan, Store};

use crate::model::{fingerprint, sha256_hex, KINDS};
use crate::repository;

pub const TASK_KIND: &str = "memory_capture";
pub const PROMPT_SCHEMA_VERSION: i64 = 1;

/// Provider 失败分类（§8.2）：超时/连接类 → unknown；其余为确定性 failed。
fn is_unknown_error(err: &str) -> bool {
    let lower = err.to_lowercase();
    lower.contains("timeout") || lower.contains("connection") || lower.contains("unreachable")
}

pub struct JobRow {
    pub id: String,
    pub project_id: String,
    pub run_id: String,
    pub status: String,
    pub attempt_count: i64,
    pub error_code: String,
    pub source_digest: String,
    pub request_fingerprint: String,
}

fn job_from_row(conn: &Connection, id: &str) -> Result<Option<JobRow>, sg_store::Error> {
    let mut stmt = conn.prepare(
        "SELECT id, project_id, run_id, status, attempt_count, error_code, source_digest, request_fingerprint
         FROM memory_capture_jobs WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map([id], |r| {
        Ok(JobRow {
            id: r.get(0)?,
            project_id: r.get(1)?,
            run_id: r.get(2)?,
            status: r.get(3)?,
            attempt_count: r.get(4)?,
            error_code: r.get(5)?,
            source_digest: r.get(6)?,
            request_fingerprint: r.get(7)?,
        })
    })?;
    Ok(rows.next().transpose()?)
}

/// 脱敏 rollout 摘要：goal/终态 + rollout 事件种类与状态，不含 tool raw output
/// （rollout 行本身已做逐行秘密复检；此处再对摘要整体扫描，命中即拒绝生成候选）。
pub fn rollout_summary(
    store: &Store,
    project_id: &str,
    run_id: &str,
) -> Result<(Value, String), sg_store::Error> {
    let run: (String, String, String) = store.with_conn(|conn| {
        conn.query_row(
            "SELECT goal, status, result FROM agent_runs WHERE id = ?1",
            [run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|_| sg_store::Error::Message("not_found: run".into()))
    })?;
    let path = sg_agent_rollout_path(store, run_id);
    let mut events: Vec<Value> = Vec::new();
    if let Ok(body) = std::fs::read_to_string(&path) {
        for line in body
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        {
            // 只保留种类/状态/摘要字段；文本载荷一律剔除（排除 tool raw output）。
            if let Some(kind) = line["kind"].as_str() {
                events.push(json!({
                    "kind": kind,
                    "status": line["data"]["status"].as_str().unwrap_or_default(),
                    "redacted": line["data"]["redacted"].as_bool().unwrap_or(false),
                }));
            }
        }
    }
    let summary = json!({
        "projectId": project_id,
        "runId": run_id,
        "goal": run.0,
        "status": run.1,
        "result": run.2,
        "events": events,
    });
    let text = summary.to_string();
    let findings = scan::scan(text.as_bytes());
    if scan::has_high_risk(&findings) {
        return Err(crate::model::merr(
            crate::model::err_tokens::SECRET,
            "rollout 摘要命中高风险秘密，拒绝生成候选",
        ));
    }
    Ok((summary, sha256_hex(text.as_bytes())))
}

fn sg_agent_rollout_path(store: &Store, run_id: &str) -> std::path::PathBuf {
    store
        .data_dir
        .join("logs")
        .join("runs")
        .join(format!("{run_id}.jsonl"))
}

/// 显式排队（captureStart / 显式重试）：同 key 同指纹返回原 job；
/// 同 key 异指纹冲突；每次调用即一个新 attempt（不透明重试不存在）。
pub fn start_job(
    store: &Store,
    project_id: &str,
    run_id: &str,
    idempotency_key: &str,
    model_profile_id: &str,
    model_revision: i64,
) -> Result<Value, sg_store::Error> {
    // 排队条件：仅 completed_execution 且项目 capture_mode=suggest（MEM-019 / §13 M4）。
    let run_status: String = store.with_conn(|conn| {
        conn.query_row(
            "SELECT status FROM agent_runs WHERE id = ?1",
            [run_id],
            |r| r.get(0),
        )
        .map_err(|_| sg_store::Error::Message("not_found: run".into()))
    })?;
    if run_status != "completed_execution" {
        return Err(crate::model::merr(
            crate::model::err_tokens::DISABLED,
            format!("仅 completed_execution 的 Run 可排队（当前 {run_status}）"),
        ));
    }
    let settings = repository::settings_get(store, project_id)?;
    if settings.capture_mode != "suggest" {
        return Err(crate::model::merr(
            crate::model::err_tokens::DISABLED,
            "项目未开启候选沉淀（capture_mode=suggest）",
        ));
    }

    let (summary, source_digest) = rollout_summary(store, project_id, run_id)?;
    // 冻结输入：入队时固化脱敏摘要，worker 使用同一份（§8.2 已冻结来源）。
    let fp = fingerprint(&json!({
        "captureMode": settings.capture_mode,
        "promptSchemaVersion": PROMPT_SCHEMA_VERSION,
        "runId": run_id,
        "sourceDigest": source_digest,
    }));
    let idempotency_key = format!("capture::{idempotency_key}");

    store.with_tx_immediate(|tx| {
        if let Some(previous) = crate::mutation::receipt_get(tx, &idempotency_key)? {
            if previous.fingerprint == fp {
                return Ok(previous.result);
            }
            return Err(crate::model::merr(
                crate::model::err_tokens::CONFLICT,
                "相同幂等键对应不同请求内容",
            ));
        }
        let job_id = ids::new_id("memjob");
        let now = sg_store::timefmt::now();
        tx.execute(
            "INSERT INTO memory_capture_jobs(
                id, project_id, run_id, source_digest, prompt_schema_version,
                model_profile_id, model_revision, summary_json, idempotency_key, request_fingerprint,
                status, attempt_count, started_at, updated_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'pending',0,?11,?11)",
            rusqlite::params![
                job_id,
                project_id,
                run_id,
                source_digest,
                PROMPT_SCHEMA_VERSION,
                model_profile_id,
                model_revision,
                summary.to_string(),
                idempotency_key,
                fp,
                now
            ],
        )?;
        let result = json!({"jobId": job_id, "status": "pending"});
        crate::mutation::receipt_put(
            tx,
            &idempotency_key,
            project_id,
            "memory.captureStart",
            &job_id,
            &fp,
            &result,
        )?;
        Ok(result)
    })
}

/// worker：pending → in_flight（attempt+1）。已在 in_flight 的 job 拒绝重复驱动。
pub fn mark_in_flight(store: &Store, job_id: &str) -> Result<bool, sg_store::Error> {
    store.with_conn(|conn| {
        let changed = conn.execute(
            "UPDATE memory_capture_jobs SET status='in_flight', attempt_count=attempt_count+1, updated_at=?2
             WHERE id=?1 AND status='pending'",
            rusqlite::params![job_id, sg_store::timefmt::now()],
        )?;
        Ok(changed > 0)
    })
}

/// 成功：写候选（单事务：job 终态 + 候选行），候选经 schema 校验与去重。
/// 候选正文写入 objects（先扫秘密，fail-closed）；候选本身永不被检索注入。
pub fn mark_succeeded(
    store: &Store,
    job_id: &str,
    candidate: &CandidateParsed,
    tokens_in: i64,
    tokens_out: i64,
) -> Result<Value, sg_store::Error> {
    let job = store.with_conn(|conn| {
        job_from_row(conn, job_id)?.ok_or_else(|| sg_store::Error::Message("not_found: job".into()))
    })?;
    let content_sha = sha256_hex(candidate.body.as_bytes());

    // 候选正文对象先写（事务外；失败由 GC 补偿）。
    use sg_store::{objects, PutOptions};
    let mut reader = candidate.body.as_bytes();
    let info = objects::put(
        store,
        &mut reader,
        PutOptions {
            max_bytes: 64 << 10,
            allow_secrets: false,
        },
    )?;

    let candidate_id = ids::new_id("memc");
    let now = sg_store::timefmt::now();
    store.with_tx_immediate(|tx| {
        let changed = tx.execute(
            "UPDATE memory_capture_jobs SET status='succeeded', finished_at=?2, updated_at=?2
             WHERE id=?1 AND status='in_flight'",
            rusqlite::params![job_id, now],
        )?;
        if changed == 0 {
            return Err(sg_store::Error::Message("capture_job_not_in_flight".into()));
        }
        // 去重：同项目同内容候选已存在（pending 或 rejected）→ 不再重复推荐。
        let dup: i64 = tx.query_row(
            "SELECT COUNT(*) FROM memory_candidates
             WHERE project_id = ?1 AND content_sha256 = ?2 AND status IN ('pending','rejected')",
            rusqlite::params![job.project_id, content_sha],
            |r| r.get(0),
        )?;
        if dup > 0 {
            return Ok(json!({"jobId": job_id, "status": "succeeded", "candidate": null, "deduped": true}));
        }
        tx.execute(
            "INSERT INTO memory_candidates(
                id, job_id, project_id, kind, subject_key, title, summary,
                object_sha256, content_sha256, request_fingerprint, status, created_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'pending',?11)",
            rusqlite::params![
                candidate_id,
                job_id,
                job.project_id,
                candidate.kind,
                crate::model::subject_key(&candidate.title),
                candidate.title,
                candidate.summary,
                info.sha256,
                content_sha,
                job.request_fingerprint,
                now
            ],
        )?;
        Ok(json!({"jobId": job_id, "status": "succeeded", "candidate": candidate_id, "tokensIn": tokens_in, "tokensOut": tokens_out}))
    })
}

/// 确定性失败 / 不确定（unknown）：终态写入；unknown 不生成候选。
pub fn mark_terminal(
    store: &Store,
    job_id: &str,
    terminal: &str,
    error_code: &str,
) -> Result<(), sg_store::Error> {
    debug_assert!(matches!(terminal, "failed" | "unknown" | "cancelled"));
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE memory_capture_jobs SET status=?2, error_code=?3, finished_at=?4, updated_at=?4
             WHERE id=?1 AND status IN ('pending','in_flight')",
            rusqlite::params![job_id, terminal, error_code, sg_store::timefmt::now()],
        )?;
        Ok(())
    })
}

/// Provider 错误字符串 → 终态与稳定 error_code。
pub fn classify_provider_error(err: &str) -> (&'static str, &'static str) {
    if is_unknown_error(err) {
        ("unknown", "MEMORY_CAPTURE_UNKNOWN")
    } else if err.contains("memory_secret_detected") {
        ("failed", "MEMORY_SECRET_DETECTED")
    } else {
        ("failed", "MEMORY_CAPTURE_FAILED")
    }
}

/// 启动 reconciliation（§8.2）：遗留 in_flight 无可靠 Provider 查询能力 → unknown；
/// pending 保持 pending（由显式重试或重启后的 reconcile 重驱）。
pub fn reconcile_broken_in_flight(store: &Store) -> Result<i64, sg_store::Error> {
    store.with_conn(|conn| {
        let changed = conn.execute(
            "UPDATE memory_capture_jobs SET status='unknown', error_code='MEMORY_CAPTURE_UNKNOWN',
                    finished_at=?1, updated_at=?1
             WHERE status='in_flight'",
            [sg_store::timefmt::now()],
        )?;
        Ok(changed as i64)
    })
}

pub fn get_job(store: &Store, project_id: &str, job_id: &str) -> Result<Value, sg_store::Error> {
    store.with_conn(|conn| {
        repository::require_project(conn, project_id)?;
        let job = conn
            .query_row(
                "SELECT id, run_id, status, attempt_count, error_code, provider_request_id,
                        prompt_schema_version, source_digest, started_at, updated_at, finished_at
                 FROM memory_capture_jobs WHERE id = ?1 AND project_id = ?2",
                rusqlite::params![job_id, project_id],
                |r| {
                    Ok(json!({
                        "jobId": r.get::<_, String>(0)?,
                        "runId": r.get::<_, String>(1)?,
                        "status": r.get::<_, String>(2)?,
                        "attemptCount": r.get::<_, i64>(3)?,
                        "errorCode": r.get::<_, String>(4)?,
                        "providerRequestId": r.get::<_, String>(5)?,
                        "promptSchemaVersion": r.get::<_, i64>(6)?,
                        "sourceDigest": r.get::<_, String>(7)?,
                        "startedAt": r.get::<_, String>(8)?,
                        "updatedAt": r.get::<_, String>(9)?,
                        "finishedAt": r.get::<_, Option<String>>(10)?,
                    }))
                },
            )
            .map_err(|_| sg_store::Error::Message("not_found: job".into()))?;
        let mut candidates = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT id, kind, title, summary, status, created_at FROM memory_candidates
             WHERE job_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map([job_id], |r| {
            Ok(json!({
                "candidateId": r.get::<_, String>(0)?,
                "kind": r.get::<_, String>(1)?,
                "title": r.get::<_, String>(2)?,
                "summary": r.get::<_, String>(3)?,
                "status": r.get::<_, String>(4)?,
                "createdAt": r.get::<_, String>(5)?,
            }))
        })?;
        for row in rows {
            candidates.push(row?);
        }
        Ok(json!({"job": job, "candidates": candidates}))
    })
}

/// memory-candidate schema 校验（与 contracts 侧 `memory-candidate` JSON Schema 一致）。
pub struct CandidateParsed {
    pub title: String,
    pub kind: String,
    pub summary: String,
    pub body: String,
}

pub fn parse_candidate_response(
    raw: &str,
    max_body_bytes: i64,
) -> Result<CandidateParsed, sg_store::Error> {
    let json_start = raw.find('{');
    let json_end = raw.rfind('}');
    let (s, e) = match (json_start, json_end) {
        (Some(s), Some(e)) if e > s => (s, e + 1),
        _ => {
            return Err(crate::model::merr(
                crate::model::err_tokens::INVALID_STATE,
                "响应不是 JSON 对象",
            ))
        }
    };
    let parsed: Value = serde_json::from_str(&raw[s..e]).map_err(|_| {
        crate::model::merr(
            crate::model::err_tokens::INVALID_STATE,
            "响应 JSON 解析失败",
        )
    })?;
    let title = parsed["title"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    let kind = parsed["kind"].as_str().unwrap_or("fact").to_string();
    let body = parsed["body"].as_str().unwrap_or_default().to_string();
    let summary = parsed["summary"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    if title.is_empty() || title.chars().count() > 200 {
        return Err(crate::model::merr(
            crate::model::err_tokens::INVALID_STATE,
            "候选 title 非法",
        ));
    }
    if !KINDS.contains(&kind.as_str()) {
        return Err(crate::model::merr(
            crate::model::err_tokens::INVALID_STATE,
            format!("候选 kind 非法：{kind}"),
        ));
    }
    if body.trim().is_empty() || body.len() as i64 > max_body_bytes {
        return Err(crate::model::merr(
            crate::model::err_tokens::INVALID_STATE,
            format!("候选 body 非法（1..{max_body_bytes} 字节）"),
        ));
    }
    let summary = summary.chars().take(512).collect::<String>();
    Ok(CandidateParsed {
        title,
        kind,
        summary,
        body,
    })
}
