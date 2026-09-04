//! Provider 能力快照（ADR-033 / Codex 能力差距方案 M0 §4.1）。
//!
//! 版本化保存在 `model_profiles.capabilities_json` 中，来源三级分离：
//! - `preset`：预设候选，未经真实探测，不得宣称可用；
//! - `probe`：`modelProfile.test` 真实最小探测通过后写入，带 verifiedAt/expiresAt/digest；
//! - `manual`：手动覆盖，只能收紧/禁用能力，不能把未验证能力标成可用。
//!
//! 有效快照 = probe 快照被 manual 覆盖后的结果；过期（expiresAt 已过）视为
//! unknown → 调用方回退保守路径（legacy codec），不阻断既有 Run。
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use sg_store::Store;

pub const CAPABILITY_SCHEMA_VERSION: i64 = 1;
/// 探测结果有效期：过期后回退保守路径（ADR-033 决策 2）。
pub const PROBE_TTL_SECS: i64 = 7 * 24 * 3600;

/// 能力快照（§4.1 契约；unknown 用字符串枚举表达"未验证"）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CapabilitySnapshot {
    #[serde(default = "default_schema_version")]
    #[serde(rename = "schemaVersion")]
    pub schema_version: i64,
    #[serde(default = "default_protocols")]
    pub protocols: Vec<String>,
    #[serde(default = "default_preferred")]
    #[serde(rename = "preferredProtocol")]
    pub preferred_protocol: String,
    /// true/false/"unknown"：M0 探测只验证 chat_completions 往返；
    /// 原生工具/流式能力留 unknown，由 M1/M2 探测升级。
    #[serde(default = "default_unknown")]
    #[serde(rename = "nativeTools")]
    pub native_tools: serde_json::Value,
    #[serde(default = "default_unknown")]
    #[serde(rename = "streamText")]
    pub stream_text: serde_json::Value,
    #[serde(default = "default_unknown")]
    #[serde(rename = "streamToolArguments")]
    pub stream_tool_arguments: serde_json::Value,
    #[serde(default = "default_reasoning_transport")]
    #[serde(rename = "reasoningTransport")]
    pub reasoning_transport: String,
    #[serde(default = "default_compaction")]
    pub compaction: String,
    #[serde(default = "default_cache")]
    pub cache: String,
    #[serde(default = "default_server_state")]
    #[serde(rename = "serverState")]
    pub server_state: String,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default)]
    #[serde(rename = "verifiedAt")]
    pub verified_at: String,
    #[serde(default)]
    #[serde(rename = "expiresAt")]
    pub expires_at: String,
    #[serde(default)]
    pub digest: String,
}

fn default_schema_version() -> i64 {
    CAPABILITY_SCHEMA_VERSION
}
fn default_protocols() -> Vec<String> {
    vec!["chat_completions".into()]
}
fn default_preferred() -> String {
    "chat_completions".into()
}
fn default_unknown() -> serde_json::Value {
    serde_json::json!("unknown")
}
fn default_reasoning_transport() -> String {
    "none".into()
}
fn default_compaction() -> String {
    "local_structured".into()
}
fn default_cache() -> String {
    "implicit".into()
}
fn default_server_state() -> String {
    "unsupported".into()
}
fn default_source() -> String {
    "preset".into()
}

impl Default for CapabilitySnapshot {
    fn default() -> Self {
        serde_json::from_value(serde_json::json!({})).unwrap()
    }
}

impl CapabilitySnapshot {
    /// canonical JSON（serde_json BTreeMap 键序）→ SHA-256 digest。
    /// 探测摘要 3 次运行必须一致（M0 退出标准）。
    pub fn canonical_digest(&self) -> String {
        let value = serde_json::to_value(self).unwrap_or(serde_json::json!({}));
        // digest 不含来源与时间戳：同一 Profile/模型重复探测摘要稳定。
        let mut v = match value {
            serde_json::Value::Object(m) => m,
            other => return sg_store::ids::hex(&Sha256::digest(other.to_string().as_bytes())),
        };
        for key in ["source", "verifiedAt", "expiresAt", "digest"] {
            v.remove(key);
        }
        sg_store::ids::hex(&Sha256::digest(
            serde_json::Value::Object(v).to_string().as_bytes(),
        ))
    }

    /// 快照是否已探测且未过期（manual 覆盖被视为有效直至手动修改）。
    pub fn verified(&self) -> bool {
        if self.source == "manual" {
            return true;
        }
        if self.source != "probe" || self.expires_at.is_empty() {
            return false;
        }
        match (
            sg_store::timefmt::parse(&self.expires_at),
            sg_store::timefmt::parse(&self.verified_at),
        ) {
            (Some(exp), Some(v)) => exp > v && exp.unix_timestamp() > now_unix(),
            _ => false,
        }
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 从 capabilities_json 读取有效快照；无快照/不可解析 → preset 默认（保守）。
pub fn effective_snapshot(capabilities: &serde_json::Value) -> CapabilitySnapshot {
    serde_json::from_value(capabilities.clone()).unwrap_or_default()
}

/// 探测通过后写入 probe 快照：保留 manual 覆盖子树，替换其余字段。
pub fn record_probe(
    store: &Store,
    profile_id: &str,
    capabilities: &serde_json::Value,
) -> Result<CapabilitySnapshot, sg_store::Error> {
    let now = sg_store::timefmt::now();
    let expires_at = sg_store::timefmt::parse(&now)
        .map(|t| sg_store::timefmt::format_now(t + time::Duration::seconds(PROBE_TTL_SECS)))
        .unwrap_or_else(|| now.clone());
    let mut snapshot: CapabilitySnapshot =
        serde_json::from_value(capabilities.clone()).unwrap_or_default();
    snapshot.schema_version = CAPABILITY_SCHEMA_VERSION;
    snapshot.protocols = vec!["chat_completions".into()];
    snapshot.preferred_protocol = "chat_completions".into();
    snapshot.source = "probe".into();
    snapshot.verified_at = now.clone();
    snapshot.expires_at = expires_at;
    snapshot.digest = snapshot.canonical_digest();

    // manual 覆盖：只允许收紧——manual 中显式为 false/"none"/"unsupported" 的键优先。
    let mut merged = serde_json::to_value(&snapshot).map_err(|e| sg_store::Error::Message(e.to_string()))?;
    if let Some(serde_json::Value::Object(manual)) = capabilities.get("manual") {
        if let Some(target) = merged.as_object_mut() {
            for (k, v) in manual {
                let restrictive = match v {
                    serde_json::Value::Bool(false) => true,
                    serde_json::Value::String(s) => s == "none" || s == "unsupported",
                    _ => false,
                };
                if restrictive {
                    target.insert(k.clone(), v.clone());
                }
            }
        }
    }

    let merged_json = serde_json::to_string(&merged).map_err(|e| sg_store::Error::Message(e.to_string()))?;
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE model_profiles SET capabilities_json=?1, revision=revision+1, updated_at=?2
             WHERE id=?3",
            rusqlite::params![merged_json, sg_store::timefmt::now(), profile_id],
        )?;
        Ok(())
    })?;
    serde_json::from_value(merged).map_err(|e| sg_store::Error::Message(e.to_string()))
}

/// 供观测表回填：按 model 查找未过期的 probe 摘要（无则空串）。
pub fn probe_digest_for_model(store: &Store, model: &str) -> String {
    if model.is_empty() {
        return String::new();
    }
    store
        .with_conn(|conn| {
            let caps: Option<String> = conn
                .query_row(
                    "SELECT capabilities_json FROM model_profiles
                     WHERE default_model = ?1 ORDER BY updated_at DESC LIMIT 1",
                    [model],
                    |r| r.get(0),
                )
                .ok();
            let caps = match caps {
                Some(c) => c,
                None => return Ok(String::new()),
            };
            let snap: CapabilitySnapshot = serde_json::from_str(&caps).unwrap_or_default();
            if snap.source == "probe" && snap.verified() && !snap.digest.is_empty() {
                Ok(snap.digest)
            } else {
                Ok(String::new())
            }
        })
        .unwrap_or_default()
}
