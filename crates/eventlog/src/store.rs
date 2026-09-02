//! 文件事件仓 + 对象域（ADR-031 C1/C2）：
//! - 事件：`.sixgates/events/<workitemId>/<ULID>.json`，一事件一文件，
//!   临时文件 + rename 原子落盘；eventId 幂等（同内容重试安全，异内容报冲突）；
//! - 对象：`.sixgates/objects/<sha256 前 2 位>/<sha256>`，content-addressed、只增不改；
//! - 装载即校验：文件名=eventId，交由 [`crate::dag::DagView`] 做 fail-closed 完整性检查。
//!
//! 提交原子性（C1 r3：事件与对象引用须同 commit 可达）由调用方的 git 流程保证；
//! 本模块提供 [`EventStore::objects_present`] 供发布前校验对象齐备。

use std::path::PathBuf;

use crate::{Envelope, Error};

pub struct EventStore {
    root: PathBuf,
}

impl EventStore {
    /// `root` 为仓库内工件域根（通常 `<repo>/.sixgates`）。
    pub fn open(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn events_dir(&self, workitem_id: &str) -> PathBuf {
        self.root.join("events").join(workitem_id)
    }

    pub fn objects_dir(&self) -> PathBuf {
        self.root.join("objects")
    }

    /// 原子写入一个事件文件。eventId 已存在时：内容一致 → 幂等成功；不一致 → 冲突。
    pub fn write_event(&self, env: &Envelope) -> Result<(), Error> {
        let dir = self.events_dir(&env.workitem_id);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", env.event_id));
        let body = serde_json::to_vec_pretty(env)?;
        if path.exists() {
            let existing = std::fs::read(&path)?;
            if existing == body {
                return Ok(()); // eventId 幂等重试（C3）
            }
            return Err(Error::EventConflict(env.event_id.clone()));
        }
        let tmp = dir.join(format!(".tmp-{}-{}.json", env.event_id, std::process::id()));
        std::fs::write(&tmp, &body)?;
        std::fs::rename(&tmp, &path)?; // 同目录 rename：崩溃不留半文件
        Ok(())
    }

    /// 装载一个工作项的全部事件（按文件名词干 = eventId 交付校验）。
    pub fn load_workitem(&self, workitem_id: &str) -> Result<Vec<(String, Envelope)>, Error> {
        let dir = self.events_dir(workitem_id);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut loaded = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(".tmp-") {
                continue; // 崩溃残留的临时文件不是事件
            }
            let stem = name
                .strip_suffix(".json")
                .ok_or_else(|| Error::BadEventId(name.clone()))?
                .to_string();
            let body = std::fs::read(entry.path())?;
            let env: Envelope = serde_json::from_slice(&body)?;
            loaded.push((stem, env));
        }
        Ok(loaded)
    }

    /// 写入 content-addressed 对象，返回 sha256。已存在时校验后幂等返回。
    pub fn put_object(&self, bytes: &[u8]) -> Result<String, Error> {
        let sha = crate::sha256_hex(bytes);
        let path = self.object_path(&sha);
        if path.exists() {
            let existing = std::fs::read(&path)?;
            if existing == bytes {
                return Ok(sha);
            }
            return Err(Error::ObjectCorrupt(sha));
        }
        std::fs::create_dir_all(path.parent().expect("对象路径恒有父目录"))?;
        let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(sha)
    }

    pub fn get_object(&self, sha: &str) -> Result<Vec<u8>, Error> {
        let path = self.object_path(sha);
        if !path.exists() {
            return Err(Error::ObjectMissing(sha.to_string()));
        }
        let bytes = std::fs::read(&path)?;
        if crate::sha256_hex(&bytes) != sha {
            return Err(Error::ObjectCorrupt(sha.to_string()));
        }
        Ok(bytes)
    }

    /// 发布前校验：引用对象是否齐备（C1 r3 提交原子性的仓库侧检查）。
    pub fn objects_present(&self, refs: &[String]) -> Result<bool, Error> {
        Ok(refs.iter().all(|sha| self.object_path(sha).exists()))
    }

    fn object_path(&self, sha: &str) -> PathBuf {
        let (prefix, _) = sha.split_at(2.min(sha.len()));
        self.objects_dir().join(prefix).join(sha)
    }
}

/// 便捷装配：装载并构建已校验 DAG 视图。
pub fn load_dag(store: &EventStore, workitem_id: &str) -> Result<crate::dag::DagView, Error> {
    crate::dag::DagView::build(store.load_workitem(workitem_id)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Payload, ROOT_PARENT};

    fn tmp_store() -> (tempdir::TempDirGuard, EventStore) {
        let dir = tempdir::TempDirGuard::new();
        let store = EventStore::open(dir.path.clone());
        (dir, store)
    }

    // 极简 tempdir：测试进程内唯一目录，drop 时清理。
    mod tempdir {
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU64, Ordering};

        static SEQ: AtomicU64 = AtomicU64::new(0);

        pub struct TempDirGuard {
            pub path: PathBuf,
        }

        impl TempDirGuard {
            pub fn new() -> Self {
                let n = SEQ.fetch_add(1, Ordering::SeqCst);
                let path = std::env::temp_dir().join(format!(
                    "sg-eventlog-test-{}-{}",
                    std::process::id(),
                    n
                ));
                std::fs::create_dir_all(&path).unwrap();
                Self { path }
            }
        }

        impl Drop for TempDirGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.path);
            }
        }
    }

    fn sample(num: u128, parent: &str) -> Envelope {
        let mut e = Envelope::new(
            "wi1",
            "at1",
            parent,
            Payload::AttemptStarted { gate: "dev".into() },
            "t",
        );
        e.event_id = crate::encode_ulid(num);
        e
    }

    #[test]
    fn write_load_roundtrip_and_idempotent_retry() {
        let (_g, store) = tmp_store();
        let e = sample(1, ROOT_PARENT);
        store.write_event(&e).unwrap();
        store.write_event(&e).unwrap(); // eventId 幂等重试（C7-2 判据）
        let loaded = store.load_workitem("wi1").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].0, e.event_id);
        assert_eq!(loaded[0].1.event_id, e.event_id);
    }

    #[test]
    fn conflicting_content_same_event_id_errors() {
        let (_g, store) = tmp_store();
        let mut e = sample(1, ROOT_PARENT);
        store.write_event(&e).unwrap();
        e.producer = "tampered".into();
        assert!(matches!(
            store.write_event(&e),
            Err(Error::EventConflict(_))
        ));
    }

    #[test]
    fn crash_leftover_tmp_file_is_not_an_event() {
        let (_g, store) = tmp_store();
        let e = sample(1, ROOT_PARENT);
        store.write_event(&e).unwrap();
        let p = store
            .events_dir("wi1")
            .join(format!(".tmp-{}-999.json", e.event_id));
        std::fs::write(&p, b"garbage").unwrap();
        assert_eq!(store.load_workitem("wi1").unwrap().len(), 1);
    }

    #[test]
    fn object_domain_roundtrip_integrity_and_presence() {
        let (_g, store) = tmp_store();
        let sha = store.put_object(b"manifest-bytes").unwrap();
        assert_eq!(store.get_object(&sha).unwrap(), b"manifest-bytes");
        assert!(store.objects_present(std::slice::from_ref(&sha)).unwrap());
        // 篡改 → 完整性校验 fail-closed（C7-16）
        let path = store.object_path(&sha);
        std::fs::write(&path, b"tampered").unwrap();
        assert!(matches!(
            store.get_object(&sha),
            Err(Error::ObjectCorrupt(_))
        ));
        // 缺失（B 机仅 clone 到一半）→ ObjectMissing（C1 对象域）
        assert!(matches!(
            store.get_object(&crate::sha256_hex(b"nope")),
            Err(Error::ObjectMissing(_))
        ));
    }

    #[test]
    fn load_dag_end_to_end() {
        let (_g, store) = tmp_store();
        let a = sample(1, ROOT_PARENT);
        let b = sample(2, &a.event_id);
        store.write_event(&a).unwrap();
        store.write_event(&b).unwrap();
        let view = load_dag(&store, "wi1").unwrap();
        assert_eq!(view.causal_order().len(), 2);
    }
}
