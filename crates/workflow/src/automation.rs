//! 自动化调度核心（EvoFlow 方案 M6-02 / ADR-039 §6.13）：
//! durable schedule + TriggerReceipt 幂等（同 automation+scheduled_for 唯一）
//! + misfire（skip/run_once/catch_up_one，禁止无限补跑）+ overlap（skip/queue_one）
//! + 暂停/恢复。只产 intent：副作用检查（grant/预算/审批）在消费侧。

use serde::Serialize;
use sg_store::{ids, outbox, timefmt, Error, Store};
use time::Duration as TimeDuration;

#[derive(Debug, Clone, Serialize)]
pub struct AutomationRecord {
    pub id: String,
    pub key: String,
    pub project_id: Option<String>,
    pub workitem_id: Option<String>,
    pub intent_json: String,
    pub interval_secs: i64,
    pub next_fire_at: String,
    pub misfire_policy: String,
    pub overlap_policy: String,
    pub autonomy_grant_id: Option<String>,
    pub status: String,
    /// WP-12：shadow 策略（1=只产建议不执行；默认 1，0043 列）。
    pub shadow_mode: bool,
    pub revision: i64,
}

fn row(conn: &rusqlite::Connection, id: &str) -> Result<AutomationRecord, Error> {
    conn.query_row(
        "SELECT id, key, COALESCE(project_id,''), COALESCE(workitem_id,''), intent_json,
                interval_secs, next_fire_at, misfire_policy, overlap_policy,
                COALESCE(autonomy_grant_id,''), status, COALESCE(shadow_mode,1), revision
         FROM automations WHERE id=?1",
        [id],
        |r| {
            Ok(AutomationRecord {
                id: r.get(0)?,
                key: r.get(1)?,
                project_id: empty_none(r.get::<_, String>(2)?),
                workitem_id: empty_none(r.get::<_, String>(3)?),
                intent_json: r.get(4)?,
                interval_secs: r.get(5)?,
                next_fire_at: r.get(6)?,
                misfire_policy: r.get(7)?,
                overlap_policy: r.get(8)?,
                autonomy_grant_id: empty_none(r.get::<_, String>(9)?),
                status: r.get(10)?,
                shadow_mode: r.get::<_, i64>(11)? != 0,
                revision: r.get(12)?,
            })
        },
    )
    .map_err(|_| Error::Message(format!("automation_not_found: {id}")))
}

fn empty_none(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// 创建自动化（interval 调度；next_fire = now + interval）。
#[allow(clippy::too_many_arguments)]
pub fn create(
    store: &Store,
    key: &str,
    project_id: Option<&str>,
    workitem_id: Option<&str>,
    intent_json: &str,
    interval_secs: i64,
    misfire_policy: &str,
    overlap_policy: &str,
    grant_id: Option<&str>,
    created_by: &str,
) -> Result<AutomationRecord, Error> {
    let id = ids::new_id("auto");
    let now = timefmt::now();
    let next = advance_time(&now, interval_secs)?;
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO automations(id, key, project_id, workitem_id, intent_json, interval_secs,
                next_fire_at, misfire_policy, overlap_policy, autonomy_grant_id, status, revision,
                created_by, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'active',1,?11,?12,?12)",
            rusqlite::params![
                id,
                key,
                project_id,
                workitem_id,
                intent_json,
                interval_secs,
                next,
                misfire_policy,
                overlap_policy,
                grant_id,
                created_by,
                now
            ],
        )
        .map_err(|e| {
            if e.to_string().contains("UNIQUE") {
                Error::Message(format!("automation_key_exists: {key}"))
            } else {
                e.into()
            }
        })?;
        row(conn, &id)
    })
}

/// RFC3339 固定格式字典序安全的时间推进（timefmt::now 同格式）。
fn advance_time(now: &str, secs: i64) -> Result<String, Error> {
    let t = timefmt::parse(now)
        .ok_or_else(|| Error::Message(format!("automation_time_invalid: {now}")))?;
    Ok(timefmt::format_now(t + TimeDuration::seconds(secs)))
}

/// 到期扫描：返回到期且 active 的 automation（fired 前调用方先过 overlap 闸）。
pub fn due(store: &Store, now: &str) -> Result<Vec<AutomationRecord>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id FROM automations
             WHERE status='active' AND next_fire_at <= ?1
             ORDER BY next_fire_at LIMIT 10",
        )?;
        let rows = stmt.query_map([now], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(row(conn, &r?)?);
        }
        Ok(out)
    })
}

/// TriggerReceipt 认领（幂等锚点）：同 (automation, scheduled_for) 唯一。
/// 返回 Ok(None) = 该 scheduled_for 已有 receipt（重复启动/时钟跳变 → 不重复执行）。
pub fn claim_receipt(
    store: &Store,
    automation_id: &str,
    scheduled_for: &str,
) -> Result<Option<String>, Error> {
    let id = ids::new_id("arun");
    let receipt = ids::new_id("rct");
    let now = timefmt::now();
    let inserted = store.with_conn(|conn| {
        Ok(conn
            .execute(
                "INSERT INTO automation_runs(id, automation_id, scheduled_for, receipt, status, created_at)
                 VALUES (?1,?2,?3,?4,'fired',?5)
                 ON CONFLICT(automation_id, scheduled_for) DO NOTHING",
                rusqlite::params![id, automation_id, scheduled_for, receipt, now],
            )?)
    })?;
    if inserted == 0 {
        return Ok(None);
    }
    Ok(Some(receipt))
}

/// 重调度：next_fire = max(scheduled_for, now) + interval（misfire 后不回溯补跑）。
pub fn reschedule(store: &Store, automation_id: &str, from: &str) -> Result<(), Error> {
    let a = store.with_conn(|conn| row(conn, automation_id))?;
    let base: &str = if from > a.next_fire_at.as_str() {
        from
    } else {
        a.next_fire_at.as_str()
    };
    let next = advance_time(base, a.interval_secs)?;
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE automations SET next_fire_at=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![next, now, automation_id],
        )?;
        Ok(())
    })
}

/// misfire 处理（§6.13）：now 越过 next_fire 一个整周期以上才算 misfire
/// （评审 P1 修复：正常调度器有毫秒级抖动，只越过 next_fire 不算过期——
/// 否则 skip 策略几乎每次都误判放弃）。skip → 标记 skipped_misfire；
/// run_once/catch_up_one → 触发一次（上限 1，禁止无限补跑）。
/// 返回是否应触发。
pub fn misfire_decision(
    misfire_policy: &str,
    next_fire_at: &str,
    now: &str,
    interval_secs: i64,
) -> Result<bool, Error> {
    let overdue = match (timefmt::parse(now), timefmt::parse(next_fire_at)) {
        (Some(n), Some(f)) => n > f + TimeDuration::seconds(interval_secs.max(1)),
        _ => now > next_fire_at,
    };
    match misfire_policy {
        "skip" => Ok(!overdue),
        "run_once" | "catch_up_one" => Ok(true), // 补跑一次，重调度向前（reschedule 已钳 now）
        _ => Ok(!overdue),
    }
}

/// overlap 闸：上一触发尚无终态（fired 未推进）时按策略 skip/queue_one。
/// 排除本次 scheduled_for 自身的 receipt 行（评审 P1 修复：认领先行后，
/// 当前触发的 fired 行不得计入 overlap，否则首轮即被误判上一轮在途）。
pub fn overlap_allowed(store: &Store, automation_id: &str, policy: &str) -> Result<bool, Error> {
    overlap_allowed_for(store, automation_id, policy, None)
}

/// 带排除项的 overlap 闸（fire_one 认领后传当前 scheduled_for）。
pub fn overlap_allowed_for(
    store: &Store,
    automation_id: &str,
    policy: &str,
    exclude_scheduled_for: Option<&str>,
) -> Result<bool, Error> {
    let pending: i64 = store.with_conn(|conn| {
        let n = match exclude_scheduled_for {
            Some(sf) => conn.query_row(
                "SELECT COUNT(*) FROM automation_runs
                 WHERE automation_id=?1 AND status='fired' AND scheduled_for<>?2",
                rusqlite::params![automation_id, sf],
                |r| r.get(0),
            ),
            None => conn.query_row(
                "SELECT COUNT(*) FROM automation_runs
                 WHERE automation_id=?1 AND status='fired'",
                [automation_id],
                |r| r.get(0),
            ),
        };
        n.map_err(Error::from)
    })?;
    match policy {
        "queue_one" => Ok(pending == 0 || pending < 2),
        _ => Ok(pending == 0), // skip
    }
}

/// 意图落账：fired → intent_created / blocked_no_grant / skipped_*。
pub fn record_intent(
    store: &Store,
    automation_id: &str,
    scheduled_for: &str,
    status: &str,
    run_intent_id: Option<&str>,
    note: &str,
) -> Result<(), Error> {
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE automation_runs SET status=?1, run_intent_id=COALESCE(?2, run_intent_id),
                note=?3, created_at=created_at
             WHERE automation_id=?4 AND scheduled_for=?5",
            rusqlite::params![status, run_intent_id, note, automation_id, scheduled_for],
        )?;
        Ok(())
    })?;
    // P0-5：shadow_fallback 通知由 force_shadow_mode_v2 单一权威发出
    //（metric digest 幂等键）；此处只发 blocked_no_grant/failed。
    if matches!(status, "blocked_no_grant" | "failed") {
        // 通知 outbox（桌面通知首期消费）。
        store.with_conn(|conn| {
            conn.execute(
                "INSERT INTO notification_outbox(id, kind, automation_id, payload_json, created_at)
                 VALUES (?1,'automation_blocked',?2,?3,?4)",
                rusqlite::params![
                    ids::new_id("ntf"),
                    automation_id,
                    serde_json::json!({"status": status, "note": note}).to_string(),
                    now
                ],
            )?;
            Ok(())
        })?;
    }
    Ok(())
}

/// 暂停/恢复（CAS revision）。
pub fn set_status(
    store: &Store,
    automation_id: &str,
    status: &str,
    expected_revision: i64,
) -> Result<AutomationRecord, Error> {
    store.with_conn(|conn| {
        let changed = conn.execute(
            "UPDATE automations SET status=?1, revision=revision+1, updated_at=?2
             WHERE id=?3 AND revision=?4",
            rusqlite::params![status, timefmt::now(), automation_id, expected_revision],
        )?;
        if changed == 0 {
            return Err(Error::Message(
                "automation_conflict: revision 不匹配".into(),
            ));
        }
        row(conn, automation_id)
    })
}

/// 启动 reconciliation：上一进程遗留的 fired（未推进）按 misfire 策略收敛——
/// 重复启动不重复执行（receipt 已去重），此处只把孤儿 fired 标 failed + 通知。
pub fn reconcile_orphans(store: &Store) -> Result<usize, Error> {
    let orphans: Vec<(String, String)> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT automation_id, scheduled_for FROM automation_runs WHERE status='fired'",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;
    for (aid, sfor) in &orphans {
        record_intent(
            store,
            aid,
            sfor,
            "failed",
            None,
            "启动 reconciliation：遗留触发未推进",
        )?;
    }
    Ok(orphans.len())
}

/// WP-12：shadow 策略开关（CAS expectedRevision；系统回退走 force 变体）。
pub fn set_shadow_mode(
    store: &Store,
    automation_id: &str,
    shadow_mode: bool,
    expected_revision: i64,
) -> Result<AutomationRecord, Error> {
    let now = timefmt::now();
    let changed = store.with_conn(|conn| {
        conn.execute(
            "UPDATE automations SET shadow_mode=?1, revision=revision+1, updated_at=?2
             WHERE id=?3 AND revision=?4",
            rusqlite::params![shadow_mode as i64, now, automation_id, expected_revision],
        )?;
        Ok(conn.changes())
    })?;
    if changed == 0 {
        return Err(Error::Message(format!(
            "automation_conflict: revision 不匹配（期望 {expected_revision}）"
        )));
    }
    store.with_conn(|conn| row(conn, automation_id))
}

/// P0-5：误报率自动回 shadow——**带 CAS 的两窗迟滞**（审计 §7 P0-5）：
/// - 触发条件（新口径）：reviewed≥min_sample 且 review_coverage≥50% 且 rate>阈值；
/// - metric 快照 digest 冻结（decided/reviewed/fp/coverage/window/阈值/策略版本），
///   同快照重复评估幂等跳过（防同窗多 tick 重复计数）；
/// - 连续两个**不同**超阈值快照（bad_window_streak≥2）才回退；
/// - 回退 CAS：expected revision + shadow_mode=0 命中才写（用户并发改配置不覆盖）；
/// - 同事务写 automations（shadow_mode/revision/cooldown/streak/last_metric_snapshot_digest）、
///   automation_policy_transitions（UNIQUE 兜底）、audit、notification_outbox
///   （幂等键 = fallback|automation|metric_digest，0051 唯一索引防重复通知）。
///
/// 返回 (本窗是否超阈值, 是否已回退)。
pub fn force_shadow_mode_v2(
    store: &Store,
    automation_id: &str,
    stats: (i64, i64, i64, f64, f64), // (decided, reviewed, fp, rate, coverage)
    window_days: i64,
) -> Result<(bool, bool), Error> {
    let (decided, reviewed, fp, rate, coverage) = stats;
    let digest = {
        use sha2::Digest;
        format!(
            "sha256:{}",
            ids::hex(&sha2::Sha256::digest(
                format!(
                    "fpmetrics|{automation_id}|{decided}|{reviewed}|{fp}|{rate:.6}|{coverage:.6}|{window_days}|{}|{}",
                    shadow_fp_threshold(),
                    shadow_min_sample()
                )
                .as_bytes()
            ))
        )
    };
    let min = shadow_min_sample();
    let threshold = shadow_fp_threshold();
    let breach = reviewed >= min && coverage >= 0.5 && rate > threshold;
    let now = timefmt::now();
    store.with_tx(|conn| {
        let (revision, streak, last_digest): (i64, i64, String) = conn.query_row(
            "SELECT revision, bad_window_streak, last_metric_snapshot_digest
             FROM automations WHERE id=?1",
            [automation_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        // 同快照幂等：本窗已按此 digest 评估过（streak 已计）——不重复计数/通知。
        // 是否"已回退"以 transition 事实为准（streak 阶段 shadow_mode 仍是 0）。
        if !last_digest.is_empty() && last_digest == digest {
            let transitioned: i64 = conn.query_row(
                "SELECT COUNT(*) FROM automation_policy_transitions
                 WHERE automation_id=?1 AND metric_snapshot_digest=?2",
                rusqlite::params![automation_id, digest],
                |r| r.get(0),
            )?;
            return Ok((breach, transitioned > 0));
        }
        let new_streak = if breach { streak + 1 } else { 0 };
        if !breach {
            conn.execute(
                "UPDATE automations SET bad_window_streak=0, last_metric_snapshot_digest=?1, updated_at=?2
                 WHERE id=?3",
                rusqlite::params![digest, now, automation_id],
            )?;
            return Ok((false, false));
        }
        // 超阈值但未到两窗：只累计 streak + 快照（不回退、不通知）。
        if new_streak < 2 {
            conn.execute(
                "UPDATE automations SET bad_window_streak=?1, last_metric_snapshot_digest=?2, updated_at=?3
                 WHERE id=?4",
                rusqlite::params![new_streak, digest, now, automation_id],
            )?;
            sg_store::audit::append_at(
                conn,
                "system",
                "automation.bad_window",
                "automation",
                automation_id,
                serde_json::json!({"streak": new_streak, "metricDigest": digest,
                                   "reviewed": reviewed, "falsePositives": fp, "rate": rate,
                                   "reviewCoverage": coverage}),
            )?;
            return Ok((true, false));
        }
        // 两窗迟滞满足 → 回退（CAS：expected revision + shadow_mode=0；
        // 命中 0 行 = 用户并发改配置 → 不覆盖，下轮重评）。
        let changed = conn.execute(
            "UPDATE automations SET shadow_mode=1, revision=revision+1, bad_window_streak=0,
                    cooldown_started_at=?1, last_metric_snapshot_digest=?2, updated_at=?1
             WHERE id=?3 AND revision=?4 AND shadow_mode=0",
            rusqlite::params![now, digest, automation_id, revision],
        )?;
        if changed == 0 {
            return Ok((true, false));
        }
        conn.execute(
            "INSERT OR IGNORE INTO automation_policy_transitions
             (id, automation_id, metric_snapshot_digest, target_mode, expected_revision,
              review_coverage, sample_size, false_positives, window_days, policy_version, actor, created_at)
             VALUES (?1,?2,?3,1,?4,?5,?6,?7,?8,'sp2','system',?9)",
            rusqlite::params![
                ids::new_id("apt"),
                automation_id,
                digest,
                revision,
                coverage,
                reviewed,
                fp,
                window_days,
                now
            ],
        )?;
        sg_store::audit::append_at(
            conn,
            "system",
            "automation.shadow_fallback",
            "automation",
            automation_id,
            serde_json::json!({"metricDigest": digest, "reviewed": reviewed,
                               "falsePositives": fp, "rate": rate, "reviewCoverage": coverage}),
        )?;
        // 通知幂等：同 (automation, metric digest) 只一条（0051 唯一索引兜底）。
        conn.execute(
            "INSERT OR IGNORE INTO notification_outbox
             (id, kind, automation_id, payload_json, idempotency_key, created_at)
             VALUES (?1,'automation_blocked',?2,?3,?4,?5)",
            rusqlite::params![
                ids::new_id("ntf"),
                automation_id,
                serde_json::json!({
                    "status": "shadow_fallback",
                    "note": format!("两窗误报率 {rate:.2}>{threshold:.2}（{fp}/{reviewed}，coverage {coverage:.2}），自动回 shadow"),
                    "metricDigest": digest,
                })
                .to_string(),
                format!("fallback|{automation_id}|{digest}"),
                now
            ],
        )?;
        outbox::emit_at(
            conn,
            "automation",
            automation_id,
            "automation.shadow_fallback",
            serde_json::json!({"automationId": automation_id, "reason": "false_positive_rate",
                               "metricDigest": digest}),
        )?;
        Ok((true, true))
    })
}

/// P0-5：cooldown 窗口（回退后的人工冷静期；env 可配，默认 24h）。
/// Some(until) = 冷静期内（不可人工切 live）。
pub fn cooldown_until(store: &Store, automation_id: &str) -> Result<Option<String>, Error> {
    let started: Option<String> = store.with_conn(|conn| {
        Ok(conn
            .query_row(
                "SELECT cooldown_started_at FROM automations WHERE id=?1",
                [automation_id],
                |r| r.get(0),
            )
            .ok()
            .flatten())
    })?;
    let Some(started) = started.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let hours = std::env::var("RATIFLOW_AUTOMATION_COOLDOWN_HOURS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(24);
    let Some(t0) = timefmt::parse(&started) else {
        return Ok(None);
    };
    let until = (t0 + time::Duration::hours(hours))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    if timefmt::now() < until {
        Ok(Some(until))
    } else {
        Ok(None)
    }
}

/// P0-5：durable run intent 入队（幂等：ri|automation|scheduled_for）。
/// 返回 intent id。
pub fn enqueue_run_intent(
    store: &Store,
    automation_id: &str,
    workitem_id: Option<&str>,
    scheduled_for: &str,
    intent: &serde_json::Value,
    grant_digest: &str,
    policy_snapshot: &str,
) -> Result<String, Error> {
    let idempotency_key = format!("ri|{automation_id}|{scheduled_for}");
    let existing: Option<String> = store.with_conn(|conn| {
        Ok(conn
            .query_row(
                "SELECT id FROM run_intents WHERE idempotency_key=?1",
                [&idempotency_key],
                |r| r.get(0),
            )
            .ok())
    })?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = ids::new_id("rint");
    let now = timefmt::now();
    let inserted = store.with_conn(|conn| {
        conn.execute(
            "INSERT OR IGNORE INTO run_intents
             (id, source, automation_id, workitem_id, intent_json, policy_snapshot,
              context_digest, grant_digest, idempotency_key, state, created_at, updated_at)
             VALUES (?1,'automation',?2,?3,?4,?5,'',?6,?7,'pending',?8,?8)",
            rusqlite::params![
                id,
                automation_id,
                workitem_id,
                intent.to_string(),
                policy_snapshot,
                grant_digest,
                idempotency_key,
                now
            ],
        )?;
        Ok(conn.changes() == 1)
    })?;
    if inserted {
        return Ok(id);
    }
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT id FROM run_intents WHERE idempotency_key=?1",
            [&idempotency_key],
            |r| r.get(0),
        )
        .map_err(Error::from)
    })
}

/// WP-12 门槛配置（本地假设，env 可覆盖）：最小样本/窗口天数/误报率阈值。
pub fn shadow_min_sample() -> i64 {
    std::env::var("RATIFLOW_SHADOW_MIN_SAMPLE")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(30)
}

pub fn shadow_window_days() -> i64 {
    std::env::var("RATIFLOW_SHADOW_WINDOW_DAYS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(14)
}

pub fn shadow_fp_threshold() -> f64 {
    std::env::var("RATIFLOW_SHADOW_FP_THRESHOLD_PCT")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .map(|pct| pct / 100.0)
        .unwrap_or(0.10)
}

/// 窗内已复核的已决建议数（分母口径，P0-5）。
fn reviewed_of(
    conn: &rusqlite::Connection,
    automation_id: &str,
    window_days: i64,
) -> rusqlite::Result<i64> {
    let start = (timefmt::parse(&timefmt::now()).unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        - time::Duration::days(window_days))
    .format(&time::format_description::well_known::Rfc3339)
    .unwrap_or_default();
    conn.query_row(
        "SELECT COUNT(*) FROM shadow_decisions d
         JOIN shadow_suggestions s ON s.id = d.suggestion_id
         WHERE s.source='automation' AND s.automation_id=?1 AND d.decided_at >= ?2
           AND EXISTS(SELECT 1 FROM shadow_reviews r WHERE r.suggestion_id = d.suggestion_id)",
        rusqlite::params![automation_id, start],
        |r| r.get(0),
    )
}

/// 影子误报率（P0-5 修正口径，按 automation 作用域、decided_at 入窗）：
/// 分子 = decision∈{rejected,expired} 且存在 false_positive=1 复核；
/// **分母 = 已复核的已决建议**（不以未复核 rejection 稀释）；
/// 同时输出 review_coverage = 已复核/已决。
/// 返回 (decided, reviewed, fp, rate, coverage)。
pub fn shadow_false_positive_stats(
    store: &Store,
    automation_id: &str,
    window_days: i64,
) -> Result<(i64, i64, i64, f64, f64), Error> {
    let start = (timefmt::parse(&timefmt::now()).unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        - time::Duration::days(window_days))
    .format(&time::format_description::well_known::Rfc3339)
    .unwrap_or_default();
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN d.decision IN ('rejected','expired')
                       AND EXISTS(SELECT 1 FROM shadow_reviews r
                                  WHERE r.suggestion_id = d.suggestion_id AND r.false_positive = 1)
                      THEN 1 ELSE 0 END), 0)
             FROM shadow_decisions d
             JOIN shadow_suggestions s ON s.id = d.suggestion_id
             WHERE s.source='automation' AND s.automation_id=?1 AND d.decided_at >= ?2",
            rusqlite::params![automation_id, start],
            |r| {
                let decided: i64 = r.get(0)?;
                let fp: i64 = r.get(1)?;
                let reviewed = reviewed_of(conn, automation_id, window_days)?;
                let rate = if reviewed > 0 {
                    fp as f64 / reviewed as f64
                } else {
                    0.0
                };
                let coverage = if decided > 0 {
                    reviewed as f64 / decided as f64
                } else {
                    0.0
                };
                Ok((decided, reviewed, fp, rate, coverage))
            },
        )
        .map_err(Error::from)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-auto-{}-{}",
            std::process::id(),
            sg_store::ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        store
            .with_conn(|c| {
                c.execute_batch(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj','u','n','p','main','t');
                    INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                     VALUES ('wi','pj','t','','[]','requirements','t','t');",
                )
                .map_err(Error::from)?;
                Ok(())
            })
            .unwrap();
        store
    }

    // ---- P0-5：误报率口径 / 两窗迟滞 CAS / cooldown / durable intents ----

    fn automation_with_suggestions(
        store: &Store,
        decided_reviewed: &[(bool, bool)], // (第 i 条: decision 是否 rejected, 是否已复核)
        fp: &[usize],                      // fp=1 的复核集合
    ) -> String {
        let id = ids::new_id("auto");
        let now = timefmt::now();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO automations(id, key, interval_secs, next_fire_at, shadow_mode, created_at, updated_at)
                     VALUES (?1,'k',60,?2,0,?2,?2)",
                    rusqlite::params![id, now],
                )?;
                for (i, (rejected, reviewed)) in decided_reviewed.iter().enumerate() {
                    let sid = format!("shs_p5_{i}");
                    c.execute(
                        "INSERT INTO shadow_suggestions(id, source, automation_id, workitem_id, scope_key,
                             suggestion_type, suggestion_digest, content_json, input_state_digest, generated_at)
                         VALUES (?1,'automation',?2,NULL,?2,'automation_intent',?3,'{}','',?4)",
                        rusqlite::params![sid, id, format!("sha256:p5{i}"), now],
                    )?;
                    c.execute(
                        "INSERT INTO shadow_decisions(suggestion_id, decision, decided_by, decided_at, note)
                         VALUES (?1,?2,'o',?3,'')",
                        rusqlite::params![sid, if *rejected { "rejected" } else { "accepted" }, now],
                    )?;
                    if *reviewed {
                        let is_fp = fp.contains(&i);
                        c.execute(
                            "INSERT INTO shadow_reviews(id, suggestion_id, false_positive, reviewer, note, reviewed_at)
                             VALUES (?1,?2,?3,'qa','',?4)",
                            rusqlite::params![format!("shr_p5_{i}"), sid, is_fp as i64, now],
                        )?;
                    }
                }
                Ok(())
            })
            .unwrap();
        id
    }

    #[test]
    fn p5_stats_reviewed_denominator_and_coverage() {
        let store = setup();
        // 30 决定：29 accepted（10 复核）+ 1 rejected（已复核 fp=1）。
        let mut plan = vec![(false, true); 10];
        plan.extend(vec![(false, false); 19]);
        plan.push((true, true));
        let id = automation_with_suggestions(&store, &plan, &[29]);
        let (decided, reviewed, fp, rate, coverage) =
            shadow_false_positive_stats(&store, &id, 14).unwrap();
        assert_eq!((decided, reviewed, fp), (30, 11, 1));
        assert!(
            (rate - 1.0 / 11.0).abs() < 1e-9,
            "分母=已复核已决（{rate}）"
        );
        assert!(
            (coverage - 11.0 / 30.0).abs() < 1e-9,
            "coverage=已复核/已决"
        );
    }

    #[test]
    fn p5_two_window_hysteresis_cas_and_digest_idempotency() {
        let store = setup();
        // 窗一：decided=30 全复核（reviewed=30 达 min_sample、coverage=1.0）、
        // fp=6（20%>10%）→ 超阈值但 streak=1 不回退。
        let plan = [vec![(false, true); 24], vec![(true, true); 6]].concat();
        let id = automation_with_suggestions(&store, &plan, &(24..30).collect::<Vec<_>>());
        let s1 = shadow_false_positive_stats(&store, &id, 14).unwrap();
        let (breach1, fell1) = force_shadow_mode_v2(&store, &id, s1, 14).unwrap();
        assert!(breach1 && !fell1, "第一窗只累计 streak 不回退");
        let mode: i64 = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT shadow_mode FROM automations WHERE id=?1",
                    [&id],
                    |r| r.get(0),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(mode, 0, "仍在 live");
        // 同 digest 幂等：重复评估不重复计数。
        let (_b, f) = force_shadow_mode_v2(&store, &id, s1, 14).unwrap();
        assert!(!f, "同 metric digest 幂等跳过");
        let streak: i64 = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT bad_window_streak FROM automations WHERE id=?1",
                    [&id],
                    |r| r.get(0),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(streak, 1, "幂等跳过不叠加 streak");
        // 窗二（新 digest：新增 1 条 fp 复核 → 7/31）→ streak=2 → 回退 + cooldown + transition + 单通知。
        let plan2 = [plan, vec![(true, true); 1]].concat();
        let mut fp2 = (24..30).collect::<Vec<_>>();
        fp2.push(30);
        store.with_conn(|c| {
            let sid = "shs_p5_30";
            c.execute(
                "INSERT INTO shadow_suggestions(id, source, automation_id, workitem_id, scope_key,
                     suggestion_type, suggestion_digest, content_json, input_state_digest, generated_at)
                 VALUES (?1,'automation',?2,NULL,?2,'automation_intent','sha256:p5b','{}','',?3)",
                rusqlite::params![sid, id, timefmt::now()],
            )?;
            c.execute(
                "INSERT INTO shadow_decisions(suggestion_id, decision, decided_by, decided_at, note)
                 VALUES (?1,'rejected','o',?2,'')",
                rusqlite::params![sid, timefmt::now()],
            )?;
            c.execute(
                "INSERT INTO shadow_reviews(id, suggestion_id, false_positive, reviewer, note, reviewed_at)
                 VALUES ('shr_p5_30',?1,1,'qa','',?2)",
                rusqlite::params![sid, timefmt::now()],
            )?;
            Ok(())
        })
        .unwrap();
        let _ = plan2;
        let s2 = shadow_false_positive_stats(&store, &id, 14).unwrap();
        let (breach2, fell2) = force_shadow_mode_v2(&store, &id, s2, 14).unwrap();
        assert!(breach2 && fell2, "第二窗回退");
        let (mode, cooldown): (i64, String) = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT shadow_mode, cooldown_started_at FROM automations WHERE id=?1",
                    [&id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(mode, 1, "已回 shadow");
        assert!(!cooldown.is_empty(), "cooldown 已置");
        let transitions: i64 = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM automation_policy_transitions WHERE automation_id=?1",
                    [&id],
                    |r| r.get(0),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(transitions, 1, "回退只有一条 transition");
        let notes: i64 = store.with_conn(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM notification_outbox WHERE automation_id=?1 AND kind='automation_blocked'",
                [&id],
                |r| r.get(0),
            )
            .unwrap())
        })
        .unwrap();
        assert_eq!(notes, 1, "回退只发一条通知（幂等键唯一）");
        // 同 digest 重复评估 → 幂等（transition/通知不增）。
        force_shadow_mode_v2(&store, &id, s2, 14).unwrap();
        let transitions2: i64 = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM automation_policy_transitions WHERE automation_id=?1",
                    [&id],
                    |r| r.get(0),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(transitions2, 1);
        // cooldown 生效。
        assert!(
            cooldown_until(&store, &id).unwrap().is_some(),
            "cooldown 窗口内"
        );
    }

    #[test]
    fn p5_cas_drift_does_not_override_user_config() {
        let store = setup();
        // 两窗超阈值后，手工并发改 revision（模拟用户同时改配置）→ CAS 不命中不覆盖。
        let plan = vec![(true, true); 30];
        let id = automation_with_suggestions(&store, &plan, &(0..30).collect::<Vec<_>>());
        let s = shadow_false_positive_stats(&store, &id, 14).unwrap();
        let _ = force_shadow_mode_v2(&store, &id, s, 14).unwrap(); // streak=1
                                                                   // 用户并发动作：自行切回 shadow（revision+1 且 shadow_mode=1）——
                                                                   // 系统 CAS（AND shadow_mode=0）不再命中，不得覆盖/再推进 revision。
        store.with_conn(|c| {
            c.execute(
                "UPDATE automations SET revision=revision+1, shadow_mode=1, interval_secs=120, updated_at=?1 WHERE id=?2",
                [timefmt::now(), id.clone()],
            )?;
            Ok(())
        })
        .unwrap();
        // 新 digest（再补一条 fp）→ streak 应到 2，但 CAS 漂移 → 不回退。
        store.with_conn(|c| {
            let sid = "shs_p5_cas";
            c.execute(
                "INSERT INTO shadow_suggestions(id, source, automation_id, workitem_id, scope_key,
                     suggestion_type, suggestion_digest, content_json, input_state_digest, generated_at)
                 VALUES (?1,'automation',?2,NULL,?2,'automation_intent','sha256:cas','{}','',?3)",
                rusqlite::params![sid, id, timefmt::now()],
            )?;
            c.execute(
                "INSERT INTO shadow_decisions(suggestion_id, decision, decided_by, decided_at, note)
                 VALUES (?1,'rejected','o',?2,'')",
                rusqlite::params![sid, timefmt::now()],
            )?;
            c.execute(
                "INSERT INTO shadow_reviews(id, suggestion_id, false_positive, reviewer, note, reviewed_at)
                 VALUES ('shr_p5_cas',?1,1,'qa','',?2)",
                rusqlite::params![sid, timefmt::now()],
            )?;
            Ok(())
        })
        .unwrap();
        let s2 = shadow_false_positive_stats(&store, &id, 14).unwrap();
        let (_b, fell) = force_shadow_mode_v2(&store, &id, s2, 14).unwrap();
        assert!(!fell, "CAS 不命中（用户已自行切 shadow）不重复回退");
        let (mode, revision): (i64, i64) = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT shadow_mode, revision FROM automations WHERE id=?1",
                    [&id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(mode, 1, "用户配置保持");
        let transitions: i64 = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM automation_policy_transitions WHERE automation_id=?1",
                    [&id],
                    |r| r.get(0),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(transitions, 0, "系统不写 transition（未覆盖用户配置）");
        let _ = revision;
    }

    #[test]
    fn p5_enqueue_run_intent_idempotent() {
        let store = setup();
        let id = ids::new_id("auto");
        let now = timefmt::now();
        store.with_conn(|c| {
            c.execute(
                "INSERT INTO automations(id, key, interval_secs, next_fire_at, created_at, updated_at)
                 VALUES (?1,'k',60,?2,?2,?2)",
                rusqlite::params![id, now],
            )?;
            Ok(())
        })
        .unwrap();
        let intent = serde_json::json!({"goal": "巡检"});
        let r1 = enqueue_run_intent(
            &store,
            &id,
            None,
            "2026-09-08T12:00:00Z",
            &intent,
            "g",
            "sp2",
        )
        .unwrap();
        let r2 = enqueue_run_intent(
            &store,
            &id,
            None,
            "2026-09-08T12:00:00Z",
            &intent,
            "g",
            "sp2",
        )
        .unwrap();
        assert_eq!(r1, r2, "同 scheduled_for 幂等");
        let n: i64 = store
            .with_conn(|c| {
                Ok(
                    c.query_row("SELECT COUNT(*) FROM run_intents", [], |r| r.get(0))
                        .unwrap(),
                )
            })
            .unwrap();
        assert_eq!(n, 1);
        let (state, attempt): (String, i64) = store
            .with_conn(|c| {
                Ok(
                    c.query_row("SELECT state, attempt FROM run_intents", [], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })
                    .unwrap(),
                )
            })
            .unwrap();
        assert_eq!((state.as_str(), attempt), ("pending", 0));
    }

    #[test]
    fn receipt_dedupes_across_repeated_claims() {
        let store = setup();
        let a = create(
            &store,
            "hourly-check",
            Some("pj"),
            Some("wi"),
            r#"{"kind":"run"}"#,
            3600,
            "skip",
            "skip",
            None,
            "admin",
        )
        .unwrap();
        // 同一 scheduled_for 两次认领：第二次 None（M6 退出标准：重复启动/时钟跳变不重复执行）。
        let r1 = claim_receipt(&store, &a.id, "2026-09-06T12:00:00.000Z").unwrap();
        assert!(r1.is_some());
        let r2 = claim_receipt(&store, &a.id, "2026-09-06T12:00:00.000Z").unwrap();
        assert!(r2.is_none(), "重复认领必须被 receipt 去重");
        // 不同 scheduled_for 正常。
        let r3 = claim_receipt(&store, &a.id, "2026-09-06T13:00:00.000Z").unwrap();
        assert!(r3.is_some());
    }

    #[test]
    fn misfire_and_overlap_policies() {
        let store = setup();
        let skip = create(
            &store,
            "m-skip",
            None,
            Some("wi"),
            "{}",
            60,
            "skip",
            "skip",
            None,
            "a",
        )
        .unwrap();
        let once = create(
            &store,
            "m-once",
            None,
            Some("wi"),
            "{}",
            60,
            "run_once",
            "queue_one",
            None,
            "a",
        )
        .unwrap();
        // 未过期（now=当前，next_fire=now+60s，未越过整周期）→ 触发。
        assert!(misfire_decision(
            &skip.misfire_policy,
            &skip.next_fire_at,
            &timefmt::now(),
            skip.interval_secs
        )
        .unwrap());
        // 已过期（now=2099 远超 next_fire+interval）→ skip 不触发，run_once 补一次。
        assert!(!misfire_decision(
            &skip.misfire_policy,
            &skip.next_fire_at,
            "2099-01-01T00:00:00.000Z",
            skip.interval_secs
        )
        .unwrap());
        // 毫秒级抖动（now=next_fire+1s < next_fire+interval）→ 不算 misfire（评审 P1）。
        let jittered = timefmt::parse(&skip.next_fire_at)
            .map(|t| timefmt::format_now(t + TimeDuration::seconds(1)))
            .unwrap_or_default();
        assert!(misfire_decision(
            &skip.misfire_policy,
            &skip.next_fire_at,
            &jittered,
            skip.interval_secs
        )
        .unwrap());
        assert!(misfire_decision(
            &once.misfire_policy,
            &once.next_fire_at,
            "2099-01-01T00:00:00.000Z",
            once.interval_secs
        )
        .unwrap());
        // overlap：skip 策略下存在未推进 fired → 拒绝；queue_one 允许一单排队。
        let _ = claim_receipt(&store, &skip.id, "2026-09-06T12:00:00.000Z").unwrap();
        assert!(!overlap_allowed(&store, &skip.id, "skip").unwrap());
        assert!(overlap_allowed(&store, &once.id, "queue_one").unwrap());
    }

    #[test]
    fn reconcile_marks_orphan_fired_failed() {
        let store = setup();
        let a = create(
            &store,
            "m-orph",
            None,
            Some("wi"),
            "{}",
            3600,
            "skip",
            "skip",
            None,
            "a",
        )
        .unwrap();
        let _ = claim_receipt(&store, &a.id, "2026-09-06T12:00:00.000Z").unwrap();
        // 模拟崩溃遗留（fired 未推进）→ reconciliation 收敛。
        let n = reconcile_orphans(&store).unwrap();
        assert_eq!(n, 1);
        // 通知 outbox 有 automation_blocked。
        let notes: i64 = store
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM notification_outbox WHERE kind='automation_blocked'",
                    [],
                    |r| r.get(0),
                )
                .map_err(Error::from)
            })
            .unwrap();
        assert_eq!(notes, 1);
        // 再跑一次为空（幂等）。
        assert_eq!(reconcile_orphans(&store).unwrap(), 0);
    }

    #[test]
    fn status_cas_and_reschedule() {
        let store = setup();
        let a = create(
            &store,
            "m-cas",
            None,
            Some("wi"),
            "{}",
            60,
            "skip",
            "skip",
            None,
            "a",
        )
        .unwrap();
        // CAS 暂停。
        let paused = set_status(&store, &a.id, "paused", a.revision).unwrap();
        assert_eq!(paused.status, "paused");
        // 旧 revision 再改 → 冲突。
        assert!(set_status(&store, &a.id, "active", a.revision).is_err());
        // reschedule 向前钳制。
        reschedule(&store, &a.id, "2099-01-01T00:00:00.000Z").unwrap();
        let after = store.with_conn(|conn| row(conn, &a.id)).unwrap();
        assert!(after.next_fire_at.as_str() > "2099-01-01T00:00:00.000Z");
    }
}
