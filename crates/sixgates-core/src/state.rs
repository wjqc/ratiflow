//! 应用状态：DB actor 句柄 + Run 运行时 + 适配器装配。Rust core 是业务唯一写入者。
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::sync::{Arc, Mutex};

use sg_agent::Gateway;
use sg_integrations::{FakeGitLab, FakeSSH};
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
        let credentials: std::sync::Arc<dyn sg_settings::credentials::CredentialStore> =
            if cfg!(target_os = "macos") {
                std::sync::Arc::new(sg_settings::credentials::MacKeychain)
            } else {
                std::sync::Arc::new(sg_settings::credentials::InMemoryCredentials::default())
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
            _ => {
                // E2E 钩子（仅未配置真实模型时生效）：SIXGATES_FAKE_MODEL_SCRIPT 指向
                // JSON 数组 [{content, tokensIn, tokensOut}]，按序作为脚本响应。
                let fake = sg_integrations::FakeModel::default();
                if let Ok(path) = std::env::var("SIXGATES_FAKE_MODEL_SCRIPT") {
                    if let Ok(body) = std::fs::read_to_string(&path) {
                        if let Ok(list) = serde_json::from_str::<Vec<serde_json::Value>>(&body) {
                            for item in list {
                                fake.push_response(
                                    item["content"].as_str().unwrap_or(""),
                                    item["tokensIn"].as_i64().unwrap_or(1),
                                    item["tokensOut"].as_i64().unwrap_or(1),
                                );
                            }
                        }
                    }
                }
                // 设置域 Profile 优先（model_routes 主档 → 最早可用档案），
                // 无可用 Profile 时回落 fake 脚本。
                Gateway::new(Box::new(crate::model_source::ProfileModel::new(
                    run_store.clone(),
                    credentials.clone(),
                    Box::new(fake),
                )))
            }
        };
        let ssh: Arc<dyn sg_integrations::SSHAdapter> = Arc::new(FakeSSH::default());
        let _ = gitlab_fake;
        // 执行模式：SIXGATES_EXEC_MODE 显式覆盖（开发/E2E 用），否则按 Docker 可用性探测
        // （不静默降级，ADR-024）。设置域 executionProfile 接线在 M3/F10 重排来源优先级。
        let executor_mode = std::env::var("SIXGATES_EXEC_MODE")
            .ok()
            .and_then(|m| match m.as_str() {
                "docker" => Some(sg_executor::Mode::Docker),
                "safe_restricted" => Some(sg_executor::Mode::SafeRestricted),
                "unsafe_explicit" => Some(sg_executor::Mode::UnsafeExplicit),
                "disabled" => Some(sg_executor::Mode::Disabled),
                _ => None,
            })
            .unwrap_or_else(|| {
                sg_executor::detect_mode(
                    sg_executor::docker_available(),
                    std::env::var("SIXGATES_UNSAFE_EXEC")
                        .map(|v| v == "1")
                        .unwrap_or(false),
                )
            });
        Self {
            db,
            run_store,
            runs: Arc::new(RunRegistry::default()),
            handle: tokio::runtime::Handle::current(),
            gitlab,
            model: Arc::new(model),
            ssh,
            credentials,
            executor_mode,
            core_version: core_version.into(),
            last_pushed: AtomicI64::new(initial_watermark),
        }
    }
}
