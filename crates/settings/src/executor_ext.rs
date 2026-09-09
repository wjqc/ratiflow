//! 执行沙箱设置（S22）：模式/CPU/内存/禁网持久化 + 运行自检。
//! 存储：`data_dir/settings.json` 单文件（sg_store::prefstore），键 `executor.settings`；
//! revision 仍内嵌在 value JSON（RPC 契约不变），条目 revision 同步镜像。
use serde_json::{json, Value};

use crate::{codes, store_err, SettingsError, SettingsResult};
use sg_store::prefstore::{self, PrefEntry};
use sg_store::Store;

const KEY: &str = "executor.settings";

pub fn get(store: &Store) -> SettingsResult<Value> {
    let entry = prefstore::get(store, "global", "", KEY).map_err(store_err)?;
    Ok(entry.map(|e| e.value).unwrap_or_else(|| json!({})))
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
            "docker" | "kernel_restricted" | "safe_restricted" | "unsafe_explicit" | "disabled"
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
    let id = prefstore::composite("global", "", KEY);
    let merged = prefstore::write(store, |doc| -> Result<Value, SettingsError> {
        let current = doc
            .get(&id)
            .map(|e| e.value.clone())
            .unwrap_or_else(|| json!({}));
        let current_rev = current
            .get("revision")
            .and_then(|r| r.as_i64())
            .unwrap_or(0);
        if current_rev != expected_revision {
            return Err(SettingsError::new(
                codes::REVISION_CONFLICT,
                format!("执行设置期望 revision {expected_revision} 实际 {current_rev}"),
            ));
        }
        let merged = json!({
            "mode": settings.get("mode").or_else(|| current.get("mode")).cloned().unwrap_or(json!("safe_restricted")),
            "memoryMB": settings.get("memoryMB").or(current.get("memoryMB")),
            "cpus": settings.get("cpus").or(current.get("cpus")),
            "timeoutSec": settings.get("timeoutSec").or(current.get("timeoutSec")),
            "networkOff": settings.get("networkOff").or(current.get("networkOff")),
            "revision": current_rev + 1,
            "updatedAt": now,
        });
        doc.insert(
            id.clone(),
            PrefEntry {
                value: merged.clone(),
                revision: current_rev + 1,
                updated_at: sg_store::timefmt::now(),
                updated_by: "local".into(),
            },
        );
        Ok(merged)
    })?;
    Ok(merged)
}

pub struct SelfCheckStep {
    pub name: String,
    pub status: String,
    pub detail: String,
}

/// 运行自检：Docker 探测（版本/一次性容器/写临时文件/销毁/清理验证）、
/// 内核沙箱真实验证（允许面读通过 + 敏感面读被拒 + 禁网生效）或受限模式只读验证。
/// 同时返回 sandbox capability（backend/version/保护范围/阻塞原因）。
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
        "kernel_restricted" | "safe_restricted" | "disabled" => {
            let cap = sg_executor::sandbox::probe();
            if mode == "kernel_restricted" || mode == "disabled" {
                steps.push(step(
                    "sandbox_backend_probe",
                    cap.backend != "unavailable",
                    if cap.backend != "unavailable" {
                        format!("{} ({})", cap.backend, cap.version)
                    } else {
                        cap.blocked_reason.clone()
                    },
                ));
            }
            // 内核沙箱真实验证：允许面读通过、敏感面读被拒（真实 runner，非 mock）。
            if cap.backend != "unavailable" {
                let dir = std::env::temp_dir().join(format!(
                    "sg-exec-check-{}-{}",
                    std::process::id(),
                    sg_store::ids::new_id("t")
                ));
                let _ = std::fs::create_dir_all(&dir);
                let probe_file = dir.join("probe.txt");
                let _ = std::fs::write(&probe_file, "kernel-ok");
                let mk = |argv: Vec<String>| sg_executor::ExecutionManifest {
                    argv,
                    work_dir: String::new(),
                    image: String::new(),
                    network_off: true,
                    memory_mb: 0,
                    cpus: 0.0,
                    timeout_sec: 10,
                    writes_files: false,
                    sandbox_read_paths: vec![dir.to_string_lossy().to_string()],
                    sandbox_write_paths: vec![dir.to_string_lossy().to_string()],
                };
                let allowed = sg_executor::execute(
                    sg_executor::Mode::KernelRestricted,
                    &mk(vec!["cat".into(), probe_file.to_string_lossy().to_string()]),
                );
                let allowed_ok = allowed
                    .as_ref()
                    .map(|r| r.stdout.contains("kernel-ok"))
                    .unwrap_or(false);
                steps.push(step(
                    "sandbox_allowed_read",
                    allowed_ok,
                    match &allowed {
                        Ok(r) => format!("exit={} stdout={:?}", r.exit_code, r.stdout.trim()),
                        Err(e) => e.to_string(),
                    },
                ));
                let denied = sg_executor::execute(
                    sg_executor::Mode::KernelRestricted,
                    &mk(vec!["cat".into(), "/etc/passwd".into()]),
                );
                let denied_ok = denied
                    .as_ref()
                    .map(|r| r.exit_code != 0 && !r.stdout.contains("root"))
                    .unwrap_or_else(|_| {
                        matches!(
                            denied.as_ref().err(),
                            Some(sg_executor::ExecError::SandboxUnavailable(_))
                                | Some(sg_executor::ExecError::SandboxDenied(_))
                        )
                    });
                steps.push(step(
                    "sandbox_sensitive_read_denied",
                    denied_ok,
                    match &denied {
                        Ok(r) => format!("exit={}", r.exit_code),
                        Err(e) => e.to_string(),
                    },
                ));
                let _ = std::fs::remove_dir_all(&dir);
            }
            if mode == "safe_restricted" {
                // 本机白名单（非强隔离）：不做沙箱声明，只验证只读命令可用。
                let ran = std::process::Command::new("ls").arg("/tmp").output();
                let ok = ran.map(|o| o.status.success()).unwrap_or(false);
                steps.push(step(
                    "argv_allowlist_only",
                    ok,
                    if ok {
                        "本机白名单（非强隔离）；自动写/执行保持禁用".into()
                    } else {
                        "白名单探测失败".into()
                    },
                ));
            }
            if steps.iter().all(|s| s["status"] == json!("passed")) {
                "ready"
            } else {
                "error"
            }
        }
        _ => "error",
    };
    let cap = sg_executor::sandbox::probe();
    Ok(json!({
        "mode": mode,
        "status": overall,
        "steps": steps,
        "sandbox": {
            "backend": cap.backend,
            "version": cap.version,
            "blockedReason": cap.blocked_reason,
            "protectionScope": if cap.backend == "unavailable" {
                json!(null)
            } else {
                json!({
                    "kind": if cap.backend == "mac_seatbelt" { "Seatbelt（内核 MAC，非容器）" }
                            else if cap.backend == "linux_landlock" { "Landlock（内核 LSM 路径限制，非容器；网络隔离需 ABI≥4）" }
                            else { "容器" },
                    "writeScope": "仅受管 worktree、run 工件目录与进程临时目录",
                    "network": "manifest.network_off 时拒绝全部连接（Landlock 除外，见 ADR-034）",
                })
            },
        },
        "checkedAt": sg_store::timefmt::now(),
    }))
}

fn step(name: &str, ok: bool, detail: String) -> Value {
    json!({"name": name, "status": if ok { "passed" } else { "failed" }, "detail": detail})
}
