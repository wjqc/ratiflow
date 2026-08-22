//! 设置域 RPC 接线（薄适配：校验参数 → service → 映射错误/事件）。
use serde_json::{json, Value};
use sg_protocol::{ErrorCode, RpcError};
use sg_settings as settings;

use crate::state::AppState;

type R = Result<Value, RpcError>;

fn serr(e: settings::SettingsError) -> RpcError {
    let kind = match e.code {
        "REVISION_CONFLICT" => ErrorCode::Conflict,
        "MANAGED_READ_ONLY" => ErrorCode::Forbidden,
        "CREDENTIAL_MISSING"
        | "CREDENTIAL_EXPIRED"
        | "CREDENTIAL_AUTH_FAILED"
        | "CREDENTIAL_STORE_UNAVAILABLE" => ErrorCode::Unauthorized,
        "BACKUP_INCOMPATIBLE" | "BACKUP_CORRUPT" | "BACKUP_VERIFY_FAILED" => ErrorCode::Conflict,
        "NOT_FOUND" => ErrorCode::NotFound,
        "INVALID_PARAMS" => ErrorCode::InvalidParams,
        _ => ErrorCode::InternalError,
    };
    let mut err = RpcError::new(kind, e.message);
    err.data = e.details.or(Some(json!({"fieldErrors": e.field_errors})));
    err.retryable = false;
    err
}

fn s<'a>(p: &'a Value, k: &str) -> Result<&'a str, RpcError> {
    p.get(k)
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::new(ErrorCode::InvalidParams, format!("缺少 {k}")))
}
fn opt_s<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(|v| v.as_str())
}
fn n(p: &Value, k: &str) -> Result<i64, RpcError> {
    p.get(k)
        .and_then(|v| v.as_i64())
        .ok_or_else(|| RpcError::new(ErrorCode::InvalidParams, format!("缺少 {k}")))
}

fn changed(state: &AppState, resource: &str, id: &str) {
    let _ = sg_store::outbox::emit(
        &state.store,
        resource,
        id,
        &format!("{resource}.changed"),
        json!({"id": id}),
    );
}

const PREFIXES: [&str; 16] = [
    "settings.",
    "modelProfile.",
    "modelRoute.",
    "toolPolicy.",
    "executionProfile.",
    "gitlabProfile.",
    "sshTarget.",
    "credentialRef.",
    "backup.",
    "audit.get",
    "audit.export",
    "logs.",
    "operation.",
    "knowledge.settings.",
    "knowledge.searchV2",
    "project.inspectRoot",
];

pub fn dispatch(state: &AppState, method: &str, p: &Value) -> Option<R> {
    if !PREFIXES.iter().any(|pfx| method.starts_with(pfx)) {
        return None;
    }
    Some(run(state, method, p))
}

fn run(state: &AppState, method: &str, p: &Value) -> R {
    match method {
        // --- 系统与设置 ---
        "settings.summary" => settings::settings::summary::aggregate(&state.store).map_err(serr),
        "settings.get" => {
            let keys: Option<Vec<String>> = p.get("keys").and_then(|v| v.as_array()).map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            });
            let items = settings::settings::get(
                &state.store,
                opt_s(p, "scope").unwrap_or("global"),
                opt_s(p, "projectId"),
                keys.as_deref(),
            )
            .map_err(serr)?;
            Ok(json!({"items": items}))
        }
        "settings.effective" => {
            let items = settings::settings::effective(&state.store, opt_s(p, "projectId"), None)
                .map_err(serr)?;
            Ok(json!({"items": items}))
        }
        "settings.update" => {
            let patches: Vec<settings::settings::Patch> = p
                .get("patches")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|item| {
                            Some(settings::settings::Patch {
                                key: item.get("key")?.as_str()?.to_string(),
                                value: item.get("value")?.clone(),
                                expected_revision: item
                                    .get("expectedRevision")
                                    .and_then(|v| v.as_i64()),
                            })
                        })
                        .collect()
                })
                .ok_or_else(|| RpcError::new(ErrorCode::InvalidParams, "patches 必填"))?;
            let updated = settings::settings::update(
                &state.store,
                opt_s(p, "scope").unwrap_or("global"),
                opt_s(p, "projectId"),
                &patches,
                "local",
            )
            .map_err(serr)?;
            changed(state, "settings", "app");
            Ok(json!({"items": updated}))
        }

        // --- 模型 ---
        "modelProfile.list" => {
            let v = settings::profiles::model_list(&state.store).map_err(serr)?;
            Ok(json!({"items": v}))
        }
        "modelProfile.get" => {
            let v =
                settings::profiles::model_get(&state.store, s(p, "profileId")?).map_err(serr)?;
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "modelProfile.create" => {
            let v = settings::profiles::model_create(&state.store, p).map_err(serr)?;
            changed(state, "modelProfile", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "modelProfile.update" => {
            let v = settings::profiles::model_update(
                &state.store,
                s(p, "profileId")?,
                p,
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            changed(state, "modelProfile", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "modelProfile.remove" => {
            settings::profiles::model_remove(
                &state.store,
                s(p, "profileId")?,
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            Ok(json!({"status": "removed"}))
        }
        "modelProfile.test" => {
            let profile =
                settings::profiles::model_get(&state.store, s(p, "profileId")?).map_err(serr)?;
            let api_key = match &profile.credential_ref_id {
                Some(rid) => Some(
                    settings::profiles::reveal_for(&state.store, state.credentials.as_ref(), rid)
                        .map_err(serr)?,
                ),
                None => std::env::var("SIXGATES_MODEL_API_KEY").ok(),
            };
            let report = sg_integrations::model_test(
                &profile.base_url,
                api_key.as_deref(),
                &profile.default_model,
                &profile.capabilities,
            );
            let status = if report.status == "ready" {
                "ready"
            } else {
                "error"
            };
            let _ = settings::profiles::model_mark_tested(&state.store, &profile.id, status)
                .map_err(serr);
            Ok(serde_json::to_value(&report).unwrap_or_default())
        }
        "modelProfile.syncModels" => {
            Ok(json!({"models": [], "note": "syncModels 留待 provider 模型列表接入（P1）"}))
        }
        "modelRoute.get" => settings::profiles::route_get(&state.store).map_err(serr),
        "modelRoute.update" => {
            let v = settings::profiles::route_update(
                &state.store,
                p.get("route").unwrap_or(p),
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            changed(state, "modelRoute", "route");
            Ok(v)
        }

        // --- 工具/执行 ---
        "toolPolicy.list" => {
            let v = settings::policy_ext::tool_list(&state.store).map_err(serr)?;
            Ok(json!({"items": v}))
        }
        "toolPolicy.update" => {
            let v = settings::policy_ext::tool_update(&state.store, p, n(p, "expectedRevision")?)
                .map_err(serr)?;
            changed(state, "policy", &v.tool_id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "toolPolicy.effective" => {
            let v = settings::policy_ext::tool_effective(&state.store, opt_s(p, "projectId"))
                .map_err(serr)?;
            Ok(json!({"items": v}))
        }
        "executionProfile.list" => {
            let v = settings::policy_ext::execution_list(&state.store).map_err(serr)?;
            Ok(json!({"items": v}))
        }
        "executionProfile.create" => {
            let v = settings::policy_ext::execution_create(
                &state.store,
                s(p, "name")?,
                s(p, "mode")?,
                p.get("limits").unwrap_or(&Value::Null),
            )
            .map_err(serr)?;
            changed(state, "executionProfile", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "executionProfile.update" => {
            let v = settings::policy_ext::execution_update(
                &state.store,
                s(p, "profileId")?,
                p,
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            changed(state, "executionProfile", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "executionProfile.remove" => {
            settings::policy_ext::execution_remove(
                &state.store,
                s(p, "profileId")?,
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            Ok(json!({"status": "removed"}))
        }

        // --- GitLab ---
        "gitlabProfile.list" => {
            let v = settings::profiles::gitlab_list(&state.store).map_err(serr)?;
            Ok(json!({"items": v}))
        }
        "gitlabProfile.create" => {
            let v = settings::profiles::gitlab_create(&state.store, p).map_err(serr)?;
            changed(state, "gitlabProfile", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "gitlabProfile.update" => {
            let v = settings::profiles::gitlab_update(
                &state.store,
                s(p, "profileId")?,
                p,
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            changed(state, "gitlabProfile", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "gitlabProfile.remove" => {
            settings::profiles::gitlab_remove(
                &state.store,
                s(p, "profileId")?,
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            Ok(json!({"status": "removed"}))
        }
        "gitlabProfile.test" => {
            let profile =
                settings::profiles::gitlab_get(&state.store, s(p, "profileId")?).map_err(serr)?;
            let token = match &profile.credential_ref_id {
                Some(rid) => Some(
                    settings::profiles::reveal_for(&state.store, state.credentials.as_ref(), rid)
                        .map_err(serr)?,
                ),
                None => std::env::var("SIXGATES_GITLAB_TOKEN").ok(),
            };
            let report = sg_integrations::gitlab_test(&profile.base_url, token.as_deref());
            let status = if report.status == "ready" {
                "ready"
            } else {
                "degraded"
            };
            let _ = settings::profiles::gitlab_update_status(&state.store, &profile.id, status);
            Ok(serde_json::to_value(&report).unwrap_or_default())
        }
        "gitlabProfile.capabilities" => {
            let _profile =
                settings::profiles::gitlab_get(&state.store, s(p, "profileId")?).map_err(serr)?;
            Ok(
                json!({"issueReadWrite": true, "ciRead": true, "registryRead": true, "note": "能力探测以 test 步骤为准"}),
            )
        }

        // --- SSH ---
        "sshTarget.list" => {
            let v = settings::profiles::ssh_list(&state.store).map_err(serr)?;
            Ok(json!({"items": v}))
        }
        "sshTarget.get" => {
            let v = settings::profiles::ssh_get(&state.store, s(p, "targetId")?).map_err(serr)?;
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "sshTarget.create" => {
            let v = settings::profiles::ssh_create(&state.store, p).map_err(serr)?;
            changed(state, "sshTarget", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "sshTarget.update" => {
            let v = settings::profiles::ssh_update(
                &state.store,
                s(p, "targetId")?,
                p,
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            changed(state, "sshTarget", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "sshTarget.remove" => {
            settings::profiles::ssh_remove(
                &state.store,
                s(p, "targetId")?,
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            Ok(json!({"status": "removed"}))
        }
        "sshTarget.test" => {
            let target =
                settings::profiles::ssh_get(&state.store, s(p, "targetId")?).map_err(serr)?;
            let report = sg_integrations::ssh_target_test(
                &target.host,
                target.port,
                &target.username,
                &target.remote_dir,
                &target.fingerprint,
            );
            let status = match report.status.as_str() {
                "ready" => "ready",
                "action_required" => "configured",
                _ => "error",
            };
            let _ = settings::profiles::ssh_mark_tested(&state.store, &target.id, status);
            Ok(serde_json::to_value(&report).unwrap_or_default())
        }
        "sshTarget.acceptHostKey" => {
            let v = settings::profiles::ssh_accept_host_key(
                &state.store,
                s(p, "targetId")?,
                s(p, "fingerprint")?,
            )
            .map_err(serr)?;
            changed(state, "sshTarget", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }

        // --- 凭据 ---
        "credentialRef.list" => {
            let v = settings::credentials::list(&state.store).map_err(serr)?;
            Ok(json!({"items": v}))
        }
        "credentialRef.create" => {
            let secret = p
                .get("secret")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let v = settings::credentials::create(
                &state.store,
                state.credentials.as_ref(),
                s(p, "name")?,
                s(p, "kind")?,
                opt_s(p, "provider").unwrap_or(""),
                &secret,
                opt_s(p, "projectId"),
            )
            .map_err(serr)?;
            let _ = settings::audit_ext::append(
                &state.store,
                &settings::audit_ext::AuditEvent {
                    actor: "local",
                    actor_kind: "user",
                    action: "credentialRef.create",
                    target_type: "credential_ref",
                    target_id: &v.id,
                    result: "success",
                    correlation_id: None,
                    project_id: None,
                    before_summary: None,
                    after_summary: Some(&json!({"hasSecret": true})),
                },
            );
            changed(state, "credentialRef", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "credentialRef.replace" => {
            let secret = p
                .get("secret")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let id = s(p, "refId")?.to_string();
            let v = settings::credentials::replace(
                &state.store,
                state.credentials.as_ref(),
                &id,
                &secret,
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            changed(state, "credentialRef", &id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "credentialRef.remove" => {
            settings::credentials::remove(
                &state.store,
                state.credentials.as_ref(),
                s(p, "refId")?,
                n(p, "expectedRevision")?,
                p.get("force").and_then(|v| v.as_bool()).unwrap_or(false),
            )
            .map_err(serr)?;
            Ok(json!({"status": "removed"}))
        }
        "credentialRef.verify" => {
            let v = settings::credentials::verify(
                &state.store,
                state.credentials.as_ref(),
                s(p, "refId")?,
            )
            .map_err(serr)?;
            Ok(serde_json::to_value(v).unwrap_or_default())
        }

        // --- 备份 ---
        "backup.list" => {
            let v = settings::backup_ext::list(&state.store).map_err(serr)?;
            Ok(json!({"items": v}))
        }
        "backup.create" => {
            let op =
                settings::operations::begin(&state.store, "backup.create", false).map_err(serr)?;
            let _ = settings::operations::progress(
                &state.store,
                &op.operation_id,
                1,
                3,
                "backup.step.snapshot",
            );
            let snap = sg_store::backup::snapshot(&state.store)
                .map_err(|e| serr(settings::SettingsError::new("INTERNAL", e.to_string())))?;
            let _ = settings::operations::progress(
                &state.store,
                &op.operation_id,
                2,
                3,
                "backup.step.register",
            );
            let record = settings::backup_ext::register(&state.store, &snap, 1).map_err(serr)?;
            let _ = settings::operations::progress(
                &state.store,
                &op.operation_id,
                3,
                3,
                "backup.step.done",
            );
            let _ = settings::operations::finish(
                &state.store,
                &op.operation_id,
                "succeeded",
                json!({"backupId": record.id}),
            );
            changed(state, "backup", &record.id);
            Ok(serde_json::to_value(record).unwrap_or_default())
        }
        "backup.verify" => {
            let v = settings::backup_ext::verify(&state.store, s(p, "backupId")?).map_err(serr)?;
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "backup.restore" => {
            let outcome =
                settings::backup_ext::restore(&state.store, s(p, "backupId")?).map_err(serr)?;
            Ok(
                json!({"restored": outcome.restored, "requiresRestart": outcome.requires_restart, "safetySnapshot": outcome.safety_snapshot}),
            )
        }
        "backup.delete" => {
            settings::backup_ext::delete(&state.store, s(p, "backupId")?).map_err(serr)?;
            Ok(json!({"status": "deleted"}))
        }

        // --- 审计/日志 ---
        "audit.get" => {
            let v = settings::audit_ext::get(
                &state.store,
                p.get("entryId")
                    .and_then(|v| v.as_i64())
                    .or_else(|| {
                        p.get("entryId")
                            .and_then(|v| v.as_str())
                            .and_then(|s| s.parse().ok())
                    })
                    .ok_or_else(|| RpcError::new(ErrorCode::InvalidParams, "entryId 必填"))?,
            )
            .map_err(serr)?;
            Ok(json!({"entry": v}))
        }
        "audit.export" => settings::audit_ext::export(
            &state.store,
            p.get("filters").unwrap_or(&json!({})),
            p.get("limit").and_then(|v| v.as_i64()).unwrap_or(200),
        )
        .map_err(serr),
        "logs.list" => {
            settings::backup_ext::logs_list(p.get("limit").and_then(|v| v.as_i64()).unwrap_or(50))
                .map_err(serr)
        }
        "logs.exportDiagnosticBundle" => {
            settings::backup_ext::export_diagnostic_bundle(&state.store).map_err(serr)
        }

        // --- 长操作 ---
        "operation.get" => {
            let v = settings::operations::get(&state.store, s(p, "operationId")?).map_err(serr)?;
            Ok(serde_json::to_value(v).unwrap_or_default())
        }

        // --- 知识默认 / 检索 v2 / 项目根 ---
        "knowledge.settings.get" => {
            settings::knowledge_defaults::get(&state.store, opt_s(p, "projectId")).map_err(serr)
        }
        "knowledge.settings.update" => {
            let revision = settings::knowledge_defaults::update(
                &state.store,
                opt_s(p, "projectId"),
                p.get("settings").unwrap_or(&json!({})),
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            changed(state, "knowledge", "defaults");
            Ok(json!({"revision": revision}))
        }
        "knowledge.searchV2" => sg_knowledge::search_v2(
            &state.store,
            s(p, "projectId")?,
            s(p, "query")?,
            p.get("includeTests")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            p.get("limit").and_then(|v| v.as_i64()).unwrap_or(20),
        )
        .map_err(|e| RpcError::new(ErrorCode::InternalError, e.to_string())),
        "project.inspectRoot" => sg_project::inspect_root(s(p, "path")?)
            .map_err(|e| RpcError::new(ErrorCode::InvalidParams, e.to_string())),

        _ => Err(RpcError::new(
            ErrorCode::MethodNotFound,
            format!("unknown {method}"),
        )),
    }
}
