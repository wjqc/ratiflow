//! 多模态附件（F03）：本地导入 → objects → 解析状态机。
//! 图片只有模型声明支持视觉才可解析；失败保留附件可补文字继续。

use serde::Serialize;
use sg_store::{ids, objects, outbox, timefmt, Error, Store};

#[derive(Debug, Clone, Serialize)]
pub struct Attachment {
    pub id: String,
    pub workitem_id: String,
    pub kind: String,
    pub filename: String,
    pub content_type: String,
    pub object_sha256: String,
    pub size: i64,
    pub parse_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extracted_object_sha256: Option<String>,
    pub created_at: String,
}

/// 导入附件：内容进 objects（类型嗅探 + 大小限制 + 秘密扫描）。
pub fn import(
    store: &Store,
    workitem_id: &str,
    filename: &str,
    content: &[u8],
    opts: objects::PutOptions,
) -> Result<Attachment, Error> {
    if filename.is_empty() {
        return Err(Error::Message("filename required".into()));
    }
    let info = objects::put(store, content, opts)?;
    let kind = if info.content_type.starts_with("image/") {
        "image"
    } else {
        "document"
    };
    let id = ids::new_id("att");
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO attachments(id, workitem_id, kind, filename, content_type, object_sha256, size, parse_state, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,'pending',?8)",
            rusqlite::params![id, workitem_id, kind, filename, info.content_type, info.sha256, info.size, now],
        )?;
        Ok(())
    })?;
    outbox::emit(
        store,
        "workitem",
        workitem_id,
        "attachment.imported",
        serde_json::json!({"attachmentId": id, "kind": kind, "filename": filename}),
    )?;
    get(store, &id)
}

pub fn get(store: &Store, id: &str) -> Result<Attachment, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT id, workitem_id, kind, filename, content_type, object_sha256, size, parse_state, COALESCE(extracted_object_sha256,''), created_at
             FROM attachments WHERE id=?1",
            [id],
            |r| {
                let extracted: String = r.get(8)?;
                Ok(Attachment {
                    id: r.get(0)?, workitem_id: r.get(1)?, kind: r.get(2)?, filename: r.get(3)?,
                    content_type: r.get(4)?, object_sha256: r.get(5)?, size: r.get(6)?,
                    parse_state: r.get(7)?,
                    extracted_object_sha256: if extracted.is_empty() { None } else { Some(extracted) },
                    created_at: r.get(9)?,
                })
            },
        )
        .map_err(|_| Error::Message("attachment_not_found".into()))
    })
}

pub fn list(store: &Store, workitem_id: &str) -> Result<Vec<Attachment>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, workitem_id, kind, filename, content_type, object_sha256, size, parse_state, COALESCE(extracted_object_sha256,''), created_at
             FROM attachments WHERE workitem_id=?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map([workitem_id], |r| {
            let extracted: String = r.get(8)?;
            Ok(Attachment {
                id: r.get(0)?, workitem_id: r.get(1)?, kind: r.get(2)?, filename: r.get(3)?,
                content_type: r.get(4)?, object_sha256: r.get(5)?, size: r.get(6)?,
                parse_state: r.get(7)?,
                extracted_object_sha256: if extracted.is_empty() { None } else { Some(extracted) },
                created_at: r.get(9)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

/// 解析状态推进：parsed（带提取文本对象）/ failed / vision_unsupported。
/// 解析失败不删除原附件（可人工补充文字继续需求关）。
pub fn set_parse_result(
    store: &Store,
    id: &str,
    state: &str,
    extracted_text: Option<&str>,
    error: &str,
) -> Result<(), Error> {
    if !matches!(state, "parsed" | "failed" | "vision_unsupported") {
        return Err(Error::Message(format!("invalid parse state {state}")));
    }
    let mut extracted_sha: Option<String> = None;
    if let Some(text) = extracted_text {
        if state == "parsed" {
            let info = objects::put(store, text.as_bytes(), objects::PutOptions::default())?;
            extracted_sha = Some(info.sha256);
        }
    }
    let changed = store.with_conn(|conn| {
        conn.execute(
            "UPDATE attachments SET parse_state=?1, extracted_object_sha256=COALESCE(?2, extracted_object_sha256), error=?3 WHERE id=?4",
            rusqlite::params![state, extracted_sha, error, id],
        )?;
        Ok(conn.changes())
    })?;
    if changed == 0 {
        return Err(Error::Message("attachment_not_found".into()));
    }
    Ok(())
}

/// 仅删除关系；对象按保留策略回收。
pub fn remove(store: &Store, id: &str) -> Result<(), Error> {
    let changed = store.with_conn(|conn| {
        conn.execute("DELETE FROM attachments WHERE id=?1", [id])?;
        Ok(conn.changes())
    })?;
    if changed == 0 {
        return Err(Error::Message("attachment_not_found".into()));
    }
    Ok(())
}

pub fn open_object(store: &Store, id: &str) -> Result<Vec<u8>, Error> {
    let att = get(store, id)?;
    objects::open(store, &att.object_sha256)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-att-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        store.with_conn(|c| {
            c.execute("INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at) VALUES ('pj','u','n','p','main',?1)", [timefmt::now()])?;
            c.execute("INSERT INTO workitems(id, project_id, title, created_at, updated_at) VALUES ('wi','pj','t',?1,?1)", [timefmt::now()])?;
            Ok(())
        }).unwrap();
        store
    }

    #[test]
    fn import_document_and_image_kind() {
        let s = setup();
        let doc = import(
            &s,
            "wi",
            "需求.md",
            "# 需求\n正文".as_bytes(),
            objects::PutOptions::default(),
        )
        .unwrap();
        assert_eq!(doc.kind, "document");
        let png_header = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];
        let img = import(
            &s,
            "wi",
            "截图.png",
            &png_header,
            objects::PutOptions::default(),
        )
        .unwrap();
        assert_eq!(img.kind, "image");
        assert_eq!(img.content_type, "image/png");
        assert_eq!(list(&s, "wi").unwrap().len(), 2);
    }

    #[test]
    fn parse_states_keep_attachment() {
        let s = setup();
        let att = import(
            &s,
            "wi",
            "原型.png",
            &[0x89, b'P', b'N', b'G', 1, 2, 3],
            objects::PutOptions::default(),
        )
        .unwrap();
        set_parse_result(
            &s,
            &att.id,
            "vision_unsupported",
            None,
            "provider 无视觉能力",
        )
        .unwrap();
        let after = get(&s, &att.id).unwrap();
        assert_eq!(after.parse_state, "vision_unsupported");
        assert!(after.extracted_object_sha256.is_none());
        assert!(!open_object(&s, &att.id).unwrap().is_empty());
        set_parse_result(
            &s,
            &att.id,
            "parsed",
            Some("登录页原型：账号密码 + SSO 按钮"),
            "",
        )
        .unwrap();
        assert_eq!(get(&s, &att.id).unwrap().parse_state, "parsed");
    }
}
