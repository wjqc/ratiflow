//! 凭据引用：明文只进 OS Keychain（trait + macOS security CLI 实现 + 测试内存实现）。
//! 流程（手册 §6/§10）：prepare 行 → keychain 写入 → DB 提交 → 失败补偿删除。
use serde::Serialize;
use std::process::Command;

use crate::{codes, store_err, SettingsError, SettingsResult};
use sg_store::{ids, timefmt, Store};

pub const KEYCHAIN_SERVICE: &str = "com.sixgates.desktop";

pub trait CredentialStore: Send + Sync {
    /// 写入并返回 account 名（= ref id）。
    fn put(&self, account: &str, secret: &str) -> Result<(), String>;
    fn get(&self, account: &str) -> Result<String, String>;
    fn delete(&self, account: &str) -> Result<(), String>;
    fn backend_name(&self) -> &'static str;
}

/// macOS Keychain（/usr/bin/security）；仅存取，永不打印。
pub struct MacKeychain;

impl CredentialStore for MacKeychain {
    fn put(&self, account: &str, secret: &str) -> Result<(), String> {
        use std::io::Write;
        use std::process::Stdio;
        if secret.contains('\n') {
            return Err("secret 不能包含换行（stdin 传递按行分帧）".into());
        }
        // 密码经 stdin 传入（缺陷审计 P1-10）：argv 形态任意本地进程 ps 可见。
        // `-w` 置于末位时 security 从 stdin 读两行（password + retype），不进 argv。
        let mut child = Command::new("security")
            .args([
                "add-generic-password",
                "-s",
                KEYCHAIN_SERVICE,
                "-a",
                account,
                "-U",
                "-w",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        {
            let mut stdin = child.stdin.take().ok_or("keychain stdin 不可用")?;
            stdin
                .write_all(secret.as_bytes())
                .and_then(|_| stdin.write_all(b"\n"))
                .and_then(|_| stdin.write_all(secret.as_bytes()))
                .and_then(|_| stdin.write_all(b"\n"))
                .map_err(|e| e.to_string())?;
        }
        let output = child.wait_with_output().map_err(|e| e.to_string())?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }

    fn get(&self, account: &str) -> Result<String, String> {
        let output = Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                KEYCHAIN_SERVICE,
                "-a",
                account,
                "-w",
            ])
            .output()
            .map_err(|e| e.to_string())?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        let output = Command::new("security")
            .args([
                "delete-generic-password",
                "-s",
                KEYCHAIN_SERVICE,
                "-a",
                account,
            ])
            .output()
            .map_err(|e| e.to_string())?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }

    fn backend_name(&self) -> &'static str {
        "mac_keychain"
    }
}

/// 测试内存实现（显式测试配置专用）。
#[derive(Default)]
pub struct InMemoryCredentials {
    map: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

impl CredentialStore for InMemoryCredentials {
    fn put(&self, account: &str, secret: &str) -> Result<(), String> {
        self.map
            .lock()
            .unwrap()
            .insert(account.into(), secret.into());
        Ok(())
    }
    fn get(&self, account: &str) -> Result<String, String> {
        self.map
            .lock()
            .unwrap()
            .get(account)
            .cloned()
            .ok_or_else(|| "not found".into())
    }
    fn delete(&self, account: &str) -> Result<(), String> {
        self.map.lock().unwrap().remove(account);
        Ok(())
    }
    fn backend_name(&self) -> &'static str {
        "in_memory"
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CredentialRef {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub provider: String,
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_verified_at: Option<String>,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// keychain 可用性探测（不可用时 create/replace 失败关闭，不落库）。
pub fn keychain_available(backend: &dyn CredentialStore) -> bool {
    let probe = format!("sg_probe_{}", ids::new_id("t"));
    match backend.put(&probe, "probe") {
        Ok(()) => {
            let _ = backend.delete(&probe);
            true
        }
        Err(_) => false,
    }
}

/// create：prepare → keychain write → DB commit；keychain 失败则补偿删除并报错。
/// secret 在函数返回前离开作用域（不缓存、不日志、不返回）。
pub fn create(
    store: &Store,
    backend: &dyn CredentialStore,
    name: &str,
    kind: &str,
    provider: &str,
    secret: &str,
    project_id: Option<&str>,
) -> SettingsResult<CredentialRef> {
    if !matches!(
        kind,
        "gitlab_token" | "model_api_key" | "ssh_key" | "generic_secret"
    ) {
        return Err(
            SettingsError::new("INVALID_PARAMS", format!("未知凭据类型 {kind}"))
                .with_fields(serde_json::json!({"kind": "类型不合法"})),
        );
    }
    if !keychain_available(backend) {
        return Err(SettingsError::new(
            codes::CREDENTIAL_STORE_UNAVAILABLE,
            format!("Keychain 后端 {} 不可用", backend.backend_name()),
        ));
    }
    let id = ids::new_id("cr");
    backend.put(&id, secret).map_err(|e| {
        SettingsError::new(codes::CREDENTIAL_STORE_UNAVAILABLE, "写入 Keychain 失败")
            .with_details(serde_json::json!({"backend": backend.backend_name(), "reason": e}))
    })?;

    let now = timefmt::now();
    let inserted = store.with_conn(|conn| {
        let changed = conn.execute(
            "INSERT INTO credential_refs(id, name, kind, provider, keychain_service, keychain_account, scope, project_id, status, revision, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?1,'global',?6,'active',1,?7,?7)",
            rusqlite::params![id, name, kind, provider, KEYCHAIN_SERVICE, project_id.unwrap_or(""), now],
        )?;
        Ok(changed)
    }).map_err(store_err)?;
    if inserted == 0 {
        // 补偿：DB 失败删除 keychain 条目。
        let _ = backend.delete(&id);
        return Err(SettingsError::new(
            "INTERNAL",
            "凭据登记失败（已回滚 Keychain）",
        ));
    }
    get(store, &id)
}

pub fn get(store: &Store, id: &str) -> SettingsResult<CredentialRef> {
    let row: Option<CredentialRef> = store.with_conn(|conn| {
        Ok(conn.query_row(
            "SELECT id, name, kind, provider, scope, project_id, COALESCE(expires_at,''), status, COALESCE(last_verified_at,''), revision, created_at, updated_at
             FROM credential_refs WHERE id=?1",
            [id],
            |r| {
                let project: String = r.get(5)?;
                let expires: String = r.get(6)?;
                let verified: String = r.get(8)?;
                Ok(CredentialRef {
                    id: r.get(0)?, name: r.get(1)?, kind: r.get(2)?, provider: r.get(3)?,
                    scope: r.get(4)?,
                    project_id: if project.is_empty() { None } else { Some(project) },
                    expires_at: if expires.is_empty() { None } else { Some(expires) },
                    status: r.get(7)?,
                    last_verified_at: if verified.is_empty() { None } else { Some(verified) },
                    revision: r.get(9)?, created_at: r.get(10)?, updated_at: r.get(11)?,
                })
            },
        ).ok())
    }).map_err(store_err)?;
    row.ok_or_else(|| SettingsError::new("NOT_FOUND", format!("凭据 {id} 不存在")))
}

pub fn list(store: &Store) -> SettingsResult<Vec<CredentialRef>> {
    store
        .with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT id FROM credential_refs ORDER BY created_at")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .map_err(store_err)?
        .into_iter()
        .map(|id| get(store, &id))
        .collect()
}

/// replace：新 keychain 条目成功后才删除旧值并 revision+1；secret 不回显。
pub fn replace(
    store: &Store,
    backend: &dyn CredentialStore,
    id: &str,
    secret: &str,
    expected_revision: i64,
) -> SettingsResult<CredentialRef> {
    let current = get(store, id)?;
    if current.revision != expected_revision {
        return Err(SettingsError::new(
            codes::REVISION_CONFLICT,
            format!(
                "期望 revision {} 实际 {}",
                expected_revision, current.revision
            ),
        ));
    }
    backend.put(id, secret).map_err(|e| {
        SettingsError::new(codes::CREDENTIAL_STORE_UNAVAILABLE, "覆盖 Keychain 失败")
            .with_details(serde_json::json!({"reason": e}))
    })?;
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE credential_refs SET revision=revision+1, status='active', last_verified_at=NULL, updated_at=?1 WHERE id=?2",
            rusqlite::params![timefmt::now(), id],
        )?;
        Ok(())
    }).map_err(store_err)?;
    get(store, id)
}

/// 依赖检查：被 Profile 引用时拒绝删除；force=true 时删除并将引用方标记 credential_missing。
pub fn remove(
    store: &Store,
    backend: &dyn CredentialStore,
    id: &str,
    expected_revision: i64,
    force: bool,
) -> SettingsResult<()> {
    let current = get(store, id)?;
    if current.revision != expected_revision {
        return Err(SettingsError::new(
            codes::REVISION_CONFLICT,
            format!(
                "期望 revision {} 实际 {}",
                expected_revision, current.revision
            ),
        ));
    }
    let dependents: i64 = store
        .with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT (SELECT COUNT(*) FROM model_profiles WHERE credential_ref_id=?1)
                  + (SELECT COUNT(*) FROM gitlab_profiles WHERE credential_ref_id=?1)
                  + (SELECT COUNT(*) FROM ssh_targets WHERE credential_ref_id=?1)",
                [id],
                |r| r.get(0),
            )?)
        })
        .map_err(store_err)?;
    if dependents > 0 && !force {
        return Err(SettingsError::new(
            "CONFLICT",
            format!(
                "凭据被 {} 个 Profile 引用；需 force 或先解除引用",
                dependents
            ),
        )
        .with_details(serde_json::json!({"dependentCount": dependents})));
    }
    // 引用方进入 credential_missing（置 NULL 解除外键；不自动回退其他秘密）。
    store.with_conn(|conn| {
        conn.execute("UPDATE model_profiles SET status='degraded', credential_ref_id=NULL WHERE credential_ref_id=?1", [id])?;
        conn.execute("UPDATE gitlab_profiles SET status='degraded', credential_ref_id=NULL WHERE credential_ref_id=?1", [id])?;
        conn.execute("UPDATE ssh_targets SET status='degraded', credential_ref_id=NULL WHERE credential_ref_id=?1", [id])?;
        conn.execute("DELETE FROM credential_refs WHERE id=?1", [id])?;
        Ok(())
    }).map_err(store_err)?;
    let _ = backend.delete(id);
    Ok(())
}

/// verify：Keychain 可读即通过（真实认证由各 Profile.test 完成）。
pub fn verify(
    store: &Store,
    backend: &dyn CredentialStore,
    id: &str,
) -> SettingsResult<CredentialRef> {
    let _current = get(store, id)?;
    let readable = backend.get(id).is_ok();
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE credential_refs SET status=?1, last_verified_at=?2, revision=revision+1, updated_at=?2 WHERE id=?3",
            rusqlite::params![
                if readable { "active" } else { "missing" },
                timefmt::now(),
                id
            ],
        )?;
        Ok(())
    }).map_err(store_err)?;
    get(store, id)
}

/// 读取秘密（最窄作用域：仅集成适配器构造时调用）。
pub fn reveal(backend: &dyn CredentialStore, id: &str) -> SettingsResult<String> {
    backend.get(id).map_err(|_| {
        SettingsError::new(
            codes::CREDENTIAL_MISSING,
            format!("凭据 {id} 在 Keychain 中缺失（需重新绑定）"),
        )
    })
}
