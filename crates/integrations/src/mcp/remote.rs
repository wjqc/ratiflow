//! 远程 MCP 传输（RDWS 审计后续：远程 MCP 两种传输类型）。
//!
//! - `SseWire`：legacy HTTP+SSE（MCP 2024-11-05 transport）——GET 建立 SSE 通道，
//!   首个 `endpoint` 事件给出 POST 端点；请求经 POST 送出（202 受理），
//!   响应在 SSE 通道上按 JSON-RPC id 匹配返回。
//! - `StreamableHttpWire`：Streamable HTTP（MCP 2025-03-26 transport）——
//!   每个请求 POST 到单一端点，响应为 application/json 或 text/event-stream；
//!   服务端可下发 Mcp-Session-Id，后续请求回带。
//!
//! 治理边界（与 stdio 一致，见 client.rs McpWire）：
//! - 相位语义：on_intent 在请求字节送出前、on_flushed 在送出后（HTTP 侧 =
//!   send 返回/请求体已写）。送出前失败 = 可重试 Transport；送出后失败面归
//!   unknown（由编排器按 effect class 定终态）。
//! - 远程 server 不经本机内核沙箱（无本地进程）；治理依赖注册审批 + URL 冻结
//!   与静态头（如 Authorization）注册时存储。server → client 主动推送流
//!   （Streamable GET 通道）首版不消费，不影响请求/响应。
//! - 帧上限沿用 stdio 会话上限（单帧 4MiB / 会话 32MiB）。

use std::io::{BufRead, BufReader, Read};
use std::time::Duration;

use serde_json::{json, Value};

use super::client::{McpError, McpWire, MCP_MAX_FRAME_BYTES, MCP_MAX_SESSION_BYTES};

/// 静态请求头（注册时冻结；如 Authorization）。值不进日志/读模型。
pub type StaticHeaders = Vec<(String, String)>;

fn agent(read_timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(read_timeout)
        .build()
}

/// ureq/io 错误归一：读超时 → Timeout（unknown 语义边界），其余 Transport。
fn map_io(method: &str, e: std::io::Error) -> McpError {
    if matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ) {
        McpError::Timeout(format!("{method} 读超时"))
    } else {
        McpError::Transport(format!("io: {e}"))
    }
}

fn map_ureq(method: &str, e: ureq::Error) -> McpError {
    match e {
        ureq::Error::Status(code, resp) => {
            let mut body = String::new();
            // 读取错误体片段（限长，避免恶意 server 撑爆内存）。
            let _ = resp.into_reader().take(2048).read_to_string(&mut body);
            McpError::Protocol(format!(
                "{method}: HTTP {code}: {}",
                body.chars().take(200).collect::<String>()
            ))
        }
        ureq::Error::Transport(t) => {
            // 连接/读超时在 transport 分类里：按消息甄别 Timeout。
            let msg = t.to_string();
            if msg.contains("timed out") || msg.contains("timeout") {
                McpError::Timeout(format!("{method} 超时: {msg}"))
            } else {
                McpError::Transport(msg)
            }
        }
    }
}

/// 校验远程 URL：绝对 http/https 且含主机（注册与连接双处校验——连接侧兜底注册后漂移）。
pub fn valid_remote_url(url: &str) -> Result<(), String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| format!("远程 MCP 仅支持 http/https URL（实际 {url:?}）"))?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() {
        return Err("URL 缺少主机名".into());
    }
    Ok(())
}

/// SSE 帧读取：从 reader 读一个事件的 data 载荷（跳过注释/其他字段行）。
/// 返回 None = 流结束（EOF）。
fn read_sse_data<R: BufRead>(reader: &mut R) -> Result<Option<String>, McpError> {
    let mut data = String::new();
    let mut has_data = false;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).map_err(|e| {
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) {
                McpError::Timeout("SSE 流读超时".into())
            } else {
                McpError::Transport(format!("SSE 流读取失败: {e}"))
            }
        })?;
        if n == 0 {
            return Ok(None); // EOF：对端关闭（transport 事件，由调用方按相位定终态）
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if has_data {
                return Ok(Some(data));
            }
            continue; // 帧间空行
        }
        if let Some(payload) = line.strip_prefix("data:") {
            let payload = payload.strip_prefix(' ').unwrap_or(payload);
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(payload);
            has_data = true;
        }
        // event:/id:/注释行：不影响 data 装配。
    }
}

/// JSON-RPC 响应匹配（共用）：错误对象 → Protocol；结果透传。
fn take_result(v: &Value) -> Result<Value, McpError> {
    if let Some(err) = v.get("error") {
        return Err(McpError::Protocol(format!(
            "server 返回错误: {}",
            err["message"].as_str().unwrap_or("unknown")
        )));
    }
    Ok(v.get("result").cloned().unwrap_or(Value::Null))
}

/// legacy HTTP+SSE 传输。
pub struct SseWire {
    agent: ureq::Agent,
    post_url: String,
    reader: BufReader<Box<dyn Read + Send + Sync>>,
    session_bytes: usize,
    headers: StaticHeaders,
}

impl SseWire {
    /// 建立 SSE 通道并取 endpoint。`read_timeout` 作用于通道读间隔。
    pub fn connect(
        url: &str,
        read_timeout: Duration,
        headers: StaticHeaders,
    ) -> Result<Self, McpError> {
        valid_remote_url(url).map_err(McpError::Transport)?;
        let agent = agent(read_timeout);
        let mut req = agent
            .get(url)
            .set("Accept", "text/event-stream")
            .set("Cache-Control", "no-cache");
        for (k, v) in &headers {
            req = req.set(k, v);
        }
        let resp = req.call().map_err(|e| map_ureq("sse connect", e))?;
        if !(200..300).contains(&resp.status()) {
            return Err(McpError::Protocol(format!(
                "sse connect: HTTP {}",
                resp.status()
            )));
        }
        let reader = BufReader::new(resp.into_reader());
        let mut wire = Self {
            agent,
            post_url: String::new(),
            reader,
            session_bytes: 0,
            headers,
        };
        // 首个 endpoint 事件：data 为 POST 端点 URI（MCP 规范 2024-11-05：
        // 纯文本，非 JSON；可相对）。空载荷/心跳行跳过。
        let endpoint = loop {
            let data = read_sse_data(&mut wire.reader)?
                .ok_or_else(|| McpError::Transport("SSE 通道在 endpoint 事件前关闭".into()))?;
            let ep = data.trim().to_string();
            if ep.is_empty() {
                continue;
            }
            break ep;
        };
        wire.post_url = resolve_against(url, &endpoint)?;
        Ok(wire)
    }
}

/// 相对端点拼到 base（MCP 规范：endpoint 可为绝对或相对路径）。
fn resolve_against(base: &str, endpoint: &str) -> Result<String, McpError> {
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        return Ok(endpoint.to_string());
    }
    let idx = base
        .find("://")
        .ok_or_else(|| McpError::Transport("base URL 非法".into()))?;
    let after = &base[idx + 3..];
    let host_end = after.find('/').map(|i| idx + 3 + i).unwrap_or(base.len());
    let origin = &base[..host_end];
    if endpoint.starts_with('/') {
        return Ok(format!("{origin}{endpoint}"));
    }
    // 相对非根路径：挂到 base 的目录段。
    let dir_end = base.rfind('/').unwrap_or(host_end);
    Ok(format!("{}/{endpoint}", &base[..dir_end]))
}

impl McpWire for SseWire {
    fn request(
        &mut self,
        id: u64,
        method: &str,
        params: Value,
        timeout: Duration,
        on_intent: &dyn Fn(&str) -> Result<(), String>,
        on_flushed: &dyn Fn(&str) -> Result<(), String>,
    ) -> Result<Value, McpError> {
        let body = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        on_intent(method).map_err(McpError::Transport)?;
        let mut req = self
            .agent
            .post(&self.post_url)
            .set("Content-Type", "application/json");
        for (k, v) in &self.headers {
            req = req.set(k, v);
        }
        // POST 受理即请求字节送出（响应在 SSE 通道上异步返回）。
        let resp = req.send_json(body).map_err(|e| map_ureq(method, e))?;
        on_flushed(method).map_err(McpError::Transport)?;
        if !(200..300).contains(&resp.status()) {
            return Err(McpError::Protocol(format!(
                "{method}: HTTP {}（POST 未受理）",
                resp.status()
            )));
        }
        let _ = timeout; // 读超时由 agent 的 timeout_read 承载（连接级统一配置）
        loop {
            let data = read_sse_data(&mut self.reader)?.ok_or_else(|| {
                McpError::Transport(format!("{method}: SSE 通道在响应返回前关闭"))
            })?;
            if data.len() > MCP_MAX_FRAME_BYTES {
                return Err(McpError::Transport(format!(
                    "{method}: 响应帧超限（{} bytes）",
                    data.len()
                )));
            }
            self.session_bytes += data.len();
            if self.session_bytes > MCP_MAX_SESSION_BYTES {
                return Err(McpError::Transport("会话字节超限（32MiB）".into()));
            }
            let v: Value = serde_json::from_str(&data)
                .map_err(|e| McpError::Protocol(format!("SSE 载荷非 JSON: {e}")))?;
            if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                return take_result(&v);
            }
            // 通知/他人响应：跳过。
        }
    }

    fn notify(&mut self, method: &str) -> Result<(), McpError> {
        let body = json!({"jsonrpc":"2.0","method":method});
        let mut req = self
            .agent
            .post(&self.post_url)
            .set("Content-Type", "application/json");
        for (k, v) in &self.headers {
            req = req.set(k, v);
        }
        req.send_json(body).map_err(|e| map_ureq(method, e))?;
        Ok(())
    }

    fn close(&mut self) {
        // 丢弃 reader 即关闭底层连接。
        self.reader = BufReader::new(Box::new(std::io::empty()) as Box<dyn Read + Send + Sync>);
    }
}

/// Streamable HTTP 传输（单一端点 POST；json 或 SSE 形态响应）。
pub struct StreamableHttpWire {
    agent: ureq::Agent,
    url: String,
    session_id: Option<String>,
    headers: StaticHeaders,
}

impl StreamableHttpWire {
    pub fn new(url: &str, read_timeout: Duration, headers: StaticHeaders) -> Self {
        Self {
            agent: agent(read_timeout),
            url: url.to_string(),
            session_id: None,
            headers,
        }
    }
}

impl McpWire for StreamableHttpWire {
    fn request(
        &mut self,
        id: u64,
        method: &str,
        params: Value,
        _timeout: Duration,
        on_intent: &dyn Fn(&str) -> Result<(), String>,
        on_flushed: &dyn Fn(&str) -> Result<(), String>,
    ) -> Result<Value, McpError> {
        let body = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        on_intent(method).map_err(McpError::Transport)?;
        let mut req = self
            .agent
            .post(&self.url)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json, text/event-stream");
        for (k, v) in &self.headers {
            req = req.set(k, v);
        }
        if let Some(sid) = &self.session_id {
            req = req.set("Mcp-Session-Id", sid);
        }
        // send 返回 = 请求字节已送出且响应头已到（送出前的连接失败 = 可重试）。
        let resp = req.send_json(body).map_err(|e| map_ureq(method, e))?;
        on_flushed(method).map_err(McpError::Transport)?;
        // 会话头：首个响应携带即采纳（后续请求回带）。
        if self.session_id.is_none() {
            if let Some(sid) = resp.header("Mcp-Session-Id") {
                self.session_id = Some(sid.to_string());
            }
        }
        let status = resp.status();
        if !(200..300).contains(&status) {
            let mut err = String::new();
            let _ = resp.into_reader().take(2048).read_to_string(&mut err);
            return Err(McpError::Protocol(format!(
                "{method}: HTTP {status}: {}",
                err.chars().take(200).collect::<String>()
            )));
        }
        let content_type = resp.header("Content-Type").unwrap_or("").to_string();
        if content_type.starts_with("text/event-stream") {
            let mut reader = BufReader::new(resp.into_reader());
            loop {
                let data = read_sse_data(&mut reader)?.ok_or_else(|| {
                    McpError::Transport(format!("{method}: SSE 响应流在结果前关闭"))
                })?;
                if data.len() > MCP_MAX_FRAME_BYTES {
                    return Err(McpError::Transport(format!(
                        "{method}: 响应帧超限（{} bytes）",
                        data.len()
                    )));
                }
                let v: Value = serde_json::from_str(&data)
                    .map_err(|e| McpError::Protocol(format!("SSE 载荷非 JSON: {e}")))?;
                if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                    return take_result(&v);
                }
                // 响应流内先到的通知/请求：跳过（server→client 推送首版不消费）。
            }
        }
        // application/json（或未标类型按 json 处理）：单响应体。
        let mut body = String::new();
        let reader = resp.into_reader();
        reader
            .take(MCP_MAX_FRAME_BYTES as u64 + 1)
            .read_to_string(&mut body)
            .map_err(|e| map_io(method, e))?;
        if body.len() > MCP_MAX_FRAME_BYTES {
            return Err(McpError::Transport(format!(
                "{method}: 响应体超限（{} bytes）",
                body.len()
            )));
        }
        let v: Value = serde_json::from_str(&body)
            .map_err(|e| McpError::Protocol(format!("响应体非 JSON: {e}")))?;
        if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
            return take_result(&v);
        }
        Err(McpError::Protocol(format!(
            "{method}: 响应 id 不匹配（期待 {id}）"
        )))
    }

    fn notify(&mut self, method: &str) -> Result<(), McpError> {
        let body = json!({"jsonrpc":"2.0","method":method});
        let mut req = self
            .agent
            .post(&self.url)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json, text/event-stream");
        for (k, v) in &self.headers {
            req = req.set(k, v);
        }
        if let Some(sid) = &self.session_id {
            req = req.set("Mcp-Session-Id", sid);
        }
        // 202 Accepted（或 2xx）= 已受理；无响应体语义。
        let resp = req.send_json(body).map_err(|e| map_ureq(method, e))?;
        if !(200..300).contains(&resp.status()) {
            return Err(McpError::Protocol(format!(
                "{method}: 通知 HTTP {}",
                resp.status()
            )));
        }
        Ok(())
    }

    fn close(&mut self) {
        // 无长连接可关（每请求独立）；会话终止 DELETE 首版不发送。
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::client::McpClient;
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    /// 极简 HTTP 测试服务：手写行协议（无外部依赖）。
    struct MiniHttpServer {
        listener: TcpListener,
        addr: std::net::SocketAddr,
    }

    impl MiniHttpServer {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            Self { listener, addr }
        }
        fn url(&self, path: &str) -> String {
            format!("http://{}{}", self.addr, path)
        }
    }

    /// 读一个 HTTP 请求（请求行+头+Content-Length 体），返回 (method, path, body)。
    fn read_request(stream: &mut TcpStream) -> (String, String, Value) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("").to_string();
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; content_length];
        if content_length > 0 {
            reader.read_exact(&mut body).unwrap();
        }
        (
            method,
            path,
            serde_json::from_slice(&body).unwrap_or(Value::Null),
        )
    }

    fn write_response(stream: &mut TcpStream, status: &str, content_type: &str, body: &str) {
        let resp = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(resp.as_bytes()).unwrap();
        stream.flush().unwrap();
    }

    fn write_sse(stream: &mut TcpStream, frames: &[String]) {
        let mut out = String::new();
        for f in frames {
            out.push_str(&format!("event: message\ndata: {f}\n\n"));
        }
        let resp = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n";
        stream.write_all(resp.as_bytes()).unwrap();
        for f in frames {
            let chunk = format!("event: message\ndata: {f}\n\n");
            stream
                .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                .unwrap();
            stream.write_all(chunk.as_bytes()).unwrap();
            stream.write_all(b"\r\n").unwrap();
        }
        stream.flush().unwrap();
    }

    /// Streamable HTTP happy 链：initialize/initialized/tools/list/tools/call（json 响应）。
    #[test]
    fn streamable_http_full_chain_json_responses() {
        let server = MiniHttpServer::start();
        let url = server.url("/mcp");
        std::thread::spawn(move || {
            for stream in server.listener.incoming() {
                let mut stream = stream.unwrap();
                let (method, _path, body) = read_request(&mut stream);
                let id = body["id"].as_u64().unwrap_or(0);
                let m = body["method"].as_str().unwrap_or("");
                match (method.as_str(), m) {
                    (_, "initialize") => write_response(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        &json!({"jsonrpc":"2.0","id":id,"result":{
                            "serverInfo":{"name":"remote-fake","version":"2"},
                            "protocolVersion":"2025-03-26"}}).to_string(),
                    ),
                    (_, "notifications/initialized") => {
                        write_response(&mut stream, "202 Accepted", "application/json", "")
                    }
                    (_, "tools/list") => write_response(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        &json!({"jsonrpc":"2.0","id":id,"result":{"tools":[
                            {"name":"query","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}}]}}).to_string(),
                    ),
                    (_, "tools/call") => write_response(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        &json!({"jsonrpc":"2.0","id":id,"result":{
                            "content":[{"type":"text","text":"remote-done"}],"isError":false}}).to_string(),
                    ),
                    _ => write_response(&mut stream, "404 Not Found", "application/json", "{}"),
                }
            }
        });
        let mut client = McpClient::new(StreamableHttpWire::new(
            &url,
            Duration::from_secs(5),
            vec![],
        ));
        let info = client.initialize().unwrap();
        assert_eq!(info.name, "remote-fake");
        let tools = client.list_tools().unwrap();
        assert_eq!(tools.len(), 1);
        assert!(tools[0].read_only_hint);
        let out = client
            .call_tool("query", json!({}), Duration::from_secs(5))
            .unwrap();
        match out {
            crate::mcp::McpToolCallOutcome::Ok { text, is_error } => {
                assert_eq!(text, "remote-done");
                assert!(!is_error);
            }
            _ => panic!("应为 Ok"),
        }
    }

    /// Streamable HTTP SSE 形态响应 + 相位语义（intent 中止 → 零请求送出）。
    #[test]
    fn streamable_http_sse_response_and_phase_semantics() {
        let server = MiniHttpServer::start();
        let url = server.url("/mcp");
        let requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let req_log = requests.clone();
        std::thread::spawn(move || {
            for stream in server.listener.incoming() {
                let mut stream = stream.unwrap();
                let (_method, _path, body) = read_request(&mut stream);
                req_log
                    .lock()
                    .unwrap()
                    .push(body["method"].as_str().unwrap_or("").to_string());
                let id = body["id"].as_u64().unwrap_or(0);
                match body["method"].as_str().unwrap_or("") {
                    "initialize" => write_sse(
                        &mut stream,
                        &[json!({"jsonrpc":"2.0","id":id,"result":{
                            "serverInfo":{"name":"sse-fake","version":"1"},
                            "protocolVersion":"2025-03-26"}})
                        .to_string()],
                    ),
                    "notifications/initialized" => {
                        write_response(&mut stream, "202 Accepted", "application/json", "")
                    }
                    "tools/list" => write_sse(
                        &mut stream,
                        &[
                            json!({"jsonrpc":"2.0","method":"notifications/progress","params":{}})
                                .to_string(),
                            json!({"jsonrpc":"2.0","id":id,"result":{"tools":[
                            {"name":"send","inputSchema":{"type":"object"}}]}})
                            .to_string(),
                        ],
                    ),
                    _ => write_response(&mut stream, "404 Not Found", "application/json", "{}"),
                }
            }
        });
        let mut client = McpClient::new(StreamableHttpWire::new(
            &url,
            Duration::from_secs(5),
            vec![],
        ));
        assert!(client.initialize().is_ok());
        // SSE 形态 + 前置通知跳过。
        let tools = client.list_tools().unwrap();
        assert_eq!(tools.len(), 1);
        assert!(!tools[0].read_only_hint, "缺省保守按写");
        // 相位：intent 中止（持久化后、送出前崩溃模拟）→ Transport 且零请求。
        let before = requests.lock().unwrap().len();
        let err = client
            .call_tool_phased(
                "send",
                json!({}),
                Duration::from_secs(5),
                &|_m| Err("crash after intent persist".into()),
                &|_m| Ok(()),
            )
            .unwrap_err();
        assert!(matches!(err, McpError::Transport(_)), "{err}");
        assert_eq!(
            requests.lock().unwrap().len(),
            before,
            "intent 中止 → 请求未送出"
        );
    }

    /// Streamable HTTP 超时 → Timeout（unknown 语义边界，非 failed）。
    #[test]
    fn streamable_http_timeout_maps_to_timeout() {
        let server = MiniHttpServer::start();
        let url = server.url("/mcp");
        std::thread::spawn(move || {
            for stream in server.listener.incoming() {
                let mut stream = stream.unwrap();
                let (_m, _p, body) = read_request(&mut stream);
                let id = body["id"].as_u64().unwrap_or(0);
                if body["method"].as_str() == Some("initialize") {
                    write_response(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        &json!({"jsonrpc":"2.0","id":id,"result":{
                            "serverInfo":{"name":"slow","version":"1"},
                            "protocolVersion":"2025-03-26"}})
                        .to_string(),
                    );
                } else if body["method"].as_str() == Some("notifications/initialized") {
                    write_response(&mut stream, "202 Accepted", "application/json", "");
                } else {
                    std::thread::sleep(Duration::from_secs(3)); // 超过 500ms 读超时
                }
            }
        });
        let mut client = McpClient::new(StreamableHttpWire::new(
            &url,
            Duration::from_millis(500),
            vec![],
        ));
        assert!(client.initialize().is_ok());
        let out = client
            .call_tool("send", json!({}), Duration::from_millis(500))
            .unwrap();
        assert!(matches!(out, crate::mcp::McpToolCallOutcome::Timeout));
    }

    /// legacy SSE：GET 通道 + endpoint 事件 + POST 受理、响应按 id 回流。
    #[test]
    fn sse_legacy_transport_full_chain() {
        let server = MiniHttpServer::start();
        let base = server.url("/sse");
        // POST 端点 → SSE 通道帧：用通道传递（单线程串行连接也能成立——
        // 本测试服务按连接顺序处理：先 GET（保持打开），POST 到来时把响应
        // 写进既有 GET 流）。
        struct Shared {
            sse_stream: Option<TcpStream>,
        }
        let shared = Arc::new(Mutex::new(Shared { sse_stream: None }));
        let sh = shared.clone();
        std::thread::spawn(move || {
            for stream in server.listener.incoming() {
                let mut stream = stream.unwrap();
                let (method, path, body) = read_request(&mut stream);
                if method == "GET" {
                    // SSE 通道：先送 endpoint，保留连接等后续写入。
                    let mut s = stream.try_clone().unwrap();
                    let resp = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n";
                    s.write_all(resp.as_bytes()).unwrap();
                    let chunk = "event: endpoint\ndata: /messages\n\n";
                    s.write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                        .unwrap();
                    s.write_all(chunk.as_bytes()).unwrap();
                    s.write_all(b"\r\n").unwrap();
                    s.flush().unwrap();
                    sh.lock().unwrap().sse_stream = Some(s);
                } else if path == "/messages" {
                    let id = body["id"].as_u64();
                    match (body["method"].as_str().unwrap_or(""), id) {
                        ("notifications/initialized", _) => {
                            write_response(&mut stream, "202 Accepted", "application/json", "");
                        }
                        (m, Some(id)) => {
                            write_response(&mut stream, "202 Accepted", "application/json", "");
                            let result = match m {
                                "initialize" => {
                                    json!({"serverInfo":{"name":"legacy-sse","version":"1"},"protocolVersion":"2024-11-05"})
                                }
                                "tools/list" => {
                                    json!({"tools":[{"name":"q","inputSchema":{"type":"object"}}]})
                                }
                                _ => json!({"content":[{"type":"text","text":"sse-done"}]}),
                            };
                            let frame =
                                json!({"jsonrpc":"2.0","id":id,"result":result}).to_string();
                            if let Some(sse) = sh.lock().unwrap().sse_stream.as_mut() {
                                let chunk = format!("event: message\ndata: {frame}\n\n");
                                sse.write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                                    .unwrap();
                                sse.write_all(chunk.as_bytes()).unwrap();
                                sse.write_all(b"\r\n").unwrap();
                                sse.flush().unwrap();
                            }
                        }
                        _ => {
                            write_response(&mut stream, "400 Bad Request", "application/json", "{}")
                        }
                    }
                }
            }
        });
        let mut client = McpClient::new(
            SseWire::connect(
                &base,
                Duration::from_secs(5),
                vec![("Authorization".into(), "Bearer t".into())],
            )
            .unwrap(),
        );
        let info = client.initialize().unwrap();
        assert_eq!(info.name, "legacy-sse");
        assert_eq!(client.list_tools().unwrap().len(), 1);
        let out = client
            .call_tool("q", json!({}), Duration::from_secs(5))
            .unwrap();
        match out {
            crate::mcp::McpToolCallOutcome::Ok { text, .. } => assert_eq!(text, "sse-done"),
            _ => panic!("应为 Ok"),
        }
    }

    /// URL 校验负例：非 http/https 拒绝。
    #[test]
    fn remote_url_validation_rejects_non_http() {
        assert!(valid_remote_url("file:///etc/passwd").is_err());
        assert!(valid_remote_url("javascript:alert(1)").is_err());
        assert!(valid_remote_url("not a url").is_err());
        assert!(valid_remote_url("https://example.com/mcp").is_ok());
    }
}
