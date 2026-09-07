//! 受控 MCP ToolProvider 客户端（ADR-035 / Codex 能力差距方案 M6）。
//!
//! 边界（方案 §5 M6）：MCP 只是 ToolProvider adapter——服务器进程由本模块按
//! **每次调用拉起**（进程级隔离，server 重启语义天然成立；长驻连接为后续扩展），
//! 注册/审批/撤销与风险映射在 settings/core 层。远端（https）传输本构建未启用，
//! 显式 fail-closed：远端执行不受本机沙箱保护，接入前须有独立评审。
//!
//! 协议：JSON-RPC 2.0，stdio 按行分帧（MCP 规范）。initialize → notifications/
//! initialized → tools/list → tools/call。

pub mod client;
pub mod types;

pub use client::{McpClient, McpTransport, SandboxedTransport, ScriptedTransport, StdioTransport};
pub use types::{
    canonical_schema, valid_tool_name, McpServerInfo, McpToolCallOutcome, McpToolDescriptor,
};
