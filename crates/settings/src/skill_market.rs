//! 技能市场源（可配置），两种类型：
//! - `zcode_local`：本机插件市场目录（known_marketplaces.json + installed_plugins.json + cache/）；
//! - `remote_git`：远程 https 市场仓库（marketplace.json 清单或纯技能仓库）。
//!
//! 浏览/导入只读 SKILL.md 文本，绝不执行市场内任何文件（M6 退出标准，同 skill_registry）；
//! 远程导入按 catalog pin SHA/ref 拉取插件仓库，仅提取 SKILL.md；
//! 导入正文经 objects put 秘密扫描（fail-closed），落 enabled 技能 + draft 版本，审计留痕。

use serde::Serialize;

use crate::{store_err, SettingsError, SettingsResult};
use sg_store::{ids, timefmt, Store};

const BODY_MAX_BYTES: usize = 512 << 10;
const DEFAULT_ROOT: &str = "~/.zcode/cli/plugins";

// ---------------- 源 CRUD ----------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketSource {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub root_path: String,
    pub marketplace_id: String,
    pub enabled: bool,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

fn row_source(r: &rusqlite::Row<'_>) -> rusqlite::Result<MarketSource> {
    Ok(MarketSource {
        id: r.get(0)?,
        name: r.get(1)?,
        kind: r.get(2)?,
        root_path: r.get(3)?,
        marketplace_id: r.get(4)?,
        enabled: r.get::<_, i64>(5)? != 0,
        revision: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

const SOURCE_COLUMNS: &str =
    "id, name, kind, root_path, marketplace_id, enabled, revision, created_at, updated_at";

fn invalid(msg: &str) -> SettingsError {
    SettingsError::new("INVALID_PARAMS", msg)
}

/// 展开 `~/` 前缀（HOME 缺失时原样返回，由后续目录校验兜底）。
fn expand_root(path: &str) -> std::path::PathBuf {
    let p = path.trim();
    if p == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return home.into();
        }
    } else if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return std::path::Path::new(&home).join(rest);
        }
    }
    p.into()
}

/// 根目录必须存在且具备插件市场布局特征（三者其一），防误指向任意目录。
fn validate_plugins_root(root_path: &str) -> SettingsResult<()> {
    let root = expand_root(root_path);
    if !root.is_dir() {
        return Err(invalid(&format!("市场源目录不存在：{}", root.display())));
    }
    let shaped = root.join("marketplaces").is_dir()
        || root.join("installed_plugins.json").is_file()
        || root.join("cache").is_dir();
    if !shaped {
        return Err(invalid(
            "不是插件市场目录（缺少 marketplaces/、installed_plugins.json 或 cache/）",
        ));
    }
    Ok(())
}

fn local_git_allowed() -> bool {
    // 气隙环境/测试夹具：允许本地 bare 仓库路径作为 git 远端（生产仅 https）。
    std::env::var("RATIFLOW_SKILL_MARKET_LOCAL_GIT")
        .ok()
        .as_deref()
        == Some("1")
}

fn is_allowed_git_remote(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    local_git_allowed() && url.starts_with('/')
}

fn validate_remote_git(url: &str) -> SettingsResult<()> {
    if !is_allowed_git_remote(url) {
        return Err(invalid("远程市场仓库仅支持 https:// URL"));
    }
    if local_git_allowed() {
        return Ok(());
    }
    // 可达性预检：ls-remote 快速失败，避免保存一个拉不动的仓库地址。
    match std::process::Command::new("git")
        .args(["ls-remote", "--heads", url])
        .output()
    {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(invalid(&format!(
            "市场仓库不可达：{}",
            String::from_utf8_lossy(&o.stderr).trim()
        ))),
        Err(e) => Err(invalid(&format!("git 不可用：{e}"))),
    }
}

/// 按源类型做保存校验。
fn validate_by_kind(kind: &str, root_path: &str) -> SettingsResult<()> {
    match kind {
        "zcode_local" => validate_plugins_root(root_path),
        "remote_git" => validate_remote_git(root_path),
        _ => Err(invalid("非法市场源类型（zcode_local / remote_git）")),
    }
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

/// 克隆结果；Drop 时清理临时目录（正文必须在 guard 存活期内读取）。
struct ClonedRepo {
    dir: std::path::PathBuf,
    head_sha: String,
}

impl Drop for ClonedRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// https-only 克隆（测试可放宽本地路径）。pin 规则：
/// sha 优先（全量克隆 + checkout 校验，可重现）→ 否则 ref（浅克隆分支/标签）→ 否则 HEAD 浅克隆。
fn git_clone_pin(url: &str, ref_: Option<&str>, sha: Option<&str>) -> Result<ClonedRepo, String> {
    let dir = std::env::temp_dir().join(format!(
        "sg-mkt-clone-{}-{}",
        std::process::id(),
        ids::new_id("t")
    ));
    let mut cmd = std::process::Command::new("git");
    cmd.args(["clone", "--quiet"]);
    if sha.is_none() {
        cmd.args(["--depth", "1"]);
    }
    if let Some(r) = ref_ {
        cmd.args(["--branch", r]);
    }
    cmd.arg(url).arg(dir.to_string_lossy().as_ref());
    let out = cmd.output().map_err(|e| format!("git 不可用：{e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(format!(
            "clone 失败：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let mut head =
        git_out(&dir, &["rev-parse", "HEAD"]).ok_or_else(|| "无法解析 HEAD".to_string())?;
    if let Some(sha) = sha {
        let co = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["checkout", "--quiet", sha])
            .output()
            .map_err(|e| format!("git 不可用：{e}"))?;
        if !co.status.success() {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(format!(
                "pin SHA 不可达：{}",
                String::from_utf8_lossy(&co.stderr).trim()
            ));
        }
        head = sha.to_string();
    }
    Ok(ClonedRepo {
        dir,
        head_sha: head,
    })
}

/// 市场清单条目里可拉取的插件 git 来源。支持四种形态：
/// - 字符串 "./plugins/x"：插件在市场仓库内（url 为空 = 用市场仓库本身，pin 市场 HEAD）；
/// - {"source":"git-subdir"|"git","url","path","ref","sha"}：外链 git 仓库；
/// - {"source":"github","repo"}：GitHub 仓库；
/// - {"source":"url","url"(,"path","ref","sha")}：直接给 https git 地址（.zip 打包形态不支持，跳过）。
struct PluginGitSource {
    url: String,
    path: String,
    ref_: Option<String>,
    sha: Option<String>,
}

fn plugin_git_source(entry: &serde_json::Value) -> Option<PluginGitSource> {
    let valid_sha = |s: &str| s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit());
    // 字符串形态：市场仓库内相对路径。
    if let Some(rel) = entry.get("source").and_then(|v| v.as_str()) {
        let p = rel.trim().trim_start_matches("./");
        if p.is_empty() {
            return None;
        }
        return Some(PluginGitSource {
            url: String::new(),
            path: p.trim_matches('/').to_string(),
            ref_: None,
            sha: None,
        });
    }
    let src = entry.get("source")?;
    let kind = src.get("source").and_then(|v| v.as_str()).unwrap_or("");
    let (url, path) = match kind {
        "git-subdir" | "git" | "url" => {
            let url = src.get("url").and_then(|v| v.as_str())?.to_string();
            let local_ok = local_git_allowed() && url.starts_with('/');
            if !url.starts_with("https://") && !local_ok {
                return None;
            }
            if url.starts_with("https://") && url.ends_with(".zip") {
                return None; // zip 打包形态 v1 不支持（无解包面）。
            }
            (
                url,
                src.get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            )
        }
        "github" => (
            format!(
                "https://github.com/{}.git",
                src.get("repo").and_then(|v| v.as_str())?
            ),
            String::new(),
        ),
        _ => return None,
    };
    Some(PluginGitSource {
        path: path.trim_matches('/').to_string(),
        ref_: src.get("ref").and_then(|v| v.as_str()).map(str::to_string),
        sha: src
            .get("sha")
            .and_then(|v| v.as_str())
            .filter(|s| valid_sha(s))
            .map(str::to_string),
        url,
    })
}

/// 读取市场仓库清单（.claude-plugin/marketplace.json 或 marketplace.json），
/// 返回（市场名, 可拉取插件条目）。无清单文件 → None（按纯技能仓库处理）。
fn read_catalog(
    repo_dir: &std::path::Path,
) -> Option<(String, Vec<(MarketPluginEntry, PluginGitSource)>)> {
    for rel in [".claude-plugin/marketplace.json", "marketplace.json"] {
        let Ok(data) = std::fs::read_to_string(repo_dir.join(rel)) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) else {
            continue;
        };
        let Some(arr) = v.get("plugins").and_then(|p| p.as_array()) else {
            continue;
        };
        let mut out = Vec::new();
        for p in arr {
            let Some(name) = p.get("name").and_then(|x| x.as_str()) else {
                continue;
            };
            let Some(gs) = plugin_git_source(p) else {
                continue;
            };
            out.push((
                MarketPluginEntry {
                    name: name.to_string(),
                    description: p
                        .get("description")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .chars()
                        .take(200)
                        .collect(),
                    version: p
                        .get("version")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    category: p
                        .get("category")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                },
                gs,
            ));
        }
        let mkt_name = v
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        return Some((mkt_name, out));
    }
    None
}

fn url_tail(url: &str) -> String {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(url)
        .trim_end_matches(".git")
        .to_string()
}

/// 遍历仓库内全部 **/SKILL.md（symlink 跳过、.git 跳过、根内 canonical 复核），
/// 返回条目 + 实际文件路径（导入按 dir_name 命中后读取）。
fn walk_skill_entries(
    root: &std::path::Path,
    dir: &std::path::Path,
    version: &str,
    out: &mut Vec<(MarketSkillEntry, std::path::PathBuf)>,
) -> Result<(), String> {
    if dir.components().count() - root.components().count() > 8 {
        return Ok(());
    }
    let root_canonical = root.canonicalize().map_err(|e| e.to_string())?;
    let entries = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        if ft.is_dir() {
            if path.file_name().map(|n| n == ".git").unwrap_or(false) {
                continue;
            }
            walk_skill_entries(root, &path, version, out)?;
        } else if path.file_name().map(|n| n == "SKILL.md").unwrap_or(false) {
            let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
            if !canonical.starts_with(&root_canonical) {
                continue;
            }
            let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
            if meta.len() as usize > BODY_MAX_BYTES {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&path) else {
                continue;
            };
            let dir_name = path
                .parent()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "skill".into());
            out.push((
                MarketSkillEntry {
                    name: sanitize_name(&dir_name),
                    dir_name,
                    description: parse_description(&body),
                    plugin: String::new(),
                    version: version.to_string(),
                },
                path,
            ));
        }
    }
    Ok(())
}

fn sort_entries(entries: &mut [MarketSkillEntry]) {
    entries.sort_by(|a, b| {
        (a.plugin.clone(), a.name.clone()).cmp(&(b.plugin.clone(), b.name.clone()))
    });
}

fn insert_row(
    store: &Store,
    kind: &str,
    name: &str,
    root_path: &str,
    marketplace_id: &str,
    enabled: bool,
) -> SettingsResult<MarketSource> {
    let id = ids::new_id("mkt");
    let now = timefmt::now();
    store
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO skill_market_sources(id, name, kind, root_path, marketplace_id, enabled, revision, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,1,?7,?7)",
                rusqlite::params![id, name, kind, root_path, marketplace_id, enabled as i64, now],
            )
            .map_err(sg_store::Error::from)
        })
        .map_err(store_err)?;
    source_get(store, &id)
}

fn source_get(store: &Store, id: &str) -> SettingsResult<MarketSource> {
    store
        .with_conn(|conn| {
            conn.query_row(
                &format!("SELECT {SOURCE_COLUMNS} FROM skill_market_sources WHERE id=?1"),
                [id],
                row_source,
            )
            .map_err(|_| sg_store::Error::Message(format!("市场源 {id} 不存在")))
        })
        .map_err(store_err)
}

/// 首次访问惰性播种默认两个源（对应 ZCode 市场源面板的两个官方市场，均可编辑/删除）。
/// 播种绕过目录校验：机器上没有该目录时 browse 会按源报告错误，不影响列表。
fn ensure_seeded(store: &Store) -> SettingsResult<()> {
    let n: i64 = store
        .with_conn(|conn| {
            Ok(conn
                .query_row("SELECT COUNT(*) FROM skill_market_sources", [], |r| {
                    r.get(0)
                })
                .unwrap_or(0))
        })
        .map_err(store_err)?;
    if n > 0 {
        return Ok(());
    }
    for (name, mkt) in [
        ("zcode-plugins-official", "zcode-plugins-official"),
        ("Claude Code 插件", "claude-plugins-official"),
    ] {
        insert_row(store, "zcode_local", name, DEFAULT_ROOT, mkt, true)?;
    }
    Ok(())
}

pub fn source_list(store: &Store) -> SettingsResult<Vec<MarketSource>> {
    ensure_seeded(store)?;
    store
        .with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SOURCE_COLUMNS} FROM skill_market_sources ORDER BY created_at, rowid"
            ))?;
            let rows = stmt.query_map([], row_source)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .map_err(store_err)
}

/// 新增（sourceId=None）或编辑（CAS）。rootPath 每次都按类型校验。
#[allow(clippy::too_many_arguments)]
pub fn source_save(
    store: &Store,
    source_id: Option<&str>,
    kind: &str,
    name: &str,
    root_path: &str,
    marketplace_id: &str,
    enabled: Option<bool>,
    expected_revision: Option<i64>,
) -> SettingsResult<MarketSource> {
    let kind = kind.trim();
    if kind.is_empty() {
        return Err(invalid("市场源类型不能为空"));
    }
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return Err(invalid("市场源名称不能为空且不超过 120 字符"));
    }
    let root_path = root_path.trim();
    if root_path.is_empty() {
        return Err(invalid("市场源根目录/仓库地址不能为空"));
    }
    validate_by_kind(kind, root_path)?;
    let marketplace_id = marketplace_id.trim();
    let enabled = enabled.unwrap_or(true);
    match source_id {
        None => insert_row(store, kind, name, root_path, marketplace_id, enabled),
        Some(id) => {
            source_get(store, id)?; // 不存在时报明确错误（区分于 revision 冲突）。
            let expected = expected_revision
                .ok_or_else(|| invalid("编辑市场源必须提供 expectedRevision（CAS）"))?;
            let updated = store
                .with_conn(|conn| {
                    conn.execute(
                        "UPDATE skill_market_sources SET kind=?2, name=?3, root_path=?4, marketplace_id=?5, enabled=?6,
                            revision=revision+1, updated_at=?7
                         WHERE id=?1 AND revision=?8",
                        rusqlite::params![
                            id,
                            kind,
                            name,
                            root_path,
                            marketplace_id,
                            enabled as i64,
                            timefmt::now(),
                            expected
                        ],
                    )
                    .map_err(sg_store::Error::from)
                })
                .map_err(store_err)?;
            if updated == 0 {
                return Err(invalid("revision 冲突：市场源已被其他人修改"));
            }
            source_get(store, id)
        }
    }
}

pub fn source_remove(store: &Store, source_id: &str, expected_revision: i64) -> SettingsResult<()> {
    let deleted = store
        .with_conn(|conn| {
            conn.execute(
                "DELETE FROM skill_market_sources WHERE id=?1 AND revision=?2",
                rusqlite::params![source_id, expected_revision],
            )
            .map_err(sg_store::Error::from)
        })
        .map_err(store_err)?;
    if deleted == 0 {
        return Err(invalid("市场源不存在或 revision 冲突"));
    }
    Ok(())
}

// ---------------- 浏览 ----------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketSkillEntry {
    /// Ratiflow 技能名（目录名净化，导入即用此名；同名幂等）。
    pub name: String,
    /// 市场内原始技能目录名（导入定位用）。
    pub dir_name: String,
    pub description: String,
    pub plugin: String,
    pub version: String,
}

/// 远程清单插件条目（技能按需拉取：marketPluginSkills）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketPluginEntry {
    pub name: String,
    pub description: String,
    pub version: String,
    pub category: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketBrowse {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub root_path: String,
    pub marketplace_id: String,
    pub enabled: bool,
    pub revision: i64,
    /// 展开后的根目录/仓库地址（展示用）。
    pub resolved_root: String,
    pub marketplace_name: String,
    pub description: String,
    pub plugin_count: i64,
    /// zcode_local / 纯技能仓库：可直接导入的技能。
    pub skills: Vec<MarketSkillEntry>,
    /// remote_git 且有 marketplace.json 清单：插件目录（技能按需拉取）。
    pub plugins: Vec<MarketPluginEntry>,
    /// 非空 = 该源浏览失败原因（单源故障不拖垮整页）。
    pub error: String,
}

/// 已安装插件条目（installed_plugins.json → plugins[]）。
struct InstalledPlugin {
    name: String,
    version: String,
    install_path: std::path::PathBuf,
}

/// 净化为 Ratiflow 技能名（[A-Za-z0-9_-]，与 skills_ext::validate_name 一致）。
fn sanitize_name(dir: &str) -> String {
    let cleaned: String = dir
        .chars()
        .take(64)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "market-skill".into()
    } else {
        cleaned
    }
}

/// frontmatter description 优先（去引号）；否则首个非空段落（跳过标题行）。≤200 字符。
fn parse_description(md: &str) -> String {
    let take = |s: &str| s.chars().take(200).collect::<String>();
    if let Some(rest) = md.strip_prefix("---\n") {
        if let Some(idx) = rest.find("\n---") {
            for line in rest[..idx].lines() {
                if let Some(v) = line.strip_prefix("description:") {
                    let v = v.trim().trim_matches('"').trim_matches('\'').trim();
                    if !v.is_empty() {
                        return take(v);
                    }
                }
            }
        }
    }
    for para in md.split("\n\n") {
        let t = para
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string();
        if !t.is_empty() {
            return take(&t);
        }
    }
    String::new()
}

fn version_key(v: &str) -> Vec<u64> {
    v.split('.').map(|s| s.parse().unwrap_or(0)).collect()
}

/// 读取插件目录下 skills/*/SKILL.md（symlink 一律跳过；单文件 ≤512KiB）。
fn scan_plugin_skills(
    plugin: &str,
    version: &str,
    plugin_dir: &std::path::Path,
) -> Vec<MarketSkillEntry> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(plugin_dir.join("skills")) else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() || !ft.is_dir() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().to_string();
        let md_path = entry.path().join("SKILL.md");
        let Ok(md_ft) = std::fs::metadata(&md_path) else {
            continue;
        };
        if md_ft.is_symlink() || !md_ft.is_file() || md_ft.len() as usize > BODY_MAX_BYTES {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&md_path) else {
            continue;
        };
        out.push(MarketSkillEntry {
            name: sanitize_name(&dir_name),
            dir_name,
            description: parse_description(&body),
            plugin: plugin.to_string(),
            version: version.to_string(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 已安装插件清单（installed_plugins.json 缺失/损坏 → 空，调用方回退 cache 扫描）。
fn scan_installed(root: &std::path::Path, marketplace_id: &str) -> Vec<InstalledPlugin> {
    let Ok(data) = std::fs::read_to_string(root.join("installed_plugins.json")) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) else {
        return Vec::new();
    };
    let Some(arr) = v.get("plugins").and_then(|p| p.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|p| {
            let mkt = p.get("marketplace").and_then(|x| x.as_str())?;
            if mkt != marketplace_id {
                return None;
            }
            Some(InstalledPlugin {
                name: p.get("name").and_then(|x| x.as_str())?.to_string(),
                version: p
                    .get("version")
                    .and_then(|x| x.as_str())
                    .unwrap_or("0.0.0")
                    .to_string(),
                install_path: std::path::PathBuf::from(
                    p.get("installPath").and_then(|x| x.as_str())?,
                ),
            })
        })
        .collect()
}

/// 回退：installed_plugins.json 缺该市场时扫 cache/<marketplace>/<plugin>/<version>/，
/// 每插件取最高版本（确定性）。
fn scan_cache_fallback(root: &std::path::Path, marketplace_id: &str) -> Vec<MarketSkillEntry> {
    let mut out = Vec::new();
    let cache = root.join("cache").join(marketplace_id);
    let Ok(plugin_dirs) = std::fs::read_dir(&cache) else {
        return out;
    };
    let mut plugins: Vec<String> = plugin_dirs
        .flatten()
        .filter(|e| {
            e.file_type()
                .map(|f| f.is_dir() && !f.is_symlink())
                .unwrap_or(false)
        })
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    plugins.sort();
    for plugin in plugins {
        let base = cache.join(&plugin);
        let Ok(versions) = std::fs::read_dir(&base) else {
            continue;
        };
        let mut best: Option<(String, std::path::PathBuf)> = None;
        for v in versions.flatten() {
            if v.file_type()
                .map(|f| f.is_symlink() || !f.is_dir())
                .unwrap_or(true)
            {
                continue;
            }
            let ver = v.file_name().to_string_lossy().to_string();
            let better = match &best {
                None => true,
                Some((cur, _)) => version_key(&ver) >= version_key(cur),
            };
            if better {
                best = Some((ver, v.path()));
            }
        }
        if let Some((ver, dir)) = best {
            out.extend(scan_plugin_skills(&plugin, &ver, &dir));
        }
    }
    out
}

/// 浏览全部源（含禁用：skills 为空）。单源故障记录在 error，不中断整页。
pub fn browse(store: &Store) -> SettingsResult<Vec<MarketBrowse>> {
    let sources = source_list(store)?;
    Ok(sources.iter().map(browse_source).collect())
}

fn browse_source(src: &MarketSource) -> MarketBrowse {
    let out = MarketBrowse {
        id: src.id.clone(),
        name: src.name.clone(),
        kind: src.kind.clone(),
        root_path: src.root_path.clone(),
        marketplace_id: src.marketplace_id.clone(),
        enabled: src.enabled,
        revision: src.revision,
        resolved_root: expand_root(&src.root_path).to_string_lossy().to_string(),
        marketplace_name: String::new(),
        description: String::new(),
        plugin_count: 0,
        skills: Vec::new(),
        plugins: Vec::new(),
        error: String::new(),
    };
    if !src.enabled {
        return out;
    }
    match src.kind.as_str() {
        "remote_git" => browse_remote(src, out),
        _ => browse_local(src, out),
    }
}

fn browse_local(src: &MarketSource, mut out: MarketBrowse) -> MarketBrowse {
    let root = expand_root(&src.root_path);
    if !root.is_dir() {
        out.error = format!("目录不存在：{}", root.display());
        return out;
    }
    // 市场定位：marketplace_id 为空 → 取 known_marketplaces 首个（确定性）。
    let mut mkt_id = src.marketplace_id.clone();
    let known = std::fs::read_to_string(root.join("known_marketplaces.json"))
        .ok()
        .and_then(|d| serde_json::from_str::<serde_json::Value>(&d).ok());
    if let Some(v) = known {
        let arr = v.get("marketplaces").and_then(|m| m.as_array());
        let pick = arr.and_then(|a| {
            a.iter()
                .find(|m| {
                    !mkt_id.is_empty()
                        && m.get("id").and_then(|x| x.as_str()) == Some(mkt_id.as_str())
                })
                .or_else(|| a.first())
                .cloned()
        });
        match pick {
            Some(m) => {
                if mkt_id.is_empty() {
                    mkt_id = m
                        .get("id")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                }
                out.marketplace_name = m
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                out.description = m
                    .get("description")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                out.plugin_count = m.get("pluginCount").and_then(|x| x.as_i64()).unwrap_or(0);
            }
            None => {
                out.error = format!("市场清单中未找到 {mkt_id}");
                return out;
            }
        }
    }
    if mkt_id.is_empty() {
        out.error = "无法定位市场清单（缺 known_marketplaces.json 且未指定市场）".into();
        return out;
    }
    out.marketplace_id = mkt_id.clone();
    if out.marketplace_name.is_empty() {
        out.marketplace_name = mkt_id.clone();
    }

    let installed = scan_installed(&root, &mkt_id);
    if installed.is_empty() {
        out.skills = scan_cache_fallback(&root, &mkt_id);
        return out;
    }
    let mut entries = Vec::new();
    for p in &installed {
        entries.extend(scan_plugin_skills(&p.name, &p.version, &p.install_path));
    }
    sort_entries(&mut entries);
    out.skills = entries;
    out
}

/// 远程仓库浏览：有 marketplace.json 清单 → 插件目录（技能按需拉取）；
/// 无清单 → 纯技能仓库，直接列出可导入技能。
fn browse_remote(src: &MarketSource, mut out: MarketBrowse) -> MarketBrowse {
    let url = src.root_path.trim();
    if !is_allowed_git_remote(url) {
        out.error = "远程市场源仅支持 https:// 仓库".into();
        return out;
    }
    out.resolved_root = url.to_string();
    out.description = format!("远程市场仓库：{url}");
    out.marketplace_name = url_tail(url);
    let market = match git_clone_pin(url, None, None) {
        Ok(c) => c,
        Err(e) => {
            out.error = format!("市场仓库拉取失败：{e}");
            return out;
        }
    };
    match read_catalog(&market.dir) {
        Some((mkt_name, catalog)) => {
            if !mkt_name.is_empty() {
                out.marketplace_name = mkt_name;
            }
            out.plugins = catalog.into_iter().map(|(e, _)| e).collect();
            out.plugin_count = out.plugins.len() as i64;
        }
        None => {
            let version = market.head_sha.chars().take(8).collect::<String>();
            let mut found = Vec::new();
            if let Err(e) = walk_skill_entries(&market.dir, &market.dir, &version, &mut found) {
                out.error = format!("仓库扫描失败：{e}");
                return out;
            }
            let mut entries: Vec<MarketSkillEntry> = found.into_iter().map(|(e, _)| e).collect();
            sort_entries(&mut entries);
            out.skills = entries;
        }
    }
    out
}

// ---------------- 导入 ----------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketImport {
    pub skill: super::skills_ext::Skill,
    pub version_id: String,
}

/// 按需拉取一个远程清单插件内的技能清单（marketPluginSkills）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketPluginSkills {
    pub items: Vec<MarketSkillEntry>,
    /// 插件仓库实际检出的 commit（pin SHA 时即 catalog 的 sha）。
    pub resolved_sha: String,
    pub pinned_ref: String,
}

/// 读取 SKILL.md：路径必须落在 root 内（防目录穿越）、≤512KiB、必须是常规文件。
fn read_body_in(md_path: &std::path::Path, root: &std::path::Path) -> SettingsResult<String> {
    let root_canonical = root
        .canonicalize()
        .map_err(|e| invalid(&format!("市场源根不可达：{e}")))?;
    let canonical = md_path
        .canonicalize()
        .map_err(|e| invalid(&format!("技能文件不可达：{e}")))?;
    if !canonical.starts_with(&root_canonical) {
        return Err(invalid("技能路径越界"));
    }
    let meta =
        std::fs::metadata(&canonical).map_err(|e| invalid(&format!("技能文件不可读：{e}")))?;
    if !meta.is_file() || meta.len() as usize > BODY_MAX_BYTES {
        return Err(invalid("SKILL.md 缺失或超过 512KiB 上限"));
    }
    std::fs::read_to_string(&canonical).map_err(|e| invalid(&format!("技能文件不可读：{e}")))
}

/// 本地源解析：重新浏览并按 (plugin, dir_name) 命中，读回正文。
/// 返回（条目, 正文, 市场标签, 文件路径）。
fn resolve_local_skill(
    src: &MarketSource,
    plugin: &str,
    skill_dir: &str,
) -> SettingsResult<(MarketSkillEntry, String, String, String)> {
    let browse = browse_source(src);
    if !browse.error.is_empty() {
        return Err(invalid(&format!("市场源不可用：{}", browse.error)));
    }
    let entry = browse
        .skills
        .iter()
        .find(|e| e.plugin == plugin && e.dir_name == skill_dir)
        .cloned()
        .ok_or_else(|| invalid("市场内未找到该技能（可能已被卸载或升级，请刷新）"))?;
    let md_path = expand_root(&src.root_path)
        .join("cache")
        .join(&browse.marketplace_id)
        .join(plugin)
        .join(&entry.version)
        .join("skills")
        .join(skill_dir)
        .join("SKILL.md");
    let body = read_body_in(&md_path, &expand_root(&src.root_path))?;
    Ok((
        entry,
        body,
        browse.marketplace_id.clone(),
        md_path.to_string_lossy().to_string(),
    ))
}

/// 远程源解析：克隆市场仓库 → 命中清单插件 → 按 pin 规则克隆插件仓库 → 命中技能 → 读正文。
/// 无清单的纯技能仓库：直接在市场仓库内命中。临时克隆在返回前读取正文，Drop 自动清理。
/// 返回（条目, 正文, 市场标签, 插件仓库 URL, 检出 SHA）。
fn resolve_remote_skill(
    src: &MarketSource,
    plugin: &str,
    skill_dir: &str,
) -> SettingsResult<(MarketSkillEntry, String, String, String, String)> {
    let url = src.root_path.trim();
    if !is_allowed_git_remote(url) {
        return Err(invalid("远程市场源仅支持 https:// 仓库"));
    }
    let market =
        git_clone_pin(url, None, None).map_err(|e| invalid(&format!("市场仓库拉取失败：{e}")))?;
    let (label, catalog) = match read_catalog(&market.dir) {
        Some((n, c)) => (if n.is_empty() { url_tail(url) } else { n }, Some(c)),
        None => (url_tail(url), None),
    };
    if let Some(catalog) = catalog {
        let (pe, gs) = catalog
            .into_iter()
            .find(|(e, _)| e.name == plugin)
            .ok_or_else(|| invalid("市场清单中未找到该插件"))?;
        // 外链源 → 按 pin 规则克隆插件仓库；仓库内相对路径源（url 为空）→ 市场仓库本身。
        let cloned = if gs.url.is_empty() {
            None
        } else {
            Some(
                git_clone_pin(&gs.url, gs.ref_.as_deref(), gs.sha.as_deref())
                    .map_err(|e| invalid(&format!("插件仓库拉取失败：{e}")))?,
            )
        };
        let repo_root = cloned
            .as_ref()
            .map(|c| c.dir.clone())
            .unwrap_or_else(|| market.dir.clone());
        let sha = cloned
            .as_ref()
            .map(|c| c.head_sha.clone())
            .unwrap_or_else(|| market.head_sha.clone());
        let base = if gs.path.is_empty() {
            repo_root.clone()
        } else {
            repo_root.join(&gs.path)
        };
        let entry = scan_plugin_skills(plugin, &pe.version, &base)
            .into_iter()
            .find(|e| e.dir_name == skill_dir)
            .ok_or_else(|| invalid("插件内未找到该技能（可能已被上游移除，请刷新）"))?;
        let md = base.join("skills").join(skill_dir).join("SKILL.md");
        let body = read_body_in(&md, &repo_root)?;
        let mut entry = entry;
        if entry.version.is_empty() {
            entry.version = sha.chars().take(8).collect();
        }
        let repo_url_out = if gs.url.is_empty() {
            url.to_string()
        } else {
            gs.url.clone()
        };
        return Ok((entry, body, label, repo_url_out, sha));
    }
    // 纯技能仓库：市场仓库本身就是技能内容。
    let version = market.head_sha.chars().take(8).collect::<String>();
    let mut found = Vec::new();
    walk_skill_entries(&market.dir, &market.dir, &version, &mut found)
        .map_err(|e| invalid(&format!("仓库扫描失败：{e}")))?;
    let (entry, md) = found
        .into_iter()
        .find(|(e, _)| e.dir_name == skill_dir)
        .ok_or_else(|| invalid("仓库内未找到该技能（可能已被上游移除，请刷新）"))?;
    let body = read_body_in(&md, &market.dir)?;
    Ok((entry, body, label, url.to_string(), market.head_sha.clone()))
}

/// 按需拉取远程清单插件内的技能列表（浏览用，不落库）。
pub fn plugin_skills(
    store: &Store,
    source_id: &str,
    plugin: &str,
) -> SettingsResult<MarketPluginSkills> {
    let src = source_get(store, source_id)?;
    if !src.enabled {
        return Err(invalid("市场源已停用"));
    }
    if src.kind != "remote_git" {
        return Err(invalid("仅远程 Git 市场源支持按插件拉取技能清单"));
    }
    let url = src.root_path.trim();
    if !is_allowed_git_remote(url) {
        return Err(invalid("远程市场源仅支持 https:// 仓库"));
    }
    let market =
        git_clone_pin(url, None, None).map_err(|e| invalid(&format!("市场仓库拉取失败：{e}")))?;
    let (_, catalog) =
        read_catalog(&market.dir).ok_or_else(|| invalid("市场仓库无 marketplace.json 清单"))?;
    let (pe, gs) = catalog
        .into_iter()
        .find(|(e, _)| e.name == plugin)
        .ok_or_else(|| invalid("市场清单中未找到该插件"))?;
    let cloned = if gs.url.is_empty() {
        None
    } else {
        Some(
            git_clone_pin(&gs.url, gs.ref_.as_deref(), gs.sha.as_deref())
                .map_err(|e| invalid(&format!("插件仓库拉取失败：{e}")))?,
        )
    };
    let repo_root = cloned
        .as_ref()
        .map(|c| c.dir.clone())
        .unwrap_or_else(|| market.dir.clone());
    let sha = cloned
        .as_ref()
        .map(|c| c.head_sha.clone())
        .unwrap_or_else(|| market.head_sha.clone());
    let base = if gs.path.is_empty() {
        repo_root.clone()
    } else {
        repo_root.join(&gs.path)
    };
    let items = scan_plugin_skills(plugin, &pe.version, &base);
    Ok(MarketPluginSkills {
        items,
        resolved_sha: sha,
        pinned_ref: gs.ref_.unwrap_or_default(),
    })
}

/// 从市场源导入一个 SKILL.md：enabled 技能（source=market，同名幂等）+ draft 版本
/// （同内容幂等）+ 审计。仅读取该文件文本；市场内任何文件都不执行。
pub fn import_from_source(
    store: &Store,
    source_id: &str,
    plugin: &str,
    version: &str,
    skill_dir: &str,
    created_by: &str,
) -> SettingsResult<MarketImport> {
    let src = source_get(store, source_id)?;
    if !src.enabled {
        return Err(invalid("市场源已停用"));
    }
    let (entry, body, marketplace, repo_url, resolved_sha, path_str) = match src.kind.as_str() {
        "remote_git" => {
            let (e, body, label, repo_url, sha) = resolve_remote_skill(&src, plugin, skill_dir)?;
            (e, body, label, repo_url, sha, String::new())
        }
        _ => {
            let (e, body, marketplace, path_str) = resolve_local_skill(&src, plugin, skill_dir)?;
            (e, body, marketplace, String::new(), String::new(), path_str)
        }
    };
    let version = if version.is_empty() {
        entry.version.clone()
    } else {
        version.to_string()
    };
    // 幂等语义：create 同名返回既有；create_version 同内容返回既有。
    // 仅当确实新增了版本（首次导入或上游内容漂移）时写审计。
    let versions_before = match super::skills_ext::skill_by_name(store, &entry.name) {
        Ok(Some(existing)) => super::skills_ext::version_list(store, &existing.id)
            .map(|v| v.len())
            .unwrap_or(0),
        _ => 0,
    };
    let skill = super::skills_ext::create(
        store,
        &entry.name,
        &entry.description,
        &body,
        "market",
        None,
    )?;
    let version_row = super::skills_ext::create_version(
        store,
        &skill.id,
        &body,
        &format!("market:{}/{}@{}", marketplace, plugin, version),
    )?;
    let versions_after = super::skills_ext::version_list(store, &skill.id)
        .map(|v| v.len())
        .unwrap_or(0);
    if versions_after > versions_before {
        sg_store::audit::append(
            store,
            created_by,
            "skill.market.import",
            "skill",
            &skill.id,
            serde_json::json!({
                "sourceId": source_id,
                "marketplace": marketplace,
                "plugin": plugin,
                "version": version,
                "skillDir": skill_dir,
                "path": path_str,
                "repo": repo_url,
                "sha": resolved_sha,
                "executed": false,
                "note": "仅读取 SKILL.md；无任何文件被执行",
            }),
        )
        .map_err(store_err)?;
    }
    Ok(MarketImport {
        skill,
        version_id: version_row.id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-mkt-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Store::open(&dir, "test").unwrap()
    }

    /// 两个市场 + installed 清单只含 mkt-a；mkt-b 走 cache 回退（版本取高、symlink 跳过）。
    fn fixture_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "sg-mkt-root-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        let cache = root.join("cache");
        std::fs::create_dir_all(root.join("marketplaces")).unwrap();

        std::fs::write(
            root.join("known_marketplaces.json"),
            r#"{"version":1,"marketplaces":[
                {"id":"mkt-a","name":"mkt-a","description":"市场A","pluginCount":12},
                {"id":"mkt-b","name":"mkt-b","description":"市场B","pluginCount":287}]}"#,
        )
        .unwrap();

        let p1 = cache.join("mkt-a/plugin-one/1.0.0");
        std::fs::create_dir_all(p1.join("skills/alpha")).unwrap();
        std::fs::write(
            p1.join("skills/alpha/SKILL.md"),
            "---\nname: alpha-skill\ndescription: \"Alpha 技能说明\"\n---\n\n# Alpha\n正文。\n",
        )
        .unwrap();
        // 未安装的 1.1.0 不应出现在清单（installed 只登记 1.0.0）。
        let p1_new = cache.join("mkt-a/plugin-one/1.1.0");
        std::fs::create_dir_all(p1_new.join("skills/alpha")).unwrap();
        std::fs::write(p1_new.join("skills/alpha/SKILL.md"), "新版本正文").unwrap();

        let p2_old = cache.join("mkt-b/plugin-two/0.1.0");
        std::fs::create_dir_all(p2_old.join("skills/beta")).unwrap();
        std::fs::write(p2_old.join("skills/beta/SKILL.md"), "旧版").unwrap();
        let p2 = cache.join("mkt-b/plugin-two/0.2.0");
        std::fs::create_dir_all(p2.join("skills/beta")).unwrap();
        std::fs::write(
            p2.join("skills/beta/SKILL.md"),
            "---\ndescription: Beta 新版说明\n---\n\n# Beta\n正文。\n",
        )
        .unwrap();
        // skills 下的目录 symlink 必须被跳过（指向市场外）。
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc", p2.join("skills/evil")).unwrap();

        std::fs::write(
            root.join("installed_plugins.json"),
            format!(
                r#"{{"version":1,"plugins":[
                {{"id":"plugin-one@mkt-a","name":"plugin-one","marketplace":"mkt-a",
                   "version":"1.0.0","installPath":"{}"}}]}}"#,
                p1.to_string_lossy().replace('\\', "\\\\")
            ),
        )
        .unwrap();
        root
    }

    #[test]
    fn seeds_defaults_and_browses_sources() {
        let store = setup();
        let root = fixture_root();
        // 首次列出 → 播种两个默认源。
        let seeded = source_list(&store).unwrap();
        assert_eq!(seeded.len(), 2);
        assert_eq!(seeded[0].marketplace_id, "zcode-plugins-official");
        assert_eq!(seeded[1].name, "Claude Code 插件");
        assert_eq!(seeded[1].root_path, "~/.zcode/cli/plugins");

        // 用户添加指向 fixture 的源。
        let src = source_save(
            &store,
            None,
            "zcode_local",
            "测试市场",
            &root.to_string_lossy(),
            "mkt-a",
            Some(true),
            None,
        )
        .unwrap();
        let items = browse(&store).unwrap();
        let a = items.iter().find(|b| b.id == src.id).unwrap();
        assert_eq!(a.error, "");
        assert_eq!(a.marketplace_name, "mkt-a");
        assert_eq!(a.plugin_count, 12);
        // installed 清单：只有 1.0.0 的 alpha（1.1.0 未安装）。
        assert_eq!(a.skills.len(), 1);
        assert_eq!(a.skills[0].plugin, "plugin-one");
        assert_eq!(a.skills[0].version, "1.0.0");
        assert_eq!(a.skills[0].name, "alpha");
        assert_eq!(a.skills[0].description, "Alpha 技能说明");

        // mkt-b 无已安装插件 → cache 回退：取最高版本 0.2.0；symlink evil 被跳过。
        let src_b = source_save(
            &store,
            None,
            "zcode_local",
            "回退市场",
            &root.to_string_lossy(),
            "mkt-b",
            Some(true),
            None,
        )
        .unwrap();
        let items = browse(&store).unwrap();
        let b = items.iter().find(|x| x.id == src_b.id).unwrap();
        assert_eq!(b.skills.len(), 1, "symlink evil 不得入列");
        assert_eq!(b.skills[0].version, "0.2.0");
        assert_eq!(b.skills[0].description, "Beta 新版说明");
    }

    #[test]
    fn source_validation_rejects_bad_root_and_cas_conflict() {
        let store = setup();
        let bogus = std::env::temp_dir().join(format!("sg-mkt-bogus-{}", ids::new_id("t")));
        std::fs::create_dir_all(&bogus).unwrap();
        let err = source_save(
            &store,
            None,
            "zcode_local",
            "坏源",
            &bogus.to_string_lossy(),
            "",
            Some(true),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("不是插件市场目录"), "{err}");
        let err = source_save(
            &store,
            None,
            "zcode_local",
            "",
            "/tmp",
            "",
            Some(true),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("名称"), "{err}");

        let root = fixture_root();
        let src = source_save(
            &store,
            None,
            "zcode_local",
            "测试市场",
            &root.to_string_lossy(),
            "mkt-a",
            Some(true),
            None,
        )
        .unwrap();
        // 旧 revision 编辑 → CAS 冲突。
        let err = source_save(
            &store,
            Some(&src.id),
            "zcode_local",
            "改名",
            &root.to_string_lossy(),
            "mkt-a",
            None,
            Some(src.revision - 1),
        )
        .unwrap_err();
        assert!(err.to_string().contains("revision 冲突"), "{err}");
        // 正确 CAS 编辑 + 删除。
        let edited = source_save(
            &store,
            Some(&src.id),
            "zcode_local",
            "改名",
            &root.to_string_lossy(),
            "mkt-b",
            Some(false),
            Some(src.revision),
        )
        .unwrap();
        assert_eq!(edited.name, "改名");
        assert!(!edited.enabled);
        source_remove(&store, &src.id, edited.revision).unwrap();
        assert!(source_get(&store, &src.id).is_err());
    }

    #[test]
    fn import_creates_skill_version_audit_and_is_idempotent() {
        let store = setup();
        let root = fixture_root();
        let src = source_save(
            &store,
            None,
            "zcode_local",
            "测试市场",
            &root.to_string_lossy(),
            "mkt-a",
            Some(true),
            None,
        )
        .unwrap();
        let out =
            import_from_source(&store, &src.id, "plugin-one", "1.0.0", "alpha", "tester").unwrap();
        assert_eq!(out.skill.source, "market");
        assert!(out.skill.enabled);
        assert_eq!(out.skill.description, "Alpha 技能说明");
        let versions = super::super::skills_ext::version_list(&store, &out.skill.id).unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].status, "draft");
        assert!(versions[0].description.contains("plugin-one@1.0.0"));

        // 幂等：再次导入同技能/同版本 → 同 skill、同 version，审计不重复。
        let again =
            import_from_source(&store, &src.id, "plugin-one", "1.0.0", "alpha", "tester").unwrap();
        assert_eq!(again.skill.id, out.skill.id);
        assert_eq!(again.version_id, out.version_id);
        let audits: i64 = store
            .with_conn(|conn| {
                Ok(conn
                    .query_row(
                        "SELECT COUNT(*) FROM audit_log WHERE action='skill.market.import'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(0))
            })
            .unwrap();
        assert_eq!(audits, 1);

        // 未知技能 / 未知源 → 拒绝。
        assert!(
            import_from_source(&store, &src.id, "plugin-one", "1.0.0", "ghost", "tester").is_err()
        );
        assert!(import_from_source(
            &store,
            "mkt_ghost",
            "plugin-one",
            "1.0.0",
            "alpha",
            "tester"
        )
        .is_err());
    }

    /// 建工作仓库 → commit → clone --bare。返回（bare 路径, HEAD sha）。
    fn make_bare(name: &str, files: &[(&str, &str)]) -> (String, String) {
        let work = std::env::temp_dir().join(format!(
            "sg-mkt-w-{}-{}-{}",
            std::process::id(),
            ids::new_id("t"),
            name
        ));
        std::fs::create_dir_all(&work).unwrap();
        for (rel, body) in files {
            let p = work.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "m@s.local"],
            vec!["config", "user.name", "m"],
            vec!["add", "."],
            vec!["commit", "-qm", "v1"],
        ] {
            let o = std::process::Command::new("git")
                .arg("-C")
                .arg(&work)
                .args(&args)
                .output()
                .unwrap();
            assert!(
                o.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&o.stderr)
            );
        }
        let sha = std::process::Command::new("git")
            .arg("-C")
            .arg(&work)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let sha = String::from_utf8_lossy(&sha.stdout).trim().to_string();
        let bare = work.join("origin.git");
        let o = std::process::Command::new("git")
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
        (bare.to_string_lossy().to_string(), sha)
    }

    /// 远程单测需要放宽 https 限制（本地 bare 路径作为远端）。
    fn allow_local_git() {
        std::env::set_var("RATIFLOW_SKILL_MARKET_LOCAL_GIT", "1");
    }

    /// 纯技能仓库：无清单，仓库内 skills/alpha/SKILL.md。
    #[test]
    fn remote_flat_repo_browse_and_import() {
        allow_local_git();
        let store = setup();
        let (bare, _sha) = make_bare(
            "flat",
            &[(
                "skills/alpha/SKILL.md",
                "---\nname: alpha\ndescription: \"远程 Alpha\"\n---\n\n# Alpha\n正文。\n",
            )],
        );
        let src = source_save(
            &store,
            None,
            "remote_git",
            "远端技能库",
            &bare,
            "",
            Some(true),
            None,
        )
        .unwrap();
        let items = browse(&store).unwrap();
        let b = items.iter().find(|x| x.id == src.id).unwrap();
        assert_eq!(b.error, "");
        assert!(b.plugins.is_empty(), "无清单仓库不产生插件目录");
        assert_eq!(b.skills.len(), 1);
        assert_eq!(b.skills[0].name, "alpha");
        assert_eq!(b.skills[0].description, "远程 Alpha");

        let out = import_from_source(&store, &src.id, "", "", "alpha", "tester").unwrap();
        assert_eq!(out.skill.source, "market");
        assert_eq!(out.skill.description, "远程 Alpha");
        let versions = super::super::skills_ext::version_list(&store, &out.skill.id).unwrap();
        assert_eq!(versions[0].status, "draft");
        assert!(versions[0].description.contains("market:"), "{versions:?}");
        // 审计含仓库与检出 SHA。
        let audit: String = store
            .with_conn(|conn| {
                Ok(conn
                    .query_row(
                        "SELECT detail FROM audit_log WHERE action='skill.market.import'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or_default())
            })
            .unwrap();
        assert!(audit.contains(&bare) || audit.contains("sha"), "{audit}");
    }

    /// 清单仓库：.claude-plugin/marketplace.json 指向插件仓库（pin SHA）。
    /// 浏览列插件；按需拉取技能；导入按 SHA 检出。
    #[test]
    fn remote_catalog_repo_lists_plugins_and_imports_pinned() {
        allow_local_git();
        let store = setup();
        let (plugin_bare, plugin_sha) = make_bare(
            "plugin",
            &[
                (
                    "plugins/p1/skills/remote-alpha/SKILL.md",
                    "---\ndescription: \"目录 Alpha\"\n---\n\n# A\n正文。\n",
                ),
                ("plugins/p1/README.md", "非技能文件，不应被导入"),
            ],
        );
        let marketplace_json = format!(
            r#"{{"name":"remote-mkt","plugins":[
                {{"name":"plugin-one","description":"远程插件","version":"1.0.0","category":"tools",
                  "source":{{"source":"git-subdir","url":"{plugin_bare}","path":"plugins/p1","sha":"{plugin_sha}"}}}},
                {{"name":"plugin-two","description":"内置插件","source":"./plugins/p2"}}]}}"#
        );
        let (market_bare, market_sha) = make_bare(
            "market",
            &[
                (".claude-plugin/marketplace.json", marketplace_json.as_str()),
                (
                    "plugins/p2/skills/two/SKILL.md",
                    "---\ndescription: \"内置技能\"\n---\n\n# T\n正文。\n",
                ),
            ],
        );

        let src = source_save(
            &store,
            None,
            "remote_git",
            "远端插件市场",
            &market_bare,
            "",
            Some(true),
            None,
        )
        .unwrap();
        let items = browse(&store).unwrap();
        let b = items.iter().find(|x| x.id == src.id).unwrap();
        assert_eq!(b.error, "");
        assert_eq!(b.plugin_count, 2);
        assert_eq!(b.plugins[0].name, "plugin-one");
        assert_eq!(b.plugins[0].version, "1.0.0");
        assert!(b.skills.is_empty(), "清单源浏览不直接列技能");

        // 按需拉取插件技能（外链 + pin SHA）。
        let ps = plugin_skills(&store, &src.id, "plugin-one").unwrap();
        assert_eq!(ps.items.len(), 1, "README.md 不得入列");
        assert_eq!(ps.items[0].dir_name, "remote-alpha");
        assert_eq!(ps.resolved_sha, plugin_sha, "pin SHA 检出");

        // 仓库内相对路径插件：不额外克隆，pin 市场 HEAD。
        let ps2 = plugin_skills(&store, &src.id, "plugin-two").unwrap();
        assert_eq!(ps2.items.len(), 1);
        assert_eq!(ps2.items[0].dir_name, "two");
        assert_eq!(ps2.resolved_sha, market_sha);

        // 导入：走清单 → pin 拉取 → 只取 SKILL.md。
        let out = import_from_source(
            &store,
            &src.id,
            "plugin-one",
            "1.0.0",
            "remote-alpha",
            "tester",
        )
        .unwrap();
        assert_eq!(out.skill.name, "remote-alpha");
        assert_eq!(out.skill.description, "目录 Alpha");
        let versions = super::super::skills_ext::version_list(&store, &out.skill.id).unwrap();
        assert!(versions[0]
            .description
            .contains("remote-mkt/plugin-one@1.0.0"));
        // 幂等：重复导入审计不翻倍。
        import_from_source(
            &store,
            &src.id,
            "plugin-one",
            "1.0.0",
            "remote-alpha",
            "tester",
        )
        .unwrap();
        let audits: i64 = store
            .with_conn(|conn| {
                Ok(conn
                    .query_row(
                        "SELECT COUNT(*) FROM audit_log WHERE action='skill.market.import'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(0))
            })
            .unwrap();
        assert_eq!(audits, 1);
    }

    #[test]
    fn remote_source_rejects_non_https_and_bad_kind() {
        allow_local_git();
        let store = setup();
        // 非 https 远端一律拒绝（本地路径放行仅限测试旗标下的 bare 仓库）。
        let err = source_save(
            &store,
            None,
            "remote_git",
            "坏远端",
            "ftp://example.com/repo.git",
            "",
            Some(true),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("https"), "{err}");
        // 非法类型。
        let err = source_save(
            &store,
            None,
            "bogus_kind",
            "坏类型",
            "/tmp",
            "",
            Some(true),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("非法市场源类型"), "{err}");
    }
}
