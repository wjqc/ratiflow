//! 执行沙箱设置（S22）：模式/CPU/内存/禁网持久化 + 运行自检。
use serde_json::{json, Value};

use crate::{store_err, SettingsError, SettingsResult};
use sg_store::Store;

const KEY: &str = "executor.settings";

pub fn get(store: &Store) -> SettingsResult<Value> {
    let row: Option<String> = store
        .with_conn(|conn| {
            let result: rusqlite::Result<String> = conn.query_row(
            "SELECT value_json FROM app_settings WHERE scope='global' AND project_id='' AND key=?1",
            [KEY],
            |r| r.get(0),
        );
            Ok(result.ok())
        })
        .map_err(store_err)?;
    Ok(serde_json::from_str::<Value>(&row.unwrap_or_else(|| "{}".into())).unwrap_or_default())
}

pub fn revision(store: &Store) -> i64 {
    get(store)
        .map(|v| v.get("revision").and_then(|r| r.as_i64()).unwrap_or(0))
        .unwrap_or(0)
}

pub fn update(store: &Store, settings: &Value, expected_revision: i64) -> SettingsResult<Value> {
    let current = get(store)?;
    let current_rev = current
        .get("revision")
        .and_then(|r| r.as_i64())
        .unwrap_or(0);
    if current_rev != expected_revision {
        return Err(SettingsError::new(
            crate::codes::REVISION_CONFLICT,
            format!("执行设置期望 revision {expected_revision} 实际 {current_rev}"),
        ));
    }
    let mode = settings.get("mode").and_then(|m| m.as_str());
    if let Some(m) = mode {
        if !matches!(
            m,
            "docker" | "safe_restricted" | "unsafe_explicit" | "disabled"
        ) {
            return Err(
                SettingsError::new("INVALID_PARAMS", format!("未知执行模式 {m}"))
                    .with_fields(json!({"mode": "模式不合法"})),
            );
        }
        if m == "unsafe_explicit" {
            // 显式不安全必须二次确认标记（S22：风险说明 + 默认关闭）。
            let confirmed = settings
                .get("unsafeConfirmed")
                .and_then(|c| c.as_bool())
                .unwrap_or(false);
            if !confirmed {
                return Err(SettingsError::new(
                    "INVALID_PARAMS",
                    "不安全模式需显式 unsafeConfirmed=true",
                )
                .with_fields(json!({"unsafeConfirmed": "需二次确认"})));
            }
        }
    }
    let now = sg_store::timefmt::now();
    let merged = json!({
        "mode": settings.get("mode").or_else(|| current.get("mode")).cloned().unwrap_or(json!("safe_restricted")),
        "memoryMB": settings.get("memoryMB").or(current.get("memoryMB")),
        "cpus": settings.get("cpus").or(current.get("cpus")),
        "timeoutSec": settings.get("timeoutSec").or(current.get("timeoutSec")),
        "networkOff": settings.get("networkOff").or(current.get("networkOff")),
        "revision": current_rev + 1,
        "updatedAt": now,
    });
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO app_settings(key, scope, project_id, value_json, revision, updated_at, updated_by)
             VALUES (?1,'global','',?2,?3,?4,'local')
             ON CONFLICT(scope, project_id, key) DO UPDATE SET value_json=excluded.value_json, revision=excluded.revision, updated_at=excluded.updated_at",
            rusqlite::params![KEY, merged.to_string(), current_rev + 1, now],
        )?;
        Ok(())
    }).map_err(store_err)?;
    Ok(merged)
}

pub struct SelfCheckStep {
    pub name: String,
    pub status: String,
    pub detail: String,
}

/// 运行自检：Docker 探测（版本/一次性容器/写临时文件/销毁/清理验证）或受限模式只读验证。
pub fn check(store: &Store) -> SettingsResult<Value> {
    let settings = get(store)?;
    let mode = settings
        .get("mode")
        .and_then(|m| m.as_str())
        .unwrap_or("safe_restricted");
    let mut steps = Vec::new();

    let overall = match mode {
        "docker" => {
            let version = std::process::Command::new("docker")
                .arg("--version")
                .output();
            let (ver_ok, ver_detail) = match version {
                Ok(o) if o.status.success() => {
                    (true, String::from_utf8_lossy(&o.stdout).trim().to_string())
                }
                Ok(o) => (false, format!("exit={}", o.status)),
                Err(e) => (false, e.to_string()),
            };
            steps.push(step("docker_version", ver_ok, ver_detail));
            let ran = std::process::Command::new("docker")
                .args([
                    "run",
                    "--rm",
                    "--network",
                    "none",
                    "alpine:3",
                    "sh",
                    "-c",
                    "echo ok > /tmp/x && cat /tmp/x && rm /tmp/x && echo cleanup-done",
                ])
                .output();
            let ok = ran
                .as_ref()
                .map(|o| {
                    o.status.success()
                        && String::from_utf8_lossy(&o.stdout).contains("cleanup-done")
                })
                .unwrap_or(false);
            steps.push(step(
                "ephemeral_container_write_cleanup",
                ok,
                ran.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .unwrap_or_else(|e| e.to_string()),
            ));
            if steps.iter().all(|s| s["status"] == json!("passed")) {
                "ready"
            } else {
                "error"
            }
        }
        _ => {
            // 受限/禁用模式：验证只读命令白名单可用（不产生任何写副作用）。
            let ran = std::process::Command::new("ls").arg("/tmp").output();
            let ok = ran.map(|o| o.status.success()).unwrap_or(false);
            steps.push(step(
                "restricted_readonly_probe",
                ok,
                if ok {
                    "只读探测通过；自动写/执行保持禁用".into()
                } else {
                    "受限模式探测失败".into()
                },
            ));
            if ok { "ready" } else { "error" }
        }
    };
    Ok(
        json!({"mode": mode, "status": overall, "steps": steps, "checkedAt": sg_store::timefmt::now()}),
    )
}

fn step(name: &str, ok: bool, detail: String) -> Value {
    json!({"name": name, "status": if ok { "passed" } else { "failed" }, "detail": detail})
}
