//! 数据化工作流模板（EvoFlow 方案 M1-02 / ADR-036）：
//! 逻辑身份（workflow_templates）+ 不可变版本（workflow_template_versions，
//! draft → active → deprecated）+ 关卡定义（workflow_gate_definitions）。
//! gate_id 是模板版本内稳定字符串，schema 不假设固定六关；active 版本不可原地编辑。

use sg_store::{ids, timefmt, Error, Store};
use sha2::{Digest, Sha256};

/// M1 功能开关（方案 §11.1）：`RATIFLOW_WORKFLOW_TEMPLATE_V2`。
/// 默认关闭 = 新模板管理 RPC 与非默认模板创建不可用；默认六关行为不变。
/// 实例顺序解析对默认模板与 legacy 枚举逐字相同（parity 由单测断言），
/// 因此关闭 flag 不影响已存在实例的一致性（§11.3 只读延续）。
/// 模板域开关：**默认开启**（关卡/交付物配置化为产品能力，用户 2026-09-07 拍板）；
/// `RATIFLOW_WORKFLOW_TEMPLATE_V2=0` 为显式关闭（kill switch，回退 legacy 行为：
/// 新模板 RPC feature_disabled，workitem.create 拒绝非默认 templateId）。
/// 默认模板 six-gate-default 与 legacy 六关逐字同序（parity 单测），默认行为不变。
pub fn template_v2_enabled() -> bool {
    std::env::var("RATIFLOW_WORKFLOW_TEMPLATE_V2")
        .map(|v| v != "0")
        .unwrap_or(true)
}

/// 内置默认模板 key（迁移 0032 创建，激活版本 v1）。
pub const DEFAULT_TEMPLATE_KEY: &str = "six-gate-default";

/// gate_id / template key 约束（ADR-036 决策 2）。
pub fn valid_key(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TemplateRecord {
    pub id: String,
    pub key: String,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct VersionRecord {
    pub id: String,
    pub template_id: String,
    pub version_no: i64,
    pub status: String,
    pub content_digest: String,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
}

/// skip 策略（WP-8）：`forbidden`（默认）= 不可跳过；`manual_approval` = 仅人工
/// 经审批跳过（subject_type=gate_skip，替代证据必填）。schema 严格（未知字段
/// 创建即拒），不存在任何自动跳过形态。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkipPolicy {
    pub mode: SkipMode,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipMode {
    Forbidden,
    ManualApproval,
}

/// fast-track 缩减项（WP-8）：仅可缩减关卡层活动/交付物/关卡层人工复核。
/// schema 严格（deny_unknown_fields）——不存在豁免 Tool 风险审批与 Policy
/// 强制审批的声明面，越界声明在模板创建即被拒（评审修正项）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FastTrackPolicy {
    #[serde(default)]
    pub skippable_activities: Vec<String>,
    #[serde(default)]
    pub waived_deliverables: Vec<WaivedDeliverable>,
    #[serde(default)]
    pub reduced_approval: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaivedDeliverable {
    pub kind: String,
    /// 替代证据 kind（替代证据必填，fast-track 应用时校验在案）。
    pub substitute_evidence_kind: String,
}

/// fast-track 六因素受控枚举（WP-8 / v1.4 §WP-8 映射表）：
/// 全真才可生成快通道建议（落 shadow，不自动执行）。
/// P0-2（审计 §7）：六因素全部由服务端从权威事实派生
/// （fast_track::derive_fast_track_facts），客户端不得提交；
/// 任一数据源缺失/漂移 = false（不得默认 true）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FastTrackFactors {
    pub no_protected_path: bool,
    pub api_schema_unchanged: bool,
    pub effect_class_read_only: bool,
    pub reversibility_confirmed: bool,
    pub provenance_complete: bool,
    pub test_evidence_present: bool,
}

impl FastTrackFactors {
    pub fn all_true(&self) -> bool {
        self.no_protected_path
            && self.api_schema_unchanged
            && self.effect_class_read_only
            && self.reversibility_confirmed
            && self.provenance_complete
            && self.test_evidence_present
    }
}

/// 部署/迁移类交付物 kind：含此类 deliverable 的关恒 forbidden，不可放宽
/// （skip_policy 声明 manual_approval 在创建即拒）。
pub fn is_deployment_class_kind(kind: &str) -> bool {
    let k = kind.trim().to_ascii_lowercase();
    k.starts_with("deployment") || k.starts_with("migration")
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GateDefinition {
    pub id: String,
    pub version_id: String,
    pub gate_id: String,
    pub ordinal: i64,
    pub title: String,
    pub purpose: String,
    pub deliverables: Vec<String>,
    /// 本关验收策略（WP-7 双形态：字符串=仅展示；对象=结构化机器契约，
    /// 形状由 sg-workflow acceptance 校验；空 = 沿用通用六输入门禁基线）。
    pub acceptance: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_policy_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_policy_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_policy_ref: Option<String>,
    /// skip 策略（WP-8；None = '{}' = forbidden 缺省）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_policy: Option<SkipPolicy>,
    /// fast-track 缩减策略（WP-8；None = '{}' = 全禁缺省）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fast_track_policy: Option<FastTrackPolicy>,
}

/// 版本内容的输入形状（ordinal 由数组位置隐含：1..n 连续）。
/// acceptance 与 policy refs = 门禁数据化输入面（评审 P0-4）：每关可声明
/// 验收策略与 Context/Team/Workspace 策略引用，随版本冻结、参与 digest。
/// acceptance 元素为原始 JSON（字符串或 {verifier,...} 对象），创建/激活时经
/// acceptance::parse_elements 校验（WP-7：未知 verifier/缺参数/越界即拒）。
/// skip/fast-track 策略为 WP-8 增量：schema 严格、参与 digest v4。
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct GateDefInput {
    pub gate_id: String,
    pub title: String,
    #[serde(default)]
    pub purpose: String,
    #[serde(default)]
    pub deliverables: Vec<String>,
    #[serde(default)]
    pub acceptance: Vec<serde_json::Value>,
    #[serde(default)]
    pub context_policy_ref: Option<String>,
    #[serde(default)]
    pub team_policy_ref: Option<String>,
    #[serde(default)]
    pub workspace_policy_ref: Option<String>,
    #[serde(default)]
    pub skip_policy: Option<SkipPolicy>,
    #[serde(default)]
    pub fast_track_policy: Option<FastTrackPolicy>,
}

fn ref_str(r: &Option<String>) -> &str {
    r.as_deref().unwrap_or("-")
}

/// acceptance 槽（digest v3）：逐元素 canonical 形态（`str|文本` / `obj|{...}`）
/// 以逗号拼接；无法解析的元素按 `invalid|<原始JSON>` 入哈希（fail-closed：
/// 垃圾元素改变 digest 且激活前的 parse 校验会先行拒绝）。
fn acceptance_v3_slot(vals: &[serde_json::Value]) -> String {
    vals.iter()
        .map(
            |v| match serde_json::from_value::<crate::acceptance::AcceptanceElement>(v.clone()) {
                Ok(el) => crate::acceptance::canonical_form(&el),
                Err(_) => format!("invalid|{v}"),
            },
        )
        .collect::<Vec<_>>()
        .join(",")
}

/// 策略槽（digest v4）：None → `-`；Some → canonical JSON（serde 确定性字段序）。
fn policy_slot<T: serde::Serialize>(policy: &Option<T>) -> String {
    match policy {
        None => "-".into(),
        Some(p) => serde_json::to_string(p).unwrap_or_else(|_| "invalid".into()),
    }
}

/// 版本内容 digest：
/// sha256("v4|" + join("ordinal|gate_id|title|purpose|deliverables_csv|acceptance_v3|ctx_ref|team_ref|ws_ref|skip_policy|fast_track_policy", "\n"))。
/// v3（WP-7）：acceptance 槽升级为逐元素 canonical 形态。
/// v4（WP-8）：追加 skip_policy/fast_track_policy 两槽（canonical JSON / `-`）；
/// v1/v2/v3 存量激活版本不重算，仅新版本生效。版本标签永不复用为两套 canonical schema。
pub fn content_digest(defs: &[GateDefInput]) -> String {
    let lines: Vec<String> = defs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            format!(
                "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
                i + 1,
                d.gate_id,
                d.title,
                d.purpose,
                d.deliverables.join(","),
                acceptance_v3_slot(&d.acceptance),
                ref_str(&d.context_policy_ref),
                ref_str(&d.team_policy_ref),
                ref_str(&d.workspace_policy_ref),
                policy_slot(&d.skip_policy),
                policy_slot(&d.fast_track_policy),
            )
        })
        .collect();
    let mut hasher = Sha256::new();
    hasher.update(format!("v4|{}", lines.join("\n")).as_bytes());
    sg_store::ids::hex(&hasher.finalize())
}

fn validate_defs(defs: &[GateDefInput]) -> Result<(), Error> {
    if defs.is_empty() {
        return Err(Error::Message(
            "workflow_template_invalid: 至少需要一个关卡".into(),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for (i, d) in defs.iter().enumerate() {
        if !valid_key(&d.gate_id) {
            return Err(Error::Message(format!(
                "workflow_template_invalid: gate_id 非法（^[a-z][a-z0-9_-]{{0,63}}$）：{}",
                d.gate_id
            )));
        }
        if !seen.insert(d.gate_id.as_str()) {
            return Err(Error::Message(format!(
                "workflow_template_invalid: gate_id 重复：{}",
                d.gate_id
            )));
        }
        if d.title.trim().is_empty() {
            return Err(Error::Message(format!(
                "workflow_template_invalid: {} 缺少标题",
                d.gate_id
            )));
        }
        if d.deliverables.is_empty() {
            return Err(Error::Message(format!(
                "workflow_template_invalid: {} 至少声明一个交付物 kind",
                d.gate_id
            )));
        }
        // WP-7：结构化 acceptance 契约在创建/改稿/激活三口统一校验
        //（未知 verifier/缺参数/越界 → acceptance_schema_invalid；
        //  kill switch 关闭遇结构化项 → feature_disabled）。
        crate::acceptance::parse_elements(&d.acceptance)
            .map_err(|e| Error::Message(format!("{}（gate: {}）", e, d.gate_id)))?;
        // WP-8：skip/fast-track 策略校验。部署/迁移类交付物的关恒 forbidden，
        // manual_approval 声明创建即拒；fast-track 缩减项引用不得为空。
        if let Some(skip) = &d.skip_policy {
            if skip.mode == SkipMode::ManualApproval
                && d.deliverables.iter().any(|k| is_deployment_class_kind(k))
            {
                return Err(Error::Message(format!(
                    "workflow_template_invalid: 含部署/迁移类交付物的关恒 forbidden，不可声明 manual_approval（gate: {}）",
                    d.gate_id
                )));
            }
        }
        if let Some(ft) = &d.fast_track_policy {
            if ft
                .waived_deliverables
                .iter()
                .any(|w| w.kind.trim().is_empty() || w.substitute_evidence_kind.trim().is_empty())
            {
                return Err(Error::Message(format!(
                    "workflow_template_invalid: waived_deliverables 的 kind 与替代证据 kind 必填（gate: {}）",
                    d.gate_id
                )));
            }
            if ft.reduced_approval
                && ft.skippable_activities.is_empty()
                && ft.waived_deliverables.is_empty()
            {
                return Err(Error::Message(format!(
                    "workflow_template_invalid: reduced_approval 须伴随至少一项活动/交付物缩减（gate: {}）",
                    d.gate_id
                )));
            }
        }
        let _ = i; // ordinal 连续由数组位置隐含
    }
    Ok(())
}

fn template_row(conn: &rusqlite::Connection, id: &str) -> Result<TemplateRecord, Error> {
    conn.query_row(
        "SELECT id, key, name, created_at, updated_at FROM workflow_templates WHERE id=?1",
        [id],
        |r| {
            Ok(TemplateRecord {
                id: r.get(0)?,
                key: r.get(1)?,
                name: r.get(2)?,
                created_at: r.get(3)?,
                updated_at: r.get(4)?,
            })
        },
    )
    .map_err(|_| Error::Message(format!("workflow_template_invalid: 模板 {id} 不存在")))
}

fn version_row(conn: &rusqlite::Connection, id: &str) -> Result<VersionRecord, Error> {
    conn.query_row(
        "SELECT id, template_id, version_no, status, content_digest, created_by, created_at, updated_at
         FROM workflow_template_versions WHERE id=?1",
        [id],
        |r| {
            Ok(VersionRecord {
                id: r.get(0)?,
                template_id: r.get(1)?,
                version_no: r.get(2)?,
                status: r.get(3)?,
                content_digest: r.get(4)?,
                created_by: r.get(5)?,
                created_at: r.get(6)?,
                updated_at: r.get(7)?,
            })
        },
    )
    .map_err(|_| Error::Message(format!("workflow_version_not_active: 版本 {id} 不存在")))
}

/// 创建模板逻辑对象（key 全局唯一）。
pub fn create_template(store: &Store, key: &str, name: &str) -> Result<TemplateRecord, Error> {
    if !valid_key(key) {
        return Err(Error::Message(
            "workflow_template_invalid: template key 非法".into(),
        ));
    }
    if name.trim().is_empty() {
        return Err(Error::Message("workflow_template_invalid: 名称必填".into()));
    }
    let id = ids::new_id("wtpl");
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO workflow_templates(id, key, name, created_at, updated_at) VALUES (?1,?2,?3,?4,?4)",
            rusqlite::params![id, key, name.trim(), now],
        )
        .map_err(|e| {
            if e.to_string().contains("UNIQUE") {
                Error::Message(format!("workflow_template_invalid: key 已存在：{key}"))
            } else {
                e.into()
            }
        })?;
        template_row(conn, &id)
    })
}

/// 创建草稿版本（version_no = 当前最大 + 1）；active 不可原地编辑。
pub fn create_version(
    store: &Store,
    template_id: &str,
    defs: &[GateDefInput],
    created_by: &str,
) -> Result<VersionRecord, Error> {
    validate_defs(defs)?;
    let version_id = ids::new_id("wfv");
    let now = timefmt::now();
    store.with_conn(|conn| {
        template_row(conn, template_id)?;
        conn.execute(
            "INSERT INTO workflow_template_versions(id, template_id, version_no, status, content_digest, created_by, created_at, updated_at)
             VALUES (?1,?2,
               (SELECT COALESCE(MAX(version_no),0)+1 FROM workflow_template_versions WHERE template_id=?2),
               'draft', ?3, ?4, ?5, ?5)",
            rusqlite::params![
                version_id,
                template_id,
                content_digest(defs),
                created_by,
                now
            ],
        )?;
        insert_defs(conn, &version_id, defs)?;
        version_row(conn, &version_id)
    })
}

fn insert_defs(
    conn: &rusqlite::Connection,
    version_id: &str,
    defs: &[GateDefInput],
) -> Result<(), Error> {
    // 策略列约定：None 落 '{}'（= 禁/缺省）；读向 parse 回 None。
    fn policy_json<T: serde::Serialize>(p: &Option<T>) -> String {
        p.as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".into()))
            .unwrap_or_else(|| "{}".into())
    }
    let now = timefmt::now();
    for (i, d) in defs.iter().enumerate() {
        conn.execute(
            "INSERT INTO workflow_gate_definitions(id, version_id, gate_id, ordinal, title, purpose, deliverables_json, acceptance_json, context_policy_ref, team_policy_ref, workspace_policy_ref, skip_policy_json, fast_track_policy_json, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            rusqlite::params![
                ids::new_id("wgd"),
                version_id,
                d.gate_id,
                (i + 1) as i64,
                d.title.trim(),
                d.purpose.trim(),
                serde_json::to_string(&d.deliverables).unwrap_or_else(|_| "[]".into()),
                serde_json::to_string(&d.acceptance).unwrap_or_else(|_| "[]".into()),
                d.context_policy_ref,
                d.team_policy_ref,
                d.workspace_policy_ref,
                policy_json(&d.skip_policy),
                policy_json(&d.fast_track_policy),
                now
            ],
        )?;
    }
    Ok(())
}

/// 策略列读向：'{}'/null/垃圾 → None（fail-closed：落回禁用缺省，不放大权限）。
fn parse_policy_json<T: serde::de::DeserializeOwned>(raw: &str) -> Option<T> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    if v.is_null() || v.as_object().map(|o| o.is_empty()).unwrap_or(false) {
        return None;
    }
    serde_json::from_value(v).ok()
}

/// 修改草稿版本的关卡定义（仅 draft；active/deprecated 拒绝——事实只追加）。
pub fn update_draft(
    store: &Store,
    version_id: &str,
    defs: &[GateDefInput],
) -> Result<VersionRecord, Error> {
    validate_defs(defs)?;
    store.with_conn(|conn| {
        let version = version_row(conn, version_id)?;
        if version.status != "draft" {
            return Err(Error::Message(
                "workflow_version_not_active: 仅 draft 版本可编辑".into(),
            ));
        }
        conn.execute(
            "DELETE FROM workflow_gate_definitions WHERE version_id=?1",
            [version_id],
        )?;
        insert_defs(conn, version_id, defs)?;
        conn.execute(
            "UPDATE workflow_template_versions SET content_digest=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![content_digest(defs), timefmt::now(), version_id],
        )?;
        version_row(conn, version_id)
    })
}

/// 激活：draft → active；同模板旧 active → deprecated。激活前重算 digest 校验内容未被篡改。
/// 整体在单事务内（缺陷审计 2026-09-07）：两条 UPDATE 非原子会出现"零 active 版本"窗口，
/// 期间 create_with_template 全量被砖且无自愈路径。
pub fn activate(store: &Store, version_id: &str) -> Result<VersionRecord, Error> {
    store.with_tx(|conn| {
        let version = version_row(conn, version_id)?;
        if version.status == "active" {
            return Ok(version); // 幂等重放
        }
        if version.status != "draft" {
            return Err(Error::Message(
                "workflow_version_not_active: 仅 draft 版本可激活".into(),
            ));
        }
        let defs = definitions(conn, version_id)?;
        let inputs: Vec<GateDefInput> = defs
            .iter()
            .map(|d| GateDefInput {
                gate_id: d.gate_id.clone(),
                title: d.title.clone(),
                purpose: d.purpose.clone(),
                deliverables: d.deliverables.clone(),
                acceptance: d.acceptance.clone(),
                context_policy_ref: d.context_policy_ref.clone(),
                team_policy_ref: d.team_policy_ref.clone(),
                workspace_policy_ref: d.workspace_policy_ref.clone(),
                skip_policy: d.skip_policy.clone(),
                fast_track_policy: d.fast_track_policy.clone(),
            })
            .collect();
        validate_defs(&inputs)?;
        let digest = content_digest(&inputs);
        if digest != version.content_digest {
            return Err(Error::Message(
                "workflow_template_invalid: 内容 digest 漂移，拒绝激活".into(),
            ));
        }
        let now = timefmt::now();
        conn.execute(
            "UPDATE workflow_template_versions SET status='deprecated', updated_at=?1
             WHERE template_id=?2 AND status='active'",
            rusqlite::params![now, version.template_id],
        )?;
        conn.execute(
            "UPDATE workflow_template_versions SET status='active', updated_at=?1 WHERE id=?2",
            rusqlite::params![now, version_id],
        )?;
        version_row(conn, version_id)
    })
}

/// 弃用：active → deprecated。阻止新实例冻结；既有实例不受影响（EV-002）。
pub fn deprecate(store: &Store, version_id: &str) -> Result<VersionRecord, Error> {
    store.with_conn(|conn| {
        let version = version_row(conn, version_id)?;
        if version.status == "deprecated" {
            return Ok(version); // 幂等重放
        }
        if version.status != "active" {
            return Err(Error::Message(
                "workflow_version_not_active: 仅 active 版本可弃用".into(),
            ));
        }
        conn.execute(
            "UPDATE workflow_template_versions SET status='deprecated', updated_at=?1 WHERE id=?2",
            rusqlite::params![timefmt::now(), version_id],
        )?;
        version_row(conn, version_id)
    })
}

/// 解析默认模板的 active 版本（迁移 0032 保证存在；缺失即拒服务）。
pub fn default_active_version_id(store: &Store) -> Result<String, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT v.id FROM workflow_template_versions v
             JOIN workflow_templates t ON t.id = v.template_id
             WHERE t.key=?1 AND v.status='active'",
            [DEFAULT_TEMPLATE_KEY],
            |r| r.get::<_, String>(0),
        )
        .map_err(|_| Error::Message("workflow_template_invalid: 默认模板无激活版本".into()))
    })
}

/// 按 key 解析 active 版本；key 为空回退默认模板。
pub fn active_version_id_for_key(store: &Store, key: &str) -> Result<String, Error> {
    let key = if key.is_empty() {
        DEFAULT_TEMPLATE_KEY
    } else {
        key
    };
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT v.id FROM workflow_template_versions v
             JOIN workflow_templates t ON t.id = v.template_id
             WHERE t.key=?1 AND v.status='active'",
            [key],
            |r| r.get::<_, String>(0),
        )
        .map_err(|_| Error::Message(format!("workflow_version_not_active: {key} 无激活版本")))
    })
}

/// 版本的关卡定义（ordinal 升序）。
pub fn definitions(
    conn: &rusqlite::Connection,
    version_id: &str,
) -> Result<Vec<GateDefinition>, Error> {
    let mut stmt = conn.prepare(
        "SELECT id, version_id, gate_id, ordinal, title, purpose, deliverables_json,
                COALESCE(acceptance_json,'[]'), context_policy_ref, team_policy_ref, workspace_policy_ref,
                COALESCE(skip_policy_json,'{}'), COALESCE(fast_track_policy_json,'{}')
         FROM workflow_gate_definitions WHERE version_id=?1 ORDER BY ordinal",
    )?;
    let rows = stmt.query_map([version_id], |r| {
        let skip_raw: String = r.get(11)?;
        let ft_raw: String = r.get(12)?;
        Ok(GateDefinition {
            id: r.get(0)?,
            version_id: r.get(1)?,
            gate_id: r.get(2)?,
            ordinal: r.get(3)?,
            title: r.get(4)?,
            purpose: r.get(5)?,
            deliverables: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
            acceptance: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default(),
            context_policy_ref: r.get(8)?,
            team_policy_ref: r.get(9)?,
            workspace_policy_ref: r.get(10)?,
            skip_policy: parse_policy_json(&skip_raw),
            fast_track_policy: parse_policy_json(&ft_raw),
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn definitions_via_store(
    store: &Store,
    version_id: &str,
) -> Result<Vec<GateDefinition>, Error> {
    store.with_conn(|conn| definitions(conn, version_id))
}

/// 模板列表（含版本摘要）。
pub fn list_templates(store: &Store) -> Result<Vec<(TemplateRecord, Vec<VersionRecord>)>, Error> {
    store.with_conn(|conn| {
        let mut templates = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT id, key, name, created_at, updated_at FROM workflow_templates ORDER BY created_at, id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(TemplateRecord {
                    id: r.get(0)?,
                    key: r.get(1)?,
                    name: r.get(2)?,
                    created_at: r.get(3)?,
                    updated_at: r.get(4)?,
                })
            })?;
            for row in rows {
                templates.push(row?);
            }
        }
        let mut out = Vec::new();
        for t in templates {
            let mut versions = Vec::new();
            let mut stmt = conn.prepare(
                "SELECT id, template_id, version_no, status, content_digest, created_by, created_at, updated_at
                 FROM workflow_template_versions WHERE template_id=?1 ORDER BY version_no",
            )?;
            let rows = stmt.query_map([&t.id], |r| {
                Ok(VersionRecord {
                    id: r.get(0)?,
                    template_id: r.get(1)?,
                    version_no: r.get(2)?,
                    status: r.get(3)?,
                    content_digest: r.get(4)?,
                    created_by: r.get(5)?,
                    created_at: r.get(6)?,
                    updated_at: r.get(7)?,
                })
            })?;
            for row in rows {
                versions.push(row?);
            }
            out.push((t, versions));
        }
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-wftpl-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Store::open(&dir, "test").unwrap()
    }

    fn three_gates() -> Vec<GateDefInput> {
        vec![
            GateDefInput {
                gate_id: "triage".into(),
                title: "分诊关".into(),
                purpose: "快速分诊".into(),
                deliverables: vec!["doc".into()],
                acceptance: vec![serde_json::json!("分诊结论落档")],
                context_policy_ref: None,
                team_policy_ref: None,
                workspace_policy_ref: None,
                skip_policy: None,
                fast_track_policy: None,
            },
            GateDefInput {
                gate_id: "fix".into(),
                title: "修复关".into(),
                purpose: "实施热修复".into(),
                deliverables: vec!["code".into()],
                acceptance: vec![],
                context_policy_ref: None,
                team_policy_ref: None,
                workspace_policy_ref: None,
                skip_policy: None,
                fast_track_policy: None,
            },
            GateDefInput {
                gate_id: "confirm".into(),
                title: "确认关".into(),
                purpose: "确认恢复".into(),
                deliverables: vec!["verification".into()],
                acceptance: vec![],
                context_policy_ref: None,
                team_policy_ref: None,
                workspace_policy_ref: None,
                skip_policy: None,
                fast_track_policy: None,
            },
        ]
    }

    #[test]
    fn builtin_default_template_exists_and_digest_stable() {
        let store = setup();
        let version_id = default_active_version_id(&store).unwrap();
        let defs = definitions_via_store(&store, &version_id).unwrap();
        assert_eq!(defs.len(), 6);
        assert_eq!(defs[0].gate_id, "requirements");
        assert_eq!(defs[5].deliverables, vec!["verification".to_string()]);
        // 内置模板 digest 与迁移常量一致（内容未被篡改）。
        let version = store.with_conn(|c| version_row(c, &version_id)).unwrap();
        let inputs: Vec<GateDefInput> = defs
            .iter()
            .map(|d| GateDefInput {
                gate_id: d.gate_id.clone(),
                title: d.title.clone(),
                purpose: d.purpose.clone(),
                deliverables: d.deliverables.clone(),
                acceptance: d.acceptance.clone(),
                context_policy_ref: d.context_policy_ref.clone(),
                team_policy_ref: d.team_policy_ref.clone(),
                workspace_policy_ref: d.workspace_policy_ref.clone(),
                skip_policy: d.skip_policy.clone(),
                fast_track_policy: d.fast_track_policy.clone(),
            })
            .collect();
        assert_eq!(content_digest(&inputs), version.content_digest);
    }

    /// WP-8：skip/fast-track 策略——schema 严格（未知字段创建即拒）、部署/迁移类
    /// 关恒 forbidden、策略参与 digest v4、roundtrip 读回保真。
    #[test]
    fn skip_fast_track_policy_validation_and_digest_v4() {
        let store = setup();
        let t = create_template(&store, "wp8-skip", "跳关策略").unwrap();
        let defs = |skip: Option<SkipPolicy>, ft: Option<FastTrackPolicy>| -> Vec<GateDefInput> {
            vec![GateDefInput {
                gate_id: "review".into(),
                title: "评审关".into(),
                purpose: String::new(),
                deliverables: vec!["doc".into()],
                acceptance: vec![],
                context_policy_ref: None,
                team_policy_ref: None,
                workspace_policy_ref: None,
                skip_policy: skip,
                fast_track_policy: ft,
            }]
        };
        // manual_approval 可创建（非部署类）。
        let v = create_version(
            &store,
            &t.id,
            &defs(
                Some(SkipPolicy {
                    mode: SkipMode::ManualApproval,
                }),
                None,
            ),
            "tester",
        )
        .unwrap();
        activate(&store, &v.id).unwrap();
        // 部署/迁移类交付物的关恒 forbidden：manual_approval 创建即拒。
        let deploy_defs = vec![GateDefInput {
            gate_id: "deploy".into(),
            title: "部署关".into(),
            purpose: String::new(),
            deliverables: vec!["deployment".into()],
            acceptance: vec![],
            context_policy_ref: None,
            team_policy_ref: None,
            workspace_policy_ref: None,
            skip_policy: Some(SkipPolicy {
                mode: SkipMode::ManualApproval,
            }),
            fast_track_policy: None,
        }];
        let err = create_version(&store, &t.id, &deploy_defs, "tester")
            .unwrap_err()
            .to_string();
        assert!(err.contains("恒 forbidden"), "{err}");
        // fast-track schema 严格：未知字段（越界豁免声明）反序列化即拒——
        // RPC/存储面无法构造出该策略，创建面无从接收（policy schema 校验即拒）。
        assert!(
            serde_json::from_value::<FastTrackPolicy>(serde_json::json!({
                "skippable_activities": ["self_test"],
                "waived_deliverables": [],
                "reduced_approval": true,
                "waive_policy_approvals": true
            }))
            .is_err()
        );
        // digest v4：策略进 digest（None 与 Some 哈希不同、同输入确定）。
        let d_none = content_digest(&defs(None, None));
        let d_skip = content_digest(&defs(
            Some(SkipPolicy {
                mode: SkipMode::ManualApproval,
            }),
            None,
        ));
        let d_ft = content_digest(&defs(
            None,
            Some(FastTrackPolicy {
                skippable_activities: vec!["self_test".into()],
                waived_deliverables: vec![WaivedDeliverable {
                    kind: "doc".into(),
                    substitute_evidence_kind: "manual".into(),
                }],
                reduced_approval: true,
            }),
        ));
        assert_eq!(d_none, content_digest(&defs(None, None)), "确定");
        assert_ne!(d_none, d_skip);
        assert_ne!(d_none, d_ft);
        assert_ne!(d_skip, d_ft);
        // 读回保真：策略随版本冻结。
        let readback = definitions_via_store(&store, &v.id).unwrap();
        assert_eq!(
            readback[0].skip_policy,
            Some(SkipPolicy {
                mode: SkipMode::ManualApproval
            })
        );
        assert_eq!(readback[0].fast_track_policy, None);
    }

    #[test]
    fn draft_activate_deprecate_lifecycle() {
        let store = setup();
        let t = create_template(&store, "hotfix-3", "三关热修复").unwrap();
        let v1 = create_version(&store, &t.id, &three_gates(), "tester").unwrap();
        assert_eq!(v1.version_no, 1);
        assert_eq!(v1.status, "draft");
        // 非法模板拒绝激活（EV-004：重复 gate_id）。
        let bad = vec![
            GateDefInput {
                gate_id: "fix".into(),
                title: "a".into(),
                purpose: String::new(),
                deliverables: vec!["code".into()],
                acceptance: vec![],
                context_policy_ref: None,
                team_policy_ref: None,
                workspace_policy_ref: None,
                skip_policy: None,
                fast_track_policy: None,
            },
            GateDefInput {
                gate_id: "fix".into(),
                title: "b".into(),
                purpose: String::new(),
                deliverables: vec!["code".into()],
                acceptance: vec![],
                context_policy_ref: None,
                team_policy_ref: None,
                workspace_policy_ref: None,
                skip_policy: None,
                fast_track_policy: None,
            },
        ];
        assert!(create_version(&store, &t.id, &bad, "tester").is_err());
        // draft 定义更新重算 digest。
        let v1 = update_draft(&store, &v1.id, &three_gates()).unwrap();
        assert!(activate(&store, &v1.id).unwrap().status == "active");
        // 二次激活幂等。
        assert_eq!(activate(&store, &v1.id).unwrap().status, "active");
        // active 不可编辑。
        assert!(update_draft(&store, &v1.id, &three_gates()).is_err());
        // 新 draft v2 激活 → 旧版本 deprecated（单 active）。
        let v2 = create_version(&store, &t.id, &three_gates(), "tester").unwrap();
        assert_eq!(v2.version_no, 2);
        activate(&store, &v2.id).unwrap();
        let v1 = store.with_conn(|c| version_row(c, &v1.id)).unwrap();
        assert_eq!(v1.status, "deprecated");
        // deprecate：新实例不再冻结它。
        deprecate(&store, &v2.id).unwrap();
        assert!(active_version_id_for_key(&store, "hotfix-3").is_err());
    }

    #[test]
    fn key_validation() {
        assert!(valid_key("six-gate-default"));
        assert!(valid_key("fix_1"));
        assert!(!valid_key("Fix"));
        assert!(!valid_key("1abc"));
        assert!(!valid_key("has space"));
    }

    /// WP-7：结构化 acceptance 全链——创建校验、digest v3 确定性、
    /// 读回（definitions）保真双形态；kill switch 关闭拒绝创建。
    #[test]
    fn structured_acceptance_lifecycle_and_digest_v3() {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let store = setup();
        let t = create_template(&store, "wp7-acc", "结构化验收").unwrap();
        let defs = |structured: bool| -> Vec<GateDefInput> {
            vec![GateDefInput {
                gate_id: "confirm".into(),
                title: "确认关".into(),
                purpose: String::new(),
                deliverables: vec!["verification".into()],
                acceptance: if structured {
                    vec![
                        serde_json::json!("自由文本仅展示"),
                        serde_json::json!({"verifier": "evidence_verified", "evidence_kind": "test_report", "min_count": 1}),
                    ]
                } else {
                    vec![serde_json::json!("自由文本仅展示")]
                },
                context_policy_ref: None,
                team_policy_ref: None,
                workspace_policy_ref: None,
                skip_policy: None,
                fast_track_policy: None,
            }]
        };
        // kill switch 关闭：结构化项创建即拒（feature_disabled）。
        std::env::remove_var(crate::acceptance::FLAG);
        assert!(create_version(&store, &t.id, &defs(true), "tester")
            .unwrap_err()
            .to_string()
            .contains("feature_disabled"));
        // 纯字符串版本不受 kill switch 影响。
        let v1 = create_version(&store, &t.id, &defs(false), "tester").unwrap();
        activate(&store, &v1.id).unwrap();
        // flag 开启：结构化版本可创建并激活；digest 确定且随结构化内容变化。
        std::env::set_var(crate::acceptance::FLAG, "1");
        let v2 = create_version(&store, &t.id, &defs(true), "tester").unwrap();
        assert!(activate(&store, &v2.id).unwrap().status == "active");
        std::env::remove_var(crate::acceptance::FLAG);
        let d2a = content_digest(&defs(true));
        let d2b = content_digest(&defs(true));
        assert_eq!(d2a, d2b, "digest v3 必须确定");
        assert_ne!(d2a, content_digest(&defs(false)), "结构化项必须进 digest");
        // 读回保真：字符串仍是字符串，对象仍是对象（InstanceGate/读模型同源）。
        let readback = definitions_via_store(&store, &v2.id).unwrap();
        assert_eq!(
            readback[0].acceptance[0],
            serde_json::json!("自由文本仅展示")
        );
        assert_eq!(
            readback[0].acceptance[1],
            serde_json::json!({"verifier": "evidence_verified", "evidence_kind": "test_report", "min_count": 1})
        );
    }
}
