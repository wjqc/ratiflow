//! fast-track 快通道（EvoFlow WP-8；策略 schema 见 sg-workflow template）。
//!
//! P0-2（审计《Ratiflow_RDWS_v1.4实施审计与整改方案_v1.0》§7）：六因素全部由
//! 服务端从权威事实派生（`derive_fast_track_facts`），客户端 factors 已废弃——
//! RPC 只收 workItemId/gate/expectedStateDigest/idempotencyKey，携带 factors 即拒。
//! 数据源映射（v1.4 §WP-8 权威数据源表；任一缺失/漂移 = false，不得默认 true）：
//! - `no_protected_path` / `effect_class_read_only` / `reversibility_confirmed`：
//!   当前 attempt 内 tool_proposals × 工具注册表（effect_class/reversibility/
//!   protected_target；未知工具一律 false）；无提案 = 无执行事实 = false。
//! - `api_schema_unchanged`：requirement 修订集（schema 契约面）自 attempt
//!   建立后无新增修订；无任何修订 = 无法建立基线 = false。
//! - `test_evidence_present`：当前 attempt 内（晚于 attempt 建立）的 verified
//!   test_report 证据。
//! - `provenance_complete`：`sg_provenance::impact::for_workitem` completeness
//!   == complete（空图/超限均不满足）。
//!
//! 输入状态摘要（input_state_digest）冻结上述全部证据引用 + 模板版本 + 策略
//! 版本 + attempt 输入包摘要：任何漂移（删证据/改 schema/provenance 变化/
//! policy version 变化）→ 重评估得到不同摘要 → 旧未决建议由
//! `shadow::expire_stale` 过期（expired 决定，不改建议行）。
//!
//! 流程不变：全真 → 建议落 shadow（source=fast_track，**不自动执行**）→
//! 人工经 automation.decideSuggestion 决定；accepted 后缩减在域内生效——
//! - 活动缩减：活跃 attempt 中 skippable_activities 的 pending 活动标记 skipped；
//! - 交付物豁免：waived_deliverables 以替代证据（verified 证据 kind 匹配）替代，
//!   替代证据缺失照实不满足（deliverable 门禁 fail-closed）；
//! - reduced_approval 仅声明可缩减关卡层人工复核，Tool 风险/Policy 强制审批
//!   永不在豁免面（策略 schema 无此声明位）。

use serde_json::json;
use sg_store::{ids, outbox, timefmt, Error, Store};
use sg_workflow::shadow::Suggestion;
use sg_workflow::template::{FastTrackFactors, GateDefinition, WaivedDeliverable};

/// 建议有效期（24h；到期由 expire_stale 收敛，未决建议不再可采纳）。
const SUGGESTION_TTL_MINUTES: i64 = 24 * 60;
/// 注册表 reversibility 词汇中视为"已确认可恢复"的取值。
const REVERSIBLE_VALUES: [&str; 2] = ["reversible", "logical_restore"];

fn gate_policy(store: &Store, workitem_id: &str, gate: &str) -> Result<GateDefinition, Error> {
    let instance = sg_workflow::instance::for_workitem(store, workitem_id)?.ok_or_else(|| {
        Error::Message(format!(
            "fast_track_invalid: 工作项 {workitem_id} 无工作流实例"
        ))
    })?;
    let defs = sg_workflow::template::definitions_via_store(store, &instance.template_version_id)?;
    defs.into_iter()
        .find(|d| d.gate_id == gate)
        .ok_or_else(|| Error::Message(format!("fast_track_invalid: 未知关 {gate}")))
}

/// 服务端派生的快通道事实（六因素 + 证据引用 + 输入状态摘要）。
#[derive(Debug, Clone)]
pub struct FastTrackFacts {
    pub factors: FastTrackFactors,
    /// 证据引用（进建议 content 与拒绝错误信息，审计可见）。
    pub evidence: serde_json::Value,
    /// 权威输入状态摘要（冻结进建议；漂移检测用）。
    pub input_state_digest: String,
    /// 策略版本（模板版本 + 风险策略版本）。
    pub policy_version: String,
}

/// 当前 attempt 内的工具提案（tool, action_digest；含全部决定状态）。
fn attempt_proposals(
    store: &Store,
    attempt: &crate::attempt::StageAttempt,
) -> Result<Vec<(String, String)>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT p.tool, p.action_digest FROM tool_proposals p
             JOIN agent_runs r ON r.id = p.agent_run_id
             WHERE r.stage_attempt_id=?1 ORDER BY p.created_at, p.id",
        )?;
        let rows = stmt
            .query_map([&attempt.id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
}

/// gate 域证据行（id, kind, object_sha256, verified, created_at）。
type EvidenceRow = (String, String, String, bool, String);

fn gate_evidence(store: &Store, workitem_id: &str, gate: &str) -> Result<Vec<EvidenceRow>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, kind, object_sha256, verified, created_at FROM evidences
             WHERE workitem_id=?1 AND gate=?2 ORDER BY created_at, id",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![workitem_id, gate], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)? != 0,
                    r.get::<_, String>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
}

/// schema 契约面（requirement 修订集）：返回 (修订 id, content_sha256, created_at)。
fn schema_revisions(
    store: &Store,
    workitem_id: &str,
) -> Result<Vec<(String, String, String)>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT rv.id, rv.content_sha256, rv.created_at
             FROM requirement_revisions rv
             JOIN requirement_documents rd ON rd.id = rv.document_id
             WHERE rd.workitem_id=?1 ORDER BY rv.created_at, rv.id",
        )?;
        let rows = stmt
            .query_map([workitem_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
}

/// 工具注册表分类视图（依赖倒置：workitem 不依赖 sg-agent，由调用方注入
/// `Fn(&str) -> Option<ToolClass>`——RPC 面用 sg_agent::tools 注册表，
/// 单测用本地夹具）。
#[derive(Debug, Clone)]
pub struct ToolClass {
    pub effect_class: String,
    pub reversibility: String,
    pub protected_target: bool,
}

fn reversible(class: &ToolClass) -> bool {
    REVERSIBLE_VALUES.contains(&class.reversibility.as_str())
}

/// 六因素服务端派生（P0-2 核心）：全部来源只读权威表/注册表，缺失即 false。
/// `tool_class` 返回 None = 未注册工具（类别不可知 → 相关三因素全部 false）。
pub fn derive_fast_track_facts<F>(
    store: &Store,
    workitem_id: &str,
    gate: &str,
    def: &GateDefinition,
    attempt: &crate::attempt::StageAttempt,
    tool_class: F,
) -> Result<FastTrackFacts, Error>
where
    F: Fn(&str) -> Option<ToolClass>,
{
    // ① 工具面：attempt 内全部提案 × 注册表（未知工具 = unknown → false）。
    let proposals = attempt_proposals(store, attempt)?;
    let mut tool_facts = serde_json::Map::new();
    let (mut no_protected, mut read_only, mut rev) = (
        !proposals.is_empty(),
        !proposals.is_empty(),
        !proposals.is_empty(),
    );
    for (tool, _digest) in &proposals {
        match tool_class(tool) {
            Some(class) => {
                no_protected = no_protected && !class.protected_target;
                read_only = read_only && class.effect_class == "read";
                let rev_ok = reversible(&class);
                rev = rev && rev_ok;
                tool_facts.insert(
                    tool.clone(),
                    json!({
                        "effectClass": class.effect_class,
                        "reversibility": class.reversibility,
                        "protectedTarget": class.protected_target,
                    }),
                );
            }
            None => {
                // 未注册工具：类别不可知 → 三因素全部不满足（保守面）。
                no_protected = false;
                read_only = false;
                rev = false;
                tool_facts.insert(tool.clone(), json!({"registered": false}));
            }
        }
    }

    // ② 证据面：当前 attempt 内 verified test_report。
    let evidence = gate_evidence(store, workitem_id, gate)?;
    let test_evidence: Vec<&EvidenceRow> = evidence
        .iter()
        .filter(|(_, kind, _, verified, created)| {
            kind == "test_report" && *verified && created.as_str() >= attempt.created_at.as_str()
        })
        .collect();
    let test_evidence_present = !test_evidence.is_empty();

    // ③ schema 契约面：attempt 建立后无新增修订，且基线非空。
    let revisions = schema_revisions(store, workitem_id)?;
    let api_schema_unchanged = !revisions.is_empty()
        && revisions
            .iter()
            .all(|(_, _, created)| created.as_str() < attempt.created_at.as_str());

    // ④ 谱系面：全图 complete。
    let impact = sg_provenance::impact::for_workitem(store, workitem_id)?;
    let provenance_complete = impact.completeness == sg_provenance::impact::Completeness::Complete;

    let factors = FastTrackFactors {
        no_protected_path: no_protected,
        api_schema_unchanged,
        effect_class_read_only: read_only,
        reversibility_confirmed: rev,
        provenance_complete,
        test_evidence_present,
    };

    // ⑤ 冻结输入状态摘要：任何证据/策略/模板/输入包漂移必变。
    let policy_version = format!(
        "ft2|{}|{}",
        sg_policy::risk_model::POLICY_VERSION,
        def.version_id
    );
    let mut digest_lines: Vec<String> = Vec::new();
    digest_lines.push("ft2".into());
    digest_lines.push(format!("policy|{policy_version}"));
    digest_lines.push(format!(
        "ftpolicy|{}",
        serde_json::to_string(&def.fast_track_policy).unwrap_or_default()
    ));
    digest_lines.push(format!(
        "attempt|{}|{}",
        attempt.id, attempt.input_package_sha256
    ));
    digest_lines.push(format!("props|{}", proposals.len()));
    for (tool, digest) in &proposals {
        digest_lines.push(format!("P|{tool}|{digest}"));
    }
    digest_lines.push(format!("evidence|{}", evidence.len()));
    for (id, kind, object, verified, created) in &evidence {
        digest_lines.push(format!("E|{id}|{kind}|{object}|{verified}|{created}"));
    }
    digest_lines.push(format!("schema|{}", revisions.len()));
    for (id, content, created) in &revisions {
        digest_lines.push(format!("S|{id}|{content}|{created}"));
    }
    digest_lines.push(format!("facts|{}", impact.workitem_facts_digest));
    let input_state_digest = {
        use sha2::Digest;
        format!(
            "sha256:{}",
            sg_store::ids::hex(&sha2::Sha256::digest(digest_lines.join("\n").as_bytes()))
        )
    };

    let evidence_ref = json!({
        "proposals": proposals
            .iter()
            .map(|(t, d)| json!({"tool": t, "actionDigest": d}))
            .collect::<Vec<_>>(),
        "toolFacts": tool_facts,
        "testEvidence": test_evidence
            .iter()
            .map(|(id, _, _, _, _)| json!({"id": id}))
            .collect::<Vec<_>>(),
        "schemaRevisions": revisions.len(),
        "provenance": {
            "completeness": impact.completeness.as_str(),
            "reason": impact.reason,
            "factsDigest": impact.workitem_facts_digest,
        },
    });
    Ok(FastTrackFacts {
        factors,
        evidence: evidence_ref,
        input_state_digest,
        policy_version,
    })
}

/// 六因素评估 + 建议落 shadow（P0-2：服务端事实版）。策略未声明 / 派生非全真 →
/// 拒（先过期同 scope 未决建议——输入已漂移，旧建议不可再采纳）。
/// 同 attempt 同输入状态重放 → 返回既有建议（digest 幂等）。
/// `tool_class`：工具注册表分类注入（依赖倒置，见 ToolClass）。
pub fn evaluate_and_suggest<F>(
    store: &Store,
    workitem_id: &str,
    gate: &str,
    tool_class: F,
) -> Result<Suggestion, Error>
where
    F: Fn(&str) -> Option<ToolClass>,
{
    let def = gate_policy(store, workitem_id, gate)?;
    if def.fast_track_policy.is_none() {
        return Err(Error::Message(format!(
            "fast_track_forbidden: 关 {gate} 未声明 fast-track 策略"
        )));
    }
    let attempt = crate::attempt::ensure_active(store, workitem_id, gate)?;
    let facts = derive_fast_track_facts(store, workitem_id, gate, &def, &attempt, tool_class)?;
    if !facts.factors.all_true() {
        sg_workflow::shadow::expire_stale(
            store,
            "fast_track",
            workitem_id,
            &facts.input_state_digest,
        )?;
        return Err(Error::Message(format!(
            "fast_track_factors_not_all_true: 六因素须全真方可生成快通道建议（服务端派生：{}）",
            facts.evidence
        )));
    }
    // 身份幂等：digest 绑 (attempt, 权威输入状态摘要)——客户端无可影响面。
    let suggestion_digest = {
        use sha2::Digest;
        format!(
            "sha256:{}",
            sg_store::ids::hex(&sha2::Sha256::digest(
                format!(
                    "fast_track|{workitem_id}|{gate}|{}|{}",
                    attempt.id, facts.input_state_digest
                )
                .as_bytes()
            ))
        )
    };
    // 幂等重放：同 attempt 同输入状态已有建议 → 原样返回（并收敛过期旧建议）。
    if let Some(existing) = existing_by_digest(store, &suggestion_digest)? {
        let _ = sg_workflow::shadow::expire_stale(
            store,
            "fast_track",
            workitem_id,
            &facts.input_state_digest,
        )?;
        return Ok(existing);
    }
    let hypothetical = format!("sha256:{}", {
        use sha2::Digest;
        sg_store::ids::hex(&sha2::Sha256::digest(
            format!("fast_track_apply|{workitem_id}|{gate}|{}", attempt.id).as_bytes(),
        ))
    });
    let expires_at = sg_store::timefmt::now_plus_minutes(SUGGESTION_TTL_MINUTES);
    let suggestion = sg_workflow::shadow::record(
        store,
        &sg_workflow::shadow::SuggestionInput {
            source: "fast_track",
            automation_id: None,
            workitem_id: Some(workitem_id),
            suggestion_type: "gate_fast_track",
            suggestion_digest: &suggestion_digest,
            content: json!({
                "gate": gate,
                "factors": facts.factors,
                "facts": facts.evidence,
                "inputStateDigest": facts.input_state_digest,
            }),
            hypothetical_action_digest: &hypothetical,
            policy_version: &facts.policy_version,
            input_state_digest: &facts.input_state_digest,
            expires_at: Some(&expires_at),
            model: "",
            prompt_version: "",
        },
    )?;
    // 新建议落地：同 scope 旧输入状态的未决建议全部过期（只保留当前事实的建议）。
    sg_workflow::shadow::expire_stale(store, "fast_track", workitem_id, &facts.input_state_digest)?;
    Ok(suggestion)
}

fn existing_by_digest(store: &Store, digest: &str) -> Result<Option<Suggestion>, Error> {
    // 查 id 与 get 分两段：with_conn 内不得嵌套 shadow::get（Mutex 不可重入）。
    let id: Option<String> = store.with_conn(|conn| {
        match conn.query_row(
            "SELECT id FROM shadow_suggestions WHERE source='fast_track' AND suggestion_digest=?1",
            [digest],
            |r| r.get::<_, String>(0),
        ) {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(other) => Err(other.into()),
        }
    })?;
    match id {
        Some(id) => Ok(Some(sg_workflow::shadow::get(store, &id)?)),
        None => Ok(None),
    }
}

/// gate_fast_track_waivers 权威豁免（P0-3：采纳建议 ≠ 应用豁免——两动作分离）。
/// apply_waiver：独立领域 mutation；action_digest 绑
/// (workitem|gate|attempt|policy digest|waived kind|替代证据内容 digest|建议决定 digest)，
/// UNIQUE 承载幂等。应用时同步落活动缩减（skippable_activities pending→skipped）。
/// 参数即豁免申请的完整领域输入面（聚合结构会掩盖 digest 输入来源）。
#[allow(clippy::too_many_arguments)]
pub fn apply_waiver(
    store: &Store,
    workitem_id: &str,
    gate: &str,
    waived_kind: &str,
    substitute_evidence_id: &str,
    rationale: &str,
    suggestion_id: Option<&str>,
    created_by: &str,
) -> Result<serde_json::Value, Error> {
    let def = gate_policy(store, workitem_id, gate)?;
    let policy = def.fast_track_policy.ok_or_else(|| {
        Error::Message(format!(
            "fast_track_forbidden: 关 {gate} 未声明 fast-track 策略"
        ))
    })?;
    let spec = policy
        .waived_deliverables
        .iter()
        .find(|w| w.kind == waived_kind)
        .ok_or_else(|| {
            Error::Message(format!(
                "waiver_state_changed: kind {waived_kind} 不在关 {gate} 的豁免清单内"
            ))
        })?;
    // 替代证据在案 + verified + kind 匹配策略声明的替代类型。
    let evidence = sg_evidence::list(store, workitem_id, None)?
        .into_iter()
        .find(|e| e.id == substitute_evidence_id)
        .ok_or_else(|| {
            Error::Message(format!(
                "substitute_evidence_missing: 证据 {substitute_evidence_id} 不存在"
            ))
        })?;
    if !evidence.verified {
        return Err(Error::Message(
            "substitute_evidence_missing: 替代证据未核验".into(),
        ));
    }
    if evidence.kind != spec.substitute_evidence_kind {
        return Err(Error::Message(format!(
            "substitute_evidence_missing: 证据 kind {} 与豁免要求的 {} 不符",
            evidence.kind, spec.substitute_evidence_kind
        )));
    }
    let attempt = crate::attempt::ensure_active(store, workitem_id, gate)?;
    // 建议关联（可选）：给定时必须是指定关已采纳的 fast_track 建议。
    let mut suggestion_digest = String::new();
    if let Some(sid) = suggestion_id {
        let adopted =
            sg_workflow::shadow::decided_suggestions(store, "fast_track", workitem_id, "accepted")?
                .into_iter()
                .find(|s| s.id == sid);
        match adopted {
            Some(s) if s.content.get("gate").and_then(|g| g.as_str()) == Some(gate) => {
                suggestion_digest = s.suggestion_digest.clone();
            }
            _ => {
                return Err(Error::Message(
                    "waiver_state_changed: 建议不存在或未被采纳（采纳 ≠ 应用：请先决定建议）"
                        .into(),
                ));
            }
        }
    }
    // 策略摘要（canonical fast_track_policy）。
    let policy_digest = {
        use sha2::Digest;
        format!(
            "sha256:{}",
            sg_store::ids::hex(&sha2::Sha256::digest(
                serde_json::to_string(&policy)
                    .unwrap_or_default()
                    .as_bytes()
            ))
        )
    };
    let subst_digest = {
        use sha2::Digest;
        format!(
            "sha256:{}",
            sg_store::ids::hex(&sha2::Sha256::digest(
                format!(
                    "{}|{}|{}|{}|{}",
                    evidence.id,
                    evidence.kind,
                    evidence.object_sha256,
                    evidence.verified,
                    evidence.created_at
                )
                .as_bytes()
            ))
        )
    };
    let action_digest = {
        use sha2::Digest;
        format!(
            "sha256:{}",
            sg_store::ids::hex(&sha2::Sha256::digest(
                format!(
                    "gate_ft_waiver|{workitem_id}|{gate}|{}|{policy_digest}|{waived_kind}|{subst_digest}|{suggestion_digest}",
                    attempt.id
                )
                .as_bytes()
            ))
        )
    };
    // 幂等：同 action_digest 已有 active/revoked 豁免 → 原样返回。
    let existing: Option<(String, String)> = store.with_conn(|conn| {
        Ok(conn
            .query_row(
                "SELECT id, status FROM gate_fast_track_waivers WHERE action_digest=?1",
                [&action_digest],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok())
    })?;
    if let Some((id, status)) = existing {
        return Ok(
            json!({"waiverId": id, "status": status, "actionDigest": action_digest, "idempotentReplay": true}),
        );
    }
    let id = ids::new_id("gfw");
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO gate_fast_track_waivers(id, workitem_id, gate, stage_attempt_id, policy_digest,
                 waived_kind, substitute_evidence_id, substitute_evidence_digest, rationale,
                 suggestion_id, action_digest, status, created_by, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'active',?12,?13)",
            rusqlite::params![
                id,
                workitem_id,
                gate,
                attempt.id,
                policy_digest,
                waived_kind,
                substitute_evidence_id,
                subst_digest,
                rationale,
                suggestion_id,
                action_digest,
                created_by,
                now
            ],
        )?;
        Ok(())
    })
    .map_err(|e| {
        if e.to_string().contains("UNIQUE constraint failed") {
            Error::Message("waiver_state_changed: 同参豁免已存在（并发收敛重读）".into())
        } else {
            e
        }
    })?;
    // 活动缩减（应用豁免的领域效果；幂等——pending 才命中）。
    if !policy.skippable_activities.is_empty() {
        store.with_conn(|conn| {
            for key in &policy.skippable_activities {
                conn.execute(
                    "UPDATE stage_activities SET state='skipped', updated_at=?2
                     WHERE stage_attempt_id=?1 AND activity_key=?3 AND state='pending'",
                    rusqlite::params![attempt.id, timefmt::now(), key],
                )?;
            }
            Ok(())
        })?;
    }
    sg_store::audit::append(
        store,
        created_by,
        "fast_track.waiver_applied",
        "workitem",
        workitem_id,
        json!({"waiverId": id, "gate": gate, "waivedKind": waived_kind,
               "substituteEvidenceId": substitute_evidence_id, "suggestionId": suggestion_id}),
    )?;
    outbox::emit(
        store,
        "workitem",
        workitem_id,
        "fast_track.waiver_applied",
        json!({"waiverId": id, "gate": gate, "waivedKind": waived_kind}),
    )?;
    Ok(json!({"waiverId": id, "status": "active", "actionDigest": action_digest}))
}

/// 撤销豁免（安全收紧通道：不受 flag 关闭影响，始终可用）。
/// 撤销使 (workitem,gate) 的 pending 放行失效（gate_release_requests → superseded；
/// pending gate_release 审批 → expired）——基于旧豁免集的放行不得继续。
pub fn revoke_waiver(
    store: &Store,
    waiver_id: &str,
    reason: &str,
    revoked_by: &str,
) -> Result<serde_json::Value, Error> {
    let row: Option<(String, String, String, String)> = store.with_conn(|conn| {
        Ok(conn
            .query_row(
                "SELECT workitem_id, gate, status, COALESCE(revoked_reason,'')
                 FROM gate_fast_track_waivers WHERE id=?1",
                [waiver_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .ok())
    })?;
    let Some((workitem_id, gate, status, prior_reason)) = row else {
        return Err(Error::Message(format!(
            "waiver_state_changed: 豁免 {waiver_id} 不存在"
        )));
    };
    if status == "revoked" {
        return Ok(json!({
            "waiverId": waiver_id, "status": "revoked",
            "revokedReason": prior_reason, "idempotentReplay": true,
        }));
    }
    let now = timefmt::now();
    store.with_tx(|conn| {
        let n = conn.execute(
            "UPDATE gate_fast_track_waivers SET status='revoked', revoked_at=?1, revoked_reason=?2
             WHERE id=?3 AND status='active'",
            rusqlite::params![now, reason, waiver_id],
        )?;
        if n != 1 {
            return Err(Error::Message(
                "waiver_state_changed: 豁免已被并发撤销".into(),
            ));
        }
        // pending 放行失效（豁免集已变——旧 pending 不得继续）。
        conn.execute(
            "UPDATE gate_release_requests SET state='superseded', decided_at=?1
             WHERE state='pending' AND stage_attempt_id IN
               (SELECT id FROM stage_attempts WHERE workitem_id=?2 AND gate=?3)",
            rusqlite::params![now, workitem_id, gate],
        )?;
        conn.execute(
            "UPDATE approvals SET status='expired', reason='豁免撤销，pending 放行失效'
             WHERE subject_type='gate_release' AND workitem_id=?1 AND status='requested'
               AND stage_attempt_id IN
                 (SELECT id FROM stage_attempts WHERE workitem_id=?1 AND gate=?2)",
            rusqlite::params![workitem_id, gate],
        )?;
        sg_store::audit::append_at(
            conn,
            revoked_by,
            "fast_track.waiver_revoked",
            "workitem",
            &workitem_id,
            json!({"waiverId": waiver_id, "reason": reason}),
        )?;
        outbox::emit_at(
            conn,
            "workitem",
            &workitem_id,
            "fast_track.waiver_revoked",
            json!({"waiverId": waiver_id, "gate": gate}),
        )?;
        Ok(())
    })?;
    Ok(json!({"waiverId": waiver_id, "status": "revoked", "revokedReason": reason}))
}

/// 交付物豁免清单（P0-3：改读 gate_fast_track_waivers 权威表——
/// active 豁免的 kind 集 × 策略声明的替代类型；无豁免 = 空）。
pub fn waived_deliverables(
    store: &Store,
    workitem_id: &str,
    gate: &str,
) -> Result<Vec<WaivedDeliverable>, Error> {
    let def = gate_policy(store, workitem_id, gate).ok();
    let Some(def) = def.filter(|d| d.fast_track_policy.is_some()) else {
        return Ok(Vec::new());
    };
    let policy = def.fast_track_policy.unwrap();
    let waived_kinds: Vec<String> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT waived_kind FROM gate_fast_track_waivers
             WHERE workitem_id=?1 AND gate=?2 AND status='active'",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![workitem_id, gate], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    Ok(policy
        .waived_deliverables
        .into_iter()
        .filter(|w| waived_kinds.contains(&w.kind))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sg_store::{ids, timefmt};

    fn setup() -> Store {
        let dir =
            std::env::temp_dir().join(format!("sg-ft-{}-{}", std::process::id(), ids::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj','u','n','p','main',?1)",
                    [timefmt::now()],
                )?;
                Ok(())
            })
            .unwrap();
        store
    }

    fn workitem_with_ft_gate(store: &Store) -> String {
        let t = sg_workflow::template::create_template(store, "ft-tpl", "快通道模板").unwrap();
        let v = sg_workflow::template::create_version(
            store,
            &t.id,
            &[sg_workflow::template::GateDefInput {
                gate_id: "build".into(),
                title: "构建关".into(),
                purpose: String::new(),
                agent: String::new(),
                deliverables: vec!["code".into()],
                acceptance: vec![],
                context_policy_ref: None,
                team_policy_ref: None,
                workspace_policy_ref: None,
                skip_policy: None,
                fast_track_policy: Some(sg_workflow::template::FastTrackPolicy {
                    skippable_activities: vec!["execution".into()],
                    waived_deliverables: vec![sg_workflow::template::WaivedDeliverable {
                        kind: "code".into(),
                        substitute_evidence_kind: "manual".into(),
                    }],
                    reduced_approval: true,
                }),
            }],
            "tester",
        )
        .unwrap();
        sg_workflow::template::activate(store, &v.id).unwrap();
        crate::create_with_template(store, "pj", "快通道任务", "", None, &[], Some("ft-tpl"))
            .unwrap()
            .id
    }

    /// 本地工具注册表夹具（依赖倒置注入；与 sg_agent::tools 词汇一致）。
    fn registry(tool: &str) -> Option<ToolClass> {
        match tool {
            "read_file" | "search_knowledge" => Some(ToolClass {
                effect_class: "read".into(),
                reversibility: "logical_restore".into(),
                protected_target: false,
            }),
            "write_file" => Some(ToolClass {
                effect_class: "local_write".into(),
                reversibility: "logical_restore".into(),
                protected_target: false,
            }),
            _ => None,
        }
    }

    /// 服务端事实六因素夹具：read-only 工具提案 + verified test 证据 + schema 基线
    /// （attempt 建立前）+ 谱系节点。drift 参数控制哪一项被破坏。
    fn seed_server_facts(store: &Store, wi: &str, drift: &str) {
        let attempt = crate::attempt::ensure_active(store, wi, "build").unwrap();
        let now = timefmt::now();
        store
            .with_conn(|c| {
                // 谱系节点（for_workitem complete 需要 ≥1 节点）。
                c.execute(
                    "INSERT INTO provenance_nodes(id, workitem_id, node_type, entity_id, content_digest, verification_state, created_at)
                     VALUES ('pn1', ?1, 'requirement_item', 'ri1', 'cd1', 'verified', ?2)",
                    rusqlite::params![wi, now],
                )?;
                // schema 基线：attempt 建立前的修订。
                c.execute(
                    "INSERT INTO requirement_documents(id, workitem_id, source_kind, title, created_at)
                     VALUES ('rd1', ?1, 'inline', '需求', ?2)",
                    rusqlite::params![wi, now],
                )?;
                let rev_created = if drift == "schema" { &now } else { "2020-01-01T00:00:00.000Z" };
                c.execute(
                    "INSERT INTO requirement_revisions(id, document_id, revision_no, object_sha256, content_sha256, created_by, created_at)
                     VALUES ('rr1', 'rd1', 1, 'obj1', 'cs1', 't', ?1)",
                    [rev_created],
                )?;
                // 工具面：read_file 提案（经 agent_runs.stage_attempt_id 挂到 attempt）。
                c.execute(
                    "INSERT INTO context_manifests(id, workitem_id, scope, data_policy, created_at)
                     VALUES ('ctxft', ?1, '{}', 'standard', ?2)",
                    rusqlite::params![wi, now],
                )?;
                c.execute(
                    "INSERT INTO agent_runs(id, workitem_id, task_id, goal, input_baseline_sha, context_manifest_id,
                         tool_allowlist, budget, policy_snapshot, idempotency_key, status, stage_attempt_id, created_at, updated_at)
                     VALUES ('run1', ?1, '', 'g', '', 'ctxft', '[]', '{}', 'default', ?2, 'completed_execution', ?3, ?4, ?4)",
                    rusqlite::params![wi, ids::new_id("ik"), attempt.id, now],
                )?;
                let tool = match drift {
                    "protected" => "write_file",
                    _ => "read_file",
                };
                c.execute(
                    "INSERT INTO tool_proposals(id, agent_run_id, tool, arguments, risk, action_digest, requires_approval, decision, created_at)
                     VALUES ('tp1', 'run1', ?1, '{}', 'low', 'ad1', 0, 'executed', ?2)",
                    rusqlite::params![tool, now],
                )?;
                // 证据面：verified test_report（drift=evidence 时不种）。
                if drift != "evidence" {
                    c.execute(
                        "INSERT INTO evidences(id, workitem_id, gate, kind, title, object_sha256, payload, source, verified, verified_at, verified_by, created_at)
                         VALUES ('ev1', ?1, 'build', 'test_report', '测试', 'obj', '{}', 'local', 1, ?2, 'qa', ?2)",
                        rusqlite::params![wi, now],
                    )?;
                }
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn server_facts_all_true_suggest_and_apply() {
        let store = setup();
        let wi = workitem_with_ft_gate(&store);
        seed_server_facts(&store, &wi, "none");
        // 全真 → 建议落 shadow；同状态重放幂等返回同一建议。
        let s1 = evaluate_and_suggest(&store, &wi, "build", registry).unwrap();
        assert_eq!(s1.source, "fast_track");
        assert_eq!(s1.scope_key, wi);
        assert_eq!(s1.content["gate"], json!("build"));
        assert_eq!(
            s1.content["factors"]["reversibility_confirmed"],
            json!(true),
            "六因素含 reversibility_confirmed"
        );
        assert!(!s1.input_state_digest.is_empty());
        assert!(s1.expires_at.is_some());
        let replay = evaluate_and_suggest(&store, &wi, "build", registry).unwrap();
        assert_eq!(replay.id, s1.id);
        // 采纳 ≠ 应用（P0-3 两动作分离）：仅决定建议时无豁免。
        sg_workflow::shadow::decide(&store, &s1.id, "accepted", "owner", "采纳").unwrap();
        assert!(
            waived_deliverables(&store, &wi, "build")
                .unwrap()
                .is_empty(),
            "仅采纳建议：无豁免（必须显式 applyWaiver）"
        );
        // 替代证据（manual，匹配策略 substitute_evidence_kind）→ 应用豁免。
        let now = timefmt::now();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO evidences(id, workitem_id, gate, kind, title, object_sha256, payload, source, verified, verified_at, verified_by, created_at)
                     VALUES ('ev_manual', ?1, 'build', 'manual', '替代核验', 'objm', '{}', 'local', 1, ?2, 'qa', ?2)",
                    rusqlite::params![wi, now],
                )?;
                Ok(())
            })
            .unwrap();
        let w1 = apply_waiver(
            &store,
            &wi,
            "build",
            "code",
            "ev_manual",
            "线下核验替代",
            Some(&s1.id),
            "owner",
        )
        .unwrap();
        assert_eq!(w1["status"], json!("active"));
        let replay = apply_waiver(
            &store,
            &wi,
            "build",
            "code",
            "ev_manual",
            "线下核验替代",
            Some(&s1.id),
            "owner",
        )
        .unwrap();
        assert_eq!(replay["idempotentReplay"], json!(true), "同参豁免幂等");
        assert_eq!(
            waived_deliverables(&store, &wi, "build").unwrap().len(),
            1,
            "应用后豁免生效"
        );
        // 未采纳建议的豁免引用 → 拒。
        let err = apply_waiver(
            &store,
            &wi,
            "build",
            "code",
            "ev_manual",
            "r",
            Some("shs_nonexistent"),
            "owner",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("waiver_state_changed"), "{err}");
        // 撤销：豁免面清空 + 幂等。
        let revoked =
            revoke_waiver(&store, w1["waiverId"].as_str().unwrap(), "复核不通过", "qa").unwrap();
        assert_eq!(revoked["status"], json!("revoked"));
        let revoked2 =
            revoke_waiver(&store, w1["waiverId"].as_str().unwrap(), "重复撤销", "qa").unwrap();
        assert_eq!(revoked2["idempotentReplay"], json!(true), "撤销幂等");
        assert!(
            waived_deliverables(&store, &wi, "build")
                .unwrap()
                .is_empty(),
            "撤销后豁免面清空"
        );
    }

    #[test]
    fn missing_or_drifted_facts_reject_and_expire() {
        let store = setup();
        let wi = workitem_with_ft_gate(&store);
        // 无任何服务端事实 → 拒（证据引用进错误信息）。
        let err = evaluate_and_suggest(&store, &wi, "build", registry)
            .unwrap_err()
            .to_string();
        assert!(err.contains("fast_track_factors_not_all_true"), "{err}");
        assert!(err.contains("proposals"), "错误信息携带证据引用：{err}");

        // 建立全真事实 → 建议 s1 未决。
        seed_server_facts(&store, &wi, "none");
        let s1 = evaluate_and_suggest(&store, &wi, "build", registry).unwrap();

        // 漂移①：删除证据 → test_evidence_present=false → 拒 + 旧建议过期。
        store
            .with_conn(|c| {
                c.execute("DELETE FROM evidences WHERE id='ev1'", [])?;
                Ok(())
            })
            .unwrap();
        let err = evaluate_and_suggest(&store, &wi, "build", registry)
            .unwrap_err()
            .to_string();
        assert!(err.contains("fast_track_factors_not_all_true"), "{err}");
        let obs = sg_workflow::shadow::observations(&store, Some("fast_track"), None).unwrap();
        let s1_state = obs
            .items
            .iter()
            .find(|o| o.suggestion.id == s1.id)
            .and_then(|o| o.decision.as_ref())
            .map(|d| d.decision.clone())
            .unwrap_or_default();
        assert_eq!(s1_state, "expired", "删证据使旧建议过期");
        // 过期建议不可采纳。
        let conflict = sg_workflow::shadow::decide(&store, &s1.id, "accepted", "owner", "")
            .unwrap_err()
            .to_string();
        assert!(conflict.contains("不可改判"), "{conflict}");
    }

    #[test]
    fn drift_matrix_rejects() {
        for drift in ["protected", "schema", "evidence"] {
            let store = setup();
            let wi = workitem_with_ft_gate(&store);
            seed_server_facts(&store, &wi, drift);
            let err = evaluate_and_suggest(&store, &wi, "build", registry)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("fast_track_factors_not_all_true"),
                "{drift} 应拒绝：{err}"
            );
        }
    }

    #[test]
    fn policy_undeclared_gate_rejected() {
        let store = setup();
        let wi = crate::create_with_template(&store, "pj", "无策略任务", "", None, &[], None)
            .unwrap()
            .id;
        let err = evaluate_and_suggest(&store, &wi, "requirements", registry)
            .unwrap_err()
            .to_string();
        assert!(err.contains("fast_track_forbidden"), "{err}");
        let _ = workitem_with_ft_gate(&store); // 模板独立存在，不影响默认实例
    }
}
