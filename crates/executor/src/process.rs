//! 受约束进程执行（M3 自 lib.rs 拆出）：真实超时 kill、管道读线程、进程组管理。
//!
//! 沙箱包裹（sandbox-exec）会引入中间进程：直接 kill 中间进程会遗留真实工作
//! 进程。子进程以独立进程组启动（pre_exec setpgid），超时对**整组** SIGKILL，
//! 满足"超时后无遗留子进程/容器"。
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::{ExecError, ExecResult, ExecutionManifest};

/// 打开进程组 kill（unix）。
#[cfg(unix)]
fn use_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn use_process_group(_cmd: &mut Command) {}

/// kill 整个进程组（负 pid）；失败退化为单进程 kill。
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    unsafe {
        if libc::kill(-(pid as i32), libc::SIGKILL) != 0 {
            libc::kill(pid as i32, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_process_group(pid: u32) {
    let _ = pid;
}

/// 受限进程执行：真实超时 kill（非事后标记）；读管道走独立线程，避免子进程
/// 填满管道缓冲死锁。pre_exec 钩子在子进程 exec 前运行（Landlock restrict 用）。
pub fn run_local(
    bin: &str,
    args: &[String],
    m: &ExecutionManifest,
    mode: &str,
    pre_exec: Option<Box<dyn Fn() -> Result<(), String> + Send + Sync>>,
    cancel: Option<&sg_integrations::CancelToken>,
) -> Result<ExecResult, ExecError> {
    let start = Instant::now();
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    use_process_group(&mut cmd);
    if !m.work_dir.is_empty() {
        let dir = std::path::Path::new(&m.work_dir);
        if dir.is_dir() {
            cmd.current_dir(dir);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        if let Some(hook) = pre_exec {
            // safety: pre_exec 仅登记 fork 后 exec 前的钩子；钩子内容见 Landlock 模块。
            unsafe {
                cmd.pre_exec(move || {
                    hook().map_err(|e| {
                        eprintln!(
                            "{{\"level\":\"warn\",\"msg\":\"sandbox pre_exec failed: {e}\"}}"
                        );
                        std::io::Error::other(e)
                    })
                });
            }
        }
    }
    #[cfg(not(unix))]
    drop(pre_exec);

    let mut child = cmd.spawn().map_err(|e| ExecError::Io(e.to_string()))?;
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let t_out = std::thread::spawn(move || read_all(stdout_pipe));
    let t_err = std::thread::spawn(move || read_all(stderr_pipe));

    let deadline = Instant::now() + Duration::from_secs(m.timeout_sec.max(1) as u64);
    let mut timed_out = false;
    let mut cancelled = false;
    let mut status = None;
    loop {
        match child.try_wait().map_err(|e| ExecError::Io(e.to_string()))? {
            Some(s) => {
                status = Some(s);
                break;
            }
            None => {
                // 取消面（缺陷审计 P1-9）：取消请求置位即杀整组，副作用不再继续发生。
                if cancel.is_some_and(|c| c.is_cancelled()) {
                    kill_process_group(child.id());
                    let _ = child.kill();
                    cancelled = true;
                    break;
                }
                if Instant::now() >= deadline {
                    // 先杀整组（含沙箱中间进程与真实工作进程），再回收僵尸。
                    kill_process_group(child.id());
                    let _ = child.kill();
                    timed_out = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    if timed_out || cancelled {
        let _ = child.wait(); // 回收僵尸进程
    }
    let stdout = String::from_utf8_lossy(&t_out.join().unwrap_or_default()).to_string();
    let stderr = String::from_utf8_lossy(&t_err.join().unwrap_or_default()).to_string();
    Ok(ExecResult {
        mode: mode.into(),
        exit_code: status.and_then(|s| s.code()).unwrap_or(-1),
        stdout,
        stderr,
        duration_ms: start.elapsed().as_millis() as u64,
        cancelled,
        timed_out,
        sandbox_policy_digest: String::new(),
    })
}

fn read_all(mut pipe: Option<impl std::io::Read>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(r) = pipe.as_mut() {
        let _ = std::io::Read::read_to_end(r, &mut buf);
    }
    buf
}

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

    /// policy snapshot digest（审计与 send_phase 证据携带）。
    pub fn policy_digest(&self) -> &str {
        &self.policy_digest
    }

    /// stderr 快照（环形保留最近内容 + 是否超限丢弃标记）。
    pub fn stderr_snapshot(&self) -> (String, bool) {
        let ring = self.stderr_ring.lock().map(|r| r.clone()).unwrap_or_default();
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
    pub fn wait_timeout(&mut self, d: Duration) -> Result<Option<std::process::ExitStatus>, ExecError> {
        let deadline = Instant::now() + d;
        loop {
            match self.child.try_wait().map_err(|e| ExecError::Io(e.to_string()))? {
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
    policy: &crate::sandbox::SandboxPolicy,
    argv: &[String],
) -> Result<ManagedChild, ExecError> {
    if !policy.network_off {
        return Err(ExecError::SandboxDenied(
            "mcp_sandbox_policy: MCP 沙箱策略必须禁网（network_off=false 不可接受）".into(),
        ));
    }
    #[cfg(target_os = "macos")]
    {
        let profile = crate::sandbox::macos::generate_profile(policy);
        let args = crate::sandbox::macos::wrap_args(argv, &profile);
        let mut cmd = Command::new(crate::sandbox::macos::SANDBOX_EXEC);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        use_process_group(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| ExecError::Io(e.to_string()))?;
        let stdin = child.stdin.take().ok_or_else(|| ExecError::Io("stdin 不可用".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| ExecError::Io("stdout 不可用".into()))?;
        let stderr = child.stderr.take().ok_or_else(|| ExecError::Io("stderr 不可用".into()))?;
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
        let _ = (policy, argv);
        Err(ExecError::SandboxDenied(
            "mcp_platform_unsupported: Linux Landlock ABI v1 无网络隔离位（需 ABI≥4/内核≥6.7，UP-3a），MCP 沙箱暂不支持".into(),
        ))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (policy, argv);
        Err(ExecError::SandboxDenied(
            "mcp_platform_unsupported: 本平台无内核沙箱后端".into(),
        ))
    }
}

#[cfg(all(test, target_os = "macos"))]
mod managed_tests {
    use super::*;
    use crate::sandbox::SandboxPolicy;

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
        assert!(mc.policy_digest().starts_with("sha256:"), "policy digest 携带");
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
        let bad = SandboxPolicy { network_off: false, ..policy() };
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
                assert!(truncated, "64KiB 环形应置 truncated（snap {}B）", snap.len());
                assert!(snap.len() <= 64 * 1024 + 4096, "环形不超过上限量级");
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        mc.kill_group(Duration::from_secs(5));
    }
}
