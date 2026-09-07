//! 证据与通关文牒（FR-PLT-010 行为等价）。
use serde::Serialize;
use sg_store::{ids, objects, outbox, timefmt, Error, Store};

#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    pub id: String,
    pub workitem_id: String,
    pub gate: String,
    pub kind: String,
    pub title: String,
    pub object_sha256: String,
    pub payload: String,
    pub source: String,
    pub verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_by: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct RecordInput<'a> {
    pub workitem_id: &'a str,
    pub gate: &'a str,
    pub kind: &'a str,
    pub title: &'a str,
    pub content: Option<&'a str>,
    pub payload: &'a str,
    pub source: &'a str,
}

pub fn record(store: &Store, input: &RecordInput<'_>) -> Result<Evidence, Error> {
    let RecordInput {
        workitem_id,
        gate,
        kind,
        title,
        content,
        payload,
        source,
    } = *input;
    if workitem_id.is_empty() || gate.is_empty() || kind.is_empty() {
        return Err(Error::Message("workitem/gate/kind required".into()));
    }
    let source = if source.is_empty() { "local" } else { source };
    let object_sha = match content {
        Some(text) if !text.is_empty() => {
            let info = objects::put(store, text.as_bytes(), objects::PutOptions::default())
                .map_err(|e| Error::Message(format!("store evidence: {e}")))?;
            info.sha256
        }
        _ => String::new(),
    };
    let id = ids::new_id("ev");
    let payload_body = if payload.is_empty() { "{}" } else { payload };
    let created_at = store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO evidences(id, workitem_id, gate, kind, title, object_sha256, payload, source, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![id, workitem_id, gate, kind, title, object_sha, payload_body, source, timefmt::now()],
        )?;
        let created: String = conn.query_row("SELECT created_at FROM evidences WHERE id=?1", [&id], |r| r.get(0))?;
        Ok(created)
    })?;
    outbox::emit(
        store,
        "evidence",
        &id,
        "evidence.recorded",
        serde_json::json!({"workitemId": workitem_id, "gate": gate, "kind": kind}),
    )?;
    Ok(Evidence {
        id,
        workitem_id: workitem_id.into(),
        gate: gate.into(),
        kind: kind.into(),
        title: title.into(),
        object_sha256: object_sha,
        payload: payload_body.into(),
        source: source.into(),
        verified: false,
        verified_at: None,
        verified_by: None,
        created_at,
    })
}

pub fn verify(store: &Store, evidence_id: &str, verified_by: &str) -> Result<(), Error> {
    let changed = store.with_conn(|conn| {
        conn.execute(
            "UPDATE evidences SET verified=1, verified_at=?1, verified_by=?2 WHERE id=?3 AND verified=0",
            rusqlite::params![timefmt::now(), verified_by, evidence_id],
        )?;
        Ok(conn.changes())
    })?;
    if changed == 0 {
        return Err(Error::Message(
            "evidence not found or already verified".into(),
        ));
    }
    Ok(())
}

pub fn list(store: &Store, workitem_id: &str, gate: Option<&str>) -> Result<Vec<Evidence>, Error> {
    store.with_conn(|conn| {
        let (sql, params): (&str, Vec<Box<dyn rusqlite::ToSql>>) = match gate {
            Some(g) => (
                "SELECT id, workitem_id, gate, kind, title, COALESCE(object_sha256,''), payload, source, verified,
                        COALESCE(verified_at,''), COALESCE(verified_by,''), created_at
                 FROM evidences WHERE workitem_id=?1 AND gate=?2 ORDER BY created_at",
                vec![Box::new(workitem_id.to_string()), Box::new(g.to_string())],
            ),
            None => (
                "SELECT id, workitem_id, gate, kind, title, COALESCE(object_sha256,''), payload, source, verified,
                        COALESCE(verified_at,''), COALESCE(verified_by,''), created_at
                 FROM evidences WHERE workitem_id=?1 ORDER BY created_at",
                vec![Box::new(workitem_id.to_string())],
            ),
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
            let verified_at: String = r.get(9)?;
            let verified_by: String = r.get(10)?;
            Ok(Evidence {
                id: r.get(0)?, workitem_id: r.get(1)?, gate: r.get(2)?, kind: r.get(3)?,
                title: r.get(4)?, object_sha256: r.get(5)?, payload: r.get(6)?, source: r.get(7)?,
                verified: r.get::<_, i64>(8)? == 1,
                verified_at: if verified_at.is_empty() { None } else { Some(verified_at) },
                verified_by: if verified_by.is_empty() { None } else { Some(verified_by) },
                created_at: r.get(11)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

/// 关卡结论摘要（WP-8 outcome 语义）：
/// - `passed`：技术评估通过（passed=true）；
/// - `skipped_with_waiver`：经 gate_skip 审批跳过（**passed 布尔不篡改，照实
///   false**；完成谓词以 outcome 为准，旧消费者保守视为未全通过 = fail-closed）；
/// - `failed` / `unknown`：未通过/未评估（阻断签发）。
#[derive(Debug, Clone, Serialize)]
pub struct GateSummary {
    pub gate: String,
    pub passed: bool,
    pub evidence_ids: Vec<String>,
    pub failed_inputs: Vec<String>,
    /// 缺省 "passed"；skipped 关为 "skipped_with_waiver"（passed 照实 false）。
    pub outcome: String,
    pub waiver_approval_id: String,
}

impl GateSummary {
    /// 完成谓词（WP-8）：每关 outcome ∈ {passed, skipped_with_waiver}。
    pub fn completes(&self) -> bool {
        self.outcome == "passed" || self.outcome == "skipped_with_waiver"
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Passport {
    pub id: String,
    pub workitem_id: String,
    pub object_sha256: String,
    pub inputs_sha256: String,
    pub created_at: String,
    pub gates: Vec<GateSummary>,
}

/// 签发通关文牒：实例全部关卡必须 passed（M1-06：关卡数按实例定义，不再假设 6）。
pub fn issue_passport(
    store: &Store,
    workitem_id: &str,
    gates: &[GateSummary],
    shared_summary: &str,
) -> Result<Passport, Error> {
    if gates.is_empty() {
        return Err(Error::Message(
            "passport_incomplete_gates: 实例无关卡结论".into(),
        ));
    }
    // 完成谓词：outcome ∈ {passed, skipped_with_waiver}；矛盾摘要（outcome=passed
    // 而 passed=false）按 fail-closed 拒签。
    if gates
        .iter()
        .any(|g| !g.completes() || (g.outcome == "passed" && !g.passed))
    {
        return Err(Error::Message(
            "passport_incomplete_gates: 存在未通过且未豁免跳过的关卡".into(),
        ));
    }
    let evidences = list(store, workitem_id, None)?;
    let hashes: Vec<&str> = evidences
        .iter()
        .filter(|e| !e.object_sha256.is_empty())
        .map(|e| e.object_sha256.as_str())
        .collect();
    let summary = serde_json::json!({
        "workitemId": workitem_id,
        "gateResults": gates,
        "evidenceHashes": hashes,
        "shared": if shared_summary.is_empty() { serde_json::Value::Null } else { serde_json::from_str::<serde_json::Value>(shared_summary).unwrap_or(serde_json::Value::Null) },
        "issuedAt": timefmt::now(),
    });
    let body = serde_json::to_string_pretty(&summary).map_err(|e| Error::Message(e.to_string()))?;
    let info = objects::put(store, body.as_bytes(), objects::PutOptions::default())?;

    let id = ids::new_id("psp");
    let created_at = store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO passports(id, workitem_id, object_sha256, inputs_sha256, shared_summary, created_at)
             VALUES (?1,?2,?3,?4,?5,?6)",
            rusqlite::params![id, workitem_id, info.sha256.clone(), info.sha256.clone(), summary.to_string(), timefmt::now()],
        )?;
        for (i, g) in gates.iter().enumerate() {
            conn.execute(
                "INSERT INTO passport_gates(passport_id, seq, gate, passed, evidence_ids, failed_inputs, outcome, waiver_approval_id)
                 VALUES (?1,?2,?3,?4,?5,'[]',?6,?7)",
                rusqlite::params![
                    id,
                    i as i64,
                    g.gate,
                    g.passed,
                    serde_json::to_string(&g.evidence_ids).unwrap_or_else(|_| "[]".into()),
                    g.outcome,
                    g.waiver_approval_id
                ],
            )?;
        }
        let created: String = conn.query_row("SELECT created_at FROM passports WHERE id=?1", [&id], |r| r.get(0))?;
        Ok(created)
    })?;
    outbox::emit(
        store,
        "passport",
        &id,
        "passport.issued",
        serde_json::json!({"workitemId": workitem_id, "sha256": info.sha256.clone()}),
    )?;
    Ok(Passport {
        id,
        workitem_id: workitem_id.into(),
        object_sha256: info.sha256.clone(),
        inputs_sha256: info.sha256,
        created_at,
        gates: gates.to_vec(),
    })
}

pub fn latest_passport(store: &Store, workitem_id: &str) -> Result<Option<Passport>, Error> {
    // WP-9：被 rework 登记失效的护照不再返回（要求重签）；空登记表逐字等价。
    let row: Option<String> = store.with_conn(|conn| {
        let result: rusqlite::Result<String> = conn.query_row(
            "SELECT id FROM passports WHERE workitem_id=?1
               AND id NOT IN (SELECT fact_id FROM rework_affected_facts WHERE fact_kind='passport')
             ORDER BY created_at DESC LIMIT 1",
            [workitem_id],
            |r| r.get(0),
        );
        Ok(result.ok())
    })?;
    match row {
        Some(id) => {
            let gates = store.with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT gate, passed, evidence_ids, outcome, waiver_approval_id FROM passport_gates WHERE passport_id=?1 ORDER BY seq",
                )?;
                let rows = stmt.query_map([&id], |r| {
                    Ok(GateSummary {
                        gate: r.get(0)?,
                        passed: r.get::<_, i64>(1)? == 1,
                        evidence_ids: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
                        failed_inputs: vec![],
                        outcome: r.get(3)?,
                        waiver_approval_id: r.get(4)?,
                    })
                })?;
                let mut out = Vec::new();
                for row in rows {
                    out.push(row?);
                }
                Ok(out)
            })?;
            let base = store.with_conn(|conn| {
                conn.query_row(
                    "SELECT id, object_sha256, inputs_sha256, created_at FROM passports WHERE id=?1",
                    [&id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)),
                )
                .map_err(|_| Error::Message("passport_not_found".into()))
            })?;
            Ok(Some(Passport {
                id: base.0,
                workitem_id: workitem_id.into(),
                object_sha256: base.1,
                inputs_sha256: base.2,
                created_at: base.3,
                gates,
            }))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Store {
        let dir =
            std::env::temp_dir().join(format!("sg-ev-{}-{}", std::process::id(), ids::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        store.with_conn(|c| {
            c.execute("INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at) VALUES ('pj','u','n','p','main',?1)", [timefmt::now()])?;
            c.execute("INSERT INTO workitems(id, project_id, title, created_at, updated_at) VALUES ('wi','pj','t',?1,?1)", [timefmt::now()])?;
            Ok(())
        }).unwrap();
        store
    }

    fn all_gates_pass() -> Vec<GateSummary> {
        [
            "requirements",
            "design",
            "development",
            "testing",
            "deployment",
            "verification",
        ]
        .iter()
        .map(|g| GateSummary {
            gate: g.to_string(),
            passed: true,
            evidence_ids: vec![],
            failed_inputs: vec![],
            outcome: "passed".into(),
            waiver_approval_id: String::new(),
        })
        .collect()
    }

    #[test]
    fn record_verify_list() {
        let s = setup();
        let ev = record(
            &s,
            &RecordInput {
                workitem_id: "wi",
                gate: "testing",
                kind: "test_report",
                title: "JUnit",
                content: Some("<tests tests='12' failures='0'/>"),
                payload: "{}",
                source: "gitlab",
            },
        )
        .unwrap();
        assert!(!ev.object_sha256.is_empty());
        verify(&s, &ev.id, "qa").unwrap();
        assert!(verify(&s, &ev.id, "qa").is_err(), "重复复验拒绝");
        let items = list(&s, "wi", Some("testing")).unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].verified);
    }

    #[test]
    fn passport_requires_all_six_passed() {
        let s = setup();
        // M1-06：关卡数按实例定义——空序列拒绝、存在未通过关拒绝；
        // 全部通过的 N 关（任意 N≥1）均可签发（5 关实例合法）。
        assert!(issue_passport(&s, "wi", &[], "").is_err());
        let five = &all_gates_pass()[..5];
        assert!(issue_passport(&s, "wi", five, "").is_ok());
        let mut gates = all_gates_pass();
        gates[3].passed = false;
        assert!(issue_passport(&s, "wi", &gates, "").is_err());
        let p = issue_passport(&s, "wi", &all_gates_pass(), "").unwrap();
        assert_eq!(p.gates.len(), 6);
        // WP-8：skipped_with_waiver 关（passed 照实 false）可签发，outcome/豁免审批落库。
        let mut skipped = all_gates_pass();
        skipped[3].passed = false;
        skipped[3].outcome = "skipped_with_waiver".into();
        skipped[3].waiver_approval_id = "appr_waiver".into();
        let ps = issue_passport(&s, "wi", &skipped, "").unwrap();
        assert_eq!(ps.gates.len(), 6);
        let latest = latest_passport(&s, "wi").unwrap().unwrap();
        let skipped_gate = latest.gates.iter().find(|g| g.gate == "testing").unwrap();
        assert_eq!(skipped_gate.outcome, "skipped_with_waiver");
        assert!(!skipped_gate.passed);
        assert_eq!(skipped_gate.waiver_approval_id, "appr_waiver");
    }
}
