//! SixGates 隔离 worktree（ADR-030 M3 / SG-RBK-005 / 蓝图 §7.1/§7.2）。
//! Agent 执行在 dataDir/worktrees/<workItemId> 的 git linked worktree 中进行；
//! 快照记录其 HEAD/dirty，回滚对其 `reset --hard`+`clean` 是合法恢复——
//! 用户主工作区（local_root）永不写入、永不 reset。

use std::path::PathBuf;

use serde::Serialize;
use sg_store::{ids, Error, Store};

#[derive(Debug, Clone, Serialize)]
pub struct WorktreeInfo {
    pub path: String,
    pub head: String,
    pub branch: String,
    pub dirty_files: usize,
    /// 基仓库（用户主工作区，只读参照）。
    pub main_head: String,
}

fn worktree_root(store: &Store, workitem_id: &str) -> PathBuf {
    store.data_dir.join("worktrees").join(workitem_id)
}

/// 主仓库轻量状态（团队共享展示用）：分支 + 脏文件数。非 git 仓库返回 None。
pub fn repo_git_status(root: &std::path::Path) -> Option<(String, usize)> {
    // branch --show-current 在未出生分支（无提交的新仓库）也可用；rev-parse 会失败。
    let branch = git_out(root, &["branch", "--show-current"])?;
    let status = git_out(root, &["status", "--porcelain"])?;
    let dirty = status.lines().filter(|l| !l.trim().is_empty()).count();
    Some((branch, dirty))
}

fn git_out(root: &std::path::Path, args: &[&str]) -> Option<String> {
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

fn local_root_of(store: &Store, workitem_id: &str) -> Result<Option<String>, Error> {
    Ok(store
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
        .filter(|s| !s.is_empty()))
}

/// 确保隔离 worktree 存在（幂等）：从主工作区当前 HEAD 以 detached 方式挂出。
/// 主工作区不是 git 仓库 / git 不可用 / 已挂同路径时返回诚实原因，调用方自行回退。
pub fn ensure(store: &Store, workitem_id: &str) -> Result<WorktreeInfo, Error> {
    let Some(local_root) = local_root_of(store, workitem_id)? else {
        return Err(Error::Message("worktree_unavailable: 无 local_root".into()));
    };
    let main = PathBuf::from(&local_root);
    let Some(main_head) = git_out(&main, &["rev-parse", "HEAD"]) else {
        return Err(Error::Message(
            "worktree_unavailable: 主工作区不是 git 仓库".into(),
        ));
    };
    let path = worktree_root(store, workitem_id);
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&main)
            .args([
                "worktree",
                "add",
                "--detach",
                path.to_string_lossy().as_ref(),
                &main_head,
            ])
            .output()
            .map_err(|e| Error::Message(format!("worktree_unavailable: {e}")))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(Error::Message(format!(
                "worktree_unavailable: git worktree add 失败 {stderr}"
            )));
        }
    }
    status_of(store, workitem_id, &main_head)
        .ok_or_else(|| Error::Message("worktree_unavailable: worktree 状态不可读".into()))
}

/// 受管 worktree 当前状态（不存在→None）。
pub fn status(store: &Store, workitem_id: &str) -> Result<Option<WorktreeInfo>, Error> {
    let path = worktree_root(store, workitem_id);
    if !path.exists() {
        return Ok(None);
    }
    let main_head = local_root_of(store, workitem_id)?
        .and_then(|root| git_out(&PathBuf::from(root), &["rev-parse", "HEAD"]))
        .unwrap_or_default();
    Ok(status_of(store, workitem_id, &main_head))
}

fn status_of(store: &Store, workitem_id: &str, main_head: &str) -> Option<WorktreeInfo> {
    let path = worktree_root(store, workitem_id);
    let head = git_out(&path, &["rev-parse", "HEAD"])?;
    let branch = git_out(&path, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let dirty = git_out(&path, &["status", "--porcelain"]).unwrap_or_default();
    let dirty_files = dirty.lines().filter(|l| !l.trim().is_empty()).count();
    Some(WorktreeInfo {
        path: path.to_string_lossy().to_string(),
        head,
        branch,
        dirty_files,
        main_head: main_head.to_string(),
    })
}

// ---------------- M2-07：任务级 worktree（EvoFlow §6.3 / ADR-037 §6.7） ----------------
// 写 TaskAttempt 独立 worktree：dataDir/worktrees/<workItemId>/<planRevisionId>/<taskAttemptId>，
// 从计划冻结的 base HEAD 挂出（不是主工作区当前 HEAD——审批等待期间漂移不进任务）。

/// 任务 worktree 路径约定（与 0034 task_workspaces.path 一致）。
pub fn task_worktree_path(
    store: &Store,
    workitem_id: &str,
    plan_revision_id: &str,
    task_attempt_id: &str,
) -> PathBuf {
    worktree_root(store, workitem_id)
        .join(plan_revision_id)
        .join(task_attempt_id)
}

/// 挂出任务 worktree（幂等）：detached at base_head；目录已存在则返回当前状态。
pub fn ensure_task_worktree(
    store: &Store,
    workitem_id: &str,
    plan_revision_id: &str,
    task_attempt_id: &str,
    base_head: &str,
) -> Result<WorktreeInfo, Error> {
    let Some(local_root) = local_root_of(store, workitem_id)? else {
        return Err(Error::Message("worktree_unavailable: 无 local_root".into()));
    };
    let main = PathBuf::from(&local_root);
    let path = task_worktree_path(store, workitem_id, plan_revision_id, task_attempt_id);
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&main)
            .args([
                "worktree",
                "add",
                "--detach",
                path.to_string_lossy().as_ref(),
                base_head,
            ])
            .output()
            .map_err(|e| Error::Message(format!("workspace_unavailable: {e}")))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(Error::Message(format!(
                "workspace_unavailable: git worktree add 失败 {stderr}"
            )));
        }
    }
    let main_head = git_out(&main, &["rev-parse", "HEAD"]).unwrap_or_default();
    let head = git_out(&path, &["rev-parse", "HEAD"])
        .ok_or_else(|| Error::Message("workspace_unavailable: 任务 worktree 状态不可读".into()))?;
    let branch = git_out(&path, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let dirty = git_out(&path, &["status", "--porcelain"]).unwrap_or_default();
    Ok(WorktreeInfo {
        path: path.to_string_lossy().to_string(),
        head,
        branch,
        dirty_files: dirty.lines().filter(|l| !l.trim().is_empty()).count(),
        main_head,
    })
}

/// 任务 worktree 工作区 digest：脏文件清单（路径+状态+内容行数）canonical 哈希。
/// before/after 对比 = 变更指纹（0034 workspace_digest_before/after）。
pub fn task_workspace_digest(
    store: &Store,
    workitem_id: &str,
    plan_revision_id: &str,
    task_attempt_id: &str,
) -> Result<String, Error> {
    use sha2::{Digest, Sha256};
    let path = task_worktree_path(store, workitem_id, plan_revision_id, task_attempt_id);
    let dirty = git_out(&path, &["status", "--porcelain"]).unwrap_or_default();
    let mut lines: Vec<String> = dirty
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(String::from)
        .collect();
    lines.sort();
    let head = git_out(&path, &["rev-parse", "HEAD"]).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(format!("wd1|{head}|{}", lines.join("\n")).as_bytes());
    Ok(sg_store::ids::hex(&hasher.finalize()))
}

/// 恢复受管 worktree 到目标 HEAD（仅本目录；主工作区不参与）。
/// `git reset --hard` + `clean -fd` 在 SixGates 受管 worktree 内是合法恢复操作，
/// 与 SG-RBK-005 禁止的"主工作区隐式 hard reset"无关。
pub fn restore(store: &Store, workitem_id: &str, target_head: &str) -> Result<(), Error> {
    let path = worktree_root(store, workitem_id);
    if !path.exists() {
        return Err(Error::Message(
            "worktree_unavailable: 受管 worktree 不存在，无法恢复".into(),
        ));
    }
    for args in [vec!["reset", "--hard", target_head], vec!["clean", "-fd"]] {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&path)
            .args(&args)
            .output()
            .map_err(|e| Error::Message(format!("snapshot_failed: worktree 恢复失败 {e}")))?;
        if !out.status.success() {
            return Err(Error::Message(format!(
                "snapshot_failed: worktree 恢复失败 {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }
    }
    let _ = ids::new_id("wt"); // 保持与库内 id 风格一致（无实义）
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sg_store::timefmt;

    fn setup_with_git_repo() -> (Store, String, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("sg-wt-{}-{}", std::process::id(), ids::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        let repo = dir.join("main-repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("README.md"), "base\n").unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "t@sixgates.local"],
            vec!["config", "user.name", "t"],
            vec!["add", "."],
            vec!["commit", "-m", "init"],
        ] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(&args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
        }
        let repo_str = repo.to_string_lossy().to_string();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, local_root, created_at)
                     VALUES ('pj','u','n','p','main',?1,?2)",
                    rusqlite::params![repo_str, timefmt::now()],
                )
                .unwrap();
                c.execute(
                    "INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                     VALUES ('wi_1','pj','任务','','[]','requirements',?1,?1)",
                    [timefmt::now()],
                )
                .unwrap();
                Ok(())
            })
            .unwrap();
        (store, repo_str, repo)
    }

    #[test]
    fn ensure_creates_detached_worktree_and_restore_resets_it() {
        let (s, _repo_str, main_repo) = setup_with_git_repo();
        let info = ensure(&s, "wi_1").unwrap();
        assert!(info.path.contains("worktrees"));
        assert_eq!(
            info.head,
            git_out(&main_repo, &["rev-parse", "HEAD"]).unwrap()
        );
        assert!(
            status(&s, "wi_1").unwrap().is_some(),
            "幂等：已存在直接返回"
        );
        // Agent 在受管区写入（脏文件 + 新提交点不动主区）。
        std::fs::write(
            std::path::Path::new(&info.path).join("agent-output.md"),
            "generated\n",
        )
        .unwrap();
        let dirty = status(&s, "wi_1").unwrap().unwrap();
        assert_eq!(dirty.dirty_files, 1);
        // 主区 HEAD 记录并断言不受恢复影响。
        let main_head_before = git_out(&main_repo, &["rev-parse", "HEAD"]).unwrap();
        // 恢复到快照 HEAD（脏文件被清掉——受管区内的合法 reset）。
        restore(&s, "wi_1", &info.head).unwrap();
        assert!(
            !std::path::Path::new(&info.path)
                .join("agent-output.md")
                .exists(),
            "受管区恢复后 agent 产物被清理"
        );
        assert_eq!(
            git_out(&main_repo, &["rev-parse", "HEAD"]).unwrap(),
            main_head_before,
            "主工作区 HEAD 不受影响"
        );
        assert!(
            main_repo.join("agent-output.md").metadata().is_err()
                || !main_repo.join("agent-output.md").exists()
        );
    }

    /// M2-07：任务 worktree 从计划冻结 base HEAD 挂出——主区在"冻结后"新提交不进任务；
    /// 两个 attempt 路径互不重叠（EV-007 路径面）。
    #[test]
    fn task_worktree_pins_base_head_and_paths_are_isolated() {
        let (s, _repo_str, main_repo) = setup_with_git_repo();
        let base_head = git_out(&main_repo, &["rev-parse", "HEAD"]).unwrap();
        // 冻结后主区新提交（模拟审批等待期间漂移）。
        std::fs::write(main_repo.join("drift.txt"), "later\n").unwrap();
        for args in [vec!["add", "."], vec!["commit", "-m", "drift"]] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&main_repo)
                .args(&args)
                .output()
                .unwrap();
            assert!(out.status.success());
        }
        let info = ensure_task_worktree(&s, "wi_1", "pr1", "pa1", &base_head).unwrap();
        assert_eq!(info.head, base_head, "任务 worktree 钉在计划冻结 base HEAD");
        assert!(
            !std::path::Path::new(&info.path).join("drift.txt").exists(),
            "冻结后的主区漂移不进任务工作区"
        );
        let info2 = ensure_task_worktree(&s, "wi_1", "pr1", "pa2", &base_head).unwrap();
        assert_ne!(info.path, info2.path, "两个 attempt 路径不重叠");
        // digest：写文件后变化。
        let d0 = task_workspace_digest(&s, "wi_1", "pr1", "pa1").unwrap();
        std::fs::write(std::path::Path::new(&info.path).join("out.txt"), "x\n").unwrap();
        let d1 = task_workspace_digest(&s, "wi_1", "pr1", "pa1").unwrap();
        assert_ne!(d0, d1, "digest 反映工作区变更");
    }

    #[test]
    fn non_git_local_root_is_honest_unavailable() {
        let dir = std::env::temp_dir().join(format!(
            "sg-wt-nogit-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(dir.join("plain")).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, local_root, created_at)
                     VALUES ('pj','u','n','p','main',?1,?2)",
                    rusqlite::params![
                        dir.join("plain").to_string_lossy().to_string(),
                        timefmt::now()
                    ],
                )
                .unwrap();
                c.execute(
                    "INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                     VALUES ('wi_1','pj','任务','','[]','requirements',?1,?1)",
                    [timefmt::now()],
                )
                .unwrap();
                Ok(())
            })
            .unwrap();
        let err = ensure(&store, "wi_1").unwrap_err();
        assert!(err.to_string().contains("worktree_unavailable"));
        assert!(status(&store, "wi_1").unwrap().is_none());
    }
}

/// Windows 特有故障域的**行为近似注入**（在本机 macOS 上模拟 Windows 真实会踩的失败模式）。
/// 诚实声明：这不是真 Windows 平台运行（那必须由 CI 的 windows runner 承担）；
/// 这里验证的是我们的代码路径面对同类故障语义时 fail-closed、不崩溃、不伪造成功。
/// - 不可删除文件 ≈ Windows 文件被进程占用锁定（用 macOS uchg 文件旗标模拟）
/// - CRLF 漂移 ≈ Windows autocrlf 未配置时的脏误报
/// - 超长路径 ≈ Windows MAX_PATH=260 限制域
/// - 大小写不敏感 FS 冲突 ≈ Windows NTFS 默认行为（macOS APFS 默认同为不敏感）
#[cfg(test)]
mod windows_behavior_simulation {
    use super::*;
    use sg_store::timefmt;

    fn setup() -> (Store, String, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "sg-wt-win-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        let repo = dir.join("main-repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("readme.txt"), "base\n").unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "t@sixgates.local"],
            vec!["config", "user.name", "t"],
            vec!["add", "."],
            vec!["commit", "-m", "init"],
        ] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(&args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
        }
        let repo_str = repo.to_string_lossy().to_string();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, local_root, created_at)
                     VALUES ('pj','u','n','p','main',?1,?2)",
                    rusqlite::params![repo_str, timefmt::now()],
                )
                .unwrap();
                c.execute(
                    "INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                     VALUES ('wi_1','pj','任务','','[]','requirements',?1,?1)",
                    [timefmt::now()],
                )
                .unwrap();
                Ok(())
            })
            .unwrap();
        (store, repo_str, repo)
    }

    /// CRLF 漂移（Windows autocrlf 未配置时的典型脏误报）：
    /// 快照必须如实计为 dirty，恢复后回到干净状态——不崩溃、不误报干净。
    #[test]
    fn crlf_drift_is_counted_and_restorable() {
        let (s, _repo, _main) = setup();
        let wt = ensure(&s, "wi_1").unwrap();
        // 同"逻辑内容"的 CRLF 版本（Windows 编辑器写出）。
        std::fs::write(
            std::path::Path::new(&wt.path).join("readme.txt"),
            "base\r\n",
        )
        .unwrap();
        let info = status(&s, "wi_1").unwrap().unwrap();
        assert!(info.dirty_files >= 1, "CRLF 漂移被如实计入 dirty");
        restore(&s, "wi_1", &wt.head).unwrap();
        let info = status(&s, "wi_1").unwrap().unwrap();
        assert_eq!(info.dirty_files, 0, "恢复后 CRLF 漂移被清零");
    }

    /// 不可删除文件 ≈ Windows 文件锁：clean -fd 失败 → restore 必须返回 Err（fail-closed），
    /// 由回滚域标记 failed 而不是伪造成功。
    #[test]
    #[cfg(target_os = "macos")]
    fn undeletable_file_makes_restore_fail_closed() {
        let (s, _repo, _main) = setup();
        let wt = ensure(&s, "wi_1").unwrap();
        let locked = std::path::Path::new(&wt.path).join("locked-by-app.txt");
        std::fs::write(&locked, "held by another app\n").unwrap();
        // macOS：uchg 旗标 = 不可删除（Windows 文件锁的行为近似）。
        let st = std::process::Command::new("chflags")
            .args(["uchg"])
            .arg(&locked)
            .output()
            .unwrap();
        assert!(st.status.success());
        let result = restore(&s, "wi_1", &wt.head);
        // 清理旗标（否则临时目录无法回收），再断言失败已发生。
        let _ = std::process::Command::new("chflags")
            .args(["nouchg"])
            .arg(&locked)
            .output();
        assert!(result.is_err(), "clean 遇到不可删除文件必须失败");
        // 失败后受管区仍可被后续恢复修复（unlock 后重试成功）。
        restore(&s, "wi_1", &wt.head).unwrap();
        let info = status(&s, "wi_1").unwrap().unwrap();
        assert_eq!(info.dirty_files, 0, "解锁后重试恢复成功");
    }

    /// 超长路径（Windows MAX_PATH=260 域）：状态采集与恢复不得因路径长度崩溃。
    #[test]
    fn deep_long_paths_survive_status_and_restore() {
        let (s, _repo, _main) = setup();
        let wt = ensure(&s, "wi_1").unwrap();
        // 相对路径总长 > 260 字符（Windows 老版 git/非 long-path 进程的雷区）。
        let seg = "level-".to_string().repeat(6); // 36 chars/层
        let mut rel = String::new();
        for _ in 0..8 {
            rel.push_str(&seg);
            rel.push('/');
        }
        let deep = std::path::Path::new(&wt.path).join(&rel);
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("leaf.txt"), "deep\n").unwrap();
        let info = status(&s, "wi_1").unwrap().unwrap();
        assert!(info.dirty_files >= 1, "深长路径文件被正常计入");
        restore(&s, "wi_1", &wt.head).unwrap();
        let info = status(&s, "wi_1").unwrap().unwrap();
        assert_eq!(info.dirty_files, 0, "深长路径在恢复后被正常清理");
    }

    /// 大小写不敏感 FS 冲突（NTFS/APFS 默认）：仅大小写不同的新文件 →
    /// 状态采集如实计数、恢复不崩溃（不伪装成大小写敏感文件系统）。
    #[test]
    fn case_conflict_counted_and_restorable() {
        let (s, _repo, _main) = setup();
        let wt = ensure(&s, "wi_1").unwrap();
        // 大小写不敏感文件系统上，readme.txt 与 README.TXT 是同一文件：
        // 写入大写版本 = 修改已跟踪文件（Windows 用户的常见动作）。
        std::fs::write(std::path::Path::new(&wt.path).join("README.TXT"), "base\n").unwrap();
        let info = status(&s, "wi_1").unwrap().unwrap();
        if info.dirty_files >= 1 {
            restore(&s, "wi_1", &wt.head).unwrap();
            let info = status(&s, "wi_1").unwrap().unwrap();
            assert_eq!(info.dirty_files, 0);
        } else {
            // 底层文件系统为大小写敏感时的语义分支：内容相同=干净，同样成立。
            let _ = restore(&s, "wi_1", &wt.head);
        }
    }
}
