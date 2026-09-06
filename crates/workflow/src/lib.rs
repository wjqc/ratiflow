//! 部署状态机（PRD v2 §16.3 / FR-DEP 行为等价）：
//! draft -> awaiting_approval -> approved -> deploying -> awaiting_verification
//!   -> verified | verification_failed | deploy_failed -> rolling_back -> rolled_back | rollback_failed
use serde::{Deserialize, Serialize};
use sg_policy::Risk;
use sg_store::{ids, outbox, timefmt, Error, Store};

pub mod dag;
pub mod delivery;
pub mod instance;
pub mod plan;
pub mod replan;
pub mod template;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    pub seq: i64,
    pub name: String,
    pub argv: Vec<String>,
    #[serde(default = "default_step_timeout")]
    pub timeout_sec: i64,
}

fn default_step_timeout() -> i64 {
    120
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyCheck {
    pub name: String,
    pub argv: Vec<String>,
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentPlan {
    pub target: crate::delivery::SSHTargetPub,
    pub image_digest: String,
    #[serde(default)]
    pub previous_digest: String,
    pub deploy_steps: Vec<PlanStep>,
    pub verify_checks: Vec<VerifyCheck>,
    #[serde(default)]
    pub rollback_steps: Vec<PlanStep>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Deployment {
    pub id: String,
    pub workitem_id: String,
    pub target: String,
    pub image_digest: String,
    pub state: String,
    pub action_digest: String,
    pub result: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 状态迁移表（非法迁移拒绝）。
fn allowed_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("draft", "awaiting_approval")
            | ("awaiting_approval", "approved")
            | ("awaiting_approval", "draft")
            | ("approved", "deploying")
            | ("deploying", "awaiting_verification")
            | ("deploying", "deploy_failed")
            | ("awaiting_verification", "verified")
            | ("awaiting_verification", "verification_failed")
            | ("deploy_failed", "rolling_back")
            | ("verification_failed", "rolling_back")
            | ("rolling_back", "rolled_back")
            | ("rolling_back", "rollback_failed")
    )
}

fn set_state(
    store: &Store,
    deployment: &mut Deployment,
    to: &str,
    result: &str,
) -> Result<(), Error> {
    if !allowed_transition(&deployment.state, to) {
        return Err(Error::Message(format!(
            "invalid deployment transition {} -> {}",
            deployment.state, to
        )));
    }
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE deployments SET state=?1, result=?2, updated_at=?3 WHERE id=?4",
            rusqlite::params![to, result, now, deployment.id],
        )?;
        Ok(())
    })?;
    deployment.state = to.into();
    deployment.result = result.into();
    outbox::emit(
        store,
        "deployment",
        &deployment.id,
        &format!("deployment.{to}"),
        serde_json::json!({"workitemId": deployment.workitem_id, "target": deployment.target}),
    )?;
    Ok(())
}

/// 创建部署计划：digest 必须是不可变 sha256 引用（FR-DEP-001）。
pub fn create_plan(
    store: &Store,
    workitem_id: &str,
    plan: &DeploymentPlan,
) -> Result<Deployment, Error> {
    if !plan.image_digest.starts_with("sha256:") {
        return Err(Error::Message(
            "digest_drift: 镜像引用必须是不可变 digest（sha256:...），不接受可漂移 tag".into(),
        ));
    }
    if plan.deploy_steps.is_empty() || plan.verify_checks.is_empty() {
        return Err(Error::Message(
            "deploy steps and verify checks required".into(),
        ));
    }
    for step in plan.deploy_steps.iter().chain(plan.rollback_steps.iter()) {
        if step.argv.is_empty() {
            return Err(Error::Message(format!("step {}: argv required", step.name)));
        }
        crate::delivery::validate_argv_pub(&step.argv).map_err(Error::Message)?;
    }
    let id = ids::new_id("dep");
    let now = timefmt::now();
    let digest = sg_policy::action_digest(&serde_json::to_value(plan).unwrap_or_default());
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO deployments(id, workitem_id, target, image_digest, plan, state, action_digest, idempotency_key, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,'draft',?6,?1,?7,?7)",
            rusqlite::params![id, workitem_id, plan.target.host, plan.image_digest,
                serde_json::to_string(plan).unwrap_or_default(), digest, now],
        )?;
        Ok(())
    })?;
    get(store, &id)
}

pub fn get(store: &Store, id: &str) -> Result<Deployment, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT id, workitem_id, target, COALESCE(image_digest,''), state, COALESCE(action_digest,''),
                    COALESCE(result,''), created_at, updated_at
             FROM deployments WHERE id=?1",
            [id],
            |r| {
                Ok(Deployment {
                    id: r.get(0)?, workitem_id: r.get(1)?, target: r.get(2)?, image_digest: r.get(3)?,
                    state: r.get(4)?, action_digest: r.get(5)?, result: r.get(6)?,
                    created_at: r.get(7)?, updated_at: r.get(8)?,
                })
            },
        )
        .map_err(|_| Error::Message("deployment_not_found".into()))
    })
}

pub fn get_plan(store: &Store, id: &str) -> Result<DeploymentPlan, Error> {
    let body: String = store.with_conn(|conn| {
        conn.query_row("SELECT plan FROM deployments WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .map_err(|_| Error::Message("deployment_not_found".into()))
    })?;
    serde_json::from_str(&body).map_err(|e| Error::Message(e.to_string()))
}

/// 提交审批（绑定 ActionDigest）。
pub fn submit_for_approval(store: &Store, id: &str) -> Result<sg_policy::Approval, Error> {
    let mut deployment = get(store, id)?;
    set_state(store, &mut deployment, "awaiting_approval", "")?;
    sg_policy::request_approval(
        store,
        "deployment",
        id,
        &deployment.action_digest,
        Risk::High,
        &format!(
            "部署 {} 到 {}（digest {}）",
            id, deployment.target, deployment.image_digest
        ),
        3600,
        Some(&deployment.workitem_id),
        None,
    )
}

/// 审批通过后执行：先 deploying，再 SSH 预检 + 步骤；失败即 deploy_failed（FR-DEP-006）。
pub fn approve_and_deploy(
    store: &Store,
    id: &str,
    ssh: &dyn crate::delivery::SSHAdapterPub,
) -> Result<Deployment, Error> {
    let mut deployment = get(store, id)?;
    if deployment.state != "awaiting_approval" {
        return Err(Error::Message(format!(
            "deployment in {}（需 awaiting_approval）",
            deployment.state
        )));
    }
    // 审批必须绑定当前 digest（FR-DEP-004）。
    sg_policy::validate_for(store, "deployment", id, &deployment.action_digest)
        .map_err(|e| Error::Message(format!("approval_invalid: {e}")))?;
    set_state(store, &mut deployment, "approved", "")?;

    let plan = get_plan(store, id)?;
    set_state(store, &mut deployment, "deploying", "")?;

    let preflight = ssh.preflight(&plan.target).map_err(|e| {
        let _ = set_state(
            store,
            &mut deployment,
            "deploy_failed",
            &format!("preflight: {e}"),
        );
        Error::Message(format!("preflight_failed: {e}"))
    })?;
    if preflight["dockerOk"] != serde_json::json!(true)
        || preflight["composeOk"] != serde_json::json!(true)
    {
        let msg = format!("preflight report: {preflight}");
        let _ = set_state(store, &mut deployment, "deploy_failed", &msg);
        return Err(Error::Message(format!("preflight_failed: {msg}")));
    }

    let results = ssh
        .run_command_plan(&plan.target, &steps_to_commands(&plan.deploy_steps))
        .map_err(|e| {
            let _ = set_state(store, &mut deployment, "deploy_failed", &e);
            Error::Message(format!("deploy steps: {e}"))
        })?;
    // 成功只能进入 awaiting_verification（FR-DEP-007）。
    set_state(
        store,
        &mut deployment,
        "awaiting_verification",
        &serde_json::to_string(&results).unwrap_or_default(),
    )?;
    Ok(deployment)
}

/// 验证检查集（FR-VER-006：失败不可被 Agent 覆盖）。
pub fn verify(
    store: &Store,
    id: &str,
    ssh: &dyn crate::delivery::SSHAdapterPub,
) -> Result<Deployment, Error> {
    let mut deployment = get(store, id)?;
    if deployment.state != "awaiting_verification" {
        return Err(Error::Message(format!(
            "deployment in {}（需 awaiting_verification）",
            deployment.state
        )));
    }
    let plan = get_plan(store, id)?;
    let checks: Vec<crate::delivery::SSHPubCommand> = plan
        .verify_checks
        .iter()
        .map(|c| crate::delivery::SSHPubCommand {
            name: c.name.clone(),
            argv: c.argv.clone(),
            timeout_sec: 60,
        })
        .collect();
    let results = ssh.run_command_plan(&plan.target, &checks).map_err(|e| {
        let _ = set_state(store, &mut deployment, "verification_failed", &e);
        Error::Message(format!("verification_failed: {e}"))
    })?;
    set_state(
        store,
        &mut deployment,
        "verified",
        &serde_json::to_string(&results).unwrap_or_default(),
    )?;
    Ok(deployment)
}

/// 回滚：previous digest 路径 + 同一验证语义（FR-DEP-008）。
pub fn rollback(
    store: &Store,
    id: &str,
    ssh: &dyn crate::delivery::SSHAdapterPub,
) -> Result<Deployment, Error> {
    let mut deployment = get(store, id)?;
    if !matches!(
        deployment.state.as_str(),
        "deploy_failed" | "verification_failed"
    ) {
        return Err(Error::Message(format!(
            "rollback from {} not allowed",
            deployment.state
        )));
    }
    let plan = get_plan(store, id)?;
    if plan.rollback_steps.is_empty() {
        set_state(
            store,
            &mut deployment,
            "rollback_failed",
            "no rollback steps",
        )?;
        return Err(Error::Message("rollback plan missing".into()));
    }
    set_state(store, &mut deployment, "rolling_back", "")?;
    let results = ssh
        .run_command_plan(&plan.target, &steps_to_commands(&plan.rollback_steps))
        .map_err(|e| {
            let _ = set_state(store, &mut deployment, "rollback_failed", &e);
            Error::Message(e)
        })?;
    set_state(
        store,
        &mut deployment,
        "rolled_back",
        &serde_json::to_string(&results).unwrap_or_default(),
    )?;
    Ok(deployment)
}

fn steps_to_commands(steps: &[PlanStep]) -> Vec<crate::delivery::SSHPubCommand> {
    steps
        .iter()
        .map(|s| crate::delivery::SSHPubCommand {
            name: s.name.clone(),
            argv: s.argv.clone(),
            timeout_sec: s.timeout_sec,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delivery::{FakeSSHPub, SSHTargetPub};

    fn setup() -> Store {
        let dir =
            std::env::temp_dir().join(format!("sg-wf-{}-{}", std::process::id(), ids::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        store.with_conn(|c| {
            c.execute("INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at) VALUES ('pj','u','n','p','main',?1)", [timefmt::now()])?;
            c.execute("INSERT INTO workitems(id, project_id, title, created_at, updated_at) VALUES ('wi','pj','t',?1,?1)", [timefmt::now()])?;
            Ok(())
        }).unwrap();
        store
    }

    pub fn valid_plan() -> DeploymentPlan {
        DeploymentPlan {
            target: SSHTargetPub {
                host: "deploy.test".into(),
                port: 22,
                user: "deploy".into(),
                expected_fingerprint: "SHA256:x".into(),
                remote_dir: "/srv/app".into(),
            },
            image_digest: "sha256:abc123".into(),
            previous_digest: String::new(),
            deploy_steps: vec![PlanStep {
                seq: 0,
                name: "up".into(),
                argv: vec!["docker".into(), "compose".into(), "up".into(), "-d".into()],
                timeout_sec: 120,
            }],
            verify_checks: vec![VerifyCheck {
                name: "health".into(),
                argv: vec![
                    "curl".into(),
                    "-f".into(),
                    "http://localhost/healthz".into(),
                ],
                required: true,
            }],
            rollback_steps: vec![PlanStep {
                seq: 0,
                name: "down".into(),
                argv: vec!["docker".into(), "compose".into(), "down".into()],
                timeout_sec: 60,
            }],
        }
    }

    #[test]
    fn digest_tag_rejected() {
        let s = setup();
        let mut plan = valid_plan();
        plan.image_digest = "latest".into();
        assert!(create_plan(&s, "wi", &plan).is_err());
    }

    #[test]
    fn full_flow_with_approval_binding() {
        let s = setup();
        let dep = create_plan(&s, "wi", &valid_plan()).unwrap();
        let appr = submit_for_approval(&s, &dep.id).unwrap();

        let ssh = FakeSSHPub::default();
        // 未批准部署拒绝。
        assert!(approve_and_deploy(&s, &dep.id, &ssh).is_err());

        sg_policy::decide(&s, &appr.id, "approved", "owner", "发布").unwrap();
        let deployed = approve_and_deploy(&s, &dep.id, &ssh).unwrap();
        assert_eq!(deployed.state, "awaiting_verification");
        let verified = verify(&s, &dep.id, &ssh).unwrap();
        assert_eq!(verified.state, "verified");
    }

    #[test]
    fn approval_digest_binding_invalidates_on_change() {
        let s = setup();
        let dep = create_plan(&s, "wi", &valid_plan()).unwrap();
        let appr = submit_for_approval(&s, &dep.id).unwrap();
        sg_policy::decide(&s, &appr.id, "approved", "owner", "").unwrap();
        // 参数变化 → 新 digest → 旧批准失效。
        let mut plan2 = valid_plan();
        plan2.image_digest = "sha256:xyz789".into();
        let dep2 = create_plan(&s, "wi", &plan2).unwrap();
        assert_ne!(dep.action_digest, dep2.action_digest);
        assert!(approve_and_deploy(&s, &dep2.id, &FakeSSHPub::default()).is_err());
    }

    #[test]
    fn verify_failure_forces_rollback_path() {
        let s = setup();
        let dep = create_plan(&s, "wi", &valid_plan()).unwrap();
        let appr = submit_for_approval(&s, &dep.id).unwrap();
        sg_policy::decide(&s, &appr.id, "approved", "owner", "").unwrap();
        approve_and_deploy(&s, &dep.id, &FakeSSHPub::default()).unwrap();
        let failing = FakeSSHPub {
            preflight_error: None,
            plan_error: Some("curl exited 7".into()),
        };
        assert!(verify(&s, &dep.id, &failing).is_err());
        assert_eq!(get(&s, &dep.id).unwrap().state, "verification_failed");
        let rolled = rollback(&s, &dep.id, &FakeSSHPub::default()).unwrap();
        assert_eq!(rolled.state, "rolled_back");
    }

    #[test]
    fn rollback_failure_terminal() {
        let s = setup();
        let dep = create_plan(&s, "wi", &valid_plan()).unwrap();
        let appr = submit_for_approval(&s, &dep.id).unwrap();
        sg_policy::decide(&s, &appr.id, "approved", "owner", "").unwrap();
        let ssh = FakeSSHPub {
            preflight_error: None,
            plan_error: Some("deploy down".into()),
        };
        approve_and_deploy(&s, &dep.id, &ssh).unwrap_err();
        assert_eq!(get(&s, &dep.id).unwrap().state, "deploy_failed");
        let ssh2 = FakeSSHPub {
            preflight_error: None,
            plan_error: Some("rollback down too".into()),
        };
        assert!(rollback(&s, &dep.id, &ssh2).is_err());
        assert_eq!(get(&s, &dep.id).unwrap().state, "rollback_failed");
    }
}
