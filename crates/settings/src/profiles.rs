//! Profile 仓储：model/gitlab/ssh 的 CRUD（revision 乐观锁 + managed 只读 + 依赖检查）。
use serde::Serialize;
use serde_json::{json, Value};

use crate::{codes, credentials, store_err, SettingsError, SettingsResult};
use sg_store::{ids, timefmt, Store};

#[derive(Debug, Clone, Serialize)]
pub struct ModelProfile {
    pub id: String,
    pub revision: i64,
    pub name: String,
    pub provider_kind: String,
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_ref_id: Option<String>,
    pub default_model: String,
    pub capabilities: Value,
    pub limits: Value,
    pub data_policy: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_source: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tested_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitlabProfile {
    pub id: String,
    pub revision: i64,
    pub name: String,
    pub base_url: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_ref_id: Option<String>,
    pub capabilities: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_source: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tested_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SshTarget {
    pub id: String,
    pub revision: i64,
    pub name: String,
    pub host: String,
    pub port: i64,
    pub username: String,
    pub remote_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_ref_id: Option<String>,
    pub jump_host: String,
    pub fingerprint: String,
    pub fingerprint_status: String,
    pub allowed_commands: Value,
    pub project_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tested_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

fn opt(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(String::from)
}

/// 可选外键：空串 → NULL（FK 列传 "" 会触发引用校验）。
fn fk_opt(v: &Value, key: &str) -> Option<String> {
    match opt(v, key) {
        Some(s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

// --- ModelProfile ---

pub fn model_list(store: &Store) -> SettingsResult<Vec<ModelProfile>> {
    let ids: Vec<String> = store
        .with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT id FROM model_profiles ORDER BY created_at")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .map_err(store_err)?;
    ids.iter().map(|id| model_get(store, id)).collect()
}

pub fn model_get(store: &Store, id: &str) -> SettingsResult<ModelProfile> {
    store.with_conn(|conn| Ok(conn.query_row(
            "SELECT id, revision, name, provider_kind, base_url, COALESCE(credential_ref_id,''), default_model,
                    capabilities_json, limits_json, data_policy_json, COALESCE(managed_source,''), status,
                    COALESCE(last_tested_at,''), created_at, updated_at
             FROM model_profiles WHERE id=?1",
            [id],
            row_model,
        ).ok()))
        .map_err(store_err)?
        .ok_or_else(|| SettingsError::new("NOT_FOUND", format!("模型 Profile {id} 不存在")))
}

type ModelRow<'a> = rusqlite::Row<'a>;
fn row_model(r: &ModelRow<'_>) -> rusqlite::Result<ModelProfile> {
    let cred: String = r.get(5)?;
    let managed: String = r.get(10)?;
    let tested: String = r.get(12)?;
    Ok(ModelProfile {
        id: r.get(0)?,
        revision: r.get(1)?,
        name: r.get(2)?,
        provider_kind: r.get(3)?,
        base_url: r.get(4)?,
        credential_ref_id: if cred.is_empty() { None } else { Some(cred) },
        default_model: r.get(6)?,
        capabilities: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default(),
        limits: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
        data_policy: serde_json::from_str(&r.get::<_, String>(9)?).unwrap_or_default(),
        managed_source: if managed.is_empty() {
            None
        } else {
            Some(managed)
        },
        status: r.get(11)?,
        last_tested_at: if tested.is_empty() {
            None
        } else {
            Some(tested)
        },
        created_at: r.get(13)?,
        updated_at: r.get(14)?,
    })
}

pub fn model_create(store: &Store, p: &Value) -> SettingsResult<ModelProfile> {
    let name = opt(p, "name").ok_or_else(|| {
        SettingsError::new("INVALID_PARAMS", "name 必填")
            .with_fields(serde_json::json!({"name":"必填"}))
    })?;
    let kind = opt(p, "providerKind").unwrap_or_else(|| "openai_compatible".into());
    if !matches!(kind.as_str(), "openai_compatible" | "fake") {
        return Err(SettingsError::new(
            "INVALID_PARAMS",
            format!("providerKind {kind} 不受支持"),
        ));
    }
    let id = ids::new_id("mp");
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO model_profiles(id, name, provider_kind, base_url, credential_ref_id, default_model,
                capabilities_json, limits_json, data_policy_json, status, revision, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'configured',1,?10,?10)",
            rusqlite::params![
                id, name, kind,
                opt(p, "baseUrl").unwrap_or_default(),
                fk_opt(p, "credentialRefId"),
                opt(p, "defaultModel").unwrap_or_default(),
                p.get("capabilities").unwrap_or(&Value::Null).to_string(),
                p.get("limits").unwrap_or(&Value::Null).to_string(),
                p.get("dataPolicy").unwrap_or(&Value::Null).to_string(),
                now,
            ],
        )?;
        Ok(())
    }).map_err(store_err)?;
    model_get(store, &id)
}

pub fn model_update(
    store: &Store,
    id: &str,
    p: &Value,
    expected_revision: i64,
) -> SettingsResult<ModelProfile> {
    let current = model_get(store, id)?;
    if current.managed_source.is_some() {
        return Err(SettingsError::new(
            codes::MANAGED_READ_ONLY,
            "环境导入的 Profile 只读；替代路径：创建新 Profile",
        ));
    }
    if current.revision != expected_revision {
        return Err(SettingsError::new(
            codes::REVISION_CONFLICT,
            format!(
                "期望 revision {expected_revision} 实际 {}",
                current.revision
            ),
        ));
    }
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE model_profiles SET
                name=COALESCE(?1,name), base_url=COALESCE(?2,base_url),
                credential_ref_id=COALESCE(?3,credential_ref_id), default_model=COALESCE(?4,default_model),
                limits_json=COALESCE(?5,limits_json), data_policy_json=COALESCE(?6,data_policy_json),
                status='configured', revision=revision+1, updated_at=?7
             WHERE id=?8",
            rusqlite::params![
                opt(p, "name"), opt(p, "baseUrl"), fk_opt(p, "credentialRefId"), opt(p, "defaultModel"),
                p.get("limits").map(|v| v.to_string()), p.get("dataPolicy").map(|v| v.to_string()),
                now, id,
            ],
        )?;
        Ok(())
    }).map_err(store_err)?;
    model_get(store, id)
}

/// 删除前检查路由引用。
pub fn model_remove(store: &Store, id: &str, expected_revision: i64) -> SettingsResult<()> {
    let current = model_get(store, id)?;
    if current.managed_source.is_some() {
        return Err(SettingsError::new(
            codes::MANAGED_READ_ONLY,
            "托管 Profile 不可删除",
        ));
    }
    if current.revision != expected_revision {
        return Err(SettingsError::new(
            codes::REVISION_CONFLICT,
            "revision 不匹配",
        ));
    }
    let routes: i64 = store
        .with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM model_routes WHERE primary_profile_id=?1",
                [id],
                |r| r.get(0),
            )?)
        })
        .map_err(store_err)?;
    if routes > 0 {
        return Err(SettingsError::new(
            "CONFLICT",
            format!("Profile 被 {routes} 条路由引用"),
        ));
    }
    store
        .with_conn(|conn| {
            conn.execute("DELETE FROM model_profiles WHERE id=?1", [id])?;
            Ok(())
        })
        .map_err(store_err)?;
    Ok(())
}

pub fn model_mark_tested(store: &Store, id: &str, status: &str) -> SettingsResult<ModelProfile> {
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE model_profiles SET status=?1, last_tested_at=?2, revision=revision+1, updated_at=?2 WHERE id=?3",
            rusqlite::params![status, timefmt::now(), id],
        )?;
        Ok(())
    }).map_err(store_err)?;
    model_get(store, id)
}

// --- model_routes ---

pub fn route_get(store: &Store) -> SettingsResult<Value> {
    let rows: Vec<(String, String, String, String, Value, i64)> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, scope, task_kind, primary_profile_id, budget_json, revision FROM model_routes",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
                r.get::<_, i64>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }).map_err(store_err)?;
    Ok(serde_json::json!(rows.iter().map(|(id, scope, kind, primary, budget, rev)| {
        serde_json::json!({"id": id, "scope": scope, "taskKind": kind, "primaryProfileId": primary, "budget": budget, "revision": rev})
    }).collect::<Vec<_>>()))
}

pub fn route_update(store: &Store, route: &Value, expected_revision: i64) -> SettingsResult<Value> {
    let task_kind = opt(route, "taskKind")
        .ok_or_else(|| SettingsError::new("INVALID_PARAMS", "taskKind 必填"))?;
    let primary = opt(route, "primaryProfileId")
        .ok_or_else(|| SettingsError::new("INVALID_PARAMS", "primaryProfileId 必填"))?;
    model_get(store, &primary)?; // 存在性校验
    let scope = opt(route, "scope").unwrap_or_else(|| "global".into());
    let current: Option<i64> = store
        .with_conn(|conn| {
            let result: rusqlite::Result<i64> = conn.query_row(
                "SELECT revision FROM model_routes WHERE scope=?1 AND task_kind=?2",
                [&scope, &task_kind],
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
                    "INSERT INTO model_routes(id, scope, task_kind, primary_profile_id, fallback_json, budget_json, revision, updated_at)
                     VALUES (?1,?2,?3,?4,'[]',?5,1,?6)",
                    rusqlite::params![ids::new_id("mr"), scope, task_kind, primary,
                        route.get("budget").unwrap_or(&Value::Null).to_string(), now],
                )?;
                Ok(())
            }).map_err(store_err)?;
        }
        Some(rev) => {
            if rev != expected_revision {
                return Err(SettingsError::new(
                    codes::REVISION_CONFLICT,
                    format!("路由期望 revision {expected_revision} 实际 {rev}"),
                ));
            }
            store.with_conn(|conn| {
                conn.execute(
                    "UPDATE model_routes SET primary_profile_id=?1, budget_json=COALESCE(?2,budget_json),
                            fallback_json=COALESCE(?3,fallback_json), revision=revision+1, updated_at=?4
                     WHERE scope=?5 AND task_kind=?6",
                    rusqlite::params![primary,
                        route.get("budget").map(|v| v.to_string()),
                        route.get("fallback").map(|v| v.to_string()),
                        now, scope, task_kind],
                )?;
                Ok(())
            }).map_err(store_err)?;
        }
    }
    route_get(store)
}

// --- GitLab / SSH CRUD 通用小宏 ---

pub fn gitlab_list(store: &Store) -> SettingsResult<Vec<GitlabProfile>> {
    let ids: Vec<String> = store
        .with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT id FROM gitlab_profiles ORDER BY created_at")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .map_err(store_err)?;
    ids.iter().map(|id| gitlab_get(store, id)).collect()
}

pub fn gitlab_get(store: &Store, id: &str) -> SettingsResult<GitlabProfile> {
    let row: Option<GitlabProfile> = store.with_conn(|conn| {
        Ok(conn.query_row(
            "SELECT id, revision, name, base_url, display_name, COALESCE(credential_ref_id,''),
                    capabilities_json, COALESCE(managed_source,''), status, COALESCE(last_tested_at,''),
                    created_at, updated_at
             FROM gitlab_profiles WHERE id=?1",
            [id],
            |r| {
                let cred: String = r.get(5)?;
                let managed: String = r.get(7)?;
                let tested: String = r.get(9)?;
                Ok(GitlabProfile {
                    id: r.get(0)?, revision: r.get(1)?, name: r.get(2)?, base_url: r.get(3)?,
                    display_name: r.get(4)?,
                    credential_ref_id: if cred.is_empty() { None } else { Some(cred) },
                    capabilities: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
                    managed_source: if managed.is_empty() { None } else { Some(managed) },
                    status: r.get(8)?,
                    last_tested_at: if tested.is_empty() { None } else { Some(tested) },
                    created_at: r.get(10)?, updated_at: r.get(11)?,
                })
            },
        ).ok())
    }).map_err(store_err)?;
    row.ok_or_else(|| SettingsError::new("NOT_FOUND", format!("GitLab Profile {id} 不存在")))
}

pub fn gitlab_create(store: &Store, p: &Value) -> SettingsResult<GitlabProfile> {
    let name = opt(p, "name").ok_or_else(|| SettingsError::new("INVALID_PARAMS", "name 必填"))?;
    let base_url =
        opt(p, "baseUrl").ok_or_else(|| SettingsError::new("INVALID_PARAMS", "baseUrl 必填"))?;
    let id = ids::new_id("glp");
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO gitlab_profiles(id, name, base_url, display_name, credential_ref_id, status, revision, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,'configured',1,?6,?6)",
            rusqlite::params![id, name, base_url, opt(p, "displayName").unwrap_or_default(),
                fk_opt(p, "credentialRefId"), now],
        )?;
        Ok(())
    }).map_err(store_err)?;
    gitlab_get(store, &id)
}

pub fn gitlab_update(
    store: &Store,
    id: &str,
    p: &Value,
    expected_revision: i64,
) -> SettingsResult<GitlabProfile> {
    let current = gitlab_get(store, id)?;
    if current.managed_source.is_some() {
        return Err(SettingsError::new(
            codes::MANAGED_READ_ONLY,
            "托管 Profile 只读",
        ));
    }
    if current.revision != expected_revision {
        return Err(SettingsError::new(
            codes::REVISION_CONFLICT,
            "revision 不匹配",
        ));
    }
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE gitlab_profiles SET name=COALESCE(?1,name), base_url=COALESCE(?2,base_url),
                    display_name=COALESCE(?3,display_name), credential_ref_id=COALESCE(?4,credential_ref_id),
                    status='configured', revision=revision+1, updated_at=?5
             WHERE id=?6",
            rusqlite::params![opt(p, "name"), opt(p, "baseUrl"), opt(p, "displayName"),
                fk_opt(p, "credentialRefId"), timefmt::now(), id],
        )?;
        Ok(())
    }).map_err(store_err)?;
    gitlab_get(store, id)
}

pub fn gitlab_remove(store: &Store, id: &str, expected_revision: i64) -> SettingsResult<()> {
    let current = gitlab_get(store, id)?;
    if current.revision != expected_revision {
        return Err(SettingsError::new(
            codes::REVISION_CONFLICT,
            "revision 不匹配",
        ));
    }
    store
        .with_conn(|conn| {
            conn.execute("DELETE FROM gitlab_profiles WHERE id=?1", [id])?;
            Ok(())
        })
        .map_err(store_err)?;
    Ok(())
}

pub fn ssh_list(store: &Store) -> SettingsResult<Vec<SshTarget>> {
    let ids: Vec<String> = store
        .with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT id FROM ssh_targets ORDER BY created_at")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .map_err(store_err)?;
    ids.iter().map(|id| ssh_get(store, id)).collect()
}

pub fn ssh_get(store: &Store, id: &str) -> SettingsResult<SshTarget> {
    let row: Option<SshTarget> = store.with_conn(|conn| {
        Ok(conn.query_row(
            "SELECT id, revision, name, host, port, username, remote_dir, COALESCE(credential_ref_id,''),
                    jump_host, fingerprint, fingerprint_status, allowed_commands_json, project_id, status,
                    COALESCE(last_tested_at,''), created_at, updated_at
             FROM ssh_targets WHERE id=?1",
            [id],
            |r| {
                let cred: String = r.get(7)?;
                let tested: String = r.get(14)?;
                Ok(SshTarget {
                    id: r.get(0)?, revision: r.get(1)?, name: r.get(2)?, host: r.get(3)?,
                    port: r.get(4)?, username: r.get(5)?, remote_dir: r.get(6)?,
                    credential_ref_id: if cred.is_empty() { None } else { Some(cred) },
                    jump_host: r.get(8)?, fingerprint: r.get(9)?, fingerprint_status: r.get(10)?,
                    allowed_commands: serde_json::from_str(&r.get::<_, String>(11)?).unwrap_or_default(),
                    project_id: r.get(12)?, status: r.get(13)?,
                    last_tested_at: if tested.is_empty() { None } else { Some(tested) },
                    created_at: r.get(15)?, updated_at: r.get(16)?,
                })
            },
        ).ok())
    }).map_err(store_err)?;
    row.ok_or_else(|| SettingsError::new("NOT_FOUND", format!("SSH 目标 {id} 不存在")))
}

pub fn ssh_create(store: &Store, p: &Value) -> SettingsResult<SshTarget> {
    let host = opt(p, "host").ok_or_else(|| SettingsError::new("INVALID_PARAMS", "host 必填"))?;
    let user = opt(p, "user").ok_or_else(|| SettingsError::new("INVALID_PARAMS", "user 必填"))?;
    let id = ids::new_id("ssh");
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO ssh_targets(id, name, host, port, username, remote_dir, credential_ref_id, jump_host,
                project_id, status, revision, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'configured',1,?10,?10)",
            rusqlite::params![id,
                opt(p, "name").unwrap_or_else(|| host.clone()),
                host,
                p.get("port").and_then(|v| v.as_i64()).unwrap_or(22),
                user,
                opt(p, "remoteDir").unwrap_or_default(),
                fk_opt(p, "credentialRefId"),
                opt(p, "jumpHost").unwrap_or_default(),
                opt(p, "projectId").unwrap_or_default(),
                now],
        )?;
        Ok(())
    }).map_err(store_err)?;
    ssh_get(store, &id)
}

pub fn ssh_update(
    store: &Store,
    id: &str,
    p: &Value,
    expected_revision: i64,
) -> SettingsResult<SshTarget> {
    let current = ssh_get(store, id)?;
    if current.revision != expected_revision {
        return Err(SettingsError::new(
            codes::REVISION_CONFLICT,
            "revision 不匹配",
        ));
    }
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE ssh_targets SET name=COALESCE(?1,name), host=COALESCE(?2,host), port=COALESCE(?3,port),
                    username=COALESCE(?4,username), remote_dir=COALESCE(?5,remote_dir),
                    credential_ref_id=COALESCE(?6,credential_ref_id), jump_host=COALESCE(?7,jump_host),
                    status='configured', revision=revision+1, updated_at=?8
             WHERE id=?9",
            rusqlite::params![opt(p, "name"), opt(p, "host"), p.get("port").and_then(|v| v.as_i64()),
                opt(p, "user"), opt(p, "remoteDir"), fk_opt(p, "credentialRefId"), opt(p, "jumpHost"),
                timefmt::now(), id],
        )?;
        Ok(())
    }).map_err(store_err)?;
    ssh_get(store, id)
}

pub fn ssh_remove(store: &Store, id: &str, expected_revision: i64) -> SettingsResult<()> {
    let current = ssh_get(store, id)?;
    if current.revision != expected_revision {
        return Err(SettingsError::new(
            codes::REVISION_CONFLICT,
            "revision 不匹配",
        ));
    }
    store
        .with_conn(|conn| {
            conn.execute("DELETE FROM ssh_targets WHERE id=?1", [id])?;
            Ok(())
        })
        .map_err(store_err)?;
    Ok(())
}

/// 首次指纹确认（用户显式 accept）。
pub fn ssh_accept_host_key(
    store: &Store,
    id: &str,
    fingerprint: &str,
) -> SettingsResult<SshTarget> {
    let current = ssh_get(store, id)?;
    if current.fingerprint_status == "changed" {
        return Err(SettingsError::new(
            "HOST_KEY_CHANGED",
            "指纹已变化；确认新指纹前阻断",
        ));
    }
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE ssh_targets SET fingerprint=?1, fingerprint_status='accepted', revision=revision+1, updated_at=?2 WHERE id=?3",
            rusqlite::params![fingerprint, timefmt::now(), id],
        )?;
        Ok(())
    }).map_err(store_err)?;
    ssh_get(store, id)
}

/// 测试后回写状态（不改指纹——首用确认必须显式）。
pub fn ssh_mark_tested(store: &Store, id: &str, status: &str) -> SettingsResult<SshTarget> {
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE ssh_targets SET status=?1, last_tested_at=?2, revision=revision+1, updated_at=?2 WHERE id=?3",
            rusqlite::params![status, timefmt::now(), id],
        )?;
        Ok(())
    }).map_err(store_err)?;
    ssh_get(store, id)
}

/// env 导入（幂等）：环境变量存在时注册 managed 只读 Profile，写入标记。
pub fn import_env_profiles(store: &Store) -> SettingsResult<Vec<String>> {
    let mut imported = Vec::new();
    let now = timefmt::now();
    let env_model = std::env::var("SIXGATES_MODEL_BASE_URL")
        .ok()
        .filter(|v| !v.is_empty());
    if let Some(base) = env_model {
        let id = "mp_env_model";
        let exists: Option<String> = store
            .with_conn(|conn| {
                let result: rusqlite::Result<String> =
                    conn.query_row("SELECT id FROM model_profiles WHERE id=?1", [id], |r| {
                        r.get(0)
                    });
                Ok(result.ok())
            })
            .map_err(store_err)?;
        store.with_conn(|conn| {
            if exists.is_none() {
                conn.execute(
                    "INSERT INTO model_profiles(id, name, provider_kind, base_url, default_model, managed_source, status, revision, created_at, updated_at)
                     VALUES (?1,'环境变量模型','openai_compatible',?2,?3,'env','managed_read_only',1,?4,?4)",
                    rusqlite::params![id, base, std::env::var("SIXGATES_MODEL_NAME").unwrap_or_default(), now],
                )?;
            }
            conn.execute(
                "INSERT OR IGNORE INTO settings_managed_imports(source, resource_type, resource_id, imported_at) VALUES ('env','model_profile',?1,?2)",
                rusqlite::params![id, now],
            )?;
            Ok(())
        }).map_err(store_err)?;
        imported.push(id.into());
    }
    let env_gitlab = std::env::var("SIXGATES_GITLAB_URL")
        .ok()
        .filter(|v| !v.is_empty());
    if let Some(base) = env_gitlab {
        let id = "glp_env_gitlab";
        let exists: Option<String> = store
            .with_conn(|conn| {
                let result: rusqlite::Result<String> =
                    conn.query_row("SELECT id FROM gitlab_profiles WHERE id=?1", [id], |r| {
                        r.get(0)
                    });
                Ok(result.ok())
            })
            .map_err(store_err)?;
        store.with_conn(|conn| {
            if exists.is_none() {
                conn.execute(
                    "INSERT INTO gitlab_profiles(id, name, base_url, managed_source, status, revision, created_at, updated_at)
                     VALUES (?1,'环境变量 GitLab',?2,'env','managed_read_only',1,?3,?3)",
                    rusqlite::params![id, base, now],
                )?;
            }
            conn.execute(
                "INSERT OR IGNORE INTO settings_managed_imports(source, resource_type, resource_id, imported_at) VALUES ('env','gitlab_profile',?1,?2)",
                rusqlite::params![id, now],
            )?;
            Ok(())
        }).map_err(store_err)?;
        imported.push(id.into());
    }
    Ok(imported)
}

/// 为集成适配器读取秘密（最窄作用域）。kind 用于审计定位，不参与读取逻辑。
pub fn reveal_for(
    store: &Store,
    backend: &dyn credentials::CredentialStore,
    ref_id: &str,
) -> SettingsResult<String> {
    credentials::get(store, ref_id)?;
    credentials::reveal(backend, ref_id)
}

/// 测试后回写状态（不动 revision 之外的配置）。
pub fn gitlab_update_status(store: &Store, id: &str, status: &str) -> SettingsResult<()> {
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE gitlab_profiles SET status=?1, last_tested_at=?2, revision=revision+1, updated_at=?2 WHERE id=?3",
            rusqlite::params![status, timefmt::now(), id],
        )?;
        Ok(())
    }).map_err(store_err)
}

/// SSH 目标绑定项目（S31）：project_id + 是否允许自动部署。
pub fn ssh_bind_project(
    store: &Store,
    target_id: &str,
    project_id: &str,
    bind: bool,
    allow_auto_deploy: bool,
) -> SettingsResult<SshTarget> {
    let current = ssh_get(store, target_id)?;
    let new_project = if bind {
        project_id.to_string()
    } else {
        String::new()
    };
    let commands = if allow_auto_deploy {
        json!([
            "docker compose up -d",
            "docker compose down",
            "docker compose ps"
        ])
    } else {
        current.allowed_commands.clone()
    };
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE ssh_targets SET project_id=?1, allowed_commands_json=?2, revision=revision+1, updated_at=?3 WHERE id=?4",
            rusqlite::params![new_project, commands.to_string(), timefmt::now(), target_id],
        )?;
        Ok(())
    }).map_err(store_err)?;
    ssh_get(store, target_id)
}
