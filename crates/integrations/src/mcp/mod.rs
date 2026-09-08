//! 受控 MCP ToolProvider 客户端（ADR-035 / Codex 能力差距方案 M6）。
//!
//! 边界（方案 §5 M6）：MCP 只是 ToolProvider adapter——服务器连接由本模块按
//! **每次调用建立**（stdio=进程级隔离、远端=按调用建连；server 重启语义天然
//! 成立；长驻连接为后续扩展），注册/审批/撤销与风险映射在 settings/core 层。
//! 远程传输（sse / streamable-http）不经本机内核沙箱（无本地进程）：治理依赖
//! 注册审批 + URL 冻结 + 静态头存储（见 remote.rs）。
//!
//! 协议：JSON-RPC 2.0。initialize → notifications/initialized → tools/list →
//! tools/call。stdio 按行分帧；远程见 remote.rs（SSE / Streamable HTTP）。

pub mod client;
pub mod remote;
pub mod types;

pub use client::{McpClient, McpTransport, SandboxedTransport, ScriptedTransport, StdioTransport};
pub use remote::{SseWire, StreamableHttpWire};
pub use types::{
    canonical_schema, valid_tool_name, McpServerInfo, McpToolCallOutcome, McpToolDescriptor,
};
