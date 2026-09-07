//! macOS Seatbelt 沙箱（ADR-034）：sandbox-exec + 默认 deny 的最小 profile。
//!
//! 安全声明（诚实边界）：Seatbelt 由内核 TrustedBSD MAC 强制，约束文件读/写、
//! 网络与进程行为，但**不是**容器——共享宿主内核与文件系统命名空间，无 PID/用户
//! 隔离。强隔离需求仍走 Docker。
//!
//! profile 设计（方案 §5 M3）：
//! - `(deny default)` 起手，仅显式放行；
//! - 读：系统运行库/动态链接器/二进制目录（内置模板）+ 明确输入路径；
//! - 写：受管 worktree、run 工件目录、进程临时目录（TMPDIR）、/dev/null；
//! - 网络：network_off=true → `(deny network*)`（连接 loopback/公网均失败）；
//! - `/etc` 只放行 DNS/时区等字面文件，`/etc/passwd` 一类敏感面不在放行面。

use std::path::Path;
use std::process::Command;

use super::{SandboxBackend, SandboxCapability, SandboxPolicy};
use sg_sandbox::seatbelt::{generate_profile, wrap_args};

/// 探测：sandbox-exec 存在且能执行一次最小 profile 的真实命令。
pub fn probe() -> SandboxCapability {
    if !Path::new(sg_sandbox::seatbelt::SANDBOX_EXEC).exists() {
        return SandboxCapability {
            backend: SandboxBackend::Unavailable.as_str().into(),
            version: String::new(),
            blocked_reason: "sandbox-exec 不存在（非标准 macOS 环境）".into(),
        };
    }
    let output = Command::new(sg_sandbox::seatbelt::SANDBOX_EXEC)
        .arg("-p")
        .arg("(version 1)(allow default)")
        .arg("/usr/bin/true")
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let kernel = std::process::Command::new("/usr/bin/uname")
                .arg("-r")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            SandboxCapability {
                backend: SandboxBackend::MacSeatbelt.as_str().into(),
                version: format!("seatbelt (darwin {})", kernel),
                blocked_reason: String::new(),
            }
        }
        Ok(o) => SandboxCapability {
            backend: SandboxBackend::Unavailable.as_str().into(),
            version: String::new(),
            blocked_reason: format!(
                "sandbox-exec 探测命令失败 exit={}（{}）",
                o.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&o.stderr)
                    .trim()
                    .chars()
                    .take(120)
                    .collect::<String>()
            ),
        },
        Err(e) => SandboxCapability {
            backend: SandboxBackend::Unavailable.as_str().into(),
            version: String::new(),
            blocked_reason: format!("sandbox-exec 无法执行: {e}"),
        },
    }
}

/// 统一执行入口（mod::backend_execute 分发）：profile 生成 → sandbox-exec 包裹 →
/// 进程组管理的受限执行。策略 digest 由调用方（lib.rs）附加。
pub fn execute(
    policy: &SandboxPolicy,
    m: &crate::ExecutionManifest,
    cancel: Option<&sg_integrations::CancelToken>,
) -> Result<crate::ExecResult, crate::ExecError> {
    let profile = generate_profile(policy);
    let args = wrap_args(&m.argv, &profile);
    crate::process::run_local(
        sg_sandbox::seatbelt::SANDBOX_EXEC,
        &args,
        m,
        "kernel_restricted",
        None,
        cancel,
    )
}
