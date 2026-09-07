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
