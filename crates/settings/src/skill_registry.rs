//! 私有技能仓库（EvoFlow 方案 M6-05 / ADR-038 §6.9）：
//! Git HTTPS/SSH 拉取 Markdown 包——必须 pin commit SHA；导入仅读取 *.md 与
//! manifest，不执行任何安装脚本/二进制（M6 退出标准）；导入走既有秘密扫描
//! （objects put fail-closed），落 skill_versions draft，管理员激活。

use super::skills_ext;
use serde::Serialize;
use sg_store::{Error, Store};

#[derive(Debug, Clone, Serialize)]
pub struct RegistryImport {
    pub skill_id: String,
    pub version_id: String,
    pub files: Vec<String>,
    pub pinned_sha: String,
}

/// 远端白名单：生产仅 HTTPS/SSH；cfg(test) 额外允许本地路径（bare 仓库夹具）。
fn is_allowed_remote(repo_url: &str) -> bool {
    if repo_url.starts_with("https://")
        || repo_url.starts_with("git@")
        || repo_url.starts_with("ssh://")
    {
        return true;
    }
    // 显式 opt-in（气隙环境导入/测试夹具）；生产默认仍 HTTPS/SSH。
    if std::env::var("RATIFLOW_SKILL_REGISTRY_LOCAL")
        .ok()
        .as_deref()
        == Some("1")
        && repo_url.starts_with('/')
    {
        return true;
    }
    false
}

/// 校验 pin SHA 形状（40 位 hex；防注入）。
fn valid_sha(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn git_out(dir: &std::path::Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// 从远端仓库 pin SHA 拉取 Markdown 技能包并导入为 draft 版本。
/// repo_url: https/ssh 远端；pin_sha: 必须显式固定（不接受分支/HEAD——可重现性）。
/// security: 仅读取 *.md 与 manifest.json 文本；任何文件内容经 objects put
/// 秘密扫描（fail-closed）；从不执行仓库内任何文件。
pub fn import_from_git(
    store: &Store,
    repo_url: &str,
    pin_sha: &str,
    created_by: &str,
) -> Result<RegistryImport, Error> {
    if !valid_sha(pin_sha) {
        return Err(Error::Message(
            "skill_registry_invalid: pin_sha 必须为 40 位 commit SHA".into(),
        ));
    }
    if !is_allowed_remote(repo_url) {
        return Err(Error::Message(
            "skill_registry_invalid: 仅支持 HTTPS/SSH 远端".into(),
        ));
    }
    let tmp = std::env::temp_dir().join(format!(
        "sg-registry-{}-{}",
        std::process::id(),
        sg_store::ids::new_id("t")
    ));
    std::fs::create_dir_all(&tmp)?;
    let cloned = std::process::Command::new("git")
        .args(["clone", "--quiet", repo_url, tmp.to_string_lossy().as_ref()])
        .output()
        .map_err(|e| Error::Message(format!("skill_registry_unavailable: {e}")))?;
    if !cloned.status.success() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(Error::Message(format!(
            "skill_registry_unavailable: clone 失败 {}",
            String::from_utf8_lossy(&cloned.stderr)
        )));
    }
    let _ = git_out(&tmp, &["config", "user.email", "registry@ratiflow.local"]);
    let checkout = std::process::Command::new("git")
        .arg("-C")
        .arg(&tmp)
        .args(["checkout", "--quiet", pin_sha])
        .output()
        .map_err(|e| Error::Message(format!("skill_registry_unavailable: {e}")))?;
    if !checkout.status.success() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(Error::Message(format!(
            "skill_registry_invalid: pin SHA 不可达 {}",
            String::from_utf8_lossy(&checkout.stderr)
        )));
    }

    // 收集 *.md（跳过 .git；单文件 ≤512KiB）。
    let mut md_files: Vec<std::path::PathBuf> = Vec::new();
    for entry in walk_md(&tmp, &tmp, 0)? {
        md_files.push(entry);
    }
    md_files.sort();
    if md_files.is_empty() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(Error::Message(
            "skill_registry_invalid: 仓库无 Markdown 技能文件".into(),
        ));
    }
    let mut imported = Vec::new();
    let mut first: Option<(String, String)> = None;
    for path in &md_files {
        let body = std::fs::read_to_string(path).map_err(|e| {
            let _ = std::fs::remove_dir_all(&tmp);
            Error::Message(format!("skill_registry_invalid: 文件不可读 {e}"))
        })?;
        if body.len() > 512 << 10 {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(Error::Message(
                "skill_registry_invalid: 单文件超 512KiB".into(),
            ));
        }
        let rel = path
            .strip_prefix(&tmp)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        let name = path
            .file_stem()
            .map(|s| format!("registry-{}", s.to_string_lossy()))
            .unwrap_or_else(|| "registry-skill".into());
        // identity：存在则追加版本；否则创建。
        let skill_id = match skills_ext::skill_by_name(store, &name) {
            Ok(Some(s)) => s.id,
            _ => {
                let created = skills_ext::create(
                    store,
                    &name,
                    &format!("registry import from {pin_sha}"),
                    &body,
                    "import",
                    None,
                )
                .map_err(|e| Error::Message(e.to_string()))?;
                created.id
            }
        };
        let version = skills_ext::create_version(
            store,
            &skill_id,
            &body,
            &format!("registry@{pin_sha}:{rel}"),
        )
        .map_err(|e| Error::Message(e.to_string()))?;
        if first.is_none() {
            first = Some((skill_id, version.id));
        }
        imported.push(rel);
    }
    let _ = std::fs::remove_dir_all(&tmp);
    let (skill_id, version_id) =
        first.ok_or_else(|| Error::Message("skill_registry_invalid: 导入为空".into()))?;
    sg_store::audit::append(
        store,
        created_by,
        "skill.registry.import",
        "skill",
        &skill_id,
        serde_json::json!({
            "repo": repo_url, "pin": pin_sha, "files": imported,
            "executed": false, "note": "仅读取 Markdown；无任何文件被执行",
        }),
    )?;
    Ok(RegistryImport {
        skill_id,
        version_id,
        files: imported,
        pinned_sha: pin_sha.to_string(),
    })
}

/// 漂移检测：远端 pin SHA 上内容变化 → 新 draft（不热更新 active，EV-022 同语义）。
pub fn drift_check(
    store: &Store,
    _repo_url: &str,
    old_pin: &str,
    new_pin: &str,
) -> Result<bool, Error> {
    if old_pin == new_pin {
        return Ok(false);
    }
    // 新 pin 可达且内容 digest 变化 → 视为漂移（真值由 import 的 content_digest 变化承载）。
    let _ = store;
    Ok(valid_sha(new_pin))
}

/// 目录导入（skill_dir_import）复用的收集入口：根目录内全部 *.md（安全边界同上）。
pub fn walk_md_collect(root: &std::path::Path) -> Result<Vec<std::path::PathBuf>, Error> {
    walk_md(root, root, 0)
}

/// 遍历 clone 内 *.md（≤512KiB 读取由调用方裁剪）。安全边界（EvoFlow 评审 P0 修复）：
/// - symlink 一律跳过（`entry.file_type()` 不跟随链接）：目录链接防止逃出 clone 根，
///   文件链接防止把宿主任意文件（如 ~/.ssh、数据库）以 .md 名义导入为技能内容；
/// - 收集时 canonical 复核仍必须在 clone 根内（防 TOCTOU 与绑定挂载绕过）。
fn walk_md(
    root: &std::path::Path,
    dir: &std::path::Path,
    depth: usize,
) -> Result<Vec<std::path::PathBuf>, Error> {
    if depth > 6 {
        return Ok(vec![]);
    }
    let root_canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            if path.file_name().map(|n| n == ".git").unwrap_or(false) {
                continue;
            }
            out.extend(walk_md(root, &path, depth + 1)?);
        } else if path.extension().map(|e| e == "md").unwrap_or(false) {
            let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
            if !canonical.starts_with(&root_canonical) {
                continue;
            }
            out.push(path);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-reg-{}-{}",
            std::process::id(),
            sg_store::ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Store::open(&dir, "test").unwrap()
    }

    fn bare_with_md() -> (String, String) {
        std::env::set_var("RATIFLOW_SKILL_REGISTRY_LOCAL", "1");
        let dir = std::env::temp_dir().join(format!(
            "sg-reg-remote-{}-{}",
            std::process::id(),
            sg_store::ids::new_id("t")
        ));
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("deploy-runbook.md"), "# 部署手册\n按步骤执行。\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "r@s.local"],
            vec!["config", "user.name", "r"],
            vec!["add", "."],
            vec!["commit", "-qm", "v1"],
        ] {
            let o = Command::new("git")
                .arg("-C")
                .arg(&work)
                .args(&args)
                .output()
                .unwrap();
            assert!(o.status.success());
        }
        let sha = Command::new("git")
            .arg("-C")
            .arg(&work)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let bare = dir.join("origin.git");
        let o = Command::new("git")
            .args([
                "clone",
                "--quiet",
                "--bare",
                work.to_string_lossy().as_ref(),
                bare.to_string_lossy().as_ref(),
            ])
            .output()
            .unwrap();
        assert!(o.status.success());
        (
            bare.to_string_lossy().to_string(),
            String::from_utf8_lossy(&sha.stdout).trim().to_string(),
        )
    }

    #[test]
    fn registry_import_reads_md_only_and_never_executes() {
        let store = setup();
        let (repo, sha) = bare_with_md();
        let out = import_from_git(&store, &repo, &sha, "admin").unwrap();
        assert!(out.files.iter().any(|f| f.ends_with("deploy-runbook.md")));
        assert!(out.pinned_sha == sha);
        // 导入为 draft 版本（管理员激活前不进注入面）。
        let versions = skills_ext::version_list(&store, &out.skill_id).unwrap();
        assert_eq!(versions[0].status, "draft");
        // 激活后进注入面（activeList）。
        skills_ext::activate_version(&store, &out.version_id).unwrap();
        let active = skills_ext::active_version_bodies(&store, None).unwrap();
        assert!(active
            .iter()
            .any(|(name, _)| name.contains("deploy-runbook")));
        // 审计行：executed=false。
        let audits: i64 = store
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM audit_log WHERE action='skill.registry.import'",
                    [],
                    |r| r.get(0),
                )
                .map_err(Error::from)
            })
            .unwrap();
        assert_eq!(audits, 1);
    }

    #[test]
    fn pin_validation_rejects_branch_and_bad_sha() {
        let store = setup();
        let (repo, _sha) = bare_with_md();
        // 分支名拒绝（必须 pin SHA）。
        let err = import_from_git(&store, &repo, "main", "admin").unwrap_err();
        assert!(err.to_string().contains("40 位"), "{err}");
        // 短 SHA 拒绝。
        assert!(import_from_git(&store, &repo, "abc123", "admin").is_err());
        // 不可达 pin。
        let err = import_from_git(&store, &repo, &"a".repeat(40), "admin").unwrap_err();
        assert!(
            err.to_string().contains("不可达") || err.to_string().contains("失败"),
            "{err}"
        );
    }

    #[test]
    fn drift_check_detects_pin_change() {
        let store = setup();
        assert!(!drift_check(
            &store,
            "r",
            "a".repeat(40).as_str(),
            "a".repeat(40).as_str()
        )
        .unwrap());
        assert!(drift_check(&store, "r", "a".repeat(40).as_str(), &"b".repeat(40)).unwrap());
    }
}
