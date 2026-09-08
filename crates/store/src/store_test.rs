#[cfg(test)]
mod tests {
    use crate::{backup, migration, objects, outbox, scan, Store};

    fn open() -> (Store, tempdir::TempDirGuard) {
        let dir = tempdir::make("sg-store-test");
        let store = Store::open(dir.path(), "test").expect("open store");
        (store, dir)
    }

    mod tempdir {
        use std::path::PathBuf;

        pub struct TempDirGuard(pub PathBuf);
        impl Drop for TempDirGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        pub fn make(prefix: &str) -> TempDirGuard {
            let path = std::env::temp_dir().join(format!(
                "{prefix}-{}-{}",
                std::process::id(),
                crate::ids::new_id("d")
            ));
            std::fs::create_dir_all(&path).unwrap();
            TempDirGuard(path)
        }

        impl TempDirGuard {
            pub fn path(&self) -> &std::path::Path {
                &self.0
            }
        }
    }

    #[test]
    fn migration_versions_strictly_increasing_and_unique() {
        let versions: Vec<i64> = crate::migration::MIGRATIONS
            .iter()
            .map(|(v, _)| *v)
            .collect();
        let mut sorted = versions.clone();
        sorted.sort();
        assert_eq!(versions, sorted, "迁移必须按版本升序");
        let mut dedup = versions.clone();
        dedup.dedup();
        assert_eq!(versions.len(), dedup.len(), "迁移版本不得重复");
    }

    #[test]
    fn migrations_apply_and_are_idempotent() {
        let (store, _guard) = open();
        assert!(store.schema_version().unwrap() >= 15);
        migration::run(&store).unwrap();
        assert_eq!(
            store.schema_version().unwrap(),
            migration::MIGRATIONS.last().unwrap().0
        );
        store.quick_check().unwrap();
    }

    #[test]
    fn v2_database_adopts_and_upgrades() {
        let dir = tempdir::make("sg-v2-adopt");
        // 手工构造 v2 骨架库：app_meta.schema_version=1，无 schema_migrations。
        {
            let conn = rusqlite::Connection::open(dir.path().join("ratiflow.db")).unwrap();
            conn.execute_batch(include_str!("../migrations/0001_init.sql"))
                .unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        }
        let store = Store::open(dir.path(), "test").unwrap();
        assert_eq!(
            store.schema_version().unwrap(),
            migration::MIGRATIONS.last().unwrap().0
        );
    }

    #[test]
    fn migration_0020_rebuild_preserves_profiles_and_routes() {
        let dir = tempdir::make("sg-migration-0020");
        // 构造 0015–0019 版老库：带 CHECK 旧约束的 model_profiles + 种子数据。
        {
            let conn = rusqlite::Connection::open(dir.path().join("ratiflow.db")).unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );",
            )
            .unwrap();
            for (version, body) in migration::MIGRATIONS {
                if *version > 19 {
                    break;
                }
                conn.execute_batch(body).unwrap();
                conn.execute(
                    "INSERT INTO schema_migrations(version) VALUES (?1)",
                    [version],
                )
                .unwrap();
            }
            conn.execute(
                "INSERT INTO credential_refs(id, name, kind, keychain_service, keychain_account, created_at, updated_at)
                 VALUES ('cr_seed','seed','model_api_key','svc','acc','2026-01-01','2026-01-01')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO model_profiles(id, name, provider_kind, base_url, credential_ref_id, default_model, created_at, updated_at)
                 VALUES ('mp_seed','seed','openai_compatible','https://old.example.com/v1','cr_seed','m1','2026-01-01','2026-01-01')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO model_routes(id, scope, task_kind, primary_profile_id, revision, updated_at)
                 VALUES ('mr_seed','global','default','mp_seed',1,'2026-01-01')",
                [],
            )
            .unwrap();
        }
        // 打开即续跑 0020（表重建）。
        let store = Store::open(dir.path(), "test").unwrap();
        assert_eq!(
            store.schema_version().unwrap(),
            migration::MIGRATIONS.last().unwrap().0
        );
        let name: String = store
            .with_conn(|conn| {
                Ok(conn.query_row(
                    "SELECT default_model FROM model_profiles WHERE id='mp_seed'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(name, "m1");
        let primary: String = store
            .with_conn(|conn| {
                Ok(conn.query_row(
                    "SELECT primary_profile_id FROM model_routes WHERE id='mr_seed'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(primary, "mp_seed");
        // 新 kind 可写（旧 CHECK 会拒绝）。
        let inserted = store
            .with_conn(|conn| {
                Ok(conn.execute(
                    "INSERT INTO model_profiles(id, name, provider_kind, base_url, created_at, updated_at)
                     VALUES ('mp_zhipu','zhipu','zhipu','https://open.bigmodel.cn/api/paas/v4','2026-01-02','2026-01-02')",
                    [],
                )?)
            })
            .unwrap();
        assert_eq!(inserted, 1);
    }

    #[test]
    fn objects_put_idempotent_and_secret_rejected() {
        let (store, _guard) = open();
        let info = objects::put(
            &store,
            &b"evidence body"[..],
            objects::PutOptions::default(),
        )
        .unwrap();
        assert_eq!(info.size, 13);
        let again = objects::put(
            &store,
            &b"evidence body"[..],
            objects::PutOptions::default(),
        )
        .unwrap();
        assert_eq!(again.sha256, info.sha256);
        let body = objects::open(&store, &info.sha256).unwrap();
        assert_eq!(body, b"evidence body");

        let secret = b"password: supersecretvalue123";
        let err = objects::put(&store, &secret[..], objects::PutOptions::default()).unwrap_err();
        assert!(err.to_string().contains("object_contains_secrets"));
        let ok = objects::put(
            &store,
            &secret[..],
            objects::PutOptions {
                allow_secrets: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!ok.sha256.is_empty());
    }

    #[test]
    fn objects_max_size() {
        let (store, _guard) = open();
        let big = vec![b'a'; 1025];
        let err = objects::put(
            &store,
            &big[..],
            objects::PutOptions {
                max_bytes: 1024,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("max size"));
    }

    #[test]
    fn scan_mask_and_no_crossline() {
        let masked =
            scan::mask(b"db.password = supersecretvalue123 api_key = sk-0123456789abcdef").0;
        assert!(masked.contains("[REDACTED:password_assignment]"));
        assert!(!masked.contains("supersecretvalue123"));

        let env = b"RATIFLOW_GITLAB_TOKEN=\nRATIFLOW_MODEL_BASE_URL=https://api.example.com/v1\n";
        assert!(!scan::has_high_risk(&scan::scan(env)));
    }

    #[test]
    fn scan_assignment_rules_ignore_code_shapes() {
        // 代码赋值不是秘密：值含代码标点（:: < > ( ) ;）不命中（快照/write_draft 曾被误拒）。
        let code = concat!(
            "let token = std::sync::Arc::new(sg_integrations::CancelToken::new());\n",
            "pub fn register(&self, run_id: &str, token: Arc<CancelToken>) {}\n",
            "let secret = format!(\"{}\", value);\n",
            "let password = input.trim().to_string();\n",
        );
        assert!(
            !scan::has_high_risk(&scan::scan(code.as_bytes())),
            "代码赋值不应命中: {:?}",
            scan::scan(code.as_bytes())
        );
        // 带引号或秘密材料字符集的裸值仍命中。
        assert!(scan::has_high_risk(&scan::scan(
            b"password = \"hunter2pass\""
        )));
        assert!(scan::has_high_risk(&scan::scan(
            b"API_KEY=abcdef1234567890abcdef"
        )));
        assert!(scan::has_high_risk(&scan::scan(
            b"token: 'ghp_0123456789abcdefghijklmnopqrstuvwxyz'"
        )));
    }

    #[test]
    fn outbox_emit_replay() {
        let (store, _guard) = open();
        let s1 = outbox::emit(
            &store,
            "workitem",
            "wi_1",
            "workitem.created",
            serde_json::json!({"title": "t"}),
        )
        .unwrap();
        let s2 = outbox::emit(
            &store,
            "workitem",
            "wi_1",
            "stage.passed",
            serde_json::json!({"gate": "requirements"}),
        )
        .unwrap();
        assert!(s2 > s1);
        let replayed = outbox::replay(&store, s1, 10).unwrap();
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0]["type"], "stage.passed");
        assert_eq!(outbox::latest_sequence(&store).unwrap(), s2);
    }

    #[test]
    fn backup_snapshot_manifest() {
        let (store, _guard) = open();
        objects::put(&store, &b"payload"[..], objects::PutOptions::default()).unwrap();
        let snap = backup::snapshot(&store).unwrap();
        assert!(snap.manifest["snapshotSha256"].as_str().unwrap().len() == 64);
        assert_eq!(snap.manifest["objectsCount"].as_i64().unwrap(), 1);
    }

    /// A34（RFC v1.0 §17）：populated v23 库经 0024 闭包换表后存量知识数据完整保留，
    /// 来源转 origin='local'+legacy_local，chunks 补 project_id，replay_status=legacy_pending。
    #[test]
    fn migration_0024_preserves_knowledge_data() {
        let dir = tempdir::make("sg-mig-0024");
        {
            let conn = rusqlite::Connection::open(dir.path().join("ratiflow.db")).unwrap();
            conn.execute_batch("CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY);")
                .unwrap();
            for (v, body) in crate::migration::MIGRATIONS
                .iter()
                .filter(|(v, _)| *v <= 23)
            {
                conn.execute_batch(body).unwrap();
                conn.execute("INSERT INTO schema_migrations(version) VALUES (?1)", [v])
                    .unwrap();
            }
            conn.execute(
                "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                 VALUES ('pj', 'u', 'n', 'p', 'main', '2026-01-01T00:00:00.000Z')",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO workitems(id, project_id, title, created_at, updated_at)
                 VALUES ('w1', 'pj', 't', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO knowledge_sources(id, project_id, kind, name, locator, scan_state, created_at, updated_at)
                 VALUES ('ks1', 'pj', 'repo_path', '主仓库', '/tmp/x', 'indexed', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO knowledge_chunks(id, source_id, ordinal, object_sha256) VALUES ('kc1', 'ks1', 0, 'aa')",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO context_manifests(id, workitem_id, scope, data_policy, created_at)
                 VALUES ('cm1', 'w1', 'standard', 'standard', '2026-01-01T00:00:00.000Z')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO context_manifest_items(manifest_id, source_id, object_sha256, purpose, included, ordinal)
                 VALUES ('cm1', 'ks1', '', 'retrieval', 1, 0)",
                [],
            ).unwrap();
        }
        let store = Store::open(dir.path(), "test").unwrap();
        assert_eq!(
            store.schema_version().unwrap(),
            migration::MIGRATIONS.last().unwrap().0
        );
        store.with_conn(|c| {
            // 来源：origin/local/legacy_local/present。
            let (origin, legacy, present): (String, i64, i64) = c.query_row(
                "SELECT origin, legacy_local, present FROM knowledge_sources WHERE id='ks1'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            assert_eq!((origin.as_str(), legacy, present), ("local", 1, 1));
            // chunks：project_id 回填，generation_id 空。
            let (pid, gid): (String, Option<String>) = c.query_row(
                "SELECT project_id, generation_id FROM knowledge_chunks WHERE id='kc1'", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
            assert_eq!(pid.as_str(), "pj");
            assert!(gid.is_none());
            // context items 保留 + replay_status 默认。
            let n: i64 = c.query_row("SELECT COUNT(*) FROM context_manifest_items WHERE manifest_id='cm1'", [], |r| r.get(0))?;
            assert_eq!(n, 1);
            let rs: String = c.query_row("SELECT replay_status FROM context_manifests WHERE id='cm1'", [], |r| r.get(0))?;
            assert_eq!(rs, "legacy_pending");
            // 新表全部存在。
            for t in ["knowledge_generations","knowledge_generation_sources","knowledge_generation_active",
                      "knowledge_generation_retention","knowledge_generation_activation_history",
                      "knowledge_ops","context_manifest_blocks","context_manifest_item_sources","context_migration_jobs"] {
                let ok: i64 = c.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1", [t], |r| r.get(0))?;
                assert_eq!(ok, 1, "缺表 {t}");
            }
            // manifest 来源 partial unique index 存在。
            let idx: i64 = c.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_src_manifest_stable'",
                [], |r| r.get(0)).or_else(|_| c.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name LIKE 'sqlite_autoindex%'",
                [], |r| r.get(0)))?;
            assert!(idx >= 1);
            Ok(())
        }).unwrap();
        store.quick_check().unwrap();
    }

    /// A38（§19.1）：legacy 纪元文件被保留且不被新 schema 污染；v2 承接数据。
    #[test]
    fn legacy_epoch_file_isolated_and_v2_carries_data() {
        let dir = tempdir::make("sg-epoch");
        {
            let conn = rusqlite::Connection::open(dir.path().join("ratiflow.db")).unwrap();
            conn.execute_batch("CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY);")
                .unwrap();
            for (v, body) in crate::migration::MIGRATIONS
                .iter()
                .filter(|(v, _)| *v <= 23)
            {
                conn.execute_batch(body).unwrap();
                conn.execute("INSERT INTO schema_migrations(version) VALUES (?1)", [v])
                    .unwrap();
            }
            conn.execute(
                "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                 VALUES ('pj', 'u', 'n', 'p', 'main', '2026-01-01T00:00:00.000Z')",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO knowledge_sources(id, project_id, kind, name, locator, created_at, updated_at)
                 VALUES ('ks_old', 'pj', 'document', 'n', 'l', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
                [],
            ).unwrap();
        }
        let store = Store::open(dir.path(), "test").unwrap();
        let n: i64 = store
            .with_conn(|c| {
                c.query_row("SELECT COUNT(*) FROM knowledge_sources", [], |r| r.get(0))
                    .map_err(crate::Error::from)
            })
            .unwrap();
        assert_eq!(n, 1, "v2 承接 legacy 数据");
        // 旧文件 schema_migrations 仍停在 23（只读保留，未被 0024 污染）。
        let conn = rusqlite::Connection::open(dir.path().join("ratiflow.db")).unwrap();
        let maxv: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(maxv, 23);
        let cnt: i64 = conn
            .query_row("SELECT COUNT(*) FROM knowledge_sources", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cnt, 1);
    }

    /// A38（§19.1）：DB 含高于支持上限的 schema 版本 → 拒绝打开。
    #[test]
    fn future_schema_version_refuses_open() {
        let dir = tempdir::make("sg-future-schema");
        let store = Store::open(dir.path(), "test").unwrap();
        drop(store);
        let conn = rusqlite::Connection::open(dir.path().join("ratiflow-v3.db")).unwrap();
        conn.execute("INSERT INTO schema_migrations(version) VALUES (999)", [])
            .unwrap();
        drop(conn);
        let result = Store::open(dir.path(), "test");
        assert!(result.is_err(), "未来 schema 版本必须拒启");
    }

    /// M0-05（EvoFlow 方案 §7.1）：空目录全新安装只建 v3，不产生旧纪元文件；
    /// v3 纪元元数据落 app_meta。
    #[test]
    fn fresh_dir_opens_v3_without_v2() {
        let dir = tempdir::make("sg-v3-fresh");
        let store = Store::open(dir.path(), "test").unwrap();
        assert_eq!(
            store.schema_version().unwrap(),
            migration::MIGRATIONS.last().unwrap().0
        );
        assert!(dir.path().join("ratiflow-v3.db").exists());
        assert!(!dir.path().join("ratiflow-v2.db").exists());
        assert!(!dir.path().join("ratiflow.db").exists());
        let epoch: String = store
            .with_conn(|c| {
                c.query_row("SELECT value FROM app_meta WHERE key='db_epoch'", [], |r| {
                    r.get(0)
                })
                .map_err(crate::Error::from)
            })
            .unwrap();
        assert_eq!(epoch, "v3");
    }

    /// 构造 v2 纪元存量库：迁移 ≤30 + 一条已执行提案事实（含 FK 链）。
    fn build_v2_fixture(dir: &tempdir::TempDirGuard) {
        let conn = rusqlite::Connection::open(dir.path().join("ratiflow-v2.db")).unwrap();
        conn.execute_batch("CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY);")
            .unwrap();
        for (v, body) in migration::MIGRATIONS.iter().filter(|(v, _)| *v <= 30) {
            conn.execute_batch(body).unwrap();
            conn.execute("INSERT INTO schema_migrations(version) VALUES (?1)", [v])
                .unwrap();
        }
        conn.execute_batch(
            "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
             VALUES ('pj','u','n','p','main','2026-01-01T00:00:00.000Z');
            INSERT INTO workitems(id, project_id, title, created_at, updated_at)
             VALUES ('wi','pj','t','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z');
            INSERT INTO context_manifests(id, workitem_id, scope, data_policy, created_at)
             VALUES ('ctx1','wi','{}','standard','2026-01-01T00:00:00.000Z');
            INSERT INTO agent_runs(id, workitem_id, task_id, goal, input_baseline_sha, context_manifest_id,
                tool_allowlist, budget, policy_snapshot, idempotency_key, status, created_at, updated_at)
             VALUES ('run_seed','wi','','g','deadbeef','ctx1','[]','{}','default','ik_seed','failed',
                '2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z');
            INSERT INTO tool_proposals(id, agent_run_id, tool, arguments, risk, action_digest,
                requires_approval, decision, result, created_at)
             VALUES ('tp_seed','run_seed','read_file','{}','low','d0',0,'executed','ok',
                '2026-01-01T00:00:00.000Z');",
        )
        .unwrap();
    }

    /// M0-05（§7.1 第 1/2 条）：v2 存量库升级 —— v3 承接数据并推进 schema，
    /// v2 原文件保留在 30（旧包回退点，不被新 schema 污染）。
    #[test]
    fn v2_epoch_imported_to_v3_and_v2_preserved() {
        let dir = tempdir::make("sg-v3-import");
        build_v2_fixture(&dir);
        let store = Store::open(dir.path(), "test").unwrap();
        assert_eq!(
            store.schema_version().unwrap(),
            migration::MIGRATIONS.last().unwrap().0
        );
        let (decision, tool): (String, String) = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT decision, tool FROM tool_proposals WHERE id='tp_seed'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(crate::Error::from)
            })
            .unwrap();
        assert_eq!(
            (decision.as_str(), tool.as_str()),
            ("executed", "read_file")
        );
        // 旧包回退点：v2 停在 30，存量事实同在。
        let conn = rusqlite::Connection::open(dir.path().join("ratiflow-v2.db")).unwrap();
        let maxv: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(maxv, 30);
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tool_proposals WHERE id='tp_seed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    /// M0-05（EV-023）：本次新建 v3 迁移失败 → 半成品删除、v2 保留、应用拒绝伪启动。
    #[test]
    fn v3_migration_failure_removes_half_baked_and_keeps_v2() {
        let dir = tempdir::make("sg-v3-poison");
        build_v2_fixture(&dir);
        // 故障注入：预建同名表使 0031 的 CREATE TABLE 失败（毒化随导入进入 v3）。
        {
            let conn = rusqlite::Connection::open(dir.path().join("ratiflow-v2.db")).unwrap();
            conn.execute_batch("CREATE TABLE tool_execution_outcomes (id TEXT PRIMARY KEY);")
                .unwrap();
        }
        let result = Store::open(dir.path(), "test");
        assert!(result.is_err(), "迁移失败必须拒绝启动");
        assert!(
            !dir.path().join("ratiflow-v3.db").exists(),
            "半成品 v3 必须删除"
        );
        assert!(dir.path().join("ratiflow-v2.db").exists(), "v2 必须保留");
    }

    /// M0-06：decision 扩 unknown/indeterminate；outcome 事实表 CHECK / UNIQUE 生效。
    #[test]
    fn migration_0031_outcome_semantics() {
        let (store, _guard) = open();
        store
            .with_conn(|c| {
                c.execute_batch(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj','u','n','p','main','t');
                    INSERT INTO workitems(id, project_id, title, created_at, updated_at)
                     VALUES ('wi','pj','t','t','t');
                    INSERT INTO context_manifests(id, workitem_id, scope, data_policy, created_at)
                     VALUES ('ctx1','wi','{}','standard','t');
                    INSERT INTO agent_runs(id, workitem_id, task_id, goal, input_baseline_sha, context_manifest_id,
                        tool_allowlist, budget, policy_snapshot, idempotency_key, status, created_at, updated_at)
                     VALUES ('run_t','wi','','g','sha','ctx1','[]','{}','default','ik_t','running','t','t');
                    INSERT INTO tool_proposals(id, agent_run_id, tool, arguments, risk, action_digest,
                        requires_approval, decision, result, created_at)
                     VALUES ('tp_t','run_t','read_file','{}','low','d',0,'executed','ok','t');",
                )
                .map_err(crate::Error::from)?;
                Ok(())
            })
            .unwrap();
        // 新终态 decision 可写。
        store
            .with_conn(|c| {
                c.execute("UPDATE tool_proposals SET decision='unknown' WHERE id='tp_t'", [])
                    .map_err(crate::Error::from)?;
                c.execute(
                    "INSERT INTO tool_execution_outcomes(id, proposal_id, outcome, reason, reconciliation, created_at)
                     VALUES ('o1','tp_t','unknown','side_effect_unverified','pending','t')",
                    [],
                )
                .map_err(crate::Error::from)?;
                Ok(())
            })
            .unwrap();
        // 非法 outcome 拒绝（executed 不是一等 outcome 枚举值）。
        let bad: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "INSERT INTO tool_execution_outcomes(id, proposal_id, outcome, created_at)
                 VALUES ('o2','tp_t','executed','t')",
                [],
            )?)
        });
        assert!(bad.is_err(), "outcome CHECK 应拒绝 executed");
        // 幂等键：同提案第二条 outcome 拒绝。
        let dup: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "INSERT INTO tool_execution_outcomes(id, proposal_id, outcome, created_at)
                 VALUES ('o3','tp_t','failed','t')",
                [],
            )?)
        });
        assert!(dup.is_err(), "proposal_id UNIQUE 应拒绝重复 outcome");
        // indeterminate decision 合法。
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE tool_proposals SET decision='indeterminate' WHERE id='tp_t'",
                    [],
                )
                .map_err(crate::Error::from)
            })
            .unwrap();
        // 存量事实保留（事实只追加，迁移不回写历史行）。
        let n: i64 = store
            .with_conn(|c| {
                c.query_row("SELECT COUNT(*) FROM tool_proposals", [], |r| r.get(0))
                    .map_err(crate::Error::from)
            })
            .unwrap();
        assert_eq!(n, 1);
    }
    /// M2-01（0033 / EV-005 前置）：Plan DAG 表语义——
    /// CHECK 拒绝非法枚举；attempt 幂等（task_id+attempt_no 唯一）；
    /// 单活跃 partial index；FK 链完整。
    #[test]
    fn migration_0033_plan_dag_semantics() {
        let (store, _guard) = open();
        store
            .with_conn(|c| {
                c.execute_batch(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj','u','n','p','main','t');
                    INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                     VALUES ('wi','pj','t','','[]','requirements','t','t');
                    INSERT INTO context_manifests(id, workitem_id, scope, data_policy, created_at)
                     VALUES ('ctx1','wi','{}','standard','t');
                    INSERT INTO stage_attempts(id, workitem_id, gate, attempt_no, branch_no, state, entry_snapshot_id,
                        input_package_sha256, active_output_package_id, predecessor_attempt_id, created_at, updated_at)
                     VALUES ('att1','wi','requirements',1,1,'prepared','','',NULL,NULL,'t','t');
                    INSERT INTO plan_revisions(id, workitem_id, stage_attempt_id, revision_no, status, digest, created_at, updated_at)
                     VALUES ('pr1','wi','att1',1,'draft','d','t','t');
                    INSERT INTO plan_tasks(id, plan_revision_id, task_key, kind, title, effect_class, created_at)
                     VALUES ('pt1','pr1','t1','local_write','写任务','local_write','t');
                    INSERT INTO plan_tasks(id, plan_revision_id, task_key, kind, title, effect_class, created_at)
                     VALUES ('pt2','pr1','t2','verification','验证任务','read','t');
                    INSERT INTO plan_task_edges(id, plan_revision_id, from_task_id, to_task_id, created_at)
                     VALUES ('pe1','pr1','pt1','pt2','t');",
                )
                .map_err(crate::Error::from)?;
                Ok(())
            })
            .unwrap();
        // attempt 幂等：同 task 同 attempt_no 拒绝。
        let dup: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "INSERT INTO plan_task_attempts(id, task_id, attempt_no, state, created_at, updated_at)
                 VALUES ('pa1','pt1',1,'running','t','t')",
                [],
            )?)
        });
        assert!(dup.is_ok());
        let dup2: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "INSERT INTO plan_task_attempts(id, task_id, attempt_no, state, created_at, updated_at)
                 VALUES ('pa2','pt1',1,'pending','t','t')",
                [],
            )?)
        });
        assert!(dup2.is_err(), "UNIQUE(task_id, attempt_no) 应拒绝重复");
        // 单活跃：同 task 第二个进行中 attempt 拒绝；终态后可再开。
        let second_active: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "INSERT INTO plan_task_attempts(id, task_id, attempt_no, state, created_at, updated_at)
                 VALUES ('pa3','pt1',2,'ready','t','t')",
                [],
            )?)
        });
        assert!(
            second_active.is_err(),
            "单活跃 partial index 应拒绝并行 attempt"
        );
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE plan_task_attempts SET state='succeeded' WHERE id='pa1'",
                    [],
                )
                .map_err(crate::Error::from)?;
                Ok(())
            })
            .unwrap();
        let next_attempt: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "INSERT INTO plan_task_attempts(id, task_id, attempt_no, state, created_at, updated_at)
                 VALUES ('pa4','pt1',2,'ready','t','t')",
                [],
            )?)
        });
        assert!(next_attempt.is_ok(), "终态后允许新 attempt");
        // 非法枚举拒绝。
        let bad_kind: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "INSERT INTO plan_tasks(id, plan_revision_id, task_key, kind, effect_class, created_at)
                 VALUES ('pt3','pr1','t3','deploy','read','t')",
                [],
            )?)
        });
        assert!(bad_kind.is_err(), "kind CHECK 应拒绝 deploy");
        let bad_effect: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "INSERT INTO plan_tasks(id, plan_revision_id, task_key, kind, effect_class, created_at)
                 VALUES ('pt4','pr1','t4','read','write','t')",
                [],
            )?)
        });
        assert!(bad_effect.is_err(), "effect_class CHECK 应拒绝 write");
        // 非法状态拒绝。
        let bad_state: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "INSERT INTO plan_task_attempts(id, task_id, attempt_no, state, created_at, updated_at)
                 VALUES ('pa5','pt2',1,'done','t','t')",
                [],
            )?)
        });
        assert!(bad_state.is_err(), "attempt state CHECK 应拒绝 done");
    }
    /// M2-05（0034）：autonomy_grants/workspace_policy_versions/task_workspaces/
    /// run_interrupts/agent_runs 冻结 refs/approvals 扩作用域。
    #[test]
    fn migration_0034_autonomy_workspace_semantics() {
        let (store, _guard) = open();
        store
            .with_conn(|c| {
                c.execute_batch(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj','u','n','p','main','t');
                    INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                     VALUES ('wi','pj','t','','[]','requirements','t','t');
                    INSERT INTO context_manifests(id, workitem_id, scope, data_policy, created_at)
                     VALUES ('ctx1','wi','{}','standard','t');
                    INSERT INTO stage_attempts(id, workitem_id, gate, attempt_no, branch_no, state, entry_snapshot_id,
                        input_package_sha256, active_output_package_id, predecessor_attempt_id, created_at, updated_at)
                     VALUES ('att1','wi','requirements',1,1,'prepared','','',NULL,NULL,'t','t');
                    INSERT INTO plan_revisions(id, workitem_id, stage_attempt_id, revision_no, status, digest, created_at, updated_at)
                     VALUES ('pr1','wi','att1',1,'draft','d','t','t');
                    INSERT INTO plan_tasks(id, plan_revision_id, task_key, kind, effect_class, created_at)
                     VALUES ('pt1','pr1','t1','local_write','local_write','t');
                    INSERT INTO plan_task_attempts(id, task_id, attempt_no, state, created_at, updated_at)
                     VALUES ('pa1','pt1',1,'ready','t','t');",
                )
                .map_err(crate::Error::from)?;
                Ok(())
            })
            .unwrap();
        // grant：合法行 + 非法状态拒绝。
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO autonomy_grants(id, workitem_id, gate_id, plan_digest, limits_json, granted_at, expires_at, created_at, updated_at)
                     VALUES ('ag1','wi','requirements','pd1','{}','t','t','t','t')",
                    [],
                )
                .map_err(crate::Error::from)?;
                Ok(())
            })
            .unwrap();
        let bad_grant: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "INSERT INTO autonomy_grants(id, status, granted_at, created_at, updated_at)
                 VALUES ('ag2','paused','t','t','t')",
                [],
            )?)
        });
        assert!(bad_grant.is_err(), "grant status CHECK 应拒绝 paused");
        // workspace policy：合法 + 非法策略拒绝。
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO workspace_policy_versions(id, strategy, sandbox_minimum, digest, created_at, updated_at)
                     VALUES ('wsp1','task_worktree','kernel_restricted','d','t','t')",
                    [],
                )
                .map_err(crate::Error::from)?;
                Ok(())
            })
            .unwrap();
        let bad_policy: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "INSERT INTO workspace_policy_versions(id, strategy, sandbox_minimum, digest, created_at, updated_at)
                 VALUES ('wsp2','unsafe','kernel_restricted','d','t','t')",
                [],
            )?)
        });
        assert!(bad_policy.is_err(), "strategy CHECK 应拒绝 unsafe");
        // task workspace：attempt 唯一 + path 唯一（两个写 task 不共享路径——EV-007）。
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO task_workspaces(id, task_attempt_id, workspace_policy_version_id, path, base_head, created_at, updated_at)
                     VALUES ('tw1','pa1','wsp1','dataDir/worktrees/wi/pr1/pa1','head1','t','t')",
                    [],
                )
                .map_err(crate::Error::from)?;
                Ok(())
            })
            .unwrap();
        let dup_attempt: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "INSERT INTO task_workspaces(id, task_attempt_id, path, created_at, updated_at)
                 VALUES ('tw2','pa1','another/path','t','t')",
                [],
            )?)
        });
        assert!(dup_attempt.is_err(), "task_attempt UNIQUE 应拒绝第二工作区");
        let dup_path: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "INSERT INTO plan_task_attempts(id, task_id, attempt_no, state, created_at, updated_at)
                 VALUES ('pa2','pt1',2,'pending','t','t')",
                [],
            )?)
        });
        assert!(dup_path.is_err(), "单活跃约束承接：同任务第二活跃拒绝");
        // approvals 扩作用域：plan_revision/autonomy_grant 可写；未知类型拒绝。
        let plan_approval: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "INSERT INTO approvals(id, subject_type, subject_id, action_digest, risk, expires_at, created_at)
                 VALUES ('apr1','plan_revision','pr1','d','medium','t','t')",
                [],
            )?)
        });
        assert!(plan_approval.is_ok());
        let bad_subject: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "INSERT INTO approvals(id, subject_type, subject_id, action_digest, risk, expires_at, created_at)
                 VALUES ('apr2','plan','pr1','d','medium','t','t')",
                [],
            )?)
        });
        assert!(
            bad_subject.is_err(),
            "approvals subject_type CHECK 应拒绝 plan"
        );
        // agent_runs 冻结 refs 列存在且可写。
        store
            .with_conn(|c| {
                c.execute_batch(
                    "INSERT INTO agent_runs(id, workitem_id, task_id, goal, input_baseline_sha, context_manifest_id,
                        tool_allowlist, budget, policy_snapshot, idempotency_key, status, plan_revision_id,
                        plan_task_attempt_id, workspace_policy_version_id, phase, created_at, updated_at)
                     VALUES ('run1','wi','','g','sha','ctx1','[]','{}','default','ik34','queued',
                        'pr1','pa1','wsp1','execution','t','t');
                    INSERT INTO run_interrupts(id, run_id, kind, question, created_at, updated_at)
                     VALUES ('ri1','run1','clarification','要不要继续？','t','t');",
                )
                .map_err(crate::Error::from)?;
                Ok(())
            })
            .unwrap();
        let bad_phase: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute("UPDATE agent_runs SET phase='idle' WHERE id='run1'", [])?)
        });
        assert!(bad_phase.is_err(), "agent_runs phase CHECK 应拒绝 idle");
    }
    /// 0042（RDWS v1.4 §1.6）：新列/新表 CHECK 语义 + 存量数据迁移验证。
    /// 迁移验证按计划要求做行数/分类数断言：手搭 schema≤41 旧库插入旧行 → 开 Store
    /// 触发 0042 → approvals/tool_proposals 行数与 mcp/builtin 分类数一致。
    #[test]
    fn migration_0042_risk_grant_semantics() {
        let (store, _guard) = open();
        seed_minimal_fixtures(&store);
        // approvals：五个新 subject_type 可写，非法值拒绝；三新列存在且默认值正确。
        for subject in [
            "mcp_import_probe",
            "mcp_import_activate",
            "gate_manual_confirm",
            "gate_skip",
            "rework",
        ] {
            store
                .with_conn(|c| {
                    Ok(c.execute(
                        "INSERT INTO approvals(id, subject_type, subject_id, action_digest, risk, expires_at, created_at)
                         VALUES (?1,?2,'s','d','low','t','t')",
                        [format!("apr-{subject}"), subject.to_string()],
                    )?)
                })
                .unwrap();
        }
        let bad_subject: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "INSERT INTO approvals(id, subject_type, subject_id, action_digest, risk, expires_at, created_at)
                 VALUES ('apr-bad','weird','s','d','low','t','t')",
                [],
            )?)
        });
        assert!(
            bad_subject.is_err(),
            "approvals subject_type CHECK 应拒绝 weird"
        );
        let (impact, scope, ver): (String, String, i64) = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT impact_digest, scope_facts_digest, digest_schema_version
                     FROM approvals WHERE id='apr-rework'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(
            (impact.as_str(), scope.as_str(), ver),
            ("", "", 0),
            "新列 legacy 默认"
        );

        // tool_proposals：send_phase/tool_provider 列 + CHECK。
        store
            .with_conn(|c| {
                Ok(c.execute(
                    "INSERT INTO tool_proposals(id, agent_run_id, tool, arguments, risk, action_digest,
                         requires_approval, decision, created_at, tool_provider, send_phase)
                     VALUES ('tp1','run1','mcp:srv:t','{}','low','d',0,'proposed','t','mcp','not_sent')",
                    [],
                )?)
            })
            .unwrap();
        let bad_phase: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "UPDATE tool_proposals SET send_phase='flushed' WHERE id='tp1'",
                [],
            )?)
        });
        assert!(bad_phase.is_err(), "send_phase CHECK 应拒绝 flushed");

        // grant_usage_ledger：dimension/state CHECK + 消费行唯一键。
        store
            .with_conn(|c| {
                Ok(c.execute(
                    "INSERT INTO grant_usage_ledger(id, grant_id, run_id, dimension, consumption_key,
                         reserved_amount, state, reserved_at)
                     VALUES ('gl1','ag1','run1','tokens_in','mc1',100,'reserved','t')",
                    [],
                )?)
            })
            .unwrap();
        let dup_consumption: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "INSERT INTO grant_usage_ledger(id, grant_id, run_id, dimension, consumption_key,
                     reserved_amount, state, reserved_at)
                 VALUES ('gl2','ag1','run1','tokens_in','mc1',50,'reserved','t')",
                [],
            )?)
        });
        assert!(
            dup_consumption.is_err(),
            "UNIQUE(grant,run,dimension,consumption) 应拒绝重复消费行"
        );
        let bad_dim: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "INSERT INTO grant_usage_ledger(id, grant_id, run_id, dimension, consumption_key,
                     reserved_amount, state, reserved_at)
                 VALUES ('gl3','ag1','run1','widgets','mc1',1,'reserved','t')",
                [],
            )?)
        });
        assert!(bad_dim.is_err(), "dimension CHECK 应拒绝 widgets");

        // gate_manual_confirmations：state CHECK + approval/action digest 唯一。
        store
            .with_conn(|c| {
                Ok(c.execute(
                    "INSERT INTO gate_manual_confirmations(id, workitem_id, gate, stage_attempt_id,
                         acceptance_item_digest, approval_id, action_digest, state, created_at, updated_at)
                     VALUES ('gmc1','wi','requirements','att1','ad1','apr-gate_skip','adg1','requested','t','t')",
                    [],
                )?)
            })
            .unwrap();
        let bad_confirm: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "UPDATE gate_manual_confirmations SET state='done' WHERE id='gmc1'",
                [],
            )?)
        });
        assert!(bad_confirm.is_err(), "confirmation state CHECK 应拒绝 done");

        // mcp_repo_imports：八态 CHECK + 内容三元组唯一。
        store
            .with_conn(|c| {
                Ok(c.execute(
                    "INSERT INTO mcp_repo_imports(id, repo_url, ref_name, pinned_sha, manifest_digest,
                         status, created_at, updated_at)
                     VALUES ('imp1','https://example.test/repo.git','main','aa','md','imported','t','t')",
                    [],
                )?)
            })
            .unwrap();
        let bad_import: Result<usize, crate::Error> = store.with_conn(|c| {
            Ok(c.execute(
                "UPDATE mcp_repo_imports SET status='cloning' WHERE id='imp1'",
                [],
            )?)
        });
        assert!(bad_import.is_err(), "import status CHECK 应拒绝 cloning");
    }

    /// 存量库升级到 0042：行数与 tool_provider 分类数迁移前后一致。
    #[test]
    fn migration_0042_populated_upgrade_preserves_rows() {
        use crate::migration::MIGRATIONS;
        let dir = tempdir::make("sg-0042-upgrade");
        // 手搭 schema≤41：逐条应用 1..=41 并登记版本（复制 runner 的登记纪律）。
        {
            let conn = rusqlite::Connection::open(dir.path().join("ratiflow-v3.db")).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);",
            )
            .unwrap();
            conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
            conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            for (v, sql) in MIGRATIONS.iter() {
                if *v > 41 {
                    break;
                }
                conn.execute_batch(sql).unwrap();
                conn.execute("INSERT INTO schema_migrations(version) VALUES (?1)", [v])
                    .unwrap();
            }
            conn.execute_batch("COMMIT").unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            // 旧形态数据：approvals 旧行 ×2、tool_proposals 含 mcp__/builtin 两类。
            conn.execute_batch(
                "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                 VALUES ('pj','u','n','p','main','t');
                 INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                 VALUES ('wi','pj','t','','[]','requirements','t','t');
                 INSERT INTO context_manifests(id, workitem_id, scope, data_policy, created_at)
                 VALUES ('ctx1','wi','{}','standard','t');
                 INSERT INTO agent_runs(id, workitem_id, task_id, goal, input_baseline_sha, context_manifest_id,
                     tool_allowlist, budget, policy_snapshot, idempotency_key, status, created_at, updated_at)
                 VALUES ('run1','wi','','g','sha','ctx1','[]','{}','default','ik','queued','t','t');
                 INSERT INTO approvals(id, subject_type, subject_id, action_digest, risk, status, expires_at, created_at)
                 VALUES ('a1','tool_proposal','s1','d','low','approved','t','t');
                 INSERT INTO approvals(id, subject_type, subject_id, action_digest, risk, status, expires_at, created_at)
                 VALUES ('a2','gate_release','s2','d','high','requested','t','t');
                 INSERT INTO tool_proposals(id, agent_run_id, tool, arguments, risk, action_digest,
                     requires_approval, decision, created_at)
                 VALUES ('t1','run1','mcp__srv__tool','{}','high','d',1,'approved','t');
                 INSERT INTO tool_proposals(id, agent_run_id, tool, arguments, risk, action_digest,
                     requires_approval, decision, created_at)
                 VALUES ('t2','run1','read_file','{}','low','d',0,'executed','t');",
            )
            .unwrap();
        }
        // 开 Store → 补跑 0042。
        let store = Store::open(dir.path(), "test").unwrap();
        let (approvals, mcp, builtin): (i64, i64, i64) = store
            .with_conn(|c| {
                Ok((
                    c.query_row("SELECT COUNT(*) FROM approvals", [], |r| r.get(0))?,
                    c.query_row(
                        "SELECT COUNT(*) FROM tool_proposals WHERE tool_provider='mcp'",
                        [],
                        |r| r.get(0),
                    )?,
                    c.query_row(
                        "SELECT COUNT(*) FROM tool_proposals WHERE tool_provider='builtin'",
                        [],
                        |r| r.get(0),
                    )?,
                ))
            })
            .unwrap();
        assert_eq!(approvals, 2, "approvals 迁移前后行数一致");
        assert_eq!((mcp, builtin), (1, 1), "存量 mcp__/builtin 分类回填精确");
        let (impact, scope): (String, String) = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT impact_digest, scope_facts_digest FROM approvals WHERE id='a1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(
            (impact.as_str(), scope.as_str()),
            ("", ""),
            "存量审批 digest 列为 legacy 空"
        );
        let phase: String = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT send_phase FROM tool_proposals WHERE id='t1'",
                    [],
                    |r| r.get(0),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(phase, "not_sent", "存量提案 send_phase 默认 not_sent");
    }

    #[test]
    fn migration_0050_rdws_mutation_receipt_domain_keys() {
        let (store, _guard) = open();
        seed_minimal_fixtures(&store);
        store
            .with_conn(|c| {
                let ins_approval = |id: &str, subject: &str, digest: &str| {
                    c.execute(
                        "INSERT INTO approvals(id, subject_type, subject_id, workitem_id,
                             stage_attempt_id, action_digest, risk, status, requested_by,
                             expires_at, reason, created_at)
                         VALUES (?1,?2,'att1','wi','att1',?3,'high','requested','local','','w','t')",
                        rusqlite::params![id, subject, digest],
                    )
                };
                // gate_skip：同 (subject_type, action_digest) 只允许一条审批行。
                ins_approval("appr_g1", "gate_skip", "sha256:a").unwrap();
                assert!(
                    ins_approval("appr_g2", "gate_skip", "sha256:a").is_err(),
                    "0050：gate_skip 同 action_digest 第二条审批必须被唯一索引拒绝"
                );
                ins_approval("appr_g3", "gate_skip", "sha256:b").unwrap();
                // partial index 只作用于 gate_skip：其他主体同 digest 不受影响。
                ins_approval("appr_r1", "gate_release", "sha256:a").unwrap();
                // （shadow_suggestions 的 digest 唯一性随 0051 表重建迁入
                //  UNIQUE(source, scope_key, suggestion_digest)，见 0051 语义测试。）
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn migration_0051_shadow_policy_v2_semantics() {
        use crate::migration::MIGRATIONS;
        let dir = tempdir::make("sg-0051-upgrade");
        // 手搭 schema≤0050 并种旧形态 shadow 建议（0043 列集，无 scope_key）。
        {
            let conn = rusqlite::Connection::open(dir.path().join("ratiflow-v3.db")).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);",
            )
            .unwrap();
            conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
            conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            for (v, sql) in MIGRATIONS.iter() {
                if *v > 50 {
                    break;
                }
                conn.execute_batch(sql).unwrap();
                conn.execute("INSERT INTO schema_migrations(version) VALUES (?1)", [v])
                    .unwrap();
            }
            conn.execute_batch("COMMIT").unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            conn.execute_batch(
                "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                 VALUES ('pj','u','n','p','main','t');
                 INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                 VALUES ('wi','pj','t','','[]','build','t','t');
                 INSERT INTO automations(id, key, interval_secs, next_fire_at, created_at, updated_at)
                 VALUES ('aut1','k1',60,'t','t','t');
                 INSERT INTO shadow_suggestions(id, source, automation_id, workitem_id, suggestion_type,
                     suggestion_digest, content_json, hypothetical_action_digest, policy_version,
                     model, prompt_version, generated_at)
                 VALUES ('sg_old1','fast_track',NULL,'wi','gate_fast_track','sha256:old1','{}','h','ft1','','','t');
                 INSERT INTO shadow_suggestions(id, source, automation_id, workitem_id, suggestion_type,
                     suggestion_digest, content_json, hypothetical_action_digest, policy_version,
                     model, prompt_version, generated_at)
                 VALUES ('sg_old2','automation','aut1',NULL,'automation_intent','sha256:old2','{}','h','sp1','','','t');
                 INSERT INTO shadow_decisions(suggestion_id, decision, decided_by, decided_at, note)
                 VALUES ('sg_old1','accepted','owner','t','存量决定必须保留');",
            )
            .unwrap();
        }
        // 升级（0051 语义在最新 schema 上验证——后续迁移不得破坏）。
        let store = Store::open(dir.path(), "test").expect("open store");
        assert!(store.schema_version().unwrap() >= 51);

        store
            .with_conn(|c| {
                // 存量回填：scope_key 推导 + legacy=1 + 决定保留。
                let (scope1, legacy1): (String, i64) = c
                    .query_row(
                        "SELECT scope_key, legacy FROM shadow_suggestions WHERE id='sg_old1'",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap();
                assert_eq!((scope1.as_str(), legacy1), ("wi", 1), "fast_track 存量 scope=workitem 且 legacy");
                let (scope2, legacy2): (String, i64) = c
                    .query_row(
                        "SELECT scope_key, legacy FROM shadow_suggestions WHERE id='sg_old2'",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap();
                assert_eq!((scope2.as_str(), legacy2), ("aut1", 1), "automation 存量 scope=automation 且 legacy");
                let decided: String = c
                    .query_row(
                        "SELECT decision FROM shadow_decisions WHERE suggestion_id='sg_old1'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(decided, "accepted", "存量决定随 FK 重建保留");

                // 新写入：scope 配对 CHECK 违约拒绝（fast_track 带 automation）。
                let bad_pair = c.execute(
                    "INSERT INTO shadow_suggestions(id, source, automation_id, workitem_id, scope_key,
                         suggestion_type, suggestion_digest, input_state_digest, generated_at)
                     VALUES ('sg_bad','fast_track','aut1',NULL,'aut1','t','sha256:b1','','t')",
                    [],
                );
                assert!(bad_pair.is_err(), "CHECK：fast_track 必须 scope=workitem 且不带 automation");

                // UNIQUE(source, scope_key, suggestion_digest)：同 scope 同 digest 拒绝；异 scope 同 digest 允许。
                let ins = |id: &str, scope: &str, digest: &str| {
                    c.execute(
                        "INSERT INTO shadow_suggestions(id, source, automation_id, workitem_id, scope_key,
                             suggestion_type, suggestion_digest, input_state_digest, generated_at)
                         VALUES (?1,'fast_track',NULL,'wi',?2,'t',?3,'sha256:st','t')",
                        rusqlite::params![id, scope, digest],
                    )
                };
                ins("sg_new1", "wi", "sha256:d1").unwrap();
                assert!(
                    ins("sg_new2", "wi", "sha256:d1").is_err(),
                    "同 (source,scope,digest) 重复拒绝"
                );

                // automations 新列默认值。
                let (cooldown, streak, metric): (String, i64, String) = c
                    .query_row(
                        "SELECT cooldown_started_at, bad_window_streak, last_metric_snapshot_digest
                         FROM automations WHERE id='aut1'",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .unwrap();
                assert_eq!((cooldown.as_str(), streak, metric.as_str()), ("", 0, ""));

                // automation_policy_transitions 唯一键。
                let ins_tr = |id: &str| {
                    c.execute(
                        "INSERT INTO automation_policy_transitions(id, automation_id, metric_snapshot_digest,
                             target_mode, expected_revision, review_coverage, sample_size, false_positives,
                             window_days, policy_version, actor, created_at)
                         VALUES (?1,'aut1','sha256:m',0,3,1.0,30,1,7,'sp2','system','t')",
                        [id],
                    )
                };
                ins_tr("tr1").unwrap();
                assert!(ins_tr("tr2").is_err(), "同 (automation,快照,目标) 重复切换拒绝");

                // notification_outbox 幂等键唯一（空串不参与）。
                let ins_note = |id: &str, key: &str| {
                    c.execute(
                        "INSERT INTO notification_outbox(id, kind, payload_json, idempotency_key, created_at)
                         VALUES (?1,'automation_blocked','{}',?2,'t')",
                        rusqlite::params![id, key],
                    )
                };
                ins_note("n1", "idem-a").unwrap();
                ins_note("n2", "").unwrap();
                ins_note("n3", "").unwrap();
                assert!(ins_note("n4", "idem-a").is_err(), "同幂等键通知拒绝（防重复通知）");
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn migration_0052_gate_skip_operations_v2() {
        use crate::migration::MIGRATIONS;
        let dir = tempdir::make("sg-0052-upgrade");
        // 手搭 schema≤0051 并种 gate_skip 审批（approved + pending 两态）。
        {
            let conn = rusqlite::Connection::open(dir.path().join("ratiflow-v3.db")).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);",
            )
            .unwrap();
            conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
            conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            for (v, sql) in MIGRATIONS.iter() {
                if *v > 51 {
                    break;
                }
                conn.execute_batch(sql).unwrap();
                conn.execute("INSERT INTO schema_migrations(version) VALUES (?1)", [v])
                    .unwrap();
            }
            conn.execute_batch("COMMIT").unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            conn.execute_batch(
                "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                 VALUES ('pj','u','n','p','main','t');
                 INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                 VALUES ('wi','pj','t','','[]','build','t','t');
                 INSERT INTO stage_attempts(id, workitem_id, gate, attempt_no, branch_no, state, entry_snapshot_id,
                     input_package_sha256, active_output_package_id, predecessor_attempt_id, created_at, updated_at)
                 VALUES ('att1','wi','build',1,1,'prepared','','',NULL,NULL,'t','t');
                 INSERT INTO approvals(id, subject_type, subject_id, workitem_id, stage_attempt_id, action_digest,
                     risk, status, requested_by, decided_by, decided_at, expires_at, reason, created_at)
                 VALUES ('appr_ok','gate_skip','att1','wi','att1','sha256:ok','high','approved','local','o','t','','w','t');
                 INSERT INTO approvals(id, subject_type, subject_id, workitem_id, stage_attempt_id, action_digest,
                     risk, status, requested_by, expires_at, reason, created_at)
                 VALUES ('appr_pd','gate_skip','att1','wi','att1','sha256:pd','high','requested','local','','w','t');",
            )
            .unwrap();
        }
        let store = Store::open(dir.path(), "test").expect("open store");
        assert!(store.schema_version().unwrap() >= 52);

        store
            .with_conn(|c| {
                // 存量 approved → legacy_completed（digest 空，只读投影）。
                let (state, cas): (String, String) = c
                    .query_row(
                        "SELECT state, current_state_digest FROM gate_skip_requests WHERE approval_id='appr_ok'",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap();
                assert_eq!((state.as_str(), cas.as_str()), ("legacy_completed", ""));
                // 存量 pending → requested 投影（可作废不可批准）。
                let state_pd: String = c
                    .query_row(
                        "SELECT state FROM gate_skip_requests WHERE approval_id='appr_pd'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(state_pd, "requested");

                // 新表唯一约束：action_digest 唯一。
                let ins = |id: &str, digest: &str| {
                    c.execute(
                        "INSERT INTO gate_skip_requests(id, workitem_id, gate, stage_attempt_id, template_version_id,
                             current_state_digest, waiver, approval_id, action_digest, requested_by, created_at, updated_at)
                         VALUES (?1,'wi','build','att1','tv','cas','w',NULL,?2,'local','t','t')",
                        rusqlite::params![id, digest],
                    )
                };
                ins("gsk1", "sha256:a").unwrap();
                assert!(ins("gsk2", "sha256:a").is_err(), "action_digest 唯一");

                // gate_fast_track_waivers：action_digest 唯一 + status CHECK。
                let wv = |id: &str, digest: &str, status: &str| {
                    c.execute(
                        "INSERT INTO gate_fast_track_waivers(id, workitem_id, gate, stage_attempt_id, policy_digest,
                             waived_kind, substitute_evidence_id, substitute_evidence_digest, rationale, action_digest,
                             status, created_by, created_at)
                         VALUES (?1,'wi','build','att1','pd','code','ev1','sd','r',?2,?3,'o','t')",
                        rusqlite::params![id, digest, status],
                    )
                };
                wv("gfw1", "sha256:w1", "active").unwrap();
                assert!(wv("gfw2", "sha256:w1", "active").is_err(), "waiver action_digest 唯一");
                assert!(wv("gfw3", "sha256:w3", "paused").is_err(), "status CHECK 只允许 active|revoked");
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn migration_0053_rework_recovery_and_baseline_correction() {
        use crate::migration::MIGRATIONS;
        let dir = tempdir::make("sg-0053-upgrade");
        // 手搭 schema≤0052 并种被 rework 污染的 baselines + 已完成 rework 操作。
        {
            let conn = rusqlite::Connection::open(dir.path().join("ratiflow-v3.db")).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);",
            )
            .unwrap();
            conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
            conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            for (v, sql) in MIGRATIONS.iter() {
                if *v > 52 {
                    break;
                }
                conn.execute_batch(sql).unwrap();
                conn.execute("INSERT INTO schema_migrations(version) VALUES (?1)", [v])
                    .unwrap();
            }
            conn.execute_batch("COMMIT").unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            conn.execute_batch(
                "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                 VALUES ('pj','u','n','p','main','t');
                 INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                 VALUES ('wi','pj','t','','[]','design','t','t');
                 INSERT INTO stage_attempts(id, workitem_id, gate, attempt_no, branch_no, state, entry_snapshot_id,
                     input_package_sha256, active_output_package_id, predecessor_attempt_id, created_at, updated_at)
                 VALUES ('att1','wi','design',1,1,'superseded','','',NULL,NULL,'t','t');
                 INSERT INTO rework_operations(id, workitem_id, from_gate, target_gate, from_attempt_id,
                     reason_code, note, current_state_digest, state, action_digest, requested_by, created_at, updated_at)
                 VALUES ('rwk1','wi','development','design','att1','regression','n','sha256:cas','completed','sha256:ad1','a','t','t');
                 INSERT INTO baselines(id, workitem_id, gate, revision_map, inputs_sha256, frozen_at)
                 VALUES ('b1','wi','design','{}','sha256:i1','t');
                 INSERT INTO baselines(id, workitem_id, gate, revision_map, inputs_sha256, frozen_at)
                 VALUES ('b2','wi','development','{}','sha256:i2','t');
                 -- 污染：rework id 被写入 successor 列。
                 UPDATE baselines SET superseded_by='rwk1' WHERE id IN ('b1','b2');",
            )
            .unwrap();
        }
        let store = Store::open(dir.path(), "test").expect("open store");
        assert!(store.schema_version().unwrap() >= 53);

        store
            .with_conn(|c| {
                // 纠偏：污染行迁 invalidated_by_rework_id，successor 列清空。
                let moved: i64 = c
                    .query_row(
                        "SELECT COUNT(*) FROM baselines WHERE invalidated_by_rework_id='rwk1' AND superseded_by IS NULL",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(moved, 2, "可确定归属的污染行全部迁移");
                // rework_operations 回填：completed → step_b_committed；pre ← current。
                let (progress, pre): (String, String) = c
                    .query_row(
                        "SELECT progress, pre_state_digest FROM rework_operations WHERE id='rwk1'",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap();
                assert_eq!((progress.as_str(), pre.as_str()), ("step_b_committed", "sha256:cas"));
                // manifest 落审计。
                let manifest: i64 = c
                    .query_row(
                        "SELECT COUNT(*) FROM audit_log WHERE action='rework_recovery_v2_manifest'
                           AND detail LIKE '%movedToInvalidated%'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(manifest, 1, "迁移 manifest 在册");
                // 新列 CHECK：非法 progress 拒绝。
                let bad = c.execute(
                    "UPDATE rework_operations SET progress='mid_air' WHERE id='rwk1'",
                    [],
                );
                assert!(bad.is_err(), "progress CHECK 只允许三态");
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn objects_gc_scan_reports_unreferenced_only() {
        let (store, _guard) = open();
        // 无引用对象 → orphan 候选；被引用对象（evidence）不在清单。
        let orphan = crate::objects::put(
            &store,
            &b"orphan-body"[..],
            crate::objects::PutOptions::default(),
        )
        .unwrap();
        let referenced = crate::objects::put(
            &store,
            &b"evidence-body"[..],
            crate::objects::PutOptions::default(),
        )
        .unwrap();
        store
            .with_conn(|c| {
                c.execute_batch(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj2','u','n','p','main','t');
                     INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                     VALUES ('wi2','pj2','t','','[]','design','t','t');",
                )?;
                c.execute(
                    "INSERT INTO evidences(id, workitem_id, gate, kind, title, object_sha256, payload, source, verified, created_at)
                     VALUES ('ev_gc','wi2','design','manual','引用',?1,'{}','local',0,'t')",
                    [&referenced.sha256],
                )?;
                Ok(())
            })
            .unwrap();
        let report = crate::objects::gc_scan(&store).unwrap();
        assert_eq!(report.total, 2);
        assert!(report.orphans.contains(&orphan.sha256), "无引用对象进清单");
        assert!(
            !report.orphans.contains(&referenced.sha256),
            "被引用对象不在清单"
        );
        assert_eq!(report.pruned, 0, "默认只报告不清理");
    }

    #[test]
    fn migration_0054_knowledge_verification_v2() {
        let (store, _guard) = open();
        assert!(store.schema_version().unwrap() >= 54);
        store
            .with_conn(|c| {
                // 旧表保留为只读历史（不 drop）。
                let legacy: i64 =
                    c.query_row("SELECT COUNT(*) FROM knowledge_verification_receipts", [], |r| r.get(0))?;
                let _ = legacy;
                // v2 表约束：mode/outcome CHECK + op_id UNIQUE。
                let seed = |id: &str, mode: &str, outcome: &str, op: &str| {
                    c.execute(
                        "INSERT INTO knowledge_verifications_v2
                         (id, project_id, source_id, stable_id, verified_input_revision, input_revision_mode,
                          verified_input_digest, outcome, verifier, verification_op_id, policy_version,
                          verified_at, evidence_ref, legacy, created_at)
                         SELECT ?1, p.id, ks.id, 'sid', ?2, ?3, ?4, ?5, 'v', ?6, 'pv', '2026-09-08T00:00:00.000Z', '', 0, '2026-09-08T00:00:00.000Z'
                         FROM projects p JOIN knowledge_sources ks ON ks.project_id = p.id
                         WHERE p.id = 'pj54' LIMIT 1",
                        rusqlite::params![id, format!("{mode}:rev"), mode, "sha", outcome, op],
                    )
                };
                // 先造 project + source 行供 FK。
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj54','u','n','p','main','t')",
                    [],
                )?;
                c.execute(
                    "INSERT INTO knowledge_sources(id, project_id, kind, name, locator, enabled, scan_state, content_sha256, created_at, updated_at)
                     VALUES ('ksrc_t','pj54','repo_path','t','docs/t.md',1,'pending','sha','t','t')",
                    [],
                )?;
                seed("kvr54a", "content_hash", "pass", "op-1").unwrap();
                assert!(
                    seed("kvr54b", "content_hash", "pass", "op-1").is_err(),
                    "verification_op_id UNIQUE（操作幂等）"
                );
                assert!(seed("kvr54c", "mid_air", "pass", "op-2").is_err(), "mode CHECK 四态");
                assert!(seed("kvr54d", "content_hash", "maybe", "op-3").is_err(), "outcome CHECK 三态");
                // 同参异 op_id = 新事件（无域 UNIQUE——改判可翻面）。
                seed("kvr54e", "content_hash", "pass", "op-4").unwrap();
                // manifest 落审计。
                let manifest: i64 = c.query_row(
                    "SELECT COUNT(*) FROM audit_log WHERE action='knowledge_verification_v2_manifest'",
                    [],
                    |r| r.get(0),
                )?;
                assert_eq!(manifest, 1, "迁移 manifest 在册（不猜测迁移的证据）");
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn migration_0055_durable_run_intents() {
        let (store, _guard) = open();
        assert!(store.schema_version().unwrap() >= 55);
        store
            .with_conn(|c| {
                c.execute_batch(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj55','u','n','p','main','t');
                     INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                     VALUES ('wi55','pj55','t','','[]','requirements','t','t');
                     INSERT INTO automations(id, key, interval_secs, next_fire_at, created_at, updated_at)
                     VALUES ('aut55','k',60,'t','t','t');",
                )?;
                // run_intents：幂等键唯一 + 状态机 CHECK。
                let ins = |id: &str, key: &str, state: &str| {
                    c.execute(
                        "INSERT INTO run_intents(id, source, automation_id, workitem_id, intent_json,
                             policy_snapshot, context_digest, grant_digest, idempotency_key, state, created_at, updated_at)
                         VALUES (?1,'automation','aut55','wi55','{}','sp2','','g',?2,?3,'t','t')",
                        rusqlite::params![id, key, state],
                    )
                };
                ins("ri1", "ri|aut55|s1", "pending").unwrap();
                assert!(ins("ri2", "ri|aut55|s1", "pending").is_err(), "idempotency_key UNIQUE");
                assert!(ins("ri3", "ri|aut55|s2", "mid_air").is_err(), "state CHECK 状态机");
                // automation_runs FK 重建后约束生效。
                c.execute(
                    "INSERT INTO automation_runs(id, automation_id, scheduled_for, receipt, status, run_intent_id, created_at)
                     VALUES ('ar1','aut55','s1','r1','intent_created','ri1','t')",
                    [],
                )?;
                let bad = c.execute(
                    "INSERT INTO automation_runs(id, automation_id, scheduled_for, receipt, status, run_intent_id, created_at)
                     VALUES ('ar2','aut55','s2','r2','fired','ri_missing','t')",
                    [],
                );
                assert!(bad.is_err(), "run_intent_id FK 生效（0055 表重建）");
                Ok(())
            })
            .unwrap();
    }


    #[test]
    fn migration_0056_workitem_search_shadow() {
        let (store, _guard) = open();
        assert!(store.schema_version().unwrap() >= 56);
        store
            .with_conn(|c| {
                c.execute_batch(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj56','u','n','p','main','t');
                     INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                     VALUES ('wi56','pj56','支付网关','描述','[]','requirements','t','t');",
                )?;
                // 影子表 FTS5 可写可查（与主表同构 trigram）。
                c.execute(
                    "INSERT INTO workitem_search_shadow(workitem_id, title, description)
                     SELECT id, title, description FROM workitems",
                    [],
                )?;
                let n: i64 = c.query_row(
                    "SELECT COUNT(*) FROM workitem_search_shadow WHERE workitem_search_shadow MATCH '支付网'",
                    [],
                    |r| r.get(0),
                )?;
                assert_eq!(n, 1, "影子表 trigram 检索可用");
                // 切换协议：主表原子替换自影子。
                c.execute("DELETE FROM workitem_search", [])?;
                c.execute(
                    "INSERT INTO workitem_search(workitem_id, title, description)
                     SELECT workitem_id, title, description FROM workitem_search_shadow",
                    [],
                )?;
                let m: i64 = c.query_row("SELECT COUNT(*) FROM workitem_search", [], |r| r.get(0))?;
                assert_eq!(m, 1);
                Ok(())
            })
            .unwrap();
    }

    /// 0058：远程 MCP 传输——CHECK 扩 ('stdio','sse','streamable-http')、
    /// headers_json 列就位、'https' 不再是合法值、静态头 JSON 可存可读。
    #[test]
    fn migration_0058_mcp_remote_transports() {
        let (store, _guard) = open();
        assert!(store.schema_version().unwrap() >= 58);
        store
            .with_conn(|c| {
                c.execute_batch(
                    "INSERT INTO mcp_servers(id, name, transport, url, headers_json, status, created_at)
                     VALUES ('m58a','sse-svc','sse','https://x/sse','[]','candidate','t');
                     INSERT INTO mcp_servers(id, name, transport, url, headers_json, status, created_at)
                     VALUES ('m58b','sh-svc','streamable-http','https://x/mcp','[]','candidate','t');
                     INSERT INTO mcp_servers(id, name, transport, command, status, created_at)
                     VALUES ('m58c','local','stdio','/bin/cat','candidate','t');",
                )?;
                c.execute(
                    "UPDATE mcp_servers SET headers_json=?1 WHERE id='m58a'",
                    [r#"[{"name":"Authorization","value":"Bearer t"}]"#],
                )?;
                let headers: String = c.query_row(
                    "SELECT headers_json FROM mcp_servers WHERE id='m58a'",
                    [],
                    |r| r.get(0),
                )?;
                assert!(headers.contains("Authorization"));
                let rejected = c.execute(
                    "INSERT INTO mcp_servers(id, name, transport, status, created_at)
                     VALUES ('m58d','legacy','https','candidate','t')",
                    [],
                );
                assert!(rejected.is_err(), "'https' 传输应被新 CHECK 拒绝");
                Ok(())
            })
            .unwrap();
    }

    fn seed_minimal_fixtures(store: &Store) {
        store
            .with_conn(|c| {
                c.execute_batch(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj','u','n','p','main','t');
                     INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                     VALUES ('wi','pj','t','','[]','requirements','t','t');
                     INSERT INTO context_manifests(id, workitem_id, scope, data_policy, created_at)
                     VALUES ('ctx1','wi','{}','standard','t');
                     INSERT INTO stage_attempts(id, workitem_id, gate, attempt_no, branch_no, state, entry_snapshot_id,
                         input_package_sha256, active_output_package_id, predecessor_attempt_id, created_at, updated_at)
                     VALUES ('att1','wi','requirements',1,1,'prepared','','',NULL,NULL,'t','t');
                     INSERT INTO autonomy_grants(id, workitem_id, gate_id, plan_digest, limits_json, granted_at, expires_at, created_at, updated_at)
                     VALUES ('ag1','wi','requirements','pd1','{}','t','t','t','t');
                     INSERT INTO agent_runs(id, workitem_id, task_id, goal, input_baseline_sha, context_manifest_id,
                         tool_allowlist, budget, policy_snapshot, idempotency_key, status, created_at, updated_at)
                     VALUES ('run1','wi','','g','sha','ctx1','[]','{}','default','ik','queued','t','t');",
                )
                .map_err(crate::Error::from)?;
                Ok(())
            })
            .unwrap();
    }
}
