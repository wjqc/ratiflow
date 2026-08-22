use getrandom::getrandom;

/// `prefix_<24hex>` 标识符；renderer 视为 opaque string，禁止解析格式。
pub fn new_id(prefix: &str) -> String {
    let mut buf = [0u8; 12];
    if getrandom(&mut buf).is_err() {
        // 熵源失败属于不可恢复系统故障。
        panic!("ids: entropy source unavailable");
    }
    format!("{prefix}_{}", hex(&buf))
}

pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}
