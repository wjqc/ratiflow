//! Git 仓库导入 MCP（RDWS 实施计划 v1.4 WP-4 / 0042 mcp_repo_imports）。
//!
//! 八态状态机：imported → awaiting_probe_approval → probing → schema_candidate →
//! awaiting_activation → active | failed | unknown | revoked。
//!
//! 安全链（两次批准前零代码执行）：
//! - importAdd 只做 ls-remote 解析 ref→SHA + 建 probe 审批（不 clone 不执行）；
//! - probe 批准后 worker CAS 认领（owner/lease/attempt）才 clone（空仓库+受限 fetch
//!   --depth 1 固定 SHA，不抓默认分支）→ 冻结五元组 → 沙箱内探针；
//! - activation 批准后才建 mcp_servers 行；每次 call 前重算工作树 Merkle 复核冻结
//!   五元组，漂移 → tool_source_drift fail-closed（仅可重新 import）。
//!
//! 形态偏差说明：计划「首版仅 HTTPS」针对 SSH（ProxyCommand 本地执行面）；本实现
//! 额外放行 file:// 本地仓库路径——单机桌面形态下导入本机仓库是正当场景，且
//! e2e/CI 需要无网络依赖的真实远端。git@/ssh:// 仍拒绝（WP-4b 立项）。
//!
//! Flag：SIXGATES_MCP_GIT_IMPORT（默认 0）。

use serde_json::{json, Value};
use sg_store::{ids, timefmt, Error, Store};
use std::path::{Path, PathBuf};
use std::process::Command;

const FLAG: &str = "SIXGATES_MCP_GIT_IMPORT";
const IMPORT_DIR: &str = "mcp-imports";
const MANIFEST_FILE: &str = "sixgates-mcp.json";
/// 后置体积上限（200MB，处于 512MiB fetch 配额内的诚实边界）。
const MAX_CHECKOUT_BYTES: u64 = 200 * 1024 * 1024;
const MAX_FILES: usize = 20_000;
const FETCH_TIMEOUT_SECS: u64 = 120;
/// worker 租约（长步骤心跳续租；过期由 reconciliation 收敛 unknown）。
const WORKER_LEASE_SECS: i64 = 300;

pub fn enabled() -> bool {
    std::env::var(FLAG).ok().as_deref() == Some("1")
}

fn flag_off() -> Error {
    Error::Message("feature_disabled: SIXGATES_MCP_GIT_IMPORT 未开启".into())
}

// ---------------------------------------------------------------------------
// 路径布局：<dataDir>/mcp-imports/<importId>/{checkout,state,tmp}
// ---------------------------------------------------------------------------

pub fn import_root(data_dir: &Path, import_id: &str) -> PathBuf {
    data_dir.join(IMPORT_DIR).join(import_id)
}

fn checkout_dir(data_dir: &Path, import_id: &str) -> PathBuf {
    import_root(data_dir, import_id).join("checkout")
}

fn state_dir(data_dir: &Path, import_id: &str) -> PathBuf {
    import_root(data_dir, import_id).join("state")
}

fn temp_dir(data_dir: &Path, import_id: &str) -> PathBuf {
    import_root(data_dir, import_id).join("tmp")
}

fn cleanup_import_dir(data_dir: &Path, import_id: &str) {
    let root = import_root(data_dir, import_id);
    // 冻结把目录置为只读——删除前恢复写位（目录需要 w 才能移除内容）。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            if let Ok(rd) = std::fs::read_dir(&d) {
                for e in rd.flatten() {
                    if let Ok(m) = std::fs::symlink_metadata(e.path()) {
                        if m.is_dir() {
                            stack.push(e.path());
                        }
                        let mut perms = m.permissions();
                        perms.set_mode(perms.mode() | 0o222);
                        let _ = std::fs::set_permissions(e.path(), perms);
                    }
                }
            }
            if let Ok(m) = std::fs::symlink_metadata(&d) {
                let mut perms = m.permissions();
                perms.set_mode(perms.mode() | 0o222);
                let _ = std::fs::set_permissions(&d, perms);
            }
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Manifest（仓库根 sixgates-mcp.json，canonical JSON，fail-closed）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ImportManifest {
    pub entrypoint_command: String,
    pub entrypoint_args: Vec<String>,
    pub writable_dirs: Vec<String>,
    pub probe_timeout_sec: u64,
    pub call_timeout_sec: u64,
}

/// manifest 校验：未知字段/高版本/绝对路径/`..`/symlink 段/非 none deps →
/// import_manifest_invalid（fail-closed）。
pub fn parse_manifest(raw: &str, checkout: &Path) -> Result<ImportManifest, String> {
    let v: Value =
        serde_json::from_str(raw).map_err(|e| format!("import_manifest_invalid: {e}"))?;
    if v.get("schemaVersion").and_then(|s| s.as_i64()) != Some(1) {
        return Err("import_manifest_invalid: 仅支持 schemaVersion=1".into());
    }
    let known = ["schemaVersion", "entrypoint", "sandbox", "timeouts", "deps"];
    if let Some(obj) = v.as_object() {
        for k in obj.keys() {
            if !known.contains(&k.as_str()) {
                return Err(format!("import_manifest_invalid: 未知字段 {k}"));
            }
        }
    }
    if v["deps"]["manager"].as_str().unwrap_or("none") != "none" {
        return Err(
            "import_deps_unsupported: 首版仅 deps.manager=none（npm/pip builder 属 WP-4b）".into(),
        );
    }
    let cmd = v["entrypoint"]["command"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let args: Vec<String> = v["entrypoint"]["args"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if cmd.is_empty() {
        return Err("import_manifest_invalid: entrypoint.command 必填".into());
    }
    let writable: Vec<String> = v["sandbox"]["writableDirs"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if v["sandbox"]["network"].as_bool() != Some(false) {
        return Err("import_manifest_invalid: sandbox.network 必须为 false（禁网硬前提）".into());
    }
    // 路径封堵：command 与 writableDirs 一律相对路径；canonicalize+前缀断言落在
    // checkout 内；任何 symlink 段拒绝。
    let assert_relative = |p: &str, what: &str| -> Result<PathBuf, String> {
        if Path::new(p).is_absolute() || p.split('/').any(|seg| seg == "..") {
            return Err(format!("import_manifest_invalid: {what} 必须为 checkout 内相对路径（拒绝绝对路径/..）：{p}"));
        }
        let joined = checkout.join(p);
        // symlink 段检查（父目录链上任何 symlink 段都拒绝——不依赖目标存在）。
        let mut cur = checkout.to_path_buf();
        for seg in Path::new(p).components() {
            cur.push(seg);
            if std::fs::symlink_metadata(&cur)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
            {
                return Err(format!(
                    "import_manifest_invalid: {what} 路径含 symlink 段：{}",
                    cur.display()
                ));
            }
        }
        let canon_parent = joined
            .parent()
            .and_then(|p| p.canonicalize().ok())
            .ok_or_else(|| format!("import_manifest_invalid: {what} 父目录不可解析：{p}"))?;
        if !canon_parent.starts_with(checkout.canonicalize().unwrap_or_default()) {
            return Err(format!(
                "import_manifest_invalid: {what} 逃逸 checkout：{p}"
            ));
        }
        Ok(joined)
    };
    // command 允许是系统解释器（如 python3）——只对参数里像路径的项做封堵？不：
    // 计划要求 command 一律相对路径（防执行任意系统二进制），入口脚本经 args 传入。
    assert_relative(&cmd, "entrypoint.command")?;
    for w in &writable {
        let joined = assert_relative(w, "sandbox.writableDirs")?;
        if joined.starts_with(checkout) {
            return Err(
                "import_manifest_invalid: writableDirs 不得指向源 checkout（只读冻结）".into(),
            );
        }
    }
    let timeouts = &v["timeouts"];
    let probe_timeout_sec = timeouts["probeSec"].as_u64().unwrap_or(30).clamp(1, 300);
    let call_timeout_sec = timeouts["callSec"].as_u64().unwrap_or(60).clamp(1, 600);
    Ok(ImportManifest {
        entrypoint_command: cmd,
        entrypoint_args: args,
        writable_dirs: writable,
        probe_timeout_sec,
        call_timeout_sec,
    })
}

pub fn manifest_digest(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    ids::hex(&Sha256::digest(raw.as_bytes()))
}

// ---------------------------------------------------------------------------
// git 命令（环境清理：GIT_CONFIG_NOSYSTEM / GLOBAL=/dev/null / hooksPath / LFS / 凭据清空）
// ---------------------------------------------------------------------------

fn git_env(cmd: &mut Command) -> &mut Command {
    cmd.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("credential.helper", "")
        .env_remove("GIT_CONFIG_LOCAL")
}

fn git_run(dir: Option<&Path>, args: &[&str], timeout_secs: u64) -> Result<String, String> {
    let mut cmd = Command::new("git");
    git_env(&mut cmd);
    if let Some(d) = dir {
        cmd.current_dir(d);
        cmd.args(["-c", "core.hooksPath=/dev/null"]);
    }
    cmd.args(args);
    let out = wait_with_timeout(cmd, timeout_secs)?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {} 失败: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr)
                .trim()
                .chars()
                .take(200)
                .collect::<String>()
        ))
    }
}

fn wait_with_timeout(mut cmd: Command, timeout_secs: u64) -> Result<std::process::Output, String> {
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("git spawn 失败: {e}"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs.max(1));
    loop {
        match child
            .try_wait()
            .map_err(|e| format!("git wait 失败: {e}"))?
        {
            Some(_) => {
                // 完成：取全量输出（管道可能阻塞——用 output 收尾）。
                return child
                    .wait_with_output()
                    .map_err(|e| format!("git output 失败: {e}"));
            }
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("git 超时（>{timeout_secs}s）"));
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
}

fn validate_repo_url(url: &str) -> Result<(), String> {
    if url.starts_with("https://") {
        return Ok(());
    }
    if url.starts_with("file://") || (Path::new(url).is_absolute() && Path::new(url).exists()) {
        return Ok(()); // 本地仓库（单机形态正当场景；见模块头偏差说明）
    }
    Err(format!(
        "import_manifest_invalid: repo_url 仅支持 https://（ssh/git@ 属 WP-4b）：{url}"
    ))
}

// ---------------------------------------------------------------------------
// 内容冻结五元组
// ---------------------------------------------------------------------------

/// 全工作树 Merkle：按路径序对 (path, sha256(content)) 链式哈希——任何文件
/// 变化（含依赖模块、入口外文件）都改变 digest。
fn checkout_merkle(root: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(root, &mut files, 0)?;
    files.sort();
    let mut hasher = Sha256::new();
    for f in &files {
        let rel = f
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .to_string();
        let content = std::fs::read(f).map_err(|e| format!("merkle 读失败 {rel}: {e}"))?;
        hasher.update(rel.as_bytes());
        hasher.update(Sha256::digest(&content));
    }
    Ok(ids::hex(&hasher.finalize()))
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) -> Result<(), String> {
    if depth > 32 {
        return Err("import_manifest_invalid: 目录层级过深".into());
    }
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let meta = std::fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() {
            // symlink 出逃拒绝（无论目标在内在外——首版一刀切，保守面）。
            return Err(format!(
                "import_symlink_escape: 检出内容含 symlink：{}",
                entry.path().display()
            ));
        }
        if meta.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == ".git" {
                continue;
            }
            collect_files(&entry.path(), out, depth + 1)?;
        } else if meta.is_file() {
            out.push(entry.path());
        }
    }
    Ok(())
}

fn dir_size(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                if let Ok(m) = std::fs::symlink_metadata(e.path()) {
                    if m.is_dir() {
                        stack.push(e.path());
                    } else if m.is_file() {
                        total += m.len();
                    }
                }
            }
        }
    }
    total
}

/// 冻结五元组：{commit_tree_digest, checkout_merkle_digest, manifest_digest,
/// entrypoint_file_hash, dirty, import_id}。
fn compute_freeze(
    checkout: &Path,
    manifest_digest_hex: &str,
    entrypoint_rel: &str,
    import_id: &str,
) -> Result<Value, String> {
    let commit_tree = git_run(Some(checkout), &["rev-parse", "HEAD^{tree}"], 30)?;
    let merkle = checkout_merkle(checkout)?;
    let entry_hash = {
        use sha2::{Digest, Sha256};
        let p = checkout.join(entrypoint_rel);
        let content = std::fs::read(&p)
            .map_err(|e| format!("import_manifest_invalid: 入口文件不可读: {e}"))?;
        ids::hex(&Sha256::digest(&content))
    };
    let dirty = !git_run(Some(checkout), &["status", "--porcelain"], 30)?.is_empty();
    if dirty {
        return Err("import_manifest_invalid: 检出后工作树不干净（拒绝冻结脏树）".into());
    }
    Ok(json!({
        "commit_tree_digest": commit_tree,
        "checkout_merkle_digest": merkle,
        "manifest_digest": manifest_digest_hex,
        "entrypoint_file_hash": entry_hash,
        "dirty": false,
        "import_id": import_id,
    }))
}

/// call 前复核：同一 canonical 算法重算 Merkle + porcelain——任一漂移 →
/// tool_source_drift（fail-closed，仅可重新 import 产新候选）。
pub fn verify_freeze(store: &Store, data_dir: &Path, import_id: &str) -> Result<(), String> {
    let row: Option<(String, String, String)> = store
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT COALESCE(content_freeze_json,''), status, COALESCE(progress_cursor,'')
                 FROM mcp_repo_imports WHERE id=?1",
                [import_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok())
        })
        .map_err(|e| e.to_string())?;
    let Some((freeze_raw, status, _cursor)) = row else {
        return Err("tool_source_drift: import 行缺失".into());
    };
    if status != "active" {
        return Ok(()); // 非 active 无执行面
    }
    let freeze: Value = serde_json::from_str(&freeze_raw)
        .map_err(|e| format!("tool_source_drift: 冻结记录损坏 {e}"))?;
    let checkout = checkout_dir(data_dir, import_id);
    let merkle = checkout_merkle(&checkout)?;
    if merkle != freeze["checkout_merkle_digest"].as_str().unwrap_or("") {
        return Err(
            "tool_source_drift: 工作树 Merkle 与冻结五元组不一致（仅可重新 import）".into(),
        );
    }
    let dirty = !git_run(Some(&checkout), &["status", "--porcelain"], 30)?.is_empty();
    if dirty || freeze["dirty"].as_bool() != Some(false) {
        return Err("tool_source_drift: 工作树脏（仅可重新 import）".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// RPC：importAdd / importDecide / importResume / importRevoke / importList / importGet
// ---------------------------------------------------------------------------

pub fn import_add(
    store: &Store,
    data_dir: &Path,
    repo_url: &str,
    ref_name: &str,
    created_by: &str,
) -> Result<Value, Error> {
    if !enabled() {
        return Err(flag_off());
    }
    validate_repo_url(repo_url).map_err(Error::Message)?;
    if ref_name.trim().is_empty() {
        return Err(Error::Message("import_manifest_invalid: ref 必填".into()));
    }
    // 解析 ref → SHA（ls-remote；失败即 failed——不建行？计划：失败即 failed。
    // 行必须存在才能承载 failed 状态与错误可见性 → 先建行再失败？八态含 failed，
    // importAdd 解析失败按计划「failed」落行（审计可见），返回错误。
    let id = ids::new_id("mcpimp");
    let now = timefmt::now();
    let sha = match ls_remote_sha(repo_url, ref_name) {
        Ok(s) => s,
        Err(e) => {
            store.with_conn(|c| {
                c.execute(
                    "INSERT INTO mcp_repo_imports(id, repo_url, ref_name, pinned_sha, manifest_digest,
                         status, error, created_by, created_at, updated_at)
                     VALUES (?1,?2,?3,'','', 'failed', ?4, ?5, ?6, ?6)",
                    rusqlite::params![id, repo_url, ref_name, e, created_by, now],
                )?;
                Ok(())
            })?;
            return Err(Error::Message(format!(
                "import_sha_mismatch: ref 解析失败 {e}"
            )));
        }
    };
    // manifest_digest 此时尚未知（未 clone）——唯一键 (repo_url, pinned_sha, manifest_digest)
    // 用占位 <pending>；clone 完成后固化真实值。同 ref 漂移 = 新 SHA = 新行。
    store.with_tx_immediate(|tx| {
        tx.execute(
            "INSERT INTO mcp_repo_imports(id, repo_url, ref_name, pinned_sha, manifest_digest,
                 status, created_by, created_at, updated_at)
             VALUES (?1,?2,?3,?4,'<pending>','awaiting_probe_approval',?5,?6,?6)",
            rusqlite::params![id, repo_url, ref_name, sha, created_by, now],
        )?;
        // 同事务建 probe 审批（subject_type=mcp_import_probe，0042 已扩枚举）。
        tx.execute(
            "INSERT INTO approvals(id, subject_type, subject_id, action_digest, risk, status,
                 expires_at, reason, created_at)
             VALUES (?1,'mcp_import_probe',?2,?3,'high','requested',?4,?5,?6)",
            rusqlite::params![
                ids::new_id("apr"),
                id,
                format!("import|{repo_url}|{sha}"),
                timefmt::now_plus_days(7),
                format!("Git 仓库导入探针批准：{repo_url}@{ref_name}"),
                now
            ],
        )?;
        Ok(())
    })?;
    let _ = data_dir; // 目录在 clone 阶段创建
    Ok(json!({"importId": id, "pinnedSha": sha, "status": "awaiting_probe_approval"}))
}

fn ls_remote_sha(repo_url: &str, ref_name: &str) -> Result<String, String> {
    // 完整 40-hex SHA：ls-remote 不按对象广告——直接接受为 pinned（存在性由受限
    // fetch 本身验证，fetch 失败即 import_sha_mismatch）。
    if ref_name.len() == 40 && ref_name.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(ref_name.to_string());
    }
    // 显式 ref（refs/heads/... 或分支/tag 名）→ 精确匹配，避免歧义解析。
    let out = {
        let mut cmd = Command::new("git");
        git_env(&mut cmd);
        let out = cmd
            .args(["ls-remote", repo_url, ref_name])
            .output()
            .map_err(|e| format!("git ls-remote 失败: {e}"))?;
        out
    };
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr)
            .trim()
            .chars()
            .take(200)
            .collect());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let mut it = line.split_whitespace();
        if let (Some(sha), Some(_r)) = (it.next(), it.next()) {
            return Ok(sha.to_string());
        }
    }
    Err("ref 未找到".into())
}

pub fn import_decide(
    store: &Store,
    data_dir: &Path,
    import_id: &str,
    decision: &str,
    decided_by: &str,
    reason: &str,
) -> Result<Value, Error> {
    if !enabled() {
        return Err(flag_off());
    }
    let now = timefmt::now();
    // 唯一权威推进入口：校验审批行一致后推进（不用通用 approval.decide）。
    let (status, approval_id, approval_status, approval_subject): (String, String, String, String) =
        store.with_conn(|c| {
            c.query_row(
                "SELECT i.status, a.id, a.status, a.subject_type
                 FROM mcp_repo_imports i JOIN approvals a ON a.subject_id = i.id
                 WHERE i.id=?1 AND a.subject_type IN ('mcp_import_probe','mcp_import_activate')
                 ORDER BY a.created_at DESC LIMIT 1",
                [import_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(|_| Error::Message("import_state_changed: import 或审批行缺失".into()))
        })?;
    if approval_status != "requested" {
        return Err(Error::Message("import_state_changed: 审批已决定".into()));
    }
    match (approval_subject.as_str(), decision) {
        ("mcp_import_probe", "approved") => {
            if status != "awaiting_probe_approval" {
                return Err(Error::Message(format!(
                    "import_state_changed: 当前状态 {status} 不可推进 probing"
                )));
            }
            store.with_tx_immediate(|tx| {
                tx.execute(
                    "UPDATE approvals SET status='approved', decided_by=?2, decided_at=?3, reason=?4 WHERE id=?1",
                    rusqlite::params![approval_id, decided_by, now, reason],
                )?;
                tx.execute(
                    "UPDATE mcp_repo_imports SET status='probing', updated_at=?2 WHERE id=?1 AND status='awaiting_probe_approval'",
                    rusqlite::params![import_id, now],
                )?;
                Ok(())
            })?;
            Ok(json!({"importId": import_id, "status": "probing"}))
        }
        ("mcp_import_activate", "approved") => {
            if status != "awaiting_activation" {
                return Err(Error::Message(format!(
                    "import_state_changed: 当前状态 {status} 不可激活"
                )));
            }
            // 事务内：建 mcp_servers 行 + 灌 mcp_server_tools + 回填 server_id → active。
            activate(store, data_dir, import_id, decided_by, &approval_id, &now)
        }
        (_, "rejected") => {
            store.with_tx_immediate(|tx| {
                tx.execute(
                    "UPDATE approvals SET status='rejected', decided_by=?2, decided_at=?3, reason=?4 WHERE id=?1",
                    rusqlite::params![approval_id, decided_by, now, reason],
                )?;
                tx.execute(
                    "UPDATE mcp_repo_imports SET status='failed', error=?2, updated_at=?3 WHERE id=?1",
                    rusqlite::params![import_id, format!("审批拒绝：{reason}"), now],
                )?;
                Ok(())
            })?;
            cleanup_import_dir(data_dir, import_id);
            Ok(json!({"importId": import_id, "status": "failed"}))
        }
        _ => Err(Error::Message(format!(
            "import_state_changed: decision={decision} 与审批类型 {approval_subject} 不匹配"
        ))),
    }
}

fn activate(
    store: &Store,
    data_dir: &Path,
    import_id: &str,
    decided_by: &str,
    approval_id: &str,
    now: &str,
) -> Result<Value, Error> {
    let (repo_url, entrypoint_json, candidates_raw): (String, String, String) = store
        .with_conn(|c| {
            c.query_row(
                "SELECT repo_url, COALESCE(entrypoint_json,''), COALESCE(schema_candidates_json,'[]')
                 FROM mcp_repo_imports WHERE id=?1",
                [import_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|_| Error::Message("import_state_changed: import 行缺失".into()))
        })?;
    let entry: Value = serde_json::from_str(&entrypoint_json)
        .map_err(|_| Error::Message("import_state_changed: entrypoint 未冻结".into()))?;
    let candidates: Vec<Value> = serde_json::from_str(&candidates_raw)
        .map_err(|_| Error::Message("import_state_changed: 候选损坏".into()))?;
    // server 名：import-<短 id>（注册表唯一、可读、可级联撤销）。
    let server_id = ids::new_id("mcp");
    let server_name = format!("import-{}", &import_id[..import_id.len().min(12)]);
    store.with_tx_immediate(|tx| {
        tx.execute(
            "INSERT INTO mcp_servers(id, name, transport, command, args_json, server_info, status,
                 approved_by, approved_at, created_at)
             VALUES (?1,?2,'stdio',?3,?4,'imported', 'active', ?5, ?5, ?5)",
            rusqlite::params![
                server_id,
                server_name,
                entry["command"].as_str().unwrap_or_default(),
                entry["args"].to_string(),
                now
            ],
        )?;
        for t in &candidates {
            tx.execute(
                "INSERT INTO mcp_server_tools(id, server_id, tool_name, description, schema_json, schema_digest,
                     read_only_hint, status, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,'active',?8)",
                rusqlite::params![
                    ids::new_id("mct"),
                    server_id,
                    t["name"].as_str().unwrap_or_default(),
                    t["description"].as_str().unwrap_or_default(),
                    t["inputSchema"].to_string(),
                    t["schemaDigest"].as_str().unwrap_or_default(),
                    t["readOnlyHint"].as_bool().unwrap_or(false) as i64,
                    now
                ],
            )?;
        }
        tx.execute(
            "UPDATE approvals SET status='approved', decided_by=?2, decided_at=?3 WHERE id=?1",
            rusqlite::params![approval_id, decided_by, now],
        )?;
        let n = tx.execute(
            "UPDATE mcp_repo_imports SET status='active', server_id=?2, updated_at=?3
             WHERE id=?1 AND status='awaiting_activation'",
            rusqlite::params![import_id, server_id, now],
        )?;
        if n != 1 {
            return Err(Error::Message("import_state_changed: CAS 未命中".into()));
        }
        Ok(())
    })?;
    let _ = (repo_url, data_dir);
    Ok(
        json!({"importId": import_id, "status": "active", "serverId": server_id, "serverName": server_name}),
    )
}

pub fn import_resume(store: &Store, data_dir: &Path, import_id: &str) -> Result<Value, Error> {
    if !enabled() {
        return Err(flag_off());
    }
    let now = timefmt::now();
    store.with_tx_immediate(|tx| {
        let (status, owner): (String, String) = tx
            .query_row(
                "SELECT status, COALESCE(worker_owner,'') FROM mcp_repo_imports WHERE id=?1",
                [import_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| Error::Message("import_state_changed: import 行缺失".into()))?;
        if status != "unknown" {
            return Err(Error::Message(format!("import_state_changed: 仅 unknown 可恢复（当前 {status}）")));
        }
        // 人工 Resume 语义：确认旧执行不再存活（owner 进程已死——本机单进程形态下，
        // owner 含进程 id+启动标识；存活检查按 /proc 或 pid 复用近似：租约必然已过期，
        // 进程 id 复用窗口由 attempt_no 单调递增兜底）后清 owner、重置 probing。
        if !owner.is_empty() && lease_alive(&owner) {
            return Err(Error::Message("import_state_changed: 旧 worker 可能仍存活（先等待租约过期并确认进程退出）".into()));
        }
        tx.execute(
            "UPDATE mcp_repo_imports SET status='probing', worker_owner='', worker_lease_expires_at=NULL,
                    error='', updated_at=?2 WHERE id=?1 AND status='unknown'",
            rusqlite::params![import_id, now],
        )?;
        Ok(())
    })?;
    let _ = data_dir;
    Ok(json!({"importId": import_id, "status": "probing"}))
}

fn lease_alive(owner: &str) -> bool {
    // owner = "<pid>:<bootNonce>:<n>";进程存活近似（/proc 或 sysinfo）。
    if let Some(pid) = owner.split(':').next().and_then(|p| p.parse::<i32>().ok()) {
        #[cfg(target_os = "macos")]
        {
            let out = std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status();
            return out.map(|s| s.success()).unwrap_or(false);
        }
        #[cfg(target_os = "linux")]
        {
            return std::path::Path::new(&format!("/proc/{pid}")).exists();
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = pid;
            return false;
        }
    }
    false
}

pub fn import_revoke(
    store: &Store,
    data_dir: &Path,
    import_id: &str,
    decided_by: &str,
    reason: &str,
) -> Result<Value, Error> {
    if !enabled() {
        return Err(flag_off());
    }
    let now = timefmt::now();
    let server_id: Option<String> = store.with_conn(|c| {
        Ok(c.query_row(
            "SELECT COALESCE(server_id,'') FROM mcp_repo_imports WHERE id=?1",
            [import_id],
            |r| r.get(0),
        )
        .ok())
    })?;
    store.with_tx_immediate(|tx| {
        let n = tx.execute(
            "UPDATE mcp_repo_imports SET status='revoked', error=?2, updated_at=?3 WHERE id=?1
               AND status NOT IN ('revoked')",
            rusqlite::params![import_id, format!("撤销：{reason}"), now],
        )?;
        if n != 1 {
            return Err(Error::Message(
                "import_state_changed: 不可达态或已撤销".into(),
            ));
        }
        if let Some(sid) = &server_id {
            // 已注册 server 级联撤销（工具 revoked，旧 Run 返 tool_revoked）。
            tx.execute("UPDATE mcp_servers SET status='revoked' WHERE id=?1", [sid])?;
            tx.execute(
                "UPDATE mcp_server_tools SET status='revoked' WHERE server_id=?1",
                [sid],
            )?;
        }
        let _ = sg_store::audit::append_at(
            tx,
            decided_by,
            "mcp.import.revoke",
            "mcp_repo_import",
            import_id,
            json!({"reason": reason}),
        )?;
        Ok(())
    })?;
    cleanup_import_dir(data_dir, import_id);
    Ok(json!({"importId": import_id, "status": "revoked"}))
}

pub fn import_list(store: &Store) -> Result<Value, Error> {
    if !enabled() {
        return Err(flag_off());
    }
    let items: Vec<Value> = store.with_conn(|c| {
        let mut stmt = c.prepare(
            "SELECT id, repo_url, ref_name, pinned_sha, status, COALESCE(error,''), created_at
             FROM mcp_repo_imports ORDER BY created_at DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(json!({
                    "importId": r.get::<_, String>(0)?,
                    "repoUrl": r.get::<_, String>(1)?,
                    "ref": r.get::<_, String>(2)?,
                    "pinnedSha": r.get::<_, String>(3)?,
                    "status": r.get::<_, String>(4)?,
                    "error": r.get::<_, String>(5)?,
                    "createdAt": r.get::<_, String>(6)?,
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    Ok(json!({"items": items}))
}

pub fn import_get(store: &Store, import_id: &str) -> Result<Value, Error> {
    if !enabled() {
        return Err(flag_off());
    }
    store.with_conn(|c| {
        c.query_row(
            "SELECT id, repo_url, ref_name, pinned_sha, manifest_digest, COALESCE(entrypoint_json,''),
                    COALESCE(schema_candidates_json,'[]'), COALESCE(content_freeze_json,'{}'),
                    COALESCE(progress_cursor,''), COALESCE(server_id,''), status, COALESCE(error,''), created_at
             FROM mcp_repo_imports WHERE id=?1",
            [import_id],
            |r| {
                Ok(json!({
                    "importId": r.get::<_, String>(0)?,
                    "repoUrl": r.get::<_, String>(1)?,
                    "ref": r.get::<_, String>(2)?,
                    "pinnedSha": r.get::<_, String>(3)?,
                    "manifestDigest": r.get::<_, String>(4)?,
                    "entrypoint": serde_json::from_str::<Value>(&r.get::<_, String>(5)?).unwrap_or(Value::Null),
                    "schemaCandidates": serde_json::from_str::<Value>(&r.get::<_, String>(6)?).unwrap_or(Value::Null),
                    "contentFreeze": serde_json::from_str::<Value>(&r.get::<_, String>(7)?).unwrap_or(Value::Null),
                    "progressCursor": r.get::<_, String>(8)?,
                    "serverId": r.get::<_, String>(9)?,
                    "status": r.get::<_, String>(10)?,
                    "error": r.get::<_, String>(11)?,
                    "createdAt": r.get::<_, String>(12)?,
                }))
            },
        )
        .map_err(|_| Error::Message("import_state_changed: import 行缺失".into()))
    })
}

// ---------------------------------------------------------------------------
// worker：durable claim（CAS）→ clone → freeze → probe → candidates → activation 审批
// ---------------------------------------------------------------------------

fn worker_owner() -> String {
    format!("{}:{}", std::process::id(), ids::new_id("boot"))
}

/// tick：扫描 probing 候选，CAS 认领后推进（幂等：cursor 每步单事务写）。
/// 返回本 tick 处理的行数。挂 main.rs（每 5s）。
pub fn tick_probing(store: &Store, data_dir: &Path) -> Result<usize, Error> {
    if !enabled() {
        return Ok(0);
    }
    // 过期租约 reconciliation：probing 且 owner 非空且租约已过 → unknown（不自动双跑）。
    reconcile_expired_leases(store)?;
    let candidates: Vec<String> = store.with_conn(|c| {
        let mut stmt = c.prepare(
            "SELECT id FROM mcp_repo_imports WHERE status='probing' AND worker_owner=''",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })?;
    let mut processed = 0;
    for id in candidates {
        let owner = worker_owner();
        let claimed = store.with_conn(|c| {
            c.execute(
                "UPDATE mcp_repo_imports SET worker_owner=?2, worker_lease_expires_at=?3,
                        worker_attempt_no=worker_attempt_no+1, updated_at=?3
                 WHERE id=?1 AND status='probing' AND worker_owner=''",
                rusqlite::params![id, owner, timefmt::now_plus_minutes(WORKER_LEASE_SECS / 60)],
            )?;
            Ok(c.changes() == 1)
        })?;
        if !claimed {
            continue; // 并发认领只一方命中
        }
        processed += 1;
        let outcome = run_probing(store, data_dir, &id, &owner);
        if let Err(e) = outcome {
            mark_failed(store, &id, &e.to_string());
        }
    }
    Ok(processed)
}

fn reconcile_expired_leases(store: &Store) -> Result<(), Error> {
    let now = timefmt::now();
    store.with_conn(|c| {
        c.execute(
            "UPDATE mcp_repo_imports SET status='unknown',
                    error='worker 租约丢失（崩溃/超时）——人工 importResume 恢复', updated_at=?1
             WHERE status='probing' AND worker_owner<>'' AND worker_lease_expires_at IS NOT NULL
               AND worker_lease_expires_at < ?1",
            [now],
        )?;
        Ok(())
    })
}

fn mark_failed(store: &Store, import_id: &str, error: &str) {
    let _ = store.with_conn(|c| {
        c.execute(
            "UPDATE mcp_repo_imports SET status='failed', error=?2, updated_at=?3 WHERE id=?1",
            rusqlite::params![import_id, error, timefmt::now()],
        )?;
        Ok(())
    });
}

fn set_cursor(store: &Store, import_id: &str, cursor: &str) -> Result<(), Error> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE mcp_repo_imports SET progress_cursor=?2, worker_lease_expires_at=?3, updated_at=?3
             WHERE id=?1",
            rusqlite::params![import_id, cursor, timefmt::now_plus_minutes(WORKER_LEASE_SECS / 60)],
        )?;
        Ok(())
    })
}

/// probing 推进：clone（幂等，cursor=cloned 跳过）→ freeze → 沙箱探针 → 候选落库 →
/// awaiting_activation + activation 审批。
fn run_probing(store: &Store, data_dir: &Path, import_id: &str, owner: &str) -> Result<(), Error> {
    let (repo_url, pinned_sha, cursor): (String, String, String) = store.with_conn(|c| {
        c.query_row(
            "SELECT repo_url, pinned_sha, COALESCE(progress_cursor,'') FROM mcp_repo_imports WHERE id=?1",
            [import_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|_| Error::Message("import 行缺失".into()))
    })?;
    let checkout = checkout_dir(data_dir, import_id);
    if cursor != "cloned" {
        std::fs::create_dir_all(&checkout).map_err(|e| Error::Message(e.to_string()))?;
        std::fs::create_dir_all(checkout.parent().unwrap())
            .map_err(|e| Error::Message(e.to_string()))?;
        std::fs::create_dir_all(state_dir(data_dir, import_id))
            .map_err(|e| Error::Message(e.to_string()))?;
        std::fs::create_dir_all(temp_dir(data_dir, import_id))
            .map_err(|e| Error::Message(e.to_string()))?;
        // 空仓库 + 受限 fetch：不 clone（clone 会抓默认分支耗盘）——init → fetch 固定 SHA。
        git_run(Some(&checkout), &["init", "-q"], 30).map_err(Error::Message)?;
        git_run(Some(&checkout), &["remote", "add", "origin", &repo_url], 30)
            .map_err(Error::Message)?;
        git_run(
            Some(&checkout),
            &["fetch", "--depth", "1", "--no-tags", "origin", &pinned_sha],
            FETCH_TIMEOUT_SECS,
        )
        .map_err(|e| Error::Message(format!("import_sha_mismatch: {e}")))?;
        git_run(
            Some(&checkout),
            &["checkout", "-q", "--detach", &pinned_sha],
            60,
        )
        .map_err(Error::Message)?;
        let head = git_run(Some(&checkout), &["rev-parse", "HEAD"], 30).map_err(Error::Message)?;
        if head != pinned_sha {
            return Err(Error::Message(format!(
                "import_sha_mismatch: HEAD {head} != {pinned_sha}"
            )));
        }
        // 不递归子模块；检出含 .gitmodules 即拒绝（未验证面不执行）。
        if checkout.join(".gitmodules").exists() {
            return Err(Error::Message(
                "import_manifest_invalid: 检出包含 .gitmodules（子模块未验证，拒绝）".into(),
            ));
        }
        // 后置校验：体积/文件数/symlink 出逃（collect_files 内拒绝）。
        if dir_size(&checkout) > MAX_CHECKOUT_BYTES {
            return Err(Error::Message(format!(
                "import_size_exceeded: >{MAX_CHECKOUT_BYTES}B"
            )));
        }
        let mut files = Vec::new();
        collect_files(&checkout, &mut files, 0).map_err(Error::Message)?;
        if files.len() > MAX_FILES {
            return Err(Error::Message(format!(
                "import_size_exceeded: >{MAX_FILES} 文件"
            )));
        }
        set_cursor(store, import_id, "cloned")?;
    }
    // manifest 解析 + 冻结五元组。
    let raw = std::fs::read_to_string(checkout.join(MANIFEST_FILE)).map_err(|e| {
        Error::Message(format!(
            "import_manifest_invalid: {MANIFEST_FILE} 不可读: {e}"
        ))
    })?;
    let mdig = manifest_digest(raw.trim());
    let manifest = parse_manifest(raw.trim(), &checkout).map_err(Error::Message)?;
    let freeze = compute_freeze(&checkout, &mdig, &manifest.entrypoint_command, import_id)
        .map_err(Error::Message)?;
    // 源 checkout 转只读（防批准后本地篡改；可写面只能落 state/tmp）。
    set_readonly_recursive(&checkout)?;
    // 沙箱探针：读面=checkout+命令目录；写面=state/tmp（manifest writableDirs 落 state 下）。
    let entry_abs = checkout.join(&manifest.entrypoint_command);
    let entry_dir = entry_abs
        .parent()
        .and_then(|p| p.canonicalize().ok())
        .unwrap_or_default();
    let _ = entry_dir;
    // FS 只读语义（§2 WP-4）：读全放行（"/"），写仅 state/tmp 白名单，网络硬禁——
    // deny-default 下的读白名单会掐死解释器运行时（python abort），且源 checkout
    // 已 OS 级只读，读面不受限不构成篡改面；全部字段仍进 policy digest。
    let mut policy = sg_sandbox::SandboxPolicy {
        read_paths: vec!["/".to_string()],
        write_paths: vec![],
        network_off: true,
    };
    let _ = checkout.canonicalize();
    for w in &manifest.writable_dirs {
        policy.write_paths.push(
            state_dir(data_dir, import_id)
                .join(w)
                .to_string_lossy()
                .to_string(),
        );
    }
    policy
        .write_paths
        .push(temp_dir(data_dir, import_id).to_string_lossy().to_string());
    // execvp 语义：无斜杠的命令只搜 PATH 不看 cwd——相对入口显式加 ./ 前缀。
    let entry_cmd = if manifest.entrypoint_command.contains('/') {
        manifest.entrypoint_command.clone()
    } else {
        format!("./{}", manifest.entrypoint_command)
    };
    let probe = {
        let mut client = sg_integrations::mcp::McpClient::new(
            sg_integrations::mcp::SandboxedTransport::spawn_in(
                &policy,
                Some(&checkout),
                &entry_cmd,
                &manifest.entrypoint_args,
            )
            .map_err(Error::Message)?,
        );
        let result = (|| -> Result<(String, Vec<Value>), String> {
            let info = client.initialize().map_err(|e| e.to_string())?;
            let tools = client.list_tools().map_err(|e| e.to_string())?;
            client.shutdown();
            Ok((
                json!({"name": info.name, "version": info.version, "protocolVersion": info.protocol_version}).to_string(),
                tools
                    .iter()
                    .map(|t| {
                        let (digest, _canonical) = sg_integrations::mcp::canonical_schema(&t.input_schema)
                            .unwrap_or_default();
                        json!({
                            "name": t.name, "description": t.description,
                            "inputSchema": t.input_schema,
                            "readOnlyHint": t.read_only_hint,
                            "schemaDigest": digest,
                        })
                    })
                    .collect(),
            ))
        })();
        result
    };
    let (server_info, tools) = probe.map_err(Error::Message)?;
    if tools.len() > 64 {
        return Err(Error::Message("import_manifest_invalid: 工具数 >64".into()));
    }
    // 候选 + 冻结落库 + awaiting_activation + activation 审批（单事务）。
    let now = timefmt::now();
    store.with_tx_immediate(|tx| {
        tx.execute(
            "UPDATE mcp_repo_imports SET entrypoint_json=?2, schema_candidates_json=?3,
                    content_freeze_json=?4, manifest_digest=?5, status='schema_candidate', updated_at=?6
             WHERE id=?1 AND status='probing'",
            rusqlite::params![
                import_id,
                json!({"command": manifest.entrypoint_command, "args": manifest.entrypoint_args,
                        "serverInfo": server_info, "repoUrl": repo_url, "pinnedSha": pinned_sha}).to_string(),
                serde_json::to_string(&tools).unwrap_or_default(),
                freeze.to_string(),
                mdig,
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO approvals(id, subject_type, subject_id, action_digest, risk, status,
                 expires_at, reason, created_at)
             VALUES (?1,'mcp_import_activate',?2,?3,'high','requested',?4,?5,?6)",
            rusqlite::params![
                ids::new_id("apr"),
                import_id,
                format!("activate|{repo_url}|{pinned_sha}|{mdig}"),
                timefmt::now_plus_days(7),
                format!("Git 仓库导入激活：{repo_url}@{pinned_sha}（{} 工具）", tools.len()),
                now
            ],
        )?;
        // probing → schema_candidate → awaiting_activation 一气推进（同一事务）。
        tx.execute(
            "UPDATE mcp_repo_imports SET status='awaiting_activation', updated_at=?2 WHERE id=?1",
            rusqlite::params![import_id, now],
        )?;
        let _ = owner;
        Ok(())
    })?;
    Ok(())
}

fn set_readonly_recursive(root: &Path) -> Result<(), Error> {
    fn walk(p: &Path) -> Result<(), Error> {
        let meta = std::fs::symlink_metadata(p).map_err(|e| Error::Message(e.to_string()))?;
        if meta.is_dir() {
            for e in std::fs::read_dir(p)
                .map_err(|e| Error::Message(e.to_string()))?
                .flatten()
            {
                walk(&e.path())?;
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(p)
                .map_err(|e| Error::Message(e.to_string()))?
                .permissions();
            perms.set_mode(perms.mode() & !0o222);
            std::fs::set_permissions(p, perms).map_err(|e| Error::Message(e.to_string()))?;
        }
        Ok(())
    }
    walk(root)
}

/// 启动扫描：临时目录清理（崩溃残留）+ 过期租约收敛。
pub fn startup_reconcile(store: &Store, data_dir: &Path) -> Result<(), Error> {
    if !enabled() {
        return Ok(());
    }
    reconcile_expired_leases(store)?;
    // 清理无对应 import 行的孤儿目录。
    if let Ok(entries) = std::fs::read_dir(data_dir.join(IMPORT_DIR)) {
        for e in entries.flatten() {
            let id = e.file_name().to_string_lossy().to_string();
            let exists: bool = store
                .with_conn(|c| {
                    Ok(c.query_row(
                        "SELECT COUNT(*) FROM mcp_repo_imports WHERE id=?1",
                        [&id],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap_or(0)
                        > 0)
                })
                .unwrap_or(true);
            if !exists {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "sg-mcpimp-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (Store::open(&dir, "test").unwrap(), dir)
    }

    fn checkout_with(root: &Path, files: &[(&str, &str)]) -> PathBuf {
        let c = root.join("checkout");
        std::fs::create_dir_all(&c).unwrap();
        for (name, content) in files {
            let p = c.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        c
    }

    const VALID: &str = r#"{"schemaVersion":1,"entrypoint":{"command":"server.py","args":["--mode","ok"]},"sandbox":{"network":false,"writableDirs":[]},"timeouts":{"probeSec":5,"callSec":10},"deps":{"manager":"none"}}"#;

    #[test]
    fn manifest_validation_negative_matrix() {
        let (store, dir) = setup();
        let c = checkout_with(&dir, &[("server.py", "#!/usr/bin/env python3\n")]);
        let cases: Vec<(&str, &str)> = vec![
            (
                r#"{"schemaVersion":2,"entrypoint":{"command":"server.py"}}"#,
                "高版本",
            ),
            (
                r#"{"schemaVersion":1,"extra":1,"entrypoint":{"command":"server.py"}}"#,
                "未知字段",
            ),
            (
                r#"{"schemaVersion":1,"entrypoint":{"command":"server.py"},"deps":{"manager":"npm"}}"#,
                "npm deps",
            ),
            (
                r#"{"schemaVersion":1,"entrypoint":{"command":"/usr/bin/python3"}}"#,
                "绝对路径 command",
            ),
            (
                r#"{"schemaVersion":1,"entrypoint":{"command":"../esc.py"}}"#,
                "上跳路径",
            ),
            (
                r#"{"schemaVersion":1,"entrypoint":{"command":"server.py"},"sandbox":{"network":true}}"#,
                "network=true",
            ),
            (
                r#"{"schemaVersion":1,"entrypoint":{"command":"server.py"},"sandbox":{"network":false,"writableDirs":["data"]}}"#,
                "writableDirs 指向 checkout",
            ),
        ];
        for (raw, label) in &cases {
            let err = parse_manifest(raw, &c).unwrap_err();
            assert!(
                err.starts_with("import_manifest_invalid")
                    || err.starts_with("import_deps_unsupported"),
                "{label}: {err}"
            );
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("server.py", c.join("link.py")).unwrap();
            let err = parse_manifest(
                r#"{"schemaVersion":1,"entrypoint":{"command":"link.py"},"sandbox":{"network":false}}"#,
                &c,
            )
            .unwrap_err();
            assert!(err.contains("symlink"), "{err}");
        }
        parse_manifest(VALID, &c).unwrap();
        drop(store);
    }

    #[test]
    fn worker_claim_cas_and_lease_convergence() {
        std::env::set_var(FLAG, "1"); // 域函数受 flag 门控（测试内显式开启）
        let (store, dir) = setup();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO mcp_repo_imports(id, repo_url, ref_name, pinned_sha, manifest_digest,
                         status, created_by, created_at, updated_at)
                     VALUES ('imp1','file:///x','main','aa','<pending>','probing','t','t','t')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let claim = |owner: &str| -> bool {
            store
                .with_conn(|c| {
                    c.execute(
                        "UPDATE mcp_repo_imports SET worker_owner=?1, worker_lease_expires_at=?2,
                                worker_attempt_no=worker_attempt_no+1
                         WHERE id='imp1' AND status='probing' AND worker_owner=''",
                        rusqlite::params![owner, timefmt::now_plus_minutes(5)],
                    )?;
                    Ok(c.changes() == 1)
                })
                .unwrap()
        };
        let c1 = claim(&worker_owner());
        let c2 = claim(&worker_owner());
        assert!(c1 ^ c2, "并发认领只一方命中");
        assert_eq!(tick_probing(&store, &dir).unwrap(), 0, "已被认领不重入");
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE mcp_repo_imports SET worker_lease_expires_at='2000-01-01T00:00:00.000Z' WHERE id='imp1'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        reconcile_expired_leases(&store).unwrap();
        let status: String = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT status FROM mcp_repo_imports WHERE id='imp1'",
                    [],
                    |r| r.get(0),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(status, "unknown", "租约丢失收敛 unknown");
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE mcp_repo_imports SET worker_owner='999999999:x:1' WHERE id='imp1'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        import_resume(&store, &dir, "imp1").unwrap();
        let (status, owner): (String, String) = store
            .with_conn(|c| {
                Ok(c
                    .query_row(
                        "SELECT status, COALESCE(worker_owner,'') FROM mcp_repo_imports WHERE id='imp1'",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap())
            })
            .unwrap();
        assert_eq!(
            (status.as_str(), owner.as_str()),
            ("probing", ""),
            "Resume 重置 probing"
        );
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE mcp_repo_imports SET status='unknown', worker_owner=?1 WHERE id='imp1'",
                    rusqlite::params![format!("{}:x:1", std::process::id())],
                )?;
                Ok(())
            })
            .unwrap();
        let err = import_resume(&store, &dir, "imp1").unwrap_err();
        assert!(err.to_string().contains("存活"), "{err}");
        std::env::remove_var(FLAG);
    }

    #[test]
    fn merkle_covers_non_entrypoint_files() {
        let (store, dir) = setup();
        let c = checkout_with(
            &dir,
            &[
                ("server.py", "print('entry')"),
                ("lib/helper.py", "DATA = 1"),
            ],
        );
        let m = checkout_merkle(&c).unwrap();
        std::fs::write(c.join("lib/helper.py"), "DATA = 2  # tampered").unwrap();
        let m2 = checkout_merkle(&c).unwrap();
        assert_ne!(m, m2, "改依赖模块必须改变 Merkle");
        drop(store);
    }
}
