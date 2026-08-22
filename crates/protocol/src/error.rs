use serde::{Deserialize, Serialize};

/// 稳定错误码（renderer 只见 code + safe message + retryable；原始错误进 core 日志）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorCode {
    ParseError,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    InternalError,
    // 领域稳定码（与 v2 Go 基线对齐）
    NotFound,
    Unauthorized,
    Forbidden,
    Conflict,
    EtagMismatch,
    RevisionFrozen,
    InvalidStageTransition,
    ApprovalRequired,
    ApprovalInvalid,
    ApprovalExpired,
    ActionDenied,
    GitlabUnconfigured,
    GitlabUnreachable,
    ModelUnavailable,
    ModelRateLimited,
    ContextTooLarge,
    BudgetExceeded,
    ResponseInvalid,
    UnsafeExecutionDisabled,
    ManifestRejected,
    ObjectSecrets,
    VisionUnsupported,
    PathOutsideProject,
    CoreUnavailable,
}

impl ErrorCode {
    pub fn code(&self) -> i64 {
        match self {
            ErrorCode::ParseError => -32700,
            ErrorCode::InvalidRequest => -32600,
            ErrorCode::MethodNotFound => -32601,
            ErrorCode::InvalidParams => -32602,
            ErrorCode::InternalError => -32603,
            ErrorCode::NotFound => 1001,
            ErrorCode::Unauthorized => 1002,
            ErrorCode::Forbidden => 1003,
            ErrorCode::Conflict => 1004,
            ErrorCode::EtagMismatch => 1101,
            ErrorCode::RevisionFrozen => 1102,
            ErrorCode::InvalidStageTransition => 1103,
            ErrorCode::ApprovalRequired => 1201,
            ErrorCode::ApprovalInvalid => 1202,
            ErrorCode::ApprovalExpired => 1203,
            ErrorCode::ActionDenied => 1204,
            ErrorCode::GitlabUnconfigured => 1301,
            ErrorCode::GitlabUnreachable => 1302,
            ErrorCode::ModelUnavailable => 1401,
            ErrorCode::ModelRateLimited => 1402,
            ErrorCode::ContextTooLarge => 1403,
            ErrorCode::BudgetExceeded => 1404,
            ErrorCode::ResponseInvalid => 1405,
            ErrorCode::UnsafeExecutionDisabled => 1501,
            ErrorCode::ManifestRejected => 1502,
            ErrorCode::ObjectSecrets => 1503,
            ErrorCode::VisionUnsupported => 1504,
            ErrorCode::PathOutsideProject => 1505,
            ErrorCode::CoreUnavailable => -32000,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            ErrorCode::ParseError => "parse_error",
            ErrorCode::InvalidRequest => "invalid_request",
            ErrorCode::MethodNotFound => "method_not_found",
            ErrorCode::InvalidParams => "invalid_params",
            ErrorCode::InternalError => "internal_error",
            ErrorCode::NotFound => "not_found",
            ErrorCode::Unauthorized => "unauthorized",
            ErrorCode::Forbidden => "forbidden",
            ErrorCode::Conflict => "conflict",
            ErrorCode::EtagMismatch => "etag_mismatch",
            ErrorCode::RevisionFrozen => "revision_frozen",
            ErrorCode::InvalidStageTransition => "invalid_stage_transition",
            ErrorCode::ApprovalRequired => "approval_required",
            ErrorCode::ApprovalInvalid => "approval_invalid",
            ErrorCode::ApprovalExpired => "approval_expired",
            ErrorCode::ActionDenied => "action_denied",
            ErrorCode::GitlabUnconfigured => "gitlab_unconfigured",
            ErrorCode::GitlabUnreachable => "gitlab_unreachable",
            ErrorCode::ModelUnavailable => "model_unavailable",
            ErrorCode::ModelRateLimited => "model_rate_limited",
            ErrorCode::ContextTooLarge => "context_too_large",
            ErrorCode::BudgetExceeded => "budget_exceeded",
            ErrorCode::ResponseInvalid => "response_invalid",
            ErrorCode::UnsafeExecutionDisabled => "unsafe_execution_disabled",
            ErrorCode::ManifestRejected => "manifest_rejected",
            ErrorCode::ObjectSecrets => "object_contains_secrets",
            ErrorCode::VisionUnsupported => "vision_unsupported",
            ErrorCode::PathOutsideProject => "path_outside_project",
            ErrorCode::CoreUnavailable => "core_unavailable",
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(
            self,
            ErrorCode::ModelRateLimited | ErrorCode::GitlabUnreachable | ErrorCode::CoreUnavailable
        )
    }
}

/// JSON-RPC error 对象。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl RpcError {
    pub fn new(kind: ErrorCode, safe_message: impl Into<String>) -> Self {
        Self { code: kind.code(), message: kind.name().to_string(), retryable: kind.retryable(), data: Some(serde_json::json!({ "detail": safe_message.into() })) }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}): {}", self.code, self.message, self.data.as_ref().map(|d| d.to_string()).unwrap_or_default())
    }
}

impl std::error::Error for RpcError {}
