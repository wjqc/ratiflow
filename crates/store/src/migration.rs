use rusqlite::Connection;

/// 嵌入迁移（v2 0001–0010 与 v3 0011+；v2 数据可直接续跑）。
pub const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/0001_init.sql")),
    (2, include_str!("../migrations/0002_auth.sql")),
    (3, include_str!("../migrations/0003_workitem.sql")),
    (4, include_str!("../migrations/0004_artifact.sql")),
    (5, include_str!("../migrations/0005_agent.sql")),
    (6, include_str!("../migrations/0006_workflow.sql")),
    (7, include_str!("../migrations/0007_gate.sql")),
    (8, include_str!("../migrations/0008_delivery.sql")),
    (9, include_str!("../migrations/0009_events.sql")),
    (10, include_str!("../migrations/0010_passport_gates.sql")),
    (11, include_str!("../migrations/0011_v3_projects.sql")),
    (12, include_str!("../migrations/0012_v3_knowledge.sql")),
    (13, include_str!("../migrations/0013_v3_attachments.sql")),
    (14, include_str!("../migrations/0014_v3_context_items.sql")),
    (15, include_str!("../migrations/0015_v3_settings.sql")),
    (16, include_str!("../migrations/0016_provenance.sql")),
    (
        17,
        include_str!("../migrations/0017_stage_attempt_release.sql"),
    ),
    (
        18,
        include_str!("../migrations/0018_stage_snapshot_rollback.sql"),
    ),
    (19, include_str!("../migrations/0019_agent_profiles.sql")),
    (
        20,
        include_str!("../migrations/0020_model_provider_presets.sql"),
    ),
    (21, include_str!("../migrations/0021_workitem_archive.sql")),
    (
        22,
        include_str!("../migrations/0022_model_profile_models.sql"),
    ),
    (23, include_str!("../migrations/0023_project_memory.sql")),
    (24, include_str!("../migrations/0024_model_turns.sql")),
    (
        25,
        include_str!("../migrations/0025_knowledge_manifest_v2.sql"),
    ),
    (26, include_str!("../migrations/0026_model_usage.sql")),
    (27, include_str!("../migrations/0027_mcp_servers.sql")),
    (28, include_str!("../migrations/0028_skills.sql")),
    (29, include_str!("../migrations/0029_memory_origin.sql")),
    (30, include_str!("../migrations/0030_skills_agent.sql")),
    (
        31,
        include_str!("../migrations/0031_execution_outcomes.sql"),
    ),
    (
        32,
        include_str!("../migrations/0032_workflow_templates.sql"),
    ),
];

/// 运行迁移：建版本表 →（接管 v2 骨架库）→ 单事务逐文件执行 + 登记 → quick_check。
pub fn run(store: &crate::Store) -> Result<(), crate::Error> {
    store.with_conn(|conn| {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );",
        )?;
        adopt_v2(conn)?;
        Ok(())
    })?;

    let applied = applied_versions(store)?;
    // 未来 schema 守卫（RFC v1.0 §19.1/A38）：DB 中存在高于本应用支持上限的已应用版本
    // → 拒绝打开，不允许静默 no-op 继续读写不认识的数据。
    if let Some(&max_known) = MIGRATIONS.iter().map(|(v, _)| v).max() {
        if applied.iter().any(|v| *v > max_known) {
            return Err(crate::Error::Message(
                "schema version above supported maximum: please upgrade the application".into(),
            ));
        }
    }
    let pending: Vec<&(i64, &str)> = MIGRATIONS
        .iter()
        .filter(|(v, _)| !applied.contains(v))
        .collect();
    if pending.is_empty() {
        return Ok(());
    }

    // 预迁移备份是硬 Gate（RFC v1.0 §12.1/A48）：快照失败立即中止，DB 保持迁移前状态。
    // 仅对已含 objects 表的库强制（0009 起才有数据对象）；全新空库与 v2 骨架（version<9）无可保数据。
    if applied.iter().max() >= Some(&9) {
        crate::backup::snapshot(store)?;
    }

    // RFC v1.0 §12.1 runner 契约：
    // - FK 必须在事务开始前关闭（事务内 PRAGMA foreign_keys 是 no-op）；
    // - 提交前在同一事务内跑 foreign_key_check，违例即 ROLLBACK；
    // - commit/rollback 后无论成败恢复 FK ON（RAII 语义）；
    // - 提交后再做一次防御性 foreign_key_check。
    store.with_conn(|conn| {
        conn.pragma_update(None, "foreign_keys", "OFF")?;
        let outcome = (|| -> Result<(), crate::Error> {
            conn.execute_batch("BEGIN IMMEDIATE")
                .map_err(|e| crate::Error::Message(format!("begin migration tx: {e}")))?;
            let inner = (|| -> Result<(), crate::Error> {
                // 写锁内重读版本表：双开应用时后到者安全降级为 no-op。
                let applied: std::collections::HashSet<i64> = {
                    let mut stmt = conn.prepare("SELECT version FROM schema_migrations")?;
                    let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
                    let mut set = std::collections::HashSet::new();
                    for row in rows {
                        set.insert(row?);
                    }
                    set
                };
                for (version, body) in MIGRATIONS {
                    if applied.contains(version) {
                        continue;
                    }
                    conn.execute_batch(body).map_err(|e| {
                        crate::Error::Message(format!("migration {version:04}: {e}"))
                    })?;
                    conn.execute(
                        "INSERT INTO schema_migrations(version) VALUES (?1)",
                        [version],
                    )?;
                }
                let violations = fk_violations(conn)?;
                if violations > 0 {
                    return Err(crate::Error::Message(format!(
                        "foreign_key_check: {violations} violations in migration tx"
                    )));
                }
                Ok(())
            })();
            match inner {
                Ok(()) => conn
                    .execute_batch("COMMIT")
                    .map_err(|e| crate::Error::Message(format!("commit migration: {e}"))),
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    Err(e)
                }
            }
        })();
        // 恢复 FK ON 不依赖调用方自觉（§12.1 第 4 条）。
        let _ = conn.pragma_update(None, "foreign_keys", "ON");
        outcome
    })?;
    // 提交后防御性复核（§12.1 第 4 条）。
    foreign_key_check(store)?;
    store.quick_check()
}

fn fk_violations(conn: &Connection) -> Result<usize, crate::Error> {
    let mut stmt = conn
        .prepare("PRAGMA foreign_key_check")
        .map_err(|e| crate::Error::Message(e.to_string()))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| crate::Error::Message(e.to_string()))?;
    let mut n = 0usize;
    for row in rows {
        let _ = row.map_err(|e| crate::Error::Message(e.to_string()))?;
        n += 1;
    }
    Ok(n)
}

/// 外键一致性（蓝图 §13.4）：迁移后立即校验，违例即失败（不带着断链服务）。
fn foreign_key_check(store: &crate::Store) -> Result<(), crate::Error> {
    let violations: usize = store.with_conn(|conn| {
        let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut n = 0usize;
        for row in rows {
            let _ = row?;
            n += 1;
        }
        Ok(n)
    })?;
    if violations > 0 {
        return Err(crate::Error::Message(format!(
            "foreign_key_check: {violations} violations after migration"
        )));
    }
    Ok(())
}

fn adopt_v2(conn: &Connection) -> Result<(), rusqlite::Error> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))?;
    if count > 0 {
        return Ok(());
    }
    // v2 骨架库：app_meta.schema_version 标记已应用版本。
    let has_meta: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='app_meta'",
            [],
            |r| r.get(0),
        )
        .ok();
    if has_meta.is_some() {
        let legacy: Option<String> = conn
            .query_row(
                "SELECT value FROM app_meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .ok();
        if let Some(v) = legacy {
            if let Ok(n) = v.parse::<i64>() {
                if n > 0 {
                    conn.execute("INSERT INTO schema_migrations(version) VALUES (?1)", [n])?;
                }
            }
        }
    }
    Ok(())
}

fn applied_versions(store: &crate::Store) -> Result<Vec<i64>, crate::Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT version FROM schema_migrations")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}
