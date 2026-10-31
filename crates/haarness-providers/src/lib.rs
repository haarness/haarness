//! Native provider adapters. Provider wire payloads never escape this crate.

mod anthropic;
pub mod auth;
mod aws_event_stream;
mod bedrock;
pub mod catalog;
mod cloud;
#[cfg(test)]
mod compatibility_tests;
pub mod connect;
mod gemini;
mod haarness_cloud;
mod key_check;
mod live_models;
mod media;
mod model_order;
mod openai;
mod openai_compatible;
mod sse;
mod test_echo;
mod tool_schema;

pub use anthropic::AnthropicProvider;
pub use auth::{
    CopilotOAuthClient, Credential, CredentialStore, OpenAiOAuthClient, XaiOAuthClient,
};
pub use bedrock::BedrockProvider;
pub use cloud::{AzureProvider, CopilotProvider, VertexProvider};
pub use gemini::GeminiProvider;
pub use openai::OpenAiProvider;
pub use openai_compatible::{
    CompatibleInstance, CompatibleModelDefaults, OpenAiCompatibleProvider,
};

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use haarness_core::{ModelProvider, ProviderRegistry, StreamLimits};
use haarness_protocol::ProviderInfo;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

/// Validated request headers shared by every configurable provider transport.
/// The wrapper deliberately has no `Debug` implementation so credentials in
/// headers are not accidentally emitted by diagnostics.
#[derive(Clone, Default)]
pub struct ProviderHeaders(HeaderMap);

impl ProviderHeaders {
    pub fn parse(headers: BTreeMap<String, String>) -> Result<Self, String> {
        let mut parsed = HeaderMap::new();
        for (name, value) in headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|error| format!("invalid header name {name}: {error}"))?;
            let value = HeaderValue::from_str(&value)
                .map_err(|error| format!("invalid value for header {name}: {error}"))?;
            parsed.insert(name, value);
        }
        Ok(Self(parsed))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn map(&self) -> HeaderMap {
        self.0.clone()
    }
}

/// Who can read a provider's prompt cache, as a digest of its endpoint,
/// credential and configured headers (which can carry a credential too), so
/// requests for different accounts never group and no secret leaves the
/// provider.
pub(crate) fn prompt_cache_scope(
    endpoint: &str,
    credential: Option<&str>,
    headers: &ProviderHeaders,
) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest as _, Sha256};
    let mut digest = Sha256::new();
    let mut part = |bytes: &[u8]| {
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    };
    part(endpoint.as_bytes());
    part(credential.unwrap_or_default().as_bytes());
    for (name, value) in &headers.0 {
        part(name.as_str().as_bytes());
        part(value.as_bytes());
    }
    URL_SAFE_NO_PAD.encode(digest.finalize())
}

/// Merge hook-supplied request headers (`chat.headers`) into a transport
/// header map. Invalid names or values are skipped, not fatal.
pub(crate) fn apply_request_headers(headers: &mut HeaderMap, extra: &BTreeMap<String, String>) {
    for (name, value) in extra {
        match (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            (Ok(name), Ok(value)) => {
                headers.insert(name, value);
            }
            _ => tracing::warn!(
                header = name,
                "ignoring invalid hook-supplied request header"
            ),
        }
    }
}

/// Keep only short identifier-like values, so a log line never carries free
/// text from the wire.
fn identifier(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')))
    .then(|| value.to_owned())
}

/// The provider's own id for one request, from its response headers.
pub(crate) fn request_id(headers: &HeaderMap) -> Option<String> {
    [
        "request-id",
        "x-request-id",
        "anthropic-request-id",
        "x-amzn-requestid",
    ]
    .iter()
    .find_map(|name| headers.get(*name)?.to_str().ok().and_then(identifier))
}

/// Say in the daemon log which response a provider's safety check stopped,
/// with the ids the provider can look it up by. Other finishes log nothing.
pub(crate) fn log_blocked_finish(
    provider: &str,
    reason: &str,
    request: Option<&str>,
    response: Option<&str>,
) {
    if matches!(
        haarness_core::ProviderEvent::finish_error(reason),
        Some(haarness_core::CoreError::ProviderBlocked(_))
    ) {
        tracing::warn!(
            provider,
            stop_reason = identifier(reason).as_deref().unwrap_or("other"),
            request_id = request.and_then(identifier).as_deref().unwrap_or("unknown"),
            response_id = response
                .and_then(identifier)
                .as_deref()
                .unwrap_or("unknown"),
            "provider safety check stopped the response"
        );
    }
}

/// Configuration for a custom provider that uses one of Haarness's native
/// Anthropic Messages or Gemini GenerateContent transports.
#[derive(Clone)]
pub struct NativeInstance {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub models: Vec<String>,
    pub discover_models: bool,
    pub model_defaults: CompatibleModelDefaults,
    pub assume_authenticated: bool,
    pub headers: ProviderHeaders,
}

/// Configurable wire transports. These are protocol names, not vendor
/// identities: a self-hosted gateway can expose any supported wire format.
#[derive(Clone)]
pub enum ProviderInstance {
    OpenAiChatCompletions(CompatibleInstance),
    AnthropicMessages(NativeInstance),
    GeminiGenerateContent(NativeInstance),
    AzureOpenAi(NativeInstance),
    GoogleVertex(NativeInstance),
    GithubCopilot(NativeInstance),
    AmazonBedrock(NativeInstance),
}

impl ProviderInstance {
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::OpenAiChatCompletions(instance) => &instance.id,
            Self::AnthropicMessages(instance) | Self::GeminiGenerateContent(instance) => {
                &instance.id
            }
            Self::AzureOpenAi(instance)
            | Self::GoogleVertex(instance)
            | Self::GithubCopilot(instance)
            | Self::AmazonBedrock(instance) => &instance.id,
        }
    }
}

#[derive(Default)]
pub struct NativeProviderRegistry {
    providers: HashMap<String, Arc<dyn ModelProvider>>,
}

impl NativeProviderRegistry {
    /// Discovery backed by the live owner-only credential store, so providers
    /// observe sign-ins and sign-outs without a server restart and persist
    /// refreshed subscription tokens safely. Covers every supported custom wire
    /// protocol; remote model discovery runs concurrently, so several slow or
    /// unreachable endpoints cost one timeout, not N.
    pub async fn discover_with_providers_and_store(
        instances: Vec<ProviderInstance>,
        disabled: &std::collections::HashSet<String>,
        credential_store: Arc<CredentialStore>,
    ) -> Self {
        let registry = Self::with_providers_and_store(instances, disabled, credential_store).await;
        futures::future::join_all(
            registry
                .providers
                .values()
                .map(|provider| provider.refresh_models(true)),
        )
        .await;
        registry
    }

    /// [`Self::discover_with_providers_and_store`] without the live model
    /// lists: a server fetches them once it can answer (see
    /// `haarness_server::models`), so a provider that is slow to say what it
    /// offers never holds its start.
    pub async fn with_providers_and_store(
        instances: Vec<ProviderInstance>,
        disabled: &std::collections::HashSet<String>,
        credential_store: Arc<CredentialStore>,
    ) -> Self {
        let mut registry = Self::default();
        let auth_store = Some(credential_store.clone());
        let openai = OpenAiProvider::from_environment().with_auth_store(auth_store.clone());
        let anthropic = AnthropicProvider::from_environment().with_auth_store(auth_store.clone());
        let gemini = GeminiProvider::from_environment().with_auth_store(auth_store.clone());
        let copilot = CopilotProvider::from_environment(auth_store.clone());
        let locals = detect_local_runtimes(disabled).await;
        if !disabled.contains("openai") {
            registry.insert(Arc::new(openai));
        }
        if !disabled.contains("haarness") {
            if let Some(provider) = haarness_cloud::HaarnessCloudProvider::from_environment() {
                registry.insert(Arc::new(provider));
            }
        }
        if !disabled.contains("anthropic") {
            registry.insert(Arc::new(anthropic));
        }
        if !disabled.contains("gemini") {
            registry.insert(Arc::new(gemini));
        }
        if !disabled.contains("azure") {
            registry.insert(Arc::new(AzureProvider::from_environment(
                auth_store.clone(),
            )));
        }
        if !disabled.contains("google-vertex") {
            registry.insert(Arc::new(VertexProvider::from_environment(
                auth_store.clone(),
            )));
        }
        if !disabled.contains("github-copilot") {
            registry.insert(Arc::new(copilot));
        }
        if !disabled.contains("amazon-bedrock") {
            registry.insert(Arc::new(BedrockProvider::from_environment(
                auth_store.clone(),
            )));
        }
        if let Some(provider) = OpenAiCompatibleProvider::from_environment() {
            if !disabled.contains(provider.id()) {
                registry.insert(Arc::new(provider));
            }
        }
        let mut declared: std::collections::HashSet<String> = instances
            .iter()
            .map(|instance| instance.id().to_owned())
            .collect();
        let mut configured: Vec<_> = instances
            .into_iter()
            .filter(|instance| !disabled.contains(instance.id()))
            .map(|instance| {
                let aliases = match &instance {
                    ProviderInstance::OpenAiChatCompletions(instance) => {
                        compatible_preset(&instance.id)
                            .into_iter()
                            .flat_map(|preset| {
                                std::iter::once(preset.id).chain(preset.aliases.iter().copied())
                            })
                            .filter(|alias| *alias != instance.id)
                            .map(str::to_owned)
                            .collect()
                    }
                    ProviderInstance::AnthropicMessages(_)
                    | ProviderInstance::GeminiGenerateContent(_)
                    | ProviderInstance::AzureOpenAi(_)
                    | ProviderInstance::GoogleVertex(_)
                    | ProviderInstance::GithubCopilot(_)
                    | ProviderInstance::AmazonBedrock(_) => Vec::new(),
                };
                (instance, aliases)
            })
            .collect();
        for preset in COMPAT_PRESETS {
            if preset_id_in(disabled, preset) || preset_id_in(&declared, preset) {
                continue;
            }
            let api_key = preset_environment_key(preset);
            declared.insert(preset.id.to_owned());
            let authenticated = api_key.is_some()
                || std::iter::once(preset.id)
                    .chain(preset.aliases.iter().copied())
                    .find_map(|id| credential_store.get(id).ok().flatten())
                    .is_some_and(|credential| {
                        credential.api_key().is_some()
                            || (preset.id == "xai" && credential.oauth_parts().is_some())
                    });
            configured.push((
                ProviderInstance::OpenAiChatCompletions(CompatibleInstance {
                    id: preset.id.to_owned(),
                    name: preset.name.to_owned(),
                    base_url: preset.base_url.to_owned(),
                    api_key,
                    models: preset
                        .models
                        .iter()
                        .map(|model| (*model).to_owned())
                        .collect(),
                    discover_models: preset.discover_models && authenticated,
                    server_output_limit: false,
                    replay_observed_reasoning: false,
                    model_defaults: CompatibleModelDefaults::default(),
                    assume_authenticated: false,
                    headers: ProviderHeaders::default(),
                }),
                preset
                    .aliases
                    .iter()
                    .map(|alias| (*alias).to_owned())
                    .collect(),
            ));
        }
        for (instance, aliases) in configured {
            let auth_store = auth_store.clone();
            registry.insert(match instance {
                ProviderInstance::OpenAiChatCompletions(instance) => Arc::new(
                    OpenAiCompatibleProvider::new(instance)
                        .with_auth_store(auth_store)
                        .with_auth_aliases(aliases),
                ),
                ProviderInstance::AnthropicMessages(instance) => {
                    Arc::new(AnthropicProvider::new(instance).with_auth_store(auth_store))
                }
                ProviderInstance::GeminiGenerateContent(instance) => {
                    Arc::new(GeminiProvider::new(instance).with_auth_store(auth_store))
                }
                ProviderInstance::AzureOpenAi(instance) => {
                    Arc::new(AzureProvider::new(instance, auth_store))
                }
                ProviderInstance::GoogleVertex(instance) => {
                    Arc::new(VertexProvider::new(instance, auth_store))
                }
                ProviderInstance::GithubCopilot(instance) => {
                    Arc::new(CopilotProvider::new(instance, auth_store))
                }
                ProviderInstance::AmazonBedrock(instance) => {
                    Arc::new(BedrockProvider::new(instance, auth_store))
                }
            });
        }
        for instance in locals {
            if !declared.contains(&instance.id) {
                registry.insert(Arc::new(OpenAiCompatibleProvider::new(instance)));
            }
        }
        if std::env::var_os("HAARNESS_TEST_PROVIDER").is_some() && !disabled.contains("echo") {
            registry.insert(Arc::new(test_echo::EchoProvider));
        }
        registry
    }

    pub fn insert(&mut self, provider: Arc<dyn ModelProvider>) {
        self.providers.insert(provider.id().to_owned(), provider);
    }

    /// Apply the configured `stream_idle_timeout` and `request_timeout` of
    /// each provider id; the others keep the defaults.
    #[must_use]
    pub fn with_stream_limits(mut self, limits: &HashMap<String, StreamLimits>) -> Self {
        for (id, limits) in limits {
            if let Some(inner) = self.providers.remove(id) {
                let provider = Arc::new(Limited {
                    inner,
                    limits: *limits,
                });
                self.providers.insert(id.clone(), provider);
            }
        }
        self
    }
}

/// A provider whose requests carry its configured stream limits.
struct Limited {
    inner: Arc<dyn ModelProvider>,
    limits: StreamLimits,
}

#[async_trait::async_trait]
impl ModelProvider for Limited {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn info(&self) -> ProviderInfo {
        self.inner.info()
    }
    fn prepare_side_question(&self, request: &mut haarness_core::ProviderRequest) {
        self.inner.prepare_side_question(request);
    }
    async fn refresh_models(&self, force: bool) {
        self.inner.refresh_models(force).await;
    }
    async fn check_api_key(&self, key: &str) -> haarness_core::KeyCheck {
        self.inner.check_api_key(key).await
    }
    fn explicit_prompt_cache_scope(&self, model: &str) -> Option<String> {
        self.inner.explicit_prompt_cache_scope(model)
    }
    fn image_limits(&self, model: &str) -> haarness_core::ImageLimits {
        self.inner.image_limits(model)
    }
    async fn stream(
        &self,
        mut request: haarness_core::ProviderRequest,
        cancel: tokio_util::sync::CancellationToken,
    ) -> haarness_core::CoreResult<haarness_core::ProviderStream> {
        request.parameters.limits = self.limits;
        self.inner.stream(request, cancel).await
    }
}

/// Wire protocol used by compatible presets and custom endpoint config.
/// Haarness deliberately does not label SDK-specific providers compatible
/// unless their documented wire format is one we implement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompatibleProtocol {
    OpenAiChatCompletions,
    AnthropicMessages,
    GeminiGenerateContent,
    AzureOpenAi,
    GoogleVertex,
    GithubCopilot,
    AmazonBedrock,
}

impl CompatibleProtocol {
    /// Accept the explicit name plus legacy spellings used by earlier config.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "openai-chat-completions" | "openai-compatible" | "chat-completions" => {
                Ok(Self::OpenAiChatCompletions)
            }
            "anthropic-messages" | "anthropic" => Ok(Self::AnthropicMessages),
            "gemini-generate-content" | "google-generative-ai" | "gemini" => {
                Ok(Self::GeminiGenerateContent)
            }
            "azure-openai" | "azure" => Ok(Self::AzureOpenAi),
            "google-vertex" | "vertex" => Ok(Self::GoogleVertex),
            "github-copilot" | "copilot" => Ok(Self::GithubCopilot),
            "amazon-bedrock" | "bedrock" => Ok(Self::AmazonBedrock),
            other => Err(format!(
                "unsupported protocol {other}; expected openai-chat-completions, anthropic-messages, gemini-generate-content, azure-openai, google-vertex, github-copilot, or amazon-bedrock"
            )),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiChatCompletions => "openai-chat-completions",
            Self::AnthropicMessages => "anthropic-messages",
            Self::GeminiGenerateContent => "gemini-generate-content",
            Self::AzureOpenAi => "azure-openai",
            Self::GoogleVertex => "google-vertex",
            Self::GithubCopilot => "github-copilot",
            Self::AmazonBedrock => "amazon-bedrock",
        }
    }
}

/// A first-class OpenAI Chat Completions endpoint. It is advertised while
/// disconnected and live-discovered after credentials are available.
#[derive(Clone, Copy, Debug)]
pub struct CompatiblePreset {
    pub id: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
    pub api_key_envs: &'static [&'static str],
    /// Legacy Haarness ids accepted in config/auth without adding duplicate
    /// provider rows to the picker.
    pub aliases: &'static [&'static str],
    pub protocol: CompatibleProtocol,
    /// Whether authenticated startup may replace the fallback through the
    /// OpenAI-compatible `GET /models` route.
    pub discover_models: bool,
    /// Small offline catalog shown before connection and retained if live
    /// discovery is unavailable.
    pub models: &'static [&'static str],
}

const fn preset_with_models(
    id: &'static str,
    name: &'static str,
    base_url: &'static str,
    api_key_envs: &'static [&'static str],
    models: &'static [&'static str],
) -> CompatiblePreset {
    CompatiblePreset {
        id,
        name,
        base_url,
        api_key_envs,
        aliases: &[],
        protocol: CompatibleProtocol::OpenAiChatCompletions,
        discover_models: true,
        models,
    }
}

const fn preset_with_aliases(
    id: &'static str,
    aliases: &'static [&'static str],
    name: &'static str,
    base_url: &'static str,
    api_key_envs: &'static [&'static str],
    models: &'static [&'static str],
) -> CompatiblePreset {
    CompatiblePreset {
        id,
        name,
        base_url,
        api_key_envs,
        aliases,
        protocol: CompatibleProtocol::OpenAiChatCompletions,
        discover_models: true,
        models,
    }
}

const fn preset_with_static_models(
    id: &'static str,
    name: &'static str,
    base_url: &'static str,
    api_key_envs: &'static [&'static str],
    models: &'static [&'static str],
) -> CompatiblePreset {
    CompatiblePreset {
        id,
        name,
        base_url,
        api_key_envs,
        aliases: &[],
        protocol: CompatibleProtocol::OpenAiChatCompletions,
        discover_models: false,
        models,
    }
}

/// Built-in compatible presets. They are always advertised, then connected by
/// a conventional environment variable or owner-only `auth.json` record.
/// Endpoints and canonical variable names are verified against provider docs.
pub const COMPAT_PRESETS: &[CompatiblePreset] = &[
    preset_with_models(
        "groq",
        "Groq",
        "https://api.groq.com/openai/v1",
        &["GROQ_API_KEY"],
        &["openai/gpt-oss-120b", "openai/gpt-oss-20b"],
    ),
    preset_with_models(
        "mistral",
        "Mistral",
        "https://api.mistral.ai/v1",
        &["MISTRAL_API_KEY"],
        &[
            "mistral-large-4-0",
            "mistral-medium-3-5",
            "mistral-small-latest",
            "mistral-large-latest",
            "devstral-latest",
        ],
    ),
    preset_with_models(
        "xai",
        "xAI",
        "https://api.x.ai/v1",
        &["XAI_API_KEY"],
        &["grok-4.7", "grok-4.3", "grok-build-0.1"],
    ),
    preset_with_models(
        "deepseek",
        "DeepSeek",
        "https://api.deepseek.com",
        &["DEEPSEEK_API_KEY"],
        &["deepseek-flash", "deepseek-v4-pro"],
    ),
    preset_with_aliases(
        "togetherai",
        &["together"],
        "Together AI",
        "https://api.together.ai/v1",
        &["TOGETHER_API_KEY"],
        &["zai-org/GLM-5.1", "moonshotai/Kimi-K2.6"],
    ),
    preset_with_aliases(
        "fireworks-ai",
        &["fireworks"],
        "Fireworks AI",
        "https://api.fireworks.ai/inference/v1",
        &["FIREWORKS_API_KEY"],
        &["accounts/fireworks/models/kimi-k2-instruct-0905"],
    ),
    preset_with_models(
        "openrouter",
        "OpenRouter",
        "https://openrouter.ai/api/v1",
        &["OPENROUTER_API_KEY"],
        &["openrouter/auto"],
    ),
    preset_with_models(
        "cerebras",
        "Cerebras",
        "https://api.cerebras.ai/v1",
        &["CEREBRAS_API_KEY"],
        &["gpt-oss-120b"],
    ),
    preset_with_models(
        "sambanova",
        "SambaNova",
        "https://api.sambanova.ai/v1",
        &["SAMBANOVA_API_KEY"],
        &["gpt-oss-120b"],
    ),
    preset_with_models(
        "nvidia",
        "NVIDIA NIM",
        "https://integrate.api.nvidia.com/v1",
        &["NVIDIA_API_KEY"],
        &["nvidia/nemotron-3-super-120b-a12b"],
    ),
    preset_with_models(
        "zai",
        "Z.AI",
        "https://api.z.ai/api/paas/v4",
        &["ZAI_API_KEY", "ZHIPU_API_KEY"],
        &["glm-5.1"],
    ),
    preset_with_models(
        "deepinfra",
        "DeepInfra",
        "https://api.deepinfra.com/v1/openai",
        &["DEEPINFRA_TOKEN", "DEEPINFRA_API_KEY"],
        &["deepseek-ai/DeepSeek-V3"],
    ),
    preset_with_models(
        "huggingface",
        "Hugging Face Inference",
        "https://router.huggingface.co/v1",
        &["HF_TOKEN"],
        &["openai/gpt-oss-120b"],
    ),
    // Sonar's documented OpenAI-compatible base has no matching `/models`
    // route. Keep its small documented catalog as a local fallback.
    preset_with_static_models(
        "perplexity",
        "Perplexity Sonar",
        "https://api.perplexity.ai",
        &["PERPLEXITY_API_KEY"],
        &[
            "sonar",
            "sonar-pro",
            "sonar-reasoning-pro",
            "sonar-deep-research",
        ],
    ),
    // Additional models.dev-compatible providers. Each serves a `/models`
    // route, so its own list decides what the picker offers and the models
    // here are only what shows before the first good read. Image, embedding,
    // guard and other non-chat entries are left out by the chat filter
    // (SiliconFlow is asked for chat models only).
    preset_with_models(
        "moonshotai",
        "Moonshot AI",
        "https://api.moonshot.ai/v1",
        &["MOONSHOT_API_KEY"],
        // kimi-k2.5 and kimi-k2-thinking were retired on 2026-08-31 and 2026-05-25.
        &["kimi-k3", "kimi-k2.6"],
    ),
    preset_with_models(
        "siliconflow",
        "SiliconFlow",
        "https://api.siliconflow.com/v1",
        &["SILICONFLOW_API_KEY"],
        &["openai/gpt-oss-120b", "Qwen/Qwen3-Coder-480B-A35B-Instruct"],
    ),
    preset_with_models(
        "siliconflow-cn",
        "SiliconFlow (China)",
        "https://api.siliconflow.cn/v1",
        &["SILICONFLOW_CN_API_KEY"],
        &[
            "deepseek-ai/DeepSeek-V3.2",
            "Qwen/Qwen3-Coder-480B-A35B-Instruct",
        ],
    ),
    preset_with_models(
        "nebius",
        "Nebius Token Factory",
        "https://api.tokenfactory.nebius.com/v1",
        &["NEBIUS_API_KEY"],
        &["openai/gpt-oss-120b", "meta-llama/Llama-3.3-70B-Instruct"],
    ),
    preset_with_models(
        "requesty",
        "Requesty",
        "https://router.requesty.ai/v1",
        &["REQUESTY_API_KEY"],
        &["gpt-5.4", "claude-sonnet-4-6"],
    ),
    preset_with_aliases(
        "novita-ai",
        &["novita"],
        "NovitaAI",
        "https://api.novita.ai/openai",
        &["NOVITA_API_KEY"],
        &["openai/gpt-oss-120b", "deepseek/deepseek-v3.2"],
    ),
    preset_with_models(
        "ollama-cloud",
        "Ollama Cloud",
        "https://ollama.com/v1",
        &["OLLAMA_API_KEY"],
        &["gpt-oss:120b"],
    ),
    preset_with_models(
        "llama",
        "Llama API",
        "https://api.llama.com/compat/v1",
        &["LLAMA_API_KEY"],
        &[
            "llama-3.3-70b-instruct",
            "llama-4-scout-17b-16e-instruct-fp8",
        ],
    ),
    preset_with_models(
        "modelscope",
        "ModelScope",
        "https://api-inference.modelscope.cn/v1",
        &["MODELSCOPE_API_KEY"],
        &["Qwen/Qwen3.5-397B-A17B", "ZhipuAI/GLM-5.2"],
    ),
    preset_with_models(
        "upstage",
        "Upstage",
        "https://api.upstage.ai/v1/solar",
        &["UPSTAGE_API_KEY"],
        &["solar-pro3", "solar-mini"],
    ),
    preset_with_models(
        "digitalocean",
        "DigitalOcean Gradient AI",
        "https://inference.do-ai.run/v1",
        &["DIGITALOCEAN_ACCESS_TOKEN"],
        &["openai-gpt-5.4", "llama3.3-70b-instruct"],
    ),
    preset_with_models(
        "wandb",
        "Weights & Biases",
        "https://api.inference.wandb.ai/v1",
        &["WANDB_API_KEY"],
        &[
            "meta-llama/Llama-3.3-70B-Instruct",
            "Qwen/Qwen3-Coder-480B-A35B-Instruct",
        ],
    ),
    preset_with_models(
        "chutes",
        "Chutes",
        "https://llm.chutes.ai/v1",
        &["CHUTES_API_KEY"],
        &["deepseek-ai/DeepSeek-V3.2-TEE", "moonshotai/Kimi-K3-TEE"],
    ),
];

#[must_use]
pub fn compatible_preset(id: &str) -> Option<&'static CompatiblePreset> {
    COMPAT_PRESETS
        .iter()
        .find(|preset| preset.id == id || preset.aliases.contains(&id))
}

/// The id a provider is listed under when `id` names it, by that id or by
/// another of its preset's ids (`together` is Together AI's `togetherai`).
/// An unlisted preset alias becomes the preset's own id; any other id is
/// returned as given.
#[must_use]
pub fn listed_provider_id(id: &str, listed: impl Fn(&str) -> bool) -> &str {
    if listed(id) {
        return id;
    }
    compatible_preset(id).map_or(id, |preset| {
        std::iter::once(preset.id)
            .chain(preset.aliases.iter().copied())
            .find(|candidate| listed(candidate))
            .unwrap_or(preset.id)
    })
}

/// Provider ids Haarness defines itself: native providers, local runtimes and
/// compatible presets (with their aliases). A credential stored for one of
/// them belongs to that provider's own endpoint.
#[must_use]
pub fn builtin_provider(id: &str) -> bool {
    matches!(
        id,
        "haarness"
            | "openai"
            | "anthropic"
            | "gemini"
            | "azure"
            | "google-vertex"
            | "github-copilot"
            | "amazon-bedrock"
            | "openai-compatible"
            | "ollama"
            | "lmstudio"
            | "echo"
    ) || compatible_preset(id).is_some()
}

/// Whether Haarness itself ships `model` for `provider`: one of the
/// provider's built-in models, or a model the catalog or the reviewed price
/// table names (by id or dated snapshot). Fine-tunes, deployment names and
/// other ids people choose are not, even when they start like a known model.
#[must_use]
pub fn ships_model(provider: &str, model: &str) -> bool {
    let builtin = match provider {
        "anthropic" => anthropic::preferred_models(),
        "openai" => openai::preferred_models(),
        "gemini" => gemini::preferred_models(),
        _ => Vec::new(),
    };
    builtin.iter().any(|info| info.id == model)
        || compatible_preset(provider).is_some_and(|preset| preset.models.contains(&model))
        || catalog::ships(model)
}

fn preset_id_in(ids: &std::collections::HashSet<String>, preset: &CompatiblePreset) -> bool {
    ids.contains(preset.id) || preset.aliases.iter().any(|alias| ids.contains(*alias))
}

fn preset_environment_key(preset: &CompatiblePreset) -> Option<String> {
    preset
        .api_key_envs
        .iter()
        .find_map(|variable| std::env::var(variable).ok())
}

/// Detect local model runtimes on their conventional loopback ports with a
/// short timeout so startup stays fast when nothing is listening.
async fn detect_local_runtimes(
    disabled: &std::collections::HashSet<String>,
) -> Vec<CompatibleInstance> {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(250))
        .build()
    {
        Ok(client) => client,
        Err(_) => return Vec::new(),
    };
    let ollama = async {
        if disabled.contains("ollama") {
            return None;
        }
        let body: serde_json::Value = client
            .get("http://127.0.0.1:11434/api/tags")
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .await
            .ok()?;
        let models: Vec<String> = body["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| entry["name"].as_str())
            .map(str::to_owned)
            .collect();
        Some(CompatibleInstance {
            id: "ollama".into(),
            name: "Ollama".into(),
            base_url: "http://127.0.0.1:11434/v1".into(),
            api_key: None,
            models,
            discover_models: true,
            server_output_limit: false,
            replay_observed_reasoning: true,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: true,
            headers: ProviderHeaders::default(),
        })
    };
    let lmstudio = async {
        if disabled.contains("lmstudio") {
            return None;
        }
        let body: serde_json::Value = client
            .get("http://127.0.0.1:1234/v1/models")
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .await
            .ok()?;
        let models: Vec<String> = body["data"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| entry["id"].as_str())
            .map(str::to_owned)
            .collect();
        Some(CompatibleInstance {
            id: "lmstudio".into(),
            name: "LM Studio".into(),
            base_url: "http://127.0.0.1:1234/v1".into(),
            api_key: None,
            models,
            discover_models: true,
            server_output_limit: false,
            replay_observed_reasoning: true,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: true,
            headers: ProviderHeaders::default(),
        })
    };
    let (ollama, lmstudio) = tokio::join!(ollama, lmstudio);
    [ollama, lmstudio].into_iter().flatten().collect()
}

fn merge_discovered_models(
    preferred: Vec<haarness_protocol::ModelInfo>,
    mut discovered: Vec<haarness_protocol::ModelInfo>,
) -> Vec<haarness_protocol::ModelInfo> {
    discovered.sort_by(|left, right| model_order::compare(&left.id, &right.id));
    discovered.dedup_by(|left, right| left.id == right.id);
    let mut discovered: HashMap<_, _> = discovered
        .into_iter()
        .map(|model| (model.id.clone(), model))
        .collect();
    let mut models: Vec<_> = preferred
        .into_iter()
        // Prefer the live object as it can carry provider-authoritative limits
        // and capabilities. The embedded catalog still fills fields omitted by
        // the provider API (notably OpenAI pricing and token limits).
        .filter_map(|model| {
            if let Some(live) = discovered.remove(&model.id) {
                return Some(live);
            }
            // Some APIs list only dated snapshots (`claude-haiku-4-5-20251001`,
            // `gpt-5.4-mini-2026-01-01`) while configs and chats name the
            // alias. Keep the alias, with the live details of its newest
            // snapshot, so a configured or stored model is still found.
            let mut snapshots: Vec<_> = discovered
                .keys()
                .filter(|id| {
                    id.strip_prefix(model.id.as_str())
                        .is_some_and(catalog::is_snapshot)
                })
                .cloned()
                .collect();
            snapshots.sort();
            let newest = snapshots.pop()?;
            for older in snapshots {
                discovered.remove(&older);
            }
            let mut live = discovered.remove(&newest)?;
            live.id = model.id;
            Some(live)
        })
        .collect();
    let mut remaining: Vec<_> = discovered.into_values().collect();
    remaining.sort_by(|left, right| model_order::compare(&left.id, &right.id));
    models.extend(remaining);
    models
}

fn is_snapshot(id: &str) -> bool {
    id.as_bytes()
        .windows(5)
        .any(|window| window[0] == b'-' && window[1..].iter().all(u8::is_ascii_digit))
}

impl ProviderRegistry for NativeProviderRegistry {
    fn get(&self, id: &str) -> Option<Arc<dyn ModelProvider>> {
        self.providers.get(id).cloned()
    }
    fn list(&self) -> Vec<ProviderInfo> {
        let mut values: Vec<_> = self
            .providers
            .values()
            .map(|provider| {
                let mut info = provider.info();
                for model in &mut info.models {
                    model.display_name =
                        haarness_protocol::model_display_name(&model.id, Some(&model.display_name));
                }
                info
            })
            .collect();
        values.sort_by(|a, b| a.id.cmp(&b.id));
        values
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use haarness_core::ModelProvider;
    use haarness_protocol::ModelInfo;

    use super::{
        COMPAT_PRESETS, CompatibleInstance, CompatibleModelDefaults, CompatibleProtocol,
        OpenAiCompatibleProvider, ProviderHeaders, merge_discovered_models,
    };

    fn model(id: &str) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            display_name: id.into(),
            reasoning: true,
            reasoning_efforts: ["low", "medium", "high"].map(str::to_owned).to_vec(),
            images: true,
            video: false,
            tools: true,
            context_window: None,
            max_output_tokens: None,
            pricing: None,
        }
    }

    #[test]
    fn a_dated_snapshot_keeps_its_alias_in_the_catalog() {
        let model = |id: &str, name: &str| ModelInfo {
            id: id.into(),
            display_name: name.into(),
            reasoning: false,
            reasoning_efforts: Vec::new(),
            images: false,
            video: false,
            tools: true,
            context_window: None,
            max_output_tokens: None,
            pricing: None,
        };
        let merged = merge_discovered_models(
            vec![
                model("claude-haiku-4-5", "alias"),
                model("claude-sonnet-4-6", "alias"),
                model("gpt-5.4-mini", "alias"),
            ],
            vec![
                model("claude-haiku-4-5-20250101", "older"),
                model("claude-haiku-4-5-20251001", "Claude Haiku 4.5"),
                model("claude-sonnet-4-6", "Claude Sonnet 4.6"),
                model("claude-opus-4-5-20251101", "Claude Opus 4.5"),
                model("gpt-5.4-mini-2025-12-01", "older"),
                model("gpt-5.4-mini-2026-01-01", "GPT-5.4 mini"),
                model("gpt-5.4-mini-tuned", "a different model"),
            ],
        );
        let ids: Vec<_> = merged.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "claude-haiku-4-5",
                "claude-sonnet-4-6",
                "gpt-5.4-mini",
                "gpt-5.4-mini-tuned",
                "claude-opus-4-5-20251101"
            ]
        );
        assert_eq!(merged[0].display_name, "Claude Haiku 4.5");
        assert_eq!(merged[2].display_name, "GPT-5.4 mini");
    }

    #[test]
    fn discovery_removes_stale_preferred_models_and_keeps_aliases_first() {
        let mut live_current = model("current");
        live_current.context_window = Some(1_000_000);
        let models = merge_discovered_models(
            vec![model("current"), model("stale")],
            vec![model("current-2026-08-01"), model("new"), live_current],
        );
        let ids: Vec<_> = models.into_iter().map(|model| model.id).collect();

        assert_eq!(ids[0], "current");
        assert!(ids.contains(&"new".into()));
        assert!(!ids.contains(&"stale".into()));
        assert_eq!(ids.last().map(String::as_str), Some("current-2026-08-01"));
    }

    #[test]
    fn discovery_keeps_provider_authoritative_metadata() {
        let mut live = model("current");
        live.context_window = Some(1_000_000);
        let models = merge_discovered_models(vec![model("current")], vec![live]);
        assert_eq!(models[0].context_window, Some(1_000_000));
    }

    #[test]
    fn configurable_protocols_preserve_legacy_aliases_and_name_native_wires() {
        assert_eq!(
            CompatibleProtocol::parse("openai-chat-completions"),
            Ok(CompatibleProtocol::OpenAiChatCompletions)
        );
        assert_eq!(
            CompatibleProtocol::parse("openai-compatible"),
            Ok(CompatibleProtocol::OpenAiChatCompletions)
        );
        assert_eq!(
            CompatibleProtocol::parse("anthropic-messages"),
            Ok(CompatibleProtocol::AnthropicMessages)
        );
        assert_eq!(
            CompatibleProtocol::parse("gemini-generate-content"),
            Ok(CompatibleProtocol::GeminiGenerateContent)
        );
        assert_eq!(
            CompatibleProtocol::parse("azure-openai"),
            Ok(CompatibleProtocol::AzureOpenAi)
        );
        assert_eq!(
            CompatibleProtocol::parse("google-vertex"),
            Ok(CompatibleProtocol::GoogleVertex)
        );
        assert_eq!(
            CompatibleProtocol::parse("github-copilot"),
            Ok(CompatibleProtocol::GithubCopilot)
        );
        assert_eq!(
            CompatibleProtocol::parse("amazon-bedrock"),
            Ok(CompatibleProtocol::AmazonBedrock)
        );
        assert!(CompatibleProtocol::parse("vendor-private-wire").is_err());
    }

    #[test]
    fn provider_headers_are_validated_without_exposing_values() {
        let headers = super::ProviderHeaders::parse(BTreeMap::from([(
            "X-Organization".into(),
            "secret-value".into(),
        )]))
        .unwrap();
        assert!(!headers.is_empty());
        let error = super::ProviderHeaders::parse(BTreeMap::from([(
            "bad header".into(),
            "secret-value".into(),
        )]))
        .err()
        .unwrap();
        assert!(error.contains("invalid header name"));
    }

    #[test]
    fn every_preset_is_visible_without_a_key_and_has_a_fallback_model() {
        let mut ids = HashSet::new();
        for preset in COMPAT_PRESETS {
            assert!(ids.insert(preset.id), "duplicate preset {}", preset.id);
            assert!(
                !preset.models.is_empty(),
                "{} needs a disconnected-state fallback model",
                preset.id
            );
            let provider = OpenAiCompatibleProvider::new(CompatibleInstance {
                id: preset.id.into(),
                name: preset.name.into(),
                base_url: preset.base_url.into(),
                api_key: None,
                models: preset.models.iter().map(|model| (*model).into()).collect(),
                discover_models: true,
                server_output_limit: false,
                replay_observed_reasoning: false,
                model_defaults: CompatibleModelDefaults::default(),
                assume_authenticated: false,
                headers: ProviderHeaders::default(),
            });
            let info = provider.info();
            assert!(!info.authenticated, "{} must require connection", preset.id);
            assert!(!info.models.is_empty());
        }
        assert_eq!(COMPAT_PRESETS.len(), 27);
        assert_eq!(
            super::compatible_preset("together").unwrap().id,
            "togetherai"
        );
        assert_eq!(
            super::compatible_preset("fireworks").unwrap().id,
            "fireworks-ai"
        );
        assert_eq!(super::compatible_preset("novita").unwrap().id, "novita-ai");
        assert!(
            !super::compatible_preset("perplexity")
                .unwrap()
                .discover_models
        );
        // Every other preset follows its provider's own list.
        for preset in COMPAT_PRESETS
            .iter()
            .filter(|preset| preset.id != "perplexity")
        {
            assert!(preset.discover_models, "{} reads its model list", preset.id);
        }

        let together = super::compatible_preset("togetherai").unwrap();
        let legacy_ids = HashSet::from(["together".to_owned()]);
        assert!(super::preset_id_in(&legacy_ids, together));
    }

    #[test]
    fn a_preset_alias_names_the_provider_the_server_lists() {
        let listed = |ids: &'static [&'static str]| move |id: &str| ids.contains(&id);
        let presets: &'static [&str] =
            &["togetherai", "fireworks-ai", "novita-ai", "openai", "echo"];
        for (alias, id) in [
            ("together", "togetherai"),
            ("fireworks", "fireworks-ai"),
            ("novita", "novita-ai"),
            ("togetherai", "togetherai"),
            ("openai", "openai"),
        ] {
            assert_eq!(super::listed_provider_id(alias, listed(presets)), id);
        }
        // A configured provider under the alias keeps it.
        assert_eq!(
            super::listed_provider_id("together", listed(&["together"])),
            "together"
        );
        assert_eq!(
            super::listed_provider_id("togetherai", listed(&["together"])),
            "together"
        );
        assert_eq!(
            super::listed_provider_id("together", listed(&[])),
            "togetherai"
        );
        assert_eq!(super::listed_provider_id("nope", listed(presets)), "nope");
    }

    /// A server starts with the providers as they are; their lists load
    /// once it answers. Before, a configured endpoint that accepted the
    /// connection and never answered held the start for its whole timeout.
    #[tokio::test]
    async fn the_registry_is_built_without_waiting_for_a_model_list() {
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let instance = super::ProviderInstance::OpenAiChatCompletions(CompatibleInstance {
            id: "stalled".into(),
            name: "Stalled".into(),
            base_url: format!("http://{}/v1", silent.local_addr().unwrap()),
            api_key: Some("fixture-key".into()),
            models: vec!["fallback".into()],
            discover_models: true,
            server_output_limit: false,
            replay_observed_reasoning: false,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: false,
            headers: ProviderHeaders::default(),
        });
        let store = std::sync::Arc::new(super::CredentialStore::new(
            std::env::temp_dir().join(format!("haarness-no-auth-{}.json", std::process::id())),
        ));
        let started = std::time::Instant::now();
        let registry = super::NativeProviderRegistry::with_providers_and_store(
            vec![instance],
            &HashSet::new(),
            store,
        )
        .await;
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        let provider = haarness_core::ProviderRegistry::get(&registry, "stalled").unwrap();
        assert_eq!(provider.info().models[0].id, "fallback");
    }
}
