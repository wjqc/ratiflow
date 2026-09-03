//! 模型网关：出网前统一裁剪/脱敏/预算；调用审计不含正文（FR-PLT-007）。
use serde::{Deserialize, Serialize};
use sg_integrations::model::{CompletionRequest, CompletionResponse, ModelProvider};
use sg_store::{ids, timefmt, Store};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Budget {
    pub max_calls: i64,
    pub max_tokens_in: i64,
    pub max_tokens_out: i64,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_calls: 50,
            max_tokens_in: 200_000,
            max_tokens_out: 20_000,
        }
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
        Self {
            provider,
            usage: std::sync::Mutex::new(Usage::default()),
        }
    }

    pub fn provider_name(&self) -> &str {
        self.provider.name()
    }

    pub fn health_check(&self) -> Result<(), String> {
        self.provider.health_check()
    }

    /// 调用：预算检查 → 全量消息脱敏 → provider → 审计。
    pub fn call(
        &self,
        store: &Store,
        run_id: &str,
        budget: &Budget,
        req: &CompletionRequest,
    ) -> Result<CompletionResponse, String> {
        {
            let usage = self.usage.lock().unwrap();
            if budget.max_calls > 0 && usage.calls >= budget.max_calls {
                return Err("budget_exceeded: calls".into());
            }
            if budget.max_tokens_in > 0 && usage.tokens_in >= budget.max_tokens_in {
                return Err("budget_exceeded: tokens_in".into());
            }
            if budget.max_tokens_out > 0 && usage.tokens_out >= budget.max_tokens_out {
                return Err("budget_exceeded: tokens_out".into());
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
            masked_messages.push(sg_integrations::model::ChatMessage {
                role: msg.role.clone(),
                content: text,
            });
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
                record(
                    store,
                    run_id,
                    &CallStats {
                        provider: self.provider.name(),
                        status: "ok",
                        tokens_in: resp.tokens_in,
                        tokens_out: resp.tokens_out,
                        latency_ms: started.elapsed().as_millis() as i64,
                        redactions,
                    },
                );
                Ok(resp)
            }
            Err(e) => {
                record(
                    store,
                    run_id,
                    &CallStats {
                        provider: self.provider.name(),
                        status: "error",
                        tokens_in: 0,
                        tokens_out: 0,
                        latency_ms: started.elapsed().as_millis() as i64,
                        redactions,
                    },
                );
                Err(e)
            }
        }
    }

    pub fn usage(&self) -> Usage {
        *self.usage.lock().unwrap()
    }
}

struct CallStats<'a> {
    provider: &'a str,
    status: &'a str,
    tokens_in: i64,
    tokens_out: i64,
    latency_ms: i64,
    redactions: usize,
}

fn record(store: &Store, run_id: &str, stats: &CallStats<'_>) {
    let (provider, status, tin, tout, latency_ms, redactions) = (
        stats.provider,
        stats.status,
        stats.tokens_in,
        stats.tokens_out,
        stats.latency_ms,
        stats.redactions,
    );
    let _ = store.with_conn(|conn| {
        let _ = conn.execute(
            "INSERT INTO model_calls(id, agent_run_id, provider, model, tokens_in, tokens_out, cost_micros, latency_ms, redactions, status, created_at)
             VALUES (?1,?2,?3,'default',?4,?5,0,?6,?7,?8,?9)",
            rusqlite::params![ids::new_id("mc"), run_id, provider, tin, tout, latency_ms, redactions as i64, status, timefmt::now()],
        );
        Ok(())
    });
}
