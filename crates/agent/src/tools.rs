//! 工具注册表（F05/M0-③）：Agent 工具的唯一权威定义与提案→Manifest 映射。
//! canonical 字典序保证序列化稳定（缓存前缀纪律的回归点）。
//! 安全：所有 path 参数经项目根守卫（逃逸拒绝）；write_file 只落工件草稿区并做秘密扫描 fail-closed。
use serde_json::{json, Value};
use sg_executor::ExecutionManifest;
use sg_policy::Risk;
use std::path::{Path, PathBuf};

pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub risk: Risk,
    pub data_level: &'static str,
    pub max_result_bytes: usize,
    pub timeout_sec: i64,
    /// 参数 JSON Schema（形状说明；对齐 contracts/jsonschema/tool-definition.schema.json）。
    pub parameters: fn() -> Value,
}

fn params_read_file() -> Value {
    json!({"type":"object","required":["path"],"properties":{
        "path":{"type":"string","description":"项目根内相对路径"}}})
}
fn params_run_command() -> Value {
    json!({"type":"object","required":["argv"],"properties":{
        "argv":{"type":"array","items":{"type":"string"},"minItems":1},
        "writes_files":{"type":"boolean","default":false}}})
}
fn params_search_knowledge() -> Value {
    json!({"type":"object","required":["query"],"properties":{
        "query":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":20,"default":5}}})
}
fn params_write_file() -> Value {
    json!({"type":"object","required":["path","content"],"properties":{
        "path":{"type":"string","description":"草稿区内相对路径"},
        "content":{"type":"string"}}})
}

const MIB: usize = 1 << 20;

/// 注册表（name 字典序 canonical）。
pub fn registry() -> Vec<&'static ToolDef> {
    static READ_FILE: ToolDef = ToolDef {
        name: "read_file",
        description: "读取项目内相对路径文件内容（只读，路径不得逃逸项目根）",
        risk: Risk::Low,
        data_level: "internal",
        max_result_bytes: MIB,
        timeout_sec: 60,
        parameters: params_read_file,
    };
    static RUN_COMMAND: ToolDef = ToolDef {
        name: "run_command",
        description: "按 argv 执行命令（无 shell；受执行模式约束，高风险需审批）",
        risk: Risk::High,
        data_level: "internal",
        max_result_bytes: MIB,
        timeout_sec: 120,
        parameters: params_run_command,
    };
    static SEARCH_KNOWLEDGE: ToolDef = ToolDef {
        name: "search_knowledge",
        description: "检索项目知识库（库内调用，免起进程）",
        risk: Risk::Low,
        data_level: "internal",
        max_result_bytes: MIB,
        timeout_sec: 30,
        parameters: params_search_knowledge,
    };
    static WRITE_FILE: ToolDef = ToolDef {
        name: "write_file",
        description: "写入工件草稿区（不直接改工作区；内容过秘密扫描）",
        risk: Risk::Medium,
        data_level: "internal",
        max_result_bytes: MIB,
        timeout_sec: 60,
        parameters: params_write_file,
    };
    // 字典序：read_file < run_command < search_knowledge < write_file
    vec![&READ_FILE, &RUN_COMMAND, &SEARCH_KNOWLEDGE, &WRITE_FILE]
}

pub fn find(name: &str) -> Option<&'static ToolDef> {
    registry().into_iter().find(|d| d.name == name)
}

impl ToolDef {
    /// 契约风格（camelCase）描述，tool.list/前端消费。
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "risk": format!("{:?}", self.risk).to_lowercase(),
            "dataLevel": self.data_level,
            "maxResultBytes": self.max_result_bytes,
            "timeoutSec": self.timeout_sec,
            "parameters": (self.parameters)(),
        })
    }
}

/// 工具执行上下文（装配层注入）。
#[derive(Clone, Debug)]
pub struct ToolCtx {
    pub mode: sg_executor::Mode,
    /// 项目根（canonical）；read_file/run_command 的路径守卫基准与 work_dir。
    /// 未登记 local_root 的项目为 None——需要根的工具显式失败。
    pub work_dir: Option<PathBuf>,
    /// 工件草稿区（<dataDir>/artifacts/<runId>/）。
    pub artifacts_dir: PathBuf,
}

/// 提案参数 → 受约束 ExecutionManifest（read_file / run_command）。
/// write_file/search_knowledge 不走进程，由装配层直接执行。
pub fn build_manifest(
    def: &ToolDef,
    args: &Value,
    ctx: &ToolCtx,
) -> Result<ExecutionManifest, String> {
    match def.name {
        "read_file" => {
            let rel = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing argument: path".to_string())?;
            let root = ctx.work_dir.as_ref().ok_or_else(|| {
                "path_outside_project: 项目未登记 local_root，read_file 不可用".to_string()
            })?;
            let abs = resolve_under(root, rel)?;
            Ok(ExecutionManifest {
                argv: vec!["cat".into(), abs.to_string_lossy().to_string()],
                work_dir: String::new(),
                image: "alpine:3".into(),
                network_off: true,
                memory_mb: 256,
                cpus: 0.5,
                timeout_sec: def.timeout_sec,
                writes_files: false,
            })
        }
        "run_command" => {
            let argv: Vec<String> = args
                .get("argv")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .ok_or_else(|| "missing argument: argv".to_string())?;
            if argv.is_empty() {
                return Err("argv must not be empty".into());
            }
            let work_dir = ctx
                .work_dir
                .as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            Ok(ExecutionManifest {
                argv,
                work_dir,
                image: "alpine:3".into(),
                network_off: true,
                memory_mb: 256,
                cpus: 0.5,
                timeout_sec: def.timeout_sec,
                writes_files: args
                    .get("writes_files")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            })
        }
        other => Err(format!("tool {other} 不经由 manifest 执行")),
    }
}

/// 路径守卫：rel 必须是相对路径、不含 `..` 路径段，且解析后不逃逸 root（含符号链接）。
/// 目标可能尚不存在（write 场景）：向上取最深存在的祖先做 canonical 校验后拼回余下段。
fn resolve_under(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return Err("path_outside_project: 不允许绝对路径".into());
    }
    if rel_path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("path_outside_project: 不允许 .. 路径段".into());
    }
    let root_canon = root
        .canonicalize()
        .map_err(|_| format!("path_outside_project: 项目根不可访问 {}", root.display()))?;
    let joined = root_canon.join(rel_path);
    // 向上找最深存在的祖先；不存在段收集后拼回。
    let mut cur = joined.clone();
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    while !cur.exists() && cur != root_canon {
        match (cur.file_name(), cur.parent()) {
            (Some(name), Some(parent)) => {
                suffix.push(name.to_os_string());
                cur = parent.to_path_buf();
            }
            _ => return Err("path_outside_project: 无法解析路径".into()),
        }
    }
    let canon = cur
        .canonicalize()
        .map_err(|_| "path_outside_project: 路径不可访问".to_string())?;
    let mut target = canon;
    for part in suffix.iter().rev() {
        target.push(part);
    }
    if !target.starts_with(&root_canon) {
        return Err(format!("path_outside_project: {rel} 逃逸项目根"));
    }
    Ok(target)
}

/// write_file：落工件草稿区（tmp+rename 原子写）；内容秘密扫描 fail-closed。
pub fn write_draft(ctx: &ToolCtx, args: &Value) -> Result<String, String> {
    let rel = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing argument: path".to_string())?;
    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing argument: content".to_string())?;
    if sg_store::scan::has_high_risk(&sg_store::scan::scan(content.as_bytes())) {
        return Err("object_contains_secrets: 草稿内容命中高风险秘密，拒绝落盘".into());
    }
    std::fs::create_dir_all(&ctx.artifacts_dir).map_err(|e| format!("create_dir_all: {e}"))?;
    let target = resolve_under(&ctx.artifacts_dir, rel)?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create_dir_all: {e}"))?;
    }
    let tmp = target.with_extension("tmp");
    std::fs::write(&tmp, content.as_bytes()).map_err(|e| format!("write: {e}"))?;
    std::fs::rename(&tmp, &target).map_err(|e| format!("rename: {e}"))?;
    Ok(format!(
        "written: {} ({} bytes)",
        target.display(),
        content.len()
    ))
}

/// 输出截断：超过 max_bytes 时保留头 3/4 + 尾 1/4，插入标记（进模型消息的是截断版）。
pub fn truncate_output(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let head_target = max_bytes * 3 / 4;
    let tail_target = max_bytes / 4;
    let mut head = head_target.min(text.len());
    while head > 0 && !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail_start = text.len().saturating_sub(tail_target);
    while tail_start < text.len() && !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!(
        "{}\n[TRUNCATED {} bytes]\n{}",
        &text[..head],
        text.len(),
        &text[tail_start..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with_root() -> ToolCtx {
        let root = std::env::temp_dir().join(format!("sg-tools-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("NOTES.md"), "hello-m03").unwrap();
        ToolCtx {
            mode: sg_executor::Mode::SafeRestricted,
            work_dir: Some(root.clone()),
            artifacts_dir: root.join("artifacts"),
        }
    }

    #[test]
    fn registry_is_canonical_and_stable() {
        let names: Vec<&str> = registry().iter().map(|d| d.name).collect();
        assert_eq!(
            names,
            vec!["read_file", "run_command", "search_knowledge", "write_file"]
        );
        let a = serde_json::to_string(&registry().iter().map(|d| d.to_json()).collect::<Vec<_>>())
            .unwrap();
        let b = serde_json::to_string(&registry().iter().map(|d| d.to_json()).collect::<Vec<_>>())
            .unwrap();
        assert_eq!(a, b, "注册表序列化两次必须字节相等");
    }

    #[test]
    fn read_file_manifest_reads_real_content() {
        let ctx = ctx_with_root();
        let def = find("read_file").unwrap();
        let manifest = build_manifest(def, &json!({"path": "NOTES.md"}), &ctx).unwrap();
        let result = sg_executor::execute(ctx.mode, &manifest).unwrap();
        assert!(result.stdout.contains("hello-m03"), "读到真实内容");
    }

    #[test]
    fn path_escape_rejected() {
        let ctx = ctx_with_root();
        let def = find("read_file").unwrap();
        for bad in ["../../etc/passwd", "/etc/passwd", "sub/../../../etc/hosts"] {
            let err = build_manifest(def, &json!({"path": bad}), &ctx).unwrap_err();
            assert!(err.contains("path_outside_project"), "{bad} → {err}");
        }
    }

    #[test]
    fn run_command_argv_passed_through() {
        let ctx = ctx_with_root();
        let def = find("run_command").unwrap();
        let manifest =
            build_manifest(def, &json!({"argv": ["wc", "-c", "NOTES.md"]}), &ctx).unwrap();
        // UnsafeExplicit 才能跑 wc（不在只读白名单）；验证 argv 逐参数传递。
        let result = sg_executor::execute(sg_executor::Mode::UnsafeExplicit, &manifest).unwrap();
        assert!(result.stdout.contains("NOTES.md"), "argv 参数真实传递");
        assert!(result.stdout.contains('9'), "字节数 9（hello-m03）");
    }

    #[test]
    fn write_draft_writes_and_secret_rejects() {
        let ctx = ctx_with_root();
        let out = write_draft(&ctx, &json!({"path": "drafts/a.md", "content": "# 草稿"})).unwrap();
        assert!(out.contains("written"));
        assert!(ctx.artifacts_dir.join("drafts/a.md").exists());
        let err = write_draft(
            &ctx,
            &json!({"path": "drafts/secret.txt", "content": "token glpat-abcdefghijklmnopqrstuvwxyz1234567890"}),
        )
        .unwrap_err();
        assert!(err.contains("object_contains_secrets"), "秘密内容拒绝落盘");
        assert!(!ctx.artifacts_dir.join("drafts/secret.txt").exists());
        // 路径逃逸
        let err = write_draft(&ctx, &json!({"path": "../escape.txt", "content": "x"})).unwrap_err();
        assert!(err.contains("path_outside_project"));
    }

    #[test]
    fn truncation_marks_and_caps() {
        let text = "x".repeat(100 * 1024);
        let out = truncate_output(&text, 8 * 1024);
        assert!(out.contains("[TRUNCATED 102400 bytes]"));
        assert!(out.len() < 10 * 1024, "截断后远小于原文");
        assert_eq!(truncate_output("short", 1024), "short");
    }
}
