use serde::{Deserialize, Serialize};
use serde_json::Value;

/// core → main 的稳定 UI 事件（F05 事件投影）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEvent {
    /// 全局递增，renderer 依此去重与补发。
    pub sequence: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workItemId: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runId: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
    /// 事件类型：context.built / plan.created / agent.started / tool.proposed /
    /// approval.requested / tool.started / tool.completed / tool.failed /
    /// artifact.revised / check.completed / gate.evaluated / agent.completed /
    /// agent.cancelled / stage.* / workitem.created …
    #[serde(rename = "type")]
    pub event_type: String,
    pub occurredAt: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub detail: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidenceIds: Vec<String>,
}

/// 通用事件 notification 载荷。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreEvent {
    pub sequence: i64,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workItemId: Option<String>,
    pub occurredAt: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub detail: Value,
}
