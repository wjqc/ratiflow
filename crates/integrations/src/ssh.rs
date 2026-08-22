//! SSH 适配器：参数化命令计划（无自由 Shell）+ fake（指纹变化/中断/回滚失败场景）。
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SSHTarget {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: i64,
    pub user: String,
    #[serde(default)]
    pub expected_fingerprint: String,
    #[serde(default)]
    pub remote_dir: String,
}

fn default_port() -> i64 {
    22
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SSHCommand {
    pub name: String,
    pub argv: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_sec: i64,
}

fn default_timeout() -> i64 {
    120
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub name: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub trait SSHAdapter: Send + Sync {
    fn preflight(&self, target: &SSHTarget) -> Result<serde_json::Value, String>;
    fn run_command_plan(
        &self,
        target: &SSHTarget,
        commands: &[SSHCommand],
    ) -> Result<Vec<CommandResult>, String>;
}

/// shell 元字符拒绝（argv 直传是硬边界）。
pub fn validate_argv(argv: &[String]) -> Result<(), String> {
    if argv.is_empty() {
        return Err("ssh_command_rejected: empty argv".into());
    }
    for arg in argv {
        if [';', '|', '&', '`', '$', '>', '<', '\n']
            .iter()
            .any(|c| arg.contains(*c))
        {
            return Err(format!(
                "ssh_command_rejected: shell metacharacter in {arg:?}"
            ));
        }
    }
    Ok(())
}

/// 系统 ssh CLI 适配器（BatchMode、StrictHostKeyChecking、argv 直传）。
pub struct SSHExec;

impl SSHAdapter for SSHExec {
    fn preflight(&self, target: &SSHTarget) -> Result<serde_json::Value, String> {
        let plan = [
            SSHCommand {
                name: "docker".into(),
                argv: vec!["docker".into(), "--version".into()],
                timeout_sec: 10,
            },
            SSHCommand {
                name: "compose".into(),
                argv: vec!["docker".into(), "compose".into(), "version".into()],
                timeout_sec: 10,
            },
            SSHCommand {
                name: "dir".into(),
                argv: vec!["test".into(), "-d".into(), target.remote_dir.clone()],
                timeout_sec: 10,
            },
        ];
        let results = self.run_command_plan(target, &plan)?;
        let docker_ok = results
            .iter()
            .find(|r| r.name == "docker")
            .map(|r| r.exit_code == 0)
            .unwrap_or(false);
        let compose_ok = results
            .iter()
            .find(|r| r.name == "compose")
            .map(|r| r.exit_code == 0)
            .unwrap_or(false);
        let dir_ok = results
            .iter()
            .find(|r| r.name == "dir")
            .map(|r| r.exit_code == 0)
            .unwrap_or(false);
        Ok(serde_json::json!({
            "host": target.host,
            "dockerOk": docker_ok,
            "composeOk": compose_ok,
            "remoteDirOk": dir_ok,
        }))
    }

    fn run_command_plan(
        &self,
        target: &SSHTarget,
        commands: &[SSHCommand],
    ) -> Result<Vec<CommandResult>, String> {
        let mut results = Vec::new();
        for cmd in commands {
            validate_argv(&cmd.argv)?;
            let host_port = format!("{}@{}:{}", target.user, target.host, target.port);
            let mut args: Vec<String> = vec![
                "-o".into(),
                "BatchMode=yes".into(),
                "-o".into(),
                "StrictHostKeyChecking=yes".into(),
                "-o".into(),
                "ConnectTimeout=10".into(),
                host_port,
                "--".into(),
            ];
            args.extend(cmd.argv.iter().cloned());
            let output = std::process::Command::new("ssh")
                .args(&args)
                .output()
                .map_err(|e| format!("ssh: {e}"))?;
            let exit = output.status.code().unwrap_or(-1);
            results.push(CommandResult {
                name: cmd.name.clone(),
                exit_code: exit,
                stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
            if exit != 0 {
                return Err(format!("command {} exited {exit}", cmd.name));
            }
        }
        Ok(results)
    }
}

/// fake：脚本化预检/命令结果与失败注入。
#[derive(Default)]
pub struct FakeSSH {
    pub preflight_error: Option<String>,
    pub plan_error: Option<String>,
}

impl SSHAdapter for FakeSSH {
    fn preflight(&self, target: &SSHTarget) -> Result<serde_json::Value, String> {
        if let Some(err) = &self.preflight_error {
            return Err(err.clone());
        }
        Ok(serde_json::json!({
            "host": target.host,
            "dockerOk": true,
            "composeOk": true,
            "remoteDirOk": true,
        }))
    }

    fn run_command_plan(
        &self,
        target: &SSHTarget,
        commands: &[SSHCommand],
    ) -> Result<Vec<CommandResult>, String> {
        let _ = target;
        if let Some(err) = &self.plan_error {
            return Err(err.clone());
        }
        Ok(commands
            .iter()
            .map(|c| CommandResult {
                name: c.name.clone(),
                exit_code: 0,
                stdout: "ok".into(),
                stderr: String::new(),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_rejects_metacharacters() {
        assert!(validate_argv(&["sh".into(), "-c".into(), "a;b".into()]).is_err());
        assert!(validate_argv(&[]).is_err());
        assert!(
            validate_argv(&["docker".into(), "compose".into(), "up".into(), "-d".into()]).is_ok()
        );
    }

    #[test]
    fn fake_ssh_flows() {
        let ssh = FakeSSH::default();
        let target = SSHTarget {
            host: "deploy.test".into(),
            port: 22,
            user: "deploy".into(),
            expected_fingerprint: String::new(),
            remote_dir: "/srv".into(),
        };
        let report = ssh.preflight(&target).unwrap();
        assert_eq!(report["dockerOk"], serde_json::json!(true));
        let results = ssh
            .run_command_plan(
                &target,
                &[SSHCommand {
                    name: "up".into(),
                    argv: vec!["docker".into(), "compose".into(), "up".into(), "-d".into()],
                    timeout_sec: 60,
                }],
            )
            .unwrap();
        assert_eq!(results.len(), 1);
    }
}
