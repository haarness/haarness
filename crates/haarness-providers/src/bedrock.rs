//! Amazon Bedrock: Converse answers streamed with ConverseStream, signed with
//! a Bedrock API key (a bearer token) or SigV4, and the models the account's
//! region offers, read from the Bedrock control plane.

use std::{
    collections::{BTreeMap, HashMap},
    fmt::Write as _,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
};

use async_stream::try_stream;
use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt as _;
use haarness_core::{
    CoreError, CoreResult, KeyCheck, ModelProvider, ProviderEvent, ProviderMessage,
    ProviderRequest, ProviderStream, STRUCTURED_OUTPUT_TOOL_NAME, StreamLimits,
    structured_output_tool,
};
use haarness_protocol::{ModelInfo, ProviderInfo};
use hmac::{Hmac, Mac as _};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;

use crate::{
    CompatibleModelDefaults, CredentialStore, NativeInstance, ProviderHeaders, aws_event_stream,
};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
struct AwsCredentials {
    access_key: String,
    secret_key: SecretString,
    session_token: Option<SecretString>,
}

/// Shown until the account's own list arrives, and whenever it cannot be read.
const DEFAULT_MODELS: &[&str] = &[
    "global.anthropic.claude-sonnet-5-5",
    "global.anthropic.claude-haiku-5-5",
    "amazon.nova-pro-v1:0",
];

/// How a model takes a reasoning level, as its Converse metadata describes
/// the `additionalModelRequestFields` it accepts.
#[derive(Clone, Debug, PartialEq)]
enum Reasoning {
    /// A level by name at `path`: `reasoning_config`, `output_config.effort`.
    Level {
        path: Vec<String>,
        levels: Vec<String>,
    },
    /// A thinking budget per level under `reasoningConfig`, with an
    /// `enabled` switch beside it when `switch` is set.
    Budget {
        switch: bool,
        budgets: Vec<(String, u64)>,
    },
}

/// What a request for one model may carry beyond the common Converse shape.
#[derive(Clone, Debug, Default, PartialEq)]
struct ModelControls {
    reasoning: Option<Reasoning>,
    /// The model takes `cachePoint` blocks (Bedrock's explicit prompt caching).
    cache_points: bool,
    /// The most output the model writes, from its metadata.
    max_output_tokens: Option<u64>,
}

pub struct BedrockProvider {
    client: reqwest::Client,
    instance: NativeInstance,
    auth_store: Option<Arc<CredentialStore>>,
    region: String,
    bearer: Option<SecretString>,
    credentials: Option<AwsCredentials>,
    /// The configured models, or a few current ones until discovery answers.
    models: Vec<ModelInfo>,
    live_models: crate::live_models::LiveModels,
    /// What the account's discovered models take, by the id requests use.
    controls: RwLock<HashMap<String, ModelControls>>,
}

impl BedrockProvider {
    pub fn from_environment(auth_store: Option<Arc<CredentialStore>>) -> Self {
        let region = std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| "us-east-1".into());
        let base_url = std::env::var("AWS_BEDROCK_RUNTIME_ENDPOINT")
            .unwrap_or_else(|_| format!("https://bedrock-runtime.{region}.amazonaws.com"));
        let instance = NativeInstance {
            id: "amazon-bedrock".into(),
            name: "Amazon Bedrock".into(),
            base_url,
            api_key: None,
            models: DEFAULT_MODELS.iter().map(|id| (*id).to_owned()).collect(),
            discover_models: true,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: false,
            headers: ProviderHeaders::default(),
        };
        Self::new_with_region(instance, auth_store, region)
    }

    pub fn new(instance: NativeInstance, auth_store: Option<Arc<CredentialStore>>) -> Self {
        let region = std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| "us-east-1".into());
        Self::new_with_region(instance, auth_store, region)
    }

    fn new_with_region(
        instance: NativeInstance,
        auth_store: Option<Arc<CredentialStore>>,
        region: String,
    ) -> Self {
        let bearer = instance
            .api_key
            .clone()
            .or_else(|| std::env::var("AWS_BEARER_TOKEN_BEDROCK").ok())
            .filter(|value| !value.is_empty())
            .map(SecretString::from);
        let credentials = aws_credentials_from_environment();
        let ids = if instance.models.is_empty() {
            DEFAULT_MODELS.iter().map(|id| (*id).to_owned()).collect()
        } else {
            instance.models.clone()
        };
        let models = ids
            .iter()
            .map(|id| {
                let controls = known_controls(id);
                model_info(id, None, &controls, &instance.model_defaults, true)
            })
            .collect();
        Self {
            client: reqwest::Client::new(),
            instance,
            auth_store,
            region,
            bearer,
            credentials,
            models,
            live_models: crate::live_models::LiveModels::default(),
            controls: RwLock::default(),
        }
    }

    fn stored_bearer(&self) -> Option<String> {
        self.auth_store
            .as_ref()?
            .get(&self.instance.id)
            .ok()??
            .api_key()
            .map(str::to_owned)
    }

    fn authenticated(&self) -> bool {
        self.bearer.is_some() || self.credentials.is_some() || self.stored_bearer().is_some()
    }

    /// Sign `headers` for a request to `url`: a Bedrock API key as a bearer
    /// token, else the AWS credentials found in the environment.
    fn authorize(
        &self,
        method: &str,
        url: &str,
        payload: &[u8],
        headers: &mut reqwest::header::HeaderMap,
    ) -> CoreResult<()> {
        if let Some(token) = self
            .bearer
            .as_ref()
            .map(|value| value.expose_secret().to_owned())
            .or_else(|| self.stored_bearer())
        {
            headers.insert(
                reqwest::header::AUTHORIZATION,
                header(&format!("Bearer {token}"))?,
            );
            return Ok(());
        }
        if let Some(credentials) = &self.credentials {
            return sign_request(method, url, payload, &self.region, credentials, headers);
        }
        Err(CoreError::Provider(
            "Amazon Bedrock is not authenticated; set AWS_BEARER_TOKEN_BEDROCK, AWS credentials, or run `haarness auth login amazon-bedrock`".into(),
        ))
    }

    /// The control plane beside the runtime endpoint (`bedrock.<region>`
    /// next to `bedrock-runtime.<region>`). A custom endpoint, such as a
    /// proxy, is asked for both.
    fn control_base(&self) -> String {
        self.instance.base_url.trim_end_matches('/').replacen(
            "://bedrock-runtime.",
            "://bedrock.",
            1,
        )
    }

    async fn control_get(&self, path: &str) -> Option<Value> {
        let url = format!("{}{path}", self.control_base());
        let mut headers = self.instance.headers.map();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        self.authorize("GET", &url, b"", &mut headers).ok()?;
        let response = self
            .client
            .get(&url)
            .headers(headers)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            tracing::warn!(status = %response.status(), path, "Bedrock did not list its models");
            return None;
        }
        response.json().await.ok()
    }

    /// The region's foundation models a chat can use, each under the id a
    /// request names it by: its own where it runs on demand, else a
    /// cross-region inference profile.
    async fn fetch_models(&self) -> Option<Vec<ModelInfo>> {
        if !self.instance.discover_models || !self.authenticated() {
            return None;
        }
        let foundation = self.control_get("/foundation-models").await?;
        let mut profiles = Vec::new();
        let mut next: Option<String> = None;
        // A finite bound, so a cursor that never ends cannot hold discovery.
        for _ in 0..10 {
            let mut path =
                "/inference-profiles?maxResults=1000&typeEquals=SYSTEM_DEFINED".to_owned();
            if let Some(token) = &next {
                path.push_str("&nextToken=");
                path.push_str(&uri_encode(token));
            }
            let page = self.control_get(&path).await?;
            profiles.extend(
                page["inferenceProfileSummaries"]
                    .as_array()?
                    .iter()
                    .cloned(),
            );
            match page["nextToken"].as_str().filter(|token| !token.is_empty()) {
                Some(token) if next.as_deref() != Some(token) => next = Some(token.to_owned()),
                _ => break,
            }
        }
        let (models, controls) = discovered_models(
            &foundation,
            &profiles,
            &self.region,
            &self.instance.model_defaults,
        );
        *self.controls.write().unwrap() = controls;
        Some(crate::merge_discovered_models(self.models.clone(), models))
    }

    fn controls(&self, model: &str) -> ModelControls {
        self.controls
            .read()
            .unwrap()
            .get(model)
            .cloned()
            .unwrap_or_else(|| known_controls(model))
    }
}

#[async_trait]
impl ModelProvider for BedrockProvider {
    fn id(&self) -> &str {
        &self.instance.id
    }

    /// Bedrock takes 20 MB in a request; its images get 15 MiB of it. A
    /// Claude model takes as many images as it does from Anthropic, named by
    /// its id or an ARN holding it (a foundation model or a cross-region
    /// inference profile). An application inference profile's ARN names no
    /// model, and finding it would take a request to Bedrock, so it gets the
    /// byte limit alone.
    fn image_limits(&self, model: &str) -> haarness_core::ImageLimits {
        let count = model
            .contains("anthropic.claude")
            .then(|| crate::anthropic::image_limits(&self.models, model).count)
            .flatten();
        haarness_core::ImageLimits {
            bytes: 15 * 1024 * 1024,
            count,
            request: Some(20_000_000 - haarness_core::REQUEST_ALLOWANCE),
        }
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.instance.id.clone(),
            name: self.instance.name.clone(),
            authenticated: self.authenticated(),
            models: self.live_models.get(&self.models),
        }
    }

    async fn refresh_models(&self, force: bool) {
        self.live_models
            .refresh(force, || self.fetch_models())
            .await;
    }

    /// A Bedrock API key lists the region's models (`ListFoundationModels`,
    /// which the key's managed policy allows). AWS answers a key it does not
    /// know with 403 "Authentication failed".
    async fn check_api_key(&self, key: &str) -> KeyCheck {
        let Ok(value) = header(&format!("Bearer {key}")) else {
            return KeyCheck::Rejected(Some("The key holds characters a key cannot".into()));
        };
        let request = self
            .client
            .get(format!(
                "{}/foundation-models?byProvider=amazon",
                self.control_base()
            ))
            .headers(self.instance.headers.map())
            .header(reqwest::header::AUTHORIZATION, value);
        crate::key_check::check(&self.instance.name, request).await
    }

    /// Requests to a model that takes cache points mark the tools, system
    /// prompt and newest turn.
    fn explicit_prompt_cache_scope(&self, model: &str) -> Option<String> {
        self.controls(model).cache_points.then(|| {
            crate::prompt_cache_scope(
                &self.instance.base_url,
                self.bearer
                    .as_ref()
                    .map(|value| value.expose_secret().to_owned())
                    .or_else(|| self.stored_bearer())
                    .as_deref()
                    .or_else(|| {
                        self.credentials
                            .as_ref()
                            .map(|value| value.access_key.as_str())
                    }),
                &self.instance.headers,
            )
        })
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        let limits = request.parameters.limits;
        let controls = self.controls(&request.model);
        let url = format!(
            "{}/model/{}/converse-stream",
            self.instance.base_url.trim_end_matches('/'),
            encode_path_segment(&request.model)
        );
        let body = bedrock_request(request, &controls)?;
        let payload = serde_json::to_vec(&body).map_err(|error| {
            CoreError::Provider(format!("cannot encode Bedrock request: {error}"))
        })?;
        let mut headers = self.instance.headers.map();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        self.authorize("POST", &url, &payload, &mut headers)?;
        let response = tokio::select! {
            () = cancel.cancelled() => return Err(CoreError::Cancelled),
            response = crate::sse::send(self.client.post(&url).headers(headers).body(payload), limits) => response?,
        };
        if !response.status().is_success() {
            let status = response.status();
            let kind = error_type(response.headers());
            let limit = limits.idle(crate::sse::is_loopback(response.url()));
            let body = crate::sse::within(limit, response.text())
                .await?
                .unwrap_or_default();
            return Err(http_error(status, kind.as_deref(), &body));
        }
        Ok(converse_stream(
            response,
            cancel,
            limits,
            self.instance.id.clone(),
        ))
    }
}

/// The answer's events as they arrive. Any byte resets the idle limit; a
/// silence that outlasts it, or a stream that ends before Bedrock said the
/// message stopped, fails the request.
fn converse_stream(
    response: reqwest::Response,
    cancel: CancellationToken,
    limits: StreamLimits,
    provider_id: String,
) -> ProviderStream {
    let limit = limits.idle(crate::sse::is_loopback(response.url()));
    let request_id = crate::request_id(response.headers());
    Box::pin(try_stream! {
        let mut chunks = response.bytes_stream();
        let mut decoder = aws_event_stream::Decoder::default();
        let mut events = ConverseEvents::default();
        loop {
            let chunk = tokio::select! {
                () = cancel.cancelled() => Err(CoreError::Cancelled),
                chunk = crate::sse::within(limit, chunks.next()) => match chunk {
                    Err(stall) => Err(stall),
                    Ok(Some(Ok(chunk))) => Ok(Some(chunk)),
                    Ok(Some(Err(error))) => Err(crate::sse::transport_error(error)),
                    Ok(None) => Ok(None),
                }
            };
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    drop(chunks);
                    Err(error)?;
                    unreachable!();
                }
            };
            let Some(chunk) = chunk else { break; };
            decoder.push(&chunk);
            while let Some(message) = decoder.next() {
                let message = message.map_err(|error| {
                    CoreError::Provider(format!("Bedrock sent an unreadable answer: {error}"))
                })?;
                for event in events.message(&message)? {
                    yield event;
                }
            }
        }
        if !decoder.is_empty() {
            Err(CoreError::Provider(
                "Bedrock connection closed before message completed".into(),
            ))?;
        }
        let finished = events.finish()?;
        if let ProviderEvent::Finished { reason } = &finished {
            crate::log_blocked_finish(&provider_id, reason, request_id.as_deref(), None);
        }
        yield finished;
    })
}

/// ConverseStream's events turned into Haarness's, holding back what is
/// only whole at its block's end: a tool call's arguments and a reasoning
/// block with its signature, which goes back to the model with the call.
#[derive(Default)]
struct ConverseEvents {
    tool: Option<(String, String, String)>,
    reasoning: Option<Value>,
    reasoning_blocks: Vec<Value>,
    stop: Option<String>,
}

impl ConverseEvents {
    fn message(&mut self, message: &aws_event_stream::Message) -> CoreResult<Vec<ProviderEvent>> {
        let payload: Value = serde_json::from_slice(&message.payload).unwrap_or(Value::Null);
        match message.header(":message-type") {
            Some("exception") => {
                let kind = message.header(":exception-type").unwrap_or("exception");
                Err(stream_exception(kind, &payload))
            }
            Some("error") => Err(CoreError::Provider(format!(
                "Bedrock stream error: [{}] {}",
                message.header(":error-code").unwrap_or("unknown"),
                message.header(":error-message").unwrap_or_default()
            ))),
            _ => self.event(message.header(":event-type").unwrap_or_default(), &payload),
        }
    }

    fn event(&mut self, kind: &str, payload: &Value) -> CoreResult<Vec<ProviderEvent>> {
        let mut events = Vec::new();
        match kind {
            "contentBlockStart" => {
                if let Some(call) = payload.pointer("/start/toolUse") {
                    let text = |key: &str| call[key].as_str().unwrap_or_default().to_owned();
                    self.tool = Some((text("toolUseId"), text("name"), String::new()));
                    // The arguments arrive in pieces, held until the block ends.
                    events.push(ProviderEvent::ContentStarted);
                }
            }
            "contentBlockDelta" => {
                let delta = &payload["delta"];
                if let Some(text) = delta["text"].as_str().filter(|text| !text.is_empty()) {
                    events.push(ProviderEvent::TextDelta(text.to_owned()));
                }
                if let (Some((_, _, input)), Some(piece)) = (
                    &mut self.tool,
                    delta.pointer("/toolUse/input").and_then(Value::as_str),
                ) {
                    input.push_str(piece);
                }
                if let Some(reasoning) = delta.get("reasoningContent") {
                    let block = self
                        .reasoning
                        .get_or_insert_with(|| json!({"reasoningText":{"text":""}}));
                    if let Some(text) = reasoning["text"].as_str().filter(|text| !text.is_empty()) {
                        let mut whole = block["reasoningText"]["text"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned();
                        whole.push_str(text);
                        block["reasoningText"]["text"] = json!(whole);
                        events.push(ProviderEvent::ReasoningDelta(text.to_owned()));
                    }
                    if let Some(signature) = reasoning["signature"].as_str() {
                        block["reasoningText"]["signature"] = json!(signature);
                    }
                    if let Some(redacted) = reasoning.get("redactedContent") {
                        *block = json!({"redactedContent": redacted});
                    }
                }
            }
            "contentBlockStop" => {
                if let Some(block) = self.reasoning.take() {
                    self.reasoning_blocks
                        .push(json!({"reasoningContent": block}));
                }
                if let Some((call_id, name, input)) = self.tool.take() {
                    let arguments = if input.trim().is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&input).map_err(|error| {
                            CoreError::Provider(format!(
                                "Bedrock returned invalid tool arguments: {error}"
                            ))
                        })?
                    };
                    if name == STRUCTURED_OUTPUT_TOOL_NAME {
                        events.push(ProviderEvent::StructuredOutput(arguments));
                    } else {
                        let provider_metadata = if self.reasoning_blocks.is_empty() {
                            Value::Null
                        } else {
                            json!({"bedrock_reasoning": std::mem::take(&mut self.reasoning_blocks)})
                        };
                        events.push(ProviderEvent::ToolCall {
                            call_id,
                            name,
                            arguments,
                            provider_metadata,
                        });
                    }
                }
            }
            "messageStop" => {
                self.stop = Some(
                    payload["stopReason"]
                        .as_str()
                        .unwrap_or("end_turn")
                        .to_owned(),
                );
            }
            "metadata" => {
                if let Some(usage) = payload.get("usage") {
                    // Bedrock's inputTokens excludes cache reads and writes;
                    // the event's input_tokens is the whole prompt.
                    let read = number(usage, "cacheReadInputTokens");
                    let write = number(usage, "cacheWriteInputTokens");
                    events.push(ProviderEvent::Usage {
                        input_tokens: number(usage, "inputTokens")
                            .saturating_add(read)
                            .saturating_add(write),
                        output_tokens: number(usage, "outputTokens"),
                        cache_read_tokens: read,
                        cache_write_tokens: write,
                    });
                }
            }
            _ => {}
        }
        Ok(events)
    }

    /// The finish, once the stream has ended: a stream that ended without
    /// Bedrock's `messageStop` broke off.
    fn finish(&mut self) -> CoreResult<ProviderEvent> {
        self.stop
            .take()
            .map(|reason| ProviderEvent::Finished { reason })
            .ok_or_else(|| {
                CoreError::Provider("Bedrock connection closed before message completed".into())
            })
    }
}

/// An exception Bedrock raised inside the stream, worded as the same
/// exception answered as a response would be, so it is retried or explained
/// alike.
fn stream_exception(kind: &str, payload: &Value) -> CoreError {
    let status = match kind {
        "throttlingException" => 429,
        "serviceUnavailableException" => 503,
        "internalServerException" => 500,
        "modelStreamErrorException" => 424,
        "validationException" => 400,
        "accessDeniedException" => 403,
        _ => 500,
    };
    let mut name = kind.to_owned();
    if let Some(first) = name.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    let status =
        reqwest::StatusCode::from_u16(status).unwrap_or(reqwest::StatusCode::INTERNAL_SERVER_ERROR);
    http_error(status, Some(&name), &payload.to_string())
}

/// Bedrock writes a model id with a geography (`us.`, `global.`), its
/// vendor and a version tag (`-v1:0`); the bare id (`claude-sonnet-5-5`,
/// `gpt-oss-120b`) is what the price table and the names know.
fn bare_model_id(id: &str) -> &str {
    let id = id.rsplit_once('/').map_or(id, |(_, id)| id);
    let id = [
        "global.", "us.", "eu.", "apac.", "jp.", "au.", "ca.", "us-gov.",
    ]
    .iter()
    .find_map(|prefix| id.strip_prefix(prefix))
    .unwrap_or(id);
    let id = id.split_once('.').map_or(id, |(_, id)| id);
    let (id, versioned) = id
        .rsplit_once(':')
        .map_or((id, false), |(id, _)| (id, true));
    let number = |tag: &str| !tag.is_empty() && tag.bytes().all(|byte| byte.is_ascii_digit());
    match id.rsplit_once('-') {
        // `-v1:0`, `-v1`, `-1:0`
        Some((stem, tag))
            if tag.strip_prefix('v').is_some_and(number) || (versioned && number(tag)) =>
        {
            stem
        }
        _ => id,
    }
}

/// What a model takes before its metadata has been read: Claude's newest
/// families take effort levels as on Anthropic's API, and cache points.
fn known_controls(id: &str) -> ModelControls {
    let bare = bare_model_id(id);
    let claude = id.contains("anthropic.claude");
    ModelControls {
        reasoning: (claude && crate::anthropic::adaptive_thinking(bare)).then(|| {
            Reasoning::Level {
                path: vec!["output_config".into(), "effort".into()],
                levels: ["low", "medium", "high", "xhigh", "max"]
                    .map(str::to_owned)
                    .to_vec(),
            }
        }),
        cache_points: claude,
        max_output_tokens: None,
    }
}

/// The picker's entry for `id`: Bedrock's name when it gave one, else the
/// one the bare id is known by; the price and context of the bare id.
fn model_info(
    id: &str,
    name: Option<&str>,
    controls: &ModelControls,
    defaults: &CompatibleModelDefaults,
    images: bool,
) -> ModelInfo {
    let bare = bare_model_id(id);
    let mut info = crate::catalog::annotate(
        "amazon-bedrock",
        ModelInfo {
            id: bare.to_owned(),
            display_name: name.unwrap_or(bare).to_owned(),
            reasoning: false,
            reasoning_efforts: Vec::new(),
            images,
            video: false,
            tools: true,
            context_window: defaults.context_window,
            max_output_tokens: controls.max_output_tokens.or(defaults.max_output_tokens),
            pricing: None,
        },
    );
    if name.is_none() {
        info.display_name = haarness_protocol::model_display_name(id, None);
        if info.display_name == id {
            info.display_name = haarness_protocol::model_display_name(bare, None);
        }
    }
    info.id = id.to_owned();
    info.reasoning_efforts = match &controls.reasoning {
        Some(Reasoning::Level { levels, .. }) => levels.clone(),
        Some(Reasoning::Budget { switch, budgets }) => switch
            .then(|| "none".to_owned())
            .into_iter()
            .chain(budgets.iter().map(|(level, _)| level.clone()))
            .collect(),
        None => Vec::new(),
    };
    info.reasoning = !info.reasoning_efforts.is_empty();
    info
}

/// The geography a region's cross-region profiles are named for.
fn geography(region: &str) -> &str {
    match region.split('-').next().unwrap_or_default() {
        "us" if region.starts_with("us-gov") => "us-gov",
        "ap" if region.starts_with("ap-northeast-1") => "jp",
        "ap" if region.starts_with("ap-southeast-2") || region.starts_with("ap-southeast-4") => {
            "au"
        }
        "ap" => "apac",
        other => other,
    }
}

/// The models of a `ListFoundationModels` answer a chat can use: active,
/// text in and out, answered through Converse, and not a safety classifier.
/// Each is named by the id a request uses: its own where it runs on demand,
/// else the global inference profile, else the one for this region's
/// geography.
fn discovered_models(
    foundation: &Value,
    profiles: &[Value],
    region: &str,
    defaults: &CompatibleModelDefaults,
) -> (Vec<ModelInfo>, HashMap<String, ModelControls>) {
    let geography = format!("{}.", geography(region));
    let rank = |id: &str| {
        if id.starts_with("global.") {
            0
        } else if id.starts_with(&geography) {
            1
        } else {
            2
        }
    };
    let mut profile_for: HashMap<&str, &str> = HashMap::new();
    for profile in profiles {
        if profile["status"] != "ACTIVE" {
            continue;
        }
        let (Some(id), Some(model)) = (
            profile["inferenceProfileId"].as_str(),
            profile
                .pointer("/models/0/modelArn")
                .and_then(Value::as_str)
                .and_then(|arn| arn.rsplit_once("foundation-model/"))
                .map(|(_, model)| model),
        ) else {
            continue;
        };
        let better = profile_for
            .get(model)
            .is_none_or(|current| rank(id) < rank(current));
        if better && rank(id) < 2 {
            profile_for.insert(model, id);
        }
    }
    let has = |entry: &Value, key: &str, value: &str| {
        entry[key]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item == value))
    };
    let mut models = Vec::new();
    let mut controls = HashMap::new();
    for entry in foundation["modelSummaries"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let Some(model) = entry["modelId"].as_str() else {
            continue;
        };
        // A chat model answers Converse both whole and streamed; a model
        // that only streams (speech to speech, video questions) is not one.
        let converse = |mode: &str| {
            entry
                .pointer(&format!("/inferenceAPIsSupported/converse/{mode}"))
                .and_then(Value::as_bool)
        };
        let streams = match (converse("streaming"), converse("sync")) {
            (Some(streaming), Some(sync)) => streaming && sync,
            _ => entry["responseStreamingSupported"] == true,
        };
        if entry.pointer("/modelLifecycle/status") != Some(&json!("ACTIVE"))
            || !has(entry, "inputModalities", "TEXT")
            || !has(entry, "outputModalities", "TEXT")
            || !streams
            || model.contains("guard")
        {
            continue;
        }
        let id = if has(entry, "inferenceTypesSupported", "ON_DEMAND") {
            model
        } else if let Some(profile) = profile_for.get(model) {
            profile
        } else {
            continue;
        };
        let converse = &entry["converse"];
        let model_controls = ModelControls {
            reasoning: converse["additionalRequestFieldsSchema"]
                .as_str()
                .and_then(reasoning_controls),
            cache_points: entry
                .pointer("/explicitPromptCaching/isSupported")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            max_output_tokens: converse["maxTokensMaximum"].as_u64(),
        };
        let images = converse["userImageTypesSupported"].as_array().map_or_else(
            || has(entry, "inputModalities", "IMAGE"),
            |types| !types.is_empty(),
        );
        models.push(model_info(
            id,
            entry["modelName"].as_str(),
            &model_controls,
            defaults,
            images,
        ));
        controls.insert(id.to_owned(), model_controls);
    }
    (models, controls)
}

/// The reasoning setting a model's `additionalRequestFieldsSchema`
/// describes, when it takes one: a level by name (`reasoning_config`,
/// `output_config.effort`) or a thinking budget per level
/// (`reasoningConfig.budgetTokens`). A bare on/off switch offers no levels.
fn reasoning_controls(schema: &str) -> Option<Reasoning> {
    let schema: Value = serde_json::from_str(schema).ok()?;
    let levels = |field: &Value| -> Option<Vec<String>> {
        (field["type"] == "enum")
            .then(|| field["enum"].as_array())
            .flatten()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .filter(|levels: &Vec<String>| !levels.is_empty())
    };
    for (key, field) in schema.as_object()? {
        if let Some(levels) = levels(field) {
            if key.contains("reason") || key.contains("effort") {
                return Some(Reasoning::Level {
                    path: vec![key.clone()],
                    levels,
                });
            }
            continue;
        }
        let Some(fields) = field.as_object() else {
            continue;
        };
        for (inner, value) in fields {
            if let Some(levels) = levels(value).filter(|_| inner.contains("effort")) {
                return Some(Reasoning::Level {
                    path: vec![key.clone(), inner.clone()],
                    levels,
                });
            }
            if inner == "budgetTokens"
                && let Some(budgets) = value["enum"].as_object()
            {
                let mut budgets: Vec<(String, u64)> = budgets
                    .iter()
                    .filter_map(|(level, budget)| Some((level.clone(), budget.as_u64()?)))
                    .collect();
                budgets.sort_by_key(|(_, budget)| *budget);
                if !budgets.is_empty() {
                    return Some(Reasoning::Budget {
                        switch: fields.contains_key("enabled"),
                        budgets,
                    });
                }
            }
        }
    }
    None
}

/// The text sent with a document attached without any, which Converse needs.
const ATTACHED_DOCUMENT: &str = "(document attached)";
/// What a tool result that printed nothing reads as.
const NO_OUTPUT: &str = "(no output)";
/// A Converse cache checkpoint: what comes before it is cached.
const CACHE_POINT: &str = r#"{"cachePoint":{"type":"default"}}"#;

fn bedrock_request(request: ProviderRequest, controls: &ModelControls) -> CoreResult<Value> {
    let mut messages = Vec::new();
    for message in request.messages {
        match message {
            // Converse refuses a blank text block, so blank text is left out
            // wherever it would stand alone.
            ProviderMessage::Text { content, .. } if content.trim().is_empty() => {}
            ProviderMessage::Text { role, content } => {
                messages.push(json!({"role":role,"content":[{"text":content}]}));
            }
            ProviderMessage::Media {
                role,
                content,
                attachments,
            } => {
                let media = crate::media::load_all(&attachments)?;
                // A document must come with text; an image needs none.
                let content = if !content.trim().is_empty() {
                    Some(content)
                } else if media.iter().any(crate::media::LoadedMedia::is_pdf) {
                    Some(ATTACHED_DOCUMENT.to_owned())
                } else {
                    None
                };
                let mut blocks: Vec<Value> = content
                    .into_iter()
                    .map(|text| json!({"text":text}))
                    .collect();
                for item in media {
                    if item.is_image() {
                        let format = item.mime_type.strip_prefix("image/").unwrap_or("png");
                        blocks.push(
                            json!({"image":{"format":format,"source":{"bytes":item.base64}}}),
                        );
                    } else if item.is_pdf() {
                        blocks.push(json!({"document":{
                            "format":"pdf",
                            "name":bedrock_document_name(&item.filename),
                            "source":{"bytes":item.base64}
                        }}));
                    } else {
                        return Err(CoreError::Provider(
                            "Bedrock Converse accepts Haarness image and PDF attachments, not audio or video"
                                .into(),
                        ));
                    }
                }
                messages.push(json!({"role":role,"content":blocks}));
            }
            ProviderMessage::ToolCall {
                call_id,
                name,
                arguments,
                provider_metadata,
            } => {
                // The reasoning that led to the call goes back with it, signed.
                let mut blocks = provider_metadata
                    .get("bedrock_reasoning")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                blocks.push(json!({"toolUse":{"toolUseId":call_id,"name":name,"input":arguments}}));
                messages.push(json!({"role":"assistant","content":blocks}));
            }
            ProviderMessage::ToolResult {
                call_id,
                output,
                is_error,
                attachments,
                ..
            } => {
                let media = crate::media::load_all(&attachments)?;
                let mut content = if !output.trim().is_empty() {
                    vec![json!({"text":output})]
                } else if media.is_empty() {
                    // A tool result needs a block of some kind.
                    vec![json!({"text":NO_OUTPUT})]
                } else {
                    Vec::new()
                };
                for item in media {
                    if item.is_image() {
                        let format = item.mime_type.strip_prefix("image/").unwrap_or("png");
                        content.push(
                            json!({"image":{"format":format,"source":{"bytes":item.base64}}}),
                        );
                    } else if item.is_pdf() {
                        content.push(json!({"document":{
                            "format":"pdf",
                            "name":bedrock_document_name(&item.filename),
                            "source":{"bytes":item.base64}
                        }}));
                    } else {
                        return Err(CoreError::Provider(
                            "Bedrock Converse tool results support Haarness images and PDFs, not audio or video"
                                .into(),
                        ));
                    }
                }
                messages.push(json!({"role":"user","content":[{"toolResult":{"toolUseId":call_id,"content":content,"status":if is_error {"error"} else {"success"}}}]}));
            }
        }
    }
    // Converse wants user and assistant turns to alternate: parallel calls
    // share one assistant turn, their results one user turn, and a prompt
    // followed by its stored reminder or a steered message is one user turn.
    let mut grouped: Vec<Value> = Vec::new();
    for message in messages {
        if let Some(previous) = grouped
            .last_mut()
            .filter(|previous| previous["role"] == message["role"])
        {
            previous["content"]
                .as_array_mut()
                .unwrap()
                .extend(message["content"].as_array().unwrap().iter().cloned());
        } else {
            grouped.push(message);
        }
    }
    // A turn's reasoning opens it, ahead of its text and calls.
    for message in &mut grouped {
        if let Some(content) = message["content"].as_array_mut() {
            content.sort_by_key(|block| block.get("reasoningContent").is_none());
        }
    }
    let mut messages = grouped;
    let mut max_tokens = request
        .parameters
        .max_output_tokens
        .filter(|limit| *limit > 0)
        .unwrap_or(8192);
    let mut extra = json!({});
    match (&controls.reasoning, request.reasoning_effort.as_deref()) {
        (Some(Reasoning::Level { path, levels }), Some(effort)) => {
            let level = levels
                .iter()
                .find(|level| *level == effort)
                .or_else(|| (effort == "minimal").then(|| levels.first()).flatten());
            if let Some(level) = level {
                let mut field = &mut extra;
                for key in path {
                    field = &mut field[key];
                }
                *field = json!(level);
            }
        }
        (Some(Reasoning::Budget { switch, .. }), Some("none" | "minimal")) => {
            if *switch {
                extra["reasoningConfig"]["enabled"] = json!(false);
            }
        }
        (Some(Reasoning::Budget { switch, budgets }), Some(effort)) => {
            let budget = budgets
                .iter()
                .find(|(level, _)| level == effort)
                .or_else(|| budgets.last())
                .map(|(_, budget)| *budget);
            if let Some(budget) = budget {
                if *switch {
                    extra["reasoningConfig"]["enabled"] = json!(true);
                }
                extra["reasoningConfig"]["budgetTokens"] = json!(budget);
                // The output limit holds the thinking too: leave room for the
                // answer, within the model's own ceiling.
                let ceiling = controls.max_output_tokens.unwrap_or(u64::MAX);
                max_tokens = max_tokens.max((budget + 8192).min(ceiling).max(budget + 1024));
            }
        }
        _ => {}
    }
    let cache = controls.cache_points && !request.parameters.uncached;
    if cache
        && let Some(content) = messages
            .last_mut()
            .and_then(|message| message["content"].as_array_mut())
    {
        content.push(serde_json::from_str(CACHE_POINT).unwrap_or_default());
    }
    let structured = request.output_schema;
    let forced = !crate::tool_schema::refuses_forced_tool_choice(&request.model);
    let mut requested_tools = request.tools;
    // Converse hands Claude the same tool schema as the Messages API.
    crate::tool_schema::retain_claude_compatible(&request.model, &mut requested_tools);
    if let Some(schema) = structured.as_ref() {
        requested_tools.retain(|tool| tool.name != STRUCTURED_OUTPUT_TOOL_NAME);
        requested_tools.push(structured_output_tool(schema));
    }
    let claude = request.model.contains("anthropic.claude");
    // Claude refuses a forced tool call while it thinks.
    let forced = forced && !(claude && extra.as_object().is_some_and(|fields| !fields.is_empty()));
    let mut tools: Vec<_> = requested_tools
        .into_iter()
        .map(|tool| json!({"toolSpec":{"name":tool.name,"description":tool.description,"inputSchema":{"json":tool.input_schema}}}))
        .collect();
    let mut system = vec![json!({"text":request.system})];
    if cache {
        system.push(serde_json::from_str(CACHE_POINT).unwrap_or_default());
        // Claude caches the tools too; other models refuse a cache point there.
        if claude && !tools.is_empty() {
            tools.push(serde_json::from_str(CACHE_POINT).unwrap_or_default());
        }
    }
    let mut body = json!({
        "system":system,
        "messages":messages,
        // The Anthropic adapter's default; a configured limit wins.
        "inferenceConfig":{"maxTokens":max_tokens}
    });
    if extra.as_object().is_some_and(|fields| !fields.is_empty()) {
        body["additionalModelRequestFields"] = extra;
    }
    if !tools.is_empty() {
        body["toolConfig"] = json!({"tools":tools});
        if structured.is_some() && forced {
            body["toolConfig"]["toolChoice"] = json!({"tool":{"name":STRUCTURED_OUTPUT_TOOL_NAME}});
        }
    }
    Ok(body)
}

fn bedrock_document_name(filename: &str) -> String {
    let stem = filename.rsplit_once('.').map_or(filename, |(stem, _)| stem);
    let mut name = stem
        .chars()
        .map(|character| {
            if character.is_alphanumeric()
                || matches!(character, ' ' | '-' | '_' | '(' | ')' | '[' | ']')
            {
                character
            } else {
                '-'
            }
        })
        .take(100)
        .collect::<String>();
    if name.trim().is_empty() {
        name = "document".into();
    }
    name
}

fn number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn aws_credentials_from_environment() -> Option<AwsCredentials> {
    if let (Ok(access_key), Ok(secret_key)) = (
        std::env::var("AWS_ACCESS_KEY_ID"),
        std::env::var("AWS_SECRET_ACCESS_KEY"),
    ) {
        return Some(AwsCredentials {
            access_key,
            secret_key: SecretString::from(secret_key),
            session_token: std::env::var("AWS_SESSION_TOKEN")
                .ok()
                .map(SecretString::from),
        });
    }
    let path = std::env::var_os("AWS_SHARED_CREDENTIALS_FILE")
        .map(PathBuf::from)
        .or_else(|| {
            haarness_protocol::home_dir().map(|home| home.join(".aws").join("credentials"))
        })?;
    let profile = std::env::var("AWS_PROFILE").unwrap_or_else(|_| "default".into());
    parse_aws_profile(&std::fs::read_to_string(path).ok()?, &profile)
}

fn parse_aws_profile(contents: &str, profile: &str) -> Option<AwsCredentials> {
    let mut in_profile = false;
    let mut values = BTreeMap::new();
    for raw in contents.lines() {
        let line = raw.trim();
        if let Some(section) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            in_profile = section == profile || section == format!("profile {profile}");
            continue;
        }
        if in_profile && let Some((key, value)) = line.split_once('=') {
            values.insert(key.trim(), value.trim().to_owned());
        }
    }
    Some(AwsCredentials {
        access_key: values.remove("aws_access_key_id")?,
        secret_key: SecretString::from(values.remove("aws_secret_access_key")?),
        session_token: values.remove("aws_session_token").map(SecretString::from),
    })
}

fn sign_request(
    method: &str,
    endpoint: &str,
    payload: &[u8],
    region: &str,
    credentials: &AwsCredentials,
    headers: &mut reqwest::header::HeaderMap,
) -> CoreResult<()> {
    sign_request_at(
        method,
        endpoint,
        payload,
        region,
        credentials,
        headers,
        Utc::now(),
    )
}

/// SigV4 at `now`. The content type is signed, so every request sends it.
fn sign_request_at(
    method: &str,
    endpoint: &str,
    payload: &[u8],
    region: &str,
    credentials: &AwsCredentials,
    headers: &mut reqwest::header::HeaderMap,
    now: chrono::DateTime<Utc>,
) -> CoreResult<()> {
    let url = url::Url::parse(endpoint).map_err(provider_error)?;
    let mut host = url
        .host_str()
        .ok_or_else(|| CoreError::Provider("Bedrock endpoint has no host".into()))?
        .to_owned();
    if let Some(port) = url.port() {
        let _ = write!(host, ":{port}");
    }
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();
    let payload_hash = hex_sha256(payload);
    headers.insert("host", header(&host)?);
    headers.insert("x-amz-date", header(&amz_date)?);
    if let Some(token) = &credentials.session_token {
        headers.insert("x-amz-security-token", header(token.expose_secret())?);
    }
    let mut signed = vec![
        ("content-type", "application/json".to_owned()),
        ("host", host),
        ("x-amz-date", amz_date.clone()),
    ];
    if let Some(token) = &credentials.session_token {
        signed.push(("x-amz-security-token", token.expose_secret().to_owned()));
    }
    signed.sort_by_key(|(name, _)| *name);
    let canonical_headers = signed
        .iter()
        .fold(String::new(), |mut output, (name, value)| {
            let _ = writeln!(output, "{name}:{}", value.trim());
            output
        });
    let signed_headers = signed
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(";");
    // AWS encodes each path segment once more for the canonical request (the
    // `:` in `amazon.nova-pro-v1:0` is signed as `%3A`, or `%253A` when the
    // path carries `%3A`), and sorts the query by encoded name.
    let canonical_path = url
        .path()
        .split('/')
        .map(uri_encode)
        .collect::<Vec<_>>()
        .join("/");
    let mut query: Vec<(String, String)> = url
        .query_pairs()
        .map(|(name, value)| (uri_encode(&name), uri_encode(&value)))
        .collect();
    query.sort();
    let canonical_query = query
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    let canonical_request = format!(
        "{method}\n{canonical_path}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );
    let scope = format!("{date}/{region}/bedrock/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex_sha256(canonical_request.as_bytes())
    );
    let date_key = hmac(
        format!("AWS4{}", credentials.secret_key.expose_secret()).as_bytes(),
        date.as_bytes(),
    )?;
    let region_key = hmac(&date_key, region.as_bytes())?;
    let service_key = hmac(&region_key, b"bedrock")?;
    let signing_key = hmac(&service_key, b"aws4_request")?;
    let signature = hex_bytes(&hmac(&signing_key, string_to_sign.as_bytes())?);
    headers.insert(
        reqwest::header::AUTHORIZATION,
        header(&format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            credentials.access_key
        ))?,
    );
    Ok(())
}

fn hmac(key: &[u8], value: &[u8]) -> CoreResult<Vec<u8>> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|error| CoreError::Provider(format!("cannot initialize SigV4: {error}")))?;
    mac.update(value);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn hex_sha256(value: &[u8]) -> String {
    hex_bytes(&Sha256::digest(value))
}

fn hex_bytes(value: &[u8]) -> String {
    value.iter().fold(String::new(), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}

fn header(value: &str) -> CoreResult<reqwest::header::HeaderValue> {
    reqwest::header::HeaderValue::from_str(value).map_err(provider_error)
}

/// RFC 3986 encoding of everything but the unreserved characters, as SigV4
/// wants it.
fn uri_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn encode_path_segment(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn provider_error(error: impl std::fmt::Display) -> CoreError {
    CoreError::Provider(error.to_string())
}

/// AWS names the kind of failure in `x-amzn-ErrorType`
/// (`ThrottlingException:http://…`), and sometimes as `__type` in the body.
fn error_type(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let value = headers.get("x-amzn-errortype")?.to_str().ok()?;
    let kind = value.split(':').next()?.trim();
    crate::identifier(kind)
}

/// A refusal in the shape the runtime reads for every provider, `HTTP 429 Too
/// Many Requests: [ThrottlingException] Too many requests…`, so it is retried
/// or explained like any other provider's instead of showing AWS's JSON.
fn http_error(status: reqwest::StatusCode, kind: Option<&str>, body: &str) -> CoreError {
    let parsed = serde_json::from_str::<Value>(body).ok();
    let kind = kind.map(str::to_owned).or_else(|| {
        let kind = parsed.as_ref()?.get("__type")?.as_str()?;
        crate::identifier(kind.rsplit('#').next()?)
    });
    let detail = parsed
        .as_ref()
        .and_then(|value| {
            ["/message", "/Message"]
                .iter()
                .find_map(|key| value.pointer(key)?.as_str())
        })
        .map(|message| message.trim().to_owned())
        .filter(|message| !message.is_empty())
        .or_else(|| crate::sse::provider_error_detail(body));
    CoreError::Provider(match (kind, detail) {
        (Some(kind), Some(detail)) => format!("HTTP {status}: [{kind}] {detail}"),
        (None, Some(detail)) => format!("HTTP {status}: {detail}"),
        (Some(kind), None) => format!("HTTP {status}: [{kind}]"),
        (None, None) => format!("HTTP {status}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
    use parking_lot::Mutex;
    use tokio::net::TcpListener;

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<(HeaderMap, Value)>>>);

    /// The request body with no model metadata read yet.
    fn bedrock_request(request: ProviderRequest) -> CoreResult<Value> {
        super::bedrock_request(request, &ModelControls::default())
    }

    /// ConverseStream events as Bedrock frames them.
    fn converse_frames(events: &[(&str, Value)]) -> Vec<u8> {
        events
            .iter()
            .flat_map(|(kind, payload)| {
                aws_event_stream::encode(
                    &[
                        (":message-type", "event"),
                        (":event-type", kind),
                        (":content-type", "application/json"),
                    ],
                    payload.to_string().as_bytes(),
                )
            })
            .collect()
    }

    async fn converse_fixture(
        State(capture): State<Capture>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Vec<u8> {
        capture.0.lock().push((headers, body));
        converse_frames(&[
            ("messageStart", json!({"role":"assistant"})),
            (
                "contentBlockDelta",
                json!({"contentBlockIndex":0,"delta":{"text":"hel"}}),
            ),
            (
                "contentBlockDelta",
                json!({"contentBlockIndex":0,"delta":{"text":"lo"}}),
            ),
            ("contentBlockStop", json!({"contentBlockIndex":0})),
            ("messageStop", json!({"stopReason":"end_turn"})),
            (
                "metadata",
                json!({"usage":{"inputTokens":2,"outputTokens":1},"metrics":{"latencyMs":5}}),
            ),
        ])
    }

    /// Feed `events` to the mapper as one answer, ending with its finish.
    fn mapped(events: &[(&str, Value)]) -> CoreResult<Vec<ProviderEvent>> {
        let mut mapper = ConverseEvents::default();
        let mut decoder = aws_event_stream::Decoder::default();
        decoder.push(&converse_frames(events));
        let mut out = Vec::new();
        while let Some(message) = decoder.next() {
            out.extend(mapper.message(&message.unwrap())?);
        }
        out.push(mapper.finish()?);
        Ok(out)
    }

    fn discovered() -> (Vec<ModelInfo>, HashMap<String, ModelControls>) {
        let foundation: Value =
            serde_json::from_str(include_str!("testdata/bedrock-foundation-models.json")).unwrap();
        let profiles: Value =
            serde_json::from_str(include_str!("testdata/bedrock-inference-profiles.json")).unwrap();
        discovered_models(
            &foundation,
            profiles["inferenceProfileSummaries"].as_array().unwrap(),
            "us-east-1",
            &CompatibleModelDefaults::default(),
        )
    }

    #[test]
    fn the_picker_lists_the_regions_chat_models_under_the_ids_requests_use() {
        let (models, _) = discovered();
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        // On demand where the model runs so, else the global profile; no
        // embeddings, speech, video-only, safety classifiers, provisioned-only
        // or legacy models.
        assert_eq!(
            ids,
            [
                "global.anthropic.claude-sonnet-5-5",
                "global.anthropic.claude-opus-4-6-v1",
                "global.openai.gpt-6.1-sol",
                "openai.gpt-oss-120b-1:0",
                "amazon.nova-micro-v1:0",
                "deepseek.v3.2",
                "global.amazon.nova-2-lite-v1:0",
            ]
        );
        let model = |id: &str| models.iter().find(|model| model.id == id).unwrap();
        let sonnet = model("global.anthropic.claude-sonnet-5-5");
        assert_eq!(sonnet.display_name, "Claude Sonnet 5.5");
        assert_eq!(
            sonnet.reasoning_efforts,
            ["low", "medium", "high", "xhigh", "max"]
        );
        assert!(sonnet.images && sonnet.reasoning);
        assert_eq!(sonnet.max_output_tokens, Some(128_000));
        // Priced as Anthropic lists the model.
        assert_eq!(
            sonnet.pricing.as_ref().map(|price| price.input),
            crate::catalog::pricing("anthropic", "claude-sonnet-5-5").map(|price| price.input)
        );
        assert!(sonnet.pricing.is_some());
        assert_eq!(
            model("global.openai.gpt-6.1-sol").reasoning_efforts,
            ["low", "medium", "high", "xhigh", "max"]
        );
        let opus = model("global.anthropic.claude-opus-4-6-v1");
        assert_eq!(opus.reasoning_efforts, ["none", "low", "medium", "high"]);
        assert!(!model("openai.gpt-oss-120b-1:0").images);
        assert!(model("amazon.nova-micro-v1:0").reasoning_efforts.is_empty());
    }

    #[test]
    fn a_models_reasoning_level_and_cache_points_go_where_its_metadata_says() {
        let (_, controls) = discovered();
        let request = |model: &str, effort: Option<&str>, tools: bool| {
            super::bedrock_request(
                ProviderRequest {
                    model: model.into(),
                    system: "system".into(),
                    messages: vec![ProviderMessage::Text {
                        role: "user".into(),
                        content: "hi".into(),
                    }],
                    tools: if tools {
                        vec![haarness_core::ToolDefinition {
                            name: "read".into(),
                            description: "Read a file".into(),
                            input_schema: json!({"type":"object","properties":{}}),
                        }]
                    } else {
                        Vec::new()
                    },
                    reasoning_effort: effort.map(str::to_owned),
                    parameters: haarness_core::RequestParameters::default(),
                    output_schema: None,
                },
                &controls[model],
            )
            .unwrap()
        };
        let sonnet = request("global.anthropic.claude-sonnet-5-5", Some("xhigh"), true);
        assert_eq!(
            sonnet["additionalModelRequestFields"],
            json!({"output_config":{"effort":"xhigh"}})
        );
        // Claude caches its tools, system prompt and newest turn.
        assert_eq!(
            sonnet["system"][1],
            json!({"cachePoint":{"type":"default"}})
        );
        assert_eq!(
            sonnet["toolConfig"]["tools"][1],
            json!({"cachePoint":{"type":"default"}})
        );
        assert_eq!(
            sonnet["messages"][0]["content"][1],
            json!({"cachePoint":{"type":"default"}})
        );
        assert_eq!(
            request("global.openai.gpt-6.1-sol", Some("high"), false)["additionalModelRequestFields"],
            json!({"reasoning_config":"high"})
        );
        let opus = request("global.anthropic.claude-opus-4-6-v1", Some("medium"), false);
        assert_eq!(
            opus["additionalModelRequestFields"],
            json!({"reasoningConfig":{"enabled":true,"budgetTokens":40000}})
        );
        // The output limit leaves the answer room beside the thinking.
        assert_eq!(opus["inferenceConfig"]["maxTokens"], 48_192);
        assert_eq!(
            request("global.anthropic.claude-opus-4-6-v1", Some("none"), false)["additionalModelRequestFields"],
            json!({"reasoningConfig":{"enabled":false}})
        );
        // No level, no extra fields; a model without cache points gets none.
        let plain = request("openai.gpt-oss-120b-1:0", Some("high"), true);
        assert!(plain.get("additionalModelRequestFields").is_none());
        assert_eq!(plain["system"].as_array().unwrap().len(), 1);
        assert_eq!(plain["toolConfig"]["tools"].as_array().unwrap().len(), 1);
        // Nova caches the system prompt and turn, not the tools.
        let nova = request("amazon.nova-micro-v1:0", None, true);
        assert_eq!(nova["system"][1], json!({"cachePoint":{"type":"default"}}));
        assert_eq!(nova["toolConfig"]["tools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn bedrock_ids_come_down_to_the_model_they_name() {
        for (id, bare) in [
            ("global.anthropic.claude-sonnet-5-5", "claude-sonnet-5-5"),
            (
                "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
                "claude-sonnet-4-5-20250929",
            ),
            ("anthropic.claude-opus-4-6-v1", "claude-opus-4-6"),
            ("openai.gpt-oss-120b-1:0", "gpt-oss-120b"),
            ("global.openai.gpt-6.1-sol", "gpt-6.1-sol"),
            ("amazon.nova-pro-v1:0", "nova-pro"),
            ("apac.amazon.nova-micro-v1:0", "nova-micro"),
        ] {
            assert_eq!(bare_model_id(id), bare, "{id}");
        }
        // Before discovery the defaults still read and price as their model.
        let provider = BedrockProvider::new_with_region(
            NativeInstance {
                id: "amazon-bedrock".into(),
                name: "Amazon Bedrock".into(),
                base_url: "https://bedrock-runtime.us-east-1.amazonaws.com".into(),
                api_key: None,
                models: Vec::new(),
                discover_models: false,
                model_defaults: CompatibleModelDefaults::default(),
                assume_authenticated: false,
                headers: ProviderHeaders::default(),
            },
            None,
            "us-east-1".into(),
        );
        let haiku = provider
            .models
            .iter()
            .find(|model| model.id == "global.anthropic.claude-haiku-5-5")
            .unwrap();
        assert_eq!(haiku.display_name, "Claude Haiku 5.5");
        assert!(haiku.pricing.is_some());
        assert!(haiku.reasoning_efforts.contains(&"max".to_owned()));
        assert_eq!(
            provider.control_base(),
            "https://bedrock.us-east-1.amazonaws.com"
        );
    }

    #[test]
    fn reasoning_schemas_of_every_shape_bedrock_lists_are_read() {
        assert_eq!(
            reasoning_controls(
                r#"{"reasoning_config":{"type":"enum","default":"medium","enum":["none","low","high"]}}"#
            ),
            Some(Reasoning::Level {
                path: vec!["reasoning_config".into()],
                levels: vec!["none".into(), "low".into(), "high".into()]
            })
        );
        assert_eq!(
            reasoning_controls(
                r#"{"output_config":{"effort":{"type":"enum","enum":["low","max"]}},"hideSamplingParameter":true}"#
            ),
            Some(Reasoning::Level {
                path: vec!["output_config".into(), "effort".into()],
                levels: vec!["low".into(), "max".into()]
            })
        );
        // Nova 2 Lite: budgets without a switch.
        assert_eq!(
            reasoning_controls(
                r#"{"topK":{"type":"number"},"reasoningConfig":{"budgetTokens":{"type":"enum","enum":{"low":10000,"medium":40000,"high":64000}}}}"#
            ),
            Some(Reasoning::Budget {
                switch: false,
                budgets: vec![
                    ("low".into(), 10_000),
                    ("medium".into(), 40_000),
                    ("high".into(), 64_000)
                ]
            })
        );
        // A bare switch offers no levels; neither does no schema.
        assert_eq!(
            reasoning_controls(
                r#"{"reasoningConfig":{"enabled":{"type":"boolean","default":false}}}"#
            ),
            None
        );
        assert_eq!(reasoning_controls("{}"), None);
        assert_eq!(reasoning_controls(""), None);
    }

    #[test]
    fn a_claude_model_takes_as_many_images_as_from_anthropic() {
        let provider = BedrockProvider::new_with_region(
            NativeInstance {
                id: "amazon-bedrock".into(),
                name: "Amazon Bedrock".into(),
                base_url: "http://127.0.0.1:9".into(),
                api_key: Some("bedrock-token".into()),
                models: vec![
                    "us.anthropic.claude-haiku-4-5-20251001-v1:0".into(),
                    "amazon.nova-pro-v1:0".into(),
                ],
                discover_models: false,
                model_defaults: CompatibleModelDefaults::default(),
                assume_authenticated: false,
                headers: ProviderHeaders::default(),
            },
            None,
            "us-east-1".into(),
        );
        let limits = provider.image_limits("us.anthropic.claude-haiku-4-5-20251001-v1:0");
        assert_eq!(
            limits,
            haarness_core::ImageLimits {
                bytes: 15 * 1024 * 1024,
                count: Some(100),
                request: Some(20_000_000 - haarness_core::REQUEST_ALLOWANCE),
            }
        );
        assert_eq!(provider.image_limits("amazon.nova-pro-v1:0").count, None);
        for arn in [
            "arn:aws:bedrock:us-east-1:123456789012:inference-profile/us.anthropic.claude-sonnet-4-5-20250929-v1:0",
            "arn:aws:bedrock:us-east-1::foundation-model/anthropic.claude-haiku-4-5-20251001-v1:0",
        ] {
            assert_eq!(provider.image_limits(arn).count, Some(100), "{arn}");
        }
        assert_eq!(
            provider
                .image_limits(
                    "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/a1b2c3"
                )
                .count,
            None
        );
    }

    #[test]
    fn profile_credentials_are_parsed_without_exposing_secrets() {
        let credentials = parse_aws_profile(
            "[default]\naws_access_key_id = key\naws_secret_access_key = secret\naws_session_token = session\n",
            "default",
        )
        .unwrap();
        assert_eq!(credentials.access_key, "key");
        assert_eq!(credentials.secret_key.expose_secret(), "secret");
    }

    #[test]
    fn converse_stream_maps_tools_structured_output_and_usage() {
        let events = mapped(&[
            ("contentBlockDelta", json!({"delta":{"reasoningContent":{"text":"thinking"}}})),
            ("contentBlockDelta", json!({"delta":{"reasoningContent":{"signature":"sig"}}})),
            ("contentBlockStop", json!({"contentBlockIndex":0})),
            ("contentBlockStart", json!({"start":{"toolUse":{"toolUseId":"call-1","name":"read"}}})),
            ("contentBlockDelta", json!({"delta":{"toolUse":{"input":"{\"path\":"}}})),
            ("contentBlockDelta", json!({"delta":{"toolUse":{"input":"\"a.txt\"}"}}})),
            ("contentBlockStop", json!({"contentBlockIndex":1})),
            ("contentBlockStart", json!({"start":{"toolUse":{"toolUseId":"call-2","name":"StructuredOutput"}}})),
            ("contentBlockDelta", json!({"delta":{"toolUse":{"input":"{\"ok\":true}"}}})),
            ("contentBlockStop", json!({"contentBlockIndex":2})),
            ("messageStop", json!({"stopReason":"tool_use"})),
            ("metadata", json!({"usage":{"inputTokens":7,"outputTokens":3,"cacheReadInputTokens":2,"cacheWriteInputTokens":1}})),
        ])
        .unwrap();
        assert!(matches!(&events[0], ProviderEvent::ReasoningDelta(value) if value == "thinking"));
        assert!(matches!(&events[1], ProviderEvent::ContentStarted));
        let ProviderEvent::ToolCall {
            call_id,
            name,
            arguments,
            provider_metadata,
        } = &events[2]
        else {
            panic!("a tool call: {events:?}");
        };
        assert_eq!((call_id.as_str(), name.as_str()), ("call-1", "read"));
        assert_eq!(arguments, &json!({"path":"a.txt"}));
        // The signed reasoning goes back with the call.
        assert_eq!(
            provider_metadata,
            &json!({"bedrock_reasoning":[{"reasoningContent":{"reasoningText":{"text":"thinking","signature":"sig"}}}]})
        );
        assert!(
            matches!(&events[4], ProviderEvent::StructuredOutput(value) if value == &json!({"ok":true}))
        );
        assert!(matches!(
            &events[5],
            // inputTokens 7 excludes the 2 cache reads and 1 write: the whole prompt is 10.
            ProviderEvent::Usage {
                input_tokens: 10,
                output_tokens: 3,
                cache_read_tokens: 2,
                cache_write_tokens: 1,
            }
        ));
        assert!(matches!(&events[6], ProviderEvent::Finished { reason } if reason == "tool_use"));

        // The reasoning is sent back ahead of the call it led to.
        let body = bedrock_request(ProviderRequest {
            model: "us.anthropic.claude-sonnet-5-5".into(),
            system: "system".into(),
            messages: vec![
                ProviderMessage::Text {
                    role: "user".into(),
                    content: "read it".into(),
                },
                ProviderMessage::Text {
                    role: "assistant".into(),
                    content: "Reading.".into(),
                },
                ProviderMessage::ToolCall {
                    call_id: "call-1".into(),
                    name: "read".into(),
                    arguments: json!({"path":"a.txt"}),
                    provider_metadata: provider_metadata.clone(),
                },
            ],
            tools: Vec::new(),
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        })
        .unwrap();
        let turn = &body["messages"][1]["content"];
        assert_eq!(
            turn[0]["reasoningContent"]["reasoningText"]["signature"],
            "sig"
        );
        assert_eq!(turn[1]["text"], "Reading.");
        assert_eq!(turn[2]["toolUse"]["toolUseId"], "call-1");
    }

    #[test]
    fn converse_stream_safety_stops_fail_the_turn_and_keep_partial_text() {
        for stop in ["content_filtered", "guardrail_intervened"] {
            let events = mapped(&[
                ("contentBlockDelta", json!({"delta":{"text":"Partial"}})),
                ("messageStop", json!({"stopReason":stop})),
            ])
            .unwrap();
            assert!(matches!(&events[0], ProviderEvent::TextDelta(text) if text == "Partial"));
            let Some(ProviderEvent::Finished { reason }) = events.last() else {
                panic!("Converse responses finish: {events:?}");
            };
            assert!(
                matches!(
                    ProviderEvent::finish_error(reason),
                    Some(haarness_core::CoreError::ProviderBlocked(_))
                ),
                "{stop} must not read as a completed reply"
            );
        }
        for stop in ["end_turn", "tool_use", "max_tokens", "stop_sequence"] {
            assert!(ProviderEvent::finish_error(stop).is_none(), "{stop}");
        }
    }

    #[test]
    fn a_stream_exception_reads_like_the_same_refusal_answered_whole() {
        let mut mapper = ConverseEvents::default();
        let message = aws_event_stream::Message {
            headers: [
                (":message-type".to_owned(), "exception".to_owned()),
                (
                    ":exception-type".to_owned(),
                    "throttlingException".to_owned(),
                ),
            ]
            .into(),
            payload: br#"{"message":"Too many requests, please wait before trying again."}"#
                .to_vec(),
        };
        assert_eq!(
            mapper.message(&message).unwrap_err().to_string(),
            "provider error: HTTP 429 Too Many Requests: [ThrottlingException] Too many requests, please wait before trying again."
        );
        // A stream that ends without Bedrock's stop broke off.
        let mut cut = ConverseEvents::default();
        cut.event("contentBlockDelta", &json!({"delta":{"text":"Hal"}}))
            .unwrap();
        assert!(
            cut.finish()
                .unwrap_err()
                .to_string()
                .contains("connection closed before message completed")
        );
    }

    #[test]
    fn a_refusal_names_the_aws_exception_and_its_words() {
        assert_eq!(
            http_error(
                reqwest::StatusCode::FORBIDDEN,
                Some("AccessDeniedException"),
                r#"{"Message":"Authentication failed: Please make sure your API Key is valid."}"#
            )
            .to_string(),
            "provider error: HTTP 403 Forbidden: [AccessDeniedException] Authentication failed: Please make sure your API Key is valid."
        );
        assert_eq!(
            http_error(
                reqwest::StatusCode::BAD_REQUEST,
                None,
                r#"{"__type":"com.amazon.bedrock#ValidationException","message":"The provided model identifier is invalid."}"#
            )
            .to_string(),
            "provider error: HTTP 400 Bad Request: [ValidationException] The provided model identifier is invalid."
        );
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "x-amzn-errortype",
            "ThrottlingException:http://internal.amazon.com/coral/com.amazon.bedrock/"
                .parse()
                .unwrap(),
        );
        assert_eq!(error_type(&headers).as_deref(), Some("ThrottlingException"));
    }

    #[test]
    fn converse_turns_alternate_when_messages_of_one_role_follow_each_other() {
        let text = |role: &str, content: &str| ProviderMessage::Text {
            role: role.into(),
            content: content.into(),
        };
        let call = |id: &str| ProviderMessage::ToolCall {
            call_id: id.into(),
            name: "read".into(),
            arguments: json!({"path":id}),
            provider_metadata: Value::Null,
        };
        let result = |id: &str| ProviderMessage::ToolResult {
            call_id: id.into(),
            name: "read".into(),
            output: "ok".into(),
            is_error: false,
            attachments: Vec::new(),
        };
        let body = bedrock_request(ProviderRequest {
            model: "test-model".into(),
            system: "test".into(),
            messages: vec![
                text("user", "Read both"),
                text(
                    "user",
                    "<system-reminder>\nToday's date is now 2026-10-14.\n</system-reminder>",
                ),
                call("a"),
                call("b"),
                result("a"),
                result("b"),
                text("assistant", "Done"),
            ],
            tools: Vec::new(),
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        })
        .unwrap();
        let messages = body["messages"].as_array().unwrap();
        let roles: Vec<_> = messages
            .iter()
            .map(|message| message["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["user", "assistant", "user", "assistant"]);
        assert_eq!(messages[0]["content"].as_array().unwrap().len(), 2);
        assert_eq!(messages[1]["content"][1]["toolUse"]["toolUseId"], "b");
        assert_eq!(messages[2]["content"][1]["toolResult"]["toolUseId"], "b");
    }

    #[test]
    fn converse_serializes_pdf_input_and_pdf_tool_results() {
        let body = bedrock_request(ProviderRequest {
            model: "test-model".into(),
            system: "test".into(),
            messages: vec![
                ProviderMessage::Media {
                    role: "user".into(),
                    content: "Review this".into(),
                    attachments: vec![
                        "data:application/pdf;name=design%20notes.pdf;base64,JVBERi0xLjQK".into(),
                    ],
                },
                ProviderMessage::ToolCall {
                    call_id: "read-1".into(),
                    name: "read".into(),
                    arguments: json!({"path":"report.pdf"}),
                    provider_metadata: Value::Null,
                },
                ProviderMessage::ToolResult {
                    call_id: "read-1".into(),
                    name: "read".into(),
                    output: "PDF read successfully".into(),
                    is_error: false,
                    attachments: vec![
                        "data:application/pdf;name=report.pdf;base64,JVBERi0xLjQK".into(),
                    ],
                },
            ],
            tools: Vec::new(),
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        })
        .unwrap();
        assert_eq!(
            body["messages"][0]["content"][1]["document"]["format"],
            "pdf"
        );
        assert_eq!(
            body["messages"][0]["content"][1]["document"]["name"],
            "design notes"
        );
        assert_eq!(
            body["messages"][2]["content"][0]["toolResult"]["content"][1]["document"]["format"],
            "pdf"
        );
    }

    #[test]
    fn converse_never_sends_a_blank_text_block() {
        fn texts(value: &Value, found: &mut Vec<String>) {
            match value {
                Value::Object(map) => {
                    if let Some(text) = map.get("text").and_then(Value::as_str) {
                        found.push(text.to_owned());
                    }
                    map.values().for_each(|value| texts(value, found));
                }
                Value::Array(items) => items.iter().for_each(|value| texts(value, found)),
                _ => {}
            }
        }
        let image = || "data:image/png;base64,iVBORw0KGgo=".to_owned();
        let call = |id: &str| ProviderMessage::ToolCall {
            call_id: id.into(),
            name: "screenshot".into(),
            arguments: json!({}),
            provider_metadata: Value::Null,
        };
        let result = |id: &str, attachments: Vec<String>| ProviderMessage::ToolResult {
            call_id: id.into(),
            name: "screenshot".into(),
            output: String::new(),
            is_error: false,
            attachments,
        };
        let body = bedrock_request(ProviderRequest {
            model: "test-model".into(),
            system: "test".into(),
            messages: vec![
                // An image sent without a word.
                ProviderMessage::Media {
                    role: "user".into(),
                    content: String::new(),
                    attachments: vec![image()],
                },
                ProviderMessage::Text {
                    role: "assistant".into(),
                    content: "\n\n".into(),
                },
                call("a"),
                call("b"),
                result("a", vec![image()]),
                result("b", Vec::new()),
                ProviderMessage::Media {
                    role: "user".into(),
                    content: "  ".into(),
                    attachments: vec![
                        "data:application/pdf;name=spec.pdf;base64,JVBERi0xLjQK".into(),
                    ],
                },
            ],
            tools: Vec::new(),
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        })
        .unwrap();
        let mut found = Vec::new();
        texts(&body["messages"], &mut found);
        assert!(
            found.iter().all(|text| !text.trim().is_empty()),
            "{found:?}"
        );
        let messages = body["messages"].as_array().unwrap();
        assert!(messages[0]["content"][0].get("image").is_some());
        assert!(
            messages[2]["content"][0]["toolResult"]["content"][0]
                .get("image")
                .is_some()
        );
        assert_eq!(
            messages[2]["content"][1]["toolResult"]["content"][0]["text"],
            NO_OUTPUT
        );
        // A document goes with text, as Converse requires.
        assert_eq!(messages[2]["content"][2]["text"], ATTACHED_DOCUMENT);
        assert!(messages[2]["content"][3].get("document").is_some());
    }

    #[test]
    fn structured_output_forces_the_tool_only_where_claude_accepts_it() {
        let choice = |model: &str| {
            bedrock_request(ProviderRequest {
                model: model.into(),
                system: "system".into(),
                messages: vec![ProviderMessage::Text {
                    role: "user".into(),
                    content: "count".into(),
                }],
                tools: Vec::new(),
                reasoning_effort: None,
                parameters: haarness_core::RequestParameters::default(),
                output_schema: Some(json!({"type":"object","properties":{"n":{"type":"integer"}}})),
            })
            .unwrap()["toolConfig"]
                .get("toolChoice")
                .cloned()
        };
        assert!(choice("us.anthropic.claude-sonnet-4-6-v1:0").is_some());
        // Answers are no longer cut at 4,096 tokens; a configured limit wins.
        let limit = |max_output_tokens| {
            bedrock_request(ProviderRequest {
                model: "us.anthropic.claude-sonnet-5-5-v1:0".into(),
                system: "system".into(),
                messages: Vec::new(),
                tools: Vec::new(),
                reasoning_effort: None,
                parameters: haarness_core::RequestParameters {
                    max_output_tokens,
                    ..haarness_core::RequestParameters::default()
                },
                output_schema: None,
            })
            .unwrap()["inferenceConfig"]["maxTokens"]
                .clone()
        };
        assert_eq!(limit(None), 8192);
        assert_eq!(limit(Some(32_000)), 32_000);
        // Opus/Sonnet 5.5 and Fable 5.1 answer 400 to a forced tool choice.
        assert!(choice("us.anthropic.claude-sonnet-5-5-v1:0").is_none());
        assert!(choice("global.anthropic.claude-opus-5-5").is_none());
    }

    #[test]
    fn claude_converse_requests_omit_tools_whose_root_schema_claude_rejects() {
        let tools = vec![
            haarness_core::ToolDefinition {
                name: "plain".into(),
                description: "plain".into(),
                input_schema: json!({"type":"object"}),
            },
            haarness_core::ToolDefinition {
                name: "mcp__fixture__either".into(),
                description: "either".into(),
                input_schema: json!({
                    "type":"object",
                    "oneOf":[{"required":["current"]},{"required":["all"]}]
                }),
            },
        ];
        let names = |model: &str| {
            let body = bedrock_request(ProviderRequest {
                model: model.into(),
                system: "system".into(),
                messages: vec![ProviderMessage::Text {
                    role: "user".into(),
                    content: "list".into(),
                }],
                tools: tools.clone(),
                reasoning_effort: None,
                parameters: haarness_core::RequestParameters::default(),
                output_schema: None,
            })
            .unwrap();
            body["toolConfig"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|tool| tool["toolSpec"]["name"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(names("us.anthropic.claude-sonnet-4-6-v1:0"), ["plain"]);
        assert_eq!(
            names("amazon.nova-pro-v1:0"),
            ["plain", "mcp__fixture__either"]
        );
    }

    #[test]
    fn sigv4_matches_the_aws_sdk_for_model_ids_and_queries() {
        // Signatures from botocore's SigV4Auth for the same request, key and
        // time: a model id holding `:` and a query to sort and encode.
        let credentials = AwsCredentials {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: SecretString::from("wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY"),
            session_token: Some(SecretString::from("session-token")),
        };
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-31T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let signature = |method: &str, url: &str, payload: &[u8]| {
            let mut headers = reqwest::header::HeaderMap::new();
            sign_request_at(
                method,
                url,
                payload,
                "us-east-1",
                &credentials,
                &mut headers,
                now,
            )
            .unwrap();
            assert_eq!(headers["x-amz-date"], "20261031T120000Z");
            assert_eq!(headers["x-amz-security-token"], "session-token");
            let authorization = headers[reqwest::header::AUTHORIZATION]
                .to_str()
                .unwrap()
                .to_owned();
            assert!(authorization.starts_with(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261031/us-east-1/bedrock/aws4_request, SignedHeaders=content-type;host;x-amz-date;x-amz-security-token, Signature="
            ));
            authorization.rsplit_once('=').unwrap().1.to_owned()
        };
        let model = format!(
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/{}/converse-stream",
            encode_path_segment("amazon.nova-pro-v1:0")
        );
        assert_eq!(
            signature("POST", &model, b"{}"),
            "c8c25997acc7b219c3a6a26c7149613cbee821ae4815ccd948d9c29c10468c56"
        );
        assert_eq!(
            signature(
                "GET",
                "https://bedrock.us-east-1.amazonaws.com/inference-profiles?maxResults=1000&typeEquals=SYSTEM_DEFINED&nextToken=a%2Bb%3D",
                b""
            ),
            "46d1159199ee827aabbcfa685795151704e809607a9e849b06007d2e2cfd0fb5"
        );
    }

    #[tokio::test]
    async fn converse_runs_end_to_end_with_bearer_auth() {
        let capture = Capture::default();
        let router = Router::new()
            .route("/model/test-model/converse-stream", post(converse_fixture))
            .with_state(capture.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let provider = BedrockProvider::new_with_region(
            NativeInstance {
                id: "amazon-bedrock".into(),
                name: "Amazon Bedrock".into(),
                base_url: format!("http://{address}"),
                api_key: Some("bedrock-token".into()),
                models: vec!["test-model".into()],
                discover_models: false,
                model_defaults: CompatibleModelDefaults::default(),
                assume_authenticated: false,
                headers: ProviderHeaders::default(),
            },
            None,
            "us-east-1".into(),
        );
        let request = ProviderRequest {
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
        };
        let events = provider
            .stream(request, CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        // The answer arrives piece by piece, then its usage and finish.
        assert!(matches!(&events[0], Ok(ProviderEvent::TextDelta(text)) if text == "hel"));
        assert!(matches!(&events[1], Ok(ProviderEvent::TextDelta(text)) if text == "lo"));
        assert!(matches!(
            &events[2],
            Ok(ProviderEvent::Usage {
                input_tokens: 2,
                output_tokens: 1,
                ..
            })
        ));
        assert!(
            matches!(&events[3], Ok(ProviderEvent::Finished { reason }) if reason == "end_turn")
        );
        let requests = capture.0.lock();
        assert_eq!(requests[0].0["authorization"], "Bearer bedrock-token");
        assert_eq!(requests[0].1["messages"][0]["content"][0]["text"], "hi");
    }

    #[tokio::test]
    async fn a_converse_stream_that_falls_silent_fails_at_the_idle_limit() {
        use haarness_core::ProviderRegistry as _;
        async fn silent() -> axum::body::Body {
            // The first event, then nothing.
            let first = converse_frames(&[("contentBlockDelta", json!({"delta":{"text":"Hal"}}))]);
            let rest = futures::stream::once(async move { Ok::<_, std::io::Error>(first) })
                .chain(futures::stream::pending());
            axum::body::Body::from_stream(rest)
        }
        let router = Router::new().route("/model/test-model/converse-stream", post(silent));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let mut registry = crate::NativeProviderRegistry::default();
        registry.insert(Arc::new(BedrockProvider::new_with_region(
            NativeInstance {
                id: "amazon-bedrock".into(),
                name: "Amazon Bedrock".into(),
                base_url: format!("http://{address}"),
                api_key: Some("bedrock-token".into()),
                models: vec!["test-model".into()],
                discover_models: false,
                model_defaults: CompatibleModelDefaults::default(),
                assume_authenticated: false,
                headers: ProviderHeaders::default(),
            },
            None,
            "us-east-1".into(),
        )));
        // `stream_idle_timeout` reaches the provider through the registry.
        let registry = registry.with_stream_limits(&std::collections::HashMap::from([(
            "amazon-bedrock".to_owned(),
            haarness_core::StreamLimits {
                idle: Some(std::time::Duration::from_millis(300)),
                total: None,
            },
        )]));
        let request = ProviderRequest {
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
        };
        let events = registry
            .get("amazon-bedrock")
            .unwrap()
            .stream(request, CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert!(matches!(&events[0], Ok(ProviderEvent::TextDelta(text)) if text == "Hal"));
        assert_eq!(
            events[1].as_ref().unwrap_err().to_string(),
            "provider error: the provider sent nothing for 300 milliseconds"
        );
    }
}
