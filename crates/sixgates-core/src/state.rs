//! 应用状态：Store + 适配器装配。Rust core 是业务唯一写入者。
use std::sync::atomic::AtomicI64;
use std::sync::Arc;

use sg_agent::Gateway;
use sg_integrations::{FakeGitLab, FakeSSH};
use sg_policy::Snapshot;
use sg_store::Store;

pub struct AppState {
    pub store: Arc<Store>,
    pub gitlab: Arc<dyn sg_integrations::GitLabClient>,
    pub model: Arc<Gateway>,
    pub ssh: Arc<dyn sg_integrations::SSHAdapter>,
    pub policy: Snapshot,
    pub credentials: std::sync::Arc<dyn sg_settings::credentials::CredentialStore>,
    pub executor_mode: sg_executor::Mode,
    pub core_version: String,
    /// 已推送事件的最高 sequence（notification 增量推送）。
    pub last_pushed: AtomicI64,
}

impl AppState {
    pub fn new(store: Store, core_version: &str) -> Self {
        // 适配器按环境装配：未配置时使用 fake 并在诊断中标记 not_ready。
        let (gitlab, gitlab_fake) = match (
            std::env::var("SIXGATES_GITLAB_URL"),
            std::env::var("SIXGATES_GITLAB_TOKEN"),
        ) {
            (Ok(url), Ok(token)) if !url.is_empty() && !token.is_empty() => (
                Arc::new(sg_integrations::GitLabHttp {
                    base_url: url,
                    token,
                }) as Arc<dyn sg_integrations::GitLabClient>,
                false,
            ),
            _ => (
                Arc::new(FakeGitLab::default()) as Arc<dyn sg_integrations::GitLabClient>,
                true,
            ),
        };
        let model = match (
            std::env::var("SIXGATES_MODEL_BASE_URL"),
            std::env::var("SIXGATES_MODEL_API_KEY"),
        ) {
            (Ok(base), Ok(key)) if !base.is_empty() && !key.is_empty() => {
                Gateway::new(Box::new(sg_integrations::ModelHttp {
                    name_value: "openai-compatible".into(),
                    base_url: base.clone(),
                    api_key: key,
                    model: std::env::var("SIXGATES_MODEL_NAME").unwrap_or_default(),
                }))
            }
            _ => Gateway::new(Box::new(sg_integrations::FakeModel::default())),
        };
        let ssh: Arc<dyn sg_integrations::SSHAdapter> = Arc::new(FakeSSH::default());
        let _ = gitlab_fake;
        let policy = Snapshot {
            tool_rules: vec![
                sg_policy::ToolRule {
                    tool: "read_file".into(),
                    risk: sg_policy::Risk::Low,
                    requires_approval: false,
                    data_level: "internal".into(),
                    max_result_bytes: 1 << 20,
                    timeout_sec: 60,
                },
                sg_policy::ToolRule {
                    tool: "write_file".into(),
                    risk: sg_policy::Risk::Medium,
                    requires_approval: false,
                    data_level: "internal".into(),
                    max_result_bytes: 1 << 20,
                    timeout_sec: 60,
                },
                sg_policy::ToolRule {
                    tool: "run_command".into(),
                    risk: sg_policy::Risk::High,
                    requires_approval: true,
                    data_level: "internal".into(),
                    max_result_bytes: 1 << 20,
                    timeout_sec: 120,
                },
            ],
            approval_ttl_secs: 3600,
        };
        let credentials: std::sync::Arc<dyn sg_settings::credentials::CredentialStore> =
            if cfg!(target_os = "macos") {
                std::sync::Arc::new(sg_settings::credentials::MacKeychain)
            } else {
                std::sync::Arc::new(sg_settings::credentials::InMemoryCredentials::default())
            };
        let executor_mode = sg_executor::detect_mode(
            sg_executor::docker_available(),
            std::env::var("SIXGATES_UNSAFE_EXEC")
                .map(|v| v == "1")
                .unwrap_or(false),
        );
        Self {
            store: Arc::new(store),
            gitlab,
            model: Arc::new(model),
            ssh,
            policy,
            credentials,
            executor_mode,
            core_version: core_version.into(),
            last_pushed: AtomicI64::new(0),
        }
    }
}
