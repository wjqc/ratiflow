//! JSON-RPC 2.0 协议类型：Electron main 与 Rust sidecar 之间的唯一契约。
//! 一行一个 UTF-8 JSON 对象（stdio 分帧）；消息上限 8 MiB。

pub mod error;
pub mod event;
pub mod rpc;

use serde::{Deserialize, Serialize};

/// 当前协议版本（握手不兼容时 main 必须显示诊断/升级界面，不创建业务窗口）。
pub const PROTOCOL_VERSION: &str = "1";
/// 单条 JSON-RPC 消息的字节上限。
pub const MAX_MESSAGE_BYTES: usize = 8 << 20;

/// sidecar 启动后 core 首先发出的 hello 握手。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocolVersion: String,
    pub coreVersion: String,
    pub schemaVersion: i64,
    pub capabilities: Vec<String>,
}

/// 与 Hello 兼容性判定。
pub fn hello_compatible(hello: &Hello) -> bool {
    hello.protocolVersion == PROTOCOL_VERSION
}

pub use error::{ErrorCode, RpcError};
pub use rpc::{err_response, event_notification, ok_response};
pub use event::{CoreEvent, TimelineEvent};
pub use rpc::{Notification, Request, Response, RpcMessage};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_response_roundtrip() {
        let req = Request {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(7)),
            method: "workitem.get".into(),
            params: Some(serde_json::json!({"projectId": "pj_1"})),
        };
        let text = serde_json::to_string(&RpcMessage::Request(req)).unwrap();
        let parsed: RpcMessage = serde_json::from_str(&text).unwrap();
        match parsed {
            RpcMessage::Request(r) => assert_eq!(r.method, "workitem.get"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn hello_compatibility() {
        let ok = Hello {
            protocolVersion: PROTOCOL_VERSION.into(),
            coreVersion: "0.1.0".into(),
            schemaVersion: 15,
            capabilities: vec![],
        };
        assert!(hello_compatible(&ok));
        let mut bad = ok.clone();
        bad.protocolVersion = "0".into();
        assert!(!hello_compatible(&bad));
    }
}
