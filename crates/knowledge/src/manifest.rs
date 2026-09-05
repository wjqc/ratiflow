//! 知识来源清单（RFC《知识来源清单随仓库走》v1.0 G2）：仓库 manifest 为权威，
//! repository-first 写协议（锁 + CAS + durable receipt），SQLite 为投影。
//! 本模块实现 §3 身份、§6 写协议、§13.1 manifest RPC 语义。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};
use sg_store::{ids, timefmt, Error, Store};
use sha2::{Digest, Sha256};

/// 六角小写摘要。
fn hex64(bytes: &[u8]) -> String {
    ids::hex(bytes)
}

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    hex64(&Sha256::digest(data))
}

/// §3.1 locator 规范化：POSIX 分隔符、无前导 '/'、无 '..' 段。
/// canonicalize 后必须在仓库根内的校验在调用方（有 project_root 时）执行。
pub fn normalize_locator(locator: &str) -> Result<String, Error> {
    let trimmed = locator.trim();
    if trimmed.is_empty() {
        return Err(Error::Message("manifest_locator_empty".into()));
    }
    if trimmed.starts_with('/') || trimmed.starts_with('\\') {
        return Err(Error::Message("manifest_locator_absolute".into()));
    }
    let mut out = Vec::new();
    for seg in trimmed.split(['/', '\\']) {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            return Err(Error::Message("manifest_locator_escape".into()));
        }
        out.push(seg);
    }
    if out.is_empty() {
        return Err(Error::Message("manifest_locator_empty".into()));
    }
    Ok(out.join("/"))
}

/// name → 可读 fileSlug 片段（ascii 字母数字与 '-'）。
fn name_slug(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-');
    if s.is_empty() {
        "src".into()
    } else {
        s.chars().take(24).collect()
    }
}

/// §3.1 repo_path 身份：identityDigest / stableId（跨机可比）/ fileSlug（仅文件名）。
pub fn repo_path_identity(normalized_locator: &str, name: &str) -> (String, String, String) {
    let digest = sha256_hex(format!("v1|repo_path|{}", normalized_locator).as_bytes());
    let stable_id = format!("repopath-{}", digest);
    let slug = format!("repopath-{}-{}", name_slug(name), &digest[..12]);
    (digest, stable_id, slug)
}

/// §3.2 document 身份：contentSha256 只覆盖规范化正文（不含 frontmatter）。
pub fn normalize_body(body: &str) -> String {
    let trimmed = body.strip_prefix('\u{feff}').unwrap_or(body);
    trimmed.replace("\r\n", "\n").replace('\r', "\n")
}

pub fn document_identity(body_normalized: &str) -> (String, String) {
    let digest = sha256_hex(body_normalized.as_bytes());
    (digest.clone(), format!("doc-{}", digest))
}

/// canonical JSON：键排序（serde_json 默认 BTreeMap）、UTF-8、紧凑分隔由调用方落盘时保证。
pub fn canonical_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// 读取当前 manifest 文件：返回 (manifestSha256, 内容 bytes)。
pub(crate) fn read_manifest(path: &Path) -> Result<Option<(String, Vec<u8>)>, Error> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some((sha256_hex(&bytes), bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(e)),
    }
}

fn fsync_dir(dir: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        let f = std::fs::File::open(dir).map_err(Error::Io)?;
        f.sync_all().map_err(Error::Io)?;
    }
    Ok(())
}

/// 唯一命名临时文件 + fsync + 原子 rename + 目录 fsync（§6.2/§6.5）。
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::Message("manifest_path_no_parent".into()))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        ids::new_id("tmp")
    ));
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    fsync_dir(dir)?;
    Ok(())
}

/// §6.2 内部写者串行：Core 单实例全局写锁（跨实例 lockfile 留 §19 接线）。
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// 冲突 / 幂等错误统一用 Message token，dispatch 层映射错误码。
fn cas_conflict() -> Error {
    Error::Message("manifest_cas_conflict".into())
}
fn idempotency_conflict() -> Error {
    Error::Message("idempotency_conflict".into())
}

#[derive(Debug)]
struct Receipt {
    status: String,
    response: Option<Value>,
    fingerprint: String,
    project_id: String,
    stable_id: String,
}

fn load_receipt(store: &Store, op_id: &str) -> Result<Option<Receipt>, Error> {
    store.with_conn(|c| {
        let mut stmt = c.prepare(
            "SELECT status, response, request_fingerprint, project_id, stable_id
             FROM knowledge_ops WHERE op_id=?1",
        )?;
        let mut rows = stmt.query([op_id])?;
        if let Some(r) = rows.next()? {
            Ok(Some(Receipt {
                status: r.get(0)?,
                response: r
                    .get::<_, Option<String>>(1)?
                    .and_then(|s| serde_json::from_str(&s).ok()),
                fingerprint: r.get(2)?,
                project_id: r.get(3)?,
                stable_id: r.get(4)?,
            }))
        } else {
            Ok(None)
        }
    })
}

/// §6.4：TTL 内终态 receipt 保留（默认 7 天）；`intent/done_file` 永不被配额驱逐。
const RECEIPT_TTL_SECS: i64 = 7 * 24 * 3600;
const RECEIPT_QUOTA: i64 = 1000;
/// 卡住水位：超过即告警限流（§6.4，write_rejected_stuck_ops）。
const STUCK_WATERMARK: i64 = 50;

fn parse_ts(ts: &str) -> i64 {
    // timefmt::now 为 RFC3339；粗粒度比较足够 TTL 用途。
    chrono_hint(ts).unwrap_or(0)
}

fn chrono_hint(ts: &str) -> Option<i64> {
    // 手工解析 RFC3339 → unix 秒（避免引入 chrono 依赖；只精确到秒级容差）。
    let (date, rest) = ts.split_once('T')?;
    let rest = rest.trim_end_matches('Z').trim_end_matches("+00:00");
    let (time, _frac) = rest.split_once('.').unwrap_or((rest, ""));
    let mut it = date.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let mo: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    let mut tit = time.split(':');
    let h: i64 = tit.next()?.parse().ok()?;
    let mi: i64 = tit.next()?.parse().ok()?;
    let s: i64 = tit.next()?.parse().ok()?;
    // days from civil（Howard Hinnant 算法）。
    let y = if mo <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + h * 3600 + mi * 60 + s)
}

fn quota_and_stuck_check(store: &Store, project_id: &str) -> Result<(), Error> {
    let now = parse_ts(&timefmt::now());
    store.with_conn(|c| {
        // 卡住水位：intent/done_file 超阈值 → 限流（清理后自动恢复）。
        let stuck: i64 = c.query_row(
            "SELECT COUNT(*) FROM knowledge_ops
             WHERE project_id=?1 AND status IN ('intent','done_file')",
            [project_id],
            |r| r.get(0),
        )?;
        if stuck >= STUCK_WATERMARK {
            return Err(Error::Message("write_rejected_stuck_ops".into()));
        }
        // 配额只清终态且过 TTL 的行；无可清且满额 → write_rejected_quota。
        let terminal: i64 = c.query_row(
            "SELECT COUNT(*) FROM knowledge_ops WHERE project_id=?1 AND status IN ('done_db','failed')",
            [project_id],
            |r| r.get(0),
        )?;
        if terminal >= RECEIPT_QUOTA {
            // 逐行判断 TTL（finished_at + 7d < now）可清则先清。
            let mut stmt = c.prepare(
                "SELECT op_id, finished_at FROM knowledge_ops
                 WHERE project_id=?1 AND status IN ('done_db','failed') AND finished_at IS NOT NULL",
            )?;
            let mut to_delete = Vec::new();
            let mut rows = stmt.query([project_id])?;
            while let Some(r) = rows.next()? {
                let op: String = r.get(0)?;
                let fin: String = r.get(1)?;
                if parse_ts(&fin) + RECEIPT_TTL_SECS < now {
                    to_delete.push(op);
                }
            }
            let reclaimable = to_delete.len() as i64;
            for op in &to_delete {
                c.execute("DELETE FROM knowledge_ops WHERE op_id=?1", [op])?;
            }
            if terminal - reclaimable >= RECEIPT_QUOTA {
                return Err(Error::Message("write_rejected_quota".into()));
            }
        }
        Ok(())
    })
}

/// 解析项目仓库根（§13.1：由 Core 按已登记项目解析，不接受请求方根路径）。
pub(crate) fn project_root_at(store: &Store, project_id: &str) -> Result<PathBuf, Error> {
    let root: String = store.with_conn(|c| {
        c.query_row(
            "SELECT local_root FROM projects WHERE id=?1",
            [project_id],
            |r| r.get(0),
        )
        .map_err(|_| Error::Message("project_not_found".into()))
    })?;
    if root.trim().is_empty() {
        return Err(Error::Message("project_root_unavailable".into()));
    }
    let root = PathBuf::from(root);
    if !root.is_dir() {
        return Err(Error::Message("project_root_unavailable".into()));
    }
    // 归一化符号链接（macOS /tmp → /private/tmp），保证 locator 前缀校验可靠。
    let canon = root
        .canonicalize()
        .map_err(|_| Error::Message("project_root_unavailable".into()))?;
    Ok(canon)
}

pub(crate) fn manifest_dir(root: &Path) -> PathBuf {
    root.join("knowledge").join("sources")
}

/// 请求指纹：canonical 覆盖身份与 CAS 语义（§6.4）。
fn fingerprint(parts: &[&str]) -> String {
    use sha2::Digest as _;
    let joined = parts.join("\u{1f}");
    hex64(&Sha256::digest(joined.as_bytes()))
}

fn receipt_insert(
    store: &Store,
    op_id: &str,
    project_id: &str,
    stable_id: &str,
    op: &str,
    fp: &str,
    expected: Option<&str>,
) -> Result<(), Error> {
    store.with_conn(|c| {
        c.execute(
            "INSERT INTO knowledge_ops(op_id, project_id, stable_id, op, request_fingerprint,
             expected_manifest_sha256, status, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,'intent',?7)",
            rusqlite::params![
                op_id,
                project_id,
                stable_id,
                op,
                fp,
                expected,
                timefmt::now()
            ],
        )
        .map_err(|e| {
            if e.to_string().contains("UNIQUE") {
                idempotency_conflict()
            } else {
                e.into()
            }
        })?;
        Ok(())
    })
}

fn receipt_mark_file(store: &Store, op_id: &str, result_sha: &str) -> Result<(), Error> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE knowledge_ops SET status='done_file', result_manifest_sha256=?2, updated_at=?3
             WHERE op_id=?1",
            rusqlite::params![op_id, result_sha, timefmt::now()],
        )?;
        Ok(())
    })
}

fn receipt_finish(store: &Store, op_id: &str, response: &Value) -> Result<(), Error> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE knowledge_ops SET status='done_db', response=?2, finished_at=?3, updated_at=?3
             WHERE op_id=?1",
            rusqlite::params![op_id, response.to_string(), timefmt::now()],
        )?;
        Ok(())
    })
}

fn receipt_fail(store: &Store, op_id: &str, err: &str) {
    let _ = store.with_conn(|c| {
        c.execute(
            "UPDATE knowledge_ops SET status='failed', error=?2, finished_at=?3, updated_at=?3
             WHERE op_id=?1",
            rusqlite::params![op_id, err, timefmt::now()],
        )?;
        Ok(())
    });
}

/// git worktree 状态（§6.1 三档，porcelain 全量判据；git 不可用返回空串=未知）。
fn git_worktree_state(root: &Path, rel: &str) -> String {
    let out = std::process::Command::new("git")
        .args([
            "-C",
            &root.to_string_lossy(),
            "status",
            "--porcelain=v1",
            "--",
            rel,
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            if text.trim().is_empty() {
                "committed".into()
            } else if text.starts_with("??") {
                "untracked".into()
            } else {
                "modified".into()
            }
        }
        _ => String::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn projection_upsert(
    store: &Store,
    project_id: &str,
    source_row_id: &str,
    stable_id: &str,
    identity_digest: &str,
    kind: &str,
    name: &str,
    locator: &str,
    manifest_sha: &str,
) -> Result<(), Error> {
    store.with_conn(|c| {
        let existing: Option<(String, i64)> = c
            .query_row(
                "SELECT id, present FROM knowledge_sources
                 WHERE project_id=?1 AND stable_id=?2 AND origin='manifest'",
                rusqlite::params![project_id, stable_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        match existing {
            Some((_id, present)) => {
                if present == 0 {
                    // 墓碑复活（A04）：present 回 1、hash 刷新，行身份不变。
                    c.execute(
                        "UPDATE knowledge_sources SET present=1, manifest_sha256=?3,
                         active_manifest_sha256=?3, name=?4, locator=?5, kind=?6,
                         identity_digest=?7, updated_at=?8 WHERE id=?1 AND project_id=?2",
                        rusqlite::params![
                            _id,
                            project_id,
                            manifest_sha,
                            name,
                            locator,
                            kind,
                            identity_digest,
                            timefmt::now()
                        ],
                    )?;
                } else {
                    // 已有同名来源且 present=1：若投影 hash 已一致则 no-op，否则刷新（reconcile upsert 语义）。
                    c.execute(
                        "UPDATE knowledge_sources SET manifest_sha256=?3, active_manifest_sha256=?3,
                         name=?4, locator=?5, identity_digest=?6, updated_at=?7
                         WHERE id=?1 AND project_id=?2",
                        rusqlite::params![
                            _id, project_id, manifest_sha, name, locator, identity_digest,
                            timefmt::now()
                        ],
                    )?;
                }
            }
            None => {
                c.execute(
                    "INSERT INTO knowledge_sources(id, project_id, kind, name, locator, enabled,
                     scan_state, created_at, updated_at, origin, stable_id, identity_digest,
                     manifest_sha256, active_manifest_sha256, present)
                     VALUES (?1,?2,?3,?4,?5,1,'pending',?6,?6,'manifest',?7,?8,?9,?9,1)",
                    rusqlite::params![
                        source_row_id,
                        project_id,
                        kind,
                        name,
                        locator,
                        timefmt::now(),
                        stable_id,
                        identity_digest,
                        manifest_sha
                    ],
                )?;
            }
        }
        Ok(())
    })
}

fn projection_tombstone(store: &Store, project_id: &str, stable_id: &str) -> Result<(), Error> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE knowledge_sources SET present=0, updated_at=?3
             WHERE project_id=?1 AND stable_id=?2 AND origin='manifest'",
            rusqlite::params![project_id, stable_id, timefmt::now()],
        )?;
        Ok(())
    })
}

fn response_value(status: &str, stable_id: &str, manifest_sha: &str, git_state: &str) -> Value {
    json!({
        "status": status,
        "stableId": stable_id,
        "manifestSha256": manifest_sha,
        "worktreeState": git_state,
        "publicationState": "unpublished",
    })
}

fn validate_manifest_content(
    kind: &str,
    content: &Value,
    expect_stable: &str,
) -> Result<(), Error> {
    if content.get("schemaVersion").and_then(|v| v.as_i64()) != Some(1) {
        return Err(Error::Message("manifest_schema_unsupported".into()));
    }
    let sid = content
        .get("stableId")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if sid != expect_stable {
        return Err(Error::Message("manifest_identity_mismatch".into()));
    }
    if content.get("kind").and_then(|v| v.as_str()) != Some(kind) {
        return Err(Error::Message("manifest_identity_mismatch".into()));
    }
    Ok(())
}

fn render_manifest(
    stable_id: &str,
    kind: &str,
    name: &str,
    locator: &str,
    enabled: bool,
    content_sha: &str,
) -> Vec<u8> {
    let content = json!({
        "contentSha256": content_sha,
        "enabled": enabled,
        "kind": kind,
        "locator": locator,
        "name": name,
        "schemaVersion": 1,
        "stableId": stable_id,
    });
    let mut s = canonical_json(&content).into_bytes();
    s.push(b'\n');
    s
}

fn file_slug_unique(dir: &Path, candidate: &str, stable_id: &str) -> String {
    // §3.3 单侧升级：12 位截断与既有不同身份文件碰撞 → 新文件升 24 位，仍碰撞升 64 位。
    for level in [12usize, 24, 64] {
        let slug = truncate_digest(candidate, level);
        let target = dir.join(format!("{}.json", slug));
        if !target.exists() {
            return slug;
        }
        if let Ok(Some((_, bytes))) = read_manifest(&target) {
            if let Ok(v) = serde_json::from_str::<Value>(&String::from_utf8_lossy(&bytes)) {
                if v.get("stableId").and_then(|s| s.as_str()) == Some(stable_id) {
                    return slug; // 同一身份的既有文件
                }
            }
        }
    }
    truncate_digest(candidate, 64)
}

fn truncate_digest(slug: &str, digest_len: usize) -> String {
    // fileSlug 形如 <prefix>-<digest>；digest 部分截断到 digest_len 位。
    match slug.rfind('-') {
        Some(pos) => {
            let digest = &slug[pos + 1..];
            let base = &slug[..pos + 1];
            format!("{}{}", base, &digest[..digest_len.min(digest.len())])
        }
        None => slug.to_string(),
    }
}

fn source_row_id(
    store: &Store,
    project_id: &str,
    stable_id: &str,
) -> Result<Option<String>, Error> {
    store.with_conn(|c| {
        let r: Option<String> = c
            .query_row(
                "SELECT id FROM knowledge_sources WHERE project_id=?1 AND stable_id=?2 AND origin='manifest'",
                rusqlite::params![project_id, stable_id],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(r)
    })
}

/// §13.1 knowledge.manifestCreate。
pub fn manifest_create(store: &Store, params: &Value) -> Result<Value, Error> {
    let project_id = str_field(params, "projectId")?;
    let op_id = str_field(params, "opId")?;
    let kind = str_field(params, "kind")?;
    let name = str_field(params, "name")?;
    if !matches!(kind.as_str(), "repo_path" | "document") {
        return Err(Error::Message("manifest_kind_not_allowed".into()));
    }
    let enabled = params
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    if !params
        .get("expectedAbsent")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Err(Error::Message("expected_absent_required".into()));
    }

    let root = project_root_at(store, &project_id)?;
    quota_and_stuck_check(store, &project_id)?;

    let (locator, identity_digest, stable_id, body) = match kind.as_str() {
        "repo_path" => {
            let locator = normalize_locator(&str_field(params, "locator")?)?;
            let abs = root.join(&locator);
            let canon = abs
                .canonicalize()
                .map_err(|_| Error::Message("manifest_locator_missing".into()))?;
            if !canon.starts_with(&root) {
                return Err(Error::Message("manifest_locator_escape".into()));
            }
            let (digest, stable, _) = repo_path_identity(&locator, &name);
            (locator, digest, stable, None)
        }
        "document" => {
            let raw = str_field(params, "body")?;
            let body = normalize_body(&raw);
            // 秘密扫描前置（§16 附件命中拒绝落盘）：明文先进 git 工作区，必须在写盘前拦截。
            if sg_store::scan::has_high_risk(&sg_store::scan::scan(body.as_bytes())) {
                return Err(Error::Message("object_contains_secrets".into()));
            }
            let (digest, stable) = document_identity(&body);
            (
                format!("knowledge/attachments/{}/source.md", stable),
                digest,
                stable,
                Some(body),
            )
        }
        _ => unreachable!(),
    };

    // 幂等键：同 opId 同指纹 → 原终态；不一致 → conflict（§6.4）。
    let fp = fingerprint(&[
        "create",
        &project_id,
        &stable_id,
        &kind,
        &name,
        &locator,
        if enabled { "1" } else { "0" },
        "expectedAbsent",
    ]);
    if let Some(r) = load_receipt(store, &op_id)? {
        return receipt_replay(&r, &fp, &project_id, &stable_id);
    }
    quota_gate_insert(store, &project_id, &stable_id, &op_id, "create", &fp, None)?;

    let _guard = WRITE_LOCK
        .lock()
        .map_err(|_| Error::Message("write_lock_poisoned".into()))?;

    // CAS expectedAbsent（锁内重检，§6.2）。
    let dir = manifest_dir(&root);
    let (_, _, candidate_slug) = repo_path_identity(&locator, &name);
    let slug = if kind == "document" {
        format!("doc-{}", &identity_digest[..12])
    } else {
        file_slug_unique(&dir, &candidate_slug, &stable_id)
    };
    let path = dir.join(format!("{}.json", slug));

    let content_sha = if kind == "document" {
        body.as_ref()
            .map(|b| sha256_hex(b.as_bytes()))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let bytes = render_manifest(&stable_id, &kind, &name, &locator, enabled, &content_sha);
    let manifest_sha = sha256_hex(&bytes);

    if let Some((current_sha, current_bytes)) = read_manifest(&path)? {
        if current_bytes == bytes {
            // 幂等成功：内容已等于目标（§6.3）。
            return finish_create(
                store,
                &project_id,
                &stable_id,
                &identity_digest,
                &kind,
                &name,
                &locator,
                &current_sha,
                &op_id,
                "",
            );
        }
        receipt_fail(store, &op_id, "manifest_cas_conflict");
        return Err(cas_conflict());
    }

    // 文档正文落盘（§3.2 步骤 6；正文身份已定，无循环）。
    // contentSha256 只覆盖正文；frontmatter 携带该 hash 与原始元数据。
    if let Some(body_text) = &body {
        let att_dir = root.join("knowledge").join("attachments").join(&stable_id);
        let mut doc = String::new();
        doc.push_str("---\n");
        doc.push_str(&format!("contentSha256: {}\n", content_sha));
        doc.push_str(&format!("importedAt: {}\n", timefmt::now()));
        doc.push_str(&format!("originalName: {}\n", name));
        doc.push_str("---\n");
        doc.push_str(body_text);
        atomic_write(&att_dir.join("source.md"), doc.as_bytes())?;
    }

    atomic_write(&path, &bytes)?;
    let git_state = git_worktree_state(
        &root,
        &path
            .strip_prefix(&root)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
    );
    receipt_mark_file(store, &op_id, &manifest_sha)?;

    finish_create(
        store,
        &project_id,
        &stable_id,
        &identity_digest,
        &kind,
        &name,
        &locator,
        &manifest_sha,
        &op_id,
        &git_state,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_create(
    store: &Store,
    project_id: &str,
    stable_id: &str,
    identity_digest: &str,
    kind: &str,
    name: &str,
    locator: &str,
    manifest_sha: &str,
    op_id: &str,
    git_state: &str,
) -> Result<Value, Error> {
    let row_id = match source_row_id(store, project_id, stable_id)? {
        Some(id) => id,
        None => ids::new_id("ks"),
    };
    projection_upsert(
        store,
        project_id,
        &row_id,
        stable_id,
        identity_digest,
        kind,
        name,
        locator,
        manifest_sha,
    )?;
    let resp = response_value("projected", stable_id, manifest_sha, git_state);
    receipt_finish(store, op_id, &resp)?;
    Ok(resp)
}

fn receipt_replay(
    r: &Receipt,
    fp: &str,
    project_id: &str,
    stable_id: &str,
) -> Result<Value, Error> {
    if r.fingerprint == fp && r.project_id == project_id && r.stable_id == stable_id {
        if let Some(resp) = &r.response {
            return Ok(resp.clone());
        }
        // 终态但无 response（failed/conflict）：返回原失败语义。
        if r.status == "failed" {
            return Err(cas_conflict());
        }
        return Err(Error::Message("manifest_op_incomplete".into()));
    }
    Err(idempotency_conflict())
}

fn quota_gate_insert(
    store: &Store,
    project_id: &str,
    stable_id: &str,
    op_id: &str,
    op: &str,
    fp: &str,
    expected: Option<&str>,
) -> Result<(), Error> {
    quota_and_stuck_check(store, project_id)?;
    receipt_insert(store, op_id, project_id, stable_id, op, fp, expected)
}

fn str_field(params: &Value, key: &str) -> Result<String, Error> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::Message(format!("missing_param_{}", key)))
}

/// §13.1 knowledge.manifestUpdate（仅 name/enabled；locator/正文属身份，禁止原地改）。
pub fn manifest_update(store: &Store, params: &Value) -> Result<Value, Error> {
    let project_id = str_field(params, "projectId")?;
    let op_id = str_field(params, "opId")?;
    let stable_id = str_field(params, "stableId")?;
    let expected = str_field(params, "expectedManifestSha256")?;
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let enabled = params.get("enabled").and_then(|v| v.as_bool());
    if name.is_none() && enabled.is_none() {
        return Err(Error::Message("manifest_update_no_fields".into()));
    }
    if params.get("locator").is_some() || params.get("body").is_some() {
        return Err(Error::Message("manifest_update_identity_immutable".into()));
    }

    let root = project_root_at(store, &project_id)?;
    let row = load_source_row(store, &project_id, &stable_id)?;
    let (row_id, kind, old_name, old_locator, old_content_sha, old_enabled) = row;

    let fp = fingerprint(&[
        "update",
        &project_id,
        &stable_id,
        name.as_deref().unwrap_or(""),
        enabled.map(|b| b.to_string()).as_deref().unwrap_or(""),
        &expected,
    ]);
    if let Some(r) = load_receipt(store, &op_id)? {
        return receipt_replay(&r, &fp, &project_id, &stable_id);
    }
    quota_gate_insert(
        store,
        &project_id,
        &stable_id,
        &op_id,
        "update",
        &fp,
        Some(&expected),
    )?;

    let _guard = WRITE_LOCK
        .lock()
        .map_err(|_| Error::Message("write_lock_poisoned".into()))?;

    let dir = manifest_dir(&root);
    let (_, _, candidate_slug) = repo_path_identity(&old_locator, &old_name);
    let slug = match kind.as_str() {
        "document" => format!("doc-{}", &stable_id["doc-".len()..12.min(stable_id.len())]),
        _ => file_slug_known(&dir, &candidate_slug, &stable_id),
    };
    let path = dir.join(format!("{}.json", slug));

    let new_name = name.unwrap_or(old_name);
    let new_enabled = enabled.unwrap_or(old_enabled);
    let bytes = render_manifest(
        &stable_id,
        &kind,
        &new_name,
        &old_locator,
        new_enabled,
        &old_content_sha,
    );
    let new_sha = sha256_hex(&bytes);

    match read_manifest(&path)? {
        Some((current_sha, current_bytes)) => {
            if current_bytes == bytes {
                let resp = response_value("projected", &stable_id, &current_sha, "");
                receipt_finish(store, &op_id, &resp)?;
                return Ok(resp);
            }
            if current_sha != expected {
                receipt_fail(store, &op_id, "manifest_cas_conflict");
                return Err(cas_conflict());
            }
        }
        None => {
            receipt_fail(store, &op_id, "manifest_cas_conflict");
            return Err(cas_conflict());
        }
    }

    atomic_write(&path, &bytes)?;
    let rel = path
        .strip_prefix(&root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let git_state = git_worktree_state(&root, &rel);
    receipt_mark_file(store, &op_id, &new_sha)?;

    store.with_conn(|c| {
        c.execute(
            "UPDATE knowledge_sources SET name=COALESCE(?3,name), enabled=COALESCE(?4,enabled),
             manifest_sha256=?5, active_manifest_sha256=?5, updated_at=?6
             WHERE id=?1 AND project_id=?2",
            rusqlite::params![
                row_id,
                project_id,
                new_name,
                new_enabled,
                new_sha,
                timefmt::now()
            ],
        )?;
        Ok(())
    })?;
    let resp = response_value("projected", &stable_id, &new_sha, &git_state);
    receipt_finish(store, &op_id, &resp)?;
    Ok(resp)
}

fn file_slug_known(_dir: &Path, candidate: &str, _stable_id: &str) -> String {
    candidate.to_string()
}

fn load_source_row(
    store: &Store,
    project_id: &str,
    stable_id: &str,
) -> Result<(String, String, String, String, String, bool), Error> {
    store.with_conn(|c| {
        let (id, kind, name, locator, content_sha, enabled): (
            String,
            String,
            String,
            String,
            String,
            i64,
        ) = c
            .query_row(
                "SELECT id, kind, name, locator, COALESCE(content_sha256,''), enabled
                 FROM knowledge_sources
                 WHERE project_id=?1 AND stable_id=?2 AND origin='manifest' AND present=1",
                rusqlite::params![project_id, stable_id],
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
            .map_err(|_| Error::Message("manifest_source_not_found".into()))?;
        Ok((id, kind, name, locator, content_sha, enabled == 1))
    })
}

/// §13.1 knowledge.manifestRemove（repository-first：删文件 → 墓碑；§6.4 中断恢复）。
pub fn manifest_remove(store: &Store, params: &Value) -> Result<Value, Error> {
    let project_id = str_field(params, "projectId")?;
    let op_id = str_field(params, "opId")?;
    let stable_id = str_field(params, "stableId")?;
    let expected = str_field(params, "expectedManifestSha256")?;

    let root = project_root_at(store, &project_id)?;
    let row = load_source_row(store, &project_id, &stable_id)?;
    let (row_id, kind, name, locator, _content_sha, _enabled) = row;

    let fp = fingerprint(&["remove", &project_id, &stable_id, &expected]);
    if let Some(r) = load_receipt(store, &op_id)? {
        return receipt_replay(&r, &fp, &project_id, &stable_id);
    }
    quota_gate_insert(
        store,
        &project_id,
        &stable_id,
        &op_id,
        "remove",
        &fp,
        Some(&expected),
    )?;

    let _guard = WRITE_LOCK
        .lock()
        .map_err(|_| Error::Message("write_lock_poisoned".into()))?;

    let (_, _, candidate_slug) = repo_path_identity(&locator, &name);
    let slug = match kind.as_str() {
        "document" => format!("doc-{}", &stable_id["doc-".len()..12.min(stable_id.len())]),
        _ => file_slug_known(&manifest_dir(&root), &candidate_slug, &stable_id),
    };
    let path = manifest_dir(&root).join(format!("{}.json", slug));

    let current = read_manifest(&path)?;
    match current {
        Some((current_sha, _bytes)) => {
            if current_sha != expected {
                receipt_fail(store, &op_id, "manifest_cas_conflict");
                return Err(cas_conflict());
            }
            std::fs::remove_file(&path)?;
            fsync_dir(path.parent().unwrap_or(Path::new(".")))?;
            let rel = path
                .strip_prefix(&root)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let git_state = git_worktree_state(&root, &rel);
            receipt_mark_file(store, &op_id, &current_sha)?;
            projection_tombstone(store, &project_id, &stable_id)?;
            let resp = json!({
                "status": "projected",
                "stableId": stable_id,
                "worktreeState": git_state,
                "removed": true,
            });
            receipt_finish(store, &op_id, &resp)?;
            Ok(resp)
        }
        None => {
            // 中断恢复（§6.4）：文件缺失 + DB hash==expected → 补墓碑，幂等成功。
            let db_sha: String = store.with_conn(|c| {
                c.query_row(
                    "SELECT COALESCE(manifest_sha256,'') FROM knowledge_sources WHERE id=?1",
                    [&row_id],
                    |r| r.get::<_, String>(0),
                )
                .map_err(Into::into)
            })?;
            if db_sha == expected {
                projection_tombstone(store, &project_id, &stable_id)?;
                let resp = json!({
                    "status": "projected",
                    "stableId": stable_id,
                    "removed": true,
                    "recovered": true,
                });
                receipt_finish(store, &op_id, &resp)?;
                return Ok(resp);
            }
            receipt_fail(store, &op_id, "manifest_cas_conflict");
            Err(cas_conflict())
        }
    }
}

/// 启动 reconciliation（§6.4 第 4 条）：done_file 未 done_db → 按 CAS 补投影。
/// 返回修复的 op 数。
pub fn reconcile_pending_ops(store: &Store, roots: &[(String, PathBuf)]) -> Result<usize, Error> {
    let mut fixed = 0usize;
    let rows: Vec<(String, String, String, String, String, Option<String>)> =
        store.with_conn(|c| {
            let mut stmt = c.prepare(
            "SELECT op_id, project_id, stable_id, op, request_fingerprint, result_manifest_sha256
             FROM knowledge_ops WHERE status='done_file'",
        )?;
            let mut out = Vec::new();
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                out.push((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ));
            }
            Ok(out)
        })?;
    let root_map: HashSet<&str> = roots.iter().map(|(id, _)| id.as_str()).collect();
    for (op_id, project_id, stable_id, op, _fp, result_sha) in rows {
        if !root_map.contains(project_id.as_str()) {
            continue;
        }
        match op.as_str() {
            "remove" => {
                projection_tombstone(store, &project_id, &stable_id)?;
                let resp = json!({"status":"projected","stableId":stable_id,"removed":true,"recovered":true});
                receipt_finish(store, &op_id, &resp)?;
                fixed += 1;
            }
            _ => {
                if let Some(sha) = result_sha {
                    let _ = store.with_conn(|c| {
                        c.execute(
                            "UPDATE knowledge_sources SET manifest_sha256=?3, active_manifest_sha256=?3,
                             present=1, updated_at=?4
                             WHERE project_id=?1 AND stable_id=?2 AND origin='manifest'",
                            rusqlite::params![project_id, stable_id, sha, timefmt::now()],
                        )?;
                        Ok(())
                    });
                    let resp = response_value("projected", &stable_id, &sha, "");
                    receipt_finish(store, &op_id, &resp)?;
                    fixed += 1;
                }
            }
        }
    }
    Ok(fixed)
}

/// 校验既有 manifest 文件身份一致性（§3.3，reconcile 用）。
pub fn verify_manifest_file(path: &Path) -> Result<(), Error> {
    let (_, bytes) =
        read_manifest(path)?.ok_or_else(|| Error::Message("manifest_missing".into()))?;
    let v: Value = serde_json::from_str(&String::from_utf8_lossy(&bytes))
        .map_err(|_| Error::Message("manifest_invalid_json".into()))?;
    let sid = v.get("stableId").and_then(|s| s.as_str()).unwrap_or("");
    let file_slug = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let prefix_ok = match sid.split_once('-') {
        Some((prefix, digest)) => {
            file_slug.starts_with(prefix)
                && file_slug.rsplit('-').next() == Some(&digest[..12.min(digest.len())])
        }
        None => false,
    };
    if !prefix_ok {
        return Err(Error::Message("manifest_identity_mismatch".into()));
    }
    validate_manifest_content(
        v.get("kind").and_then(|s| s.as_str()).unwrap_or(""),
        &v,
        sid,
    )?;
    Ok(())
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

    fn setup() -> (Store, Tmp, PathBuf) {
        let dir = std::env::temp_dir().join(format!("sg-manifest-{}", ids::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        let root = dir.join("repo");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs").join("a.md"), "# hi\n").unwrap();
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

    #[test]
    fn identity_is_deterministic_and_locator_validated() {
        let norm = normalize_locator("docs/").unwrap();
        assert_eq!(norm, "docs");
        let (d1, s1, _) = repo_path_identity(&norm, "设计文档");
        let (d2, s2, _) = repo_path_identity("docs", "设计文档");
        assert_eq!(d1, d2);
        assert_eq!(s1, s2);
        assert!(s1.starts_with("repopath-"));
        assert!(normalize_locator("../etc").is_err());
        assert!(normalize_locator("/abs").is_err());
        assert!(normalize_locator("").is_err());
        // document：contentSha256 只覆盖规范化正文（LF、去 BOM）。
        let (h1, _) = document_identity(&normalize_body("\u{feff}a\r\nb"));
        let (h2, _) = document_identity("a\nb");
        assert_eq!(h1, h2);
    }

    #[test]
    fn create_projected_then_idempotent_replay() {
        let (store, _t, _root) = setup();
        let params = json!({
            "projectId": "pj", "opId": "op-1", "kind": "repo_path",
            "name": "设计文档", "locator": "docs", "enabled": true, "expectedAbsent": true
        });
        let r1 = manifest_create(&store, &params).unwrap();
        assert_eq!(r1["status"], "projected");
        let sid = r1["stableId"].as_str().unwrap().to_string();
        let sha = r1["manifestSha256"].as_str().unwrap().to_string();
        // 幂等重放：同 opId → 原终态响应。
        let r2 = manifest_create(&store, &params).unwrap();
        assert_eq!(r2["stableId"], json!(sid));
        assert_eq!(r2["manifestSha256"], json!(sha));
        // 同 opId 不同指纹 → idempotency_conflict。
        let mut conflict = params.clone();
        conflict["name"] = json!("别的名字");
        conflict["expectedAbsent"] = json!(true);
        let err = manifest_create(&store, &conflict).unwrap_err();
        assert!(err.to_string().contains("idempotency_conflict"));
        // 投影行存在且 present=1。
        let (origin, present): (String, i64) = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT origin, present FROM knowledge_sources WHERE stable_id=?1",
                    [&sid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!((origin.as_str(), present), ("manifest", 1));
    }

    #[test]
    fn create_cas_conflict_when_file_exists_with_other_content() {
        let (store, _t, root) = setup();
        let dir = manifest_dir(&root);
        std::fs::create_dir_all(&dir).unwrap();
        let (_, _stable, slug) = repo_path_identity("docs", "设计文档");
        std::fs::write(
            dir.join(format!("{}.json", slug)),
            format!("{{\"stableId\":\"{}\"}}", "repopath-other"),
        )
        .unwrap();
        let params = json!({
            "projectId": "pj", "opId": "op-2", "kind": "repo_path",
            "name": "设计文档", "locator": "docs", "expectedAbsent": true
        });
        let err = manifest_create(&store, &params).unwrap_err();
        assert!(err.to_string().contains("manifest_cas_conflict"));
    }

    #[test]
    fn update_requires_expected_hash_and_is_cas() {
        let (store, _t, root) = setup();
        let create = json!({
            "projectId": "pj", "opId": "c1", "kind": "repo_path",
            "name": "设计文档", "locator": "docs", "expectedAbsent": true
        });
        let r = manifest_create(&store, &create).unwrap();
        let sid = r["stableId"].as_str().unwrap().to_string();
        let sha = r["manifestSha256"].as_str().unwrap().to_string();

        // 错误 expected → CAS conflict。
        let upd_bad = json!({
            "projectId": "pj", "opId": "u0", "stableId": sid,
            "expectedManifestSha256": "deadbeef", "name": "新名"
        });
        let err = manifest_update(&store, &upd_bad).unwrap_err();
        assert!(err.to_string().contains("manifest_cas_conflict"));

        // 正确 expected → 更新成功，文件内容更新。
        let upd = json!({
            "projectId": "pj", "opId": "u1", "stableId": sid,
            "expectedManifestSha256": sha, "enabled": false
        });
        let r2 = manifest_update(&store, &upd).unwrap();
        assert_eq!(r2["status"], "projected");
        let enabled: i64 = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT enabled FROM knowledge_sources WHERE stable_id=?1",
                    [&sid],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(enabled, 0);

        // locator/正文属身份：更新拒绝。
        let upd_id = json!({
            "projectId": "pj", "opId": "u2", "stableId": sid,
            "expectedManifestSha256": r2["manifestSha256"], "locator": "other"
        });
        assert!(manifest_update(&store, &upd_id).is_err());
        let _ = root;
    }

    #[test]
    fn remove_tombstone_and_intent_recovery() {
        let (store, _t, root) = setup();
        let create = json!({
            "projectId": "pj", "opId": "c1", "kind": "repo_path",
            "name": "设计文档", "locator": "docs", "expectedAbsent": true
        });
        let r = manifest_create(&store, &create).unwrap();
        let sid = r["stableId"].as_str().unwrap().to_string();
        let sha = r["manifestSha256"].as_str().unwrap().to_string();

        // 正常删除 → 墓碑。
        let rm = json!({
            "projectId": "pj", "opId": "r1", "stableId": sid,
            "expectedManifestSha256": sha
        });
        let rr = manifest_remove(&store, &rm).unwrap();
        assert_eq!(rr["removed"], json!(true));
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
        // 行仍在（tombstone 不物理删）。
        let count: i64 = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM knowledge_sources WHERE stable_id=?1",
                    [&sid],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(count, 1);

        // 中断恢复（§6.4）：remove 在"文件已删、DB 未墓碑"之间崩溃 ——
        // 模拟：回拨 DB present=1、receipt 停在 done_file，再 reconcile。
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE knowledge_sources SET present=1 WHERE stable_id=?1",
                    [&sid],
                )
                .map_err(Into::into)
            })
            .unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE knowledge_ops SET status='done_file' WHERE op_id='r1'",
                    [],
                )
                .map_err(Into::into)
            })
            .unwrap();
        let fixed = reconcile_pending_ops(&store, &[("pj".into(), root.clone())]).unwrap();
        assert_eq!(fixed, 1);
        let present2: i64 = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT present FROM knowledge_sources WHERE stable_id=?1",
                    [&sid],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(present2, 0, "reconcile 必须补齐墓碑");
        // 复活原行（A04）：重加同一来源 → 行身份不变、present 回 1。
        let rc = json!({
            "projectId": "pj", "opId": "c2", "kind": "repo_path",
            "name": "设计文档", "locator": "docs", "expectedAbsent": true
        });
        let r2 = manifest_create(&store, &rc).unwrap();
        assert_eq!(r2["stableId"], json!(sid), "重加必须复活同一身份");
        let (present3, rows): (i64, i64) = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT present, (SELECT COUNT(*) FROM knowledge_sources WHERE stable_id=?1) FROM knowledge_sources WHERE stable_id=?1",
                    [&sid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!((present3, rows), (1, 1), "tombstone 复活，不产生第二行");
    }

    #[test]
    fn document_create_writes_attachment_with_body_hash() {
        let (store, _t, root) = setup();
        let body = "# 笔记\n正文\n";
        let create = json!({
            "projectId": "pj", "opId": "d1", "kind": "document",
            "name": "笔记", "body": body, "expectedAbsent": true
        });
        let r = manifest_create(&store, &create).unwrap();
        let sid = r["stableId"].as_str().unwrap().to_string();
        assert!(sid.starts_with("doc-"));
        let written = std::fs::read_to_string(
            root.join("knowledge")
                .join("attachments")
                .join(&sid)
                .join("source.md"),
        )
        .unwrap();
        let (digest, stable2) = document_identity(&normalize_body(body));
        assert_eq!(stable2, sid);
        assert!(written.contains(&digest), "frontmatter 记录正文 hash");
        assert!(written.contains(body), "正文原样落盘");
    }
}
