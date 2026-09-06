//! 自动化调度核心（EvoFlow 方案 M6-02 / ADR-039 §6.13）：
//! durable schedule + TriggerReceipt 幂等（同 automation+scheduled_for 唯一）
//! + misfire（skip/run_once/catch_up_one，禁止无限补跑）+ overlap（skip/queue_one）
//! + 暂停/恢复。只产 intent：副作用检查（grant/预算/审批）在消费侧。

use serde::Serialize;
use sg_store::{ids, timefmt, Error, Store};
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
    pub revision: i64,
}

fn row(conn: &rusqlite::Connection, id: &str) -> Result<AutomationRecord, Error> {
    conn.query_row(
        "SELECT id, key, COALESCE(project_id,''), COALESCE(workitem_id,''), intent_json,
                interval_secs, next_fire_at, misfire_policy, overlap_policy,
                COALESCE(autonomy_grant_id,''), status, revision
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
                revision: r.get(11)?,
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

/// misfire 处理（§6.13）：到期时 now 已越过 next_fire 一个周期以上。
/// skip → 标记 skipped_misfire；run_once/catch_up_one → 触发一次（上限 1，禁止无限补跑）。
/// 返回是否应触发。
pub fn misfire_decision(
    misfire_policy: &str,
    next_fire_at: &str,
    now: &str,
) -> Result<bool, Error> {
    let overdue = now > next_fire_at;
    match misfire_policy {
        "skip" => Ok(!overdue),
        "run_once" | "catch_up_one" => Ok(true), // 补跑一次，重调度向前（reschedule 已钳 now）
        _ => Ok(!overdue),
    }
}

/// overlap 闸：上一触发尚无终态（fired 未推进）时按策略 skip/queue_one。
pub fn overlap_allowed(store: &Store, automation_id: &str, policy: &str) -> Result<bool, Error> {
    let pending: i64 = store.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM automation_runs
             WHERE automation_id=?1 AND status='fired'",
            [automation_id],
            |r| r.get(0),
        )
        .map_err(Error::from)
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
        // 未过期（now=当前，next_fire=now+60s）→ 触发。
        assert!(
            misfire_decision(&skip.misfire_policy, &skip.next_fire_at, &timefmt::now()).unwrap()
        );
        // 已过期（now=2099 远超 next_fire）→ skip 不触发，run_once 补一次。
        assert!(!misfire_decision(
            &skip.misfire_policy,
            &skip.next_fire_at,
            "2099-01-01T00:00:00.000Z"
        )
        .unwrap());
        assert!(misfire_decision(
            &once.misfire_policy,
            &once.next_fire_at,
            "2099-01-01T00:00:00.000Z"
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
