//! 记忆模型类型与规范化助手（ADR-032 / 实施方案 §6）。
use serde::{Deserialize, Serialize};

pub const KINDS: [&str; 5] = ["decision", "convention", "fact", "lesson", "preference"];
pub const STATUSES: [&str; 6] = [
    "proposed",
    "active",
    "conflicted",
    "archived",
    "rejected",
    "purged",
];
pub const SOURCE_KINDS: [&str; 7] = [
    "manual",
    "run",
    "workitem",
    "artifact",
    "evidence",
    "requirement",
    "import",
];
pub const RELATIONS: [&str; 3] = ["derived_from", "summarizes", "corrects"];

/// 汇总正文的字节数（UTF-8；预算按字节计）。
pub fn byte_len(s: &str) -> i64 {
    s.len() as i64
}

/// slug：字母数字（含 CJK）保留并小写化，其余折叠为 `-`；空则回退 `memory`。
pub fn derive_slug(title: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for c in title.trim().chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
        if out.len() >= 48 {
            break;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "memory".into()
    } else {
        out
    }
}

/// subject_key：规范化主题键（小写化 + 空白折叠），用于同主题冲突检测。
pub fn subject_key(title: &str) -> String {
    title
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// 摘要：正文去 Markdown 标记后的纯文本前缀（≤512 字符，方案 §6.3）。
pub fn derive_summary(body: &str, explicit: Option<&str>) -> String {
    let explicit = explicit.unwrap_or_default().trim();
    if !explicit.is_empty() {
        return explicit.chars().take(512).collect();
    }
    let plain: String = body
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("```"))
        .collect::<Vec<_>>()
        .join(" ");
    plain.chars().take(200).collect()
}

pub fn normalize_tags(tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = tags
        .iter()
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    sg_store::ids::hex(&Sha256::digest(data))
}

/// canonical fingerprint：serde_json Value（BTreeMap 键序）→ SHA-256。
/// Core 自算，不信任客户端 hash（方案 §8.1）。
pub fn fingerprint(parts: &serde_json::Value) -> String {
    sha256_hex(parts.to_string().as_bytes())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySettings {
    pub project_id: String,
    pub enabled: bool,
    pub capture_mode: String,
    pub max_entries: i64,
    pub max_bytes: i64,
    pub stale_after_days: i64,
    pub revision: i64,
    pub updated_at: String,
    pub updated_by: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub capture_mode: Option<String>,
    #[serde(default)]
    pub max_entries: Option<i64>,
    #[serde(default)]
    pub max_bytes: Option<i64>,
    #[serde(default)]
    pub stale_after_days: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRefInput {
    #[serde(default)]
    pub source_kind: String,
    #[serde(default)]
    pub source_id: String,
    #[serde(default)]
    pub locator: String,
    #[serde(default)]
    pub source_digest: String,
    #[serde(default = "default_relation")]
    pub relation: String,
}

fn default_relation() -> String {
    "derived_from".into()
}

#[derive(Debug, Clone)]
pub struct CreateInput {
    pub project_id: String,
    pub title: String,
    pub kind: String,
    pub body: String,
    pub summary: Option<String>,
    pub tags: Vec<String>,
    pub source_refs: Vec<SourceRefInput>,
    /// manual → active（直接确认）；import propose → proposed。
    pub target_status: &'static str,
    pub actor: String,
    pub idempotency_key: String,
    /// 与 active 同主题同内容时的行为：Reject（手工）报 conflict；Skip（导入）去重跳过。
    pub on_duplicate: DuplicateMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateMode {
    Reject,
    Skip,
}

#[derive(Debug, Clone)]
pub struct UpdateInput {
    pub project_id: String,
    pub memory_id: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub summary: Option<String>,
    pub tags: Option<Vec<String>>,
    pub expected_revision: i64,
    pub actor: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PurgeSourceRef {
    pub source_kind: String,
    pub source_id: String,
    pub locator: String,
    pub source_digest: String,
    pub relation: String,
}

/// 稳定错误 token（dispatch 层映射到 MEMORY_* 错误族；前端只依赖 code）。
pub mod err_tokens {
    pub const CONFLICT: &str = "memory_conflict";
    pub const SECRET: &str = "memory_secret_detected";
    pub const INVALID_STATE: &str = "memory_invalid_state";
    pub const OBJECT_MISSING: &str = "memory_object_missing";
    pub const PURGE_BLOCKED: &str = "memory_purge_blocked";
    pub const QUOTA: &str = "memory_quota_exceeded";
    pub const DISABLED: &str = "memory_disabled";
    pub const NOT_FOUND: &str = "not_found";
}

pub fn merr(token: &str, detail: impl std::fmt::Display) -> sg_store::Error {
    sg_store::Error::Message(format!("{token}: {detail}"))
}
