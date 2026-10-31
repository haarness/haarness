use crate::{
    auth::{Credential, CredentialStore, XaiOAuthClient, unix_millis, xai_access_token_expiring},
    sse::json_stream,
};
use async_stream::try_stream;
use async_trait::async_trait;
use futures::StreamExt;
use haarness_core::{
    CoreError, CoreResult, KeyCheck, ModelProvider, ProviderEvent, ProviderMessage,
    ProviderRequest, ProviderStream, STRUCTURED_OUTPUT_TOOL_NAME, structured_output_tool,
};
use haarness_protocol::{ModelInfo, ModelPricing, ProviderInfo};
use reqwest::header::{AUTHORIZATION, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// One OpenAI-compatible chat-completions endpoint. Instances cover the
/// legacy `OPENAI_COMPATIBLE_*` environment configuration, config-declared
/// `[providers.<id>]` entries, built-in presets (Groq, Mistral, …), and
/// auto-detected local runtimes (Ollama, LM Studio).
#[allow(clippy::struct_excessive_bools)] // independent capabilities of one endpoint
pub struct OpenAiCompatibleProvider {
    client: reqwest::Client,
    id: String,
    name: String,
    endpoint: String,
    api_key: Option<SecretString>,
    auth_store: Option<Arc<CredentialStore>>,
    auth_aliases: Vec<String>,
    xai_oauth: Option<XaiOAuthClient>,
    /// How long a sign-in renewal may wait for its server.
    renewal_limit: std::time::Duration,
    /// Held by the renewal task, not its caller (see [`Self::bearer_token`]).
    refresh_lock: Arc<Mutex<()>>,
    models: Vec<ModelInfo>,
    live_models: crate::live_models::LiveModels,
    /// Models whose endpoint explicitly accepts the OpenAI-style
    /// `reasoning_effort` request field. A model being capable of reasoning is
    /// not enough: gateways such as OpenRouter and Z.AI use different controls.
    reasoning_effort_models: HashSet<String>,
    model_defaults: CompatibleModelDefaults,
    discover_models: bool,
    /// Managed gateways choose and enforce their own per-request allowance.
    server_output_limit: bool,
    /// Local runtimes need no key; reachable means usable.
    assume_authenticated: bool,
    headers: crate::ProviderHeaders,
    /// See [`CompatibleInstance::replay_observed_reasoning`].
    replay_observed_reasoning: bool,
}

/// Declarative description of a compatible endpoint.
#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)] // independent capabilities of one endpoint
pub struct CompatibleInstance {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub models: Vec<String>,
    /// Query the compatible `/models` route and replace fallback models when
    /// it succeeds. Explicit user model allowlists set this to false.
    pub discover_models: bool,
    /// Let a managed gateway choose its budget-aware default output allowance.
    /// Explicit request-hook limits still override this default.
    pub server_output_limit: bool,
    /// Send a model's streamed reasoning back to this server with its tool
    /// calls, in the field the server used. Only for a server the user set up
    /// (a configured provider, a local runtime, `OPENAI_COMPATIBLE_*`), never
    /// for a built-in service whose API documents no such field.
    pub replay_observed_reasoning: bool,
    /// Conservative capability/limit overrides for endpoints whose `/models`
    /// response omits metadata. Explicit config wins over discovered values.
    pub model_defaults: CompatibleModelDefaults,
    pub assume_authenticated: bool,
    pub headers: crate::ProviderHeaders,
}

/// Defaults applied to every model exposed by one compatible instance.
/// `None` means "use discovery or the conservative protocol default".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompatibleModelDefaults {
    pub reasoning: Option<bool>,
    pub images: Option<bool>,
    pub video: Option<bool>,
    pub tools: Option<bool>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
}

/// The assistant-message fields a server streams its reasoning in, and takes
/// it back in: `reasoning_content` (DeepSeek, llama.cpp, SGLang),
/// `reasoning` (vLLM, Ollama, LM Studio) or `reasoning_text` (GitHub
/// Copilot).
const REASONING_FIELDS: [&str; 3] = ["reasoning_content", "reasoning", "reasoning_text"];

/// The metadata key naming the provider whose reasoning a tool call keeps.
const REASONING_SOURCE: &str = "reasoning_provider";
/// The model whose reasoning details a tool call carries: they go back to it
/// alone, as a router serving many models cannot turn one model's opaque
/// reasoning into another's (Anthropic refuses unsigned thinking).
const REASONING_MODEL: &str = "reasoning_model";

/// Whether an assistant message's reasoning goes back to the server with it,
/// so a model keeps its reasoning between tool calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReasoningReplay {
    /// DeepSeek, Moonshot and Z.ai want `reasoning_content` on every
    /// assistant message, empty when there was none.
    Required,
    /// A server the user set up gets back the reasoning it sent, in the field
    /// it sent it in.
    Observed,
    /// A built-in service or a managed gateway: nothing is sent back that
    /// its API does not document.
    Never,
}

impl OpenAiCompatibleProvider {
    fn reasoning_replay(&self, model: &str) -> ReasoningReplay {
        // A Haarness Router passes `reasoning_content` through, and DeepSeek
        // behind it refuses a tool call's next request without it.
        let routed_deepseek = self.id.starts_with("router") && model.starts_with("deepseek/");
        if matches!(self.id.as_str(), "deepseek" | "moonshotai" | "zai") || routed_deepseek {
            ReasoningReplay::Required
        } else if self.replay_observed_reasoning {
            ReasoningReplay::Observed
        } else {
            ReasoningReplay::Never
        }
    }

    #[must_use]
    pub fn new(instance: CompatibleInstance) -> Self {
        let model_defaults = instance.model_defaults;
        let provider_id = instance.id.clone();
        Self {
            client: reqwest::Client::new(),
            endpoint: format!(
                "{}/chat/completions",
                instance.base_url.trim_end_matches('/')
            ),
            id: instance.id,
            name: instance.name,
            api_key: instance.api_key.map(SecretString::from),
            auth_store: None,
            auth_aliases: Vec::new(),
            xai_oauth: (provider_id == "xai").then(XaiOAuthClient::default),
            refresh_lock: Arc::default(),
            renewal_limit: haarness_account::renewals::LIMIT,
            live_models: crate::live_models::LiveModels::default(),
            models: instance
                .models
                .into_iter()
                .map(|id| model_info(&provider_id, &id, model_defaults))
                .collect(),
            reasoning_effort_models: HashSet::new(),
            model_defaults,
            discover_models: instance.discover_models,
            server_output_limit: instance.server_output_limit,
            assume_authenticated: instance.assume_authenticated,
            headers: instance.headers,
            replay_observed_reasoning: instance.replay_observed_reasoning,
        }
    }

    /// First-party router credentials must not follow redirects.
    pub(crate) fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    pub(crate) fn with_server_output_limit(mut self) -> Self {
        self.server_output_limit = true;
        self
    }

    /// A Haarness router passes `cache_control` markers on to Anthropic
    /// models; other upstreams would reject them (see `mark_prompt_cache`).
    pub(crate) fn marks_prompt_cache(&self, model: &str) -> bool {
        self.server_output_limit && model.starts_with("anthropic/")
    }

    #[must_use]
    pub fn with_auth_store(mut self, auth_store: Option<Arc<CredentialStore>>) -> Self {
        self.auth_store = auth_store;
        self
    }

    #[must_use]
    pub fn with_auth_aliases(mut self, auth_aliases: Vec<String>) -> Self {
        self.auth_aliases = auth_aliases;
        self
    }

    /// Preserve provider-specific model discovery when a first-class
    /// transport delegates only its Chat Completions event mapping here.
    pub(crate) fn with_model_info(mut self, models: Vec<ModelInfo>) -> Self {
        self.reasoning_effort_models = models
            .iter()
            .filter(|model| !model.reasoning_efforts.is_empty())
            .map(|model| model.id.clone())
            .collect();
        self.models = models;
        self
    }

    #[must_use]
    pub fn from_environment() -> Option<Self> {
        let base = std::env::var("OPENAI_COMPATIBLE_BASE_URL").ok()?;
        Some(Self::new(CompatibleInstance {
            id: "openai-compatible".into(),
            name: "OpenAI-compatible".into(),
            base_url: base,
            api_key: std::env::var("OPENAI_COMPATIBLE_API_KEY").ok(),
            models: vec![
                std::env::var("OPENAI_COMPATIBLE_MODEL").unwrap_or_else(|_| "default".into()),
            ],
            discover_models: false,
            server_output_limit: false,
            replay_observed_reasoning: true,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: false,
            headers: crate::ProviderHeaders::default(),
        }))
    }

    /// Replace fallback models from `GET {base}/models` when discovery is
    /// enabled. On any error the local fallback remains available.
    pub async fn discover_models(self) -> Self {
        self.refresh_models(true).await;
        self
    }
    /// xAI's chat models and their input, from `/language-models`: `/models`
    /// also lists its image and video generators and says nothing of input.
    async fn xai_language_models(
        &self,
        base: &str,
        headers: reqwest::header::HeaderMap,
    ) -> Option<std::collections::HashMap<String, Value>> {
        let body: Value = self
            .client
            .get(format!("{base}/language-models"))
            .headers(headers)
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .await
            .ok()?;
        Some(
            body["models"]
                .as_array()?
                .iter()
                .filter_map(|entry| {
                    Some((
                        entry["id"].as_str()?.to_owned(),
                        entry["input_modalities"].clone(),
                    ))
                })
                .collect(),
        )
    }

    pub(crate) async fn fetch_models(&self) -> Option<Vec<ModelInfo>> {
        if !self.discover_models {
            return None;
        }
        let base = self.endpoint.strip_suffix("/chat/completions")?;
        let mut headers = self.headers.map();
        if let Ok(Some(key)) = self.bearer_token().await {
            let Ok(value) = HeaderValue::from_str(&format!("Bearer {key}")) else {
                return None;
            };
            headers.insert(AUTHORIZATION, value);
        }
        let language_models = if self.id == "xai" {
            self.xai_language_models(base, headers.clone()).await
        } else {
            None
        };
        let request = self
            .client
            .get(format!("{base}/models"))
            .headers(headers)
            .timeout(std::time::Duration::from_secs(3));
        let discovered = request
            .send()
            .await
            .ok()
            .and_then(|response| response.error_for_status().ok());
        if let Some(response) = discovered
            && let Ok(body) = response.json::<Value>().await
        {
            let models: Vec<_> = body["data"]
                .as_array()?
                .iter()
                .filter_map(|entry| match &language_models {
                    // xAI's chat models, with the input `/models` leaves out.
                    Some(chat) => {
                        let inputs = chat.get(entry["id"].as_str()?)?;
                        let mut entry = entry.clone();
                        entry["input_modalities"] = inputs.clone();
                        Some(std::borrow::Cow::Owned(entry))
                    }
                    None => Some(std::borrow::Cow::Borrowed(entry)),
                })
                .filter_map(|entry| {
                    let entry = &*entry;
                    let mut model = discovered_model_info(&self.id, entry, self.model_defaults)?;
                    if let Some(efforts) = entry["reasoning_efforts"].as_array() {
                        model.reasoning_efforts = efforts
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect();
                    } else if advertises_reasoning_effort(entry)
                        && model.reasoning_efforts.is_empty()
                    {
                        model.reasoning_efforts =
                            ["low", "medium", "high"].map(str::to_owned).to_vec();
                    }
                    if !model.reasoning {
                        model.reasoning_efforts.clear();
                    }
                    Some(model)
                })
                .collect();
            let models = if self.id == "ollama" {
                self.ollama_details(base, models).await
            } else {
                models
            };
            return Some(crate::merge_discovered_models(self.models.clone(), models));
        }
        None
    }

    /// What Ollama knows of each installed model (`/api/show`): whether it
    /// reads images, calls tools and thinks, and its context window. Its
    /// OpenAI-style list names the models and nothing more. A model runs on
    /// this computer, so its price is nothing.
    async fn ollama_details(&self, base: &str, models: Vec<ModelInfo>) -> Vec<ModelInfo> {
        let root = base.strip_suffix("/v1").unwrap_or(base).to_owned();
        let details = futures::future::join_all(models.iter().map(|model| {
            let request = self
                .client
                .post(format!("{root}/api/show"))
                .json(&json!({"model": model.id}))
                .timeout(std::time::Duration::from_secs(3));
            async move {
                request
                    .send()
                    .await
                    .ok()?
                    .error_for_status()
                    .ok()?
                    .json::<Value>()
                    .await
                    .ok()
            }
        }))
        .await;
        models
            .into_iter()
            .zip(details)
            .map(|(mut model, details)| {
                if let Some(details) = details {
                    apply_ollama_details(&mut model, &details);
                }
                model.pricing.get_or_insert(ModelPricing {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                    long_context: None,
                });
                model
            })
            .collect()
    }

    #[cfg(test)]
    fn for_test(endpoint: String) -> Self {
        let defaults = CompatibleModelDefaults {
            reasoning: Some(true),
            images: Some(true),
            max_output_tokens: Some(8_192),
            ..CompatibleModelDefaults::default()
        };
        Self {
            client: reqwest::Client::new(),
            id: "openai-compatible".into(),
            name: "OpenAI-compatible".into(),
            endpoint,
            api_key: Some(SecretString::from("test")),
            auth_store: None,
            auth_aliases: Vec::new(),
            xai_oauth: None,
            refresh_lock: Arc::default(),
            renewal_limit: haarness_account::renewals::LIMIT,
            live_models: crate::live_models::LiveModels::default(),
            models: vec![model_info("openai-compatible", "compat-test", defaults)],
            reasoning_effort_models: HashSet::from(["compat-test".into()]),
            model_defaults: defaults,
            discover_models: false,
            server_output_limit: false,
            assume_authenticated: false,
            headers: crate::ProviderHeaders::default(),
            replay_observed_reasoning: false,
        }
    }

    async fn bearer_token(&self) -> CoreResult<Option<String>> {
        self.bearer_token_renewed(false).await
    }

    /// Whether requests carry an OAuth sign-in this provider can renew.
    fn signed_in_with_oauth(&self) -> bool {
        self.api_key.is_none()
            && self.xai_oauth.is_some()
            && self
                .stored_credential()
                .ok()
                .flatten()
                .is_some_and(|(_, credential)| credential.oauth_parts().is_some())
    }

    /// The token to send; `renew` renews an OAuth sign-in even before its
    /// stated expiry, for a token the server has already refused.
    async fn bearer_token_renewed(&self, renew: bool) -> CoreResult<Option<String>> {
        if let Some(key) = &self.api_key {
            return Ok(Some(key.expose_secret().to_owned()));
        }
        let Some(store) = &self.auth_store else {
            return Ok(None);
        };
        let mut stored = self.stored_credential()?;
        let mut credential = stored.as_ref().map(|(_, credential)| credential.clone());
        let Some(current) = credential.as_ref() else {
            return Ok(None);
        };
        if let Some(key) = current.api_key() {
            return Ok(Some(key.to_owned()));
        }
        let Some(client) = &self.xai_oauth else {
            return Err(CoreError::Provider(format!(
                "{} OAuth credentials are not supported by this provider transport",
                self.name
            )));
        };
        let refresh_at = unix_millis().saturating_add(2 * 60 * 1000);
        let refused = renew
            .then(|| current.oauth_parts().map(|oauth| oauth.access.to_owned()))
            .flatten();
        let expires_soon = current.oauth_parts().is_some_and(|oauth| {
            oauth.expires == 0
                || oauth.expires <= refresh_at
                || xai_access_token_expiring(oauth.access, Duration::from_secs(2 * 60))
        }) || refused.is_some();
        if expires_soon {
            let guard = self.refresh_lock.clone().lock_owned().await;
            stored = self.stored_credential()?;
            credential = stored.as_ref().map(|(_, credential)| credential.clone());
            let current = credential
                .as_ref()
                .ok_or_else(|| CoreError::Provider(format!("{} was logged out", self.name)))?;
            // Renewed meanwhile by another request when the token is no
            // longer the refused one.
            if let Some(oauth) = current.oauth_parts()
                && (oauth.expires == 0
                    || oauth.expires <= refresh_at
                    || xai_access_token_expiring(oauth.access, Duration::from_secs(2 * 60))
                    || refused.as_deref() == Some(oauth.access))
            {
                if oauth.refresh.is_empty() {
                    return Err(CoreError::Provider(format!(
                        "{} login has no refresh token; run haarness auth login {}",
                        self.name, self.id
                    )));
                }
                let expected_refresh = oauth.refresh.to_owned();
                let credential_id = stored
                    .as_ref()
                    .map(|(id, _)| id.as_str())
                    .unwrap_or(self.id.as_str());
                // xAI spends a refresh token the moment it answers. A request
                // that gives up mid-refresh (a Stop) must not drop the new one
                // before it is saved, or the next request sends the spent one
                // and the sign-in ends: the renewal runs on its own task,
                // which keeps the lock until it has saved, or until xAI has
                // not answered for `renewal_limit`.
                let (client, store, stale, credential_id) = (
                    client.clone(),
                    store.clone(),
                    current.clone(),
                    credential_id.to_owned(),
                );
                let (limit, name) = (self.renewal_limit, self.name.clone());
                let renewed = haarness_account::renewals::spawn(async move {
                    let _guard = guard;
                    let oauth = stale.oauth_parts().expect("an OAuth credential");
                    let next = tokio::time::timeout(limit, client.refresh(oauth))
                        .await
                        .map_err(|_| {
                            CoreError::Provider(format!(
                                "{name} did not answer the sign-in renewal in time; try again"
                            ))
                        })?
                        .map_err(provider_error)?;
                    let saved = store
                        .replace_oauth_if_current(&credential_id, &expected_refresh, &next)
                        .map_err(provider_error)?;
                    Ok::<_, CoreError>(saved.then_some(next))
                })
                .await
                .map_err(|_| {
                    CoreError::Provider(format!(
                        "Renewing the {} sign-in stopped unexpectedly",
                        self.name
                    ))
                })??;
                credential = match renewed {
                    Some(next) => Some(next),
                    None => self.stored_credential()?.map(|(_, credential)| credential),
                };
            }
        }
        credential
            .as_ref()
            .and_then(Credential::oauth_parts)
            .map(|oauth| oauth.access.to_owned())
            .ok_or_else(|| CoreError::Provider(format!("{} was logged out", self.name)))
            .map(Some)
    }

    fn stored_authenticated(&self) -> bool {
        self.stored_credential()
            .ok()
            .flatten()
            .map(|(_, credential)| credential)
            .is_some_and(|credential| {
                credential.api_key().is_some()
                    || (self.xai_oauth.is_some() && credential.oauth_parts().is_some())
            })
    }

    fn stored_credential(&self) -> CoreResult<Option<(String, Credential)>> {
        let Some(store) = &self.auth_store else {
            return Ok(None);
        };
        for id in std::iter::once(&self.id).chain(self.auth_aliases.iter()) {
            if let Some(credential) = store.get(id).map_err(provider_error)? {
                return Ok(Some((id.clone(), credential)));
            }
        }
        Ok(None)
    }
}

fn model_info(provider: &str, id: &str, defaults: CompatibleModelDefaults) -> ModelInfo {
    let mut info = ModelInfo {
        id: id.into(),
        display_name: id.into(),
        reasoning: false,
        reasoning_efforts: known_reasoning_efforts(provider, id)
            .into_iter()
            .map(str::to_owned)
            .collect(),
        // Chat Completions itself does not guarantee multimodal input. Keep
        // this false unless discovery or config proves otherwise.
        images: false,
        video: false,
        tools: true,
        context_window: None,
        max_output_tokens: None,
        pricing: None,
    };
    // Perplexity Sonar has a stable, documented model set but does not expose
    // custom function tools through the generic Chat Completions contract.
    if provider == "perplexity" {
        info.tools = false;
        info.reasoning = matches!(id, "sonar-reasoning-pro" | "sonar-deep-research");
        info.context_window = match id {
            "sonar" | "sonar-reasoning-pro" | "sonar-deep-research" => Some(128_000),
            "sonar-pro" => Some(200_000),
            _ => None,
        };
        if matches!(
            id,
            "sonar" | "sonar-pro" | "sonar-reasoning-pro" | "sonar-deep-research"
        ) {
            info.max_output_tokens = Some(128_000);
        }
        info.pricing = match id {
            "sonar" => Some(ModelPricing {
                input: 1.0,
                output: 1.0,
                cache_read: 0.0,
                cache_write: 0.0,
                long_context: None,
            }),
            "sonar-pro" => Some(ModelPricing {
                input: 3.0,
                output: 15.0,
                cache_read: 0.0,
                cache_write: 0.0,
                long_context: None,
            }),
            "sonar-reasoning-pro" | "sonar-deep-research" => Some(ModelPricing {
                input: 2.0,
                output: 8.0,
                cache_read: 0.0,
                cache_write: 0.0,
                long_context: None,
            }),
            _ => None,
        };
    }
    apply_known_compatible_metadata(provider, &mut info);
    apply_model_defaults(crate::catalog::annotate(provider, info), defaults)
}

/// The first OpenRouter-style `pricing.overrides[]` tier, which applies to a
/// whole request from `min_prompt_tokens` on.
fn long_context_override(entry: &Value) -> Option<haarness_protocol::LongContextPricing> {
    let tier = entry
        .pointer("/pricing/overrides")?
        .as_array()?
        .iter()
        .filter_map(|tier| Some((tier.get("min_prompt_tokens")?.as_u64()?, tier)))
        .min_by_key(|(threshold, _)| *threshold)?;
    let rate = |key: &str| {
        tier.1
            .get(key)
            .and_then(json_number)
            .map(|value| value * 1_000_000.0)
    };
    Some(haarness_protocol::LongContextPricing {
        above_input_tokens: tier.0.saturating_sub(1),
        input: rate("prompt")?,
        output: rate("completion")?,
        cache_read: rate("input_cache_read").unwrap_or(0.0),
        cache_write: rate("input_cache_write").unwrap_or(0.0),
    })
}

fn apply_known_compatible_metadata(provider: &str, info: &mut ModelInfo) {
    if !info.reasoning_efforts.is_empty() {
        info.reasoning = true;
    }
    match (provider, info.id.as_str()) {
        ("xai", "grok-4.7") => {
            info.images = true;
            info.context_window = Some(500_000);
            info.pricing = Some(ModelPricing {
                input: 2.0,
                output: 6.0,
                cache_read: 0.5,
                cache_write: 0.0,
                // Prices double from 200K prompt tokens.
                long_context: Some(haarness_protocol::LongContextPricing {
                    above_input_tokens: 199_999,
                    input: 4.0,
                    output: 12.0,
                    cache_read: 1.0,
                    cache_write: 0.0,
                }),
            });
        }
        ("groq", "openai/gpt-oss-120b") => {
            info.display_name = "GPT-OSS 120B".into();
            info.reasoning = true;
            info.context_window = Some(131_072);
            info.max_output_tokens = Some(65_536);
            info.pricing = Some(ModelPricing {
                input: 0.15,
                output: 0.60,
                cache_read: 0.0,
                cache_write: 0.0,
                long_context: None,
            });
        }
        ("groq", "openai/gpt-oss-20b") => {
            info.display_name = "GPT-OSS 20B".into();
            info.reasoning = true;
            info.context_window = Some(131_072);
            info.max_output_tokens = Some(65_536);
            info.pricing = Some(ModelPricing {
                input: 0.075,
                output: 0.30,
                cache_read: 0.0,
                cache_write: 0.0,
                long_context: None,
            });
        }
        ("xai", "grok-4.3") => {
            info.display_name = "Grok 4.3".into();
            info.reasoning = true;
            info.images = true;
            info.context_window = Some(1_000_000);
            info.pricing = Some(ModelPricing {
                input: 1.25,
                output: 2.50,
                cache_read: 0.20,
                cache_write: 0.0,
                long_context: Some(haarness_protocol::LongContextPricing {
                    above_input_tokens: 199_999,
                    input: 2.50,
                    output: 5.0,
                    cache_read: 0.40,
                    cache_write: 0.0,
                }),
            });
        }
        ("xai", "grok-build-0.1") => {
            info.display_name = "Grok Build 0.1".into();
            info.images = true;
            info.context_window = Some(256_000);
            info.pricing = Some(ModelPricing {
                input: 1.0,
                output: 2.0,
                cache_read: 0.20,
                cache_write: 0.0,
                long_context: Some(haarness_protocol::LongContextPricing {
                    above_input_tokens: 199_999,
                    input: 2.0,
                    output: 4.0,
                    cache_read: 0.40,
                    cache_write: 0.0,
                }),
            });
        }
        (
            "deepseek",
            "deepseek-flash"
            | "deepseek-v4-flash"
            | "deepseek-v4-flash-vision-exp"
            | "deepseek-v4-pro",
        ) => {
            info.display_name = if info.id == "deepseek-v4-pro" {
                "DeepSeek V4 Pro".into()
            } else {
                "DeepSeek V4.1 Flash".into()
            };
            info.reasoning = true;
            info.context_window = Some(1_000_000);
            info.max_output_tokens = Some(384_000);
            info.images = info.id != "deepseek-v4-pro";
            // Shown at peak rates, the most a request costs; each request
            // reports what it was charged (`deepseek_charge`).
            info.pricing =
                deepseek_rates(&info.id, true).map(|(input, cache_read, output)| ModelPricing {
                    input,
                    output,
                    cache_read,
                    cache_write: 0.0,
                    long_context: None,
                });
        }
        ("together" | "togetherai", "zai-org/GLM-5.1") => {
            info.display_name = "GLM-5.1".into();
            info.context_window = Some(202_752);
            info.pricing = Some(ModelPricing {
                input: 1.40,
                output: 4.40,
                cache_read: 0.0,
                cache_write: 0.0,
                long_context: None,
            });
        }
        ("together" | "togetherai", "moonshotai/Kimi-K2.6") => {
            info.display_name = "Kimi K2.6".into();
            info.context_window = Some(262_144);
            info.pricing = Some(ModelPricing {
                input: 1.20,
                output: 4.50,
                cache_read: 0.20,
                cache_write: 0.0,
                long_context: None,
            });
        }
        ("mistral", "mistral-medium-3-5" | "mistral-medium-2604") => {
            info.display_name = "Mistral Medium 3.5".into();
            info.images = true;
            info.context_window = Some(256_000);
            info.pricing = Some(ModelPricing {
                input: 1.5,
                output: 7.5,
                cache_read: 0.15,
                cache_write: 0.0,
                long_context: None,
            });
        }
        // Mistral's pages spell the id both ways. A launch discount halves the
        // list price until about 2026-10-20.
        ("mistral", "mistral-large-4-0" | "mistral-large-4") => {
            info.display_name = "Mistral Large 4".into();
            info.images = true;
            info.context_window = Some(1_048_576);
            info.pricing = Some(ModelPricing {
                input: 1.36,
                output: 4.18,
                cache_read: 0.14,
                cache_write: 0.0,
                long_context: None,
            });
        }
        ("mistral", "mistral-large-latest") => info.display_name = "Mistral Large".into(),
        ("mistral", "devstral-latest") => info.display_name = "Devstral".into(),
        ("openrouter", "openrouter/auto") => info.display_name = "Auto Router".into(),
        ("cerebras" | "sambanova", "gpt-oss-120b") | ("huggingface", "openai/gpt-oss-120b") => {
            info.display_name = "GPT-OSS 120B".into();
            info.reasoning = true;
        }
        ("nvidia", "nvidia/nemotron-3-super-120b-a12b") => {
            info.display_name = "Nemotron 3 Super 120B".into();
        }
        ("zai", "glm-5.1") => {
            info.display_name = "GLM-5.1".into();
            info.reasoning = true;
        }
        ("deepinfra", "deepseek-ai/DeepSeek-V3") => {
            info.display_name = "DeepSeek V3".into();
        }
        _ => {}
    }
}

fn discovered_model_info(
    provider: &str,
    entry: &Value,
    defaults: CompatibleModelDefaults,
) -> Option<ModelInfo> {
    let id = entry.get("id")?.as_str()?;
    if !is_chat_completion_model(provider, entry, id) {
        return None;
    }
    let mut info = model_info(provider, id, CompatibleModelDefaults::default());
    if let Some(name) = entry
        .get("display_name")
        .or_else(|| entry.get("name"))
        .and_then(Value::as_str)
    {
        // OpenRouter names a model "Vendor: Model"; its id already says the
        // vendor, and the name alone reads as the native provider's does.
        let name = match name.split_once(": ") {
            Some((vendor, model))
                if provider == "openrouter"
                    && !vendor.is_empty()
                    && vendor.len() <= 32
                    && !model.is_empty() =>
            {
                model
            }
            _ => name,
        };
        // DeepSeek writes "DeepSeek-V4.1-Flash".
        let name = if provider == "deepseek" {
            name.replace('-', " ")
        } else {
            name.to_owned()
        };
        name.clone_into(&mut info.display_name);
    }
    info.context_window = first_window(
        entry,
        &[
            "context_length",
            "context_window",
            "max_context_length",
            "max_model_len",
        ],
    )
    .or_else(|| {
        entry
            .get("metadata")
            .and_then(|metadata| first_window(metadata, &["context_length"]))
    })
    .or(info.context_window);
    info.max_output_tokens = first_u64(
        entry,
        &["max_output_tokens", "max_completion_tokens", "max_tokens"],
    )
    .or_else(|| {
        entry
            .get("metadata")
            .and_then(|metadata| first_u64(metadata, &["max_tokens"]))
    })
    .or(info.max_output_tokens);

    if let Some(modalities) = entry
        .pointer("/architecture/input_modalities")
        .or_else(|| entry.pointer("/modalities/input"))
        // DeepSeek's own listing, also through a Haarness router.
        .or_else(|| entry.get("input_modalities"))
        .and_then(Value::as_array)
    {
        info.images = modalities
            .iter()
            .any(|value| value.as_str() == Some("image"));
        // Generic OpenAI Chat Completions has no portable video-input block.
        // Do not advertise provider-specific modalities this adapter rejects.
        info.video = false;
    }
    if let Some(parameters) = entry.get("supported_parameters").and_then(Value::as_array) {
        info.tools = parameters.iter().any(|value| {
            matches!(
                value.as_str(),
                Some("tools" | "tool_choice" | "function_call")
            )
        });
        info.reasoning = parameters.iter().any(|value| {
            matches!(
                value.as_str(),
                Some("reasoning" | "reasoning_effort" | "include_reasoning")
            )
        });
    }
    // DeepSeek lists its levels as `effort.supported_levels` (its own
    // provider can also turn thinking off, `none`), xAI as
    // `capabilities.reasoning_effort`.
    if let Some(efforts) = entry["reasoning_efforts"].as_array().or_else(|| {
        entry
            .pointer("/effort/supported_levels")
            .or_else(|| entry.pointer("/capabilities/reasoning_effort"))
            .and_then(Value::as_array)
    }) {
        info.reasoning_efforts = (provider == "deepseek")
            .then(|| "none".to_owned())
            .into_iter()
            .chain(efforts.iter().filter_map(Value::as_str).map(str::to_owned))
            .collect();
        info.reasoning = !info.reasoning_efforts.is_empty();
    }
    override_bool(entry, "supports_vision", &mut info.images);
    override_bool(entry, "supports_tools", &mut info.tools);
    override_bool(entry, "supports_tool_calling", &mut info.tools);
    override_bool(entry, "supports_function_calling", &mut info.tools);
    override_bool(entry, "supports_reasoning", &mut info.reasoning);

    if provider == "mistral" {
        if let Some(value) = entry
            .pointer("/capabilities/vision")
            .and_then(Value::as_bool)
        {
            info.images = value;
        }
        if let Some(value) = entry
            .pointer("/capabilities/function_calling")
            .and_then(Value::as_bool)
        {
            info.tools = value;
        }
    }
    if provider == "deepinfra"
        && let Some(tags) = entry.pointer("/metadata/tags").and_then(Value::as_array)
    {
        info.images = tags
            .iter()
            .any(|tag| matches!(tag.as_str(), Some("vision" | "vlm")));
        info.reasoning = tags
            .iter()
            .any(|tag| matches!(tag.as_str(), Some("reasoning" | "reasoning_effort")));
    }

    // OpenRouter and Haarness Cloud publish per-token prices as strings. Convert only this
    // documented shape; other compatible APIs use different units. OpenRouter's
    // routers, which pick a model per request, list `-1`: their price is unknown.
    if matches!(provider, "openrouter" | "haarness" | "router")
        && let (Some(input), Some(output)) = (
            entry.pointer("/pricing/prompt").and_then(json_number),
            entry.pointer("/pricing/completion").and_then(json_number),
        )
    {
        info.pricing = (input >= 0.0 && output >= 0.0).then(|| ModelPricing {
            input: input * 1_000_000.0,
            output: output * 1_000_000.0,
            cache_read: entry
                .pointer("/pricing/input_cache_read")
                .and_then(json_number)
                .unwrap_or(0.0)
                * 1_000_000.0,
            cache_write: entry
                .pointer("/pricing/input_cache_write")
                .and_then(json_number)
                .unwrap_or(0.0)
                * 1_000_000.0,
            long_context: long_context_override(entry),
        });
    }
    // xAI prices in hundredths of a cent per million tokens (12500 is $1.25),
    // with a long-context tier from `long_context_threshold` prompt tokens.
    if provider == "xai"
        && let (Some(input), Some(output)) = (
            entry.get("prompt_text_token_price").and_then(json_number),
            entry
                .get("completion_text_token_price")
                .and_then(json_number),
        )
    {
        let rate = |key: &str| {
            entry
                .get(key)
                .and_then(json_number)
                .map(|value| value / 10_000.0)
        };
        info.pricing = Some(ModelPricing {
            input: input / 10_000.0,
            output: output / 10_000.0,
            cache_read: rate("cached_prompt_text_token_price").unwrap_or(0.0),
            cache_write: 0.0,
            long_context: entry
                .get("long_context_threshold")
                .and_then(Value::as_u64)
                .zip(rate("prompt_text_token_price_long_context"))
                .zip(rate("completion_text_token_price_long_context"))
                .map(
                    |((threshold, input), output)| haarness_protocol::LongContextPricing {
                        above_input_tokens: threshold.saturating_sub(1),
                        input,
                        output,
                        cache_read: rate("cached_prompt_text_token_price_long_context")
                            .unwrap_or(0.0),
                        cache_write: 0.0,
                    },
                ),
        });
    }
    if provider == "xai" {
        // A model that reasons at a fixed depth says so in its id only.
        info.reasoning |= id.ends_with("-reasoning") && !id.ends_with("-non-reasoning");
    }
    // DeepSeek behind a self-hosted Haarness Router comes without prices: its
    // own rates, at peak (see `deepseek_charge` for what a request cost).
    if provider.starts_with("router")
        && info.pricing.is_none()
        && let Some((input, cache_read, output)) = id
            .strip_prefix("deepseek/")
            .and_then(|model| deepseek_rates(model, true))
    {
        info.pricing = Some(ModelPricing {
            input,
            output,
            cache_read,
            cache_write: 0.0,
            long_context: None,
        });
    }
    // DeepInfra's compatible model list reports USD-per-million values under
    // nested metadata. These are already in Haarness' pricing unit.
    if provider == "deepinfra"
        && let (Some(input), Some(output)) = (
            entry
                .pointer("/metadata/pricing/input_tokens")
                .and_then(json_number),
            entry
                .pointer("/metadata/pricing/output_tokens")
                .and_then(json_number),
        )
    {
        info.pricing = Some(ModelPricing {
            input,
            output,
            cache_read: entry
                .pointer("/metadata/pricing/cache_read_tokens")
                .and_then(json_number)
                .unwrap_or(0.0),
            cache_write: 0.0,
            long_context: None,
        });
    }
    Some(apply_model_defaults(info, defaults))
}

/// The text a reasoning detail shows the person: a `reasoning.text`'s text or
/// a `reasoning.summary`'s summary.
fn visible_reasoning(detail: &Value) -> Option<&str> {
    match detail.get("type").and_then(Value::as_str) {
        Some("reasoning.text") => detail.get("text"),
        Some("reasoning.summary") => detail.get("summary"),
        _ => None,
    }
    .and_then(Value::as_str)
    .filter(|text| !text.is_empty())
}

/// The reasoning details a tool call's next request may carry to `model`: all
/// of them when the same model wrote them, none when another did. Of details
/// kept before they were tagged, a Claude model gets only Anthropic's own: it
/// refuses another model's unsigned reasoning as thinking.
fn replayed_details(details: &[Value], written_by: Option<&str>, model: &str) -> Vec<Value> {
    if let Some(writer) = written_by {
        return if writer == model {
            details.to_vec()
        } else {
            Vec::new()
        };
    }
    if !model.to_ascii_lowercase().contains("claude") {
        return details.to_vec();
    }
    details
        .iter()
        .filter(|detail| {
            let format = detail["format"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase();
            format.contains("anthropic") || format.contains("claude")
        })
        .cloned()
        .collect()
}

/// The fewest output tokens worth asking again for after a balance refusal:
/// below this a reply with a tool call is likely cut off.
const MIN_AFFORDABLE_OUTPUT: u64 = 4_096;

/// The output OpenRouter says the balance covers, from its 402 refusal
/// ("You requested up to 65536 tokens, but can only afford 16000").
fn affordable_output(body: &str) -> Option<u64> {
    let detail = crate::sse::provider_error_detail(body)?;
    let rest = &detail[detail.find("can only afford ")? + "can only afford ".len()..];
    rest.split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// DeepSeek's rates per million tokens (cache-miss input, cache-hit input,
/// output) for a model, at peak or off-peak; off-peak is half.
fn deepseek_rates(model: &str, peak: bool) -> Option<(f64, f64, f64)> {
    let (input, cache_read, output) = match model {
        "deepseek-flash" | "deepseek-v4-flash" | "deepseek-v4-flash-vision-exp" => {
            (0.30, 0.006, 1.20)
        }
        "deepseek-v4-pro" => (1.32, 0.044, 3.96),
        _ => return None,
    };
    let share = if peak { 1.0 } else { 0.5 };
    Some((input * share, cache_read * share, output * share))
}

/// Whether DeepSeek bills `at` at its peak rates: 01:00-04:00 and
/// 06:00-10:00 UTC on weekdays. Its Chinese public holidays, off-peak all
/// day, are not known here, so a request on one is reported at peak.
fn deepseek_peak(at: chrono::DateTime<chrono::Utc>) -> bool {
    use chrono::{Datelike as _, Timelike as _};
    let weekday = at.weekday().num_days_from_monday() < 5;
    weekday && matches!(at.hour(), 1..=3 | 6..=9)
}

/// What a DeepSeek request cost, from its usage at the rates of the hour it
/// ran: cache hits (`prompt_cache_hit_tokens`) at their own rate.
#[allow(clippy::cast_precision_loss)] // a displayed cost, not accounting
fn deepseek_charge(model: &str, at: chrono::DateTime<chrono::Utc>, usage: &Value) -> Option<f64> {
    let (input, cache_read, output) = deepseek_rates(model, deepseek_peak(at))?;
    let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0) as f64;
    let prompt = count("prompt_tokens");
    // A router may report DeepSeek's cache hits the OpenAI way.
    let hit = usage
        .get("prompt_cache_hit_tokens")
        .or_else(|| usage.pointer("/prompt_tokens_details/cached_tokens"))
        .and_then(Value::as_u64)
        .map_or(0.0, |tokens| tokens as f64)
        .min(prompt);
    let completion = count("completion_tokens");
    (prompt > 0.0 || completion > 0.0)
        .then(|| ((prompt - hit) * input + hit * cache_read + completion * output) / 1_000_000.0)
}

/// One model's `/api/show` answer from Ollama, into what Haarness offers.
fn apply_ollama_details(model: &mut ModelInfo, details: &Value) {
    let capabilities: Vec<&str> = details["capabilities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !capabilities.is_empty() {
        model.images = capabilities.contains(&"vision");
        model.tools = capabilities.contains(&"tools");
        model.reasoning = capabilities.contains(&"thinking");
        // Ollama turns thinking off with `none`; gpt-oss always thinks.
        model.reasoning_efforts = if !model.reasoning {
            Vec::new()
        } else if model.id.starts_with("gpt-oss") {
            ["low", "medium", "high"].map(str::to_owned).to_vec()
        } else {
            ["none", "low", "medium", "high"]
                .map(str::to_owned)
                .to_vec()
        };
    }
    // `<architecture>.context_length`, e.g. `qwen3_5.context_length`.
    if let Some(window) = details["model_info"].as_object().and_then(|info| {
        info.iter()
            .find(|(key, _)| key.ends_with(".context_length"))
            .and_then(|(_, value)| value.as_u64())
    }) {
        model.context_window = Some(window);
    }
}

fn is_chat_completion_model(provider: &str, entry: &Value, id: &str) -> bool {
    // xAI serves its multi-agent model only on the Responses API: "Multi
    // Agent requests are not allowed on chat completions".
    if provider == "xai" && id.contains("multi-agent") {
        return false;
    }
    if entry.get("archived").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    if ["active", "enabled", "available"]
        .into_iter()
        .any(|key| entry.get(key).and_then(Value::as_bool) == Some(false))
    {
        return false;
    }
    if entry
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| {
            matches!(
                status.to_ascii_lowercase().as_str(),
                "archived" | "inactive" | "disabled" | "unavailable" | "deprecated"
            )
        })
    {
        return false;
    }

    let completion_chat = entry
        .pointer("/capabilities/completion_chat")
        .and_then(Value::as_bool);
    if completion_chat == Some(false) || (provider == "mistral" && completion_chat != Some(true)) {
        return false;
    }
    if entry
        .pointer("/architecture/output_modalities")
        .or_else(|| entry.pointer("/modalities/output"))
        .and_then(Value::as_array)
        .is_some_and(|modalities| {
            !modalities.is_empty()
                && !modalities
                    .iter()
                    .any(|modality| modality.as_str() == Some("text"))
        })
    {
        return false;
    }

    // OpenRouter names the routes each model answers on; batch-only models
    // list none and refuse a chat.
    if provider == "openrouter"
        && entry
            .get("endpoints")
            .and_then(Value::as_array)
            .is_some_and(|routes| {
                !routes.iter().any(|route| {
                    route
                        .as_str()
                        .is_some_and(|route| route.ends_with("/chat/completions"))
                })
            })
    {
        return false;
    }

    // OpenRouter lists image and audio generators beside chat models; their
    // replies are media a chat can't show. Its routers list image output too
    // but answer chats in text.
    if provider == "openrouter"
        && !id.starts_with("openrouter/")
        && entry
            .pointer("/architecture/output_modalities")
            .and_then(Value::as_array)
            .is_some_and(|modalities| {
                modalities
                    .iter()
                    .any(|modality| matches!(modality.as_str(), Some("image" | "audio")))
            })
    {
        return false;
    }

    // DeepInfra exposes a heterogeneous catalog through this endpoint. Its
    // nested tags are authoritative enough to fail closed: if `chat` is not
    // present, retain the curated fallback instead of guessing from an ID.
    if provider == "deepinfra"
        && !entry
            .pointer("/metadata/tags")
            .and_then(Value::as_array)
            .is_some_and(|tags| tags.iter().any(|tag| tag.as_str() == Some("chat")))
    {
        return false;
    }

    if ["task", "type", "model_type", "pipeline_tag"]
        .into_iter()
        .filter_map(|key| entry.get(key).and_then(Value::as_str))
        .any(is_non_chat_kind)
    {
        return false;
    }

    !id_looks_non_chat(id)
}

fn is_non_chat_kind(kind: &str) -> bool {
    let kind = kind.to_ascii_lowercase().replace(['_', '-'], " ");
    [
        "audio",
        "speech",
        "transcription",
        "automatic speech recognition",
        "text to speech",
        "embedding",
        "feature extraction",
        "sentence similarity",
        "rerank",
        "classification",
        "classifier",
        "moderation",
        "reward model",
        "image generation",
        "text to image",
        "image to image",
        "video generation",
        "text to video",
    ]
    .iter()
    .any(|marker| kind.contains(marker))
}

fn id_looks_non_chat(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    [
        "whisper",
        "speech-to-text",
        "text-to-speech",
        "/tts",
        "tts-",
        "/audio",
        "audio-",
        "/embed",
        "embedding",
        "/bge-",
        "/e5-",
        "/gte-",
        "rerank",
        "re-rank",
        "llama-guard",
        "/guard-",
        "-guard-",
        "classifier",
        "moderation",
        "reward-model",
        "reward_model",
        "stable-diffusion",
        "/flux",
        "flux-",
        "/imagen",
        "sdxl",
        "text-to-image",
        "image-generation",
        "text-to-video",
        "video-generation",
    ]
    .iter()
    .any(|marker| id.contains(marker))
}

fn advertises_reasoning_effort(entry: &Value) -> bool {
    entry
        .get("supported_parameters")
        .and_then(Value::as_array)
        .is_some_and(|parameters| {
            parameters
                .iter()
                .any(|parameter| parameter.as_str() == Some("reasoning_effort"))
        })
        || entry
            .pointer("/metadata/tags")
            .and_then(Value::as_array)
            .is_some_and(|tags| {
                tags.iter()
                    .any(|tag| tag.as_str() == Some("reasoning_effort"))
            })
}

fn known_reasoning_effort_model(provider: &str, model: &str) -> bool {
    !known_reasoning_efforts(provider, model).is_empty()
}

fn known_reasoning_efforts(provider: &str, model: &str) -> Vec<&'static str> {
    match (provider, model) {
        ("groq", "openai/gpt-oss-20b" | "openai/gpt-oss-120b") | ("xai", "grok-4.5") => {
            vec!["low", "medium", "high"]
        }
        ("xai", "grok-4.6" | "grok-4.7") => vec!["low", "medium", "high", "xhigh"],
        (
            "deepseek",
            "deepseek-flash"
            | "deepseek-v4-flash"
            | "deepseek-v4-pro"
            | "deepseek-v4-flash-vision-exp",
        ) => vec!["none", "low", "high", "max"],
        (
            "mistral",
            "mistral-large-4-0"
            | "mistral-large-4"
            | "mistral-medium-3-5"
            | "mistral-medium-3.5"
            | "mistral-medium-2604"
            | "mistral-small-2603"
            | "mistral-small-latest",
        ) => vec!["none", "high"],
        _ => vec![],
    }
}

fn append_reasoning_details(target: &mut Vec<Value>, details: &Value) {
    let Some(details) = details.as_array() else {
        return;
    };
    for detail in details {
        if detail.get("type").and_then(Value::as_str) == Some("reasoning.text")
            && let Some(previous) = target.last_mut()
            && previous.get("type").and_then(Value::as_str) == Some("reasoning.text")
            && same_reasoning_block(previous, detail)
        {
            let mut text = previous
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            text.push_str(
                detail
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            );
            previous["text"] = Value::String(text);
            for field in ["id", "index", "signature", "format"] {
                if previous.get(field).is_none_or(Value::is_null)
                    && let Some(value) = detail.get(field).filter(|value| !value.is_null())
                {
                    previous[field] = value.clone();
                }
            }
        } else {
            target.push(detail.clone());
        }
    }
}

/// Whether this detail begins a reasoning block rather than continuing the
/// previous one. Raw `reasoning.text` arrives as chunks of a single thought
/// and must stay welded together, so only a stated identity change breaks it;
/// every `reasoning.summary` is a complete block of its own. Without the
/// distinction the blocks concatenate and the last word of one summary fuses
/// onto the first word of the next.
fn starts_new_reasoning_block(previous: &Value, next: &Value) -> bool {
    fn kind(detail: &Value) -> Option<&str> {
        detail.get("type").and_then(Value::as_str)
    }
    kind(previous) != kind(next)
        || kind(next) == Some("reasoning.summary")
        || ["id", "index", "format"].iter().any(|field| {
            let previous = previous.get(*field).filter(|value| !value.is_null());
            let next = next.get(*field).filter(|value| !value.is_null());
            matches!((previous, next), (Some(previous), Some(next)) if previous != next)
        })
}

fn same_reasoning_block(left: &Value, right: &Value) -> bool {
    let mut matching_identity = false;
    for field in ["id", "index", "format"] {
        let left = left.get(field).filter(|value| !value.is_null());
        let right = right.get(field).filter(|value| !value.is_null());
        if let (Some(left), Some(right)) = (left, right) {
            if left != right {
                return false;
            }
            matching_identity = true;
        }
    }
    matching_identity
}

fn first_u64(entry: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        entry.get(*key).and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str()?.parse::<u64>().ok())
        })
    })
}

/// The first context window `keys` list. A gateway that lists 0 says
/// nothing about the model, so the model's own window still applies.
fn first_window(entry: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| first_u64(entry, &[key]).filter(|window| *window > 0))
}

fn json_number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse::<f64>().ok())
}

fn override_bool(entry: &Value, key: &str, target: &mut bool) {
    if let Some(value) = entry.get(key).and_then(Value::as_bool) {
        *target = value;
    }
}

fn apply_model_defaults(mut info: ModelInfo, defaults: CompatibleModelDefaults) -> ModelInfo {
    if let Some(value) = defaults.reasoning {
        info.reasoning = value;
    }
    if let Some(value) = defaults.images {
        info.images = value;
    }
    if let Some(value) = defaults.video {
        info.video = value;
    }
    if let Some(value) = defaults.tools {
        info.tools = value;
    }
    if let Some(value) = defaults.context_window {
        info.context_window = Some(value);
    }
    if let Some(value) = defaults.max_output_tokens {
        info.max_output_tokens = Some(value);
    }
    info
}

// One model response can request several tools. Chat Completions requires those
// calls in one assistant message, followed by all corresponding tool results.
/// Anthropic models behind a Haarness router: mark the stable prefix and the newest turns for prompt caching,
/// as the native Anthropic provider does. The system breakpoint also covers the tools (Anthropic caches tools,
/// then system, then messages), and the last two user or tool messages let a multi-round agent loop reuse the
/// cache. The router forwards message-level `cache_control` to Anthropic; other upstreams would reject it.
fn mark_prompt_cache(messages: &mut [Value]) {
    let marker = || json!({"type": "ephemeral"});
    let has_text = |message: &Value| match &message["content"] {
        Value::String(text) => !text.is_empty(),
        Value::Array(parts) => !parts.is_empty(),
        _ => false,
    };
    if let Some(system) = messages
        .first_mut()
        .filter(|message| message["role"] == "system" && has_text(message))
    {
        system["cache_control"] = marker();
    }
    let mut recent_turns = 0;
    for message in messages.iter_mut().skip(1).rev() {
        if recent_turns == 2 {
            break;
        }
        if matches!(message["role"].as_str(), Some("user" | "tool")) && has_text(message) {
            message["cache_control"] = marker();
            recent_turns += 1;
        }
    }
}

fn push_tool_call_message(messages: &mut Vec<Value>, message: Value) {
    if let Some(previous) = messages.last_mut()
        && previous["role"] == "assistant"
        && previous["tool_calls"].is_array()
    {
        previous["tool_calls"]
            .as_array_mut()
            .unwrap()
            .extend(message["tool_calls"].as_array().unwrap().iter().cloned());
        // Streaming attaches shared reasoning to each call. Replay each opaque
        // block once, preserving order and distinct per-call signatures.
        for field in ["reasoning_details", "content"] {
            if let Some(incoming) = message[field].as_array() {
                if !previous[field].is_array() {
                    previous[field] = json!([]);
                }
                let existing = previous[field].as_array_mut().unwrap();
                for value in incoming {
                    if !existing.contains(value) {
                        existing.push(value.clone());
                    }
                }
            }
        }
        for field in REASONING_FIELDS {
            if let Some(incoming) = message[field].as_str() {
                let existing = previous[field].as_str().unwrap_or_default();
                if existing != incoming {
                    previous[field] = json!(format!("{existing}{incoming}"));
                }
            }
        }
    } else {
        messages.push(message);
    }
}

#[async_trait]
impl ModelProvider for OpenAiCompatibleProvider {
    async fn refresh_models(&self, force: bool) {
        self.live_models
            .refresh(force, || self.fetch_models())
            .await;
    }
    async fn check_api_key(&self, key: &str) -> KeyCheck {
        // Presets without a usable `/models` route have nothing cheap to ask.
        let listed = crate::compatible_preset(&self.id).is_none_or(|preset| preset.discover_models);
        let (true, Some(base), Ok(value)) = (
            listed,
            self.endpoint.strip_suffix("/chat/completions"),
            HeaderValue::from_str(&format!("Bearer {key}")),
        ) else {
            return KeyCheck::Unsupported;
        };
        let mut headers = self.headers.map();
        headers.insert(AUTHORIZATION, value);
        // OpenRouter lists its models for any key, a made-up one included;
        // its `/key` route answers only for a key it knows.
        let route = if self.id == "openrouter" {
            "key"
        } else {
            "models"
        };
        let request = self.client.get(format!("{base}/{route}")).headers(headers);
        crate::key_check::check(&self.name, request).await
    }
    fn explicit_prompt_cache_scope(&self, model: &str) -> Option<String> {
        self.marks_prompt_cache(model).then(|| {
            crate::prompt_cache_scope(
                &self.endpoint,
                self.api_key.as_ref().map(ExposeSecret::expose_secret),
                &self.headers,
            )
        })
    }
    fn id(&self) -> &str {
        &self.id
    }
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            authenticated: self.api_key.is_some()
                || self.assume_authenticated
                || !self.headers.is_empty()
                || self.stored_authenticated(),
            models: self.live_models.get(&self.models),
        }
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        let limits = request.parameters.limits;
        let models = self.live_models.get(&self.models);
        let selected = models.iter().find(|model| model.id == request.model);
        let supports_images = selected.map_or_else(
            || self.model_defaults.images.unwrap_or(false),
            |model| model.images,
        );
        let supports_tools = selected.map_or_else(
            || self.model_defaults.tools.unwrap_or(true),
            |model| model.tools,
        );
        let supports_reasoning = selected.map_or_else(
            || self.model_defaults.reasoning.unwrap_or(false),
            |model| model.reasoning,
        );
        let max_output_tokens = selected
            .and_then(|model| model.max_output_tokens)
            .or(self.model_defaults.max_output_tokens)
            .filter(|limit| *limit > 0);
        let supports_reasoning_effort = supports_reasoning
            && (selected.is_some_and(|model| !model.reasoning_efforts.is_empty())
                || self.reasoning_effort_models.contains(&request.model)
                || known_reasoning_effort_model(&self.id, &request.model));
        let structured_output = request.output_schema;
        if structured_output.is_some() && !supports_tools {
            return Err(CoreError::Provider(format!(
                "model {} does not advertise tool support required for structured output",
                request.model
            )));
        }
        let model = request.model;
        let replay = self.reasoning_replay(&model);
        let mut messages = vec![json!({"role":"system","content":request.system})];
        let mut tool_media = Vec::new();
        for message in request.messages {
            if !matches!(&message, ProviderMessage::ToolResult { .. }) {
                messages.append(&mut tool_media);
            }
            match message {
                ProviderMessage::Text { role, content } => {
                    // Text that came with tool calls joins their message: a
                    // call's result must follow the call directly.
                    if role == "assistant"
                        && let Some(calls) = messages.last_mut().filter(|last| {
                            last["role"] == "assistant"
                                && last.get("tool_calls").is_some()
                                && last["content"].is_null()
                        })
                    {
                        calls["content"] = json!(content);
                        continue;
                    }
                    let mut message = json!({"role":role,"content":content});
                    if replay == ReasoningReplay::Required && role == "assistant" {
                        message["reasoning_content"] = json!("");
                    }
                    messages.push(message);
                }
                ProviderMessage::Media {
                    role,
                    content,
                    attachments,
                } => messages.push(openai_compatible_media_message(
                    &role,
                    &content,
                    &attachments,
                    supports_images,
                    &model,
                )?),
                ProviderMessage::ToolCall {
                    call_id,
                    name,
                    arguments,
                    provider_metadata,
                } => {
                    let mut message = json!({"role":"assistant","tool_calls":[{"id":call_id,"type":"function","function":{"name":name,"arguments":serde_json::to_string(&arguments).unwrap_or_else(|_|"{}".into())}}]});
                    if let Some(details) = provider_metadata
                        .get("reasoning_details")
                        .and_then(Value::as_array)
                        .map(|details| {
                            replayed_details(
                                details,
                                provider_metadata[REASONING_MODEL].as_str(),
                                &model,
                            )
                        })
                        .filter(|details| !details.is_empty())
                    {
                        // OpenRouter requires these opaque blocks to be
                        // replayed byte-for-byte and in their original order.
                        message["reasoning_details"] = Value::Array(details);
                    }
                    // The reasoning goes back in the field the server sent
                    // it in, so a model keeps its train of thought between
                    // tool calls; but only to the provider that produced it,
                    // so a chat moved to another provider never hands that
                    // one the first's reasoning.
                    let own = match provider_metadata[REASONING_SOURCE].as_str() {
                        Some(source) => source == self.id,
                        // Stored before reasoning was tagged, when only the
                        // providers that require it kept it.
                        None => replay == ReasoningReplay::Required,
                    };
                    match replay {
                        ReasoningReplay::Required => {
                            let reasoning = |field: &str| {
                                own.then(|| provider_metadata[field].as_str()).flatten()
                            };
                            message["reasoning_content"] =
                                json!(reasoning("reasoning_content").unwrap_or_default());
                            if let Some(reasoning) = reasoning("reasoning") {
                                message["reasoning"] = json!(reasoning);
                            }
                        }
                        ReasoningReplay::Observed if own => {
                            for field in REASONING_FIELDS {
                                if let Some(reasoning) = provider_metadata[field]
                                    .as_str()
                                    .filter(|reasoning| !reasoning.is_empty())
                                {
                                    message[field] = json!(reasoning);
                                }
                            }
                        }
                        ReasoningReplay::Observed | ReasoningReplay::Never => {}
                    }
                    if self.id == "mistral"
                        && let Some(content) = provider_metadata.get("mistral_content")
                    {
                        message["content"] = content.clone();
                    }
                    push_tool_call_message(&mut messages, message);
                }
                ProviderMessage::ToolResult {
                    call_id,
                    output,
                    attachments,
                    ..
                } => {
                    messages.push(json!({"role":"tool","tool_call_id":call_id,"content":output}));
                    if !attachments.is_empty() {
                        // Keep every tool result adjacent to its assistant batch;
                        // user media messages may only follow the completed batch.
                        tool_media.push(openai_compatible_media_message(
                            "user",
                            "The preceding tool result includes this binary document or media.",
                            &attachments,
                            supports_images,
                            &model,
                        )?);
                    }
                }
            }
        }
        messages.append(&mut tool_media);
        if self.marks_prompt_cache(&model) && !request.parameters.uncached {
            mark_prompt_cache(&mut messages);
        }
        let mut requested_tools = request.tools;
        // Haarness routers pass a Claude request's parameters to Anthropic as its input schema.
        if self.server_output_limit {
            crate::tool_schema::retain_claude_compatible(&model, &mut requested_tools);
        }
        // Opus 5.5, Sonnet 5.5 and Fable 5.1 refuse a forced tool choice. Haarness
        // routers turn `response_format` into Claude's native JSON output, which
        // the Anthropic adapter uses for these models too.
        let native_json = structured_output.is_some()
            && self.server_output_limit
            && model.starts_with("anthropic/");
        if let Some(schema) = structured_output.as_ref() {
            requested_tools.retain(|tool| tool.name != STRUCTURED_OUTPUT_TOOL_NAME);
            if !native_json {
                requested_tools.push(structured_output_tool(schema));
            }
        }
        // A forced call to a Gemini model through a gateway meets the bound
        // Google refuses under forced calling, as the native adapter does.
        let forced_gemini = structured_output.is_some()
            && !native_json
            && model
                .rsplit('/')
                .next()
                .is_some_and(|name| name.starts_with("gemini-"));
        let tools:Vec<_>=requested_tools.into_iter().map(|t|{
            let parameters = if forced_gemini { crate::gemini::without_max_items(t.input_schema) } else { t.input_schema };
            json!({"type":"function","function":{"name":t.name,"description":t.description,"parameters":parameters}})
        }).collect();
        let mut body = json!({"model":model,"messages":messages,"stream":true});
        if self.id != "perplexity" {
            body["stream_options"] = json!({"include_usage": true});
        }
        if let Some(limit) = max_output_tokens.filter(|_| !self.server_output_limit) {
            body["max_tokens"] = json!(limit);
        }
        let parameters = request.parameters;
        if let Some(limit) = parameters.max_output_tokens.filter(|limit| *limit > 0) {
            body["max_tokens"] = json!(limit);
        }
        if let Some(temperature) = parameters.temperature {
            body["temperature"] = json!(temperature);
        }
        if let Some(top_p) = parameters.top_p {
            body["top_p"] = json!(top_p);
        }
        if supports_tools && !tools.is_empty() {
            body["tools"] = json!(tools);
        }
        // Alibaba refuses a forced call while Qwen thinks, which its reasoning
        // models do unasked; DeepSeek does too, unless thinking is turned off,
        // directly or behind a router.
        let refuses_forced_while_thinking = (supports_reasoning
            && model
                .rsplit('/')
                .next()
                .is_some_and(|name| name.to_ascii_lowercase().starts_with("qwen")))
            || ((self.id == "deepseek" || model.starts_with("deepseek/"))
                && request.reasoning_effort.as_deref() != Some("none"));
        if native_json {
            body["response_format"] = json!({"type":"json_schema","json_schema":{"name":STRUCTURED_OUTPUT_TOOL_NAME,"schema":structured_output}});
        } else if structured_output.is_some()
            && !crate::tool_schema::refuses_forced_tool_choice(&model)
            && !refuses_forced_while_thinking
        {
            body["tool_choice"] = json!("required");
        }
        if self.id != "perplexity"
            && supports_reasoning_effort
            && let Some(effort) = request.reasoning_effort.as_deref()
        {
            if self.id == "deepseek" {
                body["thinking"] =
                    json!({"type": if effort == "none" { "disabled" } else { "enabled" }});
                if effort != "none" {
                    body["reasoning_effort"] = json!(match effort {
                        "minimal" | "low" => "low",
                        "max" | "ultra" => "max",
                        _ => "high",
                    });
                }
            } else {
                body["reasoning_effort"] = json!(effort);
            }
        }
        for (key, value) in &parameters.options {
            body[key.as_str()] = value.clone();
        }
        let mut headers = self.headers.map();
        crate::apply_request_headers(&mut headers, &parameters.headers);
        if let Some(key) = self.bearer_token().await? {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(|error| CoreError::Provider(error.to_string()))?,
            );
        }
        let builder = self
            .client
            .post(&self.endpoint)
            .headers(headers.clone())
            .json(&body);
        let mut response = crate::sse::send(builder, limits).await?;
        // A sign-in token the server refuses before its stated expiry
        // (revoked early, a clock off) is renewed once and the request sent
        // again; a renewal that fails says to sign in again.
        if response.status() == reqwest::StatusCode::UNAUTHORIZED && self.signed_in_with_oauth() {
            tracing::info!(provider = %self.id, "renewing a refused sign-in");
            if let Some(key) = self.bearer_token_renewed(true).await? {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {key}"))
                        .map_err(|error| CoreError::Provider(error.to_string()))?,
                );
                let builder = self
                    .client
                    .post(&self.endpoint)
                    .headers(headers.clone())
                    .json(&body);
                response = crate::sse::send(builder, limits).await?;
            }
        }
        // OpenRouter refuses a request whose whole output limit the balance
        // can't pay for, however short the reply would be. Ask once more for
        // the output it says it can afford, when that still leaves room to work.
        if self.id == "openrouter" && response.status() == reqwest::StatusCode::PAYMENT_REQUIRED {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            let requested = body["max_tokens"].as_u64();
            match affordable_output(&text).filter(|afford| {
                *afford >= MIN_AFFORDABLE_OUTPUT && requested.is_none_or(|limit| *afford < limit)
            }) {
                Some(afford) => {
                    tracing::info!(provider = %self.id, requested, afford, "lowering the output limit to what the balance covers");
                    body["max_tokens"] = json!(afford);
                    let builder = self
                        .client
                        .post(&self.endpoint)
                        .headers(headers.clone())
                        .json(&body);
                    response = crate::sse::send(builder, limits).await?;
                }
                None => {
                    let message = crate::sse::provider_error_detail(&text).map_or_else(
                        || format!("HTTP {status}"),
                        |detail| format!("HTTP {status}: {detail}"),
                    );
                    return Err(CoreError::Provider(message));
                }
            }
        }
        // Some models take only an automatic tool choice (Meta's Muse), and a
        // router may pass their refusal on as a bare 400. Ask once more without
        // forcing the call: the runtime asks for structured output again when
        // the answer comes back without it.
        if body.get("tool_choice").is_some()
            && response.status() == reqwest::StatusCode::BAD_REQUEST
        {
            tracing::info!(provider = %self.id, model = %body["model"], "asking again without forcing the tool call");
            if let Some(object) = body.as_object_mut() {
                object.remove("tool_choice");
            }
            let builder = self
                .client
                .post(&self.endpoint)
                .headers(headers)
                .json(&body);
            response = crate::sse::send(builder, limits).await?;
        }
        let request_id = crate::request_id(response.headers());
        let provider_id = self.id.clone();
        let reasoning_model = model.clone();
        // The DeepSeek model whose rates price this request: its own
        // provider's, or one behind a self-hosted router, which reports no
        // price (a Cloud router prices its own).
        let deepseek_model = if self.id == "deepseek" {
            Some(model.clone())
        } else if self.id.starts_with("router") && !self.server_output_limit {
            model.strip_prefix("deepseek/").map(str::to_owned)
        } else {
            None
        };
        let mut events = json_stream(response, cancel, limits);
        // A Cloud workspace's own router (the Haarness Cloud provider translates its errors itself).
        if self.server_output_limit && self.id != "haarness" {
            events = Box::pin(futures::StreamExt::map(events, |event| {
                event.map_err(crate::haarness_cloud::readable_router)
            }));
        }
        Ok(Box::pin(try_stream! {
            // Parallel tool calls stream interleaved deltas keyed by index.
            let mut calls: Vec<(String, String, String)> = Vec::new();
            let mut finish_reason: Option<String> = None;
            let mut response_id: Option<String> = None;
            let mut usage: Option<(u64, u64, u64, u64)> = None;
            // What OpenRouter says the request cost, or DeepSeek's rates at
            // the hour it ran, with its usage.
            let mut charged: Option<f64> = None;
            let mut reasoning_details: Vec<Value> = Vec::new();
            let mut reasoning_content = String::new();
            // The field the server streamed its reasoning in, first seen.
            let mut reasoning_field: Option<&'static str> = None;
            let mut mistral_content: Vec<Value> = Vec::new();
            // The last detail whose text reached the transcript, so a new
            // block can be separated from the one before it.
            let mut emitted_reasoning: Option<Value> = None;
            // The plain reasoning text the previous chunk carried.
            let mut last_direct_reasoning = String::new();
            let mut json_output = String::new();
            // What broke the stream after its finish reason arrived: the
            // response is complete, so it finishes first and this follows,
            // for the runtime to judge.
            let mut broken_after_finish = None;
            while let Some(event)=events.next().await{
                let event = match event {
                    Ok(event) => event,
                    Err(error) if finish_reason.is_some() => {
                        broken_after_finish = Some(error);
                        break;
                    }
                    Err(error) => {
                        // Usage that already arrived was billed; keep it.
                        if let Some((input_tokens, output_tokens, cache_read_tokens, cache_write_tokens)) = usage.take() {
                            yield ProviderEvent::Usage { input_tokens, output_tokens, cache_read_tokens, cache_write_tokens }; if let Some(usd) = charged.take() { yield ProviderEvent::Cost { usd }; }
                        }
                        Err::<(), _>(error)?;
                        continue;
                    }
                };
                // Moonshot reports usage on the choice rather than the chunk.
                if let Some(chunk_usage) = event.get("usage").filter(|value| value.is_object()).or_else(|| event["choices"][0].get("usage").filter(|value| value.is_object())) {
                    let count = |paths: &[&str]| paths.iter().find_map(|path| chunk_usage.pointer(path).and_then(Value::as_u64)).unwrap_or(0);
                    if provider_id == "openrouter" {
                        charged = chunk_usage.get("cost").and_then(Value::as_f64).filter(|cost| *cost >= 0.0).or(charged);
                    }
                    if let Some(model) = deepseek_model.as_deref() {
                        charged = deepseek_charge(model, chrono::Utc::now(), chunk_usage).or(charged);
                    }
                    usage = Some((
                        count(&["/prompt_tokens"]),
                        count(&["/completion_tokens"]),
                        // DeepSeek and Z.ai name cache hits their own way.
                        count(&["/prompt_tokens_details/cached_tokens", "/prompt_cache_hit_tokens", "/cached_tokens"]),
                        // Haarness routers report Anthropic cache writes as
                        // `cache_creation_input_tokens`; OpenRouter as `cache_write_tokens`.
                        count(&["/prompt_tokens_details/cache_creation_input_tokens", "/prompt_tokens_details/cache_write_tokens"]),
                    ));
                }
                // An error reported inside the stream (OpenRouter, Haarness
                // routers) ends the request; it is not an incomplete reply.
                if let Some(error) = event.get("error").filter(|error| !error.is_null()) {
                    if let Some((input_tokens, output_tokens, cache_read_tokens, cache_write_tokens)) = usage.take() {
                        yield ProviderEvent::Usage { input_tokens, output_tokens, cache_read_tokens, cache_write_tokens }; if let Some(usd) = charged.take() { yield ProviderEvent::Cost { usd }; }
                    }
                    let message = crate::sse::upstream_error(error)
                        .or_else(|| error.get("message").and_then(Value::as_str).map(str::to_owned))
                        .unwrap_or_else(|| error.to_string());
                    // The status the error carries decides whether another try can help.
                    let message = match error.get("code").and_then(Value::as_u64).and_then(|code| reqwest::StatusCode::from_u16(u16::try_from(code).ok()?).ok()).filter(|status| status.is_client_error() || status.is_server_error()) {
                        Some(status) => format!("HTTP {status}: {message}"),
                        None => message,
                    };
                    Err(CoreError::Provider(message))?;
                }
                let delta=&event["choices"][0]["delta"];
                if let Some(text)=delta["content"].as_str().filter(|text| !text.is_empty()){
                    if native_json {
                        if json_output.is_empty() { yield ProviderEvent::ContentStarted; }
                        json_output.push_str(text);
                    } else { yield ProviderEvent::TextDelta(text.into()); }
                }
                if let Some(parts) = delta["content"].as_array() {
                    for part in parts {
                        mistral_content.push(part.clone());
                        if part["type"] == "text" && let Some(text) = part["text"].as_str() { yield ProviderEvent::TextDelta(text.into()); }
                        if part["type"] == "thinking" && let Some(thoughts) = part["thinking"].as_array() {
                            for thought in thoughts { if let Some(text) = thought["text"].as_str() { yield ProviderEvent::ReasoningDelta(text.into()); } }
                        }
                    }
                }
                let direct_reasoning = REASONING_FIELDS.into_iter().find_map(|field| {
                    delta
                        .get(field)
                        .and_then(Value::as_str)
                        .filter(|reasoning| !reasoning.is_empty())
                        .map(|reasoning| (field, reasoning))
                });
                if let Some((field, reasoning)) = direct_reasoning {
                    reasoning_field.get_or_insert(field);
                    reasoning_content.push_str(reasoning);
                    yield ProviderEvent::ReasoningDelta(reasoning.into());
                }
                let previous_direct = std::mem::replace(
                    &mut last_direct_reasoning,
                    direct_reasoning.map(|(_, reasoning)| reasoning.to_owned()).unwrap_or_default(),
                );
                if let Some(details) = delta.get("reasoning_details") {
                    append_reasoning_details(&mut reasoning_details, details);
                    // A router can send the same reasoning twice: as text, then
                    // in its own chunk as details. Details repeating the text
                    // just shown are kept for replay only.
                    let repeated = details.as_array().is_some_and(|details| {
                        let text: String = details.iter().filter_map(visible_reasoning).collect();
                        !text.is_empty() && text == previous_direct
                    });
                    if direct_reasoning.is_none()
                        && !repeated
                        && let Some(details) = details.as_array()
                    {
                        for detail in details {
                            if let Some(text) = visible_reasoning(detail) {
                                if emitted_reasoning
                                    .as_ref()
                                    .is_some_and(|previous| starts_new_reasoning_block(previous, detail))
                                {
                                    yield ProviderEvent::ReasoningDelta("\n\n".into());
                                }
                                emitted_reasoning = Some(detail.clone());
                                yield ProviderEvent::ReasoningDelta(text.into());
                            }
                        }
                    }
                }
                if let Some(deltas)=delta["tool_calls"].as_array(){
                    for tool in deltas {
                        let index = usize::try_from(tool["index"].as_u64().unwrap_or(0)).unwrap_or(0);
                        // A call's arguments are held until the stream ends;
                        // the call starts with its first fragment.
                        if calls.len() <= index {
                            calls.resize(index + 1, (String::new(), String::new(), String::new()));
                            yield ProviderEvent::ContentStarted;
                        }
                        if let Some(id)=tool["id"].as_str(){calls[index].0 = id.into();}
                        if let Some(name)=tool["function"]["name"].as_str(){calls[index].1 = name.into();}
                        if let Some(args)=tool["function"]["arguments"].as_str(){calls[index].2.push_str(args);}
                    }
                }
                if let Some(reason)=event["choices"][0]["finish_reason"].as_str(){finish_reason = Some(reason.to_owned());}
                if response_id.is_none() { response_id = event["id"].as_str().map(str::to_owned); }
            }
            let mut provider_metadata = json!({});
            if !reasoning_details.is_empty() {
                provider_metadata["reasoning_details"] = json!(reasoning_details);
                provider_metadata[REASONING_MODEL] = json!(reasoning_model);
            }
            // Kept with the provider that produced it (see the replay above).
            match replay {
                ReasoningReplay::Required => {
                    provider_metadata[reasoning_field.unwrap_or("reasoning_content")] = json!(reasoning_content);
                    provider_metadata[REASONING_SOURCE] = json!(provider_id);
                }
                // Structured details, when the server sends them, are what it
                // takes back; the plain text would only repeat them.
                ReasoningReplay::Observed => if let Some(field) = reasoning_field.filter(|_| reasoning_details.is_empty() && !reasoning_content.is_empty()) {
                    provider_metadata[field] = json!(reasoning_content);
                    provider_metadata[REASONING_SOURCE] = json!(provider_id);
                },
                ReasoningReplay::Never => {}
            }
            if !mistral_content.is_empty() { provider_metadata["mistral_content"] = json!(mistral_content); }
            if provider_metadata.as_object().is_some_and(serde_json::Map::is_empty) { provider_metadata = Value::Null; }
            // Report usage before the tool calls, whose arguments may not parse.
            if let Some((input_tokens, output_tokens, cache_read_tokens, cache_write_tokens)) = usage.take() {
                yield ProviderEvent::Usage { input_tokens, output_tokens, cache_read_tokens, cache_write_tokens }; if let Some(usd) = charged.take() { yield ProviderEvent::Cost { usd }; }
            }
            for (call_id, name, arguments) in calls.drain(..) {
                if !name.is_empty() {
                    let arguments = serde_json::from_str(&arguments).map_err(|error| CoreError::Provider(format!("OpenAI-compatible provider returned invalid tool arguments: {error}")))?;
                    if structured_output.is_some() && name == STRUCTURED_OUTPUT_TOOL_NAME {
                        yield ProviderEvent::StructuredOutput(arguments);
                    } else {
                        yield ProviderEvent::ToolCall{call_id,name,arguments,provider_metadata:provider_metadata.clone()};
                    }
                }
            }
            if native_json && !json_output.trim().is_empty() {
                let value = serde_json::from_str(&json_output).map_err(|error| CoreError::Provider(format!("OpenAI-compatible provider returned invalid structured output: {error}")))?;
                yield ProviderEvent::StructuredOutput(value);
            }
            let reason = finish_reason.unwrap_or_else(||"incomplete".into());
            crate::log_blocked_finish(&provider_id, &reason, request_id.as_deref(), response_id.as_deref());
            yield ProviderEvent::Finished{reason};
            if let Some(error) = broken_after_finish {
                Err::<(), _>(error)?;
            }
        }))
    }
}

fn openai_compatible_media_message(
    role: &str,
    content: &str,
    attachments: &[String],
    supports_images: bool,
    model: &str,
) -> CoreResult<Value> {
    // A photo sent on its own has no text. Claude refuses a blank text block,
    // including through a router that passes the request on to Anthropic, so
    // blank text is left out (as the native adapters do).
    let mut blocks = Vec::new();
    if !content.trim().is_empty() {
        blocks.push(json!({"type":"text","text":content}));
    }
    for item in crate::media::load_all(attachments)? {
        if item.is_image() {
            if !supports_images {
                return Err(CoreError::Provider(format!(
                    "model {model} does not advertise image input; set the provider images override only when its API supports images"
                )));
            }
            blocks.push(json!({"type":"image_url","image_url":{"url":item.data_url()}}));
        } else if item.is_pdf() {
            blocks.push(json!({
                "type":"file",
                "file":{"filename":item.filename,"file_data":item.data_url()}
            }));
        } else if item.is_audio() {
            let format = match item.mime_type.as_str() {
                "audio/mpeg" => "mp3",
                "audio/wav" => "wav",
                _ => {
                    return Err(CoreError::Provider(
                        "OpenAI-compatible audio input supports MP3 and WAV; use Gemini for other audio formats"
                            .into(),
                    ));
                }
            };
            blocks.push(json!({
                "type":"input_audio",
                "input_audio":{"data":item.base64,"format":format}
            }));
        } else if item.is_video() {
            return Err(CoreError::Provider(
                "OpenAI-compatible video input is not portable; use Gemini for video understanding"
                    .into(),
            ));
        }
    }
    Ok(json!({"role":role,"content":blocks}))
}
fn provider_error(error: impl std::fmt::Display) -> CoreError {
    CoreError::Provider(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use axum::{
        Json, Router,
        extract::State,
        http::header::CONTENT_TYPE,
        routing::{get, post},
    };
    use futures::StreamExt;
    use parking_lot::Mutex;
    use serde_json::{Value, json};
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    use haarness_core::{
        ModelProvider, ProviderEvent, ProviderMessage, ProviderRequest, ToolDefinition,
    };

    use crate::auth::{Credential, CredentialStore, XaiOAuthClient, XaiOAuthConfig};

    use super::{
        CompatibleInstance, CompatibleModelDefaults, OpenAiCompatibleProvider,
        append_reasoning_details, apply_ollama_details, deepseek_charge, deepseek_peak,
        discovered_model_info, model_info, openai_compatible_media_message,
    };

    #[derive(Clone, Default)]
    struct FixtureState(Arc<Mutex<Vec<Value>>>);

    async fn completions(
        State(state): State<FixtureState>,
        Json(request): Json<Value>,
    ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
        state.0.lock().push(request);
        // Reasoning aliases and structured details precede two interleaved
        // tool calls, followed by a final usage chunk.
        let stream = [
            r#"data: {"choices":[{"delta":{"content":"","reasoning_content":"thinking "}}]}"#,
            r#"data: {"choices":[{"delta":{"reasoning_content":"preferred","reasoning":"ignored"}}]}"#,
            r#"data: {"choices":[{"delta":{"reasoning":"then "}}]}"#,
            r#"data: {"choices":[{"delta":{"reasoning_content":"private ","reasoning_details":[{"type":"reasoning.text","text":"private ","signature":null,"format":null,"id":"reasoning-1","index":0}]}}]}"#,
            r#"data: {"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","text":"context","signature":"opaque-signature","format":"anthropic-claude-v1","id":"reasoning-1","index":0},{"type":"reasoning.encrypted","data":"opaque-data","id":"reasoning-2","format":"anthropic-claude-v1","index":1},{"type":"reasoning.summary","summary":"concise summary","id":"reasoning-3","format":"anthropic-claude-v1","index":2}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"read","arguments":"{\"path\":"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"list","arguments":"{\"path\":\".\"}"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.txt\"}"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            r#"data: {"choices":[],"usage":{"prompt_tokens":42,"completion_tokens":9,"prompt_tokens_details":{"cached_tokens":30}}}"#,
        ]
        .join("\n\n")
            + "\n\n";
        ([(CONTENT_TYPE, "text/event-stream")], stream)
    }

    async fn structured_completions(
        State(state): State<FixtureState>,
        Json(request): Json<Value>,
    ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
        state.0.lock().push(request);
        let stream = [
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"structured-call","function":{"name":"StructuredOutput","arguments":"{\"count\":5}"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            r#"data: {"choices":[],"usage":{"prompt_tokens":6,"completion_tokens":2}}"#,
        ]
        .join("\n\n")
            + "\n\n";
        ([(CONTENT_TYPE, "text/event-stream")], stream)
    }

    async fn xai_refresh(State(calls): State<Arc<AtomicUsize>>) -> Json<Value> {
        calls.fetch_add(1, Ordering::SeqCst);
        Json(json!({
            "access_token": "access-new",
            "refresh_token": "refresh-new",
            "expires_in": 3600
        }))
    }

    #[tokio::test]
    async fn an_xai_refresh_the_request_gave_up_on_is_still_saved() {
        // xAI spends a refresh token once it has it, and refuses a spent one.
        async fn slow_refresh(
            State(presented): State<Arc<parking_lot::Mutex<Vec<String>>>>,
            body: String,
        ) -> (axum::http::StatusCode, Json<Value>) {
            let token = url::form_urlencoded::parse(body.as_bytes())
                .find(|(name, _)| name == "refresh_token")
                .map(|(_, value)| value.into_owned())
                .unwrap_or_default();
            let spent = presented.lock().contains(&token);
            presented.lock().push(token);
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            if spent {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(json!({"error": "invalid_grant"})),
                );
            }
            (
                axum::http::StatusCode::OK,
                Json(json!({
                    "access_token": "access-new",
                    "refresh_token": "refresh-new",
                    "expires_in": 3600
                })),
            )
        }
        let presented = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let router = Router::new()
            .route("/token", post(slow_refresh))
            .with_state(presented.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let root = std::env::temp_dir().join(format!(
            "haarness-xai-cancelled-refresh-{}-{}",
            std::process::id(),
            crate::auth::unix_millis()
        ));
        let store = Arc::new(CredentialStore::new(root.join("auth.json")));
        store
            .set(
                "xai",
                &Credential::oauth("access-old".into(), "refresh-old".into(), 1, None),
            )
            .unwrap();
        let mut provider = OpenAiCompatibleProvider::new(CompatibleInstance {
            id: "xai".into(),
            name: "xAI".into(),
            base_url: "http://unused.invalid/v1".into(),
            api_key: None,
            models: vec!["grok-test".into()],
            discover_models: false,
            server_output_limit: false,
            replay_observed_reasoning: false,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: false,
            headers: crate::ProviderHeaders::default(),
        })
        .with_auth_store(Some(store.clone()));
        provider.xai_oauth = Some(XaiOAuthClient::new(XaiOAuthConfig {
            token_url: format!("http://{address}/token"),
            device_url: format!("http://{address}/device"),
            client_id: "test-client".into(),
            scope: "test".into(),
            user_agent: "haarness-test".into(),
        }));

        // A Stop while xAI is answering the refresh, which returns at once.
        let started = std::time::Instant::now();
        let stopped = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            provider.bearer_token(),
        )
        .await;
        assert!(stopped.is_err());
        assert!(started.elapsed() < std::time::Duration::from_millis(250));
        // The next request uses the token the refresh saved.
        assert_eq!(
            provider.bearer_token().await.unwrap().as_deref(),
            Some("access-new")
        );
        assert_eq!(presented.lock().len(), 1);
        let stored = store.get("xai").unwrap().unwrap();
        assert_eq!(stored.oauth_parts().unwrap().refresh, "refresh-new");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_xai_renewal_the_server_never_answers_frees_the_next_request() {
        let stalled = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let gate = stalled.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let router = Router::new()
            .route(
                "/token",
                post(move |state: State<Arc<AtomicUsize>>| {
                    let gate = gate.clone();
                    async move {
                        if gate.load(Ordering::SeqCst) {
                            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                        }
                        xai_refresh(state).await
                    }
                }),
            )
            .with_state(calls.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let root = std::env::temp_dir().join(format!(
            "haarness-xai-stalled-refresh-{}-{}",
            std::process::id(),
            crate::auth::unix_millis()
        ));
        let store = Arc::new(CredentialStore::new(root.join("auth.json")));
        store
            .set(
                "xai",
                &Credential::oauth("access-old".into(), "refresh-old".into(), 1, None),
            )
            .unwrap();
        let mut provider = OpenAiCompatibleProvider::new(CompatibleInstance {
            id: "xai".into(),
            name: "xAI".into(),
            base_url: "http://unused.invalid/v1".into(),
            api_key: None,
            models: vec!["grok-test".into()],
            discover_models: false,
            server_output_limit: false,
            replay_observed_reasoning: false,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: false,
            headers: crate::ProviderHeaders::default(),
        })
        .with_auth_store(Some(store.clone()));
        provider.renewal_limit = std::time::Duration::from_millis(300);
        provider.xai_oauth = Some(XaiOAuthClient::new(XaiOAuthConfig {
            token_url: format!("http://{address}/token"),
            device_url: format!("http://{address}/device"),
            client_id: "test-client".into(),
            scope: "test".into(),
            user_agent: "haarness-test".into(),
        }));
        let error =
            tokio::time::timeout(std::time::Duration::from_secs(10), provider.bearer_token())
                .await
                .expect("the renewal gave up within its limit")
                .expect_err("the stalled renewal failed")
                .to_string();
        assert!(
            error.contains("xAI did not answer the sign-in renewal in time"),
            "{error}"
        );
        stalled.store(false, Ordering::SeqCst);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(10), provider.bearer_token())
                .await
                .expect("the next request was not held by the lock")
                .unwrap()
                .as_deref(),
            Some("access-new")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn xai_oauth_refresh_is_single_flight_and_persisted() {
        let calls = Arc::new(AtomicUsize::new(0));
        let router = Router::new()
            .route("/token", post(xai_refresh))
            .with_state(calls.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let root = std::env::temp_dir().join(format!(
            "haarness-xai-auth-{}-{}",
            std::process::id(),
            crate::auth::unix_millis()
        ));
        let store = Arc::new(CredentialStore::new(root.join("auth.json")));
        store
            .set(
                "xai",
                &Credential::oauth("access-old".into(), "refresh-old".into(), 1, None),
            )
            .unwrap();
        let mut provider = OpenAiCompatibleProvider::new(CompatibleInstance {
            id: "xai".into(),
            name: "xAI".into(),
            base_url: "http://unused.invalid/v1".into(),
            api_key: None,
            models: vec!["grok-test".into()],
            discover_models: false,
            server_output_limit: false,
            replay_observed_reasoning: false,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: false,
            headers: crate::ProviderHeaders::default(),
        })
        .with_auth_store(Some(store.clone()));
        provider.xai_oauth = Some(XaiOAuthClient::new(XaiOAuthConfig {
            token_url: format!("http://{address}/token"),
            device_url: format!("http://{address}/device"),
            client_id: "test-client".into(),
            scope: "test".into(),
            user_agent: "haarness-test".into(),
        }));

        let (left, right) = tokio::join!(provider.bearer_token(), provider.bearer_token());
        assert_eq!(left.unwrap().as_deref(), Some("access-new"));
        assert_eq!(right.unwrap().as_deref(), Some("access-new"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let stored = store.get("xai").unwrap().unwrap();
        let oauth = stored.oauth_parts().unwrap();
        assert_eq!(oauth.access, "access-new");
        assert_eq!(oauth.refresh, "refresh-new");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_refused_xai_sign_in_is_renewed_once_and_the_request_sent_again() {
        // xAI refuses a token its stated expiry still covers.
        async fn chat(headers: axum::http::HeaderMap) -> axum::response::Response {
            use axum::response::IntoResponse as _;
            if headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                == Some("Bearer access-new")
            {
                return (
                    [(CONTENT_TYPE, "text/event-stream")],
                    "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                )
                    .into_response();
            }
            (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(json!({"error": "Incorrect API key provided"})),
            )
                .into_response()
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let router = Router::new()
            .route("/token", post(xai_refresh))
            .with_state(calls.clone())
            .route("/v1/chat/completions", post(chat));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let root = std::env::temp_dir().join(format!(
            "haarness-xai-refused-{}-{}",
            std::process::id(),
            crate::auth::unix_millis()
        ));
        let store = Arc::new(CredentialStore::new(root.join("auth.json")));
        let in_an_hour = crate::auth::unix_millis() + 3_600_000;
        store
            .set(
                "xai",
                &Credential::oauth("access-old".into(), "refresh-old".into(), in_an_hour, None),
            )
            .unwrap();
        let mut provider = OpenAiCompatibleProvider::new(CompatibleInstance {
            id: "xai".into(),
            name: "xAI".into(),
            base_url: format!("http://{address}/v1"),
            api_key: None,
            models: vec!["grok-test".into()],
            discover_models: false,
            server_output_limit: false,
            replay_observed_reasoning: false,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: false,
            headers: crate::ProviderHeaders::default(),
        })
        .with_auth_store(Some(store.clone()));
        provider.xai_oauth = Some(XaiOAuthClient::new(XaiOAuthConfig {
            token_url: format!("http://{address}/token"),
            device_url: format!("http://{address}/device"),
            client_id: "test-client".into(),
            scope: "test".into(),
            user_agent: "haarness-test".into(),
        }));
        let mut request = short_request();
        request.model = "grok-test".into();
        let events = provider
            .stream(request, CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Ok(ProviderEvent::TextDelta(text)) if text == "ok")),
            "{events:?}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            store
                .get("xai")
                .unwrap()
                .unwrap()
                .oauth_parts()
                .unwrap()
                .access,
            "access-new"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn custom_api_key_tracks_the_live_auth_store() {
        let root = std::env::temp_dir().join(format!(
            "haarness-custom-auth-{}-{}",
            std::process::id(),
            crate::auth::unix_millis()
        ));
        let store = Arc::new(CredentialStore::new(root.join("auth.json")));
        let provider = OpenAiCompatibleProvider::new(CompatibleInstance {
            id: "company-gateway".into(),
            name: "Company Gateway".into(),
            base_url: "https://models.example.test/v1".into(),
            api_key: None,
            models: vec!["company-model".into()],
            discover_models: false,
            server_output_limit: false,
            replay_observed_reasoning: false,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: false,
            headers: crate::ProviderHeaders::default(),
        })
        .with_auth_store(Some(store.clone()));

        assert!(!provider.info().authenticated);
        store
            .set("company-gateway", &Credential::api("fixture-key".into()))
            .unwrap();
        assert!(provider.info().authenticated);
        assert_eq!(
            provider.bearer_token().await.unwrap().as_deref(),
            Some("fixture-key")
        );
        store.remove("company-gateway").unwrap();
        assert!(!provider.info().authenticated);
        let _ = std::fs::remove_dir_all(root);
    }

    async fn models() -> Json<Value> {
        Json(json!({
            "object": "list",
            "data": [
                {
                    "id": "vendor/vision-reasoner",
                    "name": "Vision Reasoner",
                    "context_length": "131072",
                    "max_completion_tokens": 8192,
                    "architecture": {"input_modalities": ["text", "image", "video"]},
                    "supported_parameters": ["tools", "reasoning_effort"],
                    "pricing": {"prompt": "0.00000025", "completion": "0.000001"}
                },
                {
                    "id": "vendor/text-only",
                    "supported_parameters": []
                }
            ]
        }))
    }

    #[test]
    fn openrouter_media_generators_stay_out_of_the_picker() {
        let output = |id: &str, modalities: Value| {
            discovered_model_info(
                "openrouter",
                &json!({"id": id, "architecture": {"output_modalities": modalities}}),
                CompatibleModelDefaults::default(),
            )
        };
        assert!(output("google/gemini-3-pro-image", json!(["image", "text"])).is_none());
        assert!(output("openai/gpt-audio", json!(["text", "audio"])).is_none());
        assert!(output("openrouter/auto", json!(["text", "image"])).is_some());
        assert!(output("anthropic/claude-haiku-5.5", json!(["text"])).is_some());
        let routes = |id: &str, endpoints: Value| {
            discovered_model_info(
                "openrouter",
                &json!({"id": id, "endpoints": endpoints}),
                CompatibleModelDefaults::default(),
            )
        };
        assert!(routes("z-ai/glm-5.3-flash:batch", json!([])).is_none());
        assert!(
            routes(
                "z-ai/glm-5.3-flash",
                json!(["/api/v1/chat/completions", "/api/v1/responses"])
            )
            .is_some()
        );
    }

    #[test]
    fn openrouter_names_drop_the_vendor_its_id_already_says() {
        let named = |id: &str, name: &str| {
            discovered_model_info(
                "openrouter",
                &json!({"id": id, "name": name}),
                CompatibleModelDefaults::default(),
            )
            .unwrap()
            .display_name
        };
        assert_eq!(
            named("anthropic/claude-haiku-5.5", "Anthropic: Claude Haiku 5.5"),
            "Claude Haiku 5.5"
        );
        assert_eq!(named("z-ai/glm-5.3", "Z.ai: GLM 5.3"), "GLM 5.3");
        assert_eq!(
            named("openrouter/auto-beta", "Auto Router (Beta)"),
            "Auto Router (Beta)"
        );
        // Another provider's names stay as it gives them.
        let other = discovered_model_info(
            "together",
            &json!({"id": "x/y", "name": "Vendor: Y"}),
            CompatibleModelDefaults::default(),
        )
        .unwrap();
        assert_eq!(other.display_name, "Vendor: Y");
    }

    #[test]
    fn reasoning_details_go_back_only_to_the_model_that_wrote_them() {
        let deepseek =
            json!({"type": "reasoning.text", "text": "plan", "format": "unknown", "index": 0});
        let claude = json!({"type": "reasoning.text", "text": "plan", "signature": "sig", "format": "anthropic-claude-v1", "index": 0});
        let both = [deepseek.clone(), claude.clone()];
        // Tagged: the writer gets them all, any other model none.
        assert_eq!(
            super::replayed_details(
                &both,
                Some("deepseek/deepseek-v4-flash"),
                "deepseek/deepseek-v4-flash"
            )
            .len(),
            2
        );
        assert!(
            super::replayed_details(
                &both,
                Some("deepseek/deepseek-v4-flash"),
                "anthropic/claude-haiku-5.5"
            )
            .is_empty()
        );
        // Untagged: Claude takes only Anthropic's own; others keep what they had.
        assert_eq!(
            super::replayed_details(&both, None, "anthropic/claude-haiku-5.5"),
            [claude]
        );
        assert_eq!(
            super::replayed_details(&both, None, "x-ai/grok-4.7").len(),
            2
        );
    }

    #[test]
    fn openrouter_routers_have_no_price() {
        // OpenRouter lists `-1` for a router that picks the model per request.
        let model = discovered_model_info(
            "openrouter",
            &json!({"id": "openrouter/auto", "pricing": {"prompt": "-1", "completion": "-1"}}),
            CompatibleModelDefaults::default(),
        )
        .unwrap();
        assert_eq!(model.display_name, "Auto Router");
        assert!(model.pricing.is_none(), "{:?}", model.pricing);
        let free = discovered_model_info(
            "openrouter",
            &json!({"id": "openrouter/free", "pricing": {"prompt": "0", "completion": "0"}}),
            CompatibleModelDefaults::default(),
        )
        .unwrap();
        assert_eq!(free.pricing.map(|price| price.output), Some(0.0));
    }

    #[test]
    fn cloud_catalog_prices_override_builtin_estimates() {
        // The Haarness Cloud provider and a Cloud workspace's own router read the same price list.
        for provider in ["haarness", "router"] {
            let model = discovered_model_info(provider, &serde_json::json!({
                "id":"openai/gpt-4.1-nano",
                "pricing":{"prompt":"0.0000001","completion":"0.0000004","input_cache_read":"0.000000025"}
            }), CompatibleModelDefaults::default()).unwrap();
            let price = model.pricing.unwrap();
            assert!((price.input - 0.1).abs() < 1e-12, "{provider}");
            assert!((price.output - 0.4).abs() < 1e-12, "{provider}");
            assert!((price.cache_read - 0.025).abs() < 1e-12, "{provider}");
        }
    }

    #[test]
    fn a_listed_window_of_0_leaves_the_model_s_own_window() {
        let listed = |entry: Value| {
            discovered_model_info("router", &entry, CompatibleModelDefaults::default())
                .unwrap()
                .context_window
        };
        let known = listed(json!({"id": "openai/gpt-4.1-nano"}));
        assert!(known.is_some_and(|window| window > 0), "{known:?}");
        assert_eq!(
            listed(json!({"id": "openai/gpt-4.1-nano", "context_length": 0})),
            known
        );
        assert_eq!(
            listed(json!({"id": "future-model", "context_length": 0, "max_model_len": 65_536})),
            Some(65_536)
        );
        assert_eq!(
            listed(json!({"id": "future-model", "context_length": "0"})),
            None
        );
    }

    #[test]
    fn mistral_discovery_keeps_only_active_chat_completion_models() {
        let entries = json!([
            {
                "id": "mistral-chat",
                "archived": false,
                "max_context_length": 32768,
                "capabilities": {
                    "completion_chat": true,
                    "function_calling": true,
                    "vision": true
                }
            },
            {
                "id": "mistral-archived",
                "archived": true,
                "capabilities": {"completion_chat": true}
            },
            {
                "id": "mistral-embed",
                "archived": false,
                "capabilities": {"completion_chat": false}
            },
            {"id": "mistral-unknown-shape", "archived": false}
        ]);
        let models: Vec<_> = entries
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| {
                discovered_model_info("mistral", entry, CompatibleModelDefaults::default())
            })
            .collect();

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "mistral-chat");
        assert_eq!(models[0].context_window, Some(32_768));
        assert!(models[0].images);
        assert!(models[0].tools);
    }

    #[test]
    fn deepinfra_discovery_requires_chat_tag_and_parses_nested_metadata() {
        let entries = json!([
            {
                "id": "Qwen/Qwen3.5-397B-A17B",
                "metadata": {
                    "context_length": 262_144,
                    "max_tokens": 65536,
                    "tags": ["chat", "vlm", "vision", "reasoning", "reasoning_effort"],
                    "pricing": {
                        "input_tokens": 0.45,
                        "output_tokens": 3.0,
                        "cache_read_tokens": 0.22
                    }
                }
            },
            {"id": "BAAI/bge-large", "metadata": {"tags": ["embed"]}},
            {"id": "openai/whisper-large-v3", "metadata": {"tags": ["stt"]}},
            {"id": "black-forest-labs/FLUX.1", "metadata": {"tags": ["image-gen"]}},
            {"id": "Pixverse/Pixverse-T2V-HD", "metadata": {"tags": ["video-gen"]}},
            {"id": "legacy-entry-without-tags"}
        ]);
        let models: Vec<_> = entries
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| {
                discovered_model_info("deepinfra", entry, CompatibleModelDefaults::default())
            })
            .collect();

        assert_eq!(models.len(), 1);
        let model = &models[0];
        assert_eq!(model.id, "Qwen/Qwen3.5-397B-A17B");
        assert_eq!(model.context_window, Some(262_144));
        assert_eq!(model.max_output_tokens, Some(65_536));
        assert!(model.images);
        assert!(model.reasoning);
        let pricing = model.pricing.unwrap();
        assert!((pricing.input - 0.45).abs() < f64::EPSILON);
        assert!((pricing.output - 3.0).abs() < f64::EPSILON);
        assert!((pricing.cache_read - 0.22).abs() < f64::EPSILON);
    }

    #[test]
    fn groq_like_discovery_rejects_inactive_and_non_chat_entries() {
        let entries = json!([
            {"id": "openai/gpt-oss-120b", "active": true},
            {
                "id": "vendor/multimodal-chat",
                "architecture": {
                    "input_modalities": ["text", "image"],
                    "output_modalities": ["text", "image"]
                }
            },
            {"id": "whisper-large-v3", "active": true},
            {"id": "meta-llama/llama-guard-4-12b", "active": true},
            {"id": "vendor/embedding-model", "task": "feature-extraction"},
            {"id": "vendor/ranker", "type": "reranker"},
            {"id": "vendor/image-model", "type": "image-generation"},
            {
                "id": "vendor/image-output-only",
                "architecture": {"output_modalities": ["image"]}
            },
            {"id": "vendor/video-model", "pipeline_tag": "text-to-video"},
            {"id": "vendor/disabled-chat", "active": false},
            {"id": "vendor/archived-chat", "status": "archived"}
        ]);
        let ids: Vec<_> = entries
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| {
                discovered_model_info("groq", entry, CompatibleModelDefaults::default())
            })
            .map(|model| model.id)
            .collect();

        assert_eq!(ids, ["openai/gpt-oss-120b", "vendor/multimodal-chat"]);
    }

    #[test]
    fn reasoning_detail_merging_requires_matching_block_identity() {
        let mut details = Vec::new();
        append_reasoning_details(
            &mut details,
            &json!([{
                "type": "reasoning.text",
                "text": "same ",
                "signature": null,
                "format": null,
                "id": "block-a",
                "index": 0
            }]),
        );
        append_reasoning_details(
            &mut details,
            &json!([{
                "type": "reasoning.text",
                "text": "block",
                "signature": "signature-a",
                "format": "anthropic-claude-v1",
                "id": "block-a",
                "index": 0
            }]),
        );
        append_reasoning_details(
            &mut details,
            &json!([{
                "type": "reasoning.text",
                "text": "distinct",
                "signature": "signature-b",
                "format": "anthropic-claude-v1",
                "id": "block-b",
                "index": 1
            }]),
        );
        append_reasoning_details(&mut details, &json!({"malformed": true}));

        assert_eq!(details.len(), 2);
        assert_eq!(details[0]["text"], "same block");
        assert_eq!(details[0]["signature"], "signature-a");
        assert_eq!(details[0]["format"], "anthropic-claude-v1");
        assert_eq!(details[1]["text"], "distinct");
        assert_eq!(details[1]["id"], "block-b");
    }

    #[test]
    fn a_photo_sent_without_text_has_no_blank_text_block() {
        // Claude refuses a blank text block, and a router passes this
        // message on to Anthropic as it is.
        for content in ["", " \n"] {
            let message = openai_compatible_media_message(
                "user",
                content,
                &["data:image/png;name=photo-1.png;base64,iVBORw0KGgo=".into()],
                true,
                "anthropic/claude-sonnet-5.5",
            )
            .unwrap();
            let blocks = message["content"].as_array().unwrap();
            assert_eq!(blocks.len(), 1, "{content:?}: {blocks:?}");
            assert_eq!(blocks[0]["type"], "image_url");
        }
        let message = openai_compatible_media_message(
            "user",
            "What is this?",
            &["data:image/png;base64,iVBORw0KGgo=".into()],
            true,
            "anthropic/claude-sonnet-5.5",
        )
        .unwrap();
        assert_eq!(message["content"][0]["text"], "What is this?");
        assert_eq!(message["content"][1]["type"], "image_url");
    }

    #[test]
    fn serializes_pdf_and_audio_chat_content_without_claiming_video_portability() {
        let message = openai_compatible_media_message(
            "user",
            "Review the meeting",
            &[
                "data:application/pdf;name=agenda.pdf;base64,JVBERi0xLjQK".into(),
                "data:audio/mpeg;name=meeting.mp3;base64,SUQz".into(),
            ],
            false,
            "multimodal-test",
        )
        .unwrap();
        assert_eq!(message["content"][1]["type"], "file");
        assert_eq!(message["content"][1]["file"]["filename"], "agenda.pdf");
        assert_eq!(message["content"][2]["type"], "input_audio");
        assert_eq!(message["content"][2]["input_audio"]["format"], "mp3");

        let error = openai_compatible_media_message(
            "user",
            "Watch this",
            &["data:video/mp4;base64,AAAAIGZ0eXA=".into()],
            true,
            "multimodal-test",
        )
        .unwrap_err();
        assert!(error.to_string().contains("not portable"));
    }

    #[test]
    fn perplexity_static_models_have_documented_context_and_output_limits() {
        for (id, context) in [
            ("sonar", 128_000),
            ("sonar-pro", 200_000),
            ("sonar-reasoning-pro", 128_000),
            ("sonar-deep-research", 128_000),
        ] {
            let model = model_info("perplexity", id, CompatibleModelDefaults::default());
            assert_eq!(model.context_window, Some(context));
            assert_eq!(model.max_output_tokens, Some(128_000));
        }
    }

    async fn completions_recording_headers(
        State(state): State<FixtureState>,
        headers: axum::http::HeaderMap,
        Json(mut request): Json<Value>,
    ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
        request["_headers"] = json!(
            headers
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_str().unwrap_or("").to_owned()))
                .collect::<std::collections::BTreeMap<_, _>>()
        );
        state.0.lock().push(request);
        (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".into(),
        )
    }

    /// OpenRouter's refusal when the balance can't pay for the whole output
    /// limit, as it answered a live request.
    async fn completions_short_of_credit(
        State(state): State<FixtureState>,
        Json(request): Json<Value>,
    ) -> axum::response::Response {
        use axum::response::IntoResponse as _;
        let limit = request["max_tokens"].as_u64();
        state.0.lock().push(request);
        match limit {
            Some(limit) if limit <= 16_000 => (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            )
                .into_response(),
            _ => (
                axum::http::StatusCode::PAYMENT_REQUIRED,
                Json(json!({"error":{"code":402,"message":"This request requires more credits, or fewer max_tokens. You requested up to 65536 tokens, but can only afford 16000. To increase, visit https://openrouter.ai/settings/keys and adjust the key's monthly limit"}})),
            )
                .into_response(),
        }
    }

    async fn openrouter_fixture(
        handler: axum::routing::MethodRouter<FixtureState>,
    ) -> (OpenAiCompatibleProvider, FixtureState) {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/chat/completions", handler)
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = OpenAiCompatibleProvider::new(CompatibleInstance {
            id: "openrouter".into(),
            name: "OpenRouter".into(),
            base_url: format!("http://{address}"),
            api_key: Some("test-key".into()),
            models: vec!["google/gemini-3.5-flash-lite".into()],
            discover_models: false,
            server_output_limit: false,
            replay_observed_reasoning: false,
            model_defaults: CompatibleModelDefaults {
                max_output_tokens: Some(65_536),
                ..Default::default()
            },
            assume_authenticated: true,
            headers: crate::ProviderHeaders::default(),
        });
        (provider, state)
    }

    async fn deepseek_fixture(
        handler: axum::routing::MethodRouter<FixtureState>,
    ) -> (OpenAiCompatibleProvider, FixtureState) {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/chat/completions", handler)
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = OpenAiCompatibleProvider::new(CompatibleInstance {
            id: "deepseek".into(),
            name: "DeepSeek".into(),
            base_url: format!("http://{address}"),
            api_key: Some("test-key".into()),
            models: vec!["deepseek-flash".into()],
            discover_models: false,
            server_output_limit: false,
            replay_observed_reasoning: false,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: true,
            headers: crate::ProviderHeaders::default(),
        });
        (provider, state)
    }

    #[test]
    fn xais_language_models_give_images_levels_and_prices() {
        // Entries as xAI's `/language-models` listed them.
        let entry = json!({"id": "grok-4.3", "object": "model", "owned_by": "xai",
            "input_modalities": ["text", "image"], "output_modalities": ["text"],
            "prompt_text_token_price": 12500, "cached_prompt_text_token_price": 2000,
            "completion_text_token_price": 25000, "prompt_text_token_price_long_context": 25000,
            "cached_prompt_text_token_price_long_context": 4000, "completion_text_token_price_long_context": 50000,
            "long_context_threshold": 200_000,
            "capabilities": {"reasoning_effort": ["none", "low", "medium", "high", "xhigh"], "default_reasoning_effort": "low"}});
        let model =
            discovered_model_info("xai", &entry, CompatibleModelDefaults::default()).unwrap();
        assert!(model.images && model.reasoning);
        assert_eq!(
            model.reasoning_efforts,
            ["none", "low", "medium", "high", "xhigh"]
        );
        let price = model.pricing.unwrap();
        assert_eq!(
            (price.input, price.cache_read, price.output),
            (1.25, 0.2, 2.5)
        );
        let long = price.long_context.unwrap();
        assert_eq!(
            (
                long.above_input_tokens,
                long.input,
                long.output,
                long.cache_read
            ),
            (199_999, 2.5, 5.0, 0.4)
        );
        let fixed = discovered_model_info(
            "xai",
            &json!({"id": "grok-4.20-0309-reasoning", "input_modalities": ["text", "image"]}),
            CompatibleModelDefaults::default(),
        )
        .unwrap();
        assert!(fixed.reasoning && fixed.reasoning_efforts.is_empty());
        assert_eq!(fixed.display_name, "Grok 4.20 Reasoning");
        let plain = discovered_model_info(
            "xai",
            &json!({"id": "grok-4.20-0309-non-reasoning"}),
            CompatibleModelDefaults::default(),
        )
        .unwrap();
        assert!(!plain.reasoning);
        // Served only on the Responses API.
        assert!(
            discovered_model_info(
                "xai",
                &json!({"id": "grok-4.20-multi-agent-0309"}),
                CompatibleModelDefaults::default()
            )
            .is_none()
        );
    }

    #[test]
    fn ollama_details_say_what_a_local_model_can_do() {
        let mut qwen = model_info("ollama", "qwen3.8:27b", CompatibleModelDefaults::default());
        // Qwen 3.8 27B, as Ollama's `/api/show` described it.
        apply_ollama_details(
            &mut qwen,
            &json!({
                "capabilities": ["completion", "vision", "tools", "thinking"],
                "model_info": {"general.architecture": "qwen3_5", "qwen3_5.context_length": 262_144}
            }),
        );
        assert!(qwen.images && qwen.tools && qwen.reasoning);
        assert_eq!(qwen.reasoning_efforts, ["none", "low", "medium", "high"]);
        assert_eq!(qwen.context_window, Some(262_144));
        let mut oss = model_info("ollama", "gpt-oss:20b", CompatibleModelDefaults::default());
        apply_ollama_details(
            &mut oss,
            &json!({"capabilities": ["completion", "tools", "thinking"]}),
        );
        assert_eq!(oss.reasoning_efforts, ["low", "medium", "high"]);
        assert!(!oss.images);
        let mut plain = model_info("ollama", "llama-small", CompatibleModelDefaults::default());
        apply_ollama_details(&mut plain, &json!({"capabilities": ["completion"]}));
        assert!(!plain.tools && !plain.reasoning && plain.reasoning_efforts.is_empty());
    }

    #[test]
    fn deepseeks_own_listing_names_images_and_levels() {
        let entry = json!({"id": "deepseek-flash", "name": "DeepSeek-V4.1-Flash",
            "input_modalities": ["text", "image"], "effort": {"supported_levels": ["low", "high", "max"], "default_level": "high"}});
        let direct =
            discovered_model_info("deepseek", &entry, CompatibleModelDefaults::default()).unwrap();
        assert!(direct.images && direct.reasoning);
        assert_eq!(direct.reasoning_efforts, ["none", "low", "high", "max"]);
        assert_eq!(direct.display_name, "DeepSeek V4.1 Flash");
        let mut routed = entry.clone();
        routed["id"] = json!("deepseek/deepseek-flash");
        let routed =
            discovered_model_info("router", &routed, CompatibleModelDefaults::default()).unwrap();
        assert!(routed.images);
        assert_eq!(routed.reasoning_efforts, ["low", "high", "max"]);
        // A self-hosted router lists no price: DeepSeek's own, at peak.
        assert_eq!(routed.pricing.map(|price| price.input), Some(0.30));
    }

    #[test]
    fn deepseek_bills_peak_hours_on_weekdays_only() {
        use chrono::TimeZone as _;
        let at = |day: u32, hour: u32, minute: u32| {
            chrono::Utc
                .with_ymd_and_hms(2026, 11, day, hour, minute, 0)
                .unwrap()
        };
        // 2026-11-02 is a Monday, 2026-11-01 a Sunday.
        assert!(deepseek_peak(at(2, 1, 0)));
        assert!(deepseek_peak(at(2, 9, 59)));
        assert!(!deepseek_peak(at(2, 4, 0)));
        assert!(!deepseek_peak(at(2, 5, 30)));
        assert!(!deepseek_peak(at(2, 10, 0)));
        assert!(!deepseek_peak(at(1, 2, 0)));
        let usage = json!({"prompt_tokens": 30_000, "prompt_cache_hit_tokens": 10_000, "completion_tokens": 100});
        let off_peak = deepseek_charge("deepseek-flash", at(1, 2, 0), &usage).unwrap();
        assert!(
            (off_peak - (20_000.0 * 0.15 + 10_000.0 * 0.003 + 100.0 * 0.6) / 1e6).abs() < 1e-12,
            "{off_peak}"
        );
        let peak = deepseek_charge("deepseek-v4-pro", at(2, 2, 0), &usage).unwrap();
        assert!(
            (peak - (20_000.0 * 1.32 + 10_000.0 * 0.044 + 100.0 * 3.96) / 1e6).abs() < 1e-12,
            "{peak}"
        );
        assert!(deepseek_charge("deepseek-next", at(2, 2, 0), &usage).is_none());
        let flash = model_info(
            "deepseek",
            "deepseek-flash",
            CompatibleModelDefaults::default(),
        );
        assert_eq!(
            flash.pricing.map(|price| (price.input, price.output)),
            Some((0.30, 1.20))
        );
    }

    #[tokio::test]
    async fn deepseek_gets_structured_output_without_a_forced_call_while_it_thinks() {
        async fn charged(
            State(state): State<FixtureState>,
            Json(request): Json<Value>,
        ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
            state.0.lock().push(request);
            (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":30000,\"prompt_cache_hit_tokens\":10000,\"completion_tokens\":100}}\n\ndata: [DONE]\n\n".into(),
            )
        }
        let (provider, state) = deepseek_fixture(post(charged)).await;
        let mut costs = Vec::new();
        for effort in ["high", "none"] {
            let mut request = short_request();
            request.model = "deepseek-flash".into();
            request.reasoning_effort = Some(effort.into());
            request.output_schema =
                Some(json!({"type":"object","properties":{"answer":{"type":"string"}}}));
            let events = provider
                .stream(request, CancellationToken::new())
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await;
            costs.extend(events.iter().filter_map(|event| match event {
                Ok(ProviderEvent::Cost { usd }) => Some(*usd),
                _ => None,
            }));
        }
        {
            let requests = state.0.lock();
            assert_eq!(requests[0]["thinking"]["type"], "enabled");
            assert!(
                requests[0].get("tool_choice").is_none(),
                "{:?}",
                requests[0]
            );
            assert_eq!(requests[1]["thinking"]["type"], "disabled");
            assert_eq!(requests[1]["tool_choice"], "required");
            // Every request says what it cost, at the hour it ran.
            assert_eq!(costs.len(), 2);
            assert!(costs.iter().all(|cost| *cost > 0.0));
        }
        // A DeepSeek model behind a router thinks too.
        let (routed, routed_state) = openrouter_fixture(post(completions_recording_headers)).await;
        let mut request = short_request();
        request.model = "deepseek/deepseek-v4-pro".into();
        request.reasoning_effort = Some("high".into());
        request.output_schema =
            Some(json!({"type":"object","properties":{"answer":{"type":"string"}}}));
        let _ = routed
            .stream(request, CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert!(routed_state.0.lock()[0].get("tool_choice").is_none());
    }

    fn short_request() -> ProviderRequest {
        ProviderRequest {
            model: "google/gemini-3.5-flash-lite".into(),
            system: "sys".into(),
            messages: vec![ProviderMessage::Text {
                role: "user".into(),
                content: "hi".into(),
            }],
            tools: vec![],
            reasoning_effort: None,
            output_schema: None,
            parameters: haarness_core::RequestParameters::default(),
        }
    }

    #[tokio::test]
    async fn openrouter_keys_are_checked_where_a_made_up_key_is_refused() {
        // OpenRouter lists its models for any key; only `/key` knows them.
        async fn key(headers: axum::http::HeaderMap) -> axum::http::StatusCode {
            if headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                == Some("Bearer known")
            {
                axum::http::StatusCode::OK
            } else {
                axum::http::StatusCode::UNAUTHORIZED
            }
        }
        let state = FixtureState::default();
        let router = Router::new()
            .route(
                "/models",
                axum::routing::get(|| async { Json(json!({"data": []})) }),
            )
            .route("/key", axum::routing::get(key))
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = OpenAiCompatibleProvider::new(CompatibleInstance {
            id: "openrouter".into(),
            name: "OpenRouter".into(),
            base_url: format!("http://{address}"),
            api_key: None,
            models: vec![],
            discover_models: true,
            server_output_limit: false,
            replay_observed_reasoning: false,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: false,
            headers: crate::ProviderHeaders::default(),
        });
        assert!(matches!(
            provider.check_api_key("known").await,
            haarness_core::KeyCheck::Accepted
        ));
        assert!(!matches!(
            provider.check_api_key("made-up").await,
            haarness_core::KeyCheck::Accepted
        ));
    }

    #[tokio::test]
    async fn an_upstream_failure_inside_the_stream_keeps_its_status() {
        async fn unavailable(
            State(state): State<FixtureState>,
            Json(request): Json<Value>,
        ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
            state.0.lock().push(request);
            (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                ": OPENROUTER PROCESSING\n\ndata: {\"choices\":[],\"error\":{\"code\":502,\"message\":\"data: {\\\"error\\\":{\\\"code\\\":\\\"TimeoutError\\\",\\\"message\\\":\\\"timed out during opening handshake\\\"}}\\n\\n\",\"metadata\":{\"error_type\":\"provider_unavailable\"}}}\n\n".into(),
            )
        }
        let (provider, _) = openrouter_fixture(post(unavailable)).await;
        let events = provider
            .stream(short_request(), CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        let Some(Err(haarness_core::CoreError::Provider(message))) = events.last() else {
            panic!("{events:?}");
        };
        assert_eq!(
            message,
            "HTTP 502 Bad Gateway: timed out during opening handshake"
        );
    }

    #[tokio::test]
    async fn a_forced_call_to_gemini_through_a_gateway_drops_array_bounds() {
        let (provider, state) = openrouter_fixture(post(completions_recording_headers)).await;
        for model in ["google/gemini-3.8-flash", "anthropic/claude-haiku-5.5"] {
            let mut request = short_request();
            request.model = model.into();
            request.output_schema =
                Some(json!({"type":"object","properties":{"answer":{"type":"string"}}}));
            request.tools = vec![haarness_core::ToolDefinition {
                name: "fill".into(),
                description: "fill a form".into(),
                input_schema: json!({"type":"object","properties":{"fields":{"type":"array","maxItems":100,"items":{"type":"string"}}}}),
            }];
            let _ = provider
                .stream(request, CancellationToken::new())
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await;
        }
        let requests = state.0.lock();
        let bound = |index: usize| {
            requests[index]["tools"][0]["function"]["parameters"]["properties"]["fields"]
                .get("maxItems")
                .cloned()
        };
        assert_eq!(requests[0]["tool_choice"], "required");
        assert_eq!(bound(0), None);
        assert_eq!(bound(1), Some(json!(100)));
    }

    #[tokio::test]
    async fn a_thinking_qwen_model_is_asked_for_structured_output_without_a_forced_call() {
        let (mut provider, state) = openrouter_fixture(post(completions_recording_headers)).await;
        provider.models = ["qwen/qwen3.8-flash", "qwen/qwen-plain", "z-ai/glm-5.3"]
            .into_iter()
            .map(|id| haarness_protocol::ModelInfo {
                reasoning: id != "qwen/qwen-plain",
                ..model_info("openrouter", id, CompatibleModelDefaults::default())
            })
            .collect();
        for model in ["qwen/qwen3.8-flash", "qwen/qwen-plain", "z-ai/glm-5.3"] {
            let mut request = short_request();
            request.model = model.into();
            request.output_schema =
                Some(json!({"type":"object","properties":{"answer":{"type":"string"}}}));
            let _ = provider
                .stream(request, CancellationToken::new())
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await;
        }
        let requests = state.0.lock();
        assert!(
            requests[0].get("tool_choice").is_none(),
            "{:?}",
            requests[0]
        );
        assert_eq!(requests[1]["tool_choice"], "required");
        assert_eq!(requests[2]["tool_choice"], "required");
    }

    #[tokio::test]
    async fn openrouter_says_what_a_request_cost() {
        async fn charged(
            State(state): State<FixtureState>,
            Json(request): Json<Value>,
        ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
            state.0.lock().push(request);
            (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":11125,\"completion_tokens\":45,\"cost\":0.0085125}}\n\ndata: [DONE]\n\n".into(),
            )
        }
        let (provider, _) = openrouter_fixture(post(charged)).await;
        let events = provider
            .stream(short_request(), CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        let usage = events.iter().position(|event| {
            matches!(
                event,
                Ok(ProviderEvent::Usage {
                    input_tokens: 11_125,
                    ..
                })
            )
        });
        let cost = events.iter().position(|event| matches!(event, Ok(ProviderEvent::Cost { usd }) if (*usd - 0.008_512_5).abs() < 1e-12));
        assert!(
            usage.is_some() && cost == usage.map(|index| index + 1),
            "{events:?}"
        );
    }

    #[tokio::test]
    async fn a_refused_forced_tool_call_is_asked_again_without_forcing() {
        // Meta's Muse through OpenRouter, as it answered a live request.
        async fn auto_only(
            State(state): State<FixtureState>,
            Json(request): Json<Value>,
        ) -> axum::response::Response {
            use axum::response::IntoResponse as _;
            let forced = request.get("tool_choice").is_some();
            state.0.lock().push(request);
            if forced {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(json!({"error":{"code":400,"message":"Provider returned error","metadata":{"raw":"{\"error\":{\"message\":\"only `\\\"auto\\\"` is supported for `tool_choice`.\"}}","provider_name":"Meta"}}})),
                )
                    .into_response();
            }
            (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            )
                .into_response()
        }
        let (provider, state) = openrouter_fixture(post(auto_only)).await;
        let mut request = short_request();
        request.model = "meta/muse-spark-1.3".into();
        request.output_schema =
            Some(json!({"type":"object","properties":{"answer":{"type":"string"}}}));
        let events = provider
            .stream(request, CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert!(events.iter().all(Result::is_ok), "{events:?}");
        let requests = state.0.lock();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["tool_choice"], "required");
        assert!(requests[1].get("tool_choice").is_none());
        // The structured output tool is still offered.
        assert!(
            requests[1]["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty())
        );
    }

    #[tokio::test]
    async fn reasoning_sent_again_as_details_in_its_own_chunk_shows_once() {
        async fn split(
            State(state): State<FixtureState>,
            Json(request): Json<Value>,
        ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
            state.0.lock().push(request);
            (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                [
                    r#"data: {"choices":[{"delta":{"reasoning":"Reading the file.","reasoning_content":"Reading the file."}}]}"#,
                    r#"data: {"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","text":"Reading the file.","index":0}]}}]}"#,
                    r#"data: {"choices":[{"delta":{"content":"done"},"finish_reason":"stop"}]}"#,
                    "data: [DONE]",
                ]
                .join("\n\n")
                    + "\n\n",
            )
        }
        let (provider, _) = openrouter_fixture(post(split)).await;
        let events = provider
            .stream(short_request(), CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        let reasoning: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Ok(ProviderEvent::ReasoningDelta(text)) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(reasoning, ["Reading the file."]);
    }

    #[tokio::test]
    async fn text_beside_tool_calls_joins_their_message() {
        // DeepSeek refused a history with the text between a call and its result.
        let (provider, state) = openrouter_fixture(post(completions_recording_headers)).await;
        let mut request = short_request();
        request.messages = vec![
            ProviderMessage::Text {
                role: "user".into(),
                content: "read it".into(),
            },
            ProviderMessage::ToolCall {
                call_id: "call-1".into(),
                name: "read".into(),
                arguments: json!({"path": "a.txt"}),
                provider_metadata: Value::Null,
            },
            ProviderMessage::Text {
                role: "assistant".into(),
                content: "Reading the file.".into(),
            },
            ProviderMessage::ToolResult {
                call_id: "call-1".into(),
                name: "read".into(),
                output: "marker".into(),
                is_error: false,
                attachments: vec![],
            },
        ];
        let _ = provider
            .stream(request, CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        let sent = state.0.lock()[0]["messages"].clone();
        let roles: Vec<&str> = sent
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["system", "user", "assistant", "tool"], "{sent}");
        assert_eq!(sent[2]["content"], "Reading the file.");
        assert_eq!(sent[2]["tool_calls"][0]["id"], "call-1");
    }

    #[tokio::test]
    async fn openrouter_asks_again_for_the_output_the_balance_covers() {
        let (provider, state) = openrouter_fixture(post(completions_short_of_credit)).await;
        let events = provider
            .stream(short_request(), CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Ok(ProviderEvent::TextDelta(text)) if text == "ok"))
        );
        let requests = state.0.lock();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["max_tokens"], 65_536);
        assert_eq!(requests[1]["max_tokens"], 16_000);
    }

    #[tokio::test]
    async fn openrouter_keeps_its_refusal_when_the_balance_covers_too_little() {
        async fn nearly_empty(
            State(state): State<FixtureState>,
            Json(request): Json<Value>,
        ) -> (axum::http::StatusCode, Json<Value>) {
            state.0.lock().push(request);
            (
                axum::http::StatusCode::PAYMENT_REQUIRED,
                Json(
                    json!({"error":{"code":402,"message":"This request requires more credits, or fewer max_tokens. You requested up to 65536 tokens, but can only afford 900."}}),
                ),
            )
        }
        let (provider, state) = openrouter_fixture(post(nearly_empty)).await;
        let Err(haarness_core::CoreError::Provider(message)) = provider
            .stream(short_request(), CancellationToken::new())
            .await
        else {
            panic!("a balance that covers 900 tokens must refuse");
        };
        assert!(
            message.starts_with("HTTP 402 Payment Required: "),
            "{message}"
        );
        assert!(message.contains("can only afford 900"), "{message}");
        assert_eq!(state.0.lock().len(), 1);
    }

    /// A server that streams `events` in a chunked body and then closes the
    /// connection without ending the body, as a connection cut short does.
    async fn cut_after(events: Vec<&'static str>) -> String {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            // The whole request, so closing never cuts it off.
            let mut request = Vec::new();
            let mut buffer = [0_u8; 8192];
            loop {
                let read = socket.read(&mut buffer).await.unwrap();
                request.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&request);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|value| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if body.len() >= length {
                        break;
                    }
                }
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            for event in events {
                let frame = format!("data: {event}\n\n");
                socket
                    .write_all(format!("{:x}\r\n{frame}\r\n", frame.len()).as_bytes())
                    .await
                    .unwrap();
            }
            socket.flush().await.unwrap();
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn a_connection_cut_after_the_finish_reason_finishes_the_response_first() {
        let request = || ProviderRequest {
            model: "compat-test".into(),
            system: "sys".into(),
            messages: vec![ProviderMessage::Text {
                role: "user".into(),
                content: "read it".into(),
            }],
            tools: vec![],
            reasoning_effort: None,
            output_schema: None,
            parameters: haarness_core::RequestParameters::default(),
        };
        let provider = |base_url| {
            OpenAiCompatibleProvider::new(CompatibleInstance {
                id: "compat".into(),
                name: "Compatible".into(),
                base_url,
                api_key: None,
                models: vec!["compat-test".into()],
                discover_models: false,
                server_output_limit: false,
                replay_observed_reasoning: false,
                model_defaults: CompatibleModelDefaults::default(),
                assume_authenticated: true,
                headers: crate::ProviderHeaders::default(),
            })
        };
        let call = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read","arguments":"{\"path\":\"a.txt\"}"}}]}}]}"#;
        // The finish reason arrived: the call and the finish come first, then
        // what broke the stream.
        let finished = provider(
            cut_after(vec![
                call,
                r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            ])
            .await,
        )
        .stream(request(), CancellationToken::new())
        .await
        .unwrap()
        .filter(|event| std::future::ready(!matches!(event, Ok(ProviderEvent::ContentStarted))))
        .collect::<Vec<_>>()
        .await;
        assert!(
            matches!(&finished[0], Ok(ProviderEvent::ToolCall { call_id, name, .. }) if call_id == "c1" && name == "read"),
            "{finished:?}"
        );
        assert!(
            matches!(&finished[1], Ok(ProviderEvent::Finished { reason }) if reason == "tool_calls"),
            "{finished:?}"
        );
        assert!(
            matches!(&finished[2], Err(haarness_core::CoreError::Provider(message)) if message.contains("body")),
            "{finished:?}"
        );
        assert_eq!(finished.len(), 3);
        // Before the finish reason, the response is cut short: only the error.
        let cut = provider(cut_after(vec![call]).await)
            .stream(request(), CancellationToken::new())
            .await
            .unwrap()
            .filter(|event| std::future::ready(!matches!(event, Ok(ProviderEvent::ContentStarted))))
            .collect::<Vec<_>>()
            .await;
        assert!(
            matches!(cut.as_slice(), [Err(haarness_core::CoreError::Provider(_))]),
            "{cut:?}"
        );
    }

    #[tokio::test]
    async fn managed_output_allowance_is_chosen_by_server_unless_explicit() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/chat/completions", post(completions_recording_headers))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let mut provider = OpenAiCompatibleProvider::new(CompatibleInstance {
            id: "router".into(),
            name: "Managed router".into(),
            base_url: format!("http://{address}"),
            api_key: None,
            models: vec!["compat-test".into()],
            discover_models: false,
            server_output_limit: true,
            replay_observed_reasoning: false,
            model_defaults: CompatibleModelDefaults {
                max_output_tokens: Some(8192),
                ..Default::default()
            },
            assume_authenticated: true,
            headers: crate::ProviderHeaders::default(),
        });
        provider.models[0].reasoning = true;
        provider.models[0].reasoning_efforts = vec!["high".into()];
        assert_eq!(provider.info().models[0].max_output_tokens, Some(8192));
        for limit in [None, Some(512)] {
            let request = ProviderRequest {
                model: "compat-test".into(),
                system: "sys".into(),
                messages: vec![ProviderMessage::Text {
                    role: "user".into(),
                    content: "hi".into(),
                }],
                tools: vec![],
                reasoning_effort: Some("high".into()),
                output_schema: None,
                parameters: haarness_core::RequestParameters {
                    max_output_tokens: limit,
                    ..Default::default()
                },
            };
            let events = provider
                .stream(request, CancellationToken::new())
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await;
            assert!(events.iter().all(Result::is_ok));
        }
        let requests = state.0.lock();
        assert!(requests[0].get("max_tokens").is_none());
        assert_eq!(requests[0]["reasoning_effort"], "high");
        assert_eq!(requests[1]["max_tokens"], 512);
    }

    #[tokio::test]
    async fn routed_claude_requests_mark_the_prompt_cache_and_others_do_not() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/chat/completions", post(completions_recording_headers))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = OpenAiCompatibleProvider::new(CompatibleInstance {
            id: "router".into(),
            name: "Managed router".into(),
            base_url: format!("http://{address}"),
            api_key: None,
            models: vec![
                "anthropic/claude-sonnet-5.5".into(),
                "openai/gpt-6.1-sol".into(),
            ],
            discover_models: false,
            server_output_limit: true,
            replay_observed_reasoning: false,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: true,
            headers: crate::ProviderHeaders::default(),
        });
        // The last: a compaction summary, which nothing reads back.
        for (model, uncached) in [
            ("anthropic/claude-sonnet-5.5", false),
            ("openai/gpt-6.1-sol", false),
            ("anthropic/claude-sonnet-5.5", true),
        ] {
            let text = |role: &str, content: &str| ProviderMessage::Text {
                role: role.into(),
                content: content.into(),
            };
            let request = ProviderRequest {
                model: model.into(),
                system: "sys".into(),
                messages: vec![
                    text("user", "first"),
                    text("assistant", "answer"),
                    text("user", "second"),
                    text("user", "third"),
                ],
                tools: vec![],
                reasoning_effort: None,
                output_schema: None,
                parameters: haarness_core::RequestParameters {
                    uncached,
                    ..haarness_core::RequestParameters::default()
                },
            };
            let events = provider
                .stream(request, CancellationToken::new())
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await;
            assert!(events.iter().all(Result::is_ok));
        }
        let requests = state.0.lock();
        let marked = |request: &Value| -> Vec<bool> {
            request["messages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|message| message["cache_control"]["type"] == "ephemeral")
                .collect()
        };
        // system, first, answer, second, third: the system prompt and the two newest user turns.
        assert_eq!(marked(&requests[0]), vec![true, false, false, true, true]);
        assert!(
            marked(&requests[1]).iter().all(|marked| !marked),
            "only Anthropic accepts cache_control"
        );
        assert!(
            marked(&requests[2]).iter().all(|marked| !marked),
            "a request nothing reads back writes no cache entry"
        );
        // The runtime holds back concurrent siblings only where markers go out.
        assert!(
            provider
                .explicit_prompt_cache_scope("anthropic/claude-sonnet-5.5")
                .is_some()
        );
        assert!(
            provider
                .explicit_prompt_cache_scope("openai/gpt-6.1-sol")
                .is_none()
        );
        let direct = OpenAiCompatibleProvider::new(CompatibleInstance {
            id: "openrouter".into(),
            name: "OpenRouter".into(),
            base_url: format!("http://{address}"),
            api_key: None,
            models: vec!["anthropic/claude-sonnet-5.5".into()],
            discover_models: false,
            server_output_limit: false,
            replay_observed_reasoning: false,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: true,
            headers: crate::ProviderHeaders::default(),
        });
        assert!(
            direct
                .explicit_prompt_cache_scope("anthropic/claude-sonnet-5.5")
                .is_none()
        );
    }

    /// A Haarness router hands a Claude request's tool parameters to Anthropic
    /// as `input_schema`, so it fails on the same root schemas.
    #[tokio::test]
    async fn routed_claude_requests_omit_tools_whose_root_schema_claude_rejects() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/chat/completions", post(completions_recording_headers))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let models = vec![
            "anthropic/claude-sonnet-5.5".into(),
            "openai/gpt-6.1-sol".into(),
        ];
        let instance = |id: &str, server_output_limit: bool| CompatibleInstance {
            id: id.into(),
            name: id.into(),
            base_url: format!("http://{address}"),
            api_key: None,
            models: models.clone(),
            discover_models: false,
            server_output_limit,
            replay_observed_reasoning: false,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: true,
            headers: crate::ProviderHeaders::default(),
        };
        let tools = vec![
            ToolDefinition {
                name: "plain".into(),
                description: "plain".into(),
                input_schema: json!({"type": "object"}),
            },
            ToolDefinition {
                name: "mcp__fixture__either".into(),
                description: "either".into(),
                input_schema: json!({"type": "object", "anyOf": [{"required": ["a"]}, {"required": ["b"]}]}),
            },
        ];
        for (provider, model) in [
            ("router", "anthropic/claude-sonnet-5.5"),
            ("router", "openai/gpt-6.1-sol"),
            ("openrouter", "anthropic/claude-sonnet-5.5"),
        ] {
            let provider = OpenAiCompatibleProvider::new(instance(provider, provider == "router"));
            let request = ProviderRequest {
                model: model.into(),
                system: "sys".into(),
                messages: vec![ProviderMessage::Text {
                    role: "user".into(),
                    content: "list".into(),
                }],
                tools: tools.clone(),
                reasoning_effort: None,
                output_schema: None,
                parameters: haarness_core::RequestParameters::default(),
            };
            let events = provider
                .stream(request, CancellationToken::new())
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await;
            assert!(events.iter().all(Result::is_ok));
        }
        let requests = state.0.lock();
        let count = |request: &Value| request["tools"].as_array().map_or(0, Vec::len);
        assert_eq!(count(&requests[0]), 1);
        assert_eq!(requests[0]["tools"][0]["function"]["name"], "plain");
        assert_eq!(count(&requests[1]), 2);
        assert_eq!(count(&requests[2]), 2, "only Haarness routers are filtered");
    }

    #[tokio::test]
    async fn hook_request_parameters_and_headers_reach_the_wire() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/chat/completions", post(completions_recording_headers))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider =
            OpenAiCompatibleProvider::for_test(format!("http://{address}/chat/completions"));
        let request = ProviderRequest {
            model: "compat-test".into(),
            system: "sys".into(),
            messages: vec![ProviderMessage::Text {
                role: "user".into(),
                content: "go".into(),
            }],
            tools: vec![],
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters {
                temperature: Some(0.25),
                top_p: Some(0.9),
                top_k: Some(40),
                max_output_tokens: Some(512),
                headers: std::collections::BTreeMap::from([
                    ("x-fixture".to_owned(), "yes".to_owned()),
                    ("bad header".to_owned(), "skipped".to_owned()),
                ]),
                options: serde_json::Map::from_iter([("seed".to_owned(), json!(7))]),
                cache_key: None,
                limits: haarness_core::StreamLimits::default(),
                uncached: false,
            },
            output_schema: None,
        };
        let events: Vec<_> = provider
            .stream(request, CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert!(events.iter().all(Result::is_ok));
        let requests = state.0.lock();
        assert_eq!(requests[0]["temperature"], 0.25);
        assert_eq!(requests[0]["top_p"], 0.9);
        assert_eq!(
            requests[0]["max_tokens"], 512,
            "hook limit overrides the model default"
        );
        assert_eq!(requests[0]["seed"], 7);
        assert!(
            requests[0].get("top_k").is_none(),
            "chat completions has no top_k field"
        );
        assert_eq!(requests[0]["_headers"]["x-fixture"], "yes");
        assert!(requests[0]["_headers"].get("bad header").is_none());
    }

    /// Streams `body` as the provider's whole SSE response and collects every
    /// event, errors included.
    async fn stream_events(
        body: &'static str,
    ) -> Vec<Result<ProviderEvent, haarness_core::CoreError>> {
        let router = Router::new().route(
            "/chat/completions",
            post(move || async move { ([("content-type", "text/event-stream")], body) }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider =
            OpenAiCompatibleProvider::for_test(format!("http://{address}/chat/completions"));
        let request = ProviderRequest {
            model: "compat-test".into(),
            system: "sys".into(),
            messages: vec![ProviderMessage::Text {
                role: "user".into(),
                content: "go".into(),
            }],
            tools: Vec::new(),
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        };
        provider
            .stream(request, CancellationToken::new())
            .await
            .unwrap()
            .collect()
            .await
    }

    fn reported(
        events: &[Result<ProviderEvent, haarness_core::CoreError>],
    ) -> Vec<(u64, u64, u64, u64)> {
        events
            .iter()
            .filter_map(|event| match event {
                Ok(ProviderEvent::Usage {
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                }) => Some((
                    input_tokens.to_owned(),
                    output_tokens.to_owned(),
                    cache_read_tokens.to_owned(),
                    cache_write_tokens.to_owned(),
                )),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn usage_is_read_wherever_providers_put_it() {
        // Moonshot: usage on the choice; DeepSeek: cache hits named its own way.
        let events = stream_events(concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\",\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":5,\"prompt_cache_hit_tokens\":60}}]}\n\n",
            "data: [DONE]\n\n"
        ))
        .await;
        assert_eq!(reported(&events), [(100, 5, 60, 0)]);
        // OpenRouter: cache writes as `cache_write_tokens`.
        let events = stream_events(concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":300,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":0,\"cache_write_tokens\":250}}}\n\n",
            "data: [DONE]\n\n"
        ))
        .await;
        assert_eq!(reported(&events), [(300, 2, 0, 250)]);
    }

    #[tokio::test]
    async fn usage_already_received_survives_errors() {
        // An error frame inside the stream fails the request after the usage
        // it already reported, rather than ending as an incomplete reply.
        let events = stream_events(concat!(
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":80,\"completion_tokens\":3}}\n\n",
            "data: {\"error\":{\"message\":\"Upstream overloaded\",\"code\":\"upstream_529\"}}\n\n",
            "data: [DONE]\n\n"
        ))
        .await;
        assert_eq!(reported(&events), [(80, 3, 0, 0)]);
        assert!(matches!(
            events.last(),
            Some(Err(haarness_core::CoreError::Provider(message))) if message == "Upstream overloaded"
        ));
        // Tool arguments that do not parse fail the request after its usage.
        let events = stream_events(concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"read\",\"arguments\":\"{broken\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":90,\"completion_tokens\":7}}\n\n",
            "data: [DONE]\n\n"
        ))
        .await;
        assert_eq!(reported(&events), [(90, 7, 0, 0)]);
        assert!(events.last().is_some_and(Result::is_err));
    }

    #[tokio::test]
    async fn parallel_tool_calls_accumulate_by_index_and_usage_is_reported() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/chat/completions", post(completions))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider =
            OpenAiCompatibleProvider::for_test(format!("http://{address}/chat/completions"));
        let request = ProviderRequest {
            model: "compat-test".into(),
            system: "sys".into(),
            messages: vec![
                ProviderMessage::Text {
                    role: "user".into(),
                    content: "go".into(),
                },
                ProviderMessage::Media {
                    role: "user".into(),
                    content: "inspect this".into(),
                    attachments: vec!["data:image/png;base64,iVBORw0KGgo=".into()],
                },
            ],
            tools: vec![ToolDefinition {
                name: "read".into(),
                description: "Read a file".into(),
                input_schema: json!({"type": "object"}),
            }],
            reasoning_effort: Some("low".into()),
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        };
        let mut stream = provider
            .stream(request, CancellationToken::new())
            .await
            .unwrap();
        let events: Vec<_> = stream
            .by_ref()
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();

        assert!(events.iter().all(|event| !matches!(
            event,
            ProviderEvent::TextDelta(text) | ProviderEvent::ReasoningDelta(text)
                if text.is_empty()
        )));

        let calls: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::ToolCall {
                    call_id,
                    name,
                    arguments,
                    ..
                } => Some((call_id.clone(), name.clone(), arguments.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            calls,
            vec![
                ("call_a".into(), "read".into(), json!({"path": "a.txt"})),
                ("call_b".into(), "list".into(), json!({"path": "."})),
            ]
        );
        // Each call starts with its first fragment, long before the calls
        // are emitted at the end of the stream.
        let started: Vec<_> = events
            .iter()
            .enumerate()
            .filter(|(_, event)| matches!(event, ProviderEvent::ContentStarted))
            .map(|(index, _)| index)
            .collect();
        let first_call = events
            .iter()
            .position(|event| matches!(event, ProviderEvent::ToolCall { .. }));
        assert_eq!(started.len(), 2, "{events:?}");
        assert!(Some(started[1]) < first_call, "{events:?}");
        let reasoning: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::ReasoningDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        // The summary is its own block, so it is broken away from the raw
        // thought before it instead of arriving as "contextconcise summary".
        assert_eq!(
            reasoning,
            [
                "thinking ",
                "preferred",
                "then ",
                "private ",
                "context",
                "\n\n",
                "concise summary"
            ]
        );
        let expected_details = json!({
            "reasoning_model": "compat-test",
            "reasoning_details": [
                {
                    "type": "reasoning.text",
                    "text": "private context",
                    "signature": "opaque-signature",
                    "format": "anthropic-claude-v1",
                    "id": "reasoning-1",
                    "index": 0
                },
                {
                    "type": "reasoning.encrypted",
                    "data": "opaque-data",
                    "id": "reasoning-2",
                    "format": "anthropic-claude-v1",
                    "index": 1
                },
                {
                    "type": "reasoning.summary",
                    "summary": "concise summary",
                    "id": "reasoning-3",
                    "format": "anthropic-claude-v1",
                    "index": 2
                }
            ]
        });
        for event in &events {
            if let ProviderEvent::ToolCall {
                provider_metadata, ..
            } = event
            {
                assert_eq!(provider_metadata, &expected_details);
            }
        }
        assert!(events.iter().any(|event| matches!(
            event,
            ProviderEvent::Usage {
                input_tokens: 42,
                output_tokens: 9,
                cache_read_tokens: 30,
                cache_write_tokens: 0,
            }
        )));
        let requests = state.0.lock();
        assert_eq!(requests[0]["reasoning_effort"], "low");
        assert_eq!(requests[0]["max_tokens"], 8_192);
        assert_eq!(requests[0]["tools"][0]["function"]["name"], "read");
        assert_eq!(requests[0]["stream_options"]["include_usage"], true);
        assert_eq!(
            requests[0]["messages"][2]["content"][1]["type"],
            "image_url"
        );
        assert_eq!(
            requests[0]["messages"][2]["content"][1]["image_url"]["url"],
            "data:image/png;base64,iVBORw0KGgo="
        );
    }

    #[tokio::test]
    async fn structured_output_uses_required_tool_choice_on_compatible_providers() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/chat/completions", post(structured_completions))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider =
            OpenAiCompatibleProvider::for_test(format!("http://{address}/chat/completions"));
        let mut stream = provider
            .stream(
                ProviderRequest {
                    model: "compat-test".into(),
                    system: "test".into(),
                    messages: vec![ProviderMessage::Text {
                        role: "user".into(),
                        content: "count".into(),
                    }],
                    tools: Vec::new(),
                    reasoning_effort: None,
                    parameters: haarness_core::RequestParameters::default(),
                    output_schema: Some(json!({
                        "type":"object",
                        "properties":{"count":{"type":"integer"}},
                        "required":["count"]
                    })),
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let events = stream.by_ref().collect::<Vec<_>>().await;
        assert!(events.into_iter().any(
            |event| matches!(event.unwrap(), ProviderEvent::StructuredOutput(value) if value == json!({"count": 5}))
        ));
        let requests = state.0.lock();
        assert_eq!(requests[0]["tool_choice"], "required");
        let tool = requests[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["function"]["name"] == "StructuredOutput")
            .unwrap();
        assert_eq!(tool["function"]["parameters"]["required"][0], "count");
    }

    #[tokio::test]
    async fn claude_on_a_haarness_router_returns_structured_output_as_native_json() {
        async fn json_reply(
            State(state): State<FixtureState>,
            Json(request): Json<Value>,
        ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
            state.0.lock().push(request);
            let stream = [
                r#"data: {"choices":[{"delta":{"content":"{\"count\":"}}]}"#,
                r#"data: {"choices":[{"delta":{"content":"7}"},"finish_reason":"stop"}]}"#,
            ]
            .join("\n\n")
                + "\n\n";
            ([(CONTENT_TYPE, "text/event-stream")], stream)
        }
        let state = FixtureState::default();
        let router = Router::new()
            .route("/chat/completions", post(json_reply))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider =
            OpenAiCompatibleProvider::for_test(format!("http://{address}/chat/completions"))
                .with_server_output_limit();
        let schema =
            json!({"type":"object","properties":{"count":{"type":"integer"}},"required":["count"]});
        let events = provider
            .stream(
                ProviderRequest {
                    model: "anthropic/claude-sonnet-5.5".into(),
                    system: "test".into(),
                    messages: vec![ProviderMessage::Text {
                        role: "user".into(),
                        content: "count".into(),
                    }],
                    tools: vec![haarness_core::ToolDefinition {
                        name: "read".into(),
                        description: "Read a file".into(),
                        input_schema: json!({"type":"object"}),
                    }],
                    reasoning_effort: None,
                    parameters: haarness_core::RequestParameters::default(),
                    output_schema: Some(schema.clone()),
                },
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        let events: Vec<_> = events.into_iter().map(Result::unwrap).collect();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ProviderEvent::StructuredOutput(value) if *value == json!({"count": 7})))
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ProviderEvent::TextDelta(_)))
        );
        let requests = state.0.lock();
        assert!(requests[0].get("tool_choice").is_none());
        assert_eq!(
            requests[0]["response_format"]["json_schema"]["schema"],
            schema
        );
        // Ordinary tools stay available; the structured-output tool is not offered.
        let names: Vec<_> = requests[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["read"]);
    }

    #[tokio::test]
    async fn parallel_tool_history_is_one_assistant_batch_with_adjacent_results() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/chat/completions", post(completions))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider =
            OpenAiCompatibleProvider::for_test(format!("http://{address}/chat/completions"));
        let shared = json!({"type":"reasoning.encrypted","id":"shared","data":"opaque"});
        let signature = json!({"type":"reasoning.encrypted","id":"b","data":"call-b-signature"});
        let call = |id: &str, details: Value| ProviderMessage::ToolCall {
            call_id: id.into(),
            name: "read".into(),
            arguments: json!({"path":id}),
            provider_metadata: json!({"reasoning_details":details}),
        };
        let result = |id: &str, media: bool| ProviderMessage::ToolResult {
            call_id: id.into(),
            name: "read".into(),
            output: id.into(),
            is_error: false,
            attachments: if media {
                vec!["data:image/png;base64,iVBORw0KGgo=".into()]
            } else {
                Vec::new()
            },
        };
        let mut stream = provider
            .stream(
                ProviderRequest {
                    model: "compat-test".into(),
                    system: "sys".into(),
                    messages: vec![
                        call("a", json!([shared])),
                        call("b", json!([shared, signature])),
                        result("a", true),
                        result("b", false),
                        call("c", json!([shared])),
                        result("c", false),
                    ],
                    tools: Vec::new(),
                    reasoning_effort: None,
                    parameters: haarness_core::RequestParameters::default(),
                    output_schema: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        let requests = state.0.lock();
        let messages = requests[0]["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 7);
        assert_eq!(messages[1]["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(messages[1]["tool_calls"][0]["id"], "a");
        assert_eq!(messages[1]["tool_calls"][1]["id"], "b");
        assert_eq!(messages[1]["reasoning_details"], json!([shared, signature]));
        assert_eq!(messages[2]["tool_call_id"], "a");
        assert_eq!(messages[3]["tool_call_id"], "b");
        assert_eq!(messages[4]["role"], "user");
        assert_eq!(messages[4]["content"][1]["type"], "image_url");
        assert_eq!(messages[5]["tool_calls"].as_array().unwrap().len(), 1);
        assert_eq!(messages[5]["tool_calls"][0]["id"], "c");
        assert_eq!(messages[6]["tool_call_id"], "c");
    }

    #[tokio::test]
    async fn reasoning_details_are_replayed_on_assistant_tool_call_messages() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/chat/completions", post(completions))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider =
            OpenAiCompatibleProvider::for_test(format!("http://{address}/chat/completions"));
        let details = json!([
            {
                "type": "reasoning.text",
                "text": "opaque context",
                "signature": "opaque-signature",
                "format": "anthropic-claude-v1",
                "id": "reasoning-previous",
                "index": 0
            },
            {
                "type": "reasoning.encrypted",
                "data": "opaque-data",
                "format": "anthropic-claude-v1",
                "id": "reasoning-encrypted",
                "index": 1
            }
        ]);
        let request = ProviderRequest {
            model: "compat-test".into(),
            system: "sys".into(),
            messages: vec![
                ProviderMessage::ToolCall {
                    call_id: "previous-call".into(),
                    name: "read".into(),
                    arguments: json!({"path": "Cargo.toml"}),
                    provider_metadata: json!({"reasoning_details": details}),
                },
                ProviderMessage::ToolResult {
                    call_id: "previous-call".into(),
                    name: "read".into(),
                    output: "workspace".into(),
                    is_error: false,
                    attachments: Vec::new(),
                },
            ],
            tools: Vec::new(),
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        };
        let mut stream = provider
            .stream(request, CancellationToken::new())
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }

        let requests = state.0.lock();
        let assistant = &requests[0]["messages"][1];
        assert_eq!(assistant["reasoning_details"], details);
        assert_eq!(assistant["tool_calls"][0]["id"], "previous-call");
        assert_eq!(assistant["tool_calls"][0]["function"]["name"], "read");
    }

    /// A server that streams its reasoning in `field`, then calls `read`.
    async fn reasoning_in_field(
        State((state, field)): State<(FixtureState, &'static str)>,
        Json(request): Json<Value>,
    ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
        use std::fmt::Write as _;
        state.0.lock().push(request);
        let mut stream = String::new();
        for chunk in [
            json!({"choices":[{"delta":{field: "Look at "}}]}),
            json!({"choices":[{"delta":{field: "the file first."}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"read","arguments":"{\"path\":\"a.txt\"}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        ] {
            let _ = write!(stream, "data: {chunk}\n\n");
        }
        ([(CONTENT_TYPE, "text/event-stream")], stream)
    }

    /// Runs one round against a server streaming reasoning in `field` as
    /// provider `id` (a server the user set up when `own`), then sends that
    /// call back as history, with its metadata or `history` when given.
    /// Returns the call's metadata and the assistant message of the second
    /// request.
    async fn reasoning_round_trip(
        id: &str,
        field: &'static str,
        own: bool,
        history: Option<Value>,
    ) -> (Value, Value) {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/chat/completions", post(reasoning_in_field))
            .with_state((state.clone(), field));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = OpenAiCompatibleProvider::new(CompatibleInstance {
            id: id.into(),
            name: "Reasoning fixture".into(),
            base_url: format!("http://{address}"),
            api_key: Some("fixture".into()),
            models: vec!["thinker".into()],
            discover_models: false,
            server_output_limit: false,
            replay_observed_reasoning: own,
            model_defaults: CompatibleModelDefaults {
                reasoning: Some(true),
                ..CompatibleModelDefaults::default()
            },
            assume_authenticated: true,
            headers: crate::ProviderHeaders::default(),
        });
        let request = |messages: Vec<ProviderMessage>| ProviderRequest {
            model: "thinker".into(),
            system: "sys".into(),
            messages,
            tools: Vec::new(),
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        };
        let mut stream = provider
            .stream(
                request(vec![ProviderMessage::Text {
                    role: "user".into(),
                    content: "Read a.txt".into(),
                }]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut metadata = Value::Null;
        while let Some(event) = stream.next().await {
            if let ProviderEvent::ToolCall {
                provider_metadata, ..
            } = event.unwrap()
            {
                metadata = provider_metadata;
            }
        }
        let mut stream = provider
            .stream(
                request(vec![
                    ProviderMessage::Text {
                        role: "user".into(),
                        content: "Read a.txt".into(),
                    },
                    ProviderMessage::ToolCall {
                        call_id: "call_a".into(),
                        name: "read".into(),
                        arguments: json!({"path": "a.txt"}),
                        provider_metadata: history.unwrap_or_else(|| metadata.clone()),
                    },
                    ProviderMessage::ToolResult {
                        call_id: "call_a".into(),
                        name: "read".into(),
                        output: "contents".into(),
                        is_error: false,
                        attachments: Vec::new(),
                    },
                ]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        let assistant = state.0.lock()[1]["messages"][2].clone();
        assert_eq!(assistant["tool_calls"][0]["id"], "call_a");
        (metadata, assistant)
    }

    #[tokio::test]
    async fn a_custom_server_gets_its_reasoning_back_in_the_field_it_used() {
        // vLLM and Ollama stream and take `reasoning`.
        let (metadata, assistant) =
            reasoning_round_trip("vllm-local", "reasoning", true, None).await;
        assert_eq!(
            metadata,
            json!({"reasoning": "Look at the file first.", "reasoning_provider": "vllm-local"})
        );
        assert_eq!(assistant["reasoning"], "Look at the file first.");
        assert!(assistant.get("reasoning_content").is_none(), "{assistant}");
        assert!(assistant.get("reasoning_provider").is_none(), "{assistant}");
        // llama.cpp and SGLang use `reasoning_content`.
        let (metadata, assistant) =
            reasoning_round_trip("llama-local", "reasoning_content", true, None).await;
        assert_eq!(
            metadata,
            json!({"reasoning_content": "Look at the file first.", "reasoning_provider": "llama-local"})
        );
        assert_eq!(assistant["reasoning_content"], "Look at the file first.");
        assert!(assistant.get("reasoning").is_none(), "{assistant}");
    }

    #[tokio::test]
    async fn built_in_services_keep_their_documented_reasoning_fields() {
        // A built-in service gets back nothing its API does not document,
        // whatever its id (Azure and Copilot wrap this adapter under ids of
        // their own).
        for id in ["groq", "azure", "github-copilot"] {
            let (metadata, assistant) = reasoning_round_trip(id, "reasoning", false, None).await;
            assert_eq!(metadata, Value::Null, "{id}");
            assert!(assistant.get("reasoning").is_none(), "{id}: {assistant}");
            assert!(
                assistant.get("reasoning_content").is_none(),
                "{id}: {assistant}"
            );
        }
        // DeepSeek still gets `reasoning_content` on every assistant message.
        let (metadata, assistant) =
            reasoning_round_trip("deepseek", "reasoning_content", false, None).await;
        assert_eq!(
            metadata,
            json!({"reasoning_content": "Look at the file first.", "reasoning_provider": "deepseek"})
        );
        assert_eq!(assistant["reasoning_content"], "Look at the file first.");
        // A call DeepSeek stored before reasoning was tagged still goes back.
        let legacy = json!({"reasoning_content": "Earlier thought."});
        let (_, assistant) =
            reasoning_round_trip("deepseek", "reasoning_content", false, Some(legacy)).await;
        assert_eq!(assistant["reasoning_content"], "Earlier thought.");
    }

    #[tokio::test]
    async fn reasoning_goes_back_only_to_the_provider_that_produced_it() {
        // A chat that moved from DeepSeek to a server of the user's own does
        // not hand DeepSeek's reasoning to it, tagged or from before tags.
        for history in [
            json!({"reasoning_content": "DeepSeek's thought.", "reasoning_provider": "deepseek"}),
            json!({"reasoning_content": "DeepSeek's thought."}),
        ] {
            let (_, assistant) =
                reasoning_round_trip("vllm-local", "reasoning", true, Some(history.clone())).await;
            assert!(
                assistant.get("reasoning").is_none(),
                "{history}: {assistant}"
            );
            assert!(
                assistant.get("reasoning_content").is_none(),
                "{history}: {assistant}"
            );
        }
        // Nor one custom server's reasoning to another.
        let other =
            json!({"reasoning": "Another server's thought.", "reasoning_provider": "llama-local"});
        let (_, assistant) =
            reasoning_round_trip("vllm-local", "reasoning", true, Some(other)).await;
        assert!(assistant.get("reasoning").is_none(), "{assistant}");
        // DeepSeek still gets the field it requires, empty, for another
        // provider's call.
        let custom = json!({"reasoning": "A custom thought.", "reasoning_provider": "vllm-local"});
        let (_, assistant) =
            reasoning_round_trip("deepseek", "reasoning_content", false, Some(custom)).await;
        assert_eq!(assistant["reasoning_content"], "");
        assert!(assistant.get("reasoning").is_none(), "{assistant}");
    }

    #[tokio::test]
    async fn discovery_preserves_authoritative_metadata_and_config_overrides() {
        let future = discovered_model_info(
            "custom",
            &json!({"id":"future-model","reasoning_efforts":["low","xhigh"]}),
            CompatibleModelDefaults::default(),
        )
        .unwrap();
        assert!(future.reasoning);
        assert_eq!(future.reasoning_efforts, ["low", "xhigh"]);
        let configured = discovered_model_info(
            "custom",
            &json!({"id":"future-model","reasoning_efforts":["low"]}),
            CompatibleModelDefaults {
                reasoning: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!configured.reasoning);
        let router = Router::new().route("/models", get(models));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let provider = OpenAiCompatibleProvider::new(CompatibleInstance {
            id: "openrouter".into(),
            name: "OpenRouter".into(),
            base_url: format!("http://{address}"),
            api_key: Some("fixture-secret".into()),
            models: vec!["openrouter/auto".into()],
            discover_models: true,
            server_output_limit: false,
            replay_observed_reasoning: false,
            model_defaults: CompatibleModelDefaults {
                // Explicit config wins even when discovery advertises tools.
                tools: Some(false),
                ..CompatibleModelDefaults::default()
            },
            assume_authenticated: false,
            headers: crate::ProviderHeaders::default(),
        })
        .discover_models()
        .await;
        assert!(
            !provider
                .info()
                .models
                .iter()
                .find(|model| model.id == "vendor/vision-reasoner")
                .unwrap()
                .reasoning_efforts
                .is_empty()
        );
        let info = provider.info();

        assert!(info.authenticated);
        assert_eq!(info.models.len(), 2);
        let vision = info
            .models
            .iter()
            .find(|model| model.id == "vendor/vision-reasoner")
            .unwrap();
        assert_eq!(vision.display_name, "Vision Reasoner");
        assert!(vision.images);
        assert!(vision.reasoning);
        assert!(!vision.video);
        assert!(!vision.tools, "explicit provider override wins");
        assert_eq!(vision.context_window, Some(131_072));
        assert_eq!(vision.max_output_tokens, Some(8_192));
        assert!((vision.pricing.unwrap().input - 0.25).abs() < f64::EPSILON);
        assert!((vision.pricing.unwrap().output - 1.0).abs() < f64::EPSILON);

        let text = info
            .models
            .iter()
            .find(|model| model.id == "vendor/text-only")
            .unwrap();
        assert!(!text.images);
        assert!(!text.reasoning);
        assert!(!text.tools);
    }

    #[tokio::test]
    async fn reasoning_capability_does_not_imply_generic_effort_request_shape() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/chat/completions", post(completions))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = OpenAiCompatibleProvider::new(CompatibleInstance {
            id: "zai".into(),
            name: "Z.AI".into(),
            base_url: format!("http://{address}"),
            api_key: Some("fixture-secret".into()),
            models: vec!["glm-5.1".into()],
            discover_models: false,
            server_output_limit: false,
            replay_observed_reasoning: false,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: false,
            headers: crate::ProviderHeaders::default(),
        });
        let mut stream = provider
            .stream(
                ProviderRequest {
                    model: "glm-5.1".into(),
                    system: "sys".into(),
                    messages: vec![ProviderMessage::Text {
                        role: "user".into(),
                        content: "think".into(),
                    }],
                    tools: Vec::new(),
                    reasoning_effort: Some("high".into()),
                    parameters: haarness_core::RequestParameters::default(),
                    output_schema: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }

        let requests = state.0.lock();
        assert!(requests[0].get("reasoning_effort").is_none());
    }

    #[tokio::test]
    async fn unsupported_capabilities_are_omitted_or_rejected_before_the_request() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/chat/completions", post(completions))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = OpenAiCompatibleProvider::new(CompatibleInstance {
            id: "perplexity".into(),
            name: "Perplexity Sonar".into(),
            base_url: format!("http://{address}"),
            api_key: Some("fixture-secret".into()),
            models: vec!["sonar-reasoning-pro".into()],
            discover_models: false,
            server_output_limit: false,
            replay_observed_reasoning: false,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: false,
            headers: crate::ProviderHeaders::default(),
        });
        let tools = vec![ToolDefinition {
            name: "read".into(),
            description: "Read a file".into(),
            input_schema: json!({"type": "object"}),
        }];

        let mut stream = provider
            .stream(
                ProviderRequest {
                    model: "sonar-reasoning-pro".into(),
                    system: "sys".into(),
                    messages: vec![ProviderMessage::Text {
                        role: "user".into(),
                        content: "answer without tools".into(),
                    }],
                    tools: tools.clone(),
                    reasoning_effort: Some("high".into()),
                    parameters: haarness_core::RequestParameters::default(),
                    output_schema: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        {
            let requests = state.0.lock();
            assert!(requests[0].get("tools").is_none());
            assert!(requests[0].get("reasoning_effort").is_none());
            assert!(requests[0].get("stream_options").is_none());
            assert_eq!(requests[0]["max_tokens"], 128_000);
        }

        let result = provider
            .stream(
                ProviderRequest {
                    model: "sonar-reasoning-pro".into(),
                    system: "sys".into(),
                    messages: vec![ProviderMessage::Media {
                        role: "user".into(),
                        content: "inspect".into(),
                        attachments: vec!["data:image/png;base64,iVBORw0KGgo=".into()],
                    }],
                    tools,
                    reasoning_effort: None,
                    parameters: haarness_core::RequestParameters::default(),
                    output_schema: None,
                },
                CancellationToken::new(),
            )
            .await;
        let error = match result {
            Ok(_) => panic!("unsupported image request must fail before HTTP"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("does not advertise image input"));
        assert_eq!(
            state.0.lock().len(),
            1,
            "rejected request never reached HTTP"
        );
    }
}
