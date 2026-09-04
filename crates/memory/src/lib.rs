//! sg-memory：项目记忆 bounded context（ADR-032 / 实施方案 v1.0）。
//!
//! 权威 = SQLite 元数据（entries/revisions/source_refs）+ app data content-addressed
//! objects；memory_fts 仅为可重建投影。只依赖 sg-store 与通用库；
//! 来源以通用 source_kind/source_id 保存，不反向依赖领域 crate（§5.1）。
pub mod candidate;
pub mod capture;
pub mod export;
pub mod model;
pub mod mutation;
pub mod purge;
pub mod repository;
pub mod retrieval;

pub use model::{
    CreateInput, DuplicateMode, MemorySettings, SettingsPatch, SourceRefInput, UpdateInput,
};

use sg_store::Store;

/// 全局 rollout flag 写入（app_settings；默认 false；测试/E2E 显式开启）。
pub fn set_feature_enabled(
    store: &Store,
    enabled: bool,
    updated_by: &str,
) -> Result<(), sg_store::Error> {
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO app_settings(scope, project_id, key, value_json, revision, updated_at, updated_by)
             VALUES ('global', '', 'memory.featureEnabled', ?1, 1, ?2, ?3)
             ON CONFLICT(scope, project_id, key) DO UPDATE SET
                value_json = excluded.value_json,
                revision = revision + 1,
                updated_at = excluded.updated_at,
                updated_by = excluded.updated_by",
            rusqlite::params![serde_json::json!(enabled).to_string(), sg_store::timefmt::now(), updated_by],
        )?;
        Ok(())
    })
}

/// 设置更新（CAS；MEM-001）：校验策略范围 → 同事务 audit/outbox/receipt。
pub fn settings_update(
    store: &Store,
    project_id: &str,
    patch: &SettingsPatch,
    expected_revision: i64,
    idempotency_key: &str,
) -> Result<MemorySettings, sg_store::Error> {
    if let Some(me) = patch.max_entries {
        if !(1..=32).contains(&me) {
            return Err(model::merr(
                model::err_tokens::QUOTA,
                "max_entries 超出 1..32",
            ));
        }
    }
    if let Some(mb) = patch.max_bytes {
        if !(1024..=65536).contains(&mb) {
            return Err(model::merr(
                model::err_tokens::QUOTA,
                "max_bytes 超出 1024..65536",
            ));
        }
    }
    if let Some(sd) = patch.stale_after_days {
        if !(1..=3650).contains(&sd) {
            return Err(model::merr(
                model::err_tokens::INVALID_STATE,
                "stale_after_days 超出 1..3650",
            ));
        }
    }
    if let Some(cm) = patch.capture_mode.as_deref() {
        if !matches!(cm, "off" | "suggest") {
            return Err(model::merr(
                model::err_tokens::INVALID_STATE,
                "capture_mode 仅 off/suggest",
            ));
        }
    }
    let fp = model::fingerprint(&serde_json::json!({
        "expectedRevision": expected_revision,
        "operation": "memory.settings.update",
        "patch": patch,
        "projectId": project_id,
    }));
    let feature = repository::feature_enabled(store)?;
    let updated = store.with_tx_immediate(|tx| {
        repository::require_project(tx, project_id)?;
        // 惰性建行后按 CAS 更新（expectedRevision=0 表示尚未初始化）。
        tx.execute(
            "INSERT OR IGNORE INTO project_memory_settings(project_id, updated_at, updated_by)
             VALUES (?1, ?2, 'local')",
            rusqlite::params![project_id, sg_store::timefmt::now()],
        )?;
        let current: i64 = tx.query_row(
            "SELECT revision FROM project_memory_settings WHERE project_id = ?1",
            [project_id],
            |r| r.get(0),
        )?;
        if current != expected_revision {
            return Err(model::merr(
                model::err_tokens::CONFLICT,
                format!("设置 revision 冲突：期望 {expected_revision}，实际 {current}"),
            ));
        }
        if repository::project_archived(tx, project_id)? && patch.enabled == Some(true) {
            return Err(model::merr(
                model::err_tokens::DISABLED,
                "项目已归档，不允许开启记忆注入",
            ));
        }
        tx.execute(
            "UPDATE project_memory_settings SET
                enabled = COALESCE(?2, enabled),
                capture_mode = COALESCE(?3, capture_mode),
                max_entries = COALESCE(?4, max_entries),
                max_bytes = COALESCE(?5, max_bytes),
                stale_after_days = COALESCE(?6, stale_after_days),
                revision = revision + 1,
                updated_at = ?7,
                updated_by = 'local'
             WHERE project_id = ?1",
            rusqlite::params![
                project_id,
                patch.enabled.map(|b| b as i64),
                patch.capture_mode,
                patch.max_entries,
                patch.max_bytes,
                patch.stale_after_days,
                sg_store::timefmt::now()
            ],
        )?;
        sg_store::audit::append_at(
            tx,
            "local",
            "memory.settings.update",
            "memory",
            project_id,
            serde_json::json!({
                "revision": expected_revision + 1,
                "fields": model::fingerprint(&serde_json::json!(patch)).chars().take(12).collect::<String>(),
            }),
        )?;
        sg_store::outbox::emit_at(
            tx,
            "memory",
            project_id,
            "memory.settings_changed",
            serde_json::json!({"projectId": project_id, "revision": expected_revision + 1}),
        )?;
        let result = repository::query_settings_row(tx, project_id, feature)?;
        mutation::receipt_put(
            tx,
            idempotency_key,
            project_id,
            "memory.settings.update",
            project_id,
            &fp,
            &serde_json::to_value(&result).unwrap_or_default(),
        )?;
        Ok(result)
    })?;
    Ok(updated)
}

/// 统计（备份 manifest / diagnostics 用；只输出计数，不输出正文）。
pub fn stats(store: &Store) -> Result<(i64, i64), sg_store::Error> {
    store.with_conn(|conn| {
        let entries: i64 =
            conn.query_row("SELECT COUNT(*) FROM memory_entries", [], |r| r.get(0))?;
        let objects: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_revisions WHERE object_sha256 IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        Ok((entries, objects))
    })
}

/// 启动检查：FTS 投影行数与权威不一致时才重建（§6.5；损坏对象如实上报，不阻断启动）。
pub fn ensure_fts_consistent(store: &Store) -> Result<serde_json::Value, sg_store::Error> {
    let (expected, actual) = store.with_conn(|conn| {
        let expected: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_entries e
             JOIN memory_revisions r ON r.id = e.current_revision_id
             WHERE e.status != 'purged'",
            [],
            |r| r.get(0),
        )?;
        let actual: i64 = conn.query_row("SELECT COUNT(*) FROM memory_fts", [], |r| r.get(0))?;
        Ok((expected, actual))
    })?;
    if expected == actual {
        return Ok(serde_json::json!({"rebuild": false, "expected": expected, "actual": actual}));
    }
    let report = repository::rebuild_index(store, None)?;
    Ok(
        serde_json::json!({"rebuild": true, "expected": expected, "actual": actual, "report": report}),
    )
}
