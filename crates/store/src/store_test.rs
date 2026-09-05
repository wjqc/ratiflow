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
            let conn = rusqlite::Connection::open(dir.path().join("sixgates.db")).unwrap();
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
            let conn = rusqlite::Connection::open(dir.path().join("sixgates.db")).unwrap();
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

        let env = b"SIXGATES_GITLAB_TOKEN=\nSIXGATES_MODEL_BASE_URL=https://api.example.com/v1\n";
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
            let conn = rusqlite::Connection::open(dir.path().join("sixgates.db")).unwrap();
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
            let conn = rusqlite::Connection::open(dir.path().join("sixgates.db")).unwrap();
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
        let conn = rusqlite::Connection::open(dir.path().join("sixgates.db")).unwrap();
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
        let conn = rusqlite::Connection::open(dir.path().join("sixgates-v3.db")).unwrap();
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
        assert!(dir.path().join("sixgates-v3.db").exists());
        assert!(!dir.path().join("sixgates-v2.db").exists());
        assert!(!dir.path().join("sixgates.db").exists());
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
        let conn = rusqlite::Connection::open(dir.path().join("sixgates-v2.db")).unwrap();
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
        let conn = rusqlite::Connection::open(dir.path().join("sixgates-v2.db")).unwrap();
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
            let conn = rusqlite::Connection::open(dir.path().join("sixgates-v2.db")).unwrap();
            conn.execute_batch("CREATE TABLE tool_execution_outcomes (id TEXT PRIMARY KEY);")
                .unwrap();
        }
        let result = Store::open(dir.path(), "test");
        assert!(result.is_err(), "迁移失败必须拒绝启动");
        assert!(
            !dir.path().join("sixgates-v3.db").exists(),
            "半成品 v3 必须删除"
        );
        assert!(dir.path().join("sixgates-v2.db").exists(), "v2 必须保留");
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
}
