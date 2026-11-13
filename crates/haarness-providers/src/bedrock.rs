//! Amazon Bedrock: Converse answers streamed with ConverseStream, signed with
//! a Bedrock API key (a bearer token) or SigV4, and the models the account's
//! region offers, read from the Bedrock control plane.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    sync::{Arc, RwLock},
    time::Duration,
};

use async_stream::try_stream;
use async_trait::async_trait;
use chrono::Utc;
use futures::{StreamExt as _, TryStreamExt as _};
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
    CompatibleModelDefaults, CredentialStore, NativeInstance, ProviderHeaders,
    aws_config::{AwsCredentials, AwsSignIn},
    aws_event_stream,
    media::Unreadable,
};

type HmacSha256 = Hmac<Sha256>;

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
    /// The AWS CLI's sign-in on this machine, for SigV4.
    sign_in: Option<AwsSignIn>,
    /// The configured models, or a few current ones until discovery answers.
    models: Vec<ModelInfo>,
    live_models: crate::live_models::LiveModels,
    /// What the account's discovered models take, by the id requests use.
    controls: RwLock<HashMap<String, ModelControls>>,
    /// The inference profile a model that does not run on demand is asked
    /// through, by its own id (`anthropic.claude-haiku-5-5` →
    /// `global.anthropic.claude-haiku-5-5`): from the model list, or learned
    /// when Bedrock refused the id itself.
    profiles: RwLock<HashMap<String, String>>,
    /// Models that refused tools on ConverseStream, answered whole instead.
    whole_with_tools: RwLock<HashSet<String>>,
    /// Models that refused a forced tool choice.
    unforced: RwLock<HashSet<String>>,
    /// Whether this computer's AWS sign-in may sign requests.
    ambient: bool,
}

/// How a request is signed.
enum Signer {
    Bearer(String),
    SigV4(AwsCredentials),
}

impl BedrockProvider {
    pub fn from_environment(auth_store: Option<Arc<CredentialStore>>) -> Self {
        let endpoint = std::env::var("AWS_BEDROCK_RUNTIME_ENDPOINT").ok();
        let instance = NativeInstance {
            id: "amazon-bedrock".into(),
            name: "Amazon Bedrock".into(),
            base_url: endpoint.clone().unwrap_or_default(),
            api_key: None,
            models: DEFAULT_MODELS.iter().map(|id| (*id).to_owned()).collect(),
            discover_models: true,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: false,
            headers: ProviderHeaders::default(),
        };
        let mut provider = Self::new_with_region(instance, auth_store, String::new());
        provider.region = provider.default_region();
        if endpoint.is_none() {
            provider.instance.base_url =
                format!("https://bedrock-runtime.{}.amazonaws.com", provider.region);
        }
        provider
    }

    pub fn new(instance: NativeInstance, auth_store: Option<Arc<CredentialStore>>) -> Self {
        // An endpoint of AWS's names its region, which a signature must name.
        let region = crate::aws_config::endpoint_region(&instance.base_url);
        let mut provider = Self::new_with_region(instance, auth_store, region.unwrap_or_default());
        if provider.region.is_empty() {
            provider.region = provider.default_region();
        }
        provider
    }

    /// The region when no endpoint names one: the environment's, else the
    /// AWS profile's, else the one a short-term API key was made for (it
    /// works nowhere else), else us-east-1.
    fn default_region(&self) -> String {
        std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .ok()
            .filter(|region| !region.is_empty())
            .or_else(crate::aws_config::profile_region)
            .or_else(|| {
                self.bearer
                    .as_ref()
                    .map(|key| key.expose_secret().to_owned())
                    .or_else(|| self.stored_bearer())
                    .and_then(|key| crate::aws_config::api_key_region(&key))
            })
            .unwrap_or_else(|| "us-east-1".into())
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
        let sign_in = AwsSignIn::from_environment();
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
            sign_in,
            models,
            live_models: crate::live_models::LiveModels::default(),
            controls: RwLock::default(),
            profiles: RwLock::default(),
            whole_with_tools: RwLock::default(),
            unforced: RwLock::default(),
            ambient: true,
        }
    }

    /// Send requests through `client`: for a provider a project defines, one
    /// that follows no redirect off its server (see [`crate::CredentialFence`]).
    #[must_use]
    pub(crate) fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// Use only the provider's own key: no `AWS_BEARER_TOKEN_BEDROCK` and no
    /// AWS setup's keys, since the provider is defined where they do not
    /// belong (see [`crate::own_cloud_address`]).
    #[must_use]
    pub fn without_ambient_credentials(mut self) -> Self {
        self.bearer = self
            .instance
            .api_key
            .clone()
            .filter(|value| !value.is_empty())
            .map(SecretString::from);
        self.sign_in = None;
        self.ambient = false;
        self
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
        self.bearer.is_some()
            || self.sign_in.as_ref().is_some_and(AwsSignIn::usable)
            || self.stored_bearer().is_some()
    }

    /// Whether a request could be signed at all, if the AWS sign-in works.
    fn can_sign(&self) -> bool {
        self.bearer.is_some() || self.sign_in.is_some() || self.stored_bearer().is_some()
    }

    /// How requests are signed: a Bedrock API key as a bearer token, else
    /// the AWS credentials this machine's AWS setup gives.
    async fn signer(&self) -> CoreResult<Signer> {
        if let Some(token) = self
            .bearer
            .as_ref()
            .map(|value| value.expose_secret().to_owned())
            .or_else(|| self.stored_bearer())
        {
            return Ok(Signer::Bearer(token));
        }
        if let Some(sign_in) = &self.sign_in {
            return Ok(Signer::SigV4(sign_in.credentials().await?));
        }
        if !self.ambient {
            return Err(CoreError::ProviderUnavailable(format!(
                "{} has no key of its own here, and your AWS sign-in goes only to Amazon Bedrock's addresses. To use the sign-in at this address, define {} in your global configuration.",
                self.instance.name, self.instance.name
            )));
        }
        Err(CoreError::Provider(
            "Amazon Bedrock is not authenticated; set AWS_BEARER_TOKEN_BEDROCK, AWS credentials, or run `haarness auth login amazon-bedrock`".into(),
        ))
    }

    /// Sign `headers` for a request to `url`.
    fn authorize(
        &self,
        signer: &Signer,
        method: &str,
        url: &str,
        payload: &[u8],
        headers: &mut reqwest::header::HeaderMap,
    ) -> CoreResult<()> {
        match signer {
            Signer::Bearer(token) => {
                headers.insert(
                    reqwest::header::AUTHORIZATION,
                    header(&format!("Bearer {token}"))?,
                );
                Ok(())
            }
            Signer::SigV4(credentials) => {
                sign_request(method, url, payload, &self.region, credentials, headers)
            }
        }
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
        let signer = self.signer().await.ok()?;
        self.authorize(&signer, "GET", &url, b"", &mut headers)
            .ok()?;
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
        if !self.instance.discover_models || !self.can_sign() {
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
        let (models, controls, aliases) = discovered_models(
            &foundation,
            &profiles,
            &self.region,
            &self.instance.model_defaults,
        );
        *self.controls.write().unwrap() = controls;
        self.profiles.write().unwrap().extend(aliases);
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

    /// The id a request for `model` names: its inference profile when it
    /// runs only through one.
    fn wire_model(&self, model: &str) -> String {
        self.profiles
            .read()
            .unwrap()
            .get(model)
            .cloned()
            .unwrap_or_else(|| model.to_owned())
    }

    /// Whether a request with tools to `model` goes to Converse, answered
    /// whole, rather than ConverseStream.
    fn answers_tools_whole(&self, model: &str) -> bool {
        refuses_streamed_tools(model) || self.whole_with_tools.read().unwrap().contains(model)
    }
}

/// The models AWS lists as taking tools on Converse but refusing them on
/// ConverseStream ("This model doesn't support tool use in streaming mode"):
/// Llama 3.1 and 3.3, Llama 4, Mistral Large 1 and 2, Pixtral Large and
/// Writer's Palmyra X4 and X5. Any other that refuses so is learned.
fn refuses_streamed_tools(model: &str) -> bool {
    [
        "meta.llama3-1-",
        "meta.llama3-3-",
        "meta.llama4-",
        "mistral.mistral-large-2402",
        "mistral.mistral-large-2407",
        "mistral.pixtral-large",
        "writer.palmyra-x4",
        "writer.palmyra-x5",
    ]
    .iter()
    .any(|family| model.contains(family))
}

/// The cross-region inference profile of this region's geography for a
/// model named by its own id (`us.amazon.nova-micro-v1:0`), for a model that
/// Bedrock runs only through a profile; none for an ARN or a profile id.
fn regional_profile(model: &str, region: &str) -> Option<String> {
    let profiled = GEOGRAPHIES.iter().any(|prefix| model.starts_with(prefix));
    (!profiled && !model.starts_with("arn:") && model.contains('.'))
        .then(|| format!("{}.{model}", geography(region)))
}

/// Whether Bedrock refused replayed reasoning: a block moved or changed, or,
/// from Claude 5.5, one bound to a prompt that has changed since.
fn refuses_reasoning(lower: &str) -> bool {
    lower.contains("bound to a different conversation")
        || (lower.contains("thinking") && lower.contains("cannot be modified"))
        || lower.contains("invalid `signature` in `thinking` block")
}

/// `body` without the reasoning it replays or asks for, when it held any:
/// asked once more after Bedrock refused the reasoning, the conversation
/// goes on without it rather than failing.
fn without_reasoning(body: &mut Value) -> bool {
    let mut removed = false;
    for message in body["messages"].as_array_mut().into_iter().flatten() {
        if let Some(content) = message["content"].as_array_mut() {
            let before = content.len();
            content.retain(|block| block.get("reasoningContent").is_none());
            removed |= content.len() != before;
        }
    }
    if let Some(extra) = body["additionalModelRequestFields"].as_object_mut() {
        for field in ["thinking", "reasoningConfig", "anthropic_beta"] {
            removed |= extra.remove(field).is_some();
        }
        if extra.is_empty() {
            body.as_object_mut()
                .map(|fields| fields.remove("additionalModelRequestFields"));
        }
    }
    removed
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
                    .or_else(|| self.sign_in.as_ref().and_then(AwsSignIn::access_key_hint)),
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
        let asked = request.model.clone();
        let mut model_id = self.wire_model(&asked);
        let controls = self.controls(&model_id);
        let models = self.live_models.get(&self.models);
        let model = models
            .iter()
            .find(|info| info.id == asked || info.id == model_id);
        let mut body = bedrock_request(request, &controls, model)?;
        if self.unforced.read().unwrap().contains(&model_id) {
            unforce(&mut body);
        }
        let tools = body.get("toolConfig").is_some();
        let (mut profiled, mut unforced, mut unreasoned, mut renewed) =
            (false, false, false, false);
        // The refusal of the model's own id, its status and trace, kept while
        // its inference profile is asked.
        let mut first_refusal = None;
        loop {
            let whole = tools && self.answers_tools_whole(&model_id);
            let url = format!(
                "{}/model/{}/{}",
                self.instance.base_url.trim_end_matches('/'),
                encode_path_segment(&model_id),
                if whole { "converse" } else { "converse-stream" }
            );
            let payload = serde_json::to_vec(&body).map_err(|error| {
                CoreError::Provider(format!("cannot encode Bedrock request: {error}"))
            })?;
            let mut headers = self.instance.headers.map();
            headers.insert(
                reqwest::header::CONTENT_TYPE,
                reqwest::header::HeaderValue::from_static("application/json"),
            );
            let signer = self.signer().await?;
            self.authorize(&signer, "POST", &url, &payload, &mut headers)?;
            // A whole answer sends nothing until it is complete: it may take
            // as long as the response limit allows.
            let wait = if whole {
                StreamLimits {
                    idle: Some(limits.total().unwrap_or(Duration::ZERO)),
                    total: limits.total,
                }
            } else {
                limits
            };
            let response = tokio::select! {
                () = cancel.cancelled() => return Err(CoreError::Cancelled),
                response = crate::sse::send(self.client.post(&url).headers(headers).body(payload), wait) => response?,
            };
            if response.status().is_success() {
                if profiled {
                    self.profiles
                        .write()
                        .unwrap()
                        .insert(asked.clone(), model_id.clone());
                }
                let provider = self.instance.id.clone();
                return Ok(if whole {
                    converse_whole(response, cancel, limits, provider)
                } else {
                    converse_stream(response, cancel, limits, provider)
                });
            }
            let status = response.status();
            let kind = error_type(response.headers());
            let trace = crate::RequestTrace::of(&response);
            let limit = limits.idle(crate::sse::is_loopback(response.url()));
            let text = crate::sse::within(limit, response.text())
                .await?
                .unwrap_or_default();
            let lower = text.to_ascii_lowercase();
            // Temporary AWS credentials that expired before their stated
            // time (or that name none) are read afresh once.
            if status == reqwest::StatusCode::FORBIDDEN
                && !renewed
                && lower.contains("security token included in the request is expired")
                && let (Ok(Signer::SigV4(_)), Some(sign_in)) = (self.signer().await, &self.sign_in)
            {
                renewed = true;
                sign_in.forget();
                continue;
            }
            if status == reqwest::StatusCode::BAD_REQUEST {
                // Llama 3.1+, Mistral Large and others take tools only on
                // Converse, as AWS documents: the turn is answered whole.
                if tools && !whole && lower.contains("tool use in streaming mode") {
                    tracing::info!(model = %model_id, "Bedrock takes this model's tools only on Converse; answering whole");
                    self.whole_with_tools
                        .write()
                        .unwrap()
                        .insert(model_id.clone());
                    continue;
                }
                // A forced tool choice the model refuses ("doesn't support
                // the toolConfig.toolChoice.tool field"): the structured
                // answer is asked for without forcing it.
                if !unforced
                    && body.pointer("/toolConfig/toolChoice").is_some()
                    && (lower.contains("toolchoice") || lower.contains("tool_choice"))
                {
                    unforced = true;
                    unforce(&mut body);
                    self.unforced.write().unwrap().insert(model_id.clone());
                    continue;
                }
                // A model named by its own id that Bedrock runs only through
                // an inference profile: asked through this region's.
                if !profiled
                    && lower.contains("with on-demand throughput isn")
                    && let Some(profile) = regional_profile(&model_id, &self.region)
                {
                    tracing::info!(model = %model_id, %profile, "Bedrock runs this model only through an inference profile");
                    profiled = true;
                    first_refusal =
                        Some((http_error(status, kind.as_deref(), &text), status, trace));
                    model_id = profile;
                    continue;
                }
                if !unreasoned && refuses_reasoning(&lower) && without_reasoning(&mut body) {
                    tracing::warn!(model = %model_id, "Bedrock refused the replayed reasoning; asking once more without it");
                    unreasoned = true;
                    continue;
                }
                // The profile guessed for this region does not exist: what
                // Bedrock said of the model's own id says more.
                if profiled
                    && lower.contains("model identifier is invalid")
                    && let Some((refusal, first_status, first_trace)) = first_refusal.take()
                {
                    // The log names the request whose refusal the person reads.
                    first_trace.refused(first_status);
                    return Err(refusal);
                }
            }
            trace.refused(status);
            return Err(http_error(status, kind.as_deref(), &text));
        }
    }
}

/// `body` with no forced tool choice.
fn unforce(body: &mut Value) {
    if let Some(config) = body["toolConfig"].as_object_mut() {
        config.remove("toolChoice");
    }
}

/// A whole Converse answer, for a model that takes tools only so, read as
/// the same events a streamed one gives.
fn converse_whole(
    response: reqwest::Response,
    cancel: CancellationToken,
    limits: StreamLimits,
    provider_id: String,
) -> ProviderStream {
    let trace = crate::RequestTrace::of(&response);
    let failed = trace.clone();
    let stream = try_stream! {
        let body = async {
            match limits.total() {
                Some(limit) => tokio::time::timeout(limit, response.text())
                    .await
                    .map_err(|_| haarness_core::response_timeout(limit))?
                    .map_err(crate::sse::transport_error),
                None => response.text().await.map_err(crate::sse::transport_error),
            }
        };
        let body = tokio::select! {
            () = cancel.cancelled() => Err(CoreError::Cancelled),
            body = body => body,
        }?;
        let answer: Value = serde_json::from_str(&body).map_err(|error| {
            CoreError::Provider(format!("Bedrock sent an unreadable answer: {error}"))
        })?;
        let mut events = ConverseEvents::default();
        for (kind, payload) in whole_answer_events(&answer) {
            for event in events.event(kind, &payload) {
                yield event;
            }
        }
        let finished = events.finish()?;
        if let ProviderEvent::Finished { reason } = &finished {
            crate::log_blocked_finish(&provider_id, reason, trace.id(), None);
        }
        yield finished;
    };
    Box::pin(stream.inspect_err(move |error| failed.failed(error)))
}

/// A whole Converse answer as the ConverseStream events that would have
/// carried it.
fn whole_answer_events(answer: &Value) -> Vec<(&'static str, Value)> {
    let mut events = Vec::new();
    let content = answer
        .pointer("/output/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for (index, block) in content.enumerate() {
        let delta = |delta: Value| json!({"contentBlockIndex":index,"delta":delta});
        if let Some(reasoning) = block.get("reasoningContent") {
            if let Some(text) = reasoning.pointer("/reasoningText/text") {
                events.push((
                    "contentBlockDelta",
                    delta(json!({"reasoningContent":{"text":text}})),
                ));
            }
            if let Some(signature) = reasoning.pointer("/reasoningText/signature") {
                events.push((
                    "contentBlockDelta",
                    delta(json!({"reasoningContent":{"signature":signature}})),
                ));
            }
            if let Some(redacted) = reasoning.get("redactedContent") {
                events.push((
                    "contentBlockDelta",
                    delta(json!({"reasoningContent":{"redactedContent":redacted}})),
                ));
            }
        } else if let Some(call) = block.get("toolUse") {
            events.push((
                "contentBlockStart",
                json!({"contentBlockIndex":index,"start":{"toolUse":{"toolUseId":call["toolUseId"],"name":call["name"]}}}),
            ));
            let input = call
                .get("input")
                .map_or_else(|| "{}".to_owned(), Value::to_string);
            events.push((
                "contentBlockDelta",
                delta(json!({"toolUse":{"input":input}})),
            ));
        } else if let Some(text) = block.get("text") {
            events.push(("contentBlockDelta", delta(json!({"text":text}))));
        }
        events.push(("contentBlockStop", json!({"contentBlockIndex":index})));
    }
    events.push((
        "messageStop",
        json!({"stopReason":answer.get("stopReason").cloned().unwrap_or(json!("end_turn"))}),
    ));
    if let Some(usage) = answer.get("usage") {
        events.push(("metadata", json!({"usage":usage})));
    }
    events
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
    let trace = crate::RequestTrace::of(&response);
    let failed = trace.clone();
    let stream = try_stream! {
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
            crate::log_blocked_finish(&provider_id, reason, trace.id(), None);
        }
        yield finished;
    };
    // An exception Bedrock sends, or a stream that breaks off, names the
    // request in the log.
    Box::pin(stream.inspect_err(move |error| failed.failed(error)))
}

/// Set on a call's metadata when some of its reasoning came after words of
/// the same answer: how many of its reasoning blocks came before them.
const REASONING_BEFORE_WORDS: &str = "bedrock_reasoning_before_words";

/// ConverseStream's events turned into Haarness's, holding back what is
/// only whole at its block's end: a tool call's arguments and a reasoning
/// block with its signature, which goes back to the model with the call.
#[derive(Default)]
struct ConverseEvents {
    tool: Option<(String, String, String)>,
    reasoning: Option<Value>,
    /// The redacted reasoning of the open block, which Bedrock may send in
    /// several pieces (each base64 on the wire).
    redacted: Vec<u8>,
    reasoning_blocks: Vec<Value>,
    /// Whether words were written since the last call, and how many of the
    /// reasoning blocks held for the next call came before any. Claude binds
    /// its reasoning to everything written before it, so each block goes
    /// back where it was written (see [`thinking_in_place`]).
    words_since_call: bool,
    reasoning_before_words: usize,
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
            _ => Ok(self.event(message.header(":event-type").unwrap_or_default(), &payload)),
        }
    }

    fn event(&mut self, kind: &str, payload: &Value) -> Vec<ProviderEvent> {
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
                    self.words_since_call = true;
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
                    // Redacted reasoning goes back whole: its pieces joined.
                    if let Some(piece) = reasoning["redactedContent"].as_str() {
                        use base64::Engine as _;
                        let engine = base64::engine::general_purpose::STANDARD;
                        match engine.decode(piece) {
                            Ok(bytes) => self.redacted.extend(bytes),
                            Err(_) => self.redacted.extend(piece.as_bytes()),
                        }
                        *block = json!({"redactedContent": engine.encode(&self.redacted)});
                    }
                }
            }
            "contentBlockStop" => {
                if let Some(block) = self.reasoning.take() {
                    self.redacted.clear();
                    if !self.words_since_call {
                        self.reasoning_before_words += 1;
                    }
                    self.reasoning_blocks
                        .push(json!({"reasoningContent": block}));
                }
                if let Some((call_id, name, input)) = self.tool.take() {
                    let arguments = haarness_core::tool_call_arguments(&input);
                    if name == STRUCTURED_OUTPUT_TOOL_NAME {
                        events.push(ProviderEvent::StructuredOutput(arguments));
                    } else {
                        self.words_since_call = false;
                        let before_words = std::mem::take(&mut self.reasoning_before_words);
                        let provider_metadata = if self.reasoning_blocks.is_empty() {
                            Value::Null
                        } else {
                            let blocks = std::mem::take(&mut self.reasoning_blocks);
                            let mut metadata = json!({});
                            if before_words < blocks.len() {
                                metadata[REASONING_BEFORE_WORDS] = json!(before_words);
                            }
                            metadata["bedrock_reasoning"] = json!(blocks);
                            metadata
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
        events
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
        "modelStreamErrorException" => 424,
        "validationException" => 400,
        "accessDeniedException" => 403,
        // `internalServerException` and anything unnamed.
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

/// The geographies inference profile ids begin with.
const GEOGRAPHIES: [&str; 8] = [
    "global.", "us.", "eu.", "apac.", "jp.", "au.", "ca.", "us-gov.",
];

/// Bedrock writes a model id with a geography (`us.`, `global.`), its
/// vendor and a version tag (`-v1:0`); the bare id (`claude-sonnet-5-5`,
/// `gpt-oss-120b`) is what the price table and the names know.
fn bare_model_id(id: &str) -> &str {
    let id = id.rsplit_once('/').map_or(id, |(_, id)| id);
    let id = GEOGRAPHIES
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
    id.clone_into(&mut info.id);
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

/// The chat models of a region, what each takes, and the inference profile
/// each model that does not run on demand is asked through.
type Discovered = (
    Vec<ModelInfo>,
    HashMap<String, ModelControls>,
    HashMap<String, String>,
);

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
) -> Discovered {
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
    let mut aliases = HashMap::new();
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
            // A model named by its own id, as in AWS's docs, is asked
            // through the profile: Bedrock refuses it on demand.
            aliases.insert(model.to_owned(), (*profile).to_owned());
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
    (models, controls, aliases)
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

/// The Converse request for `request`. `model` is Bedrock's entry for the
/// model, when its list was read: it names the model and says whether it
/// reads images.
fn bedrock_request(
    request: ProviderRequest,
    controls: &ModelControls,
    model: Option<&ModelInfo>,
) -> CoreResult<Value> {
    let model_name = model.map_or_else(
        || haarness_protocol::model_display_name(&request.model, None),
        |info| info.display_name.clone(),
    );
    // A model Bedrock lists without image input (gpt-oss, a text-only Llama)
    // refuses a request with an image in it; one not listed is sent them.
    let reads_images = model.is_none_or(|info| info.images);
    let newest = crate::media::newest_unanswered(&request.messages);
    // Converse takes images and PDFs. Anything else is refused in the
    // person's newest message and is a note everywhere else.
    let block = |item: crate::media::LoadedMedia, place: Unreadable<'_>| -> CoreResult<Value> {
        Ok(if item.is_image() && reads_images {
            let format = item.mime_type.strip_prefix("image/").unwrap_or("png");
            json!({"image":{"format":format,"source":{"bytes":item.base64}}})
        } else if item.is_pdf() {
            json!({"document":{
                "format":"pdf",
                "name":bedrock_document_name(&item.filename),
                "source":{"bytes":item.base64}
            }})
        } else {
            json!({"text":place.stand_in(&item)?})
        })
    };
    // Each message, with how many of its opening reasoning blocks Claude
    // wrote before the words of the message ahead of it (a call's only).
    let mut messages: Vec<(Value, usize)> = Vec::new();
    for (index, message) in request.messages.into_iter().enumerate() {
        match message {
            // Converse refuses a blank text block, so blank text is left out
            // wherever it would stand alone.
            ProviderMessage::Text { content, .. } if content.trim().is_empty() => {}
            ProviderMessage::Text { role, content } => {
                messages.push((json!({"role":role,"content":[{"text":content}]}), 0));
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
                let place = Unreadable::at(index, newest, &model_name);
                for item in media {
                    blocks.push(block(item, place)?);
                }
                messages.push((json!({"role":role,"content":blocks}), 0));
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
                // A call saved before this was recorded had all of it first.
                let before_words = provider_metadata[REASONING_BEFORE_WORDS]
                    .as_u64()
                    .and_then(|count| usize::try_from(count).ok())
                    .map_or(blocks.len(), |count| count.min(blocks.len()));
                blocks.push(json!({"toolUse":{"toolUseId":crate::tool_ids::portable(&call_id),"name":name,"input":arguments}}));
                messages.push((json!({"role":"assistant","content":blocks}), before_words));
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
                // A tool's file never refuses the turn: the model goes on
                // without it.
                for item in media {
                    content.push(block(item, Unreadable::FromTool)?);
                }
                messages.push((json!({"role":"user","content":[{"toolResult":{"toolUseId":crate::tool_ids::portable(&call_id),"content":content,"status":if is_error {"error"} else {"success"}}}]}), 0));
            }
        }
    }
    let mut messages = thinking_in_place(messages);
    // Within the model's own ceiling: Bedrock refuses a request asking for
    // more ("The maximum tokens you requested exceeds the model limit").
    let mut max_tokens = request
        .parameters
        .max_output_tokens
        .filter(|limit| *limit > 0)
        .unwrap_or(8192)
        .min(
            controls
                .max_output_tokens
                .filter(|limit| *limit > 0)
                .unwrap_or(u64::MAX),
        );
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
    // Claude 5.5 (Fable and Mythos from 5.1) thinks whatever the request
    // says and binds each thinking block to everything before it: a block
    // whose prompt changed (tools offered, a compaction, a moved block) is
    // refused with "The block is bound to a different conversation" unless
    // the request asks Bedrock to drop such a block instead, as Haarness asks
    // Anthropic's API.
    if claude
        && crate::anthropic::binds_thinking(&request.model)
        && !matches!(controls.reasoning, Some(Reasoning::Budget { .. }))
    {
        extra["thinking"] = json!({
            "type":"adaptive",
            "display":"summarized",
            "block_binding":{"prefix_mismatch_behavior":"drop_block"}
        });
        extra["anthropic_beta"] = json!([crate::anthropic::THINKING_BINDING_BETA]);
    }
    let mut tools: Vec<_> = requested_tools
        .into_iter()
        .map(|tool| json!({"toolSpec":{"name":tool.name,"description":tool.description,"inputSchema":{"json":tool.input_schema}}}))
        .collect();
    // Converse refuses a blank system block, and a cache point alone.
    let mut system: Vec<Value> = (!request.system.trim().is_empty())
        .then(|| json!({"text":request.system}))
        .into_iter()
        .collect();
    if cache {
        if !system.is_empty() {
            system.push(serde_json::from_str(CACHE_POINT).unwrap_or_default());
        }
        // Claude caches the tools too; other models refuse a cache point there.
        if claude && !tools.is_empty() {
            tools.push(serde_json::from_str(CACHE_POINT).unwrap_or_default());
        }
    }
    let mut body = json!({
        "messages":messages,
        // The Anthropic adapter's default; a configured limit wins.
        "inferenceConfig":{"maxTokens":max_tokens}
    });
    if !system.is_empty() {
        body["system"] = json!(system);
    }
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

/// Converse wants user and assistant turns to alternate: parallel calls
/// share one assistant turn, their results one user turn, and a prompt
/// followed by its stored reminder or a steered message is one user turn.
///
/// Each reasoning block goes back where the model wrote it. Words written
/// before a call are their own message, ahead of the call carrying the
/// reasoning; the reasoning written before those words opens the turn and
/// the rest follows them. Claude refuses a turn whose reasoning was moved
/// ("blocks in the latest assistant message cannot be modified"), and from
/// Claude 5.5 a block moved past words is bound to a different prompt, so
/// one misplaced block failed every later request of the chat.
fn thinking_in_place(messages: Vec<(Value, usize)>) -> Vec<Value> {
    let mut grouped: Vec<Value> = Vec::new();
    for (mut message, before_words) in messages {
        let Some(previous) = grouped
            .last_mut()
            .filter(|previous| previous["role"] == message["role"])
        else {
            grouped.push(message);
            continue;
        };
        let content = message["content"]
            .as_array_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        let earlier = previous["content"].as_array_mut().unwrap();
        let words_only = earlier.iter().all(|block| block.get("text").is_some());
        if before_words > 0 && words_only {
            let words = std::mem::take(earlier);
            let mut content = content.into_iter();
            earlier.extend(content.by_ref().take(before_words));
            earlier.extend(words);
            earlier.extend(content);
        } else {
            earlier.extend(content);
        }
    }
    grouped
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
        .or_else(|| crate::sse::provider_error_detail(body))
        .map(|detail| account_reason(&detail).unwrap_or(detail));
    CoreError::Provider(match (kind, detail) {
        (Some(kind), Some(detail)) => format!("HTTP {status}: [{kind}] {detail}"),
        (None, Some(detail)) => format!("HTTP {status}: {detail}"),
        (Some(kind), None) => format!("HTTP {status}: [{kind}]"),
        (None, None) => format!("HTTP {status}"),
    })
}

/// AWS's words for an account that may not use a model yet say neither why
/// nor where to look ("… is not available for this account … contact AWS
/// Sales"; "Too many tokens per day, please wait", when the quota is 0).
/// Both are what a new account gets. A model that takes no tools is said
/// so too, with what to do.
fn account_reason(detail: &str) -> Option<String> {
    if let Some((model, _)) = detail.split_once(" is not available for this account") {
        return Some(format!(
            "AWS has not enabled {model} for this account yet. It refuses this way while the account is on AWS's Free plan, while AWS is still verifying a new account, or while the model's Marketplace agreement is pending; the model's page in the Bedrock console says which, and AWS Support can tell you more."
        ));
    }
    // A model that takes no tools at all (DeepSeek-R1, Llama 3, Mistral 7B,
    // Mixtral): Haarness works through its tools.
    let lower = detail.to_ascii_lowercase();
    if (lower.starts_with("this model doesn't support tool use")
        || lower.starts_with("this model does not support tool use"))
        && !lower.contains("streaming")
    {
        return Some(
            "This model can't use tools, which Haarness needs to read and change your files. Pick a model that can, such as Claude, Nova or gpt-oss.".to_owned(),
        );
    }
    detail.starts_with("Too many tokens per day").then(|| {
        "This model's daily token quota on this AWS account is used up, or is 0, as on a new account. Service Quotas in the AWS console shows it.".to_owned()
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
        super::bedrock_request(request, &ModelControls::default(), None)
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

    fn discovered() -> Discovered {
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
        let (models, _, _) = discovered();
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
        let (_, controls, _) = discovered();
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
                None,
            )
            .unwrap()
        };
        let sonnet = request("global.anthropic.claude-sonnet-5-5", Some("xhigh"), true);
        assert_eq!(
            sonnet["additionalModelRequestFields"]["output_config"],
            json!({"effort":"xhigh"})
        );
        assert_eq!(
            sonnet["additionalModelRequestFields"]["thinking"]["block_binding"],
            json!({"prefix_mismatch_behavior":"drop_block"})
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
        cut.event("contentBlockDelta", &json!({"delta":{"text":"Hal"}}));
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
        // What a new account is told, in words that say why and where to look.
        assert_eq!(
            http_error(
                reqwest::StatusCode::FORBIDDEN,
                Some("AccessDeniedException"),
                r#"{"message":"anthropic.claude-haiku-5-5 is not available for this account. You can explore other available models on Amazon Bedrock. For additional access options, contact AWS Sales at https://aws.amazon.com/contact-us/sales-support/"}"#
            )
            .to_string(),
            "provider error: HTTP 403 Forbidden: [AccessDeniedException] AWS has not enabled anthropic.claude-haiku-5-5 for this account yet. It refuses this way while the account is on AWS's Free plan, while AWS is still verifying a new account, or while the model's Marketplace agreement is pending; the model's page in the Bedrock console says which, and AWS Support can tell you more."
        );
        let daily = http_error(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            Some("ThrottlingException"),
            r#"{"message":"Too many tokens per day, please wait before trying again."}"#,
        );
        assert_eq!(
            daily.to_string(),
            "provider error: HTTP 429 Too Many Requests: [ThrottlingException] This model's daily token quota on this AWS account is used up, or is 0, as on a new account. Service Quotas in the AWS console shows it."
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
                call("functions.b:1"),
                result("a"),
                result("functions.b:1"),
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
        // Another model's id, in the characters Converse takes.
        assert_eq!(
            messages[1]["content"][1]["toolUse"]["toolUseId"],
            "functions_b_1"
        );
        assert_eq!(
            messages[2]["content"][1]["toolResult"]["toolUseId"],
            "functions_b_1"
        );
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

    /// A Bedrock that answers each request as `answer` says, by the path
    /// asked for and the body sent, keeping what it was sent.
    type Answer = Arc<dyn Fn(&str, &Value) -> (u16, Vec<u8>) + Send + Sync>;
    /// The path and body of each request a scripted Bedrock was sent.
    type Seen = Arc<Mutex<Vec<(String, Value)>>>;

    async fn scripted_bedrock(answer: Answer) -> (String, Seen) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let state = (answer, seen.clone());
        let router = Router::new()
            .fallback(
                |State((answer, seen)): State<(Answer, Seen)>,
                 uri: axum::http::Uri,
                 Json(body): Json<Value>| async move {
                    let path = uri.path().to_owned();
                    let number = {
                        let mut seen = seen.lock();
                        seen.push((path.clone(), body.clone()));
                        seen.len()
                    };
                    let (status, bytes) = answer(&path, &body);
                    // Each answer names its request, as AWS does.
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        [("x-amzn-requestid", format!("bedrock-step-{number}"))],
                        bytes,
                    )
                },
            )
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (format!("http://{address}"), seen)
    }

    fn scripted_provider(base_url: String, models: &[&str]) -> BedrockProvider {
        BedrockProvider::new_with_region(
            NativeInstance {
                id: "amazon-bedrock".into(),
                name: "Amazon Bedrock".into(),
                base_url,
                api_key: Some("bedrock-token".into()),
                models: models.iter().map(|model| (*model).to_owned()).collect(),
                discover_models: false,
                model_defaults: CompatibleModelDefaults::default(),
                assume_authenticated: false,
                headers: ProviderHeaders::default(),
            },
            None,
            "us-east-1".into(),
        )
    }

    fn ask(model: &str, tools: bool) -> ProviderRequest {
        ProviderRequest {
            model: model.into(),
            system: "system".into(),
            messages: vec![ProviderMessage::Text {
                role: "user".into(),
                content: "list the files".into(),
            }],
            tools: if tools {
                vec![haarness_core::ToolDefinition {
                    name: "list".into(),
                    description: "List files".into(),
                    input_schema: json!({"type":"object","properties":{"path":{"type":"string"}}}),
                }]
            } else {
                Vec::new()
            },
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        }
    }

    async fn answer_of(
        provider: &BedrockProvider,
        request: ProviderRequest,
    ) -> CoreResult<Vec<ProviderEvent>> {
        provider
            .stream(request, CancellationToken::new())
            .await?
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect()
    }

    fn refusal(message: &str) -> (u16, Vec<u8>) {
        (400, json!({"message": message}).to_string().into_bytes())
    }

    fn streamed_text(text: &str) -> (u16, Vec<u8>) {
        (
            200,
            converse_frames(&[
                ("messageStart", json!({"role":"assistant"})),
                (
                    "contentBlockDelta",
                    json!({"contentBlockIndex":0,"delta":{"text":text}}),
                ),
                ("contentBlockStop", json!({"contentBlockIndex":0})),
                ("messageStop", json!({"stopReason":"end_turn"})),
                (
                    "metadata",
                    json!({"usage":{"inputTokens":3,"outputTokens":1}}),
                ),
            ]),
        )
    }

    #[test]
    fn reasoning_goes_back_where_claude_wrote_it() {
        let sign = |text: &str, signature: &str| {
            [
                (
                    "contentBlockDelta",
                    json!({"delta":{"reasoningContent":{"text":text}}}),
                ),
                (
                    "contentBlockDelta",
                    json!({"delta":{"reasoningContent":{"signature":signature}}}),
                ),
                ("contentBlockStop", json!({})),
            ]
        };
        let call = |id: &str| {
            [
                (
                    "contentBlockStart",
                    json!({"start":{"toolUse":{"toolUseId":id,"name":"read"}}}),
                ),
                (
                    "contentBlockDelta",
                    json!({"delta":{"toolUse":{"input":"{}"}}}),
                ),
                ("contentBlockStop", json!({})),
            ]
        };
        let words = [
            (
                "contentBlockDelta",
                json!({"delta":{"text":"Let me look."}}),
            ),
            ("contentBlockStop", json!({})),
        ];
        let stop = [("messageStop", json!({"stopReason":"tool_use"}))];
        let calls = |events: Vec<(&str, Value)>| -> Vec<ProviderMessage> {
            mapped(&events)
                .unwrap()
                .into_iter()
                .filter_map(|event| match event {
                    ProviderEvent::ToolCall {
                        call_id,
                        name,
                        arguments,
                        provider_metadata,
                    } => Some(ProviderMessage::ToolCall {
                        call_id,
                        name,
                        arguments,
                        provider_metadata,
                    }),
                    _ => None,
                })
                .collect()
        };
        let turn = |history: Vec<ProviderMessage>| -> Vec<String> {
            let mut messages = vec![ProviderMessage::Text {
                role: "user".into(),
                content: "read it".into(),
            }];
            messages.extend(history);
            let body = bedrock_request(ProviderRequest {
                model: "global.anthropic.claude-sonnet-5-5".into(),
                system: "system".into(),
                messages,
                tools: Vec::new(),
                reasoning_effort: None,
                parameters: haarness_core::RequestParameters::default(),
                output_schema: None,
            })
            .unwrap();
            body["messages"][1]["content"]
                .as_array()
                .unwrap()
                .iter()
                .map(|block| {
                    if let Some(signature) =
                        block.pointer("/reasoningContent/reasoningText/signature")
                    {
                        format!("reasoning {}", signature.as_str().unwrap())
                    } else if let Some(text) = block.get("text") {
                        format!("words {}", text.as_str().unwrap())
                    } else {
                        format!("call {}", block["toolUse"]["toolUseId"].as_str().unwrap())
                    }
                })
                .collect()
        };
        let said = || ProviderMessage::Text {
            role: "assistant".into(),
            content: "Let me look.".into(),
        };

        // Claude 5.5 often writes a few words, then thinks, then calls: the
        // thinking goes back after the words, not ahead of them.
        let mut history = vec![said()];
        history.extend(calls(
            [
                &words[..],
                &sign("plan", "s1")[..],
                &call("a")[..],
                &stop[..],
            ]
            .concat(),
        ));
        assert_eq!(
            turn(history),
            ["words Let me look.", "reasoning s1", "call a"]
        );

        // Thinking first, then words, then the call.
        let mut history = vec![said()];
        history.extend(calls(
            [
                &sign("plan", "s1")[..],
                &words[..],
                &call("a")[..],
                &stop[..],
            ]
            .concat(),
        ));
        assert_eq!(
            turn(history),
            ["reasoning s1", "words Let me look.", "call a"]
        );

        // Thinking, words, more thinking, the call.
        let mut history = vec![said()];
        history.extend(calls(
            [
                &sign("plan", "s1")[..],
                &words[..],
                &sign("more", "s2")[..],
                &call("a")[..],
                &stop[..],
            ]
            .concat(),
        ));
        assert_eq!(
            turn(history),
            [
                "reasoning s1",
                "words Let me look.",
                "reasoning s2",
                "call a"
            ]
        );

        // Thinking between two calls of one answer stays between them: two
        // reasoning blocks side by side are refused as modified.
        let history = calls(
            [
                &sign("first", "s1")[..],
                &call("a")[..],
                &sign("second", "s2")[..],
                &call("b")[..],
                &stop[..],
            ]
            .concat(),
        );
        assert_eq!(
            turn(history),
            ["reasoning s1", "call a", "reasoning s2", "call b"]
        );

        // A call saved before Haarness kept the place has its reasoning first.
        let old = ProviderMessage::ToolCall {
            call_id: "a".into(),
            name: "read".into(),
            arguments: json!({}),
            provider_metadata: json!({"bedrock_reasoning":[{"reasoningContent":{"reasoningText":{"text":"plan","signature":"s1"}}}]}),
        };
        assert_eq!(
            turn(vec![said(), old]),
            ["reasoning s1", "words Let me look.", "call a"]
        );
    }

    #[test]
    fn claude_5_5_asks_bedrock_to_drop_thinking_bound_to_an_older_prompt() {
        let extra = |model: &str| {
            bedrock_request(ask(model, true)).unwrap()["additionalModelRequestFields"].clone()
        };
        for model in [
            "global.anthropic.claude-sonnet-5-5",
            "us.anthropic.claude-haiku-5-5",
            "global.anthropic.claude-fable-5-1",
        ] {
            let fields = extra(model);
            assert_eq!(fields["thinking"]["type"], "adaptive", "{model}");
            assert_eq!(
                fields["thinking"]["block_binding"]["prefix_mismatch_behavior"], "drop_block",
                "{model}"
            );
            assert_eq!(
                fields["anthropic_beta"],
                json!(["thinking-binding-controls-2026-08-01"]),
                "{model}"
            );
        }
        // Older Claude and other models bind nothing.
        for model in [
            "global.anthropic.claude-sonnet-4-6",
            "global.anthropic.claude-sonnet-5",
            "amazon.nova-pro-v1:0",
            "openai.gpt-oss-120b-1:0",
        ] {
            assert!(extra(model).get("thinking").is_none(), "{model}");
        }
        // Haiku 5.5 still takes the forced structured answer it took.
        let mut structured = ask("global.anthropic.claude-haiku-5-5", false);
        structured.output_schema = Some(json!({"type":"object"}));
        assert!(
            bedrock_request(structured).unwrap()["toolConfig"]
                .get("toolChoice")
                .is_some()
        );
    }

    #[test]
    fn requests_stay_within_the_models_output_ceiling_and_skip_a_blank_system() {
        let controls = ModelControls {
            max_output_tokens: Some(4096),
            ..ModelControls::default()
        };
        let mut request = ask("writer.palmyra-vision-7b", false);
        request.system = "  ".into();
        let body = super::bedrock_request(request, &controls, None).unwrap();
        assert_eq!(body["inferenceConfig"]["maxTokens"], 4096);
        assert!(body.get("system").is_none(), "{body}");
        let mut configured = ask("writer.palmyra-vision-7b", false);
        configured.parameters.max_output_tokens = Some(32_000);
        assert_eq!(
            super::bedrock_request(configured, &controls, None).unwrap()["inferenceConfig"]["maxTokens"],
            4096
        );
    }

    #[test]
    fn redacted_reasoning_sent_in_pieces_goes_back_whole() {
        use base64::Engine as _;
        let engine = base64::engine::general_purpose::STANDARD;
        let events = mapped(&[
            (
                "contentBlockDelta",
                json!({"delta":{"reasoningContent":{"redactedContent":engine.encode(b"abc")}}}),
            ),
            (
                "contentBlockDelta",
                json!({"delta":{"reasoningContent":{"redactedContent":engine.encode(b"def")}}}),
            ),
            ("contentBlockStop", json!({})),
            (
                "contentBlockStart",
                json!({"start":{"toolUse":{"toolUseId":"a","name":"read"}}}),
            ),
            ("contentBlockStop", json!({})),
            ("messageStop", json!({"stopReason":"tool_use"})),
        ])
        .unwrap();
        let metadata = events
            .iter()
            .find_map(|event| match event {
                ProviderEvent::ToolCall {
                    provider_metadata, ..
                } => Some(provider_metadata.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            metadata["bedrock_reasoning"][0]["reasoningContent"]["redactedContent"],
            engine.encode(b"abcdef")
        );
    }

    #[test]
    fn a_model_that_takes_no_tools_says_what_to_do() {
        assert_eq!(
            http_error(
                reqwest::StatusCode::BAD_REQUEST,
                Some("ValidationException"),
                r#"{"message":"This model doesn't support tool use."}"#
            )
            .to_string(),
            "provider error: HTTP 400 Bad Request: [ValidationException] This model can't use tools, which Haarness needs to read and change your files. Pick a model that can, such as Claude, Nova or gpt-oss."
        );
    }

    #[tokio::test]
    async fn a_model_that_refuses_streamed_tools_is_answered_whole() {
        let answer: Answer = Arc::new(|path, body| {
            if path.ends_with("/converse-stream") {
                if body.get("toolConfig").is_some() {
                    return refusal("This model doesn't support tool use in streaming mode.");
                }
                return streamed_text("hello");
            }
            (
                200,
                json!({
                    "output":{"message":{"role":"assistant","content":[
                        {"text":"Listing."},
                        {"toolUse":{"toolUseId":"tooluse_1","name":"list","input":{"path":"."}}}
                    ]}},
                    "stopReason":"tool_use",
                    "usage":{"inputTokens":10,"outputTokens":4,"totalTokens":14},
                    "metrics":{"latencyMs":10}
                })
                .to_string()
                .into_bytes(),
            )
        });
        let (base, seen) = scripted_bedrock(answer).await;
        let provider = scripted_provider(
            base,
            &["acme.chat-v1:0", "us.meta.llama3-3-70b-instruct-v1:0"],
        );
        let events = answer_of(&provider, ask("acme.chat-v1:0", true))
            .await
            .unwrap();
        assert!(
            matches!(&events[0], ProviderEvent::TextDelta(text) if text == "Listing."),
            "{events:?}"
        );
        assert!(
            events.iter().any(|event| matches!(
                event,
                ProviderEvent::ToolCall { call_id, name, arguments, .. }
                    if call_id == "tooluse_1" && name == "list" && arguments == &json!({"path":"."})
            )),
            "{events:?}"
        );
        assert!(events.iter().any(|event| matches!(
            event,
            ProviderEvent::Usage {
                input_tokens: 10,
                output_tokens: 4,
                ..
            }
        )));
        assert!(
            matches!(events.last(), Some(ProviderEvent::Finished { reason }) if reason == "tool_use")
        );
        // Learned: the next request with tools goes whole at once; one
        // without tools still streams.
        answer_of(&provider, ask("acme.chat-v1:0", true))
            .await
            .unwrap();
        answer_of(&provider, ask("acme.chat-v1:0", false))
            .await
            .unwrap();
        // Llama 3.3, which AWS lists so, goes whole from the first request.
        answer_of(&provider, ask("us.meta.llama3-3-70b-instruct-v1:0", true))
            .await
            .unwrap();
        let paths: Vec<String> = seen.lock().iter().map(|(path, _)| path.clone()).collect();
        assert_eq!(
            paths,
            [
                "/model/acme.chat-v1:0/converse-stream",
                "/model/acme.chat-v1:0/converse",
                "/model/acme.chat-v1:0/converse",
                "/model/acme.chat-v1:0/converse-stream",
                "/model/us.meta.llama3-3-70b-instruct-v1:0/converse",
            ]
        );
    }

    #[tokio::test]
    async fn a_model_named_by_its_own_id_is_asked_through_its_inference_profile() {
        let answer: Answer = Arc::new(|path, _| {
            if path.starts_with("/model/acme.") {
                return refusal(
                    "Invocation of model ID acme.chat-v1:0 with on-demand throughput isn\u{2019}t supported. Retry your request with the ID or ARN of an inference profile that contains this model.",
                );
            }
            streamed_text("hello")
        });
        let (base, seen) = scripted_bedrock(answer).await;
        let provider = scripted_provider(base, &["acme.chat-v1:0"]);
        for _ in 0..2 {
            let events = answer_of(&provider, ask("acme.chat-v1:0", false))
                .await
                .unwrap();
            assert!(matches!(&events[0], ProviderEvent::TextDelta(text) if text == "hello"));
        }
        let paths: Vec<String> = seen.lock().iter().map(|(path, _)| path.clone()).collect();
        assert_eq!(
            paths,
            [
                "/model/acme.chat-v1:0/converse-stream",
                "/model/us.acme.chat-v1:0/converse-stream",
                "/model/us.acme.chat-v1:0/converse-stream",
            ]
        );
        // Discovery names the profile up front for the models it lists.
        let (_, _, aliases) = discovered();
        assert_eq!(
            aliases
                .get("anthropic.claude-sonnet-5-5")
                .map(String::as_str),
            Some("global.anthropic.claude-sonnet-5-5")
        );
        assert!(!aliases.contains_key("amazon.nova-micro-v1:0"));
        // A guessed profile that does not exist leaves Bedrock's first words.
        let answer: Answer = Arc::new(|path, _| {
            if path.starts_with("/model/acme.") {
                return refusal(
                    "Invocation of model ID acme.chat-v1:0 with on-demand throughput isn\u{2019}t supported. Retry your request with the ID or ARN of an inference profile that contains this model.",
                );
            }
            refusal("The provided model identifier is invalid.")
        });
        let (base, _) = scripted_bedrock(answer).await;
        let provider = scripted_provider(base, &["acme.chat-v1:0"]);
        let (logged, _guard) = crate::log_capture::Logged::start();
        let error = answer_of(&provider, ask("acme.chat-v1:0", false))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("on-demand throughput"), "{error}");
        // The log names the request whose words the person reads: the first.
        let lines = logged.lines("provider refused the request");
        assert_eq!(lines.len(), 1, "{}", logged.all());
        assert!(
            lines[0].contains(" status=400") && lines[0].contains(" request_id=bedrock-step-1"),
            "{}",
            lines[0]
        );
    }

    #[tokio::test]
    async fn a_forced_tool_choice_the_model_refuses_is_asked_unforced() {
        let answer: Answer = Arc::new(|_, body| {
            if body.pointer("/toolConfig/toolChoice").is_some() {
                return refusal(
                    "This model doesn't support the toolConfig.toolChoice.tool field. Remove toolConfig.toolChoice.tool and try again.",
                );
            }
            streamed_text("{}")
        });
        let (base, seen) = scripted_bedrock(answer).await;
        let provider = scripted_provider(base, &["openai.gpt-oss-120b-1:0"]);
        let structured = || {
            let mut request = ask("openai.gpt-oss-120b-1:0", false);
            request.output_schema = Some(json!({"type":"object"}));
            request
        };
        answer_of(&provider, structured()).await.unwrap();
        answer_of(&provider, structured()).await.unwrap();
        let forced: Vec<bool> = seen
            .lock()
            .iter()
            .map(|(_, body)| body.pointer("/toolConfig/toolChoice").is_some())
            .collect();
        assert_eq!(forced, [true, false, false]);
    }

    #[tokio::test]
    async fn reasoning_bedrock_refuses_is_left_out_once_instead_of_failing_the_turn() {
        // Bedrock's words for a block whose prompt changed, as it answered
        // Claude Opus 5.5 asked not to drop such blocks.
        let answer: Answer = Arc::new(|_, body| {
            if body.to_string().contains("reasoningContent") {
                return refusal(
                    "The model returned the following errors: messages.1.content.0: Invalid `signature` in `thinking` block. The block is bound to a different conversation. Remove the block, or set `thinking.block_binding.prefix_mismatch_behavior` to \"drop_block\". The `system` prompt differs from the one this block was created with.",
                );
            }
            streamed_text("done")
        });
        let (base, seen) = scripted_bedrock(answer).await;
        let provider = scripted_provider(base, &["global.anthropic.claude-opus-5-5"]);
        let mut request = ask("global.anthropic.claude-opus-5-5", true);
        request.messages.extend([
            ProviderMessage::ToolCall {
                call_id: "a".into(),
                name: "list".into(),
                arguments: json!({}),
                provider_metadata: json!({"bedrock_reasoning":[{"reasoningContent":{"reasoningText":{"text":"plan","signature":"s1"}}}]}),
            },
            ProviderMessage::ToolResult {
                call_id: "a".into(),
                name: "list".into(),
                output: "a.txt".into(),
                is_error: false,
                attachments: Vec::new(),
            },
        ]);
        let events = answer_of(&provider, request).await.unwrap();
        assert!(matches!(&events[0], ProviderEvent::TextDelta(text) if text == "done"));
        let bodies = seen.lock();
        assert_eq!(bodies.len(), 2);
        assert!(
            bodies[1]
                .1
                .get("additionalModelRequestFields")
                .is_none_or(|fields| fields.get("thinking").is_none())
        );
        assert_eq!(
            bodies[1].1["messages"][1]["content"][0]["toolUse"]["toolUseId"],
            "a"
        );
    }

    #[tokio::test]
    async fn requests_sign_with_the_aws_sign_in_when_there_is_no_api_key() {
        let answer: Answer = Arc::new(|_, _| streamed_text("hi"));
        let (base, _) = scripted_bedrock(answer).await;
        let mut provider = scripted_provider(base.clone(), &["test-model"]);
        provider.bearer = None;
        provider.sign_in = Some(AwsSignIn::fixed(AwsCredentials {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: SecretString::from("secret"),
            session_token: None,
        }));
        assert!(provider.authenticated());
        let signer = provider.signer().await.unwrap();
        let mut headers = reqwest::header::HeaderMap::new();
        provider
            .authorize(
                &signer,
                "POST",
                &format!("{base}/model/test-model/converse-stream"),
                b"{}",
                &mut headers,
            )
            .unwrap();
        assert!(
            headers[reqwest::header::AUTHORIZATION]
                .to_str()
                .unwrap()
                .starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/")
        );
        answer_of(&provider, ask("test-model", false))
            .await
            .unwrap();
    }

    #[test]
    fn media_converse_cannot_take_is_a_note_except_in_the_newest_message() {
        let audio = || "data:audio/mpeg;name=meeting.mp3;base64,SUQz".to_owned();
        let shot = || "data:image/png;name=shot.png;base64,iVBORw0KGgo=".to_owned();
        let text = |role: &str, content: &str| ProviderMessage::Text {
            role: role.into(),
            content: content.into(),
        };
        let chat = |newest: Option<ProviderMessage>| {
            let mut messages = vec![
                ProviderMessage::Media {
                    role: "user".into(),
                    content: "What does it say?".into(),
                    attachments: vec![audio()],
                },
                text("assistant", "I can't hear recordings."),
                text("user", "Record the demo"),
                ProviderMessage::ToolCall {
                    call_id: "record".into(),
                    name: "record".into(),
                    arguments: json!({}),
                    provider_metadata: Value::Null,
                },
                ProviderMessage::ToolResult {
                    call_id: "record".into(),
                    name: "record".into(),
                    output: String::new(),
                    is_error: false,
                    attachments: vec![audio(), shot()],
                },
            ];
            messages.extend(newest);
            ProviderRequest {
                model: "openai.gpt-oss-120b-1:0".into(),
                system: "test".into(),
                messages,
                tools: Vec::new(),
                reasoning_effort: None,
                parameters: haarness_core::RequestParameters::default(),
                output_schema: None,
            }
        };
        let newest = |attachment: String| {
            Some(ProviderMessage::Media {
                role: "user".into(),
                content: "And this?".into(),
                attachments: vec![attachment],
            })
        };

        // A model not listed yet reads the screenshot; the recordings are
        // notes, and the tool result is not empty.
        let body = bedrock_request(chat(None)).unwrap();
        let sent = body["messages"].to_string();
        assert_eq!(
            sent.matches("[Audio meeting.mp3 is left out").count(),
            2,
            "{sent}"
        );
        assert!(!sent.contains("SUQz"), "{sent}");
        assert!(
            body["messages"][4]["content"][0]["toolResult"]["content"][1]
                .get("image")
                .is_some(),
            "{sent}"
        );

        // Bedrock lists gpt-oss without image input: its screenshot is a
        // note too, and the newest message is refused by the model's name.
        let gpt_oss = model_info(
            "openai.gpt-oss-120b-1:0",
            Some("gpt-oss-120b"),
            &ModelControls::default(),
            &CompatibleModelDefaults::default(),
            false,
        );
        let listed =
            |request| super::bedrock_request(request, &ModelControls::default(), Some(&gpt_oss));
        let sent = listed(chat(None)).unwrap()["messages"].to_string();
        assert!(
            sent.contains("[Image shot.png is not shown to you"),
            "{sent}"
        );
        assert!(!sent.contains("iVBORw0KGgo="), "{sent}");
        let name = &gpt_oss.display_name;
        assert_eq!(
            listed(chat(newest(audio()))).unwrap_err().to_string(),
            format!(
                "provider error: Haarness can't send audio to {name}. Send the message without the recording, or switch to a model that takes audio."
            )
        );
        assert_eq!(
            listed(chat(newest(shot()))).unwrap_err().to_string(),
            format!(
                "provider error: {name} doesn't read images. Send the message without the picture, or switch to a model that reads them."
            )
        );
    }
}
