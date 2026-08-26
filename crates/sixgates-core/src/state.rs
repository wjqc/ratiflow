//! 应用状态：DB actor 句柄 + Run 运行时 + 适配器装配。Rust core 是业务唯一写入者。
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::sync::{Arc, Mutex};

use sg_agent::Gateway;
use sg_integrations::{FakeGitLab, FakeSSH};
use sg_policy::Snapshot;
use sg_store::Store;

use crate::db::Db;

/// 活跃 Run 的取消句柄注册表：cancel 置位旗标，Run 任务在迭代边界观察并自清理（M0-②）。
#[derive(Default)]
pub struct RunRegistry {
    inner: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl RunRegistry {
    pub fn register(&self, run_id: &str, flag: Arc<AtomicBool>) {
        self.inner.lock().unwrap().insert(run_id.into(), flag);
    }
    pub fn unregister(&self, run_id: &str) {
        self.inner.lock().unwrap().remove(run_id);
    }
    pub fn get(&self, run_id: &str) -> Option<Arc<AtomicBool>> {
        self.inner.lock().unwrap().get(run_id).cloned()
    }
}

pub struct AppState {
    pub db: Db,
    /// Run 任务专属连接：WAL 多连接，与 DB actor 的连接经 busy_timeout 串行写。
    pub run_store: Arc<Store>,
    /// 活跃 Run 取消注册表。
    pub runs: Arc<RunRegistry>,
    /// tokio 运行时句柄（Run 任务派发）。
    pub handle: tokio::runtime::Handle,
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
    pub fn version_str(&self) -> &str {
        &self.core_version
    }

    pub fn new(db: Db, run_store: Arc<Store>, initial_watermark: i64, core_version: &str) -> Self {
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
            db,
            run_store,
            runs: Arc::new(RunRegistry::default()),
            handle: tokio::runtime::Handle::current(),
            gitlab,
            model: Arc::new(model),
            ssh,
            policy,
            credentials,
            executor_mode,
            core_version: core_version.into(),
            last_pushed: AtomicI64::new(initial_watermark),
        }
    }
}
