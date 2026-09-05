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
    ///
    /// Schema 纪元（RFC v1.0 §19.1）：manifest 域 schema（0024+）写入 `sixgates-v2.db`，
    /// 旧版本应用只打开 `sixgates.db` —— 物理隔离，旧包读写不到新数据。
    /// v2 不存在而 legacy 存在时，用 SQLite backup API 做一次性在线拷贝（含 WAL 一致性），
    /// 旧文件保留只读（回滚 = 删除 v2 切回旧路径）。
    pub fn open(data_dir: &Path, version: &str) -> Result<Self, Error> {
        std::fs::create_dir_all(data_dir.join("objects"))?;
        std::fs::create_dir_all(data_dir.join("logs"))?;
        std::fs::create_dir_all(data_dir.join("docs"))?;
        check_filesystem(data_dir)?;
        write_probe(data_dir)?;

        let legacy_path = data_dir.join("sixgates.db");
        let db_path = data_dir.join("sixgates-v2.db");
        if !db_path.exists() && legacy_path.exists() {
            Self::import_legacy_epoch(&legacy_path, &db_path)?;
        }
        let conn = Connection::open(&db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // 单写者：连接全程持锁串行化。
        let store = Self {
            conn: Mutex::new(conn),
            data_dir: data_dir.to_path_buf(),
            version: version.to_string(),
        };
        store.migrate()?;
        Ok(store)
    }

    /// 一次性纪元导入：legacy `sixgates.db` → `sixgates-v2.db`（在线 backup API，含 WAL 一致性）。
    /// 旧文件保留不动；导入失败删除半成品 v2，回滚即切回旧路径（§19.1）。
    fn import_legacy_epoch(legacy: &Path, v2: &Path) -> Result<(), Error> {
        let src = Connection::open_with_flags(legacy, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut dst = Connection::open(v2)?;
        let import = (|| -> Result<(), rusqlite::Error> {
            let bc = rusqlite::backup::Backup::new(&src, &mut dst)?;
            bc.run_to_completion(64, std::time::Duration::from_millis(5), None)?;
            Ok(())
        })();
        if let Err(e) = import {
            let _ = std::fs::remove_file(v2);
            return Err(e.into());
        }
        Ok(())
    }

    /// 在锁内执行数据库操作（单写者串行）。
    ///
    /// 闭包内禁止调用任何会再次进入 with_conn/with_tx 的函数：
    /// Mutex 不可重入，重入将死锁（曾导致 project::find_by_locator 挂死）。
    pub fn with_conn<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let conn = self.conn.lock().expect("store connection poisoned");
        f(&conn)
    }

    /// 事务执行。
    pub fn with_tx<T>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let conn = self.conn.lock().expect("store connection poisoned");
        let tx = conn.unchecked_transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// 立即事务（BEGIN IMMEDIATE）：读阶段就取写锁。
    /// 供"先读判定、再写"且怕多进程竞态的序列使用（如 schema 迁移）。
    pub fn with_tx_immediate<T>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        use rusqlite::TransactionBehavior;
        let mut conn = self.conn.lock().expect("store connection poisoned");
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
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
            conn.query_row(
                "SELECT COALESCE(MAX(version),0) FROM schema_migrations",
                [],
                |r| r.get(0),
            )
            .map_err(Error::from)
        })
    }

    /// 完整性检查（sidecar 重启后运行）。
    pub fn quick_check(&self) -> Result<(), Error> {
        let ok: String = self.with_conn(|conn| {
            conn.query_row("PRAGMA quick_check", [], |r| r.get(0))
                .map_err(Error::from)
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
                    if abs.starts_with(mnt_path)
                        && (best.is_none() || mnt.len() > best.as_ref().unwrap().0)
                    {
                        best = Some((mnt.len(), fs.to_string()));
                    }
                }
            }
            if let Some((_, fs)) = best {
                if matches!(
                    fs.as_str(),
                    "nfs" | "nfs4" | "cifs" | "smbfs" | "9p" | "sshfs" | "webdav"
                ) {
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
                        let fstype = rest.1[open + 1..]
                            .split(',')
                            .next()
                            .unwrap_or("")
                            .trim_end_matches(')')
                            .to_string();
                        let mnt_path = Path::new(mnt);
                        if abs.starts_with(mnt_path)
                            && (best.is_none() || mnt.len() > best.as_ref().unwrap().0)
                        {
                            best = Some((mnt.len(), format!("{device}:{fstype}")));
                        }
                    }
                }
            }
            if let Some((_, info)) = best {
                if ["nfs", "smbfs", "afpfs", "afp", "webdav"]
                    .iter()
                    .any(|fs| info.contains(fs))
                {
                    return Err(Error::UnsupportedFilesystem(format!(
                        "{dir:?} 位于网络文件系统 {info}"
                    )));
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
