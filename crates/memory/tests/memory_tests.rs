//! sg-memory 单测（实施方案 §14.1 M1 子集）：
//! 迁移、不可变修订、幂等/CAS、项目隔离、Secret fail-closed、FTS、
//! 生命周期、冲突、purge（共享对象/漂移/墓碑）、孤儿对象、备份统计、导入导出、上下文选择。
use serde_json::{json, Value};

use sg_memory as mem;
use sg_memory::model::CreateInput;
use sg_store::{objects, Store};

struct Temp {
    path: std::path::PathBuf,
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn tempdir(tag: &str) -> Temp {
    let path = std::env::temp_dir().join(format!(
        "{tag}-{}-{}",
        std::process::id(),
        sg_store::ids::new_id("t")
    ));
    std::fs::create_dir_all(&path).unwrap();
    Temp { path }
}

fn open_store(tag: &str) -> (Store, Temp) {
    let t = tempdir(tag);
    let store = Store::open(&t.path, "test").unwrap();
    (store, t)
}

fn add_project(store: &Store, id: &str) {
    store
        .with_conn(|c| {
            c.execute(
                "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                 VALUES (?1, 'u', 'ns-' || ?1, 'prj-' || ?1, 'main', ?2)",
                rusqlite::params![id, sg_store::timefmt::now()],
            )?;
            Ok(())
        })
        .unwrap();
}

fn create_simple(store: &Store, project: &str, title: &str, body: &str) -> Value {
    create_with_key(store, project, title, body, &sg_store::ids::new_id("k"))
}

fn create_with_key(store: &Store, project: &str, title: &str, body: &str, key: &str) -> Value {
    let input = CreateInput {
        project_id: project.to_string(),
        title: title.to_string(),
        kind: "lesson".into(),
        body: body.to_string(),
        summary: None,
        tags: vec!["test".into()],
        source_refs: vec![],
        target_status: "active",
        actor: "tester".into(),
        idempotency_key: key.to_string(),
        on_duplicate: mem::model::DuplicateMode::Reject,
    };
    mem::mutation::create(store, &input).unwrap()
}

fn assert_err_token<T>(result: Result<T, sg_store::Error>, token: &str) {
    let msg = result.err().expect("期望失败").to_string();
    assert!(msg.starts_with(token), "期望错误 token {token}，实际 {msg}");
}

/// 迁移：空库直达最新版本（v24）；外键检查通过（migration::run 内置）。
#[test]
fn migration_empty_db_reaches_v23() {
    let (store, _t) = open_store("sg-mem-mig");
    assert_eq!(store.schema_version().unwrap(), 25);
    // v23 表存在。
    let n: i64 = store
        .with_conn(|c| {
            let mut total = 0;
            for table in [
                "project_memory_settings",
                "memory_entries",
                "memory_revisions",
                "memory_source_refs",
                "context_manifest_memories",
                "memory_capture_jobs",
                "memory_candidates",
                "memory_mutation_receipts",
            ] {
                total += c.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |r| r.get::<_, i64>(0),
                )?;
            }
            Ok(total)
        })
        .unwrap();
    assert_eq!(n, 8);
}

/// 迁移：populated v22 → 增量升级到最新。
#[test]
fn migration_populated_v22_to_v23() {
    let t = tempdir("sg-mem-mig22");
    {
        let conn = rusqlite::Connection::open(t.path.join("sixgates.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);",
        )
        .unwrap();
        for (v, body) in sg_store::migration::MIGRATIONS
            .iter()
            .filter(|(v, _)| *v <= 22)
        {
            conn.execute_batch(body)
                .map_err(|e| panic!("migration {v}: {e}"))
                .unwrap();
            conn.execute("INSERT INTO schema_migrations(version) VALUES (?1)", [v])
                .unwrap();
        }
        add_project_raw(&conn, "pj");
    }
    let store = Store::open(&t.path, "test").unwrap();
    assert_eq!(store.schema_version().unwrap(), 25);
    store.quick_check().unwrap();
}

fn add_project_raw(conn: &rusqlite::Connection, id: &str) {
    conn.execute(
        "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
         VALUES (?1, 'u', 'n', 'p', 'main', '2026-01-01T00:00:00.000Z')",
        [id],
    )
    .unwrap();
}

/// 不可变修订：update 新建 revision、旧 revision 正文可查且不变（MEM-004）。
#[test]
fn update_creates_new_revision_old_body_intact() {
    let (store, _t) = open_store("sg-mem-rev");
    add_project(&store, "pj");
    let created = create_simple(&store, "pj", "首版结论", "本地优先是权威方案。");
    let mem_id = created["memoryId"].as_str().unwrap();
    let rev1 = created["revisionId"].as_str().unwrap();

    let detail1 = mem::repository::detail(&store, "pj", mem_id, None).unwrap();
    assert_eq!(detail1["revisionNo"], 1);
    assert_eq!(detail1["body"], "本地优先是权威方案。");

    let updated = mem::mutation::update(
        &store,
        &mem::UpdateInput {
            project_id: "pj".into(),
            memory_id: mem_id.into(),
            title: None,
            body: Some("修订后的结论，跨机后置。".into()),
            summary: None,
            tags: None,
            expected_revision: 1,
            actor: "tester".into(),
            idempotency_key: sg_store::ids::new_id("k"),
        },
    )
    .unwrap();
    assert_eq!(updated["revisionNo"], 2);

    // 当前 = 新正文；指定旧 revision 仍可读原正文。
    let detail_now = mem::repository::detail(&store, "pj", mem_id, None).unwrap();
    assert_eq!(detail_now["body"], "修订后的结论，跨机后置。");
    let detail_old = mem::repository::detail(&store, "pj", mem_id, Some(rev1)).unwrap();
    assert_eq!(detail_old["body"], "本地优先是权威方案。");

    // entry metadata revision 已 +1（乐观锁位）。
    assert_eq!(detail_now["revision"], 2);
    // 修订历史有两条。
    assert_eq!(detail_now["revisions"].as_array().unwrap().len(), 2);
}

/// 幂等：同 key 同内容重放返回原结果；同 key 异内容 MEMORY_CONFLICT（MEM-005/006）。
#[test]
fn idempotency_replay_and_conflict() {
    let (store, _t) = open_store("sg-mem-idem");
    add_project(&store, "pj");
    let key = sg_store::ids::new_id("k");
    let first = create_with_key(&store, "pj", "幂等条目", "内容 A。", &key);
    let replay = create_with_key(&store, "pj", "幂等条目", "内容 A。", &key);
    assert_eq!(first["memoryId"], replay["memoryId"]);

    // 同 key 异内容：fingerprint 冲突，且不产生第二个 revision。
    let conflict = mem::mutation::create(
        &store,
        &CreateInput {
            project_id: "pj".into(),
            title: "幂等条目".into(),
            kind: "lesson".into(),
            body: "内容 B 不同。".into(),
            summary: None,
            tags: vec!["test".into()],
            source_refs: vec![],
            target_status: "active",
            actor: "tester".into(),
            idempotency_key: key.clone(),
            on_duplicate: mem::model::DuplicateMode::Reject,
        },
    );
    assert_err_token(conflict, "memory_conflict");
    let n: i64 = store
        .with_conn(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM memory_revisions WHERE memory_id = ?1",
                [first["memoryId"].as_str().unwrap()],
                |r| r.get(0),
            )
            .map_err(sg_store::Error::from)
        })
        .unwrap();
    assert_eq!(n, 1);
}

/// 并发 update：两个同 expectedRevision 的更新恰好一个成功（CAS，MEM-005）。
#[test]
fn concurrent_update_exactly_one_cas_success() {
    let (store, _t) = open_store("sg-mem-cas");
    add_project(&store, "pj");
    let created = create_simple(&store, "pj", "并发条目", "初始内容。");
    let mem_id = created["memoryId"].as_str().unwrap().to_string();
    let store = std::sync::Arc::new(store);

    let mut handles = Vec::new();
    for i in 0..2 {
        let store = store.clone();
        let mem_id = mem_id.clone();
        handles.push(std::thread::spawn(move || {
            mem::mutation::update(
                &store,
                &mem::UpdateInput {
                    project_id: "pj".into(),
                    memory_id: mem_id,
                    title: None,
                    body: Some(format!("并发写 {i}。")),
                    summary: None,
                    tags: None,
                    expected_revision: 1,
                    actor: "tester".into(),
                    idempotency_key: sg_store::ids::new_id("k"),
                },
            )
        }));
    }
    let mut ok = 0;
    let mut conflict = 0;
    for h in handles {
        match h.join().unwrap() {
            Ok(_) => ok += 1,
            Err(e) => {
                assert!(e.to_string().starts_with("memory_conflict"));
                conflict += 1;
            }
        }
    }
    assert_eq!((ok, conflict), (1, 1));
}

/// 项目隔离：跨项目不可枚举/读取（MEM-010）。
#[test]
fn project_isolation() {
    let (store, _t) = open_store("sg-mem-iso");
    add_project(&store, "pj_a");
    add_project(&store, "pj_b");
    let created = create_simple(&store, "pj_a", "A 项目机密", "仅 A 可见。");
    let mem_id = created["memoryId"].as_str().unwrap();

    assert_err_token(
        mem::repository::detail(&store, "pj_b", mem_id, None),
        "not_found",
    );
    let list_b = mem::repository::list(&store, "pj_b", None, None, None, None, 50).unwrap();
    assert_eq!(list_b["items"].as_array().unwrap().len(), 0);
    let search_b = mem::repository::search(&store, "pj_b", "仅 A 可见", None, 50).unwrap();
    assert_eq!(search_b["items"].as_array().unwrap().len(), 0);
    // purgePreview 同样隔离。
    assert_err_token(mem::purge::preview(&store, "pj_b", mem_id), "not_found");
}

/// Secret fail-closed：拒绝后无 entry/revision/FTS/outbox/receipt 副作用（MEM-003/§14.3-10）。
#[test]
fn secret_rejected_without_side_effects() {
    let (store, _t) = open_store("sg-mem-secret");
    add_project(&store, "pj");
    let secret_body = "配置如下：\npassword = \"supersecret123\"\n";
    let key = sg_store::ids::new_id("k");
    let result = mem::mutation::create(
        &store,
        &CreateInput {
            project_id: "pj".into(),
            title: "含密条目".into(),
            kind: "fact".into(),
            body: secret_body.into(),
            summary: None,
            tags: vec![],
            source_refs: vec![],
            target_status: "active",
            actor: "tester".into(),
            idempotency_key: key.clone(),
            on_duplicate: mem::model::DuplicateMode::Reject,
        },
    );
    assert_err_token(result, "memory_secret_detected");

    let (entries, revisions, fts, outbox, receipts): (i64, i64, i64, i64, i64) = store
        .with_conn(|c| {
            Ok((
                c.query_row("SELECT COUNT(*) FROM memory_entries", [], |r| r.get(0))?,
                c.query_row("SELECT COUNT(*) FROM memory_revisions", [], |r| r.get(0))?,
                c.query_row("SELECT COUNT(*) FROM memory_fts", [], |r| r.get(0))?,
                c.query_row(
                    "SELECT COUNT(*) FROM events_outbox WHERE type LIKE 'memory.%'",
                    [],
                    |r| r.get(0),
                )?,
                c.query_row("SELECT COUNT(*) FROM memory_mutation_receipts", [], |r| {
                    r.get(0)
                })?,
            ))
        })
        .unwrap();
    assert_eq!((entries, revisions, fts, outbox, receipts), (0, 0, 0, 0, 0));
}

/// FTS：中文、英文、特殊转义字符、空 query（MEM-011）。
#[test]
fn fts_search_multilang_and_special_chars() {
    let (store, _t) = open_store("sg-mem-fts");
    add_project(&store, "pj");
    create_simple(
        &store,
        "pj",
        "中文检索结论",
        "F TS 中文分词依赖 trigram，检索应当命中。",
    );
    create_simple(
        &store,
        "pj",
        "english note",
        "trigram tokenizer powers retrieval.",
    );

    let hit_cn = mem::repository::search(&store, "pj", "中文分词", None, 10).unwrap();
    assert_eq!(hit_cn["items"].as_array().unwrap().len(), 1);
    let hit_en = mem::repository::search(&store, "pj", "tokenizer", None, 10).unwrap();
    assert_eq!(hit_en["items"].as_array().unwrap().len(), 1);
    // 特殊字符不进 FTS 语法层：引号/百分号安全。
    let weird = mem::repository::search(&store, "pj", "\"%分词*(", None, 10).unwrap();
    assert!(weird["items"].is_array());
    let weird2 = mem::repository::search(&store, "pj", "trigram \"tokenizer", None, 10).unwrap();
    assert_eq!(weird2["items"].as_array().unwrap().len(), 1);
    // 空 query → 空 items（列表由 memory.list 兜底）。
    let empty = mem::repository::search(&store, "pj", "   ", None, 10).unwrap();
    assert_eq!(empty["items"].as_array().unwrap().len(), 0);
}

/// 置顶排序 + 默认列表（MEM-007）。
#[test]
fn pin_affects_order_only() {
    let (store, _t) = open_store("sg-mem-pin");
    add_project(&store, "pj");
    let a = create_simple(&store, "pj", "普通条目", "普通内容。");
    let b = create_simple(&store, "pj", "置顶条目", "置顶内容。");
    let b_id = b["memoryId"].as_str().unwrap().to_string();
    let b_rev = b["revisionNo"].as_i64().unwrap();
    let entry_rev: i64 = store
        .with_conn(|c| {
            c.query_row(
                "SELECT revision FROM memory_entries WHERE id = ?1",
                [&b_id],
                |r| r.get(0),
            )
            .map_err(sg_store::Error::from)
        })
        .unwrap();
    mem::mutation::pin(
        &store,
        "pj",
        &b_id,
        true,
        entry_rev,
        "tester",
        &sg_store::ids::new_id("k"),
    )
    .unwrap();

    let list = mem::repository::list(&store, "pj", None, None, None, None, 50).unwrap();
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], json!(b_id));
    let _ = (a, b_rev);
}

/// 归档/恢复生命周期 + 项目归档门（MEM-008/027）。
#[test]
fn archive_restore_and_archived_project_guard() {
    let (store, _t) = open_store("sg-mem-life");
    add_project(&store, "pj");
    let created = create_simple(&store, "pj", "生命周期条目", "内容。");
    let mem_id = created["memoryId"].as_str().unwrap().to_string();

    // 归档后 context 选择不包含。
    let entry_rev = entry_revision(&store, &mem_id);
    let archived = mem::mutation::archive(
        &store,
        "pj",
        &mem_id,
        entry_rev,
        "tester",
        &sg_store::ids::new_id("k"),
    )
    .unwrap();
    assert_eq!(archived["status"], "archived");
    let (included, _) =
        mem::retrieval::select_for_context(&store, "pj", &["内容".to_string()]).unwrap();
    assert_eq!(included.len(), 0);

    // 归档状态下禁止 update（需先恢复）。
    let entry_rev = entry_revision(&store, &mem_id);
    assert_err_token(
        mem::mutation::update(
            &store,
            &mem::UpdateInput {
                project_id: "pj".into(),
                memory_id: mem_id.clone(),
                title: None,
                body: Some("不应成功".into()),
                summary: None,
                tags: None,
                expected_revision: entry_rev,
                actor: "tester".into(),
                idempotency_key: sg_store::ids::new_id("k"),
            },
        ),
        "memory_invalid_state",
    );

    // 恢复 → active；更新随之放行。
    let entry_rev = entry_revision(&store, &mem_id);
    let restored = mem::mutation::restore(
        &store,
        "pj",
        &mem_id,
        entry_rev,
        "tester",
        &sg_store::ids::new_id("k"),
    )
    .unwrap();
    assert_eq!(restored["status"], "active");

    // 项目归档后写操作 fail-closed。
    store
        .with_conn(|c| {
            c.execute(
                "UPDATE projects SET archived_at = ?1 WHERE id = 'pj'",
                [sg_store::timefmt::now()],
            )?;
            Ok(())
        })
        .unwrap();
    assert_err_token(
        mem::mutation::archive(
            &store,
            "pj",
            &mem_id,
            entry_revision(&store, &mem_id),
            "tester",
            &sg_store::ids::new_id("k"),
        ),
        "memory_disabled",
    );
}

fn entry_revision(store: &Store, memory_id: &str) -> i64 {
    store
        .with_conn(|c| {
            c.query_row(
                "SELECT revision FROM memory_entries WHERE id = ?1",
                [memory_id],
                |r| r.get(0),
            )
            .map_err(sg_store::Error::from)
        })
        .unwrap()
}

/// 同主题冲突：两个 active 异内容互标 conflicted 且不注入；裁决后可恢复（§8.3）。
#[test]
fn subject_conflict_marks_both_and_blocks_injection() {
    let (store, _t) = open_store("sg-mem-conflict");
    add_project(&store, "pj");
    create_simple(&store, "pj", "部署顺序", "先构建后部署。");
    create_simple(&store, "pj", "部署顺序", "先部署后构建。");

    let counts = store
        .with_conn(|c| mem::repository::counts(c, "pj"))
        .unwrap();
    assert_eq!(counts["conflicted"], json!(2));
    assert_eq!(counts["active"], json!(0));

    let (_, excluded) =
        mem::retrieval::select_for_context(&store, "pj", &["部署".to_string()]).unwrap();
    assert_eq!(excluded.len(), 2);
    assert!(excluded.iter().all(|e| e["reason"] == "conflicted"));

    // 裁决：归档其一 → 另一个 restore 回 active。
    let ids: Vec<String> = store
        .with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id FROM memory_entries WHERE project_id='pj' AND status='conflicted' ORDER BY id",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
        .unwrap();
    mem::mutation::archive(
        &store,
        "pj",
        &ids[0],
        entry_revision(&store, &ids[0]),
        "tester",
        &sg_store::ids::new_id("k"),
    )
    .unwrap();
    let restored = mem::mutation::restore(
        &store,
        "pj",
        &ids[1],
        entry_revision(&store, &ids[1]),
        "tester",
        &sg_store::ids::new_id("k"),
    )
    .unwrap();
    assert_eq!(restored["status"], "active");
}

/// purge 全流程：token 两步、墓碑、FTS 清除、token 单次使用（MEM-009/§6.3）。
#[test]
fn purge_flow_with_token_and_tombstone() {
    let (store, _t) = open_store("sg-mem-purge");
    add_project(&store, "pj");
    let created = create_simple(&store, "pj", "待清除条目", "即将被清除的正文。");
    let mem_id = created["memoryId"].as_str().unwrap().to_string();

    let preview = mem::purge::preview(&store, "pj", &mem_id).unwrap();
    assert_eq!(preview["canPurge"], json!(true));
    let token = preview["confirmationToken"].as_str().unwrap().to_string();

    // 错误 revision → conflict。
    assert_err_token(
        mem::purge::purge(
            &store,
            "pj",
            &mem_id,
            999,
            &token,
            &sg_store::ids::new_id("k"),
        ),
        "memory_conflict",
    );
    // 正确 purge。
    let result = mem::purge::purge(
        &store,
        "pj",
        &mem_id,
        1,
        &token,
        &sg_store::ids::new_id("k"),
    )
    .unwrap();
    assert_eq!(result["status"], "purged");

    // 墓碑：正文为空、objectSha 为空、contentSha 保留。
    let detail = mem::repository::detail(&store, "pj", &mem_id, None).unwrap();
    assert_eq!(detail["status"], "purged");
    assert!(detail["body"].is_null());
    assert!(detail["objectSha256"].is_null());
    assert!(!detail["contentSha256"].as_str().unwrap().is_empty());

    // FTS 已清除。
    let hit = mem::repository::search(&store, "pj", "即将被清除", None, 10).unwrap();
    assert_eq!(hit["items"].as_array().unwrap().len(), 0);

    // token 单次使用：已被删除 → 再次 purge 被拒（purge_blocked 优先于状态检查）。
    assert_err_token(
        mem::purge::purge(
            &store,
            "pj",
            &mem_id,
            entry_revision(&store, &mem_id),
            &token,
            &sg_store::ids::new_id("k"),
        ),
        "memory_purge_blocked",
    );
}

/// purge 共享对象：被知识库引用时 blocked 且不发 token（§10.3）。
#[test]
fn purge_blocked_by_shared_object() {
    let (store, _t) = open_store("sg-mem-share");
    add_project(&store, "pj");
    let created = create_simple(&store, "pj", "共享正文条目", "这段正文同时被知识库引用。");
    let mem_id = created["memoryId"].as_str().unwrap().to_string();
    let sha: String = store
        .with_conn(|c| {
            c.query_row(
                "SELECT object_sha256 FROM memory_revisions WHERE memory_id = ?1",
                [&mem_id],
                |r| r.get(0),
            )
            .map_err(sg_store::Error::from)
        })
        .unwrap();

    // 知识库分块引用同一对象。
    store
        .with_conn(|c| {
            c.execute(
                "INSERT INTO knowledge_sources(id, project_id, kind, name, locator, created_at, updated_at)
                 VALUES ('ks_x', 'pj', 'document', 'n', 'l', ?1, ?1)",
                [sg_store::timefmt::now()],
            )?;
            c.execute(
                "INSERT INTO knowledge_chunks(id, source_id, ordinal, object_sha256, project_id) VALUES ('kc_x', 'ks_x', 0, ?1, 'pj')",
                [&sha],
            )?;
            Ok(())
        })
        .unwrap();

    let preview = mem::purge::preview(&store, "pj", &mem_id).unwrap();
    assert_eq!(preview["canPurge"], json!(false));
    assert!(preview["confirmationToken"].is_null());
    assert_eq!(preview["blockers"].as_array().unwrap().len(), 1);
    assert!(preview["backupRefs"]["note"]
        .as_str()
        .unwrap()
        .contains("备份"));
}

/// purge 影响漂移：预览后条目 revision 变化 → token 绑定 digest 不匹配 → blocked。
#[test]
fn purge_blocked_on_impact_drift() {
    let (store, _t) = open_store("sg-mem-drift");
    add_project(&store, "pj");
    let created = create_simple(&store, "pj", "漂移条目", "内容。");
    let mem_id = created["memoryId"].as_str().unwrap().to_string();
    let preview = mem::purge::preview(&store, "pj", &mem_id).unwrap();
    let token = preview["confirmationToken"].as_str().unwrap().to_string();

    // 预览后置顶（revision +1，影响 digest 变化）。
    mem::mutation::pin(
        &store,
        "pj",
        &mem_id,
        true,
        1,
        "tester",
        &sg_store::ids::new_id("k"),
    )
    .unwrap();
    assert_err_token(
        mem::purge::purge(
            &store,
            "pj",
            &mem_id,
            2,
            &token,
            &sg_store::ids::new_id("k"),
        ),
        "memory_purge_blocked",
    );
}

/// 对象先写、事务失败 → 孤儿对象可被引用扫描发现（§6.8 补偿路径）。
#[test]
fn orphan_object_after_failed_transaction() {
    let (store, _t) = open_store("sg-mem-orphan");
    add_project(&store, "pj");
    // 项目归档：对象写入成功、事务内项目校验失败。
    store
        .with_conn(|c| {
            c.execute(
                "UPDATE projects SET archived_at = ?1 WHERE id = 'pj'",
                [sg_store::timefmt::now()],
            )?;
            Ok(())
        })
        .unwrap();
    let body = "孤儿对象正文。";
    let result = mem::mutation::create(
        &store,
        &CreateInput {
            project_id: "pj".into(),
            title: "不会存在的条目".into(),
            kind: "fact".into(),
            body: body.into(),
            summary: None,
            tags: vec![],
            source_refs: vec![],
            target_status: "active",
            actor: "tester".into(),
            idempotency_key: sg_store::ids::new_id("k"),
            on_duplicate: mem::model::DuplicateMode::Reject,
        },
    );
    assert_err_token(result, "memory_disabled");

    // 对象已入库但零引用：GC 可安全清理。
    use sha2::Digest as _;
    let sum = sg_store::ids::hex(&sha2::Sha256::digest(body.as_bytes()));
    let obj_exists: bool = store
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM objects WHERE sha256 = ?1",
                [&sum],
                |r| r.get::<_, i64>(0),
            )? > 0)
        })
        .unwrap();
    assert!(obj_exists, "对象应已写入（补偿路径前提）");
    let entries: i64 = store
        .with_conn(|c| {
            c.query_row("SELECT COUNT(*) FROM memory_entries", [], |r| r.get(0))
                .map_err(sg_store::Error::from)
        })
        .unwrap();
    assert_eq!(entries, 0);
}

/// FTS rebuild：可重复执行；对象缺失如实上报 corrupt（§6.5）。
#[test]
fn rebuild_index_reports_corruption() {
    let (store, t) = open_store("sg-mem-rebuild");
    add_project(&store, "pj");
    let created = create_simple(&store, "pj", "重建条目", "重建正文内容。");
    let mem_id = created["memoryId"].as_str().unwrap().to_string();

    let report = mem::repository::rebuild_index(&store, Some("pj")).unwrap();
    assert_eq!(report["rebuilt"], json!(1));
    assert_eq!(report["corrupt"].as_array().unwrap().len(), 0);

    // 删除对象文件 → rebuild 报 corrupt。
    let sha: String = store
        .with_conn(|c| {
            c.query_row(
                "SELECT object_sha256 FROM memory_revisions WHERE memory_id = ?1",
                [&mem_id],
                |r| r.get(0),
            )
            .map_err(sg_store::Error::from)
        })
        .unwrap();
    let path = objects::object_path(&store, &sha);
    std::fs::remove_file(&path).unwrap();
    let report = mem::repository::rebuild_index(&store, Some("pj")).unwrap();
    assert_eq!(report["rebuilt"], json!(0));
    assert_eq!(report["corrupt"].as_array().unwrap().len(), 1);
    let _ = t;
}

/// 备份 manifest 包含 memory rows/objects 统计（M1 任务）。
#[test]
fn backup_manifest_counts_memory() {
    let (store, _t) = open_store("sg-mem-backup");
    add_project(&store, "pj");
    create_simple(&store, "pj", "备份统计条目", "内容。");
    let snap = sg_store::backup::snapshot(&store).unwrap();
    assert_eq!(snap.manifest["memoryEntries"], json!(1));
    assert_eq!(snap.manifest["memoryObjects"], json!(1));
    let (entries, objects) = mem::stats(&store).unwrap();
    assert_eq!((entries, objects), (1, 1));
}

/// 设置 CAS、范围校验与 feature flag 默认值（MEM-001/§6.1）。
#[test]
fn settings_cas_validation_and_default_flag() {
    let (store, _t) = open_store("sg-mem-settings");
    add_project(&store, "pj");
    // feature flag 默认 false。
    assert!(!mem::repository::feature_enabled(&store).unwrap());

    let s0 = mem::repository::settings_get(&store, "pj").unwrap();
    assert!(!s0.enabled);
    assert_eq!(s0.max_entries, 8);
    assert_eq!(s0.max_bytes, 12288);

    let s1 = mem::settings_update(
        &store,
        "pj",
        &mem::SettingsPatch {
            enabled: Some(true),
            max_entries: Some(4),
            ..Default::default()
        },
        s0.revision,
        &sg_store::ids::new_id("k"),
    )
    .unwrap();
    assert!(s1.enabled);
    assert_eq!(s1.max_entries, 4);
    assert_eq!(s1.revision, 2);

    // 旧 expectedRevision → conflict。
    assert_err_token(
        mem::settings_update(
            &store,
            "pj",
            &mem::SettingsPatch {
                enabled: Some(false),
                ..Default::default()
            },
            s0.revision,
            &sg_store::ids::new_id("k"),
        ),
        "memory_conflict",
    );
    // 范围校验。
    assert_err_token(
        mem::settings_update(
            &store,
            "pj",
            &mem::SettingsPatch {
                max_entries: Some(99),
                ..Default::default()
            },
            s1.revision,
            &sg_store::ids::new_id("k"),
        ),
        "memory_quota_exceeded",
    );
}

/// 上下文选择：disabled/stale/预算排序与确定性（§7.1）。
#[test]
fn context_selection_reasons_and_budget() {
    let (store, _t) = open_store("sg-mem-ctx");
    add_project(&store, "pj");
    // 先建条目（同主题异内容各一条，分数可区分）。
    create_simple(&store, "pj", "缓存策略", "构建缓存策略说明。");
    create_simple(&store, "pj", "缓存清理", "定期清理构建产物。");
    // 默认 flag off + 项目未开启 → 全部 disabled。
    let (included, excluded) =
        mem::retrieval::select_for_context(&store, "pj", &["缓存".to_string()]).unwrap();
    assert_eq!(included.len(), 0);
    assert_eq!(excluded.len(), 2);
    assert!(excluded.iter().all(|e| e["reason"] == "disabled"));

    // 开启 flag 与项目。
    mem::set_feature_enabled(&store, true, "tester").unwrap();
    mem::settings_update(
        &store,
        "pj",
        &mem::SettingsPatch {
            enabled: Some(true),
            max_entries: Some(1),
            ..Default::default()
        },
        1,
        &sg_store::ids::new_id("k"),
    )
    .unwrap();

    let (included, excluded) =
        mem::retrieval::select_for_context(&store, "pj", &["缓存".to_string()]).unwrap();
    // max_entries=1：两条命中只有一条入选（分数高者），另一条 over_budget。
    assert_eq!(included.len(), 1);
    assert_eq!(included[0]["title"], "缓存策略");
    let over: Vec<&Value> = excluded
        .iter()
        .filter(|e| e["reason"] == "over_budget")
        .collect();
    assert_eq!(over.len(), 1);
    assert_eq!(over[0]["title"], "缓存清理");
}

/// 过期（stale）条目被排除。
#[test]
fn context_selection_excludes_stale() {
    let (store, _t) = open_store("sg-mem-stale");
    add_project(&store, "pj");
    mem::set_feature_enabled(&store, true, "tester").unwrap();
    mem::settings_update(
        &store,
        "pj",
        &mem::SettingsPatch {
            enabled: Some(true),
            stale_after_days: Some(30),
            ..Default::default()
        },
        1,
        &sg_store::ids::new_id("k"),
    )
    .unwrap();
    create_simple(&store, "pj", "陈旧条目", "很久以前的结论。");
    store
        .with_conn(|c| {
            c.execute(
                "UPDATE memory_entries SET updated_at = '2020-01-01T00:00:00.000Z'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let (included, excluded) =
        mem::retrieval::select_for_context(&store, "pj", &["结论".to_string()]).unwrap();
    assert_eq!(included.len(), 0);
    assert_eq!(excluded[0]["reason"], "stale");
}

/// 导入/导出闭环 + Secret 拒绝 + 去重（MEM-022/023）。
#[test]
fn import_export_roundtrip_and_dedupe() {
    let (store, _t) = open_store("sg-mem-io");
    add_project(&store, "pj");
    use base64::Engine as _;
    let md = "# 部署顺序结论\n\n先构建后部署，回滚走快照。\n";
    let b64 = base64::engine::general_purpose::STANDARD.encode(md);

    let imported = mem::export::import(
        &store,
        "pj",
        "deploy.md",
        &b64,
        "active",
        &sg_store::ids::new_id("k"),
    )
    .unwrap();
    assert_eq!(imported["created"].as_array().unwrap().len(), 1);

    // 重复导入 → duplicates（不静默覆盖）。
    let again = mem::export::import(
        &store,
        "pj",
        "deploy.md",
        &b64,
        "proposed",
        &sg_store::ids::new_id("k"),
    )
    .unwrap();
    assert_eq!(again["duplicates"].as_array().unwrap().len(), 1);

    // 导出：目录产物 + index。
    let exported = mem::export::export(&store, "pj", None, false).unwrap();
    let export_id = exported["exportId"].as_str().unwrap().to_string();
    let dir = store
        .data_dir
        .join("exports")
        .join("memory")
        .join(&export_id);
    assert!(dir.join("_index.json").exists());
    assert!(dir.join("_README.txt").exists());
    let index = std::fs::read_to_string(dir.join("_index.json")).unwrap();
    assert!(index.contains("deploy") || index.contains("contentSha256"));

    // Secret 导入拒绝。
    let bad =
        base64::engine::general_purpose::STANDARD.encode("api_key = \"abcdefghijklmnopqrst\"");
    assert_err_token(
        mem::export::import(
            &store,
            "pj",
            "bad.md",
            &bad,
            "active",
            &sg_store::ids::new_id("k"),
        ),
        "memory_secret_detected",
    );
}

/// M5 演练：populated 库 → 备份快照（恢复的本质 = 快照文件成为当前库）→
/// 记忆表/FTS/对象对账一致 + foreign_key_check 通过（§16.3/MEM-026）。
#[test]
fn migration_and_backup_restore_drill() {
    let (store, _t) = open_store("sg-mem-drill");
    add_project(&store, "pj");
    let a = create_simple(&store, "pj", "演练条目一", "快照恢复对账正文一。");
    create_simple(&store, "pj", "演练条目二", "快照恢复对账正文二。");
    // 条目一更新一次 → 3 个 revision / 2 个 entry。
    mem::mutation::update(
        &store,
        &mem::UpdateInput {
            project_id: "pj".into(),
            memory_id: a["memoryId"].as_str().unwrap().into(),
            title: None,
            body: Some("对账正文一修订。".into()),
            summary: None,
            tags: None,
            expected_revision: 1,
            actor: "tester".into(),
            idempotency_key: sg_store::ids::new_id("k"),
        },
    )
    .unwrap();

    let snap = sg_store::backup::snapshot(&store).unwrap();
    let restored = rusqlite::Connection::open_with_flags(
        &snap.path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let counts: (i64, i64, i64, i64) = restored
        .query_row(
            "SELECT (SELECT COUNT(*) FROM memory_entries),
                    (SELECT COUNT(*) FROM memory_revisions),
                    (SELECT COUNT(*) FROM memory_fts),
                    (SELECT COUNT(*) FROM memory_revisions WHERE object_sha256 IS NOT NULL)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(counts, (2, 3, 2, 3));
    let fk: i64 = restored
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(fk, 0);
    assert_eq!(snap.manifest["memoryEntries"], json!(2));
    assert_eq!(snap.manifest["memoryObjects"], json!(3));
}

/// M5 演练（§16.3 旧包兼容前提）：v23 迁移必须 additive——
/// 只允许 CREATE/INDEX，禁止 DROP TABLE / DROP COLUMN / RENAME。
#[test]
fn migration_v23_is_additive_only() {
    let body = include_str!("../../../crates/store/migrations/0023_project_memory.sql");
    let lowered = body.to_lowercase();
    for banned in ["drop table", "drop column", "rename to", "alter table"] {
        assert!(
            !lowered.contains(banned),
            "0023 含非 additive 语句：{banned}（旧包必须能在 v23 库启动）"
        );
    }
}
