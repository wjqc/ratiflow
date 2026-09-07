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
    /// M2-08：副作用分类（PlanGuard phase 判定输入；ADR-037 §6.6）。
    pub effect_class: &'static str,
    /// WP-1（RDWS v1.4 A4）：可恢复性声明（0018 词汇），进统一风险评估。
    pub reversibility: &'static str,
    /// WP-1：静态受保护目标声明（部署交付物/迁移/依赖锁/CI 配置类写入）；
    /// 运行时路径级保护判定由 Policy protected-target registry 承接（WP-8）。
    pub protected_target: bool,
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
fn params_apply_patch() -> Value {
    json!({"type":"object","required":["patch"],"properties":{
        "patch":{"type":"string","description":"unified diff 补丁文本"},
        "baseHead":{"type":"string","description":"期望的 worktree HEAD（CAS；缺省跳过 HEAD 校验）"},
        "expectedHashes":{"type":"object","description":"可选：路径 → 期望的当前内容 sha256"}}})
}

fn params_write_file() -> Value {
    json!({"type":"object","required":["path","content"],"properties":{
        "path":{"type":"string","description":"草稿区内相对路径"},
        "content":{"type":"string"}}})
}

const MIB: usize = 1 << 20;

/// 注册表（name 字典序 canonical）。
pub fn registry() -> Vec<&'static ToolDef> {
    static APPLY_PATCH: ToolDef = ToolDef {
        name: "apply_patch",
        description: "对受管 worktree 应用 unified diff 增量修改（高风险需审批；CAS 防漂移；主工作区不受影响）",
        risk: Risk::High,
        data_level: "internal",
        max_result_bytes: MIB,
        timeout_sec: 60,
        parameters: params_apply_patch,
        reversibility: "logical_restore",
        protected_target: false,
        effect_class: "local_write",
    };
    static READ_FILE: ToolDef = ToolDef {
        name: "read_file",
        description: "读取项目内相对路径文件内容（只读，路径不得逃逸项目根）",
        risk: Risk::Low,
        data_level: "internal",
        max_result_bytes: MIB,
        timeout_sec: 60,
        parameters: params_read_file,
        reversibility: "logical_restore",
        protected_target: false,
        effect_class: "read",
    };
    static RUN_COMMAND: ToolDef = ToolDef {
        name: "run_command",
        description: "按 argv 执行命令（无 shell；受执行模式约束，高风险需审批）",
        risk: Risk::High,
        data_level: "internal",
        max_result_bytes: MIB,
        timeout_sec: 120,
        parameters: params_run_command,
        reversibility: "manual",
        protected_target: false,
        effect_class: "local_write",
    };
    static SEARCH_KNOWLEDGE: ToolDef = ToolDef {
        name: "search_knowledge",
        description: "检索项目知识库（库内调用，免起进程）",
        risk: Risk::Low,
        data_level: "internal",
        max_result_bytes: MIB,
        timeout_sec: 30,
        parameters: params_search_knowledge,
        reversibility: "logical_restore",
        protected_target: false,
        effect_class: "read",
    };
    static WRITE_FILE: ToolDef = ToolDef {
        name: "write_file",
        description: "写入工件草稿区（不直接改工作区；内容过秘密扫描）",
        risk: Risk::Medium,
        data_level: "internal",
        max_result_bytes: MIB,
        timeout_sec: 60,
        parameters: params_write_file,
        reversibility: "logical_restore",
        protected_target: false,
        effect_class: "local_write",
    };
    // 字典序：apply_patch < read_file < run_command < search_knowledge < write_file
    vec![
        &APPLY_PATCH,
        &READ_FILE,
        &RUN_COMMAND,
        &SEARCH_KNOWLEDGE,
        &WRITE_FILE,
    ]
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
    /// P0-4：隔离 worktree 不可用且回退到用户 local_root 时置位——
    /// run_command 等可写工具被拒绝（蓝图 SG-RBK-005：可写执行必须在受管域内）。
    pub read_only: bool,
    /// Run 取消令牌（缺陷审计 P1-9）：在途子进程执行中置位即整组 kill，
    /// 取消不再等子进程跑满 timeout。装配层从 Run 注册表注入；None = 无取消面。
    pub cancel: Option<std::sync::Arc<sg_integrations::CancelToken>>,
}

/// 提案参数 → 受约束 ExecutionManifest（read_file / run_command）。
/// write_file/search_knowledge 不走进程，由装配层直接执行。
/// M3（ADR-034）：manifest 携带沙箱路径——读=work_dir，写=受管 worktree + 工件草稿区。
pub fn build_manifest(
    def: &ToolDef,
    args: &Value,
    ctx: &ToolCtx,
) -> Result<ExecutionManifest, String> {
    let sandbox_read_paths: Vec<String> = ctx
        .work_dir
        .as_ref()
        .map(|p| vec![p.to_string_lossy().to_string()])
        .unwrap_or_default();
    let sandbox_write_paths: Vec<String> = ctx
        .work_dir
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .into_iter()
        .chain(std::iter::once(
            ctx.artifacts_dir.to_string_lossy().to_string(),
        ))
        .collect();
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
                sandbox_read_paths,
                sandbox_write_paths: vec![ctx.artifacts_dir.to_string_lossy().to_string()],
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
                sandbox_read_paths,
                sandbox_write_paths,
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

/// ADR-033 M1：从同一 Registry 生成原生 function 定义（OpenAI 兼容形状）。
/// 禁止复制 Schema——直接序列化各 ToolDef 的 parameters()。
pub fn provider_tools_json(allowlist: &[String]) -> String {
    let defs: Vec<serde_json::Value> = registry()
        .iter()
        .filter(|d| allowlist.iter().any(|a| a == d.name))
        .map(|d| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": d.name,
                    "description": d.description,
                    "parameters": (d.parameters)(),
                },
            })
        })
        .collect();
    serde_json::to_string(&serde_json::Value::Array(defs)).unwrap_or_else(|_| "[]".into())
}

/// allowlist 工具 Schema 的 canonical digest（提案指纹与 Run 快照用）。
pub fn schema_digest(allowlist: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let defs: Vec<serde_json::Value> = registry()
        .iter()
        .filter(|d| allowlist.iter().any(|a| a == d.name))
        .map(|d| d.to_json())
        .collect();
    sg_store::ids::hex(&Sha256::digest(
        serde_json::to_string(&defs).unwrap_or_default().as_bytes(),
    ))
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
            read_only: false,
            cancel: None,
        }
    }

    #[test]
    fn registry_is_canonical_and_stable() {
        let names: Vec<&str> = registry().iter().map(|d| d.name).collect();
        assert_eq!(
            names,
            vec![
                "apply_patch",
                "read_file",
                "run_command",
                "search_knowledge",
                "write_file"
            ]
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
