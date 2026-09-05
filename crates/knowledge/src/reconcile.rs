//! 声明式 reconciler + 双平面 freshness + generation 状态机
//! （RFC《知识来源清单随仓库走》v1.0 §7/§9/§10，G3）。
//!
//! 权威：仓库 `knowledge/sources/*.json`；SQLite knowledge_sources 为投影（origin='manifest'）。
//! 集合收敛 → desired 事务 → generation 构建 → activation CAS 单事务切换。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use sg_store::{timefmt, Error, Store};
use sha2::{Digest, Sha256};

use crate::manifest::{
    canonical_json, manifest_dir, normalize_locator, project_root_at, read_manifest,
    repo_path_identity, sha256_hex,
};

fn sha(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 未来/未知 schema 与未知字段 fail-closed（§3.4）。
const KNOWN_FIELDS: [&str; 7] = [
    "schemaVersion",
    "stableId",
    "kind",
    "name",
    "locator",
    "enabled",
    "contentSha256",
];

struct ValidManifest {
    stable_id: String,
    kind: String,
    name: String,
    locator: String,
    enabled: bool,
}

/// §3.3/§3.4 校验：schemaVersion、未知字段、identity 派生一致、fileSlug 前缀一致。
fn validate_file(path: &Path) -> Result<ValidManifest, Error> {
    let (_, bytes) =
        read_manifest(path)?.ok_or_else(|| Error::Message("manifest_missing".into()))?;
    let v: Value = serde_json::from_str(&String::from_utf8_lossy(&bytes))
        .map_err(|_| Error::Message("manifest_invalid_json".into()))?;
    let obj = v
        .as_object()
        .ok_or_else(|| Error::Message("manifest_invalid_json".into()))?;
    if v.get("schemaVersion").and_then(|x| x.as_i64()) != Some(1) {
        return Err(Error::Message("manifest_schema_unsupported".into()));
    }
    for key in obj.keys() {
        if !KNOWN_FIELDS.contains(&key.as_str()) {
            return Err(Error::Message(format!("manifest_unknown_field:{key}")));
        }
    }
    let stable_id = v
        .get("stableId")
        .and_then(|x| x.as_str())
        .ok_or_else(|| Error::Message("manifest_missing_stable_id".into()))?;
    let kind = v
        .get("kind")
        .and_then(|x| x.as_str())
        .ok_or_else(|| Error::Message("manifest_missing_kind".into()))?;
    if !matches!(kind, "repo_path" | "document") {
        return Err(Error::Message("manifest_kind_not_allowed".into()));
    }
    let name = v
        .get("name")
        .and_then(|x| x.as_str())
        .ok_or_else(|| Error::Message("manifest_missing_name".into()))?;
    let locator = v
        .get("locator")
        .and_then(|x| x.as_str())
        .ok_or_else(|| Error::Message("manifest_missing_locator".into()))?;
    let enabled = v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(false);
    // identity 派生一致。
    let expect = match kind {
        "repo_path" => {
            let norm = normalize_locator(locator)?;
            repo_path_identity(&norm, name).1
        }
        _ => {
            if locator.starts_with("knowledge/attachments/doc-") {
                let digest = locator
                    .trim_start_matches("knowledge/attachments/doc-")
                    .split('/')
                    .next()
                    .unwrap_or("");
                format!("doc-{}", digest)
            } else {
                return Err(Error::Message("manifest_locator_escape".into()));
            }
        }
    };
    if stable_id != expect {
        return Err(Error::Message("manifest_identity_mismatch".into()));
    }
    // fileSlug 前缀一致（截断摘要为 identityDigest 前缀 + kind 前缀一致）。
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let digest_part = stable_id
        .split_once('-')
        .map(|(_, d)| d.to_string())
        .unwrap_or_default();
    let slug_digest = stem.rsplit('-').next().unwrap_or("");
    // fileSlug 前缀：repo_path→"repopath"，document→"doc"（与 §3.3 写入侧一致）。
    let kind_prefix = match kind {
        "repo_path" => "repopath",
        _ => "doc",
    };
    if !stem.starts_with(kind_prefix) || !digest_part.starts_with(slug_digest) {
        return Err(Error::Message("manifest_identity_mismatch".into()));
    }
    Ok(ValidManifest {
        stable_id: stable_id.to_string(),
        kind: kind.to_string(),
        name: name.to_string(),
        locator: locator.to_string(),
        enabled,
    })
}

/// §13.2 committed 输入版本：commit SHA + subtree digest；git 失败回退 worktree Merkle。
fn compute_input_revision(root: &Path, locator: &str) -> Result<(String, String), Error> {
    let head = std::process::Command::new("git")
        .args(["-C", &root.to_string_lossy(), "rev-parse", "HEAD"])
        .output();
    if let Ok(out) = &head {
        if out.status.success() {
            let commit = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let tree = std::process::Command::new("git")
                .args([
                    "-C",
                    &root.to_string_lossy(),
                    "rev-parse",
                    &format!("HEAD:{locator}"),
                ])
                .output();
            if let Ok(t) = tree {
                if t.status.success() {
                    let subtree = String::from_utf8_lossy(&t.stdout).trim().to_string();
                    return Ok(("committed".into(), format!("{commit}|{subtree}")));
                }
            }
        }
    }
    Ok(("worktree".into(), worktree_merkle(root, locator)?))
}

/// worktree 目录 Merkle：路径字节排序 + 每文件正文 hash 组合（§7.1）。
pub fn worktree_merkle(root: &Path, locator: &str) -> Result<String, Error> {
    let base = root.join(locator);
    if !base.exists() {
        return Err(Error::Message("manifest_locator_missing".into()));
    }
    let mut files: Vec<PathBuf> = Vec::new();
    walk(&base, &mut files)?;
    files.sort_by(|a, b| {
        a.to_string_lossy()
            .as_bytes()
            .cmp(b.to_string_lossy().as_bytes())
    });
    let mut hasher = Sha256::new();
    for f in &files {
        let rel = f.strip_prefix(&base).unwrap_or(f);
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update([0u8]);
        let body = std::fs::read(f)?;
        hasher.update(sha(&body).as_bytes());
    }
    Ok(format!("worktree|{}", hex(&hasher.finalize())))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Error> {
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        if p.is_dir() {
            walk(&p, out)?;
        } else if p.is_file() {
            out.push(p);
        }
    }
    Ok(())
}

fn load_projection(store: &Store, project_id: &str) -> Result<BTreeMap<String, Value>, Error> {
    store.with_conn(|c| {
        let mut stmt = c.prepare(
            "SELECT id, stable_id, COALESCE(name,''), COALESCE(locator,''), enabled, present
             FROM knowledge_sources WHERE project_id=?1 AND origin='manifest'",
        )?;
        let mut rows = stmt.query([project_id])?;
        let mut map = BTreeMap::new();
        while let Some(r) = rows.next()? {
            let stable_id: String = r.get(1)?;
            map.insert(
                stable_id.clone(),
                json!({
                    "rowId": r.get::<_, String>(0)?,
                    "stableId": stable_id,
                    "name": r.get::<_, String>(2)?,
                    "locator": r.get::<_, String>(3)?,
                    "enabled": r.get::<_, i64>(4)?,
                    "present": r.get::<_, i64>(5)?,
                }),
            );
        }
        Ok(map)
    })
}

fn file_sha_or_empty(path: &Path) -> String {
    std::fs::read(path).map(|b| sha(&b)).unwrap_or_default()
}

/// §10 声明式 reconciler：validated manifest → upsert/insert/tombstone → desired 事务。
/// 两遍执行零变更 = 收敛（A22）。
pub fn sync_from_repo(store: &Store, project_id: &str, root: &Path) -> Result<Value, Error> {
    let dir = manifest_dir(root);
    let mut manifests = Vec::new();
    if dir.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
            .collect();
        entries.sort();
        for path in entries {
            manifests.push(validate_file(&path)?);
        }
    }

    // stableId 唯一性（重复身份 → 整体拒绝，§3.3）。
    let mut seen = std::collections::HashSet::new();
    for m in &manifests {
        if !seen.insert(m.stable_id.clone()) {
            return Err(Error::Message("manifest_duplicate_stable_id".into()));
        }
    }

    let desired: BTreeMap<&str, &ValidManifest> = manifests
        .iter()
        .map(|m| (m.stable_id.as_str(), m))
        .collect();
    let current = load_projection(store, project_id)?;

    let mut inserted = 0usize;
    let mut updated = 0usize;
    let mut tombstoned = 0usize;

    // desired 事务（§7.3）：集合变更原子生效。
    store.with_tx(|tx| {
        // upsert + insert（含 tombstone 复活）。
        for (stable_id, m) in &desired {
            let cur = current.get(*stable_id);
            let now = timefmt::now();
            let (action, _need_hash_refresh): (&str, bool) = match cur {
                None => ("insert", true),
                Some(c) => {
                    let present = c["present"].as_i64().unwrap_or(0);
                    let name_changed = c["name"].as_str().unwrap_or("") != m.name;
                    let locator_changed = c["locator"].as_str().unwrap_or("") != m.locator;
                    let enabled_changed = (c["enabled"].as_i64().unwrap_or(0) == 1) != m.enabled;
                    if present == 0 {
                        ("revive", true)
                    } else if name_changed || locator_changed || enabled_changed {
                        ("update", false)
                    } else {
                        ("none", false)
                    }
                }
            };
            match action {
                "insert" | "revive" => {
                    let row_id = cur
                        .map(|c| c["rowId"].as_str().unwrap_or_default().to_string())
                        .unwrap_or_default();
                    if row_id.is_empty() {
                        tx.execute(
                            "INSERT INTO knowledge_sources(id, project_id, kind, name, locator,
                             enabled, scan_state, created_at, updated_at, origin, stable_id,
                             identity_digest, manifest_sha256, active_manifest_sha256, present,
                             input_revision_mode)
                             VALUES (?1,?2,?3,?4,?5,?6,'pending',?7,?7,'manifest',?8,'',?9,?9,1,'committed')",
                            rusqlite::params![
                                sg_store::ids::new_id("ks"),
                                project_id,
                                m.kind,
                                m.name,
                                m.locator,
                                m.enabled as i64,
                                now,
                                stable_id,
                                file_sha_or_empty(&manifest_file(root, m)),
                            ],
                        )?;
                    } else {
                        // 复活原 tombstone 行：行身份不变、present 回 1（A04）。
                        tx.execute(
                            "UPDATE knowledge_sources SET name=?3, locator=?4, enabled=?5,
                             present=1, kind=?6, active_manifest_sha256=?7, manifest_sha256=?7,
                             updated_at=?8
                             WHERE id=?1 AND project_id=?2",
                            rusqlite::params![
                                row_id, project_id, m.name, m.locator, m.enabled as i64, m.kind,
                                file_sha_or_empty(&manifest_file(root, m)), now
                            ],
                        )?;
                    }
                    inserted += 1;
                }
                "update" => {
                    let row_id = cur.map(|c| c["rowId"].as_str().unwrap_or("")).unwrap_or("");
                    tx.execute(
                        "UPDATE knowledge_sources SET name=?3, locator=?4, enabled=?5, updated_at=?6
                         WHERE id=?1 AND project_id=?2",
                        rusqlite::params![
                            row_id, project_id, m.name, m.locator, m.enabled as i64, now
                        ],
                    )?;
                    updated += 1;
                }
                _ => {}
            }
        }
        // tombstone：DB − M（仅 manifest 行；present=1 → 0）。
        for (stable_id, c) in &current {
            if !desired.contains_key(stable_id.as_str()) && c["present"].as_i64().unwrap_or(0) == 1
            {
                tx.execute(
                    "UPDATE knowledge_sources SET present=0, updated_at=?3
                     WHERE project_id=?1 AND stable_id=?2 AND origin='manifest'",
                    rusqlite::params![project_id, stable_id, timefmt::now()],
                )?;
                tombstoned += 1;
            }
        }
        // desired_input_revision 刷新（committed 优先，git 失败回退 worktree Merkle）。
        for (stable_id, m) in &desired {
            if let Ok((mode, rev)) = compute_input_revision(root, &m.locator) {
                tx.execute(
                    "UPDATE knowledge_sources SET desired_input_revision=?3, input_revision_mode=?4
                     WHERE project_id=?1 AND stable_id=?2 AND origin='manifest' AND present=1",
                    rusqlite::params![project_id, stable_id, rev, mode],
                )?;
            }
        }
        Ok(())
    })?;

    Ok(json!({
        "inserted": inserted,
        "updated": updated,
        "tombstoned": tombstoned,
        "manifestCount": desired.len(),
    }))
}

/// 已知来源的 manifest 文件路径（按身份派生，文件名截断 12 位 + 可读前缀不可逆推 ——
/// 实际文件名前缀来自 name slug，这里按目录扫描匹配 identityDigest 前缀）。
fn manifest_file(root: &Path, m: &ValidManifest) -> PathBuf {
    let digest = m
        .stable_id
        .split_once('-')
        .map(|(_, d)| d.to_string())
        .unwrap_or_default();
    if m.kind == "document" {
        return manifest_dir(root).join(format!("doc-{}.json", &digest[..12.min(digest.len())]));
    }
    // repo_path：扫描目录找 stableId 匹配的文件。
    let dir = manifest_dir(root);
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.filter_map(|e| e.ok().map(|e| e.path())) {
            if let Ok(vm) = validate_file(&e) {
                if vm.stable_id == m.stable_id {
                    return e;
                }
            }
        }
    }
    dir.join(format!("repopath-{}.json", &digest[..12.min(digest.len())]))
}

/// 集合快照：digest + 逐源 (stableId, manifestSha, inputRevision, enabled, present)。
type SourceSet = (String, Vec<(String, String, String, i64, i64)>);

/// 当前 manifest 集合（present=1）的 canonical 快照与 digest（§9）。
fn current_source_set(store: &Store, project_id: &str) -> Result<SourceSet, Error> {
    let rows: Vec<(String, String, String, i64, i64)> = store.with_conn(|c| {
        let mut stmt = c.prepare(
            "SELECT stable_id, COALESCE(active_manifest_sha256,''), COALESCE(desired_input_revision,''),
                    enabled, present
             FROM knowledge_sources
             WHERE project_id=?1 AND origin='manifest' AND present=1
             ORDER BY stable_id",
        )?;
        let mut out = Vec::new();
        let mut rows = stmt.query([project_id])?;
        while let Some(r) = rows.next()? {
            out.push((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?));
        }
        Ok(out)
    })?;
    let mut arr = Vec::new();
    for (stable_id, msha, rev, enabled, present) in &rows {
        arr.push(json!({
            "stableId": stable_id,
            "manifestSha": msha,
            "inputRevision": rev,
            "enabled": *enabled == 1,
            "present": *present == 1,
        }));
    }
    let mut map = Map::new();
    map.insert("sources".into(), Value::Array(arr));
    let canonical = canonical_json(&Value::Object(map));
    let digest = sha256_hex(canonical.as_bytes());
    Ok((digest, rows))
}

/// §9 generation 幂等键 = sha256(source_set_digest + "|" + chunker_version)。
const CHUNKER_VERSION: &str = "chunk-v1";

/// 构建 + 激活当前集合的 generation（G3 主入口，reconcile 后调用）。
/// 返回 (generationId, status)。
pub fn build_and_activate_generation(
    store: &Store,
    project_id: &str,
    root: &Path,
) -> Result<(String, String), Error> {
    let (set_digest, detail) = current_source_set(store, project_id)?;
    if detail.is_empty() {
        return Ok((String::new(), "empty".into()));
    }
    let idem = sha256_hex(format!("{}|{}", set_digest, CHUNKER_VERSION).as_bytes());

    // 已有同幂等键的 ready/active generation → 复用（reconciler 两遍不再重建）。
    let existing: Option<(String, String)> = store.with_conn(|c| {
        match c.query_row(
            "SELECT id, status FROM knowledge_generations
             WHERE project_id=?1 AND idempotency_key=?2 AND status IN ('ready','active')",
            rusqlite::params![project_id, idem],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ) {
            Ok(pair) => Ok(Some(pair)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(other) => Err(Error::from(other)),
        }
    })?;
    if let Some((gid, st)) = existing {
        return Ok((gid, st));
    }

    let gid = sg_store::ids::new_id("gen");
    let now = timefmt::now();
    // pending → building（同事务登记快照）。
    store.with_tx(|tx| {
        tx.execute(
            "INSERT INTO knowledge_generations(id, project_id, idempotency_key, source_set_digest,
             status, lease_owner, chunker_version, created_at)
             VALUES (?1,?2,?3,?4,'building','core',?5,?6)",
            rusqlite::params![gid, project_id, idem, set_digest, CHUNKER_VERSION, now],
        )?;
        for (stable_id, msha, rev, enabled, present) in &detail {
            let source_id: String = tx.query_row(
                "SELECT id FROM knowledge_sources WHERE project_id=?1 AND stable_id=?2 AND origin='manifest'",
                rusqlite::params![project_id, stable_id],
                |r| r.get(0),
            )?;
            tx.execute(
                "INSERT INTO knowledge_generation_sources(generation_id, project_id, source_id,
                 stable_id, manifest_sha256, input_revision, enabled, present)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                rusqlite::params![gid, project_id, source_id, stable_id, msha, rev, enabled, present],
            )?;
        }
        Ok(())
    })?;

    // building：逐源扫描（enabled 来源），成功后归一 indexed 版本（§7）。
    let mut scan_failed = String::new();
    for (stable_id, _msha, _rev, enabled, _present) in &detail {
        if *enabled != 1 {
            continue;
        }
        let (source_id, locator): (String, String) = store.with_conn(|c| {
            c.query_row(
                "SELECT id, locator FROM knowledge_sources WHERE project_id=?1 AND stable_id=?2",
                rusqlite::params![project_id, stable_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(Into::into)
        })?;
        let mode: String = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT COALESCE(input_revision_mode,'committed') FROM knowledge_sources WHERE id=?1",
                    [&source_id],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })?;
        let abs = root.join(&locator);
        let scan_root = ancestor_root(&abs);
        let scan_result = if mode == "committed" {
            crate::scan_modes::scan_source_committed(store, &source_id, root, 500, 2 << 20)
                .map(|_| ())
        } else {
            crate::scan_source(store, &source_id, Some(&scan_root), 500, 2 << 20).map(|_| ())
        };
        if let Err(e) = scan_result {
            let msg = e.to_string();
            // partial_indexed / indexed 是可接受的终态；其余终态与错误 → failed。
            if !msg.contains("scan_terminal:partial_indexed") {
                scan_failed = format!("{stable_id}: {msg}");
                break;
            }
        }
        store.with_conn(|c| {
            c.execute(
                "UPDATE knowledge_sources SET indexed_manifest_sha256=active_manifest_sha256,
                 indexed_input_revision=desired_input_revision, scan_state='indexed', updated_at=?2
                 WHERE id=?1",
                rusqlite::params![source_id, timefmt::now()],
            )?;
            // chunks 归属本 generation（§12：generation_id FK）。
            c.execute(
                "UPDATE knowledge_chunks SET generation_id=?2 WHERE source_id=?1 AND generation_id IS NULL",
                rusqlite::params![source_id, gid],
            )?;
            Ok(())
        })?;
    }

    if !scan_failed.is_empty() {
        store.with_conn(|c| {
            c.execute(
                "UPDATE knowledge_generations SET status='failed', error=?2 WHERE id=?1",
                rusqlite::params![gid, scan_failed],
            )?;
            Ok(())
        })?;
        return Err(Error::Message(format!(
            "generation_scan_failed:{scan_failed}"
        )));
    }
    store.with_conn(|c| {
        c.execute(
            "UPDATE knowledge_generations SET status='ready' WHERE id=?1",
            [&gid],
        )?;
        Ok(())
    })?;

    try_activate(store, project_id, &gid, &set_digest)
}

/// locator 可能指向文件：扫描器根取存在的最近祖先目录。
fn ancestor_root(path: &Path) -> PathBuf {
    let mut cur = path.to_path_buf();
    loop {
        if cur.is_dir() {
            return cur;
        }
        match cur.parent() {
            Some(p) => cur = p.to_path_buf(),
            None => return cur,
        }
    }
}

/// §7 消费前判定：来源内容是否 current（双版本对齐）。返回 (current, stale)。
pub fn source_current(store: &Store, source_row_id: &str) -> Result<(bool, bool), Error> {
    store.with_conn(|c| {
        let (am, im, dr, ir, enabled, present): (String, String, String, String, i64, i64) = c
            .query_row(
                "SELECT COALESCE(active_manifest_sha256,''), COALESCE(indexed_manifest_sha256,''),
                        COALESCE(desired_input_revision,''), COALESCE(indexed_input_revision,''),
                        enabled, present
                 FROM knowledge_sources WHERE id=?1",
                [source_row_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .map_err(|_| Error::Message("source_not_found".into()))?;
        let current = present == 1 && enabled == 1 && am == im && dr == ir && !am.is_empty();
        let stale = present == 1 && enabled == 1 && !current;
        Ok((current, stale))
    })
}

/// §5 current 分支：Context builder 只能消费这些来源（fail-closed 排除 stale）。
pub fn current_only_source_ids(store: &Store, project_id: &str) -> Result<Vec<String>, Error> {
    store.with_conn(|c| {
        let mut stmt = c.prepare(
            "SELECT id FROM knowledge_sources
             WHERE project_id=?1 AND origin='manifest' AND present=1 AND enabled=1
               AND active_manifest_sha256=indexed_manifest_sha256
               AND desired_input_revision=indexed_input_revision
               AND COALESCE(active_manifest_sha256,'')!=''",
        )?;
        let mut out = Vec::new();
        let mut rows = stmt.query([project_id])?;
        while let Some(r) = rows.next()? {
            out.push(r.get(0)?);
        }
        Ok(out)
    })
}

/// activation CAS + 单事务切换（§7.5/§9）。独立成函数便于直接测试 superseded 路径。
pub fn try_activate(
    store: &Store,
    project_id: &str,
    gid: &str,
    expected_set_digest: &str,
) -> Result<(String, String), Error> {
    let (now_digest, now_detail) = current_source_set(store, project_id)?;
    let mut mismatch = now_digest != expected_set_digest;
    if !mismatch {
        let snap: Vec<(String, String, String, i64, i64)> = store.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT stable_id, manifest_sha256, input_revision, enabled, present
                 FROM knowledge_generation_sources WHERE generation_id=?1 ORDER BY stable_id",
            )?;
            let mut out = Vec::new();
            let mut rows = stmt.query([gid])?;
            while let Some(r) = rows.next()? {
                out.push((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?));
            }
            Ok(out)
        })?;
        if snap.len() != now_detail.len() {
            mismatch = true;
        } else {
            for ((s1, m1, r1, e1, p1), (s2, m2, r2, e2, p2)) in snap.iter().zip(now_detail.iter()) {
                if s1 != s2 || m1 != m2 || r1 != r2 || *e1 != *e2 || *p1 != *p2 {
                    mismatch = true;
                    break;
                }
            }
        }
    }

    store.with_tx(|tx| {
        if mismatch {
            tx.execute(
                "UPDATE knowledge_generations SET status='superseded' WHERE id=?1",
                [gid],
            )?;
            return Ok((gid.to_string(), "superseded".to_string()));
        }
        let previous: Option<(String, String)> = match tx.query_row(
            "SELECT g.id, g.source_set_digest FROM knowledge_generation_active a
             JOIN knowledge_generations g ON g.id=a.generation_id
             WHERE a.project_id=?1",
            [project_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ) {
            Ok(pair) => Some(pair),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(other) => return Err(other.into()),
        };
        let now = timefmt::now();
        if let Some((prev_id, _prev_digest)) = &previous {
            tx.execute(
                "UPDATE knowledge_generations SET status='superseded' WHERE id=?1",
                [prev_id],
            )?;
            tx.execute(
                "INSERT INTO knowledge_generation_retention(generation_id, project_id, retained_until)
                 VALUES (?1,?2,?3)
                 ON CONFLICT(generation_id) DO UPDATE SET retained_until=excluded.retained_until",
                rusqlite::params![prev_id, project_id, timefmt::now_plus_days(7)],
            )?;
        }
        tx.execute(
            "UPDATE knowledge_generations SET status='active', activated_at=?2 WHERE id=?1",
            rusqlite::params![gid, now],
        )?;
        tx.execute(
            "INSERT INTO knowledge_generation_active(project_id, generation_id) VALUES (?1,?2)
             ON CONFLICT(project_id) DO UPDATE SET generation_id=excluded.generation_id",
            rusqlite::params![project_id, gid],
        )?;
        tx.execute(
            "INSERT INTO knowledge_generation_activation_history(id, project_id,
             previous_generation_id, previous_generation_key, current_generation_id,
             current_generation_key, reason, actor, source_set_digest, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,'activate','core',?7,?8)",
            rusqlite::params![
                sg_store::ids::new_id("act"),
                project_id,
                previous.as_ref().map(|(id, _)| id.clone()),
                previous
                    .as_ref()
                    .map(|(id, dg)| format!("{project_id}/{id}/{dg}"))
                    .unwrap_or_default(),
                gid,
                format!("{project_id}/{gid}/{expected_set_digest}"),
                expected_set_digest,
                now
            ],
        )?;
        Ok((gid.to_string(), "active".to_string()))
    })
}

/// §19.5 回退（A40）：回滚到上一 active generation（activation_history + retention 驱动）。
/// 保留期内：previous payload 可用 → 指针原子切换 + history(reason=rollback) + 新 retention。
pub fn rollback_to_previous(store: &Store, project_id: &str) -> Result<(String, String), Error> {
    let current: Option<(String, String)> = store.with_conn(|c| {
        match c.query_row(
            "SELECT a.generation_id, g.source_set_digest FROM knowledge_generation_active a
             JOIN knowledge_generations g ON g.id=a.generation_id WHERE a.project_id=?1",
            [project_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ) {
            Ok(pair) => Ok(Some(pair)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(other) => Err(Error::from(other)),
        }
    })?;
    let Some((current_id, _current_digest)) = current else {
        return Err(Error::Message("no_active_generation".into()));
    };
    // 找最近一次 history 里 previous 且 payload 可用、仍在 retention 的 generation。
    let target: Option<String> = store.with_conn(|c| {
        match c.query_row(
            "SELECT h.previous_generation_id FROM knowledge_generation_activation_history h
             JOIN knowledge_generations g ON g.id = h.previous_generation_id
             WHERE h.project_id=?1 AND h.current_generation_id=?2
               AND g.payload_available=1 AND g.status='superseded'
               AND EXISTS (SELECT 1 FROM knowledge_generation_retention r
                           WHERE r.generation_id=g.id AND r.retained_until > ?3)
             ORDER BY h.created_at DESC LIMIT 1",
            rusqlite::params![project_id, current_id, timefmt::now()],
            |r| r.get(0),
        ) {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(other) => Err(Error::from(other)),
        }
    })?;
    let Some(target_id) = target else {
        return Err(Error::Message("rollback_target_unavailable".into()));
    };
    store.with_tx(|tx| {
        let now = timefmt::now();
        tx.execute(
            "UPDATE knowledge_generations SET status='superseded' WHERE id=?1",
            [&current_id],
        )?;
        tx.execute(
            "INSERT INTO knowledge_generation_retention(generation_id, project_id, retained_until)
             VALUES (?1,?2,?3) ON CONFLICT(generation_id) DO UPDATE SET retained_until=excluded.retained_until",
            rusqlite::params![current_id, project_id, timefmt::now_plus_days(7)],
        )?;
        tx.execute(
            "UPDATE knowledge_generations SET status='active', activated_at=?2, payload_available=1 WHERE id=?1",
            rusqlite::params![target_id, now],
        )?;
        tx.execute(
            "UPDATE knowledge_generation_active SET generation_id=?2 WHERE project_id=?1",
            rusqlite::params![project_id, target_id],
        )?;
        tx.execute(
            "INSERT INTO knowledge_generation_activation_history(id, project_id,
             previous_generation_id, previous_generation_key, current_generation_id,
             current_generation_key, reason, actor, source_set_digest, created_at)
             VALUES (?1,?2,?3,'',?4,'','rollback','core','',?5)",
            rusqlite::params![
                sg_store::ids::new_id("act"),
                project_id,
                current_id,
                target_id,
                now
            ],
        )?;
        Ok((target_id.clone(), "active".to_string()))
    })
}

/// project_root 统一入口转发。
pub fn project_root(store: &Store, project_id: &str) -> Result<PathBuf, Error> {
    project_root_at(store, project_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 测试根：git 仓库 + docs/a.md + manifest 文件。
    fn setup() -> (Store, Tmp, PathBuf) {
        let dir = std::env::temp_dir().join(format!("sg-reconcile-{}", sg_store::ids::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        let root = dir.join("repo");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs").join("a.md"), "# 认证 登录\n").unwrap();
        // git 初始化 + 提交（committed 输入版本）。
        let _ = std::process::Command::new("git")
            .args(["-C", &root.to_string_lossy(), "init"])
            .output();
        let _ = std::process::Command::new("git")
            .args(["-C", &root.to_string_lossy(), "config", "user.email", "t@t"])
            .output();
        let _ = std::process::Command::new("git")
            .args(["-C", &root.to_string_lossy(), "config", "user.name", "t"])
            .output();
        let _ = std::process::Command::new("git")
            .args(["-C", &root.to_string_lossy(), "add", "."])
            .output();
        let _ = std::process::Command::new("git")
            .args(["-C", &root.to_string_lossy(), "commit", "-m", "init"])
            .output();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, local_root, created_at)
                     VALUES ('pj', 'u', 'n', 'p', 'main', ?1, '2026-01-01T00:00:00.000Z')",
                    [&root.to_string_lossy()],
                )
                .map_err(Into::into)
            })
            .unwrap();
        (store, Tmp(dir), root)
    }

    fn write_manifest(root: &Path, name: &str, locator: &str, enabled: bool) -> String {
        let norm = normalize_locator(locator).unwrap();
        let (digest, stable, _) = repo_path_identity(&norm, name);
        let slug = format!(
            "{}-{}-{}",
            stable.split('-').next().unwrap(),
            name_slug_of(name),
            &digest[..12]
        );
        let content = json!({
            "contentSha256": "", "enabled": enabled, "kind": "repo_path",
            "locator": locator, "name": name, "schemaVersion": 1, "stableId": stable
        });
        let dir = manifest_dir(root);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{}.json", slug)),
            canonical_json(&content) + "\n",
        )
        .unwrap();
        stable
    }

    fn name_slug_of(name: &str) -> String {
        let s: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        s.trim_matches('-').to_string()
    }

    #[test]
    fn reconcile_two_pass_idempotent_and_tombstone() {
        let (store, _t, root) = setup();
        let sid = write_manifest(&root, "设计文档", "docs", true);
        let r1 = sync_from_repo(&store, "pj", &root).unwrap();
        assert_eq!(r1["inserted"], json!(1));
        // 第二遍零变更（A13/A22 收敛）。
        let r2 = sync_from_repo(&store, "pj", &root).unwrap();
        assert_eq!(
            (&r2["inserted"], &r2["updated"], &r2["tombstoned"]),
            (&json!(0), &json!(0), &json!(0))
        );
        assert_eq!(r2["manifestCount"], json!(1));

        // 改名：stableId 由 locator 派生不变 → 同一行 upsert update（§10 收敛语义）。
        let sid2 = write_manifest(&root, "设计文档新名", "docs", true);
        assert_eq!(sid, sid2, "身份只由 kind+locator 派生，改名不换身份");
        let r3 = sync_from_repo(&store, "pj", &root).unwrap();
        assert_eq!(r3["updated"], json!(1), "name 变化 → upsert update");

        // 删除 manifest 文件 → tombstone（A05）。
        let digest = sid.split_once('-').unwrap().1;
        let dir = manifest_dir(&root);
        for e in std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
        {
            if e.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .contains(&digest[..12])
            {
                std::fs::remove_file(e).unwrap();
            }
        }
        let r4 = sync_from_repo(&store, "pj", &root).unwrap();
        assert_eq!(r4["tombstoned"], json!(1));
        let present: i64 = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT present FROM knowledge_sources WHERE stable_id=?1",
                    [&sid],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(present, 0);
    }

    #[test]
    fn revive_restores_tombstone_without_new_row() {
        let (store, _t, root) = setup();
        let sid = write_manifest(&root, "设计文档", "docs", true);
        sync_from_repo(&store, "pj", &root).unwrap();
        let digest = sid.split_once('-').unwrap().1;
        let dir = manifest_dir(&root);
        for e in std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
        {
            if e.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .contains(&digest[..12])
            {
                std::fs::remove_file(e).unwrap();
            }
        }
        sync_from_repo(&store, "pj", &root).unwrap();
        // 重新写回同一 manifest → 复活原行。
        write_manifest(&root, "设计文档", "docs", true);
        let r = sync_from_repo(&store, "pj", &root).unwrap();
        assert_eq!(r["inserted"], json!(1), "复活计入 inserted");
        let (present, rows): (i64, i64) = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT present, (SELECT COUNT(*) FROM knowledge_sources WHERE stable_id=?1) FROM knowledge_sources WHERE stable_id=?1",
                    [&sid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!((present, rows), (1, 1), "复活原行，无第二行（A04/A3b）");
    }

    #[test]
    fn generation_builds_activates_and_cas_supersedes() {
        let (store, _t, root) = setup();
        let _sid = write_manifest(&root, "设计文档", "docs", true);
        sync_from_repo(&store, "pj", &root).unwrap();
        let (gid, status) = build_and_activate_generation(&store, "pj", &root).unwrap();
        assert_eq!(status, "active");
        // 单 active + 指针 + history + retention。
        let (n_active, n_ptr, n_hist): (i64, i64, i64) = store
            .with_conn(|c| {
                let a: i64 = c.query_row("SELECT COUNT(*) FROM knowledge_generations WHERE project_id='pj' AND status='active'", [], |r| r.get::<_, i64>(0)).map_err(Error::from)?;
                let p: i64 = c.query_row("SELECT COUNT(*) FROM knowledge_generation_active WHERE project_id='pj'", [], |r| r.get::<_, i64>(0)).map_err(Error::from)?;
                let h: i64 = c.query_row("SELECT COUNT(*) FROM knowledge_generation_activation_history WHERE project_id='pj'", [], |r| r.get::<_, i64>(0)).map_err(Error::from)?;
                Ok((a, p, h))
            })
            .unwrap();
        assert_eq!((n_active, n_ptr, n_hist), (1, 1, 1));

        // 幂等：同集合再跑 → 复用同一 generation（不重复激活）。
        let (gid2, status2) = build_and_activate_generation(&store, "pj", &root).unwrap();
        assert_eq!((gid2.as_str(), status2.as_str()), (gid.as_str(), "active"));

        // A26/A16b：CAS superseded —— 构造快照与当前集合不一致的 ready generation，
        // try_activate 必须拒绝切换且不动旧 active。
        let bad_gid = sg_store::ids::new_id("gen");
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO knowledge_generations(id, project_id, idempotency_key, source_set_digest,
                     status, lease_owner, chunker_version, created_at)
                     VALUES (?1,'pj','bad-key','baddigest','ready','core','chunk-v1','2026-01-01T00:00:00.000Z')",
                    [&bad_gid],
                )
                .map_err(Into::into)
            })
            .unwrap();
        let (_g3, status3) = try_activate(&store, "pj", &bad_gid, "baddigest").unwrap();
        assert_eq!(status3, "superseded", "CAS 比对失败 → superseded");
        let still_active: i64 = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM knowledge_generations WHERE id=?1 AND status='active'",
                    [&gid],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(still_active, 1, "漂移时旧 active 不被顶替");
    }

    #[test]
    fn freshness_current_stale_and_current_branch() {
        let (store, _t, root) = setup();
        let sid = write_manifest(&root, "设计文档", "docs", true);
        sync_from_repo(&store, "pj", &root).unwrap();
        let (gid, status) = build_and_activate_generation(&store, "pj", &root).unwrap();
        assert_eq!(status, "active");
        let source_id: String = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT id FROM knowledge_sources WHERE stable_id=?1",
                    [&sid],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        let (current, stale) = source_current(&store, &source_id).unwrap();
        assert!(current && !stale);
        // current 分支包含该来源。
        let ids = current_only_source_ids(&store, "pj").unwrap();
        assert!(ids.contains(&source_id));
        assert!(!gid.is_empty());

        // 内容版本漂移（模拟 pull 提交了新内容）：HEAD 变 → desired 变 → stale。
        std::fs::write(root.join("docs").join("a.md"), "# 改过的内容\n").unwrap();
        let _ = std::process::Command::new("git")
            .args(["-C", &root.to_string_lossy(), "add", "."])
            .output();
        let _ = std::process::Command::new("git")
            .args(["-C", &root.to_string_lossy(), "commit", "-m", "update"])
            .output();
        // 手动重算 desired（模拟事件钩子/复核修复路径）。
        let (mode, rev) = compute_input_revision_pub(&root, "docs").unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE knowledge_sources SET desired_input_revision=?2, input_revision_mode=?3 WHERE id=?1",
                    rusqlite::params![source_id, rev, mode],
                )
                .map_err(Into::into)
            })
            .unwrap();
        let (current2, stale2) = source_current(&store, &source_id).unwrap();
        assert!(!current2 && stale2, "内容版本变化 → stale_last_good");
        let ids2 = current_only_source_ids(&store, "pj").unwrap();
        assert!(!ids2.contains(&source_id), "stale 来源不进 current 分支");
    }

    fn compute_input_revision_pub(root: &Path, locator: &str) -> Result<(String, String), Error> {
        compute_input_revision(root, locator)
    }
}

#[cfg(test)]
mod g5g6_tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use std::process::Command;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git(root: &Path, args: &[&str]) {
        Command::new("git")
            .arg("-C")
            .arg(root.to_string_lossy().as_ref())
            .args(args)
            .output()
            .unwrap();
    }

    fn setup() -> (Store, Tmp, PathBuf) {
        let dir = std::env::temp_dir().join(format!("sg-g56-{}", sg_store::ids::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        let root = dir.join("repo");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs").join("a.md"), "# 认证 登录\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["config", "user.email", "t@t"]);
        git(&root, &["config", "user.name", "t"]);
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "init"]);
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, local_root, created_at)
                     VALUES ('pj', 'u', 'n', 'p', 'main', ?1, '2026-01-01T00:00:00.000Z')",
                    [&root.to_string_lossy()],
                )
                .map_err(Into::into)
            })
            .unwrap();
        (store, Tmp(dir), root)
    }

    fn set_flag(store: &Store, key: &str, value: bool) {
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO app_settings(scope, project_id, key, value_json, revision, updated_at)
                     VALUES ('global', '', ?1, ?2, 1, ?3)
                     ON CONFLICT(scope, project_id, key) DO UPDATE SET value_json=excluded.value_json",
                    rusqlite::params![key, json!(value).to_string(), timefmt::now()],
                )
                .map_err(Into::into)
            })
            .unwrap();
    }

    fn write_manifest(store: &Store, root: &Path) -> String {
        crate::manifest::manifest_create(
            store,
            &json!({
                "projectId": "pj", "opId": format!("c-{}", sg_store::ids::new_id("o")),
                "kind": "repo_path", "name": "设计文档", "locator": "docs", "expectedAbsent": true
            }),
        )
        .unwrap();
        let dir = manifest_dir(root);
        let mut latest = None;
        for e in std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
        {
            latest = Some(e);
        }
        latest
            .unwrap()
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_string()
    }

    /// G5/A12 族：committed 扫描四类计数 + partial_indexed 终态 + 秘密单文件排除。
    #[test]
    fn committed_scan_counts_and_partial_state() {
        let (store, _t, root) = setup();
        for i in 0..4 {
            std::fs::write(
                root.join("docs").join(format!("ok{i}.md")),
                format!("# 认证 登录方案\\n正常内容 {i}\\n"),
            )
            .unwrap();
        }
        std::fs::write(
            root.join("docs").join("secret.md"),
            "aws_access_key_id = AKIAIOSFODNN7EXAMPLE\n",
        )
        .unwrap();
        std::fs::write(root.join("docs").join("bin.dat"), [0u8, 1, 2, 0, 3]).unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "files"]);
        let src = crate::create_source(&store, "pj", "repo_path", "设计文档", "docs").unwrap();
        let (_s, counts) =
            crate::scan_modes::scan_source_committed(&store, &src.id, &root, 500, 2 << 20).unwrap();
        assert_eq!(counts.indexed, 5);
        assert_eq!(counts.secret_skipped, 1, "单个秘密命中即排除");
        assert_eq!(counts.binary_skipped, 1, "NUL 探测二进制排除");
        let state: String = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT scan_state FROM knowledge_sources WHERE id=?1",
                    [&src.id],
                    |r| r.get(0),
                )
                .map_err(Error::from)
            })
            .unwrap();
        assert_eq!(state, "partial_indexed");
        let hits = crate::search(&store, "pj", "认证", 10).unwrap();
        assert!(!hits.is_empty(), "可索引内容照常检索");
    }

    /// G5/A20-A21：全 secret → failed(secret_threshold_exceeded)；纯二进制 → no_indexable_files。
    #[test]
    fn secret_threshold_and_no_indexable_states() {
        let (store, _t, root) = setup();
        std::fs::write(root.join("docs").join("s.md"), "AKIAIOSFODNN7EXAMPLE\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "secret"]);
        let src = crate::create_source(&store, "pj", "repo_path", "设计文档", "docs").unwrap();
        assert!(
            crate::scan_modes::scan_source_committed(&store, &src.id, &root, 500, 2 << 20).is_err()
        );
        let (state, reason): (String, String) = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT scan_state, error FROM knowledge_sources WHERE id=?1",
                    [&src.id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(Error::from)
            })
            .unwrap();
        assert_eq!(state, "failed");
        assert!(reason.contains("secret_threshold_exceeded"));

        // 纯二进制目录。
        let dir2 = root.join("bin");
        std::fs::create_dir_all(&dir2).unwrap();
        std::fs::write(dir2.join("b.dat"), [0u8, 1, 2, 3]).unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "bin"]);
        let src2 = crate::create_source(&store, "pj", "repo_path", "二进制来源", "bin").unwrap();
        assert!(
            crate::scan_modes::scan_source_committed(&store, &src2.id, &root, 500, 2 << 20)
                .is_err()
        );
        let state2: String = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT scan_state FROM knowledge_sources WHERE id=?1",
                    [&src2.id],
                    |r| r.get(0),
                )
                .map_err(Error::from)
            })
            .unwrap();
        assert_eq!(state2, "no_indexable_files", "A21 纯二进制终态");
    }

    /// G6/A39-A40：GC 暂停 + 回退演练（manifest flag 已随「无版本开关」决策移除，路径常开）。
    #[test]
    fn flags_gc_pause_and_rollback_drill() {
        let (store, _t, root) = setup();

        // gen1 激活。
        write_manifest(&store, &root);
        sync_from_repo(&store, "pj", &root).unwrap();
        let (g1, st1) = build_and_activate_generation(&store, "pj", &root).unwrap();
        assert_eq!(st1, "active");

        // 提交新内容 → gen2 激活，gen1 superseded + retention。
        std::fs::write(root.join("docs").join("b.md"), "# 新内容\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "update"]);
        sync_from_repo(&store, "pj", &root).unwrap();
        let (g2, st2) = build_and_activate_generation(&store, "pj", &root).unwrap();
        assert_eq!(st2, "active");
        assert_ne!(g1, g2);

        // GC 暂停：不回收。
        set_flag(&store, "knowledge.gcPaused", true);
        let swept = crate::gc::sweep(&store, "pj").unwrap();
        assert_eq!(swept["paused"], json!(true));
        set_flag(&store, "knowledge.gcPaused", false);

        // 回退演练（A40）。
        let (target, status) = rollback_to_previous(&store, "pj").unwrap();
        assert_eq!((target.as_str(), status.as_str()), (g1.as_str(), "active"));
        let payload: i64 = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT payload_available FROM knowledge_generations WHERE id=?1",
                    [&g1],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(Error::from)
            })
            .unwrap();
        assert_eq!(payload, 1);
        let hits = crate::search(&store, "pj", "认证", 10).unwrap();
        assert!(!hits.is_empty(), "回退后检索恢复");
    }
}
