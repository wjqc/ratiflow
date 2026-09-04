//! sg-context：统一 Context Manifest owner（ADR-032 / 实施方案 v1.0 §5.1/§7.3）。
//!
//! 职责：知识、附件与项目记忆的 manifest 创建/读取/内容块装载。
//! 依赖 sg-store、sg-knowledge（检索职责仍在彼处）、sg-memory；不依赖 sg-agent。
//! 两条 Run 入口（agent.start / stage.startActivity）都必须经由本 crate 的
//! `builder::build_manifest`，禁止客户端自报关键绑定。
pub mod blocks;
pub mod builder;
pub mod manifest;

pub use builder::{build_manifest, BuildInput};

#[cfg(test)]
mod tests {
    use sg_store::Store;

    fn setup() -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "sg-ctx-{}-{}",
            std::process::id(),
            sg_store::ids::new_id("t")
        ));
        let store = Store::open(&dir, "test").unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj', 'u', 'n', 'p', 'main', ?1)",
                    [sg_store::timefmt::now()],
                )?;
                c.execute(
                    "INSERT INTO workitems(id, project_id, title, created_at, updated_at)
                     VALUES ('wi', 'pj', 't', ?1, ?1)",
                    [sg_store::timefmt::now()],
                )?;
                Ok(())
            })
            .unwrap();
        (store, dir)
    }

    /// 统一 builder：manifest 落库 + 记忆 included/excluded 证据同事务写入（§7.3）。
    #[test]
    fn build_manifest_freezes_memory_selection() {
        let (store, dir) = setup();
        let _ = &dir;
        // 一条 active 记忆（默认 flag off → excluded/disabled 也应留证据）。
        let created = sg_memory::mutation::create(
            &store,
            &sg_memory::CreateInput {
                project_id: "pj".into(),
                title: "缓存结论".into(),
                kind: "lesson".into(),
                body: "构建缓存策略说明。".into(),
                summary: None,
                tags: vec![],
                source_refs: vec![],
                target_status: "active",
                actor: "tester".into(),
                idempotency_key: sg_store::ids::new_id("k"),
                on_duplicate: sg_memory::DuplicateMode::Reject,
            },
        )
        .unwrap();
        let memory_id = created["memoryId"].as_str().unwrap().to_string();

        let manifest = crate::build_manifest(
            &store,
            &crate::BuildInput {
                project_id: "pj",
                workitem_id: "wi",
                goal: "缓存",
                selected_sources: &[],
            },
        )
        .unwrap();
        assert!(manifest["id"].as_str().unwrap().starts_with("ctx_"));
        assert_eq!(manifest["memory"]["included"], 0);
        assert_eq!(manifest["memory"]["excluded"], 1);

        // flag/项目关闭 → excluded=disabled；冻结行不可漂移。
        let rows: i64 = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM context_manifest_memories WHERE manifest_id = ?1 AND reason = 'disabled'",
                    [manifest["id"].as_str().unwrap()],
                    |r| r.get(0),
                )
                .map_err(sg_store::Error::from)
            })
            .unwrap();
        assert_eq!(rows, 1);

        // 开启后再次构建：included=1，且冻结 revision 与 entry 一致。
        sg_memory::set_feature_enabled(&store, true, "tester").unwrap();
        sg_memory::settings_update(
            &store,
            "pj",
            &sg_memory::SettingsPatch {
                enabled: Some(true),
                ..Default::default()
            },
            1,
            &sg_store::ids::new_id("k"),
        )
        .unwrap();
        let manifest2 = crate::build_manifest(
            &store,
            &crate::BuildInput {
                project_id: "pj",
                workitem_id: "wi",
                goal: "缓存",
                selected_sources: &[],
            },
        )
        .unwrap();
        assert_eq!(manifest2["memory"]["included"], 1);
        let ev = crate::blocks::memory_evidence(&store, manifest2["id"].as_str().unwrap()).unwrap();
        assert_eq!(ev["count"], 1);
        assert_eq!(ev["ids"][0].as_str(), Some(memory_id.as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 归属验证：workitem 不属于该项目 → 拒绝（MEM-010）。
    #[test]
    fn build_manifest_rejects_cross_project() {
        let (store, dir) = setup();
        let result = crate::build_manifest(
            &store,
            &crate::BuildInput {
                project_id: "pj_other",
                workitem_id: "wi",
                goal: "x",
                selected_sources: &[],
            },
        );
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
