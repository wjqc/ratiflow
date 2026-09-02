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
}
