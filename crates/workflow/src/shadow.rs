//! 通用 suggestion/observation 基础设施（EvoFlow WP-8a；表 0043，供 WP-8/10/12 消费）。
//!
//! 三表分工（修正 v1.1 单表自相矛盾）：建议**不可变**（无决定字段）→ 决定
//! **append-only**（一建议一终局，PK=suggestion_id；重放同决定返回既有行，
//! 不一致 = Conflict）→ 复核 **append-only**（误报率分子来源）。
//! 写侧由领域路径生成（WP-8 fast-track 判定、WP-12 automation tick，shadow_mode
//! 列随 0043 就位、语义 WP-12 激活）；本模块提供写入 API 与 decide/review/observations。
//!
//! 误报率口径：分子 = decision∈{rejected, expired} 且存在 false_positive=1 复核；
//! 分母 = 同过滤域已决定建议数。

use serde::Serialize;
use sg_store::{ids, timefmt, Error, Store};

const SOURCES: [&str; 2] = ["fast_track", "automation"];
const DECISIONS: [&str; 4] = ["accepted", "rejected", "ignored", "expired"];

#[derive(Debug, Clone, Serialize)]
pub struct Suggestion {
    pub id: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workitem_id: Option<String>,
    pub suggestion_type: String,
    pub suggestion_digest: String,
    pub content: serde_json::Value,
    pub hypothetical_action_digest: String,
    pub policy_version: String,
    pub model: String,
    pub prompt_version: String,
    pub generated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    pub suggestion_id: String,
    pub decision: String,
    pub decided_by: String,
    pub decided_at: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Review {
    pub id: String,
    pub suggestion_id: String,
    pub false_positive: bool,
    pub reviewer: String,
    pub note: String,
    pub reviewed_at: String,
}

/// 建议与其决定/复核的联合读形（observations 条目）。
#[derive(Debug, Clone, Serialize)]
pub struct Observation {
    #[serde(flatten)]
    pub suggestion: Suggestion,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<Decision>,
    pub reviews: Vec<Review>,
}

/// 误报率口径（同过滤域）：分子 = decision∈{rejected,expired} 且有 false_positive=1
/// 复核；分母 = 已决定建议数。
#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub total: i64,
    pub decided: i64,
    pub false_positive_candidates: i64,
    pub false_positives: i64,
    pub false_positive_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Observations {
    pub items: Vec<Observation>,
    pub stats: Stats,
}

/// 写入建议（不可变事实；领域路径专用——fast-track 判定 / automation tick）。
#[derive(Debug, Clone)]
pub struct SuggestionInput<'a> {
    pub source: &'a str,
    pub automation_id: Option<&'a str>,
    pub workitem_id: Option<&'a str>,
    pub suggestion_type: &'a str,
    pub suggestion_digest: &'a str,
    pub content: serde_json::Value,
    pub hypothetical_action_digest: &'a str,
    pub policy_version: &'a str,
    pub model: &'a str,
    pub prompt_version: &'a str,
}

const SUGGESTION_COLS: &str = "id, source, automation_id, workitem_id, suggestion_type,
        suggestion_digest, content_json, hypothetical_action_digest, policy_version,
        model, prompt_version, generated_at";

fn suggestion_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<Suggestion> {
    Ok(Suggestion {
        id: r.get(0)?,
        source: r.get(1)?,
        automation_id: r.get(2)?,
        workitem_id: r.get(3)?,
        suggestion_type: r.get(4)?,
        suggestion_digest: r.get(5)?,
        content: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
        hypothetical_action_digest: r.get(7)?,
        policy_version: r.get(8)?,
        model: r.get(9)?,
        prompt_version: r.get(10)?,
        generated_at: r.get(11)?,
    })
}

pub fn record(store: &Store, input: &SuggestionInput<'_>) -> Result<Suggestion, Error> {
    if !SOURCES.contains(&input.source) {
        return Err(Error::Message(format!(
            "shadow_suggestion_invalid: source 须为 {:?}（得到 {:?}）",
            SOURCES, input.source
        )));
    }
    if input.suggestion_type.trim().is_empty() || input.suggestion_digest.trim().is_empty() {
        return Err(Error::Message(
            "shadow_suggestion_invalid: suggestion_type/suggestion_digest 必填".into(),
        ));
    }
    let id = ids::new_id("shs");
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO shadow_suggestions
             (id, source, automation_id, workitem_id, suggestion_type, suggestion_digest,
              content_json, hypothetical_action_digest, policy_version, model, prompt_version, generated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            rusqlite::params![
                id,
                input.source,
                input.automation_id,
                input.workitem_id,
                input.suggestion_type.trim(),
                input.suggestion_digest,
                serde_json::to_string(&input.content).unwrap_or_else(|_| "{}".into()),
                input.hypothetical_action_digest,
                input.policy_version,
                input.model,
                input.prompt_version,
                now
            ],
        )?;
        Ok(())
    })?;
    store.with_conn(|conn| {
        conn.query_row(
            &format!("SELECT {SUGGESTION_COLS} FROM shadow_suggestions WHERE id=?1"),
            [&id],
            suggestion_from,
        )
        .map_err(Error::from)
    })
}

/// 按 id 取建议（WP-8 应用钩子定位用）。
pub fn get(store: &Store, suggestion_id: &str) -> Result<Suggestion, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            &format!("SELECT {SUGGESTION_COLS} FROM shadow_suggestions WHERE id=?1"),
            [suggestion_id],
            suggestion_from,
        )
        .map_err(|_| Error::Message(format!("shadow_suggestion_missing: {suggestion_id}")))
    })
}

/// 指定来源+工作项、且已获指定决定的建议（新→旧；WP-8 fast-track 应用判定用）。
pub fn decided_suggestions(
    store: &Store,
    source: &str,
    workitem_id: &str,
    decision: &str,
) -> Result<Vec<Suggestion>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {SUGGESTION_COLS} FROM shadow_suggestions s
             JOIN shadow_decisions d ON d.suggestion_id = s.id
             WHERE s.source=?1 AND s.workitem_id=?2 AND d.decision=?3
             ORDER BY s.generated_at DESC, s.rowid DESC"
        ))?;
        let rows = stmt.query_map(
            rusqlite::params![source, workitem_id, decision],
            suggestion_from,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    })
}

/// 决定（append-only，一建议一终局）：重放同决定幂等返回既有行；不一致 = Conflict。
pub fn decide(
    store: &Store,
    suggestion_id: &str,
    decision: &str,
    decided_by: &str,
    note: &str,
) -> Result<Decision, Error> {
    if !DECISIONS.contains(&decision) {
        return Err(Error::Message(format!(
            "shadow_decision_invalid: decision 须为 {:?}（得到 {decision:?}）",
            DECISIONS
        )));
    }
    let now = timefmt::now();
    store.with_conn(|conn| {
        let exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM shadow_suggestions WHERE id=?1",
            [suggestion_id],
            |r| r.get(0),
        )
        .map_err(Error::from)?;
        if exists == 0 {
            return Err(Error::Message(format!(
                "shadow_suggestion_missing: {suggestion_id}"
            )));
        }
        let existing = match conn.query_row(
            "SELECT suggestion_id, decision, decided_by, decided_at, note
             FROM shadow_decisions WHERE suggestion_id=?1",
            [suggestion_id],
            |r| {
                Ok(Decision {
                    suggestion_id: r.get(0)?,
                    decision: r.get(1)?,
                    decided_by: r.get(2)?,
                    decided_at: r.get(3)?,
                    note: r.get(4)?,
                })
            },
        ) {
            Ok(d) => Some(d),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(other) => return Err(other.into()),
        };
        if let Some(d) = existing {
            if d.decision == decision {
                return Ok(d); // 幂等重放
            }
            return Err(Error::Message(format!(
                "shadow_decision_conflict: 建议 {suggestion_id} 已决定为 {}（收到 {decision}），不可改判",
                d.decision
            )));
        }
        conn.execute(
            "INSERT INTO shadow_decisions(suggestion_id, decision, decided_by, decided_at, note)
             VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![suggestion_id, decision, decided_by, now, note],
        )?;
        Ok(Decision {
            suggestion_id: suggestion_id.into(),
            decision: decision.into(),
            decided_by: decided_by.into(),
            decided_at: now,
            note: note.into(),
        })
    })
}

/// 复核（append-only；前置：建议已有决定——FK 引用 shadow_decisions）。
pub fn review(
    store: &Store,
    suggestion_id: &str,
    false_positive: bool,
    reviewer: &str,
    note: &str,
) -> Result<Review, Error> {
    let id = ids::new_id("shr");
    let now = timefmt::now();
    store.with_conn(|conn| {
        let exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM shadow_suggestions WHERE id=?1",
            [suggestion_id],
            |r| r.get(0),
        )
        .map_err(Error::from)?;
        if exists == 0 {
            return Err(Error::Message(format!(
                "shadow_suggestion_missing: {suggestion_id}"
            )));
        }
        let decided: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM shadow_decisions WHERE suggestion_id=?1",
                [suggestion_id],
                |r| r.get(0),
            )
            .map_err(Error::from)?;
        if decided == 0 {
            return Err(Error::Message(format!(
                "shadow_decision_missing: 建议 {suggestion_id} 未决定，不可复核"
            )));
        }
        conn.execute(
            "INSERT INTO shadow_reviews(id, suggestion_id, false_positive, reviewer, note, reviewed_at)
             VALUES (?1,?2,?3,?4,?5,?6)",
            rusqlite::params![id, suggestion_id, false_positive as i64, reviewer, note, now],
        )?;
        Ok(Review {
            id,
            suggestion_id: suggestion_id.into(),
            false_positive,
            reviewer: reviewer.into(),
            note: note.into(),
            reviewed_at: now,
        })
    })
}

/// 观察面（可按 source / automation 过滤；新→旧）+ 误报率口径统计。
pub fn observations(
    store: &Store,
    source: Option<&str>,
    automation_id: Option<&str>,
) -> Result<Observations, Error> {
    if let Some(s) = source {
        if !SOURCES.contains(&s) {
            return Err(Error::Message(format!(
                "shadow_suggestion_invalid: source 须为 {:?}（得到 {s:?}）",
                SOURCES
            )));
        }
    }
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {SUGGESTION_COLS} FROM shadow_suggestions
             WHERE (?1='' OR source=?1) AND (?2='' OR automation_id=?2)
             ORDER BY generated_at DESC, rowid DESC"
        ))?;
        let rows = stmt.query_map(
            rusqlite::params![source.unwrap_or(""), automation_id.unwrap_or("")],
            suggestion_from,
        )?;
        let mut items = Vec::new();
        for s in rows {
            let s = s?;
            let decision = match conn.query_row(
                "SELECT suggestion_id, decision, decided_by, decided_at, note
                 FROM shadow_decisions WHERE suggestion_id=?1",
                [&s.id],
                |r| {
                    Ok(Decision {
                        suggestion_id: r.get(0)?,
                        decision: r.get(1)?,
                        decided_by: r.get(2)?,
                        decided_at: r.get(3)?,
                        note: r.get(4)?,
                    })
                },
            ) {
                Ok(d) => Some(d),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(other) => return Err(other.into()),
            };
            let mut stmt_r = conn.prepare(
                "SELECT id, suggestion_id, false_positive, reviewer, note, reviewed_at
                 FROM shadow_reviews WHERE suggestion_id=?1 ORDER BY reviewed_at, rowid",
            )?;
            let reviews = stmt_r
                .query_map([&s.id], |r| {
                    Ok(Review {
                        id: r.get(0)?,
                        suggestion_id: r.get(1)?,
                        false_positive: r.get::<_, i64>(2)? != 0,
                        reviewer: r.get(3)?,
                        note: r.get(4)?,
                        reviewed_at: r.get(5)?,
                    })
                })
                .map_err(Error::from)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(Error::from)?;
            items.push(Observation {
                suggestion: s,
                decision,
                reviews,
            });
        }
        // 误报率口径（同过滤域）。
        let mut stats = Stats {
            total: items.len() as i64,
            decided: 0,
            false_positive_candidates: 0,
            false_positives: 0,
            false_positive_rate: 0.0,
        };
        for o in &items {
            if let Some(d) = &o.decision {
                stats.decided += 1;
                if d.decision == "rejected" || d.decision == "expired" {
                    stats.false_positive_candidates += 1;
                    if o.reviews.iter().any(|rv| rv.false_positive) {
                        stats.false_positives += 1;
                    }
                }
            }
        }
        if stats.decided > 0 {
            stats.false_positive_rate = stats.false_positives as f64 / stats.decided as f64;
        }
        Ok(Observations { items, stats })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-shadow-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO automations(id, key, interval_secs, next_fire_at, created_at, updated_at)
                     VALUES ('aut_t','shadow-t',60,'2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        store
    }

    fn input(digest: &'static str) -> SuggestionInput<'static> {
        SuggestionInput {
            source: "fast_track",
            automation_id: None,
            workitem_id: None,
            suggestion_type: "gate_skip",
            suggestion_digest: digest,
            content: json!({"gate": "testing"}),
            hypothetical_action_digest: "sha256:hh",
            policy_version: "pv1",
            model: "test-model",
            prompt_version: "pp1",
        }
    }

    #[test]
    fn write_decide_review_full_chain_with_replay_and_conflict() {
        let store = setup();
        // 写入（含 automation 外键形态）。
        let mut inp = input("d1");
        inp.automation_id = Some("aut_t");
        let s1 = record(&store, &inp).unwrap();
        assert_eq!(s1.source, "fast_track");
        assert_eq!(s1.automation_id.as_deref(), Some("aut_t"));
        // 非法 source / 空 digest 拒。
        let mut bad = input("d2");
        bad.source = "magic";
        assert!(record(&store, &bad).is_err());
        let mut nodigest = input("");
        nodigest.suggestion_digest = "";
        assert!(record(&store, &nodigest).is_err());
        // 决定 → 重放幂等 → 改判 Conflict。
        let d = decide(&store, &s1.id, "accepted", "owner", "采纳").unwrap();
        assert_eq!(d.decision, "accepted");
        let replay = decide(&store, &s1.id, "accepted", "owner", "").unwrap();
        assert_eq!(replay.decided_at, d.decided_at);
        assert!(decide(&store, &s1.id, "rejected", "owner", "").is_err());
        // 未决定不可复核（用独立未决定建议验证）；决定后复核 append-only。
        let s_undecided = record(&store, &input("d9")).unwrap();
        assert!(review(&store, &s_undecided.id, true, "qa", "").is_err());
        let r1 = review(&store, &s1.id, false, "qa", "非误报").unwrap();
        let r2 = review(&store, &s1.id, true, "qa2", "").unwrap();
        assert!(!r1.false_positive && r2.false_positive);
        // 不存在建议决定/复核拒绝。
        assert!(decide(&store, "shs_none", "accepted", "o", "").is_err());
        // 非法 decision 枚举拒绝。
        assert!(decide(&store, &s1.id, "maybe", "o", "").is_err());
    }

    #[test]
    fn false_positive_rate_follows_definition() {
        let store = setup();
        let s1 = record(&store, &input("e1")).unwrap();
        let s2 = record(&store, &input("e2")).unwrap();
        let _s3 = record(&store, &input("e3")).unwrap();
        // s1: rejected + fp=1 复核 → 分子；s2: rejected + fp=0 复核 → 候选非分子；s3 未决定。
        decide(&store, &s1.id, "rejected", "o", "").unwrap();
        review(&store, &s1.id, true, "qa", "").unwrap();
        decide(&store, &s2.id, "rejected", "o", "").unwrap();
        review(&store, &s2.id, false, "qa", "").unwrap();
        let obs = observations(&store, None, None).unwrap();
        assert_eq!(obs.stats.total, 3);
        assert_eq!(obs.stats.decided, 2);
        assert_eq!(obs.stats.false_positive_candidates, 2);
        assert_eq!(obs.stats.false_positives, 1);
        assert!((obs.stats.false_positive_rate - 0.5).abs() < 1e-9);
        // 按 source 过滤：automation 源为空集。
        let auto = observations(&store, Some("automation"), None).unwrap();
        assert_eq!(auto.stats.total, 0);
        // 非法 source 拒。
        assert!(observations(&store, Some("magic"), None).is_err());
    }
}
