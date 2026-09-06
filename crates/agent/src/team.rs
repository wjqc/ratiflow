//! Agent Team 解析（EvoFlow 方案 M4-03 / ADR-038 §6.8）：
//! TeamVersion 冻结 role_key → profile_version_id 成员表；解析带 fallback 证据
//! （generic = 回退内置通用 profile；fail_closed = 专属不可用即失败）。
//! Lead/Supervisor/worker 是角色约定不是硬编码。

use serde::Serialize;
use sg_store::{ids, timefmt, Error, Store};

pub const GENERIC_PROFILE_LABEL: &str = "builtin_generic";

#[derive(Debug, Clone, Serialize)]
pub struct TeamRecord {
    pub id: String,
    pub key: String,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TeamVersionRecord {
    pub id: String,
    pub team_id: String,
    pub version_no: i64,
    pub status: String,
    pub lead_role_key: String,
    pub max_concurrency: i64,
    pub required_caps_json: String,
    pub review_policy: String,
    pub fallback_mode: String,
    pub content_digest: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TeamMember {
    pub role_key: String,
    pub profile_version_id: String,
    pub fallback_mode: String,
}

fn team_version_row(conn: &rusqlite::Connection, id: &str) -> Result<TeamVersionRecord, Error> {
    conn.query_row(
        "SELECT id, team_id, version_no, status, lead_role_key, max_concurrency,
                required_caps_json, review_policy, fallback_mode, content_digest, created_at
         FROM agent_team_versions WHERE id=?1",
        [id],
        |r| {
            Ok(TeamVersionRecord {
                id: r.get(0)?,
                team_id: r.get(1)?,
                version_no: r.get(2)?,
                status: r.get(3)?,
                lead_role_key: r.get(4)?,
                max_concurrency: r.get(5)?,
                required_caps_json: r.get(6)?,
                review_policy: r.get(7)?,
                fallback_mode: r.get(8)?,
                content_digest: r.get(9)?,
                created_at: r.get(10)?,
            })
        },
    )
    .map_err(|_| Error::Message(format!("team_version_not_found: {id}")))
}

/// 创建 Team 逻辑身份（key 唯一）。
pub fn create_team(store: &Store, key: &str, name: &str) -> Result<TeamRecord, Error> {
    let id = ids::new_id("team");
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO agent_teams(id, key, name, created_at, updated_at) VALUES (?1,?2,?3,?4,?4)",
            rusqlite::params![id, key, name.trim(), now],
        )
        .map_err(|e| {
            if e.to_string().contains("UNIQUE") {
                Error::Message(format!("team_key_exists: {key}"))
            } else {
                e.into()
            }
        })?;
        Ok(TeamRecord {
            id,
            key: key.to_string(),
            name: name.trim().to_string(),
            created_at: now.clone(),
            updated_at: now,
        })
    })
}

#[derive(Debug, Clone)]
pub struct TeamMemberInput {
    pub role_key: String,
    pub profile_version_id: String,
    pub fallback_mode: String,
}

/// 创建 draft 版本：校验成员 profile version 存在、role_key 唯一（PK 保证）、
#[allow(clippy::too_many_arguments)]
/// lead 在成员表内；digest = 成员(role,profile,fallback) 排序 canonical。
pub fn create_version(
    store: &Store,
    team_id: &str,
    lead_role_key: &str,
    max_concurrency: i64,
    required_caps: &[String],
    review_policy: &str,
    fallback_mode: &str,
    members: &[TeamMemberInput],
    created_by: &str,
) -> Result<TeamVersionRecord, Error> {
    use sha2::{Digest, Sha256};
    if members.is_empty() {
        return Err(Error::Message("team_invalid: 至少一个成员".into()));
    }
    if !members.iter().any(|m| m.role_key == lead_role_key) {
        return Err(Error::Message(format!(
            "team_invalid: lead 角色 {lead_role_key} 不在成员表"
        )));
    }
    if !matches!(review_policy, "none" | "peer_review" | "lead_review") {
        return Err(Error::Message("team_invalid: 非法 review_policy".into()));
    }
    if !matches!(fallback_mode, "generic" | "fail_closed") {
        return Err(Error::Message("team_invalid: 非法 fallback_mode".into()));
    }
    // 成员 profile version 存在性 + digest 素材。
    let mut entries: Vec<String> = Vec::new();
    store.with_conn(|conn| {
        for m in members {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM agent_profile_versions WHERE id=?1",
                    [&m.profile_version_id],
                    |r| r.get(0),
                )
                .map_err(Error::from)?;
            if n == 0 {
                return Err(Error::Message(format!(
                    "team_invalid: 成员 {} 的 profile version 不存在",
                    m.role_key
                )));
            }
            entries.push(format!(
                "{}|{}|{}",
                m.role_key, m.profile_version_id, m.fallback_mode
            ));
        }
        Ok(())
    })?;
    entries.sort();
    entries.push(format!("lead|{lead_role_key}"));
    entries.push(format!("concurrency|{max_concurrency}"));
    entries.push(format!("review|{review_policy}"));
    entries.push(format!("fallback|{fallback_mode}"));
    let mut hasher = Sha256::new();
    hasher.update(format!("tv1|{}", entries.join("\n")).as_bytes());
    let digest = sg_store::ids::hex(&hasher.finalize());

    let id = ids::new_id("tv");
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO agent_team_versions(id, team_id, version_no, status, lead_role_key, max_concurrency,
                required_caps_json, review_policy, fallback_mode, content_digest, created_by, created_at, updated_at)
             VALUES (?1,?2,(SELECT COALESCE(MAX(version_no),0)+1 FROM agent_team_versions WHERE team_id=?2),
                'draft',?3,?4,?5,?6,?7,?8,?9,?10,?10)",
            rusqlite::params![
                id,
                team_id,
                lead_role_key,
                max_concurrency,
                serde_json::to_string(required_caps).unwrap_or_default(),
                review_policy,
                fallback_mode,
                digest,
                created_by,
                now
            ],
        )?;
        for m in members {
            conn.execute(
                "INSERT INTO agent_team_members(team_version_id, role_key, profile_version_id, fallback_mode, created_at)
                 VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params![id, m.role_key, m.profile_version_id, m.fallback_mode, now],
            )?;
        }
        team_version_row(conn, &id)
    })
}

/// 激活（draft → active；同 team 旧 active → deprecated）。
pub fn activate(store: &Store, version_id: &str) -> Result<TeamVersionRecord, Error> {
    let cur = store.with_conn(|conn| team_version_row(conn, version_id))?;
    if cur.status == "active" {
        return Ok(cur);
    }
    if cur.status != "draft" {
        return Err(Error::Message(format!(
            "team_invalid: {} 不可激活（仅 draft）",
            cur.status
        )));
    }
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE agent_team_versions SET status='deprecated', updated_at=?1
             WHERE team_id=?2 AND status='active'",
            rusqlite::params![now, cur.team_id],
        )?;
        conn.execute(
            "UPDATE agent_team_versions SET status='active', updated_at=?1 WHERE id=?2",
            rusqlite::params![now, version_id],
        )?;
        team_version_row(conn, version_id)
    })
}

/// 按 key 取 active 版本；key 空 = 默认无团队（返回 None——团队是可选结构）。
pub fn active_version_by_key(store: &Store, key: &str) -> Result<Option<TeamVersionRecord>, Error> {
    store.with_conn(|conn| {
        let row = conn
            .query_row(
                "SELECT v.id FROM agent_team_versions v
                 JOIN agent_teams t ON t.id = v.team_id
                 WHERE t.key=?1 AND v.status='active'",
                [key],
                |r| r.get::<_, String>(0),
            )
            .ok();
        match row {
            Some(id) => team_version_row(conn, &id).map(Some),
            None => Ok(None),
        }
    })
}

pub fn members_of(store: &Store, team_version_id: &str) -> Result<Vec<TeamMember>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT role_key, profile_version_id, fallback_mode
             FROM agent_team_members WHERE team_version_id=?1 ORDER BY role_key",
        )?;
        let rows = stmt.query_map([team_version_id], |r| {
            Ok(TeamMember {
                role_key: r.get(0)?,
                profile_version_id: r.get(1)?,
                fallback_mode: r.get(2)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedMember {
    pub role_key: String,
    /// 命中的 profile version（专属或 generic 回退）。
    pub profile_version_id: String,
    /// 专属命中 = none；回退 = fallback_mode + 原因（证据入 Run 冻结面）。
    pub via: String,
}

/// Role 选路（§6.8）：成员专属 profile version 健在 → 命中；
/// 不可用（版本被删/禁用）→ 按 fallback：generic 回退通用（证据记录），
/// fail_closed → 明确失败（Run 拒启，不静默换人）。
pub fn resolve_role(
    store: &Store,
    team_version_id: &str,
    role_key: &str,
) -> Result<ResolvedMember, Error> {
    let members = members_of(store, team_version_id)?;
    let member = members
        .iter()
        .find(|m| m.role_key == role_key)
        .ok_or_else(|| Error::Message(format!("team_role_missing: {role_key} 不在团队")))?;
    let usable: i64 = store.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM agent_profile_versions v
             JOIN agent_profiles p ON p.id = v.profile_id
             WHERE v.id=?1 AND p.enabled=1",
            [&member.profile_version_id],
            |r| r.get(0),
        )
        .map_err(Error::from)
    })?;
    if usable > 0 {
        return Ok(ResolvedMember {
            role_key: role_key.to_string(),
            profile_version_id: member.profile_version_id.clone(),
            via: "direct".into(),
        });
    }
    if member.fallback_mode == "fail_closed" {
        return Err(Error::Message(format!(
            "team_fail_closed: 角色 {role_key} 专属 profile 不可用且配置 fail_closed"
        )));
    }
    // generic 回退：解析内置通用 profile 的 active 版本（0019 ensure_builtin_generic 固定 id）。
    let generic: Option<String> = store
        .with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT v.id FROM agent_profile_versions v
                     JOIN agent_profiles p ON p.id = v.profile_id
                     WHERE p.name='builtin-generic' AND p.project_id IS NULL AND p.enabled=1
                     ORDER BY v.version_no DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .ok())
        })
        .unwrap_or(None);
    match generic {
        Some(pid) => Ok(ResolvedMember {
            role_key: role_key.to_string(),
            profile_version_id: pid,
            via: format!("fallback:generic ({GENERIC_PROFILE_LABEL})"),
        }),
        None => Err(Error::Message(format!(
            "team_fail_closed: 角色 {role_key} 专属不可用且无通用回退"
        ))),
    }
}

/// 全量 Team 列表（含版本摘要）。
pub fn list_teams(store: &Store) -> Result<Vec<(TeamRecord, Vec<TeamVersionRecord>)>, Error> {
    store.with_conn(|conn| {
        let mut teams = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT id, key, name, created_at, updated_at FROM agent_teams ORDER BY created_at, id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(TeamRecord {
                    id: r.get(0)?,
                    key: r.get(1)?,
                    name: r.get(2)?,
                    created_at: r.get(3)?,
                    updated_at: r.get(4)?,
                })
            })?;
            for row in rows {
                teams.push(row?);
            }
        }
        let mut out = Vec::new();
        for t in teams {
            let mut versions = Vec::new();
            let mut stmt = conn.prepare(
                "SELECT id, team_id, version_no, status, lead_role_key, max_concurrency,
                        required_caps_json, review_policy, fallback_mode, content_digest, created_at
                 FROM agent_team_versions WHERE team_id=?1 ORDER BY version_no",
            )?;
            let rows = stmt.query_map([&t.id], |r| {
                Ok(TeamVersionRecord {
                    id: r.get(0)?,
                    team_id: r.get(1)?,
                    version_no: r.get(2)?,
                    status: r.get(3)?,
                    lead_role_key: r.get(4)?,
                    max_concurrency: r.get(5)?,
                    required_caps_json: r.get(6)?,
                    review_policy: r.get(7)?,
                    fallback_mode: r.get(8)?,
                    content_digest: r.get(9)?,
                    created_at: r.get(10)?,
                })
            })?;
            for row in rows {
                versions.push(row?);
            }
            out.push((t, versions));
        }
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-team-{}-{}",
            std::process::id(),
            sg_store::ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        // 内置通用 profile（0019 ensure_builtin_generic 语义）。
        profile::ensure_builtin_generic(&store).unwrap();
        store
    }

    fn make_profile_version(store: &Store, name: &str) -> String {
        let p = profile::create_profile(store, None, name, "local_harness").unwrap();
        let v = profile::create_version(store, &p.id, "persona", "", &[], "", "{}", "{}").unwrap();
        v.id
    }

    #[test]
    fn team_version_lifecycle_and_activation() {
        let store = setup();
        let team = create_team(&store, "frontend-squad", "前端小组").unwrap();
        let pv = make_profile_version(&store, "前端工程师");
        let v1 = create_version(
            &store,
            &team.id,
            "lead",
            3,
            &["frontend".to_string()],
            "lead_review",
            "generic",
            &[
                TeamMemberInput {
                    role_key: "lead".into(),
                    profile_version_id: pv.clone(),
                    fallback_mode: "fail_closed".into(),
                },
                TeamMemberInput {
                    role_key: "worker".into(),
                    profile_version_id: pv.clone(),
                    fallback_mode: "generic".into(),
                },
            ],
            "admin",
        )
        .unwrap();
        assert_eq!(v1.version_no, 1);
        activate(&store, &v1.id).unwrap();
        assert!(active_version_by_key(&store, "frontend-squad")
            .unwrap()
            .is_some());
        // lead 不在成员表 → 拒绝。
        assert!(create_version(
            &store,
            &team.id,
            "boss",
            3,
            &[],
            "none",
            "generic",
            &[TeamMemberInput {
                role_key: "worker".into(),
                profile_version_id: pv.clone(),
                fallback_mode: "generic".into()
            }],
            "admin",
        )
        .is_err());
        // 不存在的 profile version → 拒绝。
        assert!(create_version(
            &store,
            &team.id,
            "lead",
            3,
            &[],
            "none",
            "generic",
            &[TeamMemberInput {
                role_key: "lead".into(),
                profile_version_id: "pv_ghost".into(),
                fallback_mode: "generic".into()
            }],
            "admin",
        )
        .is_err());
    }

    #[test]
    fn role_resolution_direct_generic_and_fail_closed() {
        let store = setup();
        let team = create_team(&store, "squad2", "小组2").unwrap();
        let pv = make_profile_version(&store, "专属分身");
        // 两个将变得"不可用"的专属（先建后禁用——版本存在性在创建期校验）。
        let p_strict =
            profile::create_profile(&store, None, "strict-owner", "local_harness").unwrap();
        let pv_strict = profile::create_version(&store, &p_strict.id, "p", "", &[], "", "{}", "{}")
            .unwrap()
            .id;
        let p_blocked =
            profile::create_profile(&store, None, "blocked-owner", "local_harness").unwrap();
        let pv_blocked =
            profile::create_version(&store, &p_blocked.id, "p", "", &[], "", "{}", "{}")
                .unwrap()
                .id;
        let v1 = create_version(
            &store,
            &team.id,
            "lead",
            3,
            &[],
            "none",
            "generic",
            &[
                TeamMemberInput {
                    role_key: "lead".into(),
                    profile_version_id: pv.clone(),
                    fallback_mode: "generic".into(),
                },
                TeamMemberInput {
                    role_key: "strict".into(),
                    profile_version_id: pv_strict.clone(),
                    fallback_mode: "generic".into(),
                },
                TeamMemberInput {
                    role_key: "blocked".into(),
                    profile_version_id: pv_blocked.clone(),
                    fallback_mode: "fail_closed".into(),
                },
            ],
            "admin",
        )
        .unwrap();
        activate(&store, &v1.id).unwrap();
        // 直接命中。
        let r = resolve_role(&store, &v1.id, "lead").unwrap();
        assert_eq!(r.via, "direct");
        // 禁用两个专属 profile（模拟 Provider/身份不可用）。
        store
            .with_conn(|conn| {
                conn.execute(
                    "UPDATE agent_profiles SET enabled=0 WHERE id IN (?1, ?2)",
                    rusqlite::params![p_strict.id, p_blocked.id],
                )?;
                Ok(())
            })
            .unwrap();
        // 专属不可用 → generic 回退带证据。
        let r = resolve_role(&store, &v1.id, "strict").unwrap();
        assert!(r.via.starts_with("fallback:generic"), "{:?}", r.via);
        assert_ne!(r.profile_version_id, pv_strict);
        // fail_closed → 明确失败（不静默换人）。
        let err = resolve_role(&store, &v1.id, "blocked").unwrap_err();
        assert!(err.to_string().contains("team_fail_closed"), "{err}");
        // 未知角色。
        assert!(resolve_role(&store, &v1.id, "ghost").is_err());
    }
}
