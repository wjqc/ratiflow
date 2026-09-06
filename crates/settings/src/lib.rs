#![allow(clippy::result_large_err)] // 领域错误在 RPC 边界统一装箱；crate 内直传避免到处 Box

//! 设置域服务（ZCode 手册 §5/§7/§9）：键值设置、凭据引用（Keychain）、
//! Profile 注册表、策略解析、备份恢复与长操作记录。
//! 业务规则在本 crate；dispatch 只做参数转换。

pub mod audit_ext;
pub mod backup_ext;
pub mod capability;
pub mod credentials;
pub mod executor_ext;
pub mod knowledge_defaults;
pub mod mcp_ext;
pub mod operations;
pub mod policy_ext;
pub mod profiles;
pub mod settings;
pub mod skill_registry;
pub mod skills_ext;
#[cfg(test)]
mod tests;

use sg_store::Error;

/// 设置域稳定错误码（映射 sg_protocol::ErrorCode；字符串用于存储层比较）。
pub mod codes {
    pub const REVISION_CONFLICT: &str = "REVISION_CONFLICT";
    pub const MANAGED_READ_ONLY: &str = "MANAGED_READ_ONLY";
    pub const CREDENTIAL_MISSING: &str = "CREDENTIAL_MISSING";
    pub const CREDENTIAL_STORE_UNAVAILABLE: &str = "CREDENTIAL_STORE_UNAVAILABLE";
    pub const BACKUP_INCOMPATIBLE: &str = "BACKUP_INCOMPATIBLE";
    pub const BACKUP_CORRUPT: &str = "BACKUP_CORRUPT";
}

/// 领域错误：code 为稳定字符串，message 面向日志（UI 文案由前端 i18n 映射）。
#[derive(Debug)]
pub struct SettingsError {
    pub code: &'static str,
    pub message: String,
    pub field_errors: Option<serde_json::Value>,
    pub correlation_id: Option<String>,
    pub details: Option<serde_json::Value>,
}

impl SettingsError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            field_errors: None,
            correlation_id: None,
            details: None,
        }
    }
    pub fn with_fields(mut self, fields: serde_json::Value) -> Self {
        self.field_errors = Some(fields);
        self
    }
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

pub type SettingsResult<T> = Result<T, SettingsError>;

pub fn store_err(e: Error) -> SettingsError {
    SettingsError::new("INTERNAL", e.to_string())
}

/// correlationId 贯穿：请求层生成并写入审计/错误。
pub fn new_correlation_id() -> String {
    sg_store::ids::new_id("corr")
}
