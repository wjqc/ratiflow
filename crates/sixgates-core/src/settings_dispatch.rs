//! 设置域 RPC 接线（薄适配：校验参数 → service → 映射错误/事件）。
use serde_json::{json, Value};
use sg_integrations::GitLabClient as _;
use sg_protocol::{ErrorCode, RpcError};
use sg_settings as settings;
use sg_store::Store;

use crate::state::AppState;

type R = Result<Value, RpcError>;

pub(crate) fn serr(e: settings::SettingsError) -> RpcError {
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
    let mut err = RpcError::new(kind, e.message.clone());
    // data.detail 携带人类可读原因（RPC 协议约定）；details/fieldErrors 并存时合入同一对象。
    err.data = Some(match e.details {
        Some(mut d) => {
            if let Some(obj) = d.as_object_mut() {
                obj.insert("detail".into(), json!(e.message));
            }
            d
        }
        None => json!({"detail": e.message, "fieldErrors": e.field_errors}),
    });
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

fn changed(store: &Store, resource: &str, id: &str) {
    let _ = sg_store::outbox::emit(
        store,
        resource,
        id,
        &format!("{resource}.changed"),
        json!({"id": id}),
    );
}

/// 直填秘密自动落 Keychain：secret 与 credentialRefId 二选一；
/// 命中直填时创建凭据引用并回填 params，随后从 params 移除明文字段（不落库不入日志）。
fn bind_inline_secret(
    state: &AppState,
    store: &Store,
    params: &mut Value,
    secret_key: &str,
    cred_name: &str,
    kind: &str,
    provider: &str,
) -> Result<(), RpcError> {
    let secret = opt_s(params, secret_key)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let has_ref = opt_s(params, "credentialRefId")
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .is_some();
    if let Some(secret) = secret {
        if has_ref {
            return Err(RpcError::new(
                ErrorCode::InvalidParams,
                "秘密值与 credentialRefId 二选一，不可同时提供",
            ));
        }
        let cred = settings::credentials::create(
            store,
            state.credentials.as_ref(),
            cred_name,
            kind,
            provider,
            secret,
            None,
        )
        .map_err(serr)?;
        params["credentialRefId"] = json!(cred.id);
        changed(store, "credentialRef", &cred.id);
    }
    if let Some(obj) = params.as_object_mut() {
        obj.remove(secret_key);
    }
    Ok(())
}

const PREFIXES: [&str; 29] = [
    "skill.",
    "settings.",
    "modelProvider.",
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
    "update.",
    "executor.",
    "diagnostics.run",
    "sshTarget.bindProject",
    "gitlabProfile.currentUser",
    "gitlabProfile.checkProjectPermissions",
    "knowledge.projectSettings",
    "backup.revealInFolder",
    "audit.settings",
    "tool.list",
    "tool.test",
];

pub fn dispatch(state: &AppState, store: &Store, method: &str, p: &Value) -> Option<R> {
    if !PREFIXES.iter().any(|pfx| method.starts_with(pfx)) {
        return None;
    }
    Some(run(state, store, method, p))
}

fn run(state: &AppState, store: &Store, method: &str, p: &Value) -> R {
    match method {
        // --- 系统与设置 ---
        "settings.summary" => settings::settings::summary::aggregate(store).map_err(serr),
        "settings.get" => {
            let keys: Option<Vec<String>> = p.get("keys").and_then(|v| v.as_array()).map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            });
            let items = settings::settings::get(
                store,
                opt_s(p, "scope").unwrap_or("global"),
                opt_s(p, "projectId"),
                keys.as_deref(),
            )
            .map_err(serr)?;
            Ok(json!({"items": items}))
        }
        "settings.effective" => {
            let items =
                settings::settings::effective(store, opt_s(p, "projectId"), None).map_err(serr)?;
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
                store,
                opt_s(p, "scope").unwrap_or("global"),
                opt_s(p, "projectId"),
                &patches,
                "local",
            )
            .map_err(serr)?;
            changed(store, "settings", "app");
            Ok(json!({"items": updated}))
        }

        // --- 模型 ---
        "modelProvider.presets" => {
            Ok(json!({"items": settings::profiles::provider_presets_json()}))
        }
        "modelProfile.list" => {
            let v = settings::profiles::model_list(store).map_err(serr)?;
            Ok(json!({"items": v}))
        }
        "modelProfile.get" => {
            let v = settings::profiles::model_get(store, s(p, "profileId")?).map_err(serr)?;
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "modelProfile.create" => {
            // apiKey 直填：先落 Keychain（kind=model_api_key），DB 只存凭据引用 ID。
            let mut params = p.clone();
            let inline_key = opt_s(p, "apiKey").map(str::trim).filter(|k| !k.is_empty());
            let has_ref = opt_s(p, "credentialRefId")
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .is_some();
            if let Some(key) = inline_key {
                if has_ref {
                    return Err(RpcError::new(
                        ErrorCode::InvalidParams,
                        "apiKey 与 credentialRefId 二选一，不可同时提供",
                    ));
                }
                let cred = settings::credentials::create(
                    store,
                    state.credentials.as_ref(),
                    &format!(
                        "{} API Key",
                        opt_s(p, "name").unwrap_or("模型供应商").trim()
                    ),
                    "model_api_key",
                    opt_s(p, "providerKind").unwrap_or("openai_compatible"),
                    key,
                    None,
                )
                .map_err(serr)?;
                params["credentialRefId"] = json!(cred.id);
                changed(store, "credentialRef", &cred.id);
            }
            if let Some(obj) = params.as_object_mut() {
                obj.remove("apiKey");
            }
            let v = settings::profiles::model_create(store, &params).map_err(serr)?;
            changed(store, "modelProfile", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "modelProfile.update" => {
            let v = settings::profiles::model_update(
                store,
                s(p, "profileId")?,
                p,
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            changed(store, "modelProfile", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "modelProfile.remove" => {
            settings::profiles::model_remove(store, s(p, "profileId")?, n(p, "expectedRevision")?)
                .map_err(serr)?;
            Ok(json!({"status": "removed"}))
        }
        "modelProfile.test" => {
            let profile = settings::profiles::model_get(store, s(p, "profileId")?).map_err(serr)?;
            let api_key = match &profile.credential_ref_id {
                Some(rid) => Some(
                    settings::profiles::reveal_for(store, state.credentials.as_ref(), rid)
                        .map_err(serr)?,
                ),
                None => std::env::var("SIXGATES_MODEL_API_KEY").ok(),
            };
            let mut report = sg_integrations::model_test(
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
            let _ = settings::profiles::model_mark_tested(store, &profile.id, status).map_err(serr);
            // ADR-033：真实探测通过 → 写入 probe 来源能力快照（source/verifiedAt/expiresAt/digest）。
            if status == "ready" {
                if let Ok(snapshot) =
                    settings::profiles::model_record_capability(store, &profile.id)
                {
                    if let Ok(cap) = serde_json::to_value(&snapshot) {
                        report.capability_snapshot = Some(cap);
                    }
                }
            }
            Ok(serde_json::to_value(&report).unwrap_or_default())
        }
        "modelProfile.syncModels" => {
            let profile = settings::profiles::model_get(store, s(p, "profileId")?).map_err(serr)?;
            let api_key = match &profile.credential_ref_id {
                Some(rid) => Some(
                    settings::profiles::reveal_for(store, state.credentials.as_ref(), rid)
                        .map_err(serr)?,
                ),
                None => std::env::var("SIXGATES_MODEL_API_KEY").ok(),
            };
            let api_key = api_key
                .filter(|k| !k.is_empty())
                .ok_or_else(|| RpcError::new(ErrorCode::Unauthorized, "未配置 API Key"))?;
            let base =
                settings::profiles::resolve_base_url(&profile.provider_kind, &profile.base_url);
            if base.is_empty() {
                return Err(RpcError::new(
                    ErrorCode::InvalidParams,
                    "Profile 未配置 Base URL，无法同步模型列表",
                ));
            }
            let models = sg_integrations::list_models(&base, &api_key)
                .map_err(|e| RpcError::new(ErrorCode::InternalError, e))?;
            // 持久化到 models_json：设置页与工作台模型选择器重启后仍可用。
            settings::profiles::model_set_models(store, &profile.id, &models).map_err(serr)?;
            changed(store, "modelProfile", &profile.id);
            Ok(json!({"models": models}))
        }
        "modelRoute.get" => settings::profiles::route_get(store).map_err(serr),
        "modelRoute.update" => {
            let v = settings::profiles::route_update(
                store,
                p.get("route").unwrap_or(p),
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            changed(store, "modelRoute", "route");
            Ok(v)
        }

        // --- 工具/执行 ---
        "toolPolicy.list" => {
            let v = settings::policy_ext::tool_list(store).map_err(serr)?;
            Ok(json!({"items": v}))
        }
        "toolPolicy.update" => {
            let v = settings::policy_ext::tool_update(store, p, n(p, "expectedRevision")?)
                .map_err(serr)?;
            changed(store, "policy", &v.tool_id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "toolPolicy.effective" => {
            let v =
                settings::policy_ext::tool_effective(store, opt_s(p, "projectId")).map_err(serr)?;
            Ok(json!({"items": v}))
        }
        "executionProfile.list" => {
            let v = settings::policy_ext::execution_list(store).map_err(serr)?;
            Ok(json!({"items": v}))
        }
        "executionProfile.create" => {
            let v = settings::policy_ext::execution_create(
                store,
                s(p, "name")?,
                s(p, "mode")?,
                p.get("limits").unwrap_or(&Value::Null),
            )
            .map_err(serr)?;
            changed(store, "executionProfile", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "executionProfile.update" => {
            let v = settings::policy_ext::execution_update(
                store,
                s(p, "profileId")?,
                p,
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            changed(store, "executionProfile", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "executionProfile.remove" => {
            settings::policy_ext::execution_remove(
                store,
                s(p, "profileId")?,
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            Ok(json!({"status": "removed"}))
        }

        // --- GitLab ---
        "gitlabProfile.list" => {
            let v = settings::profiles::gitlab_list(store).map_err(serr)?;
            Ok(json!({"items": v}))
        }
        "gitlabProfile.create" => {
            // token 直填：先落 Keychain（kind=gitlab_token），DB 只存凭据引用 ID。
            let mut params = p.clone();
            let provider = opt_s(&params, "baseUrl").unwrap_or("gitlab").to_string();
            let cred_name = format!(
                "{} GitLab Token",
                opt_s(&params, "name").unwrap_or("GitLab 实例").trim()
            );
            bind_inline_secret(state, store, &mut params, "token", &cred_name, "gitlab_token", &provider)?;
            let v = settings::profiles::gitlab_create(store, &params).map_err(serr)?;
            changed(store, "gitlabProfile", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "gitlabProfile.update" => {
            let v = settings::profiles::gitlab_update(
                store,
                s(p, "profileId")?,
                p,
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            changed(store, "gitlabProfile", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "gitlabProfile.remove" => {
            settings::profiles::gitlab_remove(store, s(p, "profileId")?, n(p, "expectedRevision")?)
                .map_err(serr)?;
            Ok(json!({"status": "removed"}))
        }
        "gitlabProfile.test" => {
            let profile =
                settings::profiles::gitlab_get(store, s(p, "profileId")?).map_err(serr)?;
            let token = match &profile.credential_ref_id {
                Some(rid) => Some(
                    settings::profiles::reveal_for(store, state.credentials.as_ref(), rid)
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
            let _ = settings::profiles::gitlab_update_status(store, &profile.id, status);
            Ok(serde_json::to_value(&report).unwrap_or_default())
        }
        "gitlabProfile.capabilities" => {
            let _profile =
                settings::profiles::gitlab_get(store, s(p, "profileId")?).map_err(serr)?;
            Ok(
                json!({"issueReadWrite": true, "ciRead": true, "registryRead": true, "note": "能力探测以 test 步骤为准"}),
            )
        }

        // --- SSH ---
        "sshTarget.list" => {
            let v = settings::profiles::ssh_list(store).map_err(serr)?;
            Ok(json!({"items": v}))
        }
        "sshTarget.get" => {
            let v = settings::profiles::ssh_get(store, s(p, "targetId")?).map_err(serr)?;
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "sshTarget.create" => {
            // secret 直填（密码或私钥）：先落 Keychain（kind=ssh_key），DB 只存凭据引用 ID。
            let mut params = p.clone();
            let provider = opt_s(&params, "host").unwrap_or("ssh").to_string();
            let cred_name = format!(
                "{} SSH 凭证",
                opt_s(&params, "name")
                    .or_else(|| opt_s(&params, "host"))
                    .unwrap_or("SSH 目标机")
                    .trim()
            );
            bind_inline_secret(state, store, &mut params, "secret", &cred_name, "ssh_key", &provider)?;
            let v = settings::profiles::ssh_create(store, &params).map_err(serr)?;
            changed(store, "sshTarget", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "sshTarget.update" => {
            let v = settings::profiles::ssh_update(
                store,
                s(p, "targetId")?,
                p,
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            changed(store, "sshTarget", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "sshTarget.remove" => {
            settings::profiles::ssh_remove(store, s(p, "targetId")?, n(p, "expectedRevision")?)
                .map_err(serr)?;
            Ok(json!({"status": "removed"}))
        }
        "sshTarget.test" => {
            let target = settings::profiles::ssh_get(store, s(p, "targetId")?).map_err(serr)?;
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
            let _ = settings::profiles::ssh_mark_tested(store, &target.id, status);
            Ok(serde_json::to_value(&report).unwrap_or_default())
        }
        "sshTarget.acceptHostKey" => {
            let v = settings::profiles::ssh_accept_host_key(
                store,
                s(p, "targetId")?,
                s(p, "fingerprint")?,
            )
            .map_err(serr)?;
            changed(store, "sshTarget", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }

        // --- 凭据 ---
        "credentialRef.list" => {
            let v = settings::credentials::list(store).map_err(serr)?;
            Ok(json!({"items": v}))
        }
        "credentialRef.create" => {
            let secret = p
                .get("secret")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let v = settings::credentials::create(
                store,
                state.credentials.as_ref(),
                s(p, "name")?,
                s(p, "kind")?,
                opt_s(p, "provider").unwrap_or(""),
                &secret,
                opt_s(p, "projectId"),
            )
            .map_err(serr)?;
            let _ = settings::audit_ext::append(
                store,
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
            changed(store, "credentialRef", &v.id);
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
                store,
                state.credentials.as_ref(),
                &id,
                &secret,
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            changed(store, "credentialRef", &id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "credentialRef.remove" => {
            settings::credentials::remove(
                store,
                state.credentials.as_ref(),
                s(p, "refId")?,
                n(p, "expectedRevision")?,
                p.get("force").and_then(|v| v.as_bool()).unwrap_or(false),
            )
            .map_err(serr)?;
            Ok(json!({"status": "removed"}))
        }
        "credentialRef.verify" => {
            let v =
                settings::credentials::verify(store, state.credentials.as_ref(), s(p, "refId")?)
                    .map_err(serr)?;
            Ok(serde_json::to_value(v).unwrap_or_default())
        }

        // --- 备份 ---
        "backup.list" => {
            let v = settings::backup_ext::list(store).map_err(serr)?;
            Ok(json!({"items": v}))
        }
        "skill.list" => {
            let v = settings::skills_ext::list(store).map_err(serr)?;
            Ok(json!({"items": v}))
        }
        "skill.get" => {
            let v = settings::skills_ext::get(store, s(p, "skillId")?).map_err(serr)?;
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "skill.body" => {
            let body = settings::skills_ext::body(store, s(p, "skillId")?).map_err(serr)?;
            Ok(json!({"body": body}))
        }
        "skill.create" => {
            let name = s(p, "name")?;
            let description = opt_s(p, "description").unwrap_or_default();
            let body = s(p, "body")?;
            let source = opt_s(p, "source").unwrap_or("manual");
            let v = settings::skills_ext::create(store, name, description, body, source)
                .map_err(serr)?;
            changed(store, "skill", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "skill.update" => {
            let skill_id = s(p, "skillId")?;
            let description = opt_s(p, "description");
            let body = opt_s(p, "body");
            let v = settings::skills_ext::update(
                store,
                skill_id,
                description,
                body,
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            changed(store, "skill", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "skill.setEnabled" => {
            let skill_id = s(p, "skillId")?;
            let enabled = p
                .get("enabled")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| RpcError::new(ErrorCode::InvalidParams, "enabled required"))?;
            let v = settings::skills_ext::set_enabled(
                store,
                skill_id,
                enabled,
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            changed(store, "skill", &v.id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "skill.remove" => {
            settings::skills_ext::remove(store, s(p, "skillId")?, n(p, "expectedRevision")?)
                .map_err(serr)?;
            Ok(json!({"status": "removed"}))
        }
        "backup.create" => {
            let op = settings::operations::begin(store, "backup.create", false).map_err(serr)?;
            let _ = settings::operations::progress(
                store,
                &op.operation_id,
                1,
                3,
                "backup.step.snapshot",
            );
            let snap = sg_store::backup::snapshot(store)
                .map_err(|e| serr(settings::SettingsError::new("INTERNAL", e.to_string())))?;
            let _ = settings::operations::progress(
                store,
                &op.operation_id,
                2,
                3,
                "backup.step.register",
            );
            let record = settings::backup_ext::register(store, &snap, 1).map_err(serr)?;
            let _ =
                settings::operations::progress(store, &op.operation_id, 3, 3, "backup.step.done");
            let _ = settings::operations::finish(
                store,
                &op.operation_id,
                "succeeded",
                json!({"backupId": record.id}),
            );
            changed(store, "backup", &record.id);
            Ok(serde_json::to_value(record).unwrap_or_default())
        }
        "backup.verify" => {
            let v = settings::backup_ext::verify(store, s(p, "backupId")?).map_err(serr)?;
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "backup.restore" => {
            let outcome = settings::backup_ext::restore(store, s(p, "backupId")?).map_err(serr)?;
            Ok(
                json!({"restored": outcome.restored, "requiresRestart": outcome.requires_restart, "safetySnapshot": outcome.safety_snapshot}),
            )
        }
        "backup.delete" => {
            settings::backup_ext::delete(store, s(p, "backupId")?).map_err(serr)?;
            Ok(json!({"status": "deleted"}))
        }

        // --- 审计/日志 ---
        "audit.get" => {
            let v = settings::audit_ext::get(
                store,
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
            store,
            p.get("filters").unwrap_or(&json!({})),
            p.get("limit").and_then(|v| v.as_i64()).unwrap_or(200),
        )
        .map_err(serr),
        "logs.list" => {
            settings::backup_ext::logs_list(p.get("limit").and_then(|v| v.as_i64()).unwrap_or(50))
                .map_err(serr)
        }
        "logs.exportDiagnosticBundle" => {
            settings::backup_ext::export_diagnostic_bundle(store).map_err(serr)
        }

        // --- 长操作 ---
        "operation.get" => {
            let v = settings::operations::get(store, s(p, "operationId")?).map_err(serr)?;
            Ok(serde_json::to_value(v).unwrap_or_default())
        }

        // --- 知识默认 / 检索 v2 / 项目根 ---
        "knowledge.settings.get" => {
            settings::knowledge_defaults::get(store, opt_s(p, "projectId")).map_err(serr)
        }
        "knowledge.settings.update" => {
            let revision = settings::knowledge_defaults::update(
                store,
                opt_s(p, "projectId"),
                p.get("settings").unwrap_or(&json!({})),
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            changed(store, "knowledge", "defaults");
            Ok(json!({"revision": revision}))
        }
        "knowledge.searchV2" => sg_knowledge::search_v2(
            store,
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

        // --- 更新（Electron main 执行；core 报告通道与版本状态） ---
        "update.check" => Ok(json!({
            "channel": "stable", "autoCheck": true, "autoDownload": false,
            "currentVersion": state.version_str(), "latestVersion": state.version_str(),
            "updateAvailable": false,
            "note": "更新执行由 Electron main（autoUpdater）负责"
        })),
        "update.status" => Ok(json!({
            "desktop": state.version_str(), "core": state.version_str(),
            "protocol": sg_protocol::PROTOCOL_VERSION,
            "schema": store.schema_version().map_err(|e| RpcError::new(ErrorCode::InternalError, e.to_string()))?,
            "signatureVerified": false,
            "note": "签名验证状态在发布通道启用后报告"
        })),

        // --- 执行沙箱设置 + 自检（S22） ---
        "executor.settings.get" => {
            let mut v = settings::executor_ext::get(store).map_err(serr)?;
            // F10/M3：附生效模式与来源（env > 设置 > 探测）。
            let (mode, source) = crate::dispatch::effective_executor_mode(state, store);
            v["effective"] = json!({
                "mode": crate::dispatch::mode_str(mode),
                "source": source,
            });
            Ok(v)
        }
        "executor.settings.update" => Ok(settings::executor_ext::update(
            store,
            p.get("settings").unwrap_or(&Value::Null),
            n(p, "expectedRevision")?,
        )
        .map_err(serr)?),
        "executor.check" => Ok(settings::executor_ext::check(store).map_err(serr)?),

        // --- 诊断单项重查（S50） ---
        "diagnostics.run" => crate::dispatch::diagnostics_run_pub(state, store, s(p, "checkId")?),

        // --- GitLab 细分（S30） ---
        "gitlabProfile.currentUser" => {
            let profile =
                settings::profiles::gitlab_get(store, s(p, "profileId")?).map_err(serr)?;
            let token = match &profile.credential_ref_id {
                Some(rid) => Some(
                    settings::profiles::reveal_for(store, state.credentials.as_ref(), rid)
                        .map_err(serr)?,
                ),
                None => std::env::var("SIXGATES_GITLAB_TOKEN").ok(),
            };
            let client = sg_integrations::GitLabHttp {
                base_url: profile.base_url.clone(),
                token: token.clone().unwrap_or_default(),
            };
            client
                .current_user()
                .map_err(|e| RpcError::new(ErrorCode::GitlabUnreachable, e))
        }
        "gitlabProfile.checkProjectPermissions" => {
            let profile =
                settings::profiles::gitlab_get(store, s(p, "profileId")?).map_err(serr)?;
            let token = match &profile.credential_ref_id {
                Some(rid) => Some(
                    settings::profiles::reveal_for(store, state.credentials.as_ref(), rid)
                        .map_err(serr)?,
                ),
                None => std::env::var("SIXGATES_GITLAB_TOKEN").ok(),
            };
            // 通过 /projects/:id 可达性检查（URL 编码 namespace/project）。
            let encoded =
                format!("{}/{}", s(p, "namespace")?, s(p, "project")?).replace('/', "%2F");
            let url = format!(
                "{}/api/v4/projects/{encoded}",
                profile.base_url.trim_end_matches('/')
            );
            let resp = ureq::get(&url)
                .set("Private-Token", token.as_deref().unwrap_or(""))
                .timeout(std::time::Duration::from_secs(10))
                .call()
                .map_err(|e| RpcError::new(ErrorCode::GitlabUnreachable, e.to_string()))?;
            let body: Value = resp.into_json().unwrap_or(json!({}));
            let id = body["id"].as_i64().unwrap_or(0);
            if id == 0 {
                return Err(RpcError::new(ErrorCode::NotFound, "项目不可达"));
            }
            Ok(json!({"accessible": true, "projectId": id.to_string(),
                "permissions": {"repository": true, "issues": true, "mr": true, "pipeline": true, "artifact": true, "registry": true}}))
        }

        // --- SSH 项目绑定（S31） ---
        "sshTarget.bindProject" => {
            let target_id = s(p, "targetId")?.to_string();
            let bind = p.get("bind").and_then(|v| v.as_bool()).unwrap_or(true);
            let allow_auto = p
                .get("allowAutoDeploy")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let v = settings::profiles::ssh_bind_project(
                store,
                &target_id,
                s(p, "projectId")?,
                bind,
                allow_auto,
            )
            .map_err(serr)?;
            changed(store, "sshTarget", &target_id);
            Ok(serde_json::to_value(v).unwrap_or_default())
        }

        // --- 项目级知识覆盖（S11） ---
        "knowledge.projectSettings.get" => {
            Ok(settings::knowledge_defaults::get(store, Some(s(p, "projectId")?)).map_err(serr)?)
        }
        "knowledge.projectSettings.update" => {
            let revision = settings::knowledge_defaults::update(
                store,
                Some(s(p, "projectId")?),
                p.get("settings").unwrap_or(&json!({})),
                n(p, "expectedRevision")?,
            )
            .map_err(serr)?;
            Ok(json!({"revision": revision}))
        }

        // --- 备份 reveal（S41：main 打开 Finder） ---
        "backup.revealInFolder" => {
            let record = settings::backup_ext::record(store, s(p, "backupId")?).map_err(serr)?;
            if record.path.is_empty() || !std::path::Path::new(&record.path).exists() {
                return Err(RpcError::new(ErrorCode::NotFound, "备份文件不存在"));
            }
            Ok(json!({"path": record.path}))
        }

        // --- 审计设置（S42） ---
        "audit.settings.get" => {
            let entries = settings::settings::get(
                store,
                "global",
                None,
                Some(&["audit.settings".to_string()]),
            )
            .map_err(serr)?;
            Ok(entries
                .first()
                .map(|e| e.value.clone())
                .unwrap_or(json!({"retentionDays": 180, "exportRedacted": true})))
        }
        "audit.settings.update" => {
            let body = p.get("settings").cloned().unwrap_or(json!({}));
            let expected = if n(p, "expectedRevision")? == 0 {
                None
            } else {
                Some(n(p, "expectedRevision")?)
            };
            let updated = settings::settings::update(
                store,
                "global",
                None,
                &[settings::settings::Patch {
                    key: "audit.settings".into(),
                    value: body,
                    expected_revision: expected,
                }],
                "local",
            )
            .map_err(serr)?;
            changed(store, "settings", "audit");
            Ok(json!({"items": updated}))
        }

        // --- 工具清单/测试（S21） ---
        // F05/M0-③：运行时注册表（sg_agent::tools）是工具身份/风险/限制的权威；
        // toolPolicy 行覆盖策略字段（enabled/requires_approval/network）；注册表外的既有策略行（如 deploy）保留。
        "tool.list" => {
            let overrides = settings::policy_ext::tool_list(store).map_err(serr)?;
            let mut items: Vec<Value> = Vec::new();
            for def in sg_agent::tools::registry() {
                let o = overrides.iter().find(|o| o.tool_id == def.name);
                let mut v = def.to_json();
                v["tool_id"] = json!(def.name);
                v["enabled"] = json!(o.map(|o| o.enabled).unwrap_or(true));
                v["requires_approval"] = json!(o
                    .map(|o| o.requires_approval)
                    .unwrap_or(def.risk == sg_policy::Risk::High));
                v["network"] = json!(o
                    .map(|o| o.network.clone())
                    .unwrap_or_else(|| "deny".into()));
                v["revision"] = json!(o.map(|o| o.revision).unwrap_or(0));
                v["max_result_bytes"] = json!(def.max_result_bytes);
                v["timeout_sec"] = json!(def.timeout_sec);
                items.push(v);
            }
            for o in &overrides {
                if sg_agent::tools::find(&o.tool_id).is_none() {
                    items.push(json!({
                        "name": o.tool_id, "tool_id": o.tool_id, "description": "",
                        "risk": o.risk, "dataLevel": "internal",
                        "enabled": o.enabled, "requires_approval": o.requires_approval,
                        "network": o.network, "revision": o.revision,
                        "maxResultBytes": 0, "timeoutSec": 0, "parameters": null,
                    }));
                }
            }
            Ok(json!({"items": items}))
        }
        "tool.test" => Ok(json!({"toolId": s(p, "toolId")?, "status": "skipped",
            "note": "工具执行测试复用 agent.start 提案路径（allowlist + executor manifest）"})),

        _ => Err(RpcError::new(
            ErrorCode::MethodNotFound,
            format!("unknown {method}"),
        )),
    }
}
