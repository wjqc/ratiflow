//! 工具策略/执行 Profile/知识默认设置的持久化与合成。
use serde::Serialize;
use serde_json::Value;

use crate::{codes, store_err, SettingsError, SettingsResult};
use sg_store::{ids, timefmt, Store};

#[derive(Debug, Clone, Serialize)]
pub struct ToolPolicy {
    pub tool_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub project_id: String,
    pub enabled: bool,
    pub risk: String,
    pub requires_approval: bool,
    pub network: String,
    pub revision: i64,
}

pub fn tool_list(store: &Store) -> SettingsResult<Vec<ToolPolicy>> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT tool_id, project_id, enabled, risk, requires_approval, network, revision FROM tool_policies ORDER BY tool_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ToolPolicy {
                tool_id: r.get(0)?,
                project_id: r.get(1)?,
                enabled: r.get::<_, i64>(2)? == 1,
                risk: r.get(3)?,
                requires_approval: r.get::<_, i64>(4)? == 1,
                network: r.get(5)?,
                revision: r.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }).map_err(store_err)
}

pub fn tool_update(store: &Store, p: &Value, expected_revision: i64) -> SettingsResult<ToolPolicy> {
    let tool_id = p
        .get("toolId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SettingsError::new("INVALID_PARAMS", "toolId 必填"))?
        .to_string();
    let project_id = p
        .get("projectId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let current: Option<i64> = store
        .with_conn(|conn| {
            let result: rusqlite::Result<i64> = conn.query_row(
                "SELECT revision FROM tool_policies WHERE tool_id=?1 AND project_id=?2",
                [&tool_id, &project_id],
                |r| r.get(0),
            );
            Ok(result.ok())
        })
        .map_err(store_err)?;
    let now = timefmt::now();
    match current {
        None => {
            store.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO tool_policies(tool_id, project_id, enabled, risk, requires_approval, network, revision, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,1,?7)",
                    rusqlite::params![tool_id, project_id,
                        p.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true) as i64,
                        p.get("risk").and_then(|v| v.as_str()).unwrap_or("medium"),
                        p.get("requiresApproval").and_then(|v| v.as_bool()).unwrap_or(false) as i64,
                        p.get("network").and_then(|v| v.as_str()).unwrap_or("deny"),
                        now],
                )?;
                Ok(())
            }).map_err(store_err)?;
        }
        Some(rev) => {
            if rev != expected_revision {
                return Err(SettingsError::new(
                    codes::REVISION_CONFLICT,
                    format!("策略期望 revision {expected_revision} 实际 {rev}"),
                ));
            }
            store.with_conn(|conn| {
                conn.execute(
                    "UPDATE tool_policies SET enabled=COALESCE(?1,enabled), risk=COALESCE(?2,risk),
                            requires_approval=COALESCE(?3,requires_approval), network=COALESCE(?4,network),
                            revision=revision+1, updated_at=?5
                     WHERE tool_id=?6 AND project_id=?7",
                    rusqlite::params![p.get("enabled").and_then(|v| v.as_bool()).map(|b| b as i64),
                        p.get("risk").and_then(|v| v.as_str()),
                        p.get("requiresApproval").and_then(|v| v.as_bool()).map(|b| b as i64),
                        p.get("network").and_then(|v| v.as_str()),
                        now, tool_id, project_id],
                )?;
                Ok(())
            }).map_err(store_err)?;
        }
    }
    tool_list(store)
        .map_err(|_| SettingsError::new("INTERNAL", "读取更新后策略失败"))?
        .into_iter()
        .find(|t| t.tool_id == tool_id && t.project_id == project_id)
        .ok_or_else(|| SettingsError::new("INTERNAL", "策略写入后不可见"))
}

/// 合成：项目覆盖 > 全局 > 内置默认（read_file 低风险等）。
pub fn tool_effective(store: &Store, project_id: Option<&str>) -> SettingsResult<Vec<ToolPolicy>> {
    let defaults = [
        ("read_file", "low", false),
        ("write_file", "medium", false),
        ("run_command", "high", true),
        ("deploy", "high", true),
    ];
    let mut merged: Vec<ToolPolicy> = defaults
        .iter()
        .map(|(id, risk, appr)| ToolPolicy {
            tool_id: (*id).into(),
            project_id: String::new(),
            enabled: true,
            risk: (*risk).into(),
            requires_approval: *appr,
            network: "deny".into(),
            revision: 0,
        })
        .collect();
    for policy in tool_list(store)? {
        if let Some(slot) = merged.iter_mut().find(|m| m.tool_id == policy.tool_id) {
            slot.enabled = policy.enabled;
            slot.risk = policy.risk.clone();
            slot.requires_approval = policy.requires_approval;
            slot.network = policy.network.clone();
            slot.revision = policy.revision;
        } else {
            merged.push(policy);
        }
    }
    if let Some(pid) = project_id.filter(|p| !p.is_empty()) {
        for policy in tool_list(store)? {
            if policy.project_id == pid {
                if let Some(slot) = merged.iter_mut().find(|m| m.tool_id == policy.tool_id) {
                    slot.enabled = policy.enabled;
                    slot.requires_approval = policy.requires_approval;
                    slot.network = policy.network.clone();
                    slot.revision = policy.revision;
                }
            }
        }
    }
    Ok(merged)
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionProfile {
    pub id: String,
    pub revision: i64,
    pub name: String,
    pub mode: String,
    pub limits: Value,
    pub created_at: String,
    pub updated_at: String,
}

pub fn execution_list(store: &Store) -> SettingsResult<Vec<ExecutionProfile>> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT id, revision, name, mode, limits_json, created_at, updated_at FROM execution_profiles ORDER BY created_at")?;
        let rows = stmt.query_map([], |r| {
            Ok(ExecutionProfile {
                id: r.get(0)?, revision: r.get(1)?, name: r.get(2)?, mode: r.get(3)?,
                limits: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
                created_at: r.get(5)?, updated_at: r.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }).map_err(store_err)
}

pub fn execution_create(
    store: &Store,
    name: &str,
    mode: &str,
    limits: &Value,
) -> SettingsResult<ExecutionProfile> {
    if !matches!(
        mode,
        "docker" | "safe_restricted" | "unsafe_explicit" | "disabled"
    ) {
        return Err(SettingsError::new(
            "INVALID_PARAMS",
            format!("未知执行模式 {mode}"),
        ));
    }
    let id = ids::new_id("exec");
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO execution_profiles(id, name, mode, limits_json, revision, created_at, updated_at)
             VALUES (?1,?2,?3,?4,1,?5,?5)",
            rusqlite::params![id, name, mode, limits.to_string(), now],
        )?;
        Ok(())
    }).map_err(store_err)?;
    execution_list(store)?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| SettingsError::new("INTERNAL", "写入后不可见"))
}

pub fn execution_update(
    store: &Store,
    id: &str,
    p: &Value,
    expected_revision: i64,
) -> SettingsResult<ExecutionProfile> {
    let current = execution_list(store)?
        .into_iter()
        .find(|x| x.id == id)
        .ok_or_else(|| SettingsError::new("NOT_FOUND", "执行 Profile 不存在"))?;
    if current.revision != expected_revision {
        return Err(SettingsError::new(
            codes::REVISION_CONFLICT,
            "revision 不匹配",
        ));
    }
    store
        .with_conn(|conn| {
            conn.execute(
                "UPDATE execution_profiles SET name=COALESCE(?1,name), mode=COALESCE(?2,mode),
                    limits_json=COALESCE(?3,limits_json), revision=revision+1, updated_at=?4
             WHERE id=?5",
                rusqlite::params![
                    p.get("name").and_then(|v| v.as_str()),
                    p.get("mode").and_then(|v| v.as_str()),
                    p.get("limits").map(|v| v.to_string()),
                    timefmt::now(),
                    id
                ],
            )?;
            Ok(())
        })
        .map_err(store_err)?;
    execution_list(store)?
        .into_iter()
        .find(|x| x.id == id)
        .ok_or_else(|| SettingsError::new("INTERNAL", "更新后不可见"))
}

pub fn execution_remove(store: &Store, id: &str, expected_revision: i64) -> SettingsResult<()> {
    let current = execution_list(store)?
        .into_iter()
        .find(|x| x.id == id)
        .ok_or_else(|| SettingsError::new("NOT_FOUND", "执行 Profile 不存在"))?;
    if current.revision != expected_revision {
        return Err(SettingsError::new(
            codes::REVISION_CONFLICT,
            "revision 不匹配",
        ));
    }
    store
        .with_conn(|conn| {
            conn.execute("DELETE FROM execution_profiles WHERE id=?1", [id])?;
            Ok(())
        })
        .map_err(store_err)?;
    Ok(())
}
