//! B7 结构化验收契约（RDWS 实施计划 v1.4 WP-7）。
//!
//! acceptance 元素双形态（untagged）：
//! - 字符串：自由文本，**仅展示**，不参与任何自动判定；
//! - 对象：tagged union 机器契约——每 verifier 独立 params schema，未知 verifier/
//!   缺参数/参数越界在模板版本创建即拒（acceptance_schema_invalid）。
//!
//! digest v3：结构化形态 canonical 序列化进模板 content_digest（v1/v2 存量激活
//! 版本不重算）；WP-8 的 skip/fast_track 列为 v4，标签不复用。
//!
//! kill switch（RATIFLOW_STRUCTURED_ACCEPTANCE=0，fail-closed）：
//! - 禁止创建/激活含结构化项的新模板版本（feature_disabled）；
//! - 已有实例 evaluate/release 遇结构化强制项 → failed_inputs 记
//!   acceptance_evaluator_unavailable（放行被阻）；
//! - manual_confirm 人工路径保留；**绝不静默降级回旧六输入放行**。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sg_store::Error;

/// 六 verifier tagged union（serde tag=verifier；params 即变体字段）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verifier", rename_all = "snake_case")]
pub enum AcceptanceVerifier {
    /// 关基线已冻结指定 kind 的交付物。
    ArtifactFrozen { artifact_kind: String },
    /// 指定 kind 的冻结交付物正文非空。
    TextNonempty { artifact_kind: String },
    /// 本关已核验证据 ≥ min_count（min_count ≥ 1）。
    EvidenceVerified {
        evidence_kind: String,
        min_count: u32,
    },
    /// 需求覆盖完整（scope 目前仅 requirement_revision）。
    CoverageComplete { scope: String },
    /// 摘要匹配（expected_from: baseline|approval|policy）。
    DigestMatch { expected_from: String },
    /// 人工确认事实（gate_manual_confirmations 表的 confirmed 行）。
    ManualConfirm {
        confirmation_subject: String,
        confirm_role: String,
    },
}

impl AcceptanceVerifier {
    pub fn verifier_name(&self) -> &'static str {
        match self {
            AcceptanceVerifier::ArtifactFrozen { .. } => "artifact_frozen",
            AcceptanceVerifier::TextNonempty { .. } => "text_nonempty",
            AcceptanceVerifier::EvidenceVerified { .. } => "evidence_verified",
            AcceptanceVerifier::CoverageComplete { .. } => "coverage_complete",
            AcceptanceVerifier::DigestMatch { .. } => "digest_match",
            AcceptanceVerifier::ManualConfirm { .. } => "manual_confirm",
        }
    }

    /// 值域校验（serde 形状之外的领域约束）。
    fn validate(&self) -> Result<(), String> {
        match self {
            AcceptanceVerifier::ArtifactFrozen { artifact_kind }
            | AcceptanceVerifier::TextNonempty { artifact_kind } => {
                if artifact_kind.trim().is_empty() {
                    return Err("artifact_kind 必填".into());
                }
            }
            AcceptanceVerifier::EvidenceVerified {
                evidence_kind,
                min_count,
            } => {
                if evidence_kind.trim().is_empty() {
                    return Err("evidence_kind 必填".into());
                }
                if *min_count < 1 {
                    return Err("min_count 必须 ≥ 1".into());
                }
            }
            AcceptanceVerifier::CoverageComplete { scope } => {
                if scope != "requirement_revision" {
                    return Err(format!("未知 scope {scope:?}（仅 requirement_revision）"));
                }
            }
            AcceptanceVerifier::DigestMatch { expected_from } => {
                if !matches!(expected_from.as_str(), "baseline" | "approval" | "policy") {
                    return Err(format!("未知 expected_from {expected_from:?}"));
                }
            }
            AcceptanceVerifier::ManualConfirm {
                confirmation_subject,
                confirm_role,
            } => {
                if confirmation_subject != "gate_manual_confirm" {
                    return Err(format!(
                        "confirmation_subject 必须 gate_manual_confirm（得到 {confirmation_subject:?}）"
                    ));
                }
                if confirm_role != "user" {
                    return Err(format!("confirm_role 目前仅 user（得到 {confirm_role:?}）"));
                }
            }
        }
        Ok(())
    }
}

/// acceptance 元素（双形态）：字符串=展示；对象=机器契约。
/// 手写 Deserialize（untagged 套内部 tagged 在 serde 的 Content 缓冲下不可靠，
/// 且手工分派能给出「未知 verifier/缺参数」的显式错误而非聚合 no-variant）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum AcceptanceElement {
    /// 自由文本：仅展示，不进判定。
    Display(String),
    /// 机器契约。
    Structured(AcceptanceVerifier),
}

impl<'de> Deserialize<'de> for AcceptanceElement {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        if let Some(s) = v.as_str() {
            return Ok(AcceptanceElement::Display(s.to_string()));
        }
        if !v.is_object() {
            return Err(serde::de::Error::custom(
                "acceptance 元素必须是字符串或 {verifier,...} 对象",
            ));
        }
        let verifier = v
            .get("verifier")
            .and_then(|t| t.as_str())
            .ok_or_else(|| serde::de::Error::custom("结构化 acceptance 缺 verifier 字段"))?;
        if !KNOWN_VERIFIERS.contains(&verifier) {
            return Err(serde::de::Error::custom(format!(
                "未知 verifier {verifier:?}（合法集：{KNOWN_VERIFIERS:?}）"
            )));
        }
        let parsed: AcceptanceVerifier = serde_json::from_value(v)
            .map_err(|e| serde::de::Error::custom(format!("verifier 参数非法：{e}")))?;
        Ok(AcceptanceElement::Structured(parsed))
    }
}

const KNOWN_VERIFIERS: [&str; 6] = [
    "artifact_frozen",
    "text_nonempty",
    "evidence_verified",
    "coverage_complete",
    "digest_match",
    "manual_confirm",
];

pub const FLAG: &str = "RATIFLOW_STRUCTURED_ACCEPTANCE";

pub fn enabled() -> bool {
    std::env::var(FLAG).ok().as_deref() == Some("1")
}

/// 解析+校验单个元素（模板创建/激活入口共用）。
/// 结构化元素 + flag 关闭 → feature_disabled（不建含机器契约的新版本）。
pub fn parse_element(v: &Value) -> Result<AcceptanceElement, Error> {
    let el: AcceptanceElement = serde_json::from_value(v.clone()).map_err(|e| {
        Error::Message(format!(
            "acceptance_schema_invalid: acceptance 元素形状非法（未知 verifier/缺参数/类型不符）：{e}"
        ))
    })?;
    if let AcceptanceElement::Structured(verifier) = &el {
        if !enabled() {
            return Err(Error::Message(
                "feature_disabled: RATIFLOW_STRUCTURED_ACCEPTANCE 未开启，禁止创建含结构化验收项的模板版本"
                    .into(),
            ));
        }
        verifier
            .validate()
            .map_err(|e| Error::Message(format!("acceptance_schema_invalid: {e}")))?;
    }
    Ok(el)
}

/// canonical 序列化（digest v3 消费）：字符串原样；结构化按 serde 确定性字段序。
pub fn canonical_form(el: &AcceptanceElement) -> String {
    match el {
        AcceptanceElement::Display(s) => format!("str|{s}"),
        AcceptanceElement::Structured(v) => {
            format!("obj|{}", serde_json::to_string(v).unwrap_or_default())
        }
    }
}

/// 批量解析（模板保存入口）。
pub fn parse_elements(values: &[Value]) -> Result<Vec<AcceptanceElement>, Error> {
    values.iter().map(parse_element).collect()
}

/// 元素 digest（manual_confirm 链绑定验收项身份；canonical 形态哈希）。
pub fn element_digest(el: &AcceptanceElement) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "sha256:{}",
        sg_store::ids::hex(&Sha256::digest(canonical_form(el).as_bytes()))
    )
}

/// 实例指定关的结构化验收元素（gate.evaluate 接线取数口，WP-7）：
/// 实例冻结的模板版本 → 关定义 acceptance_json → 双形态解析。
/// 无实例/关无 acceptance → 空向量（六输入基线行为不变）。
/// 存量行元素非法 → acceptance_schema_invalid（fail-closed，不静默跳过）。
pub fn elements_for_gate(
    store: &sg_store::Store,
    workitem_id: &str,
    gate: &str,
) -> Result<Vec<AcceptanceElement>, Error> {
    let Some(instance) = crate::instance::for_workitem(store, workitem_id)? else {
        return Ok(vec![]);
    };
    let row: Option<String> = store.with_conn(|conn| {
        match conn.query_row(
            "SELECT COALESCE(acceptance_json,'[]') FROM workflow_gate_definitions
             WHERE version_id=?1 AND gate_id=?2",
            rusqlite::params![instance.template_version_id, gate],
            |r| r.get::<_, String>(0),
        ) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(other) => Err(other.into()),
        }
    })?;
    let vals: Vec<Value> =
        serde_json::from_str(&row.unwrap_or_else(|| "[]".into())).unwrap_or_default();
    vals.iter()
        .map(|v| {
            serde_json::from_value(v.clone()).map_err(|e| {
                Error::Message(format!(
                    "acceptance_schema_invalid: 存量 acceptance 元素非法：{e}"
                ))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn parse(v: Value) -> Result<AcceptanceElement, Error> {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(FLAG, "1");
        let r = parse_element(&v);
        std::env::remove_var(FLAG);
        r
    }

    /// 负例矩阵：未知 verifier/缺参数/越界值在创建即拒（RDWS-011）。
    #[test]
    fn schema_negative_matrix() {
        let cases = [
            (json!({"verifier": "magic"}), "未知 verifier"),
            (
                json!({"verifier": "evidence_verified", "evidence_kind": "test", "min_count": 0}),
                "min_count 下界",
            ),
            (
                json!({"verifier": "evidence_verified", "evidence_kind": "test"}),
                "缺 min_count",
            ),
            (
                json!({"verifier": "artifact_frozen", "artifact_kind": ""}),
                "kind 空",
            ),
            (
                json!({"verifier": "coverage_complete", "scope": "everything"}),
                "scope 越界",
            ),
            (
                json!({"verifier": "digest_match", "expected_from": "vibes"}),
                "expected_from 越界",
            ),
            (
                json!({"verifier": "manual_confirm", "confirmation_subject": "other", "confirm_role": "user"}),
                "subject 越界",
            ),
            (
                json!({"verifier": "manual_confirm", "confirmation_subject": "gate_manual_confirm", "confirm_role": "admin"}),
                "role 越界",
            ),
        ];
        for (v, label) in cases {
            let err = parse(v).unwrap_err();
            assert!(
                err.to_string().contains("acceptance_schema_invalid"),
                "{label}: {err}"
            );
        }
    }

    /// 正例：六 verifier 全形 + 字符串展示形态；digest v3 canonical 确定性。
    #[test]
    fn all_verifiers_parse_and_canonical_is_deterministic() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(FLAG, "1");
        let good = [
            json!("任意自由文本（仅展示）"),
            json!({"verifier": "artifact_frozen", "artifact_kind": "prd"}),
            json!({"verifier": "text_nonempty", "artifact_kind": "tech_design"}),
            json!({"verifier": "evidence_verified", "evidence_kind": "test_report", "min_count": 1}),
            json!({"verifier": "coverage_complete", "scope": "requirement_revision"}),
            json!({"verifier": "digest_match", "expected_from": "baseline"}),
            json!({"verifier": "manual_confirm", "confirmation_subject": "gate_manual_confirm", "confirm_role": "user"}),
        ];
        let els = parse_elements(&good).unwrap();
        std::env::remove_var(FLAG);
        assert_eq!(els.len(), 7);
        assert!(matches!(els[0], AcceptanceElement::Display(_)));
        // canonical 稳定：同一元素两次序列化一致；不同参数必不同。
        assert_eq!(canonical_form(&els[1]), canonical_form(&els[1]));
        assert_ne!(canonical_form(&els[1]), canonical_form(&els[2]));
        assert!(element_digest(&els[6]).starts_with("sha256:"));
    }

    /// kill switch：flag 关闭时结构化元素拒绝创建，字符串形态不受影响。
    #[test]
    fn kill_switch_blocks_structured_creation_only() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(FLAG);
        let err = parse_element(&json!({"verifier": "artifact_frozen", "artifact_kind": "prd"}))
            .unwrap_err();
        assert!(err.to_string().contains("feature_disabled"), "{err}");
        assert!(matches!(
            parse_element(&json!("仅展示文本")).unwrap(),
            AcceptanceElement::Display(_)
        ));
    }
}
