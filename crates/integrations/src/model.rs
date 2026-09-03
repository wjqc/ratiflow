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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub content: String,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub finish_reason: String,
}

pub trait ModelProvider: Send + Sync {
    fn name(&self) -> &str;
    fn health_check(&self) -> Result<(), String>;
    fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, String>;
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
        let mut messages: Vec<serde_json::Value> = Vec::new();
        if !req.system_prompt.is_empty() {
            messages.push(serde_json::json!({"role": "system", "content": req.system_prompt}));
        }
        for msg in &req.messages {
            // 兼容性降级（语义保留）：
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
        let body =
            serde_json::json!({"model": model, "messages": messages, "max_tokens": req.max_tokens});
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
        Ok(CompletionResponse {
            content,
            tokens_in: parsed["usage"]["prompt_tokens"].as_i64().unwrap_or(0),
            tokens_out: parsed["usage"]["completion_tokens"].as_i64().unwrap_or(0),
            finish_reason: parsed["choices"][0]["finish_reason"]
                .as_str()
                .unwrap_or_default()
                .into(),
        })
    }
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
#[derive(Default)]
pub struct FakeModel {
    script: std::sync::Mutex<std::collections::VecDeque<CompletionResponse>>,
    errors: std::sync::Mutex<std::collections::VecDeque<String>>,
    pub calls: std::sync::Mutex<Vec<CompletionRequest>>,
}

impl FakeModel {
    pub fn push_response(&self, content: &str, tokens_in: i64, tokens_out: i64) {
        self.script.lock().unwrap().push_back(CompletionResponse {
            content: content.into(),
            tokens_in,
            tokens_out,
            finish_reason: "stop".into(),
        });
    }

    pub fn push_error(&self, error: &str) {
        self.errors.lock().unwrap().push_back(error.into());
    }
}

impl ModelProvider for FakeModel {
    fn name(&self) -> &str {
        "fake"
    }

    fn health_check(&self) -> Result<(), String> {
        Ok(())
    }

    fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, String> {
        self.calls.lock().unwrap().push(req.clone());
        if let Some(err) = self.errors.lock().unwrap().pop_front() {
            return Err(err);
        }
        self.script
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "model_unavailable: script exhausted".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};

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
            }],
            max_tokens: 16,
            response_schema: None,
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
