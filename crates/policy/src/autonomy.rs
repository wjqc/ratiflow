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
    let grant = refresh_expiry(
        store,
        load_grant(store, grant_id).map_err(|_| GrantError::NotFound)?,
        now,
    );
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
    // WP-1（RDWS v1.4）：ledger 耗尽闭合——任一限额维度的累计占用已达上限 →
    // 状态位写实为 exhausted（治理可见）并拒绝。逐消费行的零消耗校验在 reserve。
    // P2-2（评审修复）：与 reserve 同受 SIXGATES_UNIFIED_RISK 门控——flag=0 时
    // 完全恢复 WP-1 前行为（RDWS-003 探针：flag 关后 ledger 只读）。
    let exhausted_dim = if crate::risk_model::enabled() {
        store
            .with_conn(|conn| -> Result<Option<String>, Error> {
                let limits = limits_of(conn, grant_id)?;
                if let Some(map) = limits.as_object() {
                    for (dim, v) in map {
                        let Some(limit) = v.as_i64() else { continue };
                        if limit <= 0 {
                            continue;
                        }
                        if usage_total(conn, grant_id, dim)? >= limit {
                            return Ok(Some(dim.clone()));
                        }
                    }
                }
                Ok(None)
            })
            .unwrap_or(None)
    } else {
        None
    };
    if let Some(_dim) = exhausted_dim {
        let _ = store.with_conn(|conn| {
            conn.execute(
                "UPDATE autonomy_grants SET status='exhausted', updated_at=?1 WHERE id=?2 AND status='active'",
                rusqlite::params![sg_store::timefmt::now(), grant_id],
            )
            .map_err(Error::from)?;
            Ok(())
        });
        return Err(GrantError::Exhausted);
    }
    Ok(grant)
}

/// 派发闸专用的状态/时限校验（工具/风险白名单由 Goal 执行时逐动作校验）。
pub fn validate_grant_status(
    store: &Store,
    grant_id: &str,
    now: &str,
) -> Result<GrantRecord, GrantError> {
    let grant = refresh_expiry(
        store,
        load_grant(store, grant_id).map_err(|_| GrantError::NotFound)?,
        now,
    );
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
    Ok(grant)
}

/// 撤销（缺陷审计 2026-09-07 补齐产品化路径）：即时生效——逐动作校验都按行内
/// status 判定，撤销后下一次 validate 即 Revoked。对已 revoked 的重放幂等返回。
pub fn revoke_grant(store: &Store, grant_id: &str, reason: &str) -> Result<GrantRecord, Error> {
    let updated = store.with_conn(|conn| {
        conn.execute(
            "UPDATE autonomy_grants SET status='revoked', revoked_at=?1, revoked_reason=?2, updated_at=?1
             WHERE id=?3 AND status='active'",
            rusqlite::params![sg_store::timefmt::now(), reason, grant_id],
        )
        .map_err(Error::from)?;
        Ok(conn.changes())
    })?;
    if updated == 0 {
        let grant = load_grant(store, grant_id)?;
        if grant.status == "revoked" {
            return Ok(grant);
        }
        return Err(Error::Message(format!(
            "autonomy_grant_invalid: grant {grant_id} 状态 {} 不可撤销",
            grant.status
        )));
    }
    load_grant(store, grant_id)
}

/// 到期落库：active 且已过 expires_at → 状态位写实为 expired（治理可见），
/// 返回刷新后的记录；未到期/已终态原样返回。
fn refresh_expiry(store: &Store, grant: GrantRecord, now: &str) -> GrantRecord {
    let due = grant.status == "active"
        && grant
            .expires_at
            .as_ref()
            .is_some_and(|e| !e.is_empty() && e.as_str() <= now);
    if !due {
        return grant;
    }
    let _ = store.with_conn(|conn| {
        conn.execute(
            "UPDATE autonomy_grants SET status='expired', updated_at=?1 WHERE id=?2 AND status='active'",
            rusqlite::params![sg_store::timefmt::now(), grant.id],
        )
        .map_err(Error::from)?;
        Ok(())
    });
    load_grant(store, &grant.id).unwrap_or(grant)
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

/// 从 policy_snapshot JSON 读 grant id（WP-1：模型/工具消费记账的归属键）。
pub fn grant_id_of_snapshot(policy_snapshot: &str) -> String {
    serde_json::from_str::<serde_json::Value>(policy_snapshot)
        .ok()
        .and_then(|v| {
            v.get("autonomyGrantId")
                .and_then(|g| g.as_str())
                .map(String::from)
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Grant 计量（RDWS 实施计划 v1.4 WP-1 / 0042 grant_usage_ledger）
//
// consumption_key 粒度：模型每 model_call、工具每 proposal 各自独立行——
// 一个 Run 多次消费互不撞键；UNIQUE(grant,run,dimension,consumption_key) 是领域幂等键。
//
// 占用公式 = SUM(COALESCE(settled_amount, reserved_amount))；超限即
// autonomy_budget_exhausted（零消耗：单事务任一维超限全部不占用）。
// settle 只接受 actual ≤ reserved（CAS state='reserved'）；actual 超 reserve、
// Provider 未返回用量或价格未知 → reconciliation_required 并保留 reserved 占用，
// 禁止写 0 冒充。TTL/状态位不在此层——事实只追加，回填走 reconcile。
// ---------------------------------------------------------------------------

/// 一次消费的维度与数量（多维必须同事务原子 reserve）。
pub type ConsumptionDims<'a> = &'a [(&'a str, i64)];

fn limits_of(conn: &rusqlite::Connection, grant_id: &str) -> Result<serde_json::Value, Error> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT limits_json FROM autonomy_grants WHERE id=?1",
            [grant_id],
            |r| r.get(0),
        )
        .map_err(|_| Error::Message("autonomy_grant_required: grant 不存在".into()))?;
    Ok(serde_json::from_str(&raw.unwrap_or_default()).unwrap_or(serde_json::Value::Null))
}

fn usage_total(conn: &rusqlite::Connection, grant_id: &str, dimension: &str) -> Result<i64, Error> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(COALESCE(settled_amount, reserved_amount)),0)
         FROM grant_usage_ledger WHERE grant_id=?1 AND dimension=?2",
        [grant_id, dimension],
        |r| r.get(0),
    )?)
}

/// reserve：多维同一 BEGIN IMMEDIATE 事务内校验并写入。
/// 幂等：同 (grant,run,dimension,consumption_key) 已有 reserved/settled 行且数量一致 → Ok；
/// 数量不一致 → grant_ledger_conflict。任一维超限 → autonomy_budget_exhausted（零消耗）。
pub fn reserve(
    store: &Store,
    grant_id: &str,
    run_id: &str,
    consumption_key: &str,
    dims: ConsumptionDims,
    evidence: &serde_json::Value,
) -> Result<(), Error> {
    if dims.is_empty() {
        return Err(Error::Message("grant_ledger_invalid: 空 reserve".into()));
    }
    store.with_tx_immediate(|tx| {
        let limits = limits_of(tx, grant_id)?;
        let now = sg_store::timefmt::now();
        // 幂等复查：同消费行已存在——数量一致且状态为 reserved/settled → 直接成功
        // （网络/上游重放）；数量不一致或状态为对账/人工面 → 冲突（不静默改写事实）。
        let mut all_present = true;
        for (dim, amount) in dims {
            let row: Option<(i64, i64)> = tx
                .query_row(
                    "SELECT reserved_amount, CASE WHEN state IN ('reserved','settled')
                              THEN 0 ELSE 1 END FROM grant_usage_ledger
                     WHERE grant_id=?1 AND run_id=?2 AND dimension=?3 AND consumption_key=?4",
                    rusqlite::params![grant_id, run_id, dim, consumption_key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })
                .map_err(Error::from)?;
            match row {
                Some((reserved, bad_state)) => {
                    if bad_state == 1 {
                        return Err(Error::Message(format!(
                            "grant_ledger_conflict: {dim}/{consumption_key} 处于对账/人工状态"
                        )));
                    }
                    if reserved != *amount {
                        return Err(Error::Message(format!(
                            "grant_ledger_conflict: {dim}/{consumption_key} 已保留 {reserved}，重放量 {amount}"
                        )));
                    }
                }
                None => all_present = false,
            }
        }
        if all_present {
            return Ok(());
        }
        for (dim, amount) in dims {
            if *amount < 0 {
                return Err(Error::Message(format!(
                    "grant_ledger_invalid: {dim} 保留量不可为负"
                )));
            }
            // limits_json 键与 ledger 维度同名；缺省/≤0 = 该维不限。
            let limit = limits.get(*dim).and_then(|v| v.as_i64()).unwrap_or(0);
            if limit > 0 {
                let total = usage_total(tx, grant_id, dim)?;
                if total + amount > limit {
                    return Err(Error::Message(format!(
                        "autonomy_budget_exhausted: {dim} 占用 {total} + 本次 {amount} 超限额 {limit}（零消耗）"
                    )));
                }
            }
            tx.execute(
                "INSERT INTO grant_usage_ledger(id, grant_id, run_id, dimension, consumption_key,
                     reserved_amount, reservation_evidence_json, state, reserved_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,'reserved',?8)",
                rusqlite::params![
                    sg_store::ids::new_id("gul"),
                    grant_id,
                    run_id,
                    dim,
                    consumption_key,
                    amount,
                    evidence.to_string(),
                    now
                ],
            )?;
        }
        Ok(())
    })
}

/// settle：消费结束落实际用量（CAS state='reserved'）。
/// actual=None（usage 未知）或 actual>reserved → reconciliation_required（保留 reserved 占用）。
/// 幂等：已 settled 同值 → Ok；不一致 → grant_ledger_conflict。
pub fn settle(
    store: &Store,
    grant_id: &str,
    run_id: &str,
    consumption_key: &str,
    actuals: &[(&str, Option<i64>)],
    evidence_json: &str,
) -> Result<(), Error> {
    store.with_tx_immediate(|tx| {
        let now = sg_store::timefmt::now();
        for (dim, actual) in actuals {
            let row: Option<(i64, String, Option<i64>)> = tx
                .query_row(
                    "SELECT reserved_amount, state, settled_amount FROM grant_usage_ledger
                     WHERE grant_id=?1 AND run_id=?2 AND dimension=?3 AND consumption_key=?4",
                    rusqlite::params![grant_id, run_id, dim, consumption_key],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })
                .map_err(Error::from)?;
            let Some((reserved, state, settled)) = row else {
                // 未 reserve 先 settle：无法对账（保留占用事实缺失），记告警语义错误。
                return Err(Error::Message(format!(
                    "grant_ledger_conflict: {dim}/{consumption_key} 无 reserved 行"
                )));
            };
            match (&state[..], actual) {
                ("settled", Some(a)) if settled == Some(*a) => continue, // 幂等重放
                ("settled", _) => {
                    return Err(Error::Message(format!(
                        "grant_ledger_conflict: {dim}/{consumption_key} 已 settled 为 {settled:?}"
                    )))
                }
                _ => {}
            }
            let (new_state, settled_val) = match actual {
                Some(a) if *a >= 0 && *a <= reserved => ("settled", Some(*a)),
                // 超 reserve 或用量未知：不写 0 冒充，进对账并保留 reserved 占用。
                _ => ("reconciliation_required", None),
            };
            let n = tx.execute(
                "UPDATE grant_usage_ledger SET state=?5, settled_amount=?6, settled_at=?7,
                        settlement_evidence_json=?8
                 WHERE grant_id=?1 AND run_id=?2 AND dimension=?3 AND consumption_key=?4
                   AND state='reserved'",
                rusqlite::params![
                    grant_id,
                    run_id,
                    dim,
                    consumption_key,
                    new_state,
                    settled_val,
                    now,
                    evidence_json
                ],
            )?;
            if n != 1 {
                return Err(Error::Message(format!(
                    "grant_ledger_conflict: {dim}/{consumption_key} CAS 未命中（并发 settle）"
                )));
            }
        }
        Ok(())
    })
}

/// 崩溃恢复（§1.4）：run 已终态但仍有 reserved 行 → 按权威事实回填：
/// - consumption_key 命中 model_calls 行 → 按 tokens_in/out 实际 settle；
/// - 命中 tool_proposals 且 decision=executed → settle 1；
/// - 无法对账 → reconciliation_required + notification_outbox 告警（人工处置后
///   置 manual_action_required）。
///
/// 返回处理行数。幂等可重入。
pub fn reconcile_run_residues(store: &Store, run_id: &str) -> Result<i64, Error> {
    let rows: Vec<(String, String, String, i64)> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT l.dimension, l.consumption_key, l.id, l.reserved_amount
             FROM grant_usage_ledger l
             JOIN agent_runs r ON r.id = l.run_id
             WHERE l.run_id=?1 AND l.state='reserved'
               AND r.status IN ('completed_execution','failed','cancelled')",
        )?;
        let out = stmt
            .query_map([run_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(out)
    })?;
    let now = sg_store::timefmt::now();
    let mut handled = 0i64;
    for (dimension, key, row_id, reserved) in rows {
        // model_calls 权威行：tokens 维度按实际用量 settle；model_calls 维度 settle 1。
        let model_row: Option<(i64, i64, String)> = store.with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT tokens_in, tokens_out, status FROM model_calls WHERE id=?1",
                    [&key],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .ok())
        })?;
        let tool_row: Option<String> = store.with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT decision FROM tool_proposals WHERE id=?1",
                    [&key],
                    |r| r.get(0),
                )
                .ok())
        })?;
        let actual = match (dimension.as_str(), model_row, tool_row) {
            ("model_calls", Some((_, _, status)), _) if status == "ok" => Some(1),
            ("tokens_in", Some((tin, _, status)), _) if status == "ok" => Some(tin),
            ("tokens_out", Some((_, tout, status)), _) if status == "ok" => Some(tout),
            ("tool_calls", _, Some(decision)) if decision == "executed" => Some(1),
            _ => None,
        };
        let (state, settled) = match actual {
            Some(a) if a >= 0 && a <= reserved => ("settled", Some(a)),
            _ => ("reconciliation_required", None),
        };
        store.with_conn(|conn| {
            conn.execute(
                "UPDATE grant_usage_ledger SET state=?2, settled_amount=?3, settled_at=?4,
                        settlement_evidence_json=?5
                 WHERE id=?1 AND state='reserved'",
                rusqlite::params![
                    row_id,
                    state,
                    settled,
                    now,
                    serde_json::json!({"source": "startup_reconcile", "run": run_id}).to_string()
                ],
            )?;
            if state == "reconciliation_required" {
                sg_store::outbox::emit_at(
                    conn,
                    "grant_usage",
                    &row_id,
                    "grant.reconciliation_required",
                    serde_json::json!({
                        "runId": run_id, "dimension": dimension, "consumptionKey": key,
                        "reserved": reserved,
                    }),
                )?;
            }
            Ok(())
        })?;
        handled += 1;
    }
    Ok(handled)
}

/// 启动恢复扫描（挂 main.rs，automation 先例）：全部终态 run 的 reserved 残留。
pub fn reconcile_all_terminal_runs(store: &Store) -> Result<i64, Error> {
    if !crate::risk_model::enabled() {
        // P2-2：flag 关后 ledger 只读（保留未对账占用，不改写行）。
        return Ok(0);
    }
    let run_ids: Vec<String> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT l.run_id FROM grant_usage_ledger l
             JOIN agent_runs r ON r.id = l.run_id
             WHERE l.state='reserved'
               AND r.status IN ('completed_execution','failed','cancelled')",
        )?;
        let out = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(out)
    })?;
    let mut total = 0i64;
    for run_id in run_ids {
        total += reconcile_run_residues(store, &run_id)?;
    }
    Ok(total)
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

    #[test]
    fn revoke_is_immediate_idempotent_and_expiry_persists() {
        let store = setup();
        seed_grant(&store, "g1", "active", "", &["run_command"], &["high"]);
        validate_grant(
            &store,
            "g1",
            "run_command",
            "high",
            "2026-01-01T00:00:00.000Z",
        )
        .unwrap();
        let g = revoke_grant(&store, "g1", "人工撤销").unwrap();
        assert_eq!(g.status, "revoked");
        // 撤销即时生效：下一次逐动作校验即 Revoked。
        assert!(matches!(
            validate_grant(
                &store,
                "g1",
                "run_command",
                "high",
                "2026-01-01T00:00:00.000Z"
            ),
            Err(GrantError::Revoked)
        ));
        // 重放撤销幂等。
        assert_eq!(
            revoke_grant(&store, "g1", "again").unwrap().status,
            "revoked"
        );
        // 非 active 且非 revoked：不可撤销。
        seed_grant(&store, "g3", "exhausted", "", &[], &[]);
        assert!(revoke_grant(&store, "g3", "x").is_err());

        // 到期落库：validate 报 Expired 且行内状态位写实为 expired（治理可见）。
        seed_grant(&store, "g2", "active", "2026-01-01T00:00:00.000Z", &[], &[]);
        assert!(matches!(
            validate_grant_status(&store, "g2", "2026-09-07T00:00:00.000Z"),
            Err(GrantError::Expired)
        ));
        let status: String = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT status FROM autonomy_grants WHERE id='g2'",
                    [],
                    |r| r.get(0),
                )
                .map_err(Error::from)
            })
            .unwrap();
        assert_eq!(status, "expired");
    }
}

// ---------------- M6-03（EvoFlow 方案 §6.5 / ADR-037）：Goal 自动放行谓词 ----------------
// Goal v2 七条硬条件的可判定子集（M6 交付面）：grant 未撤销未过期 + allow_gate_release
// + 无 pending 审批 + 最新计划无 unknown/manual attempt。其余（release digest 一致、
// trace coverage/evidence/requirements 满足）由放行链既有校验承接；
// SIXGATES_AUTO_GATE_RELEASE 默认 0——谓词可用但消费侧关闭（EV-021）。

pub const AUTO_GATE_RELEASE_FLAG: &str = "SIXGATES_AUTO_GATE_RELEASE";

pub fn auto_gate_release_enabled() -> bool {
    std::env::var(AUTO_GATE_RELEASE_FLAG).ok().as_deref() == Some("1")
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AutoReleaseCheck {
    pub allowed: bool,
    pub reasons: Vec<String>,
}

fn grant_by_id(store: &Store, grant_id: &str) -> Result<(String, Option<String>, i64), Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT status, COALESCE(expires_at,''), allow_gate_release
             FROM autonomy_grants WHERE id=?1",
            [grant_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|_| Error::Message("autonomy_grant_required: grant 不存在".into()))
    })
}

/// Goal v2 自动放行谓词（返回结构化原因，任一不满足即 paused，绝不假批准）。
pub fn automatic_release_check(
    store: &Store,
    workitem_id: &str,
    grant_id: &str,
    now: &str,
) -> Result<AutoReleaseCheck, Error> {
    let mut reasons = Vec::new();
    // 1) flag。
    if !auto_gate_release_enabled() {
        reasons.push("auto_gate_release_disabled".into());
    }
    // 2) grant 状态/时限/授权位。
    match grant_by_id(store, grant_id) {
        Ok((status, expires_at, allow_release)) => {
            if status != "active" {
                reasons.push(format!("grant_{status}"));
            }
            let expired = expires_at
                .as_ref()
                .is_some_and(|e| !e.is_empty() && e.as_str() <= now);
            if expired {
                reasons.push("grant_expired".into());
            }
            if allow_release == 0 {
                reasons.push("grant_allow_gate_release_off".into());
            }
        }
        Err(e) => reasons.push(e.to_string()),
    }
    // 3) 无 pending 审批。
    let pending: i64 = store.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM approvals WHERE workitem_id=?1 AND status='requested'",
            [workitem_id],
            |r| r.get(0),
        )
        .map_err(Error::from)
    })?;
    if pending > 0 {
        reasons.push(format!("pending_approvals:{pending}"));
    }
    // 4) 最新计划无 unknown/manual attempt。
    let bad_attempts: i64 = store.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM plan_task_attempts pa
             JOIN plan_tasks pt ON pt.id = pa.task_id
             JOIN plan_revisions pr ON pr.id = pt.plan_revision_id
             WHERE pr.workitem_id=?1
               AND pa.state IN ('unknown','manual_action_required','reconciliation_required')",
            [workitem_id],
            |r| r.get(0),
        )
        .map_err(Error::from)
    })?;
    if bad_attempts > 0 {
        reasons.push(format!("unknown_or_manual_attempts:{bad_attempts}"));
    }
    Ok(AutoReleaseCheck {
        allowed: reasons.is_empty(),
        reasons,
    })
}

#[cfg(test)]
mod goal_tests {
    use super::*;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-goal-{}-{}",
            std::process::id(),
            sg_store::ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        store
            .with_conn(|c| {
                c.execute_batch(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj','u','n','p','main','t');
                    INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                     VALUES ('wi','pj','t','','[]','requirements','t','t');",
                )
                .map_err(Error::from)?;
                Ok(())
            })
            .unwrap();
        store
    }

    /// Goal v2 谓词：flag 关/无授权位/撤销/过期/unknown 任一即拒绝（EV-021）。
    #[test]
    fn automatic_release_predicate_gates() {
        let store = setup();
        let mk = |status: &str, expires: &str, allow: i64| {
            store
                .with_conn(|c| {
                    c.execute(
                        "INSERT INTO autonomy_grants(id, status, expires_at, allow_gate_release, granted_at, created_at, updated_at)
                         VALUES (?1,?2,?3,?4,'t','t','t')",
                        rusqlite::params![format!("g_{status}_{expires}_{allow}"), status, expires, allow],
                    )
                    .map_err(Error::from)?;
                    Ok(())
                })
                .unwrap();
        };
        mk("active", "2099-01-01", 1);
        mk("revoked", "2099-01-01", 1);
        mk("active", "2020-01-01", 1);
        mk("active", "2099-01-01", 0);
        // flag 关闭（默认）→ 一律拒绝。
        let r =
            automatic_release_check(&store, "wi", "g_active_2099-01-01_1", "2026-09-06").unwrap();
        assert!(
            !r.allowed
                && r.reasons
                    .contains(&"auto_gate_release_disabled".to_string())
        );
        // flag 开 → 该 grant 通过（无 pending/unknown）。
        std::env::set_var(AUTO_GATE_RELEASE_FLAG, "1");
        let r =
            automatic_release_check(&store, "wi", "g_active_2099-01-01_1", "2026-09-06").unwrap();
        assert!(r.allowed, "{:?}", r.reasons);
        // 撤销/过期/未授权位 → 各自拒绝。
        for (gid, reason) in [
            ("g_revoked_2099-01-01_1", "grant_revoked"),
            ("g_active_2020-01-01_1", "grant_expired"),
            ("g_active_2099-01-01_0", "grant_allow_gate_release_off"),
        ] {
            let r = automatic_release_check(&store, "wi", gid, "2026-09-06").unwrap();
            assert!(
                !r.allowed && r.reasons.iter().any(|x| x.contains(reason)),
                "{gid}: {:?}",
                r.reasons
            );
        }
        std::env::remove_var(AUTO_GATE_RELEASE_FLAG);
    }

    // --- WP-1 Grant 计量（RDWS v1.4 / 0042 grant_usage_ledger）---
    mod ledger_tests {
        use super::*;

        fn setup() -> Store {
            let dir = std::env::temp_dir().join(format!(
                "sg-ledger-{}-{}",
                std::process::id(),
                sg_store::ids::new_id("t")
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Store::open(&dir, "test").unwrap()
        }

        fn seed(store: &Store, limits: &serde_json::Value) -> (String, String) {
            store
                .with_conn(|c| {
                    c.execute_batch(
                        "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                         VALUES ('pj','u','n','p','main','t');
                         INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                         VALUES ('wi','pj','t','','[]','requirements','t','t');
                         INSERT INTO context_manifests(id, workitem_id, scope, data_policy, created_at)
                         VALUES ('ctx1','wi','{}','standard','t');
                         INSERT INTO agent_runs(id, workitem_id, task_id, goal, input_baseline_sha, context_manifest_id,
                             tool_allowlist, budget, policy_snapshot, idempotency_key, status, created_at, updated_at)
                         VALUES ('run1','wi','','g','sha','ctx1','[]','{}','default','ik','queued','t','t');",
                    )
                    .map_err(Error::from)?;
                    Ok(())
                })
                .unwrap();
            store
                .with_conn(|c| {
                    c.execute(
                        "INSERT INTO autonomy_grants(id, workitem_id, limits_json, status, granted_at, expires_at, created_at, updated_at)
                         VALUES ('g1','wi',?1,'active','t','','t','t')",
                        [limits.to_string()],
                    )
                    .map_err(Error::from)?;
                    Ok(())
                })
                .unwrap();
            ("g1".into(), "run1".into())
        }

        fn row_state(store: &Store, dim: &str, key: &str) -> (String, Option<i64>, i64) {
            store
                .with_conn(|c| {
                    Ok(c.query_row(
                        "SELECT state, settled_amount, reserved_amount FROM grant_usage_ledger
                         WHERE dimension=?1 AND consumption_key=?2",
                        [dim, key],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .unwrap())
                })
                .unwrap()
        }

        #[test]
        fn multi_dimension_atomic_reserve_and_limit_zero_consumption() {
            let store = setup();
            let _ = seed(
                &store,
                &serde_json::json!({"model_calls": 2, "tokens_in": 100}),
            );
            let (g, r) = ("g1", "run1");
            // 多维原子 reserve：model_calls=1 + tokens_in=80。
            reserve(
                &store,
                g,
                r,
                "mc1",
                &[("model_calls", 1), ("tokens_in", 80)],
                &serde_json::json!({"estimator": "utf8_bytes_upper_v1"}),
            )
            .unwrap();
            // 第二次消费：model_calls 到 2（达限不超），tokens_in 80+30 超 100 → 整体拒绝（零消耗）。
            let err = reserve(
                &store,
                g,
                r,
                "mc2",
                &[("model_calls", 1), ("tokens_in", 30)],
                &serde_json::json!({}),
            )
            .unwrap_err();
            assert!(
                err.to_string().contains("autonomy_budget_exhausted"),
                "{err}"
            );
            let count: i64 = store
                .with_conn(|c| {
                    Ok(c.query_row(
                        "SELECT COUNT(*) FROM grant_usage_ledger WHERE consumption_key='mc2'",
                        [],
                        |x| x.get(0),
                    )
                    .unwrap())
                })
                .unwrap();
            assert_eq!(count, 0, "超限零消耗：mc2 任何维度都不落行");
            // tokens_in 单独达限内仍可（换 key）。
            reserve(
                &store,
                g,
                r,
                "mc2",
                &[("model_calls", 1), ("tokens_in", 20)],
                &serde_json::json!({}),
            )
            .unwrap();
        }

        #[test]
        fn reserve_idempotent_replay_and_conflict() {
            let store = setup();
            let _ = seed(&store, &serde_json::json!({"tokens_in": 1000}));
            reserve(
                &store,
                "g1",
                "run1",
                "mc1",
                &[("tokens_in", 50)],
                &serde_json::json!({}),
            )
            .unwrap();
            // 同 key 同量重放 → Ok，不重复占行。
            reserve(
                &store,
                "g1",
                "run1",
                "mc1",
                &[("tokens_in", 50)],
                &serde_json::json!({}),
            )
            .unwrap();
            let n: i64 = store
                .with_conn(|c| {
                    Ok(c.query_row(
                        "SELECT COUNT(*) FROM grant_usage_ledger WHERE consumption_key='mc1'",
                        [],
                        |x| x.get(0),
                    )
                    .unwrap())
                })
                .unwrap();
            assert_eq!(n, 1, "重放幂等不追加行");
            // 同 key 异量 → 冲突。
            let err = reserve(
                &store,
                "g1",
                "run1",
                "mc1",
                &[("tokens_in", 60)],
                &serde_json::json!({}),
            )
            .unwrap_err();
            assert!(err.to_string().contains("grant_ledger_conflict"), "{err}");
        }

        #[test]
        fn settle_cas_actual_over_or_unknown_to_reconciliation() {
            let store = setup();
            let _ = seed(
                &store,
                &serde_json::json!({"tokens_in": 1000, "tokens_out": 500}),
            );
            reserve(
                &store,
                "g1",
                "run1",
                "mc1",
                &[("tokens_in", 80), ("tokens_out", 90)],
                &serde_json::json!({}),
            )
            .unwrap();
            // 正常 settle：actual ≤ reserved。
            settle(
                &store,
                "g1",
                "run1",
                "mc1",
                &[("tokens_in", Some(70)), ("tokens_out", Some(90))],
                "{}",
            )
            .unwrap();
            let (s1, v1, _) = row_state(&store, "tokens_in", "mc1");
            assert_eq!((s1.as_str(), v1), ("settled", Some(70)));
            // settled 幂等重放（同值）。
            settle(
                &store,
                "g1",
                "run1",
                "mc1",
                &[("tokens_in", Some(70))],
                "{}",
            )
            .unwrap();
            // settled 异值 → 冲突。
            let err = settle(
                &store,
                "g1",
                "run1",
                "mc1",
                &[("tokens_in", Some(60))],
                "{}",
            )
            .unwrap_err();
            assert!(err.to_string().contains("grant_ledger_conflict"), "{err}");
            // usage 未知（None）→ reconciliation_required，不写 0。
            reserve(
                &store,
                "g1",
                "run1",
                "mc2",
                &[("tokens_out", 50)],
                &serde_json::json!({}),
            )
            .unwrap();
            settle(&store, "g1", "run1", "mc2", &[("tokens_out", None)], "{}").unwrap();
            let (s2, v2, reserved) = row_state(&store, "tokens_out", "mc2");
            assert_eq!(
                (s2.as_str(), v2),
                ("reconciliation_required", None),
                "不写 0 冒充"
            );
            assert_eq!(reserved, 50, "保留 reserved 占用");
            // actual 超 reserve → 同样 reconciliation。
            reserve(
                &store,
                "g1",
                "run1",
                "mc3",
                &[("tokens_in", 10)],
                &serde_json::json!({}),
            )
            .unwrap();
            settle(
                &store,
                "g1",
                "run1",
                "mc3",
                &[("tokens_in", Some(99))],
                "{}",
            )
            .unwrap();
            assert_eq!(
                row_state(&store, "tokens_in", "mc3").0,
                "reconciliation_required"
            );
            // 未 reserve 先 settle → 冲突。
            let err = settle(
                &store,
                "g1",
                "run1",
                "ghost",
                &[("tokens_in", Some(1))],
                "{}",
            )
            .unwrap_err();
            assert!(err.to_string().contains("grant_ledger_conflict"), "{err}");
        }

        #[test]
        fn concurrent_reserves_respect_limit() {
            let store = std::sync::Arc::new(setup());
            let _ = seed(&store, &serde_json::json!({"tool_calls": 20}));
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
            let mut handles = Vec::new();
            for i in 0..8 {
                let store = store.clone();
                let barrier = barrier.clone();
                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    reserve(
                        &store,
                        "g1",
                        "run1",
                        &format!("tp{i}"),
                        &[("tool_calls", 3)],
                        &serde_json::json!({}),
                    )
                }));
            }
            let outcomes: Vec<bool> = handles
                .into_iter()
                .map(|h| h.join().unwrap().is_ok())
                .collect();
            let ok = outcomes.iter().filter(|o| **o).count();
            assert!(ok >= 6, "8×3=24 对限额 20：至少 6 个成功（实际 {ok}）");
            let total: i64 = store.with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COALESCE(SUM(COALESCE(settled_amount,reserved_amount)),0) FROM grant_usage_ledger WHERE dimension='tool_calls'",
                    [], |x| x.get(0)).unwrap())
            }).unwrap();
            assert!(total <= 20, "并发不超 Grant（实际 {total}）");
        }

        #[test]
        fn reconcile_terminal_run_from_authoritative_rows() {
            let store = setup();
            let _ = seed(
                &store,
                &serde_json::json!({"model_calls": 10, "tokens_in": 1000, "tool_calls": 10}),
            );
            // mc1：崩溃残留（reserved）+ model_calls 权威行 ok → 按 tokens settle。
            reserve(
                &store,
                "g1",
                "run1",
                "mc1",
                &[("model_calls", 1), ("tokens_in", 80)],
                &serde_json::json!({}),
            )
            .unwrap();
            store.with_conn(|c| {
                c.execute(
                    "INSERT INTO model_calls(id, agent_run_id, provider, model, tokens_in, tokens_out, cost_micros, latency_ms, redactions, status, created_at)
                     VALUES ('mc1','run1','p','default',70,10,0,1,0,'ok','t')",
                    [],
                ).map_err(Error::from)?;
                Ok(())
            }).unwrap();
            // tp1：工具残留 + proposal executed → settle 1。
            reserve(
                &store,
                "g1",
                "run1",
                "tp1",
                &[("tool_calls", 1)],
                &serde_json::json!({}),
            )
            .unwrap();
            store.with_conn(|c| {
                c.execute(
                    "INSERT INTO tool_proposals(id, agent_run_id, tool, arguments, risk, action_digest, requires_approval, decision, created_at)
                     VALUES ('tp1','run1','read_file','{}','low','d',0,'executed','t')",
                    [],
                ).map_err(Error::from)?;
                Ok(())
            }).unwrap();
            // mc2：无权威行（调用中断）→ reconciliation_required + 告警。
            reserve(
                &store,
                "g1",
                "run1",
                "mc2",
                &[("tokens_in", 50)],
                &serde_json::json!({}),
            )
            .unwrap();
            // run 未终态 → 不动。
            assert_eq!(reconcile_run_residues(&store, "run1").unwrap(), 0);
            store
                .with_conn(|c| {
                    c.execute(
                        "UPDATE agent_runs SET status='completed_execution' WHERE id='run1'",
                        [],
                    )
                    .map_err(Error::from)?;
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                reconcile_run_residues(&store, "run1").unwrap(),
                4,
                "mc1×2 维度 + tp1 + mc2"
            );
            let (s, v, _) = row_state(&store, "tokens_in", "mc1");
            assert_eq!(
                (s.as_str(), v),
                ("settled", Some(70)),
                "按权威行实际用量回填"
            );
            assert_eq!(row_state(&store, "model_calls", "mc1").1, Some(1));
            assert_eq!(row_state(&store, "tool_calls", "tp1").1, Some(1));
            assert_eq!(
                row_state(&store, "tokens_in", "mc2").0,
                "reconciliation_required"
            );
            let alerts: i64 = store.with_conn(|c| {
                Ok(c.query_row("SELECT COUNT(*) FROM events_outbox WHERE type='grant.reconciliation_required'", [], |x| x.get(0)).unwrap())
            }).unwrap();
            assert_eq!(alerts, 1, "对账告警恰好一条");
            // 幂等重入。
            assert_eq!(reconcile_run_residues(&store, "run1").unwrap(), 0);
        }
    }
}
