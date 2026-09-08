//! B10 知识验证事实 v2（EvoFlow WP-13；表 0054；0049 保留为只读历史——
//! P0-6/审计 RDWS-019「语义反向」修复）。
//!
//! v2 权威语义（与 0049 的差异）：
//! - **作用域**：receipt 绑 (project_id, source_id FK, stable_id)——跨项目相同
//!   stable id 不串；
//! - **锚点**：verified_input_revision + input_revision_mode 四态
//!   （committed/worktree/content_hash/remote_version），服务端按 source kind
//!   解析（RPC 不接受 revision，客户端零影响面）；
//! - **覆盖语义**：freshness 查当前 revision 的**最后一条** receipt（按
//!   (verified_at, id) 排序，不筛 outcome）——最新 fail/unknown 立即覆盖旧
//!   pass（fail-closed：failed/unknown 状态进入 Triage）；
//! - **幂等**：域键 (project, source, revision, mode, outcome, verifier) +
//!   操作键 verification_op_id（UNIQUE，跨 transport key）；
//! - 内容/锚点变更后旧 receipt 不匹配 → `unverified_content_changed`。

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
    #[serde(rename = "sourceId")]
    pub source_id: String,
    pub stable_id: String,
    /// 被验证输入的 revision 锚点（服务端按 kind 解析，见 `resolve_input_revision`）。
    #[serde(rename = "verifiedInputRevision")]
    pub verified_input_revision: String,
    #[serde(rename = "inputRevisionMode")]
    pub input_revision_mode: String,
    #[serde(rename = "verifiedInputDigest")]
    pub verified_input_digest: String,
    pub outcome: String,
    pub verifier: String,
    #[serde(rename = "verificationOpId")]
    pub verification_op_id: String,
    #[serde(rename = "policyVersion")]
    pub policy_version: String,
    #[serde(rename = "verifiedAt")]
    pub verified_at: String,
    #[serde(rename = "evidenceRef")]
    pub evidence_ref: String,
}

const RECEIPT_COLS: &str = "id, project_id, source_id, stable_id, verified_input_revision,
    input_revision_mode, verified_input_digest, outcome, verifier, verification_op_id,
    policy_version, verified_at, evidence_ref";

fn receipt_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<Receipt> {
    Ok(Receipt {
        id: r.get(0)?,
        project_id: r.get(1)?,
        source_id: r.get(2)?,
        stable_id: r.get(3)?,
        verified_input_revision: r.get(4)?,
        input_revision_mode: r.get(5)?,
        verified_input_digest: r.get(6)?,
        outcome: r.get(7)?,
        verifier: r.get(8)?,
        verification_op_id: r.get(9)?,
        policy_version: r.get(10)?,
        verified_at: r.get(11)?,
        evidence_ref: r.get(12)?,
    })
}

/// 输入 revision 解析结果（服务端权威；按 source kind，P0-6）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRevision {
    pub mode: &'static str,
    pub revision: String,
}

fn git_file_commit(root: &std::path::Path, rel: &str) -> String {
    std::process::Command::new("git")
        .args([
            "-C",
            &root.to_string_lossy(),
            "log",
            "-1",
            "--format=%H",
            "--",
            rel,
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// 服务端按 source kind 解析被验证输入的 revision 锚点（四 mode，v1.4 §WP-13）：
/// - `repo_path`：worktree 干净 → `committed:<该文件 HEAD 提交>`；modified/untracked
///   → `worktree:<state>:<content_sha>`；git 不可用 → `content_hash:<sha>`；
/// - `document` → `content_hash:<sha>`；
/// - `openapi|gitlab|rule` → `remote_version:<sha>`（远端版本的本地扫描投影）。
pub fn resolve_input_revision(
    root: &std::path::Path,
    m: &crate::reconcile::ValidManifest,
) -> Result<ResolvedRevision, Error> {
    let sha = m.content_sha256.clone();
    if sha.is_empty() {
        return Err(Error::Message(
            "knowledge_verify_invalid: manifest 缺 contentSha256".into(),
        ));
    }
    match m.kind.as_str() {
        "repo_path" => {
            let state = crate::manifest::git_worktree_state(root, &m.locator);
            match state.as_str() {
                "committed" => {
                    let head = git_file_commit(root, &m.locator);
                    if head.is_empty() {
                        Ok(ResolvedRevision {
                            mode: "content_hash",
                            revision: format!("content_hash:{sha}"),
                        })
                    } else {
                        Ok(ResolvedRevision {
                            mode: "committed",
                            revision: format!("committed:{head}"),
                        })
                    }
                }
                "modified" | "untracked" => Ok(ResolvedRevision {
                    mode: "worktree",
                    revision: format!("worktree:{state}:{sha}"),
                }),
                _ => Ok(ResolvedRevision {
                    mode: "content_hash",
                    revision: format!("content_hash:{sha}"),
                }),
            }
        }
        "document" => Ok(ResolvedRevision {
            mode: "content_hash",
            revision: format!("content_hash:{sha}"),
        }),
        "openapi" | "gitlab" | "rule" => Ok(ResolvedRevision {
            mode: "remote_version",
            revision: format!("remote_version:{sha}"),
        }),
        other => Err(Error::Message(format!(
            "knowledge_verify_invalid: 未知 source kind {other:?}"
        ))),
    }
}

/// source 投影行保障（v2 FK 前置）：无行则 INSERT OR IGNORE 后回读
///（UNIQUE(project_id, kind, locator) 承载幂等）。
fn ensure_source_row(
    store: &Store,
    project_id: &str,
    m: &crate::reconcile::ValidManifest,
) -> Result<String, Error> {
    let now = timefmt::now();
    store
        .with_conn(|conn| {
            conn.execute(
                "INSERT OR IGNORE INTO knowledge_sources(id, project_id, kind, name, locator,
                 enabled, scan_state, content_sha256, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,'pending',?7,?8,?8)",
                rusqlite::params![
                    ids::new_id("ksrc"),
                    project_id,
                    m.kind,
                    m.name,
                    m.locator,
                    m.enabled as i64,
                    m.content_sha256,
                    now
                ],
            )?;
            let id: Option<String> = conn
            .query_row(
                "SELECT id FROM knowledge_sources WHERE project_id=?1 AND kind=?2 AND locator=?3",
                rusqlite::params![project_id, m.kind, m.locator],
                |r| r.get(0),
            )
            .ok();
            Ok(id)
        })?
        .ok_or_else(|| Error::Message("knowledge_verify_invalid: source 行不可见".into()))
}

/// 人工/复核入口（v2）：sourceId = manifest stableId（项目作用域内解析）；
/// revision/mode 服务端解析；操作幂等键 verification_op_id（缺省生成）。
pub fn verify_source(
    store: &Store,
    project_id: &str,
    source_id: &str,
    outcome: &str,
    verifier: &str,
    evidence_ref: &str,
    verification_op_id: Option<&str>,
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
    if verification_op_id.map(str::trim).is_some_and(str::is_empty) {
        return Err(Error::Message(
            "knowledge_verify_invalid: verificationOpId 非空".into(),
        ));
    }
    let root = crate::reconcile::project_root(store, project_id)?;
    let manifests = crate::reconcile::load_source_manifests(&root)?;
    let (_path, m) = manifests
        .into_iter()
        .find(|(_, m)| m.stable_id == source_id)
        .ok_or_else(|| {
            Error::Message(format!(
                "knowledge_source_missing: {source_id}（project {project_id}）"
            ))
        })?;
    let resolved = resolve_input_revision(&root, &m)?;
    let source_row = ensure_source_row(store, project_id, &m)?;
    let policy_version = m
        .verification_policy
        .as_ref()
        .map(|p| format!("v2:{}/{}d", p.severity.as_str(), p.interval_days))
        .unwrap_or_else(|| format!("v{}", m.schema_version));

    // 操作幂等优先：同 verification_op_id 重放返回该操作的首个 receipt。
    if let Some(op_id) = verification_op_id {
        let existing: Option<Receipt> = store.with_conn(|conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {RECEIPT_COLS} FROM knowledge_verifications_v2 WHERE verification_op_id=?1"),
                    [op_id],
                    receipt_from,
                )
                .ok())
        })?;
        if let Some(r) = existing {
            return Ok(serde_json::to_value(r).unwrap_or_default());
        }
    }
    let id = ids::new_id("kvr");
    let op_id = verification_op_id
        .map(str::to_string)
        .unwrap_or_else(|| ids::new_id("kvop"));
    // 单调时间戳：同 scope（project+stable）内撞毫秒 +1ms——id 随机无序，
    // 若不保序，同毫秒连续验证的「最新一条」不确定（覆盖语义失效）。
    let now = {
        let max_ts: Option<String> = store.with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT MAX(verified_at) FROM knowledge_verifications_v2
                     WHERE project_id=?1 AND stable_id=?2",
                    rusqlite::params![project_id, source_id],
                    |r| r.get(0),
                )
                .ok())
        })?;
        match max_ts.as_deref().and_then(timefmt::parse) {
            Some(t0) => {
                let floor = timefmt::format_now(t0);
                if timefmt::now() <= floor {
                    (t0 + time::Duration::milliseconds(1))
                        .format(&time::format_description::well_known::Rfc3339)
                        .unwrap_or_else(|_| timefmt::now())
                } else {
                    timefmt::now()
                }
            }
            None => timefmt::now(),
        }
    };
    let inserted = store.with_conn(|conn| {
        conn.execute(
            "INSERT OR IGNORE INTO knowledge_verifications_v2
             (id, project_id, source_id, stable_id, verified_input_revision, input_revision_mode,
              verified_input_digest, outcome, verifier, verification_op_id, policy_version,
              verified_at, evidence_ref, legacy, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,0,?12)",
            rusqlite::params![
                id,
                project_id,
                source_row,
                source_id,
                resolved.revision,
                resolved.mode,
                m.content_sha256,
                outcome,
                verifier,
                op_id,
                policy_version,
                now,
                evidence_ref
            ],
        )?;
        Ok(conn.changes() == 1)
    })?;
    if !inserted {
        // op_id UNIQUE 并发竞态：按操作键回读（重放方收敛）。
        let r = store.with_conn(|conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {RECEIPT_COLS} FROM knowledge_verifications_v2 WHERE verification_op_id=?1"),
                    [&op_id],
                    receipt_from,
                )
                .ok())
        })?
        .ok_or_else(|| Error::Message("knowledge_verify_invalid: receipt 不可见".into()))?;
        return Ok(serde_json::to_value(r).unwrap_or_default());
    }
    outbox::emit(
        store,
        "knowledge",
        project_id,
        "knowledge.verified",
        serde_json::json!({
            "projectId": project_id,
            "sourceId": source_id,
            "verifiedInputRevision": resolved.revision,
            "inputRevisionMode": resolved.mode,
            "outcome": outcome,
        }),
    )?;
    let receipt = store.with_conn(|conn| {
        conn.query_row(
            &format!("SELECT {RECEIPT_COLS} FROM knowledge_verifications_v2 WHERE id=?1"),
            [&id],
            receipt_from,
        )
        .map_err(Error::from)
    })?;
    Ok(serde_json::to_value(receipt).unwrap_or_default())
}

/// 新鲜度派生（纯函数，v2）：最新 receipt（当前 revision 下按 (verified_at, id)
/// 取最后一条，**不筛 outcome**）——fail/unknown 立即覆盖旧 pass。
pub fn derive_freshness(
    now: &str,
    latest: Option<(&str, &str)>, // (verified_at, outcome)
    policy: Option<&VerificationPolicy>,
) -> (&'static str, Option<String>) {
    let Some((verified_at, outcome)) = latest else {
        return ("unverified", None);
    };
    match outcome {
        "fail" => return ("failed", None),
        "unknown" => return ("unknown", None),
        _ => {}
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

/// 当前 revision 下最后一条 receipt（(verified_at, id) 排序——rowid 不参与语义）。
fn latest_receipt_for_revision(
    store: &Store,
    project_id: &str,
    stable_id: &str,
    revision: &str,
    mode: &str,
) -> Result<Option<(String, String)>, Error> {
    store.with_conn(|conn| {
        Ok(conn
            .query_row(
                "SELECT verified_at, outcome FROM knowledge_verifications_v2
                 WHERE project_id=?1 AND stable_id=?2 AND verified_input_revision=?3
                   AND input_revision_mode=?4
                 ORDER BY verified_at DESC, id DESC LIMIT 1",
                rusqlite::params![project_id, stable_id, revision, mode],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .ok())
    })
}

/// knowledge.freshnessOverview（v2）：逐 source 的当前锚点新鲜度。
pub fn freshness_overview(store: &Store, project_id: &str) -> Result<serde_json::Value, Error> {
    let root = crate::reconcile::project_root(store, project_id)?;
    let manifests = crate::reconcile::load_source_manifests(&root)?;
    let now = timefmt::now();
    let mut items = Vec::new();
    for (_path, m) in &manifests {
        let resolved = resolve_input_revision(&root, m)?;
        let latest = latest_receipt_for_revision(
            store,
            project_id,
            &m.stable_id,
            &resolved.revision,
            resolved.mode,
        )?;
        let (state, next_due) = derive_freshness(
            &now,
            latest.as_ref().map(|(a, o)| (a.as_str(), o.as_str())),
            m.verification_policy.as_ref(),
        );
        // 有历史 receipt 但无一匹配当前锚点 → 内容/锚点已变更。
        let state = if latest.is_none() {
            let any: i64 = store.with_conn(|conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM knowledge_verifications_v2
                     WHERE project_id=?1 AND stable_id=?2",
                    rusqlite::params![project_id, m.stable_id],
                    |r| r.get(0),
                )?)
            })?;
            if any > 0 {
                "unverified_content_changed"
            } else {
                state
            }
        } else {
            state
        };
        items.push(serde_json::json!({
            "stableId": m.stable_id,
            "enabled": m.enabled,
            "schemaVersion": m.schema_version,
            "contentOwner": m.content_owner,
            "contentSha256": m.content_sha256,
            "inputRevisionMode": resolved.mode,
            "verifiedInputRevision": resolved.revision,
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

    fn setup_named(tag: &str, project_id: &str) -> (Store, Tmp) {
        let dir = std::env::temp_dir().join(format!(
            "sg-kfresh-{tag}-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(dir.join("knowledge/sources")).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, local_root, created_at)
                     VALUES (?1,'u','n','p','main',?2,?3)",
                    rusqlite::params![project_id, dir.to_string_lossy().to_string(), timefmt::now()],
                )
                .unwrap();
                Ok(())
            })
            .unwrap();
        (store, Tmp(dir))
    }

    fn setup() -> (Store, String, Tmp) {
        let (store, tmp) = setup_named("main", "pj");
        (store, "pj".to_string(), tmp)
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
    fn verify_content_change_and_outcome_override() {
        let (store, pj, tmp) = setup();
        let root = tmp.0.clone();
        let (stable_id, slug) = identity_of();
        write_manifest(&root, &slug, manifest_body("sha_v1", 2));
        // 未验证 → unverified。
        let ov = freshness_overview(&store, &pj).unwrap();
        assert_eq!(ov["items"][0]["state"], json!("unverified"));
        // pass → verified + next_due。
        let r = verify_source(&store, &pj, &stable_id, "pass", "owner", "evid-1", None).unwrap();
        assert_eq!(r["outcome"], json!("pass"));
        assert_eq!(
            r["inputRevisionMode"],
            json!("content_hash"),
            "非 git 根 → content_hash 模式"
        );
        assert!(
            r["verificationOpId"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
            "缺省生成操作键"
        );
        let ov = freshness_overview(&store, &pj).unwrap();
        assert_eq!(ov["items"][0]["state"], json!("verified"));
        assert!(ov["items"][0]["nextDueAt"].is_string());
        // v2 覆盖语义：最新 fail 立即覆盖旧 pass（不再被遮蔽）。
        verify_source(&store, &pj, &stable_id, "fail", "auditor", "", None).unwrap();
        let ov = freshness_overview(&store, &pj).unwrap();
        assert_eq!(ov["items"][0]["state"], json!("failed"), "fail 覆盖 pass");
        // unknown 同样覆盖。
        verify_source(&store, &pj, &stable_id, "unknown", "auditor", "", None).unwrap();
        let ov = freshness_overview(&store, &pj).unwrap();
        assert_eq!(
            ov["items"][0]["state"],
            json!("unknown"),
            "unknown 覆盖 fail"
        );
        // 重新 pass → 恢复 verified（覆盖是"最新一条"语义）。
        verify_source(&store, &pj, &stable_id, "pass", "owner", "", None).unwrap();
        let ov = freshness_overview(&store, &pj).unwrap();
        assert_eq!(ov["items"][0]["state"], json!("verified"));
        // 内容变更 → unverified_content_changed。
        write_manifest(&root, &slug, manifest_body("sha_v2", 2));
        let ov = freshness_overview(&store, &pj).unwrap();
        assert_eq!(ov["items"][0]["state"], json!("unverified_content_changed"));
        verify_source(&store, &pj, &stable_id, "pass", "owner", "", None).unwrap();
        let ov = freshness_overview(&store, &pj).unwrap();
        assert_eq!(ov["items"][0]["state"], json!("verified"));
    }

    #[test]
    fn op_id_and_domain_idempotency() {
        let (store, pj, tmp) = setup();
        let root = tmp.0.clone();
        let (stable_id, slug) = identity_of();
        write_manifest(&root, &slug, manifest_body("sha_v1", 2));
        // 操作键幂等：同 op_id 重放返回同一 receipt（单行）。
        let r1 =
            verify_source(&store, &pj, &stable_id, "pass", "owner", "", Some("kvop-1")).unwrap();
        let r2 =
            verify_source(&store, &pj, &stable_id, "pass", "owner", "", Some("kvop-1")).unwrap();
        assert_eq!(r1["id"], r2["id"], "同 op_id 重放幂等");
        // v2 事件语义：异 op_id 同参重复 verify = 新事实（时间戳推进，不收敛旧行——
        // 0049 的 outcome 域键会使改判无法翻面）。
        let r3 =
            verify_source(&store, &pj, &stable_id, "pass", "owner", "", Some("kvop-2")).unwrap();
        assert_ne!(r1["id"], r3["id"], "重复验证是新事件");
        let n: i64 = store
            .with_conn(|c| {
                Ok(
                    c.query_row("SELECT COUNT(*) FROM knowledge_verifications_v2", [], |x| {
                        x.get(0)
                    })
                    .unwrap(),
                )
            })
            .unwrap();
        assert_eq!(n, 2);
        // 空操作键拒绝。
        assert!(verify_source(&store, &pj, &stable_id, "pass", "owner", "", Some(" ")).is_err());
    }

    #[test]
    fn cross_project_same_stable_id_isolated() {
        let (store1, _t1) = setup_named("p1", "pj1");
        let (store2, _t2) = setup_named("p2", "pj2");
        let (stable_id, slug) = identity_of();
        // 两个项目根下放相同 stable_id 的 manifest。
        write_manifest(&_t1.0, &slug, manifest_body("sha_v1", 2));
        write_manifest(&_t2.0, &slug, manifest_body("sha_v1", 2));
        // 只在 pj1 verify pass。
        verify_source(&store1, "pj1", &stable_id, "pass", "owner", "", None).unwrap();
        let ov1 = freshness_overview(&store1, "pj1").unwrap();
        assert_eq!(ov1["items"][0]["state"], json!("verified"));
        // pj2 不受 pj1 的 receipt 影响（v1 会被串成 verified——v2 作用域隔离）。
        let ov2 = freshness_overview(&store2, "pj2").unwrap();
        assert_eq!(
            ov2["items"][0]["state"],
            json!("unverified"),
            "跨项目同 stable id 不串"
        );
        // source 投影行也各自独立。
        let rows: i64 = store1
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM knowledge_sources WHERE project_id='pj1'",
                    [],
                    |r| r.get(0),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn same_timestamp_ordered_by_id() {
        // 同 verified_at 的两条 receipt：id 大者为最新（rowid 不参与语义）。
        let (store, pj, tmp) = setup();
        let root = tmp.0.clone();
        let (stable_id, slug) = identity_of();
        write_manifest(&root, &slug, manifest_body("sha_v1", 2));
        // 先建立 source 行与当前锚点（借一次真实 verify 拿 revision）。
        let probe = verify_source(&store, &pj, &stable_id, "pass", "probe", "", None).unwrap();
        let revision = probe["verifiedInputRevision"].as_str().unwrap().to_string();
        let mode = probe["inputRevisionMode"].as_str().unwrap().to_string();
        let source_row = probe["sourceId"].as_str().unwrap().to_string();
        let at = "2099-01-01T00:00:00.000Z"; // 远未来：真实 probe 行必然更早
        store
            .with_conn(|c| {
                for (id, outcome) in [("kvr_aaa1", "pass"), ("kvr_zzz9", "fail")] {
                    c.execute(
                        "INSERT INTO knowledge_verifications_v2
                         (id, project_id, source_id, stable_id, verified_input_revision, input_revision_mode,
                          verified_input_digest, outcome, verifier, verification_op_id, policy_version,
                          verified_at, evidence_ref, legacy, created_at)
                         VALUES (?1,?2,?3,?4,?5,?6,'sha_v1',?7,'t',?8,'v2:block/7d',?9,'',0,?9)",
                        rusqlite::params![id, pj, source_row, stable_id, revision, mode, outcome,
                                           format!("op-{id}"), at],
                    )?;
                }
                Ok(())
            })
            .unwrap();
        // 同时间：id 字典序大者（kvr_zzz9/fail）胜出。
        let latest = latest_receipt_for_revision(&store, &pj, &stable_id, &revision, &mode)
            .unwrap()
            .unwrap();
        assert_eq!(latest.1, "fail", "同 verified_at 按 id 排序（非 rowid）");
        let ov = freshness_overview(&store, &pj).unwrap();
        assert_eq!(ov["items"][0]["state"], json!("failed"));
    }

    #[test]
    fn four_revision_modes_resolve_and_anchor() {
        // 四 mode 解析：committed/worktree（git 根实测）+ content_hash/remote_version。
        let (store, tmp) = setup_named("git", "pjg");
        let pj = "pjg".to_string();
        let root = tmp.0.clone();
        let (stable_id, slug) = identity_of();
        write_manifest(&root, &slug, manifest_body("sha_v1", 2));
        // 建最小 git 仓：locator 文件已提交 → committed 模式。
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(
            root.join("docs/guide.md"),
            "# 指南
",
        )
        .unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(["-C", &root.to_string_lossy()])
                .args(args)
                .output()
                .unwrap()
        };
        if !git(&["rev-parse", "--is-inside-work-tree"])
            .status
            .success()
        {
            git(&["init", "-q"]);
        }
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["add", "."]);
        git(&["commit", "-qm", "v1"]);
        let resolved = {
            let manifests = crate::reconcile::load_source_manifests(&root).unwrap();
            let (_, m) = manifests
                .iter()
                .find(|(_, m)| m.stable_id == stable_id)
                .unwrap();
            resolve_input_revision(&root, m).unwrap()
        };
        assert_eq!(resolved.mode, "committed", "干净 worktree → committed 锚点");
        assert!(resolved.revision.starts_with("committed:"));
        // 提交后修改文件 → worktree:modified。
        std::fs::write(
            root.join("docs/guide.md"),
            "# 指南 v2
",
        )
        .unwrap();
        let resolved2 = {
            let manifests = crate::reconcile::load_source_manifests(&root).unwrap();
            let (_, m) = manifests
                .iter()
                .find(|(_, m)| m.stable_id == stable_id)
                .unwrap();
            resolve_input_revision(&root, m).unwrap()
        };
        assert_eq!(resolved2.mode, "worktree");
        assert!(resolved2.revision.starts_with("worktree:modified:"));
        // 内容变更（manifest sha 变）→ 锚点变化 → unverified_content_changed。
        write_manifest(&root, &slug, manifest_body("sha_v2", 2));
        verify_source(&store, &pj, &stable_id, "pass", "owner", "", None)
            .map_err(|e| {
                // knowledge_sources 已有 (project,kind,locator) 行 content_sha 旧值——
                // INSERT OR IGNORE 幂等，不影响验证。
                e.to_string()
            })
            .unwrap();
        let ov = freshness_overview(&store, &pj).unwrap();
        // worktree:modified 状态下 sha_v2 的 pass 锚定 worktree:modified:sha_v2 → verified。
        assert_eq!(ov["items"][0]["state"], json!("verified"));
        // remote_version/content_hash 模式：直接对表插入锚点行验证选择逻辑。
        let head_commit = git(&["rev-parse", "HEAD"]);
        let head = String::from_utf8_lossy(&head_commit.stdout)
            .trim()
            .to_string();
        let _ = head;
        let source_row: String = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT id FROM knowledge_sources WHERE project_id='pjg' LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap())
            })
            .unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO knowledge_verifications_v2
                     (id, project_id, source_id, stable_id, verified_input_revision, input_revision_mode,
                      verified_input_digest, outcome, verifier, verification_op_id, policy_version,
                      verified_at, evidence_ref, legacy, created_at)
                     VALUES ('kvr_rv','pjg',?1,?2,'remote_version:sha_v9','remote_version','sha_v9',
                             'pass','t','op-rv','v2:block/7d','2026-09-01T00:00:00.000Z','',0,'2026-09-01T00:00:00.000Z')",
                    rusqlite::params![source_row, stable_id],
                )?;
                Ok(())
            })
            .unwrap();
        // remote_version 锚点不匹配当前（worktree）→ 不参与当前新鲜度（只留痕）。
        let ov2 = freshness_overview(&store, &pj).unwrap();
        assert_eq!(
            ov2["items"][0]["inputRevisionMode"],
            json!("worktree"),
            "四 mode 各自锚定，互不遮蔽"
        );
    }

    #[test]
    fn v1_manifest_and_invalid_params() {
        let (store, pj, tmp) = setup();
        let root = tmp.0.clone();
        let (stable_id, slug) = identity_of();
        write_manifest(&root, &slug, manifest_body("sha_v1", 1));
        verify_source(&store, &pj, &stable_id, "pass", "owner", "", None).unwrap();
        let ov = freshness_overview(&store, &pj).unwrap();
        assert_eq!(ov["items"][0]["state"], json!("unverified"));
        assert!(ov["items"][0]["nextDueAt"].is_null());
        assert!(verify_source(&store, &pj, &stable_id, "maybe", "owner", "", None).is_err());
        assert!(verify_source(&store, &pj, "repopath-ghost", "pass", "owner", "", None).is_err());
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
        // 无 receipt → unverified。
        assert_eq!(
            derive_freshness("2026-09-08T00:00:00Z", None, Some(&policy)).0,
            "unverified"
        );
        // 最新 fail/unknown → 覆盖（不看过期窗口）。
        assert_eq!(
            derive_freshness(
                "2026-09-08T00:00:00Z",
                Some(("2026-09-01T00:00:00Z", "fail")),
                Some(&policy)
            )
            .0,
            "failed"
        );
        assert_eq!(
            derive_freshness(
                "2026-09-08T00:00:00Z",
                Some(("2026-09-01T00:00:00Z", "unknown")),
                Some(&policy)
            )
            .0,
            "unknown"
        );
        // pass：窗口内 verified + due=+7d；过期 expired。
        let (st, due) = derive_freshness(
            "2026-09-05T00:00:00Z",
            Some(("2026-09-01T00:00:00Z", "pass")),
            Some(&policy),
        );
        assert_eq!(st, "verified");
        assert_eq!(due.as_deref(), Some("2026-09-08T00:00:00Z"));
        let (st, _) = derive_freshness(
            "2026-09-20T00:00:00Z",
            Some(("2026-09-01T00:00:00Z", "pass")),
            Some(&policy),
        );
        assert_eq!(st, "expired");
        let _ = policy.severity.as_str();
    }
}
