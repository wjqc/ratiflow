//! Profile 路由模型提供者：Agent Run 运行时从设置域 model_profiles 解析供应商。
//! 优先级：env 装配（state.rs 短路）→ DB Profile（路由主档 → 最早可用档案）→
//! 显式 E2E fake；普通运行态缺少可用 Profile 时返回可操作错误，不伪装成脚本耗尽。
//! API Key 每次调用经 Keychain reveal，不落内存缓存之外的任何存储。
use std::sync::Arc;

use sg_integrations::model::{CompletionRequest, CompletionResponse, ModelProvider};
use sg_integrations::stream::{StreamFuture, StreamingModelProvider};
use sg_integrations::FakeModel;
use sg_settings::credentials::CredentialStore;
use sg_settings::profiles;
use sg_store::Store;

struct Resolved {
    base_url: String,
    model: String,
    api_key: String,
    /// 供应商声明的输出上限（limits.maxOutputTokens）：请求侧超限会被端点 400 拒绝。
    max_output_tokens: Option<i64>,
    /// Profile 能力快照（ADR-033 M1）：codec 协商来源。
    capabilities: serde_json::Value,
    /// Profile 数据策略（M4）：reasoningPersist / serverState 门控。
    data_policy: serde_json::Value,
}

/// limits_json 约定键：maxOutputTokens（兼容 max_output_tokens）；<=0 视为未声明。
fn parse_max_output_tokens(limits: &serde_json::Value) -> Option<i64> {
    limits
        .get("maxOutputTokens")
        .or_else(|| limits.get("max_output_tokens"))
        .and_then(|v| v.as_i64())
        .filter(|v| *v > 0)
}

pub struct ProfileModel {
    store: Arc<Store>,
    credentials: Arc<dyn CredentialStore>,
    env_key: Option<String>,
    /// E2E fake 兜底（sync+streaming 同一实例，脚本状态共享；M2）。
    fallback: Arc<FakeModel>,
    fallback_enabled: bool,
}

impl ProfileModel {
    pub fn new(
        store: Arc<Store>,
        credentials: Arc<dyn CredentialStore>,
        fallback: Box<FakeModel>,
    ) -> Self {
        Self {
            fallback: Arc::from(fallback),
            store,
            credentials,
            env_key: std::env::var("SIXGATES_MODEL_API_KEY")
                .ok()
                .filter(|k| !k.is_empty()),
            fallback_enabled: std::env::var_os("SIXGATES_FAKE_MODEL_SCRIPT").is_some(),
        }
    }

    /// 指纹解析（不访问 Keychain）：同选择逻辑，凭据存在性不参与判定。
    fn resolve_meta(&self) -> Option<(String, String)> {
        let items = profiles::model_list(&self.store).ok()?;
        let primary: Option<String> = profiles::route_get(&self.store).ok().and_then(|routes| {
            routes.as_array().and_then(|list| {
                list.iter()
                    .filter(|r| {
                        r.get("scope").and_then(|v| v.as_str()) == Some("global")
                            && r.get("taskKind").and_then(|v| v.as_str()) == Some("default")
                    })
                    .map(|r| {
                        r.get("primaryProfileId")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string()
                    })
                    .find(|id| !id.is_empty())
            })
        });
        let meta_of = |profile: &profiles::ModelProfile| {
            if !profiles::kind_runtime_usable(&profile.provider_kind) {
                return None;
            }
            let base_url = profiles::resolve_base_url(&profile.provider_kind, &profile.base_url);
            if base_url.is_empty() {
                return None;
            }
            Some((base_url, profile.default_model.clone()))
        };
        if let Some(profile) = items
            .iter()
            .find(|p| Some(p.id.as_str()) == primary.as_deref())
        {
            if let Some(meta) = meta_of(profile) {
                return Some(meta);
            }
        }
        items
            .iter()
            .filter(|p| p.managed_source.is_none())
            .find_map(meta_of)
    }

    /// 解析当前可用供应商：model_routes 主档优先，否则最早一个可运行的
    /// 非托管 Profile（kind 可运行 + 有 Base URL + 能取到 API Key）。
    fn resolve(&self) -> Option<Resolved> {
        let items = profiles::model_list(&self.store).ok()?;
        // 主档路由精确匹配 (scope=global, taskKind=default)：
        // 其余 scope/taskKind 的路由是未来扩展位，不得旁路掉用户在 UI 设的默认。
        let primary: Option<String> = profiles::route_get(&self.store).ok().and_then(|routes| {
            routes.as_array().and_then(|list| {
                list.iter()
                    .filter(|r| {
                        r.get("scope").and_then(|v| v.as_str()) == Some("global")
                            && r.get("taskKind").and_then(|v| v.as_str()) == Some("default")
                    })
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
                max_output_tokens: parse_max_output_tokens(&profile.limits),
                capabilities: profile.capabilities.clone(),
                data_policy: profile.data_policy.clone(),
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

    /// 解析成功 → (OpenAI 兼容委托, 输出上限钳制)；失败 → 走 fake 兜底。
    fn delegate_with(
        &self,
        req: &CompletionRequest,
    ) -> Result<(sg_integrations::ModelHttp, CompletionRequest), ()> {
        self.resolve()
            .map(|r| {
                let mut req = req.clone();
                if let Some(limit) = r.max_output_tokens {
                    req.max_tokens = req.max_tokens.min(limit);
                }
                (
                    sg_integrations::ModelHttp {
                        name_value: "profile-routed".into(),
                        base_url: r.base_url,
                        api_key: r.api_key,
                        model: r.model,
                    },
                    req,
                )
            })
            .ok_or(())
    }
}

impl ModelProvider for ProfileModel {
    fn name(&self) -> &str {
        "profile-routed"
    }

    fn health_check(&self) -> Result<(), String> {
        match self.delegate_with(&CompletionRequest {
            model: String::new(),
            system_prompt: String::new(),
            messages: Vec::new(),
            max_tokens: 0,
            response_schema: None,
            tools_json: None,
        }) {
            Ok((http, _)) => http.health_check(),
            Err(()) if self.fallback_enabled => self.fallback.health_check(),
            Err(()) => {
                Err("model_unavailable: 未找到可用模型，请到“设置 → 模型”完成连接测试".into())
            }
        }
    }

    fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, String> {
        match self.delegate_with(req) {
            Ok((http, req)) => http.complete(&req),
            Err(()) if self.fallback_enabled => self.fallback.complete(req),
            Err(()) => {
                Err("model_unavailable: 未找到可用模型，请到“设置 → 模型”完成连接测试".into())
            }
        }
    }

    /// ADR-033 M1：返回主档/首个可用 Profile 的能力快照（供 codec 协商冻结）。
    fn capability(&self) -> Option<serde_json::Value> {
        Some(
            self.resolve()
                .map(|r| r.capabilities)
                .unwrap_or(serde_json::json!({"nativeTools": false})),
        )
    }

    /// M4：Profile 数据策略（reasoningPersist/serverState 门控来源）。
    fn data_policy(&self) -> Option<serde_json::Value> {
        self.resolve().map(|r| r.data_policy)
    }

    /// M4：Provider 指纹 = sha256(base_url|default_model)；checkpoint 回放绑定。
    /// 指纹解析不访问 Keychain（resolve_meta 无凭据路径）。
    fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        match self.resolve_meta() {
            Some((base_url, model)) => {
                let hex = sg_store::ids::hex(
                    Sha256::digest(format!("{base_url}|{model}").as_bytes()).as_slice(),
                );
                format!("sha256:{hex}")
            }
            None => "profile-routed".into(),
        }
    }
}

/// M2：流式委托——真实路由走 ModelHttp SSE；E2E fake 兜底共享同一脚本实例。
impl StreamingModelProvider for ProfileModel {
    fn stream_complete<'a>(
        &'a self,
        req: CompletionRequest,
        sink: Arc<dyn sg_integrations::stream::StreamSink>,
        cancel: sg_integrations::CancelToken,
    ) -> StreamFuture<'a> {
        Box::pin(async move {
            match self.delegate_with(&req) {
                Ok((http, req)) => http.stream_complete(req, sink, cancel).await,
                Err(()) if self.fallback_enabled => {
                    self.fallback.stream_complete(req, sink, cancel).await
                }
                Err(()) => {
                    Err("model_unavailable: 未找到可用模型，请到“设置 → 模型”完成连接测试".into())
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::parse_max_output_tokens;

    #[test]
    fn parse_max_output_tokens_reads_limits_convention() {
        // D4：请求侧 max_tokens 必须按供应商声明钳制，超限会被端点 400 拒绝。
        assert_eq!(
            parse_max_output_tokens(&serde_json::json!({"maxOutputTokens": 8192})),
            Some(8192)
        );
        assert_eq!(
            parse_max_output_tokens(&serde_json::json!({"max_output_tokens": 4096})),
            Some(4096)
        );
        assert_eq!(
            parse_max_output_tokens(&serde_json::json!({"maxOutputTokens": 0})),
            None
        );
        assert_eq!(
            parse_max_output_tokens(&serde_json::json!({"other": 1})),
            None
        );
        assert_eq!(parse_max_output_tokens(&serde_json::json!(null)), None);
    }
}
