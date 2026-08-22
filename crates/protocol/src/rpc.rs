use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 2.0 请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// JSON-RPC 2.0 成功/失败响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::RpcError>,
}

/// JSON-RPC notification（无 id；事件流用 method="event"）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// stdio 上传输的三种消息。
#[derive(Debug, Clone)]
pub enum RpcMessage {
    Request(Request),
    Response(Response),
    Notification(Notification),
}

impl<'de> Deserialize<'de> for RpcMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let is_request = value.get("method").is_some();
        let has_id = value.get("id").is_some();
        if is_request {
            if has_id {
                Ok(RpcMessage::Request(
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?,
                ))
            } else {
                Ok(RpcMessage::Notification(
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?,
                ))
            }
        } else {
            Ok(RpcMessage::Response(
                serde_json::from_value(value).map_err(serde::de::Error::custom)?,
            ))
        }
    }
}

impl Serialize for RpcMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            RpcMessage::Request(r) => r.serialize(serializer),
            RpcMessage::Response(r) => r.serialize(serializer),
            RpcMessage::Notification(n) => n.serialize(serializer),
        }
    }
}

impl RpcMessage {
    pub fn to_line(&self) -> String {
        match serde_json::to_string(self) {
            Ok(text) => text,
            Err(err) => format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{{\"code\":-32603,\"message\":\"serialize: {err}\"}}}}"
            ),
        }
    }
}

/// 便捷构造：带 id 的请求。
pub fn request(id: Value, method: &str, params: Value) -> Request {
    Request { jsonrpc: "2.0".into(), id: Some(id), method: method.into(), params: Some(params) }
}

impl Response {
    /// 单行 JSON（stdio 分帧）。
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            "{{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{{\"code\":-32603,\"message\":\"serialize failed\"}}}}".into()
        })
    }
}

/// 便捷构造：成功响应。
pub fn ok_response(id: Option<Value>, result: Value) -> Response {
    Response { jsonrpc: "2.0".into(), id, result: Some(result), error: None }
}

/// 便捷构造：失败响应。
pub fn err_response(id: Option<Value>, error: crate::RpcError) -> Response {
    Response { jsonrpc: "2.0".into(), id, result: None, error: Some(error) }
}

impl Notification {
    /// 单行 JSON（stdio 分帧）。
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// 便捷构造：事件 notification。
pub fn event_notification(params: Value) -> Notification {
    Notification { jsonrpc: "2.0".into(), method: "event".into(), params: Some(params) }
}
