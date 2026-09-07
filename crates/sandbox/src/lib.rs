//! sg-sandbox（RDWS 实施计划 v1.4 WP-3）：内核沙箱执行原语的**叶 crate**。
//!
//! 职责：SandboxPolicy（canonical digest）、macOS Seatbelt profile 生成、
//! ManagedChild（受管长驻子进程）与 spawn_sandboxed。executor（一次性执行）与
//! integrations（MCP stdio 传输）共同消费——这也是本 crate 存在的原因：
//! executor→workitem→workflow→integrations 是承重边，integrations 不能反向依赖
//! executor，沙箱原语必须沉到两者之下的叶子层。
//!
//! 平台矩阵（§1.10）：macOS Seatbelt ✅；Linux Landlock ABI v1 无网络位 →
//! MCP 场景 fail-closed（UP-3a：ABI≥4/内核≥6.7 真机验收后翻转）；其余平台无后端。

use sha2::{Digest, Sha256};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 沙箱 spawn 错误（与 executor::ExecError 的 Sandbox 系变体同词汇）。
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("sandbox_denied: {0}")]
    Denied(String),
    #[error("sandbox_spawn: {0}")]
    Io(String),
}

/// 沙箱策略（manifest + ToolCtx 装配）：
/// - read：系统运行库由 profile 模板内置；输入路径 = 受管 worktree/work_dir；
/// - write：受管 worktree、run 工件目录、必要临时目录；
/// - network_off 按 manifest（默认 true）。
#[derive(Debug, Clone, Default)]
pub struct SandboxPolicy {
    pub read_paths: Vec<String>,
    pub write_paths: Vec<String>,
    pub network_off: bool,
}

impl SandboxPolicy {
    /// 规范 JSON（排序去重 + canonical 序列化）→ sha256。策略任何字节变化都会
    /// 改变 digest，可被执行快照/审计固定与回查。
    pub fn digest(&self) -> String {
        let mut reads = self.read_paths.clone();
        let mut writes = self.write_paths.clone();
        reads.sort();
        reads.dedup();
        writes.sort();
        writes.dedup();
        let canonical = serde_json::json!({
            "read": reads,
            "write": writes,
            "networkOff": self.network_off,
        });
        let bytes = Sha256::digest(canonical.to_string().as_bytes());
        let mut hex = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            {
                hex.push_str(&format!("{:02x}", b));
            }
        }
        format!("sha256:{hex}")
    }
}

// ---------------------------------------------------------------------------
// macOS Seatbelt profile 生成（自 executor/sandbox/macos.rs 迁入，语义不变）。
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
pub mod seatbelt {
    use crate::SandboxPolicy;
    /// sandbox-exec 的固定路径（macOS 系统组件，不依赖 PATH——避免 PATH 劫持）。
    pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

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
}

/// 打开进程组 kill（unix）。
#[cfg(unix)]
fn use_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn use_process_group(_cmd: &mut Command) {}

// ---------------------------------------------------------------------------
// 受管长驻子进程（自 executor/src/process.rs 迁入，语义不变）。
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// WP-3（RDWS v1.4 §2）：受管长驻子进程——MCP stdio 沙箱的执行原语。
// 与 run_local 的一次性「跑完收输出」模型不同：stdin/stdout 受管双向、stderr 后台
// drain 入 64KiB 环形（超限丢弃 + truncated 标记）、进程组整组回收（SIGTERM→grace→
// SIGKILL）、policy snapshot digest 随进程携带。EOF 只在主动 shutdown 后是正常结束。
// ---------------------------------------------------------------------------

/// stderr 环形缓冲上限（超限丢弃新内容并置 truncated）。
const MANAGED_STDERR_RING: usize = 64 * 1024;

pub struct ManagedChild {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: std::io::BufReader<std::process::ChildStdout>,
    stderr_ring: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    stderr_truncated: std::sync::Arc<std::sync::atomic::AtomicBool>,
    policy_digest: String,
}

impl ManagedChild {
    /// 受管 stdin（写入请求；调用方负责 flush 语义——flush 成功即 request_flushed 相位事实）。
    pub fn stdin(&mut self) -> &mut std::process::ChildStdin {
        &mut self.stdin
    }

    /// 受管 stdout（读取响应行；EOF 在 shutdown 后为正常结束，否则为 transport 事件）。
    pub fn stdout(&mut self) -> &mut std::io::BufReader<std::process::ChildStdout> {
        &mut self.stdout
    }

    /// 子进程 pid（受管看门狗打断读阻塞用；整组回收仍走 kill_group）。
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// policy snapshot digest（审计与 send_phase 证据携带）。
    pub fn policy_digest(&self) -> &str {
        &self.policy_digest
    }

    /// stderr 快照（环形保留最近内容 + 是否超限丢弃标记）。
    pub fn stderr_snapshot(&self) -> (String, bool) {
        let ring = self
            .stderr_ring
            .lock()
            .map(|r| r.clone())
            .unwrap_or_default();
        (
            String::from_utf8_lossy(&ring).to_string(),
            self.stderr_truncated
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// 整组回收：SIGTERM 进程组 → grace（默认 5s）→ SIGKILL 进程组 → 回收僵尸。
    pub fn kill_group(&mut self, grace: Duration) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGTERM);
        }
        #[cfg(not(unix))]
        let _ = self.child.kill();
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if self.child.try_wait().is_ok_and(|s| s.is_some()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        #[cfg(unix)]
        unsafe {
            if libc::kill(-(self.child.id() as i32), libc::SIGKILL) != 0 {
                libc::kill(self.child.id() as i32, libc::SIGKILL);
            }
        }
        #[cfg(not(unix))]
        let _ = self.child.kill();
        let _ = self.child.wait(); // 回收僵尸，无进程组残留
    }

    /// 限时等待退出（长驻进程正常 wait 的受管变体）。
    pub fn wait_timeout(
        &mut self,
        d: Duration,
    ) -> Result<Option<std::process::ExitStatus>, SandboxError> {
        let deadline = Instant::now() + d;
        loop {
            match self
                .child
                .try_wait()
                .map_err(|e| SandboxError::Io(e.to_string()))?
            {
                Some(s) => return Ok(Some(s)),
                None if Instant::now() >= deadline => return Ok(None),
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }
}

/// 沙箱化 spawn（WP-3 平台矩阵）：
/// - macOS：Seatbelt profile 包裹（禁网 + FS 只读 + write 白名单进 policy）后 spawn，
///   独立进程组 + 受管三管道；
/// - Linux Landlock：无网络位（ABI v1，`handled_access_net` 需 ABI≥4/内核≥6.7）——
///   MCP policy 固定禁网，提供 spawn 变体即撒谎，fail-closed（UP-3a 真机验收后翻转）；
/// - Docker 后端不提供 spawn 变体；Windows 无后端。
pub fn spawn_sandboxed(
    policy: &crate::SandboxPolicy,
    argv: &[String],
) -> Result<ManagedChild, SandboxError> {
    spawn_sandboxed_in(policy, argv, None)
}

/// 同 spawn_sandboxed，但子进程以 work_dir 为 cwd（导入型 server 的相对入口
/// 必须以 checkout 为工作目录解析；None = 继承当前进程 cwd）。
pub fn spawn_sandboxed_in(
    policy: &crate::SandboxPolicy,
    argv: &[String],
    work_dir: Option<&std::path::Path>,
) -> Result<ManagedChild, SandboxError> {
    if !policy.network_off {
        return Err(SandboxError::Denied(
            "mcp_sandbox_policy: MCP 沙箱策略必须禁网（network_off=false 不可接受）".into(),
        ));
    }
    #[cfg(target_os = "macos")]
    {
        let profile = crate::seatbelt::generate_profile(policy);
        let args = crate::seatbelt::wrap_args(argv, &profile);
        let mut cmd = Command::new(crate::seatbelt::SANDBOX_EXEC);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = work_dir {
            cmd.current_dir(dir);
        }
        use_process_group(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| SandboxError::Io(e.to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| SandboxError::Io("stdin 不可用".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SandboxError::Io("stdout 不可用".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| SandboxError::Io("stderr 不可用".into()))?;
        let ring = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let truncated = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (ring_t, trunc_t) = (ring.clone(), truncated.clone());
        // stderr 后台 drain：64KiB 环形（保留最近），超限丢弃 + truncated 审计标记。
        let _drain = std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = [0u8; 4096];
            let mut reader = stderr;
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut guard = match ring_t.lock() {
                            Ok(g) => g,
                            Err(_) => break,
                        };
                        if guard.len() + n > MANAGED_STDERR_RING {
                            let excess = guard.len() + n - MANAGED_STDERR_RING;
                            let drop = excess.min(guard.len());
                            let _ = guard.drain(..drop);
                            trunc_t.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        guard.extend_from_slice(&buf[..n]);
                    }
                }
            }
        });
        Ok(ManagedChild {
            child,
            stdin,
            stdout: std::io::BufReader::new(stdout),
            stderr_ring: ring,
            stderr_truncated: truncated,
            policy_digest: policy.digest(),
        })
    }
    #[cfg(target_os = "linux")]
    {
        let _ = (policy, argv, work_dir);
        Err(SandboxError::Denied(
            "mcp_platform_unsupported: Linux Landlock ABI v1 无网络隔离位（需 ABI≥4/内核≥6.7，UP-3a），MCP 沙箱暂不支持".into(),
        ))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (policy, argv, work_dir);
        Err(SandboxError::Denied(
            "mcp_platform_unsupported: 本平台无内核沙箱后端".into(),
        ))
    }
}

#[cfg(all(test, target_os = "macos"))]
mod managed_tests {
    use super::*;
    use crate::SandboxPolicy;

    fn policy() -> SandboxPolicy {
        SandboxPolicy {
            read_paths: vec!["/bin".into(), "/usr/bin".into(), "/usr/lib".into()],
            write_paths: vec![],
            network_off: true,
        }
    }

    /// RDWS-005/006 前置：沙箱化 spawn 起进程组、digest 携带、EOF/kill_group 语义。
    #[test]
    fn spawn_sandboxed_runs_and_kills_group() {
        use std::io::BufRead;
        let mut mc = spawn_sandboxed(&policy(), &["/bin/cat".to_string()]).expect("spawn");
        assert!(
            mc.policy_digest().starts_with("sha256:"),
            "policy digest 携带"
        );
        use std::io::Write;
        // 受管 stdin/stdout 双向：echo 一行读回一行。
        mc.stdin().write_all(b"ping\n").unwrap();
        mc.stdin().flush().unwrap();
        let mut line = String::new();
        mc.stdout().read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "ping", "受管管道双向可用");
        // 整组回收：SIGTERM → cat 退出。
        mc.kill_group(Duration::from_secs(5));
        assert!(
            mc.wait_timeout(Duration::from_secs(2)).unwrap().is_some(),
            "kill_group 后进程组无残留"
        );
    }

    /// 禁网策略是硬前提：network_off=false 直接拒绝（不静默降级为禁网失败）。
    #[test]
    fn spawn_rejects_network_allowed_policy() {
        let bad = SandboxPolicy {
            network_off: false,
            ..policy()
        };
        let err = match spawn_sandboxed(&bad, &["/bin/cat".to_string()]) {
            Err(e) => e,
            Ok(mut mc) => {
                mc.kill_group(Duration::from_secs(1));
                panic!("network_off=false 必须拒绝");
            }
        };
        assert!(err.to_string().contains("mcp_sandbox_policy"), "{err}");
    }

    /// stderr 环形：超限丢弃 + truncated 标记。
    #[test]
    fn stderr_ring_truncates() {
        // yes 持续输出 stderr 不可行（yes 走 stdout）——用 sh 循环写 stderr。
        let mut mc = spawn_sandboxed(
            &policy(),
            &["/bin/sh".to_string(), "-c".to_string(),
             "i=0; while [ $i -lt 2000 ]; do echo \"stderr-line-$i-0123456789012345678901234567890123456789\" >&2; i=$((i+1)); done; sleep 30".to_string()],
        )
        .expect("spawn");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let (snap, truncated) = mc.stderr_snapshot();
            if truncated || Instant::now() > deadline {
                assert!(
                    truncated,
                    "64KiB 环形应置 truncated（snap {}B）",
                    snap.len()
                );
                assert!(snap.len() <= 64 * 1024 + 4096, "环形不超过上限量级");
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        mc.kill_group(Duration::from_secs(5));
    }
}
