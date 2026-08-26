//! F11/M4：契约 jsonschema 的运行时结构校验（零依赖手写，规则与
//! contracts/jsonschema/agent-decision / tool-definition 对齐；
//! 交叉校验单测保证校验器与 schema 文件不漂移）。
use serde_json::Value;

/// agent-decision：action=final 或 ^[a-z][a-z0-9_]{2,40}$；arguments 若存在须为 object；
/// summary 1..=4000（schema：required [action, summary]）。
pub fn validate_decision(action: &str, arguments: &Value, summary: &str) -> Result<(), String> {
    let tool_name_ok = action.len() >= 3
        && action.len() <= 41
        && action
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase())
        && action
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if action != "final" && !tool_name_ok {
        return Err(format!("action 不合法：{action:?}"));
    }
    // schema：arguments 可选；缺省（Null）视同空对象，存在时必须是 object。
    if !arguments.is_null() && !arguments.is_object() {
        return Err("arguments 必须是 object".into());
    }
    if summary.is_empty() || summary.chars().count() > 4000 {
        return Err("summary 长度须在 1..=4000".into());
    }
    Ok(())
}

/// tool-definition：name 模式、risk/data_level 枚举、timeoutSec 1..=600、
/// maxResultBytes 1..=32MiB、description 非空（schema required 全覆盖）。
pub fn validate_tool_def(
    name: &str,
    description: &str,
    risk: &str,
    data_level: &str,
    timeout_sec: i64,
    max_result_bytes: usize,
) -> Result<(), String> {
    let name_ok = name.len() >= 3
        && name.len() <= 41
        && name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !name_ok {
        return Err(format!("工具名不合法：{name:?}"));
    }
    if description.chars().count() < 10 {
        return Err(format!("工具 {name} description 过短（<10）"));
    }
    if !matches!(risk, "low" | "medium" | "high") {
        return Err(format!("工具 {name} risk 非法：{risk}"));
    }
    if !matches!(data_level, "public" | "internal" | "confidential") {
        return Err(format!("工具 {name} dataLevel 非法：{data_level}"));
    }
    if !(1..=600).contains(&timeout_sec) {
        return Err(format!("工具 {name} timeoutSec 越界：{timeout_sec}"));
    }
    if max_result_bytes == 0 || max_result_bytes > 32 << 20 {
        return Err(format!(
            "工具 {name} maxResultBytes 越界：{max_result_bytes}"
        ));
    }
    Ok(())
}

/// 注册表整体校验（装配层启动时调用；违例 fail-fast）。
pub fn validate_registry(defs: &[&crate::tools::ToolDef]) -> Result<(), String> {
    for def in defs {
        validate_tool_def(
            def.name,
            def.description,
            &format!("{:?}", def.risk).to_lowercase(),
            def.data_level,
            def.timeout_sec,
            def.max_result_bytes,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 交叉校验：schema 文件与手写校验器不漂移（required 集合/模式串一致）。
    #[test]
    fn validator_matches_schema_files() {
        let decision: Value = serde_json::from_str(include_str!(
            "../../../contracts/jsonschema/agent-decision.schema.json"
        ))
        .unwrap();
        let required = decision["required"].as_array().unwrap();
        assert_eq!(
            required
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["action", "summary"],
            "agent-decision required 集合变化需同步 validate_decision"
        );
        assert!(
            decision["properties"]["action"]["oneOf"][1]["pattern"]
                .as_str()
                .unwrap()
                .contains("[a-z][a-z0-9_]"),
            "action 模式变化需同步 validate_decision"
        );

        let tool: Value = serde_json::from_str(include_str!(
            "../../../contracts/jsonschema/tool-definition.schema.json"
        ))
        .unwrap();
        let required: Vec<&str> = tool["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(
            required.contains(&"name")
                && required.contains(&"risk")
                && required.contains(&"timeoutSec")
        );
        assert!(
            validate_registry(&crate::tools::registry()).is_ok(),
            "注册表过 tool-definition 校验"
        );
    }

    #[test]
    fn decision_rules() {
        assert!(validate_decision("final", &serde_json::json!({}), "done").is_ok());
        assert!(validate_decision("read_file", &serde_json::json!({"path": "a"}), "读").is_ok());
        assert!(validate_decision("Bad-Name", &serde_json::json!({}), "s").is_err());
        assert!(validate_decision("final", &serde_json::json!("x"), "s").is_err());
        assert!(validate_decision("final", &serde_json::json!({}), "").is_err());
    }
}
