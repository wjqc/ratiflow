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
    // RPC 回执（RDWS 实施计划 v1.4 §1.3）：receipt 门控 mutation 的传输层拒绝码。
    IdempotencyKeyRequired,
    ReceiptFingerprintMismatch,
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
            ErrorCode::IdempotencyKeyRequired => 2001,
            ErrorCode::ReceiptFingerprintMismatch => 2002,
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
            ErrorCode::IdempotencyKeyRequired => "idempotency_key_required",
            ErrorCode::ReceiptFingerprintMismatch => "receipt_fingerprint_mismatch",
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(
            self,
            ErrorCode::ModelRateLimited | ErrorCode::GitlabUnreachable | ErrorCode::CoreUnavailable
        )
    }

    /// 数值码 → 枚举（receipt 层按码归类，不解析错误字符串）。
    pub fn from_code(code: i64) -> Option<Self> {
        use ErrorCode::*;
        Some(match code {
            -32700 => ParseError,
            -32600 => InvalidRequest,
            -32601 => MethodNotFound,
            -32602 => InvalidParams,
            -32603 => InternalError,
            1001 => NotFound,
            1002 => Unauthorized,
            1003 => Forbidden,
            1004 => Conflict,
            1101 => EtagMismatch,
            1102 => RevisionFrozen,
            1103 => InvalidStageTransition,
            1201 => ApprovalRequired,
            1202 => ApprovalInvalid,
            1203 => ApprovalExpired,
            1204 => ActionDenied,
            1301 => GitlabUnconfigured,
            1302 => GitlabUnreachable,
            1401 => ModelUnavailable,
            1402 => ModelRateLimited,
            1403 => ContextTooLarge,
            1404 => BudgetExceeded,
            1405 => ResponseInvalid,
            1501 => UnsafeExecutionDisabled,
            1502 => ManifestRejected,
            1503 => ObjectSecrets,
            1504 => VisionUnsupported,
            1505 => PathOutsideProject,
            1601 => TraceIncomplete,
            1701 => SnapshotFailed,
            1702 => RollbackDrift,
            1703 => RollbackManualActionRequired,
            1801 => AgentProfileUnavailable,
            1802 => AgentCapabilityMismatch,
            1901 => MemoryDisabled,
            1902 => MemoryConflict,
            1903 => MemorySecretDetected,
            1904 => MemoryInvalidState,
            1905 => MemoryObjectMissing,
            1906 => MemoryPurgeBlocked,
            1907 => MemoryCaptureUnknown,
            1908 => MemoryQuotaExceeded,
            2001 => IdempotencyKeyRequired,
            2002 => ReceiptFingerprintMismatch,
            -32000 => CoreUnavailable,
            _ => return None,
        })
    }

    /// 重试分类（RDWS 实施计划 v1.4 §1.3）：错误在构造处按码归类，receipt 层禁按字符串分类。
    /// Deterministic = 参数/状态/校验/策略拒绝——同请求重放必然同样失败，envelope 落 completed 可重放；
    /// Transient = IO/内部/网络/超时——原子落 retryable_failed 释放执行语义，后续请求按 CAS 重新认领。
    /// 未知码（未登记的内部码）按 Transient 处理：IO/内部错误是 receipt 层的默认面。
    pub fn err_class(&self) -> ErrClass {
        use ErrorCode::*;
        match self {
            InternalError | ModelUnavailable | ModelRateLimited | GitlabUnreachable
            | CoreUnavailable | SnapshotFailed => ErrClass::Transient,
            _ => ErrClass::Deterministic,
        }
    }
}

/// 错误重试类别（sg_protocol 层标注，见 `ErrorCode::err_class`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrClass {
    Deterministic,
    Transient,
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

    /// 按 numeric code 归类（未知码 → Transient，与 `ErrorCode::err_class` 一致）。
    pub fn err_class(&self) -> ErrClass {
        ErrorCode::from_code(self.code)
            .map(|k| k.err_class())
            .unwrap_or(ErrClass::Transient)
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
