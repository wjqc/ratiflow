//! 导入 / 导出（方案 §11）：派生 Markdown 产物不是权威；外部修改需重新导入。
//! 导出先写临时目录、逐文件 Secret 复检、fsync、rename；renderer 永远只拿业务 ID。
use serde_json::{json, Value};

use sg_store::{scan, Store};

use crate::model::{
    byte_len, fingerprint, merr, sha256_hex, CreateInput, DuplicateMode, SourceRefInput,
};
use crate::{mutation, repository};

const IMPORT_MAX_BYTES: i64 = 2 * 1024 * 1024;

/// 单个导入文档：(title, body, kind, tags)。
type ImportDoc = (String, String, String, Vec<String>);

/// 导入：用户显式选择 `.md/.markdown/.txt/.json`；不递归、不自动扫项目（MEM-022）。
/// mode = proposed | active；返回 created/proposed/duplicates 清单。
pub fn import(
    store: &Store,
    project_id: &str,
    filename: &str,
    content_base64: &str,
    mode: &str,
    idempotency_key: &str,
) -> Result<Value, sg_store::Error> {
    if !matches!(mode, "proposed" | "active") {
        return Err(merr(
            crate::model::err_tokens::INVALID_STATE,
            format!("非法导入模式 {mode}（仅 proposed/active）"),
        ));
    }
    use base64::Engine as _;
    let content = base64::engine::general_purpose::STANDARD
        .decode(content_base64.trim())
        .map_err(|_| {
            merr(
                crate::model::err_tokens::INVALID_STATE,
                "contentBase64 不是合法 base64",
            )
        })?;
    if content.len() as i64 > IMPORT_MAX_BYTES {
        return Err(merr(
            crate::model::err_tokens::QUOTA,
            format!("导入文件超过 {} 字节上限", IMPORT_MAX_BYTES),
        ));
    }
    let text = String::from_utf8(content).map_err(|_| {
        merr(
            crate::model::err_tokens::INVALID_STATE,
            "导入文件必须为 UTF-8",
        )
    })?;
    repository::settings_get(store, project_id)?;
    // 全文先行 Secret 扫描（fail-closed）。
    let findings = scan::scan(text.as_bytes());
    if scan::has_high_risk(&findings) {
        return Err(merr(
            crate::model::err_tokens::SECRET,
            "导入内容命中高风险秘密，已拒绝",
        ));
    }
    let content_digest = sha256_hex(text.as_bytes());
    let stem = std::path::Path::new(filename)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "import".into());

    // 解析文档：JSON 数组走 schema 校验；单文档正文 = 全文。
    let docs: Vec<ImportDoc> = if filename.to_lowercase().ends_with(".json") {
        parse_json_docs(&text)?
    } else {
        vec![(
            title_from_markdown(&text).unwrap_or_else(|| stem.clone()),
            text.clone(),
            String::new(),
            vec![],
        )]
    };

    let target_status: &str = if mode == "active" {
        "active"
    } else {
        "proposed"
    };
    let mut created: Vec<Value> = Vec::new();
    let mut duplicates: Vec<Value> = Vec::new();

    // 幂等收据在逐条写入前检查：重放导入返回原结果。
    if let Some(prev_json) = store.with_conn(|conn| -> Result<Option<String>, sg_store::Error> {
        let mut stmt = conn.prepare(
            "SELECT result_json FROM memory_mutation_receipts WHERE idempotency_key = ?1",
        )?;
        let mut rows = stmt.query_map([idempotency_key], |r| r.get::<_, String>(0))?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    })? {
        return serde_json::from_str(&prev_json)
            .map_err(|_| merr(crate::model::err_tokens::CONFLICT, "收据损坏"));
    }

    for (idx, (title, body, kind, tags)) in docs.iter().enumerate() {
        let kind = if KINDS.contains(&kind.as_str()) {
            kind.clone()
        } else {
            "fact".to_string()
        };
        let input = CreateInput {
            project_id: project_id.to_string(),
            title: title.trim().to_string(),
            kind,
            body: body.clone(),
            summary: None,
            tags: tags.clone(),
            source_refs: vec![SourceRefInput {
                source_kind: "import".into(),
                source_id: filename.to_string(),
                locator: filename.to_string(),
                source_digest: sha256_hex(body.as_bytes()),
                relation: "derived_from".into(),
            }],
            target_status,
            actor: "local".to_string(),
            idempotency_key: format!("{idempotency_key}-{idx}"),
            on_duplicate: DuplicateMode::Skip,
        };
        match mutation::create(store, &input) {
            Ok(result) => {
                if result.get("skippedDuplicateOf").is_some() {
                    duplicates.push(json!({"title": title, "reason": "duplicate"}));
                } else {
                    created.push(result);
                }
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.starts_with(crate::model::err_tokens::SECRET) {
                    // 单文档命中秘密：整批拒绝（fail-closed，不静默跳过）。
                    store.with_conn(|conn| {
                        conn.execute(
                            "DELETE FROM memory_mutation_receipts WHERE idempotency_key LIKE ?1",
                            [format!("{idempotency_key}-%")],
                        )?;
                        Ok(())
                    })?;
                    return Err(e);
                }
                duplicates.push(json!({"title": title, "reason": "rejected", "detail": msg.chars().take(120).collect::<String>()}));
            }
        }
    }

    let result = json!({
        "operation": {"kind": "memory.import", "status": "succeeded", "total": docs.len()},
        "projectId": project_id,
        "mode": mode,
        "created": created,
        "duplicates": duplicates,
    });
    store.with_conn(|conn| {
        mutation::receipt_put(
            conn,
            idempotency_key,
            project_id,
            "memory.import",
            project_id,
            &fingerprint(
                &json!({"contentSha256": content_digest, "mode": mode, "projectId": project_id}),
            ),
            &result,
        )
    })?;
    sg_store::audit::append(
        store,
        "local",
        "memory.import",
        "memory",
        project_id,
        json!({"created": created.len(), "duplicates": duplicates.len(), "bytes": byte_len(&text)}),
    )?;
    Ok(result)
}

const KINDS: [&str; 5] = crate::model::KINDS;

fn parse_json_docs(text: &str) -> Result<Vec<ImportDoc>, sg_store::Error> {
    let parsed: Value = serde_json::from_str(text)
        .map_err(|_| merr(crate::model::err_tokens::INVALID_STATE, "JSON 解析失败"))?;
    let list = match parsed {
        Value::Array(items) => items,
        Value::Object(_) => vec![parsed],
        _ => {
            return Err(merr(
                crate::model::err_tokens::INVALID_STATE,
                "JSON 导入须为对象或数组",
            ))
        }
    };
    let mut out = Vec::new();
    for item in list.into_iter().take(50) {
        let title = item["title"].as_str().unwrap_or_default().to_string();
        let body = item["body"].as_str().unwrap_or_default().to_string();
        if title.trim().is_empty() || body.trim().is_empty() {
            return Err(merr(
                crate::model::err_tokens::INVALID_STATE,
                "JSON 文档缺 title/body",
            ));
        }
        let kind = item["kind"].as_str().unwrap_or("fact").to_string();
        let tags = item["tags"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        out.push((title, body, kind, tags));
    }
    Ok(out)
}

fn title_from_markdown(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("# ").map(str::to_string))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// 导出：`<dataDir>/exports/memory/<exportId>/`；逐文件 Secret 复检；fsync + rename（§11.2）。
pub fn export(
    store: &Store,
    project_id: &str,
    memory_ids: Option<&[String]>,
    include_archived: bool,
) -> Result<Value, sg_store::Error> {
    let entries: Vec<Value> = store.with_conn(|conn| {
        repository::require_project(conn, project_id)?;
        let (where_ids, params): (String, Vec<Box<dyn rusqlite::ToSql>>) = match memory_ids {
            Some(ids) if !ids.is_empty() => (
                format!(" AND e.id IN ({})", vec!["?"; ids.len()].join(",")),
                {
                    let mut v: Vec<Box<dyn rusqlite::ToSql>> =
                        vec![Box::new(project_id.to_string())];
                    for id in ids {
                        v.push(Box::new(id.clone()));
                    }
                    v
                },
            ),
            _ => (String::new(), vec![Box::new(project_id.to_string())]),
        };
        let status_sql = if include_archived {
            "AND e.status IN ('active','conflicted','archived','proposed')"
        } else {
            "AND e.status IN ('active','conflicted','proposed')"
        };
        let sql = format!(
            "SELECT e.id FROM memory_entries e
             WHERE e.project_id = ?1 {status_sql} {where_ids}
             ORDER BY e.updated_at DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
            r.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(json!(row?));
        }
        Ok(out)
    })?;

    let export_id = sg_store::ids::new_id("memexp");
    let base = store.data_dir.join("exports").join("memory");
    let tmp_dir = base.join(format!(".tmp-{export_id}"));
    let final_dir = base.join(&export_id);
    std::fs::create_dir_all(&tmp_dir)?;

    let mut index_entries = Vec::new();
    let mut exported = 0i64;
    for entry_id in entries.iter().filter_map(|v| v.as_str()) {
        let detail = repository::detail(store, project_id, entry_id, None)?;
        let body = detail["body"].as_str().unwrap_or_default();
        // 逐文件 Secret 复检（MEM-023）。
        let findings = scan::scan(body.as_bytes());
        if scan::has_high_risk(&findings) {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(merr(
                crate::model::err_tokens::SECRET,
                format!("条目 {entry_id} 正文命中高风险秘密，导出已中止"),
            ));
        }
        let slug = detail["slug"].as_str().unwrap_or(entry_id);
        let tags = detail["tags"].as_array().cloned().unwrap_or_default();
        let tag_list = tags
            .iter()
            .filter_map(|t| t.as_str())
            .map(|t| format!("  - {t}"))
            .collect::<Vec<_>>()
            .join("\n");
        let front = format!(
            "---\nid: {}\nrevisionNo: {}\nkind: {}\nstatus: {}\nupdatedAt: {}\ntags:\n{}\n---\n\n",
            detail["id"].as_str().unwrap_or_default(),
            detail["revisionNo"].as_i64().unwrap_or(0),
            detail["kind"].as_str().unwrap_or_default(),
            detail["status"].as_str().unwrap_or_default(),
            detail["updatedAt"].as_str().unwrap_or_default(),
            tag_list,
        );
        let file_path = tmp_dir.join(format!("{slug}.md"));
        write_fsync(&file_path, &format!("{front}{body}"))?;
        index_entries.push(json!({
            "id": detail["id"],
            "slug": slug,
            "kind": detail["kind"],
            "status": detail["status"],
            "revisionNo": detail["revisionNo"],
            "contentSha256": detail["contentSha256"],
            "sources": detail["sources"],
            "file": format!("{slug}.md"),
        }));
        exported += 1;
    }
    let index = json!({
        "exportId": export_id,
        "projectId": project_id,
        "exportedAt": sg_store::timefmt::now(),
        "count": exported,
        "entries": index_entries,
    });
    write_fsync(
        &tmp_dir.join("_index.json"),
        &String::from_utf8_lossy(&serde_json::to_vec_pretty(&index).unwrap_or_default()),
    )?;
    write_fsync(
        &tmp_dir.join("_README.txt"),
        "本目录为项目记忆的派生 Markdown 导出，不是权威数据。\n外部修改不会回写 SixGates；如需更新，请通过 S12 页面重新导入或编辑。\n清除（purge）后的条目不会出现在导出中。\n",
    )?;
    std::fs::rename(&tmp_dir, &final_dir)?;

    sg_store::audit::append(
        store,
        "local",
        "memory.export",
        "memory",
        project_id,
        json!({"exportId": export_id, "count": exported}),
    )?;
    Ok(json!({
        "operation": {"kind": "memory.export", "status": "succeeded", "total": exported},
        "exportId": export_id,
        "count": exported,
    }))
}

fn write_fsync(path: &std::path::Path, content: &str) -> Result<(), sg_store::Error> {
    use std::io::Write as _;
    let mut file = std::fs::File::create(path)?;
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    Ok(())
}
