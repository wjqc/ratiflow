//! 跨机事件权威底座（ADR-031 r5 契约的参考实现）：
//! 一事件一文件（ULID 命名，并行追加无文本冲突）；reducer 为确定性纯函数；
//! 三层投影——事实（仅声明）、发布（事件+见证）、有效治理（按模式化谓词取证）；
//! 一切不可验证均 fail-closed，不产半投影。
//!
//! 本 crate 不依赖 sg-store/SQLite：事件流是唯一权威，SQLite 只是可丢弃投影。

pub mod dag;
pub mod publish;
pub mod reducer;
pub mod store;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 信封 schema 首版（ADR-031 C2；字段冻结前仅允许追加可选字段）。
pub const SCHEMA_VERSION: u32 = 1;

/// 首事件 parentHead 根值（r3：根事件豁免可达性检查）。
pub const ROOT_PARENT: &str = "ROOT";

/// 放行 digest：复用 crates/workitem/src/release.rs 六分量定义，不另造事实（ADR-031 C1）。
pub fn release_digest(
    workitem_id: &str,
    gate: &str,
    attempt_id: &str,
    entry_snapshot: &str,
    manifest_sha: &str,
    policy_version: &str,
) -> String {
    let mut hasher = Sha256::new();
    for part in [
        workitem_id,
        gate,
        attempt_id,
        entry_snapshot,
        manifest_sha,
        policy_version,
    ] {
        hasher.update(part.as_bytes());
        hasher.update(b"|");
    }
    hex(&hasher.finalize())
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

// --- ULID（Crockford Base32，48bit 毫秒时间戳 + 80bit 随机）---

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// 生成 26 字符 ULID。仅作事件标识与同层并列平局键，**不作因果序**（ADR-031 C2 r3）。
pub fn new_ulid() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut rand = [0u8; 10];
    getrandom::getrandom(&mut rand).expect("系统熵源不可用");
    let mut be = [0u8; 16];
    be[6..].copy_from_slice(&rand); // 80bit 随机段对齐到低位
    let value = (ms << 80) | u128::from_be_bytes(be);
    encode_ulid(value)
}

pub fn encode_ulid(value: u128) -> String {
    let mut out = [b'0'; 26];
    let mut v = value;
    for i in (0..26).rev() {
        out[i] = CROCKFORD[(v & 0x1f) as usize];
        v >>= 5;
    }
    String::from_utf8(out.to_vec()).expect("Crockford 字母表恒为 ASCII")
}

pub fn is_ulid(s: &str) -> bool {
    s.len() == 26
        && s.bytes()
            .all(|b| CROCKFORD.contains(&b.to_ascii_uppercase()))
}

// --- 事件信封与载荷（ADR-031 C2）---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    AttemptStarted,
    OutputFrozen,
    ReleaseRequested,
    ReleaseDecided,
    StageTransition,
    PlanFrozen,
    ForkResolved,
    Supersede,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approved,
    Rejected,
}

/// 信任级别（ADR-031 C4 r4，四级）。审计级永不作放行条件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    /// 本机低信任放行：仅 local-only + self_allowed；trust_level=local 即该模式有效性凭证。
    Local,
    /// 审计信息，仅供追溯。
    Audit,
    /// 协作者自审：经验证的服务端写权限即可放行（repository-backed + self_allowed）。
    Collaborator,
    /// 门禁级：七项断言 + 可验证来源证明；distinct 的唯一依据。
    Gate,
}

/// 门禁级证明的来源（ADR-031 C4 r3，二选一）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofSource {
    /// GitLab 服务端签发且可验签的证明对象。
    ServerSignedAttestation,
    /// 不可篡改 audit-event ID + 认证 API 实时复核。
    AuditEventRecheck,
}

/// 信任证明。`verified` 表示调用方已完成来源验签/实时复核——本 crate 不做网络验证，
/// 但消费方（effective 谓词）要求 verified=true 才可采信。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceProof {
    pub trust: TrustLevel,
    pub verified: bool,
    pub source: ProofSource,
    pub protected_ref: String,
    pub commit_sha: String,
    /// 证明必须绑定到具体 release.decided 事件（防挪用）。
    pub subject_event_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Payload {
    AttemptStarted {
        gate: String,
    },
    OutputFrozen {
        digest: String,
        manifest_sha256: String,
        object_refs: Vec<String>,
    },
    ReleaseRequested {
        digest: String,
    },
    /// 事实层只呈现为 release_decided_claimed（ADR-031 C3 r4）。
    ReleaseDecided {
        digest: String,
        decision: Decision,
        /// 审计级信息：仅追溯用，永不作放行条件。
        reviewer: String,
        trust: TrustLevel,
        #[serde(skip_serializing_if = "Option::is_none")]
        proof: Option<GovernanceProof>,
    },
    StageTransition {
        from: String,
        to: String,
    },
    PlanFrozen {
        plan_doc_id: String,
    },
    /// 闭包字段不具可信性：reducer 自行从 DAG 复算并要求完全相等（ADR-031 C6 r4）。
    ForkResolved {
        conflicting_heads: Vec<String>,
        winner: String,
        superseded_descendants: Vec<String>,
        reason: String,
    },
    Supersede {
        target_attempt_id: String,
    },
}

/// 事件信封。字段与 ADR-031 C2 一致；`parent_head` 为本机所见事件头，
/// 首事件取 [`ROOT_PARENT`]。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub event_id: String,
    pub workitem_id: String,
    pub attempt_id: String,
    pub kind: EventKind,
    pub schema_version: u32,
    pub parent_head: String,
    pub idempotency_key: String,
    pub created_at: String,
    pub producer: String,
    #[serde(flatten)]
    pub payload: Payload,
}

impl Envelope {
    pub fn new(
        workitem_id: &str,
        attempt_id: &str,
        parent_head: &str,
        payload: Payload,
        producer: &str,
    ) -> Self {
        let kind = match &payload {
            Payload::AttemptStarted { .. } => EventKind::AttemptStarted,
            Payload::OutputFrozen { .. } => EventKind::OutputFrozen,
            Payload::ReleaseRequested { .. } => EventKind::ReleaseRequested,
            Payload::ReleaseDecided { .. } => EventKind::ReleaseDecided,
            Payload::StageTransition { .. } => EventKind::StageTransition,
            Payload::PlanFrozen { .. } => EventKind::PlanFrozen,
            Payload::ForkResolved { .. } => EventKind::ForkResolved,
            Payload::Supersede { .. } => EventKind::Supersede,
        };
        Self {
            event_id: new_ulid(),
            workitem_id: workitem_id.to_string(),
            attempt_id: attempt_id.to_string(),
            kind,
            schema_version: SCHEMA_VERSION,
            parent_head: parent_head.to_string(),
            idempotency_key: new_ulid(),
            created_at: now_rfc3339(),
            producer: producer.to_string(),
            payload,
        }
    }
}

pub fn now_rfc3339() -> String {
    // 无 time 依赖的 RFC3339 毫秒串；仅审计展示用，排序一律走 event_id/因果序。
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}ms-epoch", d.as_millis())
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("dag_invalid: {0}")]
    Dag(#[from] dag::DagError),
    #[error("io_error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json_error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("event_conflict: eventId={0} 已存在且内容不同（幂等键冲突）")]
    EventConflict(String),
    #[error("object_missing: sha256={0}")]
    ObjectMissing(String),
    #[error("object_corrupt: sha256={0}")]
    ObjectCorrupt(String),
    #[error("bad_event_id: {0}")]
    BadEventId(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulid_format_and_roundtrip() {
        let id = new_ulid();
        assert_eq!(id.len(), 26);
        assert!(is_ulid(&id));
        let decoded = id
            .bytes()
            .map(|b| {
                CROCKFORD
                    .iter()
                    .position(|c| *c == b.to_ascii_uppercase())
                    .unwrap() as u128
            })
            .fold(0u128, |acc, d| (acc << 5) | d);
        assert_eq!(encode_ulid(decoded), id);
    }

    #[test]
    fn ulid_lexicographic_matches_numeric_prefix() {
        // 同毫秒前缀下，字典序 = 数值序（平局键确定性依据）。
        let a = encode_ulid(1);
        let b = encode_ulid(2);
        assert!(a < b);
    }

    #[test]
    fn release_digest_matches_six_part_composition() {
        let d = release_digest("wi", "testing", "at", "es", "mf", "pv");
        assert_eq!(d.len(), 64);
        // 分量顺序敏感
        let d2 = release_digest("wi", "testing", "at", "es2", "mf", "pv");
        assert_ne!(d, d2);
    }
}
