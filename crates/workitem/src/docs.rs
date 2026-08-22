//! 需求文档（工作目录 data/docs/{workItemId}/）：v2 行为等价的文件落盘。
use std::path::Path;

use sg_store::{Error, Store};

pub fn doc_dir(store: &Store) -> std::path::PathBuf {
    store.data_dir.join("docs")
}

/// 保存文档：路径逃逸防护 + 临时文件原子落位。返回相对路径。
pub fn save(store: &Store, workitem_id: &str, filename: &str, content: &str) -> Result<String, Error> {
    if workitem_id.is_empty() || filename.is_empty() {
        return Err(Error::Message("workitem id and filename required".into()));
    }
    let clean = sanitize(filename)?;
    let dir = doc_dir(store).join(workitem_id);
    std::fs::create_dir_all(&dir)?;
    let target = dir.join(&clean);
    let tmp = dir.join(format!("{clean}.tmp"));
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, &target)?;
    Ok(format!("docs/{workitem_id}/{clean}"))
}

pub fn read(store: &Store, workitem_id: &str, filename: &str) -> Result<String, Error> {
    let clean = sanitize(filename)?;
    std::fs::read_to_string(doc_dir(store).join(workitem_id).join(&clean))
        .map_err(|_| Error::Message("doc_not_found".into()))
}

pub fn list(store: &Store, workitem_id: &str) -> Result<Vec<String>, Error> {
    let dir = doc_dir(store).join(workitem_id);
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".tmp") {
                names.push(name);
            }
        }
    }
    names.sort();
    Ok(names)
}

fn sanitize(filename: &str) -> Result<String, Error> {
    let path = Path::new(filename);
    if path.is_absolute() || filename.contains("..") || filename.contains('/') && !path.components().all(|c| c.as_os_str().to_string_lossy() != "..") {
        return Err(Error::Message(format!("invalid filename {filename:?}")));
    }
    let clean = path.file_name().ok_or_else(|| Error::Message("invalid filename".into()))?;
    Ok(clean.to_string_lossy().to_string())
}

/// 从文档正文/文件名推导标题：首行 H1 优先。
pub fn title_from_document(filename: &str, content: &str) -> String {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(h1) = trimmed.strip_prefix("# ") {
            return h1.trim().to_string();
        }
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            break;
        }
    }
    let base = Path::new(filename).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    base.strip_suffix(".md").or_else(|| base.strip_suffix(".txt")).unwrap_or(&base).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rejects_escape() {
        assert!(sanitize("../etc/passwd").is_err());
        assert!(sanitize("/abs/path").is_err());
        assert_eq!(sanitize("requirement.md").unwrap(), "requirement.md");
    }

    #[test]
    fn title_from_h1_or_filename() {
        assert_eq!(title_from_document("a.md", "# 支持单点登录\n\n正文"), "支持单点登录");
        assert_eq!(title_from_document("需求-支付对账.md", "没有标题的正文"), "需求-支付对账");
    }
}
