//! 数据化工作流模板（EvoFlow 方案 M1-02 / ADR-036）：
//! 逻辑身份（workflow_templates）+ 不可变版本（workflow_template_versions，
//! draft → active → deprecated）+ 关卡定义（workflow_gate_definitions）。
//! gate_id 是模板版本内稳定字符串，schema 不假设固定六关；active 版本不可原地编辑。

use sg_store::{ids, timefmt, Error, Store};
use sha2::{Digest, Sha256};

/// M1 功能开关（方案 §11.1）：`SIXGATES_WORKFLOW_TEMPLATE_V2`。
/// 默认关闭 = 新模板管理 RPC 与非默认模板创建不可用；默认六关行为不变。
/// 实例顺序解析对默认模板与 legacy 枚举逐字相同（parity 由单测断言），
/// 因此关闭 flag 不影响已存在实例的一致性（§11.3 只读延续）。
pub fn template_v2_enabled() -> bool {
    std::env::var("SIXGATES_WORKFLOW_TEMPLATE_V2")
        .ok()
        .as_deref()
        == Some("1")
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

#[derive(Debug, Clone, serde::Serialize)]
pub struct GateDefinition {
    pub id: String,
    pub version_id: String,
    pub gate_id: String,
    pub ordinal: i64,
    pub title: String,
    pub purpose: String,
    pub deliverables: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_policy_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_policy_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_policy_ref: Option<String>,
}

/// 版本内容的输入形状（ordinal 由数组位置隐含：1..n 连续）。
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct GateDefInput {
    pub gate_id: String,
    pub title: String,
    #[serde(default)]
    pub purpose: String,
    #[serde(default)]
    pub deliverables: Vec<String>,
}

/// 版本内容 digest（迁移 0032 内置模板常量同公式）：
/// sha256("v1|" + join("ordinal|gate_id|title|purpose|deliverables_csv", "\n"))。
pub fn content_digest(defs: &[GateDefInput]) -> String {
    let lines: Vec<String> = defs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            format!(
                "{}|{}|{}|{}|{}",
                i + 1,
                d.gate_id,
                d.title,
                d.purpose,
                d.deliverables.join(",")
            )
        })
        .collect();
    let mut hasher = Sha256::new();
    hasher.update(format!("v1|{}", lines.join("\n")).as_bytes());
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
    let now = timefmt::now();
    for (i, d) in defs.iter().enumerate() {
        conn.execute(
            "INSERT INTO workflow_gate_definitions(id, version_id, gate_id, ordinal, title, purpose, deliverables_json, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![
                ids::new_id("wgd"),
                version_id,
                d.gate_id,
                (i + 1) as i64,
                d.title.trim(),
                d.purpose.trim(),
                serde_json::to_string(&d.deliverables).unwrap_or_else(|_| "[]".into()),
                now
            ],
        )?;
    }
    Ok(())
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
pub fn activate(store: &Store, version_id: &str) -> Result<VersionRecord, Error> {
    store.with_conn(|conn| {
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
                context_policy_ref, team_policy_ref, workspace_policy_ref
         FROM workflow_gate_definitions WHERE version_id=?1 ORDER BY ordinal",
    )?;
    let rows = stmt.query_map([version_id], |r| {
        Ok(GateDefinition {
            id: r.get(0)?,
            version_id: r.get(1)?,
            gate_id: r.get(2)?,
            ordinal: r.get(3)?,
            title: r.get(4)?,
            purpose: r.get(5)?,
            deliverables: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
            context_policy_ref: r.get(7)?,
            team_policy_ref: r.get(8)?,
            workspace_policy_ref: r.get(9)?,
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
            },
            GateDefInput {
                gate_id: "fix".into(),
                title: "修复关".into(),
                purpose: "实施热修复".into(),
                deliverables: vec!["code".into()],
            },
            GateDefInput {
                gate_id: "confirm".into(),
                title: "确认关".into(),
                purpose: "确认恢复".into(),
                deliverables: vec!["verification".into()],
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
            })
            .collect();
        assert_eq!(content_digest(&inputs), version.content_digest);
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
            },
            GateDefInput {
                gate_id: "fix".into(),
                title: "b".into(),
                purpose: String::new(),
                deliverables: vec!["code".into()],
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
}
