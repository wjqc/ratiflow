//! workitem 判重检索（EvoFlow WP-11 A6；FTS5 trigram，表 0047）。
//!
//! flag `RATIFLOW_WORKITEM_FTS`（默认 0）：开启后 create 时增量索引
//! （标题+描述），`searchRebuild` 全量回填，`similar` 按 FTS5 bm25 rank
//! 返回近似工作项——**仅提示，禁自动合并**（判重语义为人工参考）。
//! trigram 支持中文子串匹配；检索词 <3 字符时 trigram 无 token，返回空集。
//! 阈值配置化（本地假设）：`RATIFLOW_WORKITEM_SIMILAR_LIMIT` 控制返回条数
//! （默认 5）。

use sg_store::{Error, Store};

pub fn enabled() -> bool {
    std::env::var("RATIFLOW_WORKITEM_FTS").ok().as_deref() == Some("1")
}

fn similar_limit() -> i64 {
    std::env::var("RATIFLOW_WORKITEM_SIMILAR_LIMIT")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(5)
}

/// 增量索引（create/update 挂钩；先删后插保证幂等）。flag 关闭时 no-op。
pub fn index_workitem(
    store: &Store,
    workitem_id: &str,
    title: &str,
    description: &str,
) -> Result<(), Error> {
    if !enabled() {
        return Ok(());
    }
    store.with_conn(|conn| {
        conn.execute(
            "DELETE FROM workitem_search WHERE workitem_id=?1",
            [workitem_id],
        )?;
        conn.execute(
            "INSERT INTO workitem_search(workitem_id, title, description) VALUES (?1,?2,?3)",
            rusqlite::params![workitem_id, title, description],
        )?;
        Ok(())
    })
}

/// 全量回填（searchRebuild）：清空后按 workitems 表重建。返回索引条数。
pub fn reindex_all(store: &Store) -> Result<i64, Error> {
    store.with_conn(|conn| {
        conn.execute("DELETE FROM workitem_search", [])?;
        conn.execute(
            "INSERT INTO workitem_search(workitem_id, title, description)
             SELECT id, title, description FROM workitems",
            [],
        )?;
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM workitem_search", [], |r| r.get(0))?;
        Ok(n)
    })
}

/// 近似工作项（FTS5 bm25 rank 升序=相关度降序；排除自身；仅提示禁自动合并）。
pub fn similar(store: &Store, workitem_id: &str) -> Result<Vec<serde_json::Value>, Error> {
    let (title, _description): (String, String) = store.with_conn(|conn| {
        conn.query_row(
            "SELECT title, description FROM workitems WHERE id=?1",
            [workitem_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| Error::Message(format!("workitem {workitem_id} not found")))
    })?;
    // 检索词=标题的三字窗口 OR（trigram 共享片段 = 相似度信号；bm25 按重合度排序）。
    let chars: Vec<char> = title.chars().collect();
    if chars.len() < 3 {
        return Ok(vec![]); // trigram 无 token 可用
    }
    let mut clauses: Vec<String> = Vec::new();
    for w in chars.windows(3) {
        let frag: String = w.iter().collect();
        clauses.push(format!("\"{}\"", frag.replace('"', "\"\"")));
    }
    clauses.dedup();
    let query = clauses.join(" OR ");
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT workitem_id, title, bm25(workitem_search)
             FROM workitem_search
             WHERE workitem_search MATCH ?1 AND workitem_id != ?2
             ORDER BY rank LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![query, workitem_id, similar_limit()],
            |r| {
                Ok(serde_json::json!({
                    "workitemId": r.get::<_, String>(0)?,
                    "title": r.get::<_, String>(1)?,
                    "rank": r.get::<_, f64>(2)?,
                }))
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sg_store::{ids, timefmt};

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-wisearch-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj','u','n','p','main',?1)",
                    [timefmt::now()],
                )?;
                Ok(())
            })
            .unwrap();
        store
    }

    #[test]
    fn chinese_substring_similar_excludes_self_and_rebuild_backfills() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("RATIFLOW_WORKITEM_FTS", "1");
        let store = setup();
        // 同含「支付网关」子串的两条 + 无关一条（create 钩子自动索引）。
        let a = crate::create(&store, "pj", "支付网关超时修复", "描述A", None, &[]).unwrap();
        let b = crate::create(&store, "pj", "支付网关重试优化", "描述B", None, &[]).unwrap();
        let c = crate::create(&store, "pj", "知识库导入工具", "描述C", None, &[]).unwrap();
        let sim = similar(&store, &a.id).unwrap();
        let ids: Vec<&str> = sim
            .iter()
            .map(|x| x["workitemId"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&b.id.as_str()), "中文子串命中：{sim:?}");
        assert!(!ids.contains(&a.id.as_str()), "排除自身");
        assert!(!ids.contains(&c.id.as_str()), "无关工作项不入近似集");
        assert_eq!(sim.len(), 1, "无关工作项不入近似集：{sim:?}");
        // 全量回填：清空后重建条数完整，similar 依旧可用。
        let n = reindex_all(&store).unwrap();
        assert_eq!(n, 3);
        let sim2 = similar(&store, &a.id).unwrap();
        assert_eq!(sim2.len(), 1);
        // flag 关闭：create 不索引 d（索引里只有 a/b/c 三条）；d 自身永不出现在结果里。
        std::env::remove_var("RATIFLOW_WORKITEM_FTS");
        let d = crate::create(&store, "pj", "支付网关限流兜底", "描述D", None, &[]).unwrap();
        let sim3 = similar(&store, &d.id).unwrap();
        assert_eq!(
            sim3.len(),
            2,
            "flag 关闭期间 d 不入索引，但共享子串的 a/b 可被检回：{sim3:?}"
        );
        assert!(sim3
            .iter()
            .all(|x| x["workitemId"].as_str() != Some(d.id.as_str())));
        // 重建补齐 flag 关闭窗口期的缺口：d 入索引后亦作为结果出现。
        std::env::set_var("RATIFLOW_WORKITEM_FTS", "1");
        assert_eq!(reindex_all(&store).unwrap(), 4);
        let sim4 = similar(&store, &d.id).unwrap();
        assert_eq!(
            sim4.len(),
            2,
            "重建后 d 入索引（自身排除；c 无共享子串仍不入集）：{sim4:?}"
        );
        std::env::remove_var("RATIFLOW_WORKITEM_FTS");
    }
}
