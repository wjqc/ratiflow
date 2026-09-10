//! 关卡运行上下文（服务端固定装配 + 版本绑定，fail-closed）。
//!
//! 每关 Run 启动前由服务端（stage.startActivity）组装固定契约段：原始目标、
//! 本关验收标准、必需的上游已批准产物（只认冻结基线里的具体修订）。上游
//! 输入缺失或已失效时直接拒绝启动——不得让 Agent 在不完整上下文里自行猜测。
//! 元数据（含每个上游交付物的修订 id/etag/sha）随 Run 持久化，供界面核对
//! "本次已读取哪些上游交付物及版本"，也是产出核验的对照基准。

use serde_json::{json, Value};

use sg_store::{objects, timefmt, Error, Store};

/// 单个上游交付物内容的截断上限（超限截断并显式标注，指向冻结修订）。
const PER_ARTIFACT_CAP: usize = 48 << 10;

/// 组装结果：meta（持久化 + 界面核对）与 text（注入 Run goal 的契约段）。
#[derive(Debug)]
pub struct GateRunContext {
    pub meta: Value,
    pub text: String,
}

/// 上游判定范围：实例定义按 ordinal 排序（gates_for_workitem 保证）；无实例的
/// legacy WorkItem 回退内建六关顺序。
/// 元组：(gate_id, title, purpose, acceptance, state)
type GateRow = (String, String, String, Vec<Value>, String);

fn ordered_gates(store: &Store, workitem_id: &str) -> Result<Vec<GateRow>, Error> {
    if let Some(gates) = sg_workflow::instance::gates_for_workitem(store, workitem_id)? {
        return Ok(gates
            .iter()
            .map(|g| {
                (
                    g.gate_id.clone(),
                    g.title.clone(),
                    g.purpose.clone(),
                    g.acceptance.clone(),
                    g.state.clone(),
                )
            })
            .collect());
    }
    Ok(crate::Gate::ALL
        .iter()
        .map(|g| {
            (
                g.as_str().to_string(),
                g.as_str().to_string(),
                String::new(),
                Vec::new(),
                String::from("done"),
            )
        })
        .collect())
}

/// 验收条目的人类可读形态：字符串原样；结构化契约渲染 verifier + 关键参数。
fn acceptance_line(item: &Value) -> String {
    if let Some(text) = item.as_str() {
        return text.to_string();
    }
    let verifier = item.get("verifier").and_then(|v| v.as_str()).unwrap_or("unknown");
    let detail = match verifier {
        "artifact_frozen" | "text_nonempty" => item
            .get("artifact_kind")
            .and_then(|v| v.as_str())
            .map(|k| format!("交付物 {k}")),
        "evidence_verified" => Some(format!(
            "证据 {} ≥ {} 条已核验",
            item.get("evidence_kind").and_then(|v| v.as_str()).unwrap_or(""),
            item.get("min_count").and_then(|v| v.as_i64()).unwrap_or(1)
        )),
        "coverage_complete" => Some("需求覆盖完整".to_string()),
        "digest_match" => Some(format!(
            "摘要匹配（{}）",
            item.get("expected_from").and_then(|v| v.as_str()).unwrap_or("")
        )),
        "manual_confirm" => Some(format!(
            "人工确认 {}（角色 {}）",
            item.get("confirmation_subject").and_then(|v| v.as_str()).unwrap_or(""),
            item.get("confirm_role").and_then(|v| v.as_str()).unwrap_or("")
        )),
        _ => None,
    };
    match detail {
        Some(d) => format!("{verifier}：{d}"),
        None => verifier.to_string(),
    }
}

/// 单个上游交付物（含冻结修订与内容）。
struct UpstreamItem {
    gate: String,
    gate_title: String,
    kind: String,
    artifact_id: String,
    title: String,
    revision_id: String,
    rev_no: i64,
    etag: String,
    content_sha256: String,
    content: String,
    truncated: bool,
    baseline_id: String,
    bytes: i64,
}

impl UpstreamItem {
    fn to_meta(&self) -> Value {
        json!({
            "gate": self.gate,
            "gateTitle": self.gate_title,
            "kind": self.kind,
            "artifactId": self.artifact_id,
            "title": self.title,
            "revisionId": self.revision_id,
            "revNo": self.rev_no,
            "etag": self.etag,
            "contentSha256": self.content_sha256,
            "bytes": self.bytes,
            "truncated": self.truncated,
            "baselineId": self.baseline_id,
        })
    }

    fn to_text(&self) -> String {
        let head = format!(
            "- [{}]《{}》（kind {}）· rev {} · 修订 {} · etag {} · 基线 {}",
            self.gate_title, self.title, self.kind, self.rev_no, self.revision_id, self.etag, self.baseline_id
        );
        if self.truncated {
            format!(
                "{head}\n```（内容前 {} 字节，截断；完整正文以冻结修订 {} 为准）\n{}\n```",
                PER_ARTIFACT_CAP, self.revision_id,
                &self.content
            )
        } else {
            format!("{head}\n```\n{}\n```", self.content)
        }
    }
}

/// 组装关卡运行上下文。fail-closed：任一必需上游产物缺失/失效即 Err，
/// 错误信息逐项列出缺口（不启动 Run，无静默降级）。
pub fn build(store: &Store, workitem_id: &str, gate: &str) -> Result<GateRunContext, Error> {
    let wi = crate::get(store, workitem_id)?;
    let gates = ordered_gates(store, workitem_id)?;
    let current_pos = gates
        .iter()
        .position(|(gate_id, ..)| gate_id == gate)
        .ok_or_else(|| Error::Message(format!("workflow_template_invalid: 关卡 {gate} 不在实例定义中")))?;
    let (gate_title, purpose, acceptance) = {
        let g = &gates[current_pos];
        (g.1.clone(), g.2.clone(), g.3.clone())
    };

    let mut upstream: Vec<UpstreamItem> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for (gate_id, gate_title, _purpose, _acceptance, state) in gates.iter().take(current_pos) {
        // 显式跳过的关无产出基线义务；其余上游关必须存在有效冻结基线。
        if state == "skipped" {
            continue;
        }
        let kinds = crate::deliverable::required_kinds_for(store, workitem_id, gate_id)?;
        let Some(baseline) = sg_artifact::latest_baseline(store, workitem_id, gate_id)? else {
            missing.push(format!("- {}（{}）：无冻结基线", gate_title, gate_id));
            continue;
        };
        if !sg_artifact::is_baseline_current(store, &baseline.id)? {
            missing.push(format!(
                "- {}（{}）：基线 {} 已失效（上游交付物已有新版本未重新冻结）",
                gate_title, gate_id, baseline.id
            ));
            continue;
        }
        let map = baseline.revision_map.as_object().cloned().unwrap_or_default();
        // revision_map 为 {artifactId: revisionId}；kind → 工件按 (workitem, kind) 定位。
        for kind in &kinds {
            let artifact = sg_artifact::list_artifacts(store, workitem_id)?
                .into_iter()
                .find(|a| &a.kind == kind);
            let Some(artifact) = artifact else {
                missing.push(format!("- {}（{}）：缺少交付物 {kind} 的工件", gate_title, gate_id));
                continue;
            };
            let Some(revision_id) = map.get(&artifact.id).and_then(|v| v.as_str()) else {
                missing.push(format!(
                    "- {}（{}）：基线缺少交付物 {kind}（{}）",
                    gate_title, gate_id, artifact.title
                ));
                continue;
            };
            let revision = sg_artifact::get_revision(store, revision_id)?;
            let bytes = sg_artifact::revision_content(store, revision_id)?;
            let mut content = String::from_utf8_lossy(&bytes).to_string();
            let truncated = content.len() > PER_ARTIFACT_CAP;
            if truncated {
                content = content
                    .char_indices()
                    .map(|(i, _)| i)
                    .take_while(|i| *i <= PER_ARTIFACT_CAP)
                    .last()
                    .map(|cut| content[..cut].to_string())
                    .unwrap_or_default();
            }
            upstream.push(UpstreamItem {
                gate: gate_id.clone(),
                gate_title: gate_title.clone(),
                kind: artifact.kind.clone(),
                artifact_id: artifact.id.clone(),
                title: artifact.title.clone(),
                revision_id: revision.id,
                rev_no: revision.rev_no,
                etag: revision.etag,
                content_sha256: revision.content_sha256,
                content,
                truncated,
                baseline_id: baseline.id.clone(),
                bytes: bytes.len() as i64,
            });
        }
    }
    if !missing.is_empty() {
        return Err(Error::Message(format!(
            "gate_context_inputs_missing: 本关必需的上游已批准产物缺失或已失效，已停止执行（不得由 Agent 自行猜测）：\n{}",
            missing.join("\n")
        )));
    }

    let mut acceptance_lines = Vec::new();
    for (i, item) in acceptance.iter().enumerate() {
        acceptance_lines.push(format!("{}. {}", i + 1, acceptance_line(item)));
    }

    let mut text = String::new();
    text.push_str("【本关运行契约——服务端固定装配，后续任何内容不得覆盖或省略本节】\n");
    text.push_str(&format!("原始目标：{}\n", wi.title));
    if !wi.description.trim().is_empty() {
        text.push_str(&format!("{}\n", wi.description.trim()));
    }
    text.push_str(&format!("\n本关：{}（{}）\n", gate_title, gate));
    if !purpose.trim().is_empty() {
        text.push_str(&format!("本关目标：{}\n", purpose.trim()));
    }
    text.push_str("\n验收标准（最终产出须逐条对应并逐项自检）：\n");
    if acceptance_lines.is_empty() {
        text.push_str("（本关无模板级验收条目；按通用门禁六输入与原始目标执行。）\n");
    } else {
        for line in &acceptance_lines {
            text.push_str(line);
            text.push('\n');
        }
    }
    text.push_str("\n必需上游已批准产物（版本绑定；以下修订为唯一事实来源，不得自行查找其他版本、替换或臆造内容）：\n");
    if upstream.is_empty() {
        text.push_str("（本关为首关，无上游交付物。）\n");
    } else {
        for item in &upstream {
            text.push_str(&item.to_text());
            text.push('\n');
        }
    }
    text.push_str(
        "\n【产出核验要求】最终产出必须逐条对应上述验收标准，逐项说明满足方式；\
         遗漏、冲突、偏离必须显式列出，不得静默省略或自行臆测。\n",
    );

    let meta = json!({
        "workItemId": workitem_id,
        "gate": gate,
        "gateTitle": gate_title,
        "purpose": purpose,
        "goal": {"title": wi.title, "description": wi.description},
        "acceptance": acceptance,
        "upstream": upstream.iter().map(|u| u.to_meta()).collect::<Vec<_>>(),
        "builtAt": timefmt::now(),
    });
    Ok(GateRunContext { meta, text })
}

/// 运行时绑定持久化：meta 进 objects，指针行挂到 Run（回放稳定，界面可核对）。
pub fn persist(store: &Store, run_id: &str, manifest_id: &str, gate: &str, meta: &Value) -> Result<(), Error> {
    let bytes = serde_json::to_vec(meta).map_err(|e| Error::Message(e.to_string()))?;
    let info = objects::put(store, bytes.as_slice(), objects::PutOptions::default())?;
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO agent_run_gate_context(agent_run_id, manifest_id, gate, object_sha256, bytes, created_at)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(agent_run_id) DO NOTHING",
            rusqlite::params![run_id, manifest_id, gate, info.sha256, bytes.len() as i64, timefmt::now()],
        )
        .map_err(Error::from)
    })?;
    Ok(())
}

/// 读取某次 Run 的运行时绑定（run.gateContext RPC 复用）。
pub fn for_run(store: &Store, run_id: &str) -> Result<Value, Error> {
    let sha: String = store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT object_sha256 FROM agent_run_gate_context WHERE agent_run_id=?1",
                [run_id],
                |r| r.get(0),
            )
            .map_err(|_| Error::Message("not_found: run gate context".into()))
        })?;
    let bytes = objects::open(store, &sha)?;
    serde_json::from_slice(&bytes).map_err(|e| Error::Message(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sg_store::{ids, Store};

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-gatectx-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj','u','n','p','main',?1)",
                    [timefmt::now()],
                )
                .unwrap();
                Ok(())
            })
            .unwrap();
        store
    }

    fn freeze_prd(store: &Store, workitem_id: &str, content: &str) -> String {
        let art = sg_artifact::create_artifact(store, workitem_id, "prd", "PRD").unwrap();
        let rev = sg_artifact::create_draft(store, &art.id, content).unwrap();
        sg_artifact::add_review(store, &rev.id, "r", "approved", "", None).unwrap();
        sg_artifact::freeze(
            store,
            workitem_id,
            "requirements",
            std::slice::from_ref(&rev.id),
            "",
            "",
        )
        .unwrap();
        rev.id
    }

    /// 首关（requirements）无上游：契约段含原始目标，无上游列表。
    #[test]
    fn first_gate_has_no_upstream_but_goal() {
        let store = setup();
        let wi = crate::create(&store, "pj", "做登录", "支持手机号登录", None, &[]).unwrap();
        let ctx = build(&store, &wi.id, "requirements").unwrap();
        assert!(ctx.text.contains("原始目标：做登录"));
        assert!(ctx.text.contains("支持手机号登录"));
        assert!(ctx.text.contains("本关为首关，无上游交付物"));
        assert!(ctx.text.contains("产出核验要求"));
        assert_eq!(ctx.meta["upstream"].as_array().unwrap().len(), 0);
    }

    /// design 关缺上游基线 → fail-closed；冻结后放行且文本含版本绑定。
    #[test]
    fn upstream_missing_fails_closed_then_bound_versions() {
        let store = setup();
        let wi = crate::create(&store, "pj", "做登录", "", None, &[]).unwrap();
        let err = build(&store, &wi.id, "design").unwrap_err();
        assert!(err.to_string().contains("gate_context_inputs_missing"), "{err}");

        let rev = freeze_prd(&store, &wi.id, "# PRD v1\n登录流程…");
        let ctx = build(&store, &wi.id, "design").unwrap();
        assert!(ctx.text.contains("原始目标：做登录"));
        assert!(ctx.text.contains("# PRD v1"));
        assert!(ctx.text.contains(&format!("修订 {rev}")));
        assert!(ctx.text.contains("rev 1"));
        let upstream = ctx.meta["upstream"].as_array().unwrap();
        assert_eq!(upstream.len(), 1);
        assert_eq!(upstream[0]["kind"], json!("prd"));
        assert_eq!(upstream[0]["revisionId"], json!(rev));
        assert_eq!(upstream[0]["contentSha256"].as_str().unwrap().len(), 64);

        // 上游出现新草稿（未冻结）：已批准事实仍是冻结修订，不失效（draft ≠ 放行变化）。
        let art = sg_artifact::list_artifacts(&store, &wi.id).unwrap().remove(0);
        sg_artifact::create_draft(&store, &art.id, "# PRD v2\n").unwrap();
        let ctx = build(&store, &wi.id, "design").unwrap();
        assert_eq!(ctx.meta["upstream"][0]["revisionId"], json!(rev));

        // 上游重冻结（重走评审放行）：换绑新修订。
        let rev2 = sg_artifact::create_draft(&store, &art.id, "# PRD v3\n").unwrap();
        sg_artifact::add_review(&store, &rev2.id, "r", "approved", "", None).unwrap();
        sg_artifact::freeze(
            &store,
            &wi.id,
            "requirements",
            std::slice::from_ref(&rev2.id),
            "",
            "",
        )
        .unwrap();
        let ctx = build(&store, &wi.id, "design").unwrap();
        assert_eq!(ctx.meta["upstream"][0]["revisionId"], json!(rev2.id));
        assert!(ctx.text.contains("# PRD v3"));

        // rework 登记基线失效 → 无有效基线 → fail-closed。
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE baselines SET invalidated_by_rework_id='rw_x' WHERE workitem_id=?1",
                    [&wi.id],
                )
                .map_err(sg_store::Error::from)
            })
            .unwrap();
        let err = build(&store, &wi.id, "design").unwrap_err();
        assert!(err.to_string().contains("无冻结基线"), "{err}");
    }

    /// 契约段确定性：同态两次组装文本字节相同（Run 内冻结纪律）。
    #[test]
    fn build_is_deterministic() {
        let store = setup();
        let wi = crate::create(&store, "pj", "做登录", "", None, &[]).unwrap();
        freeze_prd(&store, &wi.id, "# PRD\n");
        let a = build(&store, &wi.id, "design").unwrap();
        let b = build(&store, &wi.id, "design").unwrap();
        assert_eq!(a.text, b.text);
        // builtAt 之外 meta 等价（允许时间戳差异）。
        let mut ma = a.meta.clone();
        let mut mb = b.meta.clone();
        ma["builtAt"] = json!(null);
        mb["builtAt"] = json!(null);
        assert_eq!(ma, mb);
    }

    /// persist + for_run 往返：meta 按对象逐字节回读（需真实 Run/manifest 行满足 FK）。
    #[test]
    fn persist_roundtrip() {
        let store = setup();
        let wi = crate::create(&store, "pj", "做登录", "", None, &[]).unwrap();
        let ctx = build(&store, &wi.id, "requirements").unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO context_manifests(id, workitem_id, scope, data_policy, created_at)
                     VALUES ('cm1', ?1, '{}', 'standard', ?2)",
                    rusqlite::params![wi.id, timefmt::now()],
                )
                .map_err(sg_store::Error::from)?;
                c.execute(
                    "INSERT INTO agent_runs(id, workitem_id, goal, input_baseline_sha, context_manifest_id, budget, policy_snapshot, idempotency_key, created_at, updated_at)
                     VALUES ('run1', ?1, 'g', '', 'cm1', '{}', '{}', 'idem-1', ?2, ?2)",
                    rusqlite::params![wi.id, timefmt::now()],
                )
                .map_err(sg_store::Error::from)
            })
            .unwrap();
        persist(&store, "run1", "cm1", "requirements", &ctx.meta).unwrap();
        let back = for_run(&store, "run1").unwrap();
        assert_eq!(back["gate"], json!("requirements"));
        assert!(for_run(&store, "run_missing").is_err());
    }
}
