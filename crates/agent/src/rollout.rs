//! Rollout 会话日志（F04/M1）：每 Run 一条 append-only JSONL（`<dataDir>/logs/runs/<runId>.jsonl`）。
//! 只写脱敏后文本；落盘前逐行秘密复检，命中即整行 data 置换为 redacted 标记（fail-closed，信封保持可解析）。
//! Run 结束 fsync + 增量 sha256；审计行由装配层写入（路径/行数/哈希）。崩溃安全：行级 append + 每行 flush，无半行。
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub struct Rollout {
    file: std::fs::File,
    pub path: std::path::PathBuf,
    seq: i64,
    lines: i64,
    hash: Sha256,
}

impl Rollout {
    pub fn open(data_dir: &std::path::Path, run_id: &str) -> std::io::Result<Self> {
        let dir = data_dir.join("logs").join("runs");
        std::fs::create_dir_all(&dir)?;
        // run_id 由 core 生成（run_ 前缀 + hex），此处仅防御性清洗文件名。
        let safe: String = run_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = dir.join(format!("{safe}.jsonl"));
        // 恢复场景：同一 Run 追加写，seq/行数/哈希从既有内容继续。
        let (seq, lines, hash) = match std::fs::read(&path) {
            Ok(existing) => {
                let n = existing.iter().filter(|b| **b == b'\n').count() as i64;
                let mut h = Sha256::new();
                h.update(&existing);
                (n, n, h)
            }
            Err(_) => (0, 0, Sha256::new()),
        };
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        Ok(Self {
            file,
            path,
            seq,
            lines,
            hash,
        })
    }

    /// Run 的 rollout 路径（agent.get 摘要用）。
    pub fn path_for(data_dir: &std::path::Path, run_id: &str) -> std::path::PathBuf {
        let safe: String = run_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        data_dir
            .join("logs")
            .join("runs")
            .join(format!("{safe}.jsonl"))
    }

    /// 追加一行（失败不中断 Run：观测数据可用性优先，错误上抛由调用方记录 stderr）。
    pub fn append(&mut self, kind: &str, data: Value) -> std::io::Result<()> {
        let mut data = data;
        // 逐行秘密复检（fail-closed）：命中高风险即整行 data 置换，信封仍可解析。
        let probe = serde_json::to_string(&data).unwrap_or_default();
        let findings = sg_store::scan::scan(probe.as_bytes());
        if sg_store::scan::has_high_risk(&findings) {
            let kinds: Vec<String> = findings.iter().map(|f| f.kind.clone()).collect();
            data = json!({"redacted": true, "reason": "secret_detected", "kinds": kinds});
        }
        self.seq += 1;
        let line = json!({
            "seq": self.seq,
            "ts": sg_store::timefmt::now(),
            "kind": kind,
            "data": data,
        });
        let mut body = serde_json::to_string(&line).unwrap_or_default();
        body.push('\n');
        use std::io::Write;
        self.file.write_all(body.as_bytes())?;
        self.file.flush()?;
        self.hash.update(body.as_bytes());
        self.lines += 1;
        Ok(())
    }

    /// 收尾：fsync 并返回（行数, 全文 sha256）。
    pub fn finish(mut self) -> std::io::Result<(i64, String)> {
        use std::io::Write;
        self.file.flush()?;
        self.file.sync_all()?;
        Ok((
            self.lines,
            sg_store::ids::hex(&self.hash.clone().finalize()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_envelope_and_redacts_secrets() {
        let dir = std::env::temp_dir().join(format!("sg-rollout-{}", std::process::id()));
        let mut r = Rollout::open(&dir, "run_test1").unwrap();
        r.append("run_started", serde_json::json!({"goal": "分析"}))
            .unwrap();
        r.append(
            "tool_result",
            serde_json::json!({"preview": "glpat-abcdefghijklmnopqrstuvwxyz1234567890"}),
        )
        .unwrap();
        let (lines, sha) = r.finish().unwrap();
        assert_eq!(lines, 2);
        assert!(!sha.is_empty());
        let body = std::fs::read_to_string(dir.join("logs/runs/run_test1.jsonl")).unwrap();
        let mut kinds = Vec::new();
        for line in body.lines() {
            let v: Value = serde_json::from_str(line).unwrap();
            assert!(v["seq"].as_i64().is_some() && v["ts"].is_string());
            kinds.push(v["kind"].as_str().unwrap_or("").to_string());
            let text = v.to_string();
            assert!(!text.contains("glpat-"), "秘密不得出现在 rollout");
        }
        assert_eq!(kinds, vec!["run_started", "tool_result"]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
