//! 工件、不可变修订、评审与基线（v2 ADR 行为等价 + F07）。
//! 修订追加不可变；草稿乐观并发（ETag）；冻结触发下游 stale 由 core 编排。

use serde::Serialize;
use sg_store::{ids, objects, outbox, timefmt, Error, Store};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize)]
pub struct Artifact {
    pub id: String,
    pub workitem_id: String,
    pub kind: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Revision {
    pub id: String,
    pub artifact_id: String,
    pub rev_no: i64,
    pub content_sha256: String,
    pub size: i64,
    pub status: String,
    pub created_by: String,
    pub created_at: String,
    pub etag: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Baseline {
    pub id: String,
    pub workitem_id: String,
    pub gate: String,
    pub revision_map: serde_json::Value,
    pub inputs_sha256: String,
    pub gitlab_commit_sha: String,
    pub frozen_at: String,
    pub superseded_by: Option<String>,
}

pub fn etag_of(content_sha: &str) -> String {
    format!("\"{}\"", &content_sha.chars().take(16).collect::<String>())
}

pub fn create_artifact(
    store: &Store,
    workitem_id: &str,
    kind: &str,
    title: &str,
) -> Result<Artifact, Error> {
    if workitem_id.is_empty() || kind.is_empty() || title.is_empty() {
        return Err(Error::Message("workitem/kind/title required".into()));
    }
    let id = ids::new_id("art");
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO artifacts(id, workitem_id, kind, title, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?5)",
            rusqlite::params![id, workitem_id, kind, title, now],
        )?;
        Ok(())
    })?;
    get_artifact(store, &id)
}

pub fn get_artifact(store: &Store, id: &str) -> Result<Artifact, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT id, workitem_id, kind, title, created_at, updated_at FROM artifacts WHERE id=?1",
            [id],
            |r| {
                Ok(Artifact {
                    id: r.get(0)?, workitem_id: r.get(1)?, kind: r.get(2)?,
                    title: r.get(3)?, created_at: r.get(4)?, updated_at: r.get(5)?,
                })
            },
        )
        .map_err(|_| Error::Message("artifact_not_found".into()))
    })
}

pub fn list_artifacts(store: &Store, workitem_id: &str) -> Result<Vec<Artifact>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, workitem_id, kind, title, created_at, updated_at FROM artifacts WHERE workitem_id=?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map([workitem_id], |r| {
            Ok(Artifact {
                id: r.get(0)?, workitem_id: r.get(1)?, kind: r.get(2)?,
                title: r.get(3)?, created_at: r.get(4)?, updated_at: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

fn insert_revision(
    store: &Store,
    artifact_id: &str,
    content_sha: &str,
    size: i64,
    status: &str,
    created_by: &str,
) -> Result<Revision, Error> {
    let rev_no: i64 = store.with_conn(|conn| {
        conn.query_row(
            "SELECT COALESCE(MAX(rev_no),0)+1 FROM revisions WHERE artifact_id=?1",
            [artifact_id],
            |r| r.get(0),
        )
        .map_err(Error::from)
    })?;
    let id = ids::new_id("rev");
    let created_at = store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO revisions(id, artifact_id, rev_no, content_sha256, size, status, created_by, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![id, artifact_id, rev_no, content_sha, size, status, created_by, timefmt::now()],
        )?;
        conn.execute("UPDATE artifacts SET updated_at=?1 WHERE id=?2", rusqlite::params![timefmt::now(), artifact_id])?;
        let created: String = conn.query_row("SELECT created_at FROM revisions WHERE id=?1", [&id], |r| r.get(0))?;
        Ok(created)
    })?;
    let mut rev = Revision {
        id,
        artifact_id: artifact_id.into(),
        rev_no,
        content_sha256: content_sha.into(),
        size,
        status: status.into(),
        created_by: created_by.into(),
        created_at,
        etag: String::new(),
    };
    rev.etag = etag_of(&rev.content_sha256);
    Ok(rev)
}

pub fn create_draft(store: &Store, artifact_id: &str, content: &str) -> Result<Revision, Error> {
    let info = objects::put(store, content.as_bytes(), objects::PutOptions::default())
        .map_err(|e| Error::Message(format!("store draft: {e}")))?;
    insert_revision(
        store,
        artifact_id,
        &info.sha256,
        info.size,
        "draft",
        "local",
    )
}

/// If-Match 更新草稿：旧草稿 superseded，生成新修订。
pub fn update_draft(
    store: &Store,
    revision_id: &str,
    if_match: &str,
    content: &str,
) -> Result<Revision, Error> {
    let current = get_revision(store, revision_id)?;
    if current.status != "draft" {
        return Err(Error::Message(format!(
            "revision_frozen: revision is {}",
            current.status
        )));
    }
    if if_match != current.etag {
        return Err(Error::Message("etag_mismatch".into()));
    }
    let info = objects::put(store, content.as_bytes(), objects::PutOptions::default())?;
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE revisions SET status='superseded' WHERE id=?1",
            [revision_id],
        )?;
        Ok(())
    })?;
    insert_revision(
        store,
        &current.artifact_id,
        &info.sha256,
        info.size,
        "draft",
        "local",
    )
}

pub fn get_revision(store: &Store, id: &str) -> Result<Revision, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT id, artifact_id, rev_no, content_sha256, size, status, created_by, created_at FROM revisions WHERE id=?1",
            [id],
            |r| {
                let sha: String = r.get(3)?;
                Ok(Revision {
                    id: r.get(0)?, artifact_id: r.get(1)?, rev_no: r.get(2)?,
                    etag: etag_of(&sha), content_sha256: sha, size: r.get(4)?,
                    status: r.get(5)?, created_by: r.get(6)?, created_at: r.get(7)?,
                })
            },
        )
        .map_err(|_| Error::Message("revision_not_found".into()))
    })
}

pub fn list_revisions(store: &Store, artifact_id: &str) -> Result<Vec<Revision>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, artifact_id, rev_no, content_sha256, size, status, created_by, created_at
             FROM revisions WHERE artifact_id=?1 ORDER BY rev_no DESC",
        )?;
        let rows = stmt.query_map([artifact_id], |r| {
            let sha: String = r.get(3)?;
            Ok(Revision {
                id: r.get(0)?,
                artifact_id: r.get(1)?,
                rev_no: r.get(2)?,
                etag: etag_of(&sha),
                content_sha256: sha,
                size: r.get(4)?,
                status: r.get(5)?,
                created_by: r.get(6)?,
                created_at: r.get(7)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

pub fn revision_content(store: &Store, revision_id: &str) -> Result<Vec<u8>, Error> {
    let rev = get_revision(store, revision_id)?;
    objects::open(store, &rev.content_sha256)
}

pub fn add_review(
    store: &Store,
    revision_id: &str,
    reviewer: &str,
    verdict: &str,
    comment: &str,
    mr_iid: Option<&str>,
) -> Result<(), Error> {
    if !matches!(verdict, "approved" | "rejected" | "changes_requested") {
        return Err(Error::Message(format!("invalid verdict {verdict}")));
    }
    let id = ids::new_id("rev");
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO reviews(id, revision_id, reviewer, verdict, comment, gitlab_mr_iid, decided_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![id, revision_id, reviewer, verdict, comment, mr_iid, timefmt::now()],
        )?;
        if verdict == "approved" {
            conn.execute("UPDATE revisions SET status='in_review' WHERE id=?1 AND status='draft'", [revision_id])?;
        } else {
            conn.execute("UPDATE revisions SET status='draft' WHERE id=?1 AND status='in_review'", [revision_id])?;
        }
        Ok(())
    })?;
    outbox::emit(
        store,
        "workitem",
        revision_id,
        "artifact.reviewed",
        serde_json::json!({"verdict": verdict, "reviewer": reviewer}),
    )?;
    Ok(())
}

/// M2 per-gate 修正：基线按 (workitem, gate) 各自独立 active（蓝图 §5.3）。
pub fn latest_baseline(
    store: &Store,
    workitem_id: &str,
    gate: &str,
) -> Result<Option<Baseline>, Error> {
    type BaselineRow = (
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
    );
    let row: Option<BaselineRow> = store.with_conn(|conn| {
        conn.query_row(
            "SELECT id, gate, revision_map, inputs_sha256, COALESCE(gitlab_commit_sha,''), frozen_at, superseded_by
             FROM baselines WHERE workitem_id=?1 AND gate=?2 AND superseded_by IS NULL
             ORDER BY frozen_at DESC LIMIT 1",
            [workitem_id, gate],
            |r| {
                Ok((
                    r.get(0)?, r.get(1)?, r.get::<_, String>(2)?, r.get(3)?, r.get(4)?, r.get(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .optional_row()
        .map_err(Error::from)
    })?;
    Ok(
        row.map(|(id, gate, map, inputs, sha, frozen, sup)| Baseline {
            id,
            workitem_id: workitem_id.into(),
            gate,
            revision_map: serde_json::from_str(&map).unwrap_or_default(),
            inputs_sha256: inputs,
            gitlab_commit_sha: sha,
            frozen_at: frozen,
            superseded_by: sup,
        }),
    )
}

/// 冻结基线：目标修订必须 in_review；只 supersede 同一 (workitem, gate) 的旧基线（M2 P0 修正，
/// 不再全局 supersede 其他关的 active 基线）；历史保留。
pub fn freeze(
    store: &Store,
    workitem_id: &str,
    gate: &str,
    revision_ids: &[String],
    gitlab_commit_sha: &str,
    stage_attempt_id: &str,
) -> Result<Baseline, Error> {
    if revision_ids.is_empty() {
        return Err(Error::Message("revision ids required".into()));
    }
    let mut revision_map = serde_json::Map::new();
    let mut hasher = Sha256::new();
    for rev_id in revision_ids {
        let rev = get_revision(store, rev_id)?;
        if rev.status != "in_review" {
            return Err(Error::Message(format!(
                "revision_frozen: revision {} is {}（需先通过评审）",
                rev_id, rev.status
            )));
        }
        store.with_conn(|conn| {
            conn.execute("UPDATE revisions SET status='frozen' WHERE id=?1", [rev_id])?;
            Ok(())
        })?;
        revision_map.insert(
            rev.artifact_id.clone(),
            serde_json::Value::String(rev.id.clone()),
        );
        hasher.update(format!("{}={}\n", rev.artifact_id, rev.id).as_bytes());
    }
    let inputs_sha = ids::hex(&hasher.finalize());

    store.with_conn(|conn| {
        conn.execute(
            "UPDATE baselines SET superseded_by='pending' WHERE workitem_id=?1 AND gate=?2 AND superseded_by IS NULL",
            rusqlite::params![workitem_id, gate],
        )?;
        Ok(())
    })?;
    let id = ids::new_id("base");
    let frozen_at = store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO baselines(id, workitem_id, gate, revision_map, inputs_sha256, gitlab_commit_sha, frozen_at, stage_attempt_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![
                id, workitem_id, gate,
                serde_json::to_string(&serde_json::Value::Object(revision_map.clone())).unwrap_or_else(|_| "{}".into()),
                inputs_sha, gitlab_commit_sha, timefmt::now(),
                if stage_attempt_id.is_empty() { None } else { Some(stage_attempt_id) }
            ],
        )?;
        conn.execute(
            "UPDATE baselines SET superseded_by=?1 WHERE workitem_id=?2 AND gate=?3 AND superseded_by='pending' AND id<>?1",
            rusqlite::params![id, workitem_id, gate],
        )?;
        let frozen: String = conn.query_row("SELECT frozen_at FROM baselines WHERE id=?1", [&id], |r| r.get(0))?;
        Ok(frozen)
    })?;
    outbox::emit(
        store,
        "baseline",
        &id,
        "baseline.frozen",
        serde_json::json!({"workitemId": workitem_id, "gate": gate, "inputsSha256": inputs_sha}),
    )?;
    Ok(Baseline {
        id,
        workitem_id: workitem_id.into(),
        gate: gate.into(),
        revision_map: serde_json::Value::Object(revision_map),
        inputs_sha256: inputs_sha,
        gitlab_commit_sha: gitlab_commit_sha.into(),
        frozen_at,
        superseded_by: None,
    })
}

/// 基线新鲜度：map 指向的仍是各工件最新 frozen/in_review 修订。
pub fn is_baseline_current(store: &Store, baseline_id: &str) -> Result<bool, Error> {
    let map: String = store.with_conn(|conn| {
        conn.query_row(
            "SELECT revision_map FROM baselines WHERE id=?1",
            [baseline_id],
            |r| r.get(0),
        )
        .map_err(|_| Error::Message("baseline_not_found".into()))
    })?;
    let mapping: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&map).map_err(|e| Error::Message(e.to_string()))?;
    for (artifact_id, rev_value) in &mapping {
        let rev_id = rev_value.as_str().unwrap_or_default();
        let latest: Option<String> = store.with_conn(|conn| {
            conn.query_row(
                "SELECT id FROM revisions WHERE artifact_id=?1 AND status IN ('frozen','in_review')
                 ORDER BY rev_no DESC LIMIT 1",
                [artifact_id],
                |r| r.get(0),
            )
            .optional_row()
            .map_err(Error::from)
        })?;
        if let Some(latest_id) = latest {
            if latest_id != rev_id {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

trait OptionalRow<T> {
    fn optional_row(self) -> Result<Option<T>, rusqlite::Error>;
}
impl<T> OptionalRow<T> for Result<T, rusqlite::Error> {
    fn optional_row(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-art-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        store.with_conn(|c| {
            c.execute("INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at) VALUES ('pj','u','n','p','main',?1)", [timefmt::now()])?;
            c.execute("INSERT INTO workitems(id, project_id, title, created_at, updated_at) VALUES ('wi','pj','t',?1,?1)", [timefmt::now()])?;
            Ok(())
        }).unwrap();
        store
    }

    #[test]
    fn draft_etag_and_freeze_flow() {
        let s = setup();
        let art = create_artifact(&s, "wi", "prd", "PRD").unwrap();
        let rev = create_draft(&s, &art.id, "# PRD v1").unwrap();
        assert!(!rev.etag.is_empty());

        assert!(update_draft(&s, &rev.id, "\"wrong\"", "v2").is_err());
        let rev2 = update_draft(&s, &rev.id, &rev.etag, "# PRD v2 内容").unwrap();
        assert_eq!(rev2.rev_no, 2);

        assert!(freeze(
            &s,
            "wi",
            "requirements",
            std::slice::from_ref(&rev2.id),
            "",
            ""
        )
        .is_err());
        add_review(&s, &rev2.id, "pm", "approved", "LGTM", Some("7")).unwrap();
        let base = freeze(
            &s,
            "wi",
            "requirements",
            std::slice::from_ref(&rev2.id),
            "sha-c",
            "",
        )
        .unwrap();
        assert_eq!(base.revision_map.as_object().unwrap().len(), 1);
        assert!(
            update_draft(&s, &rev2.id, &rev2.etag, "hack").is_err(),
            "冻结后不可改"
        );
        assert!(is_baseline_current(&s, &base.id).unwrap());
        let content = revision_content(&s, &rev2.id).unwrap();
        assert_eq!(content, "# PRD v2 内容".as_bytes());
    }

    #[test]
    fn supersede_keeps_history() {
        let s = setup();
        let art = create_artifact(&s, "wi", "prd", "PRD").unwrap();
        let r1 = create_draft(&s, &art.id, "v1").unwrap();
        add_review(&s, &r1.id, "r", "approved", "", None).unwrap();
        let b1 = freeze(
            &s,
            "wi",
            "requirements",
            std::slice::from_ref(&r1.id),
            "sha-1",
            "",
        )
        .unwrap();

        let r2 = create_draft(&s, &art.id, "v2").unwrap();
        add_review(&s, &r2.id, "r", "approved", "", None).unwrap();
        let b2 = freeze(
            &s,
            "wi",
            "requirements",
            std::slice::from_ref(&r2.id),
            "sha-2",
            "",
        )
        .unwrap();
        assert_ne!(b1.id, b2.id);
        let latest = latest_baseline(&s, "wi", "requirements").unwrap().unwrap();
        assert_eq!(latest.id, b2.id);
        assert!(!is_baseline_current(&s, &b1.id).unwrap(), "旧基线必须过期");
    }

    /// M2 蓝图 §5.3：per-gate 基线——三关 active 基线可同时存在，互不 supersede。
    #[test]
    fn per_gate_baselines_coexist() {
        let s = setup();
        let mut bases = Vec::new();
        for (kind, gate) in [
            ("prd", "requirements"),
            ("tech_design", "design"),
            ("test_plan", "testing"),
        ] {
            let art = create_artifact(&s, "wi", kind, kind).unwrap();
            let rev = create_draft(&s, &art.id, "内容").unwrap();
            add_review(&s, &rev.id, "r", "approved", "", None).unwrap();
            bases.push(freeze(&s, "wi", gate, std::slice::from_ref(&rev.id), "", "").unwrap());
        }
        for (i, gate) in ["requirements", "design", "testing"].iter().enumerate() {
            let latest = latest_baseline(&s, "wi", gate).unwrap().unwrap();
            assert_eq!(latest.id, bases[i].id, "{gate} 关基线保持 active");
            assert!(is_baseline_current(&s, &bases[i].id).unwrap());
        }
    }
}
