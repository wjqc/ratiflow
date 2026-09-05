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

/// sandbox-exec 的固定路径（macOS 系统组件，不依赖 PATH——避免 PATH 劫持）。
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// 探测：sandbox-exec 存在且能执行一次最小 profile 的真实命令。
pub fn probe() -> SandboxCapability {
    if !Path::new(SANDBOX_EXEC).exists() {
        return SandboxCapability {
            backend: SandboxBackend::Unavailable.as_str().into(),
            version: String::new(),
            blocked_reason: "sandbox-exec 不存在（非标准 macOS 环境）".into(),
        };
    }
    let output = Command::new(SANDBOX_EXEC)
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

/// Seatbelt profile 字面量转义（profile 语法是 Scheme 风格字符串）。
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn subpath(p: &str) -> String {
    format!("(subpath {})", quote(p))
}

/// Seatbelt 的 (subpath ...) 同时按字符串前缀与解析后 vnode 前缀匹配。
/// macOS 上 /tmp、/var、/etc 是 /private/* 的符号链接：为同一目录放行
/// 字符串与解析两种形式，避免 PATH 形态差异造成误拒（实测行为）。
fn path_variants(p: &str) -> Vec<String> {
    let mut out = vec![p.to_string()];
    for (alias, real) in [
        ("/tmp", "/private/tmp"),
        ("/var", "/private/var"),
        ("/etc", "/private/etc"),
    ] {
        if p == alias {
            out.push(real.to_string());
        } else if let Some(rest) = p.strip_prefix(&format!("{alias}/")) {
            out.push(format!("{real}/{rest}"));
        }
    }
    out
}

fn allow_read(rule: &mut String, p: &str) {
    for v in path_variants(p) {
        rule.push_str(&format!("(allow file-read* {})\n", subpath(&v)));
    }
}

/// 祖先目录链（含 /private 变体）：getcwd 与路径解析需要沿父目录的读权限——
/// 只放行叶子路径会让 shell/dyld 启动即 EPERM。
fn ancestors(p: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = std::path::Path::new(p);
    while let Some(parent) = cur.parent() {
        let s = parent.to_string_lossy().to_string();
        if s.is_empty() || s == "/" {
            break;
        }
        out.push(s);
        cur = parent;
    }
    out
}

fn allow_write(rule: &mut String, p: &str) {
    for v in path_variants(p) {
        rule.push_str(&format!("(allow file-write* {})\n", subpath(&v)));
    }
}

/// 进程级临时目录（DARWIN_USER_TEMP_DIR 优先，env TMPDIR 兜底，再退 /private/tmp）。
fn temp_dir() -> String {
    unsafe {
        let mut buf = vec![0u8; 1024];
        let n = libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buf.as_mut_ptr() as *mut i8,
            buf.len(),
        );
        if n > 0 && n <= buf.len() {
            buf.truncate(n - 1); // 去掉结尾 NUL
            if let Ok(s) = String::from_utf8(buf.clone()) {
                if !s.is_empty() {
                    return s;
                }
            }
        }
    }
    std::env::var("TMPDIR").unwrap_or_else(|_| "/private/tmp/".to_string())
}

/// 生成最小 Seatbelt profile（默认 deny）。该文本的 sha256 即 policy digest 的
/// 语义内容（digest 经 SandboxPolicy::digest 覆盖路径与网络开关）。
pub fn generate_profile(policy: &SandboxPolicy) -> String {
    let mut rules = String::from("(version 1)\n(deny default)\n");
    // 进程与 dyld 基本盘：fork/exec、系统信息读取、Mach 服务查询。
    rules.push_str("(allow process-exec*)\n");
    rules.push_str("(allow process-fork)\n");
    rules.push_str("(allow process-info*)\n");
    rules.push_str("(allow sysctl-read)\n");
    rules.push_str("(allow mach-lookup)\n");
    rules.push_str("(allow iokit-open)\n");
    // 关键经验：拒绝对根目录 "/" 的读取会使 dyld 启动即 abort（exit 134）——
    // 必须 (allow file-read* (literal "/"))。
    rules.push_str("(allow file-read* (literal \"/\"))\n");
    // 读：系统运行库/二进制/dyld 共享缓存等（执行系统命令的最小读面）。
    for p in [
        "/usr/lib",
        "/usr/libexec",
        "/usr/share",
        "/System",
        "/Library",
        "/private/var/db/dyld",
        "/private/var/db/timezone",
        "/private/var/select",
        "/usr/bin",
        "/bin",
        "/sbin",
        "/usr/sbin",
        "/usr/local/bin",
        "/opt/homebrew",
        "/dev",
    ] {
        allow_read(&mut rules, p);
    }
    // /etc 只放行 DNS/时区/开发目录等字面文件；/etc/passwd 等敏感面不在放行列表。
    for f in [
        "/private/etc/resolv.conf",
        "/private/etc/hosts",
        "/private/etc/services",
        "/private/etc/protocols",
        "/private/etc/localtime",
        "/private/etc/paths",
        "/private/etc/ssl/openssl.cnf",
    ] {
        rules.push_str(&format!("(allow file-read* (literal {}))\n", quote(f)));
    }
    // 读：明确输入路径（受管 worktree / work_dir）+ 祖先链。
    for p in &policy.read_paths {
        allow_read(&mut rules, p);
    }
    for p in &policy.read_paths {
        for a in ancestors(p) {
            allow_read(&mut rules, &a);
        }
    }
    for p in &policy.write_paths {
        for a in ancestors(p) {
            allow_read(&mut rules, &a);
        }
    }
    // 写：仅受管 worktree、工件目录、进程临时目录与 /dev/null。
    rules.push_str("(allow file-write* (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/tty\"))\n");
    allow_write(&mut rules, &temp_dir());
    for p in &policy.write_paths {
        allow_write(&mut rules, p);
    }
    // 网络：默认禁；显式关闭时给明确 deny（错误表现为 operation not permitted）。
    if policy.network_off {
        rules.push_str("(deny network*)\n");
    } else {
        rules.push_str("(allow network*)\n");
    }
    rules
}

/// 受 sandbox-exec 包裹后的真实参数：`-p <profile> <argv...>`。
/// 注意 sandbox-exec 无 `--` 终结符：调用方保证 argv[0] 不以 '-' 开头
/// （validate 已拒绝）。
pub fn wrap_args(argv: &[String], profile: &str) -> Vec<String> {
    let mut out = vec!["-p".to_string(), profile.to_string()];
    out.extend(argv.iter().cloned());
    out
}

/// 统一执行入口（mod::backend_execute 分发）：profile 生成 → sandbox-exec 包裹 →
/// 进程组管理的受限执行。策略 digest 由调用方（lib.rs）附加。
pub fn execute(
    policy: &SandboxPolicy,
    m: &crate::ExecutionManifest,
) -> Result<crate::ExecResult, crate::ExecError> {
    let profile = generate_profile(policy);
    let args = wrap_args(&m.argv, &profile);
    crate::process::run_local(SANDBOX_EXEC, &args, m, "kernel_restricted", None)
}
