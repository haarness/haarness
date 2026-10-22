use async_stream::try_stream;
use async_trait::async_trait;
use futures::StreamExt;
use haarness_core::{
    CoreError, CoreResult, KeyCheck, ModelProvider, ProviderEvent, ProviderMessage,
    ProviderRequest, ProviderStream, STRUCTURED_OUTPUT_TOOL_NAME, structured_output_tool,
};
use haarness_protocol::{ModelInfo, ModelPricing, ProviderInfo};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::auth::{Credential, CredentialStore, OpenAiOAuthClient, openai_residency, unix_millis};
use crate::sse::json_stream;

/// The ChatGPT catalog hides models newer than the calling Codex client. A
/// version far ahead of any release keeps the whole account catalog visible
/// without a Haarness release per Codex build.
const CHATGPT_CATALOG_CLIENT_VERSION: &str = "99.0.0";

pub struct OpenAiProvider {
    client: reqwest::Client,
    endpoint: String,
    api_key: Option<SecretString>,
    auth_store: Option<Arc<CredentialStore>>,
    oauth: OpenAiOAuthClient,
    /// How long a sign-in renewal may wait for its server.
    renewal_limit: std::time::Duration,
    /// Held by the renewal task, not its caller (see [`Self::request_auth`]).
    refresh_lock: Arc<Mutex<()>>,
    models: Vec<ModelInfo>,
    /// Public API catalog for an API key.
    live_models: crate::live_models::LiveModels,
    /// Account catalog for a ChatGPT sign-in: the models the picker offers.
    chatgpt_models: crate::live_models::LiveModels,
    /// Every slug the ChatGPT transport accepts for this account, including
    /// hidden and retiring models that existing threads may still use.
    chatgpt_accepted: std::sync::RwLock<Option<HashSet<String>>>,
}

/// What a ChatGPT account may run, read from its own catalog.
struct ChatGptCatalog {
    advertised: Vec<ModelInfo>,
    accepted: HashSet<String>,
}

impl OpenAiProvider {
    #[must_use]
    pub fn from_environment() -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoint: std::env::var("OPENAI_BASE_URL")
                .map(|base| format!("{}/responses", base.trim_end_matches('/')))
                .unwrap_or_else(|_| "https://api.openai.com/v1/responses".into()),
            api_key: std::env::var("OPENAI_API_KEY").ok().map(SecretString::from),
            auth_store: None,
            oauth: OpenAiOAuthClient::default(),
            refresh_lock: Arc::default(),
            renewal_limit: haarness_account::renewals::LIMIT,
            live_models: crate::live_models::LiveModels::default(),
            chatgpt_models: crate::live_models::LiveModels::default(),
            chatgpt_accepted: std::sync::RwLock::default(),
            models: preferred_models(),
        }
    }

    #[must_use]
    pub fn with_auth_store(mut self, store: Option<Arc<CredentialStore>>) -> Self {
        self.auth_store = store;
        self
    }

    pub async fn discover_models(self) -> Self {
        self.refresh_models(true).await;
        self
    }
    /// The API key that unlocks public `/models` discovery, if any.
    fn discovery_key(&self) -> Option<String> {
        self.api_key
            .as_ref()
            .map(|key| key.expose_secret().to_owned())
            .or_else(|| {
                self.auth_store
                    .as_ref()?
                    .get("openai")
                    .ok()??
                    .api_key()
                    .map(str::to_owned)
            })
    }
    async fn fetch_api_models(&self) -> Option<Vec<ModelInfo>> {
        let key = self.discovery_key()?;
        let base = self.endpoint.strip_suffix("/responses")?;
        let discovered = self
            .client
            .get(format!("{base}/models"))
            .bearer_auth(key)
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .ok()
            .and_then(|response| response.error_for_status().ok());
        if let Some(response) = discovered
            && let Ok(body) = response.json::<Value>().await
        {
            let models = body["data"]
                .as_array()?
                .iter()
                .filter_map(|entry| {
                    let id = entry["id"].as_str()?;
                    if !supports_responses(id) {
                        return None;
                    }
                    let mut info = model(
                        id,
                        id.starts_with("gpt-5")
                            || id.starts_with("gpt-6")
                            || id.starts_with("o1")
                            || id.starts_with("o3")
                            || id.starts_with("o4"),
                    );
                    info.context_window = entry["context_window"]
                        .as_u64()
                        .filter(|value| *value > 0)
                        .or(info.context_window);
                    info.max_output_tokens = entry["max_output_tokens"]
                        .as_u64()
                        .filter(|value| *value > 0)
                        .or(info.max_output_tokens);
                    if let Some(reasoning) = entry["supports_reasoning"].as_bool() {
                        info.reasoning = reasoning;
                    }
                    if let Some(efforts) = entry["reasoning_efforts"].as_array() {
                        info.reasoning_efforts = efforts
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect();
                    }
                    if !info.reasoning {
                        info.reasoning_efforts.clear();
                    }

                    if let Some(name) = entry["display_name"]
                        .as_str()
                        .or_else(|| entry["name"].as_str())
                    {
                        info.display_name = haarness_protocol::model_display_name(id, Some(name));
                    }
                    Some(info)
                })
                .collect();
            return Some(crate::merge_discovered_models(self.models.clone(), models));
        }
        None
    }

    /// A ChatGPT subscription publishes its own catalog: the models this
    /// account may run today, the reasoning tiers each accepts, internal
    /// models kept out of pickers, and retirement notices. That list, not a
    /// static table, decides what Haarness offers; a failed read keeps the
    /// last good catalog.
    async fn fetch_chatgpt_models(&self) -> Option<Vec<ModelInfo>> {
        let RequestAuth::OAuth {
            access,
            account_id,
            residency,
            endpoint,
        } = self.request_auth().await.ok()?
        else {
            return None;
        };
        let base = endpoint.strip_suffix("/responses")?;
        let mut request = self
            .client
            .get(format!(
                "{base}/models?client_version={CHATGPT_CATALOG_CLIENT_VERSION}"
            ))
            .bearer_auth(access)
            .timeout(Duration::from_secs(5));
        if let Some(account_id) = account_id {
            request = request.header("ChatGPT-Account-Id", account_id);
        }
        if let Some(residency) = residency {
            request = request.header("x-openai-internal-codex-residency", residency);
        }
        let response = request.send().await.ok()?.error_for_status().ok()?;
        let body: Value = response.json().await.ok()?;
        let catalog = chatgpt_catalog(&body)?;
        tracing::debug!(
            advertised = catalog.advertised.len(),
            accepted = catalog.accepted.len(),
            "loaded the ChatGPT account model catalog"
        );
        *self.chatgpt_accepted.write().unwrap() = Some(catalog.accepted);
        Some(catalog.advertised)
    }

    #[cfg(test)]
    pub fn for_test(endpoint: String, key: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoint,
            api_key: Some(SecretString::from(key.to_owned())),
            auth_store: None,
            oauth: OpenAiOAuthClient::default(),
            refresh_lock: Arc::default(),
            renewal_limit: haarness_account::renewals::LIMIT,
            live_models: crate::live_models::LiveModels::default(),
            chatgpt_models: crate::live_models::LiveModels::default(),
            chatgpt_accepted: std::sync::RwLock::default(),
            models: preferred_models(),
        }
    }

    #[cfg(test)]
    fn for_oauth_test(endpoint: String, issuer: String, store: Arc<CredentialStore>) -> Self {
        let oauth = OpenAiOAuthClient::new(crate::auth::OpenAiOAuthConfig {
            issuer,
            client_id: "test-client".into(),
            codex_endpoint: endpoint.clone(),
            callback_port: 0,
            user_agent: "haarness-test".into(),
        });
        Self {
            client: reqwest::Client::new(),
            endpoint,
            api_key: None,
            auth_store: Some(store),
            oauth,
            refresh_lock: Arc::default(),
            renewal_limit: haarness_account::renewals::LIMIT,
            live_models: crate::live_models::LiveModels::default(),
            chatgpt_models: crate::live_models::LiveModels::default(),
            chatgpt_accepted: std::sync::RwLock::default(),
            models: preferred_models(),
        }
    }

    async fn request_auth(&self) -> CoreResult<RequestAuth> {
        if let Some(key) = &self.api_key {
            return Ok(RequestAuth::Api(key.expose_secret().to_owned()));
        }
        let store = self.auth_store.as_ref().ok_or_else(not_authenticated)?;
        let mut credential = store
            .get("openai")
            .map_err(provider_error)?
            .ok_or_else(not_authenticated)?;
        if credential.api_key().is_some() {
            return request_auth_from_credential(&credential, &self.oauth);
        }
        let refresh_at = unix_millis().saturating_add(5 * 60 * 1000);
        if credential
            .oauth_parts()
            .is_some_and(|oauth| oauth.expires <= refresh_at)
        {
            // Re-read after acquiring the lock: concurrent requests share one
            // refresh and use the credential persisted by its winner.
            let guard = self.refresh_lock.clone().lock_owned().await;
            credential = store
                .get("openai")
                .map_err(provider_error)?
                .ok_or_else(|| CoreError::Provider("OpenAI was logged out".into()))?;
            if let Some(current) = credential.oauth_parts()
                && current.expires <= refresh_at
            {
                if current.refresh.is_empty() {
                    return Err(CoreError::Provider(
                        "OpenAI login has no refresh token; run haarness auth login openai".into(),
                    ));
                }
                // OpenAI spends a refresh token the moment it answers. A
                // request that gives up mid-refresh (a Stop) must not drop the
                // new one before it is saved, or the next request sends the
                // spent one and the sign-in ends: the renewal runs on its own
                // task, which keeps the lock until it has saved, or until
                // OpenAI has not answered for `renewal_limit`.
                let (oauth, store, stale) = (self.oauth.clone(), store.clone(), credential.clone());
                let limit = self.renewal_limit;
                credential = haarness_account::renewals::spawn(async move {
                    let _guard = guard;
                    let current = stale.oauth_parts().expect("an OAuth credential");
                    let expected_refresh = current.refresh.to_owned();
                    let next = tokio::time::timeout(limit, oauth.refresh(current))
                        .await
                        .map_err(|_| {
                            CoreError::Provider(
                                "ChatGPT did not answer the sign-in renewal in time; try again"
                                    .into(),
                            )
                        })?
                        .map_err(provider_error)?;
                    if store
                        .replace_oauth_if_current("openai", &expected_refresh, &next)
                        .map_err(provider_error)?
                    {
                        Ok(next)
                    } else {
                        store
                            .get("openai")
                            .map_err(provider_error)?
                            .ok_or_else(|| CoreError::Provider("OpenAI was logged out".into()))
                    }
                })
                .await
                .map_err(|_| {
                    CoreError::Provider("Renewing the ChatGPT sign-in stopped unexpectedly".into())
                })??;
            }
        }
        request_auth_from_credential(&credential, &self.oauth)
    }

    fn uses_chatgpt_oauth(&self) -> bool {
        self.api_key.is_none()
            && self.auth_store.as_ref().is_some_and(|store| {
                store
                    .get("openai")
                    .ok()
                    .flatten()
                    .is_some_and(|credential| credential.oauth_parts().is_some())
            })
    }
}

enum RequestAuth {
    Api(String),
    OAuth {
        access: String,
        account_id: Option<String>,
        residency: Option<String>,
        endpoint: String,
    },
}

/// The public Responses API accepts `gpt-5.6` as the documented alias for
/// Sol, but the ChatGPT transport requires the concrete id. Stored threads
/// keep the alias; only the wire request is rewritten.
fn chatgpt_alias(model: &str) -> &str {
    match model {
        "gpt-5.6" => "gpt-5.6-sol",
        model => model,
    }
}

/// Static knowledge for the moments before the account catalog has loaded:
/// the model families the ChatGPT transport is known to serve. The catalog
/// replaces this as soon as it is read. Aliases are left out: `gpt-5.6` is
/// Sol by another name, and listing both shows "GPT-5.6 Sol" twice. A thread
/// already on the alias keeps working through `chatgpt_alias`.
fn chatgpt_fallback_models(preferred: &[ModelInfo]) -> Vec<ModelInfo> {
    preferred
        .iter()
        .filter(|model| model.id.starts_with("gpt-6") || model.id.starts_with("gpt-5.6"))
        .filter(|model| chatgpt_alias(&model.id) == model.id)
        .cloned()
        .collect()
}

/// Parse the ChatGPT account catalog. Hidden entries and models carrying a
/// retirement notice stay out of the advertised list but remain accepted, so
/// a thread already on one keeps working until the backend itself says no.
fn chatgpt_catalog(body: &Value) -> Option<ChatGptCatalog> {
    let mut accepted = HashSet::new();
    let mut listed = Vec::new();
    for entry in body["models"].as_array()? {
        let Some(slug) = entry["slug"].as_str().filter(|slug| !slug.is_empty()) else {
            continue;
        };
        accepted.insert(slug.to_owned());
        if entry["visibility"]
            .as_str()
            .is_some_and(|visibility| visibility != "list")
            || entry["upgrade"].is_object()
        {
            continue;
        }
        let efforts: Vec<String> = entry["supported_reasoning_levels"]
            .as_array()
            .map(|levels| {
                levels
                    .iter()
                    .filter_map(|level| level["effort"].as_str())
                    .filter(|effort| haarness_protocol::REASONING_EFFORTS.contains(effort))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let modality = |name: &str| {
            entry["input_modalities"]
                .as_array()
                .is_some_and(|values| values.iter().any(|value| value == name))
        };
        // Shared names win over the catalog's hyphenated spellings so a model
        // reads the same on an API key and on a subscription.
        let shared = haarness_protocol::model_display_name(slug, None);
        let display_name = if shared == slug {
            entry["display_name"]
                .as_str()
                .filter(|name| !name.trim().is_empty())
                .map_or_else(|| slug.to_owned(), tidy_catalog_name)
        } else {
            shared
        };
        let info = crate::catalog::annotate(
            "openai",
            ModelInfo {
                id: slug.to_owned(),
                display_name,
                reasoning: !efforts.is_empty(),
                reasoning_efforts: efforts,
                images: modality("image"),
                video: modality("video"),
                tools: true,
                context_window: entry["context_window"].as_u64().filter(|value| *value > 0),
                max_output_tokens: None,
                // Usage is covered by the subscription, not billed per token.
                pricing: Some(ModelPricing {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                    long_context: None,
                }),
            },
        );
        listed.push((entry["priority"].as_i64().unwrap_or(i64::MAX), info));
    }
    if accepted.is_empty() {
        // An empty catalog is a client-version mismatch, never an account
        // without models; keep whatever was known.
        return None;
    }
    listed.sort_by_key(|(priority, _)| *priority);
    Some(ChatGptCatalog {
        advertised: listed.into_iter().map(|(_, info)| info).collect(),
        accepted,
    })
}

/// The id that goes on the wire. On a ChatGPT sign-in the account catalog
/// is the authority once it has loaded: a model outside it fails here with
/// the alternatives instead of as an opaque 400 from the backend.
fn request_model<'a>(
    model: &'a str,
    auth: &RequestAuth,
    accepted: Option<&HashSet<String>>,
    advertised: &[ModelInfo],
) -> CoreResult<&'a str> {
    if !matches!(auth, RequestAuth::OAuth { .. }) {
        return Ok(model);
    }
    let wire = chatgpt_alias(model);
    match accepted {
        Some(accepted) if !accepted.contains(wire) => {
            let available: Vec<&str> = advertised.iter().map(|model| model.id.as_str()).collect();
            Err(CoreError::Provider(format!(
                "OpenAI model {model} is not available on this ChatGPT account. Available: {}. Choose one of those, or sign in with an OpenAI API key for the public Responses API.",
                if available.is_empty() {
                    "none".to_owned()
                } else {
                    available.join(", ")
                }
            )))
        }
        _ => Ok(wire),
    }
}

fn request_auth_from_credential(
    credential: &Credential,
    oauth: &OpenAiOAuthClient,
) -> CoreResult<RequestAuth> {
    if let Some(key) = credential.api_key() {
        return Ok(RequestAuth::Api(key.to_owned()));
    }
    let auth = credential
        .oauth_parts()
        .ok_or_else(|| CoreError::Provider("unsupported OpenAI credential".into()))?;
    Ok(RequestAuth::OAuth {
        access: auth.access.to_owned(),
        account_id: auth.account_id.map(str::to_owned),
        residency: openai_residency(auth.access),
        endpoint: oauth.codex_endpoint().to_owned(),
    })
}

#[async_trait]
impl ModelProvider for OpenAiProvider {
    async fn refresh_models(&self, force: bool) {
        // Each credential kind has its own catalog and cache, so switching
        // between a key and a sign-in never shows the other one's models.
        if self.discovery_key().is_some() {
            self.live_models
                .refresh(force, || self.fetch_api_models())
                .await;
        } else if self.uses_chatgpt_oauth() {
            self.chatgpt_models
                .refresh(force, || self.fetch_chatgpt_models())
                .await;
        }
    }
    async fn check_api_key(&self, key: &str) -> KeyCheck {
        let Some(base) = self.endpoint.strip_suffix("/responses") else {
            return KeyCheck::Unsupported;
        };
        let request = self.client.get(format!("{base}/models")).bearer_auth(key);
        crate::key_check::check("OpenAI", request).await
    }
    fn id(&self) -> &str {
        "openai"
    }

    fn info(&self) -> ProviderInfo {
        let models = if self.uses_chatgpt_oauth() {
            self.chatgpt_models
                .get(&chatgpt_fallback_models(&self.models))
        } else {
            self.live_models.get(&self.models)
        };
        ProviderInfo {
            id: "openai".into(),
            name: "OpenAI".into(),
            authenticated: self.api_key.is_some()
                || self
                    .auth_store
                    .as_ref()
                    .is_some_and(|store| store.get("openai").is_ok_and(|value| value.is_some())),
            models,
        }
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        let limits = request.parameters.limits;
        let auth = self.request_auth().await?;
        let subscription = matches!(auth, RequestAuth::OAuth { .. });
        let model = {
            let accepted = self.chatgpt_accepted.read().unwrap().clone();
            let advertised = if subscription {
                self.chatgpt_models.get(&[])
            } else {
                Vec::new()
            };
            request_model(&request.model, &auth, accepted.as_ref(), &advertised)?
        };
        let structured_output = request.output_schema;
        let mut requested_tools = request.tools;
        if let Some(schema) = structured_output.as_ref() {
            requested_tools.retain(|tool| tool.name != STRUCTURED_OUTPUT_TOOL_NAME);
            requested_tools.push(structured_output_tool(schema));
        }
        let tools: Vec<Value> = requested_tools.into_iter().map(|tool| {
            let mut value = json!({
                "type": "function", "name": tool.name, "description": tool.description, "parameters": tool.input_schema,
            });
            // Responses otherwise normalizes optional arguments into required fields.
            // Preserve native/MCP schemas; Haarness validates calls before permissions.
            if tool.name != STRUCTURED_OUTPUT_TOOL_NAME { value["strict"] = json!(false); }
            value
        }).collect();
        let mut input: Vec<Value> = Vec::new();
        for message in request.messages {
            let value = match message {
                ProviderMessage::Text { role, content } => {
                    json!({"role":role,"content":content})
                }
                ProviderMessage::Media {
                    role,
                    content,
                    attachments,
                } => {
                    let media = crate::media::load_all(&attachments)?;
                    let mut blocks = vec![json!({"type":"input_text","text":content})];
                    for item in media {
                        if item.is_image() {
                            blocks.push(json!({"type":"input_image","image_url":item.data_url()}));
                        } else if item.is_pdf() {
                            blocks.push(json!({
                                "type":"input_file",
                                "filename":item.filename,
                                "file_data":item.data_url()
                            }));
                        } else {
                            return Err(CoreError::Provider(
                                    "OpenAI Responses accepts Haarness image and PDF attachments; use Gemini for audio or video input"
                                        .into(),
                                ));
                        }
                    }
                    json!({"role":role,"content":blocks})
                }
                ProviderMessage::ToolCall {
                    call_id,
                    name,
                    arguments,
                    provider_metadata,
                } => {
                    if let Some(items) = provider_metadata
                        .get("openai_reasoning")
                        .and_then(Value::as_array)
                    {
                        input.extend(items.iter().cloned());
                    }
                    json!({"type":"function_call","call_id":call_id,"name":name,"arguments":serde_json::to_string(&arguments).unwrap_or_else(|_|"{}".into())})
                }
                ProviderMessage::ToolResult {
                    call_id,
                    output,
                    attachments,
                    ..
                } => {
                    if attachments.is_empty() {
                        json!({"type":"function_call_output","call_id":call_id,"output":output})
                    } else {
                        let mut blocks = vec![json!({"type":"input_text","text":output})];
                        for item in crate::media::load_all(&attachments)? {
                            if item.is_image() {
                                blocks.push(
                                    json!({"type":"input_image","image_url":item.data_url()}),
                                );
                            } else if item.is_pdf() {
                                blocks.push(json!({
                                    "type":"input_file",
                                    "filename":item.filename,
                                    "file_data":item.data_url()
                                }));
                            } else {
                                return Err(CoreError::Provider(
                                        "OpenAI Responses tool results support Haarness images and PDFs, not audio or video"
                                            .into(),
                                    ));
                            }
                        }
                        json!({"type":"function_call_output","call_id":call_id,"output":blocks})
                    }
                }
            };
            input.push(value);
        }
        let mut body = json!({"model": model, "instructions": request.system, "input": input, "tools": tools, "stream": true, "store": false, "include": ["reasoning.encrypted_content"]});
        if structured_output.is_some() {
            body["tool_choice"] = json!("required");
        }
        let sampling_allowed =
            !model.starts_with("gpt-6") || request.reasoning_effort.as_deref() == Some("none");
        if let Some(effort) = request.reasoning_effort {
            body["reasoning"] = json!({"effort": effort, "summary": "auto"});
        }
        let parameters = request.parameters;
        if let Some(key) = &parameters.cache_key {
            body["prompt_cache_key"] = json!(key);
        }
        // The ChatGPT transport rejects output limits and sampling controls
        // outright ("Unsupported parameter"); they only apply to the public API.
        if !subscription {
            if let Some(limit) = parameters.max_output_tokens.filter(|limit| *limit > 0) {
                body["max_output_tokens"] = json!(limit);
            }
            if let Some(temperature) = parameters.temperature.filter(|_| sampling_allowed) {
                body["temperature"] = json!(temperature);
            }
            if let Some(top_p) = parameters.top_p.filter(|_| sampling_allowed) {
                body["top_p"] = json!(top_p);
            }
        }
        let mut request = match &auth {
            RequestAuth::Api(key) => self.client.post(&self.endpoint).bearer_auth(key),
            RequestAuth::OAuth {
                access,
                account_id,
                residency,
                endpoint,
            } => {
                let mut request = self.client.post(endpoint).bearer_auth(access);
                if let Some(account_id) = account_id {
                    request = request.header("ChatGPT-Account-Id", account_id);
                }
                if let Some(residency) = residency {
                    request = request.header("x-openai-internal-codex-residency", residency);
                }
                // The backend routes a conversation to its cache by this header;
                // `prompt_cache_key` alone hits only now and then.
                if let Some(key) = &parameters.cache_key {
                    request = request.header("session_id", key);
                }
                request
            }
        };
        if !parameters.headers.is_empty() {
            let mut headers = reqwest::header::HeaderMap::new();
            crate::apply_request_headers(&mut headers, &parameters.headers);
            request = request.headers(headers);
        }
        request = request.json(&body);
        let response = crate::sse::send(request, limits).await?;
        let request_id = crate::request_id(response.headers());
        let mut events = json_stream(response, cancel.clone(), limits);
        Ok(Box::pin(try_stream! {
            // Reasoning streams as indexed parts, each a self-contained
            // summary or thought whose deltas carry no trailing break. Passed
            // straight through they run together mid-sentence, so a paragraph
            // break goes in wherever the part changes.
            let mut reasoning_part: Option<(bool, u64, u64)> = None;
            let mut reasoning_items: Vec<Value> = Vec::new();
            while let Some(event) = events.next().await {
                let event = event?;
                match event.get("type").and_then(Value::as_str).unwrap_or_default() {
                    "response.output_text.delta" => if let Some(delta) = event.get("delta").and_then(Value::as_str).filter(|delta| !delta.is_empty()) { yield ProviderEvent::TextDelta(delta.into()); },
                    kind @ ("response.reasoning_summary_text.delta" | "response.reasoning_text.delta") => if let Some(delta) = event.get("delta").and_then(Value::as_str).filter(|delta| !delta.is_empty()) {
                        // Two reasoning items in one response both number their
                        // parts from zero, so the owning output item is part of
                        // the identity.
                        let number = |key: &str| event.get(key).and_then(Value::as_u64);
                        let part = (
                            kind == "response.reasoning_summary_text.delta",
                            number("output_index").unwrap_or(0),
                            number("summary_index").or_else(|| number("content_index")).unwrap_or(0),
                        );
                        if reasoning_part.is_some_and(|previous| previous != part) {
                            yield ProviderEvent::ReasoningDelta("\n\n".into());
                        }
                        reasoning_part = Some(part);
                        yield ProviderEvent::ReasoningDelta(delta.into());
                    },
                    // The arguments.done event identifies the output item, but the public
                    // Responses stream does not include its call_id (and currently omits the
                    // documented name too). The completed output item is the authoritative
                    // function-call record and contains every field needed for continuation.
                    // A function call's arguments are held until its item is done.
                    "response.output_item.added" if event["item"]["type"] == "function_call" => {
                        yield ProviderEvent::ContentStarted;
                    }
                    "response.output_item.done" if event["item"]["type"] == "reasoning" => {
                        // Opaque reasoning is required for stateless tool continuation.
                        if event["item"]["encrypted_content"].is_string() {
                            reasoning_items.push(event["item"].clone());
                        }
                    }
                    "response.output_item.done" if event["item"]["type"] == "function_call" => {
                        let item = &event["item"];
                        let raw_arguments = item.get("arguments").and_then(Value::as_str).unwrap_or("{}");
                        let arguments = serde_json::from_str(raw_arguments).map_err(|error| CoreError::Provider(format!("OpenAI returned invalid function arguments: {error}")))?;
                        let call_id = string(item, "call_id");
                        let name = string(item, "name");
                        if call_id.is_empty() || name.is_empty() {
                            Err(CoreError::Provider("OpenAI returned an incomplete function call".into()))?;
                        }
                        if structured_output.is_some() && name == STRUCTURED_OUTPUT_TOOL_NAME {
                            yield ProviderEvent::StructuredOutput(arguments);
                        } else {
                            let provider_metadata = if reasoning_items.is_empty() { Value::Null } else { json!({"openai_reasoning":std::mem::take(&mut reasoning_items)}) };
                            yield ProviderEvent::ToolCall { call_id, name, arguments, provider_metadata };
                        }
                    }
                    "response.completed" | "response.incomplete" => {
                        if let Some(usage) = response_usage(&event) { yield usage; }
                        let reason = event["response"]["incomplete_details"]["reason"].as_str().unwrap_or(if event["type"] == "response.completed" { "completed" } else { "incomplete" });
                        crate::log_blocked_finish("openai", reason, request_id.as_deref(), event["response"]["id"].as_str());
                        yield ProviderEvent::Finished { reason: reason.into() };
                    }
                    // A failed response can still have been billed.
                    "response.failed" | "error" => {
                        if let Some(usage) = response_usage(&event) { yield usage; }
                        Err(CoreError::Provider(event.to_string()))?;
                    }
                    _ => {}
                }
            }
        }))
    }
}

/// The minor version of a GPT-5 family id (`gpt-5.4-mini` → 4); `gpt-5`,
/// `gpt-5-mini` and other unversioned ids have none.
fn gpt5_minor(id: &str) -> Option<u32> {
    let rest = id.strip_prefix("gpt-5")?;
    let digits: String = rest
        .strip_prefix(['.', '-'])?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    // A four-digit hyphenated suffix starts a snapshot date, not a minor version.
    if rest.starts_with('-') && digits.len() == 4 {
        return None;
    }
    digits.parse().ok()
}

/// Reasoning tiers the public Responses API accepts per OpenAI model family,
/// lightest first. GPT-5.1 replaced `minimal` with `none`, 5.2 added `xhigh`,
/// 5.6 added `max`, GPT-6 Astra and GPT-6.1 Sol dropped `none`; GPT-6 Sol/Luna retain it. Codex, Pro and chat variants keep
/// their own narrower sets. A request outside the set is a 400.
fn openai_reasoning_efforts(id: &str) -> Vec<&'static str> {
    let lower = id.to_ascii_lowercase();
    // A fine-tune is `ft:<family>:<org>:<suffix>`; only the family matters.
    let family = lower.strip_prefix("ft:").unwrap_or(&lower);
    let family = family.split(':').next().unwrap_or(family);
    let segment = |name: &str| family.split(['-', '.']).any(|part| part == name);
    if family.contains("deep-research") {
        return vec!["medium"];
    }
    if family.starts_with("gpt-6") {
        if family.starts_with("gpt-6-sol") || family.starts_with("gpt-6-luna") {
            return vec!["none", "low", "medium", "high", "xhigh", "max"];
        }
        return vec!["low", "medium", "high", "xhigh", "max"];
    }
    if !family.starts_with("gpt-5") {
        return vec!["low", "medium", "high"];
    }
    let minor = gpt5_minor(family);
    if segment("chat") {
        return Vec::new();
    }
    if segment("pro") {
        return if minor.is_some() {
            vec!["medium", "high", "xhigh"]
        } else {
            vec!["high"]
        };
    }
    if segment("codex") {
        return match minor {
            Some(version) if version >= 3 => vec!["none", "low", "medium", "high", "xhigh"],
            Some(version) if version >= 2 => vec!["low", "medium", "high", "xhigh"],
            _ if family.contains("codex-max") => vec!["low", "medium", "high", "xhigh"],
            _ => vec!["low", "medium", "high"],
        };
    }
    match minor {
        None => vec!["minimal", "low", "medium", "high"],
        Some(1) => vec!["none", "low", "medium", "high"],
        Some(version) if version >= 6 => vec!["none", "low", "medium", "high", "xhigh", "max"],
        Some(_) => vec!["none", "low", "medium", "high", "xhigh"],
    }
}

fn model(id: &str, reasoning: bool) -> ModelInfo {
    crate::catalog::annotate(
        "openai",
        ModelInfo {
            id: id.into(),
            display_name: id.into(),
            reasoning,
            reasoning_efforts: if reasoning {
                openai_reasoning_efforts(id)
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            } else {
                Vec::new()
            },
            images: true,
            video: false,
            tools: true,
            context_window: None,
            max_output_tokens: None,
            pricing: None,
        },
    )
}
pub(crate) fn preferred_models() -> Vec<ModelInfo> {
    vec![
        model("gpt-6.1-sol", true),
        model("gpt-6-astra", true),
        model("gpt-6-sol", true),
        model("gpt-6-luna", true),
        model("gpt-5.6", true),
        model("gpt-5.6-sol", true),
        model("gpt-5.6-terra", true),
        model("gpt-5.6-luna", true),
        model("gpt-5.5", true),
        model("gpt-5.4", true),
        model("gpt-5.4-mini", true),
        model("gpt-5.4-nano", true),
    ]
}
fn supports_responses(id: &str) -> bool {
    // The OpenAI model resource exposes no capability flags. Exclude known
    // non-text families and keep everything else so new conversational model
    // families, aliases, and user fine-tunes appear without a Haarness release.
    let unsupported = [
        "audio",
        "realtime",
        "gpt-live",
        "transcribe",
        "tts",
        "whisper",
        "embedding",
        "moderation",
        "dall-e",
        "gpt-image",
        "sora",
    ];
    !unsupported.iter().any(|kind| id.contains(kind))
}
fn string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .into()
}
/// The response's reported usage; `None` when it carries none (a proxy that
/// strips it), never an all-zero report.
fn response_usage(event: &Value) -> Option<ProviderEvent> {
    let usage = event["response"]["usage"].as_object()?;
    let usage = Value::Object(usage.clone());
    let details = |path: &str| usage.pointer(path).and_then(Value::as_u64).unwrap_or(0);
    Some(ProviderEvent::Usage {
        input_tokens: number(&usage, "input_tokens"),
        output_tokens: number(&usage, "output_tokens"),
        cache_read_tokens: details("/input_tokens_details/cached_tokens"),
        cache_write_tokens: details("/input_tokens_details/cache_write_tokens"),
    })
}

fn number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Like the other providers' errors, this one says how to connect.
fn not_authenticated() -> CoreError {
    CoreError::Provider(
        "OpenAI is not authenticated; run `haarness auth login openai`, or set OPENAI_API_KEY"
            .into(),
    )
}

fn provider_error(error: impl std::fmt::Display) -> CoreError {
    CoreError::Provider(error.to_string())
}

/// The subscription catalog spells new models `GPT-6.1-Sol`; the shared
/// names read `GPT-6.1 Sol`. Match them so a model the shared table does not
/// know yet still sits naturally beside the ones it does.
fn tidy_catalog_name(name: &str) -> String {
    let mut parts = name.split('-');
    match (parts.next(), parts.next()) {
        (Some("GPT"), Some(version)) if version.starts_with(|c: char| c.is_ascii_digit()) => {
            let rest: Vec<&str> = parts.collect();
            if rest.is_empty() {
                name.to_owned()
            } else {
                format!("GPT-{version} {}", rest.join(" "))
            }
        }
        _ => name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use axum::{
        Json, Router,
        body::Body,
        extract::State,
        http::{HeaderMap, Response, header::CONTENT_TYPE},
        routing::post,
    };
    use base64::Engine as _;
    use bytes::Bytes;
    use futures::StreamExt;
    use parking_lot::Mutex;
    use serde_json::{Value, json};
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    use haarness_core::{
        ModelProvider, ProviderEvent, ProviderMessage, ProviderRequest, ToolDefinition,
    };

    use super::{
        OpenAiProvider, RequestAuth, openai_reasoning_efforts, request_model, supports_responses,
    };

    type AuthHeaderCapture = (String, Option<String>, Option<String>);

    #[derive(Clone, Default)]
    struct FixtureState {
        requests: Arc<Mutex<Vec<Value>>>,
        auth_headers: Arc<Mutex<Vec<AuthHeaderCapture>>>,
        refreshes: Arc<AtomicUsize>,
    }

    #[test]
    fn discovery_keeps_new_text_families_and_fine_tunes() {
        assert!(supports_responses("daybreak-blue"));
        assert!(supports_responses("ft:gpt-5.6:org:project"));
        assert!(supports_responses("gpt-oss-120b"));
        assert!(!supports_responses("text-embedding-4-large"));
        assert!(!supports_responses("gpt-realtime-2.1"));
        assert!(!supports_responses("omni-moderation-latest"));
    }

    #[test]
    fn chatgpt_oauth_resolves_the_public_gpt_5_6_alias_to_sol() {
        let oauth = RequestAuth::OAuth {
            access: "access".into(),
            account_id: None,
            residency: None,
            endpoint: "https://chatgpt.example/responses".into(),
        };
        let api = RequestAuth::Api("key".into());

        assert_eq!(
            request_model("gpt-5.6", &oauth, None, &[]).unwrap(),
            "gpt-5.6-sol"
        );
        assert_eq!(
            request_model("gpt-5.6", &api, None, &[]).unwrap(),
            "gpt-5.6"
        );
        assert_eq!(
            request_model("gpt-5.6-terra", &oauth, None, &[]).unwrap(),
            "gpt-5.6-terra"
        );
        // Before the account catalog loads the backend has the final say.
        assert_eq!(
            request_model("gpt-5.4-nano", &oauth, None, &[]).unwrap(),
            "gpt-5.4-nano"
        );
        let accepted: std::collections::HashSet<String> =
            ["gpt-6-astra", "gpt-5.6-sol", "gpt-reserve"]
                .map(str::to_owned)
                .into();
        let advertised = super::preferred_models()
            .into_iter()
            .filter(|model| model.id == "gpt-6-astra")
            .collect::<Vec<_>>();
        let error = request_model("gpt-5.4-nano", &oauth, Some(&accepted), &advertised)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("gpt-5.4-nano is not available on this ChatGPT account")
                && error.contains("Available: gpt-6-astra"),
            "{error}"
        );
        // Accepted but hidden models still run for the threads that use them.
        assert_eq!(
            request_model("gpt-reserve", &oauth, Some(&accepted), &advertised).unwrap(),
            "gpt-reserve"
        );
        assert_eq!(
            request_model("gpt-5.6", &oauth, Some(&accepted), &advertised).unwrap(),
            "gpt-5.6-sol"
        );
        assert_eq!(
            request_model("gpt-5.4-nano", &api, Some(&accepted), &advertised).unwrap(),
            "gpt-5.4-nano"
        );
    }

    #[test]
    fn reasoning_tiers_follow_the_openai_model_family() {
        assert_eq!(
            openai_reasoning_efforts("gpt-5-2025-08-07"),
            ["minimal", "low", "medium", "high"]
        );
        for id in ["gpt-5-1", "gpt-5.1-2025-11-13"] {
            assert_eq!(
                openai_reasoning_efforts(id),
                ["none", "low", "medium", "high"]
            );
        }
        assert_eq!(
            openai_reasoning_efforts("gpt-6-astra"),
            ["low", "medium", "high", "xhigh", "max"]
        );
        // Probed 2026-10-10: GPT-6.1 Sol answers 400 to `none` and `temperature`.
        assert_eq!(
            openai_reasoning_efforts("gpt-6.1-sol"),
            ["low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(
            openai_reasoning_efforts("gpt-5.6-luna"),
            ["none", "low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(
            openai_reasoning_efforts("gpt-5.5"),
            ["none", "low", "medium", "high", "xhigh"]
        );
        assert_eq!(
            openai_reasoning_efforts("gpt-5.4-mini-2026-01-01"),
            ["none", "low", "medium", "high", "xhigh"]
        );
        assert_eq!(
            openai_reasoning_efforts("gpt-5.1"),
            ["none", "low", "medium", "high"]
        );
        assert_eq!(
            openai_reasoning_efforts("gpt-5-mini"),
            ["minimal", "low", "medium", "high"]
        );
        assert_eq!(
            openai_reasoning_efforts("gpt-5.3-codex"),
            ["none", "low", "medium", "high", "xhigh"]
        );
        assert_eq!(
            openai_reasoning_efforts("gpt-5.2-codex"),
            ["low", "medium", "high", "xhigh"]
        );
        assert_eq!(
            openai_reasoning_efforts("gpt-5-codex"),
            ["low", "medium", "high"]
        );
        assert_eq!(openai_reasoning_efforts("gpt-5-pro"), ["high"]);
        assert_eq!(
            openai_reasoning_efforts("gpt-5.2-pro"),
            ["medium", "high", "xhigh"]
        );
        assert!(openai_reasoning_efforts("gpt-5-chat-latest").is_empty());
        assert_eq!(
            openai_reasoning_efforts("ft:gpt-5.6:org:project"),
            ["none", "low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(
            openai_reasoning_efforts("o4-mini"),
            ["low", "medium", "high"]
        );
    }

    #[tokio::test]
    async fn usage_is_reported_only_when_the_response_carries_it() {
        async fn run(body: &'static str) -> Vec<Result<ProviderEvent, haarness_core::CoreError>> {
            let router = Router::new().route(
                "/responses",
                post(move || async move { ([("content-type", "text/event-stream")], body) }),
            );
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let provider = OpenAiProvider::for_test(format!("http://{address}/responses"), "test");
            let request = ProviderRequest {
                model: "gpt-5.5".into(),
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
        let usages = |events: &[Result<ProviderEvent, haarness_core::CoreError>]| {
            events
                .iter()
                .filter(|event| matches!(event, Ok(ProviderEvent::Usage { .. })))
                .count()
        };
        // A proxy that strips usage: no report at all, not an all-zero one.
        let events = run(
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
        )
        .await;
        assert_eq!(usages(&events), 0, "{events:?}");
        // A failed response that was billed keeps its usage, then fails;
        // cache writes are read when reported.
        let events = run(
            "data: {\"type\":\"response.failed\",\"response\":{\"usage\":{\"input_tokens\":70,\"output_tokens\":4,\"input_tokens_details\":{\"cached_tokens\":20,\"cache_write_tokens\":30}}}}\n\n",
        )
        .await;
        assert!(matches!(
            events.first(),
            Some(Ok(ProviderEvent::Usage {
                input_tokens: 70,
                output_tokens: 4,
                cache_read_tokens: 20,
                cache_write_tokens: 30
            }))
        ));
        assert!(events.last().is_some_and(Result::is_err));
    }

    #[tokio::test]
    async fn chatgpt_account_catalog_decides_oauth_models_and_requests() {
        async fn catalog(
            headers: HeaderMap,
            axum::extract::Query(query): axum::extract::Query<
                std::collections::HashMap<String, String>,
            >,
        ) -> Json<Value> {
            assert_eq!(headers["authorization"], "Bearer live-token");
            assert_eq!(headers["chatgpt-account-id"], "account-1");
            assert!(query.contains_key("client_version"));
            let levels = |efforts: &[&str]| -> Value {
                efforts
                    .iter()
                    .map(|effort| json!({"effort": effort}))
                    .collect()
            };
            Json(json!({"models": [
                {"slug":"gpt-5.6-luna","display_name":"GPT-5.6-Luna","visibility":"list","priority":8,"context_window":272_000,"input_modalities":["text","image"],"supported_reasoning_levels":levels(&["low","medium","high","xhigh","max"]),"upgrade":null},
                {"slug":"gpt-6-astra","display_name":"GPT-6-Astra","visibility":"list","priority":1,"context_window":272_000,"input_modalities":["text","image"],"supported_reasoning_levels":levels(&["low","medium","high","xhigh","max","ultra"]),"upgrade":null},
                {"slug":"gpt-reserve","display_name":"GPT-Reserve","visibility":"hide","priority":3,"supported_reasoning_levels":levels(&["low"])},
                {"slug":"gpt-5.6-sol","display_name":"GPT-5.6-Sol","visibility":"list","priority":4,"context_window":272_000,"input_modalities":["text","image"],"supported_reasoning_levels":levels(&["low","medium","high","xhigh","max","ultra"]),"upgrade":null},
                {"slug":"gpt-5.5","display_name":"GPT-5.5","visibility":"list","priority":12,"context_window":272_000,"input_modalities":["text"],"supported_reasoning_levels":levels(&["low","medium","high","xhigh"]),"upgrade":{"model":"gpt-5.6-sol","retirement_at":"2026-10-14T19:00:00Z"}},
                {"slug":"team-model","display_name":"Team model","visibility":"list","priority":20,"input_modalities":["text"],"supported_reasoning_levels":[]}
            ]}))
        }
        let state = FixtureState::default();
        let router = Router::new()
            .route("/models", axum::routing::get(catalog))
            .route("/responses", post(responses))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let root = std::env::temp_dir().join(format!(
            "haarness-chatgpt-catalog-{}-{}",
            std::process::id(),
            crate::auth::unix_millis()
        ));
        let store = Arc::new(crate::auth::CredentialStore::new(root.join("auth.json")));
        store
            .set(
                "openai",
                &crate::auth::Credential::oauth(
                    "live-token".into(),
                    "refresh-1".into(),
                    crate::auth::unix_millis() + 3_600_000,
                    Some("account-1".into()),
                ),
            )
            .unwrap();
        let provider = OpenAiProvider::for_oauth_test(
            format!("http://{address}/responses"),
            format!("http://{address}"),
            store.clone(),
        );
        // Until the catalog is read, only families the transport is known to
        // serve are offered.
        let fallback = provider.info().models;
        assert!(fallback.iter().any(|model| model.id == "gpt-6-astra"));
        assert_eq!(fallback[0].id, "gpt-6.1-sol");
        // GPT-5.4 never ran on ChatGPT; GPT-5.5 retires there on 2026-10-14.
        assert!(
            fallback
                .iter()
                .all(|model| !model.id.starts_with("gpt-5.4") && model.id != "gpt-5.5")
        );

        provider.refresh_models(true).await;
        let models = provider.info().models;
        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["gpt-6-astra", "gpt-5.6-sol", "gpt-5.6-luna", "team-model"],
            "listed models in priority order; hidden and retiring ones are dropped"
        );
        let astra = &models[0];
        assert_eq!(astra.display_name, "GPT-6 Astra");
        assert_eq!(super::tidy_catalog_name("GPT-6.1-Sol"), "GPT-6.1 Sol");
        assert_eq!(super::tidy_catalog_name("GPT-5.5"), "GPT-5.5");
        assert_eq!(super::tidy_catalog_name("GPT-Reserve"), "GPT-Reserve");
        assert_eq!(super::tidy_catalog_name("Team model"), "Team model");
        assert_eq!(
            astra.reasoning_efforts,
            ["low", "medium", "high", "xhigh", "max"],
            "tiers the request API rejects never reach a request"
        );
        assert_eq!(astra.context_window, Some(272_000));
        assert!(astra.images && !astra.video && astra.reasoning);
        assert_eq!(astra.pricing.map(|pricing| pricing.input), Some(0.0));
        let team = &models[3];
        assert_eq!(team.display_name, "Team model");
        assert!(!team.reasoning && team.reasoning_efforts.is_empty());

        let request = |model: &str| ProviderRequest {
            model: model.into(),
            system: "test".into(),
            messages: vec![ProviderMessage::Text {
                role: "user".into(),
                content: "hello".into(),
            }],
            tools: Vec::new(),
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters {
                max_output_tokens: Some(500),
                temperature: Some(0.2),
                top_p: Some(0.9),
                cache_key: Some("thread-1".into()),
                ..haarness_core::RequestParameters::default()
            },
            output_schema: None,
        };
        // Retiring models keep running for threads that already use them.
        let stream = provider
            .stream(request("gpt-5.5"), CancellationToken::new())
            .await
            .unwrap();
        assert!(stream.collect::<Vec<_>>().await.iter().all(Result::is_ok));
        let stream = provider
            .stream(request("gpt-5.6"), CancellationToken::new())
            .await
            .unwrap();
        assert!(stream.collect::<Vec<_>>().await.iter().all(Result::is_ok));
        let error = provider
            .stream(request("gpt-5.4-mini"), CancellationToken::new())
            .await
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(
            error.contains("gpt-5.4-mini is not available on this ChatGPT account")
                && error.contains("gpt-6-astra, gpt-5.6-sol, gpt-5.6-luna, team-model"),
            "{error}"
        );

        let requests = state.requests.lock();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["model"], "gpt-5.5");
        assert_eq!(requests[1]["model"], "gpt-5.6-sol");
        for request in requests.iter() {
            assert!(
                request.get("max_output_tokens").is_none()
                    && request.get("temperature").is_none()
                    && request.get("top_p").is_none(),
                "the ChatGPT transport rejects sampling and output limits: {request}"
            );
            // Without both the ChatGPT backend rarely reuses a prompt prefix.
            assert_eq!(request["prompt_cache_key"], "thread-1");
            assert_eq!(request["_session_id_header"], "thread-1");
        }
        drop(requests);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn oauth_refresh_is_single_flight_persisted_and_account_scoped() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/responses", post(responses))
            .route("/oauth/token", post(refresh_token))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let root = std::env::temp_dir().join(format!(
            "haarness-openai-refresh-{}-{}",
            std::process::id(),
            crate::auth::unix_millis()
        ));
        let store = Arc::new(crate::auth::CredentialStore::new(root.join("auth.json")));
        store
            .set(
                "openai",
                &crate::auth::Credential::oauth(
                    "expired".into(),
                    "refresh-1".into(),
                    1,
                    Some("account-1".into()),
                ),
            )
            .unwrap();
        let provider = Arc::new(OpenAiProvider::for_oauth_test(
            format!("http://{address}/responses"),
            format!("http://{address}"),
            store.clone(),
        ));
        let oauth_models = provider.info().models;
        assert!(oauth_models.iter().any(|model| model.id == "gpt-5.6-sol"));
        // The alias names the same model; listing it too showed Sol twice.
        assert!(oauth_models.iter().all(|model| model.id != "gpt-5.6"));
        assert!(oauth_models.iter().all(|model| model.id != "gpt-5.4-nano"));

        let request = || ProviderRequest {
            model: "gpt-test".into(),
            system: "test".into(),
            messages: vec![ProviderMessage::Text {
                role: "user".into(),
                content: "hello".into(),
            }],
            tools: Vec::new(),
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        };
        let (left, right) = tokio::join!(
            provider.stream(request(), CancellationToken::new()),
            provider.stream(request(), CancellationToken::new())
        );
        for stream in [left.unwrap(), right.unwrap()] {
            let events = stream.collect::<Vec<_>>().await;
            assert!(events.into_iter().all(|event| event.is_ok()));
        }

        assert_eq!(state.refreshes.load(Ordering::SeqCst), 1);
        let headers = state.auth_headers.lock();
        assert_eq!(headers.len(), 2);
        assert!(headers.iter().all(|(authorization, account, residency)| {
            authorization.starts_with("Bearer x.")
                && account.as_deref() == Some("account-2")
                && residency.as_deref() == Some("eu")
        }));
        let persisted = store.get("openai").unwrap().unwrap();
        let persisted = persisted.oauth_parts().unwrap();
        assert_eq!(persisted.refresh, "refresh-2");
        assert_eq!(persisted.account_id, Some("account-2"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_refresh_the_request_gave_up_on_is_still_saved() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/responses", post(responses))
            .route("/oauth/token", post(refresh_token))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let root = std::env::temp_dir().join(format!(
            "haarness-openai-cancelled-refresh-{}-{}",
            std::process::id(),
            crate::auth::unix_millis()
        ));
        let store = Arc::new(crate::auth::CredentialStore::new(root.join("auth.json")));
        store
            .set(
                "openai",
                &crate::auth::Credential::oauth(
                    "expired".into(),
                    "refresh-1".into(),
                    1,
                    Some("account-1".into()),
                ),
            )
            .unwrap();
        let provider = OpenAiProvider::for_oauth_test(
            format!("http://{address}/responses"),
            format!("http://{address}"),
            store.clone(),
        );
        let request = || ProviderRequest {
            model: "gpt-test".into(),
            system: "test".into(),
            messages: vec![ProviderMessage::Text {
                role: "user".into(),
                content: "hello".into(),
            }],
            tools: Vec::new(),
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        };
        // A Stop while OpenAI is answering the refresh (it takes 200 ms),
        // which returns at once.
        let started = std::time::Instant::now();
        let stopped = tokio::time::timeout(
            Duration::from_millis(20),
            provider.stream(request(), CancellationToken::new()),
        )
        .await;
        assert!(stopped.is_err());
        assert!(started.elapsed() < Duration::from_millis(150));
        // The next request uses the token the refresh saved.
        let stream = provider
            .stream(request(), CancellationToken::new())
            .await
            .unwrap();
        assert!(stream.collect::<Vec<_>>().await.iter().all(Result::is_ok));
        assert_eq!(state.refreshes.load(Ordering::SeqCst), 1);
        let persisted = store.get("openai").unwrap().unwrap();
        assert_eq!(persisted.oauth_parts().unwrap().refresh, "refresh-2");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A token endpoint that stops answering must not hold the refresh lock
    /// until restart: the renewal gives up after its limit and the next
    /// request renews again.
    #[tokio::test]
    async fn a_renewal_the_server_never_answers_frees_the_next_request() {
        let state = FixtureState::default();
        let stalled = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let gate = stalled.clone();
        let router = Router::new()
            .route("/responses", post(responses))
            .route(
                "/oauth/token",
                post(move |state: State<FixtureState>, body: String| {
                    let gate = gate.clone();
                    async move {
                        if gate.load(Ordering::SeqCst) {
                            tokio::time::sleep(Duration::from_secs(300)).await;
                        }
                        refresh_token(state, body).await
                    }
                }),
            )
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let root = std::env::temp_dir().join(format!(
            "haarness-openai-stalled-refresh-{}-{}",
            std::process::id(),
            crate::auth::unix_millis()
        ));
        let store = Arc::new(crate::auth::CredentialStore::new(root.join("auth.json")));
        store
            .set(
                "openai",
                &crate::auth::Credential::oauth("expired".into(), "refresh-1".into(), 1, None),
            )
            .unwrap();
        let mut provider = OpenAiProvider::for_oauth_test(
            format!("http://{address}/responses"),
            format!("http://{address}"),
            store.clone(),
        );
        provider.renewal_limit = Duration::from_millis(300);
        let request = || ProviderRequest {
            model: "gpt-test".into(),
            system: "test".into(),
            messages: vec![ProviderMessage::Text {
                role: "user".into(),
                content: "hello".into(),
            }],
            tools: Vec::new(),
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        };
        let stalled_request = tokio::time::timeout(
            Duration::from_secs(10),
            provider.stream(request(), CancellationToken::new()),
        )
        .await
        .expect("the renewal gave up within its limit");
        let error = stalled_request
            .err()
            .expect("the stalled renewal failed")
            .to_string();
        assert!(
            error.contains("did not answer the sign-in renewal in time"),
            "{error}"
        );
        stalled.store(false, Ordering::SeqCst);
        let stream = tokio::time::timeout(
            Duration::from_secs(10),
            provider.stream(request(), CancellationToken::new()),
        )
        .await
        .expect("the next request was not held by the lock")
        .unwrap();
        assert!(stream.collect::<Vec<_>>().await.iter().all(Result::is_ok));
        let _ = std::fs::remove_dir_all(root);
    }

    async fn responses(
        State(state): State<FixtureState>,
        headers: HeaderMap,
        Json(mut request): Json<Value>,
    ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
        if let Some(session) = headers
            .get("session_id")
            .and_then(|value| value.to_str().ok())
        {
            request["_session_id_header"] = json!(session);
        }
        state.requests.lock().push(request);
        state.auth_headers.lock().push((
            headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned(),
            headers
                .get("chatgpt-account-id")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            headers
                .get("x-openai-internal-codex-residency")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
        ));
        let stream = [
            r#"data: {"type":"response.output_item.added","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"list","arguments":""}}"#,
            r#"data: {"type":"response.function_call_arguments.done","item_id":"fc_1","arguments":"{\"path\":\".\"}"}"#,
            r#"data: {"type":"response.output_item.done","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"list","arguments":"{\"path\":\".\"}"}}"#,
            r#"data: {"type":"response.completed","response":{"usage":{"input_tokens":10,"output_tokens":4}}}"#,
        ]
        .join("\n\n")
            + "\n\n";
        ([(CONTENT_TYPE, "text/event-stream")], stream)
    }

    async fn refresh_token(State(state): State<FixtureState>, body: String) -> Json<Value> {
        assert!(body.contains("grant_type=refresh_token"));
        state.refreshes.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "chatgpt_account_id": "account-2",
                "chatgpt_compute_residency": "eu"
            }))
            .unwrap(),
        );
        Json(json!({
            "access_token": format!("x.{claims}.y"),
            "refresh_token": "refresh-2",
            "expires_in": 3600
        }))
    }

    async fn reasoning_responses(
        State(state): State<FixtureState>,
        Json(request): Json<Value>,
    ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
        state.requests.lock().push(request);
        let stream = [
            r#"data: {"type":"response.reasoning_summary_text.delta","summary_index":0,"delta":"Assessing security"}"#,
            r#"data: {"type":"response.reasoning_summary_text.delta","summary_index":0,"delta":" vulnerabilities"}"#,
            r#"data: {"type":"response.reasoning_summary_text.delta","summary_index":1,"delta":"Analyzing credential"}"#,
            r#"data: {"type":"response.reasoning_summary_text.delta","summary_index":1,"delta":" exfiltration risks"}"#,
            r#"data: {"type":"response.completed","response":{"usage":{"input_tokens":4,"output_tokens":2}}}"#,
        ]
        .join("\n\n")
            + "\n\n";
        ([(CONTENT_TYPE, "text/event-stream")], stream)
    }

    async fn delayed_text_responses(Json(_request): Json<Value>) -> Response<Body> {
        let body = async_stream::stream! {
            yield Ok::<_, std::convert::Infallible>(Bytes::from_static(
                b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"first\"}\n\n",
            ));
            tokio::time::sleep(Duration::from_millis(300)).await;
            yield Ok::<_, std::convert::Infallible>(Bytes::from_static(
                b"data: {\"type\":\"response.output_text.delta\",\"delta\":\" second\"}\n\n",
            ));
            yield Ok::<_, std::convert::Infallible>(Bytes::from_static(
                b"data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":2,\"output_tokens\":2}}}\n\n",
            ));
        };
        Response::builder()
            .header(CONTENT_TYPE, "text/event-stream")
            .body(Body::from_stream(body))
            .unwrap()
    }

    #[tokio::test]
    async fn openai_text_deltas_are_observable_before_the_response_completes() {
        let router = Router::new().route("/responses", post(delayed_text_responses));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = OpenAiProvider::for_test(format!("http://{address}/responses"), "test");
        let mut stream = provider
            .stream(
                ProviderRequest {
                    model: "gpt-test".into(),
                    system: "test".into(),
                    messages: vec![ProviderMessage::Text {
                        role: "user".into(),
                        content: "stream".into(),
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

        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), stream.next())
                .await
                .expect("the first network frame arrives")
                .expect("the provider stream stays open")
                .unwrap(),
            ProviderEvent::TextDelta(delta) if delta == "first"
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(75), stream.next())
                .await
                .is_err(),
            "the second delta must not be buffered together with the first"
        );
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), stream.next())
                .await
                .expect("the delayed network frame arrives")
                .expect("the provider stream stays open")
                .unwrap(),
            ProviderEvent::TextDelta(delta) if delta == " second"
        ));
    }

    /// Each summary part is a separate thought whose deltas carry no trailing
    /// break. Streamed straight through the transcript reads
    /// "…vulnerabilitiesAnalyzing credential…".
    #[tokio::test]
    async fn consecutive_reasoning_summary_parts_are_separated() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/responses", post(reasoning_responses))
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = OpenAiProvider::for_test(format!("http://{address}/responses"), "test");
        let events = provider
            .stream(
                ProviderRequest {
                    model: "gpt-test".into(),
                    system: "test".into(),
                    messages: vec![ProviderMessage::Text {
                        role: "user".into(),
                        content: "audit".into(),
                    }],
                    tools: Vec::new(),
                    reasoning_effort: Some("high".into()),
                    parameters: haarness_core::RequestParameters::default(),
                    output_schema: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        let reasoning: String = events
            .into_iter()
            .filter_map(|event| match event.unwrap() {
                ProviderEvent::ReasoningDelta(text) => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(
            reasoning,
            "Assessing security vulnerabilities\n\nAnalyzing credential exfiltration risks"
        );
    }

    async fn structured_responses(
        State(state): State<FixtureState>,
        Json(request): Json<Value>,
    ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
        state.requests.lock().push(request);
        let stream = [
            r#"data: {"type":"response.output_item.done","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"StructuredOutput","arguments":"{\"count\":2}"}}"#,
            r#"data: {"type":"response.completed","response":{"usage":{"input_tokens":8,"output_tokens":3}}}"#,
        ]
        .join("\n\n")
            + "\n\n";
        ([(CONTENT_TYPE, "text/event-stream")], stream)
    }

    #[tokio::test]
    async fn uses_completed_output_item_for_a_well_formed_tool_call() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/responses", post(responses))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = OpenAiProvider::for_test(format!("http://{address}/responses"), "test");
        assert!(
            provider
                .info()
                .models
                .iter()
                .any(|model| model.id == "gpt-5.4-nano")
        );
        let request = ProviderRequest {
            model: "gpt-test".into(),
            system: "test".into(),
            messages: vec![
                ProviderMessage::Text {
                    role: "user".into(),
                    content: "list files".into(),
                },
                ProviderMessage::ToolCall {
                    call_id: "previous_call".into(),
                    name: "read".into(),
                    arguments: json!({"path": "Cargo.toml"}),
                    provider_metadata: Value::Null,
                },
                ProviderMessage::ToolResult {
                    call_id: "previous_call".into(),
                    name: "read".into(),
                    output: "workspace".into(),
                    is_error: false,
                    attachments: vec![
                        "data:application/pdf;name=tool.pdf;base64,JVBERi0xLjQK".into(),
                    ],
                },
                ProviderMessage::Media {
                    role: "user".into(),
                    content: "inspect this".into(),
                    attachments: vec!["data:image/png;base64,iVBORw0KGgo=".into()],
                },
            ],
            tools: vec![ToolDefinition {
                name: "list".into(),
                description: "List files".into(),
                input_schema: json!({"type":"object","properties":{"path":{"type":"string"},"depth":{"type":"integer"}},"required":["path"]}),
            }],
            reasoning_effort: Some("medium".into()),
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        };

        let mut stream = provider
            .stream(request, CancellationToken::new())
            .await
            .unwrap();
        let events: Vec<_> = stream.by_ref().collect().await;
        let events: Vec<_> = events.into_iter().map(Result::unwrap).collect();
        // The call starts when its item is added, before its arguments.
        assert!(matches!(
            events.iter().find(|event| matches!(
                event,
                ProviderEvent::ContentStarted | ProviderEvent::ToolCall { .. }
            )),
            Some(ProviderEvent::ContentStarted)
        ));
        let calls: Vec<_> = events
            .into_iter()
            .filter_map(|event| match event {
                ProviderEvent::ToolCall {
                    call_id,
                    name,
                    arguments,
                    ..
                } => Some((call_id, name, arguments)),
                _ => None,
            })
            .collect();

        assert_eq!(
            calls,
            vec![("call_1".into(), "list".into(), json!({"path": "."}))]
        );
        let requests = state.requests.lock();
        assert_eq!(requests[0]["tools"][0]["strict"], false);
        assert_eq!(
            requests[0]["tools"][0]["parameters"]["required"],
            json!(["path"])
        );
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]["reasoning"],
            json!({"effort": "medium", "summary": "auto"}),
            "visible reasoning summaries stay enabled on the wire"
        );
        assert_eq!(requests[0]["input"][1]["call_id"], "previous_call");
        assert_eq!(requests[0]["input"][2]["call_id"], "previous_call");
        assert_eq!(requests[0]["input"][2]["output"][1]["type"], "input_file");
        assert_eq!(requests[0]["input"][2]["output"][1]["filename"], "tool.pdf");
        assert_eq!(
            requests[0]["input"][3]["content"][0],
            json!({"type": "input_text", "text": "inspect this"})
        );
        assert_eq!(requests[0]["input"][3]["content"][1]["type"], "input_image");
        assert_eq!(
            requests[0]["input"][3]["content"][1]["image_url"],
            "data:image/png;base64,iVBORw0KGgo="
        );
    }

    #[tokio::test]
    async fn structured_output_uses_a_required_schema_tool_and_a_distinct_event() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/responses", post(structured_responses))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = OpenAiProvider::for_test(format!("http://{address}/responses"), "test");
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {"count": {"type": "integer"}},
            "required": ["count"],
            "additionalProperties": false
        });
        let mut stream = provider
            .stream(
                ProviderRequest {
                    model: "gpt-test".into(),
                    system: "test".into(),
                    messages: vec![ProviderMessage::Text {
                        role: "user".into(),
                        content: "count".into(),
                    }],
                    tools: Vec::new(),
                    reasoning_effort: None,
                    parameters: haarness_core::RequestParameters::default(),
                    output_schema: Some(schema),
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let events = stream.by_ref().collect::<Vec<_>>().await;
        assert!(events.into_iter().any(
            |event| matches!(event.unwrap(), ProviderEvent::StructuredOutput(value) if value == json!({"count": 2}))
        ));
        let requests = state.requests.lock();
        assert_eq!(requests[0]["tool_choice"], "required");
        let tool = requests[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "StructuredOutput")
            .unwrap();
        assert_eq!(tool["parameters"]["type"], "object");
        assert!(tool["parameters"].get("$schema").is_none());
    }
    #[tokio::test]
    async fn discovery_uses_stored_api_credentials_and_live_limits() {
        async fn list(headers: HeaderMap) -> Json<Value> {
            assert_eq!(headers["authorization"], "Bearer fixture-only");
            Json(
                json!({"data":[{"id":"gpt-6-astra","display_name":"Live Astra","context_window":222_222,"max_output_tokens":12345,"reasoning_efforts":["low","max"]},{"id":"new-conversation-model"}]}),
            )
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/models", axum::routing::get(list)),
            )
            .await
            .unwrap()
        });
        let root = std::env::temp_dir().join(format!(
            "haarness-model-key-{}-{}",
            std::process::id(),
            crate::auth::unix_millis()
        ));
        let store = Arc::new(crate::auth::CredentialStore::new(root.join("auth.json")));
        store
            .set(
                "openai",
                &crate::auth::Credential::api("fixture-only".into()),
            )
            .unwrap();
        let provider = OpenAiProvider::for_oauth_test(
            format!("http://{address}/responses"),
            format!("http://{address}"),
            store,
        );
        provider.refresh_models(true).await;
        let info = provider.info();
        assert_eq!(info.models.len(), 2);
        assert_eq!(info.models[0].display_name, "Live Astra");
        assert_eq!(info.models[0].context_window, Some(222_222));
        assert_eq!(info.models[0].max_output_tokens, Some(12345));
        assert_eq!(info.models[0].reasoning_efforts, ["low", "max"]);
        assert!(
            info.models
                .iter()
                .any(|model| model.id == "new-conversation-model")
        );
        assert!(!info.models.iter().any(|model| model.id == "gpt-5.6"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
