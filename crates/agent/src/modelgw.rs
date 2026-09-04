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
                        model: if req.model.is_empty() { None } else { Some(req.model.as_str()) },
                        finish_reason: Some(resp.finish_reason.as_str()),
                        error_code: None,
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
                        model: if req.model.is_empty() { None } else { Some(req.model.as_str()) },
                        finish_reason: None,
                        error_code: Some(e.split(':').next().unwrap_or("model_error")),
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
    /// 请求指定模型（空 = Profile 默认）；用于回查能力快照摘要。
    model: Option<&'a str>,
    finish_reason: Option<&'a str>,
    /// 确定性失败错误串前缀（ok 时为空）。
    error_code: Option<&'a str>,
}

/// 回查该 model 的未过期 probe 能力摘要（轻量 JSON 提取；完整消费方类型在
/// model_protocol.rs）。无快照/过期 → 空串（保守 legacy 路径）。
fn probe_digest_for_model(conn: &rusqlite::Connection, model: &str) -> String {
    if model.is_empty() {
        return String::new();
    }
    let caps: Option<String> = conn
        .query_row(
            "SELECT capabilities_json FROM model_profiles
             WHERE default_model = ?1 ORDER BY updated_at DESC LIMIT 1",
            [model],
            |r| r.get(0),
        )
        .ok();
    let Some(caps) = caps else {
        return String::new();
    };
    let v: serde_json::Value = serde_json::from_str(&caps).unwrap_or(serde_json::Value::Null);
    let source = v["source"].as_str().unwrap_or_default();
    let digest = v["digest"].as_str().unwrap_or_default();
    let expires_at = v["expiresAt"].as_str().unwrap_or_default();
    if source != "probe" || digest.is_empty() {
        return String::new();
    }
    let expired = match (sg_store::timefmt::parse(expires_at), sg_store::timefmt::parse(&sg_store::timefmt::now())) {
        (Some(exp), Some(now)) => exp <= now,
        _ => true,
    };
    if expired {
        String::new()
    } else {
        digest.to_string()
    }
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
        // ADR-033 M0：模型轮次观测（additive；不存 prompt/response 正文）。
        let turn_seq: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(turn_seq),0)+1 FROM model_turns WHERE agent_run_id=?1",
                [run_id],
                |r| r.get(0),
            )
            .unwrap_or(1);
        let model_name = stats.model.unwrap_or_default();
        let finish = stats.finish_reason.unwrap_or_default();
        let error_code = if status == "ok" {
            String::new()
        } else {
            stats.error_code.unwrap_or_default().to_string()
        };
        let _ = conn.execute(
            "INSERT INTO model_turns(
                id, agent_run_id, turn_seq, protocol, capability_digest, provider, model,
                tokens_in, tokens_out, cached_tokens, reasoning_tokens, ttft_ms, total_ms,
                finish_reason, status, error_code, created_at
             ) VALUES (?1,?2,?3,'legacy_json',?4,?5,?6,?7,?8,0,0,NULL,?9,?10,?11,?12,?13)",
            rusqlite::params![
                ids::new_id("mt"),
                run_id,
                turn_seq,
                probe_digest_for_model(conn, &model_name),
                provider,
                model_name,
                tin,
                tout,
                latency_ms,
                finish,
                if status == "ok" { "ok" } else { "failed" },
                error_code,
                timefmt::now()
            ],
        );
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use sg_integrations::FakeModel;

    fn store_with_v24() -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "sg-mgw-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        let store = Store::open(&dir, "test").unwrap();
        (store, dir)
    }

    /// ADR-033 M0：每次模型调用写一条 model_turns 观测行
    /// （protocol=legacy_json、token/延迟/终态落库；不存正文）。
    #[test]
    fn gateway_records_model_turns() {
        let (store, dir) = store_with_v24();
        assert!(store.schema_version().unwrap() >= 24);
        let fake = FakeModel::default();
        fake.push_response("ok", 7, 3);
        let gateway = Gateway::new(Box::new(fake));
        let req = CompletionRequest {
            model: "m1".into(),
            system_prompt: String::new(),
            messages: vec![sg_integrations::model::ChatMessage {
                role: "user".into(),
                content: "hi".into(),
            }],
            max_tokens: 16,
            response_schema: None,
        };
        let resp = gateway
            .call(&store, "run-x", &Budget::default(), &req)
            .unwrap();
        assert_eq!(resp.content, "ok");

        let rows: Vec<(i64, String, String, i64, String, String)> = store
            .with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT turn_seq, protocol, status, tokens_in, finish_reason, capability_digest
                     FROM model_turns WHERE agent_run_id = 'run-x' ORDER BY turn_seq",
                )?;
                let rows = stmt.query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                })?;
                let mut out = Vec::new();
                for row in rows {
                    out.push(row?);
                }
                Ok(out)
            })
            .unwrap();
        assert_eq!(rows.len(), 1);
        let (seq, protocol, status, tin, finish, digest) = &rows[0];
        assert_eq!(*seq, 1);
        assert_eq!(protocol, "legacy_json");
        assert_eq!(status, "ok");
        assert_eq!(*tin, 7);
        assert_eq!(finish, "stop");
        // 无 probe 快照 → digest 空串（保守 legacy 路径）。
        assert_eq!(digest, "");
        // 错误路径：脚本耗尽 → failed 终态行。
        let err = gateway
            .call(&store, "run-x", &Budget::default(), &req)
            .unwrap_err();
        assert!(err.contains("model_unavailable"), "{err}");
        let failed: i64 = store
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM model_turns WHERE agent_run_id='run-x' AND status='failed'",
                    [],
                    |r| r.get(0),
                )
                .map_err(sg_store::Error::from)
            })
            .unwrap();
        assert_eq!(failed, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
