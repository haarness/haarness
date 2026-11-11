use crate::{
    auth::{Credential, CredentialStore, XaiOAuthClient, unix_millis, xai_access_token_expiring},
    media::Unreadable,
    ollama,
    sse::{json_lines_stream, json_stream},
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
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
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
    /// One LM Studio load at a time, so parallel requests load a model once.
    load_lock: Arc<Mutex<()>>,
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
    /// Whether each model takes audio, for the models the last listing gave
    /// input modalities for (see [`takes_audio`]).
    listed_audio: ListedAudio,
}

/// Whether each listed model takes audio, by the input modalities its
/// catalog entry lists. Shared with the Haarness Cloud provider, which makes
/// a new transport for every request.
pub(crate) type ListedAudio = Arc<std::sync::RwLock<HashMap<String, bool>>>;

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
/// The metadata key keeping what a server attached to one tool call
/// (`extra_content`: Gemini's OpenAI-compatible API puts the call's thought
/// signature there and refuses the next request when it does not come back).
const EXTRA_CONTENT: &str = "extra_content";
/// The provider and model that attached it: it goes back to them alone.
const EXTRA_CONTENT_SOURCE: &str = "extra_content_source";

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
            // A base pasted with its endpoint (`…/v1/chat/completions`)
            // still reaches it.
            endpoint: format!(
                "{}/chat/completions",
                crate::endpoint::chat_completions_base(&instance.base_url)
            ),
            id: instance.id,
            name: instance.name,
            api_key: instance.api_key.map(SecretString::from),
            auth_store: None,
            auth_aliases: Vec::new(),
            xai_oauth: (provider_id == "xai").then(XaiOAuthClient::default),
            refresh_lock: Arc::default(),
            load_lock: Arc::default(),
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
            listed_audio: ListedAudio::default(),
        }
    }

    /// First-party router credentials must not follow redirects.
    pub(crate) fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// Reads and writes what listings say of audio in `listed`.
    pub(crate) fn with_listed_audio(mut self, listed: ListedAudio) -> Self {
        self.listed_audio = listed;
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
        let model = std::env::var("OPENAI_COMPATIBLE_MODEL")
            .ok()
            .filter(|model| !model.trim().is_empty());
        Some(Self::new(CompatibleInstance {
            id: "openai-compatible".into(),
            name: "OpenAI-compatible".into(),
            base_url: base,
            api_key: std::env::var("OPENAI_COMPATIBLE_API_KEY").ok(),
            // Without a model named, the server's own list says which it
            // serves; `default` stays only for a server that lists none.
            discover_models: model.is_none(),
            models: vec![model.unwrap_or_else(|| "default".into())],
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
        // SiliconFlow's list mixes image, audio and embedding models with
        // chat ones and filters on request.
        let filter = if self.id.starts_with("siliconflow") {
            "?sub_type=chat"
        } else {
            ""
        };
        let request = self
            .client
            .get(format!("{base}/models{filter}"))
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
            let mut audio = HashMap::new();
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
                    if let Some(inputs) = input_modalities(entry) {
                        audio.insert(
                            model.id.clone(),
                            inputs.iter().any(|input| input.as_str() == Some("audio")),
                        );
                    }
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
            *self
                .listed_audio
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = audio;
            let (models, weak) = match self.id.as_str() {
                "ollama" => self.ollama_details(base, models).await,
                "lmstudio" => self.lmstudio_details(base, models).await,
                _ => (models, HashSet::new()),
            };
            let mut models = crate::merge_discovered_models(self.models.clone(), models);
            if matches!(self.id.as_str(), "ollama" | "lmstudio") {
                // Every client gives a new chat a provider's first model:
                // on this computer, one that can do the work.
                models.sort_by_key(|model| local_rank(model, &weak));
            }
            return Some(models);
        }
        None
    }

    /// What Ollama knows of each installed model (`/api/show`): whether it
    /// reads images, calls tools and thinks, and its context window. Its
    /// OpenAI-style list names the models and nothing more. A model runs on
    /// this computer, so its price is nothing. Also the models too small to
    /// follow an agent's instructions (see [`local_rank`]).
    async fn ollama_details(
        &self,
        base: &str,
        models: Vec<ModelInfo>,
    ) -> (Vec<ModelInfo>, HashSet<String>) {
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
        let mut weak = HashSet::new();
        let models = models
            .into_iter()
            .zip(details)
            .map(|(mut model, details)| {
                if let Some(details) = details {
                    apply_ollama_details(&mut model, &details);
                    if details["details"]["parameter_size"]
                        .as_str()
                        .and_then(billions_of_parameters)
                        .is_some_and(|billions| billions < SMALL_MODEL_BILLIONS)
                    {
                        weak.insert(model.id.clone());
                    }
                }
                // The window it runs with, though `/api/show` didn't answer.
                model.context_window.get_or_insert_with(|| {
                    ollama::context_window(None, &[ollama::chosen_context()])
                });
                model.pricing.get_or_insert(ModelPricing {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                    long_context: None,
                });
                model
            })
            .collect();
        (models, weak)
    }

    /// LM Studio's own list (`/api/v1/models`, from 0.4): each model's
    /// name, the longest context it takes, whether it reads images, and what
    /// is loaded with which context. `None` from an older LM Studio.
    async fn lmstudio_models(&self, root: &str) -> Option<Vec<Value>> {
        let body: Value = self
            .client
            .get(format!("{root}/api/v1/models"))
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .await
            .ok()?;
        Some(body["models"].as_array()?.clone())
    }

    /// What LM Studio knows of each downloaded model, which its OpenAI-style
    /// list leaves out. Every model calls tools there (LM Studio prompts
    /// those not trained for them itself), and it runs on this computer, so
    /// its price is nothing. Also the models too small for an agent, or not
    /// trained to call tools (see [`local_rank`]).
    async fn lmstudio_details(
        &self,
        base: &str,
        models: Vec<ModelInfo>,
    ) -> (Vec<ModelInfo>, HashSet<String>) {
        let root = base.strip_suffix("/v1").unwrap_or(base);
        let details = self.lmstudio_models(root).await.unwrap_or_default();
        let mut weak = HashSet::new();
        let models = models
            .into_iter()
            .map(|mut model| {
                if let Some(entry) = details
                    .iter()
                    .find(|entry| entry["key"] == model.id.as_str())
                {
                    apply_lmstudio_details(&mut model, entry);
                    let small = entry["params_string"]
                        .as_str()
                        .and_then(billions_of_parameters)
                        .is_some_and(|billions| billions < SMALL_MODEL_BILLIONS);
                    if small || entry["capabilities"]["trained_for_tool_use"] == false {
                        weak.insert(model.id.clone());
                    }
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
            .collect();
        (models, weak)
    }

    /// LM Studio loads a model on its first request with the context its
    /// settings name, 4K or 8K out of the box: too small for an agent's
    /// instructions and tools, so every request would be refused. A model
    /// that isn't loaded, or is loaded with too little room, is loaded here
    /// with [`LMSTUDIO_CONTEXT`] (or the model's longest, when shorter).
    /// It stays loaded, as one loaded in LM Studio does. A model LM Studio
    /// doesn't have is refused: it would answer with whichever model is
    /// loaded. Anything else that goes wrong is left to the request.
    async fn lmstudio_load(&self, model: &str) -> CoreResult<()> {
        let _guard = self.load_lock.lock().await;
        let Some(base) = self.endpoint.strip_suffix("/chat/completions") else {
            return Ok(());
        };
        let root = base.strip_suffix("/v1").unwrap_or(base);
        let Some(models) = self.lmstudio_models(root).await else {
            return Ok(());
        };
        let Some(entry) = models.iter().find(|entry| entry["key"] == model) else {
            return Err(CoreError::Provider(format!(
                "LM Studio has no model {model}. Download it in LM Studio or with `lms get {model}`, or pick a model you have."
            )));
        };
        let context = lmstudio_context(entry);
        let instances = entry["loaded_instances"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let roomy = |instance: &Value| {
            instance["config"]["context_length"]
                .as_u64()
                .is_some_and(|loaded| loaded >= LMSTUDIO_MIN_CONTEXT.min(context))
        };
        if instances.iter().any(roomy) {
            return Ok(());
        }
        for instance in &instances {
            if let Some(id) = instance["id"].as_str() {
                tracing::info!(
                    model,
                    instance = id,
                    "reloading an LM Studio model with room for the request"
                );
                let _ = self
                    .client
                    .post(format!("{root}/api/v1/models/unload"))
                    .json(&json!({"instance_id": id}))
                    .timeout(std::time::Duration::from_secs(30))
                    .send()
                    .await;
            }
        }
        // A computer short of memory for that much (LM Studio refuses a load
        // its guardrails say won't fit) may still fit the least that works.
        let smallest = LMSTUDIO_MIN_CONTEXT.min(context);
        for context in [context, smallest] {
            tracing::info!(model, context, "loading an LM Studio model");
            let loaded = self
                .client
                .post(format!("{root}/api/v1/models/load"))
                .json(&json!({"model": model, "context_length": context}))
                .timeout(std::time::Duration::from_secs(300))
                .send()
                .await;
            match loaded {
                Ok(response) if response.status().is_success() => return Ok(()),
                Ok(response)
                    if context > smallest
                        && response.status() != reqwest::StatusCode::NOT_FOUND =>
                {
                    let status = response.status();
                    let detail = response.text().await.unwrap_or_default();
                    tracing::warn!(model, context, %status, detail, "LM Studio did not load the model; trying with less room");
                }
                Ok(response) => {
                    tracing::warn!(model, status = %response.status(), "LM Studio did not load the model");
                    break;
                }
                Err(error) => {
                    tracing::warn!(model, %error, "LM Studio did not load the model");
                    break;
                }
            }
        }
        Ok(())
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
            load_lock: Arc::default(),
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
            listed_audio: ListedAudio::default(),
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
            .and_then(|value| price(value * 1_000_000.0))
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
        // Mistral's pages spell the id both ways. Prices as listed on
        // docs.mistral.ai/models/mistral-large-4-0, checked 2026-11-01.
        ("mistral", "mistral-large-4-0" | "mistral-large-4") => {
            info.display_name = "Mistral Large 4".into();
            info.images = true;
            info.context_window = Some(1_048_576);
            info.pricing = Some(ModelPricing {
                input: 0.7,
                output: 2.1,
                cache_read: 0.07,
                cache_write: 0.0,
                long_context: None,
            });
        }
        // Small 4 answers to its dated id and to `mistral-small-latest`. Prices
        // as listed on docs.mistral.ai/models/mistral-small-4-0-26-03, checked
        // 2026-11-01; the page names no cached-input price, so cached input
        // costs what input does.
        ("mistral", "mistral-small-2603" | "mistral-small-latest") => {
            info.display_name = "Mistral Small 4".into();
            info.images = true;
            info.context_window = Some(256_000);
            info.pricing = Some(ModelPricing {
                input: 0.15,
                output: 0.6,
                cache_read: 0.15,
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
        ("zai", id) if id.starts_with("glm-") => {
            if id == "glm-5.1" {
                info.display_name = "GLM-5.1".into();
            }
            info.reasoning |= !info.reasoning_efforts.is_empty() || id == "glm-5.1";
            // Z.ai's list says nothing of input; the 5.3 Flash models read
            // pictures, the others refuse anything but text.
            info.images |= matches!(id, "glm-5.3-flash" | "glm-5.3-flashx");
            // Z.ai's list gives no context windows; without one a chat
            // never compacts and fails once it outgrows the model.
            info.context_window = info.context_window.or(zai_context_window(id));
            if let Some((input, cache_read, output)) = zai_list_price(id) {
                info.pricing = Some(ModelPricing {
                    input,
                    output,
                    cache_read,
                    cache_write: 0.0,
                    long_context: None,
                });
            }
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
    // Mistral's `name` is the id of the model an alias points to
    // ("mistral-small-2603" for `magistral-small-latest`), not a label: shown
    // as one, different ids read alike and known names were lost.
    if let Some(name) = entry
        .get("display_name")
        .or_else(|| entry.get("name").filter(|_| provider != "mistral"))
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
        // Groq names some models "Qwen/Qwen3.8-27B"; the vendor is its id's.
        let name = match name.split_once('/') {
            Some((_, model)) if provider == "groq" && !model.is_empty() => model,
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

    if let Some(modalities) = input_modalities(entry) {
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
    // Groq lists what each model takes as `supported_features` ("tools",
    // "json_mode", "reasoning"). ALLaM 2 7B takes no tools and refuses every
    // request that carries them, as a chat with Haarness's tools does.
    if provider == "groq"
        && let Some(features) = entry.get("supported_features").and_then(Value::as_array)
    {
        let takes = |feature: &str| features.iter().any(|value| value.as_str() == Some(feature));
        info.tools = takes("tools");
        info.reasoning |= takes("reasoning");
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
        // Mistral lists the other ids of the same model in `aliases`
        // (`mistral-medium-latest` is Medium 3.5): an alias of a model whose
        // reasoning levels and price are known takes them, rather than
        // offering no levels and no cost.
        if info.reasoning_efforts.is_empty()
            && let Some(known) = entry["aliases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .find(|alias| known_reasoning_effort_model(provider, alias))
        {
            let target = model_info(provider, known, CompatibleModelDefaults::default());
            info.reasoning_efforts = target.reasoning_efforts;
            info.pricing = info.pricing.take().or(target.pricing);
        }
        // A model with known reasoning levels reasons, whatever the
        // catalog says of the id it is listed under.
        if !info.reasoning_efforts.is_empty() {
            info.reasoning = true;
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
        let rate = |pointer: &str| {
            entry
                .pointer(pointer)
                .and_then(json_number)
                .and_then(|value| price(value * 1_000_000.0))
        };
        info.pricing = price(input * 1_000_000.0)
            .zip(price(output * 1_000_000.0))
            .map(|(input, output)| ModelPricing {
                input,
                output,
                cache_read: rate("/pricing/input_cache_read").unwrap_or(0.0),
                cache_write: rate("/pricing/input_cache_write").unwrap_or(0.0),
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
                .and_then(|value| price(value / 10_000.0))
        };
        info.pricing =
            price(input / 10_000.0)
                .zip(price(output / 10_000.0))
                .map(|(input, output)| ModelPricing {
                    input,
                    output,
                    cache_read: rate("cached_prompt_text_token_price").unwrap_or(0.0),
                    cache_write: 0.0,
                    long_context: entry
                        .get("long_context_threshold")
                        .and_then(Value::as_u64)
                        .zip(rate("prompt_text_token_price_long_context"))
                        .zip(rate("completion_text_token_price_long_context"))
                        .map(|((threshold, input), output)| {
                            haarness_protocol::LongContextPricing {
                                above_input_tokens: threshold.saturating_sub(1),
                                input,
                                output,
                                cache_read: rate("cached_prompt_text_token_price_long_context")
                                    .unwrap_or(0.0),
                                cache_write: 0.0,
                            }
                        }),
                })
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
        info.pricing = price(input)
            .zip(price(output))
            .map(|(input, output)| ModelPricing {
                input,
                output,
                cache_read: entry
                    .pointer("/metadata/pricing/cache_read_tokens")
                    .and_then(json_number)
                    .and_then(price)
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

/// Under this many billion parameters a model on this computer seldom
/// follows an agent's instructions and tools: SmolLM2 1.7B answers with
/// nothing, Llama 3.2 3B breaks the code it edits.
const SMALL_MODEL_BILLIONS: f64 = 4.0;

/// The least window a model a new chat gets on this computer should have.
const LOCAL_ROOM: u64 = 16_384;

/// How well a model on this computer suits a new chat, best first: one that
/// calls tools, runs here rather than on a cloud the runtime signs in to,
/// isn't small (`weak`, from the runtime's details), makes its tool calls as
/// calls rather than writing them as text, and has room for the instructions
/// and tools. Equal ranks keep the catalog's order.
fn local_rank(model: &ModelInfo, weak: &HashSet<String>) -> (bool, bool, bool, bool, bool) {
    (
        !model.tools,
        model.id.ends_with("-cloud") || model.id.ends_with(":cloud"),
        weak.contains(&model.id),
        writes_calls_as_text(&model.id),
        model
            .context_window
            .is_some_and(|window| window < LOCAL_ROOM),
    )
}

/// Models a runtime lists as calling tools that mostly write their calls into
/// the reply as text instead, and then go on as if the call had run, making up
/// its result (Qwen2.5 Coder, measured on Ollama 2026-11-09: it read files it
/// never opened and once wrote invented code over a real one). Haarness runs
/// a reply that is nothing but such a call, but one inside prose can't be told
/// from an example, so these are never a new chat's first choice.
fn writes_calls_as_text(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    let name = id.rsplit('/').next().unwrap_or(&id);
    name.starts_with("qwen2.5-coder")
}

/// A parameter count as runtimes write it, in billions: `7.6B`, `135M`, `30B-A3B`
/// (its total), or a bare count (`116829156672`).
fn billions_of_parameters(text: &str) -> Option<f64> {
    let text = text.trim();
    let digits = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let number: f64 = text[..digits].parse().ok()?;
    match text[digits..]
        .chars()
        .next()
        .map(|unit| unit.to_ascii_uppercase())
    {
        Some('B') => Some(number),
        Some('M') => Some(number / 1_000.0),
        Some('K') => Some(number / 1_000_000.0),
        Some('T') => Some(number * 1_000.0),
        None => Some(number / 1e9),
        Some(_) => None,
    }
}

/// The context Haarness loads an LM Studio model with: room for the
/// instructions, tools and a long chat, without the memory of a model's
/// full window (often 256K).
const LMSTUDIO_CONTEXT: u64 = 65_536;
/// Less than this loaded, and a request is refused for its instructions and
/// tools alone.
const LMSTUDIO_MIN_CONTEXT: u64 = 32_768;

/// The context an LM Studio model runs with in Haarness: what it is loaded
/// with, else what Haarness loads it with.
fn lmstudio_context(entry: &Value) -> u64 {
    let longest = entry["max_context_length"]
        .as_u64()
        .unwrap_or(LMSTUDIO_CONTEXT);
    entry["loaded_instances"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|instance| instance["config"]["context_length"].as_u64())
        .filter(|loaded| *loaded >= LMSTUDIO_MIN_CONTEXT.min(longest))
        .max()
        .unwrap_or_else(|| LMSTUDIO_CONTEXT.min(longest))
}

/// One model's entry in LM Studio's own list, into what Haarness offers.
fn apply_lmstudio_details(model: &mut ModelInfo, entry: &Value) {
    if let Some(name) = entry["display_name"]
        .as_str()
        .filter(|name| !name.is_empty())
    {
        name.clone_into(&mut model.display_name);
    }
    let capabilities = &entry["capabilities"];
    if let Some(vision) = capabilities["vision"].as_bool() {
        model.images = vision;
    }
    // `{"allowed_options": ["off", "low", "medium", "xhigh", "on"], …}`,
    // passed on as `reasoning_effort`, which names `off` `none` (and
    // refuses `off`); `on` is the model's default.
    if let Some(options) = capabilities["reasoning"]["allowed_options"].as_array() {
        model.reasoning_efforts = options
            .iter()
            .filter_map(Value::as_str)
            .map(|option| if option == "off" { "none" } else { option })
            .filter(|option| ["none", "minimal", "low", "medium", "high", "xhigh"].contains(option))
            .map(str::to_owned)
            .collect();
        model.reasoning = !model.reasoning_efforts.is_empty();
    }
    model.tools = true;
    model.context_window = Some(lmstudio_context(entry));
}

/// Ollama's 404 for a model it hasn't downloaded, with how to get it: the
/// picker lists only downloaded models, so this is a model typed by hand or
/// removed since the chat began.
fn local_model_missing(error: CoreError, model: &str) -> CoreError {
    match error {
        CoreError::Provider(text)
            if text.starts_with("HTTP 404") && text.ends_with("not found") =>
        {
            CoreError::Provider(format!(
                "{text}. Download it with `ollama pull {model}`, or pick a model you have."
            ))
        }
        other => other,
    }
}

/// One model's `/api/show` answer from Ollama, into what Haarness offers.
fn apply_ollama_details(model: &mut ModelInfo, details: &Value) {
    apply_ollama_details_chosen(model, details, ollama::chosen_context());
}

/// [`apply_ollama_details`], with the context the person chose for Ollama
/// (`OLLAMA_CONTEXT_LENGTH`).
fn apply_ollama_details_chosen(model: &mut ModelInfo, details: &Value, chosen: Option<u64>) {
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
        // The levels Ollama lists (`thinking.values`, gpt-oss's `low` to
        // `high`); a model it lists as on or off turns thinking off with
        // `none`. gpt-oss always thinks.
        let levels: Vec<String> = details["thinking"]["values"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|level| ["minimal", "low", "medium", "high", "xhigh"].contains(level))
            .map(str::to_owned)
            .collect();
        model.reasoning_efforts = if !model.reasoning {
            Vec::new()
        } else if !levels.is_empty() {
            levels
        } else if model.id.starts_with("gpt-oss") {
            ["low", "medium", "high"].map(str::to_owned).to_vec()
        } else {
            ["none", "low", "medium", "high"]
                .map(str::to_owned)
                .to_vec()
        };
    }
    model.context_window = Some(ollama::context_window(
        ollama::longest_context(details),
        &[chosen, ollama::modelfile_context(details)],
    ));
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
        // Groq's list says it at the top level; its Orpheus voices answer
        // with speech only.
        .or_else(|| entry.get("output_modalities"))
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
        "qwen-image",
        "image-edit",
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

/// GLM models that turn thinking on and off with `thinking` and ignore
/// `reasoning_effort`.
fn zai_thinking_switch(model: &str) -> bool {
    matches!(
        model,
        "glm-5.1" | "glm-5" | "glm-5-turbo" | "glm-4.7" | "glm-4.6" | "glm-4.5"
    )
}

/// GLM context windows as each model's page on docs.z.ai lists them,
/// checked 2026-11-01: 1M for GLM-5.3 (and its Flash models) and 5.2, 200K
/// for 5.1, 5, 5-Turbo, 4.7 and 4.6, 128K for 4.5 and 4.5-Air.
fn zai_context_window(model: &str) -> Option<u64> {
    Some(match model {
        "glm-5.3" | "glm-5.3-flash" | "glm-5.3-flashx" | "glm-5.2" => 1_000_000,
        "glm-5.1" | "glm-5" | "glm-5-turbo" | "glm-4.7" | "glm-4.6" => 200_000,
        "glm-4.5" | "glm-4.5-air" => 128_000,
        _ => return None,
    })
}

/// Z.ai's list prices per million tokens (input, cached input, output), as
/// listed on docs.z.ai/guides/overview/pricing, checked 2026-11-01.
fn zai_list_price(model: &str) -> Option<(f64, f64, f64)> {
    Some(match model {
        "glm-5.3" | "glm-5.2" | "glm-5.1" => (1.4, 0.26, 4.4),
        "glm-5.3-flash" => (0.15, 0.03, 0.5),
        "glm-5.3-flashx" => (0.37, 0.075, 1.25),
        "glm-5" => (1.0, 0.2, 3.2),
        "glm-4.7" | "glm-4.6" | "glm-4.5" => (0.6, 0.11, 2.2),
        "glm-4.5-air" => (0.2, 0.03, 1.1),
        _ => return None,
    })
}

fn known_reasoning_efforts(provider: &str, model: &str) -> Vec<&'static str> {
    match (provider, model) {
        ("groq", "openai/gpt-oss-20b" | "openai/gpt-oss-120b") | ("xai", "grok-4.5") => {
            vec!["low", "medium", "high"]
        }
        ("xai", "grok-4.6" | "grok-4.7") => vec!["low", "medium", "high", "xhigh"],
        // GLM 5.3 always thinks and takes these levels only; 5.2 takes every
        // level; the older models ignore a level and switch thinking with
        // `thinking` (see `zai_thinking_switch`).
        ("zai", "glm-5.3" | "glm-5.3-flash" | "glm-5.3-flashx") => vec!["low", "high", "max"],
        ("zai", "glm-5.2") => vec!["none", "low", "medium", "high", "max"],
        ("zai", model) if zai_thinking_switch(model) => vec!["none", "high"],
        (
            "deepseek",
            "deepseek-flash"
            | "deepseek-v4-flash"
            | "deepseek-v4-pro"
            | "deepseek-v4-flash-vision-exp",
        ) => vec!["none", "low", "high", "max"],
        // Large 4 refuses "none" and takes three levels; Medium 3.5 and
        // Small refuse "low" and "medium".
        ("mistral", "mistral-large-4-0" | "mistral-large-4") => vec!["low", "medium", "high"],
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

/// Add one streamed part of a Mistral reply to what the next request sends
/// back. Mistral streams its thinking a few words a chunk, each as a part
/// of its own; sent back so, every few words cost a part's wrapping, which
/// made a reply's thinking several times larger in each later request.
/// Consecutive thinking parts, and consecutive text parts, become one.
fn push_mistral_part(content: &mut Vec<Value>, part: &Value) {
    let texts = |part: &Value| -> Option<String> {
        match part["type"].as_str()? {
            "thinking" => part["thinking"].as_array().and_then(|thoughts| {
                thoughts
                    .iter()
                    .all(|thought| thought["type"] == "text" && thought["text"].is_string())
                    .then(|| {
                        thoughts
                            .iter()
                            .filter_map(|thought| thought["text"].as_str())
                            .collect()
                    })
            }),
            "text" => part["text"].as_str().map(str::to_owned),
            _ => None,
        }
    };
    if let (Some(previous), Some(addition)) = (content.last_mut(), texts(part))
        && previous["type"] == part["type"]
        && let Some(mut joined) = texts(previous)
    {
        joined.push_str(&addition);
        if part["type"] == "thinking" {
            previous["thinking"] = json!([{"type": "text", "text": joined}]);
            if let Some(closed) = part.get("closed") {
                previous["closed"] = closed.clone();
            }
        } else {
            previous["text"] = json!(joined);
        }
        return;
    }
    content.push(part.clone());
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

/// A number a catalog wrote as a number or a string. Rust parses "NaN",
/// "inf" and "1e999" as floats; none is a price, and a NaN would make every
/// turn's cost NaN.
fn json_number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse::<f64>().ok())
        .filter(|number| number.is_finite())
}

/// A rate in USD per million tokens, once a catalog's unit is brought to it:
/// finite and not below zero. A per-token "1e303" is infinite a million, and
/// a negative rate (a router's `-1` for a price it doesn't know) is none.
fn price(rate: f64) -> Option<f64> {
    (rate.is_finite() && rate >= 0.0).then_some(rate)
}

/// What a catalog entry says its model reads (OpenRouter's `architecture`,
/// or DeepSeek's and xAI's own field), when it says.
fn input_modalities(entry: &Value) -> Option<&Vec<Value>> {
    entry
        .pointer("/architecture/input_modalities")
        .or_else(|| entry.pointer("/modalities/input"))
        // DeepSeek's own listing, also through a Haarness router.
        .or_else(|| entry.get("input_modalities"))
        .and_then(Value::as_array)
}

/// Whether `model` takes `input_audio`. Its catalog entry decides when it
/// lists input modalities. Otherwise only a family known to listen does:
/// Gemini, OpenAI's audio models, Voxtral and the omni models. On Haarness
/// Cloud only `google/` and `openai/` models can: its router sends every
/// other vendor through a route that takes no audio, whatever the model
/// does. A model that takes no audio refuses the whole request, and again on
/// every later turn, so any other model gets a note instead.
fn takes_audio(provider: &str, listed: &HashMap<String, bool>, model: &str) -> bool {
    if provider == "haarness" && !model.starts_with("google/") && !model.starts_with("openai/") {
        return false;
    }
    listed.get(model).copied().unwrap_or_else(|| {
        let name = model
            .rsplit('/')
            .next()
            .unwrap_or(model)
            .to_ascii_lowercase();
        name.starts_with("gemini")
            || name.contains("audio")
            || name.contains("voxtral")
            || name.contains("omni")
    })
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
    // An output limit of 0 asks for none: the model lists none, whatever the
    // catalog or the server says, and its requests carry none.
    if let Some(value) = defaults.max_output_tokens {
        info.max_output_tokens = (value > 0).then_some(value);
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

/// Two user messages in a row become one, for a server the person set up.
/// A chat holds them after a turn that failed before the model answered (the
/// next message follows the unanswered one), and after tool results that
/// brought several files; chat templates that want roles to alternate
/// (Gemma's, Mistral's) refuse every later request of that chat for it.
fn join_adjacent_user_messages(messages: &mut Vec<Value>) {
    let mut joined: Vec<Value> = Vec::with_capacity(messages.len());
    for message in messages.drain(..) {
        if message["role"] == "user"
            && let Some(previous) = joined
                .last_mut()
                .filter(|previous| previous["role"] == "user")
        {
            let parts = |content: Value| match content {
                Value::String(text) if text.is_empty() => Vec::new(),
                Value::String(text) => vec![json!({"type": "text", "text": text})],
                Value::Array(parts) => parts,
                _ => Vec::new(),
            };
            let earlier = previous["content"].take();
            let later = message["content"].clone();
            previous["content"] = match (earlier, later) {
                (Value::String(earlier), Value::String(later)) => {
                    json!(
                        [earlier, later]
                            .into_iter()
                            .filter(|text| !text.is_empty())
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    )
                }
                (earlier, later) => Value::Array([parts(earlier), parts(later)].concat()),
            };
            continue;
        }
        joined.push(message);
    }
    *messages = joined;
}

/// Endpoints and models (`endpoint\0model`) whose chat template takes one
/// tool call per assistant message, learned from its refusal (see
/// [`refuses_parallel_calls`]) for as long as this process runs.
fn one_call_a_message() -> &'static std::sync::Mutex<HashSet<String>> {
    static LEARNED: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> =
        std::sync::OnceLock::new();
    LEARNED.get_or_init(Default::default)
}

/// Whether a server's refusal is a chat template's that takes only one tool
/// call per assistant message: Llama 3.1's, 3.2's and 3.3's raise "This
/// model only supports single tool-calls at once!".
fn refuses_parallel_calls(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("only supports single tool-call")
        || lower.contains("only supports single tool call")
}

/// `body`'s messages with each assistant message that holds several tool
/// calls split into one per call, each followed by its result, the way a
/// model that calls one tool at a time would have written them, and the
/// server asked for one call at a time (`parallel_tool_calls`). The words
/// and reasoning that came with the calls stay with the first. Whether any
/// message held several.
fn one_call_per_message(body: &mut Value) -> bool {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return false;
    };
    let several = |message: &Value| {
        message["role"] == "assistant"
            && message["tool_calls"]
                .as_array()
                .is_some_and(|calls| calls.len() > 1)
    };
    if !messages.iter().any(several) {
        body["parallel_tool_calls"] = json!(false);
        return false;
    }
    let mut split: Vec<Value> = Vec::with_capacity(messages.len() + 4);
    let mut rest = std::mem::take(messages).into_iter().peekable();
    while let Some(message) = rest.next() {
        if !several(&message) {
            split.push(message);
            continue;
        }
        let mut results = Vec::new();
        while let Some(result) = rest.next_if(|next| next["role"] == "tool") {
            results.push(result);
        }
        let calls = message["tool_calls"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for (index, call) in calls.into_iter().enumerate() {
            let mut single = message.clone();
            single["tool_calls"] = json!([call.clone()]);
            if index > 0 {
                if let Some(object) = single.as_object_mut() {
                    object.insert("content".into(), Value::Null);
                    object.remove("reasoning_details");
                    if object.contains_key("reasoning_content") {
                        object.insert("reasoning_content".into(), json!(""));
                    }
                }
            }
            split.push(single);
            if let Some(at) = results
                .iter()
                .position(|result| result["tool_call_id"] == call["id"])
            {
                split.push(results.remove(at));
            }
        }
        // A result whose call is not there (never in a request Haarness
        // writes) still goes, after the others.
        split.extend(results);
    }
    *messages = split;
    body["parallel_tool_calls"] = json!(false);
    true
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
        // A configured output limit of 0 asks for none, whatever a model lists.
        let max_output_tokens = selected
            .and_then(|model| model.max_output_tokens)
            .filter(|_| self.model_defaults.max_output_tokens != Some(0))
            .or(self.model_defaults.max_output_tokens)
            .filter(|limit| *limit > 0);
        // Ollama runs the model with the window Haarness compacts against.
        let ollama_context = selected
            .and_then(|model| model.context_window)
            .filter(|window| *window > 0)
            .unwrap_or(ollama::CONTEXT);
        let supports_reasoning_effort = supports_reasoning
            && (selected.is_some_and(|model| !model.reasoning_efforts.is_empty())
                || self.reasoning_effort_models.contains(&request.model)
                || known_reasoning_effort_model(&self.id, &request.model));
        let structured_output = request.output_schema;
        // A model Ollama runs without tools would answer as if it had read,
        // changed and run things it never saw. Refused in the words Ollama
        // itself once used; the runtime says what to choose instead.
        if self.id == "ollama" && !supports_tools && !request.tools.is_empty() {
            return Err(CoreError::Provider(format!(
                "{} does not support tools",
                request.model
            )));
        }
        if structured_output.is_some() && !supports_tools {
            return Err(CoreError::Provider(format!(
                "model {} does not advertise tool support required for structured output",
                request.model
            )));
        }
        let model = request.model;
        let model_name = selected.map_or(model.as_str(), |model| model.display_name.as_str());
        let replay = self.reasoning_replay(&model);
        let newest = crate::media::newest_unanswered(&request.messages);
        let audio = takes_audio(
            &self.id,
            &self
                .listed_audio
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            &model,
        );
        let mut messages = vec![json!({"role":"system","content":request.system})];
        let mut tool_media = Vec::new();
        for (index, message) in request.messages.into_iter().enumerate() {
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
                    audio,
                    Unreadable::at(index, newest, model_name),
                )?),
                ProviderMessage::ToolCall {
                    call_id,
                    name,
                    arguments,
                    provider_metadata,
                } => {
                    let mut message = json!({"role":"assistant","tool_calls":[{"id":call_id,"type":"function","function":{"name":name,"arguments":serde_json::to_string(&arguments).unwrap_or_else(|_|"{}".into())}}]});
                    // The call's own extra content goes back to the server
                    // and model that attached it (see `EXTRA_CONTENT`).
                    if let Some(extra) = provider_metadata
                        .get(EXTRA_CONTENT)
                        .filter(|extra| extra.is_object())
                        .filter(|_| {
                            let source = &provider_metadata[EXTRA_CONTENT_SOURCE];
                            source["provider"] == self.id.as_str()
                                && source["model"] == model.as_str()
                        })
                    {
                        message["tool_calls"][0][EXTRA_CONTENT] = extra.clone();
                    }
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
                        // Empty too: a server that named the field wants it back.
                        ReasoningReplay::Observed if own => {
                            for field in REASONING_FIELDS {
                                if let Some(reasoning) = provider_metadata[field].as_str() {
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
                    // A server the person set up gets the words the model
                    // wrote before a call in the call's own message, as the
                    // API returns them: chat templates that want roles to
                    // alternate refuse two assistant messages in a row, and
                    // DeepSeek refuses a thinking turn's message that lacks
                    // its reasoning field.
                    if replay == ReasoningReplay::Observed
                        && message["content"].is_null()
                        && let Some(previous) = messages.last().filter(|previous| {
                            previous["role"] == "assistant"
                                && previous.get("tool_calls").is_none()
                                && previous["content"].is_string()
                        })
                    {
                        let words = previous["content"].as_str().unwrap_or_default().to_owned();
                        messages.pop();
                        if !words.trim().is_empty() {
                            message["content"] = json!(words);
                        }
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
                        // A tool's file never refuses the turn: the model
                        // goes on without it.
                        tool_media.push(openai_compatible_media_message(
                            "user",
                            "The preceding tool result includes this binary document or media.",
                            &attachments,
                            supports_images,
                            audio,
                            Unreadable::FromTool,
                        )?);
                    }
                }
            }
        }
        messages.append(&mut tool_media);
        if replay == ReasoningReplay::Observed {
            join_adjacent_user_messages(&mut messages);
        }
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
        // Gemini refuses a few JSON Schema shapes common in MCP tools through
        // a gateway as it does directly (`gemini_compatible`).
        let gemini = crate::tool_schema::gemini_model(&model);
        let forced_gemini = structured_output.is_some() && !native_json && gemini;
        let tools:Vec<_>=requested_tools.into_iter().map(|t|{
            let parameters = if gemini { crate::tool_schema::gemini_compatible(t.input_schema) } else { t.input_schema };
            let parameters = if forced_gemini { crate::gemini::without_max_items(parameters) } else { parameters };
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
            if self.id == "zai" && zai_thinking_switch(&model) {
                body["thinking"] =
                    json!({"type": if effort == "none" { "disabled" } else { "enabled" }});
            } else if self.id == "deepseek" {
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
        let single_calls_key = format!("{}\u{0}{model}", self.endpoint);
        if one_call_a_message()
            .lock()
            .is_ok_and(|learned| learned.contains(&single_calls_key))
        {
            one_call_per_message(&mut body);
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
        if self.id == "lmstudio" {
            tokio::select! {
                () = cancel.cancelled() => return Err(CoreError::Cancelled),
                loaded = self.lmstudio_load(&model) => loaded?,
            }
        }
        // Ollama takes its own API, which says how much of the model's
        // context to run it with (see [`ollama`]).
        let ollama_native = (self.id == "ollama").then(|| ollama::chat_body(&body, ollama_context));
        let builder = match &ollama_native {
            Some(native) => {
                let root = self
                    .endpoint
                    .strip_suffix("/chat/completions")
                    .unwrap_or(&self.endpoint);
                let root = root.strip_suffix("/v1").unwrap_or(root);
                self.client
                    .post(format!("{root}/api/chat"))
                    .headers(headers.clone())
                    .json(native)
            }
            None => self
                .client
                .post(&self.endpoint)
                .headers(headers.clone())
                .json(&body),
        };
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
            let trace = crate::RequestTrace::of(&response);
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
                    trace.refused(status);
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
            && ollama_native.is_none()
            && response.status() == reqwest::StatusCode::BAD_REQUEST
        {
            tracing::info!(provider = %self.id, model = %body["model"], "asking again without forcing the tool call");
            if let Some(object) = body.as_object_mut() {
                object.remove("tool_choice");
            }
            let builder = self
                .client
                .post(&self.endpoint)
                .headers(headers.clone())
                .json(&body);
            response = crate::sse::send(builder, limits).await?;
        }
        // A refusal read here is the stream's first item, as one the stream
        // reads itself would be.
        let mut trace = crate::RequestTrace::of(&response);
        let answer = if !response.status().is_success() && !self.server_output_limit {
            let status = response.status();
            let headers_seen = response.headers().clone();
            let idle = limits.idle(crate::sse::is_loopback(response.url()));
            let text = crate::sse::within(idle, response.text())
                .await?
                .unwrap_or_default();
            // A chat template that takes one tool call per assistant
            // message (Llama 3's, on a server the person runs or behind a
            // gateway) refuses every request once the model made two calls
            // at once: ask again with each call in a message of its own, as
            // every later request to that model will be.
            if matches!(status.as_u16(), 400 | 422 | 500)
                && refuses_parallel_calls(&text)
                && one_call_per_message(&mut body)
            {
                tracing::info!(provider = %self.id, model = %body["model"], "asking again with one tool call a message");
                if let Ok(mut learned) = one_call_a_message().lock() {
                    learned.insert(single_calls_key);
                }
                let builder = self
                    .client
                    .post(&self.endpoint)
                    .headers(headers)
                    .json(&body);
                let response = crate::sse::send(builder, limits).await?;
                trace = crate::RequestTrace::of(&response);
                if response.status().is_success() {
                    Ok(response)
                } else {
                    let status = response.status();
                    let headers_seen = response.headers().clone();
                    trace.refused(status);
                    let text = crate::sse::within(idle, response.text())
                        .await?
                        .unwrap_or_default();
                    Err(crate::sse::status_error(status, &headers_seen, &text))
                }
            // Some servers (older gateways and local runtimes) refuse the
            // usage option instead of ignoring it: ask once more without it.
            } else if matches!(status.as_u16(), 400 | 422)
                && body.get("stream_options").is_some()
                && text.contains("stream_options")
            {
                tracing::info!(provider = %self.id, "asking again without stream_options");
                if let Some(object) = body.as_object_mut() {
                    object.remove("stream_options");
                }
                let builder = self
                    .client
                    .post(&self.endpoint)
                    .headers(headers)
                    .json(&body);
                let response = crate::sse::send(builder, limits).await?;
                trace = crate::RequestTrace::of(&response);
                Ok(response)
            } else if matches!(status.as_u16(), 404 | 405) && missing_route(&text) {
                trace.refused(status);
                Err(CoreError::ProviderUnavailable(format!(
                    "{} has no chat API at {}. Change the provider's base URL to the one in its API documentation: it usually ends in /v1.",
                    self.name,
                    redacted_url(&self.endpoint)
                )))
            } else {
                trace.refused(status);
                Err(crate::sse::status_error(status, &headers_seen, &text))
            }
        } else {
            Ok(response)
        };
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
        let mut events = match answer {
            Ok(response) => {
                if let Some(native) = &ollama_native {
                    let tools = native["tools"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_owned))
                        .collect();
                    ollama::chat_chunks(json_lines_stream(response, cancel, limits), tools)
                } else {
                    json_stream(response, cancel, limits)
                }
            }
            Err(error) => Box::pin(futures::stream::once(std::future::ready(Err(error)))),
        };
        // A Cloud workspace's own router (the Haarness Cloud provider translates its errors itself).
        if self.server_output_limit && self.id != "haarness" {
            events = Box::pin(futures::StreamExt::map(events, |event| {
                event.map_err(crate::haarness_cloud::readable_router)
            }));
        }
        if self.id == "ollama" {
            let model = model.clone();
            events = Box::pin(futures::StreamExt::map(events, move |event| {
                event.map_err(|error| ollama::readable_error(local_model_missing(error, &model)))
            }));
        }
        Ok(Box::pin(try_stream! {
            // Parallel tool calls stream interleaved deltas keyed by index.
            let mut calls: Vec<StreamedCall> = Vec::new();
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
            // A field the server named with nothing in it. DeepSeek in
            // thinking mode streams `"reasoning_content": ""` before a call
            // it did not think about, and refuses the call's next request
            // unless the field comes back, empty or not.
            let mut named_reasoning_field: Option<&'static str> = None;
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
                    let error = CoreError::Provider(message);
                    trace.failed(&error);
                    Err(error)?;
                }
                // A reply sent whole carries `message` where a stream's
                // chunks carry `delta`.
                let whole;
                let delta = match event["choices"][0].get("message") {
                    Some(message) if event["choices"][0].get("delta").is_none() => {
                        whole = whole_reply_delta(message);
                        &whole
                    }
                    _ => &event["choices"][0]["delta"],
                };
                if let Some(text)=delta["content"].as_str().filter(|text| !text.is_empty()){
                    if native_json {
                        if json_output.is_empty() { yield ProviderEvent::ContentStarted; }
                        json_output.push_str(text);
                    } else { yield ProviderEvent::TextDelta(text.into()); }
                }
                if let Some(parts) = delta["content"].as_array() {
                    for part in parts {
                        push_mistral_part(&mut mistral_content, part);
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
                } else if named_reasoning_field.is_none() {
                    named_reasoning_field = REASONING_FIELDS
                        .into_iter()
                        .find(|field| delta.get(*field).is_some_and(Value::is_string));
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
                        let index = streamed_call_slot(&calls, tool);
                        // A call's arguments are held until the stream ends;
                        // the call starts with its first fragment.
                        if calls.len() <= index {
                            calls.resize_with(index + 1, StreamedCall::default);
                            yield ProviderEvent::ContentStarted;
                        }
                        let call = &mut calls[index];
                        if let Some(id)=tool["id"].as_str().filter(|id| !id.is_empty()){call.id = id.into();}
                        if let Some(name)=tool["function"]["name"].as_str().filter(|name| !name.is_empty()){call.name = name.into();}
                        if let Some(args)=tool["function"]["arguments"].as_str(){call.arguments.push_str(args);}
                        if let Some(extra)=tool.get("extra_content").filter(|extra| extra.is_object()){call.extra_content = Some(extra.clone());}
                    }
                }
                // Some gateways put an empty reason on every chunk, the last
                // one after the real reason among them.
                if let Some(reason)=event["choices"][0]["finish_reason"].as_str().filter(|reason| !reason.is_empty()){finish_reason = Some(reason.to_owned());}
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
                    provider_metadata[reasoning_field.or(named_reasoning_field).unwrap_or("reasoning_content")] = json!(reasoning_content);
                    provider_metadata[REASONING_SOURCE] = json!(provider_id);
                }
                // Structured details, when the server sends them, are what it
                // takes back; the plain text would only repeat them. A field
                // it only named goes back empty.
                ReasoningReplay::Observed => if let Some(field) = reasoning_field.or(named_reasoning_field).filter(|_| reasoning_details.is_empty()) {
                    provider_metadata[field] = json!(reasoning_content);
                    provider_metadata[REASONING_SOURCE] = json!(provider_id);
                },
                ReasoningReplay::Never => {}
            }
            if !mistral_content.is_empty() { provider_metadata["mistral_content"] = json!(mistral_content); }
            if provider_metadata.as_object().is_some_and(serde_json::Map::is_empty) { provider_metadata = Value::Null; }
            // A server that ends its stream as it means to, with no finish
            // reason, after tool calls whose arguments all arrived whole made
            // those calls: they run, rather than being closed unrun as a
            // response cut short, which is all the turn could do with them.
            // Words alone stay a response cut short, which is continued.
            if finish_reason.is_none() && !calls.is_empty() && calls.iter().all(StreamedCall::whole) {
                tracing::debug!(provider = %provider_id, "stream ended without a finish reason after whole tool calls");
                finish_reason = Some("tool_calls".into());
            }
            // Report usage before the tool calls, whose arguments may not parse.
            if let Some((input_tokens, output_tokens, cache_read_tokens, cache_write_tokens)) = usage.take() {
                yield ProviderEvent::Usage { input_tokens, output_tokens, cache_read_tokens, cache_write_tokens }; if let Some(usd) = charged.take() { yield ProviderEvent::Cost { usd }; }
            }
            for StreamedCall { id: call_id, name, arguments, extra_content } in calls.drain(..) {
                if !name.is_empty() {
                    // Some servers give a call no ID (Text Generation
                    // Inference, gateways before a model's own API): it gets
                    // one, which its result answers to, instead of failing
                    // the turn as a call nothing could answer.
                    let call_id = if call_id.is_empty() { crate::ollama::call_id() } else { call_id };
                    // What the server attached to this call alone (Gemini's
                    // thought signature) goes back with it, to this server
                    // and model only.
                    let provider_metadata = match extra_content {
                        Some(extra) => {
                            let mut metadata = if provider_metadata.is_object() { provider_metadata.clone() } else { json!({}) };
                            metadata[EXTRA_CONTENT] = extra;
                            metadata[EXTRA_CONTENT_SOURCE] = json!({"provider": provider_id, "model": reasoning_model});
                            metadata
                        }
                        None => provider_metadata.clone(),
                    };
                    // Arguments that do not parse are answered with an error the
                    // model can correct, as a call that fails validation is.
                    let arguments = haarness_core::tool_call_arguments(&arguments);
                    if structured_output.is_some() && name == STRUCTURED_OUTPUT_TOOL_NAME {
                        yield ProviderEvent::StructuredOutput(arguments);
                    } else {
                        yield ProviderEvent::ToolCall{call_id,name,arguments,provider_metadata};
                    }
                }
            }
            if native_json && !json_output.trim().is_empty() {
                let value = serde_json::from_str(&json_output).map_err(|error| CoreError::Provider(format!("OpenAI-compatible provider returned invalid structured output: {error}")))?;
                yield ProviderEvent::StructuredOutput(value);
            }
            let reason = finish_reason.unwrap_or_else(||"incomplete".into());
            crate::log_blocked_finish(&provider_id, &reason, trace.id(), response_id.as_deref());
            yield ProviderEvent::Finished{reason};
            if let Some(error) = broken_after_finish {
                Err::<(), _>(error)?;
            }
        }))
    }
}

/// A message carrying media for Chat Completions. Images a model doesn't
/// read, audio it doesn't take (see [`takes_audio`]) and media the protocol
/// has no portable form for (video, audio other than MP3 or WAV) get what
/// `place` says: the newest message is refused and anything else becomes a
/// note.
fn openai_compatible_media_message(
    role: &str,
    content: &str,
    attachments: &[String],
    images: bool,
    audio: bool,
    place: Unreadable<'_>,
) -> CoreResult<Value> {
    // A photo sent on its own has no text. Claude refuses a blank text block,
    // including through a router that passes the request on to Anthropic, so
    // blank text is left out (as the native adapters do).
    let mut blocks = Vec::new();
    if !content.trim().is_empty() {
        blocks.push(json!({"type":"text","text":content}));
    }
    for item in crate::media::load_all(attachments)? {
        let format = match item.mime_type.as_str() {
            "audio/mpeg" => Some("mp3"),
            "audio/wav" => Some("wav"),
            _ => None,
        }
        .filter(|_| audio);
        if item.is_image() && images {
            blocks.push(json!({"type":"image_url","image_url":{"url":item.data_url()}}));
        } else if item.is_pdf() {
            blocks.push(json!({
                "type":"file",
                "file":{"filename":item.filename,"file_data":item.data_url()}
            }));
        } else if let Some(format) = format {
            blocks.push(json!({
                "type":"input_audio",
                "input_audio":{"data":item.base64,"format":format}
            }));
        } else if audio
            && item.is_audio()
            && let Unreadable::Refuse(model) = place
        {
            return Err(CoreError::Provider(format!(
                "Haarness can send {model} audio only as MP3 or WAV. Send the message without the recording, or with it as MP3 or WAV."
            )));
        } else {
            blocks.push(json!({"type":"text","text":place.stand_in(&item)?}));
        }
    }
    Ok(json!({"role":role,"content":blocks}))
}
/// A tool call as its stream's fragments build it.
#[derive(Debug, Default)]
struct StreamedCall {
    id: String,
    name: String,
    arguments: String,
    /// What the server attached to this call alone (`extra_content`).
    extra_content: Option<Value>,
}

impl StreamedCall {
    /// Whether the call arrived whole: named, with its arguments one JSON
    /// object.
    fn whole(&self) -> bool {
        !self.name.is_empty()
            && serde_json::from_str::<Value>(&self.arguments).is_ok_and(|value| value.is_object())
    }
}

/// Which call a tool-call fragment belongs to. A fragment names its call by
/// `index`, as OpenAI's API does; Gemini's OpenAI-compatible API sends each
/// call of a batch whole and without one. Then a new ID starts a call, an
/// ID already seen continues its own, and a fragment naming neither
/// continues the latest call.
fn streamed_call_slot(calls: &[StreamedCall], fragment: &Value) -> usize {
    if let Some(index) = fragment["index"].as_u64() {
        return usize::try_from(index).unwrap_or(0);
    }
    let Some(last) = calls.last() else {
        return 0;
    };
    let latest = calls.len() - 1;
    match fragment["id"].as_str().filter(|id| !id.is_empty()) {
        Some(id) => calls
            .iter()
            .position(|call| call.id == id)
            // A call begun by a fragment that named no ID yet.
            .unwrap_or(if last.id.is_empty() {
                latest
            } else {
                calls.len()
            }),
        None => latest,
    }
}

/// A whole reply's message as one chunk's delta: each tool call gets the
/// index a stream would have given it.
fn whole_reply_delta(message: &Value) -> Value {
    let mut delta = message.clone();
    if let Some(calls) = delta.get_mut("tool_calls").and_then(Value::as_array_mut) {
        for (index, call) in calls.iter_mut().enumerate() {
            if call.get("index").is_none() {
                call["index"] = json!(index);
            }
        }
    }
    delta
}

/// Whether a 404 or 405 body says the address has no such route, rather
/// than that a model is missing (`The model ... does not exist`, `No
/// endpoints found for ...`), which the provider's own words explain.
fn missing_route(body: &str) -> bool {
    let Some(detail) = crate::sse::provider_error_detail(body) else {
        // Nothing, or a web server's HTML page.
        return true;
    };
    let lower = detail.to_ascii_lowercase();
    !lower.contains("model")
        && !lower.contains("endpoints found")
        && [
            "not found",
            "unknown request url",
            "invalid url",
            "unknown url",
            "no route",
            "cannot post",
            "unrecognized request url",
            "method not allowed",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
}

/// `url` without credentials or a query, fit to show.
fn redacted_url(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(mut url) => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        }
        Err(_) => url.to_owned(),
    }
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
        CoreError, ModelProvider, ProviderEvent, ProviderMessage, ProviderRequest, ToolDefinition,
    };

    use crate::auth::{Credential, CredentialStore, XaiOAuthClient, XaiOAuthConfig};

    use super::{
        CompatibleInstance, CompatibleModelDefaults, ListedAudio, OpenAiCompatibleProvider,
        Unreadable, append_reasoning_details, apply_lmstudio_details, apply_ollama_details,
        apply_ollama_details_chosen, deepseek_charge, deepseek_peak, discovered_model_info,
        local_model_missing, model_info, openai_compatible_media_message, push_mistral_part,
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
    fn groq_names_leave_out_the_vendor_prefix() {
        let info = discovered_model_info(
            "groq",
            &json!({"id": "qwen/qwen3.8-27b", "name": "Qwen/Qwen3.8-27B"}),
            CompatibleModelDefaults::default(),
        )
        .unwrap();
        assert_eq!(info.display_name, "Qwen3.8-27B");
    }

    #[test]
    fn a_price_that_is_not_a_finite_number_is_no_price() {
        let price = |prompt: &str, cache_read: &str| {
            discovered_model_info(
                "openrouter",
                &json!({"id": "vendor/model", "pricing": {
                    "prompt": prompt, "completion": "0.000002", "input_cache_read": cache_read
                }}),
                CompatibleModelDefaults::default(),
            )
            .unwrap()
            .pricing
        };
        // "1e303" a token is finite, but infinite a million.
        for broken in ["NaN", "inf", "-inf", "1e999", "1e303", "-0.5"] {
            assert!(price(broken, "0").is_none(), "{broken}");
            // A broken cache rate is no rate, not a NaN in every cost.
            let pricing = price("0.000001", broken).unwrap();
            assert!(pricing.cache_read.abs() < 1e-12, "{broken}");
            assert!((pricing.input - 1.0).abs() < 1e-9);
        }
        // xAI and DeepInfra list their own units; a negative rate is none.
        let xai = discovered_model_info(
            "xai",
            &json!({"id": "grok-fixture", "prompt_text_token_price": -12500,
                "completion_text_token_price": 25000}),
            CompatibleModelDefaults::default(),
        )
        .unwrap();
        assert!(xai.pricing.is_none(), "{:?}", xai.pricing);
        let deepinfra = discovered_model_info(
            "deepinfra",
            &json!({"id": "vendor/fixture", "metadata": {"tags": ["chat"], "pricing": {
                "input_tokens": 0.2, "output_tokens": -1, "cache_read_tokens": 0.1}}}),
            CompatibleModelDefaults::default(),
        )
        .unwrap();
        assert!(deepinfra.pricing.is_none(), "{:?}", deepinfra.pricing);
    }

    #[test]
    fn groq_speech_models_stay_out_of_the_picker() {
        let model = |id: &str, output: Value| {
            discovered_model_info(
                "groq",
                &json!({"id": id, "output_modalities": output}),
                CompatibleModelDefaults::default(),
            )
        };
        assert!(model("canopylabs/orpheus-v1-english", json!(["speech"])).is_none());
        assert!(model("openai/gpt-oss-120b", json!(["text"])).is_some());
    }

    #[test]
    fn mistral_aliases_take_the_levels_and_price_of_the_model_they_name() {
        let listed = |id: &str, aliases: &[&str]| {
            discovered_model_info(
                "mistral",
                &json!({
                    "id": id,
                    "name": "mistral-medium-latest",
                    "aliases": aliases,
                    "capabilities": {"completion_chat": true, "function_calling": true, "reasoning": true, "vision": true}
                }),
                CompatibleModelDefaults::default(),
            )
            .unwrap()
        };
        // Mistral's list as of 2026-11-01: the latest alias is Medium 3.5.
        let latest = listed(
            "mistral-medium-latest",
            &[
                "mistral-medium",
                "mistral-medium-3-5",
                "mistral-medium-2604",
            ],
        );
        assert!(latest.reasoning);
        assert_eq!(latest.reasoning_efforts, ["none", "high"]);
        assert_eq!(latest.pricing.map(|price| price.input), Some(1.5));
        // The dotted spelling has its own levels and reads as reasoning.
        let dotted = listed("mistral-medium-3.5", &["mistral-medium-3-5"]);
        assert!(dotted.reasoning);
        assert_eq!(dotted.reasoning_efforts, ["none", "high"]);
        // An alias of a model with no known levels keeps none.
        let large = listed("mistral-large-latest", &["mistral-large-2512"]);
        assert!(large.reasoning_efforts.is_empty());
    }

    #[test]
    fn mistral_names_are_ids_not_labels() {
        let named = |id: &str, name: &str| {
            discovered_model_info(
                "mistral",
                &json!({"id": id, "name": name, "capabilities": {"completion_chat": true}}),
                CompatibleModelDefaults::default(),
            )
            .unwrap()
            .display_name
        };
        assert_eq!(
            named("mistral-large-4-0", "mistral-large-4"),
            "Mistral Large 4"
        );
        assert_eq!(
            named("mistral-small-latest", "mistral-small-2603"),
            "Mistral Small 4"
        );
        // An alias keeps its own id rather than its target's.
        assert_eq!(
            named("magistral-small-latest", "mistral-small-2603"),
            "magistral-small-latest"
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

    /// Groq's list as it reads today (2026-11-09): ALLaM takes no tools, so
    /// a chat with it is sent without them instead of refused ("`tool
    /// calling` is not supported with this model"); Qwen reasons.
    #[test]
    fn groq_says_which_models_take_tools() {
        let entry = |id: &str, features: &[&str]| {
            json!({"id": id, "object": "model", "owned_by": "Groq", "active": true,
                "context_window": 131_072, "max_completion_tokens": 16_384,
                "input_modalities": ["text"], "output_modalities": ["text"],
                "supported_sampling_parameters": ["temperature", "top_p", "stop", "seed", "max_tokens"],
                "supported_features": features})
        };
        let model = |entry: Value| {
            discovered_model_info("groq", &entry, CompatibleModelDefaults::default()).unwrap()
        };
        let allam = model(entry("allam-2-7b", &["json_mode"]));
        assert!(!allam.tools);
        assert!(!allam.reasoning);
        let qwen = model(entry(
            "qwen/qwen3.8-27b",
            &["tools", "json_mode", "reasoning"],
        ));
        assert!(qwen.tools);
        assert!(qwen.reasoning);
        let gpt_oss = model(entry(
            "openai/gpt-oss-120b",
            &["tools", "json_mode", "structured_outputs", "reasoning"],
        ));
        assert!(gpt_oss.tools);
        assert!(gpt_oss.reasoning);
        assert_eq!(gpt_oss.reasoning_efforts, ["low", "medium", "high"]);
        // A list that says nothing of features keeps the default.
        assert!(model(json!({"id": "vendor/chat"})).tools);
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
                false,
                Unreadable::Refuse("fixture"),
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
            false,
            Unreadable::Refuse("fixture"),
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
            true,
            Unreadable::Earlier,
        )
        .unwrap();
        assert_eq!(message["content"][1]["type"], "file");
        assert_eq!(message["content"][1]["file"]["filename"], "agenda.pdf");
        assert_eq!(message["content"][2]["type"], "input_audio");
        assert_eq!(message["content"][2]["input_audio"]["format"], "mp3");
        // A model that takes no audio gets a note, and the newest message is
        // refused.
        let message = openai_compatible_media_message(
            "user",
            "Review the meeting",
            &["data:audio/mpeg;name=meeting.mp3;base64,SUQz".into()],
            false,
            false,
            Unreadable::FromTool,
        )
        .unwrap();
        assert_eq!(
            message["content"][1]["text"],
            "[Audio meeting.mp3 is left out, because it can't be sent to you. Don't guess what it says.]"
        );
        let error = openai_compatible_media_message(
            "user",
            "Review the meeting",
            &["data:audio/mpeg;name=meeting.mp3;base64,SUQz".into()],
            false,
            false,
            Unreadable::Refuse("Claude Sonnet 5.5"),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Haarness can't send audio to Claude Sonnet 5.5."),
            "{error}"
        );

        let error = openai_compatible_media_message(
            "user",
            "Watch this",
            &["data:video/mp4;base64,AAAAIGZ0eXA=".into()],
            true,
            true,
            Unreadable::Refuse("Fixture 7B"),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Haarness can't send video to Fixture 7B."),
            "{error}"
        );
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
    fn lmstudio_details_name_a_model_and_its_room() {
        let entry = |loaded: Option<u64>| {
            json!({
                "type": "llm",
                "key": "qwen3-coder-30b-a3b-instruct",
                "display_name": "Qwen3 Coder 30B A3B Instruct",
                "max_context_length": 262_144,
                "capabilities": {"vision": false, "trained_for_tool_use": false},
                "loaded_instances": loaded.map_or_else(Vec::new, |context| {
                    vec![json!({"id": "qwen3-coder-30b-a3b-instruct", "config": {"context_length": context}})]
                }),
            })
        };
        let mut model = model_info(
            "lmstudio",
            "qwen3-coder-30b-a3b-instruct",
            CompatibleModelDefaults::default(),
        );
        apply_lmstudio_details(&mut model, &entry(None));
        assert_eq!(model.display_name, "Qwen3 Coder 30B A3B Instruct");
        assert!(!model.images);
        assert!(model.tools, "LM Studio prompts any model for tools");
        assert_eq!(
            model.context_window,
            Some(65_536),
            "what Haarness loads it with"
        );
        // Loaded roomier by the person: that is what it has.
        apply_lmstudio_details(&mut model, &entry(Some(131_072)));
        assert_eq!(model.context_window, Some(131_072));
        // Loaded too small: Haarness loads it again before a request.
        apply_lmstudio_details(&mut model, &entry(Some(8_192)));
        assert_eq!(model.context_window, Some(65_536));
        // Its reasoning levels, as LM Studio names them.
        let mut thinking = entry(None);
        thinking["capabilities"]["reasoning"] =
            json!({"allowed_options": ["low", "medium", "high"], "default": "low"});
        apply_lmstudio_details(&mut model, &thinking);
        assert!(model.reasoning);
        assert_eq!(model.reasoning_efforts, ["low", "medium", "high"]);
        thinking["capabilities"]["reasoning"] =
            json!({"allowed_options": ["off", "low", "medium", "xhigh", "on"], "default": "xhigh"});
        apply_lmstudio_details(&mut model, &thinking);
        assert_eq!(model.reasoning_efforts, ["none", "low", "medium", "xhigh"]);
        // A model whose longest is short keeps it.
        let mut small = entry(None);
        small["max_context_length"] = json!(16_384);
        apply_lmstudio_details(&mut model, &small);
        assert_eq!(model.context_window, Some(16_384));
    }

    #[tokio::test]
    async fn lmstudio_loads_a_model_with_room_before_the_request() {
        #[derive(Clone, Default)]
        struct Calls(Arc<Mutex<Vec<String>>>);
        let calls = Calls::default();
        let router = Router::new()
            .route(
                "/api/v1/models",
                get(|State(calls): State<Calls>| async move {
                    calls.0.lock().push("list".into());
                    Json(json!({"models": [{
                        "type": "llm",
                        "key": "compat-test",
                        "max_context_length": 262_144,
                        "loaded_instances": [{"id": "compat-test", "config": {"context_length": 8_192}}],
                    }]}))
                }),
            )
            .route(
                "/api/v1/models/unload",
                post(|State(calls): State<Calls>, Json(body): Json<Value>| async move {
                    calls.0.lock().push(format!("unload {}", body["instance_id"]));
                    Json(json!({}))
                }),
            )
            .route(
                "/api/v1/models/load",
                post(|State(calls): State<Calls>, Json(body): Json<Value>| async move {
                    calls.0.lock().push(format!("load {} {}", body["model"], body["context_length"]));
                    Json(json!({"status": "loaded"}))
                }),
            )
            .route(
                "/v1/chat/completions",
                post(|State(calls): State<Calls>| async move {
                    calls.0.lock().push("chat".into());
                    (
                        [(CONTENT_TYPE, "text/event-stream")],
                        "data: {\"choices\":[{\"delta\":{\"content\":\"ready\"},\"finish_reason\":\"stop\"}]}\n\n",
                    )
                }),
            )
            .with_state(calls.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let mut provider =
            OpenAiCompatibleProvider::for_test(format!("http://{address}/v1/chat/completions"));
        provider.id = "lmstudio".into();
        let request = ProviderRequest {
            model: "compat-test".into(),
            system: "sys".into(),
            messages: vec![ProviderMessage::Text {
                role: "user".into(),
                content: "go".into(),
            }],
            tools: vec![],
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        };
        let events: Vec<_> = provider
            .stream(request.clone(), CancellationToken::new())
            .await
            .unwrap()
            .collect()
            .await;
        assert!(events.iter().all(Result::is_ok), "{events:?}");
        assert_eq!(
            *calls.0.lock(),
            [
                "list",
                "unload \"compat-test\"",
                "load \"compat-test\" 65536",
                "chat"
            ]
        );
        // LM Studio would answer a model it lacks with the loaded one.
        let mut missing = request.clone();
        missing.model = "qwen/qwen3-8b".into();
        let Err(error) = provider.stream(missing, CancellationToken::new()).await else {
            panic!("a model LM Studio lacks must be refused");
        };
        assert!(
            error
                .to_string()
                .contains("LM Studio has no model qwen/qwen3-8b"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn ollama_runs_a_model_with_the_window_its_chat_compacts_against() {
        // Ollama's OpenAI-style endpoint runs a model with the server's
        // default context, 4K on a small GPU, and cuts the front of a longer
        // prompt: Haarness's instructions and tools. Its own API is asked
        // for the window instead, and to refuse a prompt that won't fit.
        #[derive(Clone, Default)]
        struct Calls(Arc<Mutex<Vec<Value>>>);
        let calls = Calls::default();
        let router = Router::new()
            .route(
                "/api/chat",
                post(|State(calls): State<Calls>, Json(body): Json<Value>| async move {
                    let long = body["messages"][1]["content"] == "a long chat";
                    calls.0.lock().push(body);
                    if long {
                        return (
                            axum::http::StatusCode::BAD_REQUEST,
                            [(CONTENT_TYPE, "application/json")],
                            "{\"error\":\"request (40112 tokens) exceeds the available context size (32768 tokens), try increasing it\"}".to_owned(),
                        );
                    }
                    // Qwen2.5 Coder writes its call as text.
                    (
                        axum::http::StatusCode::OK,
                        [(CONTENT_TYPE, "application/x-ndjson")],
                        [
                            json!({"model": "qwen2.5-coder:7b", "message": {"role": "assistant", "content": "{\"name\": \"read\", "}, "done": false}),
                            json!({"model": "qwen2.5-coder:7b", "message": {"role": "assistant", "content": "\"arguments\": {\"path\": \"calc.py\"}}"}, "done": false}),
                            json!({"model": "qwen2.5-coder:7b", "message": {"role": "assistant", "content": ""}, "done": true, "done_reason": "stop", "prompt_eval_count": 12_597, "eval_count": 21}),
                        ]
                        .iter()
                        .fold(String::new(), |mut body, line| {
                            body.push_str(&line.to_string());
                            body.push('\n');
                            body
                        }),
                    )
                }),
            )
            .route(
                "/v1/chat/completions",
                post(|State(calls): State<Calls>| async move {
                    calls.0.lock().push(json!("openai-style"));
                    (
                        [(CONTENT_TYPE, "text/event-stream")],
                        "data: {\"choices\":[{\"delta\":{\"content\":\"I'm ready to help!\"},\"finish_reason\":\"stop\"}]}\n\n",
                    )
                }),
            )
            .with_state(calls.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let mut coder = model_info(
            "ollama",
            "qwen2.5-coder:7b",
            CompatibleModelDefaults::default(),
        );
        coder.context_window = Some(32_768);
        let mut provider =
            OpenAiCompatibleProvider::for_test(format!("http://{address}/v1/chat/completions"))
                .with_model_info(vec![coder]);
        provider.id = "ollama".into();
        let mut request = ProviderRequest {
            model: "qwen2.5-coder:7b".into(),
            system: "You are Haarness.".into(),
            messages: vec![ProviderMessage::Text {
                role: "user".into(),
                content: "Read calc.py".into(),
            }],
            tools: vec![ToolDefinition {
                name: "read".into(),
                description: "Read a file".into(),
                input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            }],
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        };
        let events: Vec<_> = provider
            .stream(request.clone(), CancellationToken::new())
            .await
            .unwrap()
            .collect()
            .await;
        let events: Vec<_> = events.into_iter().map(Result::unwrap).collect();
        {
            let calls = calls.0.lock();
            assert_eq!(calls.len(), 1, "{calls:?}");
            assert_eq!(calls[0]["options"]["num_ctx"], 32_768);
            assert_eq!(calls[0]["truncate"], false);
            assert_eq!(calls[0]["shift"], false);
            assert_eq!(calls[0]["tools"][0]["function"]["name"], "read");
        }
        assert!(
            events.iter().any(|event| matches!(event, ProviderEvent::ToolCall { name, arguments, .. } if name == "read" && arguments["path"] == "calc.py")),
            "{events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ProviderEvent::TextDelta(_))),
            "{events:?}"
        );
        assert!(events.iter().any(|event| matches!(
            event,
            ProviderEvent::Usage {
                input_tokens: 12_597,
                output_tokens: 21,
                ..
            }
        )));
        assert!(events.iter().any(
            |event| matches!(event, ProviderEvent::Finished { reason } if reason == "tool_calls")
        ));
        // A model Haarness knows nothing of still gets room.
        request.model = "unlisted:latest".into();
        let _: Vec<_> = provider
            .stream(request.clone(), CancellationToken::new())
            .await
            .unwrap()
            .collect()
            .await;
        assert_eq!(
            calls.0.lock()[1]["options"]["num_ctx"],
            super::ollama::CONTEXT
        );
        // A prompt over the window is refused in words compaction answers.
        request.model = "qwen2.5-coder:7b".into();
        request.messages = vec![ProviderMessage::Text {
            role: "user".into(),
            content: "a long chat".into(),
        }];
        let error = match provider.stream(request, CancellationToken::new()).await {
            Err(error) => error,
            Ok(stream) => stream
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .find_map(Result::err)
                .expect("an error"),
        };
        assert!(
            error
                .to_string()
                .contains("exceeds the available context size (32768 tokens)"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_tool_call_ollama_could_not_read_ends_the_stream_as_one() {
        // gpt-oss on Ollama 0.40.6 reasons, then writes a call to `write`
        // that the reply's full context cuts off: Ollama ends the stream
        // with its parser's error, the file so far quoted in it.
        let router = Router::new().route(
            "/api/chat",
            post(|| async {
                (
                    [(CONTENT_TYPE, "application/x-ndjson")],
                    [
                        json!({"model": "gpt-oss:20b", "message": {"role": "assistant", "content": "", "thinking": "Write the game."}, "done": false}),
                        json!({"error": "error parsing tool call: raw='{\"content\":\"import curses\\nimport random\\n', err=unexpected end of JSON input"}),
                    ]
                    .iter()
                    .fold(String::new(), |mut body, line| {
                        body.push_str(&line.to_string());
                        body.push('\n');
                        body
                    }),
                )
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let mut provider =
            OpenAiCompatibleProvider::for_test(format!("http://{address}/v1/chat/completions"));
        provider.id = "ollama".into();
        let request = ProviderRequest {
            model: "gpt-oss:20b".into(),
            system: "You are Haarness.".into(),
            messages: vec![ProviderMessage::Text {
                role: "user".into(),
                content: "Write snake.py".into(),
            }],
            tools: vec![ToolDefinition {
                name: "write".into(),
                description: "Write a file".into(),
                input_schema: json!({"type": "object", "properties": {"content": {"type": "string"}}}),
            }],
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        };
        let events: Vec<_> = provider
            .stream(request, CancellationToken::new())
            .await
            .unwrap()
            .collect()
            .await;
        assert!(
            matches!(&events[0], Ok(ProviderEvent::ReasoningDelta(text)) if text == "Write the game."),
            "{events:?}"
        );
        let error = events
            .into_iter()
            .find_map(Result::err)
            .expect("an error")
            .to_string();
        assert!(
            error.contains(&format!(
                "{}error parsing tool call: unexpected end of JSON input",
                haarness_core::UNREADABLE_TOOL_CALL
            )) && !error.contains("import curses"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn lmstudio_short_of_memory_loads_the_least_that_works() {
        #[derive(Clone, Default)]
        struct Calls(Arc<Mutex<Vec<String>>>);
        let calls = Calls::default();
        let router = Router::new()
            .route(
                "/api/v1/models",
                get(|| async {
                    Json(json!({"models": [{
                        "type": "llm",
                        "key": "qwen/qwen3-8b",
                        "max_context_length": 40_960,
                        "loaded_instances": [],
                    }]}))
                }),
            )
            .route(
                "/api/v1/models/load",
                post(|State(calls): State<Calls>, Json(body): Json<Value>| async move {
                    let context = body["context_length"].as_u64().unwrap_or(0);
                    calls.0.lock().push(format!("load {context}"));
                    if context > 32_768 {
                        return (
                            axum::http::StatusCode::BAD_REQUEST,
                            Json(json!({"error": {"message": "Model loading was stopped due to insufficient system resources."}})),
                        );
                    }
                    (axum::http::StatusCode::OK, Json(json!({"status": "loaded"})))
                }),
            )
            .route(
                "/v1/chat/completions",
                post(|State(calls): State<Calls>| async move {
                    calls.0.lock().push("chat".into());
                    (
                        [(CONTENT_TYPE, "text/event-stream")],
                        "data: {\"choices\":[{\"delta\":{\"content\":\"ready\"},\"finish_reason\":\"stop\"}]}\n\n",
                    )
                }),
            )
            .with_state(calls.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let mut provider =
            OpenAiCompatibleProvider::for_test(format!("http://{address}/v1/chat/completions"));
        provider.id = "lmstudio".into();
        let request = ProviderRequest {
            model: "qwen/qwen3-8b".into(),
            system: "sys".into(),
            messages: vec![ProviderMessage::Text {
                role: "user".into(),
                content: "go".into(),
            }],
            tools: vec![],
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        };
        let events: Vec<_> = provider
            .stream(request, CancellationToken::new())
            .await
            .unwrap()
            .collect()
            .await;
        assert!(events.iter().all(Result::is_ok), "{events:?}");
        // Its 40K is refused; 32K, the least Haarness's instructions and
        // tools leave room with, loads.
        assert_eq!(*calls.0.lock(), ["load 40960", "load 32768", "chat"]);
    }

    #[test]
    fn a_new_chat_on_this_computer_gets_a_model_that_can_do_the_work() {
        use super::{billions_of_parameters, local_rank};
        assert_eq!(billions_of_parameters("1.7B"), Some(1.7));
        assert_eq!(billions_of_parameters("135M"), Some(0.135));
        assert_eq!(billions_of_parameters("30B-A3B"), Some(30.0));
        assert_eq!(
            billions_of_parameters("116829156672").map(f64::round),
            Some(117.0)
        );
        assert_eq!(billions_of_parameters("unknown"), None);
        let model = |id: &str, tools: bool, window: u64| {
            let mut model = model_info("ollama", id, CompatibleModelDefaults::default());
            model.tools = tools;
            model.context_window = Some(window);
            model
        };
        // In the catalog's order, as this Mac's Ollama listed them.
        let mut models = [
            model("qwen2.5-coder:7b", true, 32_768),
            model("gemma3:latest", false, 32_768),
            model("gpt-oss:120b-cloud", true, 32_768),
            model("llama3.2:latest", true, 32_768),
            model("qwen3:8b", true, 32_768),
            model("smollm2:1.7b", true, 8_192),
            model("tinyllama:1.1b", true, 32_768),
        ];
        let weak: std::collections::HashSet<String> = [
            "llama3.2:latest".to_owned(),
            "smollm2:1.7b".to_owned(),
            "tinyllama:1.1b".to_owned(),
        ]
        .into();
        models.sort_by_key(|model| local_rank(model, &weak));
        let order: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(
            order,
            [
                "qwen3:8b",
                // Pulled last, so listed first, but it writes its calls as text.
                "qwen2.5-coder:7b",
                "llama3.2:latest",
                "tinyllama:1.1b",
                "smollm2:1.7b",
                "gpt-oss:120b-cloud",
                "gemma3:latest"
            ]
        );
    }

    #[test]
    fn a_model_ollama_lacks_says_how_to_get_it() {
        let missing = local_model_missing(
            CoreError::Provider("HTTP 404 Not Found: model 'llama3.2:1b' not found".into()),
            "llama3.2:1b",
        );
        assert_eq!(
            missing.to_string(),
            CoreError::Provider("HTTP 404 Not Found: model 'llama3.2:1b' not found. Download it with `ollama pull llama3.2:1b`, or pick a model you have.".into()).to_string()
        );
        let other = local_model_missing(CoreError::Provider("HTTP 500".into()), "llama3.2:1b");
        assert_eq!(
            other.to_string(),
            CoreError::Provider("HTTP 500".into()).to_string()
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
        // The window Haarness runs it with, not the 256K it could take:
        // what it asks Ollama for, and what the chat compacts against.
        assert_eq!(qwen.context_window, Some(32_768));
        // More when the person chose more for Ollama, up to the model's own.
        apply_ollama_details_chosen(
            &mut qwen,
            &json!({"capabilities": ["completion", "tools"], "model_info": {"general.architecture": "qwen3_5", "qwen3_5.context_length": 262_144}}),
            Some(131_072),
        );
        assert_eq!(qwen.context_window, Some(131_072));
        let mut oss = model_info("ollama", "gpt-oss:20b", CompatibleModelDefaults::default());
        apply_ollama_details(
            &mut oss,
            &json!({"capabilities": ["completion", "tools", "thinking"]}),
        );
        assert_eq!(oss.reasoning_efforts, ["low", "medium", "high"]);
        assert!(!oss.images);
        // The levels Ollama lists for a model, when it lists them.
        let mut listed = model_info("ollama", "some-thinker", CompatibleModelDefaults::default());
        apply_ollama_details(
            &mut listed,
            &json!({"capabilities": ["completion", "tools", "thinking"], "thinking": {"values": ["low", "medium", "high"], "default": "medium"}}),
        );
        assert_eq!(listed.reasoning_efforts, ["low", "medium", "high"]);
        let mut switch = model_info("ollama", "qwen3:8b", CompatibleModelDefaults::default());
        apply_ollama_details(
            &mut switch,
            &json!({"capabilities": ["completion", "tools", "thinking"], "thinking": {"values": [false, true], "default": true}, "model_info": {"general.architecture": "qwen3", "qwen3.context_length": 40_960}}),
        );
        assert_eq!(switch.reasoning_efforts, ["none", "low", "medium", "high"]);
        assert_eq!(switch.context_window, Some(32_768));
        // A model shorter than that runs with all it has.
        let mut short = model_info("ollama", "llama3:8b", CompatibleModelDefaults::default());
        apply_ollama_details(
            &mut short,
            &json!({"capabilities": ["completion"], "model_info": {"general.architecture": "llama", "llama.context_length": 8_192}}),
        );
        assert_eq!(short.context_window, Some(8_192));
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

    /// Gemini through a gateway refuses the JSON Schema shapes it refuses
    /// directly; other models get the tool's schema as it is.
    #[tokio::test]
    async fn gemini_through_a_gateway_gets_tool_schemas_it_takes() {
        let (provider, state) = openrouter_fixture(post(completions_recording_headers)).await;
        let schema = json!({"type":"object","properties":{
            "pair":{"type":"array","items":[{"type":"string"},{"type":"integer"}]}
        },"required":["pair","missing"]});
        for model in ["google/gemini-3.8-flash", "anthropic/claude-haiku-5.5"] {
            let mut request = short_request();
            request.model = model.into();
            request.tools = vec![haarness_core::ToolDefinition {
                name: "store".into(),
                description: "store a pair".into(),
                input_schema: schema.clone(),
            }];
            let _ = provider
                .stream(request, CancellationToken::new())
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await;
        }
        let requests = state.0.lock();
        assert_eq!(
            requests[0]["tools"][0]["function"]["parameters"],
            json!({"type":"object","properties":{
                "pair":{"type":"array","prefixItems":[{"type":"string"},{"type":"integer"}]},
                "missing":{}
            },"required":["pair","missing"]})
        );
        assert_eq!(requests[1]["tools"][0]["function"]["parameters"], schema);
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
        ) -> (
            axum::http::StatusCode,
            [(&'static str, &'static str); 1],
            Json<Value>,
        ) {
            state.0.lock().push(request);
            (
                axum::http::StatusCode::PAYMENT_REQUIRED,
                [("x-request-id", "or-credits-31")],
                Json(
                    json!({"error":{"code":402,"message":"This request requires more credits, or fewer max_tokens. You requested up to 65536 tokens, but can only afford 900."}}),
                ),
            )
        }
        let (provider, state) = openrouter_fixture(post(nearly_empty)).await;
        let (logged, _guard) = crate::log_capture::Logged::start();
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
        assert!(!message.contains("or-credits-31"), "{message}");
        assert_eq!(state.0.lock().len(), 1);
        // The refusal is named in the log by its request id.
        let lines = logged.lines("provider refused the request");
        assert_eq!(lines.len(), 1, "{}", logged.all());
        assert!(
            lines[0].contains(" status=402") && lines[0].contains(" request_id=or-credits-31"),
            "{}",
            lines[0]
        );
    }

    /// A call whose arguments are not valid JSON (ByteDance Seed wrote a
    /// regex's `\.` unescaped) reaches the runtime as a call it refuses to
    /// run, not as a provider error that ends the turn.
    #[tokio::test]
    async fn arguments_that_are_not_json_still_make_a_call() {
        let request = ProviderRequest {
            model: "compat-test".into(),
            system: "sys".into(),
            messages: vec![ProviderMessage::Text {
                role: "user".into(),
                content: "find it".into(),
            }],
            tools: vec![],
            reasoning_effort: None,
            output_schema: None,
            parameters: haarness_core::RequestParameters::default(),
        };
        let base_url = cut_after(vec![
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"bash","arguments":"{\"command\":\"grep -r \\. src\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        ])
        .await;
        let events = OpenAiCompatibleProvider::new(CompatibleInstance {
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
        .stream(request, CancellationToken::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
        let call = events.iter().find_map(|event| match event {
            Ok(ProviderEvent::ToolCall {
                name, arguments, ..
            }) => Some((name.clone(), arguments.clone())),
            _ => None,
        });
        let (name, arguments) = call.unwrap_or_else(|| panic!("no tool call: {events:?}"));
        assert_eq!(name, "bash");
        assert_eq!(
            arguments,
            haarness_core::tool_call_arguments(r#"{"command":"grep -r \. src"}"#)
        );
        assert!(arguments.is_object(), "replayable as an object");
        // The response finishes; this stand-in then cuts the connection.
        let finished = events
            .iter()
            .position(|event| matches!(event, Ok(ProviderEvent::Finished { reason }) if reason == "tool_calls"))
            .unwrap_or_else(|| panic!("no finish: {events:?}"));
        assert!(events[..finished].iter().all(Result::is_ok), "{events:?}");
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
    async fn a_text_only_model_refuses_only_the_newest_picture() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/chat/completions", post(completions))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let mut provider =
            OpenAiCompatibleProvider::for_test(format!("http://{address}/chat/completions"));
        provider.models[0].images = false;
        provider.models[0].display_name = "Coder 30B".into();
        let picture = || vec!["data:image/png;name=shot.png;base64,iVBORw0KGgo=".to_owned()];
        let text = |role: &str, content: &str| ProviderMessage::Text {
            role: role.into(),
            content: content.into(),
        };
        let request = |messages| ProviderRequest {
            model: "compat-test".into(),
            system: "sys".into(),
            messages,
            tools: vec![],
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        };
        let picture_message = || ProviderMessage::Media {
            role: "user".into(),
            content: "what is this?".into(),
            attachments: picture(),
        };

        // Sent now: the person hears it, by the model's name.
        let Err(error) = provider
            .stream(request(vec![picture_message()]), CancellationToken::new())
            .await
        else {
            panic!("a picture for a text-only model must be refused");
        };
        assert!(
            error.to_string().contains("Coder 30B doesn't read images"),
            "{error}"
        );
        assert!(state.0.lock().is_empty());

        // Sent earlier (to another model), or returned by a tool: a note.
        let messages = vec![
            picture_message(),
            text("assistant", "A cat."),
            text("user", "read the screenshot"),
            ProviderMessage::ToolCall {
                call_id: "call-1".into(),
                name: "read".into(),
                arguments: json!({"path": "shot.png"}),
                provider_metadata: Value::Null,
            },
            ProviderMessage::ToolResult {
                call_id: "call-1".into(),
                name: "read".into(),
                output: "shot.png".into(),
                attachments: picture(),
                is_error: false,
            },
        ];
        provider
            .stream(request(messages), CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        let sent = state.0.lock()[0]["messages"].to_string();
        assert!(!sent.contains("image_url"), "{sent}");
        assert_eq!(
            sent.matches("[Image shot.png is not shown to you").count(),
            2,
            "{sent}"
        );
    }

    /// Haarness Cloud lists Claude, GPT and DeepSeek without input
    /// modalities, and its router refuses `input_audio` for them ("… doesn't
    /// take audio"), a refusal every later turn would meet again. A tool's
    /// recording goes as audio only to a model whose listing says it takes
    /// audio, or to a family known to (Gemini); the rest get a note. Other
    /// vendors than Google and OpenAI go through a route that takes no audio.
    #[tokio::test]
    async fn a_tools_recording_goes_only_to_a_model_that_takes_audio() {
        let state = FixtureState::default();
        let router = Router::new()
            .route(
                "/models",
                get(|| async {
                    Json(json!({"data": [
                        {"id": "anthropic/claude-sonnet-5-5", "supports_vision": true},
                        {"id": "google/gemini-3.5-flash"},
                        {"id": "openai/gpt-audio", "architecture": {"input_modalities": ["text", "audio"]}},
                        {"id": "google/gemini-listed", "architecture": {"input_modalities": ["text", "image"]}},
                        {"id": "mistralai/voxtral-listed", "architecture": {"input_modalities": ["text", "audio"]}},
                    ]}))
                }),
            )
            .route("/chat/completions", post(completions))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let instance = || CompatibleInstance {
            id: "haarness".into(),
            name: "Haarness Cloud".into(),
            base_url: format!("http://{address}"),
            api_key: Some("fixture".into()),
            models: Vec::new(),
            discover_models: true,
            server_output_limit: false,
            replay_observed_reasoning: false,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: false,
            headers: crate::ProviderHeaders::default(),
        };
        // As Haarness Cloud does: one transport lists the models, and a new
        // one sends each request, sharing what the listing said.
        let listed = ListedAudio::default();
        let models = OpenAiCompatibleProvider::new(instance())
            .with_listed_audio(listed.clone())
            .fetch_models()
            .await
            .unwrap();
        for (model, takes) in [
            ("anthropic/claude-sonnet-5-5", false),
            ("google/gemini-3.5-flash", true),
            ("openai/gpt-audio", true),
            ("google/gemini-listed", false),
            ("deepseek/deepseek-v4-flash", false),
            ("openai/gpt-audio-mini", true),
            // Cloud sends these through a route that takes no audio.
            ("mistralai/voxtral-small-24b-2507", false),
            ("qwen/qwen3.8-omni-flash", false),
            ("mistralai/voxtral-listed", false),
        ] {
            let request = ProviderRequest {
                model: model.into(),
                system: "sys".into(),
                messages: vec![
                    ProviderMessage::Text {
                        role: "user".into(),
                        content: "record the demo".into(),
                    },
                    ProviderMessage::ToolCall {
                        call_id: "call-1".into(),
                        name: "mcp__voice__clip".into(),
                        arguments: json!({}),
                        provider_metadata: Value::Null,
                    },
                    ProviderMessage::ToolResult {
                        call_id: "call-1".into(),
                        name: "mcp__voice__clip".into(),
                        output: "recorded".into(),
                        attachments: vec!["data:audio/mpeg;name=clip.mp3;base64,SUQz".into()],
                        is_error: false,
                    },
                ],
                tools: vec![],
                reasoning_effort: None,
                parameters: haarness_core::RequestParameters::default(),
                output_schema: None,
            };
            OpenAiCompatibleProvider::new(instance())
                .with_model_info(models.clone())
                .with_listed_audio(listed.clone())
                .stream(request, CancellationToken::new())
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await;
            let sent = state.0.lock().last().unwrap()["messages"].to_string();
            assert_eq!(sent.contains("input_audio"), takes, "{model}: {sent}");
            assert_eq!(
                sent.contains("[Audio clip.mp3 is left out"),
                !takes,
                "{model}: {sent}"
            );
        }
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
    async fn whole_calls_in_a_stream_without_a_finish_reason_are_made() {
        let finish = |events: &[Result<ProviderEvent, haarness_core::CoreError>]| {
            events.iter().find_map(|event| match event {
                Ok(ProviderEvent::Finished { reason }) => Some(reason.clone()),
                _ => None,
            })
        };
        let calls = |events: &[Result<ProviderEvent, haarness_core::CoreError>]| {
            events
                .iter()
                .filter(|event| matches!(event, Ok(ProviderEvent::ToolCall { .. })))
                .count()
        };
        // A server that streams whole calls and ends without saying why.
        let events = stream_events(concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"bash\",\"arguments\":\"{\\\"command\\\":\\\"dir\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"c2\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}}]}}]}\n\n",
            "data: [DONE]\n\n"
        ))
        .await;
        assert_eq!(calls(&events), 2, "{events:?}");
        assert_eq!(finish(&events).as_deref(), Some("tool_calls"));
        // A call without an ID gets one.
        let events = stream_events(concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"\",\"function\":{\"name\":\"read\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        ))
        .await;
        assert!(
            events.iter().any(|event| matches!(event, Ok(ProviderEvent::ToolCall { call_id, .. }) if call_id.starts_with("call_") && call_id.len() > 5)),
            "{events:?}"
        );
        // An empty reason on a later chunk does not hide the real one.
        let events = stream_events(concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"edit\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n"
        ))
        .await;
        assert_eq!(finish(&events).as_deref(), Some("tool_calls"));
        // A call whose arguments stop partway is a response cut short, and so
        // are words alone with no reason.
        let events = stream_events(concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"write\",\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\",\\\"content\\\":\\\"par\"}}]}}]}\n\n",
            "data: [DONE]\n\n"
        ))
        .await;
        assert_eq!(finish(&events).as_deref(), Some("incomplete"));
        let events = stream_events(concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Halfway thr\"}}]}\n\n",
            "data: [DONE]\n\n"
        ))
        .await;
        assert_eq!(finish(&events).as_deref(), Some("incomplete"));
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
        // Tool arguments that do not parse still make a call, which the
        // runtime answers with an error instead of running it, and the usage
        // comes first.
        let events = stream_events(concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"read\",\"arguments\":\"{broken\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":90,\"completion_tokens\":7}}\n\n",
            "data: [DONE]\n\n"
        ))
        .await;
        assert_eq!(reported(&events), [(90, 7, 0, 0)]);
        assert!(events.iter().all(Result::is_ok), "{events:?}");
        assert!(events.iter().any(|event| matches!(event,
            Ok(ProviderEvent::ToolCall { arguments, .. })
                if *arguments == haarness_core::tool_call_arguments("{broken"))));
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

    /// A Mistral reasoning model's stream: its thinking a few words a part.
    async fn mistral_thinking(
        Json(_request): Json<Value>,
    ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
        use std::fmt::Write as _;
        let thinking = |text: &str| json!({"type":"thinking","thinking":[{"type":"text","text":text}],"closed":true});
        let mut stream = String::new();
        for chunk in [
            json!({"choices":[{"delta":{"role":"assistant","content":[thinking("The user wants me")]}}]}),
            json!({"choices":[{"delta":{"content":[thinking(" to read a file")]}}]}),
            json!({"choices":[{"delta":{"content":[thinking(", then answer.")]}}]}),
            json!({"choices":[{"delta":{"content":[{"type":"text","text":"Reading "}]}}]}),
            json!({"choices":[{"delta":{"content":[{"type":"text","text":"it now."}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"read","arguments":"{\"path\":\"a.txt\"}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        ] {
            let _ = write!(stream, "data: {chunk}\n\n");
        }
        ([(CONTENT_TYPE, "text/event-stream")], stream)
    }

    #[tokio::test]
    async fn mistral_thinking_streamed_in_pieces_is_sent_back_whole() {
        let router = Router::new().route("/chat/completions", post(mistral_thinking));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider =
            OpenAiCompatibleProvider::for_test(format!("http://{address}/chat/completions"));
        let mut stream = provider
            .stream(
                ProviderRequest {
                    model: "compat-test".into(),
                    system: "sys".into(),
                    messages: vec![ProviderMessage::Text {
                        role: "user".into(),
                        content: "read a.txt".into(),
                    }],
                    tools: Vec::new(),
                    reasoning_effort: None,
                    parameters: haarness_core::RequestParameters::default(),
                    output_schema: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut content = None;
        while let Some(event) = stream.next().await {
            if let ProviderEvent::ToolCall {
                provider_metadata, ..
            } = event.unwrap()
            {
                content = Some(provider_metadata["mistral_content"].clone());
            }
        }
        // One thinking part and one text part, as Mistral's own complete
        // reply has them, not a part for every few words.
        assert_eq!(
            content.unwrap(),
            json!([
                {"type":"thinking","thinking":[{"type":"text","text":"The user wants me to read a file, then answer."}],"closed":true},
                {"type":"text","text":"Reading it now."}
            ])
        );
        // Parts that are not plain text are kept as they came.
        let mut parts = vec![json!({"type":"thinking","thinking":[{"type":"text","text":"a"}]})];
        push_mistral_part(&mut parts, &json!({"type":"image_url","image_url":"x"}));
        push_mistral_part(
            &mut parts,
            &json!({"type":"thinking","thinking":[{"type":"text","text":"b"}]}),
        );
        assert_eq!(parts.len(), 3);
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

    /// A server in thinking mode that calls `read` without thinking first:
    /// it names `field` empty, then null, as DeepSeek does.
    async fn empty_reasoning_in_field(
        State((state, field)): State<(FixtureState, &'static str)>,
        Json(request): Json<Value>,
    ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
        use std::fmt::Write as _;
        state.0.lock().push(request);
        let mut stream = String::new();
        for chunk in [
            json!({"choices":[{"delta":{"role":"assistant","content":"",field: ""}}]}),
            json!({"choices":[{"delta":{field: null,"tool_calls":[{"index":0,"id":"call_a","function":{"name":"read","arguments":"{\"path\":\"a.txt\"}"}}]}}]}),
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
        round_trip_with(post(reasoning_in_field), id, field, own, history).await
    }

    async fn round_trip_with(
        handler: axum::routing::MethodRouter<(FixtureState, &'static str)>,
        id: &str,
        field: &'static str,
        own: bool,
        history: Option<Value>,
    ) -> (Value, Value) {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/chat/completions", handler)
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
    async fn a_custom_server_that_names_its_reasoning_empty_gets_the_field_back() {
        // DeepSeek behind an endpoint the person set up: a call made without
        // thinking first is refused on the next request unless its
        // `reasoning_content` comes back, empty as it was.
        let (metadata, assistant) = round_trip_with(
            post(empty_reasoning_in_field),
            "my-deepseek",
            "reasoning_content",
            true,
            None,
        )
        .await;
        assert_eq!(
            metadata,
            json!({"reasoning_content": "", "reasoning_provider": "my-deepseek"})
        );
        assert_eq!(assistant["reasoning_content"], "", "{assistant}");
        assert!(assistant.get("reasoning").is_none(), "{assistant}");
        // A built-in service whose API documents no such field still gets none.
        let (metadata, assistant) = round_trip_with(
            post(empty_reasoning_in_field),
            "groq",
            "reasoning",
            false,
            None,
        )
        .await;
        assert_eq!(metadata, Value::Null);
        assert!(assistant.get("reasoning").is_none(), "{assistant}");
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

    async fn discovered_preset(
        id: &str,
        fallback: &[&str],
        list: Value,
    ) -> (OpenAiCompatibleProvider, Arc<Mutex<Vec<String>>>) {
        let queries = Arc::new(Mutex::new(Vec::new()));
        let seen = queries.clone();
        let router = Router::new().route(
            "/models",
            get(move |uri: axum::http::Uri| {
                seen.lock().push(uri.query().unwrap_or_default().to_owned());
                let list = list.clone();
                async move { Json(list) }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = OpenAiCompatibleProvider::new(CompatibleInstance {
            id: id.into(),
            name: id.into(),
            base_url: format!("http://{address}"),
            api_key: Some("fixture-secret".into()),
            models: fallback.iter().map(|model| (*model).to_owned()).collect(),
            discover_models: true,
            server_output_limit: false,
            replay_observed_reasoning: false,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: false,
            headers: crate::ProviderHeaders::default(),
        })
        .discover_models()
        .await;
        (provider, queries)
    }

    #[tokio::test]
    async fn an_unknown_new_compatible_model_appears_with_usable_defaults() {
        let (provider, _) = discovered_preset(
            "groq",
            &["openai/gpt-oss-120b", "openai/gpt-oss-20b"],
            json!({"data": [
                // The listing's own levels win over the built-in table.
                {"id": "openai/gpt-oss-120b", "supported_parameters": ["tools", "reasoning"], "reasoning_efforts": ["low", "ultra"]},
                // Absent from the table, but it says it takes an effort.
                {"id": "vendor/new-model", "supported_parameters": ["tools", "reasoning", "reasoning_effort"]},
                {"id": "vendor/plain-model"}
            ]}),
        )
        .await;
        let models = provider.info().models;
        let find = |id: &str| models.iter().find(|model| model.id == id);
        // A listed model the provider dropped is gone.
        assert!(find("openai/gpt-oss-20b").is_none());
        assert_eq!(
            find("openai/gpt-oss-120b").unwrap().reasoning_efforts,
            ["low", "ultra"]
        );
        let new = find("vendor/new-model").unwrap();
        assert!(new.reasoning && new.tools);
        assert_eq!(new.reasoning_efforts, ["low", "medium", "high"]);
        let plain = find("vendor/plain-model").unwrap();
        assert!(plain.tools && !plain.images && plain.pricing.is_none());
    }

    #[tokio::test]
    async fn siliconflow_asks_for_chat_models_and_image_editors_stay_out() {
        let (provider, queries) = discovered_preset(
            "siliconflow-cn",
            &["deepseek-ai/DeepSeek-V3.2"],
            json!({"data": [{"id": "zai-org/GLM-5.2"}, {"id": "Qwen/Qwen-Image-Edit"}]}),
        )
        .await;
        assert_eq!(queries.lock().as_slice(), ["sub_type=chat"]);
        let ids: Vec<_> = provider
            .info()
            .models
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert_eq!(ids, ["zai-org/GLM-5.2"]);
        let (_, queries) = discovered_preset("modelscope", &["a"], json!({"data": []})).await;
        assert_eq!(queries.lock().as_slice(), [""]);
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
        // GLM-5.1 switches thinking rather than taking a level.
        assert_eq!(requests[0]["thinking"]["type"], "enabled");
    }

    #[test]
    fn glm_models_offer_the_levels_z_ai_accepts_and_have_prices() {
        let info = |id: &str| model_info("zai", id, CompatibleModelDefaults::default());
        assert_eq!(info("glm-5.3").reasoning_efforts, ["low", "high", "max"]);
        assert_eq!(info("glm-5.1").reasoning_efforts, ["none", "high"]);
        assert_eq!(
            info("glm-5.2").reasoning_efforts,
            ["none", "low", "medium", "high", "max"]
        );
        let flash = info("glm-5.3-flash");
        assert!(flash.reasoning);
        assert!(flash.images);
        assert!(!info("glm-5.3").images);
        assert_eq!(
            flash.pricing.map(|price| (price.input, price.output)),
            Some((0.15, 0.5))
        );
        assert!(info("glm-5-turbo").pricing.is_none());
        assert_eq!(info("glm-5.3-flashx").context_window, Some(1_000_000));
        assert_eq!(info("glm-5.1").context_window, Some(200_000));
        assert_eq!(info("glm-4.5-air").context_window, Some(128_000));
        assert_eq!(info("glm-5.3-flashx").display_name, "GLM-5.3-FlashX");
        assert_eq!(info("glm-5-turbo").display_name, "GLM-5-Turbo");
        assert_eq!(info("glm-4.5-air").display_name, "GLM-4.5-Air");
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
        assert!(error.to_string().contains("doesn't read images"));
        assert_eq!(
            state.0.lock().len(),
            1,
            "rejected request never reached HTTP"
        );
    }

    /// A custom endpoint at `base` for `model`, with a key.
    fn custom_at(base: String) -> OpenAiCompatibleProvider {
        OpenAiCompatibleProvider::new(custom_instance(base))
    }

    fn custom_instance(base: String) -> CompatibleInstance {
        CompatibleInstance {
            id: "my-endpoint".into(),
            name: "My endpoint".into(),
            base_url: base,
            api_key: Some("fixture".into()),
            models: vec!["fixture-model".into()],
            discover_models: false,
            server_output_limit: false,
            replay_observed_reasoning: true,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: false,
            headers: crate::ProviderHeaders::default(),
        }
    }

    fn hello(model: &str) -> ProviderRequest {
        ProviderRequest {
            model: model.into(),
            system: "sys".into(),
            messages: vec![ProviderMessage::Text {
                role: "user".into(),
                content: "hello".into(),
            }],
            tools: Vec::new(),
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        }
    }

    async fn serve(router: Router) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{address}")
    }

    /// Everything the stream sent, or the error that ended it.
    async fn outcome(
        provider: &OpenAiCompatibleProvider,
        request: ProviderRequest,
    ) -> Result<Vec<ProviderEvent>, CoreError> {
        let mut stream = provider.stream(request, CancellationToken::new()).await?;
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event?);
        }
        Ok(events)
    }

    fn text_of(events: &[ProviderEvent]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::TextDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    async fn ready_stream() -> ([(axum::http::HeaderName, &'static str); 1], &'static str) {
        (
            [(CONTENT_TYPE, "text/event-stream")],
            "data: {\"choices\":[{\"delta\":{\"content\":\"ready\"}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        )
    }

    #[tokio::test]
    async fn a_pasted_endpoint_works_and_a_wrong_address_says_what_to_change() {
        let root = serve(
            Router::new()
                .route("/v1/chat/completions", post(ready_stream))
                .fallback(|| async { axum::http::StatusCode::NOT_FOUND }),
        )
        .await;
        // The base pasted with its endpoint, as a curl example shows it.
        let pasted = custom_at(format!("{root}/v1/chat/completions/"));
        let events = outcome(&pasted, hello("fixture-model")).await.unwrap();
        assert_eq!(text_of(&events), "ready");
        // A base without its version: every request would be refused.
        let error = outcome(&custom_at(root.clone()), hello("fixture-model"))
            .await
            .unwrap_err();
        assert!(
            matches!(error, CoreError::ProviderUnavailable(_)),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            format!(
                "My endpoint has no chat API at {root}/chat/completions. Change the provider's base URL to the one in its API documentation: it usually ends in /v1."
            )
        );
    }

    #[tokio::test]
    async fn a_missing_model_keeps_the_providers_own_words() {
        let root = serve(Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                (
                    axum::http::StatusCode::NOT_FOUND,
                    Json(json!({"error":{"message":"The model `gone` does not exist or you do not have access to it.","code":"model_not_found"}})),
                )
            }),
        ))
        .await;
        let error = outcome(&custom_at(format!("{root}/v1")), hello("gone"))
            .await
            .unwrap_err();
        assert!(matches!(error, CoreError::Provider(_)), "{error:?}");
        assert!(
            error
                .to_string()
                .contains("The model `gone` does not exist"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_template_that_takes_one_call_a_message_gets_the_calls_one_by_one() {
        let state = FixtureState::default();
        let root = serve(
            Router::new()
                .route(
                    "/v1/chat/completions",
                    post(
                        |State(state): State<FixtureState>, Json(request): Json<Value>| async move {
                            // Llama 3.1's chat template on vLLM.
                            let several = request["messages"].as_array().unwrap().iter().any(|message| {
                                message["tool_calls"].as_array().is_some_and(|calls| calls.len() > 1)
                            });
                            state.0.lock().push(request);
                            if several {
                                return (
                                    axum::http::StatusCode::BAD_REQUEST,
                                    [(CONTENT_TYPE, "application/json")],
                                    r#"{"object":"error","message":"This model only supports single tool-calls at once!","type":"BadRequestError","code":400}"#,
                                );
                            }
                            (
                                axum::http::StatusCode::OK,
                                [(CONTENT_TYPE, "text/event-stream")],
                                "data: {\"choices\":[{\"delta\":{\"content\":\"ready\"},\"finish_reason\":\"stop\"}]}\n\n",
                            )
                        },
                    ),
                )
                .with_state(state.clone()),
        )
        .await;
        let mut request = hello("llama-3.1-8b");
        let call = |id: &str, path: &str| ProviderMessage::ToolCall {
            call_id: id.into(),
            name: "read".into(),
            arguments: json!({"path": path}),
            provider_metadata: Value::Null,
        };
        let result = |id: &str| ProviderMessage::ToolResult {
            call_id: id.into(),
            name: "read".into(),
            output: format!("contents of {id}"),
            is_error: false,
            attachments: vec![],
        };
        request.messages.extend([
            call("a", "a.txt"),
            call("b", "b.txt"),
            ProviderMessage::Text {
                role: "assistant".into(),
                content: "Reading both.".into(),
            },
            result("a"),
            result("b"),
        ]);
        let provider = custom_at(format!("{root}/v1"));
        let events = outcome(&provider, request.clone()).await.unwrap();
        assert_eq!(text_of(&events), "ready");
        {
            let requests = state.0.lock();
            assert_eq!(requests.len(), 2);
            let sent = requests[1]["messages"].as_array().unwrap();
            let shape: Vec<String> = sent
                .iter()
                .map(|message| match message["role"].as_str().unwrap() {
                    "assistant" => {
                        format!("call {}", message["tool_calls"][0]["id"].as_str().unwrap())
                    }
                    "tool" => format!("result {}", message["tool_call_id"].as_str().unwrap()),
                    role => role.to_owned(),
                })
                .collect();
            assert_eq!(
                shape,
                ["system", "user", "call a", "result a", "call b", "result b"]
            );
            // The words that came with the calls stay with the first.
            assert_eq!(sent[2]["content"], "Reading both.");
            assert!(sent[4]["content"].is_null());
            assert_eq!(requests[1]["parallel_tool_calls"], false);
        }
        // Every later request to that model goes one call a message at once.
        let events = outcome(&provider, request).await.unwrap();
        assert_eq!(text_of(&events), "ready");
        assert_eq!(state.0.lock().len(), 3);
        // Another refusal is still the server's own.
        assert!(!super::refuses_parallel_calls(
            "This model's maximum context length is 8192 tokens"
        ));
    }

    #[tokio::test]
    async fn a_server_that_refuses_stream_options_is_asked_again_without_them() {
        let state = FixtureState::default();
        let root = serve(
            Router::new()
                .route(
                    "/v1/chat/completions",
                    post(
                        |State(state): State<FixtureState>, Json(request): Json<Value>| async move {
                            let refuse = request.get("stream_options").is_some();
                            state.0.lock().push(request);
                            if refuse {
                                return (
                                    axum::http::StatusCode::BAD_REQUEST,
                                    [(CONTENT_TYPE, "application/json")],
                                    r#"{"error":{"message":"Unrecognized request argument supplied: stream_options"}}"#,
                                );
                            }
                            (
                                axum::http::StatusCode::OK,
                                [(CONTENT_TYPE, "text/event-stream")],
                                "data: {\"choices\":[{\"delta\":{\"content\":\"ready\"},\"finish_reason\":\"stop\"}]}\n\n",
                            )
                        },
                    ),
                )
                .with_state(state.clone()),
        )
        .await;
        let events = outcome(&custom_at(format!("{root}/v1")), hello("fixture-model"))
            .await
            .unwrap();
        assert_eq!(text_of(&events), "ready");
        let requests = state.0.lock();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].get("stream_options").is_none());
        assert_eq!(requests[1]["stream"], true);
    }

    #[tokio::test]
    async fn a_reply_sent_whole_is_read_like_a_stream() {
        let root = serve(Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                Json(json!({
                    "id": "whole-1",
                    "object": "chat.completion",
                    "choices": [{
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": "Reading both.",
                            "tool_calls": [
                                {"id":"call_a","type":"function","function":{"name":"read","arguments":"{\"path\":\"a.txt\"}"}},
                                {"id":"call_b","type":"function","function":{"name":"read","arguments":"{\"path\":\"b.txt\"}"}}
                            ]
                        },
                        "finish_reason": "tool_calls"
                    }],
                    "usage": {"prompt_tokens": 12, "completion_tokens": 5}
                }))
            }),
        ))
        .await;
        let events = outcome(&custom_at(format!("{root}/v1")), hello("fixture-model"))
            .await
            .unwrap();
        assert_eq!(text_of(&events), "Reading both.");
        let calls: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::ToolCall {
                    call_id, arguments, ..
                } => Some((call_id.clone(), arguments.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            calls,
            [
                ("call_a".to_owned(), json!({"path": "a.txt"})),
                ("call_b".to_owned(), json!({"path": "b.txt"}))
            ]
        );
        assert!(events.iter().any(|event| matches!(
            event,
            ProviderEvent::Usage {
                input_tokens: 12,
                output_tokens: 5,
                ..
            }
        )));
        assert!(events.iter().any(
            |event| matches!(event, ProviderEvent::Finished { reason } if reason == "tool_calls")
        ));
    }

    #[tokio::test]
    async fn a_call_with_broken_arguments_is_answered_with_an_error_and_the_others_run() {
        async fn calls() -> ([(axum::http::HeaderName, &'static str); 1], String) {
            use std::fmt::Write as _;
            let mut stream = String::new();
            for chunk in [
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"read","arguments":"{\"path\":\"a.txt\"}"}}]}}]}),
                // Cut off mid-object, as a model sometimes writes it.
                json!({"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"read","arguments":"{\"path\":\"b.t"}}]}}]}),
                // No parameters, sent as nothing at all.
                json!({"choices":[{"delta":{"tool_calls":[{"index":2,"id":"call_c","function":{"name":"todoread","arguments":""}}]}}]}),
                // The object encoded twice, as a string.
                json!({"choices":[{"delta":{"tool_calls":[{"index":3,"id":"call_d","function":{"name":"read","arguments":"\"{\\\"path\\\":\\\"d.txt\\\"}\""}}]}}]}),
                json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
            ] {
                let _ = write!(stream, "data: {chunk}\n\n");
            }
            ([(CONTENT_TYPE, "text/event-stream")], stream)
        }
        let root = serve(Router::new().route("/v1/chat/completions", post(calls))).await;
        let events = outcome(&custom_at(format!("{root}/v1")), hello("fixture-model"))
            .await
            .unwrap();
        let calls: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::ToolCall {
                    call_id, arguments, ..
                } => Some((call_id.as_str(), arguments.clone())),
                _ => None,
            })
            .collect();
        // The cut-off call still reaches the runtime, which answers it with
        // an error the model can correct instead of running it.
        assert_eq!(
            calls,
            [
                ("call_a", json!({"path": "a.txt"})),
                (
                    "call_b",
                    haarness_core::tool_call_arguments("{\"path\":\"b.t")
                ),
                ("call_c", json!({})),
                ("call_d", json!({"path": "d.txt"}))
            ]
        );
        let reason = events.iter().find_map(|event| match event {
            ProviderEvent::Finished { reason } => Some(reason.as_str()),
            _ => None,
        });
        assert_eq!(reason, Some("tool_calls"));
    }

    #[tokio::test]
    async fn words_before_a_call_go_in_the_calls_message_for_a_server_set_up_here() {
        let state = FixtureState::default();
        let root = serve(
            Router::new()
                .route(
                    "/v1/chat/completions",
                    post(
                        |State(state): State<FixtureState>, Json(request): Json<Value>| async move {
                            state.0.lock().push(request);
                            ready_stream().await
                        },
                    ),
                )
                .with_state(state.clone()),
        )
        .await;
        let history = || {
            vec![
                ProviderMessage::Text {
                    role: "user".into(),
                    content: "Read a.txt".into(),
                },
                ProviderMessage::Text {
                    role: "assistant".into(),
                    content: "I'll read the file.".into(),
                },
                ProviderMessage::ToolCall {
                    call_id: "call_a".into(),
                    name: "read".into(),
                    arguments: json!({"path": "a.txt"}),
                    provider_metadata: json!({"reasoning_content": "", "reasoning_provider": "my-endpoint"}),
                },
                ProviderMessage::ToolResult {
                    call_id: "call_a".into(),
                    name: "read".into(),
                    output: "contents".into(),
                    is_error: false,
                    attachments: Vec::new(),
                },
            ]
        };
        let mut request = hello("fixture-model");
        request.messages = history();
        outcome(&custom_at(format!("{root}/v1")), request)
            .await
            .unwrap();
        let sent = state.0.lock()[0]["messages"].clone();
        // system, user, one assistant message with words, call and field, tool.
        assert_eq!(sent.as_array().unwrap().len(), 4, "{sent}");
        assert_eq!(sent[2]["role"], "assistant");
        assert_eq!(sent[2]["content"], "I'll read the file.");
        assert_eq!(sent[2]["tool_calls"][0]["id"], "call_a");
        assert_eq!(sent[2]["reasoning_content"], "");
        assert_eq!(sent[3]["role"], "tool");
    }

    /// Gemini's OpenAI-compatible API, as it streamed two parallel calls:
    /// each whole and without an index, the first with its thought signature
    /// in `extra_content`, and the batch finished as `stop`.
    const GEMINI_PARALLEL: &str = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"I will read both files.\",\"role\":\"assistant\"},\"index\":0}],\"usage\":{\"completion_tokens\":10,\"prompt_tokens\":78,\"total_tokens\":147}}\n\n",
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"extra_content\":{\"google\":{\"thought_signature\":\"sig-a\"}},\"function\":{\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\"}\",\"name\":\"read\"},\"id\":\"call_a\",\"type\":\"function\"}]},\"index\":0}],\"usage\":{\"completion_tokens\":42,\"prompt_tokens\":78,\"total_tokens\":179}}\n\n",
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"function\":{\"arguments\":\"{\\\"path\\\":\\\"b.txt\\\"}\",\"name\":\"read\"},\"id\":\"call_b\",\"type\":\"function\"}]},\"index\":0}],\"usage\":{\"completion_tokens\":42,\"prompt_tokens\":78,\"total_tokens\":179}}\n\n",
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"},\"finish_reason\":\"stop\",\"index\":0}],\"usage\":{\"completion_tokens\":42,\"prompt_tokens\":78,\"total_tokens\":179}}\n\n",
        "data: [DONE]\n\n",
    );

    fn calls_of(events: &[ProviderEvent]) -> Vec<(String, String, Value, Value)> {
        events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::ToolCall {
                    call_id,
                    name,
                    arguments,
                    provider_metadata,
                } => Some((
                    call_id.clone(),
                    name.clone(),
                    arguments.clone(),
                    provider_metadata.clone(),
                )),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn calls_streamed_without_an_index_stay_apart() {
        // Gemini's parallel calls, whole and unnumbered: two calls, not one
        // with both argument objects run together.
        let streamed = stream_events(GEMINI_PARALLEL).await;
        assert_eq!(reported(&streamed), [(78, 42, 0, 0)]);
        let events: Vec<_> = streamed.into_iter().map(Result::unwrap).collect();
        let calls = calls_of(&events);
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(
            (calls[0].0.as_str(), calls[0].1.as_str()),
            ("call_a", "read")
        );
        assert_eq!(calls[0].2, json!({"path": "a.txt"}));
        assert_eq!(
            (calls[1].0.as_str(), calls[1].1.as_str()),
            ("call_b", "read")
        );
        assert_eq!(calls[1].2, json!({"path": "b.txt"}));
        assert_eq!(
            calls[0].3["extra_content"],
            json!({"google": {"thought_signature": "sig-a"}})
        );
        assert!(
            calls[1].3.get("extra_content").is_none(),
            "{:?}",
            calls[1].3
        );
        // A fragment naming neither index nor ID continues the latest call;
        // an ID first seen later names the call it began; an ID seen before
        // continues its own call.
        let events: Vec<_> = stream_events(concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"pa\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"x\",\"function\":{\"arguments\":\"th\\\":\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"y\",\"function\":{\"name\":\"list\",\"arguments\":\"{}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"x\",\"function\":{\"arguments\":\"\\\"a\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        ))
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
        let calls = calls_of(&events);
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(
            (calls[0].0.as_str(), calls[0].1.as_str(), &calls[0].2),
            ("x", "read", &json!({"path": "a"}))
        );
        assert_eq!(
            (calls[1].0.as_str(), calls[1].1.as_str(), &calls[1].2),
            ("y", "list", &json!({}))
        );
    }

    #[tokio::test]
    async fn a_calls_thought_signature_goes_back_to_the_model_that_gave_it() {
        // A stand-in for Gemini's OpenAI-compatible API: it refuses a
        // follow-up whose call lacks the signature it gave, in its own
        // envelope (a list of one error).
        let state = FixtureState::default();
        let root = serve(
            Router::new()
                .route(
                    "/v1/chat/completions",
                    post(
                        |State(state): State<FixtureState>, Json(request): Json<Value>| async move {
                            state.0.lock().push(request.clone());
                            let messages = request["messages"].as_array().cloned().unwrap_or_default();
                            let stream = |body: String| {
                                axum::response::Response::builder()
                                    .header(CONTENT_TYPE, "text/event-stream")
                                    .body(axum::body::Body::from(body))
                                    .unwrap()
                            };
                            if !messages.iter().any(|message| message["role"] == "tool") {
                                return stream(GEMINI_PARALLEL.to_owned());
                            }
                            let signed = messages.iter().any(|message| {
                                message["tool_calls"][0]["extra_content"]["google"]["thought_signature"] == "sig-a"
                            });
                            if signed {
                                return stream(concat!(
                                    "data: {\"choices\":[{\"delta\":{\"content\":\"Both read.\"},\"finish_reason\":\"stop\",\"index\":0}]}\n\n",
                                    "data: [DONE]\n\n"
                                ).to_owned());
                            }
                            axum::response::Response::builder()
                                .status(400)
                                .header(CONTENT_TYPE, "application/json")
                                .body(axum::body::Body::from("[{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"Function call is missing a thought_signature in functionCall parts.\",\n    \"status\": \"INVALID_ARGUMENT\"\n  }\n}\n]\n"))
                                .unwrap()
                        },
                    ),
                )
                .with_state(state.clone()),
        )
        .await;
        let provider = custom_at(format!("{root}/v1"));
        let mut request = hello("fixture-model");
        request.tools = vec![ToolDefinition {
            name: "read".into(),
            description: "Read a file".into(),
            input_schema: json!({"type": "object"}),
        }];
        let calls = calls_of(&outcome(&provider, request.clone()).await.unwrap());
        assert_eq!(calls.len(), 2, "{calls:?}");
        let mut follow_up = request.clone();
        follow_up.messages.push(ProviderMessage::Text {
            role: "assistant".into(),
            content: "I will read both files.".into(),
        });
        for (call_id, name, arguments, provider_metadata) in &calls {
            follow_up.messages.push(ProviderMessage::ToolCall {
                call_id: call_id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
                provider_metadata: provider_metadata.clone(),
            });
        }
        for (call_id, name, ..) in &calls {
            follow_up.messages.push(ProviderMessage::ToolResult {
                call_id: call_id.clone(),
                name: name.clone(),
                output: "contents".into(),
                is_error: false,
                attachments: Vec::new(),
            });
        }
        let answer = outcome(&provider, follow_up.clone()).await.unwrap();
        assert_eq!(text_of(&answer), "Both read.");
        let sent = state.0.lock()[1]["messages"].clone();
        let batch = &sent[2]["tool_calls"];
        assert_eq!(batch.as_array().unwrap().len(), 2, "{sent}");
        assert_eq!(
            batch[0]["extra_content"],
            json!({"google": {"thought_signature": "sig-a"}})
        );
        assert!(batch[1].get("extra_content").is_none(), "{sent}");
        // Another model of the same server, or another server, never gets
        // the signature; the refusal then keeps the server's own words.
        follow_up.model = "other-model".into();
        let refused = outcome(&provider, follow_up).await.unwrap_err();
        assert!(
            refused.to_string().contains("missing a thought_signature"),
            "{refused}"
        );
        assert!(
            state.0.lock()[2]["messages"][2]["tool_calls"][0]
                .get("extra_content")
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_server_set_up_here_gets_user_messages_that_alternate() {
        // After a turn that failed before any reply, the next message follows
        // the unanswered one; Gemma's and Mistral's chat templates refuse two
        // user messages in a row, so they go to such a server as one.
        let state = FixtureState::default();
        let root = serve(
            Router::new()
                .route(
                    "/v1/chat/completions",
                    post(
                        |State(state): State<FixtureState>, Json(request): Json<Value>| async move {
                            state.0.lock().push(request);
                            ready_stream().await
                        },
                    ),
                )
                .with_state(state.clone()),
        )
        .await;
        let user = |content: &str| ProviderMessage::Text {
            role: "user".into(),
            content: content.into(),
        };
        let mut request = hello("fixture-model");
        request.messages = vec![
            user("Fix the build"),
            user("Try again"),
            ProviderMessage::Text {
                role: "assistant".into(),
                content: "Done.".into(),
            },
            user("And the tests"),
            ProviderMessage::Media {
                role: "user".into(),
                content: "This picture shows the failure".into(),
                attachments: vec!["data:image/png;base64,iVBORw0KGgo=".into()],
            },
        ];
        let provider = OpenAiCompatibleProvider::new(CompatibleInstance {
            model_defaults: CompatibleModelDefaults {
                images: Some(true),
                ..CompatibleModelDefaults::default()
            },
            ..custom_instance(format!("{root}/v1"))
        });
        outcome(&provider, request).await.unwrap();
        let sent = state.0.lock()[0]["messages"].clone();
        let roles: Vec<_> = sent
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["role"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(roles, ["system", "user", "assistant", "user"], "{sent}");
        assert_eq!(sent[1]["content"], "Fix the build\n\nTry again");
        let parts = sent[3]["content"].as_array().unwrap();
        assert_eq!(parts[0], json!({"type": "text", "text": "And the tests"}));
        assert_eq!(parts[1]["text"], "This picture shows the failure");
        assert_eq!(parts[2]["type"], "image_url");
    }
}
