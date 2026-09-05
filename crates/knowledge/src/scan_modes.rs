//! 双模式扫描（RFC v1.0 §13.2）+ §15 secret 终态：committed（git tree 精确输入，
//! 跨机确定）为 manifest 来源默认；worktree 复用既有 scan_source（标 local/dirty）。
use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};
use sg_store::{ids, objects, scan, timefmt, Error, Store};

/// §15 secret 阈值（完整度策略；单个秘密命中即排除该文件，无阈值）。
pub const SECRET_THRESHOLD: f64 = 0.3;

#[derive(Debug, Default, Clone)]
pub struct ScanCounts {
    pub indexed: i64,
    pub secret_skipped: i64,
    pub binary_skipped: i64,
    pub unreadable: i64,
}

impl ScanCounts {
    pub fn to_json(&self) -> Value {
        json!({
            "indexed": self.indexed,
            "secretSkipped": self.secret_skipped,
            "binarySkipped": self.binary_skipped,
            "unreadable": self.unreadable,
        })
    }
}

/// §15 终态判定（互斥，自上而下；安全判定先于完整度）。
pub fn terminal_state(c: &ScanCounts) -> (&'static str, Option<&'static str>) {
    let indexable = c.indexed + c.secret_skipped + c.unreadable;
    let total = indexable + c.binary_skipped;
    if total == 0 {
        return ("no_indexable_files", None);
    }
    if indexable == 0 {
        // 全部为 binary/LFS/非 UTF-8：无可索引内容（§15 终态表条件 2）。
        return ("no_indexable_files", None);
    }
    let secret_rate = c.secret_skipped as f64 / indexable as f64;
    if secret_rate > SECRET_THRESHOLD {
        return ("failed", Some("secret_threshold_exceeded"));
    }
    if c.indexed == 0 {
        return ("scan_failed", None);
    }
    let completeness = c.indexed as f64 / total as f64;
    if completeness < 1.0 {
        return ("partial_indexed", None);
    }
    ("indexed", None)
}

fn git(root: &Path, args: &[&str]) -> Result<String, Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root.to_string_lossy().as_ref())
        .args(args)
        .output()
        .map_err(|e| Error::Message(format!("git spawn: {e}")))?;
    if !out.status.success() {
        return Err(Error::Message(format!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// committed 模式扫描（§13.2）：以 HEAD 的 git tree 为输入，只读跟踪 blob。
/// 排序 = git 输出（路径字节序）；LFS pointer/非 UTF-8/二进制 skipped；秘密单文件排除。
pub fn scan_source_committed(
    store: &Store,
    source_id: &str,
    project_root: &Path,
    max_files: usize,
    max_file_bytes: u64,
) -> Result<(crate::Source, ScanCounts), Error> {
    let source = crate::get_source(store, source_id)?;
    crate::set_scan_state(store, source_id, "scanning", "")?;
    let locator = source.locator.trim_end_matches('/').to_string();

    // 枚举 HEAD:<locator> 的 blob（committed 输入集合）。
    let spec = format!("HEAD:{locator}");
    let listing = git(project_root, &["ls-tree", "-r", &spec]).inspect_err(|e| {
        let _ = crate::set_scan_state(store, source_id, "failed", &e.to_string());
    })?;
    let mut blobs: Vec<(String, String)> = listing // (path, blob sha)
        .lines()
        .filter_map(|line| {
            let (meta, path) = line.split_once('\t')?;
            let mode_type_sha: Vec<&str> = meta.split_whitespace().collect();
            if mode_type_sha.get(1) == Some(&"blob") {
                Some((path.to_string(), mode_type_sha[2].to_string()))
            } else {
                None
            }
        })
        .collect();
    blobs.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    if blobs.len() > max_files {
        blobs.truncate(max_files);
    }

    use sha2::{Digest, Sha256};
    let mut root_hasher = Sha256::new();
    let mut chunks: Vec<(usize, String, i64)> = Vec::new();
    let mut chunk_rows: Vec<(String, String, String)> = Vec::new();
    let mut next_ordinal: usize = 0usize;
    let mut counts = ScanCounts::default();

    for (path, blob_sha) in &blobs {
        let body = match git(project_root, &["cat-file", "blob", blob_sha]) {
            Ok(s) => s.into_bytes(),
            Err(_) => {
                counts.unreadable += 1;
                continue;
            }
        };
        if body.len() as u64 > max_file_bytes {
            counts.binary_skipped += 1;
            continue;
        }
        if body[..body.len().min(8000)].contains(&0u8) {
            counts.binary_skipped += 1; // 二进制（NUL 探测）
            continue;
        }
        let findings = scan::scan(&body);
        if scan::has_high_risk(&findings) {
            counts.secret_skipped += 1; // 安全边界：单文件排除，无例外
            continue;
        }
        let text = match std::str::from_utf8(&body) {
            Ok(t) => t.to_string(),
            Err(_) => {
                counts.binary_skipped += 1; // 非 UTF-8（含 skipped-non-utf8 语义）
                continue;
            }
        };
        for chunk in crate::chunk_text(&text, 2000) {
            let info = objects::put(store, chunk.as_bytes(), objects::PutOptions::default())?;
            let chunk_id = ids::new_id("kc");
            root_hasher.update(chunk.as_bytes());
            chunks.push((next_ordinal, info.sha256.clone(), (chunk.len() / 4) as i64));
            chunk_rows.push((chunk_id, info.sha256, format!("{}\n{}", path, chunk)));
            next_ordinal += 1;
        }
        counts.indexed += 1;
    }

    let content_sha = ids::hex(&root_hasher.finalize());
    let (state, reason) = terminal_state(&counts);
    if state != "indexed" && state != "partial_indexed" {
        let diag = format!(
            "{}: {}: {}",
            state,
            reason.unwrap_or(""),
            serde_json::to_string(&counts.to_json()).unwrap_or_default()
        );
        crate::set_scan_state(store, source_id, state, &diag)?;
        return Err(Error::Message(format!("scan_terminal:{state}")));
    }

    store.with_tx(|tx| {
        tx.execute("DELETE FROM knowledge_fts WHERE source_id=?1", [source_id])?;
        tx.execute("DELETE FROM knowledge_chunks WHERE source_id=?1", [source_id])?;
        for (idx, (ordinal, object_sha, tokens)) in chunks.iter().enumerate() {
            let (chunk_id, _, body) = &chunk_rows[idx];
            tx.execute(
                "INSERT INTO knowledge_chunks(id, source_id, ordinal, object_sha256, token_count, project_id) VALUES (?1,?2,?3,?4,?5,?6)",
                rusqlite::params![chunk_id, source_id, ordinal, object_sha, tokens, source.project_id],
            )?;
            tx.execute(
                "INSERT INTO knowledge_fts(chunk_id, source_id, project_id, body) VALUES (?1,?2,?3,?4)",
                rusqlite::params![chunk_id, source_id, source.project_id, body],
            )?;
        }
        tx.execute(
            "UPDATE knowledge_sources SET scan_state=?2, content_sha256=?3, last_scanned_at=?4,
             error=?5, updated_at=?4 WHERE id=?1",
            rusqlite::params![
                source_id,
                state,
                content_sha,
                timefmt::now(),
                reason.unwrap_or(""),
            ],
        )?;
        Ok(())
    })?;

    Ok((crate::get_source(store, source_id)?, counts))
}

/// LFS pointer 判定（committed 枚举阶段的说明性实现：内容前缀匹配）。
pub fn is_lfs_pointer(body: &[u8]) -> bool {
    body.starts_with(b"version https://git-lfs")
}
