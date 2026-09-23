use crate::{
    auth::{Credential, CredentialStore, XaiOAuthClient, unix_millis, xai_access_token_expiring},
    sse::json_stream,
};
use async_stream::try_stream;
use async_trait::async_trait;
use futures::StreamExt;
use haarness_core::{
    CoreError, CoreResult, ModelProvider, ProviderEvent, ProviderMessage, ProviderRequest,
    ProviderStream, STRUCTURED_OUTPUT_TOOL_NAME, structured_output_tool,
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
pub struct OpenAiCompatibleProvider {
    client: reqwest::Client,
    id: String,
    name: String,
    endpoint: String,
    api_key: Option<SecretString>,
    auth_store: Option<Arc<CredentialStore>>,
    auth_aliases: Vec<String>,
    xai_oauth: Option<XaiOAuthClient>,
    refresh_lock: Mutex<()>,
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
}

/// Declarative description of a compatible endpoint.
#[derive(Clone)]
pub struct CompatibleInstance {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub models: Vec<String>,
    /// Query the compatible `/models` route and replace fallback models when
    /// it succeeds. Explicit user model allowlists set this to false.
    pub discover_models: bool,
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

impl OpenAiCompatibleProvider {
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
            refresh_lock: Mutex::new(()),
            live_models: crate::live_models::LiveModels::default(),
            models: instance
                .models
                .into_iter()
                .map(|id| model_info(&provider_id, &id, model_defaults))
                .collect(),
            reasoning_effort_models: HashSet::new(),
            model_defaults,
            discover_models: instance.discover_models,
            server_output_limit: false,
            assume_authenticated: instance.assume_authenticated,
            headers: instance.headers,
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
    async fn fetch_models(&self) -> Option<Vec<ModelInfo>> {
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
                .filter_map(|entry| {
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
            return Some(crate::merge_discovered_models(self.models.clone(), models));
        }
        None
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
            refresh_lock: Mutex::new(()),
            live_models: crate::live_models::LiveModels::default(),
            models: vec![model_info("openai-compatible", "compat-test", defaults)],
            reasoning_effort_models: HashSet::from(["compat-test".into()]),
            model_defaults: defaults,
            discover_models: false,
            server_output_limit: false,
            assume_authenticated: false,
            headers: crate::ProviderHeaders::default(),
        }
    }

    async fn bearer_token(&self) -> CoreResult<Option<String>> {
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
        let expires_soon = current.oauth_parts().is_some_and(|oauth| {
            oauth.expires == 0
                || oauth.expires <= refresh_at
                || xai_access_token_expiring(oauth.access, Duration::from_secs(2 * 60))
        });
        if expires_soon {
            let _guard = self.refresh_lock.lock().await;
            stored = self.stored_credential()?;
            credential = stored.as_ref().map(|(_, credential)| credential.clone());
            let current = credential
                .as_ref()
                .ok_or_else(|| CoreError::Provider(format!("{} was logged out", self.name)))?;
            if let Some(oauth) = current.oauth_parts()
                && (oauth.expires == 0
                    || oauth.expires <= refresh_at
                    || xai_access_token_expiring(oauth.access, Duration::from_secs(2 * 60)))
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
                let next = client.refresh(oauth).await.map_err(provider_error)?;
                if store
                    .replace_oauth_if_current(credential_id, &expected_refresh, &next)
                    .map_err(provider_error)?
                {
                    credential = Some(next);
                } else {
                    credential = self.stored_credential()?.map(|(_, credential)| credential);
                }
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
            }),
            "sonar-pro" => Some(ModelPricing {
                input: 3.0,
                output: 15.0,
                cache_read: 0.0,
                cache_write: 0.0,
            }),
            "sonar-reasoning-pro" | "sonar-deep-research" => Some(ModelPricing {
                input: 2.0,
                output: 8.0,
                cache_read: 0.0,
                cache_write: 0.0,
            }),
            _ => None,
        };
    }
    apply_known_compatible_metadata(provider, &mut info);
    apply_model_defaults(crate::catalog::annotate(provider, info), defaults)
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
            });
        }
        ("xai", "grok-4.3") => {
            info.display_name = "Grok 4.3".into();
            info.reasoning = true;
            info.context_window = Some(1_000_000);
            info.pricing = Some(ModelPricing {
                input: 1.25,
                output: 2.50,
                cache_read: 0.0,
                cache_write: 0.0,
            });
        }
        ("xai", "grok-build-0.1") => {
            info.display_name = "Grok Build 0.1".into();
            info.context_window = Some(256_000);
            info.pricing = Some(ModelPricing {
                input: 1.0,
                output: 2.0,
                cache_read: 0.0,
                cache_write: 0.0,
            });
        }
        (
            "deepseek",
            "deepseek-flash"
            | "deepseek-v4-flash"
            | "deepseek-v4-flash-vision-exp"
            | "deepseek-v4-pro",
        ) => {
            info.display_name = if info.id != "deepseek-v4-pro" {
                "DeepSeek V4.1 Flash".into()
            } else {
                "DeepSeek V4 Pro".into()
            };
            info.reasoning = true;
            info.context_window = Some(1_000_000);
            info.max_output_tokens = Some(384_000);
            info.images = info.id != "deepseek-v4-pro";
            // Peak/off-peak billing cannot be represented by one static price.
            info.pricing = None;
        }
        ("together" | "togetherai", "zai-org/GLM-5.1") => {
            info.display_name = "GLM-5.1".into();
            info.context_window = Some(202_752);
            info.pricing = Some(ModelPricing {
                input: 1.40,
                output: 4.40,
                cache_read: 0.0,
                cache_write: 0.0,
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
        name.clone_into(&mut info.display_name);
    }
    info.context_window = first_u64(
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
            .and_then(|metadata| first_u64(metadata, &["context_length"]))
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
    if let Some(efforts) = entry["reasoning_efforts"].as_array() {
        info.reasoning_efforts = efforts
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
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

    // OpenRouter publishes per-token prices as strings. Convert only this
    // documented shape; other compatible APIs use different units.
    if provider == "openrouter"
        && let (Some(input), Some(output)) = (
            entry.pointer("/pricing/prompt").and_then(json_number),
            entry.pointer("/pricing/completion").and_then(json_number),
        )
    {
        info.pricing = Some(ModelPricing {
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
        });
    }
    Some(apply_model_defaults(info, defaults))
}

fn is_chat_completion_model(provider: &str, entry: &Value, id: &str) -> bool {
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
        ("groq", "openai/gpt-oss-20b" | "openai/gpt-oss-120b") => vec!["low", "medium", "high"],
        ("xai", "grok-4.6" | "grok-4.7") => vec!["low", "medium", "high", "xhigh"],
        ("xai", "grok-4.5") => vec!["low", "medium", "high"],
        (
            "deepseek",
            "deepseek-flash"
            | "deepseek-v4-flash"
            | "deepseek-v4-pro"
            | "deepseek-v4-flash-vision-exp",
        ) => vec!["none", "low", "high", "max"],
        (
            "mistral",
            "mistral-medium-3-5"
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
        if let Some(incoming) = message["reasoning_content"].as_str() {
            let existing = previous["reasoning_content"].as_str().unwrap_or_default();
            if existing != incoming {
                previous["reasoning_content"] = json!(format!("{existing}{incoming}"));
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
        let replay_reasoning_content =
            matches!(self.id.as_str(), "deepseek" | "moonshotai" | "zai");
        let mut messages = vec![json!({"role":"system","content":request.system})];
        let mut tool_media = Vec::new();
        for message in request.messages {
            if !matches!(&message, ProviderMessage::ToolResult { .. }) {
                messages.append(&mut tool_media);
            }
            match message {
                ProviderMessage::Text { role, content } => {
                    let mut message = json!({"role":role,"content":content});
                    if replay_reasoning_content && role == "assistant" {
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
                        .filter(|details| !details.is_empty())
                    {
                        // OpenRouter requires these opaque blocks to be
                        // replayed byte-for-byte and in their original order.
                        message["reasoning_details"] = Value::Array(details.clone());
                    }
                    if replay_reasoning_content {
                        message["reasoning_content"] = json!(
                            provider_metadata["reasoning_content"]
                                .as_str()
                                .unwrap_or_default()
                        );
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
        let mut requested_tools = request.tools;
        if let Some(schema) = structured_output.as_ref() {
            requested_tools.retain(|tool| tool.name != STRUCTURED_OUTPUT_TOOL_NAME);
            requested_tools.push(structured_output_tool(schema));
        }
        let tools:Vec<_>=requested_tools.into_iter().map(|t|json!({"type":"function","function":{"name":t.name,"description":t.description,"parameters":t.input_schema}})).collect();
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
        if structured_output.is_some() {
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
            .headers(headers)
            .json(&body);
        let mut events = json_stream(builder.send().await.map_err(provider_error)?, cancel);
        Ok(Box::pin(try_stream! {
            // Parallel tool calls stream interleaved deltas keyed by index.
            let mut calls: Vec<(String, String, String)> = Vec::new();
            let mut finish_reason: Option<String> = None;
            let mut usage: Option<(u64, u64, u64)> = None;
            let mut reasoning_details: Vec<Value> = Vec::new();
            let mut reasoning_content = String::new();
            let mut mistral_content: Vec<Value> = Vec::new();
            // The last detail whose text reached the transcript, so a new
            // block can be separated from the one before it.
            let mut emitted_reasoning: Option<Value> = None;
            while let Some(event)=events.next().await{let event=event?;
                if let Some(chunk_usage) = event.get("usage").filter(|value| !value.is_null()) {
                    usage = Some((
                        chunk_usage["prompt_tokens"].as_u64().unwrap_or(0),
                        chunk_usage["completion_tokens"].as_u64().unwrap_or(0),
                        chunk_usage.pointer("/prompt_tokens_details/cached_tokens").and_then(Value::as_u64).unwrap_or(0),
                    ));
                }
                let delta=&event["choices"][0]["delta"];
                if let Some(text)=delta["content"].as_str().filter(|text| !text.is_empty()){yield ProviderEvent::TextDelta(text.into());}
                if let Some(parts) = delta["content"].as_array() {
                    for part in parts {
                        mistral_content.push(part.clone());
                        if part["type"] == "text" && let Some(text) = part["text"].as_str() { yield ProviderEvent::TextDelta(text.into()); }
                        if part["type"] == "thinking" && let Some(thoughts) = part["thinking"].as_array() {
                            for thought in thoughts { if let Some(text) = thought["text"].as_str() { yield ProviderEvent::ReasoningDelta(text.into()); } }
                        }
                    }
                }
                let direct_reasoning = delta
                    .get("reasoning_content")
                    .and_then(Value::as_str)
                    .or_else(|| delta.get("reasoning").and_then(Value::as_str))
                    .filter(|reasoning| !reasoning.is_empty());
                if let Some(reasoning) = direct_reasoning {
                    reasoning_content.push_str(reasoning);
                    yield ProviderEvent::ReasoningDelta(reasoning.into());
                }
                if let Some(details) = delta.get("reasoning_details") {
                    append_reasoning_details(&mut reasoning_details, details);
                    if direct_reasoning.is_none()
                        && let Some(details) = details.as_array()
                    {
                        for detail in details {
                            let visible = match detail.get("type").and_then(Value::as_str) {
                                Some("reasoning.text") => detail.get("text"),
                                Some("reasoning.summary") => detail.get("summary"),
                                _ => None,
                            }
                            .and_then(Value::as_str)
                            .filter(|text| !text.is_empty());
                            if let Some(text) = visible {
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
                        if calls.len() <= index { calls.resize(index + 1, (String::new(), String::new(), String::new())); }
                        if let Some(id)=tool["id"].as_str(){calls[index].0 = id.into();}
                        if let Some(name)=tool["function"]["name"].as_str(){calls[index].1 = name.into();}
                        if let Some(args)=tool["function"]["arguments"].as_str(){calls[index].2.push_str(args);}
                    }
                }
                if let Some(reason)=event["choices"][0]["finish_reason"].as_str(){finish_reason = Some(reason.to_owned());}
            }
            let mut provider_metadata = json!({});
            if !reasoning_details.is_empty() { provider_metadata["reasoning_details"] = json!(reasoning_details); }
            if replay_reasoning_content { provider_metadata["reasoning_content"] = json!(reasoning_content); }
            if !mistral_content.is_empty() { provider_metadata["mistral_content"] = json!(mistral_content); }
            if provider_metadata.as_object().is_some_and(|metadata| metadata.is_empty()) { provider_metadata = Value::Null; }
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
            if let Some((input_tokens, output_tokens, cache_read_tokens)) = usage {
                yield ProviderEvent::Usage { input_tokens, output_tokens, cache_read_tokens, cache_write_tokens: 0 };
            }
            yield ProviderEvent::Finished{reason:finish_reason.unwrap_or_else(||"completed".into())};
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
    let mut blocks = vec![json!({"type":"text","text":content})];
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
        append_reasoning_details, discovered_model_info, model_info,
        openai_compatible_media_message,
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

    #[tokio::test]
    async fn managed_output_allowance_is_chosen_by_server_unless_explicit() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/chat/completions", post(completions_recording_headers))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider =
            OpenAiCompatibleProvider::for_test(format!("http://{address}/chat/completions"))
                .with_server_output_limit();
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
