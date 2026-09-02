//! Profile 路由模型提供者：Agent Run 运行时从设置域 model_profiles 解析供应商。
//! 优先级：env 装配（state.rs 短路）→ DB Profile（路由主档 → 最早可用档案）→ fake 兜底。
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
        let profile = items
            .iter()
            .find(|p| Some(p.id.as_str()) == primary.as_deref())
            .or_else(|| {
                items.iter().find(|p| {
                    p.managed_source.is_none()
                        && profiles::kind_runtime_usable(&p.provider_kind)
                        && p.status != "error"
                })
            })?;
        if !profiles::kind_runtime_usable(&profile.provider_kind) {
            return None;
        }
        let base_url = profiles::resolve_base_url(&profile.provider_kind, &profile.base_url);
        if base_url.is_empty() {
            return None;
        }
        let api_key = match &profile.credential_ref_id {
            Some(rid) => profiles::reveal_for(&self.store, self.credentials.as_ref(), rid).ok()?,
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
            Err(()) => self.fallback.health_check(),
        }
    }

    fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, String> {
        match self.delegate() {
            Ok(http) => http.complete(req),
            Err(()) => self.fallback.complete(req),
        }
    }
}
