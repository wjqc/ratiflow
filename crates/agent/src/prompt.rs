//! Prompt 分层装配（F06/M2）：system 指令+工具清单 → developer 权限边界 → user 知识层 → user goal。
//! 纪律：Run 内各段冻结（同构重建得到字节相同的结果），历史只追加——提供商前缀缓存友好。
use sg_integrations::model::ChatMessage;

/// Run 的执行边界（developer 段与装配环境）。
#[derive(Clone, Debug, Default)]
pub struct PromptEnv {
    pub mode: Option<sg_executor::Mode>,
    /// 项目根标签（无 local_root 时为 None）。
    pub work_dir_label: Option<String>,
    /// F10/M3：需人工审批的工具（来自运行时权限快照；模型自知边界）。
    pub requires_approval_tools: Vec<String>,
}

/// 一次 Run 的初始装配：system 进 request.system_prompt，prefix 进 messages。
#[derive(Clone, Debug)]
pub struct InitialTurn {
    pub system_prompt: String,
    pub prefix: Vec<ChatMessage>,
}

/// system 段：基础指令 + 工具清单（注册表 canonical 序，按 allowlist 过滤）。
pub fn base_system_prompt(allowlist: &[String]) -> String {
    let mut lines = String::from(
        "你是 SixGates 交付 Agent。每轮输出一个 JSON 对象：\
         {\"action\":\"<tool|final>\",\"arguments\":{...},\"summary\":\"...\"}。\
         完成任务时 action=final 并在 summary 给出结果。你不能直接执行工具；系统会校验并执行提案。\n可用工具：",
    );
    for def in crate::tools::registry() {
        if allowlist.iter().any(|a| a == def.name) {
            lines.push_str(&format!(
                "\n- {}（风险 {:?}）：{}",
                def.name,
                format!("{:?}", def.risk).to_lowercase(),
                def.description
            ));
        }
    }
    lines
}

/// developer 段：模型看得见自己的权限边界（执行模式/网络/审批/work_dir）。
pub fn boundary_content(env: &PromptEnv) -> String {
    let mode = match env.mode {
        Some(sg_executor::Mode::Docker) => "docker 隔离",
        Some(sg_executor::Mode::SafeRestricted) => "安全受限（只读白名单）",
        Some(sg_executor::Mode::UnsafeExplicit) => "显式不安全（用户已确认）",
        Some(sg_executor::Mode::Disabled) | None => "禁用",
    };
    format!(
        "执行边界：模式 {mode}；网络禁用；高风险工具需人工审批；工作目录 {}。\
         你只能通过工具提案行动，越界提案会被策略拒绝。",
        env.work_dir_label
            .as_deref()
            .unwrap_or("未登记（需项目根的工具不可用）")
    )
}

/// 知识层（F07 指令文件 + F08 manifest 块）；两者皆空时注入占位（保持结构稳定）。
pub fn knowledge_text(instruction_layers: &str, manifest_blocks: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !instruction_layers.trim().is_empty() {
        parts.push(format!("【项目指令】\n{instruction_layers}"));
    }
    if !manifest_blocks.trim().is_empty() {
        parts.push(format!("【相关知识】\n{manifest_blocks}"));
    }
    if parts.is_empty() {
        "（无附加项目指令与相关知识）".into()
    } else {
        parts.join("\n\n")
    }
}

/// 完整装配（execute_run 的 InitialTurn；恢复路径 prefix 以 checkpoint 为准、system 仍取此处）。
pub fn assemble(env: &PromptEnv, allowlist: &[String], knowledge: &str, goal: &str) -> InitialTurn {
    assemble_with_profile(env, allowlist, knowledge, goal, None)
}

/// M4：profile developer 层（persona/版本/SOP/输出契约）紧随边界层之后，Run 内冻结。
pub fn assemble_with_profile(
    env: &PromptEnv,
    allowlist: &[String],
    knowledge: &str,
    goal: &str,
    profile_text: Option<&str>,
) -> InitialTurn {
    let mut prefix = vec![ChatMessage {
        role: "developer".into(),
        content: boundary_content(env),
    }];
    if let Some(text) = profile_text.filter(|t| !t.trim().is_empty()) {
        prefix.push(ChatMessage {
            role: "developer".into(),
            content: text.to_string(),
        });
    }
    let rest = assemble_rest(env, knowledge, goal);
    prefix.extend(rest);
    InitialTurn {
        system_prompt: base_system_prompt(allowlist),
        prefix,
    }
}

fn assemble_rest(_env: &PromptEnv, knowledge: &str, goal: &str) -> Vec<ChatMessage> {
    vec![
        ChatMessage {
            role: "user".into(),
            content: knowledge.to_string(),
        },
        ChatMessage {
            role: "user".into(),
            content: goal.to_string(),
        },
    ]
}

/// 段落字节统计（rollout instructions_assembled / context.instructions 用）。
pub fn segment_bytes(initial: &InitialTurn) -> serde_json::Value {
    serde_json::json!({
        "system": initial.system_prompt.len(),
        "boundary": initial.prefix.first().map(|m| m.content.len()).unwrap_or(0),
        "knowledge": initial.prefix.get(1).map(|m| m.content.len()).unwrap_or(0),
        "goal": initial.prefix.get(2).map(|m| m.content.len()).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembly_order_and_placeholders() {
        let env = PromptEnv {
            mode: Some(sg_executor::Mode::SafeRestricted),
            work_dir_label: Some("/tmp/proj".into()),
            requires_approval_tools: vec!["run_command".into()],
        };
        let initial = assemble(
            &env,
            &["read_file".into(), "run_command".into()],
            &knowledge_text("约定 A", "块 B"),
            "完成分析",
        );
        assert!(initial.system_prompt.contains("read_file"));
        assert!(
            !initial.system_prompt.contains("write_file"),
            "仅 allowlist 工具进清单"
        );
        assert_eq!(initial.prefix.len(), 3);
        assert_eq!(initial.prefix[0].role, "developer");
        assert!(initial.prefix[0].content.contains("安全受限"));
        assert!(
            initial.prefix[1].content.contains("约定 A")
                && initial.prefix[1].content.contains("块 B")
        );
        assert_eq!(initial.prefix[2].content, "完成分析");
        // 空知识占位保持结构稳定
        let empty = assemble(&env, &["read_file".into()], &knowledge_text("", ""), "g");
        assert!(empty.prefix[1].content.contains("无附加项目指令"));
    }

    #[test]
    fn assembly_is_deterministic_and_prefix_stable() {
        // 缓存前缀纪律回归：同构重建字节相同；迭代追加后消息序列化保持前缀。
        let env = PromptEnv {
            mode: Some(sg_executor::Mode::Docker),
            work_dir_label: Some("/w".into()),
            requires_approval_tools: vec![],
        };
        let allow = vec!["read_file".into()];
        let a = assemble(&env, &allow, "K", "G");
        let b = assemble(&env, &allow, "K", "G");
        assert_eq!(
            a.system_prompt, b.system_prompt,
            "system 段冻结（字节相同）"
        );
        assert_eq!(
            serde_json::to_string(&a.prefix).unwrap(),
            serde_json::to_string(&b.prefix).unwrap()
        );

        // 元素级 append-only：迭代只追加、不改写——旧消息子列序列化与新列中对应切片字节相等。
        // （JSON 数组闭括号使"整串前缀"恒假；提供商前缀缓存看到的是消息流，对应此切片不变式。）
        let mut grown = a.prefix.clone();
        grown.push(ChatMessage {
            role: "assistant".into(),
            content: "tool read_file({})".into(),
        });
        grown.push(ChatMessage {
            role: "tool".into(),
            content: "content".into(),
        });
        let s_small = serde_json::to_string(&a.prefix).unwrap();
        let s_slice = serde_json::to_string(&grown[..a.prefix.len()]).unwrap();
        assert_eq!(s_small, s_slice, "历史不改写：旧消息切片序列化字节相等");
        assert_eq!(grown.len(), a.prefix.len() + 2);
    }
}
