use async_stream::try_stream;
use async_trait::async_trait;
use futures::StreamExt;
use haarness_core::{
    CoreError, CoreResult, KeyCheck, ModelProvider, ProviderEvent, ProviderMessage,
    ProviderRequest, ProviderStream, STRUCTURED_OUTPUT_TOOL_NAME, structured_output_tool,
};
use haarness_protocol::{ModelInfo, ProviderInfo};
use reqwest::header::{HeaderName, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::auth::CredentialStore;
use crate::sse::json_stream;

const API_KEY_ENV_VARS: [&str; 3] = [
    "GOOGLE_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_GENERATIVE_AI_API_KEY",
];

fn api_key_from(mut lookup: impl FnMut(&str) -> Option<String>) -> Option<SecretString> {
    API_KEY_ENV_VARS
        .into_iter()
        .find_map(|name| lookup(name).map(SecretString::from))
}

pub struct GeminiProvider {
    client: reqwest::Client,
    id: String,
    name: String,
    base_url: String,
    api_key: Option<SecretString>,
    auth_store: Option<Arc<CredentialStore>>,
    models: Vec<ModelInfo>,
    live_models: crate::live_models::LiveModels,
    discover_models: bool,
    assume_authenticated: bool,
    headers: crate::ProviderHeaders,
}
impl GeminiProvider {
    #[must_use]
    pub fn from_environment() -> Self {
        Self {
            client: reqwest::Client::new(),
            id: "gemini".into(),
            name: "Google Gemini".into(),
            base_url: std::env::var("GEMINI_BASE_URL")
                .unwrap_or_else(|_| "https://generativelanguage.googleapis.com/v1beta".into()),
            api_key: api_key_from(|name| std::env::var(name).ok()),
            auth_store: None,
            models: preferred_models(),
            live_models: crate::live_models::LiveModels::default(),
            discover_models: true,
            assume_authenticated: false,
            headers: crate::ProviderHeaders::default(),
        }
    }
    #[must_use]
    pub fn new(instance: crate::NativeInstance) -> Self {
        let defaults = instance.model_defaults;
        Self {
            client: reqwest::Client::new(),
            id: instance.id,
            name: instance.name,
            base_url: instance.base_url.trim_end_matches('/').to_owned(),
            api_key: instance.api_key.map(SecretString::from),
            auth_store: None,
            models: instance
                .models
                .into_iter()
                .map(|id| configured_model(&id, defaults))
                .collect(),
            live_models: crate::live_models::LiveModels::default(),
            discover_models: instance.discover_models,
            assume_authenticated: instance.assume_authenticated,
            headers: instance.headers,
        }
    }
    #[cfg(test)]
    #[must_use]
    pub fn with_api_key(mut self, key: Option<String>) -> Self {
        if self.api_key.is_none()
            && let Some(key) = key
        {
            self.api_key = Some(SecretString::from(key));
        }
        self
    }
    #[must_use]
    pub fn with_auth_store(mut self, store: Option<Arc<CredentialStore>>) -> Self {
        self.auth_store = store;
        self
    }
    fn resolved_api_key(&self) -> Option<String> {
        self.api_key
            .as_ref()
            .map(|key| key.expose_secret().to_owned())
            .or_else(|| {
                self.auth_store
                    .as_ref()?
                    .get(&self.id)
                    .ok()
                    .flatten()?
                    .api_key()
                    .map(str::to_owned)
            })
    }
    pub async fn discover_models(self) -> Self {
        self.refresh_models(true).await;
        self
    }
    async fn fetch_models(&self) -> Option<Vec<ModelInfo>> {
        if !self.discover_models {
            return None;
        }
        let key = self.resolved_api_key();
        if key.is_none() && !self.assume_authenticated && self.headers.is_empty() {
            return None;
        }
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));
        let mut page_token: Option<String> = None;
        let mut models = Vec::new();
        for _ in 0..10 {
            let Ok(mut page_url) = url::Url::parse(&url) else {
                return None;
            };
            page_url.query_pairs_mut().append_pair("pageSize", "1000");
            if let Some(page_token) = &page_token {
                page_url
                    .query_pairs_mut()
                    .append_pair("pageToken", page_token);
            }
            let mut headers = self.headers.map();
            if let Some(key) = &key {
                let Ok(value) = HeaderValue::from_str(key) else {
                    return None;
                };
                headers.insert(HeaderName::from_static("x-goog-api-key"), value);
            }
            let request = self
                .client
                .get(page_url)
                .headers(headers)
                .timeout(std::time::Duration::from_secs(3));
            let response = request
                .send()
                .await
                .ok()
                .and_then(|response| response.error_for_status().ok())?;
            let Ok(body) = response.json::<Value>().await else {
                return None;
            };
            models.extend(
                body["models"]
                    .as_array()?
                    .iter()
                    .filter_map(|entry| discovered_model_for(entry, self.id == "gemini")),
            );
            let Some(cursor) = body["nextPageToken"]
                .as_str()
                .filter(|token| !token.is_empty())
            else {
                return Some(crate::merge_discovered_models(self.models.clone(), models));
            };
            if page_token.as_deref() == Some(cursor) {
                return None;
            }
            page_token = Some(cursor.to_owned());
        }
        None
    }

    #[cfg(test)]
    fn for_test(base_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            id: "gemini".into(),
            name: "Google Gemini".into(),
            base_url,
            api_key: Some(SecretString::from("test")),
            auth_store: None,
            models: preferred_models(),
            live_models: crate::live_models::LiveModels::default(),
            discover_models: true,
            assume_authenticated: false,
            headers: crate::ProviderHeaders::default(),
        }
    }
}

#[async_trait]
impl ModelProvider for GeminiProvider {
    async fn refresh_models(&self, force: bool) {
        self.live_models
            .refresh(force, || self.fetch_models())
            .await;
    }
    async fn check_api_key(&self, key: &str) -> KeyCheck {
        let Ok(key) = HeaderValue::from_str(key) else {
            return KeyCheck::Unsupported;
        };
        let mut headers = self.headers.map();
        headers.insert(HeaderName::from_static("x-goog-api-key"), key);
        let request = self
            .client
            .get(format!(
                "{}/models?pageSize=1",
                self.base_url.trim_end_matches('/')
            ))
            .headers(headers);
        crate::key_check::check(&self.name, request).await
    }
    fn id(&self) -> &str {
        &self.id
    }
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            authenticated: self.resolved_api_key().is_some()
                || self.assume_authenticated
                || !self.headers.is_empty(),
            models: self.live_models.get(&self.models),
        }
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        let key = self.resolved_api_key();
        if key.is_none() && !self.assume_authenticated && self.headers.is_empty() {
            return Err(CoreError::Provider(format!(
                "{} API key is not configured; store a credential for {} or set its api_key_env",
                self.name, self.id
            )));
        }
        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            self.base_url.trim_end_matches('/'),
            request.model
        );
        let contents = gemini_contents(request.messages)?;
        let structured_output = request.output_schema;
        let mut requested_tools = request.tools;
        if let Some(schema) = structured_output.as_ref() {
            requested_tools.retain(|tool| tool.name != STRUCTURED_OUTPUT_TOOL_NAME);
            requested_tools.push(structured_output_tool(schema));
        }
        let forced = structured_output.is_some();
        let declarations: Vec<_> = requested_tools
            .into_iter()
            .map(|t| {
                let schema = if forced {
                    without_max_items(t.input_schema)
                } else {
                    t.input_schema
                };
                json!({"name":t.name,"description":t.description,"parametersJsonSchema":schema})
            })
            .collect();
        let mut body = json!({"systemInstruction":{"parts":[{"text":request.system}]},"contents":contents,"tools":[{"functionDeclarations":declarations}]});
        if structured_output.is_some() {
            body["toolConfig"] = json!({"functionCallingConfig":{"mode":"ANY"}});
        }
        if let Some(effort) = request.reasoning_effort.as_deref() {
            if request.model.starts_with("gemini-3") {
                let level = match effort {
                    "none" | "minimal"
                        if gemini_reasoning_efforts(&request.model).contains(&"minimal") =>
                    {
                        "minimal"
                    }
                    "none" | "minimal" | "low" => "low",
                    "medium" => "medium",
                    _ => "high",
                };
                body["generationConfig"] =
                    json!({"thinkingConfig":{"thinkingLevel":level,"includeThoughts":true}});
            } else {
                let budget: i64 = match effort {
                    "none" | "minimal" => 0,
                    "low" => 2048,
                    "high" => 24_576,
                    _ => 8192,
                };
                if request.model.starts_with("gemini-2.5") || budget > 0 {
                    // Gemini 2.5 Pro cannot disable thinking (minimum 128).
                    let budget = if request.model.starts_with("gemini-2.5-pro") {
                        budget.max(128)
                    } else {
                        budget
                    };
                    body["generationConfig"] =
                        json!({"thinkingConfig":{"thinkingBudget":budget,"includeThoughts":true}});
                }
            }
        }
        let parameters = &request.parameters;
        if let Some(temperature) = parameters.temperature {
            body["generationConfig"]["temperature"] = json!(temperature);
        }
        if let Some(top_p) = parameters.top_p {
            body["generationConfig"]["topP"] = json!(top_p);
        }
        if let Some(top_k) = parameters.top_k {
            body["generationConfig"]["topK"] = json!(top_k);
        }
        if let Some(limit) = parameters.max_output_tokens.filter(|limit| *limit > 0) {
            body["generationConfig"]["maxOutputTokens"] = json!(limit);
        }
        let mut headers = self.headers.map();
        crate::apply_request_headers(&mut headers, &parameters.headers);
        if let Some(key) = key {
            headers.insert(
                HeaderName::from_static("x-goog-api-key"),
                HeaderValue::from_str(&key).map_err(provider_error)?,
            );
        }
        let response = self
            .client
            .post(url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(crate::sse::transport_error)?;
        let mut events = json_stream(response, cancel);
        Ok(Box::pin(try_stream! {
            let mut call_index = 0_u64;
            while let Some(event)=events.next().await {
                let event=event?;
                if let Some(parts)=event.pointer("/candidates/0/content/parts").and_then(Value::as_array) {
                    for part in parts {
                        if let Some(text)=part.get("text").and_then(Value::as_str).filter(|text| !text.is_empty()) {
                            if part.get("thought").and_then(Value::as_bool).unwrap_or(false) {
                                yield ProviderEvent::ReasoningDelta(text.into());
                            } else {
                                yield ProviderEvent::TextDelta(text.into());
                            }
                        }
                        if let Some(call)=part.get("functionCall") {
                            call_index += 1;
                            let response_id = event.get("responseId").and_then(Value::as_str).unwrap_or("response");
                            let mut provider_metadata = json!({});
                            if let Some(signature) = part.get("thoughtSignature") { provider_metadata["thoughtSignature"] = signature.clone(); }
                            if let Some(id) = call.get("id") { provider_metadata["functionCallId"] = id.clone(); }
                            if provider_metadata.as_object().is_some_and(|value| value.is_empty()) { provider_metadata = Value::Null; }
                            let name = call.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
                            let arguments = call.get("args").cloned().unwrap_or(Value::Null);
                            if structured_output.is_some() && name == STRUCTURED_OUTPUT_TOOL_NAME {
                                yield ProviderEvent::StructuredOutput(arguments);
                            } else {
                                yield ProviderEvent::ToolCall {
                                    call_id: call.get("id").and_then(Value::as_str).map_or_else(|| format!("gemini-{response_id}-{call_index}"), str::to_owned),
                                    name,
                                    arguments,
                                    provider_metadata,
                                };
                            }
                        }
                    }
                }
                if let Some(usage)=event.get("usageMetadata") { yield ProviderEvent::Usage { input_tokens:usage["promptTokenCount"].as_u64().unwrap_or(0),output_tokens:usage["candidatesTokenCount"].as_u64().unwrap_or(0)+usage["thoughtsTokenCount"].as_u64().unwrap_or(0),cache_read_tokens:usage["cachedContentTokenCount"].as_u64().unwrap_or(0),cache_write_tokens:0 }; }
                if event.pointer("/candidates/0/finishReason").is_some() { yield ProviderEvent::Finished { reason:event.pointer("/candidates/0/finishReason").and_then(Value::as_str).unwrap_or("completed").into() }; }
                if let Some(reason) = event.pointer("/promptFeedback/blockReason").and_then(Value::as_str).filter(|reason| *reason != "BLOCK_REASON_UNSPECIFIED") {
                    yield ProviderEvent::Finished { reason: if haarness_core::ProviderEvent::finish_error(reason).is_some() { reason.into() } else { "SAFETY".into() } };
                }
            }
        }))
    }
}
/// Forced function calling (`ANY`) compiles every declaration into a decoding
/// grammar, and Gemini answers 400 "invalid argument" to an array bound over
/// complex items (`browser_fill_form`'s 100 `anyOf` fields). The runtime still
/// validates each call against the full schema.
fn without_max_items(mut schema: Value) -> Value {
    fn strip(value: &mut Value) {
        match value {
            Value::Object(object) => {
                object.remove("maxItems");
                object.values_mut().for_each(strip);
            }
            Value::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    strip(&mut schema);
    schema
}

fn gemini_reasoning_efforts(id: &str) -> Vec<&'static str> {
    if id.starts_with("gemini-3.8-flash")
        || id.starts_with("gemini-3.7-flash")
        || id.starts_with("gemini-3.1-pro")
    {
        vec!["low", "medium", "high"]
    } else if id.starts_with("gemini-3-pro") || id.starts_with("gemini-2.5-pro") {
        vec!["low", "high"]
    } else {
        vec!["minimal", "low", "medium", "high"]
    }
}

fn model(id: &str) -> ModelInfo {
    crate::catalog::annotate(
        "gemini",
        ModelInfo {
            id: id.into(),
            display_name: id.into(),
            reasoning: true,
            reasoning_efforts: gemini_reasoning_efforts(id)
                .into_iter()
                .map(str::to_owned)
                .collect(),
            images: true,
            video: true,
            tools: true,
            context_window: None,
            max_output_tokens: None,
            pricing: None,
        },
    )
}

fn configured_model(id: &str, defaults: crate::CompatibleModelDefaults) -> ModelInfo {
    let mut info = model(id);
    if let Some(value) = defaults.reasoning {
        info.reasoning = value;
        if !value {
            info.reasoning_efforts.clear();
        }
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
    if defaults.context_window.is_some() {
        info.context_window = defaults.context_window;
    }
    if defaults.max_output_tokens.is_some() {
        info.max_output_tokens = defaults.max_output_tokens;
    }
    info
}

#[cfg(test)]
fn discovered_model(entry: &Value) -> Option<ModelInfo> {
    discovered_model_for(entry, true)
}

fn discovered_model_for(entry: &Value, require_gemini_prefix: bool) -> Option<ModelInfo> {
    let methods = entry["supportedGenerationMethods"].as_array()?;
    if !methods.iter().any(|method| method == "generateContent") {
        return None;
    }
    let id = entry["name"]
        .as_str()?
        .trim_start_matches("models/")
        .to_owned();
    if !supports_harness_chat(&id, require_gemini_prefix) {
        return None;
    }
    let mut info = model(&id);
    if let Some(name) = entry["displayName"]
        .as_str()
        .filter(|name| !name.is_empty())
    {
        name.clone_into(&mut info.display_name);
    }
    if let Some(limit) = entry["inputTokenLimit"].as_u64().filter(|limit| *limit > 0) {
        info.context_window = Some(limit);
    }
    if let Some(limit) = entry["outputTokenLimit"]
        .as_u64()
        .filter(|limit| *limit > 0)
    {
        info.max_output_tokens = Some(limit);
    }
    if let Some(thinking) = entry["thinking"].as_bool() {
        info.reasoning = thinking;
        if !thinking {
            info.reasoning_efforts.clear();
        }
    }
    Some(info)
}

fn supports_harness_chat(id: &str, require_gemini_prefix: bool) -> bool {
    let unsupported = [
        "-image",
        // Image models named after the brand rather than `-image`.
        "nano-banana",
        "-tts",
        "-live",
        "native-audio",
        "omni-flash",
        "gemini-omni-",
        "-transcribe",
        // Specialty models that need their own tools; the Cloud router skips them too.
        "robotics",
        "computer-use",
    ];
    (!require_gemini_prefix || id.starts_with("gemini-"))
        && !id.is_empty()
        && !unsupported.iter().any(|kind| id.contains(kind))
}

pub(crate) fn preferred_models() -> Vec<ModelInfo> {
    vec![
        model("gemini-3.8-flash"),
        model("gemini-3.7-flash"),
        model("gemini-3.6-flash"),
        model("gemini-3.5-flash"),
        model("gemini-3.5-flash-lite"),
        model("gemini-3.1-flash-lite"),
        model("gemini-3.1-pro-preview"),
        model("gemini-3-flash-preview"),
        model("gemini-2.5-pro"),
        model("gemini-2.5-flash"),
        model("gemini-2.5-flash-lite"),
    ]
}
fn provider_error(error: impl std::fmt::Display) -> CoreError {
    CoreError::Provider(error.to_string())
}

fn gemini_contents(messages: Vec<ProviderMessage>) -> CoreResult<Vec<Value>> {
    let mut contents = Vec::new();
    let mut function_ids = std::collections::HashMap::new();
    for message in messages {
        match message {
            ProviderMessage::Text { role, content } if role != "developer" => {
                contents.push(json!({
                    "role": if role == "assistant" { "model" } else { "user" },
                    "parts": [{"text": content}]
                }))
            }
            ProviderMessage::Media {
                role,
                content,
                attachments,
            } if role != "developer" => {
                let media = crate::media::load_all_with_limit(&attachments, 10 * 1024 * 1024)?;
                let mut parts = Vec::with_capacity(media.len() + usize::from(!content.is_empty()));
                if !content.is_empty() {
                    parts.push(json!({"text": content}));
                }
                parts.extend(media.into_iter().map(
                    |item| json!({"inlineData": {"mimeType": item.mime_type, "data": item.base64}}),
                ));
                contents.push(json!({
                    "role": if role == "assistant" { "model" } else { "user" },
                    "parts": parts,
                }));
            }
            ProviderMessage::Text { .. } | ProviderMessage::Media { .. } => {}
            ProviderMessage::ToolCall {
                call_id,
                name,
                arguments,
                provider_metadata,
                ..
            } => {
                let mut part = json!({"functionCall": {"name": name, "args": arguments}});
                if let Some(id) = provider_metadata
                    .get("functionCallId")
                    .and_then(Value::as_str)
                {
                    part["functionCall"]["id"] = json!(id);
                    function_ids.insert(call_id, id.to_owned());
                }
                if let Some(signature) = provider_metadata
                    .get("thoughtSignature")
                    .and_then(Value::as_str)
                {
                    part["thoughtSignature"] = Value::String(signature.into());
                }
                contents.push(json!({"role": "model", "parts": [part]}));
            }
            ProviderMessage::ToolResult {
                call_id,
                name,
                output,
                attachments,
                ..
            } => {
                let media = crate::media::load_all_with_limit(&attachments, 10 * 1024 * 1024)?;
                let mut response = json!({
                    "name": name,
                    "response": {"name": name, "content": output}
                });
                if let Some(id) = function_ids.get(&call_id) {
                    response["id"] = json!(id);
                }
                contents.push(json!({
                    "role": "user",
                    "parts": [{"functionResponse": response}]
                }));
                if !media.is_empty() {
                    contents.push(json!({
                        "role": "user",
                        "parts": media.into_iter().map(|item| json!({
                            "inlineData": {"mimeType": item.mime_type, "data": item.base64}
                        })).collect::<Vec<_>>()
                    }));
                }
            }
        }
    }
    // Parallel calls belong to a single model content, and their responses to
    // one user content. Keep signatures on their original parts.
    let mut grouped: Vec<Value> = Vec::new();
    for content in contents {
        if let Some(previous) = grouped
            .last_mut()
            .filter(|previous| previous["role"] == content["role"])
        {
            previous["parts"]
                .as_array_mut()
                .unwrap()
                .extend(content["parts"].as_array().unwrap().iter().cloned());
        } else {
            grouped.push(content);
        }
    }
    Ok(grouped)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc};

    use axum::{
        Json, Router,
        extract::{Query, State},
        http::header::CONTENT_TYPE,
        routing::{get, post},
    };
    use futures::StreamExt;
    use parking_lot::Mutex;
    use secrecy::ExposeSecret;
    use serde_json::{Value, json};
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    use haarness_core::{ModelProvider, ProviderEvent, ProviderMessage, ProviderRequest};

    use super::{
        GeminiProvider, api_key_from, discovered_model, discovered_model_for, gemini_contents,
    };

    #[derive(Clone, Default)]
    struct FixtureState(Arc<Mutex<Vec<Value>>>);

    async fn structured_content(
        State(state): State<FixtureState>,
        Json(request): Json<Value>,
    ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
        state.0.lock().push(request);
        let stream = r#"data: {"responseId":"response-1","candidates":[{"content":{"parts":[{"functionCall":{"name":"StructuredOutput","args":{"count":4}}}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":3}}

"#;
        ([(CONTENT_TYPE, "text/event-stream")], stream.into())
    }

    #[test]
    fn api_key_lookup_uses_google_then_gemini_then_compatibility_name() {
        let mut requested = Vec::new();
        let key = api_key_from(|name| {
            requested.push(name.to_owned());
            Some("unused fixture".to_owned())
        });
        assert!(key.is_some());
        assert_eq!(requested, ["GOOGLE_API_KEY"]);

        let mut requested = Vec::new();
        let key = api_key_from(|name| {
            requested.push(name.to_owned());
            (name != "GOOGLE_API_KEY").then(|| "unused fixture".to_owned())
        });
        assert!(key.is_some());
        assert_eq!(requested, ["GOOGLE_API_KEY", "GEMINI_API_KEY"]);

        let mut requested = Vec::new();
        let key = api_key_from(|name| {
            requested.push(name.to_owned());
            (name == "GOOGLE_GENERATIVE_AI_API_KEY").then(|| "unused fixture".to_owned())
        });
        assert!(key.is_some());
        assert_eq!(
            requested,
            [
                "GOOGLE_API_KEY",
                "GEMINI_API_KEY",
                "GOOGLE_GENERATIVE_AI_API_KEY"
            ]
        );
    }

    #[test]
    fn configured_gemini_transport_keeps_custom_identity_and_video_metadata() {
        let provider = GeminiProvider::new(crate::NativeInstance {
            id: "enterprise-gemini".into(),
            name: "Enterprise Gemini Gateway".into(),
            base_url: "https://models.example.test/gemini/v1beta/".into(),
            api_key: None,
            models: vec!["vendor-flash".into()],
            discover_models: false,
            model_defaults: crate::CompatibleModelDefaults {
                video: Some(true),
                tools: Some(false),
                ..Default::default()
            },
            assume_authenticated: false,
            headers: crate::ProviderHeaders::parse(BTreeMap::from([(
                "Authorization".into(),
                "Bearer fixture".into(),
            )]))
            .unwrap(),
        });

        let info = provider.info();
        assert_eq!(provider.id(), "enterprise-gemini");
        assert_eq!(info.name, "Enterprise Gemini Gateway");
        assert!(info.authenticated);
        assert_eq!(info.models[0].id, "vendor-flash");
        assert!(info.models[0].video);
        assert!(!info.models[0].tools);
        assert_eq!(
            provider.base_url,
            "https://models.example.test/gemini/v1beta"
        );
    }

    #[test]
    fn stored_gemini_auth_is_the_final_composition_fallback() {
        let provider = GeminiProvider {
            client: reqwest::Client::new(),
            id: "gemini".into(),
            name: "Google Gemini".into(),
            base_url: "http://unused.invalid".into(),
            api_key: None,
            auth_store: None,
            models: Vec::new(),
            live_models: crate::live_models::LiveModels::default(),
            discover_models: true,
            assume_authenticated: false,
            headers: crate::ProviderHeaders::default(),
        }
        .with_api_key(Some("stored fixture".into()));
        assert_eq!(
            provider.api_key.as_ref().unwrap().expose_secret(),
            "stored fixture"
        );

        let provider = GeminiProvider::for_test("http://unused.invalid".into())
            .with_api_key(Some("stored fixture".into()));
        assert_eq!(provider.api_key.as_ref().unwrap().expose_secret(), "test");
    }

    async fn models(
        Query(query): Query<std::collections::HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        if query.contains_key("pageToken") {
            Json(json!({
                "models": [{
                    "name": "models/gemini-future-4-flash",
                    "displayName": "Gemini Future 4 Flash",
                    "inputTokenLimit": 2_097_152,
                    "outputTokenLimit": 131_072,
                    "supportedGenerationMethods": ["generateContent"]
                }]
            }))
        } else {
            Json(json!({
                "models": [{
                    "name": "models/gemini-3.7-flash",
                    "supportedGenerationMethods": ["generateContent"]
                }],
                "nextPageToken": "page-2"
            }))
        }
    }

    #[tokio::test]
    async fn discovery_follows_every_models_page() {
        let router = Router::new().route("/models", get(models));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = GeminiProvider::for_test(format!("http://{address}"))
            .discover_models()
            .await;
        let models = provider.info().models;
        assert!(models.iter().any(|model| model.id == "gemini-3.7-flash"));
        let future = models
            .iter()
            .find(|model| model.id == "gemini-future-4-flash")
            .unwrap();
        assert_eq!(future.context_window, Some(2_097_152));
        assert_eq!(future.max_output_tokens, Some(131_072));
    }

    #[tokio::test]
    async fn structured_output_uses_any_function_mode_and_a_distinct_event() {
        let state = FixtureState::default();
        let router = Router::new()
            .route(
                "/models/gemini-test:streamGenerateContent",
                post(structured_content),
            )
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = GeminiProvider::for_test(format!("http://{address}"));
        let mut stream = provider
            .stream(
                ProviderRequest {
                    model: "gemini-test".into(),
                    system: "test".into(),
                    messages: vec![ProviderMessage::Text {
                        role: "user".into(),
                        content: "count".into(),
                    }],
                    tools: vec![haarness_core::ToolDefinition {
                        name: "fill".into(),
                        description: "Fill fields".into(),
                        input_schema: json!({"type":"object","properties":{"fields":{
                            "type":"array","maxItems":100,"minItems":1,
                            "items":{"anyOf":[{"type":"object","properties":{"value":{"type":"string","maxLength":2048}}}]}
                        }}}),
                    }],
                    reasoning_effort: None,
                    parameters: haarness_core::RequestParameters::default(),
                    output_schema: Some(json!({
                        "type":"object",
                        "properties":{"count":{"type":"integer"}, "agent":{"type":["string","null"]}, "kind":{"const":"cron"}},
                        "required":["count"], "additionalProperties":false
                    })),
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let events = stream.by_ref().collect::<Vec<_>>().await;
        assert!(events.into_iter().any(
            |event| matches!(event.unwrap(), ProviderEvent::StructuredOutput(value) if value == json!({"count": 4}))
        ));
        let requests = state.0.lock();
        assert_eq!(
            requests[0]["toolConfig"]["functionCallingConfig"]["mode"],
            "ANY"
        );
        // Gemini rejects this array bound under forced calling; other bounds stay.
        let fields = &requests[0]["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"]["properties"]
            ["fields"];
        assert!(fields.get("maxItems").is_none());
        assert_eq!(fields["minItems"], 1);
        assert_eq!(
            fields["items"]["anyOf"][0]["properties"]["value"]["maxLength"],
            2048
        );
        let declaration = requests[0]["tools"][0]["functionDeclarations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "StructuredOutput")
            .unwrap();
        assert_eq!(declaration["parametersJsonSchema"]["required"][0], "count");
        assert!(declaration.get("parameters").is_none());
        assert_eq!(
            declaration["parametersJsonSchema"]["properties"]["agent"]["type"],
            json!(["string", "null"])
        );
        assert_eq!(
            declaration["parametersJsonSchema"]["properties"]["kind"]["const"],
            "cron"
        );
        assert_eq!(
            declaration["parametersJsonSchema"]["additionalProperties"],
            false
        );
    }

    #[test]
    fn model_api_metadata_is_used_and_media_only_models_are_hidden() {
        let info = discovered_model(&json!({
            "name": "models/gemini-future-4-flash",
            "displayName": "Gemini Future 4 Flash",
            "inputTokenLimit": 2_097_152,
            "outputTokenLimit": 131_072,
            "supportedGenerationMethods": ["generateContent"],
            "thinking": true
        }))
        .unwrap();
        assert_eq!(info.display_name, "Gemini Future 4 Flash");
        assert_eq!(info.context_window, Some(2_097_152));
        assert_eq!(info.max_output_tokens, Some(131_072));
        assert!(info.reasoning);
        for id in [
            "gemini-omni-1.1-flash",
            "gemini-3.5-transcribe",
            "gemini-nano-banana-2.1",
            "gemini-3.8-flash-lite-tts",
            "gemini-3.8-live",
            "gemini-robotics-er-2-preview",
            "gemini-2.5-computer-use-preview-10-2025",
        ] {
            assert!(discovered_model(&json!({"name":format!("models/{id}"),"supportedGenerationMethods":["generateContent"]})).is_none());
        }
        let plain = discovered_model(&json!({"name":"models/gemini-future-plain","supportedGenerationMethods":["generateContent"],"thinking":false})).unwrap();
        assert!(!plain.reasoning);
        assert!(plain.reasoning_efforts.is_empty());

        assert!(
            discovered_model(&json!({
                "name": "models/gemini-3.1-flash-image",
                "supportedGenerationMethods": ["generateContent"]
            }))
            .is_none()
        );
    }

    #[test]
    fn custom_gemini_discovery_accepts_non_vendor_model_ids() {
        let entry = json!({
            "name": "models/company-flash",
            "supportedGenerationMethods": ["generateContent"]
        });
        assert!(discovered_model(&entry).is_none());
        assert_eq!(
            discovered_model_for(&entry, false).unwrap().id,
            "company-flash"
        );
    }

    #[test]
    fn returns_thought_signatures_on_function_call_history() {
        let contents = gemini_contents(vec![ProviderMessage::ToolCall {
            call_id: "call_1".into(),
            name: "list".into(),
            arguments: json!({"path": "."}),
            provider_metadata: json!({"thoughtSignature": "opaque-signature"}),
        }])
        .unwrap();

        assert_eq!(
            contents[0]["parts"][0]["thoughtSignature"],
            "opaque-signature"
        );
        assert_eq!(contents[0]["parts"][0]["functionCall"]["name"], "list");
    }

    #[test]
    fn serializes_document_media_and_tool_result_attachments() {
        let contents = gemini_contents(vec![
            ProviderMessage::Media {
                role: "user".into(),
                content: "Compare these".into(),
                attachments: vec![
                    "data:image/png;base64,iVBORw0KGgo=".into(),
                    "data:application/pdf;name=report.pdf;base64,JVBERi0xLjQK".into(),
                    "data:audio/mpeg;name=meeting.mp3;base64,SUQz".into(),
                    "data:video/mp4;base64,AAAAIGZ0eXA=".into(),
                ],
            },
            ProviderMessage::ToolResult {
                call_id: "read-1".into(),
                name: "read".into(),
                output: "PDF read successfully".into(),
                is_error: false,
                attachments: vec!["data:application/pdf;name=tool.pdf;base64,JVBERi0xLjQK".into()],
            },
        ])
        .unwrap();

        assert_eq!(contents[0]["parts"][0]["text"], "Compare these");
        assert_eq!(
            contents[0]["parts"][1]["inlineData"]["mimeType"],
            "image/png"
        );
        assert_eq!(
            contents[0]["parts"][2]["inlineData"]["mimeType"],
            "application/pdf"
        );
        assert_eq!(
            contents[0]["parts"][3]["inlineData"]["mimeType"],
            "audio/mpeg"
        );
        assert_eq!(
            contents[0]["parts"][4]["inlineData"]["mimeType"],
            "video/mp4"
        );
        assert_eq!(
            contents[0]["parts"][5]["functionResponse"]["response"]["content"],
            "PDF read successfully"
        );
        assert!(contents[0]["parts"][5]["functionResponse"]["parts"].is_null());
        assert_eq!(
            contents[0]["parts"][6]["inlineData"]["mimeType"],
            "application/pdf"
        );
    }
}
