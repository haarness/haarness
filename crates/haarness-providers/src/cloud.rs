//! Provider-specific cloud transports which reuse Haarness's native wire
//! parsers after applying each service's endpoint and authentication contract.

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use haarness_core::{
    CoreError, CoreResult, ModelProvider, ProviderMessage, ProviderRequest, ProviderStream,
};
use haarness_protocol::{ModelInfo, ProviderInfo};
use secrecy::{ExposeSecret as _, SecretString};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::{
    CompatibleInstance, CompatibleModelDefaults, CredentialStore, GeminiProvider, NativeInstance,
    OpenAiCompatibleProvider, ProviderHeaders,
};

enum CloudCredential {
    ApiKey(String),
    Bearer(String),
}

fn stored_credential(store: Option<&Arc<CredentialStore>>, id: &str) -> Option<CloudCredential> {
    let credential = store?.get(id).ok()??;
    if let Some(key) = credential.api_key() {
        return Some(CloudCredential::ApiKey(key.to_owned()));
    }
    credential
        .oauth_parts()
        .map(|oauth| CloudCredential::Bearer(oauth.access.to_owned()))
}

fn configured_models(
    instance: &NativeInstance,
    fallback: &[&str],
    catalog_provider: &str,
) -> Vec<ModelInfo> {
    let ids: Vec<_> = if instance.models.is_empty() {
        fallback.iter().map(|id| (*id).to_owned()).collect()
    } else {
        instance.models.clone()
    };
    ids.into_iter()
        .map(|id| {
            let defaults = instance.model_defaults;
            let known_reasoning = match catalog_provider {
                "openai" => id.starts_with('o') || id.starts_with("gpt-5"),
                "gemini" => id.starts_with("gemini-2.5") || crate::gemini::uses_thinking_level(&id),
                _ => false,
            };
            let mut model = ModelInfo {
                display_name: id.clone(),
                id,
                reasoning: defaults.reasoning.unwrap_or(known_reasoning),
                reasoning_efforts: Vec::new(),
                images: defaults
                    .images
                    .unwrap_or(matches!(catalog_provider, "openai" | "gemini")),
                video: defaults.video.unwrap_or(catalog_provider == "gemini"),
                tools: defaults.tools.unwrap_or(true),
                context_window: defaults.context_window,
                max_output_tokens: defaults.max_output_tokens,
                pricing: None,
            };
            if model.reasoning {
                model.reasoning_efforts = ["low", "medium", "high"].map(str::to_owned).to_vec();
            }
            crate::catalog::annotate(catalog_provider, model)
        })
        .collect()
}

fn headers_with(
    base: &ProviderHeaders,
    additions: impl IntoIterator<Item = (&'static str, String)>,
) -> CoreResult<ProviderHeaders> {
    let mut headers: BTreeMap<String, String> = base
        .map()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.to_string(), value.to_owned()))
        })
        .collect();
    headers.extend(
        additions
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value)),
    );
    ProviderHeaders::parse(headers).map_err(CoreError::Provider)
}

fn env_first(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
}

fn native_instance(
    id: &str,
    name: &str,
    base_url: String,
    api_key: Option<String>,
    models: &[&str],
) -> NativeInstance {
    NativeInstance {
        id: id.into(),
        name: name.into(),
        base_url,
        api_key,
        models: models.iter().map(|model| (*model).into()).collect(),
        discover_models: false,
        model_defaults: CompatibleModelDefaults::default(),
        assume_authenticated: false,
        headers: ProviderHeaders::default(),
    }
}

pub struct AzureProvider {
    instance: NativeInstance,
    auth_store: Option<Arc<CredentialStore>>,
    models: Vec<ModelInfo>,
}

impl AzureProvider {
    pub fn from_environment(auth_store: Option<Arc<CredentialStore>>) -> Self {
        let resource = std::env::var("AZURE_RESOURCE_NAME").ok();
        let base_url = std::env::var("AZURE_OPENAI_BASE_URL").unwrap_or_else(|_| {
            resource.map_or_else(String::new, |resource| {
                format!("https://{resource}.openai.azure.com/openai/v1")
            })
        });
        Self::new(
            native_instance(
                "azure",
                "Azure OpenAI",
                base_url,
                env_first(&["AZURE_OPENAI_API_KEY", "AZURE_API_KEY"]),
                &["gpt-4.1", "gpt-4.1-mini"],
            ),
            auth_store,
        )
    }

    pub fn new(instance: NativeInstance, auth_store: Option<Arc<CredentialStore>>) -> Self {
        let models = configured_models(&instance, &["gpt-4.1", "gpt-4.1-mini"], "openai");
        Self {
            instance,
            auth_store,
            models,
        }
    }

    async fn credential(&self) -> CoreResult<CloudCredential> {
        if let Some(key) = &self.instance.api_key {
            return Ok(CloudCredential::ApiKey(key.clone()));
        }
        if let Some(credential) = stored_credential(self.auth_store.as_ref(), &self.instance.id) {
            return Ok(credential);
        }
        let output = Command::new("az")
            .args([
                "account",
                "get-access-token",
                "--scope",
                "https://cognitiveservices.azure.com/.default",
                "--output",
                "json",
            ])
            .output()
            .await
            .map_err(|_| CoreError::Provider("Azure is not authenticated; set AZURE_OPENAI_API_KEY, run `haarness auth login azure`, or sign in with `az login`".into()))?;
        if !output.status.success() {
            return Err(CoreError::Provider(
                "Azure CLI could not obtain an access token; run `az login`".into(),
            ));
        }
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|error| {
            CoreError::Provider(format!("invalid Azure CLI token response: {error}"))
        })?;
        let token = value
            .get("accessToken")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| CoreError::Provider("Azure CLI returned no access token".into()))?;
        Ok(CloudCredential::Bearer(token.to_owned()))
    }
}

#[async_trait]
impl ModelProvider for AzureProvider {
    fn id(&self) -> &str {
        &self.instance.id
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.instance.id.clone(),
            name: self.instance.name.clone(),
            authenticated: self.instance.api_key.is_some()
                || stored_credential(self.auth_store.as_ref(), &self.instance.id).is_some(),
            models: self.models.clone(),
        }
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if self.instance.base_url.is_empty() {
            return Err(CoreError::Provider(
                "Azure endpoint is not configured; set AZURE_RESOURCE_NAME or AZURE_OPENAI_BASE_URL"
                    .into(),
            ));
        }
        let credential = self.credential().await?;
        let (api_key, headers) = match credential {
            CloudCredential::ApiKey(key) => (
                None,
                headers_with(&self.instance.headers, [("api-key", key)])?,
            ),
            CloudCredential::Bearer(token) => (Some(token), self.instance.headers.clone()),
        };
        OpenAiCompatibleProvider::new(CompatibleInstance {
            id: self.instance.id.clone(),
            name: self.instance.name.clone(),
            base_url: self.instance.base_url.clone(),
            api_key,
            models: self.models.iter().map(|model| model.id.clone()).collect(),
            discover_models: false,
            server_output_limit: false,
            replay_observed_reasoning: false,
            model_defaults: self.instance.model_defaults,
            assume_authenticated: true,
            headers,
        })
        .with_model_info(self.models.clone())
        .stream(request, cancel)
        .await
    }
}

pub struct CopilotProvider {
    instance: NativeInstance,
    auth_store: Option<Arc<CredentialStore>>,
    models: Vec<ModelInfo>,
    live_models: crate::live_models::LiveModels,
}

impl CopilotProvider {
    pub fn from_environment(auth_store: Option<Arc<CredentialStore>>) -> Self {
        let base_url = std::env::var("GITHUB_COPILOT_BASE_URL").unwrap_or_else(|_| {
            std::env::var("GITHUB_ENTERPRISE_URL").map_or_else(
                |_| "https://api.githubcopilot.com".into(),
                |enterprise| {
                    let domain = enterprise
                        .trim()
                        .trim_start_matches("https://")
                        .trim_start_matches("http://")
                        .trim_end_matches('/');
                    format!("https://copilot-api.{domain}")
                },
            )
        });
        let mut instance = native_instance(
            "github-copilot",
            "GitHub Copilot",
            base_url,
            env_first(&["GITHUB_COPILOT_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"]),
            &["gpt-4.1", "gpt-4o", "gpt-4o-mini"],
        );
        instance.discover_models = true;
        Self::new(instance, auth_store)
    }

    pub fn new(instance: NativeInstance, auth_store: Option<Arc<CredentialStore>>) -> Self {
        let models = configured_models(&instance, &["gpt-4.1", "gpt-4o", "gpt-4o-mini"], "openai");
        Self {
            instance,
            auth_store,
            models,
            live_models: crate::live_models::LiveModels::default(),
        }
    }

    fn token(&self) -> CoreResult<String> {
        if let Some(token) = &self.instance.api_key {
            return Ok(token.clone());
        }
        match stored_credential(self.auth_store.as_ref(), &self.instance.id) {
            Some(CloudCredential::ApiKey(token) | CloudCredential::Bearer(token)) => Ok(token),
            None => Err(CoreError::Provider(
                "GitHub Copilot is not authenticated; run `haarness auth login github-copilot` or set GITHUB_COPILOT_TOKEN".into(),
            )),
        }
    }

    fn request_headers(&self, vision: bool) -> CoreResult<ProviderHeaders> {
        let mut additions = vec![
            (
                "User-Agent",
                format!("haarness/{}", env!("CARGO_PKG_VERSION")),
            ),
            ("X-GitHub-Api-Version", "2026-06-01".into()),
            ("Openai-Intent", "conversation-edits".into()),
            ("x-initiator", "user".into()),
        ];
        if vision {
            additions.push(("Copilot-Vision-Request", "true".into()));
        }
        headers_with(&self.instance.headers, additions)
    }

    /// GitHub's Copilot model catalog includes endpoint capability metadata.
    /// Haarness exposes only models which advertise Chat Completions, since
    /// its Copilot adapter does not silently reroute Responses or Messages.
    pub async fn discover_models(self) -> Self {
        self.refresh_models(true).await;
        self
    }
    async fn fetch_models(&self) -> Option<Vec<ModelInfo>> {
        if !self.instance.discover_models {
            return None;
        }
        let Ok(token) = self.token() else {
            return None;
        };
        let Ok(headers) = self.request_headers(false) else {
            return None;
        };
        let response = reqwest::Client::new()
            .get(format!(
                "{}/models",
                self.instance.base_url.trim_end_matches('/')
            ))
            .headers(headers.map())
            .bearer_auth(token)
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .ok()
            .and_then(|response| response.error_for_status().ok());
        if let Some(response) = response
            && let Ok(body) = response.json::<serde_json::Value>().await
        {
            let entries = body
                .get("data")
                .and_then(serde_json::Value::as_array)
                .or_else(|| body.as_array())?;
            let discovered: Vec<_> = entries.iter().filter_map(copilot_model).collect();
            return Some(discovered);
        }
        None
    }
}

#[async_trait]
impl ModelProvider for CopilotProvider {
    async fn refresh_models(&self, force: bool) {
        self.live_models
            .refresh(force, || self.fetch_models())
            .await;
    }
    fn id(&self) -> &str {
        &self.instance.id
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.instance.id.clone(),
            name: self.instance.name.clone(),
            authenticated: self.instance.api_key.is_some()
                || stored_credential(self.auth_store.as_ref(), &self.instance.id).is_some(),
            models: self.live_models.get(&self.models),
        }
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        let vision = request
            .messages
            .iter()
            .any(|message| matches!(message, ProviderMessage::Media { .. }));
        let headers = self.request_headers(vision)?;
        OpenAiCompatibleProvider::new(CompatibleInstance {
            id: self.instance.id.clone(),
            name: self.instance.name.clone(),
            base_url: self.instance.base_url.clone(),
            api_key: Some(self.token()?),
            models: self
                .live_models
                .get(&self.models)
                .iter()
                .map(|model| model.id.clone())
                .collect(),
            discover_models: false,
            server_output_limit: false,
            replay_observed_reasoning: false,
            model_defaults: self.instance.model_defaults,
            assume_authenticated: false,
            headers,
        })
        .with_model_info(self.live_models.get(&self.models))
        .stream(request, cancel)
        .await
    }
}

fn copilot_model(entry: &serde_json::Value) -> Option<ModelInfo> {
    if entry
        .get("model_picker_enabled")
        .and_then(serde_json::Value::as_bool)
        == Some(false)
    {
        return None;
    }
    let endpoints = entry
        .get("supported_endpoints")
        .or_else(|| entry.pointer("/capabilities/supported_endpoints"))
        .and_then(serde_json::Value::as_array);
    if endpoints.is_some_and(|endpoints| {
        !endpoints.iter().any(|endpoint| {
            endpoint
                .as_str()
                .is_some_and(|endpoint| endpoint == "chat" || endpoint.contains("chat/completions"))
        })
    }) {
        return None;
    }
    let id = entry.get("id")?.as_str()?.to_owned();
    let supports = entry.pointer("/capabilities/supports");
    let reasoning = id.starts_with('o')
        || id.starts_with("gpt-5")
        || supports
            .and_then(|value| value.get("reasoning_effort"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
    Some(ModelInfo {
        display_name: entry
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&id)
            .to_owned(),
        id,
        reasoning,
        reasoning_efforts: if reasoning {
            ["low", "medium", "high"].map(str::to_owned).to_vec()
        } else {
            Vec::new()
        },
        images: supports
            .and_then(|value| value.get("vision"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        video: false,
        tools: supports
            .and_then(|value| value.get("tool_calls"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        // A listed 0 says nothing about the model.
        context_window: entry
            .pointer("/capabilities/limits/max_context_window_tokens")
            .and_then(serde_json::Value::as_u64)
            .filter(|window| *window > 0),
        max_output_tokens: entry
            .pointer("/capabilities/limits/max_output_tokens")
            .and_then(serde_json::Value::as_u64),
        pricing: None,
    })
}

pub struct VertexProvider {
    instance: NativeInstance,
    auth_store: Option<Arc<CredentialStore>>,
    models: Vec<ModelInfo>,
    token: Option<SecretString>,
}

impl VertexProvider {
    pub fn from_environment(auth_store: Option<Arc<CredentialStore>>) -> Self {
        let project = env_first(&[
            "GOOGLE_VERTEX_PROJECT",
            "GOOGLE_CLOUD_PROJECT",
            "GCP_PROJECT",
            "GCLOUD_PROJECT",
        ]);
        let location = env_first(&[
            "GOOGLE_VERTEX_LOCATION",
            "GOOGLE_CLOUD_LOCATION",
            "VERTEX_LOCATION",
        ])
        .unwrap_or_else(|| "us-central1".into());
        let base_url = std::env::var("GOOGLE_VERTEX_BASE_URL").unwrap_or_else(|_| {
            project.map_or_else(String::new, |project| {
                let host = if location == "global" {
                    "aiplatform.googleapis.com".into()
                } else {
                    format!("{location}-aiplatform.googleapis.com")
                };
                format!(
                    "https://{host}/v1/projects/{project}/locations/{location}/publishers/google"
                )
            })
        });
        Self::new(
            native_instance(
                "google-vertex",
                "Google Vertex AI",
                base_url,
                env_first(&["GOOGLE_OAUTH_ACCESS_TOKEN", "GOOGLE_ACCESS_TOKEN"]),
                &["gemini-2.5-pro", "gemini-2.5-flash"],
            ),
            auth_store,
        )
    }

    pub fn new(mut instance: NativeInstance, auth_store: Option<Arc<CredentialStore>>) -> Self {
        let token = instance.api_key.take().map(SecretString::from);
        let models =
            configured_models(&instance, &["gemini-2.5-pro", "gemini-2.5-flash"], "gemini");
        Self {
            instance,
            auth_store,
            models,
            token,
        }
    }

    async fn token(&self) -> CoreResult<String> {
        if let Some(token) = &self.token {
            return Ok(token.expose_secret().to_owned());
        }
        if let Some(CloudCredential::ApiKey(token) | CloudCredential::Bearer(token)) =
            stored_credential(self.auth_store.as_ref(), &self.instance.id)
        {
            return Ok(token);
        }
        for args in [
            ["auth", "application-default", "print-access-token"].as_slice(),
            ["auth", "print-access-token"].as_slice(),
        ] {
            if let Ok(output) = Command::new("gcloud").args(args).output().await
                && output.status.success()
            {
                let token = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                if !token.is_empty() {
                    return Ok(token);
                }
            }
        }
        Err(CoreError::Provider(
            "Vertex AI is not authenticated; run `gcloud auth application-default login`, run `haarness auth login google-vertex`, or set GOOGLE_OAUTH_ACCESS_TOKEN".into(),
        ))
    }
}

#[async_trait]
impl ModelProvider for VertexProvider {
    fn id(&self) -> &str {
        &self.instance.id
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.instance.id.clone(),
            name: self.instance.name.clone(),
            authenticated: self.token.is_some()
                || stored_credential(self.auth_store.as_ref(), &self.instance.id).is_some(),
            models: self.models.clone(),
        }
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if self.instance.base_url.is_empty() {
            return Err(CoreError::Provider(
                "Vertex endpoint is not configured; set GOOGLE_VERTEX_PROJECT or GOOGLE_VERTEX_BASE_URL"
                    .into(),
            ));
        }
        let token = self.token().await?;
        let headers = headers_with(
            &self.instance.headers,
            [("Authorization", format!("Bearer {token}"))],
        )?;
        GeminiProvider::new(NativeInstance {
            id: self.instance.id.clone(),
            name: self.instance.name.clone(),
            base_url: self.instance.base_url.clone(),
            api_key: None,
            models: self.models.iter().map(|model| model.id.clone()).collect(),
            discover_models: false,
            model_defaults: self.instance.model_defaults,
            assume_authenticated: true,
            headers,
        })
        .stream(request, cancel)
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, header::CONTENT_TYPE},
        routing::post,
    };
    use futures::StreamExt as _;
    use haarness_core::{ProviderEvent, ProviderMessage};
    use parking_lot::Mutex;
    use serde_json::Value;
    use tokio::net::TcpListener;

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<(HeaderMap, Value)>>>);

    async fn openai_fixture(
        State(capture): State<Capture>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
        capture.0.lock().push((headers, body));
        (
            [(CONTENT_TYPE, "text/event-stream")],
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n\ndata: [DONE]\n\n".into(),
        )
    }

    async fn vertex_fixture(
        State(capture): State<Capture>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
        capture.0.lock().push((headers, body));
        (
            [(CONTENT_TYPE, "text/event-stream")],
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hello\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":2,\"candidatesTokenCount\":1}}\n\n".into(),
        )
    }

    fn instance(id: &str, base_url: String, token: &str) -> NativeInstance {
        NativeInstance {
            id: id.into(),
            name: id.into(),
            base_url,
            api_key: Some(token.into()),
            models: vec!["test-model".into()],
            discover_models: false,
            model_defaults: CompatibleModelDefaults {
                reasoning: Some(true),
                tools: Some(true),
                ..Default::default()
            },
            assume_authenticated: false,
            headers: ProviderHeaders::default(),
        }
    }

    fn request() -> ProviderRequest {
        ProviderRequest {
            model: "test-model".into(),
            system: "system".into(),
            messages: vec![ProviderMessage::Text {
                role: "user".into(),
                content: "hi".into(),
            }],
            tools: Vec::new(),
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        }
    }

    async fn collect_text(provider: &dyn ModelProvider) -> Vec<ProviderEvent> {
        provider
            .stream(request(), CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    #[test]
    fn provider_specific_headers_preserve_custom_values() {
        let base = ProviderHeaders::parse(BTreeMap::from([("x-company".into(), "fixture".into())]))
            .unwrap();
        let headers = headers_with(&base, [("api-key", "secret".into())]).unwrap();
        assert_eq!(headers.map()["x-company"], "fixture");
        assert_eq!(headers.map()["api-key"], "secret");
    }

    #[test]
    fn copilot_catalog_filters_non_chat_models_and_maps_capabilities() {
        assert!(
            copilot_model(&serde_json::json!({
                "id":"messages-only",
                "supported_endpoints":["/v1/messages"]
            }))
            .is_none()
        );
        let model = copilot_model(&serde_json::json!({
            "id":"gpt-5-test",
            "name":"GPT 5 Test",
            "supported_endpoints":["/chat/completions"],
            "capabilities":{
                "supports":{"vision":true,"tool_calls":true},
                "limits":{"max_context_window_tokens":200_000,"max_output_tokens":32_000}
            }
        }))
        .unwrap();
        assert!(model.reasoning);
        assert!(model.images);
        assert_eq!(model.context_window, Some(200_000));
        assert_eq!(model.max_output_tokens, Some(32_000));
        let unknown = copilot_model(&serde_json::json!({
            "id":"gpt-5-test",
            "capabilities":{"limits":{"max_context_window_tokens":0}}
        }))
        .unwrap();
        assert_eq!(unknown.context_window, None, "a listed 0 is no window");

        let provider = CopilotProvider::new(
            instance("github-copilot", "https://example.test".into(), "token"),
            None,
        );
        assert_eq!(
            provider.request_headers(true).unwrap().map()["copilot-vision-request"],
            "true"
        );
    }

    #[tokio::test]
    async fn azure_uses_api_key_header_and_openai_stream_mapping() {
        let capture = Capture::default();
        let router = Router::new()
            .route("/chat/completions", post(openai_fixture))
            .with_state(capture.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = AzureProvider::new(
            instance("azure", format!("http://{address}"), "azure-secret"),
            None,
        );
        let events = collect_text(&provider).await;
        assert!(matches!(&events[0], ProviderEvent::TextDelta(text) if text == "hello"));
        let requests = capture.0.lock();
        assert_eq!(requests[0].0["api-key"], "azure-secret");
        assert!(!requests[0].0.contains_key("authorization"));
        assert_eq!(requests[0].1["model"], "test-model");
    }

    #[tokio::test]
    async fn copilot_adds_upstream_headers_and_maps_openai_stream() {
        let capture = Capture::default();
        let router = Router::new()
            .route("/chat/completions", post(openai_fixture))
            .with_state(capture.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = CopilotProvider::new(
            instance(
                "github-copilot",
                format!("http://{address}"),
                "github-token",
            ),
            None,
        );
        let events = collect_text(&provider).await;
        assert!(matches!(&events[0], ProviderEvent::TextDelta(text) if text == "hello"));
        let requests = capture.0.lock();
        assert_eq!(requests[0].0["authorization"], "Bearer github-token");
        assert_eq!(requests[0].0["x-github-api-version"], "2026-06-01");
        assert_eq!(requests[0].0["openai-intent"], "conversation-edits");
        assert_eq!(requests[0].0["x-initiator"], "user");
    }

    /// Streams reasoning in `reasoning`, as vLLM does, then calls `read`.
    async fn reasoning_fixture(
        State(capture): State<Capture>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
        capture.0.lock().push((headers, body));
        (
            [(CONTENT_TYPE, "text/event-stream")],
            "data: {\"choices\":[{\"delta\":{\"reasoning\":\"Look first.\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"read\",\"arguments\":\"{}\"}}]}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n".into(),
        )
    }

    #[tokio::test]
    async fn azure_and_copilot_never_get_reasoning_sent_back() {
        let capture = Capture::default();
        let router = Router::new()
            .route("/chat/completions", post(reasoning_fixture))
            .with_state(capture.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let base = format!("http://{address}");
        let providers: Vec<(&str, Box<dyn ModelProvider>)> = vec![
            (
                "azure",
                Box::new(AzureProvider::new(
                    instance("azure", base.clone(), "key"),
                    None,
                )),
            ),
            (
                "github-copilot",
                Box::new(CopilotProvider::new(
                    instance("github-copilot", base, "token"),
                    None,
                )),
            ),
        ];
        for (id, provider) in providers {
            let events = collect_text(provider.as_ref()).await;
            let metadata = events.iter().find_map(|event| match event {
                ProviderEvent::ToolCall {
                    provider_metadata, ..
                } => Some(provider_metadata.clone()),
                _ => None,
            });
            assert_eq!(
                metadata,
                Some(Value::Null),
                "{id} keeps no reasoning to send back"
            );
            // Even a call tagged as its own carries no reasoning field back.
            let mut history = request();
            history.messages.extend([
                ProviderMessage::ToolCall {
                    call_id: "call_a".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({}),
                    provider_metadata: serde_json::json!({"reasoning": "Look first.", "reasoning_provider": id}),
                },
                ProviderMessage::ToolResult {
                    call_id: "call_a".into(),
                    name: "read".into(),
                    output: "contents".into(),
                    is_error: false,
                    attachments: Vec::new(),
                },
            ]);
            provider
                .stream(history, CancellationToken::new())
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await;
            let body = capture.0.lock().last().unwrap().1.clone();
            let assistant = &body["messages"][2];
            assert_eq!(assistant["tool_calls"][0]["id"], "call_a", "{id}: {body}");
            assert!(assistant.get("reasoning").is_none(), "{id}: {assistant}");
            assert!(
                assistant.get("reasoning_content").is_none(),
                "{id}: {assistant}"
            );
        }
    }

    #[tokio::test]
    async fn vertex_uses_bearer_auth_and_native_gemini_stream_mapping() {
        let capture = Capture::default();
        let router = Router::new()
            .route(
                "/models/test-model:streamGenerateContent",
                post(vertex_fixture),
            )
            .with_state(capture.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = VertexProvider::new(
            instance("google-vertex", format!("http://{address}"), "google-token"),
            None,
        );
        let events = collect_text(&provider).await;
        assert!(matches!(&events[0], ProviderEvent::TextDelta(text) if text == "hello"));
        let requests = capture.0.lock();
        assert_eq!(requests[0].0["authorization"], "Bearer google-token");
        assert!(!requests[0].0.contains_key("x-goog-api-key"));
        assert_eq!(requests[0].1["contents"][0]["parts"][0]["text"], "hi");
    }
}
