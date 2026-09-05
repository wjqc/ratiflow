//! 流式模型客户端层（ADR-033 M2）：
//! - [`StreamingModelProvider`]：async/event-sink 接口；adapter 只做线协议转换，
//!   事件经 [`StreamSink`] 推送，Agent 侧由 TurnAggregator 聚合校验（不在本 crate）。
//! - [`CancelToken`] 驱动真取消：select 放弃 future 即关闭连接 socket。
//! - [`ModelHttp`] 的 OpenAI 兼容 SSE 实现（reqwest + rustls；chat_completions 流式形状）。
//! - [`FakeModel`] 流式桥：既有脚本响应按 delta 拆分发射，同一脚本可测双路径。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::cancel::CancelToken;
use crate::sse::{pump_sse, SsePumpError};
use crate::{
    model::{
        build_chat_body, cached_tokens_from_usage, ChatMessage, CompletionRequest,
        CompletionResponse, ProviderToolCall,
    },
    FakeModel, ModelHttp,
};

/// 单帧上限：一个 SSE frame（含 data JSON）超过即协议违规。
pub const SSE_MAX_FRAME_BYTES: usize = 1 << 20;
/// 单次流累计上限（计量 + 滥用防护；正常一轮远小于此）。
pub const SSE_MAX_TOTAL_BYTES: usize = 32 << 20;
/// 读空闲 watchdog：Provider 超此时长未发任何字节按流中断处理。
pub const SSE_IDLE: Duration = Duration::from_secs(60);

/// 流式用量（token 计量；Provider 未给时由 adapter 估算并如实标注）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamUsage {
    pub input: i64,
    pub cached_input: i64,
    pub output: i64,
    pub reasoning_output: i64,
    /// true = usage 来自 Provider 真实帧；false = chars/4 估算。
    pub measured: bool,
}

/// 事件汇：Provider adapter 逐事件推送；Gateway 实现侧负责 UI 转发与聚合校验。
/// 约定（方案 §5 M2）：on_reasoning_delta 只用于计量——reasoning plaintext
/// 不向 renderer 推送、不进普通 rollout。
pub trait StreamSink: Send + Sync {
    fn on_started(&self, provider_request_id: &str);
    fn on_text_delta(&self, text: &str);
    fn on_reasoning_delta(&self, text: &str);
    fn on_tool_call_delta(&self, call_id: &str, name: &str, arguments_delta: &str);
    fn on_usage(&self, usage: StreamUsage);
    fn on_completed(&self, finish_reason: &str);
}

/// 空实现（不需要事件时的占位）。
pub struct NoopSink;
impl StreamSink for NoopSink {
    fn on_started(&self, _: &str) {}
    fn on_text_delta(&self, _: &str) {}
    fn on_reasoning_delta(&self, _: &str) {}
    fn on_tool_call_delta(&self, _: &str, _: &str, _: &str) {}
    fn on_usage(&self, _: StreamUsage) {}
    fn on_completed(&self, _: &str) {}
}

/// 流式一轮的 future（对象安全；借用 provider 的存活期。Gateway 负责在
/// 运行时句柄上驱动 + select 取消）。
pub type StreamFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CompletionResponse, String>> + Send + 'a>>;

pub trait StreamingModelProvider: Send + Sync {
    /// 流式完成一轮模型调用。返回值是 adapter 侧聚合（便利用途）；
    /// 权威轮次以事件聚合器产出为准——同一响应不可双重消费。
    fn stream_complete<'a>(
        &'a self,
        req: CompletionRequest,
        sink: Arc<dyn StreamSink>,
        cancel: CancelToken,
    ) -> StreamFuture<'a>;
}

/// adapter 侧聚合器（构造返回的 CompletionResponse；权威聚合在 Gateway）。
#[derive(Default)]
struct StreamAcc {
    request_id: String,
    content: String,
    reasoning_bytes: u64,
    calls: Vec<(String, String, String)>, // (id, name, arguments)
    /// index → id：后续分片常省略 id，仅靠 index 关联（OpenAI/DeepSeek/GLM 形状）。
    index_ids: std::collections::HashMap<u64, String>,
    usage: StreamUsage,
    finish_reason: String,
    saw_finish_reason: bool,
    saw_done: bool,
    started_emitted: bool,
}

fn dispatch_chunk(v: &Value, sink: &dyn StreamSink, acc: &mut StreamAcc) {
    if !acc.started_emitted {
        acc.started_emitted = true;
        acc.request_id = v["id"].as_str().unwrap_or_default().to_string();
        sink.on_started(&acc.request_id.clone());
    }
    if let Some(usage) = v.get("usage") {
        if usage.is_object() && !usage.as_object().unwrap().is_empty() {
            acc.usage = StreamUsage {
                input: usage["prompt_tokens"].as_i64().unwrap_or(0),
                cached_input: cached_tokens_from_usage(usage),
                output: usage["completion_tokens"].as_i64().unwrap_or(0),
                reasoning_output: usage["completion_tokens_details"]["reasoning_tokens"]
                    .as_i64()
                    .unwrap_or(0),
                measured: true,
            };
        }
    }
    let choice = &v["choices"][0];
    if let Some(finish) = choice["finish_reason"].as_str() {
        if !finish.is_empty() {
            acc.finish_reason = finish.to_string();
            acc.saw_finish_reason = true;
        }
    }
    let delta = &choice["delta"];
    if let Some(text) = delta["content"].as_str() {
        if !text.is_empty() {
            acc.content.push_str(text);
            sink.on_text_delta(text);
        }
    }
    if let Some(r) = delta["reasoning_content"].as_str() {
        // 只计量：reasoning plaintext 不出 adapter（不向 renderer 推送，§5 M2）。
        acc.reasoning_bytes += r.len() as u64;
        sink.on_reasoning_delta(r);
    }
    if let Some(calls) = delta["tool_calls"].as_array() {
        for (i, tc) in calls.iter().enumerate() {
            let index = tc["index"].as_u64().unwrap_or(i as u64);
            let id = match tc["id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(String::from)
            {
                Some(id) => {
                    acc.index_ids.insert(index, id.clone());
                    id
                }
                None => acc
                    .index_ids
                    .get(&index)
                    .cloned()
                    .unwrap_or_else(|| format!("call_{index}")),
            };
            let name = tc["function"]["name"].as_str().unwrap_or_default();
            let args = tc["function"]["arguments"].as_str().unwrap_or_default();
            let entry = acc
                .calls
                .iter_mut()
                .find(|(cid, _, _)| cid == &id)
                .map(|(_, n, a)| (n, a));
            match entry {
                Some((n, a)) => {
                    if !name.is_empty() && n.is_empty() {
                        *n = name.to_string();
                    }
                    if !args.is_empty() {
                        a.push_str(args);
                        sink.on_tool_call_delta(&id, n, args);
                    }
                }
                None => {
                    acc.calls
                        .push((id.clone(), name.to_string(), args.to_string()));
                    if !args.is_empty() || !name.is_empty() {
                        sink.on_tool_call_delta(&id, name, args);
                    }
                }
            }
        }
    }
}

fn estimate_tokens_from(chars: usize) -> i64 {
    (chars / 4).max(1) as i64
}

/// 从 adapter 聚合构造 CompletionResponse（usage 缺失时 chars/4 估算，budget 仍受约束）。
fn acc_to_response(acc: &StreamAcc, req: &CompletionRequest) -> CompletionResponse {
    let (tokens_in, tokens_out) = if acc.usage.measured {
        (acc.usage.input, acc.usage.output)
    } else {
        let in_chars =
            req.system_prompt.len() + req.messages.iter().map(|m| m.content.len()).sum::<usize>();
        let out_chars = acc.content.len() + acc.calls.iter().map(|c| c.2.len()).sum::<usize>();
        (
            estimate_tokens_from(in_chars) + 512,
            estimate_tokens_from(out_chars),
        )
    };
    CompletionResponse {
        content: acc.content.clone(),
        tokens_in,
        tokens_out,
        finish_reason: acc.finish_reason.clone(),
        tool_calls: acc
            .calls
            .iter()
            .map(|(id, name, args)| ProviderToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: args.clone(),
            })
            .collect(),
        cached_tokens: acc.usage.cached_input,
        reasoning_tokens: acc.usage.reasoning_output,
        reasoning_state_encrypted: None,
        reasoning_state_status: "none".into(),
    }
}

impl ModelHttp {
    async fn send_stream(&self, body: Value) -> Result<reqwest::Response, String> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| format!("model_unavailable: client init: {e}"))?;
        let send = || {
            client
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(&body)
                .timeout(Duration::from_secs(300))
                .send()
        };
        // 429 瞬态退避重试一次（与非流式路径语义一致；重试挂在错误分支）。
        let first = send().await;
        let response = match first {
            Ok(r) if r.status().as_u16() == 429 => {
                tokio::time::sleep(Duration::from_secs(2)).await;
                send().await
            }
            other => other,
        };
        let response = response.map_err(|e| {
            let text = e.to_string();
            if text.contains("timed out") || text.contains("TimedOut") {
                format!("model_timeout: {text}")
            } else {
                format!("model_unavailable: {text}")
            }
        })?;
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            let detail: String = detail.chars().take(200).collect();
            if status.as_u16() == 429 {
                return Err("model_rate_limited".into());
            }
            return Err(if detail.is_empty() {
                format!("model_unavailable: HTTP {}", status.as_u16())
            } else {
                format!("model_unavailable: HTTP {}: {detail}", status.as_u16())
            });
        }
        Ok(response)
    }
}

impl StreamingModelProvider for ModelHttp {
    fn stream_complete(
        &self,
        req: CompletionRequest,
        sink: Arc<dyn StreamSink>,
        cancel: CancelToken,
    ) -> StreamFuture<'_> {
        Box::pin(async move {
            if self.api_key.is_empty() {
                return Err("model_unavailable: api key missing".into());
            }
            let body = build_chat_body(&req, &self.model, true);
            let response = {
                let cancel = cancel.clone();
                tokio::select! {
                    _ = cancel.cancelled() => return Err("model_cancelled".into()),
                    r = self.send_stream(body) => r,
                }
            }?;

            let mut acc = StreamAcc::default();
            let sink_for_pump = sink.clone();
            let pump = pump_sse(
                response,
                &cancel,
                SSE_MAX_FRAME_BYTES,
                SSE_MAX_TOTAL_BYTES,
                SSE_IDLE,
                &mut |frame| {
                    if frame.data.trim() == "[DONE]" {
                        acc.saw_done = true;
                        return Ok(());
                    }
                    if frame.data.trim().is_empty() {
                        return Ok(());
                    }
                    let parsed: Value = match serde_json::from_str(&frame.data) {
                        Ok(v) => v,
                        Err(e) => {
                            return Err(SsePumpError::Violation(format!(
                                "chunk JSON 解析失败: {e}"
                            )));
                        }
                    };
                    dispatch_chunk(&parsed, sink_for_pump.as_ref(), &mut acc);
                    Ok(())
                },
            )
            .await;
            match pump {
                Ok(_) => {}
                Err(e) => return Err(e.to_error_string()),
            }
            // 流终态判定：finish_reason 帧是语义完成；[DONE] 是传输哨兵（部分代理会丢弃）。
            // 两者皆无 = 中断且未收到完成帧 → 轮次 incomplete，不得制造假终态（§4.3）。
            if !acc.saw_finish_reason && !acc.saw_done {
                return Err(
                    "model_stream_interrupted: EOF 无完成帧（无 finish_reason/[DONE]）".into(),
                );
            }
            sink.on_usage(acc.usage);
            sink.on_completed(&acc.finish_reason.clone());
            if acc.content.trim().is_empty() && acc.calls.is_empty() {
                let finish = if acc.finish_reason.is_empty() {
                    "unknown"
                } else {
                    &acc.finish_reason
                };
                let hint = if finish == "length" {
                    "输出 token 预算耗尽：请换用支持更长输出的模型"
                } else {
                    "请重试或更换模型"
                };
                return Err(format!(
                    "model_empty_output: 模型未返回正文（finish_reason={finish}）；{hint}"
                ));
            }
            Ok(acc_to_response(&acc, &req))
        })
    }
}

impl FakeModel {
    /// 声明流式能力：capability 携带 streamText/streamToolArguments=true（manual 来源），
    /// Gateway 据此选择流式路径。仅供测试路径。
    pub fn enable_streaming(&self) {
        self.streaming
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// 首 delta 前挂起直到取消（取消时序测试：stub 保持连接 N 秒的进程内等价物）。
    pub fn hold_stream_until_cancel(&self) {
        self.hold_until_cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// 每 delta 之后的挂起点数量：>0 时在第 n 个 delta 后挂起等待取消
    /// （测"流中段取消"）。0 = 不挂起。
    pub fn stall_after_delta(&self, n: usize, millis: u64) {
        *self.stall_after_delta.lock().unwrap() = Some((n, millis));
    }
}

impl StreamingModelProvider for FakeModel {
    fn stream_complete(
        &self,
        req: CompletionRequest,
        sink: Arc<dyn StreamSink>,
        cancel: CancelToken,
    ) -> StreamFuture<'_> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(req.clone());
            if let Some(err) = self.errors.lock().unwrap().pop_front() {
                return Err(err);
            }
            let mut resp: CompletionResponse = self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "model_unavailable: script exhausted".to_string())?;
            // 与 sync 桥一致：脚本 action JSON → 原生 tool_calls（final 动作 = 正文答复）。
            let n = self
                .call_counter
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if req.tools_json.is_some() {
                if let Ok(list) = serde_json::from_str::<Vec<serde_json::Value>>(&resp.content) {
                    resp.tool_calls = list
                        .iter()
                        .enumerate()
                        .map(|(i, v)| ProviderToolCall {
                            id: format!("call_{n}_{i}"),
                            name: v["action"].as_str().unwrap_or_default().into(),
                            arguments: v["arguments"].to_string(),
                        })
                        .collect();
                    resp.content = String::new();
                    resp.finish_reason = "tool_calls".into();
                } else if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp.content) {
                    let action = v["action"].as_str().unwrap_or_default().to_string();
                    if action == "final" {
                        resp.content = v["summary"].as_str().unwrap_or_default().to_string();
                    } else {
                        resp.tool_calls = vec![ProviderToolCall {
                            id: format!("call_{n}"),
                            name: action,
                            arguments: v["arguments"].to_string(),
                        }];
                        resp.content = String::new();
                        resp.finish_reason = "tool_calls".into();
                    }
                }
            }
            // 取消挂起点：模拟 Provider 长时间不吐 token 的连接保持。
            if self
                .hold_until_cancel
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                cancel.cancelled().await;
                return Err("model_cancelled".into());
            }
            sink.on_started(&format!("req_fake_{n}"));
            // M4 测试：reasoning_content delta（明文只应到网关加密层）。
            let reasoning = self.reasoning_text.lock().unwrap().clone();
            sink.on_usage(StreamUsage {
                input: resp.tokens_in,
                output: resp.tokens_out,
                cached_input: resp.cached_tokens,
                reasoning_output: reasoning.as_ref().map(|r| r.len() as i64).unwrap_or(0),
                measured: true,
            });
            if let Some(rt) = reasoning {
                for piece in rt.split_inclusive('。') {
                    sink.on_reasoning_delta(piece);
                }
            }
            let stall = *self.stall_after_delta.lock().unwrap();
            let mut emitted = 0usize;
            // 正文 delta 拆分（chars 边界安全）。
            let text: Vec<char> = resp.content.chars().collect();
            for chunk in text.chunks(4) {
                let piece: String = chunk.iter().collect();
                sink.on_text_delta(&piece);
                emitted += 1;
                if let Some((after, ms)) = &stall {
                    if emitted == *after {
                        tokio::time::sleep(Duration::from_millis(*ms)).await;
                        if cancel.is_cancelled() {
                            return Err("model_cancelled".into());
                        }
                    }
                }
            }
            // 工具参数 delta：参数 JSON 一分为二（半个 JSON argument 聚合测试点）。
            for tc in &resp.tool_calls {
                let chars: Vec<char> = tc.arguments.chars().collect();
                let (a, b) = if chars.len() > 1 {
                    chars.split_at(chars.len() / 2)
                } else {
                    (chars.as_slice(), [].as_slice())
                };
                let sa: String = a.iter().collect();
                let sb: String = b.iter().collect();
                sink.on_tool_call_delta(&tc.id, &tc.name, &sa);
                if !sb.is_empty() {
                    sink.on_tool_call_delta(&tc.id, &tc.name, &sb);
                }
            }
            sink.on_completed(&resp.finish_reason.clone());
            Ok(resp)
        })
    }
}

/// 流式请求建立的墙钟计时（首 token 延迟计量辅助）。
#[derive(Debug, Clone, Copy)]
pub struct TurnTimer {
    started: Instant,
    first_delta: Option<Instant>,
}

impl TurnTimer {
    pub fn start() -> Self {
        Self {
            started: Instant::now(),
            first_delta: None,
        }
    }

    pub fn note_delta(&mut self) {
        if self.first_delta.is_none() {
            self.first_delta = Some(Instant::now());
        }
    }

    /// 首 token 延迟（ms）；无 delta 时为 None。
    pub fn ttft_ms(&self) -> Option<i64> {
        self.first_delta
            .map(|t| t.duration_since(self.started).as_millis() as i64)
    }

    pub fn elapsed_ms(&self) -> i64 {
        self.started.elapsed().as_millis() as i64
    }
}

/// ChatMessage 便捷构造（测试用）。
pub fn user_message(content: &str) -> ChatMessage {
    ChatMessage {
        role: "user".into(),
        content: content.into(),
        ..ChatMessage::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex as StdMutex;
    use std::time::{Duration, Instant};

    fn req() -> CompletionRequest {
        CompletionRequest {
            model: "m".into(),
            system_prompt: String::new(),
            messages: vec![user_message("hi")],
            max_tokens: 128,
            response_schema: None,
            tools_json: None,
        }
    }

    fn req_with_tools() -> CompletionRequest {
        CompletionRequest {
            tools_json: Some(r#"[{"type":"function","function":{"name":"read_file"}}"#.into()),
            ..req()
        }
    }

    /// 流式桩：脚本中每个元素服务一个连接（429 重试会建新连接）。
    /// Sse::hold=true 时写完挂住直到对端关闭（读取返回 0 → 记录关闭），
    /// 覆盖"取消 1 秒内 socket 关闭"断言。
    fn serve_stream(
        conns: Vec<ConnStep>,
    ) -> (String, Arc<AtomicBool>, Arc<StdMutex<Option<Instant>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let peer_closed = Arc::new(AtomicBool::new(false));
        let closed_at = Arc::new(StdMutex::new(None));
        let pc = peer_closed.clone();
        let ca = closed_at.clone();
        std::thread::spawn(move || {
            for conn in &conns {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                }
                let mut socket = stream;
                match conn {
                    ConnStep::Raw(bytes) => {
                        if socket.write_all(bytes).is_err() {
                            return;
                        }
                        let _ = socket.flush();
                    }
                    ConnStep::Sse { parts, hold } => {
                        if socket.write_all(sse_headers()).is_err() {
                            return;
                        }
                        for p in parts {
                            if socket.write_all(p).is_err() {
                                return;
                            }
                            let _ = socket.flush();
                        }
                        if !*hold {
                            let _ = socket.flush();
                            continue;
                        }
                        let mut buf = [0u8; 512];
                        loop {
                            match reader.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(_) => {}
                            }
                        }
                        *ca.lock().unwrap() = Some(Instant::now());
                        pc.store(true, Ordering::SeqCst);
                        return;
                    }
                }
            }
        });
        (format!("http://{addr}"), peer_closed, closed_at)
    }

    enum ConnStep {
        Raw(&'static [u8]),
        Sse {
            parts: Vec<&'static [u8]>,
            hold: bool,
        },
    }

    fn sse_headers() -> &'static [u8] {
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
    }

    fn stub_provider(base: String) -> ModelHttp {
        ModelHttp {
            name_value: "stub".into(),
            base_url: base,
            api_key: "k".into(),
            model: "m".into(),
        }
    }

    /// 真 SSE 传输路径：分片边界 + 半个 UTF-8 码点跨 chunk → 聚合正文完整。
    #[tokio::test]
    async fn sse_deltas_across_chunk_and_utf8_boundaries() {
        // "你好"：你在第一段结尾被切成 2 字节 + 第二段补 1 字节。
        let part1: &'static [u8] =
            b"data: {\"id\":\"req-1\",\"choices\":[{\"delta\":{\"content\":\"\xE4\xBD";
        let part2: &'static [u8] = b"\xA0\xE5\xA5\xBD\"}}]}\n\n";
        let done: &'static [u8] = b"data: [DONE]\n\n";
        let (base, _, _) = serve_stream(vec![ConnStep::Sse {
            parts: vec![part1, part2, done],
            hold: false,
        }]);
        let resp = stub_provider(base)
            .stream_complete(req(), Arc::new(NoopSink), CancelToken::new())
            .await
            .unwrap();
        assert_eq!(resp.content, "你好");
    }

    /// EOF 无完成帧（无 finish_reason/[DONE]）：中断，不制造假终态。
    #[tokio::test]
    async fn sse_eof_without_done_is_interrupted() {
        let body: &'static [u8] =
            b"data: {\"choices\":[{\"delta\":{\"content\":\"\xE5\x8D\x8A\xE6\x88\xAA\"}}]}\n\n";
        let (base, _, _) = serve_stream(vec![ConnStep::Sse {
            parts: vec![body],
            hold: false,
        }]);
        let err = stub_provider(base)
            .stream_complete(req(), Arc::new(NoopSink), CancelToken::new())
            .await
            .unwrap_err();
        assert!(err.starts_with("model_stream_interrupted"), "{err}");
    }

    /// 半个 JSON 参数：工具名/参数分片跨 chunk → 聚合出完整 arguments。
    #[tokio::test]
    async fn sse_tool_arguments_assembled_from_split_deltas() {
        let c1: &'static [u8] = b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_9\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n";
        let c2: &'static [u8] = b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"a.md\\\"}\"}}]}}]}\n\n";
        let c3: &'static [u8] = b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n";
        let (base, _, _) = serve_stream(vec![ConnStep::Sse {
            parts: vec![c1, c2, c3],
            hold: false,
        }]);
        let resp = stub_provider(base)
            .stream_complete(req_with_tools(), Arc::new(NoopSink), CancelToken::new())
            .await
            .unwrap();
        assert_eq!(resp.finish_reason, "tool_calls");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].id, "call_9");
        assert_eq!(resp.tool_calls[0].name, "read_file");
        assert_eq!(resp.tool_calls[0].arguments, r#"{"path":"a.md"}"#);
    }

    /// 429 在首个 token 前：退避重试一次后成功（与非流式语义一致）。
    #[tokio::test]
    async fn sse_429_before_token_retries_then_streams() {
        let ok: &'static [u8] =
            b"data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n";
        let (base, _, _) = serve_stream(vec![
            ConnStep::Raw(
                b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            ),
            ConnStep::Sse {
                parts: vec![ok],
                hold: false,
            },
        ]);
        let resp = stub_provider(base)
            .stream_complete(req(), Arc::new(NoopSink), CancelToken::new())
            .await
            .unwrap();
        assert_eq!(resp.content, "ok");
    }

    /// 取消断言（§5 M2）：stub 保持连接挂住，取消后 1 秒内 socket 关闭、
    /// 调用返回 model_cancelled。
    #[tokio::test]
    async fn cancel_closes_socket_within_1s_and_returns_cancelled() {
        let hold: &'static [u8] = b": hold\n\n";
        let (base, peer_closed, _) = serve_stream(vec![ConnStep::Sse {
            parts: vec![hold],
            hold: true,
        }]);
        let provider = stub_provider(base);
        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            c2.cancel();
        });
        let started = Instant::now();
        let err = provider
            .stream_complete(req(), Arc::new(NoopSink), cancel)
            .await
            .unwrap_err();
        let elapsed = started.elapsed();
        assert_eq!(err, "model_cancelled");
        assert!(elapsed < Duration::from_secs(1), "取消须 <1s：{elapsed:?}");
        // 对端（stub）应观察到连接关闭。
        for _ in 0..50 {
            if peer_closed.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            peer_closed.load(Ordering::SeqCst),
            "stub 应观察到 socket 关闭"
        );
    }

    /// 首 token 延迟（本地 stub，不含 Provider 网络）：<300ms。
    #[tokio::test]
    async fn first_token_latency_under_300ms_on_local_stub() {
        let body: &'static [u8] =
            b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\ndata: [DONE]\n\n";
        let (base, _, _) = serve_stream(vec![ConnStep::Sse {
            parts: vec![body],
            hold: false,
        }]);
        let sink = Arc::new(RecordingSink::default());
        let started = Instant::now();
        stub_provider(base)
            .stream_complete(req(), sink.clone(), CancelToken::new())
            .await
            .unwrap();
        let ttft = sink
            .first_delta_at
            .lock()
            .unwrap()
            .map(|t| t.duration_since(started))
            .expect("应记录首 token 时刻");
        assert!(ttft < Duration::from_millis(300), "首 token {ttft:?}");
        assert_eq!(*sink.text.lock().unwrap(), "hello");
    }

    /// sink 事件序：started → text* → usage → completed（终态恰好一条）。
    #[tokio::test]
    async fn sink_event_order_and_single_completed() {
        let body: &'static [u8] = b"data: {\"id\":\"rq\",\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"b\"}}]}\n\ndata: [DONE]\n\n";
        let (base, _, _) = serve_stream(vec![ConnStep::Sse {
            parts: vec![body],
            hold: false,
        }]);
        let sink = Arc::new(RecordingSink::default());
        stub_provider(base)
            .stream_complete(req(), sink.clone(), CancelToken::new())
            .await
            .unwrap();
        let events = sink.events.lock().unwrap().clone();
        assert_eq!(events[0], "started:rq");
        assert_eq!(events[1], "text");
        assert_eq!(events[2], "text");
        assert_eq!(events[3], "usage:measured=false");
        assert_eq!(*events.last().unwrap(), "completed:");
    }

    /// usage 缺失时 chars/4 估算（budget 计量不失效）。
    #[tokio::test]
    async fn usage_estimated_when_provider_omits_usage() {
        let body: &'static [u8] =
            b"data: {\"choices\":[{\"delta\":{\"content\":\"abcdefgh\"}}]}\n\ndata: [DONE]\n\n";
        let (base, _, _) = serve_stream(vec![ConnStep::Sse {
            parts: vec![body],
            hold: false,
        }]);
        let resp = stub_provider(base)
            .stream_complete(req(), Arc::new(NoopSink), CancelToken::new())
            .await
            .unwrap();
        assert_eq!(resp.tokens_out, 2, "8 chars / 4");
        assert!(resp.tokens_in > 0);
    }

    /// sink 事件顺序记录（协议形状断言）。
    #[derive(Default)]
    struct RecordingSink {
        events: StdMutex<Vec<String>>,
        text: StdMutex<String>,
        first_delta_at: StdMutex<Option<Instant>>,
    }
    impl StreamSink for RecordingSink {
        fn on_started(&self, id: &str) {
            self.events.lock().unwrap().push(format!("started:{id}"));
        }
        fn on_text_delta(&self, text: &str) {
            self.events.lock().unwrap().push("text".into());
            self.text.lock().unwrap().push_str(text);
            let mut f = self.first_delta_at.lock().unwrap();
            if f.is_none() {
                *f = Some(Instant::now());
            }
        }
        fn on_reasoning_delta(&self, _: &str) {}
        fn on_tool_call_delta(&self, _: &str, _: &str, _: &str) {}
        fn on_usage(&self, u: StreamUsage) {
            self.events
                .lock()
                .unwrap()
                .push(format!("usage:measured={}", u.measured));
        }
        fn on_completed(&self, finish: &str) {
            self.events
                .lock()
                .unwrap()
                .push(format!("completed:{finish}"));
        }
    }

    /// FakeModel 流式桥：脚本响应按 delta 拆分发射并聚合还原。
    #[tokio::test]
    async fn fake_streaming_emits_split_deltas_and_assembles() {
        let fake = FakeModel::default();
        fake.push_response(r#"{"action":"final","summary":"流式完成"}"#, 9, 7);
        let sink = Arc::new(RecordingSink::default());
        let resp = fake
            .stream_complete(req_with_tools(), sink.clone(), CancelToken::new())
            .await
            .unwrap();
        assert_eq!(resp.content, "流式完成");
        assert_eq!(*sink.text.lock().unwrap(), "流式完成");
        let events = sink.events.lock().unwrap().clone();
        assert!(events[0].starts_with("started:req_fake_"), "{events:?}");
        assert_eq!(*events.last().unwrap(), "completed:stop");
    }

    /// FakeModel 首 delta 前挂起 → 取消即时返回 model_cancelled。
    #[tokio::test]
    async fn fake_hold_until_cancel_returns_cancelled() {
        let fake = FakeModel::default();
        fake.push_response("done", 1, 1);
        fake.hold_stream_until_cancel();
        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            c2.cancel();
        });
        let started = Instant::now();
        let err = fake
            .stream_complete(req(), Arc::new(NoopSink), cancel)
            .await
            .unwrap_err();
        assert_eq!(err, "model_cancelled");
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
