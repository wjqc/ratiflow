//! AgentProfile 版本化（ADR-030 M4 / SG-AGT-001/002）。
//! persona/SOP 正文进 objects，版本冻结全部策略字段并计算 content_digest；
//! 内置通用 AgentProfile 也有固定版本与 digest（不能用无版本的隐式代码常量代替运行记录）。

use serde::Serialize;
use sg_store::{ids, objects, timefmt, Error, Store};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize)]
pub struct Profile {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub name: String,
    pub adapter_kind: String,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileVersion {
    pub id: String,
    pub profile_id: String,
    pub version_no: i64,
    pub persona_object_sha256: String,
    pub sop_object_sha256: String,
    pub capabilities: Vec<String>,
    pub output_schema_sha256: String,
    pub model_route_json: String,
    pub budget_json: String,
    pub content_digest: String,
    pub created_at: String,
}

const PROFILE_COLUMNS: &str = "id, project_id, name, adapter_kind, enabled, created_at, updated_at";

fn row_profile(r: &rusqlite::Row<'_>) -> rusqlite::Result<Profile> {
    Ok(Profile {
        id: r.get(0)?,
        project_id: r.get(1)?,
        name: r.get(2)?,
        adapter_kind: r.get(3)?,
        enabled: r.get::<_, i64>(4)? != 0,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
    })
}

const VERSION_COLUMNS: &str = "id, profile_id, version_no, persona_object_sha256, sop_object_sha256, capabilities_json, tool_policy_json, output_schema_sha256, model_route_json, budget_json, health_policy_json, content_digest, created_at";

fn row_version(r: &rusqlite::Row<'_>) -> rusqlite::Result<ProfileVersion> {
    Ok(ProfileVersion {
        id: r.get(0)?,
        profile_id: r.get(1)?,
        version_no: r.get(2)?,
        persona_object_sha256: r.get(3)?,
        sop_object_sha256: r.get(4)?,
        capabilities: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
        output_schema_sha256: r.get(7)?,
        model_route_json: r.get(8)?,
        budget_json: r.get(9)?,
        content_digest: r.get(11)?,
        created_at: r.get(12)?,
    })
}

fn put_text(store: &Store, text: &str) -> Result<String, Error> {
    if text.trim().is_empty() {
        return Ok(String::new());
    }
    let info = objects::put(store, text.as_bytes(), objects::PutOptions::default())
        .map_err(|e| Error::Message(format!("agent_profile: 存储失败 {e}")))?;
    Ok(info.sha256)
}

pub fn create_profile(
    store: &Store,
    project_id: Option<&str>,
    name: &str,
    adapter_kind: &str,
) -> Result<Profile, Error> {
    if name.trim().is_empty() {
        return Err(Error::Message("agent_profile: name required".into()));
    }
    if !matches!(adapter_kind, "local_harness" | "external_agent") {
        return Err(Error::Message("agent_profile: adapter_kind 非法".into()));
    }
    // 全局 profile 名字唯一（project_id NULL 在 SQLite UNIQUE 中互不相斥，应用层补齐）。
    let dup: i64 = store.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM agent_profiles WHERE name=?1 AND project_id IS ?2",
            rusqlite::params![name, project_id],
            |r| r.get(0),
        )
        .map_err(Error::from)
    })?;
    if dup > 0 {
        return Err(Error::Message(format!("agent_profile: {name} 已存在")));
    }
    let id = ids::new_id("ap");
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO agent_profiles(id, project_id, name, adapter_kind, enabled, created_at, updated_at)
             VALUES (?1,?2,?3,?4,1,?5,?5)",
            rusqlite::params![id, project_id, name, adapter_kind, now],
        )?;
        Ok(())
    })?;
    get_profile(store, &id)
}

pub fn get_profile(store: &Store, profile_id: &str) -> Result<Profile, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            &format!("SELECT {PROFILE_COLUMNS} FROM agent_profiles WHERE id=?1"),
            [profile_id],
            row_profile,
        )
        .map_err(|_| Error::Message(format!("not_found: agent profile {profile_id}")))
    })
}

pub fn list_profiles(store: &Store, project_id: Option<&str>) -> Result<Vec<Profile>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {PROFILE_COLUMNS} FROM agent_profiles
             WHERE project_id IS ?1 OR project_id = '' OR enabled=1
             ORDER BY created_at"
        ))?;
        let rows = stmt.query_map(
            rusqlite::params![project_id.filter(|p| !p.is_empty())],
            row_profile,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

pub fn set_profile_enabled(store: &Store, profile_id: &str, enabled: bool) -> Result<(), Error> {
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE agent_profiles SET enabled=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![enabled as i64, timefmt::now(), profile_id],
        )?;
        Ok(())
    })?;
    Ok(())
}

/// 冻结一个新版本：正文进 objects；digest = sha256(canonical 版本内容)。
#[allow(clippy::too_many_arguments)]
pub fn create_version(
    store: &Store,
    profile_id: &str,
    persona: &str,
    sop: &str,
    capabilities: &[String],
    output_schema: &str,
    model_route_json: &str,
    budget_json: &str,
) -> Result<ProfileVersion, Error> {
    let _profile = get_profile(store, profile_id)?;
    let persona_sha = put_text(store, persona)?;
    let sop_sha = put_text(store, sop)?;
    let schema_sha = put_text(store, output_schema)?;
    let caps_json = serde_json::to_string(capabilities).unwrap_or_else(|_| "[]".into());
    let mut hasher = Sha256::new();
    for part in [
        persona_sha.as_str(),
        sop_sha.as_str(),
        caps_json.as_str(),
        schema_sha.as_str(),
        model_route_json,
        budget_json,
    ] {
        hasher.update(part.as_bytes());
        hasher.update(b"|");
    }
    let digest = ids::hex(&hasher.finalize());
    // 同内容幂等：digest 相同返回既有版本。
    if let Some(existing) = store.with_conn(|conn| {
        let v = conn
            .query_row(
                &format!(
                    "SELECT {VERSION_COLUMNS} FROM agent_profile_versions
                     WHERE profile_id=?1 AND content_digest=?2"
                ),
                [profile_id, &digest],
                row_version,
            )
            .ok();
        Ok(v)
    })? {
        return Ok(existing);
    }
    let version_no: i64 = store.with_conn(|conn| {
        conn.query_row(
            "SELECT COALESCE(MAX(version_no),0) FROM agent_profile_versions WHERE profile_id=?1",
            [profile_id],
            |r| r.get(0),
        )
        .map_err(Error::from)
    })?;
    let id = ids::new_id("apv");
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO agent_profile_versions(id, profile_id, version_no, persona_object_sha256, sop_object_sha256, capabilities_json, tool_policy_json, output_schema_sha256, model_route_json, budget_json, health_policy_json, content_digest, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,'{}',?7,?8,?9,'{}',?10,?11)",
            rusqlite::params![
                id, profile_id, version_no + 1, persona_sha, sop_sha, caps_json,
                schema_sha, model_route_json, budget_json, digest, timefmt::now()
            ],
        )?;
        Ok(())
    })?;
    get_version(store, &id)
}

pub fn get_version(store: &Store, version_id: &str) -> Result<ProfileVersion, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            &format!("SELECT {VERSION_COLUMNS} FROM agent_profile_versions WHERE id=?1"),
            [version_id],
            row_version,
        )
        .map_err(|_| Error::Message(format!("not_found: profile version {version_id}")))
    })
}

pub fn versions(store: &Store, profile_id: &str) -> Result<Vec<ProfileVersion>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {VERSION_COLUMNS} FROM agent_profile_versions WHERE profile_id=?1 ORDER BY version_no"
        ))?;
        let rows = stmt.query_map([profile_id], row_version)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

/// persona/SOP 正文（从 objects 读；空串表示未配置）。
pub fn version_texts(store: &Store, version: &ProfileVersion) -> Result<(String, String), Error> {
    let persona = if version.persona_object_sha256.is_empty() {
        String::new()
    } else {
        String::from_utf8_lossy(&sg_store::objects::open(
            store,
            &version.persona_object_sha256,
        )?)
        .to_string()
    };
    let sop = if version.sop_object_sha256.is_empty() {
        String::new()
    } else {
        String::from_utf8_lossy(&sg_store::objects::open(store, &version.sop_object_sha256)?)
            .to_string()
    };
    Ok((persona, sop))
}

/// 内置通用 AgentProfile（固定版本 + digest；幂等）。
pub fn ensure_builtin_generic(store: &Store) -> Result<ProfileVersion, Error> {
    const BUILTIN_NAME: &str = "builtin-generic";
    let existing: Option<String> = store.with_conn(|conn| {
        let id = conn
            .query_row(
                "SELECT id FROM agent_profiles WHERE name=?1 AND project_id IS NULL",
                [BUILTIN_NAME],
                |r| r.get::<_, String>(0),
            )
            .ok();
        Ok(id)
    })?;
    let profile_id = match existing {
        Some(id) => id,
        None => {
            let id = ids::new_id("ap");
            let now = timefmt::now();
            store.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO agent_profiles(id, project_id, name, adapter_kind, enabled, created_at, updated_at)
                     VALUES (?1,NULL,'builtin-generic','local_harness',1,?2,?2)",
                    rusqlite::params![id, now],
                )?;
                Ok(())
            })?;
            id
        }
    };
    create_version(
        store,
        &profile_id,
        "你是 SixGates 内置通用交付 Agent：在不假设专用角色的情况下完成任务。",
        "通用作业流程：理解目标 → 检索必要上下文 → 产出 → 自检。",
        &[],
        "",
        "{}",
        "{}",
    )
}

/// 绑定（蓝图 §5.2 stage_agent_bindings）。
#[allow(clippy::too_many_arguments)]
pub fn set_binding(
    store: &Store,
    project_id: Option<&str>,
    gate: &str,
    activity_key: &str,
    profile_version_id: &str,
    fallback_mode: &str,
    priority: i64,
) -> Result<(), Error> {
    if !matches!(fallback_mode, "generic" | "fail_closed") {
        return Err(Error::Message("agent_binding: fallback_mode 非法".into()));
    }
    get_version(store, profile_version_id)?;
    let now = timefmt::now();
    let existing: Option<(String, i64)> = store.with_conn(|conn| {
        let row = conn
            .query_row(
                "SELECT id, revision FROM stage_agent_bindings
                 WHERE project_id IS ?1 AND gate=?2 AND activity_key=?3 AND priority=?4",
                rusqlite::params![project_id, gate, activity_key, priority],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )
            .ok();
        Ok(row)
    })?;
    match existing {
        Some((id, revision)) => {
            store.with_conn(|conn| {
                conn.execute(
                    "UPDATE stage_agent_bindings SET profile_version_id=?1, fallback_mode=?2, enabled=1, revision=?3, updated_at=?4 WHERE id=?5",
                    rusqlite::params![profile_version_id, fallback_mode, revision + 1, now, id],
                )?;
                Ok(())
            })?;
        }
        None => {
            store.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO stage_agent_bindings(id, project_id, gate, activity_key, profile_version_id, fallback_mode, priority, enabled, revision, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,1,1,?8)",
                    rusqlite::params![ids::new_id("sab"), project_id, gate, activity_key, profile_version_id, fallback_mode, priority, now],
                )?;
                Ok(())
            })?;
        }
    }
    Ok(())
}

pub fn remove_binding(store: &Store, binding_id: &str) -> Result<(), Error> {
    store.with_conn(|conn| {
        conn.execute("DELETE FROM stage_agent_bindings WHERE id=?1", [binding_id])?;
        Ok(())
    })?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct Binding {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub gate: String,
    pub activity_key: String,
    pub profile_version_id: String,
    pub fallback_mode: String,
    pub priority: i64,
    pub enabled: bool,
    pub revision: i64,
    pub updated_at: String,
}

pub fn list_bindings(store: &Store, project_id: Option<&str>) -> Result<Vec<Binding>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, project_id, gate, activity_key, profile_version_id, fallback_mode, priority, enabled, revision, updated_at
             FROM stage_agent_bindings
             WHERE project_id IS NULL OR project_id = ?1 OR (?1 = '' AND project_id IS NULL)
             ORDER BY gate, activity_key, priority DESC",
        )?;
        let project = project_id.unwrap_or("");
        let rows = stmt.query_map([project], |r| {
            Ok(Binding {
                id: r.get(0)?,
                project_id: r.get(1)?,
                gate: r.get(2)?,
                activity_key: r.get(3)?,
                profile_version_id: r.get(4)?,
                fallback_mode: r.get(5)?,
                priority: r.get(6)?,
                enabled: r.get::<_, i64>(7)? != 0,
                revision: r.get(8)?,
                updated_at: r.get(9)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

/// 适配器健康（M4 诚实实现）：local_harness 恒健康；external_agent 需要已配置模型
/// （model_profiles 存在），未配置即 unhealthy（选路时回退/fail-closed）。
pub fn adapter_health(store: &Store, profile: &Profile) -> Result<(bool, String), Error> {
    match profile.adapter_kind.as_str() {
        "local_harness" => Ok((true, String::new())),
        "external_agent" => {
            let count: i64 = store.with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM model_profiles", [], |r| r.get(0))
                    .map_err(Error::from)
            })?;
            if count > 0 {
                Ok((true, String::new()))
            } else {
                Ok((false, "model_not_configured".into()))
            }
        }
        other => Ok((false, format!("unknown_adapter:{other}"))),
    }
}
