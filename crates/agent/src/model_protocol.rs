//! 统一模型交互协议类型与事件聚合器（ADR-033 / Codex 能力差距方案 M0 §4.2）。
//!
//! M0 只定义类型与聚合器，**不切生产路径**：现有 Run 循环仍走 legacy_json 正文协议。
//! M1 起 `ModelTurn` 的 ToolCall/Text 分支替换 `parse_decision`，codec 由 Run 冻结的
//! 能力快照决定。Agent 不解析 SSE/品牌字段——线协议转换属于 Provider adapter。

use serde::{Deserialize, Serialize};
use sg_store::ids;
use sha2::{Digest, Sha256};

/// Provider 能力快照消费方类型（与 sg-settings 探测写入的 §4.1 JSON 同形）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CapabilitySnapshot {
    #[serde(rename = "schemaVersion", default = "default_schema_version")]
    pub schema_version: i64,
    #[serde(default = "default_protocols")]
    pub protocols: Vec<String>,
    #[serde(rename = "preferredProtocol", default = "default_preferred")]
    pub preferred_protocol: String,
    #[serde(rename = "nativeTools", default)]
    pub native_tools: serde_json::Value,
    #[serde(rename = "streamText", default)]
    pub stream_text: serde_json::Value,
    #[serde(rename = "streamToolArguments", default)]
    pub stream_tool_arguments: serde_json::Value,
    #[serde(rename = "reasoningTransport", default = "default_reasoning_transport")]
    pub reasoning_transport: String,
    #[serde(default = "default_compaction")]
    pub compaction: String,
    #[serde(default = "default_cache")]
    pub cache: String,
    #[serde(rename = "serverState", default = "default_server_state")]
    pub server_state: String,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(rename = "verifiedAt", default)]
    pub verified_at: String,
    #[serde(rename = "expiresAt", default)]
    pub expires_at: String,
    #[serde(default)]
    pub digest: String,
}

fn default_schema_version() -> i64 {
    1
}
fn default_protocols() -> Vec<String> {
    vec!["chat_completions".into()]
}
fn default_preferred() -> String {
    "chat_completions".into()
}
fn default_reasoning_transport() -> String {
    "none".into()
}
fn default_compaction() -> String {
    "local_structured".into()
}
fn default_cache() -> String {
    "implicit".into()
}
fn default_server_state() -> String {
    "unsupported".into()
}
fn default_source() -> String {
    "preset".into()
}

impl Default for CapabilitySnapshot {
    fn default() -> Self {
        serde_json::from_value(serde_json::json!({})).unwrap()
    }
}

impl CapabilitySnapshot {
    /// M0/M1 codec 选择：仅 nativeTools 显式 true（probe/manual 已验证）才走原生协议，
    /// 其余（false/unknown/未验证）一律保守回退 legacy_json（ADR-033 决策 2/3）。
    pub fn preferred_codec(&self) -> Codec {
        if self.verified() && self.native_tools == serde_json::json!(true) {
            Codec::NativeTools
        } else {
            Codec::LegacyJson
        }
    }

    fn verified(&self) -> bool {
        if self.source == "manual" {
            return true;
        }
        self.source == "probe"
            && !self.verified_at.is_empty()
            && !self.expires_at.is_empty()
            && match (
                sg_store::timefmt::parse(&self.expires_at),
                sg_store::timefmt::parse(&self.verified_at),
            ) {
                (Some(exp), Some(v)) => {
                    exp > v
                        && exp.unix_timestamp()
                            > std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs() as i64)
                                .unwrap_or(i64::MAX)
                }
                _ => false,
            }
    }
}

/// 模型交互 codec（Run 冻结快照决定；M0 生产路径恒为 LegacyJson）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    LegacyJson,
    NativeTools,
}

/// 统一模型输入项（§4.2）；checkpoint 升级为 versioned ModelInputItem[] 是 M4 工作。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelInputItem {
    Message {
        role: String,
        content: String,
    },
    ToolCall {
        call_id: String,
        name: String,
        arguments_json: String,
    },
    ToolResult {
        call_id: String,
        output: String,
    },
    ReasoningOpaque {
        provider: String,
        payload: String,
    },
    CompactionOpaque {
        provider: String,
        payload: String,
    },
}

/// 统一模型事件（Provider adapter 产出；Agent 不解析 SSE/品牌字段）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelEvent {
    Started {
        provider_request_id: String,
    },
    TextDelta {
        text: String,
    },
    ReasoningSummaryDelta {
        text: String,
    },
    ToolCallDelta {
        call_id: String,
        name: String,
        arguments_delta: String,
    },
    ItemCompleted {
        item: ModelInputItem,
    },
    Usage {
        input: i64,
        cached_input: i64,
        output: i64,
        reasoning_output: i64,
    },
    Completed {
        finish_reason: String,
    },
}

/// 一轮模型交互的最终产物（由事件聚合器产生；Agent 只消费 ModelTurn）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelTurn {
    pub provider_request_id: String,
    /// 最终正文（TextDelta 聚合）。
    pub text: String,
    /// reasoning summary（可展示部分；正文永不落普通 rollout）。
    pub reasoning_summary: String,
    /// 完成的工具调用（arguments_delta 聚合后的完整 JSON 串）。
    pub tool_calls: Vec<ToolCallComplete>,
    pub usage: Usage,
    pub finish_reason: String,
    /// 流中断且未收到完成帧时为 true：已显示 delta 不得当最终答案（§4.3）。
    pub incomplete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallComplete {
    pub call_id: String,
    pub name: String,
    pub arguments_json: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub input: i64,
    pub cached_input: i64,
    pub output: i64,
    pub reasoning_output: i64,
    /// WP-1 计量语义（RDWS-003）：Provider 是否实际下发过 usage 事件——
    /// 缺失时数值不可作为 settle 实际量（估算值只服务 budget，不进 ledger）。
    pub measured: bool,
}

/// 协议错误终态（§4.3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelProtocolError {
    /// Profile 要求的能力未验证，非重试型。
    CapabilityMissing,
    /// tool call/transcript/delta 不满足内部契约（同一响应不可双重消费）。
    ProtocolViolation(String),
    /// 流中断且未收到完成帧 → 轮次 incomplete。
    StreamInterrupted,
    /// 本地 abort（Provider 是否继续计费未知 → 成本 unknown，不重放）。
    Cancelled,
}

/// 事件 → ModelTurn 聚合器。大小上限与 UTF-8/delta 聚合的计量在 Gateway chunk 层
/// （M2）；本聚合器只负责把事件流变成最终轮次，并执行协议判定。
#[derive(Debug, Default)]
pub struct TurnAggregator {
    provider_request_id: String,
    text: String,
    reasoning_summary: String,
    usage: Usage,
    finish_reason: String,
    tool_calls: Vec<ToolCallComplete>,
    tool_call_deltas: std::collections::HashMap<String, (String, String)>,
    completed: bool,
    cancelled: bool,
    violation: Option<String>,
}

impl TurnAggregator {
    pub fn new() -> Self {
        Self::default()
    }

    /// 消费一个事件；返回 Err 即协议违规/取消终态，调用方不得继续消费同一响应。
    pub fn feed(&mut self, event: ModelEvent) -> Result<(), ModelProtocolError> {
        if self.completed || self.cancelled || self.violation.is_some() {
            return Err(ModelProtocolError::ProtocolViolation(
                "completed/cancelled/violation 之后仍收到事件".into(),
            ));
        }
        match event {
            ModelEvent::Started {
                provider_request_id,
            } => {
                self.provider_request_id = provider_request_id;
            }
            ModelEvent::TextDelta { text } => self.text.push_str(&text),
            ModelEvent::ReasoningSummaryDelta { text } => self.reasoning_summary.push_str(&text),
            ModelEvent::ToolCallDelta {
                call_id,
                name,
                arguments_delta,
            } => {
                // call_id 首次出现时记录工具名；后续分片只拼 arguments。
                let entry = self
                    .tool_call_deltas
                    .entry(call_id.clone())
                    .or_insert_with(|| (name.clone(), String::new()));
                if entry.0 != name {
                    self.violation = Some(format!("call_id {call_id} 工具名漂移"));
                    return Err(ModelProtocolError::ProtocolViolation(
                        self.violation.clone().unwrap(),
                    ));
                }
                entry.1.push_str(&arguments_delta);
            }
            ModelEvent::ItemCompleted { item } => match item {
                ModelInputItem::ToolCall {
                    call_id,
                    name,
                    arguments_json,
                } => {
                    // 完成帧与 delta 聚合互斥：同一响应不可双重消费同一 call（§4.3）。
                    if self.tool_call_deltas.contains_key(&call_id) {
                        self.violation = Some(format!("call_id {call_id} 双重消费"));
                        return Err(ModelProtocolError::ProtocolViolation(
                            self.violation.clone().unwrap(),
                        ));
                    }
                    self.tool_call_deltas
                        .insert(call_id.clone(), (name, arguments_json));
                }
                ModelInputItem::Message { content, .. } => self.text.push_str(&content),
                ModelInputItem::ToolResult { .. } => {
                    // ToolResult 是请求侧输入项；完成帧中不应出现，出现即协议违规。
                    self.violation = Some("ItemCompleted 携带 ToolResult".into());
                    return Err(ModelProtocolError::ProtocolViolation(
                        self.violation.clone().unwrap(),
                    ));
                }
                // reasoning/compaction opaque payload 只透传给 checkpoint 层（M4），
                // M0 聚合器不消费。
                ModelInputItem::ReasoningOpaque { .. }
                | ModelInputItem::CompactionOpaque { .. } => {}
            },
            ModelEvent::Usage {
                input,
                cached_input,
                output,
                reasoning_output,
            } => {
                self.usage = Usage {
                    input,
                    cached_input,
                    output,
                    reasoning_output,
                    measured: true,
                };
            }
            ModelEvent::Completed { finish_reason } => {
                self.finish_reason = finish_reason;
                self.completed = true;
            }
        }
        Ok(())
    }

    /// 本地取消：已收 delta 作废，不产生假终态。
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    /// 终结聚合：取消 → Cancelled；有违规 → ProtocolViolation；
    /// 未收到完成帧 → StreamInterrupted（turn.incomplete=true 的来源）。
    pub fn finish(self) -> Result<ModelTurn, ModelProtocolError> {
        if self.cancelled {
            return Err(ModelProtocolError::Cancelled);
        }
        if let Some(v) = self.violation {
            return Err(ModelProtocolError::ProtocolViolation(v));
        }
        let incomplete = !self.completed;
        if incomplete {
            return Err(ModelProtocolError::StreamInterrupted);
        }
        // 并行工具调用首期拒绝（ADR-033 决策 6）：同一轮 >1 个 tool call 视为协议违规。
        let mut tool_calls: Vec<ToolCallComplete> = self
            .tool_call_deltas
            .into_iter()
            .map(|(call_id, (name, arguments_json))| ToolCallComplete {
                call_id,
                name,
                arguments_json,
            })
            .collect();
        tool_calls.sort_by(|a, b| a.call_id.cmp(&b.call_id));
        let _ = &self.tool_calls; // 聚合来源（deltas）；保留字段供调试断言
        if tool_calls.len() > 1 {
            return Err(ModelProtocolError::ProtocolViolation(format!(
                "并行工具调用 {} 个，首期拒绝",
                tool_calls.len()
            )));
        }
        Ok(ModelTurn {
            provider_request_id: self.provider_request_id,
            text: self.text,
            reasoning_summary: self.reasoning_summary,
            tool_calls,
            usage: self.usage,
            finish_reason: self.finish_reason,
            incomplete,
        })
    }
}

/// 工具提案指纹（M1 启用；M0 先冻结定义，避免重连/重放创建重复 Proposal）：
/// run_id + turn_seq + call_id + tool_schema_digest + canonical_arguments。
pub fn proposal_fingerprint(
    run_id: &str,
    turn_seq: i64,
    call_id: &str,
    tool_schema_digest: &str,
    canonical_arguments: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(run_id.as_bytes());
    hasher.update(turn_seq.to_le_bytes());
    hasher.update(call_id.as_bytes());
    hasher.update(tool_schema_digest.as_bytes());
    hasher.update(canonical_arguments.as_bytes());
    ids::hex(&hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_turn_aggregation() {
        let mut agg = TurnAggregator::new();
        agg.feed(ModelEvent::Started {
            provider_request_id: "req-1".into(),
        })
        .unwrap();
        agg.feed(ModelEvent::TextDelta { text: "你".into() })
            .unwrap();
        agg.feed(ModelEvent::TextDelta { text: "好".into() })
            .unwrap();
        agg.feed(ModelEvent::Usage {
            input: 10,
            cached_input: 4,
            output: 2,
            reasoning_output: 0,
        })
        .unwrap();
        agg.feed(ModelEvent::Completed {
            finish_reason: "stop".into(),
        })
        .unwrap();
        let turn = agg.finish().unwrap();
        assert_eq!(turn.text, "你好");
        assert!(!turn.incomplete);
        assert_eq!(turn.usage.cached_input, 4);
        assert_eq!(turn.finish_reason, "stop");
    }

    #[test]
    fn tool_call_delta_assembly() {
        let mut agg = TurnAggregator::new();
        agg.feed(ModelEvent::ToolCallDelta {
            call_id: "c1".into(),
            name: "read_file".into(),
            arguments_delta: "{\"path\":".into(),
        })
        .unwrap();
        agg.feed(ModelEvent::ToolCallDelta {
            call_id: "c1".into(),
            name: "read_file".into(),
            arguments_delta: "\"a.md\"}".into(),
        })
        .unwrap();
        agg.feed(ModelEvent::Completed {
            finish_reason: "tool_calls".into(),
        })
        .unwrap();
        let turn = agg.finish().unwrap();
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].arguments_json, "{\"path\":\"a.md\"}");
        assert_eq!(turn.tool_calls[0].name, "read_file");
    }

    #[test]
    fn parallel_tool_calls_rejected() {
        let mut agg = TurnAggregator::new();
        for cid in ["c1", "c2"] {
            agg.feed(ModelEvent::ItemCompleted {
                item: ModelInputItem::ToolCall {
                    call_id: cid.into(),
                    name: "read_file".into(),
                    arguments_json: "{}".into(),
                },
            })
            .unwrap();
        }
        agg.feed(ModelEvent::Completed {
            finish_reason: "tool_calls".into(),
        })
        .unwrap();
        assert!(matches!(
            agg.finish(),
            Err(ModelProtocolError::ProtocolViolation(_))
        ));
    }

    #[test]
    fn stream_interrupted_without_completed() {
        let mut agg = TurnAggregator::new();
        agg.feed(ModelEvent::TextDelta {
            text: "半截".into(),
        })
        .unwrap();
        assert_eq!(agg.finish(), Err(ModelProtocolError::StreamInterrupted));
    }

    #[test]
    fn cancel_discards_deltas() {
        let mut agg = TurnAggregator::new();
        agg.feed(ModelEvent::TextDelta {
            text: "部分".into(),
        })
        .unwrap();
        agg.cancel();
        assert_eq!(agg.finish(), Err(ModelProtocolError::Cancelled));
    }

    #[test]
    fn completed_then_event_is_violation() {
        let mut agg = TurnAggregator::new();
        agg.feed(ModelEvent::Completed {
            finish_reason: "stop".into(),
        })
        .unwrap();
        assert!(matches!(
            agg.feed(ModelEvent::TextDelta { text: "x".into() }),
            Err(ModelProtocolError::ProtocolViolation(_))
        ));
    }

    #[test]
    fn codec_selection_conservative_by_default() {
        // 未探测（preset）/unknown → legacy；显式 true 且已验证 → native。
        let unverified = CapabilitySnapshot::default();
        assert_eq!(unverified.preferred_codec(), Codec::LegacyJson);
        let mut native = CapabilitySnapshot {
            source: "probe".into(),
            verified_at: "2026-09-04T00:00:00.000Z".into(),
            expires_at: "2999-01-01T00:00:00.000Z".into(),
            native_tools: serde_json::json!(true),
            ..Default::default()
        };
        assert_eq!(native.preferred_codec(), Codec::NativeTools);
        // 过期 probe → 回退 legacy。
        native.expires_at = "2020-01-01T00:00:00.000Z".into();
        assert_eq!(native.preferred_codec(), Codec::LegacyJson);
    }

    #[test]
    fn proposal_fingerprint_is_stable_and_input_sensitive() {
        let a = proposal_fingerprint("run1", 1, "c1", "digest", "{\"a\":1}");
        let b = proposal_fingerprint("run1", 1, "c1", "digest", "{\"a\":1}");
        let c = proposal_fingerprint("run1", 2, "c1", "digest", "{\"a\":1}");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}

/// P1-2（评审修复）：未收到 Usage 事件的轮次 measured=false——
/// 数值不可作 ledger settle 实际量（settle None 进 reconciliation）。
#[test]
fn usage_missing_marks_unmeasured() {
    let mut agg = TurnAggregator::new();
    agg.feed(ModelEvent::TextDelta { text: "ok".into() })
        .unwrap();
    agg.feed(ModelEvent::Completed {
        finish_reason: "stop".into(),
    })
    .unwrap();
    let turn = agg.finish().unwrap();
    assert!(!turn.usage.measured, "缺 Usage 事件 → measured=false");

    let mut agg2 = TurnAggregator::new();
    agg2.feed(ModelEvent::Usage {
        input: 7,
        cached_input: 0,
        output: 3,
        reasoning_output: 0,
    })
    .unwrap();
    agg2.feed(ModelEvent::Completed {
        finish_reason: "stop".into(),
    })
    .unwrap();
    assert!(
        agg2.finish().unwrap().usage.measured,
        "收到 Usage 事件 → measured=true"
    );
}
