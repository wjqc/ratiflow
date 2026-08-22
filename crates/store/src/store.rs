use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::Connection;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported_filesystem: {0}")]
    UnsupportedFilesystem(String),
    #[error("{0}")]
    Message(String),
}

/// 本地存储：单写者 SQLite + objects 文件库。
pub struct Store {
    conn: Mutex<Connection>,
    pub data_dir: PathBuf,
    pub version: String,
}

impl Store {
    /// 打开/创建数据目录并应用迁移。拒绝网络文件系统（WAL 不安全）。
    pub fn open(data_dir: &Path, version: &str) -> Result<Self, Error> {
        std::fs::create_dir_all(data_dir.join("objects"))?;
        std::fs::create_dir_all(data_dir.join("logs"))?;
        std::fs::create_dir_all(data_dir.join("docs"))?;
        check_filesystem(data_dir)?;
        write_probe(data_dir)?;

        let db_path = data_dir.join("sixgates.db");
        let conn = Connection::open(&db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // 单写者：连接全程持锁串行化。
        let store = Self { conn: Mutex::new(conn), data_dir: data_dir.to_path_buf(), version: version.to_string() };
        store.migrate()?;
        Ok(store)
    }

    /// 在锁内执行数据库操作（单写者串行）。
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T, Error>) -> Result<T, Error> {
        let conn = self.conn.lock().expect("store connection poisoned");
        f(&conn)
    }

    /// 事务执行。
    pub fn with_tx<T>(&self, f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, Error>) -> Result<T, Error> {
        let conn = self.conn.lock().expect("store connection poisoned");
        let tx = conn.unchecked_transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    fn migrate(&self) -> Result<(), Error> {
        crate::migration::run(self)
    }

    /// 当前 schema 版本。
    pub fn schema_version(&self) -> Result<i64, Error> {
        self.with_conn(|conn| {
            conn.query_row("SELECT COALESCE(MAX(version),0) FROM schema_migrations", [], |r| r.get(0))
                .map_err(Error::from)
        })
    }

    /// 完整性检查（sidecar 重启后运行）。
    pub fn quick_check(&self) -> Result<(), Error> {
        let ok: String = self.with_conn(|conn| {
            conn.query_row("PRAGMA quick_check", [], |r| r.get(0)).map_err(Error::from)
        })?;
        if !ok.eq_ignore_ascii_case("ok") {
            return Err(Error::Message(format!("integrity check: {ok}")));
        }
        Ok(())
    }
}

fn check_filesystem(dir: &Path) -> Result<(), Error> {
    // darwin/linux：解析 mount 信息，拒绝已知网络文件系统。
    #[cfg(target_os = "linux")]
    {
        if let Ok(mounts) = std::fs::read_to_string("/proc/mounts") {
            let abs = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
            let mut best: Option<(usize, String)> = None;
            for line in mounts.lines() {
                let mut parts = line.split_whitespace();
                let (_dev, mnt, fstype) = (parts.next(), parts.next(), parts.next());
                if let (Some(mnt), Some(fs)) = (mnt, fstype) {
                    let mnt_path = Path::new(&mnt.replace("\\040", " "));
                    if abs.starts_with(mnt_path) && (best.is_none() || mnt.len() > best.as_ref().unwrap().0) {
                        best = Some((mnt.len(), fs.to_string()));
                    }
                }
            }
            if let Some((_, fs)) = best {
                if matches!(fs.as_str(), "nfs" | "nfs4" | "cifs" | "smbfs" | "9p" | "sshfs" | "webdav") {
                    return Err(Error::UnsupportedFilesystem(format!("{dir:?} 位于 {fs}")));
                }
            }
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        // macOS：/sbin/mount 输出含 (nfs, smbfs, afpfs) 等类型；解析失败不阻断。
        if let Ok(out) = std::process::Command::new("/sbin/mount").output() {
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            let abs = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
            let mut best: Option<(usize, String)> = None;
            for line in text.lines() {
                if let Some(rest) = line.split_once(" on ") {
                    let device = rest.0;
                    if let Some(open) = rest.1.find(" (") {
                        let mnt = &rest.1[..open];
                        let fstype = rest.1[open + 1..].split(',').next().unwrap_or("").trim_end_matches(')').to_string();
                        let mnt_path = Path::new(mnt);
                        if abs.starts_with(mnt_path) && (best.is_none() || mnt.len() > best.as_ref().unwrap().0) {
                            best = Some((mnt.len(), format!("{device}:{fstype}")));
                        }
                    }
                }
            }
            if let Some((_, info)) = best {
                if ["nfs", "smbfs", "afpfs", "afp", "webdav"].iter().any(|fs| info.contains(fs)) {
                    return Err(Error::UnsupportedFilesystem(format!("{dir:?} 位于网络文件系统 {info}")));
                }
            }
        }
        Ok(())
    }
}

fn write_probe(dir: &Path) -> Result<(), Error> {
    let probe = dir.join(".sixgates-write-probe");
    let tmp = dir.join(".sixgates-write-probe.tmp");
    std::fs::write(&tmp, b"probe")?;
    std::fs::rename(&tmp, &probe)?;
    std::fs::remove_file(&probe)?;
    Ok(())
}
