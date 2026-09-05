//! 关前快照（ADR-030 M3 / SG-RBK-001/002 / 蓝图 §7.1）。
//! 快照 = 控制面 + 工作区 + 外部引用三份 canonical JSON 清单，各自 SHA-256 合成 root_digest；
//! 清单正文与脏 patch 进 objects。主工作区只读探测（SG-RBK-005：绝不隐式 reset）。
//! 快照失败 → attempt 停留在 preparing（不可执行、不在单活跃索引内），错误向上传播。

use serde::Serialize;
use sg_store::{ids, objects, outbox, timefmt, Error, Store};
use sha2::{Digest, Sha256};

use crate::Gate;

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub id: String,
    pub workitem_id: String,
    pub stage_attempt_id: String,
    pub kind: String,
    pub control_manifest_sha256: String,
    pub workspace_manifest_sha256: String,
    pub external_manifest_sha256: String,
    pub root_digest: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotResource {
    pub resource_type: String,
    pub resource_key: String,
    pub version_ref: String,
    pub object_sha256: String,
    pub reversibility: String,
    pub metadata_json: String,
}

const SNAP_COLUMNS: &str = "id, workitem_id, stage_attempt_id, kind, control_manifest_sha256, workspace_manifest_sha256, external_manifest_sha256, root_digest, created_at";

fn row_snapshot(r: &rusqlite::Row<'_>) -> rusqlite::Result<Snapshot> {
    Ok(Snapshot {
        id: r.get(0)?,
        workitem_id: r.get(1)?,
        stage_attempt_id: r.get(2)?,
        kind: r.get(3)?,
        control_manifest_sha256: r.get(4)?,
        workspace_manifest_sha256: r.get(5)?,
        external_manifest_sha256: r.get(6)?,
        root_digest: r.get(7)?,
        created_at: r.get(8)?,
    })
}

fn put_canonical(store: &Store, value: &serde_json::Value) -> Result<String, Error> {
    let canonical = serde_json::to_string(value).unwrap_or_default();
    let info = objects::put(store, canonical.as_bytes(), objects::PutOptions::default())
        .map_err(|e| Error::Message(format!("snapshot_failed: 存储清单失败 {e}")))?;
    Ok(info.sha256)
}

/// 控制面清单：需求修订指针、各关 active attempt/基线、current_gate、阶段投影、schema/core 版本。
fn control_manifest(
    store: &Store,
    workitem_id: &str,
    gate: Gate,
    attempt_id: &str,
) -> Result<serde_json::Value, Error> {
    let wi = crate::get(store, workitem_id)?;
    let stages = crate::stages(store, workitem_id)?;
    let mut revisions = Vec::new();
    for (doc, revs) in crate::requirements::revisions(store, workitem_id)? {
        if let Some(latest) = revs.last() {
            revisions.push(serde_json::json!({
                "documentId": doc.id,
                "revisionId": latest.id,
                "revisionNo": latest.revision_no,
                "contentSha256": latest.content_sha256,
            }));
        }
    }
    let mut baselines = Vec::new();
    for g in Gate::ALL {
        if let Some(base) = sg_artifact::latest_baseline(store, workitem_id, g.as_str())? {
            baselines.push(serde_json::json!({
                "gate": g.as_str(),
                "baselineId": base.id,
                "inputsSha256": base.inputs_sha256,
            }));
        }
    }
    Ok(serde_json::json!({
        "workItemId": workitem_id,
        "snapshotGate": gate.as_str(),
        "attemptId": attempt_id,
        "currentGate": wi.current_gate,
        "schemaVersion": store.schema_version().unwrap_or(0),
        "coreVersion": store.version,
        "requirementRevisions": revisions,
        "baselines": baselines,
        "stages": stages.iter().map(|s| serde_json::json!({
            "gate": s.gate, "state": s.state, "inputBaselineSha": s.input_baseline_sha,
        })).collect::<Vec<_>>(),
    }))
}

fn git_output(root: &std::path::Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// 工作区清单（只读）：Git HEAD/branch、dirty 统计、脏 patch 对象。
/// git 缺失或非仓库时诚实标记 available=false，不伪造。
fn workspace_manifest(
    store: &Store,
    workitem_id: &str,
) -> Result<(serde_json::Value, Vec<SnapshotResource>), Error> {
    let local_root: Option<String> = store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT COALESCE(p.local_root,'') FROM projects p
                 JOIN workitems w ON w.project_id = p.id WHERE w.id=?1",
                [workitem_id],
                |r| r.get::<_, String>(0),
            )
            .map_err(Error::from)
        })
        .ok()
        .filter(|s| !s.is_empty());
    // 受管隔离 worktree（SixGates 所有，回滚可整体恢复——SG-RBK-005 合法操作域）。
    let (managed_json, managed_resources) = match crate::worktree::status(store, workitem_id)? {
        Some(wt) => {
            let mut res = vec![SnapshotResource {
                resource_type: "worktree_head".into(),
                resource_key: "managed".into(),
                version_ref: wt.head.clone(),
                object_sha256: String::new(),
                reversibility: "logical_restore".into(),
                metadata_json: serde_json::json!({ "managed": true, "branch": wt.branch })
                    .to_string(),
            }];
            if wt.dirty_files > 0 {
                res.push(SnapshotResource {
                    resource_type: "worktree_dirty".into(),
                    resource_key: "managed".into(),
                    version_ref: String::new(),
                    object_sha256: String::new(),
                    reversibility: "logical_restore".into(),
                    metadata_json: serde_json::json!({ "files": wt.dirty_files }).to_string(),
                });
            }
            (
                serde_json::json!({
                    "available": true,
                    "head": wt.head,
                    "branch": wt.branch,
                    "dirtyFiles": wt.dirty_files,
                    "managed": true,
                }),
                res,
            )
        }
        None => (
            serde_json::json!({"available": false, "managed": true}),
            vec![],
        ),
    };
    let Some(root) = local_root else {
        return Ok((
            serde_json::json!({
                "available": false,
                "reason": "no_local_root",
                "managedWorktree": managed_json,
            }),
            managed_resources,
        ));
    };
    let root_path = std::path::PathBuf::from(&root);
    let head = git_output(&root_path, &["rev-parse", "HEAD"]);
    let Some(head) = head else {
        return Ok((
            serde_json::json!({"available": false, "reason": "not_a_git_repo"}),
            vec![],
        ));
    };
    let branch = git_output(&root_path, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let dirty = git_output(&root_path, &["status", "--porcelain"]).unwrap_or_default();
    let dirty_lines: Vec<&str> = dirty.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut resources = vec![SnapshotResource {
        resource_type: "git_head".into(),
        resource_key: root.clone(),
        version_ref: head.clone(),
        object_sha256: String::new(),
        reversibility: "logical_restore".into(),
        metadata_json: r#"{"managed":false}"#.into(),
    }];
    let mut patch_sha = String::new();
    if !dirty_lines.is_empty() {
        // 脏 patch 落 objects（快照只读采集，不做任何写回）。快照是对工作区的忠实采集，
        // 仅本地入库供回滚还原、不经出网，秘密治理在出网侧 mask；这里不阻断采集，
        // findings 仍登记 objects.secret_findings 留审计。
        let diff = git_output(&root_path, &["diff"]).unwrap_or_default();
        let patch = format!("{diff}\n--- untracked ---\n{dirty}");
        let info = objects::put(
            store,
            patch.as_bytes(),
            objects::PutOptions {
                allow_secrets: true,
                ..Default::default()
            },
        )
        .map_err(|e| Error::Message(format!("snapshot_failed: patch 存储失败 {e}")))?;
        patch_sha = info.sha256;
        resources.push(SnapshotResource {
            resource_type: "git_dirty_patch".into(),
            resource_key: root.clone(),
            version_ref: String::new(),
            object_sha256: patch_sha.clone(),
            reversibility: "logical_restore".into(),
            metadata_json: serde_json::json!({ "files": dirty_lines.len(), "managed": false })
                .to_string(),
        });
    }
    let resources = merge_resources(resources, managed_resources);
    Ok((
        serde_json::json!({
            "available": true,
            "root": root,
            "head": head,
            "branch": branch,
            "dirtyFiles": dirty_lines.len(),
            "dirtyPatchObjectSha256": patch_sha,
            "managed": false,
            "managedWorktree": managed_json,
        }),
        resources,
    ))
}

/// 受管 worktree 资源并入（主路径尾部调用，避开模块级 resources 函数名遮蔽）。
fn merge_resources(
    base: Vec<SnapshotResource>,
    managed: Vec<SnapshotResource>,
) -> Vec<SnapshotResource> {
    let mut out = base;
    out.extend(managed);
    out
}

/// 外部引用清单：GitLab Issue 指针与部署副作用（阶段回滚不冒充部署补偿，SG-RBK-008 → manual）。
fn external_manifest(
    store: &Store,
    workitem_id: &str,
) -> Result<(serde_json::Value, Vec<SnapshotResource>), Error> {
    let issue_iid: Option<String> = store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT COALESCE(gitlab_issue_iid,'') FROM workitems WHERE id=?1",
                [workitem_id],
                |r| r.get::<_, String>(0),
            )
            .map_err(Error::from)
        })
        .ok()
        .filter(|s| !s.is_empty());
    let deployments: Vec<serde_json::Value> = store
        .with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, state, COALESCE(image_digest,'') FROM deployments
                 WHERE workitem_id=?1 ORDER BY created_at",
            )?;
            let rows = stmt.query_map([workitem_id], |r| {
                Ok(serde_json::json!({
                    "deploymentId": r.get::<_, String>(0)?,
                    "state": r.get::<_, String>(1)?,
                    "imageDigest": r.get::<_, String>(2)?,
                }))
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .unwrap_or_default();
    let resources = deployments
        .iter()
        .map(|d| SnapshotResource {
            resource_type: "deployment".into(),
            resource_key: d["deploymentId"].as_str().unwrap_or_default().into(),
            version_ref: d["imageDigest"].as_str().unwrap_or_default().into(),
            object_sha256: String::new(),
            // 部署补偿属 deployment.rollback 域；阶段回滚对其只能转人工（SG-RBK-008）。
            reversibility: "manual".into(),
            metadata_json: serde_json::json!({ "state": d["state"] }).to_string(),
        })
        .collect();
    Ok((
        serde_json::json!({
            "gitlabIssueIid": issue_iid,
            "deployments": deployments,
        }),
        resources,
    ))
}

fn insert_resources(
    store: &Store,
    snapshot_id: &str,
    resources: &[SnapshotResource],
) -> Result<(), Error> {
    for r in resources {
        store.with_conn(|conn| {
            conn.execute(
                "INSERT OR IGNORE INTO snapshot_resources(snapshot_id, resource_type, resource_key, version_ref, object_sha256, reversibility, metadata_json)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                rusqlite::params![
                    snapshot_id, r.resource_type, r.resource_key, r.version_ref,
                    r.object_sha256, r.reversibility, r.metadata_json
                ],
            )?;
            Ok(())
        })?;
    }
    Ok(())
}

/// 创建快照（kind 由调用方决定）：三清单 → root_digest → 资源明细 → snapshot.created 事件。
pub fn create(
    store: &Store,
    workitem_id: &str,
    gate: Gate,
    attempt_id: &str,
    kind: &str,
) -> Result<Snapshot, Error> {
    let control = control_manifest(store, workitem_id, gate, attempt_id)?;
    let (workspace, mut resources) = workspace_manifest(store, workitem_id)?;
    let (external, ext_resources) = external_manifest(store, workitem_id)?;
    resources.extend(ext_resources);
    let control_sha = put_canonical(store, &control)?;
    let workspace_sha = put_canonical(store, &workspace)?;
    let external_sha = put_canonical(store, &external)?;
    let mut hasher = Sha256::new();
    for part in [&control_sha, &workspace_sha, &external_sha] {
        hasher.update(part.as_bytes());
        hasher.update(b"|");
    }
    let root_digest = ids::hex(&hasher.finalize());
    // 幂等：同 (attempt, kind, root_digest) 已存在则复用（UNIQUE 语义，蓝图 §5.2）。
    if let Some(existing) = store.with_conn(|conn| {
        let snap = conn
            .query_row(
                &format!(
                    "SELECT {SNAP_COLUMNS} FROM state_snapshots
                     WHERE stage_attempt_id=?1 AND kind=?2 AND root_digest=?3"
                ),
                rusqlite::params![attempt_id, kind, root_digest],
                row_snapshot,
            )
            .ok();
        Ok(snap)
    })? {
        return Ok(existing);
    }
    let id = ids::new_id("snap");
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO state_snapshots(id, workitem_id, stage_attempt_id, kind, control_manifest_sha256, workspace_manifest_sha256, external_manifest_sha256, root_digest, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![
                id, workitem_id, attempt_id, kind,
                control_sha, workspace_sha, external_sha, root_digest, timefmt::now()
            ],
        )?;
        Ok(())
    })?;
    insert_resources(store, &id, &resources)?;
    // 控制面资源明细：需求修订与各关基线均可逻辑恢复。
    for rev in control["requirementRevisions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        insert_resources(
            store,
            &id,
            &[SnapshotResource {
                resource_type: "requirement_revision".into(),
                resource_key: rev["documentId"].as_str().unwrap_or_default().into(),
                version_ref: rev["revisionId"].as_str().unwrap_or_default().into(),
                object_sha256: rev["contentSha256"].as_str().unwrap_or_default().into(),
                reversibility: "logical_restore".into(),
                metadata_json: "{}".into(),
            }],
        )?;
    }
    for base in control["baselines"].as_array().cloned().unwrap_or_default() {
        insert_resources(
            store,
            &id,
            &[SnapshotResource {
                resource_type: "baseline".into(),
                resource_key: base["gate"].as_str().unwrap_or_default().into(),
                version_ref: base["baselineId"].as_str().unwrap_or_default().into(),
                // inputs_sha256 是派生哈希不是 objects 内容；对象引用留空（指针型资源）。
                object_sha256: String::new(),
                reversibility: "logical_restore".into(),
                metadata_json: serde_json::json!({
                    "inputsSha256": base["inputsSha256"],
                })
                .to_string(),
            }],
        )?;
    }
    outbox::emit(
        store,
        "workitem",
        workitem_id,
        "snapshot.created",
        serde_json::json!({
            "workitemId": workitem_id,
            "snapshotId": id,
            "attemptId": attempt_id,
            "kind": kind,
            "rootDigest": root_digest,
        }),
    )?;
    let snapshot =
        get(store, &id)?.ok_or_else(|| Error::Message("snapshot_failed: 不可见".into()))?;
    Ok(snapshot)
}

pub fn get(store: &Store, snapshot_id: &str) -> Result<Option<Snapshot>, Error> {
    store.with_conn(|conn| {
        let snap = conn
            .query_row(
                &format!("SELECT {SNAP_COLUMNS} FROM state_snapshots WHERE id=?1"),
                [snapshot_id],
                row_snapshot,
            )
            .ok();
        Ok(snap)
    })
}

pub fn list(store: &Store, workitem_id: &str) -> Result<Vec<Snapshot>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {SNAP_COLUMNS} FROM state_snapshots WHERE workitem_id=?1 ORDER BY created_at"
        ))?;
        let rows = stmt.query_map([workitem_id], row_snapshot)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

/// 指定关最新关前快照。
pub fn latest_entry(
    store: &Store,
    workitem_id: &str,
    gate: Gate,
) -> Result<Option<Snapshot>, Error> {
    store.with_conn(|conn| {
        let snap = conn
            .query_row(
                &format!(
                    "SELECT {SNAP_COLUMNS} FROM state_snapshots
                     WHERE workitem_id=?1 AND kind='stage_entry'
                       AND stage_attempt_id IN (SELECT id FROM stage_attempts WHERE workitem_id=?1 AND gate=?2)
                     ORDER BY created_at DESC LIMIT 1"
                ),
                [workitem_id, gate.as_str()],
                row_snapshot,
            )
            .ok();
        Ok(snap)
    })
}

pub fn resources(store: &Store, snapshot_id: &str) -> Result<Vec<SnapshotResource>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT resource_type, resource_key, version_ref, object_sha256, reversibility, metadata_json
             FROM snapshot_resources WHERE snapshot_id=?1 ORDER BY resource_type, resource_key",
        )?;
        let rows = stmt.query_map([snapshot_id], |r| {
            Ok(SnapshotResource {
                resource_type: r.get(0)?,
                resource_key: r.get(1)?,
                version_ref: r.get(2)?,
                object_sha256: r.get(3)?,
                reversibility: r.get(4)?,
                metadata_json: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

/// 对象完整性检查（蓝图 §13.1）：清单与 patch 对象缺失即 snapshot_failed。
pub fn verify_objects(store: &Store, snapshot_id: &str) -> Result<(), Error> {
    let snapshot = get(store, snapshot_id)?
        .ok_or_else(|| Error::Message(format!("snapshot_failed: 快照 {snapshot_id} 不存在")))?;
    let shas = [
        snapshot.control_manifest_sha256.as_str(),
        snapshot.workspace_manifest_sha256.as_str(),
        snapshot.external_manifest_sha256.as_str(),
    ];
    for sha in shas {
        if !sha.is_empty() && !object_readable(store, sha)? {
            return Err(Error::Message(format!(
                "snapshot_failed: 清单对象 {sha} 缺失"
            )));
        }
    }
    for r in resources(store, snapshot_id)? {
        if !r.object_sha256.is_empty() && !object_readable(store, &r.object_sha256)? {
            return Err(Error::Message(format!(
                "snapshot_failed: 资源对象 {} 缺失",
                r.object_sha256
            )));
        }
    }
    Ok(())
}

/// 对象可读（文件存在且可打开），不只查表。
fn object_readable(store: &Store, sha: &str) -> Result<bool, Error> {
    let exists: i64 = store.with_conn(|conn| {
        conn.query_row("SELECT COUNT(*) FROM objects WHERE sha256=?1", [sha], |r| {
            r.get(0)
        })
        .map_err(Error::from)
    })?;
    if exists == 0 {
        return Ok(false);
    }
    Ok(sg_store::objects::open(store, sha).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sg_store::Store;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-snap-{}-{}",
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
    fn snapshot_is_deterministic_and_complete() {
        let s = setup();
        let wi = crate::create(&s, "pj", "快照", "", None, &[]).unwrap();
        crate::requirements::import_revision(
            &s,
            &wi.id,
            "requirement.md",
            "# 需求\n\n- 支持单点登录\n",
            "inline",
            "local-user",
            "verified",
        )
        .unwrap();
        let attempt = crate::attempt::create(&s, &wi.id, Gate::Requirements, None).unwrap();
        let snap1 = create(&s, &wi.id, Gate::Requirements, &attempt.id, "stage_entry").unwrap();
        let snap2 = create(&s, &wi.id, Gate::Requirements, &attempt.id, "stage_entry").unwrap();
        assert_eq!(
            snap1.root_digest, snap2.root_digest,
            "同状态快照 root_digest 一致"
        );
        assert!(!snap1.control_manifest_sha256.is_empty());
        // 对象完整性通过。
        verify_objects(&s, &snap1.id).unwrap();
        // 资源含需求修订与 git_head（无 local_root → workspace unavailable 但快照成立）。
        let resources = resources(&s, &snap1.id).unwrap();
        assert!(resources
            .iter()
            .any(|r| r.resource_type == "requirement_revision"));
        assert!(latest_entry(&s, &wi.id, Gate::Requirements)
            .unwrap()
            .is_some());
    }

    #[test]
    fn missing_object_detected() {
        let s = setup();
        let wi = crate::create(&s, "pj", "缺失对象", "", None, &[]).unwrap();
        let attempt = crate::attempt::create(&s, &wi.id, Gate::Requirements, None).unwrap();
        let snap = create(&s, &wi.id, Gate::Requirements, &attempt.id, "stage_entry").unwrap();
        // 故障注入：删除清单对象文件。
        let rel = &snap.control_manifest_sha256;
        let path = s.data_dir.join("objects").join(&rel[0..2]).join(rel);
        std::fs::remove_file(&path).unwrap();
        let err = verify_objects(&s, &snap.id).unwrap_err();
        assert!(err.to_string().contains("snapshot_failed"));
    }
}
