//! settings.summary 聚合：诊断、阻塞、备份、凭据与最近变更（S00）。
//! 返回语义与 i18n key；targetRoute 来自允许路由集合。
use serde_json::{json, Value};

use crate::{store_err, SettingsResult};
use sg_store::Store;

const ALLOWED_ROUTES: [&str; 8] = [
    "/settings/models",
    "/settings/gitlab",
    "/settings/ssh",
    "/settings/credentials",
    "/settings/backup",
    "/settings/knowledge",
    "/settings/tools",
    "/settings/execution",
];

pub fn aggregate(store: &Store) -> SettingsResult<Value> {
    let mut blockers: Vec<Value> = Vec::new();
    let mut components: Vec<Value> = Vec::new();

    // 模型：无可用 Profile（ready/configured 均视为已配置）→ 阻塞 agent_run。
    let model_profiles: Vec<(String, String)> = store
        .with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT id, status FROM model_profiles")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .map_err(store_err)?;
    let model_ready = model_profiles.iter().any(|(_, status)| status == "ready");
    let model_managed_only = model_profiles
        .iter()
        .all(|(id, _)| id.starts_with("mp_env"))
        && !model_profiles.is_empty();
    components.push(json!({"id": "model", "status": if model_profiles.is_empty() { "error" } else if model_ready { "ready" } else { "configured" },
        "profileCount": model_profiles.len(), "managedOnly": model_managed_only}));
    if model_profiles.is_empty() {
        blockers.push(json!({
            "id": "model_not_configured", "severity": "blocking", "scope": "global",
            "capabilities": ["agent_run"], "titleKey": "settings.blocker.modelMissing",
            "targetRoute": "/settings/models",
        }));
    }

    // GitLab。
    let gitlab_profiles: i64 = store
        .with_conn(|conn| {
            Ok(conn.query_row("SELECT COUNT(*) FROM gitlab_profiles", [], |r| r.get(0))?)
        })
        .map_err(store_err)?;
    components.push(json!({"id": "gitlab", "status": if gitlab_profiles == 0 { "error" } else { "configured" }, "profileCount": gitlab_profiles}));
    if gitlab_profiles == 0 {
        blockers.push(json!({
            "id": "gitlab_not_configured", "severity": "blocking", "scope": "global",
            "capabilities": ["issue_import", "mr"], "titleKey": "settings.blocker.gitlabMissing",
            "targetRoute": "/settings/gitlab",
        }));
    }

    // SSH。
    let ssh_targets: i64 = store
        .with_conn(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM ssh_targets", [], |r| r.get(0))?))
        .map_err(store_err)?;
    components.push(json!({"id": "ssh", "status": if ssh_targets == 0 { "error" } else { "configured" }, "targetCount": ssh_targets}));
    if ssh_targets == 0 {
        blockers.push(json!({
            "id": "ssh_not_configured", "severity": "degraded", "scope": "global",
            "capabilities": ["deploy"], "titleKey": "settings.blocker.sshMissing",
            "targetRoute": "/settings/ssh",
        }));
    }

    // 知识来源。
    let sources: i64 = store
        .with_conn(|conn| {
            Ok(conn.query_row("SELECT COUNT(*) FROM knowledge_sources", [], |r| r.get(0))?)
        })
        .map_err(store_err)?;
    components.push(json!({"id": "knowledge", "status": if sources == 0 { "configured" } else { "ready" }, "sourceCount": sources}));

    // 凭据/备份/审计计数。
    let credential_count: i64 = store
        .with_conn(|conn| {
            Ok(conn.query_row("SELECT COUNT(*) FROM credential_refs", [], |r| r.get(0))?)
        })
        .map_err(store_err)?;
    let last_backup: Option<(String, String, i64)> = store
        .with_conn(|conn| {
            let result: rusqlite::Result<(String, String, i64)> = conn.query_row(
            "SELECT id, created_at, verified FROM backup_records ORDER BY created_at DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        );
            Ok(result.ok())
        })
        .map_err(store_err)?;
    let audit_7d: i64 = store
        .with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM audit_log WHERE created_at >= datetime('now', '-7 days')",
                [],
                |r| r.get(0),
            )?)
        })
        .map_err(store_err)?;

    // 最近变更：settings.json 单文件条目（app_settings 表已不再写入）。
    let pref_doc = sg_store::prefstore::load(store).map_err(store_err)?;
    let mut recent_entries: Vec<(String, &sg_store::prefstore::PrefEntry)> = pref_doc
        .iter()
        .map(|(id, entry)| {
            let (_, _, key) = sg_store::prefstore::split_composite(id);
            (key, entry)
        })
        .collect();
    recent_entries.sort_by(|a, b| b.1.updated_at.cmp(&a.1.updated_at));
    let recent: Vec<Value> = recent_entries
        .into_iter()
        .take(5)
        .map(|(key, entry)| {
            json!({
                "key": key,
                "revision": entry.revision,
                "updatedAt": entry.updated_at,
                "updatedBy": entry.updated_by,
            })
        })
        .collect();

    let overall = if blockers.iter().any(|b| b["severity"] == json!("blocking")) {
        "action_required"
    } else if !blockers.is_empty() {
        "degraded"
    } else {
        "ready"
    };

    Ok(json!({
        "overallStatus": overall,
        "checkedAt": sg_store::timefmt::now(),
        "blockers": blockers.iter().filter(|b| ALLOWED_ROUTES.contains(&b["targetRoute"].as_str().unwrap_or(""))).collect::<Vec<_>>(),
        "components": components,
        "dataSafety": {
            "credentialRefCount": credential_count,
            "lastBackup": last_backup.map(|(id, at, verified)| json!({"id": id, "createdAt": at, "verified": verified == 1})),
            "auditEventsLast7Days": audit_7d,
        },
        "recentChanges": recent,
    }))
}
