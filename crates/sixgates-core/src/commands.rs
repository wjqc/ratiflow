//! Slash 指令（EvoFlow 方案 M5-06 / ADR-039 §6.13）：
//! 前端字符串 → typed intent → preview（目标/影响/token）→ 确认后 execute。
//! 破坏性或状态改变指令必须先 preview：previewToken = canonical intent 的
//! sha256，execute 重算比对（不一致拒绝）；execute 命中与 UI 相同的领域服务
//! 与审批链（放行仍需 decideRelease 人工审批），不得旁路。

use serde_json::{json, Value};
use sg_store::{Error, Store};
use sha2::{Digest, Sha256};

use crate::dispatch::release_policy_version;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashIntent {
    /// /放行 [gate]：当前关（或指定关）gate.requestRelease。
    GateRelease,
    /// /打回 [gate]：pending 放行请求打回（changes_requested）。
    GateReject,
    /// /回滚：当前关快照回滚预览+请求（需后续审批）。
    Rollback,
    /// /取消 <runId>：Agent Run 取消。
    CancelRun,
    /// /换模板 <targetVersionId>：实例迁移（仅未开工）。
    Migrate,
}

impl SlashIntent {
    pub fn verb(&self) -> &'static str {
        match self {
            SlashIntent::GateRelease => "放行",
            SlashIntent::GateReject => "打回",
            SlashIntent::Rollback => "回滚",
            SlashIntent::CancelRun => "取消",
            SlashIntent::Migrate => "换模板",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Parsed {
    pub intent: SlashIntent,
    pub workitem_id: String,
    pub gate: Option<String>,
    pub run_id: Option<String>,
    pub target_version_id: Option<String>,
}

/// 解析中文指令文本：`/放行`、`/打回 testing`、`/取消 run_xxx`、`/换模板 wfv_xxx`。
/// workitem 由调用面（当前选中任务）提供。
pub fn parse(text: &str, workitem_id: &str) -> Result<Parsed, Error> {
    let text = text.trim();
    let rest = text
        .strip_prefix("/放行")
        .map(|r| (SlashIntent::GateRelease, r))
        .or_else(|| {
            text.strip_prefix("/打回")
                .map(|r| (SlashIntent::GateReject, r))
        })
        .or_else(|| {
            text.strip_prefix("/回滚")
                .map(|r| (SlashIntent::Rollback, r))
        })
        .or_else(|| {
            text.strip_prefix("/取消")
                .map(|r| (SlashIntent::CancelRun, r))
        })
        .or_else(|| {
            text.strip_prefix("/换模板")
                .map(|r| (SlashIntent::Migrate, r))
        });
    let Some((intent, rest)) = rest else {
        return Err(Error::Message(format!(
            "command_parse_failed: 未知指令 {text}"
        )));
    };
    let args: Vec<&str> = rest.split_whitespace().collect();
    Ok(Parsed {
        intent,
        workitem_id: workitem_id.to_string(),
        gate: args.first().map(|s| s.to_string()),
        run_id: args.first().map(|s| s.to_string()),
        target_version_id: args.first().map(|s| s.to_string()),
    })
}

fn canonical(parsed: &Parsed) -> String {
    format!(
        "cmd1|{:?}|{}|{}|{}|{}",
        parsed.intent,
        parsed.workitem_id,
        parsed.gate.as_deref().unwrap_or(""),
        parsed.run_id.as_deref().unwrap_or(""),
        parsed.target_version_id.as_deref().unwrap_or("")
    )
}

/// previewToken = canonical intent sha256（无状态 token；execute 重算比对）。
pub fn preview_token(parsed: &Parsed) -> String {
    sg_store::ids::hex(&Sha256::digest(canonical(parsed).as_bytes()))
}

/// preview：目标 + 影响范围 + token（不做任何副作用）。
pub fn preview(store: &Store, text: &str, workitem_id: &str) -> Result<Value, Error> {
    let parsed = parse(text, workitem_id)?;
    let token = preview_token(&parsed);
    let wi = sg_workitem::get(store, workitem_id)?;
    let gate = parsed
        .gate
        .clone()
        .unwrap_or_else(|| wi.current_gate.clone());
    let target = match parsed.intent {
        SlashIntent::GateRelease => format!("关卡 {} 放行请求（需人工审批后生效）", gate),
        SlashIntent::GateReject => format!("关卡 {} pending 放行请求打回", gate),
        SlashIntent::Rollback => format!("关卡 {} 回滚（快照恢复，需审批）", gate),
        SlashIntent::CancelRun => format!("取消运行 {}", parsed.run_id.as_deref().unwrap_or("?")),
        SlashIntent::Migrate => format!(
            "实例迁移至 {}（仅未开工任务）",
            parsed.target_version_id.as_deref().unwrap_or("?")
        ),
    };
    Ok(json!({
        "intent": parsed.intent.verb(),
        "workItemId": workitem_id,
        "gate": gate,
        "target": target,
        "requiresApproval": matches!(parsed.intent, SlashIntent::GateRelease | SlashIntent::Rollback),
        "previewToken": token,
        "note": "确认后执行；执行命中与 UI 相同的领域服务与审批链",
    }))
}

/// execute：token 比对（重算 canonical sha256），命中同一领域服务。
pub fn execute(store: &Store, text: &str, workitem_id: &str, token: &str) -> Result<Value, Error> {
    let parsed = parse(text, workitem_id)?;
    if preview_token(&parsed) != token {
        return Err(Error::Message(
            "command_token_mismatch: preview token 与指令不一致，请重新预览".into(),
        ));
    }
    let wi = sg_workitem::get(store, workitem_id)?;
    let gate = parsed
        .gate
        .clone()
        .unwrap_or_else(|| wi.current_gate.clone());
    match parsed.intent {
        SlashIntent::GateRelease => {
            // 同一放行链：request_release（冻结输出包+审批）。仍需 decideRelease 人工审批。
            let rr = sg_workitem::release::request_release(
                store,
                workitem_id,
                &gate,
                &release_policy_version(store),
                3600,
            )?;
            Ok(json!({"executed": "gate.requestRelease", "release": rr}))
        }
        SlashIntent::GateReject => {
            // pending 放行审批 → changes_requested（经 approval 链，AC-SW-05 同链）。
            let approval_id: Option<String> = store
                .with_conn(|conn| {
                    Ok(conn
                        .query_row(
                            "SELECT a.id FROM approvals a
                             JOIN stage_attempts sa ON sa.id = a.stage_attempt_id
                             WHERE a.subject_type='gate_release' AND sa.workitem_id=?1 AND sa.gate=?2
                               AND a.status='requested'
                             ORDER BY a.created_at DESC LIMIT 1",
                            rusqlite::params![workitem_id, gate],
                            |r| r.get(0),
                        )
                        .ok())
                })
                .unwrap_or(None);
            let Some(approval_id) = approval_id else {
                return Err(Error::Message(format!(
                    "command_target_missing: {} 关无待决放行审批",
                    gate
                )));
            };
            let rr = sg_workitem::release::decide_release(
                store,
                &approval_id,
                "changes_requested",
                "slash",
                "Slash 打回",
                &release_policy_version(store),
            )?;
            Ok(json!({"executed": "gate.decideRelease", "release": rr}))
        }
        SlashIntent::Rollback => {
            // 回滚：当前关 entry 快照 → request（建审批，批准后执行同链）。
            let snapshot = sg_workitem::snapshot::latest_entry(store, workitem_id, &gate)?
                .ok_or_else(|| {
                    Error::Message(format!("command_target_missing: {} 关无关前快照", gate))
                })?;
            let pv = release_policy_version(store);
            let _ = sg_workitem::rollback::preview(store, workitem_id, &snapshot.id, &pv)?;
            let requested = sg_workitem::rollback::request(
                store,
                workitem_id,
                &snapshot.id,
                "slash",
                &pv,
                3600,
            )?;
            Ok(json!({"executed": "rollback.request", "operation": requested}))
        }
        SlashIntent::CancelRun => {
            let run_id = parsed
                .run_id
                .clone()
                .ok_or_else(|| Error::Message("command_target_missing: /取消 需要 runId".into()))?;
            sg_agent::cancel(store, &run_id)?;
            Ok(json!({"executed": "agent.cancel", "runId": run_id}))
        }
        SlashIntent::Migrate => {
            let target = parsed.target_version_id.clone().ok_or_else(|| {
                Error::Message("command_target_missing: /换模板 需要 targetVersionId".into())
            })?;
            sg_workflow::instance::migrate(store, workitem_id, &target)?;
            Ok(json!({"executed": "workflow.migrate", "toVersionId": target}))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_token_mismatch() {
        let parsed = parse("/打回 testing", "wi").unwrap();
        assert_eq!(parsed.intent, SlashIntent::GateReject);
        assert_eq!(parsed.gate.as_deref(), Some("testing"));
        // 未知指令拒绝。
        assert!(parse("/不存在的指令", "wi").is_err());
        // preview token 稳定且随意图变化。
        let p1 = parse("/打回 testing", "wi").unwrap();
        let p2 = parse("/打回 development", "wi").unwrap();
        assert_ne!(preview_token(&p1), preview_token(&p2));
    }
}
