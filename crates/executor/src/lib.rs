//! 受约束执行（v2 ADR-024 行为等价）：只接受 ExecutionManifest，拒绝自由 Shell。
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Docker,
    SafeRestricted,
    UnsafeExplicit,
    Disabled,
}

/// 模式判定：不静默降级（无 Docker 且未显式开启 → Disabled 失败关闭）。
pub fn detect_mode(docker_available: bool, unsafe_explicit: bool) -> Mode {
    if docker_available {
        Mode::Docker
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
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecResult {
    pub mode: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub timed_out: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("unsafe_execution_disabled: {0}")]
    Disabled(String),
    #[error("manifest_rejected: {0}")]
    Rejected(String),
    #[error("execute: {0}")]
    Io(String),
}

const SAFE_READ_ONLY: [&str; 10] = ["ls", "cat", "grep", "find", "wc", "head", "tail", "git", "rg", "stat"];

pub fn validate(mode: Mode, m: &ExecutionManifest) -> Result<(), ExecError> {
    if m.argv.is_empty() {
        return Err(ExecError::Rejected("argv required".into()));
    }
    for arg in &m.argv {
        if [';', '|', '&', '`', '$', '>', '<', '\n'].iter().any(|c| arg.contains(*c)) {
            return Err(ExecError::Rejected(format!("shell metacharacter in argv {arg:?}")));
        }
    }
    if mode == Mode::SafeRestricted && m.writes_files {
        return Err(ExecError::Disabled("safe restricted mode cannot write files".into()));
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
            if !SAFE_READ_ONLY.contains(&m.argv[0].as_str()) {
                return Err(ExecError::Disabled(format!("命令 {} 不在只读白名单", m.argv[0])));
            }
            run_local(&m.argv[0], &m.argv[1..], m, "safe_restricted")
        }
        Mode::UnsafeExplicit => {
            let bin = m.argv[0].clone();
            run_local(&bin, &m.argv[1..], m, "unsafe_explicit")
        }
        Mode::Docker => {
            let docker = which_docker().ok_or_else(|| ExecError::Io("docker unavailable".into()))?;
            let mut args: Vec<String> = vec!["run".into(), "--rm".into()];
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
            run_local(&docker, &args, m, "docker")
        }
    }
}

fn run_local(bin: &str, args: &[String], m: &ExecutionManifest, mode: &str) -> Result<ExecResult, ExecError> {
    let start = Instant::now();
    let mut cmd = Command::new(bin);
    cmd.args(args);
    if !m.work_dir.is_empty() {
        let dir = std::path::Path::new(&m.work_dir);
        if dir.is_dir() {
            cmd.current_dir(dir);
        }
    }
    let output = cmd
        .output()
        .map_err(|e| ExecError::Io(e.to_string()))?;
    let elapsed = start.elapsed();
    Ok(ExecResult {
        mode: mode.into(),
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        duration_ms: elapsed.as_millis() as u64,
        timed_out: elapsed > Duration::from_secs(m.timeout_sec as u64),
    })
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
        }
    }

    #[test]
    fn disabled_fails_closed() {
        let err = execute(Mode::Disabled, &manifest(&["echo", "hi"])).unwrap_err();
        assert!(err.to_string().contains("unsafe_execution_disabled"));
    }

    #[test]
    fn detect_mode_no_silent_degrade() {
        assert_eq!(detect_mode(true, false), Mode::Docker);
        assert_eq!(detect_mode(false, false), Mode::Disabled);
        assert_eq!(detect_mode(false, true), Mode::UnsafeExplicit);
    }

    #[test]
    fn validate_rejects_shell_and_write() {
        let bad = manifest(&["sh", "-c", "a;b"]);
        assert!(validate(Mode::Docker, &bad).is_err());
        let mut write = manifest(&["ls"]);
        write.writes_files = true;
        assert!(validate(Mode::SafeRestricted, &write).is_err());
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
}
