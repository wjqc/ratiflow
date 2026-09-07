//! `apply_patch` 的补丁引擎（ADR 依据：Codex 能力差距方案 M5）。
//!
//! 职责：解析 unified diff → 路径/限额校验 → dry-run（内存应用，精确匹配上下文）
//! → 原子落盘。治理边界不变：模型只能"提案"补丁（Tool Proposal + 审批 + CAS），
//! 本模块不做任何审批判断；目标域固定为受管 worktree。
//!
//! 三态判定（方案 §5 M5 副作用不确定性）以文件 hash 表达：
//! - 现状 == before hash → 可安全写入；
//! - 现状 == after hash  → 已完成（重放不重复修改）；
//! - 其他               → unknown/冲突，停止。

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// 单文件补丁。
#[derive(Debug, Clone)]
pub struct FilePatch {
    /// 新路径（+++ 侧；Add 场景为目标创建路径）。
    pub path: String,
    /// 旧路径（--- 侧；/dev/null = 新建）。
    pub old_path: Option<String>,
    /// true = 删除文件（+++ 为 /dev/null）。
    pub is_delete: bool,
    /// true = 新建文件（--- 为 /dev/null）。
    pub is_new: bool,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone)]
pub struct Hunk {
    /// --- 侧起始行（1-based，容错偏移用）。
    pub old_start: usize,
    /// +++ 侧起始行。
    pub new_start: usize,
    pub lines: Vec<PatchLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchLineKind {
    Context,
    Add,
    Del,
}

#[derive(Debug, Clone)]
pub struct PatchLine {
    pub kind: PatchLineKind,
    pub text: String,
}

/// 限额（方案：拒绝超额文件/字节数）。
#[derive(Debug, Clone, Copy)]
pub struct PatchLimits {
    pub max_files: usize,
    pub max_total_bytes: usize,
}

impl Default for PatchLimits {
    fn default() -> Self {
        Self {
            max_files: 20,
            max_total_bytes: 512 * 1024,
        }
    }
}

/// 受保护路径（永不许补丁触碰；前缀匹配）。
pub const PROTECTED_PREFIXES: [&str; 3] = [".git/", ".ratiflow/", "ratiflow.lock"];

/// 解析 unified diff（容忍 `diff --git` 头；要求 ---/+++ 对与 @@ hunk）。
pub fn parse_patch(text: &str) -> Result<Vec<FilePatch>, String> {
    let mut patches: Vec<FilePatch> = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.peek() {
        let line = *line;
        if line.starts_with("diff --git ") || line.starts_with("Index: ") || line.is_empty() {
            lines.next();
            continue;
        }
        if !line.starts_with("--- ") {
            return Err(format!(
                "patch_parse_failed: 意外行（期望 --- 头）：{line:.60}"
            ));
        }
        let old_raw = line[4..].trim().to_string();
        lines.next();
        let plus = match lines.next() {
            Some(l) if l.starts_with("+++ ") => l[4..].trim().to_string(),
            other => {
                return Err(format!(
                    "patch_parse_failed: --- 后缺 +++ 头（{:?}）",
                    other.map(|s| s.chars().take(40).collect::<String>())
                ))
            }
        };
        let old_path = strip_path(&old_raw);
        let new_path = strip_path(&plus);
        let is_new = old_path == "/dev/null";
        let is_delete = new_path == "/dev/null";
        if !is_new && !is_delete && old_path != new_path {
            // 换名（rename）补丁不支持；若任一侧路径本身非法，优先给路径错误码。
            check_path(&old_path)?;
            check_path(&new_path)?;
            return Err(format!(
                "patch_parse_failed: 重命名/换名补丁不支持（{old_path} → {new_path}）"
            ));
        }
        let target = if is_delete {
            old_path.clone()
        } else {
            new_path.clone()
        };
        let mut hunks: Vec<Hunk> = Vec::new();
        // 消费本文件的 @@ hunks。
        while let Some(l) = lines.peek() {
            let l = *l;
            if l.starts_with("@@ ") {
                lines.next();
                hunks.push(parse_hunk(l, &mut lines)?);
            } else if l.starts_with("--- ")
                || l.starts_with("diff --git ")
                || l.starts_with("Index: ")
            {
                break;
            } else if l.trim().is_empty() {
                // 补丁尾部的空行：吞掉继续。
                lines.next();
            } else {
                return Err(format!(
                    "patch_parse_failed: hunk 外意外行：{:.60}",
                    l.chars().take(60).collect::<String>()
                ));
            }
        }
        if hunks.is_empty() && !is_new {
            return Err(format!("patch_parse_failed: {target} 无 hunk"));
        }
        patches.push(FilePatch {
            path: target,
            old_path: if is_new { None } else { Some(old_path) },
            is_delete,
            is_new,
            hunks,
        });
    }
    if patches.is_empty() {
        return Err("patch_parse_failed: 空补丁".into());
    }
    Ok(patches)
}

/// 解析一个 @@ hunk（头部后跟随的行由调用方迭代消费）。
fn parse_hunk(header: &str, lines: &mut dyn Iterator<Item = &str>) -> Result<Hunk, String> {
    // @@ -l,c +l,c @@ ...
    let head_err = || format!("patch_parse_failed: 非法 hunk 头：{header:.50}");
    let mut parts = header.split_whitespace();
    // 跳过前导 @@（header 形如 "@@ -l,c +l,c @@ context"）。
    let tok1 = parts.next().ok_or_else(head_err)?;
    let minus = if tok1.starts_with('-') {
        tok1
    } else {
        parts
            .next()
            .filter(|p| p.starts_with('-'))
            .ok_or_else(head_err)?
    };
    let plus = parts
        .next()
        .filter(|p| p.starts_with('+'))
        .ok_or_else(head_err)?;
    let old_start: usize = minus[1..]
        .split(',')
        .next()
        .and_then(|n| n.parse().ok())
        .ok_or_else(head_err)?;
    let new_start: usize = plus[1..]
        .split(',')
        .next()
        .and_then(|n| n.parse().ok())
        .ok_or_else(head_err)?;
    let mut hunk_lines = Vec::new();
    // 计算期望行数，读满为止（容忍头部计数缺失）。
    let expect_old = minus[1..]
        .split(',')
        .nth(1)
        .and_then(|n| n.parse::<usize>().ok());
    let expect_new = plus[1..]
        .split(',')
        .nth(1)
        .and_then(|n| n.parse::<usize>().ok());
    let expected = match (expect_old, expect_new) {
        (Some(o), Some(n)) => o + n,
        _ => usize::MAX,
    };
    while hunk_lines.len() < expected {
        let Some(l) = lines.next() else { break };
        if let Some(rest) = l.strip_prefix('\\') {
            // "\ No newline at end of file"：忽略语义，按无操作行。
            let _ = rest;
            continue;
        }
        let (kind, text) = if let Some(t) = l.strip_prefix(' ') {
            (PatchLineKind::Context, t)
        } else if let Some(t) = l.strip_prefix('+') {
            (PatchLineKind::Add, t)
        } else if let Some(t) = l.strip_prefix('-') {
            (PatchLineKind::Del, t)
        } else if l.is_empty() {
            // 空行 = 上下文空行（部分工具省略前导空格）。
            (PatchLineKind::Context, "")
        } else {
            break; // 下一个文件的 --- 头等。
        };
        hunk_lines.push(PatchLine {
            kind,
            text: text.to_string(),
        });
    }
    if hunk_lines.is_empty() {
        return Err(head_err());
    }
    Ok(Hunk {
        old_start,
        new_start,
        lines: hunk_lines,
    })
}

fn strip_path(raw: &str) -> String {
    // 去时间戳（\t 之后）与 a/ b/ 前缀。
    let no_ts = raw.split('\t').next().unwrap_or(raw).trim();
    let p = no_ts
        .strip_prefix("a/")
        .or_else(|| no_ts.strip_prefix("b/"))
        .unwrap_or(no_ts);
    p.trim_matches('"').to_string()
}

/// 路径与限额校验（绝对路径/.. /受保护路径/二进制标记/超额）。
pub fn validate(
    patches: &[FilePatch],
    raw_text_len: usize,
    limits: &PatchLimits,
) -> Result<(), String> {
    if patches.len() > limits.max_files {
        return Err(format!(
            "patch_rejected: 文件数 {} 超上限 {}",
            patches.len(),
            limits.max_files
        ));
    }
    if raw_text_len > limits.max_total_bytes {
        return Err(format!(
            "patch_rejected: 补丁 {} 字节超上限 {}",
            raw_text_len, limits.max_total_bytes
        ));
    }
    for p in patches {
        check_path(&p.path)?;
        if let Some(old) = &p.old_path {
            check_path(old)?;
        }
    }
    Ok(())
}

fn check_path(path: &str) -> Result<(), String> {
    let rel = Path::new(path);
    if path.is_empty() {
        return Err("patch_rejected: 空路径".into());
    }
    if rel.is_absolute() || path.starts_with('/') {
        return Err(format!("patch_rejected: 绝对路径 {path:?}"));
    }
    if rel
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!("patch_rejected: 路径含 .. 段 {path:?}"));
    }
    let norm = format!("{}/", path);
    if PROTECTED_PREFIXES.iter().any(|p| norm.starts_with(p)) || path == ".git" {
        return Err(format!("patch_rejected: 受保护路径 {path:?}"));
    }
    Ok(())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    sg_store::ids::hex(Sha256::digest(bytes).as_slice())
}

/// 一次 dry-run 产出的目标文件变更（含三态判定所需的 hash 对）。
#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: String,
    pub is_delete: bool,
    pub is_new: bool,
    pub before_hash: String,
    pub after_hash: String,
    pub after_content: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetState {
    /// 现状 == before hash：可写入。
    Before,
    /// 现状 == after hash：已完成（重放）。
    After,
    /// 其他：unknown/冲突。
    Conflict,
}

pub fn current_state(root: &Path, change: &FileChange) -> TargetState {
    let bytes = std::fs::read(root.join(&change.path)).unwrap_or_default();
    let h = sha256_hex(&bytes);
    if h == change.before_hash {
        TargetState::Before
    } else if h == change.after_hash {
        TargetState::After
    } else if change.is_new && bytes.is_empty() && change.before_hash.is_empty() {
        TargetState::Before
    } else {
        TargetState::Conflict
    }
}

/// 内存 dry-run：对每个文件精确应用 hunk，产出 after 内容与 hash 对。
/// 任何 hunk 上下文不匹配即 Err（不写盘）。
pub fn dry_run(root: &Path, patches: &[FilePatch]) -> Result<Vec<FileChange>, String> {
    let mut changes = Vec::with_capacity(patches.len());
    for p in patches {
        // 符号链接逃逸：解析后必须仍在 root 内。
        let target = resolve_in_root(root, &p.path)?;
        if p.is_delete {
            let bytes = std::fs::read(&target)
                .map_err(|_| format!("patch_conflict: 删除目标不存在 {}", p.path))?;
            if is_probably_binary(&bytes) {
                return Err(format!("patch_rejected: 二进制文件 {}", p.path));
            }
            changes.push(FileChange {
                path: p.path.clone(),
                is_delete: true,
                is_new: false,
                before_hash: sha256_hex(&bytes),
                after_hash: sha256_hex(b""),
                after_content: Vec::new(),
            });
            continue;
        }
        let before: Vec<u8> = if p.is_new {
            Vec::new()
        } else {
            let bytes = std::fs::read(&target)
                .map_err(|_| format!("patch_conflict: 目标不存在 {}", p.path))?;
            if is_probably_binary(&bytes) {
                return Err(format!("patch_rejected: 二进制文件 {}", p.path));
            }
            bytes
        };
        let before_text = String::from_utf8(before)
            .map_err(|_| format!("patch_rejected: 目标非 UTF-8（按二进制拒绝）{}", p.path))?;
        let mut lines: Vec<String> = if p.is_new {
            Vec::new()
        } else {
            before_text.lines().map(|l| l.to_string()).collect()
        };
        let mut offset: isize = 0;
        for (i, hunk) in p.hunks.iter().enumerate() {
            apply_hunk(&mut lines, hunk, offset)
                .map_err(|e| format!("patch_conflict: {} hunk {} {}", p.path, i + 1, e))?;
            let dels = hunk
                .lines
                .iter()
                .filter(|l| l.kind == PatchLineKind::Del)
                .count();
            let adds = hunk
                .lines
                .iter()
                .filter(|l| l.kind == PatchLineKind::Add)
                .count();
            offset += adds as isize - dels as isize;
        }
        let mut after = lines.join("\n");
        // 原文件以换行结尾 → 保留结尾换行（lines() 丢弃了尾换行）。
        if before_text.ends_with('\n') || p.is_new {
            after.push('\n');
        }
        let after_bytes = after.into_bytes();
        changes.push(FileChange {
            path: p.path.clone(),
            is_delete: false,
            is_new: p.is_new,
            before_hash: if p.is_new {
                String::new()
            } else {
                sha256_hex(before_text.as_bytes())
            },
            after_hash: sha256_hex(&after_bytes),
            after_content: after_bytes,
        });
    }
    Ok(changes)
}

/// hunk 应用：从 header 行号+偏移附近开始找 context/del 序列，精确匹配后替换。
fn apply_hunk(lines: &mut Vec<String>, hunk: &Hunk, offset: isize) -> Result<(), String> {
    let mut expected: Vec<&PatchLine> = Vec::new();
    let mut inserts: Vec<&PatchLine> = Vec::new();
    for l in &hunk.lines {
        match l.kind {
            PatchLineKind::Context => {
                expected.push(l);
                inserts.push(l);
            }
            PatchLineKind::Del => {
                expected.push(l);
            }
            PatchLineKind::Add => {
                inserts.push(l);
            }
        }
    }
    if expected.is_empty() {
        // 纯 Add hunk：在 header 位置插入。
        let at = (hunk.old_start as isize - 1 + offset).clamp(0, lines.len() as isize) as usize;
        for (k, ins) in inserts.iter().enumerate() {
            lines.insert(at + k, ins.text.clone());
        }
        return Ok(());
    }
    let want: Vec<&str> = expected.iter().map(|l| l.text.as_str()).collect();
    let preferred = (hunk.old_start as isize - 1 + offset).clamp(0, lines.len() as isize) as usize;
    let mut at = None;
    for start in search_order(preferred, lines.len(), want.len()) {
        if lines[start..start + want.len()] == want[..] {
            at = Some(start);
            break;
        }
    }
    let at = at.ok_or_else(|| "上下文不匹配（目标已漂移或补丁过期）".to_string())?;
    let replace_with: Vec<String> = inserts.iter().map(|l| l.text.clone()).collect();
    lines.splice(at..at + want.len(), replace_with);
    Ok(())
}

/// 搜索顺序：preferred 起向两侧扩展。
fn search_order(preferred: usize, total: usize, pat: usize) -> Vec<usize> {
    if pat > total {
        return Vec::new();
    }
    let max_start = total - pat;
    let mut out = Vec::new();
    out.push(preferred.min(max_start));
    let mut back = preferred.min(max_start);
    let mut fwd = preferred.min(max_start);
    loop {
        let mut progressed = false;
        if back > 0 {
            back -= 1;
            out.push(back);
            progressed = true;
        }
        if fwd < max_start {
            fwd += 1;
            out.push(fwd);
            progressed = true;
        }
        if !progressed {
            break;
        }
    }
    out
}

fn is_probably_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|&b| b == 0)
}

/// 解析目标路径：不许逃逸 root（含符号链接）；不存在段按最深存在祖先校验。
fn resolve_in_root(root: &Path, rel: &str) -> Result<PathBuf, String> {
    check_path(rel)?;
    let root_canon = root
        .canonicalize()
        .map_err(|_| format!("patch_rejected: worktree 不可访问 {}", root.display()))?;
    let joined = root_canon.join(rel);
    let mut cur = joined.clone();
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    while !cur.exists() && cur != root_canon {
        match (cur.file_name(), cur.parent()) {
            (Some(name), Some(parent)) => {
                suffix.push(name.to_os_string());
                cur = parent.to_path_buf();
            }
            _ => return Err("patch_rejected: 无法解析路径".into()),
        }
    }
    let canon = cur
        .canonicalize()
        .map_err(|_| "patch_rejected: 路径不可访问".to_string())?;
    let mut target = canon;
    for part in suffix.iter().rev() {
        target.push(part);
    }
    if !target.starts_with(&root_canon) {
        return Err(format!("patch_rejected: {rel} 逃逸 worktree"));
    }
    Ok(target)
}

/// 落盘：原子写（tmp+rename）/删除。调用方保证已过审批与 CAS。
pub fn apply(root: &Path, changes: &[FileChange]) -> Result<(), String> {
    for c in changes {
        let target = resolve_in_root(root, &c.path)?;
        if c.is_delete {
            std::fs::remove_file(&target)
                .map_err(|e| format!("patch_apply: 删除失败 {}: {e}", c.path))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("patch_apply: 建目录失败 {}: {e}", c.path))?;
        }
        let tmp = target.with_extension("sgpatch-tmp");
        std::fs::write(&tmp, &c.after_content)
            .map_err(|e| format!("patch_apply: 写失败 {}: {e}", c.path))?;
        std::fs::rename(&tmp, &target)
            .map_err(|e| format!("patch_apply: rename 失败 {}: {e}", c.path))?;
    }
    Ok(())
}

/// 反向补丁（Add↔Del 行互换、新旧侧互换）：用于"已应用"确认——
/// 反向补丁能干净 dry-run = 目标处于 after 状态（重放 no-op 判定）。
pub fn reverse_patch(patches: &[FilePatch]) -> Vec<FilePatch> {
    patches
        .iter()
        .map(|p| FilePatch {
            path: if p.is_new {
                p.old_path.clone().unwrap_or_else(|| p.path.clone())
            } else {
                p.path.clone()
            },
            old_path: Some(p.path.clone()),
            is_delete: p.is_new,
            is_new: p.is_delete,
            hunks: p
                .hunks
                .iter()
                .map(|h| Hunk {
                    old_start: h.new_start,
                    new_start: h.old_start,
                    lines: h
                        .lines
                        .iter()
                        .map(|l| PatchLine {
                            kind: match l.kind {
                                PatchLineKind::Add => PatchLineKind::Del,
                                PatchLineKind::Del => PatchLineKind::Add,
                                PatchLineKind::Context => PatchLineKind::Context,
                            },
                            text: l.text.clone(),
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect()
}

/// canonical patch digest：逐文件（path|before|after）排序后 sha256。
pub fn canonical_digest(changes: &[FileChange]) -> String {
    let mut lines: Vec<String> = changes
        .iter()
        .map(|c| format!("{}|{}|{}", c.path, c.before_hash, c.after_hash))
        .collect();
    lines.sort();
    sha256_hex(lines.join("\n").as_bytes())
}

/// 从提案参数提取补丁文本（契约：{patch, baseHead?, expectedHashes?}）。
pub fn patch_from_args(args: &Value) -> Result<String, String> {
    args.get("patch")
        .and_then(|v| v.as_str())
        .map(String::from)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing argument: patch".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE_FILE: &str = "line1\nline2\nline3\nline4\nline5\n";

    fn write_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sg-patch-{}-{}",
            std::process::id(),
            sg_store::ids::new_id("t")
        ));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.txt"), BASE_FILE).unwrap();
        dir
    }

    #[test]
    fn parse_and_apply_modify() {
        let patch = "--- a/src/a.txt\n+++ b/src/a.txt\n@@ -2,3 +2,3 @@ line1\n line2\n-line3\n+line3-modified\n line4\n";
        let patches = parse_patch(patch).unwrap();
        validate(&patches, patch.len(), &PatchLimits::default()).unwrap();
        let root = write_root();
        let changes = dry_run(&root, &patches).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "src/a.txt");
        assert_eq!(changes[0].before_hash, sha256_hex(BASE_FILE.as_bytes()));
        apply(&root, &changes).unwrap();
        let after = std::fs::read_to_string(root.join("src/a.txt")).unwrap();
        assert_eq!(after, "line1\nline2\nline3-modified\nline4\nline5\n");
        // 三态：现状 == after hash。
        assert_eq!(current_state(&root, &changes[0]), TargetState::After);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn add_and_delete_files() {
        let patch = "--- /dev/null\n+++ b/new.md\n@@ -0,0 +1,2 @@\n+hello\n+new\n--- a/src/a.txt\n+++ /dev/null\n@@ -1,5 +0,0 @@\n-line1\n-line2\n-line3\n-line4\n-line5\n";
        let patches = parse_patch(patch).unwrap();
        let root = write_root();
        let changes = dry_run(&root, &patches).unwrap();
        assert_eq!(changes.len(), 2);
        assert!(changes[0].is_new);
        assert!(changes[1].is_delete);
        apply(&root, &changes).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("new.md")).unwrap(),
            "hello\nnew\n"
        );
        assert!(!root.join("src/a.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reject_bad_paths_and_limits() {
        for bad in [
            "--- /etc/passwd\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n",
            "--- a/../../etc/passwd\n+++ b/../../etc/passwd\n@@ -1 +1 @@\n-a\n+b\n",
            "--- a/.git/config\n+++ b/.git/config\n@@ -1 +1 @@\n-a\n+b\n",
        ] {
            let err = parse_patch(bad)
                .and_then(|p| validate(&p, bad.len(), &PatchLimits::default()).map(|_| p))
                .and_then(|p| dry_run(&write_root(), &p).map(|_| ()))
                .unwrap_err();
            assert!(
                err.contains("patch_rejected") || err.contains("patch_conflict"),
                "{bad} → {err}"
            );
        }
        // 超额文件数。
        let many: String = (0..30)
            .map(|i| format!("--- /dev/null\n+++ b/f{i}.txt\n@@ -0,0 +1,1 @@\n+x{i}\n"))
            .collect();
        let patches = parse_patch(&many).unwrap();
        assert!(validate(&patches, many.len(), &PatchLimits::default()).is_err());
    }

    #[test]
    fn context_mismatch_is_conflict() {
        let patch = "--- a/src/a.txt\n+++ b/src/a.txt\n@@ -2,3 +2,3 @@\n wrong\n-context\n-lines\n";
        let patches = parse_patch(patch).unwrap();
        let root = write_root();
        let err = dry_run(&root, &patches).unwrap_err();
        assert!(
            err.contains("patch_conflict") && err.contains("上下文不匹配"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn symlink_escape_rejected() {
        #[cfg(unix)]
        {
            let root = write_root();
            std::os::unix::fs::symlink("/etc", root.join("src/evil")).unwrap();
            let patch =
                "--- a/src/evil/passwd\n+++ b/src/evil/passwd\n@@ -1 +1 @@\n-root\n+owned\n";
            let patches = parse_patch(patch).unwrap();
            let err = dry_run(&root, &patches).unwrap_err();
            assert!(
                err.contains("patch_rejected") || err.contains("patch_conflict"),
                "{err}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn binary_target_rejected() {
        let root = write_root();
        std::fs::write(root.join("bin.dat"), [0u8, 1, 0, 2]).unwrap();
        let patch = "--- a/bin.dat\n+++ b/bin.dat\n@@ -1 +1 @@\n-x\n+y\n";
        let patches = parse_patch(patch).unwrap();
        let err = dry_run(&root, &patches).unwrap_err();
        assert!(err.contains("二进制"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn replay_is_noop_via_state_check() {
        let patch = "--- a/src/a.txt\n+++ b/src/a.txt\n@@ -2,3 +2,3 @@\n line2\n-line3\n+line3-modified\n line4\n";
        let patches = parse_patch(patch).unwrap();
        let root = write_root();
        let changes = dry_run(&root, &patches).unwrap();
        apply(&root, &changes).unwrap();
        // 重放：现状已是 after → Before 状态判定为 After → 调用方跳过写入。
        assert_eq!(current_state(&root, &changes[0]), TargetState::After);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn canonical_digest_stable_and_input_sensitive() {
        let c1 = FileChange {
            path: "a".into(),
            is_delete: false,
            is_new: true,
            before_hash: String::new(),
            after_hash: "h2".into(),
            after_content: Vec::new(),
        };
        let c2 = FileChange {
            path: "b".into(),
            ..c1.clone()
        };
        let d1 = canonical_digest(&[c1.clone(), c2.clone()]);
        let d2 = canonical_digest(&[c2.clone(), c1.clone()]);
        assert_eq!(d1, d2, "顺序无关");
        let c3 = FileChange {
            after_hash: "h3".into(),
            ..c1.clone()
        };
        assert_ne!(canonical_digest(&[c1]), canonical_digest(&[c3]));
    }
}
