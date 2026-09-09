//! 阶段 Agent 选路（ADR-030 M4 / SG-AGT-003..005 / 蓝图 §8.1）。
//! 优先级固定：任务显式覆盖 → 任务级默认（"@ 指派"，workitem 冻结版本）→
//! 项目 activity 绑定 → 全局绑定 → 内置通用 Agent；
//! 校验 enabled + profile digest + 适配器健康 + 能力；fallback 分 generic / fail_closed；
//! 每次选路冻结 requested/resolved/候选/原因/fallback 标记并写入 agent_selections。

use serde::Serialize;
use sg_store::{ids, outbox, timefmt, Error, Store};
use sha2::{Digest, Sha256};

use crate::profile::{
    self, adapter_health, ensure_builtin_generic, get_profile, get_version, Binding, ProfileVersion,
};

#[derive(Debug, Clone, Serialize)]
pub struct Selection {
    pub id: String,
    pub stage_activity_id: String,
    pub requested_profile_version_id: Option<String>,
    pub resolved_profile_version_id: String,
    pub source_scope: String,
    pub fallback_used: bool,
    pub reason_code: String,
    pub candidate_report: serde_json::Value,
    pub selection_digest: String,
    pub created_at: String,
}

/// 选路上下文。
pub struct ResolveContext<'a> {
    pub project_id: &'a str,
    pub gate: &'a str,
    pub activity_key: &'a str,
    pub stage_activity_id: &'a str,
    pub task_override_version_id: Option<&'a str>,
    /// 任务级默认 Agent（workitems.default_profile_version_id，"@ 指派"）：
    /// 优先级介于任务显式覆盖与项目/全局绑定之间；None = 未指派。
    pub workitem_default_version_id: Option<&'a str>,
    pub required_capabilities: &'a [String],
    pub persist: bool,
}

/// 五级候选：task_override → workitem_default → project_binding → global_binding → builtin_generic。
pub fn resolve(store: &Store, ctx: &ResolveContext<'_>) -> Result<Selection, Error> {
    let ResolveContext {
        project_id,
        gate,
        activity_key,
        stage_activity_id,
        task_override_version_id,
        workitem_default_version_id,
        required_capabilities,
        persist,
    } = *ctx;
    resolve_inner(
        store,
        project_id,
        gate,
        activity_key,
        stage_activity_id,
        task_override_version_id,
        workitem_default_version_id,
        required_capabilities,
        persist,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_inner(
    store: &Store,
    project_id: &str,
    gate: &str,
    activity_key: &str,
    stage_activity_id: &str,
    task_override_version_id: Option<&str>,
    workitem_default_version_id: Option<&str>,
    required_capabilities: &[String],
    persist: bool,
) -> Result<Selection, Error> {
    let builtin = ensure_builtin_generic(store)?;
    let mut report: Vec<serde_json::Value> = Vec::new();
    let mut fallback_mode_of_failed: Option<String> = None;

    // 1. 任务显式覆盖。
    if let Some(version_id) = task_override_version_id.filter(|v| !v.is_empty()) {
        let outcome = check_candidate(store, version_id, required_capabilities);
        let ok = outcome.is_ok();
        report.push(candidate("task_override", version_id, &outcome));
        if ok {
            return commit(
                store,
                stage_activity_id,
                Some(version_id),
                version_id,
                "task_override",
                false,
                "",
                report,
                persist,
            );
        }
        fallback_mode_of_failed = Some("generic".into());
    }

    // 1.5 任务级默认（"@ 指派"，创建/更新时冻结的版本）：候选不可用不阻断——
    //     记入报告后继续走绑定链（指派是偏好不是硬约束；与 fail_closed 语义区分）。
    if let Some(version_id) = workitem_default_version_id.filter(|v| !v.is_empty()) {
        let outcome = check_candidate(store, version_id, required_capabilities);
        let ok = outcome.is_ok();
        report.push(candidate("workitem_default", version_id, &outcome));
        if ok {
            let had_failure = task_override_version_id.is_some();
            return commit(
                store,
                stage_activity_id,
                Some(version_id),
                version_id,
                "workitem_default",
                had_failure,
                if had_failure {
                    "task_override_failed"
                } else {
                    ""
                },
                report,
                persist,
            );
        }
    }

    // 2/3. 项目绑定 → 全局绑定（priority 降序）。
    let bindings = profile::list_bindings(store, Some(project_id))?;
    let matching: Vec<&Binding> = bindings
        .iter()
        .filter(|b| b.gate == gate && b.activity_key == activity_key && b.enabled)
        .collect();
    let mut specific_failed: Option<String> = None;
    let has_specific = !matching.is_empty();
    for binding in matching {
        let scope = if binding.project_id.is_some() {
            "project_binding"
        } else {
            "global_binding"
        };
        let outcome = check_candidate(store, &binding.profile_version_id, required_capabilities);
        let ok = outcome.is_ok();
        report.push(candidate(scope, &binding.profile_version_id, &outcome));
        if ok {
            let had_failure = specific_failed.is_some() || task_override_version_id.is_some();
            return commit(
                store,
                stage_activity_id,
                None,
                &binding.profile_version_id,
                scope,
                had_failure,
                if had_failure {
                    specific_failed.as_deref().unwrap_or("")
                } else {
                    ""
                },
                report,
                persist,
            );
        }
        specific_failed = Some(outcome.err().unwrap_or_default());
        fallback_mode_of_failed = Some(binding.fallback_mode.clone());
    }

    // 4. 回退：generic → builtin；fail_closed → 拒绝（SG-AGT-004）。
    if fallback_mode_of_failed.as_deref() == Some("fail_closed") && has_specific {
        return Err(Error::Message(format!(
            "agent_profile_unavailable: {gate}/{activity_key} 绑定 fail_closed 且专属 Agent 不可用（{}）",
            specific_failed.unwrap_or_default()
        )));
    }
    report.push(candidate("builtin_generic", &builtin.id, &Ok(())));
    commit(
        store,
        stage_activity_id,
        None,
        &builtin.id,
        "builtin_generic",
        true,
        &specific_failed.unwrap_or_else(|| "no_binding".into()),
        report,
        persist,
    )
}

fn candidate(scope: &str, version_id: &str, outcome: &Result<(), String>) -> serde_json::Value {
    serde_json::json!({
        "scope": scope,
        "profileVersionId": version_id,
        "ok": outcome.is_ok(),
        "reason": outcome.as_ref().err().cloned().unwrap_or_default(),
    })
}

/// 单候选校验：版本存在 → profile enabled → 适配器健康 → 能力覆盖。
fn check_candidate(
    store: &Store,
    version_id: &str,
    required_capabilities: &[String],
) -> Result<(), String> {
    let version = match get_version(store, version_id) {
        Ok(v) => v,
        Err(_) => return Err("version_not_found".into()),
    };
    let prof = match get_profile(store, &version.profile_id) {
        Ok(p) => p,
        Err(_) => return Err("profile_not_found".into()),
    };
    if !prof.enabled {
        return Err("profile_disabled".into());
    }
    let (healthy, reason) = adapter_health(store, &prof).unwrap_or((false, "health_error".into()));
    if !healthy {
        return Err(format!("adapter_unhealthy:{reason}"));
    }
    for need in required_capabilities {
        if !version.capabilities.iter().any(|c| c == need) {
            return Err(format!("capability_mismatch:{need}"));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn commit(
    store: &Store,
    stage_activity_id: &str,
    requested: Option<&str>,
    resolved: &str,
    scope: &str,
    fallback_used: bool,
    reason_code: &str,
    report: Vec<serde_json::Value>,
    persist: bool,
) -> Result<Selection, Error> {
    let digest = {
        let mut hasher = Sha256::new();
        hasher.update(stage_activity_id.as_bytes());
        hasher.update(b"|");
        hasher.update(resolved.as_bytes());
        hasher.update(b"|");
        hasher.update(scope.as_bytes());
        hasher.update(b"|");
        hasher.update(if fallback_used { b"1" } else { b"0" });
        ids::hex(&hasher.finalize())
    };
    let selection = Selection {
        id: ids::new_id("asel"),
        stage_activity_id: stage_activity_id.into(),
        requested_profile_version_id: requested.filter(|s| !s.is_empty()).map(String::from),
        resolved_profile_version_id: resolved.into(),
        source_scope: scope.into(),
        fallback_used,
        reason_code: reason_code.into(),
        candidate_report: serde_json::Value::Array(report),
        selection_digest: digest,
        created_at: timefmt::now(),
    };
    if persist {
        store.with_conn(|conn| {
            conn.execute(
                "INSERT INTO agent_selections(id, stage_activity_id, requested_profile_version_id, resolved_profile_version_id, source_scope, fallback_used, reason_code, candidate_report_json, selection_digest, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                rusqlite::params![
                    selection.id,
                    selection.stage_activity_id,
                    selection.requested_profile_version_id,
                    selection.resolved_profile_version_id,
                    selection.source_scope,
                    selection.fallback_used as i64,
                    selection.reason_code,
                    serde_json::to_string(&selection.candidate_report).unwrap_or_default(),
                    selection.selection_digest,
                    selection.created_at
                ],
            )?;
            Ok(())
        })?;
        outbox::emit(
            store,
            "agent",
            &selection.id,
            if fallback_used {
                "agent.fallback_used"
            } else {
                "agent.selection_resolved"
            },
            serde_json::json!({
                "selectionId": selection.id,
                "stageActivityId": stage_activity_id,
                "resolvedProfileVersionId": resolved,
                "scope": scope,
                "reasonCode": reason_code,
            }),
        )?;
    }
    Ok(selection)
}

/// 选路预览（不做任何写入；蓝图 §10.3 resolvePreview）。
pub fn resolve_preview(
    store: &Store,
    project_id: &str,
    gate: &str,
    activity_key: &str,
    required_capabilities: &[String],
) -> Result<serde_json::Value, Error> {
    // 预览不落库：用临时 activity id。
    match resolve_inner(
        store,
        project_id,
        gate,
        activity_key,
        "preview",
        None,
        None,
        required_capabilities,
        false,
    ) {
        Ok(s) => Ok(serde_json::to_value(s).unwrap_or_default()),
        Err(e) => Ok(serde_json::json!({
            "error": e.to_string(),
            "resolved": false,
        })),
    }
}

pub fn get(store: &Store, selection_id: &str) -> Result<Selection, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT id, stage_activity_id, requested_profile_version_id, resolved_profile_version_id, source_scope, fallback_used, reason_code, candidate_report_json, selection_digest, created_at
             FROM agent_selections WHERE id=?1",
            [selection_id],
            |r| {
                Ok(Selection {
                    id: r.get(0)?,
                    stage_activity_id: r.get(1)?,
                    requested_profile_version_id: r.get(2)?,
                    resolved_profile_version_id: r.get(3)?,
                    source_scope: r.get(4)?,
                    fallback_used: r.get::<_, i64>(5)? != 0,
                    reason_code: r.get(6)?,
                    candidate_report: serde_json::from_str(&r.get::<_, String>(7)?)
                        .unwrap_or_default(),
                    selection_digest: r.get(8)?,
                    created_at: r.get(9)?,
                })
            },
        )
        .map_err(|_| Error::Message(format!("not_found: selection {selection_id}")))
    })
}

pub fn resolved_version(store: &Store, selection: &Selection) -> Result<ProfileVersion, Error> {
    get_version(store, &selection.resolved_profile_version_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile;
    use sg_store::Store;

    fn setup() -> Store {
        let dir =
            std::env::temp_dir().join(format!("sg-rt-{}-{}", std::process::id(), ids::new_id("t")));
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

    fn make_profile(store: &Store, name: &str, adapter: &str, caps: &[&str]) -> String {
        let p = profile::create_profile(store, Some("pj"), name, adapter).unwrap();
        let caps: Vec<String> = caps.iter().map(|s| s.to_string()).collect();
        profile::create_version(store, &p.id, "persona", "sop", &caps, "", "{}", "{}")
            .unwrap()
            .id
    }

    #[test]
    fn unbound_activity_falls_back_to_builtin_generic() {
        let s = setup();
        let sel = resolve(
            &s,
            &ResolveContext {
                project_id: "pj",
                gate: "development",
                activity_key: "code_analysis",
                stage_activity_id: "act_1",
                task_override_version_id: None,
                workitem_default_version_id: None,
                required_capabilities: &[],
                persist: true,
            },
        )
        .unwrap();
        assert_eq!(sel.source_scope, "builtin_generic");
        assert!(sel.fallback_used);
        assert_eq!(sel.reason_code, "no_binding");
        // resolved 是内置通用 profile。
        let version = resolved_version(&s, &sel).unwrap();
        let prof = profile::get_profile(&s, &version.profile_id).unwrap();
        assert_eq!(prof.name, "builtin-generic");
    }

    #[test]
    fn project_binding_wins_and_is_not_fallback() {
        let s = setup();
        let version = make_profile(&s, "前端分身", "local_harness", &["frontend"]);
        profile::set_binding(
            &s,
            Some("pj"),
            "development",
            "frontend",
            &version,
            "generic",
            0,
        )
        .unwrap();
        let sel = resolve(
            &s,
            &ResolveContext {
                project_id: "pj",
                gate: "development",
                activity_key: "frontend",
                stage_activity_id: "act_1",
                task_override_version_id: None,
                workitem_default_version_id: None,
                required_capabilities: &[],
                persist: true,
            },
        )
        .unwrap();
        assert_eq!(sel.source_scope, "project_binding");
        assert!(!sel.fallback_used);
        assert_eq!(sel.resolved_profile_version_id, version);
    }

    #[test]
    fn workitem_default_wins_over_bindings_but_not_task_override() {
        let s = setup();
        let assigned = make_profile(&s, "被指派分身", "local_harness", &[]);
        let bound = make_profile(&s, "绑定分身", "local_harness", &[]);
        let task_override = make_profile(&s, "关卡显式分身", "local_harness", &[]);
        profile::set_binding(
            &s,
            Some("pj"),
            "development",
            "frontend",
            &bound,
            "generic",
            0,
        )
        .unwrap();
        // 任务级默认（"@ 指派"）健康 → 压过项目绑定。
        let sel = resolve(
            &s,
            &ResolveContext {
                project_id: "pj",
                gate: "development",
                activity_key: "frontend",
                stage_activity_id: "act_1",
                task_override_version_id: None,
                workitem_default_version_id: Some(&assigned),
                required_capabilities: &[],
                persist: true,
            },
        )
        .unwrap();
        assert_eq!(sel.source_scope, "workitem_default");
        assert!(!sel.fallback_used);
        assert_eq!(sel.resolved_profile_version_id, assigned);
        assert_eq!(
            sel.requested_profile_version_id.as_deref(),
            Some(assigned.as_str())
        );
        // 任务显式覆盖（关卡级）> 任务默认。
        let sel = resolve(
            &s,
            &ResolveContext {
                project_id: "pj",
                gate: "development",
                activity_key: "frontend",
                stage_activity_id: "act_2",
                task_override_version_id: Some(&task_override),
                workitem_default_version_id: Some(&assigned),
                required_capabilities: &[],
                persist: true,
            },
        )
        .unwrap();
        assert_eq!(sel.source_scope, "task_override");
        assert_eq!(sel.resolved_profile_version_id, task_override);
    }

    #[test]
    fn workitem_default_unhealthy_falls_through_to_bindings() {
        let s = setup();
        let assigned_profile =
            profile::create_profile(&s, Some("pj"), "指派但禁用", "local_harness").unwrap();
        profile::set_profile_enabled(&s, &assigned_profile.id, false).unwrap();
        let assigned = profile::create_version(
            &s,
            &assigned_profile.id,
            "persona",
            "sop",
            &[],
            "",
            "{}",
            "{}",
        )
        .unwrap()
        .id;
        let bound = make_profile(&s, "绑定分身", "local_harness", &[]);
        profile::set_binding(
            &s,
            Some("pj"),
            "development",
            "frontend",
            &bound,
            "generic",
            0,
        )
        .unwrap();
        // 指派是偏好不是硬约束：候选不可用 → 记报告后继续绑定链（不拒启）。
        let sel = resolve(
            &s,
            &ResolveContext {
                project_id: "pj",
                gate: "development",
                activity_key: "frontend",
                stage_activity_id: "act_1",
                task_override_version_id: None,
                workitem_default_version_id: Some(&assigned),
                required_capabilities: &[],
                persist: true,
            },
        )
        .unwrap();
        assert_eq!(sel.source_scope, "project_binding");
        assert_eq!(sel.resolved_profile_version_id, bound);
        assert!(
            sel.candidate_report
                .to_string()
                .contains("workitem_default"),
            "失败候选应留痕：{}",
            sel.candidate_report
        );
    }

    #[test]
    fn unhealthy_external_agent_falls_back_with_reason() {
        let s = setup();
        // external_agent 且未配置模型 → unhealthy → 回退通用（SG-AGT-004）。
        let version = make_profile(&s, "外部分身", "external_agent", &[]);
        profile::set_binding(
            &s,
            Some("pj"),
            "testing",
            "e2e_testing",
            &version,
            "generic",
            0,
        )
        .unwrap();
        let sel = resolve(
            &s,
            &ResolveContext {
                project_id: "pj",
                gate: "testing",
                activity_key: "e2e_testing",
                stage_activity_id: "act_2",
                task_override_version_id: None,
                workitem_default_version_id: None,
                required_capabilities: &[],
                persist: true,
            },
        )
        .unwrap();
        assert_eq!(sel.source_scope, "builtin_generic");
        assert!(sel.fallback_used);
        assert!(
            sel.reason_code.contains("adapter_unhealthy"),
            "{}",
            sel.reason_code
        );
        // 候选报告记录失败原因。
        let failed = sel
            .candidate_report
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["ok"] == serde_json::json!(false))
            .unwrap();
        assert_eq!(failed["scope"], serde_json::json!("project_binding"));
    }

    #[test]
    fn fail_closed_rejects_when_specialist_unavailable() {
        let s = setup();
        let version = make_profile(&s, "外部分身FC", "external_agent", &[]);
        profile::set_binding(
            &s,
            Some("pj"),
            "testing",
            "e2e_testing",
            &version,
            "fail_closed",
            0,
        )
        .unwrap();
        let err = resolve(
            &s,
            &ResolveContext {
                project_id: "pj",
                gate: "testing",
                activity_key: "e2e_testing",
                stage_activity_id: "act_3",
                task_override_version_id: None,
                workitem_default_version_id: None,
                required_capabilities: &[],
                persist: true,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("agent_profile_unavailable"));
    }

    #[test]
    fn capability_mismatch_falls_back_and_records_reason() {
        let s = setup();
        let version = make_profile(&s, "仅前端", "local_harness", &["frontend"]);
        profile::set_binding(
            &s,
            Some("pj"),
            "development",
            "backend",
            &version,
            "generic",
            0,
        )
        .unwrap();
        let sel = resolve(
            &s,
            &ResolveContext {
                project_id: "pj",
                gate: "development",
                activity_key: "backend",
                stage_activity_id: "act_4",
                task_override_version_id: None,
                workitem_default_version_id: None,
                required_capabilities: &["backend".to_string()],
                persist: true,
            },
        )
        .unwrap();
        assert_eq!(sel.source_scope, "builtin_generic");
        assert!(
            sel.reason_code.contains("capability_mismatch:backend"),
            "{}",
            sel.reason_code
        );
    }

    #[test]
    fn task_override_wins_over_binding() {
        let s = setup();
        let bound = make_profile(&s, "绑定版", "local_harness", &[]);
        let over = make_profile(&s, "覆盖版", "local_harness", &[]);
        profile::set_binding(
            &s,
            Some("pj"),
            "development",
            "frontend",
            &bound,
            "generic",
            0,
        )
        .unwrap();
        let sel = resolve(
            &s,
            &ResolveContext {
                project_id: "pj",
                gate: "development",
                activity_key: "frontend",
                stage_activity_id: "act_5",
                task_override_version_id: Some(&over),
                workitem_default_version_id: None,
                required_capabilities: &[],
                persist: true,
            },
        )
        .unwrap();
        assert_eq!(sel.source_scope, "task_override");
        assert_eq!(sel.resolved_profile_version_id, over);
        assert!(!sel.fallback_used);
    }

    #[test]
    fn resolve_preview_writes_nothing() {
        let s = setup();
        let version = make_profile(&s, "预览绑定", "local_harness", &[]);
        profile::set_binding(
            &s,
            Some("pj"),
            "design",
            "technical_design",
            &version,
            "generic",
            0,
        )
        .unwrap();
        let view = resolve_preview(&s, "pj", "design", "technical_design", &[]).unwrap();
        assert_eq!(view["source_scope"], serde_json::json!("project_binding"));
        let count: i64 = s
            .with_conn(|c| {
                c.query_row("SELECT COUNT(*) FROM agent_selections", [], |r| r.get(0))
                    .map_err(Error::from)
            })
            .unwrap();
        assert_eq!(count, 0, "预览不落库");
    }
}
