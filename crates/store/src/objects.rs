use std::io::Read;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::{ids, timefmt, Error, Store};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Finding {
    pub kind: String,
    pub count: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ObjectInfo {
    pub sha256: String,
    pub size: i64,
    pub content_type: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy)]
pub struct PutOptions {
    pub max_bytes: i64,
    /// 显式接受秘密风险（必须审计）。
    pub allow_secrets: bool,
}

impl Default for PutOptions {
    fn default() -> Self {
        Self {
            max_bytes: 32 << 20,
            allow_secrets: false,
        }
    }
}

/// 内容寻址写入：临时文件 → SHA-256 → 扫描 → 事务登记 → 原子移动。
/// 同哈希重复写入幂等；高风险秘密默认拒绝入库。
pub fn put(store: &Store, mut reader: impl Read, opts: PutOptions) -> Result<ObjectInfo, Error> {
    let tmp_dir = store.data_dir.join("objects").join("tmp");
    std::fs::create_dir_all(&tmp_dir)?;
    let tmp_path = tmp_dir.join(format!("ingest-{}", ids::new_id("t")));
    let mut hasher = Sha256::new();
    let mut file = std::fs::File::create(&tmp_path)?;
    let mut written: i64 = 0;
    let mut limited = (&mut reader).take(opts.max_bytes as u64 + 1);
    loop {
        let mut buf = [0u8; 64 << 10];
        let n = limited.read(&mut buf)?;
        if n == 0 {
            break;
        }
        written += n as i64;
        if written > opts.max_bytes {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(Error::Message(format!(
                "object exceeds max size {} bytes",
                opts.max_bytes
            )));
        }
        std::io::Write::write_all(&mut file, &buf[..n])?;
        hasher.update(&buf[..n]);
    }
    drop(file);
    let sum = ids::hex(&hasher.finalize());

    let body = std::fs::read(&tmp_path)?;
    let findings = crate::scan::scan(&body);
    let has_high = crate::scan::has_high_risk(&findings);
    if has_high && !opts.allow_secrets {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(Error::Message("object_contains_secrets".into()));
    }

    let content_type = sniff_content_type(&body);
    let now = timefmt::now();
    // 先落对象文件再登记 DB 行（缺陷审计）：反向顺序会留下"有行无文件"的
    // object_not_found 态；先文件后行的崩溃残骸只是待 GC 的孤儿文件（无害）。
    let final_path = object_path(store, &sum);
    if !final_path.exists() {
        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&tmp_path, &final_path)?;
    } else {
        let _ = std::fs::remove_file(&tmp_path);
    }
    let created = store.with_tx(|tx| {
        tx.execute(
            "INSERT INTO objects(sha256, size, content_type, secret_findings, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(sha256) DO NOTHING",
            rusqlite::params![
                sum,
                written,
                content_type,
                serde_json::to_string(&findings).unwrap_or_else(|_| "[]".into()),
                now
            ],
        )?;
        let created: String = tx.query_row(
            "SELECT created_at FROM objects WHERE sha256 = ?1",
            [&sum],
            |r| r.get(0),
        )?;
        Ok(created)
    })?;

    Ok(ObjectInfo {
        sha256: sum,
        size: written,
        content_type,
        created_at: created,
    })
}

/// 打开对象内容。
pub fn open(store: &Store, sum: &str) -> Result<Vec<u8>, Error> {
    valid_sha(sum)?;
    let path = object_path(store, sum);
    std::fs::read(&path).map_err(|_| Error::Message("object_not_found".into()))
}

/// 对象元数据。
pub fn meta(store: &Store, sum: &str) -> Result<ObjectInfo, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT sha256, size, content_type, created_at FROM objects WHERE sha256 = ?1",
            [sum],
            |r| {
                Ok(ObjectInfo {
                    sha256: r.get(0)?,
                    size: r.get(1)?,
                    content_type: r.get(2)?,
                    created_at: r.get(3)?,
                })
            },
        )
        .map_err(|_| Error::Message("object_not_found".into()))
    })
}

pub fn object_path(store: &Store, sum: &str) -> PathBuf {
    store.data_dir.join("objects").join(&sum[..2]).join(sum)
}

fn valid_sha(sum: &str) -> Result<(), Error> {
    if sum.len() != 64
        || !sum
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Message("object_not_found".into()));
    }
    Ok(())
}

fn sniff_content_type(body: &[u8]) -> String {
    let head = &body[..body.len().min(512)];
    if head.starts_with(&[0x89, b'P', b'N', b'G']) {
        return "image/png".into();
    }
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return "image/jpeg".into();
    }
    if head.starts_with(b"GIF8") {
        return "image/gif".into();
    }
    if head.starts_with(b"RIFF") && head.len() > 11 && &head[8..12] == b"WEBP" {
        return "image/webp".into();
    }
    if head.starts_with(b"%PDF") {
        return "application/pdf".into();
    }
    if head.contains(&0) {
        return "application/octet-stream".into();
    }
    "text/plain; charset=utf-8".into()
}
