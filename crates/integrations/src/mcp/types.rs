//! MCP 类型与 Schema canonical 化。
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// initialize 响应中的服务器身份。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpServerInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub protocol_version: String,
}

/// tools/list 中的工具描述。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDescriptor {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// inputSchema（JSON Schema 对象；canonical 化后生成 digest）。
    #[serde(default)]
    pub input_schema: serde_json::Value,
    /// MCP annotations.readOnlyHint（缺省 = 可能写 → 保守按写处理）。
    #[serde(default)]
    pub read_only_hint: bool,
}

/// tools/call 的结果三态：ok / is_error / unknown（超时）。
#[derive(Debug, Clone)]
pub enum McpToolCallOutcome {
    Ok {
        text: String,
        is_error: bool,
    },
    /// 超时：Provider 业务幂等键/查询能力缺失 → tool_outcome_unknown（禁自动重试）。
    Timeout,
}

/// 工具名约束（模型可见名 = mcp__{server}__{tool}，兼容 OpenAI function name 规则）。
pub fn valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// canonical Schema：排序键的最小化 JSON → sha256（跨探测稳定）。
pub fn canonical_schema(schema: &serde_json::Value) -> Result<(String, String), String> {
    if !schema.is_object() {
        return Err("schema 必须是 JSON 对象".into());
    }
    let canonical =
        canonicalize_value(schema).ok_or_else(|| "schema canonical 化失败".to_string())?;
    let text = serde_json::to_string(&canonical).map_err(|e| e.to_string())?;
    if text.len() > 64 * 1024 {
        return Err("schema 超 64KiB 上限（恶意/异常 Schema 拒绝）".into());
    }
    let hex: String = Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok((text, format!("sha256:{hex}")))
}

/// 递归排序对象键（BTreeMap 序）。
fn canonicalize_value(v: &serde_json::Value) -> Option<serde_json::Value> {
    use serde_json::Value;
    Some(match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                out.insert(k.clone(), canonicalize_value(map.get(k)?)?);
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(canonicalize_value)
                .collect::<Option<Vec<_>>>()?,
        ),
        other => other.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_schema_sorts_keys_and_digest_stable() {
        let a = json!({"type":"object","properties":{"b":{"type":"string"},"a":{"type":"number"}}});
        let b =
            json!({"properties":{"a":{"type": "number"},"b":{"type":"string"}},"type":"object"});
        let (ta, da) = canonical_schema(&a).unwrap();
        let (tb, db) = canonical_schema(&b).unwrap();
        assert_eq!(ta, tb, "键序 canonical");
        assert_eq!(da, db);
        assert!(da.starts_with("sha256:"));
        // 非对象拒绝。
        assert!(canonical_schema(&json!("x")).is_err());
        assert!(canonical_schema(&json!([])).is_err());
    }

    #[test]
    fn tool_name_validation() {
        assert!(valid_tool_name("query_db"));
        assert!(valid_tool_name("get-weather"));
        assert!(!valid_tool_name(""));
        assert!(!valid_tool_name("has space"));
        assert!(!valid_tool_name("中文名"));
    }
}
