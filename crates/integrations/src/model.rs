//! OpenAI 兼容模型适配器 + fake（限流/超时/无效 JSON 契约场景）。
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub system_prompt: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub max_tokens: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<serde_json::Value>,
    /// 原生 function calling（ADR-033 M1）：OpenAI 兼容 function 定义数组（JSON 串）；
    /// None = legacy_json 协议。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_json: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// role=tool：本条结果对应的原生调用 id（原生 transcript）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// role=assistant：原生 tool_calls 数组（JSON 串，OpenAI 形状）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls_json: Option<String>,
}

/// Provider 原生工具调用（M1）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProviderToolCall {
    pub id: String,
    pub name: String,
    /// 原始 arguments JSON 串（原样传递，不重排序）。
    pub arguments: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub content: String,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub finish_reason: String,
    #[serde(default)]
    pub tool_calls: Vec<ProviderToolCall>,
    #[serde(default)]
    pub cached_tokens: i64,
    #[serde(default)]
    pub reasoning_tokens: i64,
    /// WP-1 计量：Provider 是否真实返回 usage（false = 数值为估算/缺省，不得作
    /// ledger settle 实际量——settle None 进 reconciliation）。
    #[serde(default)]
    pub usage_present: bool,
    /// M4：本轮流式的 reasoning plaintext 经 AES-256-GCM 加密后的状态
    /// （base64(nonce||ciphertext||tag)）。None = 未启用持久化/无 reasoning。
    /// 网关层填充；明文永不出网关，不进 rollout/日志/UI。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_state_encrypted: Option<String>,
    /// M4：reasoning 持久化状态（"encrypted"|"dropped_policy"|"dropped_key_unavailable"|"none"）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning_state_status: String,
}

pub trait ModelProvider: Send + Sync {
    fn name(&self) -> &str;
    fn health_check(&self) -> Result<(), String>;
    fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, String>;
    /// Provider 能力快照（ADR-033 §4.1）；None = 未验证 → 保守 legacy 路径。
    fn capability(&self) -> Option<serde_json::Value> {
        None
    }
    /// 数据策略（ADR-033 §4.1 dataPolicy）：reasoningPersist / serverState 等门控。
    fn data_policy(&self) -> Option<serde_json::Value> {
        None
    }
    /// Provider 指纹（checkpoint v2 绑定回放来源；跨 Provider 拒绝回放）。
    fn fingerprint(&self) -> String {
        self.name().to_string()
    }
}

/// ureq 2.x 对 HTTP 4xx/5xx 一律返回 Err(Error::Status)——必须先解包再按状态码分类，
/// 否则限流/服务端错误全被误报成 model_timeout（前端文案随之失真）。
fn classify_ureq(
    transport_code: &str,
    result: Result<ureq::Response, ureq::Error>,
) -> Result<ureq::Response, String> {
    match result {
        Ok(response) => Ok(response),
        Err(ureq::Error::Status(code, response)) => {
            if code == 429 {
                return Err("model_rate_limited".into());
            }
            let detail = response.into_string().unwrap_or_default();
            let detail = detail.chars().take(200).collect::<String>();
            if detail.is_empty() {
                Err(format!("{transport_code}: HTTP {code}"))
            } else {
                Err(format!("{transport_code}: HTTP {code}: {detail}"))
            }
        }
        Err(e) => {
            let text = e.to_string();
            if text.contains("timed out") || text.contains("TimedOut") {
                Err(format!("model_timeout: {text}"))
            } else {
                Err(format!("{transport_code}: {text}"))
            }
        }
    }
}

/// OpenAI 兼容 /chat/completions（BYOK；密钥只在内存）。
pub struct ModelHttp {
    pub name_value: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

impl ModelProvider for ModelHttp {
    fn name(&self) -> &str {
        &self.name_value
    }

    fn health_check(&self) -> Result<(), String> {
        if self.api_key.is_empty() {
            return Err("model api key missing".into());
        }
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));
        let response = ureq::get(&url)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .timeout(std::time::Duration::from_secs(10))
            .call();
        let response = classify_ureq("model_unavailable", response)?;
        let _ = response;
        Ok(())
    }

    fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, String> {
        if self.api_key.is_empty() {
            return Err("model_unavailable: api key missing".into());
        }
        let model = if req.model.is_empty() {
            self.model.clone()
        } else {
            req.model.clone()
        };
        let body = build_chat_body(req, &model, false);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let send = |body: &serde_json::Value| {
            classify_ureq(
                "model_unavailable",
                ureq::post(&url)
                    .set("Authorization", &format!("Bearer {}", self.api_key))
                    // 推理型模型非流式生成可达数分钟（16k token 预算），给足读超时。
                    .timeout(std::time::Duration::from_secs(300))
                    .send_json(body.clone()),
            )
        };
        // 限流是长任务（如 PRD 起草）最常撞上的瞬态错误：429 退避后重试一次。
        // ureq 对 4xx 一律返回 Err——重试只能挂在错误分支上，Ok 分支里判状态码是死代码。
        let mut result = send(&body);
        if result
            .as_ref()
            .err()
            .is_some_and(|e| e == "model_rate_limited")
        {
            std::thread::sleep(std::time::Duration::from_secs(2));
            result = send(&body);
        }
        let response = result?;
        let parsed: serde_json::Value = response
            .into_json()
            .map_err(|e| format!("model_invalid_json: {e}"))?;
        let content = parsed["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if content.is_empty() {
            let finish = parsed["choices"][0]["finish_reason"]
                .as_str()
                .unwrap_or("unknown");
            // 独立错误码：finish_reason=length 是输出上限耗尽而非上下文过大，
            // 错误串不得含 "length" 等关键词，否则被上游误归类为 context_too_large。
            let hint = if finish == "length" {
                "输出 token 预算耗尽：请换用支持更长输出的模型"
            } else {
                "请重试或更换模型"
            };
            return Err(format!(
                "model_empty_output: 模型未返回正文（finish_reason={finish}）；{hint}"
            ));
        }
        // 原生工具调用解析（OpenAI 形状；DeepSeek/GLM 兼容端点同形）。
        let tool_calls: Vec<ProviderToolCall> = parsed["choices"][0]["message"]["tool_calls"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .enumerate()
                    .map(|(i, tc)| ProviderToolCall {
                        id: tc["id"]
                            .as_str()
                            .map(String::from)
                            .unwrap_or_else(|| format!("call_{i}")),
                        name: tc["function"]["name"].as_str().unwrap_or_default().into(),
                        arguments: tc["function"]["arguments"]
                            .as_str()
                            .unwrap_or_default()
                            .into(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(CompletionResponse {
            content,
            tokens_in: parsed["usage"]["prompt_tokens"].as_i64().unwrap_or(0),
            tokens_out: parsed["usage"]["completion_tokens"].as_i64().unwrap_or(0),
            finish_reason: parsed["choices"][0]["finish_reason"]
                .as_str()
                .unwrap_or_default()
                .into(),
            tool_calls,
            cached_tokens: cached_tokens_from_usage(&parsed["usage"]),
            reasoning_tokens: parsed["usage"]["completion_tokens_details"]["reasoning_tokens"]
                .as_i64()
                .unwrap_or(0),
            usage_present: parsed["usage"].get("prompt_tokens").is_some(),
            reasoning_state_encrypted: None,
            reasoning_state_status: "none".into(),
        })
    }
}

/// 缓存命中 Token 提取：OpenAI 形状（prompt_tokens_details.cached_tokens）优先，
/// 回落 DeepSeek 顶层形状（prompt_cache_hit_tokens）；都没有时为 0。
pub fn cached_tokens_from_usage(usage: &serde_json::Value) -> i64 {
    usage["prompt_tokens_details"]["cached_tokens"]
        .as_i64()
        .or_else(|| usage["prompt_cache_hit_tokens"].as_i64())
        .unwrap_or(0)
}

/// OpenAI 兼容 /chat/completions 请求体（流式/非流式共用同一构造，M1 transcript 形状不变）。
pub fn build_chat_body(req: &CompletionRequest, model: &str, stream: bool) -> serde_json::Value {
    let native = req.tools_json.is_some();
    let mut messages: Vec<serde_json::Value> = Vec::new();
    if !req.system_prompt.is_empty() {
        messages.push(serde_json::json!({"role": "system", "content": req.system_prompt}));
    }
    for msg in &req.messages {
        // 原生 transcript：assistant tool_calls 与 tool 结果按 OpenAI 形状透传，
        // call_id 原样保留（M1：不再把 tool 降级成 user）。
        if native && msg.role == "assistant" {
            if let Some(tc) = msg.tool_calls_json.as_deref() {
                let calls: serde_json::Value =
                    serde_json::from_str(tc).unwrap_or(serde_json::Value::Null);
                messages.push(serde_json::json!({
                    "role": "assistant",
                    "content": msg.content,
                    "tool_calls": calls,
                }));
                continue;
            }
        }
        if native && msg.role == "tool" {
            if let Some(id) = msg.tool_call_id.as_deref() {
                messages.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": id,
                    "content": msg.content,
                }));
                continue;
            }
        }
        // legacy 兼容性降级（语义保留）：
        // - developer 是 OpenAI 新式角色，兼容端点（DeepSeek 等）只认 system 等；
        // - harness 的 tool 消息是扁平文本、不带 tool_call_id，严格端点会 400，
        //   统一转成 user 消息承载工具输出。
        let role = match msg.role.as_str() {
            "developer" => "system",
            "tool" => "user",
            other => other,
        };
        messages.push(serde_json::json!({"role": role, "content": msg.content}));
    }
    let mut body =
        serde_json::json!({"model": model, "messages": messages, "max_tokens": req.max_tokens});
    if stream {
        body["stream"] = serde_json::json!(true);
    }
    if native {
        if let Some(tools) = req.tools_json.as_deref() {
            if let Ok(defs) = serde_json::from_str::<serde_json::Value>(tools) {
                body["tools"] = defs;
                body["tool_choice"] = serde_json::json!("auto");
            }
        }
    }
    body
}

/// OpenAI 兼容 GET /models（modelProfile.syncModels 用）；返回模型 ID 列表（排序去重）。
pub fn list_models(base_url: &str, api_key: &str) -> Result<Vec<String>, String> {
    if api_key.is_empty() {
        return Err("model_unavailable: api key missing".into());
    }
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let response = classify_ureq(
        "model_unavailable",
        ureq::get(&url)
            .set("Authorization", &format!("Bearer {}", api_key))
            .timeout(std::time::Duration::from_secs(15))
            .call(),
    )?;
    let parsed: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("model_invalid_json: {e}"))?;
    let mut ids: Vec<String> = parsed["data"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|m| m["id"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        return Err("model_invalid_json: empty models list".into());
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// 脚本化 fake（内部队列加锁，complete 经 &self 调用）。
/// `native=true` 时若请求带原生 tools，脚本中的 legacy action JSON 会被桥接为
/// 原生 tool_calls 响应（final 动作仍为正文）——同一脚本可测双协议。
/// 流式桥（M2）：`enable_streaming` 后同一脚本经 delta 拆分走 StreamingModelProvider。
#[derive(Default)]
pub struct FakeModel {
    pub(crate) script: std::sync::Mutex<std::collections::VecDeque<CompletionResponse>>,
    pub(crate) errors: std::sync::Mutex<std::collections::VecDeque<String>>,
    pub calls: std::sync::Mutex<Vec<CompletionRequest>>,
    pub native: std::sync::atomic::AtomicBool,
    /// 流式能力声明 + 流式路径开关（M2 测试桥）。
    pub(crate) streaming: std::sync::atomic::AtomicBool,
    /// 首 delta 前挂起直到取消（取消时序测试）。
    pub(crate) hold_until_cancel: std::sync::atomic::AtomicBool,
    /// 第 n 个 delta 后挂起 millis 毫秒再检查取消（流中段取消测试）。
    pub(crate) stall_after_delta: std::sync::Mutex<Option<(usize, u64)>>,
    /// M4：数据策略（reasoningPersist 门控测试位）。
    pub(crate) data_policy: std::sync::Mutex<Option<serde_json::Value>>,
    /// M4：流式发射的 reasoning 明文（仅测试路径；网关加密后明文即弃）。
    pub(crate) reasoning_text: std::sync::Mutex<Option<String>>,
    pub(crate) call_counter: std::sync::atomic::AtomicU64,
}

impl FakeModel {
    /// 声明数据策略（测试用：reasoningPersist=encrypted_at_rest 等）。
    pub fn set_data_policy(&self, policy: serde_json::Value) {
        *self.data_policy.lock().unwrap() = Some(policy);
    }

    /// M4 测试：流式时发射 reasoning_content delta（DeepSeek 形状）。
    pub fn set_reasoning_text(&self, text: &str) {
        *self.reasoning_text.lock().unwrap() = Some(text.into());
    }
}

impl FakeModel {
    pub fn push_response(&self, content: &str, tokens_in: i64, tokens_out: i64) {
        self.script.lock().unwrap().push_back(CompletionResponse {
            content: content.into(),
            tokens_in,
            tokens_out,
            finish_reason: "stop".into(),
            ..CompletionResponse::default()
        });
    }

    pub fn push_error(&self, error: &str) {
        self.errors.lock().unwrap().push_back(error.into());
    }

    /// M4：带缓存命中 tokens 的脚本响应（缓存观测测试）。
    pub fn push_response_cached(
        &self,
        content: &str,
        tokens_in: i64,
        tokens_out: i64,
        cached: i64,
    ) {
        self.script.lock().unwrap().push_back(CompletionResponse {
            content: content.into(),
            tokens_in,
            tokens_out,
            cached_tokens: cached,
            finish_reason: "stop".into(),
            ..CompletionResponse::default()
        });
    }

    /// 声明原生工具能力（ADR-033：fake 以 manual 来源宣称，仅供测试路径）。
    pub fn enable_native(&self) {
        self.native
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

impl ModelProvider for FakeModel {
    fn name(&self) -> &str {
        "fake"
    }

    fn health_check(&self) -> Result<(), String> {
        Ok(())
    }

    fn capability(&self) -> Option<serde_json::Value> {
        if self.native.load(std::sync::atomic::Ordering::Relaxed) {
            let streaming = self.streaming.load(std::sync::atomic::Ordering::Relaxed);
            Some(serde_json::json!({
                "schemaVersion": 1,
                "protocols": ["chat_completions"],
                "preferredProtocol": "chat_completions",
                "nativeTools": true,
                "streamText": streaming,
                "streamToolArguments": streaming,
                "reasoningTransport": "none",
                "compaction": "local_structured",
                "cache": "implicit",
                "serverState": "unsupported",
                "source": "manual",
                "verifiedAt": "1970-01-01T00:00:00.000Z",
                "expiresAt": "2999-01-01T00:00:00.000Z",
            }))
        } else {
            None
        }
    }

    fn data_policy(&self) -> Option<serde_json::Value> {
        self.data_policy.lock().unwrap().clone()
    }

    fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, String> {
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
        // 原生桥接：脚本 action JSON → 原生 tool_calls（final 动作 = 正文答复）。
        let n = self
            .call_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if req.tools_json.is_some() {
            // 数组脚本 → 单响应多 tool call（供并行拒绝测试）。
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
                return Ok(resp);
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp.content) {
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
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};

    /// 缓存命中提取：OpenAI 嵌套形状 / DeepSeek 顶层形状 / 双缺失回落 0。
    #[test]
    fn cached_tokens_accepts_both_provider_shapes() {
        let openai = serde_json::json!({"prompt_tokens": 100, "prompt_tokens_details": {"cached_tokens": 64}});
        assert_eq!(cached_tokens_from_usage(&openai), 64);
        let deepseek = serde_json::json!({"prompt_tokens": 100, "prompt_cache_hit_tokens": 64, "prompt_cache_miss_tokens": 36});
        assert_eq!(cached_tokens_from_usage(&deepseek), 64);
        let none = serde_json::json!({"prompt_tokens": 100});
        assert_eq!(cached_tokens_from_usage(&none), 0);
    }

    /// 本地 HTTP stub：按脚本逐请求返回原始响应（覆盖 ureq 真实传输路径）。
    fn serve(script: Vec<String>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for body in script {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut content_length = 0usize;
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_lowercase().strip_prefix("content-length:") {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
                if content_length > 0 {
                    let mut buf = vec![0u8; content_length];
                    reader.read_exact(&mut buf).unwrap();
                }
                let mut socket = stream;
                socket.write_all(body.as_bytes()).unwrap();
            }
        });
        format!("http://{addr}")
    }

    fn stub_req() -> CompletionRequest {
        CompletionRequest {
            model: "m".into(),
            system_prompt: String::new(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: "hi".into(),
                ..Default::default()
            }],
            max_tokens: 16,
            response_schema: None,
            tools_json: None,
        }
    }

    #[test]
    fn rate_limited_retries_once_then_succeeds() {
        // 429 → 退避重试 → 200。这是上轮"429 重试是死代码"缺陷的回归测试。
        let ok = r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
        let base = serve(vec![
            "HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
            format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", ok.len(), ok),
        ]);
        let provider = ModelHttp {
            name_value: "stub".into(),
            base_url: base,
            api_key: "k".into(),
            model: "m".into(),
        };
        let resp = provider.complete(&stub_req()).unwrap();
        assert_eq!(resp.content, "ok");
        assert_eq!(resp.finish_reason, "stop");
    }

    #[test]
    fn empty_content_uses_dedicated_code_without_length_keyword() {
        // finish_reason=length 的空正文是输出上限耗尽：错误串不得含 "length"，
        // 否则 agent 侧分类器会误判为 context_too_large。
        let body =
            r#"{"choices":[{"message":{"content":""},"finish_reason":"length"}],"usage":{}}"#;
        let base = serve(vec![format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )]);
        let provider = ModelHttp {
            name_value: "stub".into(),
            base_url: base,
            api_key: "k".into(),
            model: "m".into(),
        };
        let err = provider.complete(&stub_req()).unwrap_err();
        // 独立前缀 + 面向用户的处置提示；agent 分类器按前缀优先匹配，
        // 因此串中的 finish_reason=length 不会落入 context_too_large。
        assert!(err.starts_with("model_empty_output"), "{err}");
        assert!(err.contains("输出 token 预算耗尽"), "{err}");
    }
}
