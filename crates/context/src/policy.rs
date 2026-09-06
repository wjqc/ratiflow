//! Context Policy（EvoFlow 方案 M4-06 / ADR-038 §6.10 / M4-08）：
//! 服务端派生最终工具集——Registry ∩ policy 白名单 ∩ 客户端请求；
//! 客户端 toolAllowlist 只能收紧不能扩大（EV-014）。excluded 带原因。

use std::collections::BTreeSet;

use serde::Serialize;
use sg_store::{Error, Store};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize)]
pub struct ContextPolicyVersion {
    pub id: String,
    pub key: String,
    pub version_no: i64,
    pub status: String,
    pub gate_id: Option<String>,
    pub sources_json: String,
    pub allowed_tools_json: String,
    pub compaction_json: String,
    pub content_digest: String,
}

fn digest_of(key: &str, sources: &str, allowed: &str, compaction: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("cp1|{key}|{sources}|{allowed}|{compaction}").as_bytes());
    sg_store::ids::hex(&hasher.finalize())
}

fn policy_row(conn: &rusqlite::Connection, id: &str) -> Result<ContextPolicyVersion, Error> {
    conn.query_row(
        "SELECT id, key, version_no, status, gate_id, sources_json, allowed_tools_json,
                compaction_json, content_digest
         FROM context_policy_versions WHERE id=?1",
        [id],
        |r| {
            Ok(ContextPolicyVersion {
                id: r.get(0)?,
                key: r.get(1)?,
                version_no: r.get(2)?,
                status: r.get(3)?,
                gate_id: r.get(4)?,
                sources_json: r.get(5)?,
                allowed_tools_json: r.get(6)?,
                compaction_json: r.get(7)?,
                content_digest: r.get(8)?,
            })
        },
    )
    .map_err(|_| Error::Message(format!("context_policy_not_found: {id}")))
}

/// 创建 draft 版本（digest 服务端计算；allowed_tools 空 = 不限制）。
pub fn create_version(
    store: &Store,
    key: &str,
    gate_id: Option<&str>,
    sources_json: &str,
    allowed_tools: &[String],
    compaction_json: &str,
    created_by: &str,
) -> Result<ContextPolicyVersion, Error> {
    let digest = digest_of(
        key,
        sources_json,
        &serde_json::to_string(allowed_tools).unwrap_or_default(),
        compaction_json,
    );
    let id = sg_store::ids::new_id("cpv");
    let now = sg_store::timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO context_policy_versions(id, key, version_no, status, gate_id, sources_json,
                allowed_tools_json, compaction_json, content_digest, created_by, created_at, updated_at)
             VALUES (?1,?2,(SELECT COALESCE(MAX(version_no),0)+1 FROM context_policy_versions WHERE key=?3),
                'draft',?4,?5,?6,?7,?8,?9,?10,?10)",
            rusqlite::params![
                id,
                key,
                key,
                gate_id,
                sources_json,
                serde_json::to_string(allowed_tools).unwrap_or_default(),
                compaction_json,
                digest,
                created_by,
                now
            ],
        )?;
        policy_row(conn, &id)
    })
}

/// 激活（draft → active；同 key 旧 active → deprecated）。
pub fn activate(store: &Store, version_id: &str) -> Result<ContextPolicyVersion, Error> {
    store.with_conn(|conn| {
        let cur = policy_row(conn, version_id)?;
        if cur.status == "active" {
            return Ok(cur);
        }
        if cur.status != "draft" {
            return Err(Error::Message(format!(
                "context_policy_invalid: {} 不可激活（仅 draft）",
                cur.status
            )));
        }
        conn.execute(
            "UPDATE context_policy_versions SET status='deprecated', updated_at=?1
             WHERE key=?2 AND status='active'",
            rusqlite::params![sg_store::timefmt::now(), cur.key],
        )?;
        conn.execute(
            "UPDATE context_policy_versions SET status='active', updated_at=?1 WHERE id=?2",
            rusqlite::params![sg_store::timefmt::now(), version_id],
        )?;
        policy_row(conn, version_id)
    })
}

/// 按 key 取 active 版本。
pub fn active_by_key(store: &Store, key: &str) -> Result<Option<ContextPolicyVersion>, Error> {
    store.with_conn(|conn| {
        let row: Option<String> = conn
            .query_row(
                "SELECT id FROM context_policy_versions WHERE key=?1 AND status='active'",
                [key],
                |r| r.get(0),
            )
            .ok();
        match row {
            Some(id) => policy_row(conn, &id).map(Some),
            None => Ok(None),
        }
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedToolSet {
    pub effective: Vec<String>,
    /// 客户端请求但被服务端移除的工具 + 原因。
    pub excluded: Vec<ExcludedTool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExcludedTool {
    pub tool: String,
    pub reason: String,
}

/// 服务端交集（EV-014）：final = registry ∩ policy ∩ client。
/// - policy 白名单为空 = 不限制；
/// - 客户端请求超出 policy 的部分被移除（reason=not_in_policy）；
/// - registry 外的工具无论谁请求都不存在（reason=not_in_registry）。
pub fn resolve_tools(
    registry: &[String],
    policy_allowed: Option<&[String]>,
    client_request: &[String],
) -> ResolvedToolSet {
    let registry_set: BTreeSet<&String> = registry.iter().collect();
    let policy_set: Option<BTreeSet<&String>> =
        policy_allowed.map(|list| list.iter().collect::<BTreeSet<_>>());
    let mut effective: Vec<String> = Vec::new();
    let mut excluded: Vec<ExcludedTool> = Vec::new();
    let mut requested: BTreeSet<&String> = BTreeSet::new();
    for c in client_request {
        requested.insert(c);
    }
    for tool in client_request {
        if !registry_set.contains(tool) {
            excluded.push(ExcludedTool {
                tool: (*tool).clone(),
                reason: "not_in_registry".into(),
            });
            continue;
        }
        if let Some(policy) = &policy_set {
            if !policy.contains(tool) {
                excluded.push(ExcludedTool {
                    tool: (*tool).clone(),
                    reason: "not_in_policy".into(),
                });
                continue;
            }
        }
        effective.push((*tool).clone());
    }
    // policy 白名单中的工具即便客户端未请求也保留在集合外（渐进暴露由 Gate 阶段决定）；
    // effective 只含交集（客户端请求 ∩ policy ∩ registry），确定性排序。
    effective.sort();
    let _ = requested;
    ResolvedToolSet {
        effective,
        excluded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-ctxp-{}-{}",
            std::process::id(),
            sg_store::ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Store::open(&dir, "test").unwrap()
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn policy_lifecycle_single_active_per_key() {
        let store = setup();
        let v1 = create_version(
            &store,
            "requirements-policy",
            Some("requirements"),
            "[]",
            &strings(&["read_file", "search_knowledge"]),
            "{}",
            "admin",
        )
        .unwrap();
        activate(&store, &v1.id).unwrap();
        let active = active_by_key(&store, "requirements-policy")
            .unwrap()
            .unwrap();
        assert_eq!(active.id, v1.id);
        // v2 激活 → v1 deprecated。
        let v2 = create_version(
            &store,
            "requirements-policy",
            Some("requirements"),
            "[]",
            &strings(&["read_file"]),
            "{}",
            "admin",
        )
        .unwrap();
        activate(&store, &v2.id).unwrap();
        let active = active_by_key(&store, "requirements-policy")
            .unwrap()
            .unwrap();
        assert_eq!(active.id, v2.id);
        assert_eq!(active.allowed_tools_json, r#"["read_file"]"#);
    }

    /// EV-014：客户端扩大 allowlist → 服务端交集移除越权工具。
    #[test]
    fn client_expansion_is_stripped_by_server_intersection() {
        let registry = strings(&[
            "read_file",
            "search_knowledge",
            "run_command",
            "apply_patch",
        ]);
        let policy = strings(&["read_file", "search_knowledge"]);
        // 客户端请求包含越权 run_command/apply_patch + 不存在的工具。
        let client = strings(&["read_file", "search_knowledge", "run_command", "ghost_tool"]);
        let r = resolve_tools(&registry, Some(&policy), &client);
        assert_eq!(
            r.effective,
            vec!["read_file".to_string(), "search_knowledge".to_string()],
            "服务端交集移除越权工具"
        );
        assert!(r
            .excluded
            .iter()
            .any(|e| e.tool == "run_command" && e.reason == "not_in_policy"));
        assert!(r
            .excluded
            .iter()
            .any(|e| e.tool == "ghost_tool" && e.reason == "not_in_registry"));
        // 空 policy 白名单 = 不限制（registry 内全放）。
        let r = resolve_tools(&registry, None, &client);
        assert!(r.effective.contains(&"run_command".to_string()));
        // 客户端收紧：只请求 read_file → 只给 read_file。
        let r = resolve_tools(&registry, None, &strings(&["read_file"]));
        assert_eq!(r.effective, vec!["read_file".to_string()]);
    }
}
