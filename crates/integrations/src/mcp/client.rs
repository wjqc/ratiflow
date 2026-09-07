//! MCP JSON-RPC 客户端：传输抽象（stdio / 测试脚本）+ initialize/tools/call。
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

use super::types::{McpServerInfo, McpToolCallOutcome, McpToolDescriptor};

/// 传输抽象：按行收发 JSON-RPC。测试用 ScriptedTransport 注入协议场景。
pub trait McpTransport: Send {
    fn send_line(&mut self, line: &str) -> Result<(), String>;
    /// 带超时收行；超时返回 Err("timeout")。
    fn recv_line(&mut self, timeout: Duration) -> Result<String, String>;
    /// 关闭底层连接/进程（超时看门狗与重放边界用）。
    fn shutdown(&mut self);
}

/// stdio 传输：子进程按行交换 JSON-RPC（进程随 client 生命周期）。
pub struct StdioTransport {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl StdioTransport {
    /// 拉起 MCP server（本地 stdio；调用方负责命令来源为管理员注册的受控配置）。
    /// M3 沙箱接线：command 经注册审批后固定；当前以直启落地，沙箱包裹为
    /// ADR-035 的后续硬化项（内核沙箱包裹 spawn 见 executor::sandbox）。
    pub fn spawn(command: &str, args: &[String]) -> Result<Self, String> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("mcp spawn 失败: {e}"))?;
        let stdin = child.stdin.take().ok_or("mcp stdin 不可用")?;
        let stdout = child.stdout.take().ok_or("mcp stdout 不可用")?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }
}

impl McpTransport for StdioTransport {
    fn send_line(&mut self, line: &str) -> Result<(), String> {
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|_| self.stdin.write_all(b"\n"))
            .and_then(|_| self.stdin.flush())
            .map_err(|e| format!("mcp 写失败: {e}"))
    }

    fn recv_line(&mut self, timeout: Duration) -> Result<String, String> {
        // 同步读无超时：看门狗线程超时后 kill 进程 → EOF → 按超时归类；
        // 成功读到行后先 disarm 再 join（否则 watchdog 余眠会阻塞并误杀子进程）。
        let child_pid = self.child.id();
        let disarmed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = disarmed.clone();
        let watchdog = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + timeout;
            while std::time::Instant::now() < deadline {
                if flag.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            if !flag.load(std::sync::atomic::Ordering::Relaxed) {
                unsafe {
                    // server 由本模块拉起；kill 单进程即触发读端 EOF。
                    libc::kill(child_pid as i32, libc::SIGKILL);
                }
            }
        });
        let mut line = String::new();
        let r = self.stdout.read_line(&mut line);
        disarmed.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = watchdog.join();
        match r {
            Ok(0) | Err(_) => Err("timeout".into()),
            Ok(_) => Ok(line),
        }
    }

    fn shutdown(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 测试脚本传输：按脚本回放响应、记录请求（协议场景 E2E 用）。
#[derive(Default)]
pub struct ScriptedTransport {
    pub requests: std::sync::Mutex<Vec<String>>,
    script: std::sync::Mutex<std::collections::VecDeque<Result<String, String>>>,
}

impl ScriptedTransport {
    /// script：逐条预置响应（Err("timeout") 模拟超时/无响应）。
    pub fn new(script: Vec<Result<String, String>>) -> Self {
        Self {
            requests: std::sync::Mutex::new(Vec::new()),
            script: std::sync::Mutex::new(script.into_iter().collect()),
        }
    }
}

impl McpTransport for ScriptedTransport {
    fn send_line(&mut self, line: &str) -> Result<(), String> {
        self.requests.lock().unwrap().push(line.to_string());
        Ok(())
    }

    fn recv_line(&mut self, _timeout: Duration) -> Result<String, String> {
        self.script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err("timeout".into()))
    }

    fn shutdown(&mut self) {}
}

/// MCP 客户端（一次会话：initialize → tools/list → tools/call → 结束）。
pub struct McpClient<T: McpTransport> {
    transport: T,
    next_id: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("mcp_timeout: {0}")]
    Timeout(String),
    #[error("mcp_protocol: {0}")]
    Protocol(String),
    #[error("mcp_transport: {0}")]
    Transport(String),
}

impl<T: McpTransport> McpClient<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            next_id: 1,
        }
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        self.request_phased(method, params, timeout, &|_| Ok(()), &|_| Ok(()))
    }

    /// WP-3（RDWS v1.4 §2）：带调用相位回调的请求——on_intent 在写入 stdin **前**
    /// （此时落库即「intent 已持久化、flush 是否发生不可知」窗口的权威起点），
    /// on_flushed 在 flush 成功后（副作用可能已发生的事实起点）。回调 Err = 中止调用
    /// （transport 错误语义）。McpClient 不决定重试，相位终态由编排器定。
    fn request_phased(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
        on_intent: &dyn Fn(&str) -> Result<(), String>,
        on_flushed: &dyn Fn(&str) -> Result<(), String>,
    ) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        let line = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string();
        on_intent(method).map_err(McpError::Transport)?;
        self.transport
            .send_line(&line)
            .map_err(McpError::Transport)?;
        on_flushed(method).map_err(McpError::Transport)?;
        loop {
            let raw = self.transport.recv_line(timeout).map_err(|e| {
                if e == "timeout" {
                    McpError::Timeout(format!("{method} 超时（{timeout:?}）"))
                } else {
                    McpError::Transport(e)
                }
            })?;
            let v: Value = serde_json::from_str(raw.trim())
                .map_err(|e| McpError::Protocol(format!("响应非 JSON: {e}")))?;
            if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                if let Some(err) = v.get("error") {
                    return Err(McpError::Protocol(format!(
                        "server 返回错误: {}",
                        err["message"].as_str().unwrap_or("unknown")
                    )));
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            // 通知/其他 id 的响应：跳过继续等。
        }
    }

    fn notify(&mut self, method: &str) -> Result<(), McpError> {
        let line = json!({"jsonrpc":"2.0","method":method}).to_string();
        self.transport.send_line(&line).map_err(McpError::Transport)
    }

    /// 握手：initialize → notifications/initialized。
    pub fn initialize(&mut self) -> Result<McpServerInfo, McpError> {
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "sixgates-core", "version": env!("CARGO_PKG_VERSION")},
            }),
            Duration::from_secs(30),
        )?;
        self.notify("notifications/initialized")?;
        Ok(McpServerInfo {
            name: result["serverInfo"]["name"]
                .as_str()
                .unwrap_or_default()
                .into(),
            version: result["serverInfo"]["version"]
                .as_str()
                .unwrap_or_default()
                .into(),
            protocol_version: result["protocolVersion"]
                .as_str()
                .unwrap_or_default()
                .into(),
        })
    }

    /// 工具清单（read_only_hint 缺省 false = 保守按写处理）。
    pub fn list_tools(&mut self) -> Result<Vec<McpToolDescriptor>, McpError> {
        let result = self.request("tools/list", json!({}), Duration::from_secs(30))?;
        let mut out = Vec::new();
        for t in result["tools"].as_array().cloned().unwrap_or_default() {
            out.push(McpToolDescriptor {
                name: t["name"].as_str().unwrap_or_default().into(),
                description: t["description"].as_str().unwrap_or_default().into(),
                input_schema: t["inputSchema"].clone(),
                read_only_hint: t["annotations"]["readOnlyHint"].as_bool().unwrap_or(false),
            });
        }
        Ok(out)
    }

    /// 调用工具。超时 → McpToolCallOutcome::Timeout（调用方按 sideEffect 归类
    /// tool_outcome_unknown / 可重试错误）。
    pub fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<McpToolCallOutcome, McpError> {
        self.call_tool_phased(name, arguments, timeout, &|_| Ok(()), &|_| Ok(()))
    }

    /// WP-3：带 send_phase 回调的工具调用（相位语义见 request_phased）。
    pub fn call_tool_phased(
        &mut self,
        name: &str,
        arguments: Value,
        timeout: Duration,
        on_intent: &dyn Fn(&str) -> Result<(), String>,
        on_flushed: &dyn Fn(&str) -> Result<(), String>,
    ) -> Result<McpToolCallOutcome, McpError> {
        let result = match self.request_phased(
            "tools/call",
            json!({"name": name, "arguments": arguments}),
            timeout,
            on_intent,
            on_flushed,
        ) {
            Ok(r) => r,
            Err(McpError::Timeout(_e)) => return Ok(McpToolCallOutcome::Timeout),
            Err(e) => return Err(e),
        };
        let is_error = result["isError"].as_bool().unwrap_or(false);
        let mut text = String::new();
        if let Some(items) = result["content"].as_array() {
            for item in items {
                if item["type"].as_str() == Some("text") {
                    if let Some(t) = item["text"].as_str() {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(t);
                    }
                }
            }
        }
        Ok(McpToolCallOutcome::Ok { text, is_error })
    }

    pub fn shutdown(&mut self) {
        self.transport.shutdown();
    }
}

/// WP-3（RDWS v1.4）流控上限：单帧 ≤4MiB、单次会话累计 ≤32MiB（超限 = transport
/// 事件，由调用方按 send_phase + effect class 定终态，不得先行写死 failed）。
pub const MCP_MAX_FRAME_BYTES: usize = 4 << 20;
pub const MCP_MAX_SESSION_BYTES: usize = 32 << 20;

/// 受管沙箱传输：MCP server 进程经 sg_sandbox::spawn_sandboxed 拉起（禁网 + FS
/// 限制 + 进程组回收 + stderr 64KiB 环形）。平台不支持时返回
/// mcp_platform_unsupported / mcp_sandbox_unavailable 前缀错误（调用方 fail-closed，
/// 不得回退直启）。EOF 不再伪装 timeout：未主动 shutdown 的对端关闭是 transport 事件。
pub struct SandboxedTransport {
    managed: sg_sandbox::ManagedChild,
    session_bytes: usize,
}

impl SandboxedTransport {
    pub fn spawn(
        policy: &sg_sandbox::SandboxPolicy,
        command: &str,
        args: &[String],
    ) -> Result<Self, String> {
        Self::spawn_in(policy, None, command, args)
    }

    /// 同 spawn，但子进程以 work_dir 为 cwd（导入型 server 以 checkout 为工作目录）。
    pub fn spawn_in(
        policy: &sg_sandbox::SandboxPolicy,
        work_dir: Option<&std::path::Path>,
        command: &str,
        args: &[String],
    ) -> Result<Self, String> {
        let argv = std::iter::once(command.to_string())
            .chain(args.iter().cloned())
            .collect::<Vec<_>>();
        let managed = sg_sandbox::spawn_sandboxed_in(policy, &argv, work_dir)
            .map_err(|e| format!("mcp_sandbox_unavailable: {e}"))?;
        Ok(Self {
            managed,
            session_bytes: 0,
        })
    }

    /// policy snapshot digest（审计 sandboxed:true + digest 证据）。
    pub fn policy_digest(&self) -> String {
        self.managed.policy_digest().to_string()
    }

    /// stderr 快照（环形最近内容 + 是否超限丢弃，进 provider_evidence）。
    pub fn stderr_snapshot(&self) -> (String, bool) {
        self.managed.stderr_snapshot()
    }
}

impl McpTransport for SandboxedTransport {
    fn send_line(&mut self, line: &str) -> Result<(), String> {
        if line.len() > MCP_MAX_FRAME_BYTES {
            return Err(format!(
                "mcp_frame_too_long: 请求帧 {}B 超单帧上限 {MCP_MAX_FRAME_BYTES}B",
                line.len()
            ));
        }
        self.managed
            .stdin()
            .write_all(line.as_bytes())
            .and_then(|_| self.managed.stdin().write_all(b"\n"))
            .and_then(|_| self.managed.stdin().flush())
            .map_err(|e| format!("mcp_transport_write: {e}"))
    }

    fn recv_line(&mut self, timeout: Duration) -> Result<String, String> {
        // 同步读无超时：看门狗超时 kill 子进程（单 pid 即触发读端 EOF）打断阻塞；
        // 成功读到行先 disarm 再 join。整组回收由 shutdown 的 kill_group 承担。
        let pid = self.managed.pid();
        let disarmed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = disarmed.clone();
        let watchdog = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + timeout;
            while std::time::Instant::now() < deadline {
                if flag.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            if !flag.load(std::sync::atomic::Ordering::Relaxed) {
                unsafe {
                    libc::kill(pid as i32, libc::SIGKILL);
                }
            }
        });
        let mut line = String::new();
        let r = self.managed.stdout().read_line(&mut line);
        disarmed.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = watchdog.join();
        match r {
            Ok(0) => Err("mcp_transport_eof: 对端关闭（未主动 shutdown）".into()),
            Err(e) => Err(format!("mcp_transport_read: {e}")),
            Ok(_) => {
                if line.len() > MCP_MAX_FRAME_BYTES {
                    return Err(format!(
                        "mcp_frame_too_long: 响应帧 {}B 超单帧上限 {MCP_MAX_FRAME_BYTES}B",
                        line.len()
                    ));
                }
                self.session_bytes += line.len();
                if self.session_bytes > MCP_MAX_SESSION_BYTES {
                    return Err(format!(
                        "mcp_session_budget: 累计 {}B 超会话上限 {MCP_MAX_SESSION_BYTES}B",
                        self.session_bytes
                    ));
                }
                Ok(line)
            }
        }
    }

    fn shutdown(&mut self) {
        // EOF 只在主动 shutdown 后是正常结束：整组 SIGTERM→5s→SIGKILL 回收。
        self.managed.kill_group(Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(id: u64, result: Value) -> Result<String, String> {
        Ok(json!({"jsonrpc":"2.0","id":id,"result":result}).to_string())
    }

    fn init_tools_script(tools: Value) -> Vec<Result<String, String>> {
        vec![
            resp(
                1,
                json!({"serverInfo":{"name":"fake-mcp","version":"1.0"},"protocolVersion":"2024-11-05"}),
            ),
            resp(2, json!({"tools": tools})),
        ]
    }

    #[test]
    fn initialize_and_list_tools() {
        let transport = ScriptedTransport::new(init_tools_script(json!([
            {"name":"query_db","description":"查询","inputSchema":{"type":"object"},
             "annotations":{"readOnlyHint":true}},
            {"name":"send_email","inputSchema":{"type":"object"}}
        ])));
        let mut client = McpClient::new(transport);
        let info = client.initialize().unwrap();
        assert_eq!(info.name, "fake-mcp");
        let tools = client.list_tools().unwrap();
        assert_eq!(tools.len(), 2);
        assert!(tools[0].read_only_hint);
        assert!(!tools[1].read_only_hint, "缺省 = 保守按写");
        // 请求序：initialize → initialized(通知) → tools/list。
        let reqs = client.transport.requests.lock().unwrap();
        assert!(reqs[0].contains("initialize"));
        assert!(reqs[1].contains("notifications/initialized"));
        assert!(reqs[2].contains("tools/list"));
    }

    #[test]
    fn call_tool_timeout_maps_to_unknown() {
        let transport = ScriptedTransport::new(vec![
            resp(1, json!({"serverInfo":{"name":"f","version":"1"}})),
            resp(2, json!({"tools":[]})),
            Err("timeout".into()),
        ]);
        let mut client = McpClient::new(transport);
        client.initialize().unwrap();
        let _ = client.list_tools().unwrap();
        let outcome = client
            .call_tool("send_email", json!({}), Duration::from_millis(10))
            .unwrap();
        assert!(matches!(outcome, McpToolCallOutcome::Timeout));
    }

    #[test]
    fn call_tool_error_and_content() {
        let transport = ScriptedTransport::new(vec![
            resp(1, json!({"serverInfo":{"name":"f","version":"1"}})),
            resp(2, json!({"tools":[]})),
            resp(
                3,
                json!({"content":[{"type":"text","text":"done"}],"isError":false}),
            ),
            resp(
                4,
                json!({"content":[{"type":"text","text":"boom"}],"isError":true}),
            ),
        ]);
        let mut client = McpClient::new(transport);
        client.initialize().unwrap();
        let _ = client.list_tools().unwrap();
        let ok = client
            .call_tool("t", json!({}), Duration::from_secs(5))
            .unwrap();
        match ok {
            McpToolCallOutcome::Ok { text, is_error } => {
                assert_eq!(text, "done");
                assert!(!is_error);
            }
            _ => panic!("应为 Ok"),
        }
        let err = client
            .call_tool("t", json!({}), Duration::from_secs(5))
            .unwrap();
        match err {
            McpToolCallOutcome::Ok { text, is_error } => {
                assert_eq!(text, "boom");
                assert!(is_error);
            }
            _ => panic!("应为 is_error"),
        }
    }

    #[test]
    fn server_rpc_error_surfaces() {
        let transport = ScriptedTransport::new(vec![Err(json!({
            "jsonrpc":"2.0","id":1,
            "error":{"code":-32601,"message":"method not found"}
        })
        .to_string())]);
        let mut client = McpClient::new(transport);
        let err = client.initialize().unwrap_err();
        assert!(err.to_string().contains("method not found"), "{err}");
    }
}
