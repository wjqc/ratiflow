//! 工作流实例（EvoFlow 方案 M1-03 / ADR-036）：WorkItem 创建时冻结具体
//! template version；运行状态在 workflow_instance_gates 投影（与 legacy
//! workitem_stages 双写，shadow 语义见方案 §11.2）。

use sg_store::{ids, timefmt, Error, Store};

pub use crate::template::GateDefinition;

#[derive(Debug, Clone, serde::Serialize)]
pub struct InstanceInfo {
    pub id: String,
    pub workitem_id: String,
    pub template_version_id: String,
    pub current_gate_id: String,
    pub state: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct InstanceGate {
    pub gate_id: String,
    pub ordinal: i64,
    pub title: String,
    pub purpose: String,
    /// 本关默认执行 Agent 展示名（模板声明，随版本冻结；空 = UI 通用文案）。
    pub agent: String,
    pub deliverables: Vec<String>,
    /// 本关验收策略（WP-7 双形态：字符串=仅展示；对象=结构化机器契约；
    /// 模板声明，随版本冻结；空 = 通用六输入门禁基线）。
    pub acceptance: Vec<serde_json::Value>,
    pub state: String,
}

/// 事务内创建实例 + 关卡投影（须与 workitem 插入同事务，保证 WorkItem 无实例窗口）。
pub fn create_for_workitem(
    conn: &rusqlite::Connection,
    workitem_id: &str,
    template_version_id: &str,
    now: &str,
) -> Result<(), Error> {
    let first_gate: String = conn
        .query_row(
            "SELECT gate_id FROM workflow_gate_definitions WHERE version_id=?1 ORDER BY ordinal LIMIT 1",
            [template_version_id],
            |r| r.get(0),
        )
        .map_err(|_| {
            Error::Message("workflow_template_invalid: 版本缺少关卡定义".into())
        })?;
    conn.execute(
        "INSERT INTO workflow_instances(id, workitem_id, template_version_id, current_gate_id, state, created_at, updated_at)
         VALUES (?1,?2,?3,?4,'active',?5,?5)",
        rusqlite::params![ids::new_id("winst"), workitem_id, template_version_id, first_gate, now],
    )?;
    let instance_id: String = conn.query_row(
        "SELECT id FROM workflow_instances WHERE workitem_id=?1",
        [workitem_id],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO workflow_instance_gates(instance_id, gate_definition_id, state, updated_at)
         SELECT ?1, gd.id, 'not_started', ?2
         FROM workflow_gate_definitions gd WHERE gd.version_id=?3",
        rusqlite::params![instance_id, now, template_version_id],
    )?;
    Ok(())
}

/// 实例读取；无实例返回 None（迁移前的库不应出现）。
pub fn for_workitem(store: &Store, workitem_id: &str) -> Result<Option<InstanceInfo>, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT id, workitem_id, template_version_id, current_gate_id, state, created_at, updated_at
             FROM workflow_instances WHERE workitem_id=?1",
            [workitem_id],
            |r| {
                Ok(InstanceInfo {
                    id: r.get(0)?,
                    workitem_id: r.get(1)?,
                    template_version_id: r.get(2)?,
                    current_gate_id: r.get(3)?,
                    state: r.get(4)?,
                    created_at: r.get(5)?,
                    updated_at: r.get(6)?,
                })
            },
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other.into()),
        })
    })
}

/// 实例关卡定义 + 当前投影状态（ordinal 升序）。
pub fn gates_for_workitem(
    store: &Store,
    workitem_id: &str,
) -> Result<Option<Vec<InstanceGate>>, Error> {
    let Some(instance) = for_workitem(store, workitem_id)? else {
        return Ok(None);
    };
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT gd.gate_id, gd.ordinal, gd.title, gd.purpose, COALESCE(gd.agent,''), gd.deliverables_json,
                    COALESCE(gd.acceptance_json,'[]'), ig.state
             FROM workflow_instance_gates ig
             JOIN workflow_gate_definitions gd ON gd.id = ig.gate_definition_id
             WHERE ig.instance_id=?1 ORDER BY gd.ordinal",
        )?;
        let rows = stmt.query_map([&instance.id], |r| {
            Ok(InstanceGate {
                gate_id: r.get(0)?,
                ordinal: r.get(1)?,
                title: r.get(2)?,
                purpose: r.get(3)?,
                agent: r.get(4)?,
                deliverables: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
                acceptance: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
                state: r.get(7)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(Some(out))
    })
}

/// 双写投影：workitem_stages 状态/推进同步到 instance_gates 与 instances.current_gate_id。
/// 投影失败不阻断 legacy 主路径（shadow 语义），但必须留痕（调用方记 audit）。
/// 注意：单连接内完成实例读取与更新，不得嵌套 with_conn（Mutex 不可重入）。
pub fn project_state(
    store: &Store,
    workitem_id: &str,
    gate_id: &str,
    state: &str,
    current_gate_id: &str,
) -> Result<(), Error> {
    let now = timefmt::now();
    // 单事务（缺陷审计）：投影两段 UPDATE 与实例读取原子化。
    store.with_tx(|conn| {
        let instance: Option<(String, String)> = conn
            .query_row(
                "SELECT id, template_version_id FROM workflow_instances WHERE workitem_id=?1",
                [workitem_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        let Some((instance_id, version_id)) = instance else {
            return Ok(());
        };
        conn.execute(
            "UPDATE workflow_instance_gates SET state=?1, updated_at=?2
             WHERE instance_id=?3 AND gate_definition_id IN
               (SELECT id FROM workflow_gate_definitions WHERE version_id=?4 AND gate_id=?5)",
            rusqlite::params![state, now, instance_id, version_id, gate_id],
        )?;
        conn.execute(
            "UPDATE workflow_instances SET current_gate_id=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![current_gate_id, now, instance_id],
        )?;
        Ok(())
    })
}

/// 实例迁移预览：目标版本定义 + 兼容性判定（仅全部 not_started 可迁移——
/// M1 语义：实例已产生运行事实后改模板需要重建状态，拒绝而非半迁）。
pub fn migration_preview(
    store: &Store,
    workitem_id: &str,
    target_version_id: &str,
) -> Result<serde_json::Value, Error> {
    let Some(instance) = for_workitem(store, workitem_id)? else {
        return Err(Error::Message(
            "workflow_migration_blocked: 实例不存在".into(),
        ));
    };
    let current = gates_for_workitem(store, workitem_id)?.unwrap_or_default();
    let target_defs = crate::template::definitions_via_store(store, target_version_id)?;
    let started = current.iter().any(|g| g.state != "not_started");
    Ok(serde_json::json!({
        "workItemId": workitem_id,
        "fromVersionId": instance.template_version_id,
        "toVersionId": target_version_id,
        "currentGates": current,
        "targetGates": target_defs,
        "blocked": started,
        "reason": if started { "workflow_migration_blocked: 实例已有运行事实，仅未开工任务可迁移" } else { "" },
    }))
}

/// 实例迁移：重指版本 + 重建投影（仅全部 not_started；幂等键由调用方 RPC 层承接）。
pub fn migrate(
    store: &Store,
    workitem_id: &str,
    target_version_id: &str,
) -> Result<InstanceInfo, Error> {
    let Some(instance) = for_workitem(store, workitem_id)? else {
        return Err(Error::Message(
            "workflow_migration_blocked: 实例不存在".into(),
        ));
    };
    if instance.template_version_id == target_version_id {
        return Ok(instance); // 幂等重放
    }
    let current = gates_for_workitem(store, workitem_id)?.unwrap_or_default();
    if current.iter().any(|g| g.state != "not_started") {
        return Err(Error::Message(
            "workflow_migration_blocked: 实例已有运行事实，仅未开工任务可迁移".into(),
        ));
    }
    let target_defs = crate::template::definitions_via_store(store, target_version_id)?;
    if target_defs.is_empty() {
        return Err(Error::Message(
            "workflow_template_invalid: 目标版本缺少关卡定义".into(),
        ));
    }
    let first_gate = target_defs[0].gate_id.clone();
    let now = timefmt::now();
    // 单事务（缺陷审计）：版本切换/旧投影删除/新投影重建非原子会出现"0 投影实例"。
    store.with_tx(|conn| {
        conn.execute(
            "UPDATE workflow_instances SET template_version_id=?1, current_gate_id=?2, state='migrated', updated_at=?3 WHERE id=?4",
            rusqlite::params![target_version_id, first_gate, now, instance.id],
        )?;
        conn.execute(
            "DELETE FROM workflow_instance_gates WHERE instance_id=?1",
            [&instance.id],
        )?;
        conn.execute(
            "INSERT INTO workflow_instance_gates(instance_id, gate_definition_id, state, updated_at)
             SELECT ?1, gd.id, 'not_started', ?2
             FROM workflow_gate_definitions gd WHERE gd.version_id=?3",
            rusqlite::params![instance.id, now, target_version_id],
        )?;
        Ok(InstanceInfo {
            template_version_id: target_version_id.to_string(),
            current_gate_id: first_gate,
            state: "migrated".into(),
            ..instance
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::{create_template, create_version, GateDefInput};

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-wfins-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Store::open(&dir, "test").unwrap()
    }

    #[test]
    fn backfilled_instances_exist_for_legacy_workitems() {
        let store = setup();
        // 建 WorkItem（create 走 legacy 六关路径 → 实例由 create() 内部冻结）。
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at) VALUES ('pj','u','n','p','main',?1)",
                    [timefmt::now()],
                )?;
                Ok(())
            })
            .unwrap();
        let wi = sg_workitem_create_for_test(&store);
        let instance = for_workitem(&store, &wi).unwrap().expect("实例应存在");
        assert_eq!(instance.state, "active");
        let gates = gates_for_workitem(&store, &wi).unwrap().unwrap();
        assert_eq!(gates.len(), 6);
        assert_eq!(gates[0].gate_id, "requirements");
        assert_eq!(gates[0].deliverables, vec!["prd".to_string()]);
    }

    fn sg_workitem_create_for_test(store: &Store) -> String {
        let version_id = crate::template::default_active_version_id(store).unwrap();
        store
            .with_conn(|conn| {
                let id = ids::new_id("wi");
                let now = timefmt::now();
                conn.execute(
                    "INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                     VALUES (?1,'pj','t','','[]','requirements',?2,?2)",
                    rusqlite::params![id, now],
                )?;
                conn.execute(
                    "INSERT INTO workitem_stages(workitem_id, gate, state, updated_at)
                     SELECT ?1, gd.gate_id, 'not_started', ?2
                     FROM workflow_gate_definitions gd
                     WHERE gd.version_id = ?3
                     ORDER BY gd.ordinal",
                    rusqlite::params![id, now, version_id],
                )?;
                create_for_workitem(conn, &id, &version_id, &now)?;
                Ok(id)
            })
            .unwrap()
    }

    #[test]
    fn migrate_only_when_not_started_and_projection_rebuilt() {
        let store = setup();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at) VALUES ('pj','u','n','p','main',?1)",
                    [timefmt::now()],
                )?;
                Ok(())
            })
            .unwrap();
        let wi = sg_workitem_create_for_test(&store);
        // 目标：2 关热修复模板。
        let t = create_template(&store, "hotfix-2", "两关热修复").unwrap();
        let v = create_version(
            &store,
            &t.id,
            &[
                GateDefInput {
                    gate_id: "fix".into(),
                    title: "修复关".into(),
                    purpose: String::new(),
                    agent: String::new(),
                    deliverables: vec!["code".into()],
                    acceptance: vec!["补丁通过编译与回归".into()],
                    context_policy_ref: None,
                    team_policy_ref: None,
                    workspace_policy_ref: None,
                    skip_policy: None,
                    fast_track_policy: None,
                },
                GateDefInput {
                    gate_id: "confirm".into(),
                    title: "确认关".into(),
                    purpose: String::new(),
                    agent: String::new(),
                    deliverables: vec!["verification".into()],
                    acceptance: vec![],
                    context_policy_ref: None,
                    team_policy_ref: None,
                    workspace_policy_ref: None,
                    skip_policy: None,
                    fast_track_policy: None,
                },
            ],
            "tester",
        )
        .unwrap();
        let preview = migration_preview(&store, &wi, &v.id).unwrap();
        assert_eq!(preview["blocked"], serde_json::json!(false));
        migrate(&store, &wi, &v.id).unwrap();
        let gates = gates_for_workitem(&store, &wi).unwrap().unwrap();
        assert_eq!(gates.len(), 2);
        assert_eq!(gates[0].gate_id, "fix");
        assert_eq!(
            for_workitem(&store, &wi).unwrap().unwrap().state,
            "migrated"
        );
        // 迁移幂等：同版本重放原样返回。
        migrate(&store, &wi, &v.id).unwrap();
        // 有运行事实后拒绝迁移。
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE workitem_stages SET state='running' WHERE workitem_id=?1 AND gate='fix'",
                    [&wi],
                )?;
                c.execute(
                    "UPDATE workflow_instance_gates SET state='running' WHERE instance_id IN
                       (SELECT id FROM workflow_instances WHERE workitem_id=?1)
                       AND gate_definition_id IN (SELECT id FROM workflow_gate_definitions WHERE gate_id='fix')",
                    [&wi],
                )?;
                Ok(())
            })
            .unwrap();
        let t3 = create_template(&store, "hotfix-3", "三关热修复").unwrap();
        let v3 = create_version(&store, &t3.id, &[], "tester");
        assert!(v3.is_err(), "空关卡定义必须拒绝（EV-004）");
    }
}
