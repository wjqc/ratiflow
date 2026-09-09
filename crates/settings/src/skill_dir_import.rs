//! 开发规范目录导入（通用机制）：任意外部 Markdown 目录（如 agent-dev-spec）
//! 整体导入为技能库——每个 .md 文件 = 一个技能（draft 版本，管理员激活后才进注入面）。
//! 安全边界与 skill_registry 一致：symlink 一律跳过、canonical 复核不逃逸根目录、
//! 深度 ≤6、单文件 ≤512KiB、只读不执行；正文经 objects::put 秘密扫描 fail-closed。
//! 幂等：同内容重导命中既有 content_digest，不产生新版本（三态：imported/updated/skipped）。
//! 技能名 = 相对路径去扩展名、分隔符转 '-'（README.md 多目录同名靠路径消歧）。

use super::skills_ext;
use serde::Serialize;
use sg_store::{audit, Error, Store};

/// 单次导入的文件数上限（与知识扫描 maxFilesPerSource 对齐；超限 fail-loud，
/// 调用方用 subdirs 收窄）。
const MAX_FILES: usize = 500;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryImport {
    /// 新建技能（含首个 draft 版本）的相对路径。
    pub imported: Vec<String>,
    /// 既有技能产生了新 draft 版本（内容变化）。
    pub updated: Vec<String>,
    /// 内容未变（digest 幂等命中，无新版本）。
    pub skipped: Vec<String>,
    /// 逐文件失败清单（非法名/空正文/秘密扫描/超限等），不中断其余文件。
    pub failed: Vec<FailedFile>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedFile {
    pub file: String,
    pub reason: String,
}

/// 相对路径 → 技能名：去扩展名、`/` 与 `.` 转 `-`。
fn skill_name_for(rel: &str) -> String {
    let stem = rel.strip_suffix(".md").unwrap_or(rel);
    stem.chars()
        .map(|c| match c {
            '/' | '.' | '_' => '-',
            other => other,
        })
        .collect()
}

/// 导入目录下全部（或 subdirs 前缀内的）.md 为技能 draft 版本。
/// path 必须是存在的绝对目录；subdirs 为相对根的首段前缀过滤（如
/// ["standards","workflow","lessons"]）。
pub fn import_directory(
    store: &Store,
    path: &str,
    subdirs: &[String],
    created_by: &str,
) -> Result<DirectoryImport, Error> {
    let root = std::path::Path::new(path);
    if !path.starts_with('/') {
        return Err(Error::Message(
            "skill_directory_invalid: path 必须为绝对路径".into(),
        ));
    }
    let canonical = root
        .canonicalize()
        .map_err(|_| Error::Message("skill_directory_invalid: 目录不存在或不可访问".into()))?;
    if !canonical.is_dir() {
        return Err(Error::Message(
            "skill_directory_invalid: path 不是目录".into(),
        ));
    }

    let mut files = super::skill_registry::walk_md_collect(&canonical)?;
    files.sort();
    // 前缀过滤（首段匹配：standards 匹配 standards/... 全部）。
    if !subdirs.is_empty() {
        files.retain(|p| {
            let rel = p.strip_prefix(&canonical).unwrap_or(p);
            rel.components()
                .next()
                .and_then(|c| c.as_os_str().to_str())
                .map(|first| subdirs.iter().any(|s| s == first))
                .unwrap_or(false)
        });
    }
    if files.is_empty() {
        return Err(Error::Message(
            "skill_directory_invalid: 目录（或所选子目录）内无 Markdown 文件".into(),
        ));
    }
    if files.len() > MAX_FILES {
        return Err(Error::Message(format!(
            "skill_directory_invalid: 文件数 {} 超上限 {MAX_FILES}，请用 subdirs 收窄",
            files.len()
        )));
    }

    let mut out = DirectoryImport {
        imported: Vec::new(),
        updated: Vec::new(),
        skipped: Vec::new(),
        failed: Vec::new(),
    };
    for file in &files {
        let rel = file
            .strip_prefix(&canonical)
            .unwrap_or(file)
            .to_string_lossy()
            .to_string();
        let name = skill_name_for(&rel);
        let body = match std::fs::read_to_string(file) {
            Ok(body) => body,
            Err(e) => {
                out.failed.push(FailedFile {
                    file: rel,
                    reason: format!("不可读：{e}"),
                });
                continue;
            }
        };
        // description 取首个一级标题行；无标题回退首段非空行截断。
        let description = body
            .lines()
            .find(|l| l.starts_with("# "))
            .map(|l| l.trim_start_matches("# ").trim().to_string())
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| {
                body.lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty() && !l.starts_with('#'))
                    .map(|l| l.chars().take(80).collect())
                    .unwrap_or_else(|| rel.clone())
            });
        // 既有技能：非 import 来源视为名称冲突（不允许目录导入覆盖手工/市场资产）。
        let existing =
            skills_ext::skill_by_name(store, &name).map_err(|e| Error::Message(e.to_string()))?;
        let skill_id = match existing {
            Some(skill) => {
                if skill.source != "import" {
                    out.failed.push(FailedFile {
                        file: rel,
                        reason: format!("名称 {} 与 {} 来源技能冲突", name, skill.source),
                    });
                    continue;
                }
                skill.id
            }
            None => match skills_ext::create(store, &name, &description, &body, "import", None) {
                Ok(created) => {
                    out.imported.push(rel.clone());
                    created.id
                }
                Err(e) => {
                    // 秘密扫描 fail-closed / 空正文 / 非法名：记失败，不影响其余文件。
                    out.failed.push(FailedFile {
                        file: rel,
                        reason: e.to_string(),
                    });
                    continue;
                }
            },
        };
        // create_version 按 content_digest 幂等：命中既有版本 id → skipped；
        // 新版本 id → updated（对 imported 的首版本不计 updated）。
        let before: Vec<String> = skills_ext::version_list(store, &skill_id)
            .map_err(|e| Error::Message(e.to_string()))?
            .into_iter()
            .map(|v| v.id)
            .collect();
        match skills_ext::create_version(
            store,
            &skill_id,
            &body,
            &format!("directory import: {rel}"),
        ) {
            Ok(version) => {
                if !before.is_empty() {
                    if before.contains(&version.id) {
                        out.skipped.push(rel);
                    } else {
                        out.updated.push(rel);
                    }
                }
            }
            Err(e) => {
                // 秘密扫描 fail-closed / 空正文等：记失败，不影响其余文件。
                out.failed.push(FailedFile {
                    file: rel,
                    reason: e.to_string(),
                });
            }
        }
    }

    audit::append(
        store,
        created_by,
        "skill.directory.import",
        "skill",
        &canonical.to_string_lossy(),
        serde_json::json!({
            "root": path,
            "subdirs": subdirs,
            "imported": out.imported.len(),
            "updated": out.updated.len(),
            "skipped": out.skipped.len(),
            "failed": out.failed.len(),
            "executed": false,
            "note": "仅读取 Markdown；无任何文件被执行",
        }),
    )?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sg_store::Store;

    fn setup() -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "sg-dirimport-{}-{}",
            std::process::id(),
            sg_store::ids::new_id("t")
        ));
        let spec = dir.join("spec");
        std::fs::create_dir_all(spec.join("standards").join("core")).unwrap();
        std::fs::create_dir_all(spec.join("templates")).unwrap();
        std::fs::write(
            spec.join("standards")
                .join("core")
                .join("side-effect-safety.md"),
            "# 写操作安全\n写工具必须显式声明。\n",
        )
        .unwrap();
        std::fs::write(spec.join("README.md"), "# 规范套件\n总说明。\n").unwrap();
        std::fs::write(
            spec.join("templates").join("adr.md"),
            "# ADR 模板\n背景/决策。\n",
        )
        .unwrap();
        std::fs::write(spec.join("templates").join("ignored.txt"), "非 markdown").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        (Store::open(&dir, "test").unwrap(), spec)
    }

    #[test]
    fn directory_import_creates_draft_skills_with_path_names() {
        let (store, spec) = setup();
        let out = import_directory(&store, spec.to_str().unwrap(), &[], "admin").unwrap();
        assert_eq!(out.imported.len(), 3, "{:?}", out.imported);
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        // 名称按相对路径消歧（多个 README 不冲突）。
        let names: Vec<String> = skills_ext::list(&store)
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(names.contains(&"README".to_string()), "{names:?}");
        assert!(names.contains(&"standards-core-side-effect-safety".to_string()));
        assert!(names.contains(&"templates-adr".to_string()));
        // 全部 draft：激活前不进注入面。
        for s in skills_ext::list(&store).unwrap() {
            for v in skills_ext::version_list(&store, &s.id).unwrap() {
                assert_eq!(v.status, "draft");
            }
        }
        // 审计 executed=false。
        let n: i64 = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM audit_log WHERE action='skill.directory.import'",
                    [],
                    |r| r.get(0),
                )
                .map_err(Error::from)
            })
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn reimport_is_idempotent_and_drift_makes_new_draft() {
        let (store, spec) = setup();
        import_directory(&store, spec.to_str().unwrap(), &[], "admin").unwrap();
        // 同内容重导：全部 skipped，无新版本。
        let again = import_directory(&store, spec.to_str().unwrap(), &[], "admin").unwrap();
        assert!(again.imported.is_empty() && again.updated.is_empty());
        assert_eq!(again.skipped.len(), 3);
        // 内容变化 → 新 draft（不热更新既有版本）。
        std::fs::write(
            spec.join("standards")
                .join("core")
                .join("side-effect-safety.md"),
            "# 写操作安全 v2\n超时不等于失败。\n",
        )
        .unwrap();
        let third = import_directory(&store, spec.to_str().unwrap(), &[], "admin").unwrap();
        assert_eq!(
            third.updated,
            vec!["standards/core/side-effect-safety.md".to_string()]
        );
        let skill = skills_ext::skill_by_name(&store, "standards-core-side-effect-safety")
            .unwrap()
            .unwrap();
        assert_eq!(
            skills_ext::version_list(&store, &skill.id).unwrap().len(),
            2
        );
    }

    #[test]
    fn subdirs_filter_and_validation_errors() {
        let (store, spec) = setup();
        let out = import_directory(
            &store,
            spec.to_str().unwrap(),
            &["templates".into()],
            "admin",
        )
        .unwrap();
        assert_eq!(out.imported, vec!["templates/adr.md".to_string()]);
        // 相对路径拒绝。
        let err = import_directory(&store, "relative/path", &[], "admin").unwrap_err();
        assert!(err.to_string().contains("绝对路径"));
        // 不存在目录拒绝。
        let err = import_directory(&store, "/nonexistent-xyz", &[], "admin").unwrap_err();
        assert!(err.to_string().contains("不存在"));
        // 空匹配拒绝（fail-loud）。
        let err = import_directory(
            &store,
            spec.to_str().unwrap(),
            &["no-such-dir".into()],
            "admin",
        )
        .unwrap_err();
        assert!(err.to_string().contains("无 Markdown"));
    }

    #[test]
    fn secret_file_fails_closed_per_file() {
        let (store, spec) = setup();
        std::fs::write(
            spec.join("standards").join("core").join("leaked.md"),
            "# 泄漏\npassword = \"supersecret123\"\n",
        )
        .unwrap();
        let out = import_directory(&store, spec.to_str().unwrap(), &[], "admin").unwrap();
        assert_eq!(out.failed.len(), 1, "{:?}", out.failed);
        assert!(out.failed[0].file.ends_with("leaked.md"));
        // 其余文件照常导入（失败不扩散）。
        assert_eq!(out.imported.len(), 3);
    }
}
