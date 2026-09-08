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
    pub scope_key: String,
    pub suggestion_type: String,
    pub suggestion_digest: String,
    pub content: serde_json::Value,
    pub hypothetical_action_digest: String,
    pub policy_version: String,
    /// 服务端派生的输入状态摘要（P0-2：权威事实冻结进建议；漂移检测用）。
    pub input_state_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// 0051 存量标记：无法证明 scope/input digest → 只读，不参与 live 门槛。
    pub legacy: bool,
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

/// 误报率口径（P0-5 修正）：分子 = decision∈{rejected,expired} 且有
/// false_positive=1 复核；**分母 = 已复核的已决建议**（不以未复核 rejection 稀释）；
/// 同时输出 review_coverage = 已复核/已决。
#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub total: i64,
    pub decided: i64,
    pub reviewed: i64,
    pub review_coverage: f64,
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
/// 0051 v2：scope_key 由 source 推导（fast_track→workitem_id / automation→automation_id，
/// 表 CHECK 强制成对）；input_state_digest 为服务端派生输入摘要（authority 冻结）。
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
    pub input_state_digest: &'a str,
    /// None = 不设有效期（automation tick 建议；P0-5 语义激活时再定）。
    pub expires_at: Option<&'a str>,
    pub model: &'a str,
    pub prompt_version: &'a str,
}

const SUGGESTION_COLS: &str = "id, source, automation_id, workitem_id, scope_key, suggestion_type,
        suggestion_digest, content_json, hypothetical_action_digest, policy_version,
        input_state_digest, expires_at, legacy, model, prompt_version, generated_at";

fn suggestion_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<Suggestion> {
    Ok(Suggestion {
        id: r.get(0)?,
        source: r.get(1)?,
        automation_id: r.get(2)?,
        workitem_id: r.get(3)?,
        scope_key: r.get(4)?,
        suggestion_type: r.get(5)?,
        suggestion_digest: r.get(6)?,
        content: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default(),
        hypothetical_action_digest: r.get(8)?,
        policy_version: r.get(9)?,
        input_state_digest: r.get(10)?,
        expires_at: r.get(11)?,
        legacy: r.get::<_, i64>(12)? != 0,
        model: r.get(13)?,
        prompt_version: r.get(14)?,
        generated_at: r.get(15)?,
    })
}

/// scope_key 推导（与 0051 表 CHECK 同一规则；写前在 Rust 侧先给出可读错误）。
fn scope_key_of(input: &SuggestionInput<'_>) -> Result<String, Error> {
    match input.source {
        "fast_track" => match (input.workitem_id, input.automation_id) {
            (Some(w), None) if !w.is_empty() => Ok(w.to_string()),
            _ => Err(Error::Message(
                "shadow_suggestion_invalid: fast_track 建议必须携带 workitem_id 且不带 automation_id"
                    .into(),
            )),
        },
        "automation" => match (input.automation_id, input.workitem_id) {
            (Some(a), _) if !a.is_empty() => Ok(a.to_string()),
            _ => Err(Error::Message(
                "shadow_suggestion_invalid: automation 建议必须携带 automation_id".into(),
            )),
        },
        _ => Err(Error::Message(format!(
            "shadow_suggestion_invalid: source 须为 {:?}（得到 {:?}）",
            SOURCES, input.source
        ))),
    }
}

pub fn record(store: &Store, input: &SuggestionInput<'_>) -> Result<Suggestion, Error> {
    let scope_key = scope_key_of(input)?;
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
             (id, source, automation_id, workitem_id, scope_key, suggestion_type, suggestion_digest,
              content_json, hypothetical_action_digest, policy_version, input_state_digest,
              expires_at, legacy, model, prompt_version, generated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,0,?13,?14,?15)",
            rusqlite::params![
                id,
                input.source,
                input.automation_id,
                input.workitem_id,
                scope_key,
                input.suggestion_type.trim(),
                input.suggestion_digest,
                serde_json::to_string(&input.content).unwrap_or_else(|_| "{}".into()),
                input.hypothetical_action_digest,
                input.policy_version,
                input.input_state_digest,
                input.expires_at,
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

/// 过期同 scope 未决建议（0051 v2 语义；v1.4 §WP-8a）：
/// - 未决定（无 shadow_decisions 行）且 input_state_digest ≠ 当前权威摘要 → expired；
/// - 未决定且 expires_at 已过 → expired；
/// - 过期不改建议行，INSERT shadow_decisions('expired','system')，与人工决定竞态时
///   只有一方插入（PK 冲突忽略）；已决定建议永不过期；legacy 行不参与（只读投影）。
///
/// 返回本次过期条数。
pub fn expire_stale(
    store: &Store,
    source: &str,
    scope_key: &str,
    current_input_state_digest: &str,
) -> Result<i64, Error> {
    let now = timefmt::now();
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id FROM shadow_suggestions s
             WHERE s.source=?1 AND s.scope_key=?2 AND s.legacy=0
               AND NOT EXISTS (SELECT 1 FROM shadow_decisions d WHERE d.suggestion_id=s.id)
               AND (s.input_state_digest != ?3
                    OR (s.expires_at IS NOT NULL AND s.expires_at < ?4))",
        )?;
        let stale: Vec<String> = stmt
            .query_map(
                rusqlite::params![source, scope_key, current_input_state_digest, now],
                |r| r.get(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let mut n: i64 = 0;
        for id in stale {
            n += conn
                .execute(
                    "INSERT OR IGNORE INTO shadow_decisions(suggestion_id, decision, decided_by, decided_at, note)
                     VALUES (?1,'expired','system',?2,'输入状态漂移或超时，建议过期')",
                    rusqlite::params![id, timefmt::now()],
                )? as i64;
        }
        Ok(n)
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
        // 误报率口径（P0-5：分母=已复核已决）。
        let mut stats = Stats {
            total: items.len() as i64,
            decided: 0,
            reviewed: 0,
            review_coverage: 0.0,
            false_positive_candidates: 0,
            false_positives: 0,
            false_positive_rate: 0.0,
        };
        for o in &items {
            if let Some(d) = &o.decision {
                stats.decided += 1;
                let has_review = !o.reviews.is_empty();
                if has_review {
                    stats.reviewed += 1;
                }
                if d.decision == "rejected" || d.decision == "expired" {
                    stats.false_positive_candidates += 1;
                    if o.reviews.iter().any(|rv| rv.false_positive) {
                        stats.false_positives += 1;
                    }
                }
            }
        }
        if stats.decided > 0 {
            stats.review_coverage = stats.reviewed as f64 / stats.decided as f64;
        }
        if stats.reviewed > 0 {
            stats.false_positive_rate = stats.false_positives as f64 / stats.reviewed as f64;
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
                c.execute_batch(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj','u','n','p','main','2026-01-01T00:00:00.000Z');
                     INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                     VALUES ('wi','pj','t','','[]','build','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z');
                     INSERT INTO automations(id, key, interval_secs, next_fire_at, created_at, updated_at)
                     VALUES ('aut_t','shadow-t',60,'2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z');",
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
            workitem_id: Some("wi"),
            suggestion_type: "gate_skip",
            suggestion_digest: digest,
            content: json!({"gate": "testing"}),
            hypothetical_action_digest: "sha256:hh",
            policy_version: "pv1",
            input_state_digest: "sha256:state-1",
            expires_at: None,
            model: "test-model",
            prompt_version: "pp1",
        }
    }

    #[test]
    fn write_decide_review_full_chain_with_replay_and_conflict() {
        let store = setup();
        // 写入（含 automation 外键形态）。
        let mut inp = input("d1");
        inp.source = "automation";
        inp.automation_id = Some("aut_t");
        inp.workitem_id = None;
        let s1 = record(&store, &inp).unwrap();
        assert_eq!(s1.source, "automation");
        assert_eq!(s1.automation_id.as_deref(), Some("aut_t"));
        // 0051：scope_key 随 source 推导。
        assert_eq!(s1.scope_key, "aut_t");
        // 非法 source / 空 digest / scope 配对违约拒绝。
        let mut bad = input("d2");
        bad.source = "magic";
        assert!(record(&store, &bad).is_err());
        let mut nodigest = input("");
        nodigest.suggestion_digest = "";
        assert!(record(&store, &nodigest).is_err());
        let mut noscope = input("d3");
        noscope.workitem_id = None; // fast_track 无 workitem → 违约
        assert!(record(&store, &noscope).is_err());
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
    fn expire_stale_only_undecided_and_drifted() {
        let store = setup();
        // s1：输入摘要一致的未决建议 → 不过期。
        let s1 = record(&store, &input("e1")).unwrap();
        // s2：已决定（accepted）→ 永不过期。
        let mut s2 = input("e2");
        s2.suggestion_digest = "e2";
        let s2 = record(&store, &s2).unwrap();
        decide(&store, &s2.id, "accepted", "owner", "").unwrap();
        // s3：已过期（expires_at 过去）→ 到期过期。
        let mut s3 = input("e3");
        s3.suggestion_digest = "e3";
        s3.expires_at = Some("2020-01-01T00:00:00.000Z");
        let s3 = record(&store, &s3).unwrap();

        let n = expire_stale(&store, "fast_track", "wi", "sha256:state-1").unwrap();
        assert_eq!(n, 1, "只有 s3（超时）被过期");
        let conflict = decide(&store, &s3.id, "accepted", "owner", "")
            .unwrap_err()
            .to_string();
        assert!(
            conflict.contains("不可改判") && conflict.contains("expired"),
            "过期建议不可再人工决定：{conflict}"
        );

        // 输入漂移（state-2）：s1 未决且摘要不同 → 过期。
        let n2 = expire_stale(&store, "fast_track", "wi", "sha256:state-2").unwrap();
        assert_eq!(n2, 1, "摘要漂移使未决建议过期");
        let obs = observations(&store, Some("fast_track"), None).unwrap();
        let by_digest: std::collections::HashMap<&str, Option<&Decision>> = obs
            .items
            .iter()
            .map(|o| (o.suggestion.suggestion_digest.as_str(), o.decision.as_ref()))
            .collect();
        assert_eq!(by_digest["e1"].as_ref().unwrap().decision, "expired");
        assert_eq!(by_digest["e2"].as_ref().unwrap().decision, "accepted");
        assert_eq!(by_digest["e3"].as_ref().unwrap().decision, "expired");
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
