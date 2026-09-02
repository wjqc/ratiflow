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
            messages.push(serde_json::json!({"role": msg.role, "content": msg.content}));
        }
        let body =
            serde_json::json!({"model": model, "messages": messages, "max_tokens": req.max_tokens});
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        // 限流是长任务（如 PRD 起草）最常撞上的瞬态错误：429 退避后重试一次。
        let send = |body: &serde_json::Value| {
            classify_ureq(
                "model_unavailable",
                ureq::post(&url)
                    .set("Authorization", &format!("Bearer {}", self.api_key))
                    .timeout(std::time::Duration::from_secs(120))
                    .send_json(body.clone()),
            )
        };
        let mut response = send(&body)?;
        if response.status() == 429 {
            std::thread::sleep(std::time::Duration::from_secs(2));
            response = send(&body)?;
        }
        let parsed: serde_json::Value = response
            .into_json()
            .map_err(|e| format!("model_invalid_json: {e}"))?;
        let content = parsed["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if content.is_empty() {
            return Err("model_unavailable: no choices".into());
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
