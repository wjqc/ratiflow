//! Agent 工具执行接线（F05/M0-③）：注册表查找 → manifest/直连执行 → 截断。
//! read_file/run_command 走受约束执行；write_file 落工件草稿区；search_knowledge 库内调用。
use std::sync::Arc;

use serde_json::Value;
use sg_agent::tools::{self, ToolCtx};
use sg_store::Store;

/// 共享工具执行器句柄。
pub type SharedToolExecutor =
    Arc<dyn Fn(&sg_agent::Proposal) -> Result<String, String> + Send + Sync>;

/// 构建注入 execute_run 的工具执行器（Run 任务内使用，经 run_store 访问库）。
pub fn make_executor(ctx: ToolCtx, store: Arc<Store>, project_id: String) -> SharedToolExecutor {
    Arc::new(move |p: &sg_agent::Proposal| -> Result<String, String> {
        let def = tools::find(&p.tool).ok_or_else(|| format!("unknown tool: {}", p.tool))?;
        let args: Value = serde_json::from_str(&p.arguments).unwrap_or(Value::Null);
        let out = match p.tool.as_str() {
            "write_file" => tools::write_draft(&ctx, &args)?,
            "search_knowledge" => {
                let query = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .ok_or("missing argument: query")?;
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(5)
                    .clamp(1, 20);
                let hits = sg_knowledge::search_v2(&store, &project_id, query, false, limit)
                    .map_err(|e| e.to_string())?;
                serde_json::to_string(&hits).map_err(|e| e.to_string())?
            }
            _ => {
                // P0-4：隔离执行域不可用时拒绝可写命令（read_file/search_knowledge 只读放行）。
                if ctx.read_only && p.tool == "run_command" {
                    return Err(
                        "action_denied: 隔离 worktree 不可用，拒绝在用户主工作区执行命令".into(),
                    );
                }
                let manifest = tools::build_manifest(def, &args, &ctx)?;
                let result =
                    sg_executor::execute(ctx.mode, &manifest).map_err(|e| e.to_string())?;
                serde_json::to_string(&result).map_err(|e| e.to_string())?
            }
        };
        // 输出截断（F05）：进模型消息的是截断版。
        Ok(tools::truncate_output(&out, def.max_result_bytes))
    })
}
