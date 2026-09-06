//! 受约束执行（v2 ADR-024 行为等价 + ADR-034 M3）：只接受 ExecutionManifest，
//! 拒绝自由 Shell。本文件只保留 orchestration；进程管理在 `process.rs`，
//! 内核沙箱在 `sandbox/`（macOS Seatbelt / Linux Landlock）。
pub mod process;
pub mod sandbox;
pub mod workspace;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Docker,
    /// 内核沙箱受限执行（M3 新写模式）：Seatbelt/Landlock 强制路径与网络边界，
    /// 允许落在受管域内的写操作。
    KernelRestricted,
    /// 旧模式：仅 argv 只读白名单，**无内核强制**。兼容读取保留，新配置不再写入。
    SafeRestricted,
    UnsafeExplicit,
    Disabled,
}

/// 模式判定：不静默降级。优先级 Docker > KernelRestricted > UnsafeExplicit > Disabled。
pub fn detect_mode(docker_available: bool, unsafe_explicit: bool) -> Mode {
    detect_mode_with_kernel(
        docker_available,
        unsafe_explicit,
        sandbox::probe().backend.as_str() != "unavailable",
    )
}

/// 可注入探测结果的形式（设置页自检/测试复用）。
pub fn detect_mode_with_kernel(
    docker_available: bool,
    unsafe_explicit: bool,
    kernel_available: bool,
) -> Mode {
    if docker_available {
        Mode::Docker
    } else if kernel_available {
        Mode::KernelRestricted
    } else if unsafe_explicit {
        Mode::UnsafeExplicit
    } else {
        Mode::Disabled
    }
}

pub fn docker_available() -> bool {
    which_docker().is_some()
}

fn which_docker() -> Option<String> {
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in path.split(':') {
        let candidate = std::path::Path::new(dir).join("docker");
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }
    None
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionManifest {
    pub argv: Vec<String>,
    #[serde(default)]
    pub work_dir: String,
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub network_off: bool,
    #[serde(default)]
    pub memory_mb: i64,
    #[serde(default)]
    pub cpus: f64,
    pub timeout_sec: i64,
    #[serde(default)]
    pub writes_files: bool,
    /// M3：沙箱明确输入路径（受管 worktree/work_dir）；缺省 = 仅系统运行面可读。
    #[serde(default)]
    pub sandbox_read_paths: Vec<String>,
    /// M3：沙箱明确写路径（受管 worktree、run 工件目录）；缺省 = 只读执行。
    #[serde(default)]
    pub sandbox_write_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecResult {
    pub mode: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub timed_out: bool,
    /// M3：kernel_restricted 时的策略 digest（执行快照/审计固定）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sandbox_policy_digest: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("unsafe_execution_disabled: {0}")]
    Disabled(String),
    #[error("manifest_rejected: {0}")]
    Rejected(String),
    #[error("sandbox_unavailable: {0}")]
    SandboxUnavailable(String),
    #[error("sandbox_denied: {0}")]
    SandboxDenied(String),
    #[error("execute: {0}")]
    Io(String),
}

const SAFE_READ_ONLY: [&str; 10] = [
    "ls", "cat", "grep", "find", "wc", "head", "tail", "git", "rg", "stat",
];

pub fn validate(mode: Mode, m: &ExecutionManifest) -> Result<(), ExecError> {
    if m.argv.is_empty() {
        return Err(ExecError::Rejected("argv required".into()));
    }
    for arg in &m.argv {
        if [';', '|', '&', '`', '$', '>', '<', '\n']
            .iter()
            .any(|c| arg.contains(*c))
        {
            return Err(ExecError::Rejected(format!(
                "shell metacharacter in argv {arg:?}"
            )));
        }
    }
    if mode == Mode::SafeRestricted && m.writes_files {
        return Err(ExecError::Disabled(
            "safe restricted mode cannot write files".into(),
        ));
    }
    if m.timeout_sec <= 0 {
        return Err(ExecError::Rejected("timeout required".into()));
    }
    Ok(())
}

pub fn execute(mode: Mode, m: &ExecutionManifest) -> Result<ExecResult, ExecError> {
    validate(mode, m)?;
    match mode {
        Mode::Disabled => Err(ExecError::Disabled(
            "无 Docker 且未显式开启不安全模式；自动写文件和命令执行保持禁用".into(),
        )),
        Mode::SafeRestricted => {
            // 兼容读取模式（ADR-034）：argv 只读白名单，无内核强制——
            // 不得宣称沙箱保护；新配置使用 kernel_restricted。
            if !SAFE_READ_ONLY.contains(&m.argv[0].as_str()) {
                return Err(ExecError::Disabled(format!(
                    "命令 {} 不在只读白名单",
                    m.argv[0]
                )));
            }
            process::run_local(&m.argv[0], &m.argv[1..], m, "safe_restricted", None)
        }
        Mode::KernelRestricted => execute_kernel_restricted(m),
        Mode::UnsafeExplicit => {
            let bin = m.argv[0].clone();
            process::run_local(&bin, &m.argv[1..], m, "unsafe_explicit", None)
        }
        Mode::Docker => {
            let docker =
                which_docker().ok_or_else(|| ExecError::Io("docker unavailable".into()))?;
            let mut args: Vec<String> = vec!["run".into(), "--rm".into()];
            // 超时可追踪的容器名：kill 客户端进程不等于停容器，需 docker rm -f 兜底。
            let container = format!("sg-exec-{}-{}", std::process::id(), start_nanos());
            args.push("--name".into());
            args.push(container.clone());
            if m.network_off {
                args.push("--network".into());
                args.push("none".into());
            }
            if m.memory_mb > 0 {
                args.push("--memory".into());
                args.push(format!("{}m", m.memory_mb));
            }
            if !m.image.is_empty() {
                args.push(m.image.clone());
            }
            args.extend(m.argv.iter().cloned());
            let result = process::run_local(&docker, &args, m, "docker", None)?;
            if result.timed_out {
                let _ = std::process::Command::new(&docker)
                    .args(["rm", "-f", &container])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
            Ok(result)
        }
    }
}

/// 内核沙箱受限执行（ADR-034）：
/// - backend 探测为 Unavailable → sandbox_unavailable（fail-closed，不回退未隔离执行）；
/// - Linux Landlock 且 network_off → sandbox_denied（Landlock FS 限制不含网络，
///   强网络隔离由 Docker 承担——诚实降级，不假装隔离）；
/// - 策略 digest 附在 ExecResult（执行快照/审计回查）。
fn execute_kernel_restricted(m: &ExecutionManifest) -> Result<ExecResult, ExecError> {
    let cap = sandbox::probe();
    if cap.backend == sandbox::SandboxBackend::Unavailable.as_str() {
        return Err(ExecError::SandboxUnavailable(cap.blocked_reason));
    }
    let policy = sandbox::SandboxPolicy {
        read_paths: m.sandbox_read_paths.clone(),
        write_paths: m.sandbox_write_paths.clone(),
        network_off: m.network_off,
    };
    let digest = policy.digest();
    if cap.backend == sandbox::SandboxBackend::LinuxLandlock.as_str() && m.network_off {
        return Err(ExecError::SandboxDenied(
            "Landlock 不含网络隔离（需 ABI≥4 或改用 Docker/显式放开 networkOff）".into(),
        ));
    }
    let mut result = sandbox::backend_execute(&policy, m)?;
    result.sandbox_policy_digest = digest;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(argv: &[&str]) -> ExecutionManifest {
        ExecutionManifest {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            work_dir: String::new(),
            image: "alpine:3".into(),
            network_off: true,
            memory_mb: 256,
            cpus: 0.5,
            timeout_sec: 10,
            writes_files: false,
            sandbox_read_paths: vec![],
            sandbox_write_paths: vec![],
        }
    }

    #[test]
    fn disabled_fails_closed() {
        let err = execute(Mode::Disabled, &manifest(&["echo", "hi"])).unwrap_err();
        assert!(err.to_string().contains("unsafe_execution_disabled"));
    }

    #[test]
    fn detect_mode_no_silent_degrade() {
        // Docker 优先；Docker 不可用但内核沙箱可用 → kernel_restricted；
        // 全部不可用且未显式开启 → Disabled（fail-closed）。
        assert_eq!(detect_mode_with_kernel(true, false, true), Mode::Docker);
        assert_eq!(
            detect_mode_with_kernel(false, false, true),
            Mode::KernelRestricted
        );
        assert_eq!(detect_mode_with_kernel(false, false, false), Mode::Disabled);
        assert_eq!(
            detect_mode_with_kernel(false, true, false),
            Mode::UnsafeExplicit
        );
    }

    #[test]
    fn validate_rejects_shell_and_write() {
        let bad = manifest(&["sh", "-c", "a;b"]);
        assert!(validate(Mode::Docker, &bad).is_err());
        let mut write = manifest(&["ls"]);
        write.writes_files = true;
        assert!(validate(Mode::SafeRestricted, &write).is_err());
        // kernel_restricted 允许受管域内写（这正是 M3 的意义）。
        assert!(validate(Mode::KernelRestricted, &write).is_ok());
        let mut no_timeout = manifest(&["ls"]);
        no_timeout.timeout_sec = 0;
        assert!(validate(Mode::Docker, &no_timeout).is_err());
    }

    #[test]
    fn safe_restricted_allowlist() {
        let ok = execute(Mode::SafeRestricted, &manifest(&["ls", "-la"])).unwrap();
        assert_eq!(ok.mode, "safe_restricted");
        assert!(execute(Mode::SafeRestricted, &manifest(&["curl", "http://x"])).is_err());
    }

    #[test]
    fn unsafe_explicit_executes() {
        let result = execute(Mode::UnsafeExplicit, &manifest(&["echo", "unsafe-ok"])).unwrap();
        assert!(result.stdout.contains("unsafe-ok"));
        assert_eq!(result.mode, "unsafe_explicit");
    }

    #[test]
    fn timeout_kills_process() {
        let mut m = manifest(&["sleep", "5"]);
        m.timeout_sec = 1;
        let result = execute(Mode::UnsafeExplicit, &m).unwrap();
        assert!(result.timed_out, "超时必须标记 timed_out");
        assert!(
            result.duration_ms < 3_000,
            "进程必须被真实 kill（耗时 {}ms）",
            result.duration_ms
        );
        assert_eq!(result.exit_code, -1);
    }

    #[test]
    fn large_output_no_pipe_deadlock() {
        // 2MB 输出远超管道缓冲（64KB）：读线程缺失会死锁到超时。
        let m = manifest(&["head", "-c", "2000000", "/dev/zero"]);
        let result = execute(Mode::SafeRestricted, &m).unwrap();
        assert!(!result.timed_out);
        assert_eq!(result.stdout.len(), 2_000_000);
    }

    /// sandbox-test 目录（每测试独占），返回已清理的路径。
    fn sandbox_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sg-sbx-{}-{}-{}",
            tag,
            std::process::id(),
            sg_store::ids::new_id("t")
        ));
        std::fs::create_dir_all(dir.join("worktree/sub")).unwrap();
        std::fs::write(dir.join("worktree/notes.txt"), "hello-m03").unwrap();
        dir
    }

    fn kernel_manifest(root: &std::path::Path, argv: &[&str]) -> ExecutionManifest {
        let worktree = root.join("worktree");
        let mut m = manifest(argv);
        m.work_dir = worktree.to_string_lossy().to_string();
        m.sandbox_read_paths = vec![worktree.to_string_lossy().to_string()];
        m.sandbox_write_paths = vec![
            worktree.to_string_lossy().to_string(),
            root.join("artifacts").to_string_lossy().to_string(),
        ];
        m
    }

    /// ADR-034 安全矩阵（真实 Seatbelt runner，不接受 mock 代替）。
    /// 仅在 macOS 上跑（CI 目标平台）；其他平台该组测试编译为空。
    #[cfg(target_os = "macos")]
    mod macos_kernel {
        use super::*;

        #[test]
        fn kernel_restricted_reads_worktree_and_blocks_sensitive_paths() {
            let root = sandbox_root("mat");
            // 允许：worktree 内读取。
            let ok = execute(
                Mode::KernelRestricted,
                &kernel_manifest(&root, &["cat", "notes.txt"]),
            )
            .unwrap();
            assert!(
                ok.stdout.contains("hello-m03"),
                "worktree 读取被误拒: {}",
                ok.stderr
            );
            assert_eq!(ok.mode, "kernel_restricted");
            assert!(ok.sandbox_policy_digest.starts_with("sha256:"));
            // 拒绝：/etc/passwd（绝对路径）。沙箱拒绝表现为命令退出非零、无内容。
            let denied = execute(
                Mode::KernelRestricted,
                &kernel_manifest(&root, &["cat", "/etc/passwd"]),
            )
            .unwrap();
            assert!(
                denied.exit_code != 0 && !denied.stdout.contains("root"),
                "/etc/passwd 必须被沙箱拒绝: {} {}",
                denied.exit_code,
                denied.stdout
            );
            // 拒绝：父目录逃逸。
            let err = execute(
                Mode::KernelRestricted,
                &kernel_manifest(&root, &["cat", "../worktree/../../etc/passwd"]),
            );
            // 路径守卫可能先拒绝；若执行则沙箱必须拒绝。
            if let Ok(r) = err {
                assert!(r.exit_code != 0, "逃逸读取必须失败");
            }
            // 拒绝：$HOME/.ssh。
            let home = std::env::var("HOME").unwrap();
            let err = execute(
                Mode::KernelRestricted,
                &kernel_manifest(&root, &["ls", &format!("{home}/.ssh")]),
            );
            if let Ok(r) = err {
                assert!(r.exit_code != 0, "~/.ssh 读取必须失败");
            }
            // 拒绝：符号链接逃逸（worktree 内 symlink 指向 /etc/passwd）。
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink("/etc/passwd", root.join("worktree/evil-link")).unwrap();
                let r = execute(
                    Mode::KernelRestricted,
                    &kernel_manifest(&root, &["cat", "evil-link"]),
                )
                .unwrap();
                assert!(
                    r.exit_code != 0 && !r.stdout.contains("root"),
                    "symlink 逃逸必须被拒: {}",
                    r.stdout
                );
            }
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn kernel_restricted_writes_only_in_managed_paths() {
            let root = sandbox_root("wr");
            let ok = execute(
                Mode::KernelRestricted,
                &kernel_manifest(&root, &["touch", "managed.txt"]),
            )
            .unwrap();
            assert_eq!(ok.exit_code, 0, "worktree 写被误拒: {}", ok.stderr);
            assert!(root.join("worktree/managed.txt").exists());
            // 拒绝：受管域外写（HOME）。
            let home = std::env::var("HOME").unwrap();
            let target = format!("{home}/sg-sbx-escape-{}", std::process::id());
            let r = execute(
                Mode::KernelRestricted,
                &kernel_manifest(&root, &["touch", &target]),
            )
            .unwrap();
            assert!(r.exit_code != 0, "受管域外写必须被拒: {}", r.stderr);
            assert!(!std::path::Path::new(&target).exists());
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn kernel_restricted_network_off_blocks_loopback() {
            let root = sandbox_root("net");
            // 本机 listener 打开（listen backlog 会完成握手）：连接被拒只能来自沙箱。
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            // nc -z 只做 TCP 连接探测，连上即退出 0（不需要应用层响应）。
            let base_argv = ["nc", "-z", "-G", "2", "127.0.0.1"];
            let mut m = kernel_manifest(
                &root,
                &[
                    base_argv[0],
                    base_argv[1],
                    base_argv[2],
                    base_argv[3],
                    base_argv[4],
                    &port.to_string(),
                ],
            );
            m.network_off = true;
            let r = execute(Mode::KernelRestricted, &m).unwrap();
            assert!(r.exit_code != 0, "network_off 下 loopback 连接必须失败");
            // 对照组：允许网络时同一 listener 可连（证明失败是沙箱所为）。
            let mut m2 = kernel_manifest(
                &root,
                &[
                    base_argv[0],
                    base_argv[1],
                    base_argv[2],
                    base_argv[3],
                    base_argv[4],
                    &port.to_string(),
                ],
            );
            m2.network_off = false;
            let r2 = execute(Mode::KernelRestricted, &m2).unwrap();
            assert_eq!(r2.exit_code, 0, "放网后 loopback 应可连: {}", r2.stderr);
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn kernel_restricted_subprocess_inherits_restrictions() {
            let root = sandbox_root("sub");
            // sh -c 子进程同样被沙箱约束（metachar 检查不拦无元字符的 -c 脚本）。
            let r = execute(
                Mode::KernelRestricted,
                &kernel_manifest(&root, &["/bin/sh", "-c", "cat /etc/passwd"]),
            )
            .unwrap();
            assert!(
                r.exit_code != 0 && !r.stdout.contains("root"),
                "子进程必须继承沙箱"
            );
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn kernel_restricted_timeout_kills_whole_group() {
            let root = sandbox_root("to");
            // 脚本内容经文件传递（argv 元字符纪律不受影响）：后台 sleep 是 sh 的子进程，
            // 进程组 kill 必须同时清掉 sh 与 sleep。
            let script = root.join("worktree/spawn_child.sh");
            std::fs::write(&script, "sleep 30 &\nwait\n").unwrap();
            let script_str = script.to_string_lossy().to_string();
            let mut m = kernel_manifest(&root, &["/bin/sh", &script_str]);
            m.timeout_sec = 1;
            let started = std::time::Instant::now();
            let r = execute(Mode::KernelRestricted, &m).unwrap();
            assert!(r.timed_out && started.elapsed() < std::time::Duration::from_secs(3));
            // 进程组 kill：sleep 子进程必须无遗留。
            std::thread::sleep(std::time::Duration::from_millis(300));
            // pgrep -x 精确进程名（-f 会自匹配 pgrep 自身 argv 里的模式串）。
            let leftover = std::process::Command::new("/usr/bin/pgrep")
                .arg("-x")
                .arg("sleep")
                .output()
                .unwrap();
            let n = String::from_utf8_lossy(&leftover.stdout)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count();
            assert_eq!(n, 0, "超时后不允许遗留子进程");
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn probe_reports_real_backend() {
            let cap = sandbox::probe();
            assert_eq!(cap.backend, "mac_seatbelt", "本机必须探测到 Seatbelt");
            assert!(cap.blocked_reason.is_empty());
            assert!(cap.version.contains("seatbelt"));
        }
    }
}

#[cfg(test)]
mod compat_tests_keep_imports {}

fn start_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
