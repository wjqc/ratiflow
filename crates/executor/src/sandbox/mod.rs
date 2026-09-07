//! OS 内核级命令沙箱（ADR-034 / Codex 能力差距方案 M3）。
//!
//! 把 SafeRestricted 的"argv 白名单 + 本机裸进程"升级为内核强制的 containment：
//! - macOS：Seatbelt（sandbox-exec，默认 deny 的最小 profile）；
//! - Linux：Landlock（read/write/execute 路径限制；网络隔离需 ABI≥4，否则诚实降级）；
//! - Docker：既有容器路径（更强隔离，继续保留）；
//! - Windows / 未识别环境：Unavailable，fail-closed。
//!
//! 不变量（方案 §3）：argv 白名单、元字符拒绝、路径 canonical 守卫继续作为
//! defense-in-depth；沙箱不可用时写操作 fail-closed，绝不静默回退未隔离执行。

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 沙箱后端（方案 §5 M3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxBackend {
    MacSeatbelt,
    LinuxLandlock,
    Docker,
    Unavailable,
}

impl SandboxBackend {
    pub fn as_str(&self) -> &'static str {
        match self {
            SandboxBackend::MacSeatbelt => "mac_seatbelt",
            SandboxBackend::LinuxLandlock => "linux_landlock",
            SandboxBackend::Docker => "docker",
            SandboxBackend::Unavailable => "unavailable",
        }
    }
}

/// 启动探测结果（backend/version/policy digest 入执行快照）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxCapability {
    pub backend: String,
    pub version: String,
    /// 可用为空串；不可用/受限时给人读的阻塞原因（设置页"实际保护范围"消费）。
    #[serde(default)]
    pub blocked_reason: String,
}

/// 沙箱策略（manifest + ToolCtx 装配）：
/// - read：系统运行库由 profile 模板内置；输入路径 = 受管 worktree/work_dir；
/// - write：受管 worktree、run 工件目录、必要临时目录；
/// - network_off 按 manifest（默认 true）。
#[derive(Debug, Clone, Default)]
pub struct SandboxPolicy {
    pub read_paths: Vec<String>,
    pub write_paths: Vec<String>,
    pub network_off: bool,
}

impl SandboxPolicy {
    /// 规范 JSON（排序去重 + canonical 序列化）→ sha256。策略任何字节变化都会
    /// 改变 digest，可被执行快照/审计固定与回查。
    pub fn digest(&self) -> String {
        let mut reads = self.read_paths.clone();
        let mut writes = self.write_paths.clone();
        reads.sort();
        reads.dedup();
        writes.sort();
        writes.dedup();
        let canonical = serde_json::json!({
            "read": reads,
            "write": writes,
            "networkOff": self.network_off,
        });
        let hex = sg_store::ids::hex(&Sha256::digest(canonical.to_string().as_bytes()));
        format!("sha256:{hex}")
    }
}

/// 探测当前主机的内核沙箱能力（每次调用都真实执行一次探测命令，不缓存——
/// 设置页自检与执行快照要求如实反映当下）。
pub fn probe() -> SandboxCapability {
    #[cfg(target_os = "macos")]
    {
        return macos::probe();
    }
    #[cfg(target_os = "linux")]
    {
        return linux::probe();
    }
    #[allow(unreachable_code)]
    {
        SandboxCapability {
            backend: SandboxBackend::Unavailable.as_str().to_string(),
            version: String::new(),
            blocked_reason: "该平台尚未定义内核沙箱后端；命令执行 fail-closed".into(),
        }
    }
}

/// 按当前平台后端执行（backend 已探明可用）。统一入口保持 lib.rs 只做编排。
pub(crate) fn backend_execute(
    policy: &SandboxPolicy,
    m: &crate::ExecutionManifest,
    cancel: Option<&sg_integrations::CancelToken>,
) -> Result<crate::ExecResult, crate::ExecError> {
    #[cfg(target_os = "macos")]
    {
        return macos::execute(policy, m, cancel);
    }
    #[cfg(target_os = "linux")]
    {
        return linux::execute(policy, m, cancel);
    }
    #[allow(unreachable_code)]
    {
        Err(crate::ExecError::SandboxUnavailable(
            "该平台尚未定义内核沙箱后端".into(),
        ))
    }
}

/// 当前策略对应的内核强制 profile 文本（macOS Seatbelt / 诊断展示）。
/// Linux 无文本 profile（syscall 规则），返回策略 canonical JSON 供展示。
pub fn policy_profile_text(backend: &SandboxBackend, policy: &SandboxPolicy) -> String {
    #[cfg(target_os = "macos")]
    if *backend == SandboxBackend::MacSeatbelt {
        return macos::generate_profile(policy);
    }
    #[cfg(target_os = "linux")]
    if *backend == SandboxBackend::LinuxLandlock {
        return linux::policy_summary(policy);
    }
    serde_json::json!({
        "backend": backend.as_str(),
        "read": policy.read_paths,
        "write": policy.write_paths,
        "networkOff": policy.network_off,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_digest_is_canonical_and_path_order_insensitive() {
        let a = SandboxPolicy {
            read_paths: vec!["/w/a".into(), "/w/b".into()],
            write_paths: vec!["/w/a".into()],
            network_off: true,
        };
        let b = SandboxPolicy {
            read_paths: vec!["/w/b".into(), "/w/a".into()],
            write_paths: vec!["/w/a".into()],
            network_off: true,
        };
        assert_eq!(a.digest(), b.digest(), "路径顺序不影响 digest");
        let c = SandboxPolicy {
            network_off: false,
            ..a.clone()
        };
        assert_ne!(a.digest(), c.digest(), "网络策略变化必须改变 digest");
        assert!(a.digest().starts_with("sha256:"));
    }

    #[test]
    fn probe_returns_honest_capability() {
        let cap = probe();
        // 本仓库目标平台至少给出真实探测结果；其余平台 Unavailable。
        assert!(matches!(
            cap.backend.as_str(),
            "mac_seatbelt" | "linux_landlock" | "unavailable"
        ));
        if cap.backend == "unavailable" {
            assert!(!cap.blocked_reason.is_empty());
        }
    }
}
