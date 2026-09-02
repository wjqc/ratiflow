//! Git 发布状态机（ADR-031 C3）：draft → committed → published（→ revoked）。
//!
//! 判据只认**远端响应**（`git ls-remote` 的精确 ref SHA + 该 commit 树含事件文件）；
//! 本机 remote-tracking ref 是缓存，不作判据（C3 r3）。
//! 任何 git/网络失败 → `State::Unknown`（publication_unknown，fail-closed），
//! 不冒充验证结果（C3 r3）。force-push 使已发布事件不可达 → `Revoked`，
//! 不与 stale 混用（C3 r2）；撤销判定需要 prior 见证佐证"曾经发布过"。

use std::path::Path;

use crate::reducer::PublicationWitness;
use crate::store::EventStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// 事件文件尚未提交。
    Draft,
    /// 本地已提交、未发布（或远端 ref 尚无该链）。
    Committed,
    /// 远端精确可达：remote ref SHA 为祖先且其树含该事件文件。
    Published,
    /// 曾经验证发布过，现在远端不再包含（force-push / ref 改写）→ 阻断放行。
    Revoked,
    /// 断网 / 远端不可达 / git 失败：fail-closed（C3 r3）。
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EventPublishStatus {
    pub state: State,
    pub witness: Option<PublicationWitness>,
}

fn git_out(root: &Path, args: &[&str]) -> Option<String> {
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

/// 事件文件最近一次被提交的 commit SHA；未提交 → None（Draft）。
pub fn last_event_commit(repo_root: &Path, rel_path: &str) -> Option<String> {
    git_out(
        repo_root,
        &["log", "-n", "1", "--format=%H", "--", rel_path],
    )
    .filter(|s| !s.is_empty())
}

/// 远端 ref 的精确 SHA（`ls-remote` 实时响应）。远端不可达 → Err（映射 Unknown）。
pub fn remote_ref_sha(
    repo_root: &Path,
    remote: &str,
    ref_name: &str,
) -> Result<Option<String>, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-remote", remote, ref_name])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(stdout
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next())
        .filter(|s| !s.is_empty())
        .map(str::to_string))
}

fn is_ancestor(repo_root: &Path, ancestor: &str, descendant: &str) -> Option<bool> {
    // 退出码 0 = 祖先；1 = 非祖先；其他 = git 失败（None → 上层判 Unknown）。
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .status()
        .ok()?;
    let code = status.code()?;
    if code == 0 {
        Some(true)
    } else if code == 1 {
        Some(false)
    } else {
        None
    }
}

fn tree_contains(repo_root: &Path, commit: &str, rel_path: &str) -> Option<bool> {
    Some(
        git_out(
            repo_root,
            &["cat-file", "-e", &format!("{commit}:{rel_path}")],
        )
        .is_some(),
    )
}

/// 单事件发布分类（C3）。`prior` 为本机留存的既往见证——Revoked 判定的依据。
pub fn classify(
    repo_root: &Path,
    rel_path: &str,
    event_id: &str,
    remote: &str,
    ref_name: &str,
    prior: &[PublicationWitness],
) -> EventPublishStatus {
    let Some(event_commit) = last_event_commit(repo_root, rel_path) else {
        return EventPublishStatus {
            state: State::Draft,
            witness: None,
        };
    };
    let remote_sha = match remote_ref_sha(repo_root, remote, ref_name) {
        Ok(sha) => sha,
        Err(reason) => {
            return EventPublishStatus {
                state: State::Unknown,
                witness: Some(PublicationWitness::Revoked {
                    event_id: event_id.to_string(),
                    reason: format!("verify_failed: {reason}"),
                }),
            }
        }
    };
    let Some(remote_sha) = remote_sha else {
        // 远端无此 ref：从未发布，除非 prior 证明发布过（ref 被删除 = 撤销）。
        return if had_verified(prior, event_id) {
            EventPublishStatus {
                state: State::Revoked,
                witness: Some(PublicationWitness::Revoked {
                    event_id: event_id.to_string(),
                    reason: "ref_deleted".into(),
                }),
            }
        } else {
            EventPublishStatus {
                state: State::Committed,
                witness: None,
            }
        };
    };
    let reachable = match (
        is_ancestor(repo_root, &event_commit, &remote_sha),
        tree_contains(repo_root, &remote_sha, rel_path),
    ) {
        (Some(true), Some(true)) => true,
        (Some(false), _) | (_, Some(false)) => false,
        _ => {
            // git 判定失败 → 与断网同等对待（C3 r3 fail-closed）。
            return EventPublishStatus {
                state: State::Unknown,
                witness: None,
            };
        }
    };
    if reachable {
        EventPublishStatus {
            state: State::Published,
            witness: Some(PublicationWitness::Verified {
                event_id: event_id.to_string(),
                ref_name: ref_name.to_string(),
                commit_sha: remote_sha,
            }),
        }
    } else if had_verified(prior, event_id) {
        // 曾经发布、现在不可达 → ref 被改写（force-push）→ Revoked（C3 r2，不记 stale）。
        EventPublishStatus {
            state: State::Revoked,
            witness: Some(PublicationWitness::Revoked {
                event_id: event_id.to_string(),
                reason: "ref_rewritten".into(),
            }),
        }
    } else {
        EventPublishStatus {
            state: State::Committed,
            witness: None,
        }
    }
}

fn had_verified(prior: &[PublicationWitness], event_id: &str) -> bool {
    prior
        .iter()
        .any(|w| matches!(w, PublicationWitness::Verified { event_id: e, .. } if e == event_id))
}

/// 对账（reconciliation，C3）：为工作项全部事件生成当前发布见证集。
/// 供 reducer 发布层消费；`Store::load_workitem` 失败向上传播。
pub fn reconcile(
    store: &EventStore,
    repo_root: &Path,
    workitem_id: &str,
    remote: &str,
    ref_name: &str,
    prior: &[PublicationWitness],
) -> Result<Vec<PublicationWitness>, crate::Error> {
    let events = store.load_workitem(workitem_id)?;
    let mut witnesses = Vec::new();
    for (stem, _env) in events {
        let rel_path = format!(".sixgates/events/{workitem_id}/{stem}.json");
        let status = classify(repo_root, &rel_path, &stem, remote, ref_name, prior);
        if let Some(w) = status.witness {
            witnesses.push(w);
        }
    }
    Ok(witnesses)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Repo {
        path: std::path::PathBuf,
    }

    impl Repo {
        fn new(bare_origin: bool) -> Self {
            let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let path = std::env::temp_dir().join(format!("sg-pub-{}-{}", std::process::id(), n));
            std::fs::create_dir_all(&path).unwrap();
            run(&path, &["init", "-b", "main"]);
            run(&path, &["config", "user.email", "t@x"]);
            run(&path, &["config", "user.name", "t"]);
            let mut origin_path = path.clone();
            if bare_origin {
                origin_path = std::env::temp_dir().join(format!(
                    "sg-pub-origin-{}-{}",
                    std::process::id(),
                    n
                ));
                run(
                    &path,
                    &[
                        "init",
                        "-b",
                        "main",
                        "--bare",
                        origin_path.to_str().unwrap(),
                    ],
                );
                run(
                    &path,
                    &["remote", "add", "origin", origin_path.to_str().unwrap()],
                );
            }
            drop(origin_path);
            Self { path }
        }

        fn commit_file(&self, rel: &str, body: &[u8]) {
            let p = self.path.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
            run(&self.path, &["add", rel]);
            run(&self.path, &["commit", "-m", "evt"]);
        }

        fn push(&self, force: bool) {
            let mut args = vec!["push", "origin", "HEAD:refs/heads/main"];
            if force {
                args.insert(1, "--force");
            }
            run(&self.path, &args);
        }

        fn drop_history_rewrite(&self) {
            // 制造与已发布历史无关的新根提交，再 force-push。
            run(&self.path, &["checkout", "--orphan", "rewrite"]);
            run(&self.path, &["commit", "--allow-empty", "-m", "rewrite"]);
            self.push(true);
            run(&self.path, &["checkout", "main"]);
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn run(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git 可用");
        assert!(
            out.status.success(),
            "git {:?} 失败: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    const REL: &str = ".sixgates/events/wi1/E.json";
    const REF: &str = "refs/heads/main";

    #[test]
    fn draft_when_uncommitted() {
        let repo = Repo::new(false);
        std::fs::create_dir_all(repo.path.join(".sixgates/events/wi1")).unwrap();
        std::fs::write(repo.path.join(REL), b"{}").unwrap();
        let st = classify(&repo.path, REL, "E", "origin", REF, &[]);
        assert_eq!(st.state, State::Draft);
    }

    #[test]
    fn committed_then_published_via_remote_verification() {
        let repo = Repo::new(true);
        repo.commit_file(REL, b"{}");
        let st = classify(&repo.path, REL, "E", "origin", REF, &[]);
        assert_eq!(st.state, State::Committed, "远端无 ref → 未发布");
        repo.push(false);
        let st = classify(&repo.path, REL, "E", "origin", REF, &[]);
        assert_eq!(st.state, State::Published);
        let Some(PublicationWitness::Verified { commit_sha, .. }) = st.witness else {
            panic!("应有 Verified 见证");
        };
        // 见证 SHA 必须等于远端 ls-remote 的精确值（不是本机 HEAD 缓存）。
        let remote_sha = remote_ref_sha(&repo.path, "origin", REF).unwrap().unwrap();
        assert_eq!(commit_sha, remote_sha);
    }

    #[test]
    fn force_push_after_publication_is_revoked_with_prior_witness() {
        let repo = Repo::new(true);
        repo.commit_file(REL, b"{}");
        repo.push(false);
        let prior = vec![verified_witness(&repo)];
        repo.drop_history_rewrite();
        let st = classify(&repo.path, REL, "E", "origin", REF, &prior);
        assert_eq!(
            st.state,
            State::Revoked,
            "force-push 后曾发布事件 → revoked（C7-12）"
        );
        let Some(PublicationWitness::Revoked { reason, .. }) = st.witness else {
            panic!()
        };
        assert_eq!(reason, "ref_rewritten");
        // 无 prior 见证时同一状态只能判 Committed（无从证明曾经发布）。
        let st = classify(&repo.path, REL, "E", "origin", REF, &[]);
        assert_eq!(st.state, State::Committed);
    }

    #[test]
    fn unreachable_remote_is_unknown_fail_closed() {
        let repo = Repo::new(false);
        repo.commit_file(REL, b"{}");
        run(
            &repo.path,
            &["remote", "add", "origin", "/nonexistent/remote.git"],
        );
        let st = classify(&repo.path, REL, "E", "origin", REF, &[]);
        assert_eq!(
            st.state,
            State::Unknown,
            "远端不可达 → publication_unknown（C7-17）"
        );
    }

    fn verified_witness(repo: &Repo) -> PublicationWitness {
        let sha = remote_ref_sha(&repo.path, "origin", REF).unwrap().unwrap();
        PublicationWitness::Verified {
            event_id: "E".into(),
            ref_name: REF.into(),
            commit_sha: sha,
        }
    }
}
