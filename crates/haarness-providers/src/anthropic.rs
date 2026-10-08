use async_stream::try_stream;
use async_trait::async_trait;
use futures::StreamExt;
use haarness_core::{
    CoreError, CoreResult, ModelProvider, ProviderEvent, ProviderMessage, ProviderRequest,
    ProviderStream, STRUCTURED_OUTPUT_TOOL_NAME, structured_output_tool,
};
use haarness_protocol::{ModelInfo, ProviderInfo};
use reqwest::header::{HeaderName, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::auth::CredentialStore;
use crate::sse::json_stream;

pub struct AnthropicProvider {
    client: reqwest::Client,
    id: String,
    name: String,
    endpoint: String,
    api_key: Option<SecretString>,
    auth_store: Option<Arc<CredentialStore>>,
    models: Vec<ModelInfo>,
    live_models: crate::live_models::LiveModels,
    discover_models: bool,
    adaptive_models: std::sync::RwLock<std::collections::HashMap<String, bool>>,
    assume_authenticated: bool,
    headers: crate::ProviderHeaders,
}

/// The Messages endpoint under `ANTHROPIC_BASE_URL`. Anthropic's SDKs and
/// Claude Code take the API root (`https://api.anthropic.com`) and add
/// `/v1/messages`; a base that already names `/v1` or the endpoint is kept.
fn messages_endpoint(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/messages") {
        base.to_owned()
    } else if base.ends_with("/v1") {
        format!("{base}/messages")
    } else {
        format!("{base}/v1/messages")
    }
}

impl AnthropicProvider {
    #[must_use]
    pub fn from_environment() -> Self {
        Self {
            client: reqwest::Client::new(),
            id: "anthropic".into(),
            name: "Anthropic".into(),
            endpoint: std::env::var("ANTHROPIC_BASE_URL")
                .map_or_else(|_| "https://api.anthropic.com/v1/messages".into(), |base| {
                    messages_endpoint(&base)
                }),
            api_key: std::env::var("ANTHROPIC_API_KEY")
                .ok()
                .map(SecretString::from),
            auth_store: None,
            models: preferred_models(),
            live_models: crate::live_models::LiveModels::default(),
            adaptive_models: std::sync::RwLock::default(),
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
            endpoint: format!(
                "{}/messages",
                instance
                    .base_url
                    .trim_end_matches('/')
                    .trim_end_matches("/messages")
            ),
            api_key: instance.api_key.map(SecretString::from),
            auth_store: None,
            models: instance
                .models
                .into_iter()
                .map(|id| configured_model(&id, defaults))
                .collect(),
            live_models: crate::live_models::LiveModels::default(),
            adaptive_models: std::sync::RwLock::default(),
            discover_models: instance.discover_models,
            assume_authenticated: instance.assume_authenticated,
            headers: instance.headers,
        }
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
        let base = self.endpoint.strip_suffix("/messages")?;
        let url = format!("{base}/models");
        let mut after_id: Option<String> = None;
        let mut models = Vec::new();
        let mut adaptive_models = std::collections::HashMap::new();
        // The API allows 1,000 entries per page. Keep a finite upper bound so
        // a malformed cursor can never hold startup indefinitely.
        for _ in 0..10 {
            let Ok(mut page_url) = url::Url::parse(&url) else {
                return None;
            };
            page_url.query_pairs_mut().append_pair("limit", "1000");
            if let Some(after_id) = &after_id {
                page_url.query_pairs_mut().append_pair("after_id", after_id);
            }
            let mut headers = self.headers.map();
            if !headers.contains_key("anthropic-version") {
                headers.insert(
                    HeaderName::from_static("anthropic-version"),
                    HeaderValue::from_static("2023-06-01"),
                );
            }
            if let Some(key) = &key {
                let Ok(value) = HeaderValue::from_str(key) else {
                    return None;
                };
                headers.insert(HeaderName::from_static("x-api-key"), value);
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
            for entry in body["data"].as_array()? {
                if let (Some(id), Some(adaptive)) = (
                    entry["id"].as_str(),
                    entry
                        .pointer("/capabilities/thinking/types/adaptive/supported")
                        .and_then(Value::as_bool),
                ) {
                    adaptive_models.insert(id.to_owned(), adaptive);
                }
            }
            models.extend(
                body["data"]
                    .as_array()?
                    .iter()
                    .filter_map(|entry| discovered_model_for(entry, self.id == "anthropic")),
            );
            if !body["has_more"].as_bool().unwrap_or(false) {
                *self.adaptive_models.write().unwrap() = adaptive_models;
                return Some(crate::merge_discovered_models(self.models.clone(), models));
            }
            let cursor = body["last_id"].as_str().filter(|id| !id.is_empty())?;
            if after_id.as_deref() == Some(cursor) {
                return None;
            }
            after_id = Some(cursor.to_owned());
        }
        None
    }

    #[cfg(test)]
    fn for_test(endpoint: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            id: "anthropic".into(),
            name: "Anthropic".into(),
            endpoint,
            api_key: Some(SecretString::from("test")),
            auth_store: None,
            models: preferred_models(),
            live_models: crate::live_models::LiveModels::default(),
            adaptive_models: std::sync::RwLock::default(),
            discover_models: true,
            assume_authenticated: false,
            headers: crate::ProviderHeaders::default(),
        }
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
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
            authenticated: self.resolved_api_key().is_some()
                || self.assume_authenticated
                || !self.headers.is_empty(),
            models: self.live_models.get(&self.models),
        }
    }

    fn prepare_side_question(&self, request: &mut ProviderRequest) {
        let adaptive = self
            .adaptive_models
            .read()
            .unwrap()
            .get(&request.model)
            .copied()
            .unwrap_or_else(|| adaptive_thinking(&request.model));
        if let Some(schema) = request.output_schema.take() {
            request
                .tools
                .retain(|entry| entry.name != STRUCTURED_OUTPUT_TOOL_NAME);
            // Adaptive models use native JSON formatting, not a generated
            // tool. Preserve their actual tool block on the side request.
            if !adaptive {
                let tool = structured_output_tool(&schema);
                request.tools.push(tool);
            }
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
        let adaptive = self
            .adaptive_models
            .read()
            .unwrap()
            .get(&request.model)
            .copied()
            .unwrap_or_else(|| adaptive_thinking(&request.model));
        let structured_output = request.output_schema;
        let native_json = adaptive && structured_output.is_some();
        let mut requested_tools = request.tools;
        crate::tool_schema::retain_claude_compatible(&request.model, &mut requested_tools);
        if let Some(schema) = structured_output.as_ref() {
            requested_tools.retain(|tool| tool.name != STRUCTURED_OUTPUT_TOOL_NAME);
            if !native_json {
                requested_tools.push(structured_output_tool(schema));
            }
        }
        let mut tools: Vec<_> = requested_tools.into_iter().map(|tool| json!({"name":tool.name,"description":tool.description,"input_schema":tool.input_schema})).collect();
        // Prompt caching: mark the stable prefix (tools + system) and the last
        // two non-assistant messages so multi-round loops reuse the cache.
        if let Some(last) = tools.last_mut() {
            last["cache_control"] = json!({"type":"ephemeral"});
        }
        let system =
            json!([{"type":"text","text":request.system,"cache_control":{"type":"ephemeral"}}]);
        let mut messages: Vec<_> = request
            .messages
            .into_iter()
            .filter_map(|message| match message {
                ProviderMessage::Text { role, content } if role != "developer" => {
                    Some(Ok(json!({"role":role,"content":[{"type":"text","text":content}]})))
                }
                ProviderMessage::Text { .. } => None,
                ProviderMessage::Media { role, .. } if role == "developer" => None,
                ProviderMessage::Media {
                    role,
                    content,
                    attachments,
                } => Some(crate::media::load_all(&attachments).and_then(|media| {
                    let mut blocks = Vec::with_capacity(media.len() + 1);
                    if !content.is_empty() {
                        blocks.push(json!({"type":"text","text":content}));
                    }
                    for item in media {
                        if item.is_image() {
                            blocks.push(json!({"type":"image","source":{"type":"base64","media_type":item.mime_type,"data":item.base64}}));
                        } else if item.is_pdf() {
                            blocks.push(json!({
                                "type":"document",
                                "source":{"type":"base64","media_type":"application/pdf","data":item.base64},
                                "title":item.filename
                            }));
                        } else {
                            return Err(CoreError::Provider(
                                "Anthropic accepts Haarness image and PDF attachments; use Gemini for audio or video input"
                                    .into(),
                            ));
                        }
                    }
                    Ok(json!({"role":role,"content":blocks}))
                })),
                ProviderMessage::ToolCall {
                    call_id,
                    name,
                    arguments,
                    provider_metadata,
                } => {
                    let mut blocks = provider_metadata.get("anthropic_thinking")
                        .and_then(Value::as_array).cloned().unwrap_or_default();
                    blocks.push(json!({"type":"tool_use","id":call_id,"name":name,"input":arguments}));
                    Some(Ok(json!({"role":"assistant","content":blocks})))
                },
                ProviderMessage::ToolResult {
                    call_id,
                    output,
                    is_error,
                    attachments,
                    ..
                } => Some(crate::media::load_all(&attachments).and_then(|media| {
                    let content = if media.is_empty() {
                        Value::String(output)
                    } else {
                        let mut blocks = vec![json!({"type":"text","text":output})];
                        for item in media {
                            if item.is_image() {
                                blocks.push(json!({"type":"image","source":{"type":"base64","media_type":item.mime_type,"data":item.base64}}));
                            } else if item.is_pdf() {
                                blocks.push(json!({
                                    "type":"document",
                                    "source":{"type":"base64","media_type":"application/pdf","data":item.base64},
                                    "title":item.filename
                                }));
                            } else {
                                return Err(CoreError::Provider(
                                    "Anthropic tool results support Haarness images and PDFs, not audio or video"
                                        .into(),
                                ));
                            }
                        }
                        Value::Array(blocks)
                    };
                    Ok(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":call_id,"content":content,"is_error":is_error}]}))
                })),
            })
            .collect::<CoreResult<_>>()?;
        let mut marked = 0;
        for message in messages.iter_mut().rev() {
            if marked == 2 {
                break;
            }
            if message["role"] == "user" {
                if let Some(block) = message["content"]
                    .as_array_mut()
                    .and_then(|blocks| blocks.last_mut())
                {
                    block["cache_control"] = json!({"type":"ephemeral"});
                    marked += 1;
                }
            }
        }
        let catalog = self.live_models.get(&self.models);
        let max_output = catalog
            .iter()
            .find(|info| request.model.starts_with(&info.id) || info.id == request.model)
            .and_then(|info| info.max_output_tokens)
            .or_else(|| {
                crate::catalog::lookup("anthropic", &request.model)
                    .map(|entry| entry.max_output_tokens)
            })
            .unwrap_or(8192);
        let mut body = json!({"model":request.model,"system":system,"messages":messages,"tools":tools,"max_tokens":8192,"stream":true});
        if native_json {
            body["output_config"]["format"] =
                json!({"type":"json_schema", "schema":structured_output});
        } else if structured_output.is_some() {
            body["tool_choice"] = json!({"type":"any"});
        }
        if adaptive {
            body["thinking"] = json!({"type":"adaptive", "display":"summarized"});
            if let Some(effort) = request.reasoning_effort.as_deref() {
                body["output_config"]["effort"] = json!(match effort {
                    "none" | "minimal" => "low",
                    "max" => "max",
                    value => value,
                });
            }
        } else {
            match request.reasoning_effort.as_deref() {
                Some("minimal") | None => {}
                Some(effort) => {
                    let budget: u64 = match effort {
                        "low" => 4096,
                        "high" => 24_576,
                        _ => 12_288,
                    };
                    body["thinking"] = json!({"type":"enabled","budget_tokens":budget});
                    // Anthropic requires max_tokens to exceed the thinking budget;
                    // stay within the model's output ceiling where it allows.
                    body["max_tokens"] = json!((budget + 8192).min(max_output).max(budget + 1024));
                }
            }
        }
        let parameters = &request.parameters;
        let bound_thinking = adaptive
            && (request.model.starts_with("claude-fable-5-1")
                || request.model.starts_with("claude-opus-5-5"));
        if bound_thinking {
            // Like OpenCode, allow prompt/tool refresh and compaction to retire
            // invalidated reasoning instead of breaking the conversation.
            body["thinking"]["block_binding"] = json!({"prefix_mismatch_behavior":"drop_block"});
        }
        if let Some(limit) = parameters.max_output_tokens.filter(|limit| *limit > 0) {
            // A thinking budget must stay below max_tokens.
            let floor = body["thinking"]["budget_tokens"]
                .as_u64()
                .map_or(0, |budget| budget + 1024);
            body["max_tokens"] = json!(limit.max(floor));
        }
        let thinking = body.get("thinking").is_some();
        // New Claude families reject sampling parameters; older models only
        // permit temperature/top_k when thinking is off, and top_p >= .95 on.
        let sampling = !adaptive_thinking(&request.model);
        if let Some(temperature) = parameters.temperature.filter(|_| sampling && !thinking) {
            body["temperature"] = json!(temperature);
        }
        if let Some(top_p) = parameters
            .top_p
            .filter(|value| sampling && (!thinking || *value >= 0.95))
        {
            body["top_p"] = json!(top_p);
        }
        if let Some(top_k) = parameters.top_k.filter(|_| sampling && !thinking) {
            body["top_k"] = json!(top_k);
        }
        let mut headers = self.headers.map();
        crate::apply_request_headers(&mut headers, &parameters.headers);
        if bound_thinking {
            let beta = "thinking-binding-controls-2026-08-01";
            let existing = headers
                .get("anthropic-beta")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            if !existing.split(',').any(|value| value.trim() == beta) {
                let value = if existing.is_empty() {
                    beta.to_owned()
                } else {
                    format!("{existing},{beta}")
                };
                headers.insert(
                    HeaderName::from_static("anthropic-beta"),
                    HeaderValue::from_str(&value).map_err(provider_error)?,
                );
            }
        }
        if !headers.contains_key("anthropic-version") {
            headers.insert(
                HeaderName::from_static("anthropic-version"),
                HeaderValue::from_static("2023-06-01"),
            );
        }
        if let Some(key) = key {
            headers.insert(
                HeaderName::from_static("x-api-key"),
                HeaderValue::from_str(&key).map_err(provider_error)?,
            );
        }
        let response = self
            .client
            .post(&self.endpoint)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(crate::sse::transport_error)?;
        let mut events = json_stream(response, cancel);
        Ok(Box::pin(try_stream! {
            let mut tool: Option<(String,String,String,Value)> = None;
            let mut thinking_block: Option<Value> = None;
            let mut thinking_blocks: Vec<Value> = Vec::new();
            let mut json_output = String::new();
            // A response can carry several thinking blocks. Their deltas end
            // without a break, so consecutive blocks would otherwise fuse into
            // one paragraph mid-sentence. The break waits for the next block's
            // first delta: an empty block earns no separator, and prose or a
            // tool call in between already ends the reasoning run.
            let mut thinking_seen = false;
            let mut thinking_break = false;
            // input/cache counters arrive on message_start; output arrives on
            // message_delta. Each event reports the merged total so far.
            let mut input_tokens = 0_u64;
            let mut cache_read = 0_u64;
            let mut cache_write = 0_u64;
            let mut output_tokens = 0_u64;
            while let Some(event) = events.next().await {
                let event = event?;
                match event["type"].as_str().unwrap_or_default() {
                    "message_start" => {
                        if event.pointer("/message/input_transformations").and_then(Value::as_array).is_some_and(|items| items.iter().any(|item| item["reason"] == "prefix_binding_mismatch")) {
                            tracing::warn!("Anthropic dropped preserved thinking after the conversation prefix changed");
                        }
                        if let Some(usage) = event.pointer("/message/usage") {
                        input_tokens = usage["input_tokens"].as_u64().unwrap_or(0);
                        cache_read = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
                        cache_write = usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                        output_tokens = usage["output_tokens"].as_u64().unwrap_or(0);
                        // The prompt is billed from here on: report it now so a
                        // stream cut before message_delta still counts it.
                        yield ProviderEvent::Usage {
                            input_tokens: input_tokens.saturating_add(cache_read).saturating_add(cache_write),
                            output_tokens,
                            cache_read_tokens: cache_read,
                            cache_write_tokens: cache_write,
                        };
                    }},
                    "content_block_start" if matches!(event["content_block"]["type"].as_str(), Some("thinking" | "redacted_thinking")) => {
                        thinking_break = thinking_seen;
                        thinking_block = Some(event["content_block"].clone());
                    },
                    "content_block_start" => {
                        thinking_seen = false;
                        thinking_break = false;
                        if event["content_block"]["type"] == "tool_use" {
                            tool = Some((
                                event["content_block"]["id"].as_str().unwrap_or_default().into(),
                                event["content_block"]["name"].as_str().unwrap_or_default().into(), String::new(), event["content_block"].get("input").cloned().unwrap_or_else(||json!({}))));
                        }
                    },
                    "content_block_delta" => match event["delta"]["type"].as_str().unwrap_or_default() {
                        "text_delta" => if let Some(text) = event["delta"]["text"].as_str().filter(|text| !text.is_empty()) { if native_json { json_output.push_str(text); } else { yield ProviderEvent::TextDelta(text.into()); } },
                        "thinking_delta" => if let Some(text) = event["delta"]["thinking"].as_str().filter(|text| !text.is_empty()) {
                            if let Some(block) = &mut thinking_block {
                                let mut value = block["thinking"].as_str().unwrap_or_default().to_owned();
                                value.push_str(text);
                                block["thinking"] = json!(value);
                            }
                            if std::mem::take(&mut thinking_break) {
                                yield ProviderEvent::ReasoningDelta("\n\n".into());
                            }
                            thinking_seen = true;
                            yield ProviderEvent::ReasoningDelta(text.into());
                        },
                        "input_json_delta" => if let (Some((_,_,buffer,_)), Some(json)) = (&mut tool, event["delta"]["partial_json"].as_str()) { buffer.push_str(json); },
                        "signature_delta" => if let (Some(block), Some(signature)) = (&mut thinking_block, event["delta"]["signature"].as_str()) {
                            let mut value = block["signature"].as_str().unwrap_or_default().to_owned();
                            value.push_str(signature);
                            block["signature"] = json!(value);
                        },
                        _ => {}
                    },
                    "content_block_stop" => {
                        if let Some(block) = thinking_block.take() { thinking_blocks.push(block); }
                        if let Some((call_id,name,arguments,initial_input)) = tool.take() {
                        let arguments = if arguments.is_empty() { initial_input } else {
                            serde_json::from_str(&arguments).map_err(|error| CoreError::Provider(format!("Anthropic returned invalid tool arguments: {error}")))?
                        };
                        if structured_output.is_some() && name == STRUCTURED_OUTPUT_TOOL_NAME {
                            yield ProviderEvent::StructuredOutput(arguments);
                        } else {
                            let provider_metadata = if thinking_blocks.is_empty() { Value::Null } else {
                                json!({"anthropic_thinking":std::mem::take(&mut thinking_blocks)})
                            };
                            yield ProviderEvent::ToolCall { call_id, name, arguments, provider_metadata };
                        }
                    }},
                    "message_delta" => if let Some(usage) = event.get("usage") {
                        // Anthropic's input_tokens excludes cache reads and
                        // writes; the event's input_tokens is the whole prompt.
                        let input = usage["input_tokens"].as_u64().unwrap_or(input_tokens);
                        let read = usage["cache_read_input_tokens"].as_u64().unwrap_or(cache_read);
                        let write = usage["cache_creation_input_tokens"].as_u64().unwrap_or(cache_write);
                        yield ProviderEvent::Usage {
                            input_tokens: input.saturating_add(read).saturating_add(write),
                            output_tokens: usage["output_tokens"].as_u64().unwrap_or(output_tokens),
                            cache_read_tokens: read,
                            cache_write_tokens: write,
                        };
                    },
                    "message_stop" => {
                        if native_json && !json_output.is_empty() {
                            let value = serde_json::from_str(&json_output).map_err(|error| CoreError::Provider(format!("Anthropic returned invalid structured output: {error}")))?;
                            yield ProviderEvent::StructuredOutput(value);
                        }
                        yield ProviderEvent::Finished { reason: "completed".into() };
                    },
                    "error" => Err(CoreError::Provider(event.to_string()))?,
                    _ => {}
                }
            }
        }))
    }
}

fn adaptive_thinking(id: &str) -> bool {
    [
        "claude-fable-5",
        "claude-mythos-5",
        "claude-opus-5",
        "claude-sonnet-5",
        "claude-opus-4-7",
        "claude-opus-4-8",
    ]
    .iter()
    .any(|prefix| id.starts_with(prefix))
}

fn model(id: &str) -> ModelInfo {
    crate::catalog::annotate(
        "anthropic",
        ModelInfo {
            id: id.into(),
            display_name: id.into(),
            reasoning: true,
            reasoning_efforts: if adaptive_thinking(id) {
                vec!["low", "medium", "high", "xhigh", "max"]
            } else {
                vec!["low", "medium", "high"]
            }
            .into_iter()
            .map(str::to_owned)
            .collect(),
            images: true,
            video: false,
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

fn discovered_model_for(entry: &Value, require_claude_prefix: bool) -> Option<ModelInfo> {
    let id = entry["id"].as_str()?.to_owned();
    if id.is_empty() || (require_claude_prefix && !id.starts_with("claude-")) {
        return None;
    }
    let mut info = model(&id);
    if let Some(name) = entry["display_name"]
        .as_str()
        .filter(|name| !name.is_empty())
    {
        name.clone_into(&mut info.display_name);
    }
    if let Some(limit) = positive_u64(&entry["max_input_tokens"]) {
        info.context_window = Some(limit);
    }
    if let Some(limit) = positive_u64(&entry["max_tokens"]) {
        info.max_output_tokens = Some(limit);
    }
    if let Some(supported) = entry
        .pointer("/capabilities/thinking/supported")
        .and_then(Value::as_bool)
    {
        info.reasoning = supported;
        if !supported {
            info.reasoning_efforts.clear();
        }
    }
    if let Some(effort) = entry.pointer("/capabilities/effort") {
        if effort["supported"].as_bool() == Some(false) {
            info.reasoning_efforts.clear();
        } else if effort["supported"].as_bool() == Some(true) {
            info.reasoning_efforts = ["low", "medium", "high", "xhigh", "max"]
                .into_iter()
                .filter(|name| effort[*name]["supported"].as_bool() == Some(true))
                .map(str::to_owned)
                .collect();
        }
    }
    if let Some(supported) = entry
        .pointer("/capabilities/image_input/supported")
        .and_then(Value::as_bool)
    {
        info.images = supported;
    }
    Some(info)
}

fn positive_u64(value: &Value) -> Option<u64> {
    value.as_u64().filter(|value| *value > 0)
}

fn preferred_models() -> Vec<ModelInfo> {
    vec![
        model("claude-opus-5-5"),
        model("claude-fable-5-1"),
        model("claude-fable-5"),
        model("claude-opus-5"),
        model("claude-sonnet-5"),
        model("claude-opus-4-8"),
        model("claude-opus-4-7"),
        model("claude-opus-4-6"),
        model("claude-sonnet-4-6"),
        model("claude-haiku-4-5"),
    ]
}
fn provider_error(error: impl std::fmt::Display) -> CoreError {
    CoreError::Provider(error.to_string())
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
    use serde_json::{Value, json};
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    use haarness_core::{ModelProvider, ProviderEvent, ProviderMessage, ProviderRequest};

    #[test]
    fn base_url_follows_the_sdk_convention() {
        for (base, endpoint) in [
            (
                "https://api.anthropic.com",
                "https://api.anthropic.com/v1/messages",
            ),
            (
                "https://api.anthropic.com/",
                "https://api.anthropic.com/v1/messages",
            ),
            (
                "https://proxy.example/v1",
                "https://proxy.example/v1/messages",
            ),
            (
                "https://proxy.example/v1/messages",
                "https://proxy.example/v1/messages",
            ),
            (
                "https://proxy.example/anthropic",
                "https://proxy.example/anthropic/v1/messages",
            ),
        ] {
            assert_eq!(super::messages_endpoint(base), endpoint, "{base}");
        }
    }

    use super::{AnthropicProvider, discovered_model, discovered_model_for};

    #[derive(Clone, Default)]
    struct FixtureState(Arc<Mutex<Vec<Value>>>);

    #[test]
    fn model_api_metadata_overrides_the_embedded_fallback() {
        let info = discovered_model(&json!({
            "id": "claude-future-6",
            "display_name": "Claude Future 6",
            "max_input_tokens": 2_000_000,
            "max_tokens": 192_000,
            "capabilities": {
                "thinking": {"supported": true},
                "image_input": {"supported": false}
            }
        }))
        .unwrap();

        assert_eq!(info.display_name, "Claude Future 6");
        assert_eq!(info.context_window, Some(2_000_000));
        assert_eq!(info.max_output_tokens, Some(192_000));
        assert!(info.reasoning);
        assert!(!info.images);
    }

    #[test]
    fn custom_anthropic_discovery_accepts_non_vendor_model_ids() {
        let entry = json!({"id": "company-sonnet", "display_name": "Company Sonnet"});
        assert!(discovered_model(&entry).is_none());
        assert_eq!(
            discovered_model_for(&entry, false).unwrap().id,
            "company-sonnet"
        );
    }

    #[test]
    fn configured_anthropic_transport_keeps_custom_identity_and_headers() {
        let provider = AnthropicProvider::new(crate::NativeInstance {
            id: "enterprise-claude".into(),
            name: "Enterprise Claude Gateway".into(),
            base_url: "https://models.example.test/anthropic/v1/messages/".into(),
            api_key: None,
            models: vec!["vendor-sonnet".into()],
            discover_models: false,
            model_defaults: crate::CompatibleModelDefaults {
                context_window: Some(262_144),
                ..Default::default()
            },
            assume_authenticated: false,
            headers: crate::ProviderHeaders::parse(BTreeMap::from([(
                "X-Tenant".into(),
                "commercial".into(),
            )]))
            .unwrap(),
        });

        let info = provider.info();
        assert_eq!(provider.id(), "enterprise-claude");
        assert_eq!(info.name, "Enterprise Claude Gateway");
        assert!(info.authenticated);
        assert_eq!(info.models[0].id, "vendor-sonnet");
        assert_eq!(info.models[0].context_window, Some(262_144));
        assert_eq!(
            provider
                .headers
                .map()
                .get("x-tenant")
                .unwrap()
                .to_str()
                .unwrap(),
            "commercial"
        );
        assert_eq!(
            provider.endpoint,
            "https://models.example.test/anthropic/v1/messages"
        );
    }

    async fn messages(
        State(state): State<FixtureState>,
        Json(request): Json<Value>,
    ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
        state.0.lock().push(request);
        let stream = [
            r#"data: {"type":"message_start","message":{"usage":{"input_tokens":100,"cache_read_input_tokens":80,"cache_creation_input_tokens":20}}}"#,
            r#"data: {"type":"content_block_start","content_block":{"type":"tool_use","id":"tool_1","name":"list"}}"#,
            r#"data: {"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{\"path\":\".\"}"}}"#,
            r#"data: {"type":"content_block_stop"}"#,
            r#"data: {"type":"message_delta","usage":{"output_tokens":7}}"#,
            r#"data: {"type":"message_stop"}"#,
        ]
        .join("\n\n")
            + "\n\n";
        ([(CONTENT_TYPE, "text/event-stream")], stream)
    }

    async fn thinking_messages(
        State(state): State<FixtureState>,
        Json(request): Json<Value>,
    ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
        state.0.lock().push(request);
        let stream = [
            r#"data: {"type":"content_block_start","content_block":{"type":"thinking"}}"#,
            r#"data: {"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"Tracing TLS"}}"#,
            r#"data: {"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":" downgrade"}}"#,
            r#"data: {"type":"content_block_stop"}"#,
            r#"data: {"type":"content_block_start","content_block":{"type":"thinking"}}"#,
            r#"data: {"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"Identifying SSRF"}}"#,
            r#"data: {"type":"content_block_stop"}"#,
            r#"data: {"type":"message_stop"}"#,
        ]
        .join("\n\n")
            + "\n\n";
        ([(CONTENT_TYPE, "text/event-stream")], stream)
    }

    /// Two thinking blocks in one response are two thoughts. Streamed straight
    /// through they would arrive as "…downgradeIdentifying SSRF".
    #[tokio::test]
    async fn consecutive_thinking_blocks_are_separated() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/messages", post(thinking_messages))
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = AnthropicProvider::for_test(format!("http://{address}/messages"));
        let events = provider
            .stream(
                ProviderRequest {
                    model: "claude-test".into(),
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
        assert_eq!(reasoning, "Tracing TLS downgrade\n\nIdentifying SSRF");
    }

    async fn structured_messages(
        State(state): State<FixtureState>,
        Json(request): Json<Value>,
    ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
        state.0.lock().push(request);
        let stream = [
            r#"data: {"type":"message_start","message":{"usage":{"input_tokens":9}}}"#,
            r#"data: {"type":"content_block_start","content_block":{"type":"tool_use","id":"tool_structured","name":"StructuredOutput"}}"#,
            r#"data: {"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{\"count\":3}"}}"#,
            r#"data: {"type":"content_block_stop"}"#,
            r#"data: {"type":"message_delta","usage":{"output_tokens":4}}"#,
            r#"data: {"type":"message_stop"}"#,
        ]
        .join("\n\n")
            + "\n\n";
        ([(CONTENT_TYPE, "text/event-stream")], stream)
    }

    async fn models(Query(query): Query<std::collections::HashMap<String, String>>) -> Json<Value> {
        if query.contains_key("after_id") {
            Json(json!({
                "data": [{
                    "id": "claude-future-6",
                    "display_name": "Claude Future 6",
                    "max_input_tokens": 2_000_000,
                    "max_tokens": 192_000
                }],
                "has_more": false
            }))
        } else {
            Json(json!({
                "data": [{"id": "claude-opus-5", "display_name": "Claude Opus 5"}],
                "has_more": true,
                "last_id": "claude-opus-5"
            }))
        }
    }

    #[tokio::test]
    async fn discovery_follows_every_models_page() {
        let router = Router::new().route("/models", get(models));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = AnthropicProvider::for_test(format!("http://{address}/messages"))
            .discover_models()
            .await;
        let models = provider.info().models;
        assert!(models.iter().any(|model| model.id == "claude-opus-5"));
        let future = models
            .iter()
            .find(|model| model.id == "claude-future-6")
            .unwrap();
        assert_eq!(future.context_window, Some(2_000_000));
        assert_eq!(future.max_output_tokens, Some(192_000));
    }

    #[tokio::test]
    async fn streams_tool_calls_and_serializes_their_results() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/messages", post(messages))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = AnthropicProvider::for_test(format!("http://{address}/messages"));
        let request = ProviderRequest {
            model: "claude-test".into(),
            system: "test".into(),
            messages: vec![
                ProviderMessage::ToolCall {
                    call_id: "previous".into(),
                    name: "read".into(),
                    arguments: json!({"path": "Cargo.toml"}),
                    provider_metadata: Value::Null,
                },
                ProviderMessage::ToolResult {
                    call_id: "previous".into(),
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
            tools: Vec::new(),
            reasoning_effort: None,
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
        assert!(events.iter().any(|event| matches!(
            event,
            ProviderEvent::ToolCall { call_id, name, arguments, .. }
                if call_id == "tool_1" && name == "list" && *arguments == json!({"path": "."})
        )));
        assert!(
            events.iter().any(|event| matches!(
                event,
                // Anthropic's 100 uncached + 80 read + 20 written: the whole prompt.
                ProviderEvent::Usage {
                    input_tokens: 200,
                    output_tokens: 7,
                    cache_read_tokens: 80,
                    cache_write_tokens: 20,
                }
            )),
            "message_start usage must merge with message_delta output: {events:?}"
        );
        // The prompt is reported as soon as message_start bills it, so a stream
        // cut before message_delta still counts it; the last report wins.
        let usages: Vec<_> = events
            .iter()
            .filter(|event| matches!(event, ProviderEvent::Usage { .. }))
            .collect();
        assert!(matches!(
            usages.as_slice(),
            [
                ProviderEvent::Usage {
                    input_tokens: 200,
                    output_tokens: 0,
                    ..
                },
                ProviderEvent::Usage {
                    input_tokens: 200,
                    output_tokens: 7,
                    ..
                },
            ]
        ));
        let requests = state.0.lock();
        assert_eq!(requests[0]["messages"][0]["content"][0]["id"], "previous");
        assert_eq!(
            requests[0]["messages"][1]["content"][0]["tool_use_id"],
            "previous"
        );
        assert_eq!(
            requests[0]["messages"][1]["content"][0]["content"][1]["type"],
            "document"
        );
        assert_eq!(
            requests[0]["messages"][1]["content"][0]["content"][1]["title"],
            "tool.pdf"
        );
        assert_eq!(
            requests[0]["messages"][2]["content"][1]["source"]["type"],
            "base64"
        );
        assert_eq!(
            requests[0]["messages"][2]["content"][1]["source"]["media_type"],
            "image/png"
        );
        assert_eq!(
            requests[0]["messages"][2]["content"][1]["source"]["data"],
            "iVBORw0KGgo="
        );
        // Prompt caching: the system prefix and the trailing user turn are
        // marked as cache breakpoints.
        assert_eq!(
            requests[0]["system"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(
            requests[0]["messages"][1]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
    }

    #[tokio::test]
    async fn structured_output_uses_required_any_and_is_not_an_ordinary_tool_call() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/messages", post(structured_messages))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = AnthropicProvider::for_test(format!("http://{address}/messages"));
        let mut stream = provider
            .stream(
                ProviderRequest {
                    model: "claude-test".into(),
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
            |event| matches!(event.unwrap(), ProviderEvent::StructuredOutput(value) if value == json!({"count": 3}))
        ));
        let requests = state.0.lock();
        assert_eq!(requests[0]["tool_choice"]["type"], "any");
        let tool = requests[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "StructuredOutput")
            .unwrap();
        assert_eq!(tool["input_schema"]["required"][0], "count");
    }

    /// Claude answers HTTP 400 for the whole request when one tool's root
    /// schema combines alternatives, so a single such MCP tool broke every turn.
    #[tokio::test]
    async fn claude_requests_omit_tools_whose_root_schema_claude_rejects() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/messages", post(messages))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = AnthropicProvider::for_test(format!("http://{address}/messages"));
        let tool = |name: &str, input_schema: Value| haarness_core::ToolDefinition {
            name: name.into(),
            description: name.into(),
            input_schema,
        };
        let either = json!([{"required":["current"]},{"required":["all"]}]);
        let tools = vec![
            // Nested combiners and a property named like one are accepted.
            tool(
                "plain",
                json!({"type":"object","properties":{
                    "value":{"anyOf":[{"type":"string"},{"type":"integer"}]},
                    "oneOf":{"type":"string"}
                }}),
            ),
            tool("mcp__fixture__any", json!({"type":"object","anyOf":either})),
            tool("mcp__fixture__one", json!({"type":"object","oneOf":either})),
            tool("mcp__fixture__all", json!({"type":"object","allOf":either})),
            tool("mcp__fixture__list", json!({"type":"array","items":{}})),
        ];
        for model in ["claude-test", "MiniMax-M3"] {
            let request = ProviderRequest {
                model: model.into(),
                system: "test".into(),
                messages: vec![ProviderMessage::Text {
                    role: "user".into(),
                    content: "list".into(),
                }],
                tools: tools.clone(),
                reasoning_effort: None,
                parameters: haarness_core::RequestParameters::default(),
                output_schema: None,
            };
            let mut stream = provider
                .stream(request, CancellationToken::new())
                .await
                .unwrap();
            let _events: Vec<_> = stream.by_ref().collect().await;
        }
        let requests = state.0.lock();
        let names = |request: &Value| {
            request["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|tool| tool["name"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&requests[0]), ["plain"]);
        assert_eq!(
            requests[0]["tools"][0]["cache_control"]["type"],
            "ephemeral"
        );
        // Other models behind the Messages protocol keep every tool.
        assert_eq!(names(&requests[1]).len(), 5);
    }

    #[tokio::test]
    async fn reasoning_effort_becomes_a_thinking_budget() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/messages", post(messages))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = AnthropicProvider::for_test(format!("http://{address}/messages"));
        let request = ProviderRequest {
            model: "claude-test".into(),
            system: "test".into(),
            messages: vec![ProviderMessage::Text {
                role: "user".into(),
                content: "think".into(),
            }],
            tools: Vec::new(),
            reasoning_effort: Some("high".into()),
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        };
        let mut stream = provider
            .stream(request, CancellationToken::new())
            .await
            .unwrap();
        let _events: Vec<_> = stream.by_ref().collect().await;
        let requests = state.0.lock();
        assert_eq!(requests[0]["thinking"]["budget_tokens"], 24_576);
        let max_tokens = requests[0]["max_tokens"].as_u64().unwrap();
        assert!(max_tokens > 24_576, "max_tokens must exceed the budget");
    }
    #[tokio::test]
    async fn adaptive_models_use_native_json_and_live_reasoning_options() {
        async fn reply(
            State(state): State<FixtureState>,
            Json(body): Json<Value>,
        ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
            state.0.lock().push(body);
            ([(CONTENT_TYPE, "text/event-stream")], concat!(
                "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"{\\\"count\\\":3}\"}}\n\n",
                "data: {\"type\":\"message_stop\"}\n\n"
            ).into())
        }
        let state = FixtureState::default();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new()
            .route("/messages", post(reply))
            .with_state(state.clone());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = AnthropicProvider::for_test(format!("http://{address}/messages"));
        // An unknown model can advertise adaptive support without a static entry.
        provider
            .adaptive_models
            .write()
            .unwrap()
            .insert("future-deployment".into(), true);
        for model in ["claude-fable-5-1", "future-deployment"] {
            let stream = provider.stream(ProviderRequest {
                model: model.into(), system: "test".into(), messages: vec![ProviderMessage::Text { role:"user".into(), content:"count".into() }], tools: vec![], reasoning_effort:Some("xhigh".into()),
                parameters: haarness_core::RequestParameters::default(),
                output_schema:Some(json!({"type":"object","properties":{"count":{"type":"integer"}},"required":["count"],"additionalProperties":false})),
            }, CancellationToken::new()).await.unwrap();
            let events: Vec<_> = stream.collect().await;
            assert!(events.into_iter().any(|event| matches!(event.unwrap(), ProviderEvent::StructuredOutput(value) if value == json!({"count":3}))));
        }
        for body in state.0.lock().iter() {
            assert_eq!(body["thinking"]["type"], "adaptive");
            assert_eq!(body["output_config"]["effort"], "xhigh");
            assert_eq!(body["output_config"]["format"]["type"], "json_schema");
            assert!(body.get("tool_choice").is_none());
            assert!(body["tools"].as_array().unwrap().is_empty());
        }
        let info = super::discovered_model(&json!({"id":"claude-future", "capabilities":{"effort":{"supported":true,"low":{"supported":true},"max":{"supported":true}}}})).unwrap();
        assert_eq!(info.reasoning_efforts, ["low", "max"]);
    }

    #[tokio::test]
    async fn structured_side_questions_preserve_wire_tools_without_forcing_output() {
        async fn reply(
            State(state): State<FixtureState>,
            Json(body): Json<Value>,
        ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
            state.0.lock().push(body);
            (
                [(CONTENT_TYPE, "text/event-stream")],
                "data: {\"type\":\"message_stop\"}\n\n".into(),
            )
        }
        let state = FixtureState::default();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new()
            .route("/messages", post(reply))
            .with_state(state.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = AnthropicProvider::for_test(format!("http://{address}/messages"));
        for model in ["claude-test", "claude-fable-5-1"] {
            let main = ProviderRequest {
                model: model.into(),
                system: "Stable system".into(),
                messages: vec![ProviderMessage::Text {
                    role: "user".into(),
                    content: "Main task".into(),
                }],
                tools: vec![
                    haarness_core::ToolDefinition {
                        name: "read".into(),
                        description: "Read context".into(),
                        input_schema: json!({"type":"object"}),
                    },
                    // A request hook may supply an older generated schema.
                    // Both requests must use the canonical current definition.
                    haarness_core::structured_output_tool(
                        &json!({"type":"object","properties":{"old":{"type":"integer"}}}),
                    ),
                ],
                reasoning_effort: None,
                parameters: haarness_core::RequestParameters::default(),
                output_schema: Some(
                    json!({"type":"object","properties":{"answer":{"type":"string"}}}),
                ),
            };
            let mut side = main.clone();
            provider.prepare_side_question(&mut side);
            assert!(side.output_schema.is_none());
            for request in [main, side] {
                let events: Vec<_> = provider
                    .stream(request, CancellationToken::new())
                    .await
                    .unwrap()
                    .collect()
                    .await;
                assert!(events.iter().all(Result::is_ok));
            }
        }
        let requests = state.0.lock();
        for pair in requests.chunks_exact(2) {
            assert_eq!(pair[0]["system"], pair[1]["system"]);
            assert_eq!(pair[0]["tools"], pair[1]["tools"]);
            assert!(pair[1].get("tool_choice").is_none());
            assert!(pair[1]["output_config"].get("format").is_none());
        }
        assert_eq!(requests[0]["tool_choice"]["type"], "any");
        assert_eq!(
            requests[2]["output_config"]["format"]["type"],
            "json_schema"
        );
        server.abort();
    }

    #[tokio::test]
    async fn partial_model_list_failure_does_not_replace_the_catalog() {
        async fn partial(
            Query(query): Query<std::collections::HashMap<String, String>>,
        ) -> (axum::http::StatusCode, Json<Value>) {
            if query.contains_key("after_id") {
                (axum::http::StatusCode::SERVICE_UNAVAILABLE, Json(json!({})))
            } else {
                (
                    axum::http::StatusCode::OK,
                    Json(
                        json!({"data":[{"id":"claude-partial"}],"has_more":true,"last_id":"claude-partial"}),
                    ),
                )
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/models", get(partial)))
                .await
                .unwrap()
        });
        let provider = AnthropicProvider::for_test(format!("http://{address}/messages"));
        let before = provider.info().models;
        assert!(provider.fetch_models().await.is_none());
        assert_eq!(
            serde_json::to_value(provider.info().models).unwrap(),
            serde_json::to_value(before).unwrap()
        );
    }
}
