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
    /// Schema 纪元（EvoFlow 方案 §7.1 / M0-05）：本包只打开 `ratiflow-v3.db`。
    /// 纪元链：legacy `ratiflow.db` → `ratiflow-v2.db` → `ratiflow-v3.db`，均为 SQLite
    /// backup API 一次性在线拷贝（含 WAL 一致性）；源文件保留不动 —— 旧包只打开旧纪元
    /// 文件（物理隔离），回滚 = 换回旧包。v3 导入或迁移失败时删除半成品 v3、保留 v2，
    /// 应用拒绝伪启动；下次打开重新导入。存量 v3 迁移失败则保留现场（预迁移备份在 backups/）。
    /// 不做 v3 → v2 反向覆盖（§7.1 第 6 条）。
    pub fn open(data_dir: &Path, version: &str) -> Result<Self, Error> {
        std::fs::create_dir_all(data_dir.join("objects"))?;
        std::fs::create_dir_all(data_dir.join("logs"))?;
        std::fs::create_dir_all(data_dir.join("docs"))?;
        check_filesystem(data_dir)?;
        write_probe(data_dir)?;

        let legacy_path = data_dir.join("ratiflow.db");
        let v2_path = data_dir.join("ratiflow-v2.db");
        let db_path = data_dir.join("ratiflow-v3.db");
        // v1→v2：保留既有链路，让 v2 始终是旧包可用的完整回退点。
        if !v2_path.exists() && legacy_path.exists() {
            Self::import_epoch(&legacy_path, &v2_path)?;
        }
        // v2→v3：仅当 v3 不存在且 v2 存在时一次性拷贝（§7.1 第 1 条）。
        let fresh_v3 = !db_path.exists();
        if fresh_v3 && v2_path.exists() {
            Self::import_epoch(&v2_path, &db_path)?;
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
        match store.migrate() {
            Ok(()) => Ok(store),
            Err(e) => {
                // 半成品 v3（本次新建）：删除连同 WAL/SHM，v2 保留，下次重新导入（EV-023）。
                // 存量 v3 不删：带数据现场交给调用方与预迁移备份处置。
                if fresh_v3 {
                    let _ = std::fs::remove_file(&db_path);
                    let _ = std::fs::remove_file(data_dir.join("ratiflow-v3.db-wal"));
                    let _ = std::fs::remove_file(data_dir.join("ratiflow-v3.db-shm"));
                }
                Err(e)
            }
        }
    }

    /// 一次性纪元导入（在线 backup API，含 WAL 一致性）。源文件保留不动；
    /// 导入失败删除半成品目标文件（§19.1 / §7.1 第 4 条）。
    fn import_epoch(src: &Path, dst: &Path) -> Result<(), Error> {
        let source = Connection::open_with_flags(src, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut target = Connection::open(dst)?;
        let import = (|| -> Result<(), rusqlite::Error> {
            let bc = rusqlite::backup::Backup::new(&source, &mut target)?;
            bc.run_to_completion(64, std::time::Duration::from_millis(5), None)?;
            Ok(())
        })();
        if let Err(e) = import {
            let _ = std::fs::remove_file(dst);
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
    let probe = dir.join(".ratiflow-write-probe");
    let tmp = dir.join(".ratiflow-write-probe.tmp");
    std::fs::write(&tmp, b"probe")?;
    std::fs::rename(&tmp, &probe)?;
    std::fs::remove_file(&probe)?;
    Ok(())
}
