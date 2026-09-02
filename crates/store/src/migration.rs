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
    let pending: Vec<&(i64, &str)> = MIGRATIONS
        .iter()
        .filter(|(v, _)| !applied.contains(v))
        .collect();
    if pending.is_empty() {
        return Ok(());
    }

    // 预迁移备份（在线快照）。
    let _ = crate::backup::snapshot(store);

    store.with_tx(|tx| {
        for (version, body) in &pending {
            tx.execute_batch(body)
                .map_err(|e| crate::Error::Message(format!("migration {version:04}: {e}")))?;
            tx.execute(
                "INSERT INTO schema_migrations(version) VALUES (?1)",
                [version],
            )?;
        }
        Ok(())
    })?;
    foreign_key_check(store)?;
    store.quick_check()
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
