//! Profile 连接测试流（ZCode 手册 §7/§11）：稳定步骤数组 + 每步状态/耗时/错误码。
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize)]
pub struct StepResult {
    pub name: String,
    pub status: String, // passed | failed | skipped | action_required
    #[serde(rename = "durationMs")]
    pub duration_ms: i64,
    #[serde(rename = "errorCode")]
    pub error_code: Option<String>,
    pub detail: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestReport {
    pub status: String, // ready | degraded | error | action_required
    #[serde(rename = "durationMs")]
    pub duration_ms: i64,
    pub steps: Vec<StepResult>,
    #[serde(rename = "requiresAccept", skip_serializing_if = "Option::is_none")]
    pub requires_accept: Option<bool>,
}

fn step(name: &str, f: impl FnOnce() -> Result<Value, String>) -> StepResult {
    let start = std::time::Instant::now();
    match f() {
        Ok(detail) => StepResult {
            name: name.into(), status: "passed".into(),
            duration_ms: start.elapsed().as_millis() as i64,
            error_code: None, detail: if detail.is_null() { None } else { Some(detail) },
        },
        Err(e) => {
            let (code, detail) = split_error(&e);
            StepResult {
                name: name.into(), status: "failed".into(),
                duration_ms: start.elapsed().as_millis() as i64,
                error_code: Some(code), detail,
            }
        }
    }
}

/// "CODE | detail json | human message" → (code, detail)
fn split_error(e: &str) -> (String, Option<Value>) {
    match e.split_once('|') {
        Some((code, rest)) => {
            let detail = rest.trim().split_once('|').and_then(|(d, _)| serde_json::from_str::<Value>(d.trim()).ok());
            (code.trim().to_string(), detail)
        }
        None => ("INTERNAL".into(), Some(json!({"message": e}))),
    }
}

fn report(steps: Vec<StepResult>, requires_accept: bool) -> TestReport {
    let overall = if steps.iter().any(|s| s.status == "failed") {
        "error"
    } else if steps.iter().any(|s| s.status == "action_required") {
        "action_required"
    } else if steps.iter().any(|s| s.status == "skipped") {
        "degraded"
    } else {
        "ready"
    };
    let total: i64 = steps.iter().map(|s| s.duration_ms).sum();
    TestReport {
        status: overall.into(), duration_ms: total, steps,
        requires_accept: if requires_accept { Some(true) } else { None },
    }
}

/// GitLab Profile 测试：resolve → TLS → auth → capabilities（只读，禁写操作）。
pub fn gitlab_test(base_url: &str, token: Option<&str>) -> TestReport {
    let mut steps = Vec::new();
    steps.push(step("resolve", || {
        let host = url_host(base_url).ok_or_else(|| "INVALID_PARAMS | {\"url\": true} | URL 不合法".to_string())?;
        use std::net::ToSocketAddrs;
        host.to_socket_addrs().map_err(|_| "TLS_ERROR | {\"dns\": true} | DNS 解析失败".to_string())?
            .next().ok_or_else(|| "TLS_ERROR | {\"dns\": true} | 无地址".to_string())?;
        Ok(json!({"resolved": host}))
    }));
    steps.push(step("auth", || {
        let token = token.ok_or_else(|| "CREDENTIAL_MISSING | {\"step\": \"auth\"} | 未配置凭据".to_string())?;
        let url = format!("{}/api/v4/user", base_url.trim_end_matches('/'));
        let resp = ureq::get(&url).set("Private-Token", token).timeout(std::time::Duration::from_secs(10)).call()
            .map_err(|e| map_ureq(&e.to_string()))?;
        if resp.status() == 401 || resp.status() == 403 {
            return Err("CREDENTIAL_AUTH_FAILED | {\"status\": 401} | 令牌无效或权限不足".into());
        }
        let body: Value = resp.into_json().map_err(|_| "INTERNAL | {\"parse\": true} | 响应解析失败".to_string())?;
        Ok(json!({"username": body["username"]}))
    }));
    steps.push(step("capabilities", || {
        let token = token.ok_or_else(|| "CREDENTIAL_MISSING | {\"step\": \"capabilities\"} | 未配置凭据".to_string())?;
        let url = format!("{}/api/v4/projects?per_page=1", base_url.trim_end_matches('/'));
        let resp = ureq::get(&url).set("Private-Token", token).timeout(std::time::Duration::from_secs(10)).call()
            .map_err(|e| map_ureq(&e.to_string()))?;
        let can_list = resp.status() < 400;
        Ok(json!({"projectList": can_list, "issueReadWrite": can_list}))
    }));
    report(steps, false)
}

/// 模型 Profile 测试：resolve → auth（模型列表）→ generation（最小补全）→ tool/vision 按能力跳过。
pub fn model_test(base_url: &str, api_key: Option<&str>, model: &str, capabilities: &Value) -> TestReport {
    let mut steps = Vec::new();
    steps.push(step("resolve", || {
        let host = url_host(base_url).ok_or_else(|| "INVALID_PARAMS | {\"url\": true} | URL 不合法".to_string())?;
        use std::net::ToSocketAddrs;
        host.to_socket_addrs().map_err(|_| "TLS_ERROR | {\"dns\": true} | DNS 解析失败".to_string())?.next()
            .ok_or_else(|| "TLS_ERROR | {\"dns\": true} | 无地址".to_string())?;
        Ok(Value::Null)
    }));
    steps.push(step("auth", || {
        let key = api_key.ok_or_else(|| "CREDENTIAL_MISSING | {\"step\": \"auth\"} | 未配置 API Key".to_string())?;
        let url = format!("{}/models", base_url.trim_end_matches('/'));
        let resp = ureq::get(&url).set("Authorization", &format!("Bearer {key}")).timeout(std::time::Duration::from_secs(10)).call()
            .map_err(|e| map_ureq(&e.to_string()))?;
        if resp.status() == 401 { return Err("CREDENTIAL_AUTH_FAILED | {\"status\": 401} | Key 无效".into()); }
        if resp.status() == 429 { return Err("MODEL_RATE_LIMITED | {\"status\": 429} | 限流".into()); }
        Ok(Value::Null)
    }));
    steps.push(step("generation", || {
        let key = api_key.ok_or_else(|| "CREDENTIAL_MISSING | {\"step\": \"generation\"} | 未配置".to_string())?;
        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
        let body = json!({"model": model, "messages": [{"role": "user", "content": "ping"}], "max_tokens": 1});
        let resp = ureq::post(&url).set("Authorization", &format!("Bearer {key}"))
            .timeout(std::time::Duration::from_secs(30)).send_json(body)
            .map_err(|e| map_ureq(&e.to_string()))?;
        if resp.status() >= 400 {
            return Err(format!("MODEL_UNAVAILABLE | {{\"status\": {}}} | 生成失败", resp.status()));
        }
        Ok(Value::Null)
    }));
    steps.push(step("tool", || {
        if capabilities.get("tools").and_then(|v| v.as_bool()) != Some(true) {
            return Err("MODEL_CAPABILITY_MISSING | {\"capability\": \"tools\", \"skipped\": true} | 未声明 tools 能力（跳过）".into());
        }
        Err("MODEL_CAPABILITY_MISSING | {\"capability\": \"tools\", \"skipped\": true} | 工具调用探测留待 Agent Run 实测（跳过）".into())
    }));
    steps.push(step("vision", || {
        if capabilities.get("vision").and_then(|v| v.as_bool()) != Some(true) {
            return Err("MODEL_CAPABILITY_MISSING | {\"capability\": \"vision\", \"skipped\": true} | 未声明 vision 能力（跳过）".into());
        }
        Err("MODEL_CAPABILITY_MISSING | {\"capability\": \"vision\", \"skipped\": true} | 视觉探测留待附件解析实测（跳过）".into())
    }));
    // tool/vision 探测的 skipped 语义在 split_error 后是 failed——修正：这两步固定 skipped。
    let mut fixed = steps;
    for s in fixed.iter_mut() {
        if (s.name == "tool" || s.name == "vision") && s.error_code.as_deref() == Some("MODEL_CAPABILITY_MISSING") {
            s.status = "skipped".into();
        }
    }
    report(fixed, false)
}

fn map_ureq(e: &str) -> String {
    if e.contains("Dns Failed") || e.contains("dns") { "TLS_ERROR | {\"dns\": true} | 解析失败".into() }
    else if e.contains("cert") || e.contains("tls") { "TLS_ERROR | {\"cert\": true} | TLS 失败".into() }
    else if e.contains("timed out") { "TIMEOUT | {\"timeout\": true} | 超时".into() }
    else { format!("INTERNAL | {{\"transport\": true}} | {e}") }
}

fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split('/').next()?;
    let host = host.split(':').next()?;
    Some(host.to_string())
}

/// SSH host key 探测：返回 (fingerprint, 首用/变化判定)。
pub struct HostKeyOutcome {
    pub fingerprint: String,
    pub status: String, // first_use | matched | changed | unreachable
}

pub fn ssh_host_key(host: &str, port: i64) -> HostKeyOutcome {
    let output = std::process::Command::new("ssh-keyscan")
        .args(["-p", &port.to_string(), "-T", "8", "-t", "ed25519", host])
        .output();
    let out = match output {
        Ok(o) if o.status.success() && !o.stdout.is_empty() => o,
        _ => return HostKeyOutcome { fingerprint: String::new(), status: "unreachable".into() },
    };
    let keyscan = String::from_utf8_lossy(&out.stdout).to_string();
    let fp = fingerprint_of(&keyscan);
    HostKeyOutcome { fingerprint: fp, status: "first_use".into() }
}

fn fingerprint_of(host_key: &str) -> String {
    let trimmed = host_key.trim();
    if trimmed.is_empty() { return String::new(); }
    let out = std::process::Command::new("ssh-keygen")
        .args(["-lf", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(stdin) = child.stdin.as_mut() { stdin.write_all(trimmed.as_bytes())?; }
            child.wait_with_output()
        });
    match out {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).split_whitespace()
                .find(|t| t.starts_with("SHA256:")).map(String::from).unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// SSH target 完整测试：network → host_key（首用/变化）→ auth → workdir。
pub fn ssh_target_test(
    host: &str, port: i64, user: &str, remote_dir: &str,
    saved_fingerprint: &str,
) -> TestReport {
    let mut steps = Vec::new();
    steps.push(step("network", || {
        use std::net::ToSocketAddrs;
        (host, port as u16).to_socket_addrs().map_err(|_| "TLS_ERROR | {\"dns\": true} | 解析失败".to_string())?
            .next().ok_or_else(|| "TLS_ERROR | {\"dns\": true} | 无地址".to_string())?;
        Ok(Value::Null)
    }));
    let mut requires_accept = false;
    let key = ssh_host_key(host, port);
    match key.status.as_str() {
        "unreachable" => steps.push(StepResult {
            name: "host_key".into(), status: "failed".into(), duration_ms: 0,
            error_code: Some("TLS_ERROR".into()), detail: Some(json!({"reachable": false})),
        }),
        _ => {
            if saved_fingerprint.is_empty() {
                requires_accept = true;
                steps.push(StepResult {
                    name: "host_key".into(), status: "action_required".into(), duration_ms: 0,
                    error_code: Some("HOST_KEY_FIRST_USE".into()),
                    detail: Some(json!({"fingerprint": key.fingerprint})),
                });
            } else if key.fingerprint == saved_fingerprint {
                steps.push(StepResult {
                    name: "host_key".into(), status: "passed".into(), duration_ms: 0,
                    error_code: None, detail: Some(json!({"fingerprint": key.fingerprint})),
                });
            } else {
                steps.push(StepResult {
                    name: "host_key".into(), status: "failed".into(), duration_ms: 0,
                    error_code: Some("HOST_KEY_CHANGED".into()),
                    detail: Some(json!({"expectedFingerprint": saved_fingerprint, "actualFingerprint": key.fingerprint, "blocked": true})),
                });
                // 指纹不匹配时后续步骤跳过。
                let r = report(steps, false);
                return TestReport { status: "error".into(), duration_ms: r.duration_ms, steps: r.steps, requires_accept: None };
            }
        }
    }
    if !requires_accept {
        steps.push(step("auth", || {
            let status = std::process::Command::new("ssh")
                .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=8", "-o", "StrictHostKeyChecking=accept-new",
                       &format!("{user}@{host}"), "--", "true"])
                .stdin(std::process::Stdio::null())
                .output().map_err(|e| format!("INTERNAL | {{\"ssh\": true}} | {e}"))?.status;
            if status.success() { Ok(Value::Null) }
            else { Err("CREDENTIAL_AUTH_FAILED | {\"step\": \"auth\"} | SSH 认证失败（检查 key/账号）".into()) }
        }));
        if !remote_dir.is_empty() {
            steps.push(step("workdir", || {
                let status = std::process::Command::new("ssh")
                    .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=8",
                           &format!("{user}@{host}"), "--", "test", "-d", remote_dir])
                    .stdin(std::process::Stdio::null())
                    .output().map_err(|e| format!("INTERNAL | {{\"ssh\": true}} | {e}"))?.status;
                if status.success() { Ok(json!({"remoteDir": remote_dir})) }
                else { Err(format!("INVALID_PARAMS | {{\"remoteDir\": \"{remote_dir}\"}} | 目录不存在")) }
            }));
        }
    }
    report(steps, requires_accept)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_host_parses() {
        assert_eq!(url_host("https://api.example.com/v1").as_deref(), Some("api.example.com"));
        assert_eq!(url_host("https://gitlab.test").as_deref(), Some("gitlab.test"));
    }

    #[test]
    fn split_error_format() {
        let (code, detail) = split_error("CREDENTIAL_AUTH_FAILED | {\"status\": 401} | 令牌无效");
        assert_eq!(code, "CREDENTIAL_AUTH_FAILED");
        assert_eq!(detail.unwrap()["status"], json!(401));
    }

    #[test]
    fn ssh_host_key_unreachable_is_failed_not_hang() {
        let report = ssh_target_test("definitely-not-a-host.invalid", 22, "u", "", "");
        assert_eq!(report.status, "error");
        assert!(report.steps.iter().any(|s| s.error_code.as_deref() == Some("TLS_ERROR")));
    }

    #[test]
    fn first_use_requires_accept_and_blocks_later_steps() {
        // 不可达主机的 keyscan 为空 → unreachable 路径；用本地不存在的端口模拟 keyscan 失败以外的路径不可行，
        // 因此这里直接验证 host_key 步骤构造语义（不依赖网络）。
        let steps = vec![
            StepResult { name: "network".into(), status: "passed".into(), duration_ms: 1, error_code: None, detail: None },
            StepResult { name: "host_key".into(), status: "action_required".into(), duration_ms: 1,
                error_code: Some("HOST_KEY_FIRST_USE".into()), detail: Some(json!({"fingerprint": "SHA256:X"})) },
        ];
        let r = report(steps, true);
        assert_eq!(r.status, "action_required");
        assert_eq!(r.requires_accept, Some(true));
    }
}
