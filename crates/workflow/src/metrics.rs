//! A5 指标投影（EvoFlow WP-10，纯读；口径=RDWS v1.2 修正表）。
//!
//! 六指标全部快照值 + 最小样本 insufficient_data：
//! - 孤儿率 = gaps().orphanCount / 该 workitem 全部 provenance 节点数
//!   （列表截断只影响展示不影响计数；节点<5 → insufficient）；
//! - 回环率 = completed rework / 期间有放行的 workitem（按 completed_at 入窗，
//!   cancelled/blocked 不计；action_digest UNIQUE 保证重放不重复计数；workitem<5 →
//!   insufficient），附 reason_code 分布；
//! - 审批通过率（subject_type 分层）= approved/(approved+rejected)，decided_at
//!   30 天窗；pending/expired/changes_requested 各自独立单列（n<10 → insufficient）；
//! - 审批延迟 = median/P95(decided_at−created_at)，分层同上；
//!   rubber_stamp_suspect：median<2s 且通过率≥95% 且 n≥20；
//! - 工具提案批准率 = approvals(subject_type='tool_proposal') approved/(approved+
//!   rejected)（n<20 → insufficient）；提案执行侧状态不入批准率；
//! - AI 建议采纳率 = shadow_decisions accepted/已决定（min 30）。
//!
//! scope 目前仅 `global`；窗口固定 30 天（decided_at/completed_at 入窗）。

use serde_json::{json, Value};
use sg_store::{timefmt, Error, Store};

const WINDOW_DAYS: i64 = 30;
fn window_start() -> Result<String, Error> {
    let now = timefmt::now();
    let t = timefmt::parse(&now)
        .ok_or_else(|| Error::Message("metrics_invalid: 时间解析失败".into()))?
        - time::Duration::days(WINDOW_DAYS);
    Ok(t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or(now))
}

fn rate(approved: i64, rejected: i64) -> Option<f64> {
    let n = approved + rejected;
    if n == 0 {
        None
    } else {
        Some(approved as f64 / n as f64)
    }
}

fn percentile(sorted: &[f64], q: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let idx = ((sorted.len() as f64 * q).ceil() as usize).saturating_sub(1);
    sorted.get(idx).copied()
}

fn seconds_between(start: &str, end: &str) -> Option<f64> {
    let s = timefmt::parse(start)?.unix_timestamp();
    let e = timefmt::parse(end)?.unix_timestamp();
    Some((e - s) as f64)
}

/// 审批分层（decided 行按 decided_at 入窗计率与延迟；pending 全量单列；
/// expired 按 created_at 入窗单列；changes_requested 独立单列）。
fn approval_layers(store: &Store, window: &str) -> Result<Value, Error> {
    let rows: Vec<(String, String, String, String)> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT subject_type, status, created_at, COALESCE(decided_at, '')
             FROM approvals
             WHERE (status IN ('approved','rejected','changes_requested') AND decided_at >= ?1)
                OR status = 'pending'
                OR (status = 'expired' AND created_at >= ?1)",
        )?;
        let rows = stmt.query_map([window], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    })?;
    #[derive(Default)]
    struct Layer {
        approved: i64,
        rejected: i64,
        pending: i64,
        expired: i64,
        changes_requested: i64,
        latencies: Vec<f64>,
    }
    let mut layers: std::collections::BTreeMap<String, Layer> = std::collections::BTreeMap::new();
    for (subject, status, created_at, decided_at) in &rows {
        let l = layers.entry(subject.clone()).or_default();
        match status.as_str() {
            "approved" => {
                l.approved += 1;
                if let Some(d) = seconds_between(created_at, decided_at) {
                    l.latencies.push(d);
                }
            }
            "rejected" => {
                l.rejected += 1;
                if let Some(d) = seconds_between(created_at, decided_at) {
                    l.latencies.push(d);
                }
            }
            "changes_requested" => l.changes_requested += 1,
            "pending" => l.pending += 1,
            "expired" => l.expired += 1,
            _ => {}
        }
    }
    let mut out = serde_json::Map::new();
    for (subject, l) in layers {
        let n = l.approved + l.rejected;
        let r = rate(l.approved, l.rejected);
        let pass_rate_v = r.map(|x| json!(x)).unwrap_or(Value::Null);
        let mut lat = l.latencies.clone();
        lat.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = percentile(&lat, 0.5);
        let p95 = percentile(&lat, 0.95);
        let median_v = median.map(|x| json!(x)).unwrap_or(Value::Null);
        let p95_v = p95.map(|x| json!(x)).unwrap_or(Value::Null);
        // 橡皮图章嫌疑：median<2s 且通过率≥95% 且 n≥20。
        let stamp = matches!(median, Some(med) if med < 2.0)
            && matches!(r, Some(rr) if rr >= 0.95)
            && n >= 20;
        let min = if subject == "tool_proposal" { 20 } else { 10 };
        out.insert(
            subject,
            json!({
                "approved": l.approved,
                "rejected": l.rejected,
                "pending": l.pending,
                "expired": l.expired,
                "changesRequested": l.changes_requested,
                "passRate": pass_rate_v,
                "sample": n,
                "insufficientData": n < min,
                "latencyMedianSecs": median_v,
                "latencyP95Secs": p95_v,
                "rubberStampSuspect": stamp,
            }),
        );
    }
    Ok(json!(out))
}

/// 六指标总览（scope 目前仅 global）。
pub fn overview(store: &Store, scope: &str) -> Result<Value, Error> {
    if scope != "global" {
        return Err(Error::Message(
            "metrics_invalid: scope 目前仅支持 global".into(),
        ));
    }
    let window = window_start()?;

    // 1) 孤儿率（全局聚合；节点<5 → insufficient）。
    let (total_nodes, total_orphans, per_workitem): (i64, i64, Vec<(String, i64, i64)>) = {
        let nodes: Vec<(String, i64)> = store.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT workitem_id, COUNT(*) FROM provenance_nodes GROUP BY workitem_id",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
        })?;
        let orphans: Vec<(String, i64)> = store.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT workitem_id, COUNT(*) FROM provenance_nodes n
                 WHERE NOT EXISTS (SELECT 1 FROM provenance_edges e WHERE e.from_node_id = n.id)
                   AND NOT EXISTS (SELECT 1 FROM provenance_edges e WHERE e.to_node_id = n.id)
                 GROUP BY workitem_id",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
        })?;
        let om: std::collections::BTreeMap<String, i64> = orphans.into_iter().collect();
        let mut per = Vec::new();
        let mut tn = 0i64;
        let mut to = 0i64;
        for (wi, n) in nodes {
            let o = om.get(&wi).copied().unwrap_or(0);
            tn += n;
            to += o;
            per.push((wi, o, n));
        }
        (tn, to, per)
    };

    // 2) 回环率（completed_at 入窗；cancelled/blocked 不计）。
    let completed_rework: i64 = store.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM rework_operations WHERE state='completed' AND completed_at >= ?1",
            [&window],
            |r| r.get(0),
        )
        .map_err(Error::from)
    })?;
    let released_workitems: i64 = store.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(DISTINCT a.workitem_id) FROM gate_release_requests r
             JOIN stage_attempts a ON a.id = r.stage_attempt_id
             WHERE r.state='approved' AND r.decided_at >= ?1",
            [&window],
            |r| r.get(0),
        )
        .map_err(Error::from)
    })?;
    let reason_dist: Vec<(String, i64)> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT reason_code, COUNT(*) FROM rework_operations
             WHERE state='completed' AND completed_at >= ?1 GROUP BY reason_code",
        )?;
        let rows = stmt.query_map([&window], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    })?;

    // 6) AI 建议采纳率。
    let (decided_suggestions, accepted_suggestions): (i64, i64) = store.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(CASE WHEN decision='accepted' THEN 1 ELSE 0 END),0)
             FROM shadow_decisions WHERE decided_at >= ?1",
            [&window],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(Error::from)
    })?;

    let orphan_rate_v = if total_nodes > 0 {
        json!(total_orphans as f64 / total_nodes as f64)
    } else {
        Value::Null
    };
    let loop_rate_v = if released_workitems > 0 {
        json!(completed_rework as f64 / released_workitems as f64)
    } else {
        Value::Null
    };
    let adoption_rate_v = if decided_suggestions > 0 {
        json!(accepted_suggestions as f64 / decided_suggestions as f64)
    } else {
        Value::Null
    };
    Ok(json!({
        "scope": scope,
        "windowDays": WINDOW_DAYS,
        "orphanRate": {
            "totalNodes": total_nodes,
            "orphanCount": total_orphans,
            "rate": orphan_rate_v,
            "sample": total_nodes,
            "insufficientData": total_nodes < 5,
            "perWorkitem": per_workitem.iter().map(|(wi, o, n)| json!({
                "workitemId": wi, "orphanCount": o, "nodeCount": n,
                "insufficientData": *n < 5,
            })).collect::<Vec<_>>(),
        },
        "loopRate": {
            "completedReworks": completed_rework,
            "workitemsWithReleases": released_workitems,
            "rate": loop_rate_v,
            "sample": released_workitems,
            "insufficientData": released_workitems < 5,
            "reasonDistribution": reason_dist.into_iter().map(|(k, v)| json!({"reasonCode": k, "count": v})).collect::<Vec<_>>(),
        },
        "approvalLayers": approval_layers(store, &window)?,
        "aiSuggestionAdoption": {
            "decided": decided_suggestions,
            "accepted": accepted_suggestions,
            "rate": adoption_rate_v,
            "sample": decided_suggestions,
            "insufficientData": decided_suggestions < 30,
        },
    }))
}

/// WP-11 纯读先行：Triage 聚合（每 workitem 的 gaps 概要）。
pub fn triage_list(store: &Store) -> Result<Value, Error> {
    let workitems: Vec<String> = store.with_conn(|conn| {
        let mut stmt =
            conn.prepare("SELECT id FROM workitems ORDER BY created_at DESC, id DESC LIMIT 200")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    })?;
    let mut items = Vec::new();
    for wi in workitems {
        let gaps = sg_provenance::gaps(store, &wi)?;
        items.push(json!({
            "workitemId": wi,
            "orphanCount": gaps["orphanCount"],
            "unverifiedCount": gaps["unverifiedCount"],
            "uncoveredCount": gaps["uncoveredItemCount"],
        }));
    }
    Ok(json!({"items": items}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sg_store::ids;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-metrics-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj','u','n','p','main',?1)",
                    [timefmt::now()],
                )?;
                Ok(())
            })
            .unwrap();
        store
    }

    fn days_ago(days: i64) -> String {
        let t = timefmt::parse(&timefmt::now()).unwrap() - time::Duration::days(days);
        t.format(&time::format_description::well_known::Rfc3339)
            .unwrap()
    }

    fn workitem(store: &Store, id: &str) {
        let _ = store.with_conn(|c| {
            c.execute(
                "INSERT INTO workitems(id, project_id, title, created_at, updated_at)
                 VALUES (?1,'pj','指标任务',?2,?2)",
                rusqlite::params![id, timefmt::now()],
            )?;
            Ok(())
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn approval(
        store: &Store,
        id: &str,
        subject: &str,
        status: &str,
        created: &str,
        decided: &str,
        wi: Option<&str>,
    ) {
        let _ = store.with_conn(|c| {
            c.execute(
                "INSERT INTO approvals(id, subject_type, subject_id, workitem_id, action_digest, risk, status,
                                        requested_by, expires_at, reason, created_at, decided_at, decided_by)
                 VALUES (?1,?2,?3,?4,'d','low',?5,'local','','',?6,?7,?8)",
                rusqlite::params![id, subject, format!("s_{id}"), wi, status, created, decided, if decided.is_empty() { None } else { Some("owner") }],
            )
            .unwrap();
            Ok(())
        });
    }

    /// 回环分母链：attempt + gate_result + output_package + approved release_request。
    fn released_workitem(store: &Store, wi: &str, decided: &str) {
        let _ = store.with_conn(|c| {
            c.execute(
                "INSERT INTO stage_attempts(id, workitem_id, gate, attempt_no, state, created_at, updated_at)
                 VALUES (?1,?2,'development',1,'approved',?3,?3)",
                rusqlite::params![format!("att_{wi}"), wi, timefmt::now()],
            )
            .unwrap();
            c.execute(
                "INSERT INTO gate_results(id, workitem_id, gate, inputs, result, computed_at)
                 VALUES (?1,?2,'development','{}','{}',?3)",
                rusqlite::params![format!("gr_{wi}"), wi, timefmt::now()],
            )
            .unwrap();
            c.execute(
                "INSERT INTO stage_output_packages(id, stage_attempt_id, package_no, manifest_object_sha256, digest, gate_evaluation_id, created_at)
                 VALUES (?1,?2,1,'sha','d',?3,?4)",
                rusqlite::params![format!("pkg_{wi}"), format!("att_{wi}"), format!("gr_{wi}"), timefmt::now()],
            )
            .unwrap();
            c.execute(
                "INSERT INTO gate_release_requests(id, stage_attempt_id, output_package_id, release_digest, state, decided_at, created_at)
                 VALUES (?1,?2,?3,'d','approved',?4,?5)",
                rusqlite::params![format!("grr_{wi}"), format!("att_{wi}"), format!("pkg_{wi}"), decided, timefmt::now()],
            )
            .unwrap();
            Ok(())
        });
    }

    fn completed_rework(store: &Store, id: &str, wi: &str, reason: &str, completed: &str) {
        let _ = store.with_conn(|c| {
            c.execute(
                "INSERT INTO rework_operations(id, workitem_id, from_gate, target_gate, reason_code,
                     current_state_digest, state, action_digest, completed_at, created_at, updated_at)
                 VALUES (?1,?2,'development','design',?3,'cas','completed',?4,?5,?6,?6)",
                rusqlite::params![id, wi, reason, format!("d_{id}"), completed, timefmt::now()],
            )
            .unwrap();
            Ok(())
        });
    }

    #[test]
    fn overview_rates_windows_and_insufficient() {
        let store = setup();
        for i in 0..5 {
            workitem(&store, &format!("wi{i}"));
        }
        // 审批分层：gate_release 窗内 3 approved + 1 rejected（n=4 <10 → insufficient）。
        for i in 0..3 {
            approval(
                &store,
                &format!("gr{i}"),
                "gate_release",
                "approved",
                &days_ago(1),
                &days_ago(1),
                Some("wi0"),
            );
        }
        approval(
            &store,
            "gr9",
            "gate_release",
            "rejected",
            &days_ago(1),
            &days_ago(1),
            Some("wi0"),
        );
        // 窗外（40 天前）不入窗。
        approval(
            &store,
            "grold",
            "gate_release",
            "approved",
            &days_ago(40),
            &days_ago(40),
            Some("wi1"),
        );
        // tool_proposal：20 秒级延迟全批 → 橡皮图章嫌疑。
        for i in 0..20 {
            approval(
                &store,
                &format!("tp{i}"),
                "tool_proposal",
                "approved",
                &days_ago(1),
                &days_ago(1),
                Some("wi2"),
            );
        }
        // 回环：wi0 窗内放行；1 completed（regression）+1 cancelled（不计）。
        released_workitem(&store, "wi0", &days_ago(2));
        completed_rework(&store, "rwk1", "wi0", "regression", &days_ago(1));
        let _ = store.with_conn(|c| {
            c.execute(
                "INSERT INTO rework_operations(id, workitem_id, from_gate, target_gate, reason_code,
                     current_state_digest, state, action_digest, created_at, updated_at)
                 VALUES ('rwk2','wi0','development','design','other','cas','cancelled','d2',?1,?1)",
                [timefmt::now()],
            )
            .unwrap();
            Ok(())
        });
        // 孤儿：wi3 三节点一条件边 → 1 孤儿。
        use sg_provenance::{node_type, relation, EdgeInput, NodeInput};
        let node = |store: &Store, key: &str, wi: &str| {
            sg_provenance::register_node(
                store,
                &NodeInput {
                    project_id: "",
                    workitem_id: wi,
                    node_type: node_type::STAGE_ATTEMPT,
                    entity_id: key,
                    content_digest: "",
                    verification_state: "verified",
                },
            )
            .unwrap()
        };
        node(&store, "n1", "wi3");
        node(&store, "n2", "wi3");
        node(&store, "n3", "wi3");
        sg_provenance::add_edge(
            &store,
            &EdgeInput {
                workitem_id: "wi3",
                from_node_type: node_type::STAGE_ATTEMPT,
                from_entity_id: "n1",
                relation: relation::DERIVED_FROM,
                to_node_type: node_type::STAGE_ATTEMPT,
                to_entity_id: "n2",
                stage_attempt_id: "",
                created_by_run_id: "",
            },
        )
        .unwrap();
        // AI 建议采纳：1 accepted（<30 → insufficient）。
        let _ = store.with_conn(|c| {
            c.execute(
                "INSERT INTO shadow_suggestions(id, source, workitem_id, suggestion_type, suggestion_digest, content_json, generated_at)
                 VALUES ('shs1','fast_track','wi0','gate_fast_track','d1','{}',?1)",
                [timefmt::now()],
            )
            .unwrap();
            c.execute(
                "INSERT INTO shadow_decisions(suggestion_id, decision, decided_by, decided_at, note)
                 VALUES ('shs1','accepted','owner',?1,'')",
                [timefmt::now()],
            )
            .unwrap();
            Ok(())
        });

        let ov = overview(&store, "global").unwrap();
        // 审批分层：gate_release。
        let gr = &ov["approvalLayers"]["gate_release"];
        assert_eq!(gr["approved"], json!(3));
        assert_eq!(gr["rejected"], json!(1));
        assert_eq!(gr["passRate"], json!(0.75));
        assert_eq!(gr["insufficientData"], json!(true));
        // 橡皮图章：20 全批、median≈0s → suspect。
        let tp = &ov["approvalLayers"]["tool_proposal"];
        assert_eq!(tp["approved"], json!(20));
        assert_eq!(tp["rubberStampSuspect"], json!(true));
        assert_eq!(tp["insufficientData"], json!(false));
        // 窗外审批不入窗：gate_release sample 恰为 4（grold 不计）。
        // 回环：cancelled 不计、rate=1/1、insufficient（workitem<5）。
        assert_eq!(ov["loopRate"]["completedReworks"], json!(1));
        assert_eq!(ov["loopRate"]["workitemsWithReleases"], json!(1));
        assert_eq!(ov["loopRate"]["insufficientData"], json!(true));
        assert_eq!(
            ov["loopRate"]["reasonDistribution"][0]["reasonCode"],
            json!("regression")
        );
        // 孤儿率：3 节点 1 孤儿。
        assert_eq!(ov["orphanRate"]["totalNodes"], json!(3));
        assert_eq!(ov["orphanRate"]["orphanCount"], json!(1));
        // 采纳率：<30 insufficient。
        assert_eq!(ov["aiSuggestionAdoption"]["decided"], json!(1));
        assert_eq!(ov["aiSuggestionAdoption"]["insufficientData"], json!(true));
        // scope 非法。
        assert!(overview(&store, "per-workitem").is_err());
    }

    #[test]
    fn triage_list_aggregates_gaps() {
        let store = setup();
        workitem(&store, "wi0");
        use sg_provenance::{node_type, NodeInput};
        sg_provenance::register_node(
            &store,
            &NodeInput {
                project_id: "",
                workitem_id: "wi0",
                node_type: node_type::STAGE_ATTEMPT,
                entity_id: "lone",
                content_digest: "",
                verification_state: "verified",
            },
        )
        .unwrap();
        let t = triage_list(&store).unwrap();
        let item = &t["items"][0];
        assert_eq!(item["workitemId"], json!("wi0"));
        assert_eq!(item["orphanCount"], json!(1));
    }
}
