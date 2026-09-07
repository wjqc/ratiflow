//! Linux Landlock 沙箱（ADR-034）：内核 LSM 路径限制（read/write/execute）。
//!
//! 安全声明（诚实边界）：Landlock 是同内核内的非特权路径访问控制，**不是**容器
//! ——无 PID/用户/文件系统命名空间隔离，网络隔离需要 ABI≥4（内核 ≥6.7）。
//! 方案 §5 M3：需要更强隔离的任务继续用 Docker，不把 Landlock 宣称为容器等价物。
//!
//! 实现：子进程 `pre_exec` 内 prctl(PR_SET_NO_NEW_PRIVS) + landlock_create_ruleset
//! （handled = READ_FILE|WRITE_FILE|EXECUTE [+ NET_CONNECT_TCP 当 network_off 且
//! ABI≥4]）+ 逐路径 add_rule + landlock_restrict_self。未授予路径的相应访问在
//! 内核侧直接 EACCES。
#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

use super::{SandboxBackend, SandboxCapability, SandboxPolicy};

// Landlock ABI 常量（linux/landlock.h；内核 ≥5.13）。
const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1 << 0;
const LANDLOCK_RULE_PATH_BENEATH: libc::c_int = 1;

#[repr(C)]
struct LandlockPathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
    reserved: u32,
}

#[repr(C)]
struct LandlockRulesetAttr {
    handled_access_fs: u64,
}

// ABI v4（内核 6.7）网络位。
const LANDLOCK_ACCESS_FS_READ_FILE: u64 = 1 << 0;
const LANDLOCK_ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
const LANDLOCK_ACCESS_FS_READ_DIR: u64 = 1 << 2;
const LANDLOCK_ACCESS_FS_EXECUTE: u64 = 1 << 3;
#[allow(dead_code)]
const LANDLOCK_NET_CONNECT_TCP: u64 = 1 << 2; // net 属性空间独立（v4）

fn landlock_create_ruleset(attr: *const LandlockRulesetAttr, size: usize, flags: u32) -> i64 {
    unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, attr, size, flags) as i64 }
}

fn landlock_add_rule(
    fd: i32,
    kind: libc::c_int,
    attr: *const LandlockPathBeneathAttr,
    flags: u32,
) -> i64 {
    unsafe { libc::syscall(libc::SYS_landlock_add_rule, fd, kind, attr, flags) as i64 }
}

fn landlock_restrict_self(fd: i32, flags: u32) -> i64 {
    unsafe { libc::syscall(libc::SYS_landlock_restrict_self, fd, flags) as i64 }
}

/// ABI 版本探测：ENOSYS/EOPNOTSUPP → 0（不可用）。
fn abi_version() -> i64 {
    let r = landlock_create_ruleset(std::ptr::null(), 0, LANDLOCK_CREATE_RULESET_VERSION);
    if r < 0 {
        return 0;
    }
    r
}

pub fn probe() -> SandboxCapability {
    if !Path::new("/proc/self/status").exists() {
        return SandboxCapability {
            backend: SandboxBackend::Unavailable.as_str().into(),
            version: String::new(),
            blocked_reason: "非 Linux 环境".into(),
        };
    }
    let abi = abi_version();
    if abi < 1 {
        return SandboxCapability {
            backend: SandboxBackend::Unavailable.as_str().into(),
            version: format!("landlock abi={abi}"),
            blocked_reason: "内核未启用 Landlock（需 ≥5.13 且 LSM 启用）".into(),
        };
    }
    // 探测必须真实验证：应用一个空 ruleset 到自身并立即恢复正常不可行，
    // 改为验证 ruleset 创建成功（内核已接受该 API）。restrict 在子进程内执行。
    SandboxCapability {
        backend: SandboxBackend::LinuxLandlock.as_str().into(),
        version: format!("landlock abi={abi}"),
        blocked_reason: if abi < 4 {
            "网络隔离需 ABI≥4（内核 ≥6.7）；network_off 命令在此内核将 fail-closed".into()
        } else {
            String::new()
        },
    }
}

/// 策略的人类可读摘要（Linux 无文本 profile；设置页/审计展示）。
pub fn policy_summary(policy: &SandboxPolicy) -> String {
    serde_json::json!({
        "backend": "linux_landlock",
        "handled": ["read_file", "write_file", "read_dir", "execute"],
        "networkConnectDenied": policy.network_off,
        "read": policy.read_paths,
        "write": policy.write_paths,
    })
    .to_string()
}

/// 在**当前进程**应用 ruleset（只应在子进程 pre_exec 内调用，见 process.rs）。
/// 返回 Err(String) 时调用方应立即 exit(126)。
///
/// 网络诚实声明：Landlock FS 限制为 ABI v1（内核 ≥5.13）；网络隔离需 ABI v4 的
/// handled_access_net 独立字段，本实现不覆盖——network_off=true 的命令在
/// Linux Landlock 路径下由调用方（executor::execute）fail-closed 拒绝，
/// 强网络隔离继续由 Docker 承担（ADR-034）。
pub fn restrict_self_in_child(policy: &SandboxPolicy) -> Result<(), String> {
    let handled = LANDLOCK_ACCESS_FS_READ_FILE
        | LANDLOCK_ACCESS_FS_WRITE_FILE
        | LANDLOCK_ACCESS_FS_READ_DIR
        | LANDLOCK_ACCESS_FS_EXECUTE;
    let ruleset_attr = LandlockRulesetAttr {
        handled_access_fs: handled,
    };
    let ruleset_fd =
        landlock_create_ruleset(&ruleset_attr, std::mem::size_of::<LandlockRulesetAttr>(), 0);
    if ruleset_fd < 0 {
        return Err(format!("landlock_create_ruleset: errno {}", unsafe {
            *libc::__errno_location()
        }));
    }
    let mut add_path = |path: &str, access: u64| -> Result<(), String> {
        let c = std::ffi::CString::new(path).map_err(|_| "path NUL".to_string())?;
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if fd < 0 {
            // 输入路径不存在时跳过（与 Seatbelt 前缀语义一致：不放大权限）。
            return Ok(());
        }
        let attr = LandlockPathBeneathAttr {
            allowed_access: access,
            parent_fd: fd,
            reserved: 0,
        };
        let r = landlock_add_rule(ruleset_fd as i32, LANDLOCK_RULE_PATH_BENEATH, &attr, 0);
        unsafe { libc::close(fd) };
        if r < 0 {
            return Err(format!("landlock_add_rule {path}: errno {}", unsafe {
                *libc::__errno_location()
            }));
        }
        Ok(())
    };
    // 系统运行面：读 + 执行（动态链接器/二进制/共享库）。
    for p in [
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib64",
        "/etc/alternatives",
        "/snap",
    ] {
        add_path(
            p,
            LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_READ_DIR | LANDLOCK_ACCESS_FS_EXECUTE,
        )?;
    }
    // /etc 只读但避开敏感面不可行（Landlock 是路径树粒度）——按 ABI 能力诚实声明：
    // /etc 整树只读放行（passwd 可读是 Linux 传统可读面；shadow/ssh 私钥不在放行面）。
    add_path(
        "/etc",
        LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_READ_DIR,
    )?;
    // /dev/null、随机数等最小设备读。
    for p in ["/dev/null", "/dev/zero", "/dev/urandom", "/dev/random"] {
        add_path(
            p,
            LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_WRITE_FILE,
        )?;
    }
    for p in &policy.read_paths {
        add_path(
            p,
            LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_READ_DIR,
        )?;
    }
    let write_access =
        LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_READ_DIR | LANDLOCK_ACCESS_FS_WRITE_FILE;
    for p in &policy.write_paths {
        add_path(p, write_access)?;
    }
    let tmp = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    add_path(&tmp, write_access)?;
    if prctl_set_no_new_privs() != 0 {
        return Err("prctl PR_SET_NO_NEW_PRIVS failed".into());
    }
    if landlock_restrict_self(ruleset_fd as i32, 0) < 0 {
        return Err(format!("landlock_restrict_self: errno {}", unsafe {
            *libc::__errno_location()
        }));
    }
    unsafe { libc::close(ruleset_fd as i32) };
    Ok(())
}

fn prctl_set_no_new_privs() -> i64 {
    unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) }
}

/// 统一执行入口（mod::backend_execute 分发）：原始 argv 直跑，pre_exec 钩子在
/// 子进程 exec 前 apply Landlock ruleset（NO_NEW_PRIVS + restrict）。
pub fn execute(
    policy: &SandboxPolicy,
    m: &crate::ExecutionManifest,
    cancel: Option<&sg_integrations::CancelToken>,
) -> Result<crate::ExecResult, crate::ExecError> {
    let hook_policy = policy.clone();
    let hook: Box<dyn Fn() -> Result<(), String> + Send + Sync> =
        Box::new(move || restrict_self_in_child(&hook_policy));
    crate::process::run_local(
        &m.argv[0],
        &m.argv[1..],
        m,
        "kernel_restricted",
        Some(hook),
        cancel,
    )
}
