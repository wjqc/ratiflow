use crate::*;
use serde_json::json;
use sg_store::Store;

fn open() -> Store {
    let dir = std::env::temp_dir().join(format!(
        "sg-set-{}-{}",
        std::process::id(),
        sg_store::ids::new_id("t")
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Store::open(&dir, "test").unwrap()
}

// --- settings ---

#[test]
fn settings_update_revision_conflict_and_merge() {
    let s = open();
    settings::update(
        &s,
        "global",
        None,
        &[settings::Patch {
            key: "ui.density".into(),
            value: json!("compact"),
            expected_revision: None,
        }],
        "local",
    )
    .unwrap();

    // 错误 revision 拒绝（不覆盖）。
    let err = settings::update(
        &s,
        "global",
        None,
        &[settings::Patch {
            key: "ui.density".into(),
            value: json!("cozy"),
            expected_revision: Some(99),
        }],
        "local",
    )
    .unwrap_err();
    assert_eq!(err.code, codes::REVISION_CONFLICT);

    // 正确 revision 推进。
    let updated = settings::update(
        &s,
        "global",
        None,
        &[settings::Patch {
            key: "ui.density".into(),
            value: json!("cozy"),
            expected_revision: Some(1),
        }],
        "local",
    )
    .unwrap();
    assert_eq!(updated[0].revision, 2);

    // 项目覆盖 + effective 合成。
    settings::update(
        &s,
        "global",
        Some("pj_1"),
        &[settings::Patch {
            key: "ui.density".into(),
            value: json!("dense"),
            expected_revision: None,
        }],
        "local",
    )
    .unwrap();
    let merged = settings::effective(&s, Some("pj_1"), None).unwrap();
    let entry = merged.iter().find(|e| e.key == "ui.density").unwrap();
    assert_eq!(entry.value, json!("dense"));
    assert_eq!(entry.source, "project");
}

// --- credentials ---

#[test]
fn credential_lifecycle_keychain_and_revision() {
    let s = open();
    let backend = credentials::InMemoryCredentials::default();

    let created = credentials::create(
        &s,
        &backend,
        "GitLab Token",
        "gitlab_token",
        "gitlab",
        "secret-value-1",
        None,
    )
    .unwrap();
    assert_eq!(created.status, "active");
    // 回读校验（DTO 无 secret 字段——序列化里不出现）。
    let dto = serde_json::to_value(&created).unwrap();
    assert!(
        dto.to_string().find("secret-value").is_none(),
        "DTO 不得包含明文"
    );

    let replaced = credentials::replace(
        &s,
        &backend,
        &created.id,
        "secret-value-2",
        created.revision,
    )
    .unwrap();
    assert_eq!(replaced.revision, created.revision + 1);
    assert_eq!(
        credentials::reveal(&backend, &created.id).unwrap(),
        "secret-value-2"
    );

    let err = credentials::replace(&s, &backend, &created.id, "x", 1).unwrap_err();
    assert_eq!(err.code, codes::REVISION_CONFLICT);

    let verified = credentials::verify(&s, &backend, &created.id).unwrap();
    assert_eq!(verified.status, "active");
    credentials::remove(&s, &backend, &created.id, verified.revision, false).unwrap();
    assert!(credentials::get(&s, &created.id).is_err());
}

#[test]
fn credential_delete_blocked_by_dependents_force_marks_degraded() {
    let s = open();
    let backend = credentials::InMemoryCredentials::default();
    let cred = credentials::create(
        &s,
        &backend,
        "模型 Key",
        "model_api_key",
        "openai",
        "sk-x",
        None,
    )
    .unwrap();
    profiles::model_create(
        &s,
        &json!({"name":"主力","providerKind":"fake","credentialRefId":cred.id}),
    )
    .unwrap();

    let blocked = credentials::remove(&s, &backend, &cred.id, cred.revision, false).unwrap_err();
    assert_eq!(blocked.code, "CONFLICT");

    credentials::remove(&s, &backend, &cred.id, cred.revision, true).unwrap();
    let profiles = profiles::model_list(&s).unwrap();
    assert_eq!(
        profiles[0].status, "degraded",
        "引用方进入 degraded，不自动回退"
    );
}

// --- profiles ---

#[test]
fn model_profile_managed_readonly_and_route_reference() {
    let s = open();
    let p = profiles::model_create(&s, &json!({"name":"P1","providerKind":"fake"})).unwrap();
    // 路由引用阻止删除（用当前 revision 隔离该语义）。
    let updated = profiles::model_update(&s, &p.id, &json!({"name":"改名"}), p.revision).unwrap();
    profiles::route_update(
        &s,
        &json!({"taskKind":"default","primaryProfileId":updated.id}),
        0,
    )
    .unwrap();
    let blocked = profiles::model_remove(&s, &updated.id, updated.revision).unwrap_err();
    assert_eq!(blocked.code, "CONFLICT");

    // env 托管只读。
    std::env::set_var("SIXGATES_MODEL_BASE_URL", "https://env.example.com/v1");
    let imported = profiles::import_env_profiles(&s).unwrap();
    assert!(imported.iter().any(|id| id == "mp_env_model"));
    let managed = profiles::model_get(&s, "mp_env_model").unwrap();
    assert_eq!(managed.managed_source.as_deref(), Some("env"));
    let err = profiles::model_update(&s, "mp_env_model", &json!({"name":"x"}), managed.revision)
        .unwrap_err();
    assert_eq!(err.code, codes::MANAGED_READ_ONLY);
    assert!(profiles::model_remove(&s, "mp_env_model", 1).is_err());
    std::env::remove_var("SIXGATES_MODEL_BASE_URL");
}

#[test]
fn ssh_host_key_accept_flow() {
    let s = open();
    let t = profiles::ssh_create(
        &s,
        &json!({"name":"部署机","host":"deploy.test","user":"deploy"}),
    )
    .unwrap();
    assert_eq!(t.fingerprint_status, "unverified");

    let accepted = profiles::ssh_accept_host_key(&s, &t.id, "SHA256:AAA").unwrap();
    assert_eq!(accepted.fingerprint_status, "accepted");
    assert_eq!(accepted.fingerprint, "SHA256:AAA");
}

// --- policy ---

#[test]
fn tool_policy_effective_with_project_override() {
    let s = open();
    policy_ext::tool_update(&s, &json!({"toolId":"read_file","enabled":false}), 0).unwrap();
    policy_ext::tool_update(
        &s,
        &json!({"toolId":"write_file","projectId":"pj_1","requiresApproval":true}),
        0,
    )
    .unwrap();

    let global = policy_ext::tool_effective(&s, None).unwrap();
    let read = global.iter().find(|t| t.tool_id == "read_file").unwrap();
    assert!(!read.enabled, "全局禁用生效");

    let project = policy_ext::tool_effective(&s, Some("pj_1")).unwrap();
    let write = project.iter().find(|t| t.tool_id == "write_file").unwrap();
    assert!(write.requires_approval, "项目覆盖生效");
}

#[test]
fn knowledge_defaults_revision_and_merge() {
    let s = open();
    knowledge_defaults::update(&s, None, &json!({"maxChunkChars": 2000}), 0).unwrap();
    let err =
        knowledge_defaults::update(&s, None, &json!({"maxChunkChars": 4000}), 99).unwrap_err();
    assert_eq!(err.code, codes::REVISION_CONFLICT);
    knowledge_defaults::update(&s, Some("pj_1"), &json!({"maxChunkChars": 4000}), 0).unwrap();
    let merged = knowledge_defaults::get(&s, Some("pj_1")).unwrap();
    assert_eq!(merged["maxChunkChars"], json!(4000));
    let global_view = knowledge_defaults::get(&s, None).unwrap();
    assert_eq!(global_view["maxChunkChars"], json!(2000));
}

// --- operations ---

#[test]
fn operation_progress_and_finish() {
    let s = open();
    let op = operations::begin(&s, "backup.create", false).unwrap();
    operations::progress(&s, &op.operation_id, 2, 5, "backup.step.objects").unwrap();
    operations::finish(
        &s,
        &op.operation_id,
        "succeeded",
        json!({"backupId": "bk_1"}),
    )
    .unwrap();
    let got = operations::get(&s, &op.operation_id).unwrap();
    assert_eq!(got.status, "succeeded");
    assert_eq!(got.progress["completed"], json!(2));
    assert_eq!(got.result.unwrap()["backupId"], json!("bk_1"));
}

// --- audit ---

#[test]
fn audit_redaction_and_export() {
    let s = open();
    let seq = audit_ext::append(
        &s,
        &audit_ext::AuditEvent {
            actor: "local",
            actor_kind: "user",
            action: "credentialRef.create",
            target_type: "credential_ref",
            target_id: "cr_1",
            result: "success",
            correlation_id: Some("corr_t1"),
            project_id: None,
            before_summary: Some(&json!({"before": "password = supersecret123"})),
            after_summary: Some(&json!({"hasSecret": true})),
        },
    )
    .unwrap();
    let entry = audit_ext::get(&s, seq).unwrap();
    assert!(entry.metadata_redacted);
    let before = entry.before_summary.unwrap();
    assert!(!before.contains("supersecret123"), "before_summary 已脱敏");

    let exported = audit_ext::export(&s, &json!({}), 10).unwrap();
    let text = exported.to_string();
    assert!(!text.contains("supersecret123"), "导出全文无秘密");
    assert!(text.contains("credentialRef.create"));
}

// --- backup ---

#[test]
fn backup_verify_detects_corruption() {
    let s = open();
    let snap = sg_store::backup::snapshot(&s).unwrap();
    let rec = backup_ext::register(&s, &snap, 1).unwrap();
    let ok = backup_ext::verify(&s, &rec.id).unwrap();
    assert!(ok.verified);

    // 篡改快照 → corrupt。
    std::fs::write(&snap.path, b"tampered").unwrap();
    let bad = backup_ext::verify(&s, &rec.id).unwrap();
    assert!(!bad.verified);
    assert_eq!(bad.status, "corrupt");
}

#[test]
fn backup_restore_requires_verified_and_reports_restart() {
    let s = open();
    let snap = sg_store::backup::snapshot(&s).unwrap();
    let rec = backup_ext::register(&s, &snap, 1).unwrap();

    // 未 verify 先恢复 → 拒绝。
    let err = backup_ext::restore(&s, &rec.id).unwrap_err();
    assert_eq!(err.code, codes::BACKUP_CORRUPT);

    backup_ext::verify(&s, &rec.id).unwrap();
    let outcome = backup_ext::restore(&s, &rec.id).unwrap();
    assert!(outcome.restored);
    assert!(outcome.requires_restart, "恢复必须要求重启");
}

// --- summary 聚合（阻塞语义） ---

#[test]
fn summary_blockers_semantics() {
    let s = open();
    // 无模型 Profile 且无 env → 阻塞 agent_run。
    let summary = crate::settings::summary::aggregate(&s).unwrap();
    assert_eq!(summary["overallStatus"], json!("action_required"));
    let model_blocker = summary["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == json!("model_not_configured"))
        .unwrap();
    assert_eq!(model_blocker["capabilities"], json!(["agent_run"]));
    assert_eq!(model_blocker["targetRoute"], json!("/settings/models"));

    // 配置模型后阻塞解除。
    profiles::model_create(&s, &json!({"name":"P","providerKind":"fake"})).unwrap();
    let ready = crate::settings::summary::aggregate(&s).unwrap();
    assert!(ready["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .all(|b| b["id"] != json!("model_not_configured")));
}
