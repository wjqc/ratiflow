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
    TraceIncomplete,
    SnapshotFailed,
    RollbackDrift,
    RollbackManualActionRequired,
    AgentProfileUnavailable,
    AgentCapabilityMismatch,
    // 项目记忆（ADR-032 / 实施方案 v1.0 §9.3 MEMORY family）
    MemoryDisabled,
    MemoryConflict,
    MemorySecretDetected,
    MemoryInvalidState,
    MemoryObjectMissing,
    MemoryPurgeBlocked,
    MemoryCaptureUnknown,
    MemoryQuotaExceeded,
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
            ErrorCode::TraceIncomplete => 1601,
            ErrorCode::SnapshotFailed => 1701,
            ErrorCode::RollbackDrift => 1702,
            ErrorCode::RollbackManualActionRequired => 1703,
            ErrorCode::AgentProfileUnavailable => 1801,
            ErrorCode::AgentCapabilityMismatch => 1802,
            ErrorCode::MemoryDisabled => 1901,
            ErrorCode::MemoryConflict => 1902,
            ErrorCode::MemorySecretDetected => 1903,
            ErrorCode::MemoryInvalidState => 1904,
            ErrorCode::MemoryObjectMissing => 1905,
            ErrorCode::MemoryPurgeBlocked => 1906,
            ErrorCode::MemoryCaptureUnknown => 1907,
            ErrorCode::MemoryQuotaExceeded => 1908,
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
            ErrorCode::TraceIncomplete => "trace_incomplete",
            ErrorCode::SnapshotFailed => "snapshot_failed",
            ErrorCode::RollbackDrift => "rollback_drift",
            ErrorCode::RollbackManualActionRequired => "rollback_manual_action_required",
            ErrorCode::AgentProfileUnavailable => "agent_profile_unavailable",
            ErrorCode::AgentCapabilityMismatch => "agent_capability_mismatch",
            ErrorCode::MemoryDisabled => "memory_disabled",
            ErrorCode::MemoryConflict => "memory_conflict",
            ErrorCode::MemorySecretDetected => "memory_secret_detected",
            ErrorCode::MemoryInvalidState => "memory_invalid_state",
            ErrorCode::MemoryObjectMissing => "memory_object_missing",
            ErrorCode::MemoryPurgeBlocked => "memory_purge_blocked",
            ErrorCode::MemoryCaptureUnknown => "memory_capture_unknown",
            ErrorCode::MemoryQuotaExceeded => "memory_quota_exceeded",
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
        Self {
            code: kind.code(),
            message: kind.name().to_string(),
            retryable: kind.retryable(),
            data: Some(serde_json::json!({ "detail": safe_message.into() })),
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({}): {}",
            self.code,
            self.message,
            self.data
                .as_ref()
                .map(|d| d.to_string())
                .unwrap_or_default()
        )
    }
}

impl std::error::Error for RpcError {}
