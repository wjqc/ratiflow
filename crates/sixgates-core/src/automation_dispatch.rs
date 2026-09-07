//! 自动化调度编排（EvoFlow 方案 M6-04 / ADR-039 §6.13）：
//! fire_due（到期扫描 → overlap 闸 → receipt 认领 → misfire 决策 → grant 检查 →
//! 意图落账 + 重调度）。只产 RunIntent：真正的 Run 创建走消费侧（Goal 执行器，
//! 复用既有 agent/plan 装配与审批链），无有效 grant 永远只落 blocked_no_grant。
//! main.rs 定时器（SIXGATES_AUTOMATIONS=1）周期调用 fire_due；启动时先跑
//! reconcile_orphans（重复启动不重复执行）。

use serde_json::{json, Value};
use sg_store::{outbox, Error, Store};

/// 消费一个到期 automation。返回 (status, note)。
pub fn fire_one(
    store: &Store,
    automation_id: &str,
    scheduled_for: &str,
) -> Result<(String, String), Error> {
    let a = store.with_conn(|conn| {
        conn.query_row(
            "SELECT intent_json, COALESCE(autonomy_grant_id,''), overlap_policy, misfire_policy
             FROM automations WHERE id=?1",
            [automation_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .map_err(Error::from)
    })?;
    let (intent_json, grant_id, overlap_policy, _misfire_policy) = a;
    // receipt 认领先行（幂等：重复启动/时钟跳变）——本 scheduled_for 已被处理登记，
    // 后续 overlap/misfire 决策都在"已认领"前提下进行（评审 P1 修复：原先 overlap
    // 分支先 record_intent 后认领，UPDATE 不到行却返回成功）。
    let Some(_receipt) =
        sg_workflow::automation::claim_receipt(store, automation_id, scheduled_for)?
    else {
        return Ok(("deduped".into(), "receipt exists".into()));
    };
    // overlap 闸（fired 未推进即视为上一轮在途）；排除本次认领的 receipt 行。
    if !sg_workflow::automation::overlap_allowed_for(
        store,
        automation_id,
        &overlap_policy,
        Some(scheduled_for),
    )? {
        sg_workflow::automation::record_intent(
            store,
            automation_id,
            scheduled_for,
            "skipped_overlap",
            None,
            "上一触发未推进（overlap skip）",
        )?;
        sg_workflow::automation::reschedule(store, automation_id, &sg_store::timefmt::now())?;
        return Ok(("skipped_overlap".into(), "overlap".into()));
    }
    // misfire 决策（skip 策略下越过整周期才算过期——毫秒级抖动不放弃）。
    let now = sg_store::timefmt::now();
    let probe = store.with_conn(|conn| {
        conn.query_row(
            "SELECT misfire_policy, next_fire_at, interval_secs FROM automations WHERE id=?1",
            [automation_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )
        .map_err(Error::from)
    })?;
    let decision = sg_workflow::automation::misfire_decision(&probe.0, &probe.1, &now, probe.2)?;
    if !decision {
        sg_workflow::automation::record_intent(
            store,
            automation_id,
            scheduled_for,
            "skipped_misfire",
            None,
            "misfire skip 策略放弃过期触发",
        )?;
        sg_workflow::automation::reschedule(store, automation_id, &now)?;
        return Ok(("skipped_misfire".into(), "misfire".into()));
    }
    // Grant 检查（无有效 grant → 永远只落 blocked，不产生任何副作用）。
    let intent: Value = serde_json::from_str(&intent_json).unwrap_or_else(|_| json!({}));
    let workitem_id = intent
        .get("workItemId")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if grant_id.is_empty() {
        sg_workflow::automation::record_intent(
            store,
            automation_id,
            scheduled_for,
            "blocked_no_grant",
            None,
            "无 AutonomyGrant：仅记录意图，不执行（M6 退出标准）",
        )?;
        sg_workflow::automation::reschedule(store, automation_id, &now)?;
        return Ok(("blocked_no_grant".into(), "no grant".into()));
    }
    // Grant 有效性（状态/时限/工具白名单；Goal 分派用中性工具名/低风险——
    // 白名单空 = 不限制；非空白名单不含中性名会被逐动作校验兜住）。
    if let Err(e) = sg_policy::autonomy::validate_grant_status(store, &grant_id, &now) {
        sg_workflow::automation::record_intent(
            store,
            automation_id,
            scheduled_for,
            "blocked_no_grant",
            None,
            &format!("grant 验证失败：{e}"),
        )?;
        sg_workflow::automation::reschedule(store, automation_id, &now)?;
        return Ok(("blocked_no_grant".into(), e.to_string()));
    }
    // 意图创建（Run 创建由 Goal 执行器消费 intent；此处只落账+事件）。
    let run_intent_id = sg_store::ids::new_id("rint");
    sg_workflow::automation::record_intent(
        store,
        automation_id,
        scheduled_for,
        "intent_created",
        Some(&run_intent_id),
        "RunIntent 已创建（消费侧经既有装配/审批链）",
    )?;
    sg_workflow::automation::reschedule(store, automation_id, &now)?;
    let workitem = if workitem_id.is_empty() {
        None
    } else {
        Some(workitem_id.as_str())
    };
    outbox::emit(
        store,
        "automation",
        automation_id,
        "automation.triggered",
        json!({
            "automationId": automation_id,
            "scheduledFor": scheduled_for,
            "runIntentId": run_intent_id,
            "workItemId": workitem,
            "intent": intent,
        }),
    )?;
    Ok(("intent_created".into(), run_intent_id))
}

/// 到期批量消费（timer tick）。
pub fn fire_due(store: &Store) -> Result<Vec<(String, String, String)>, Error> {
    let now = sg_store::timefmt::now();
    let due = sg_workflow::automation::due(store, &now)?;
    let mut out = Vec::new();
    for a in due {
        let (status, note) = fire_one(store, &a.id, &a.next_fire_at)?;
        out.push((a.id.clone(), status, note));
    }
    Ok(out)
}
