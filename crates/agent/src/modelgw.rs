//! 模型网关：出网前统一裁剪/脱敏/预算；调用审计不含正文（FR-PLT-007）。
use serde::{Deserialize, Serialize};
use sg_integrations::model::{CompletionRequest, CompletionResponse, ModelProvider};
use sg_store::{ids, timefmt, Error, Store};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Budget {
    pub max_calls: i64,
    pub max_tokens_in: i64,
    pub max_tokens_out: i64,
}

impl Default for Budget {
    fn default() -> Self {
        Self { max_calls: 50, max_tokens_in: 200_000, max_tokens_out: 20_000 }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Usage {
    pub calls: i64,
    pub tokens_in: i64,
    pub tokens_out: i64,
}

pub struct Gateway {
    provider: Box<dyn ModelProvider>,
    usage: std::sync::Mutex<Usage>,
}

impl Gateway {
    pub fn new(provider: Box<dyn ModelProvider>) -> Self {
        Self { provider, usage: std::sync::Mutex::new(Usage::default()) }
    }

    pub fn provider_name(&self) -> &str {
        self.provider.name()
    }

    pub fn health_check(&self) -> Result<(), String> {
        self.provider.health_check()
    }

    /// 调用：预算检查 → 全量消息脱敏 → provider → 审计。
    pub fn call(&self, store: &Store, run_id: &str, budget: &Budget, req: &CompletionRequest) -> Result<CompletionResponse, String> {
        {
            let usage = self.usage.lock().unwrap();
            if budget.max_calls > 0 && usage.calls >= budget.max_calls {
                return Err("budget_exceeded: calls".into());
            }
            if budget.max_tokens_in > 0 && usage.tokens_in >= budget.max_tokens_in {
                return Err("budget_exceeded: tokens_in".into());
            }
        }

        let mut masked = req.clone();
        let mut redactions = 0usize;
        let (masked_prompt, n) = sg_store::scan::mask(req.system_prompt.as_bytes());
        masked.system_prompt = masked_prompt;
        redactions += n;
        let mut masked_messages = Vec::with_capacity(req.messages.len());
        for msg in &req.messages {
            let (text, n) = sg_store::scan::mask(msg.content.as_bytes());
            redactions += n;
            masked_messages.push(sg_integrations::model::ChatMessage { role: msg.role.clone(), content: text });
        }
        masked.messages = masked_messages;

        let started = std::time::Instant::now();
        let result = self.provider.complete(&masked);
        match result {
            Ok(resp) => {
                {
                    let mut usage = self.usage.lock().unwrap();
                    usage.calls += 1;
                    usage.tokens_in += resp.tokens_in;
                    usage.tokens_out += resp.tokens_out;
                }
                record(store, run_id, self.provider.name(), "ok", resp.tokens_in, resp.tokens_out,
                    started.elapsed().as_millis() as i64, redactions);
                Ok(resp)
            }
            Err(e) => {
                record(store, run_id, self.provider.name(), "error", 0, 0,
                    started.elapsed().as_millis() as i64, redactions);
                Err(e)
            }
        }
    }

    pub fn usage(&self) -> Usage {
        *self.usage.lock().unwrap()
    }
}

fn record(store: &Store, run_id: &str, provider: &str, status: &str, tin: i64, tout: i64, latency_ms: i64, redactions: usize) {
    let _ = store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO model_calls(id, agent_run_id, provider, model, tokens_in, tokens_out, cost_micros, latency_ms, redactions, status, created_at)
             VALUES (?1,?2,?3,'default',?4,?5,0,?6,?7,?8,?9)",
            rusqlite::params![ids::new_id("mc"), run_id, provider, tin, tout, latency_ms, redactions as i64, status, timefmt::now()],
        );
        Ok(())
    });
}
