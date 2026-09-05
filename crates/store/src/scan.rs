//! 秘密/PII 扫描与脱敏（v2 ADR-019 行为等价）：objects 入库与模型出网共用同一规则。
use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Finding {
    pub kind: String,
    pub count: usize,
}

struct Rule {
    kind: &'static str,
    re: Regex,
}

fn rules() -> &'static Vec<Rule> {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        vec![
            Rule { kind: "private_key", re: Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----").unwrap() },
            Rule { kind: "aws_access_key", re: Regex::new(r"\bAKIA[0-9A-Z]{16}\b").unwrap() },
            Rule { kind: "github_token", re: Regex::new(r"\bgh[pousr]_[A-Za-z0-9]{36,}\b").unwrap() },
            Rule { kind: "gitlab_token", re: Regex::new(r"\bglpat-[A-Za-z0-9_\-]{20,}\b").unwrap() },
            Rule { kind: "bearer_token", re: Regex::new(r"(?i)\bbearer\s+[A-Za-z0-9._\-]{20,}\b").unwrap() },
            // 值限定为带引号字面量或秘密材料字符集（不含 : . < > ( ) ; 等代码标点），
            // 避免 `let token = std::sync::Arc::new(...)` 之类代码赋值误报阻断快照/草稿落盘。
            Rule { kind: "password_assignment", re: Regex::new(r#"(?i)["']?password["']?[ \t]*[:=][ \t]*(?:"[^"\n]{8,}"|'[^'\n]{8,}'|[A-Za-z0-9_\-+/=]{8,})"#).unwrap() },
            Rule { kind: "api_key_assignment", re: Regex::new(r#"(?i)["']?(api[_-]?key|secret|token)["']?[ \t]*[:=][ \t]*(?:"[^"\n]{16,}"|'[^'\n]{16,}'|[A-Za-z0-9_\-+/=]{16,})"#).unwrap() },
            Rule { kind: "email_pii", re: Regex::new(r"\b[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}\b").unwrap() },
        ]
    })
}

/// 返回按 Kind 聚合的命中；空切片表示未命中。
pub fn scan(data: &[u8]) -> Vec<Finding> {
    let text = String::from_utf8_lossy(data);
    let mut out = Vec::new();
    for rule in rules() {
        let n = rule.re.find_iter(&text).count();
        if n > 0 {
            out.push(Finding {
                kind: rule.kind.into(),
                count: n,
            });
        }
    }
    out
}

/// 高风险秘密类别（PII 仅提示不阻断）。
pub fn has_high_risk(findings: &[Finding]) -> bool {
    findings
        .iter()
        .any(|f| !matches!(f.kind.as_str(), "email_pii"))
}

/// 脱敏：命中子串替换为 [REDACTED:kind]，返回脱敏文本与次数。
pub fn mask(data: &[u8]) -> (String, usize) {
    let mut text = String::from_utf8_lossy(data).to_string();
    let mut total = 0usize;
    for rule in rules() {
        while let Some(m) = rule.re.find(&text) {
            let range = m.range();
            let replacement = format!("[REDACTED:{}]", rule.kind);
            text.replace_range(range, &replacement);
            total += 1;
        }
    }
    (text, total)
}
