//! Profile 路由模型提供者：Agent Run 运行时从设置域 model_profiles 解析供应商。
//! 优先级：env 装配（state.rs 短路）→ DB Profile（路由主档 → 最早可用档案）→
//! 显式 E2E fake；普通运行态缺少可用 Profile 时返回可操作错误，不伪装成脚本耗尽。
//! API Key 每次调用经 Keychain reveal，不落内存缓存之外的任何存储。
use std::sync::Arc;

use sg_integrations::model::{CompletionRequest, CompletionResponse, ModelProvider};
use sg_settings::credentials::CredentialStore;
use sg_settings::profiles;
use sg_store::Store;

struct Resolved {
    base_url: String,
    model: String,
    api_key: String,
}

pub struct ProfileModel {
    store: Arc<Store>,
    credentials: Arc<dyn CredentialStore>,
    env_key: Option<String>,
    fallback: Box<dyn ModelProvider>,
    fallback_enabled: bool,
}

impl ProfileModel {
    pub fn new(
        store: Arc<Store>,
        credentials: Arc<dyn CredentialStore>,
        fallback: Box<dyn ModelProvider>,
    ) -> Self {
        Self {
            store,
            credentials,
            env_key: std::env::var("SIXGATES_MODEL_API_KEY")
                .ok()
                .filter(|k| !k.is_empty()),
            fallback,
            fallback_enabled: std::env::var_os("SIXGATES_FAKE_MODEL_SCRIPT").is_some(),
        }
    }

    /// 解析当前可用供应商：model_routes 主档优先，否则最早一个可运行的
    /// 非托管 Profile（kind 可运行 + 有 Base URL + 能取到 API Key）。
    fn resolve(&self) -> Option<Resolved> {
        let items = profiles::model_list(&self.store).ok()?;
        let primary: Option<String> = profiles::route_get(&self.store).ok().and_then(|routes| {
            routes.as_array().and_then(|list| {
                list.iter()
                    .map(|r| {
                        r.get("primaryProfileId")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string()
                    })
                    .find(|id| !id.is_empty())
            })
        });
        let resolve_profile = |profile: &profiles::ModelProfile| {
            if !profiles::kind_runtime_usable(&profile.provider_kind) {
                return None;
            }
            let base_url = profiles::resolve_base_url(&profile.provider_kind, &profile.base_url);
            if base_url.is_empty() {
                return None;
            }
            let api_key = match &profile.credential_ref_id {
                Some(rid) => {
                    profiles::reveal_for(&self.store, self.credentials.as_ref(), rid).ok()?
                }
                None => self.env_key.clone()?,
            };
            if api_key.trim().is_empty() {
                return None;
            }
            Some(Resolved {
                base_url,
                model: profile.default_model.clone(),
                api_key,
            })
        };

        if let Some(profile) = items
            .iter()
            .find(|p| Some(p.id.as_str()) == primary.as_deref())
        {
            if let Some(resolved) = resolve_profile(profile) {
                return Some(resolved);
            }
        }

        // `status=error` is the latest connection-test result, not a disable
        // switch. Keep attempting the configured endpoint so the Agent reports
        // the provider's real error and a user can recover without a core restart.
        items
            .iter()
            .filter(|p| p.managed_source.is_none())
            .find_map(resolve_profile)
    }

    /// 解析成功 → 构建 OpenAI 兼容委托；失败 → 走 fake 兜底。
    fn delegate(&self) -> Result<sg_integrations::ModelHttp, ()> {
        self.resolve()
            .map(|r| sg_integrations::ModelHttp {
                name_value: "profile-routed".into(),
                base_url: r.base_url,
                api_key: r.api_key,
                model: r.model,
            })
            .ok_or(())
    }
}

impl ModelProvider for ProfileModel {
    fn name(&self) -> &str {
        "profile-routed"
    }

    fn health_check(&self) -> Result<(), String> {
        match self.delegate() {
            Ok(http) => http.health_check(),
            Err(()) if self.fallback_enabled => self.fallback.health_check(),
            Err(()) => {
                Err("model_unavailable: 未找到可用模型，请到“设置 → 模型”完成连接测试".into())
            }
        }
    }

    fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, String> {
        match self.delegate() {
            Ok(http) => http.complete(req),
            Err(()) if self.fallback_enabled => self.fallback.complete(req),
            Err(()) => {
                Err("model_unavailable: 未找到可用模型，请到“设置 → 模型”完成连接测试".into())
            }
        }
    }
}
