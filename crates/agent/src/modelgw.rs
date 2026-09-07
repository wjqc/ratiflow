//! 模型网关：出网前统一裁剪/脱敏/预算；调用审计不含正文（FR-PLT-007）。
//! M2（ADR-033）：`call_turn` 提供流式路径——事件聚合器是权威轮次来源，
//! 首 token 计量（ttft）、chunk 上限、终文本秘密扫描都在此完成；
//! 非 `call`（同步 ureq 非流式）保留为压缩调用与 stream=false 回滚落点。
use serde::{Deserialize, Serialize};
use sg_integrations::cancel::CancelToken;
use sg_integrations::model::{CompletionRequest, CompletionResponse, ModelProvider};
use sg_integrations::stream::{
    StreamSink, StreamUsage as WireStreamUsage, StreamingModelProvider, TurnTimer,
};
use sg_store::{ids, timefmt, Store};
use std::sync::Arc;

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

/// 易失 delta 类别（M2）：与耐久事件严格分离——delta 只进高频 UI 通道，
/// 不写 outbox/SQLite；renderer 断线后经 agent.get/trace 对账重建事实。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaKind {
    Output,
    ReasoningSummary,
    ToolArguments,
}

impl DeltaKind {
    pub fn event_type(&self) -> &'static str {
        match self {
            DeltaKind::Output => "run.output_delta",
            DeltaKind::ReasoningSummary => "run.reasoning_summary_delta",
            DeltaKind::ToolArguments => "run.tool_arguments_delta",
        }
    }
}

/// UI delta 转发器（装配层实现：Core 高频通道）。实现必须非阻塞（try_send 丢帧
/// 优于阻塞 Run 循环）——丢 delta 不影响最终事实（§3 不变量 5）。
pub trait TurnDeltaForwarder: Send + Sync {
    fn forward(&self, run_id: &str, kind: DeltaKind, text: &str);
}

/// call_turn 的运行期选项：取消令牌 + 可选 UI 转发 + 缓存域键（观测）。
pub struct TurnOpts<'a> {
    pub cancel: &'a CancelToken,
    pub forwarder: Option<Arc<dyn TurnDeltaForwarder>>,
    /// M4：prompt_cache_key = tenant/workitem/tool-schema/instruction digest 组合，
    /// 不含原始用户身份或秘密；仅作观测与显式键缓存能力启用时的请求字段。
    pub prompt_cache_key: String,
}

/// M4：压缩策略选择（provider_opaque → local_structured → fail）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionStrategy {
    /// Provider 不透明压缩（需 capability.compaction=provider_opaque 且
    /// dataPolicy.serverState=allowed；当前无 Provider 声明 → 自动回落）。
    ProviderOpaque,
    /// 本地 F09 结构化摘要（现状路径）。
    LocalStructured,
}

impl CompactionStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            CompactionStrategy::ProviderOpaque => "provider_opaque",
            CompactionStrategy::LocalStructured => "local_structured",
        }
    }
}

/// Run 启动时冻结压缩策略（按能力快照与数据策略；快照缺失 = 保守本地）。
pub fn select_compaction(
    capability: Option<serde_json::Value>,
    data_policy: Option<serde_json::Value>,
) -> CompactionStrategy {
    use crate::model_protocol::CapabilitySnapshot;
    let cap_ok = capability
        .and_then(|v| serde_json::from_value::<CapabilitySnapshot>(v).ok())
        .map(|c| c.compaction == "provider_opaque")
        .unwrap_or(false);
    let policy_ok = data_policy
        .and_then(|p| {
            p.get("serverState")
                .and_then(|s| s.as_str())
                .map(String::from)
        })
        .map(|s| s == "allowed")
        .unwrap_or(false);
    if cap_ok && policy_ok {
        CompactionStrategy::ProviderOpaque
    } else {
        CompactionStrategy::LocalStructured
    }
}

pub struct Gateway {
    provider: Arc<dyn ModelProvider>,
    /// 流式能力（M2）：None = 该装配无流式路径，恒走非流式。
    /// 与 provider 可指向同一实例（Arc 共享，fake 脚本/配置状态一致）。
    streaming: Option<Arc<dyn StreamingModelProvider>>,
    /// block_on 用的运行时句柄（Run 循环在 spawn_blocking 线程，无隐式上下文）。
    handle: std::sync::OnceLock<tokio::runtime::Handle>,
    /// M4：reasoning 状态加密保险库（Keychain 派生密钥）；未装配 = 不持久化。
    vault: std::sync::OnceLock<Arc<crate::reasoning_state::ReasoningVault>>,
    usage: std::sync::Mutex<Usage>,
}

impl Gateway {
    pub fn new(provider: Box<dyn ModelProvider>) -> Self {
        Self::with_shared(Arc::from(provider), None)
    }

    /// 装配流式路径（core 装配层用）：provider 与 streaming 可共享同一实例。
    pub fn with_shared(
        provider: Arc<dyn ModelProvider>,
        streaming: Option<Arc<dyn StreamingModelProvider>>,
    ) -> Self {
        Self {
            provider,
            streaming,
            handle: std::sync::OnceLock::new(),
            vault: std::sync::OnceLock::new(),
            usage: std::sync::Mutex::new(Usage::default()),
        }
    }

    /// 注入 tokio 运行时句柄（app 启动时一次）。未注入时回退 try_current/临时运行时。
    pub fn set_runtime_handle(&self, handle: tokio::runtime::Handle) {
        let _ = self.handle.set(handle);
    }

    /// 注入 reasoning 状态保险库（app 启动时一次）。
    pub fn set_reasoning_vault(&self, vault: Arc<crate::reasoning_state::ReasoningVault>) {
        let _ = self.vault.set(vault);
    }

    /// Provider 指纹（checkpoint v2 绑定；跨 Provider 拒绝回放）。
    pub fn fingerprint(&self) -> String {
        self.provider.fingerprint()
    }

    /// 数据策略（reasoningPersist 门控来源）。
    pub fn data_policy(&self) -> Option<serde_json::Value> {
        self.provider.data_policy()
    }

    fn rt(&self) -> tokio::runtime::Handle {
        if let Some(h) = self.handle.get() {
            return h.clone();
        }
        if let Ok(h) = tokio::runtime::Handle::try_current() {
            return h;
        }
        // 兜底：纯同步调用方（罕见路径）临时运行时。进程生命周期内复用一个。
        static FALLBACK: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
        FALLBACK
            .get_or_init(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("fallback tokio runtime")
            })
            .handle()
            .clone()
    }

    pub fn provider_name(&self) -> &str {
        self.provider.name()
    }

    pub fn health_check(&self) -> Result<(), String> {
        self.provider.health_check()
    }

    /// 流式选路：能力快照显式 false（manual 禁用/探测否定）→ 非流式；
    /// unknown/未验证 → 尝试流式（打开期失败自动回退，见 call_turn）；
    /// SIXGATES_MODEL_STREAM=0 全局回滚开关。带工具请求且 streamToolArguments
    /// 显式 false → 非流式。
    pub fn wants_stream(&self, req: &CompletionRequest) -> bool {
        if std::env::var("SIXGATES_MODEL_STREAM")
            .map(|v| v == "0")
            .unwrap_or(false)
        {
            return false;
        }
        if self.streaming.is_none() {
            return false;
        }
        let cap = self.provider.capability().and_then(|v| {
            serde_json::from_value::<crate::model_protocol::CapabilitySnapshot>(v).ok()
        });
        let Some(cap) = cap else {
            return true;
        };
        if cap.stream_text == serde_json::json!(false) {
            return false;
        }
        if req.tools_json.is_some() && cap.stream_tool_arguments == serde_json::json!(false) {
            return false;
        }
        true
    }

    fn check_budget(&self, budget: &Budget) -> Result<(), String> {
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
        Ok(())
    }

    fn add_usage(&self, tokens_in: i64, tokens_out: i64) {
        let mut usage = self.usage.lock().unwrap();
        usage.calls += 1;
        usage.tokens_in += tokens_in;
        usage.tokens_out += tokens_out;
    }

    /// 一轮模型交互（M2 主路径）：预算 → 脱敏 → 流式/非流式 → 审计。
    /// 流式路径以事件聚合器产出为权威轮次（协议违规/中断/取消在此定型）。
    pub fn call_turn(
        &self,
        store: &Store,
        run_id: &str,
        budget: &Budget,
        req: &CompletionRequest,
        opts: &TurnOpts<'_>,
    ) -> Result<CompletionResponse, String> {
        self.check_budget(budget)?;
        if self.wants_stream(req) {
            if let Some(sp) = self.streaming.as_ref() {
                match self.call_streaming(store, run_id, req, sp.as_ref(), opts) {
                    Err((e, saw_delta))
                        if !saw_delta && open_phase_failure(&e) && !opts.cancel.is_cancelled() =>
                    {
                        // 流打开期失败（未产出任何 token）：同协议非流式聚合兜底，
                        // 不回退正文 JSON。已见 delta 的中断不兜底（禁止双重消费）。
                        eprintln!(
                            "{{\"level\":\"warn\",\"msg\":\"stream open failed; fallback non-stream: {e}\"}}"
                        );
                        return self.call(store, run_id, budget, req);
                    }
                    Err((e, _)) => return Err(e),
                    Ok(resp) => return Ok(resp),
                }
            }
        }
        self.call(store, run_id, budget, req)
    }

    /// 流式路径。Err 携带 (错误串, 是否已产出 delta)。
    #[allow(clippy::too_many_lines)]
    fn call_streaming(
        &self,
        store: &Store,
        run_id: &str,
        req: &CompletionRequest,
        sp: &dyn StreamingModelProvider,
        opts: &TurnOpts<'_>,
    ) -> Result<CompletionResponse, (String, bool)> {
        let started = std::time::Instant::now();
        let (masked, mut redactions) = mask_request(req);
        let sink = Arc::new(GatewaySink {
            run_id: run_id.to_string(),
            forwarder: opts.forwarder.clone(),
            agg: std::sync::Mutex::new(crate::model_protocol::TurnAggregator::new()),
            timer: std::sync::Mutex::new(TurnTimer::start()),
            reasoning_bytes: std::sync::atomic::AtomicU64::new(0),
            reasoning_plain: std::sync::Mutex::new(String::new()),
        });
        let cancel = opts.cancel.clone();
        let handle = self.rt();
        let result = handle.block_on(async {
            tokio::select! {
                _ = cancel.cancelled() => Err("model_cancelled".to_string()),
                r = sp.stream_complete(masked, sink.clone() as Arc<dyn StreamSink>, cancel.clone()) => r,
            }
        });
        // 聚合器终态判定（权威）：取消/中断/违规在这里定型。
        let agg_result = {
            let mut guard = sink.agg.lock().unwrap();
            let mut agg = std::mem::take(&mut *guard);
            if result.as_ref().err() == Some(&"model_cancelled".to_string()) {
                agg.cancel();
            }
            agg.finish()
        };
        let saw_delta = sink.timer.lock().unwrap().ttft_ms().is_some();
        match (result, agg_result) {
            (Ok(_provider_resp), Ok(turn)) => {
                // 权威轮次 = 聚合器产出。终文本秘密扫描（§5 M2：只扫最终可展示文本
                // 与工具参数完成态；逐 delta 不扫描）。参数只检测计数——替换会破坏
                // ActionDigest 与执行一致性，秘密进参数的治理边界仍是审批+digest。
                let (text, n) = sg_store::scan::mask(turn.text.as_bytes());
                redactions += n;
                for tc in &turn.tool_calls {
                    let (_, n) = sg_store::scan::mask(tc.arguments_json.as_bytes());
                    redactions += n;
                }
                if turn.tool_calls.is_empty() && text.trim().is_empty() {
                    let finish = if turn.finish_reason.is_empty() {
                        "unknown"
                    } else {
                        &turn.finish_reason
                    };
                    let hint = if turn.finish_reason == "length" {
                        "输出 token 预算耗尽：请换用支持更长输出的模型"
                    } else {
                        "请重试或更换模型"
                    };
                    let e = format!(
                        "model_empty_output: 模型未返回正文（finish_reason={finish}）；{hint}"
                    );
                    self.record_turn(
                        store,
                        run_id,
                        req,
                        true,
                        started,
                        sink.as_ref(),
                        &e,
                        redactions,
                        &CompletionResponse::default(),
                        &opts.prompt_cache_key,
                    );
                    return Err((e, saw_delta));
                }
                let reasoning_bytes = sink
                    .reasoning_bytes
                    .load(std::sync::atomic::Ordering::Relaxed);
                // M4：reasoning plaintext 持久化决策（默认丢弃；显式启用 + 密钥可用才加密）。
                let (reasoning_state_encrypted, reasoning_state_status) =
                    self.reasoning_persist_decision(sink.as_ref());
                let resp = CompletionResponse {
                    content: text,
                    tokens_in: turn.usage.input,
                    tokens_out: turn.usage.output,
                    finish_reason: turn.finish_reason.clone(),
                    tool_calls: turn
                        .tool_calls
                        .iter()
                        .map(|tc| sg_integrations::model::ProviderToolCall {
                            id: tc.call_id.clone(),
                            name: tc.name.clone(),
                            arguments: tc.arguments_json.clone(),
                        })
                        .collect(),
                    cached_tokens: turn.usage.cached_input,
                    reasoning_tokens: turn.usage.reasoning_output,
                    reasoning_state_encrypted,
                    reasoning_state_status,
                };
                self.add_usage(resp.tokens_in, resp.tokens_out);
                self.record_turn(
                    store,
                    run_id,
                    req,
                    true,
                    started,
                    sink.as_ref(),
                    "ok",
                    redactions,
                    &resp,
                    &opts.prompt_cache_key,
                );
                let _ = reasoning_bytes; // 字节数不落库（明文纪律）
                Ok(resp)
            }
            (Ok(_), Err(e)) => {
                let code = match &e {
                    crate::model_protocol::ModelProtocolError::Cancelled => "model_cancelled",
                    crate::model_protocol::ModelProtocolError::StreamInterrupted => {
                        "model_stream_interrupted"
                    }
                    crate::model_protocol::ModelProtocolError::ProtocolViolation(_) => {
                        "model_protocol_violation"
                    }
                    crate::model_protocol::ModelProtocolError::CapabilityMissing => {
                        "model_capability_missing"
                    }
                };
                let detail = match &e {
                    crate::model_protocol::ModelProtocolError::ProtocolViolation(v) => v.clone(),
                    _ => String::new(),
                };
                let msg = if detail.is_empty() {
                    code.to_string()
                } else {
                    format!("{code}: {detail}")
                };
                self.record_turn(
                    store,
                    run_id,
                    req,
                    true,
                    started,
                    sink.as_ref(),
                    &msg,
                    redactions,
                    &CompletionResponse::default(),
                    &opts.prompt_cache_key,
                );
                Err((msg, saw_delta))
            }
            (Err(e), _) => {
                self.record_turn(
                    store,
                    run_id,
                    req,
                    true,
                    started,
                    sink.as_ref(),
                    &e,
                    redactions,
                    &CompletionResponse::default(),
                    &opts.prompt_cache_key,
                );
                Err((e, saw_delta))
            }
        }
    }

    /// M4：reasoning 持久化三态决策。默认 none/dropped_policy；仅当
    /// dataPolicy.reasoningPersist == "encrypted_at_rest" 且保险库可用时加密。
    /// 密钥不可用 = 明确回退（结构化 warn + dropped_key_unavailable），不静默。
    fn reasoning_persist_decision(&self, sink: &GatewaySink) -> (Option<String>, String) {
        let plain = std::mem::take(&mut *sink.reasoning_plain.lock().unwrap());
        if plain.is_empty() {
            return (None, "none".into());
        }
        let enabled = self
            .provider
            .data_policy()
            .and_then(|p| {
                p.get("reasoningPersist")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .map(|s| s == "encrypted_at_rest")
            .unwrap_or(false);
        if !enabled {
            return (None, "dropped_policy".into());
        }
        match self.vault.get() {
            Some(vault) => match vault.encrypt(&plain) {
                Ok(blob) => (Some(blob), "encrypted".into()),
                Err(e) => {
                    eprintln!("{{\"level\":\"warn\",\"msg\":\"reasoning persist dropped: {e}\"}}");
                    (None, "dropped_key_unavailable".into())
                }
            },
            None => {
                eprintln!("{{\"level\":\"warn\",\"msg\":\"reasoning persist dropped: vault not configured\"}}");
                (None, "dropped_key_unavailable".into())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record_turn(
        &self,
        store: &Store,
        run_id: &str,
        req: &CompletionRequest,
        streamed: bool,
        started: std::time::Instant,
        sink: &GatewaySink,
        outcome: &str,
        redactions: usize,
        resp: &CompletionResponse,
        prompt_cache_key: &str,
    ) {
        let ttft = sink.timer.lock().unwrap().ttft_ms();
        let total_ms = started.elapsed().as_millis() as i64;
        let ok = outcome == "ok";
        let protocol = if req.tools_json.is_some() {
            if streamed {
                "native_tools_sse"
            } else {
                "native_tools"
            }
        } else if streamed {
            "legacy_json_sse"
        } else {
            "legacy_json"
        };
        record(
            store,
            run_id,
            &CallStats {
                protocol,
                provider: self.provider.name(),
                status: if ok { "ok" } else { "error" },
                // 错误路径 token 未知 → 0（与 call() 一致）。
                tokens_in: if ok { resp.tokens_in } else { 0 },
                tokens_out: if ok { resp.tokens_out } else { 0 },
                cached_tokens: if ok { resp.cached_tokens } else { 0 },
                reasoning_tokens: if ok { resp.reasoning_tokens } else { 0 },
                latency_ms: total_ms,
                redactions,
                model: if req.model.is_empty() {
                    None
                } else {
                    Some(req.model.as_str())
                },
                finish_reason: None,
                error_code: if ok {
                    None
                } else {
                    Some(outcome.split(':').next().unwrap_or("model_error"))
                },
                ttft_ms: ttft,
                prompt_cache_key,
            },
        );
    }

    /// 调用：预算检查 → 全量消息脱敏 → provider → 审计。
    pub fn call(
        &self,
        store: &Store,
        run_id: &str,
        budget: &Budget,
        req: &CompletionRequest,
    ) -> Result<CompletionResponse, String> {
        self.check_budget(budget)?;
        let (masked, redactions) = mask_request(req);

        let started = std::time::Instant::now();
        let result = self.provider.complete(&masked);
        match result {
            Ok(resp) => {
                self.add_usage(resp.tokens_in, resp.tokens_out);
                record(
                    store,
                    run_id,
                    &CallStats {
                        protocol: if req.tools_json.is_some() {
                            "native_tools"
                        } else {
                            "legacy_json"
                        },
                        provider: self.provider.name(),
                        status: "ok",
                        tokens_in: resp.tokens_in,
                        tokens_out: resp.tokens_out,
                        latency_ms: started.elapsed().as_millis() as i64,
                        redactions,
                        model: if req.model.is_empty() {
                            None
                        } else {
                            Some(req.model.as_str())
                        },
                        finish_reason: Some(resp.finish_reason.as_str()),
                        error_code: None,
                        ttft_ms: None,
                        cached_tokens: resp.cached_tokens,
                        reasoning_tokens: resp.reasoning_tokens,
                        prompt_cache_key: "",
                    },
                );
                Ok(resp)
            }
            Err(e) => {
                record(
                    store,
                    run_id,
                    &CallStats {
                        protocol: if req.tools_json.is_some() {
                            "native_tools"
                        } else {
                            "legacy_json"
                        },
                        provider: self.provider.name(),
                        status: "error",
                        tokens_in: 0,
                        tokens_out: 0,
                        latency_ms: started.elapsed().as_millis() as i64,
                        redactions,
                        model: if req.model.is_empty() {
                            None
                        } else {
                            Some(req.model.as_str())
                        },
                        finish_reason: None,
                        error_code: Some(e.split(':').next().unwrap_or("model_error")),
                        ttft_ms: None,
                        cached_tokens: 0,
                        reasoning_tokens: 0,
                        prompt_cache_key: "",
                    },
                );
                Err(e)
            }
        }
    }

    pub fn usage(&self) -> Usage {
        *self.usage.lock().unwrap()
    }

    /// ADR-033 M1：codec 协商（能力快照 → native_tools/legacy_json；
    /// 未验证/缺失 → 保守 legacy）。
    pub fn codec(&self, _store: &Store) -> crate::model_protocol::Codec {
        use crate::model_protocol::CapabilitySnapshot;
        self.provider
            .capability()
            .and_then(|v| serde_json::from_value::<CapabilitySnapshot>(v).ok())
            .map(|s| s.preferred_codec())
            .unwrap_or(crate::model_protocol::Codec::LegacyJson)
    }

    /// 能力快照原始 JSON（rollout 证据）。
    pub fn capability(&self, _store: &Store) -> Option<serde_json::Value> {
        self.provider.capability()
    }
}

struct CallStats<'a> {
    protocol: &'a str,
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
    /// 首 token 延迟（ms；非流式/无 delta 为 None）。
    ttft_ms: Option<i64>,
    /// M4：缓存命中 token 与 reasoning token（计量，不含正文）。
    cached_tokens: i64,
    reasoning_tokens: i64,
    /// M4：缓存域键（观测与显式键缓存）。
    prompt_cache_key: &'a str,
}

/// 流打开期失败判定（call_turn 非流式兜底的准入）：传输层/限流/打开即中断，
/// 且未产出任何 delta（已见 delta 的中断不兜底，禁止同一响应双重消费）。
fn open_phase_failure(err: &str) -> bool {
    let code = err.split(':').next().unwrap_or("");
    matches!(
        code,
        "model_unavailable" | "model_timeout" | "model_stream_interrupted"
    )
}

/// 请求侧全量脱敏（出网前）。
fn mask_request(req: &CompletionRequest) -> (CompletionRequest, usize) {
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
            ..Default::default()
        });
    }
    masked.messages = masked_messages;
    (masked, redactions)
}

/// Gateway 侧事件汇：转发 UI delta（易失）+ 供 TurnAggregator 校验（权威）。
/// reasoning delta 只计量，不转发（M2 约定：reasoning plaintext 不向 renderer 推送）。
struct GatewaySink {
    run_id: String,
    forwarder: Option<Arc<dyn TurnDeltaForwarder>>,
    agg: std::sync::Mutex<crate::model_protocol::TurnAggregator>,
    timer: std::sync::Mutex<TurnTimer>,
    reasoning_bytes: std::sync::atomic::AtomicU64,
    /// M4：reasoning plaintext 的网关内暂存（仅供加密；随 sink 丢弃）。
    reasoning_plain: std::sync::Mutex<String>,
}

impl GatewaySink {
    fn feed(&self, event: crate::model_protocol::ModelEvent) {
        let mut agg = self.agg.lock().unwrap();
        // 违规后的重复 feed 由聚合器自行拒绝；错误在 finish() 定型。
        let _ = agg.feed(event);
    }
}

impl StreamSink for GatewaySink {
    fn on_started(&self, provider_request_id: &str) {
        self.feed(crate::model_protocol::ModelEvent::Started {
            provider_request_id: provider_request_id.to_string(),
        });
    }

    fn on_text_delta(&self, text: &str) {
        self.timer.lock().unwrap().note_delta();
        if let Some(f) = self.forwarder.as_ref() {
            f.forward(&self.run_id, DeltaKind::Output, text);
        }
        self.feed(crate::model_protocol::ModelEvent::TextDelta {
            text: text.to_string(),
        });
    }

    fn on_reasoning_delta(&self, text: &str) {
        self.reasoning_bytes
            .fetch_add(text.len() as u64, std::sync::atomic::Ordering::Relaxed);
        // 不转发 renderer、不进 rollout（§5 M2）；明文仅在网关内暂存待加密（M4）。
        self.reasoning_plain.lock().unwrap().push_str(text);
    }

    fn on_tool_call_delta(&self, call_id: &str, name: &str, arguments_delta: &str) {
        self.timer.lock().unwrap().note_delta();
        if let Some(f) = self.forwarder.as_ref() {
            f.forward(&self.run_id, DeltaKind::ToolArguments, arguments_delta);
        }
        self.feed(crate::model_protocol::ModelEvent::ToolCallDelta {
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments_delta: arguments_delta.to_string(),
        });
    }

    fn on_usage(&self, usage: WireStreamUsage) {
        self.feed(crate::model_protocol::ModelEvent::Usage {
            input: usage.input,
            cached_input: usage.cached_input,
            output: usage.output,
            reasoning_output: usage.reasoning_output,
        });
    }

    fn on_completed(&self, finish_reason: &str) {
        self.feed(crate::model_protocol::ModelEvent::Completed {
            finish_reason: finish_reason.to_string(),
        });
    }
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
    let expired = match (
        sg_store::timefmt::parse(expires_at),
        sg_store::timefmt::parse(&sg_store::timefmt::now()),
    ) {
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
    if let Err(e) = record_inner(store, run_id, stats) {
        eprintln!("{{\"level\":\"warn\",\"msg\":\"model record failed: {e}\"}}");
    }
}

fn record_inner(store: &Store, run_id: &str, stats: &CallStats<'_>) -> Result<(), sg_store::Error> {
    let (protocol, provider, status, tin, tout, latency_ms, redactions) = (
        stats.protocol,
        stats.provider,
        stats.status,
        stats.tokens_in,
        stats.tokens_out,
        stats.latency_ms,
        stats.redactions,
    );
    // 单事务 + 立即写锁（缺陷审计）：model_calls/model_turns 原子落库；
    // MAX+1 取号在 BEGIN IMMEDIATE 下进行，双开进程不再撞唯一索引（run, seq）。
    store.with_tx_immediate(|conn| {
        conn.execute(
            "INSERT INTO model_calls(id, agent_run_id, provider, model, tokens_in, tokens_out, cost_micros, latency_ms, redactions, status, created_at, cached_tokens, reasoning_tokens)
             VALUES (?1,?2,?3,'default',?4,?5,0,?6,?7,?8,?9,?10,?11)",
            rusqlite::params![ids::new_id("mc"), run_id, provider, tin, tout, latency_ms, redactions as i64, status, timefmt::now(), stats.cached_tokens, stats.reasoning_tokens],
        )?;
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
        conn.execute(
            "INSERT INTO model_turns(
                id, agent_run_id, turn_seq, protocol, capability_digest, provider, model,
                tokens_in, tokens_out, cached_tokens, reasoning_tokens, ttft_ms, total_ms,
                finish_reason, status, error_code, created_at, prompt_cache_key
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            rusqlite::params![
                ids::new_id("mt"),
                run_id,
                turn_seq,
                protocol,
                probe_digest_for_model(conn, model_name),
                provider,
                model_name,
                tin,
                tout,
                stats.cached_tokens,
                stats.reasoning_tokens,
                stats.ttft_ms,
                latency_ms,
                finish,
                if status == "ok" { "ok" } else { "failed" },
                error_code,
                timefmt::now(),
                stats.prompt_cache_key,
            ],
        )?;
        Ok(())
    })
}
