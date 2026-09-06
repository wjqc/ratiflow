//! 自治模式与授权（EvoFlow 方案 M3-03 / ADR-037 §6.5）：
//! Ask/Agent/Plan 与 executor 隔离模式正交；AutonomyGrant 限范围/限时/限额验证。
//! Goal（v1/v2）属 M6。默认无 grant = 默认人工放行不变（§3 不变量 1）。

use serde::{Deserialize, Serialize};
use sg_store::{Error, Store};

/// 自治模式（§6.5）。随 agent.start 存入 policy_snapshot JSON（autonomyMode）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyMode {
    /// 回答/受控只读检索；写工具一律拒绝。
    Ask,
    /// 单 Run 工具循环，按 ToolRule 逐动作审批（legacy 默认）。
    Agent,
    /// 先冻结计划再按 DAG 执行；批计划不覆盖 High/external/irreversible 审批。
    Plan,
}

impl AutonomyMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            AutonomyMode::Ask => "ask",
            AutonomyMode::Agent => "agent",
            AutonomyMode::Plan => "plan",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ask" => Some(AutonomyMode::Ask),
            "agent" => Some(AutonomyMode::Agent),
            "plan" => Some(AutonomyMode::Plan),
            _ => None,
        }
    }
}

/// Ask 模式工具面：写 effect 一律拒绝（§6.5 表：默认无文件/外部写）。
/// 返回 Err(token) = 拒绝。
pub fn validate_tool_for_mode(
    mode: AutonomyMode,
    effect_class: Option<crate::plan_guard::EffectClass>,
) -> Result<(), &'static str> {
    match mode {
        AutonomyMode::Ask => {
            if matches!(
                effect_class,
                Some(crate::plan_guard::EffectClass::LocalWrite)
                    | Some(crate::plan_guard::EffectClass::ExternalWrite)
                    | Some(crate::plan_guard::EffectClass::Irreversible)
            ) {
                return Err("autonomy_mode_denied: Ask 模式拒绝写副作用工具");
            }
            // unknown effect 在 Ask 模式同样拒绝（保守）。
            if effect_class.is_none() {
                return Err("plan_guard_unknown_effect: Ask 模式无法确认工具副作用");
            }
            Ok(())
        }
        // Agent/Plan 的工具面由 ToolRule/PlanGuard/审批链承接，此处不重复判定。
        AutonomyMode::Agent | AutonomyMode::Plan => Ok(()),
    }
}

/// Grant 验证错误（稳定 token，§8.3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantError {
    NotFound,
    Revoked,
    Expired,
    Exhausted,
    ToolNotAllowed,
    RiskNotAllowed,
}

impl std::fmt::Display for GrantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let token = match self {
            GrantError::NotFound => "autonomy_grant_required",
            GrantError::Revoked => "autonomy_grant_revoked",
            GrantError::Expired => "autonomy_grant_expired",
            GrantError::Exhausted => "autonomy_budget_exhausted",
            GrantError::ToolNotAllowed => "autonomy_grant_tool_denied",
            GrantError::RiskNotAllowed => "autonomy_grant_risk_denied",
        };
        write!(f, "{token}")
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GrantRecord {
    pub id: String,
    pub status: String,
    pub expires_at: Option<String>,
    pub allowed_tools: Vec<String>,
    pub allowed_risks: Vec<String>,
    pub workitem_id: Option<String>,
}

fn load_grant(store: &Store, grant_id: &str) -> Result<GrantRecord, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT id, status, expires_at, allowed_tools_json, allowed_risks_json, workitem_id
             FROM autonomy_grants WHERE id=?1",
            [grant_id],
            |r| {
                Ok(GrantRecord {
                    id: r.get(0)?,
                    status: r.get(1)?,
                    expires_at: r.get(2)?,
                    allowed_tools: serde_json::from_str(&r.get::<_, String>(3)?)
                        .unwrap_or_default(),
                    allowed_risks: serde_json::from_str(&r.get::<_, String>(4)?)
                        .unwrap_or_default(),
                    workitem_id: r.get(5)?,
                })
            },
        )
        .map_err(|_| Error::Message("autonomy_grant_required: grant 不存在".into()))
    })
}

/// Grant 有效性验证（§6.5）：未撤销/未过期/未耗尽/工具与风险在白名单内。
/// 限额计数（token/tool/cost）随 Run 消耗回写，M3 校验状态位；M6 Goal 扩展。
pub fn validate_grant(
    store: &Store,
    grant_id: &str,
    tool: &str,
    risk: &str,
    now: &str,
) -> Result<GrantRecord, GrantError> {
    let grant = load_grant(store, grant_id).map_err(|_| GrantError::NotFound)?;
    match grant.status.as_str() {
        "active" => {}
        "revoked" => return Err(GrantError::Revoked),
        "expired" => return Err(GrantError::Expired),
        "exhausted" => return Err(GrantError::Exhausted),
        _ => return Err(GrantError::NotFound),
    }
    if let Some(exp) = &grant.expires_at {
        if !exp.is_empty() && exp.as_str() <= now {
            return Err(GrantError::Expired);
        }
    }
    if !grant.allowed_tools.is_empty() && !grant.allowed_tools.iter().any(|t| t == tool) {
        return Err(GrantError::ToolNotAllowed);
    }
    if !grant.allowed_risks.is_empty() && !grant.allowed_risks.iter().any(|r| r == risk) {
        return Err(GrantError::RiskNotAllowed);
    }
    Ok(grant)
}

/// 从 policy_snapshot JSON 读自治模式（agent.start 装配时写入；缺省 Agent）。
pub fn mode_of_snapshot(policy_snapshot: &str) -> AutonomyMode {
    serde_json::from_str::<serde_json::Value>(policy_snapshot)
        .ok()
        .and_then(|v| {
            v.get("autonomyMode")
                .and_then(|m| m.as_str())
                .and_then(AutonomyMode::parse)
        })
        .unwrap_or(AutonomyMode::Agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-auton-{}-{}",
            std::process::id(),
            sg_store::ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Store::open(&dir, "test").unwrap()
    }

    fn seed_grant(
        store: &Store,
        id: &str,
        status: &str,
        expires_at: &str,
        tools: &[&str],
        risks: &[&str],
    ) {
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO autonomy_grants(id, status, expires_at, allowed_tools_json, allowed_risks_json, granted_at, created_at, updated_at)
                     VALUES (?1,?2,?3,?4,?5,'t','t','t')",
                    rusqlite::params![
                        id,
                        status,
                        expires_at,
                        serde_json::to_string(tools).unwrap(),
                        serde_json::to_string(risks).unwrap(),
                    ],
                )
                .map_err(Error::from)?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn grant_lifecycle_validation() {
        let store = setup();
        seed_grant(
            &store,
            "g-ok",
            "active",
            "2099-01-01",
            &["read_file", "apply_patch"],
            &["low", "high"],
        );
        // 有效。
        let g = validate_grant(&store, "g-ok", "apply_patch", "high", "2026-09-06").unwrap();
        assert_eq!(g.id, "g-ok");
        let err_of = |r: Result<GrantRecord, GrantError>| match r {
            Err(e) => e,
            Ok(_) => panic!("期望 Err"),
        };
        // 工具不在白名单。
        assert_eq!(
            err_of(validate_grant(
                &store,
                "g-ok",
                "run_command",
                "high",
                "2026-09-06"
            )),
            GrantError::ToolNotAllowed
        );
        // 风险不在白名单。
        assert_eq!(
            err_of(validate_grant(
                &store,
                "g-ok",
                "read_file",
                "medium",
                "2026-09-06"
            )),
            GrantError::RiskNotAllowed
        );
        // 过期（时间比较）。
        seed_grant(&store, "g-exp", "active", "2026-01-01", &[], &[]);
        assert_eq!(
            err_of(validate_grant(
                &store,
                "g-exp",
                "read_file",
                "low",
                "2026-09-06"
            )),
            GrantError::Expired
        );
        // 撤销。
        seed_grant(&store, "g-rev", "revoked", "2099-01-01", &[], &[]);
        assert_eq!(
            err_of(validate_grant(
                &store,
                "g-rev",
                "read_file",
                "low",
                "2026-09-06"
            )),
            GrantError::Revoked
        );
        // 耗尽。
        seed_grant(&store, "g-exh", "exhausted", "2099-01-01", &[], &[]);
        assert_eq!(
            err_of(validate_grant(
                &store,
                "g-exh",
                "read_file",
                "low",
                "2026-09-06"
            )),
            GrantError::Exhausted
        );
        // 不存在。
        assert_eq!(
            err_of(validate_grant(&store, "g-nope", "read_file", "low", "t")),
            GrantError::NotFound
        );
    }

    #[test]
    fn ask_mode_denies_write_and_unknown_effects() {
        use crate::plan_guard::EffectClass;
        assert!(validate_tool_for_mode(AutonomyMode::Ask, Some(EffectClass::Read)).is_ok());
        assert!(validate_tool_for_mode(AutonomyMode::Ask, Some(EffectClass::None)).is_ok());
        assert_eq!(
            validate_tool_for_mode(AutonomyMode::Ask, Some(EffectClass::LocalWrite)),
            Err("autonomy_mode_denied: Ask 模式拒绝写副作用工具")
        );
        assert_eq!(
            validate_tool_for_mode(AutonomyMode::Ask, Some(EffectClass::ExternalWrite)),
            Err("autonomy_mode_denied: Ask 模式拒绝写副作用工具")
        );
        assert!(
            validate_tool_for_mode(AutonomyMode::Ask, None).is_err(),
            "Ask 拒绝 unknown effect"
        );
        // Agent/Plan 不在此判定。
        assert!(validate_tool_for_mode(AutonomyMode::Agent, Some(EffectClass::LocalWrite)).is_ok());
    }

    #[test]
    fn mode_parse_and_snapshot_defaults() {
        assert_eq!(AutonomyMode::parse("ask"), Some(AutonomyMode::Ask));
        assert_eq!(AutonomyMode::parse("goal"), None, "Goal 属 M6");
        // 快照无 autonomyMode → Agent（legacy 默认）。
        assert_eq!(mode_of_snapshot("{}"), AutonomyMode::Agent);
        assert_eq!(
            mode_of_snapshot(r#"{"sandbox":{},"autonomyMode":"plan"}"#),
            AutonomyMode::Plan
        );
    }
}
