//! 分层指令文件（F07/M2）：全局（<dataDir>/SixGates.md）→ 项目根 → 项目 docs/。
//! 越具体越靠后；单层/总字节上限截断；层内容命中高风险秘密整体拒绝（fail-closed）。
use std::path::Path;

use serde_json::json;

#[derive(Clone, Debug)]
pub struct InstructionSettings {
    /// 按序探测的文件名（默认 SixGates.md、AGENTS.md 兼容）。
    pub file_names: Vec<String>,
    pub max_total_bytes: usize,
    pub max_layer_bytes: usize,
}

impl Default for InstructionSettings {
    fn default() -> Self {
        Self {
            file_names: vec!["SixGates.md".into(), "AGENTS.md".into()],
            max_total_bytes: 32 * 1024,
            max_layer_bytes: 8 * 1024,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct LayerInfo {
    pub label: String,
    pub path: String,
    pub bytes: usize,
    pub truncated: bool,
}

/// 聚合生效层。返回 (注入文本, 层信息, 告警)；秘密层被拒会体现在告警与缺失中。
pub fn aggregate(
    data_dir: &Path,
    project_root: Option<&Path>,
    settings: &InstructionSettings,
) -> (String, Vec<LayerInfo>, Vec<String>) {
    let mut candidates: Vec<(String, std::path::PathBuf)> = Vec::new();
    for name in &settings.file_names {
        candidates.push((format!("global:{name}"), data_dir.join(name)));
        if let Some(root) = project_root {
            candidates.push((format!("project:{name}"), root.join(name)));
            candidates.push((format!("project:docs/{name}"), root.join("docs").join(name)));
        }
    }
    let mut text = String::new();
    let mut layers = Vec::new();
    let mut warnings = Vec::new();
    let mut total = 0usize;
    for (label, path) in candidates {
        if total >= settings.max_total_bytes {
            break;
        }
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        // fail-closed：高风险秘密 → 该层整体拒绝（不注入、不截断后注入）。
        let findings = sg_store::scan::scan(body.as_bytes());
        if sg_store::scan::has_high_risk(&findings) {
            warnings.push(format!("{label} 命中高风险秘密，已整体拒绝注入"));
            continue;
        }
        let mut content = body;
        let mut truncated = false;
        if content.len() > settings.max_layer_bytes {
            content = crate::tools::truncate_output(&content, settings.max_layer_bytes);
            truncated = true;
        }
        if total + content.len() > settings.max_total_bytes {
            let room = settings.max_total_bytes.saturating_sub(total);
            content = crate::tools::truncate_output(&content, room.max(1));
            truncated = true;
        }
        total += content.len();
        layers.push(LayerInfo {
            label: label.clone(),
            path: path.to_string_lossy().to_string(),
            bytes: content.len(),
            truncated,
        });
        text.push_str(&format!("--- {label} ---\n{content}\n\n"));
    }
    (text, layers, warnings)
}

/// 指令设置从 knowledge 默认设置 JSON 提取（缺省回落 Default）。
pub fn settings_from_json(v: &serde_json::Value) -> InstructionSettings {
    let def = InstructionSettings::default();
    InstructionSettings {
        file_names: v
            .get("instructionFileNames")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|a| !a.is_empty())
            .unwrap_or(def.file_names),
        max_total_bytes: v
            .get("maxInstructionBytes")
            .and_then(|x| x.as_u64())
            .map(|n| n.clamp(1024, 256 * 1024) as usize)
            .unwrap_or(def.max_total_bytes),
        max_layer_bytes: (def.max_total_bytes / 4).max(2048),
    }
}

pub fn layers_json(layers: &[LayerInfo]) -> serde_json::Value {
    json!(layers
        .iter()
        .map(|l| json!({
            "label": l.label, "path": l.path, "bytes": l.bytes, "truncated": l.truncated,
        }))
        .collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sg-instr-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        dir
    }

    #[test]
    fn aggregates_layers_in_order_with_markers() {
        let data = tmpdir("a");
        let proj = tmpdir("b");
        std::fs::write(data.join("SixGates.md"), "全局约定").unwrap();
        std::fs::write(proj.join("AGENTS.md"), "项目约定").unwrap();
        std::fs::write(proj.join("docs").join("AGENTS.md"), "文档约定").unwrap();
        let (text, layers, warns) = aggregate(&data, Some(&proj), &InstructionSettings::default());
        assert!(warns.is_empty());
        let labels: Vec<&str> = layers.iter().map(|l| l.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "global:SixGates.md",
                "project:AGENTS.md",
                "project:docs/AGENTS.md"
            ]
        );
        let global_pos = text.find("全局约定").unwrap();
        let project_pos = text.find("项目约定").unwrap();
        let docs_pos = text.find("文档约定").unwrap();
        assert!(
            global_pos < project_pos && project_pos < docs_pos,
            "越具体越靠后"
        );
    }

    #[test]
    fn secret_layer_rejected_and_truncation_marks() {
        let data = tmpdir("c");
        let proj = tmpdir("d");
        std::fs::write(
            proj.join("AGENTS.md"),
            format!("token glpat-{}end", "x".repeat(30)),
        )
        .unwrap();
        std::fs::write(data.join("SixGates.md"), "A".repeat(20_000)).unwrap();
        let (text, layers, warns) = aggregate(&data, Some(&proj), &InstructionSettings::default());
        assert!(!warns.is_empty(), "秘密层告警");
        assert!(text.contains("TRUNCATED"), "超限层截断标记");
        assert!(
            layers.iter().all(|l| l.label != "project:AGENTS.md"),
            "秘密层不注入"
        );
    }

    #[test]
    fn settings_fallbacks() {
        let s = settings_from_json(&serde_json::json!({}));
        assert_eq!(s.file_names, vec!["SixGates.md", "AGENTS.md"]);
        assert_eq!(s.max_total_bytes, 32 * 1024);
        let s2 = settings_from_json(&serde_json::json!({
            "instructionFileNames": ["TEAM.md"], "maxInstructionBytes": 8192
        }));
        assert_eq!(s2.file_names, vec!["TEAM.md"]);
        assert_eq!(s2.max_total_bytes, 8192);
    }
}
