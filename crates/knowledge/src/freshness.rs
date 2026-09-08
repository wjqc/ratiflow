//! B10 知识验证事实（EvoFlow WP-13；表 0049，本机 SQLite 权威；manifest 只声明策略）。
//!
//! 幂等 receipt 化：同 (stable_id, 被验证内容版本, outcome, verifier) 至多一条，
//! 重复人工复核原样返回既有行。新鲜度派生（纯函数，单测锚点）：
//! - 最近一次 `outcome=pass` 且 verified_input_digest == 当前 manifest
//!   contentSha256 → verified_at + intervalDays = next_due；
//! - 内容变更后旧验证立即不匹配 → `unverified_content_changed`；
//! - 无 pass 记录 / 策略未设定（v1 manifest）→ `unverified`；
//! - 过 due → `expired`。block 级未验证源进 Triage（衔接 WP-11）。

use serde::Serialize;
use sg_store::{ids, outbox, timefmt, Error, Store};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationSeverity {
    Info,
    Warn,
    Block,
}

impl VerificationSeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            VerificationSeverity::Info => "info",
            VerificationSeverity::Warn => "warn",
            VerificationSeverity::Block => "block",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct VerificationPolicy {
    pub interval_days: i64,
    pub severity: VerificationSeverity,
}

/// 策略 shape 严格（未知子字段 fail-closed，与 manifest 顶层同纪律）。
pub fn parse_verification_policy(v: &serde_json::Value) -> Result<VerificationPolicy, Error> {
    let obj = v
        .as_object()
        .ok_or_else(|| Error::Message("manifest_verification_policy_invalid".into()))?;
    let interval_days = obj
        .get("intervalDays")
        .and_then(|x| x.as_i64())
        .filter(|n| *n > 0)
        .ok_or_else(|| {
            Error::Message("manifest_verification_policy_invalid: intervalDays 须为正整数".into())
        })?;
    let severity = match obj.get("severity").and_then(|x| x.as_str()) {
        Some("info") => VerificationSeverity::Info,
        Some("warn") => VerificationSeverity::Warn,
        Some("block") => VerificationSeverity::Block,
        _ => {
            return Err(Error::Message(
                "manifest_verification_policy_invalid: severity 须为 info|warn|block".into(),
            ))
        }
    };
    if obj.len() != 2 {
        return Err(Error::Message(
            "manifest_verification_policy_invalid: 存在未知子字段".into(),
        ));
    }
    Ok(VerificationPolicy {
        interval_days,
        severity,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct Receipt {
    pub id: String,
    #[serde(rename = "projectId")]
    pub project_id: String,
    pub stable_id: String,
    #[serde(rename = "verifiedInputDigest")]
    pub verified_input_digest: String,
    pub verifier: String,
    #[serde(rename = "policyVersion")]
    pub policy_version: String,
    #[serde(rename = "verifiedAt")]
    pub verified_at: String,
    pub outcome: String,
    #[serde(rename = "evidenceRef")]
    pub evidence_ref: String,
}

fn receipt_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<Receipt> {
    Ok(Receipt {
        id: r.get(0)?,
        project_id: r.get(1)?,
        stable_id: r.get(2)?,
        verified_input_digest: r.get(3)?,
        verifier: r.get(4)?,
        policy_version: r.get(5)?,
        verified_at: r.get(6)?,
        outcome: r.get(7)?,
        evidence_ref: r.get(8)?,
    })
}

fn receipt_by_key(
    store: &Store,
    stable_id: &str,
    digest: &str,
    outcome: &str,
    verifier: &str,
) -> Result<Option<Receipt>, Error> {
    store.with_conn(|conn| {
        match conn.query_row(
            "SELECT id, project_id, stable_id, verified_input_digest, verifier, policy_version,
                    verified_at, outcome, evidence_ref
             FROM knowledge_verification_receipts
             WHERE stable_id=?1 AND verified_input_digest=?2 AND outcome=?3 AND verifier=?4",
            rusqlite::params![stable_id, digest, outcome, verifier],
            receipt_from,
        ) {
            Ok(r) => Ok(Some(r)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(other) => Err(other.into()),
        }
    })
}

/// 人工/复核入口（幂等 receipt 化）：verified_input_digest 取当前 manifest
/// contentSha256——同一内容重复验证返回既有 receipt，内容变更后产生新 receipt。
pub fn verify_source(
    store: &Store,
    project_id: &str,
    stable_id: &str,
    outcome: &str,
    verifier: &str,
    evidence_ref: &str,
) -> Result<serde_json::Value, Error> {
    if !matches!(outcome, "pass" | "fail" | "unknown") {
        return Err(Error::Message(format!(
            "knowledge_verify_invalid: outcome 须为 pass|fail|unknown（得到 {outcome:?}）"
        )));
    }
    if verifier.trim().is_empty() {
        return Err(Error::Message(
            "knowledge_verify_invalid: verifier 必填".into(),
        ));
    }
    let root = crate::reconcile::project_root(store, project_id)?;
    let manifests = crate::reconcile::load_source_manifests(&root)?;
    let (_path, m) = manifests
        .into_iter()
        .find(|(_, m)| m.stable_id == stable_id)
        .ok_or_else(|| {
            Error::Message(format!(
                "knowledge_source_missing: {stable_id}（project {project_id}）"
            ))
        })?;
    let digest = m.content_sha256.clone();
    if digest.is_empty() {
        return Err(Error::Message(
            "knowledge_verify_invalid: manifest 缺 contentSha256".into(),
        ));
    }
    // 幂等：同键已有 receipt → 原样返回。
    if let Some(existing) = receipt_by_key(store, stable_id, &digest, outcome, verifier)? {
        return Ok(serde_json::to_value(existing).unwrap_or_default());
    }
    let id = ids::new_id("kvr");
    let now = timefmt::now();
    let policy_version = m
        .verification_policy
        .as_ref()
        .map(|p| format!("v2:{}/{}d", p.severity.as_str(), p.interval_days))
        .unwrap_or_else(|| format!("v{}", m.schema_version));
    let inserted = store.with_conn(|conn| {
        conn.execute(
            "INSERT OR IGNORE INTO knowledge_verification_receipts
             (id, project_id, stable_id, verified_input_digest, verifier, policy_version,
              verified_at, outcome, evidence_ref, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?7)",
            rusqlite::params![
                id,
                project_id,
                stable_id,
                digest,
                verifier,
                policy_version,
                now,
                outcome,
                evidence_ref
            ],
        )?;
        Ok(conn.changes())
    })?;
    let receipt = if inserted == 0 {
        receipt_by_key(store, stable_id, &digest, outcome, verifier)?
            .ok_or_else(|| Error::Message("knowledge_verify_invalid: receipt 不可见".into()))?
    } else {
        store.with_conn(|conn| {
            conn.query_row(
                "SELECT id, project_id, stable_id, verified_input_digest, verifier, policy_version,
                        verified_at, outcome, evidence_ref
                 FROM knowledge_verification_receipts WHERE id=?1",
                [&id],
                receipt_from,
            )
            .map_err(Error::from)
        })?
    };
    outbox::emit(
        store,
        "knowledge",
        project_id,
        "knowledge.verified",
        serde_json::json!({
            "projectId": project_id,
            "stableId": stable_id,
            "verifiedInputDigest": digest,
            "outcome": outcome,
        }),
    )?;
    Ok(serde_json::to_value(receipt).unwrap_or_default())
}

/// 新鲜度派生（纯函数）：返回 (state, next_due_at)。
pub fn derive_freshness(
    now: &str,
    latest_pass: Option<(&str, &str)>,
    current_digest: &str,
    policy: Option<&VerificationPolicy>,
) -> (&'static str, Option<String>) {
    let Some((pass_digest, verified_at)) = latest_pass else {
        return ("unverified", None);
    };
    if pass_digest != current_digest {
        return ("unverified_content_changed", None);
    }
    let Some(policy) = policy else {
        return ("unverified", None); // v1 未设定策略：无法派生 due
    };
    let Some(t0) = timefmt::parse(verified_at) else {
        return ("unverified", None);
    };
    let Ok(due) = (t0 + time::Duration::days(policy.interval_days))
        .format(&time::format_description::well_known::Rfc3339)
    else {
        return ("unverified", None);
    };
    if now > due.as_str() {
        ("expired", Some(due))
    } else {
        ("verified", Some(due))
    }
}

/// knowledge.freshnessOverview：逐 source 的新鲜度（含策略与最近 pass receipt）。
pub fn freshness_overview(store: &Store, project_id: &str) -> Result<serde_json::Value, Error> {
    let root = crate::reconcile::project_root(store, project_id)?;
    let manifests = crate::reconcile::load_source_manifests(&root)?;
    let now = timefmt::now();
    let mut items = Vec::new();
    for (_path, m) in &manifests {
        let latest_pass: Option<(String, String)> = store.with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT verified_input_digest, verified_at FROM knowledge_verification_receipts
                     WHERE stable_id=?1 AND outcome='pass'
                     ORDER BY verified_at DESC, rowid DESC LIMIT 1",
                    [&m.stable_id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .ok())
        })?;
        let lp_ref = latest_pass.as_ref().map(|(d, t)| (d.as_str(), t.as_str()));
        let (state, next_due) = derive_freshness(
            &now,
            lp_ref,
            &m.content_sha256,
            m.verification_policy.as_ref(),
        );
        items.push(serde_json::json!({
            "stableId": m.stable_id,
            "enabled": m.enabled,
            "schemaVersion": m.schema_version,
            "contentOwner": m.content_owner,
            "contentSha256": m.content_sha256,
            "verificationPolicy": m.verification_policy,
            "state": state,
            "nextDueAt": next_due,
        }));
    }
    Ok(serde_json::json!({ "projectId": project_id, "items": items }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sg_store::ids;
    use std::path::PathBuf;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn setup() -> (Store, String, Tmp) {
        let dir = std::env::temp_dir().join(format!(
            "sg-kfresh-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(dir.join("knowledge/sources")).unwrap();
        let store = Store::open(dir.join("_db").parent().unwrap(), "test")
            .or_else(|_| Store::open(&dir, "test"))
            .unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, local_root, created_at)
                     VALUES ('pj','u','n','p','main',?1,?2)",
                    rusqlite::params![dir.to_string_lossy().to_string(), timefmt::now()],
                )
                .unwrap();
                Ok(())
            })
            .unwrap();
        (store, "pj".to_string(), Tmp(dir))
    }

    fn identity_of() -> (String, String) {
        let norm = crate::manifest::normalize_locator("docs/guide.md").unwrap();
        let (_digest, stable_id, slug) = crate::manifest::repo_path_identity(&norm, "指南");
        (stable_id, slug)
    }

    fn write_manifest(root: &std::path::Path, slug: &str, body: serde_json::Value) {
        let dir = root.join("knowledge/sources");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{slug}.json")), body.to_string()).unwrap();
    }

    fn manifest_body(content_sha: &str, schema_version: i64) -> serde_json::Value {
        let (stable_id, _slug) = identity_of();
        let mut v = json!({
            "contentSha256": content_sha,
            "enabled": true,
            "kind": "repo_path",
            "locator": "docs/guide.md",
            "name": "指南",
            "schemaVersion": schema_version,
            "stableId": stable_id,
        });
        if schema_version == 2 {
            v["contentOwner"] = json!("doc-team");
            v["verificationPolicy"] = json!({"intervalDays": 7, "severity": "block"});
        }
        v
    }

    #[test]
    fn verify_then_content_change_flips_freshness() {
        let (store, pj, tmp) = setup();
        let root = tmp.0.clone();
        let (stable_id, slug) = identity_of();
        // v2 manifest：策略 block/7 天；内容版本 v1。
        write_manifest(&root, &slug, manifest_body("sha_v1", 2));
        // 未验证 → unverified。
        let ov = freshness_overview(&store, &pj).unwrap();
        assert_eq!(ov["items"][0]["state"], json!("unverified"));
        // 验证（pass，锚定 sha_v1）→ verified + next_due = now+7d。
        let r = verify_source(&store, &pj, &stable_id, "pass", "owner", "evid-1").unwrap();
        assert_eq!(r["outcome"], json!("pass"));
        assert_eq!(r["verifiedInputDigest"], json!("sha_v1"));
        let ov = freshness_overview(&store, &pj).unwrap();
        assert_eq!(ov["items"][0]["state"], json!("verified"));
        assert!(ov["items"][0]["nextDueAt"].is_string());
        // 幂等：同内容同 outcome 同 verifier 重放 → 仍一条 receipt。
        verify_source(&store, &pj, &stable_id, "pass", "owner", "evid-1").unwrap();
        let n: i64 = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM knowledge_verification_receipts",
                    [],
                    |x| x.get(0),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(n, 1);
        // 内容变更（contentSha256 → sha_v2）：旧验证立即不匹配。
        write_manifest(&root, &slug, manifest_body("sha_v2", 2));
        let ov = freshness_overview(&store, &pj).unwrap();
        assert_eq!(ov["items"][0]["state"], json!("unverified_content_changed"));
        // 新内容重新验证 → 恢复 verified。
        verify_source(&store, &pj, &stable_id, "pass", "owner", "").unwrap();
        let ov = freshness_overview(&store, &pj).unwrap();
        assert_eq!(ov["items"][0]["state"], json!("verified"));
        // fail outcome 不改变 verified 判定（最近 pass 仍是 sha_v2）但留痕。
        verify_source(&store, &pj, &stable_id, "fail", "auditor", "").unwrap();
        let ov = freshness_overview(&store, &pj).unwrap();
        assert_eq!(ov["items"][0]["state"], json!("verified"));
    }

    #[test]
    fn v1_manifest_and_unknown_outcome_semantics() {
        let (store, pj, tmp) = setup();
        let root = tmp.0.clone();
        let (stable_id, slug) = identity_of();
        // v1：策略未设定 → 即便验证 pass 也无法派生 due（unverified）。
        write_manifest(&root, &slug, manifest_body("sha_v1", 1));
        verify_source(&store, &pj, &stable_id, "pass", "owner", "").unwrap();
        let ov = freshness_overview(&store, &pj).unwrap();
        assert_eq!(ov["items"][0]["state"], json!("unverified"));
        assert!(ov["items"][0]["nextDueAt"].is_null());
        // 非法 outcome / 未知 stable 拒绝。
        assert!(verify_source(&store, &pj, &stable_id, "maybe", "owner", "").is_err());
        assert!(verify_source(&store, &pj, "repopath-ghost", "pass", "owner", "").is_err());
        // 策略 shape 严格：未知子字段 / 非法 severity / 非法 interval。
        assert!(
            parse_verification_policy(&json!({"intervalDays": 7, "severity": "mega"})).is_err()
        );
        assert!(parse_verification_policy(
            &json!({"intervalDays": 7, "severity": "block", "extra": 1})
        )
        .is_err());
        assert!(
            parse_verification_policy(&json!({"intervalDays": 0, "severity": "block"})).is_err()
        );
    }

    #[test]
    fn derive_freshness_boundaries() {
        let policy = VerificationPolicy {
            interval_days: 7,
            severity: VerificationSeverity::Block,
        };
        // 无 pass → unverified。
        assert_eq!(
            derive_freshness("2026-09-08T00:00:00Z", None, "d", Some(&policy)).0,
            "unverified"
        );
        // digest 不匹配 → content_changed。
        let (st, due) = derive_freshness(
            "2026-09-08T00:00:00Z",
            Some(("old", "2026-09-01T00:00:00Z")),
            "new",
            Some(&policy),
        );
        assert_eq!(st, "unverified_content_changed");
        assert!(due.is_none());
        // 窗口内 → verified + due=+7d；过期 → expired。
        let (st, due) = derive_freshness(
            "2026-09-05T00:00:00Z",
            Some(("d", "2026-09-01T00:00:00Z")),
            "d",
            Some(&policy),
        );
        assert_eq!(st, "verified");
        assert_eq!(due.as_deref(), Some("2026-09-08T00:00:00Z"));
        let (st, _) = derive_freshness(
            "2026-09-20T00:00:00Z",
            Some(("d", "2026-09-01T00:00:00Z")),
            "d",
            Some(&policy),
        );
        assert_eq!(st, "expired");
        let _ = policy.severity.as_str();
    }
}
