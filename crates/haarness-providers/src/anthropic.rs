use async_stream::try_stream;
use async_trait::async_trait;
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
use crate::media::Unreadable;
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

/// The Messages endpoint under a base URL, as `ANTHROPIC_BASE_URL` or a
/// custom Anthropic-compatible provider gives it. Anthropic's SDKs and Claude
/// Code take the API root (`https://api.anthropic.com`) and add
/// `/v1/messages`, so that is the address services document for them (Z.ai's
/// `https://api.z.ai/api/anthropic`, which answers anything else with a
/// "404 NOT_FOUND" body and no stream); a base that already names a version
/// (`/v1`) or the endpoint is kept.
pub(crate) fn messages_endpoint(base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if base.ends_with("/messages") {
        base.to_owned()
    } else if crate::ends_with_version(base) {
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
            endpoint: std::env::var("ANTHROPIC_BASE_URL").map_or_else(
                |_| "https://api.anthropic.com/v1/messages".into(),
                |base| messages_endpoint(&base),
            ),
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
            endpoint: messages_endpoint(&instance.base_url),
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

/// Anthropic takes 100 images in a request to a model with a 200K-token
/// context window, 600 in one to a larger model, and 32 MB in a request.
/// A model of unknown size gets the smaller number.
pub(crate) fn image_limits(models: &[ModelInfo], model: &str) -> haarness_core::ImageLimits {
    let larger = models
        .iter()
        .find(|info| info.id == model)
        .and_then(|info| info.context_window)
        .is_some_and(|window| window > 200_000);
    haarness_core::ImageLimits {
        count: Some(if larger { 600 } else { 100 }),
        request: Some(32_000_000 - haarness_core::REQUEST_ALLOWANCE),
        ..haarness_core::ImageLimits::default()
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    async fn refresh_models(&self, force: bool) {
        self.live_models
            .refresh(force, || self.fetch_models())
            .await;
    }
    async fn check_api_key(&self, key: &str) -> KeyCheck {
        let (Some(base), Ok(key)) = (
            self.endpoint.strip_suffix("/messages"),
            HeaderValue::from_str(key),
        ) else {
            return KeyCheck::Unsupported;
        };
        let mut headers = self.headers.map();
        headers
            .entry(HeaderName::from_static("anthropic-version"))
            .or_insert(HeaderValue::from_static("2023-06-01"));
        headers.insert(HeaderName::from_static("x-api-key"), key);
        let request = self
            .client
            .get(format!("{base}/models?limit=1"))
            .headers(headers);
        crate::key_check::check(&self.name, request).await
    }
    /// Every request marks its tools, system prompt and newest turns with
    /// `cache_control`.
    fn explicit_prompt_cache_scope(&self, _model: &str) -> Option<String> {
        Some(crate::prompt_cache_scope(
            &self.endpoint,
            self.resolved_api_key().as_deref(),
            &self.headers,
        ))
    }
    fn image_limits(&self, model: &str) -> haarness_core::ImageLimits {
        image_limits(&self.live_models.get(&self.models), model)
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
        let limits = request.parameters.limits;
        let key = self.resolved_api_key();
        if key.is_none() && !self.assume_authenticated && self.headers.is_empty() {
            return Err(CoreError::Provider(format!(
                "{name} isn't connected: its API key is not configured. Connect {name} to add a key.",
                name = self.name
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
        // two non-assistant messages so multi-round loops reuse the cache. A
        // request nothing reads back (a compaction summary) writes no entry.
        let cached = !request.parameters.uncached;
        if cached && let Some(last) = tools.last_mut() {
            last["cache_control"] = json!({"type":"ephemeral"});
        }
        let mut system = json!([{"type":"text","text":request.system}]);
        if cached {
            system[0]["cache_control"] = json!({"type":"ephemeral"});
        }
        let catalog = self.live_models.get(&self.models);
        let selected = catalog.iter().find(|info| info.id == request.model);
        let model_name = selected.map_or_else(
            || haarness_protocol::model_display_name(&request.model, None),
            |info| info.display_name.clone(),
        );
        // An Anthropic-compatible endpoint can be set up with a model that
        // reads no images; Claude reads them all.
        let reads_images = selected.is_none_or(|info| info.images);
        let newest = crate::media::newest_unanswered(&request.messages);
        // Claude takes images and PDFs. Anything else is refused in the
        // person's newest message and is a note everywhere else.
        let block = |item: crate::media::LoadedMedia, place: Unreadable<'_>| -> CoreResult<Value> {
            Ok(if item.is_image() && reads_images {
                json!({"type":"image","source":{"type":"base64","media_type":item.mime_type,"data":item.base64}})
            } else if item.is_pdf() {
                json!({
                    "type":"document",
                    "source":{"type":"base64","media_type":"application/pdf","data":item.base64},
                    "title":item.filename
                })
            } else {
                json!({"type":"text","text":place.stand_in(&item)?})
            })
        };
        let built: Vec<(Value, bool)> = request
            .messages
            .into_iter()
            .enumerate()
            .filter_map(|(index, message)| match message {
                // Claude refuses whitespace-only text blocks, so blank text
                // is left out wherever it would stand alone.
                ProviderMessage::Text { role, content }
                    if role != "developer" && !content.trim().is_empty() =>
                {
                    Some(Ok((json!({"role":role,"content":[{"type":"text","text":content}]}), false)))
                }
                ProviderMessage::Text { .. } => None,
                ProviderMessage::Media { role, .. } if role == "developer" => None,
                ProviderMessage::Media {
                    role,
                    content,
                    attachments,
                } => Some(crate::media::load_all(&attachments).and_then(|media| {
                    let place = Unreadable::at(index, newest, &model_name);
                    let mut blocks = Vec::with_capacity(media.len() + 1);
                    if !content.trim().is_empty() {
                        blocks.push(json!({"type":"text","text":content}));
                    }
                    for item in media {
                        blocks.push(block(item, place)?);
                    }
                    Ok((json!({"role":role,"content":blocks}), false))
                })),
                ProviderMessage::ToolCall {
                    call_id,
                    name,
                    arguments,
                    provider_metadata,
                } => {
                    let mut blocks = provider_metadata.get("anthropic_thinking")
                        .and_then(Value::as_array).cloned().unwrap_or_default();
                    let thinking_first = !blocks.is_empty()
                        && provider_metadata[THINKING_AFTER_WORDS].as_bool() != Some(true);
                    blocks.push(json!({"type":"tool_use","id":crate::tool_ids::portable(&call_id),"name":name,"input":arguments}));
                    Some(Ok((json!({"role":"assistant","content":blocks}), thinking_first)))
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
                        let mut blocks = Vec::with_capacity(media.len() + 1);
                        if !output.trim().is_empty() {
                            blocks.push(json!({"type":"text","text":output}));
                        }
                        // A tool's file never refuses the turn: the model
                        // goes on without it.
                        for item in media {
                            blocks.push(block(item, Unreadable::FromTool)?);
                        }
                        Value::Array(blocks)
                    };
                    Ok((json!({"role":"user","content":[{"type":"tool_result","tool_use_id":crate::tool_ids::portable(&call_id),"content":content,"is_error":is_error}]}), false))
                })),
            })
            .collect::<CoreResult<_>>()?;
        let mut messages = thinking_in_place(built);
        let mut marked = 0;
        for message in messages.iter_mut().rev().filter(|_| cached) {
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
        // The models whose thinking blocks are bound to the prompt before them.
        let bound_thinking = adaptive && binds_thinking(&request.model);
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
        // The live catalog decides which family a model is, as above.
        let sampling = !adaptive;
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
            let beta = THINKING_BINDING_BETA;
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
        let response = crate::sse::send(
            self.client
                .post(&self.endpoint)
                .headers(headers)
                .json(&body),
            limits,
        )
        .await?;
        let trace = crate::RequestTrace::of(&response);
        let provider_id = self.id.clone();
        let mut events = json_stream(response, &trace, cancel, limits);
        // A thinking block's text waits for its signature, which says
        // whether the block is a note for the person (see `crate::notes`).
        let mut hold = crate::notes::NoteHold::new(crate::notes::writes_notes(&request.model));
        Ok(Box::pin(try_stream! {
            let mut tool: Option<(String,String,String,Value)> = None;
            // message_delta carries why the response ended; message_stop reports it.
            let mut stop_reason: Option<String> = None;
            let mut message_id: Option<String> = None;
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
            // Whether Claude wrote words since its last call, and whether the
            // thinking going with the next call came after such words. Saved
            // with the call, so its thinking goes back to Claude in its place
            // (see [`thinking_in_place`]).
            let mut words_since_call = false;
            let mut thinking_after_words = false;
            // input/cache counters arrive on message_start; output arrives on
            // message_delta. Each event reports the merged total so far.
            let mut input_tokens = 0_u64;
            let mut cache_read = 0_u64;
            let mut cache_write = 0_u64;
            let mut output_tokens = 0_u64;
            loop {
                let Some(event) = crate::notes::next_before(&mut events, hold.due()).await else {
                    if let Some(event) = hold.release() { yield event; }
                    continue;
                };
                let Some(event) = event else { break };
                let event = event?;
                match event["type"].as_str().unwrap_or_default() {
                    "message_start" => {
                        message_id = event.pointer("/message/id").and_then(Value::as_str).map(str::to_owned);
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
                        if thinking_blocks.is_empty() {
                            thinking_after_words = words_since_call;
                        }
                        thinking_block = Some(event["content_block"].clone());
                    },
                    "content_block_start" => {
                        thinking_seen = false;
                        thinking_break = false;
                        words_since_call |= event["content_block"]["type"] == "text";
                        if event["content_block"]["type"] == "tool_use" {
                            words_since_call = false;
                            // The arguments stream as JSON fragments, held
                            // until the block stops; the call starts now.
                            yield ProviderEvent::ContentStarted;
                            tool = Some((
                                event["content_block"]["id"].as_str().unwrap_or_default().into(),
                                event["content_block"]["name"].as_str().unwrap_or_default().into(), String::new(), event["content_block"].get("input").cloned().unwrap_or_else(||json!({}))));
                        }
                    },
                    "content_block_delta" => match event["delta"]["type"].as_str().unwrap_or_default() {
                        "text_delta" => if let Some(text) = event["delta"]["text"].as_str().filter(|text| !text.is_empty()) { if native_json { if json_output.is_empty() { yield ProviderEvent::ContentStarted; } json_output.push_str(text); } else { yield ProviderEvent::TextDelta(text.into()); } },
                        "thinking_delta" => if let Some(text) = event["delta"]["thinking"].as_str().filter(|text| !text.is_empty()) {
                            if let Some(block) = &mut thinking_block {
                                let mut value = block["thinking"].as_str().unwrap_or_default().to_owned();
                                value.push_str(text);
                                block["thinking"] = json!(value);
                            }
                            let lead = if std::mem::take(&mut thinking_break) { "\n\n" } else { "" };
                            thinking_seen = true;
                            for event in hold.push(lead, text) { yield event; }
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
                        if let Some(block) = thinking_block.take() {
                            match hold.end(block["signature"].as_str()) {
                                // A note leaves the reasoning, so the next
                                // thought starts without a break.
                                Some(event @ ProviderEvent::Note { .. }) => { thinking_seen = false; yield event; }
                                Some(event) => yield event,
                                None => {}
                            }
                            // Note or not, the block goes back as it came.
                            thinking_blocks.push(block);
                        }
                        if let Some((call_id,name,arguments,initial_input)) = tool.take() {
                        let arguments = if arguments.is_empty() { initial_input } else {
                            haarness_core::tool_call_arguments(&arguments)
                        };
                        if structured_output.is_some() && name == STRUCTURED_OUTPUT_TOOL_NAME {
                            yield ProviderEvent::StructuredOutput(arguments);
                        } else {
                            let provider_metadata = if thinking_blocks.is_empty() { Value::Null } else {
                                let mut metadata = json!({"anthropic_thinking":std::mem::take(&mut thinking_blocks)});
                                if std::mem::take(&mut thinking_after_words) {
                                    metadata[THINKING_AFTER_WORDS] = json!(true);
                                }
                                metadata
                            };
                            yield ProviderEvent::ToolCall { call_id, name, arguments, provider_metadata };
                        }
                    }},
                    "message_delta" => {
                        if let Some(reason) = event.pointer("/delta/stop_reason").and_then(Value::as_str) {
                            stop_reason = Some(reason.to_owned());
                        }
                        if let Some(usage) = event.get("usage") {
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
                    }},
                    "message_stop" => {
                        if native_json && !json_output.is_empty() {
                            let value = serde_json::from_str(&json_output).map_err(|error| CoreError::Provider(format!("Anthropic returned invalid structured output: {error}")))?;
                            yield ProviderEvent::StructuredOutput(value);
                        }
                        // `refusal` is a stop like `end_turn` on the wire; the
                        // runtime fails the turn on it and keeps the partial text.
                        let reason = stop_reason.take().unwrap_or_else(|| "completed".into());
                        trace.mark_finished();
                        crate::log_blocked_finish(&provider_id, &reason, trace.id(), message_id.as_deref());
                        yield ProviderEvent::Finished { reason };
                    },
                    "error" => {
                        let error = CoreError::Provider(event.to_string());
                        trace.failed(&error);
                        Err(error)?;
                    }
                    _ => {}
                }
            }
        }))
    }
}

/// The newest Claude release Haarness has seen. A release past it takes the
/// newest behaviour of every rule below until the live catalog says more.
pub(crate) const NEWEST_CLAUDE: (u32, u32) = (5, 5);

/// A Claude release read from its id under any vendor spelling:
/// `claude-opus-5-5`, `anthropic/claude-sonnet-5.5`,
/// `us.anthropic.claude-fable-5-1-v1:0` and the older `claude-3-5-sonnet`.
/// A dated snapshot (`-20250514`) is not a minor version.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ClaudeRelease {
    pub family: String,
    pub version: (u32, u32),
}

pub(crate) fn claude_release(id: &str) -> Option<ClaudeRelease> {
    let lower = id.to_ascii_lowercase().replace('.', "-");
    let (_, rest) = lower.split_once("claude-")?;
    let number = |part: &str| {
        (!part.is_empty() && part.len() <= 2 && part.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| part.parse::<u32>().ok())
            .flatten()
    };
    let parts: Vec<&str> = rest.split(['-', ':']).collect();
    let (family, numbers) = match parts.first().copied().and_then(number) {
        // The older spelling puts the version first: `claude-3-5-sonnet`.
        Some(_) => {
            let count = parts
                .iter()
                .take_while(|part| number(part).is_some())
                .count();
            (
                parts.get(count).copied().unwrap_or_default(),
                &parts[..count],
            )
        }
        None => {
            let count = parts[1..]
                .iter()
                .take_while(|part| number(part).is_some())
                .count();
            (parts[0], &parts[1..=count])
        }
    };
    let major = numbers.first().copied().and_then(number)?;
    let minor = numbers.get(1).copied().and_then(number).unwrap_or(0);
    Some(ClaudeRelease {
        family: family.to_owned(),
        version: (major, minor),
    })
}

/// Claude 5 onwards and Opus 4.7 onwards think adaptively, take effort
/// levels up to `max` and reject sampling parameters. A model the live
/// catalog lists says so itself; this decides only for the rest.
pub(crate) fn adaptive_thinking(id: &str) -> bool {
    claude_release(id).is_some_and(|release| {
        release.version >= (5, 0) || (release.family == "opus" && release.version >= (4, 7))
    })
}

/// Set on a call's metadata when Claude wrote its thinking after words of the
/// same response, which is where that thinking then goes back.
const THINKING_AFTER_WORDS: &str = "anthropic_thinking_after_words";

/// Messages as Claude wrote them. The words Claude writes before a call are
/// kept as their own message, ahead of the call that carries the turn's
/// thinking; Anthropic joins the two into one turn, which would put the
/// thinking after words it was written before. Claude 5.5 binds a thinking
/// block to everything before it, so such a block was retired on every
/// request (and Claude lost its reasoning, and the cache from there on).
/// A call marked `true` gets the words before it placed after its thinking.
fn thinking_in_place(built: Vec<(Value, bool)>) -> Vec<Value> {
    let mut messages: Vec<Value> = Vec::with_capacity(built.len());
    for (mut message, thinking_first) in built {
        if thinking_first
            && let Some(previous) = messages.last_mut().filter(|previous| {
                previous["role"] == "assistant"
                    && previous["content"]
                        .as_array()
                        .is_some_and(|blocks| blocks.iter().all(|block| block["type"] == "text"))
            })
            && let Some(blocks) = message["content"].as_array_mut()
        {
            let thinking = blocks
                .iter()
                .take_while(|block| {
                    matches!(
                        block["type"].as_str(),
                        Some("thinking" | "redacted_thinking")
                    )
                })
                .count();
            let words = previous["content"].as_array().cloned().unwrap_or_default();
            blocks.splice(thinking..thinking, words);
            *previous = message;
            continue;
        }
        messages.push(message);
    }
    messages
}

/// The beta that lets a request ask Claude to retire a thinking block whose
/// prompt changed instead of refusing the request (Anthropic's API takes it
/// as a header, Bedrock's Converse in the request body).
pub(crate) const THINKING_BINDING_BETA: &str = "thinking-binding-controls-2026-08-01";

/// Claude 5.5 onwards (Fable and Mythos from 5.1) binds its thinking blocks to
/// the prompt before them, so a refreshed prompt must retire them.
pub(crate) fn binds_thinking(id: &str) -> bool {
    claude_release(id).is_some_and(|release| {
        release.version >= (5, 5)
            || (matches!(release.family.as_str(), "fable" | "mythos") && release.version >= (5, 1))
    })
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
    // Haiku and Sonnet 4.5 take no `effort` but do take a thinking budget,
    // which Haarness derives from the same low/medium/high levels.
    let budget_thinking = entry
        .pointer("/capabilities/thinking/types/enabled/supported")
        .and_then(Value::as_bool)
        == Some(true)
        && info.reasoning;
    if let Some(effort) = entry.pointer("/capabilities/effort") {
        if effort["supported"].as_bool() == Some(false) {
            info.reasoning_efforts = if budget_thinking {
                ["low", "medium", "high"].map(str::to_owned).to_vec()
            } else {
                Vec::new()
            };
        } else if effort["supported"].as_bool() == Some(true) {
            // Every level the entry lists, in its order, a new one included.
            info.reasoning_efforts = effort
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(name, level)| {
                    name.as_str() != "supported" && level["supported"].as_bool() == Some(true)
                })
                .map(|(name, _)| name.clone())
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

pub(crate) fn preferred_models() -> Vec<ModelInfo> {
    vec![
        model("claude-opus-5-5"),
        model("claude-sonnet-5-5"),
        model("claude-haiku-5-5"),
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
pub(crate) mod tests {
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

    use haarness_core::{
        CoreError, KeyCheck, ModelProvider, ProviderEvent, ProviderMessage, ProviderRequest,
    };

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
            // The addresses services document for Claude Code.
            (
                "https://api.z.ai/api/anthropic",
                "https://api.z.ai/api/anthropic/v1/messages",
            ),
            (
                "https://api.deepseek.com/anthropic/",
                "https://api.deepseek.com/anthropic/v1/messages",
            ),
            (
                " https://gateway.example/claude/v2 ",
                "https://gateway.example/claude/v2/messages",
            ),
        ] {
            assert_eq!(super::messages_endpoint(base), endpoint, "{base}");
        }
    }

    use super::{AnthropicProvider, THINKING_AFTER_WORDS, discovered_model, discovered_model_for};

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
    fn the_prompt_cache_scope_separates_accounts_without_holding_the_key() {
        let provider = |key: &str| {
            let mut provider =
                AnthropicProvider::for_test("https://api.anthropic.com/v1/messages".into());
            provider.api_key = Some(key.to_owned().into());
            provider
        };
        let scope = provider("sk-ant-fixture-a")
            .explicit_prompt_cache_scope("claude-sonnet-5-5")
            .expect("every Anthropic request marks its prompt for caching");
        assert!(!scope.contains("fixture"));
        assert_eq!(
            provider("sk-ant-fixture-a").explicit_prompt_cache_scope("claude-haiku-5-5"),
            Some(scope.clone())
        );
        assert_ne!(
            provider("sk-ant-fixture-b").explicit_prompt_cache_scope("claude-sonnet-5-5"),
            Some(scope)
        );
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
    fn a_custom_endpoint_at_a_documented_claude_code_address_reaches_its_messages() {
        let at = |base: &str| {
            AnthropicProvider::new(crate::NativeInstance {
                id: "my-zai".into(),
                name: "My Z.ai".into(),
                base_url: base.into(),
                api_key: None,
                models: vec!["glm-4.5-air".into()],
                discover_models: false,
                model_defaults: crate::CompatibleModelDefaults::default(),
                assume_authenticated: false,
                headers: crate::ProviderHeaders::default(),
            })
            .endpoint
        };
        assert_eq!(
            at("https://api.z.ai/api/anthropic"),
            "https://api.z.ai/api/anthropic/v1/messages"
        );
        assert_eq!(
            at("https://api.z.ai/api/anthropic/v1/"),
            "https://api.z.ai/api/anthropic/v1/messages"
        );
        assert_eq!(
            at("https://models.example.test/anthropic/v1/messages"),
            "https://models.example.test/anthropic/v1/messages"
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

    #[tokio::test]
    async fn a_missing_key_says_what_to_connect_in_words_every_client_can_follow() {
        let mut provider = AnthropicProvider::for_test("http://127.0.0.1:9/messages".into());
        provider.api_key = None;
        let request = ProviderRequest {
            model: "claude-test".into(),
            system: String::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        };
        let Err(error) = provider.stream(request, CancellationToken::new()).await else {
            panic!("a provider without a key cannot stream");
        };
        // The same text reaches the TUI, Desktop and `haarness run`, so it
        // names no client's command; Desktop's e2e matches the middle clause.
        assert_eq!(
            error.to_string(),
            "provider error: Anthropic isn't connected: its API key is not configured. Connect Anthropic to add a key."
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

    async fn refused_messages(
        State(state): State<FixtureState>,
        Json(request): Json<Value>,
    ) -> ([(axum::http::HeaderName, &'static str); 2], String) {
        let partial = request["messages"][0]["content"][0]["text"] == "partial";
        state.0.lock().push(request);
        let mut stream = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_refused","usage":{"input_tokens":12,"output_tokens":0}}}"#,
        ];
        if partial {
            stream.extend([
                r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
                r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Before the stop"}}"#,
                r#"data: {"type":"content_block_stop","index":0}"#,
            ]);
        }
        stream.extend([
            r#"data: {"type":"message_delta","delta":{"stop_reason":"refusal","stop_sequence":null},"usage":{"output_tokens":3}}"#,
            r#"data: {"type":"message_stop"}"#,
        ]);
        (
            [
                (CONTENT_TYPE, "text/event-stream"),
                (
                    axum::http::HeaderName::from_static("request-id"),
                    "req_fixture",
                ),
            ],
            stream.join("\n\n") + "\n\n",
        )
    }

    /// A refusal is an ordinary `message_stop` on the wire. Reported as
    /// "completed" it ended the turn as a blank reply.
    #[tokio::test]
    async fn a_refusal_stop_reaches_the_runtime_as_a_blocked_finish() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/messages", post(refused_messages))
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = AnthropicProvider::for_test(format!("http://{address}/messages"));
        for prompt in ["empty", "partial"] {
            let events = provider
                .stream(
                    ProviderRequest {
                        model: "claude-test".into(),
                        system: "test".into(),
                        messages: vec![ProviderMessage::Text {
                            role: "user".into(),
                            content: prompt.into(),
                        }],
                        tools: Vec::new(),
                        reasoning_effort: None,
                        parameters: haarness_core::RequestParameters::default(),
                        output_schema: None,
                    },
                    CancellationToken::new(),
                )
                .await
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>()
                .await;
            let text: String = events
                .iter()
                .filter_map(|event| match event {
                    ProviderEvent::TextDelta(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                text,
                if prompt == "partial" {
                    "Before the stop"
                } else {
                    ""
                }
            );
            let Some(ProviderEvent::Finished { reason }) = events.last() else {
                panic!("the response must finish: {events:?}");
            };
            assert_eq!(reason, "refusal");
            let error = ProviderEvent::finish_error(reason).expect("a refusal is not a completion");
            assert!(matches!(error, CoreError::ProviderBlocked(_)), "{error:?}");
            assert!(
                error
                    .to_string()
                    .contains("safety check stopped this response"),
                "{error}"
            );
        }
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

    /// Claude thinks, says what it will do, calls a tool, then (with words
    /// first this time) thinks again and calls another.
    async fn narrated_calls(
        State(state): State<FixtureState>,
        Json(request): Json<Value>,
    ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
        state.0.lock().push(request);
        let stream = [
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"first"}}"#,
            r#"data: {"type":"content_block_stop","index":0}"#,
            r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Reading the date module."}}"#,
            r#"data: {"type":"content_block_stop","index":1}"#,
            r#"data: {"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"read"}}"#,
            r#"data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
            r#"data: {"type":"content_block_stop","index":2}"#,
            r#"data: {"type":"content_block_start","index":3,"content_block":{"type":"text","text":""}}"#,
            r#"data: {"type":"content_block_delta","index":3,"delta":{"type":"text_delta","text":" Then its tests."}}"#,
            r#"data: {"type":"content_block_stop","index":3}"#,
            r#"data: {"type":"content_block_start","index":4,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"data: {"type":"content_block_delta","index":4,"delta":{"type":"signature_delta","signature":"second"}}"#,
            r#"data: {"type":"content_block_stop","index":4}"#,
            r#"data: {"type":"content_block_start","index":5,"content_block":{"type":"tool_use","id":"toolu_2","name":"read"}}"#,
            r#"data: {"type":"content_block_delta","index":5,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
            r#"data: {"type":"content_block_stop","index":5}"#,
            r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}"#,
            r#"data: {"type":"message_stop"}"#,
        ]
        .join("\n\n")
            + "\n\n";
        ([(CONTENT_TYPE, "text/event-stream")], stream)
    }

    /// Haarness keeps the words before a call as their own message; Anthropic
    /// joins them with the call's turn. Claude 5.5 refuses (or, asked to,
    /// retires) a thinking block that ends up after words it was written
    /// before, so each block goes back where Claude wrote it.
    #[tokio::test]
    async fn thinking_goes_back_before_the_words_claude_wrote_after_it() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/messages", post(narrated_calls))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = AnthropicProvider::for_test(format!("http://{address}/messages"));
        let request = |messages| ProviderRequest {
            model: "claude-opus-5-5".into(),
            system: "test".into(),
            messages,
            tools: Vec::new(),
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        };
        let user = ProviderMessage::Text {
            role: "user".into(),
            content: "fix the date bug".into(),
        };
        let events = provider
            .stream(request(vec![user.clone()]), CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        let calls: Vec<_> = events
            .into_iter()
            .filter_map(|event| match event.unwrap() {
                ProviderEvent::ToolCall {
                    call_id,
                    provider_metadata,
                    ..
                } => Some((call_id, provider_metadata)),
                _ => None,
            })
            .collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].1["anthropic_thinking"][0]["signature"], "first");
        assert!(calls[0].1.get(THINKING_AFTER_WORDS).is_none(), "{calls:?}");
        assert_eq!(calls[1].1["anthropic_thinking"][0]["signature"], "second");
        assert_eq!(calls[1].1[THINKING_AFTER_WORDS], true);

        // The chat as the runtime keeps it: each call's words before it, the
        // results after the calls.
        let mut history = vec![user];
        let mut results = Vec::new();
        for ((call_id, provider_metadata), words) in calls
            .into_iter()
            .zip(["Reading the date module.", " Then its tests."])
        {
            history.push(ProviderMessage::Text {
                role: "assistant".into(),
                content: words.into(),
            });
            history.push(ProviderMessage::ToolCall {
                call_id: call_id.clone(),
                name: "read".into(),
                arguments: json!({}),
                provider_metadata,
            });
            results.push(ProviderMessage::ToolResult {
                call_id,
                name: "read".into(),
                output: "fn month() {}".into(),
                is_error: false,
                attachments: Vec::new(),
            });
        }
        history.extend(results);
        provider
            .stream(request(history), CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        let requests = state.0.lock();
        let shape: Vec<Vec<String>> = requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| {
                message["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|block| {
                        block["signature"]
                            .as_str()
                            .or(block["text"].as_str())
                            .or(block["id"].as_str())
                            .or(block["type"].as_str())
                            .unwrap()
                            .to_owned()
                    })
                    .collect()
            })
            .collect();
        assert_eq!(
            shape,
            [
                vec!["fix the date bug"],
                vec!["first", "Reading the date module.", "toolu_1"],
                vec![" Then its tests."],
                vec!["second", "toolu_2"],
                vec!["tool_result"],
                vec!["tool_result"],
            ]
        );
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
    async fn an_unknown_new_claude_model_appears_with_its_listed_levels() {
        async fn listing(State(state): State<FixtureState>) -> Json<Value> {
            let mut data = vec![json!({
                "id": "claude-nova-7",
                "display_name": "Claude Nova 7",
                "capabilities": {
                    "thinking": {"supported": true, "types": {"adaptive": {"supported": true}}},
                    "effort": {
                        "supported": true,
                        "low": {"supported": true},
                        "high": {"supported": true},
                        "max": {"supported": true},
                        "ultra": {"supported": true},
                        "xhigh": {"supported": false}
                    }
                }
            })];
            // The second read no longer lists Sonnet 5.5.
            if state.0.lock().is_empty() {
                data.push(json!({"id": "claude-sonnet-5-5", "display_name": "Claude Sonnet 5.5"}));
            }
            state.0.lock().push(json!({}));
            Json(json!({"data": data, "has_more": false}))
        }
        let state = FixtureState::default();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new()
            .route("/models", get(listing))
            .with_state(state.clone());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = AnthropicProvider::for_test(format!("http://{address}/messages"))
            .discover_models()
            .await;
        let ids = |provider: &AnthropicProvider| {
            provider
                .info()
                .models
                .into_iter()
                .map(|model| model.id)
                .collect::<Vec<_>>()
        };
        // The live list replaces the static one.
        assert_eq!(ids(&provider), ["claude-sonnet-5-5", "claude-nova-7"]);
        let nova = provider
            .info()
            .models
            .into_iter()
            .find(|model| model.id == "claude-nova-7")
            .unwrap();
        assert_eq!(nova.reasoning_efforts, ["low", "high", "max", "ultra"]);
        assert!(nova.reasoning && nova.tools);
        assert!(
            nova.pricing.is_none(),
            "no price is borrowed from another release"
        );
        // A model the next read no longer lists is gone, though the static
        // list still names it.
        let later = AnthropicProvider::for_test(format!("http://{address}/messages"))
            .discover_models()
            .await;
        assert!(
            later
                .models
                .iter()
                .any(|model| model.id == "claude-sonnet-5-5")
        );
        assert_eq!(ids(&later), ["claude-nova-7"]);
    }

    #[tokio::test]
    async fn sampling_follows_the_live_catalog_before_the_release_rule() {
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
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = AnthropicProvider::for_test(format!("http://{address}/messages"));
        {
            let mut live = provider.adaptive_models.write().unwrap();
            live.insert("claude-opus-6".into(), false);
            live.insert("future-deployment".into(), true);
        }
        for model in ["claude-opus-6", "future-deployment", "claude-opus-7"] {
            let request = ProviderRequest {
                model: model.into(),
                system: "test".into(),
                messages: vec![ProviderMessage::Text {
                    role: "user".into(),
                    content: "hi".into(),
                }],
                tools: vec![],
                reasoning_effort: None,
                parameters: haarness_core::RequestParameters {
                    temperature: Some(0.3),
                    ..haarness_core::RequestParameters::default()
                },
                output_schema: None,
            };
            let stream = provider
                .stream(request, CancellationToken::new())
                .await
                .unwrap();
            let _ = stream.collect::<Vec<_>>().await;
        }
        let bodies = state.0.lock();
        // The catalog says Opus 6 is not adaptive: sampling goes through.
        assert_eq!(bodies[0]["temperature"], 0.3);
        assert!(bodies[0].get("thinking").is_none());
        // An adaptive deployment the rule cannot name takes none.
        assert!(bodies[1].get("temperature").is_none());
        // Unlisted, a newer release than any known takes the newest behaviour.
        assert!(bodies[2].get("temperature").is_none());
        assert_eq!(bodies[2]["thinking"]["type"], "adaptive");
    }

    #[test]
    fn claude_releases_are_read_from_any_spelling() {
        use super::{ClaudeRelease, adaptive_thinking, binds_thinking, claude_release};
        let release = |family: &str, version| {
            Some(ClaudeRelease {
                family: family.into(),
                version,
            })
        };
        assert_eq!(claude_release("claude-opus-5-5"), release("opus", (5, 5)));
        assert_eq!(
            claude_release("anthropic/claude-sonnet-5.5"),
            release("sonnet", (5, 5))
        );
        assert_eq!(
            claude_release("us.anthropic.claude-fable-5-1-v1:0"),
            release("fable", (5, 1))
        );
        assert_eq!(
            claude_release("claude-opus-4-20250514"),
            release("opus", (4, 0))
        );
        assert_eq!(
            claude_release("claude-3-5-sonnet-20241022"),
            release("sonnet", (3, 5))
        );
        assert_eq!(claude_release("claude-test"), None);
        assert_eq!(claude_release("MiniMax-M3"), None);
        // Listed releases keep their answers; a newer one takes the newest.
        for id in [
            "claude-fable-5",
            "claude-mythos-5",
            "claude-opus-5",
            "claude-sonnet-5",
            "claude-haiku-5-5",
            "claude-opus-4-7",
            "claude-opus-4-8",
            "claude-opus-6",
            "claude-nova-7",
        ] {
            assert!(adaptive_thinking(id), "{id}");
        }
        for id in [
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "claude-haiku-4-5",
            "claude-test",
        ] {
            assert!(!adaptive_thinking(id), "{id}");
        }
        for id in [
            "claude-fable-5-1",
            "claude-mythos-5-1",
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "claude-sonnet-6",
        ] {
            assert!(binds_thinking(id), "{id}");
        }
        for id in ["claude-fable-5", "claude-opus-5", "claude-mythos-5"] {
            assert!(!binds_thinking(id), "{id}");
        }
    }

    #[tokio::test]
    async fn a_key_is_checked_with_one_model_listing() {
        async fn listing(headers: axum::http::HeaderMap) -> axum::response::Response {
            use axum::response::IntoResponse as _;
            assert_eq!(headers["anthropic-version"], "2023-06-01");
            if headers["x-api-key"] == "sk-ant-good" {
                Json(json!({"data": [], "has_more": false})).into_response()
            } else {
                (
                    axum::http::StatusCode::UNAUTHORIZED,
                    Json(json!({"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}})),
                )
                    .into_response()
            }
        }
        let router = Router::new().route("/models", get(listing));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = AnthropicProvider::for_test(format!("http://{address}/messages"));
        assert_eq!(
            provider.check_api_key("sk-ant-good").await,
            KeyCheck::Accepted
        );
        // This endpoint is not api.anthropic.com, so a refusal may be the
        // gateway's: the key is kept with what it answered.
        assert_eq!(
            provider.check_api_key("sk-ant-fake").await,
            KeyCheck::Unchecked("Anthropic answered HTTP 401 (invalid x-api-key)".into())
        );
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
                    // Written by another model the chat used first (Kimi).
                    call_id: "functions.read:0".into(),
                    name: "read".into(),
                    arguments: json!({"path": "Cargo.toml"}),
                    provider_metadata: Value::Null,
                },
                ProviderMessage::ToolResult {
                    call_id: "functions.read:0".into(),
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
        // The call starts at its block's start, before its arguments stream.
        let started = events
            .iter()
            .position(|event| matches!(event, ProviderEvent::ContentStarted));
        let called = events
            .iter()
            .position(|event| matches!(event, ProviderEvent::ToolCall { .. }));
        assert!(started.is_some() && started < called, "{events:?}");
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
        // Claude takes only ^[a-zA-Z0-9_-]+$ ids; call and result still match.
        assert_eq!(
            requests[0]["messages"][0]["content"][0]["id"],
            "functions_read_0"
        );
        assert_eq!(
            requests[0]["messages"][1]["content"][0]["tool_use_id"],
            "functions_read_0"
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
    async fn whitespace_only_text_is_never_sent_as_a_text_block() {
        fn texts(value: &Value, found: &mut Vec<String>) {
            match value {
                Value::Object(map) => {
                    if map.get("type").and_then(Value::as_str) == Some("text") {
                        found.push(map["text"].as_str().unwrap_or_default().to_owned());
                    }
                    map.values().for_each(|value| texts(value, found));
                }
                Value::Array(items) => items.iter().for_each(|value| texts(value, found)),
                _ => {}
            }
        }
        let state = FixtureState::default();
        let router = Router::new()
            .route("/messages", post(messages))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = AnthropicProvider::for_test(format!("http://{address}/messages"));
        let image = || "data:image/png;base64,iVBORw0KGgo=".to_owned();
        let request = ProviderRequest {
            model: "claude-test".into(),
            system: "test".into(),
            messages: vec![
                ProviderMessage::Text {
                    role: "user".into(),
                    content: "look".into(),
                },
                ProviderMessage::Text {
                    role: "assistant".into(),
                    content: "\n\n".into(),
                },
                ProviderMessage::ToolCall {
                    call_id: "shot".into(),
                    name: "screenshot".into(),
                    arguments: json!({}),
                    provider_metadata: Value::Null,
                },
                ProviderMessage::ToolResult {
                    call_id: "shot".into(),
                    name: "screenshot".into(),
                    output: String::new(),
                    is_error: false,
                    attachments: vec![image()],
                },
                ProviderMessage::Media {
                    role: "user".into(),
                    content: " \n".into(),
                    attachments: vec![image()],
                },
            ],
            tools: Vec::new(),
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        };
        provider
            .stream(request, CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;

        let requests = state.0.lock();
        let messages = requests[0]["messages"].as_array().unwrap();
        let roles = messages
            .iter()
            .map(|message| message["role"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(roles, ["user", "assistant", "user", "user"]);
        // Every text block anywhere, tool results included, holds text.
        let mut found = Vec::new();
        texts(&requests[0]["messages"], &mut found);
        assert_eq!(found, ["look"]);
        assert_eq!(
            messages[2]["content"][0]["content"][0]["type"], "image",
            "the screenshot is the tool result's only block"
        );
        assert_eq!(messages[3]["content"][0]["type"], "image");
        assert_eq!(
            messages[3]["content"][0]["cache_control"]["type"], "ephemeral",
            "the cache breakpoint lands on a block that is sent"
        );
    }

    #[tokio::test]
    async fn a_request_nothing_reads_back_marks_no_cache_breakpoints() {
        let state = FixtureState::default();
        let router = Router::new()
            .route("/messages", post(messages))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = AnthropicProvider::for_test(format!("http://{address}/messages"));
        for uncached in [false, true] {
            let request = ProviderRequest {
                model: "claude-test".into(),
                system: "summarize".into(),
                messages: vec![
                    ProviderMessage::Text {
                        role: "user".into(),
                        content: "the conversation".into(),
                    },
                    ProviderMessage::Text {
                        role: "assistant".into(),
                        content: "a reply".into(),
                    },
                    ProviderMessage::Text {
                        role: "user".into(),
                        content: "Summarize this conversation".into(),
                    },
                ],
                tools: vec![haarness_core::ToolDefinition {
                    name: "read".into(),
                    description: "read".into(),
                    input_schema: json!({"type": "object"}),
                }],
                reasoning_effort: None,
                parameters: haarness_core::RequestParameters {
                    uncached,
                    ..haarness_core::RequestParameters::default()
                },
                output_schema: None,
            };
            provider
                .stream(request, CancellationToken::new())
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await;
        }
        let requests = state.0.lock();
        let marks = |request: &Value| request.to_string().matches("cache_control").count();
        // Tools, system and the two user turns.
        assert_eq!(marks(&requests[0]), 4, "{}", requests[0]);
        assert_eq!(marks(&requests[1]), 0, "{}", requests[1]);
        assert_eq!(requests[1]["system"][0]["text"], "summarize");
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
        // Anthropic lists Haiku 4.5 with budget thinking and no effort parameter.
        let haiku = super::discovered_model(&json!({"id":"claude-haiku-4-5-20251001", "capabilities":{
            "effort":{"supported":false,"low":{"supported":false}},
            "thinking":{"supported":true,"types":{"enabled":{"supported":true},"adaptive":{"supported":false}}}
        }})).unwrap();
        assert_eq!(haiku.reasoning_efforts, ["low", "medium", "high"]);
        let plain = super::discovered_model(&json!({"id":"claude-plain", "capabilities":{
            "effort":{"supported":false},
            "thinking":{"supported":false}
        }}))
        .unwrap();
        assert!(plain.reasoning_efforts.is_empty() && !plain.reasoning);
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

    /// Claude 5.5 thinks, writes its note for the person as a thinking block
    /// of its own, then calls a tool. Each block's signature says its kind.
    fn noted_call_events() -> Vec<String> {
        let thinking = crate::notes::tests::thinking_signature();
        let note = crate::notes::tests::note_signature();
        [
            r#"{"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":12,"output_tokens":1}}}"#.to_owned(),
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#.into(),
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Checking the failing test."}}"#.into(),
            format!(r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"signature_delta","signature":"{thinking}"}}}}"#),
            r#"{"type":"content_block_stop","index":0}"#.into(),
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"thinking","thinking":""}}"#.into(),
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"thinking_delta","thinking":"I'll run "}}"#.into(),
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"thinking_delta","thinking":"the tests now."}}"#.into(),
            format!(r#"{{"type":"content_block_delta","index":1,"delta":{{"type":"signature_delta","signature":"{note}"}}}}"#),
            r#"{"type":"content_block_stop","index":1}"#.into(),
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"bash"}}"#.into(),
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"cargo test\"}"}}"#.into(),
            r#"{"type":"content_block_stop","index":2}"#.into(),
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":30}}"#.into(),
            r#"{"type":"message_stop"}"#.into(),
        ]
        .into()
    }

    async fn noted_call(
        State(state): State<FixtureState>,
        Json(request): Json<Value>,
    ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
        state.0.lock().push(request);
        let stream = noted_call_events()
            .iter()
            .fold(String::new(), |mut stream, event| {
                use std::fmt::Write as _;
                let _ = write!(stream, "data: {event}\n\n");
                stream
            });
        ([(CONTENT_TYPE, "text/event-stream")], stream)
    }

    async fn noted_call_stream(model: &str) -> Vec<ProviderEvent> {
        let router = Router::new()
            .route("/messages", post(noted_call))
            .with_state(FixtureState::default());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = AnthropicProvider::for_test(format!("http://{address}/messages"));
        provider
            .stream(
                ProviderRequest {
                    model: model.into(),
                    system: "test".into(),
                    messages: vec![ProviderMessage::Text {
                        role: "user".into(),
                        content: "the tests fail".into(),
                    }],
                    tools: Vec::new(),
                    reasoning_effort: None,
                    parameters: haarness_core::RequestParameters::default(),
                    output_schema: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect()
            .await
    }

    fn reasoning_text(events: &[ProviderEvent]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::ReasoningDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn notes(events: &[ProviderEvent]) -> Vec<(&str, bool)> {
        events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::Note {
                    text,
                    streamed_as_reasoning,
                } => Some((text.as_str(), *streamed_as_reasoning)),
                _ => None,
            })
            .collect()
    }

    fn call_metadata(events: &[ProviderEvent]) -> Value {
        events
            .iter()
            .find_map(|event| match event {
                ProviderEvent::ToolCall {
                    provider_metadata, ..
                } => Some(provider_metadata.clone()),
                _ => None,
            })
            .unwrap()
    }

    /// The note Claude 5.5 writes before a call reaches the person as a note,
    /// whole, and never as reasoning; the thinking before it stays reasoning.
    #[tokio::test]
    async fn a_note_block_becomes_a_note_and_thinking_stays_reasoning() {
        let events = noted_call_stream("claude-opus-5-5").await;
        assert_eq!(reasoning_text(&events), "Checking the failing test.");
        assert_eq!(notes(&events), [("I'll run the tests now.", false)]);
        let at = |wanted: fn(&ProviderEvent) -> bool| events.iter().position(wanted).unwrap();
        assert!(
            at(|event| matches!(event, ProviderEvent::ReasoningDelta(_)))
                < at(|event| matches!(event, ProviderEvent::Note { .. }))
        );
        assert!(
            at(|event| matches!(event, ProviderEvent::Note { .. }))
                < at(|event| matches!(event, ProviderEvent::ToolCall { .. }))
        );
    }

    /// Both blocks go back with the call exactly as they came, the note's
    /// signature included, whether its text waited or streamed.
    #[tokio::test]
    async fn the_note_block_goes_back_with_the_call_unchanged() {
        let waited = noted_call_stream("claude-sonnet-5-5").await;
        let streamed = noted_call_stream("claude-sonnet-4-5").await;
        let metadata = call_metadata(&waited);
        assert_eq!(metadata, call_metadata(&streamed));
        assert_eq!(
            metadata,
            json!({"anthropic_thinking":[
                {"type":"thinking","thinking":"Checking the failing test.","signature":crate::notes::tests::thinking_signature()},
                {"type":"thinking","thinking":"I'll run the tests now.","signature":crate::notes::tests::note_signature()},
            ]})
        );
        // A model that writes no notes does not wait: its thinking streams,
        // and a note, should one come, is moved after.
        assert_eq!(
            reasoning_text(&streamed),
            "Checking the failing test.\n\nI'll run the tests now."
        );
        assert_eq!(notes(&streamed), [("I'll run the tests now.", true)]);
    }

    /// Thinking that waits to be told from a note shows as reasoning once it
    /// has waited a few seconds, with the block still open.
    #[tokio::test]
    async fn waiting_thinking_shows_before_its_block_ends() {
        async fn slow(
            State(state): State<FixtureState>,
            Json(request): Json<Value>,
        ) -> (
            [(axum::http::HeaderName, &'static str); 1],
            axum::body::Body,
        ) {
            state.0.lock().push(request);
            let events = noted_call_events();
            let body = async_stream::stream! {
                for (index, event) in events.into_iter().enumerate() {
                    // The first block's signature comes long after its text.
                    if index == 3 {
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                    yield Ok::<_, std::convert::Infallible>(format!("data: {event}\n\n"));
                }
            };
            (
                [(CONTENT_TYPE, "text/event-stream")],
                axum::body::Body::from_stream(body),
            )
        }
        let router = Router::new()
            .route("/messages", post(slow))
            .with_state(FixtureState::default());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = AnthropicProvider::for_test(format!("http://{address}/messages"));
        let started = std::time::Instant::now();
        let mut stream = provider
            .stream(
                ProviderRequest {
                    model: "claude-opus-5-5".into(),
                    system: "test".into(),
                    messages: vec![ProviderMessage::Text {
                        role: "user".into(),
                        content: "the tests fail".into(),
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
        let mut events = Vec::new();
        let mut shown = None;
        while let Some(event) = stream.next().await {
            let event = event.unwrap();
            if matches!(event, ProviderEvent::ReasoningDelta(_)) {
                shown.get_or_insert_with(|| started.elapsed());
            }
            events.push(event);
        }
        let shown = shown.unwrap();
        assert!(
            shown >= std::time::Duration::from_millis(2_900)
                && shown < std::time::Duration::from_millis(4_500),
            "{shown:?}"
        );
        assert_eq!(reasoning_text(&events), "Checking the failing test.");
        assert_eq!(notes(&events), [("I'll run the tests now.", false)]);
    }

    /// A real Claude Sonnet 5.5 answer, recorded live: it thought, said in
    /// words what it would do, and called `bash`.
    const RECORDED: &str = include_str!("testdata/claude-sonnet-5-5-thinking-call.sse");

    /// The signature of the recorded answer's thinking block.
    pub(crate) fn recorded_signature() -> String {
        RECORDED
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|data| serde_json::from_str::<Value>(data).ok())
            .filter_map(|event| {
                event
                    .pointer("/delta/signature")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect()
    }

    /// Its thinking stays reasoning, its words stay reply text, and the block
    /// goes back with the call as it came.
    #[tokio::test]
    async fn a_recorded_claude_5_5_answer_keeps_its_thinking_as_reasoning() {
        async fn recorded() -> ([(axum::http::HeaderName, &'static str); 1], &'static str) {
            ([(CONTENT_TYPE, "text/event-stream")], RECORDED)
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/messages", post(recorded)))
                .await
                .unwrap()
        });
        let provider = AnthropicProvider::for_test(format!("http://{address}/messages"));
        let events: Vec<_> = provider
            .stream(
                ProviderRequest {
                    model: "claude-sonnet-5-5".into(),
                    system: "test".into(),
                    messages: vec![ProviderMessage::Text {
                        role: "user".into(),
                        content: "add figs".into(),
                    }],
                    tools: Vec::new(),
                    reasoning_effort: None,
                    parameters: haarness_core::RequestParameters::default(),
                    output_schema: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect()
            .await;
        assert!(notes(&events).is_empty(), "{events:?}");
        let thinking = reasoning_text(&events);
        assert!(thinking.starts_with("I need to add a line"), "{thinking}");
        let words: String = events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::TextDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(words.starts_with("I'll append"), "{words}");
        let metadata = call_metadata(&events);
        assert_eq!(
            metadata["anthropic_thinking"][0]["thinking"],
            json!(thinking)
        );
        assert_eq!(
            metadata["anthropic_thinking"][0]["signature"],
            json!(recorded_signature())
        );
    }
}
