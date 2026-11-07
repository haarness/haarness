//! Managed models use a local account session, never a saved upstream API key.
use crate::{
    CompatibleInstance, CompatibleModelDefaults, OpenAiCompatibleProvider, ProviderHeaders,
    openai_compatible::ListedAudio,
};
use async_trait::async_trait;
use haarness_account::{Client, Config, RouterSession};
use haarness_core::{CoreError, CoreResult, ModelProvider, ProviderRequest, ProviderStream};
use haarness_protocol::ProviderInfo;
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// The cached model-access grant, with the sign-in, account and router it
/// was issued for.
type Grant = Mutex<Option<(String, Option<String>, Option<String>, RouterSession)>>;

pub struct HaarnessCloudProvider {
    client: Client,
    grant: Arc<Grant>,
    models: RwLock<Option<([u8; 32], Arc<crate::live_models::LiveModels>)>>,
    /// Which listed models take audio, kept across the transports made for
    /// each request.
    listed_audio: ListedAudio,
    /// How long a busy refusal is held back ([`random_busy_jitter`]).
    busy_jitter: fn() -> Duration,
}
impl HaarnessCloudProvider {
    pub fn from_environment() -> Option<Self> {
        Some(Self {
            client: Client::new(Config::from_env().ok()?).ok()?,
            grant: Arc::default(),
            models: RwLock::new(None),
            listed_audio: ListedAudio::default(),
            busy_jitter: random_busy_jitter,
        })
    }
    fn identity(&self) -> Option<[u8; 32]> {
        if let Some(key) = self.client.config.api_key() {
            return Some(
                Sha256::digest(
                    serde_json::to_vec(&(
                        self.client.config.api_url.as_str(),
                        key,
                        self.client.config.selected_router().ok()?,
                    ))
                    .ok()?,
                )
                .into(),
            );
        }
        let credentials =
            haarness_account::credentials::load(&self.client.config.path()).ok()??;
        if credentials.api_url != self.client.config.api_url.as_str() {
            return None;
        }
        // Be conservative across sign-in changes, including opaque tokens. A renewed
        // credential gets a fresh catalog; an outage with the same identity keeps it.
        Some(
            Sha256::digest(
                serde_json::to_vec(&(
                    credentials.api_url,
                    credentials.access_token,
                    self.client.config.selected_account(),
                    self.client.config.selected_router().ok()?,
                ))
                .ok()?,
            )
            .into(),
        )
    }

    fn catalog(&self) -> Option<([u8; 32], Arc<crate::live_models::LiveModels>)> {
        let identity = self.identity();
        let mut catalog = self
            .models
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(identity) = identity else {
            *catalog = None;
            return None;
        };
        if catalog
            .as_ref()
            .is_none_or(|(previous, _)| *previous != identity)
        {
            *catalog = Some((
                identity,
                Arc::new(crate::live_models::LiveModels::default()),
            ));
        }
        catalog.clone()
    }
    async fn transport(&self) -> CoreResult<OpenAiCompatibleProvider> {
        Ok(router_transport(&self.client, &self.grant)
            .await?
            .with_listed_audio(Arc::clone(&self.listed_audio)))
    }
}

async fn router_transport(client: &Client, grant: &Grant) -> CoreResult<OpenAiCompatibleProvider> {
    let bearer = client.bearer(None).await.map_err(error)?;
    let account = client.config.selected_account();
    let router = client.config.selected_router().map_err(error)?;
    let mut grant = grant.lock().await;
    if grant
        .as_ref()
        .is_none_or(|(token, selected, selected_router, g)| {
            token != &bearer
                || selected != &account
                || selected_router != &router
                || g.expires_at <= chrono::Utc::now() + chrono::Duration::seconds(30)
        })
    {
        *grant = Some((
            bearer,
            account,
            router,
            client.router_session().await.map_err(error)?,
        ));
    }
    let g = &grant.as_ref().expect("grant initialized").3;
    haarness_account::safe_url(&g.router_url).map_err(error)?;
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(error)?;
    Ok(OpenAiCompatibleProvider::new(CompatibleInstance {
        id: "haarness".into(),
        name: "Haarness Cloud".into(),
        base_url: g.router_url.clone(),
        api_key: Some(g.token.clone()),
        models: Vec::new(),
        discover_models: true,
        server_output_limit: false,
        replay_observed_reasoning: false,
        model_defaults: CompatibleModelDefaults::default(),
        assume_authenticated: false,
        headers: ProviderHeaders::default(),
    })
    .with_http_client(http)
    .with_server_output_limit())
}

/// The router's answer to a grant it no longer accepts.
fn unauthorized(error: &CoreError) -> bool {
    matches!(error, CoreError::Provider(message) if message.starts_with("HTTP 401"))
}

/// The router's refusals for a busy key or router (see [`readable_with`]).
fn busy(error: &CoreError) -> bool {
    matches!(error, CoreError::Provider(message) if
        ["[concurrency_limited]", "[router_overloaded]", "[rate_limited]"]
            .iter()
            .any(|code| message.contains(code)))
}

/// The most a busy refusal is held back, in milliseconds, so that agents
/// refused together spread out their next tries.
const BUSY_JITTER_MS: u64 = 1_000;

/// A random part of [`BUSY_JITTER_MS`].
fn random_busy_jitter() -> Duration {
    Duration::from_millis(rand::random_range(0..BUSY_JITTER_MS))
}

fn error(e: impl std::fmt::Display) -> CoreError {
    CoreError::Provider(e.to_string())
}

/// Say what an exhausted Cloud balance means for the person reading it. The
/// router's 402 reads `[insufficient_credits] insufficient credits: unit
/// Nanos requested …`, which is accounting, not advice.
fn readable(error: CoreError) -> CoreError {
    readable_with(error, " (in the terminal: /billing)")
}

/// [`readable`] for a Cloud workspace's own `router` provider, whose chat is
/// read in a browser or on a phone as often as in a terminal.
pub(crate) fn readable_router(error: CoreError) -> CoreError {
    readable_with(error, "")
}

fn readable_with(error: CoreError, terminal: &str) -> CoreError {
    let CoreError::Provider(message) = error else {
        return error;
    };
    let advice = if message.contains("[insufficient_credits]") {
        if message.contains("overage opt-in false") {
            // Purchased credits only pay once overage is on, and buying
            // more does not turn it on. With overage off the router names
            // the purchased balance as what is available.
            if purchased_available(&message) {
                format!(
                    "Your included Haarness Cloud credits can't cover this request. Turn on \
                     overage in Billing so your purchased credits pay{terminal}."
                )
            } else {
                format!(
                    "Your included Haarness Cloud credits can't cover this request. Add credits \
                     and turn on overage in Billing, or choose a plan{terminal}."
                )
            }
        } else if message.contains("frozen") {
            format!(
                "Your purchased Haarness Cloud credits are on hold. Open Billing in Haarness \
                 Cloud for details{terminal}."
            )
        } else {
            format!(
                "Your Haarness Cloud credits can't cover this request. Add credits or choose a \
                 plan under Billing{terminal}."
            )
        }
    } else if message.contains("[upstream_404]") {
        // The provider stopped serving a model it still lists (a deprecation); Cloud withdraws such models at its
        // next catalogue refresh.
        "The provider no longer serves this model. Choose another model.".to_owned()
    } else if message.contains("[upstream_429]") {
        // The router names the wait when the provider did ("Try again in 12
        // seconds."), and its Retry-After arrives as "Please retry in 12s.";
        // either is kept in the words the runtime waits on.
        match advised_seconds(&message) {
            Some(seconds) => format!(
                "The provider is rate limiting this model right now. Please retry in {seconds}s, or choose another model."
            ),
            None => "The provider is rate limiting this model right now. Try again in a moment, or choose another model."
                .to_owned(),
        }
    } else if message.contains("[upstream_413]") {
        // A request over the plan's tokens a minute: the router's sentence
        // already says why and what helps, and waiting does not.
        message
            .split_once("[upstream_413]")
            .map_or_else(|| message.clone(), |(_, rest)| rest.trim().to_owned())
    } else if [
        "[upstream_500]",
        "[upstream_502]",
        "[upstream_503]",
        "[upstream_529]",
    ]
    .iter()
    .any(|code| message.contains(code))
    {
        "The provider is overloaded or unavailable right now. Try again in a moment, or choose another model."
            .to_owned()
    } else if message.contains("[payment_required]") {
        // The router refuses your own provider keys without a plan or trial; nothing is
        // charged for those requests, but the account needs one to route them.
        format!(
            "Routing your own provider keys through Haarness Cloud needs a plan or the free \
             trial. Start the trial or choose a plan under Billing{terminal}."
        )
    } else if message.contains("[budget_exceeded]") {
        // The limit is checked against the request's worst case: a long
        // context and a large answer limit can exceed it on their own.
        // Account and project budgets live in Billing, not on a key.
        let limit =
            if message.contains("the account budget") || message.contains("the project budget") {
                "raise the budget under Billing in Haarness Cloud"
            } else {
                "raise the limit on the API key in Haarness Cloud"
            };
        format!(
            "This request could cost more than a spending limit allows. Compact or shorten the \
             conversation, or {limit}."
        )
    } else if message.contains("[concurrency_limited]") {
        // A sign-in runs eight requests at once (a fan-out of agents can ask
        // for more). A slot frees when one of them finishes, which no wait
        // foretells, so the turn backs off from 2 to 16 seconds on "Try again
        // in a moment" whatever wait the router names: that tries sooner and
        // waits longer than a fixed one. "rate limit" makes it a usage limit.
        "Haarness Cloud is already running as many of your requests at once as its rate limit \
         allows. Try again in a moment, or once another chat or agent finishes."
            .to_owned()
    } else if message.contains("[router_overloaded]") {
        // The router's own limit on requests at once, across every account:
        // the wait it names spares a router that is already full.
        let next = advised_seconds(&message).map_or_else(
            || "Try again in a moment.".to_owned(),
            |seconds| format!("Please retry in {seconds}s."),
        );
        format!("Haarness Cloud's router is at its rate limit for requests at once. {next}")
    } else if message.contains("[rate_limited]") {
        // Requests a minute: the router says when the minute turns ("Please
        // retry in 12s."), and that wait is kept.
        let limit = message
            .split_once("rate limit of ")
            .map(|(_, rest)| {
                rest.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
            })
            .filter(|digits| !digits.is_empty())
            .map_or_else(|| " for".to_owned(), |digits| format!(" of {digits}"));
        let next = advised_seconds(&message).map_or_else(
            || "Try again in a moment.".to_owned(),
            |seconds| format!("Please retry in {seconds}s."),
        );
        format!("You've reached Haarness Cloud's rate limit{limit} requests a minute. {next}")
    } else if message.contains("[exposure_cap]") {
        // The router's words name only the cap, not whether this request
        // alone could cost more than it, and Cloud before Retry-After sent
        // none on any of these: the advice covers both. "usage limit" lets the
        // words read on their own, and nothing in them asks the turn to retry.
        "Too many requests are still settling on this Haarness Cloud account, past its usage \
         limit for requests in flight. Wait a moment, then try again; if nothing else is running, \
         lower the output limit or shorten the conversation."
            .to_owned()
    } else if message.contains("[account_suspended]") {
        format!(
            "This Haarness Cloud account is suspended. Open Billing in Haarness Cloud for \
             details{terminal}."
        )
    } else {
        return CoreError::Provider(message);
    };
    CoreError::Provider(advice)
}
/// The wait a rate-limited answer names, in whole seconds: the router's
/// "Try again in 12 seconds." or a Retry-After passed on as "retry in 12s".
fn advised_seconds(message: &str) -> Option<u64> {
    let lower = message.to_ascii_lowercase();
    ["retry in ", "try again in "].iter().find_map(|marker| {
        lower.split(marker).skip(1).find_map(|rest| {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let unit = rest[digits.len()..].trim_start();
            (!digits.is_empty() && (unit.starts_with('s')))
                .then(|| digits.parse().ok())
                .flatten()
        })
    })
}

/// Whether a refused request's `available` names credits: with overage off
/// the router reports the purchased balance there.
fn purchased_available(message: &str) -> bool {
    message
        .split_once("available ")
        .map(|(_, rest)| {
            rest.bytes()
                .take_while(u8::is_ascii_digit)
                .collect::<Vec<_>>()
        })
        .is_some_and(|digits| digits.iter().any(|digit| *digit != b'0'))
}

#[async_trait]
impl ModelProvider for HaarnessCloudProvider {
    fn id(&self) -> &str {
        "haarness"
    }
    /// The router transport marks an Anthropic model's prompt for caching
    /// (`OpenAiCompatibleProvider::marks_prompt_cache`). The scope is the
    /// signed-in identity's digest.
    fn explicit_prompt_cache_scope(&self, model: &str) -> Option<String> {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        model
            .starts_with("anthropic/")
            .then(|| self.identity())
            .flatten()
            .map(|identity| URL_SAFE_NO_PAD.encode(identity))
    }
    /// The router passes an Anthropic model's request on to Anthropic,
    /// whose limits apply. It takes 32 MiB in a request itself.
    fn image_limits(&self, model: &str) -> haarness_core::ImageLimits {
        if model.starts_with("anthropic/") {
            crate::anthropic::image_limits(&self.info().models, model)
        } else {
            haarness_core::ImageLimits {
                request: Some(32 * 1024 * 1024 - haarness_core::REQUEST_ALLOWANCE),
                ..haarness_core::ImageLimits::default()
            }
        }
    }
    fn info(&self) -> ProviderInfo {
        let signed_in = self.client.config.signed_in();
        ProviderInfo {
            id: self.id().into(),
            name: "Haarness Cloud".into(),
            authenticated: signed_in,
            models: self
                .catalog()
                .map_or_else(Vec::new, |(_, catalog)| catalog.get(&[])),
        }
    }
    async fn refresh_models(&self, force: bool) {
        let Some((identity, catalog)) = self.catalog() else {
            return;
        };
        catalog
            .refresh(force, || async {
                let provider = self.transport().await.ok()?;
                let models = provider.fetch_models().await?;
                (self.identity() == Some(identity)).then_some(models)
            })
            .await;
        // Token renewal can change identity while acquiring the router grant. The
        // old request must not populate a new account's cache; refresh that cache now.
        if self.identity().is_some_and(|current| current != identity) {
            if let Some((identity, catalog)) = self.catalog() {
                catalog
                    .refresh(force, || async {
                        let provider = self.transport().await.ok()?;
                        let models = provider.fetch_models().await?;
                        (self.identity() == Some(identity)).then_some(models)
                    })
                    .await;
            }
        }
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        let transport = tokio::select! { () = cancel.cancelled() => return Err(error("Request cancelled")), result = self.transport() => result? };
        // A turn can come before anything listed the models (the first one
        // after start-up). Without the catalog every model would look
        // text-only, so an image in the conversation would fail the turn.
        if self
            .catalog()
            .is_some_and(|(_, catalog)| catalog.get(&[]).is_empty())
        {
            tokio::select! {
                () = cancel.cancelled() => return Err(error("Request cancelled")),
                () = self.refresh_models(false) => {}
            }
        }
        let models = self
            .catalog()
            .map_or_else(Vec::new, |(_, catalog)| catalog.get(&[]));
        let mut stream = transport
            .with_model_info(models.clone())
            .stream(request.clone(), cancel.clone())
            .await
            .map_err(readable)?;
        let client = self.client.clone();
        let grant = Arc::clone(&self.grant);
        let listed_audio = Arc::clone(&self.listed_audio);
        let busy_jitter = self.busy_jitter;
        let used = grant
            .lock()
            .await
            .as_ref()
            .map(|(.., session)| session.token.clone());
        // Cloud can withdraw a grant before it expires (a sign-out elsewhere,
        // a changed plan), and the router then answers 401 to every request
        // until the cached copy ages out. Fetch a new grant and ask once more.
        let mut retry = Some(request);
        Ok(Box::pin(async_stream::try_stream! {
            while let Some(event) = futures::StreamExt::next(&mut stream).await {
                match event {
                    Err(failure) if retry.is_some() && unauthorized(&failure) => {
                        let request = retry.take().expect("checked above");
                        {
                            // Another request may already have replaced it.
                            let mut cached = grant.lock().await;
                            if cached.as_ref().map(|(.., session)| &session.token) == used.as_ref() {
                                *cached = None;
                            }
                        }
                        let transport = tokio::select! {
                            () = cancel.cancelled() => Err(CoreError::Cancelled),
                            result = router_transport(&client, &grant) => result,
                        }?;
                        stream = transport
                            .with_model_info(models.clone())
                            .with_listed_audio(Arc::clone(&listed_audio))
                            .stream(request, cancel.clone())
                            .await
                            .map_err(readable)?;
                    }
                    Err(failure) if busy(&failure) => {
                        retry = None;
                        // Agents refused together would all try again at the
                        // same moment, and be refused together again: each
                        // waits a random part of a second more first.
                        tokio::select! {
                            () = cancel.cancelled() => {}
                            () = tokio::time::sleep(busy_jitter()) => {}
                        }
                        Err::<(), _>(readable(failure))?;
                    }
                    event => {
                        retry = None;
                        yield event.map_err(readable)?;
                    }
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::StatusCode,
        response::IntoResponse,
        routing::{get, post},
    };
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone)]
    struct Fixture {
        url: String,
        mode: Arc<AtomicUsize>,
        calls: Arc<AtomicUsize>,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    async fn models(State(state): State<Fixture>) -> axum::response::Response {
        state.calls.fetch_add(1, Ordering::SeqCst);
        let mode = state.mode.load(Ordering::SeqCst);
        if mode == 1 {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        if mode == 3 {
            state.entered.notify_one();
            state.release.notified().await;
        }
        Json(json!({"data": if mode == 2 { vec![] } else { vec![json!({"id":"fixture-chat"})] }}))
            .into_response()
    }

    #[tokio::test]
    async fn cloud_catalog_survives_outages_and_fences_account_changes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let fixture = Fixture {
            url: url.clone(),
            mode: Arc::new(AtomicUsize::new(0)),
            calls: Arc::new(AtomicUsize::new(0)),
            entered: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
        };
        let app = Router::new().route("/models", get(models)).route("/api/v1/account/router-session", post(|State(state): State<Fixture>| async move {
            Json(json!({"token":"fixture-router", "routerUrl":state.url, "expiresAt":chrono::Utc::now() + chrono::Duration::hours(1)}))
        })).with_state(fixture.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let root = std::env::temp_dir().join(format!("haarness-cloud-catalog-{}", uuid_for_test()));
        let config = Config::new(&url, root.clone(), "fixture").unwrap();
        let mut credentials = haarness_account::Credentials {
            api_url: url.clone(),
            access_token: "first-login".into(),
            refresh_token: "fixture".into(),
            access_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        };
        haarness_account::credentials::save(&config.path(), &credentials).unwrap();
        let provider = Arc::new(HaarnessCloudProvider {
            client: Client::new(config.clone()).unwrap(),
            grant: Arc::default(),
            models: RwLock::new(None),
            listed_audio: ListedAudio::default(),
            busy_jitter: random_busy_jitter,
        });
        let refresh = || async {
            tokio::time::sleep(std::time::Duration::from_millis(1010)).await;
            provider.refresh_models(true).await;
        };
        provider.refresh_models(true).await;
        assert_eq!(provider.info().models[0].id, "fixture-chat");
        let calls = fixture.calls.load(Ordering::SeqCst);
        futures::future::join_all((0..8).map(|_| provider.refresh_models(true))).await;
        assert_eq!(
            fixture.calls.load(Ordering::SeqCst),
            calls,
            "cached refresh should coalesce"
        );
        fixture.mode.store(1, Ordering::SeqCst);
        refresh().await;
        assert_eq!(
            provider.info().models.len(),
            1,
            "outage erased last success"
        );
        fixture.mode.store(2, Ordering::SeqCst);
        refresh().await;
        assert!(
            provider.info().models.is_empty(),
            "successful empty list must remove membership"
        );
        fixture.mode.store(0, Ordering::SeqCst);
        refresh().await;
        assert_eq!(provider.info().models.len(), 1);
        // An account switch must hide the previous account even before refresh.
        std::fs::write(
            root.join("account.json"),
            json!({"apiUrl":url,"accountId":"second"}).to_string(),
        )
        .unwrap();
        assert!(provider.info().models.is_empty());
        fixture.mode.store(1, Ordering::SeqCst);
        refresh().await;
        assert!(
            provider.info().models.is_empty(),
            "failed new account reused old models"
        );
        fixture.mode.store(0, Ordering::SeqCst);
        refresh().await;
        assert_eq!(provider.info().models.len(), 1);
        credentials.access_token = "another-login".into();
        haarness_account::credentials::save(&config.path(), &credentials).unwrap();
        assert!(provider.info().models.is_empty());
        fixture.mode.store(3, Ordering::SeqCst);
        let pending = {
            let provider = provider.clone();
            tokio::spawn(async move {
                provider.refresh_models(true).await;
            })
        };
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            fixture.entered.notified(),
        )
        .await
        .unwrap();
        haarness_account::credentials::delete(&config.path()).unwrap();
        fixture.release.notify_one();
        pending.await.unwrap();
        assert!(!provider.info().authenticated);
        assert!(
            provider.info().models.is_empty(),
            "late response restored logged-out catalog"
        );
        server.abort();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_exhausted_balance_reads_as_advice() {
        let raw = CoreError::Provider(
            "HTTP 402 Payment Required: [insufficient_credits] insufficient credits: unit Nanos \
             requested 104592000, available 0, overage opt-in false"
                .into(),
        );
        let CoreError::Provider(message) = readable(raw) else {
            panic!("still a provider error");
        };
        assert!(
            message.starts_with("Your included Haarness Cloud credits can't cover this request."),
            "{message}"
        );
        assert!(
            message.contains("Add credits and turn on overage"),
            "{message}"
        );
        assert!(!message.contains("Nanos"), "{message}");
        // Purchased credits are there and only wait for overage.
        let waiting = CoreError::Provider(
            "HTTP 402: [insufficient_credits] insufficient credits: unit Nanos requested 104592000, \
             available 5000000000, overage opt-in false"
                .into(),
        );
        assert!(
            matches!(readable(waiting), CoreError::Provider(m) if m.contains("Turn on overage in Billing so your purchased credits pay") && !m.contains("Add credits"))
        );
        let spent = CoreError::Provider(
            "HTTP 402: [insufficient_credits] insufficient credits: unit Nanos requested 1, \
             available 0, overage opt-in true"
                .into(),
        );
        assert!(
            matches!(readable(spent), CoreError::Provider(m) if m.starts_with("Your Haarness Cloud credits can't cover this request."))
        );
        let other = CoreError::Provider("HTTP 500: boom".into());
        assert!(matches!(readable(other), CoreError::Provider(m) if m == "HTTP 500: boom"));

        // A Cloud workspace's chat is read in a browser too: no terminal hint, and limits explained.
        let budget = CoreError::Provider(
            "HTTP 402: [budget_exceeded] the per_request_max budget cannot fund this request \
             (up to 2600000000 nanos)"
                .into(),
        );
        let CoreError::Provider(message) = readable_router(budget) else {
            panic!("still a provider error");
        };
        assert!(
            message.starts_with("This request could cost more than a spending limit allows."),
            "{message}"
        );
        assert!(
            !message.contains("nanos") && !message.contains("/billing"),
            "{message}"
        );
        let CoreError::Provider(message) = readable_router(CoreError::Provider(
            "HTTP 402: [insufficient_credits] insufficient credits: unit Nanos requested 1, \
             available 0, overage opt-in false"
                .into(),
        )) else {
            panic!("still a provider error");
        };
        assert!(
            message.ends_with("turn on overage in Billing, or choose a plan."),
            "{message}"
        );
        let CoreError::Provider(message) = readable_router(CoreError::Provider(
            "HTTP 402: [budget_exceeded] the account budget cannot fund this request (up to 9 nanos)"
                .into(),
        )) else {
            panic!("still a provider error");
        };
        assert!(
            message.ends_with("raise the budget under Billing in Haarness Cloud."),
            "{message}"
        );

        for (raw, start) in [
            (
                "HTTP 404 Not Found: [upstream_404] upstream returned HTTP 404",
                "The provider no longer serves this model.",
            ),
            (
                "HTTP 429: [upstream_429] upstream returned HTTP 429",
                "The provider is rate limiting this model right now.",
            ),
            (
                "HTTP 502: [upstream_503] upstream returned HTTP 503",
                "The provider is overloaded or unavailable right now.",
            ),
            (
                "HTTP 413 Payload Too Large: [upstream_413] This request needs 12110 tokens, more \
                 than the 8000 tokens a minute the provider's plan allows, so the provider refused it.",
                "This request needs 12110 tokens, more than the 8000 tokens a minute",
            ),
        ] {
            let CoreError::Provider(message) = readable_router(CoreError::Provider(raw.into()))
            else {
                panic!("still a provider error");
            };
            assert!(message.starts_with(start), "{raw} → {message}");
        }

        // The router's own wait, or the Retry-After it passes on, is kept.
        for raw in [
            "HTTP 429: [upstream_429] The provider is rate limiting this key. Try again in 12 seconds.",
            "HTTP 429: [upstream_429] upstream returned HTTP 429 Please retry in 12s.",
        ] {
            let CoreError::Provider(message) = readable_router(CoreError::Provider(raw.into()))
            else {
                panic!("still a provider error");
            };
            assert!(message.contains("Please retry in 12s"), "{raw} → {message}");
        }
        assert_eq!(advised_seconds("try again in a moment"), None);

        let byok = CoreError::Provider(
            "HTTP 402: [payment_required] routing your own provider keys needs a Haarness Cloud \
             plan or the trial"
                .into(),
        );
        let CoreError::Provider(message) = readable(byok) else {
            panic!("still a provider error");
        };
        assert!(
            message.starts_with("Routing your own provider keys"),
            "{message}"
        );
        assert!(
            message.ends_with("(in the terminal: /billing)."),
            "{message}"
        );
    }

    /// Cloud's refusal for its cap on requests in flight says the same with
    /// or without a Retry-After (today's Cloud sends none), since its words
    /// don't say whether this request alone is over the cap. They read as a
    /// usage limit (`describe_error` shows them as they are) and ask for no
    /// retry.
    #[test]
    fn an_exposure_cap_reads_right_on_every_cloud() {
        let today = "HTTP 429 Too Many Requests: [exposure_cap] too many unsettled requests for this \
                     account (cap 5000000000 nanos)";
        for raw in [today.to_owned(), format!("{today} Please retry in 5s.")] {
            for readable in [readable, readable_router] {
                let CoreError::Provider(message) = readable(CoreError::Provider(raw.clone()))
                else {
                    panic!("still a provider error");
                };
                assert_eq!(
                    message,
                    "Too many requests are still settling on this Haarness Cloud account, past its \
                     usage limit for requests in flight. Wait a moment, then try again; if nothing \
                     else is running, lower the output limit or shorten the conversation.",
                    "{raw}"
                );
                let lower = message.to_ascii_lowercase();
                assert!(lower.contains("usage limit"), "{message}");
                assert!(
                    !["try again in a moment", "retry in", "won't help"]
                        .iter()
                        .any(|words| lower.contains(words)),
                    "{message}"
                );
            }
        }
    }

    /// Cloud refuses a busy key or router with a 429 of its own. The runtime
    /// (`retry_delay`) backs off on a usage limit ("rate limit") that says to
    /// wait ("Try again in a moment", or "Please retry in 12s" for that long),
    /// and the words are what the person reads once the retries run out.
    #[test]
    fn a_busy_key_or_router_is_waited_out() {
        let concurrency = "Haarness Cloud is already running as many of your requests at once as \
                           its rate limit allows. Try again in a moment, or once another chat or \
                           agent finishes.";
        let per_minute = "You've reached Haarness Cloud's rate limit of 300 requests a minute.";
        for (raw, expected) in [
            (
                "HTTP 429 Too Many Requests: [concurrency_limited] per-key concurrency limit reached",
                concurrency.to_owned(),
            ),
            // When a slot frees is not known: backing off beats the router's
            // fixed wait, which serves clients that only read Retry-After.
            (
                "HTTP 429 Too Many Requests: [concurrency_limited] per-key concurrency limit \
                 reached Please retry in 5s.",
                concurrency.to_owned(),
            ),
            (
                "HTTP 429 Too Many Requests: [router_overloaded] router concurrency limit reached",
                "Haarness Cloud's router is at its rate limit for requests at once. Try again in \
                 a moment."
                    .to_owned(),
            ),
            // A full router's own wait spares it a retry at 2 seconds.
            (
                "HTTP 429 Too Many Requests: [router_overloaded] router concurrency limit reached \
                 Please retry in 17s.",
                "Haarness Cloud's router is at its rate limit for requests at once. Please retry \
                 in 17s."
                    .to_owned(),
            ),
            (
                "HTTP 429 Too Many Requests: [rate_limited] per-key rate limit of 300 requests \
                 per minute reached",
                format!("{per_minute} Try again in a moment."),
            ),
            // The minute's end, which the router names.
            (
                "HTTP 429 Too Many Requests: [rate_limited] per-key rate limit of 300 requests \
                 per minute reached Please retry in 17s.",
                format!("{per_minute} Please retry in 17s."),
            ),
            (
                "HTTP 429 Too Many Requests: [rate_limited] slow down",
                "You've reached Haarness Cloud's rate limit for requests a minute. Try again in \
                 a moment."
                    .to_owned(),
            ),
        ] {
            for readable in [readable, readable_router] {
                let CoreError::Provider(message) = readable(CoreError::Provider(raw.into())) else {
                    panic!("still a provider error");
                };
                assert_eq!(message, expected, "{raw}");
                let lower = message.to_ascii_lowercase();
                assert!(lower.contains("rate limit"), "{message}");
                assert!(
                    lower.contains("try again in a moment") || lower.contains("retry in 17s"),
                    "{message}"
                );
            }
        }
    }

    /// A Cloud stand-in listing a model that says nothing of audio and one
    /// that takes it; it answers chats, or refuses them as a busy key.
    #[derive(Clone, Default)]
    struct Stand {
        url: Arc<std::sync::Mutex<String>>,
        refusing: Arc<std::sync::atomic::AtomicBool>,
        sent: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    }

    /// The provider signed in to a [`Stand`], its server and its folder.
    async fn stand_in() -> (
        HaarnessCloudProvider,
        Stand,
        tokio::task::JoinHandle<()>,
        std::path::PathBuf,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let stand = Stand::default();
        url.clone_into(&mut stand.url.lock().unwrap());
        let app = Router::new()
            .route(
                "/api/v1/account/router-session",
                post(|State(state): State<Stand>| async move {
                    Json(json!({"token":"grant", "routerUrl":state.url.lock().unwrap().clone(),
                        "expiresAt":chrono::Utc::now() + chrono::Duration::hours(1)}))
                }),
            )
            .route(
                "/models",
                get(|| async {
                    Json(json!({"data":[
                        {"id":"anthropic/claude-sonnet-5-5"},
                        {"id":"openai/gpt-listens", "architecture":{"input_modalities":["text","audio"]}},
                    ]}))
                }),
            )
            .route(
                "/chat/completions",
                post(
                    |State(state): State<Stand>, Json(body): Json<serde_json::Value>| async move {
                        state.sent.lock().unwrap().push(body);
                        if state.refusing.load(Ordering::SeqCst) {
                            return (
                                StatusCode::TOO_MANY_REQUESTS,
                                Json(json!({"error":{"message":"per-key concurrency limit reached",
                                    "type":"rate_limit_error","code":"concurrency_limited"}})),
                            )
                                .into_response();
                        }
                        (
                            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                        )
                            .into_response()
                    },
                ),
            )
            .with_state(stand.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let root = std::env::temp_dir().join(format!("haarness-cloud-stand-{}", uuid_for_test()));
        let config = Config::new(&url, root.clone(), "fixture").unwrap();
        haarness_account::credentials::save(
            &config.path(),
            &haarness_account::Credentials {
                api_url: url.clone(),
                access_token: "signed-in".into(),
                refresh_token: "fixture".into(),
                access_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            },
        )
        .unwrap();
        let provider = HaarnessCloudProvider {
            client: Client::new(config).unwrap(),
            grant: Arc::default(),
            models: RwLock::new(None),
            listed_audio: ListedAudio::default(),
            busy_jitter: random_busy_jitter,
        };
        (provider, stand, server, root)
    }

    /// A tool round whose result is a recording.
    fn recording(model: &str) -> ProviderRequest {
        ProviderRequest {
            model: model.into(),
            system: "system".into(),
            messages: vec![
                haarness_core::ProviderMessage::Text {
                    role: "user".into(),
                    content: "record it".into(),
                },
                haarness_core::ProviderMessage::ToolCall {
                    call_id: "call".into(),
                    name: "clip".into(),
                    arguments: json!({}),
                    provider_metadata: serde_json::Value::Null,
                },
                haarness_core::ProviderMessage::ToolResult {
                    call_id: "call".into(),
                    name: "clip".into(),
                    output: "recorded".into(),
                    is_error: false,
                    attachments: vec!["data:audio/mpeg;name=clip.mp3;base64,SUQz".into()],
                },
            ],
            tools: Vec::new(),
            reasoning_effort: None,
            output_schema: None,
            parameters: haarness_core::RequestParameters::default(),
        }
    }

    /// The Cloud provider makes a new transport for every request: what the
    /// listing said of audio reaches each of them.
    #[tokio::test]
    async fn every_request_keeps_what_the_listing_says_of_audio() {
        use futures::StreamExt as _;
        let (provider, stand, server, root) = stand_in().await;
        for (model, takes) in [
            ("anthropic/claude-sonnet-5-5", false),
            ("openai/gpt-listens", true),
        ] {
            let events = provider
                .stream(recording(model), CancellationToken::new())
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await;
            assert!(events.iter().all(Result::is_ok), "{model}: {events:?}");
            let sent = stand.sent.lock().unwrap().last().unwrap().to_string();
            assert_eq!(sent.contains("input_audio"), takes, "{model}: {sent}");
        }
        server.abort();
        let _ = std::fs::remove_dir_all(root);
    }

    /// Busy refusals are held back a random part of a second, so agents
    /// refused together do not try again together.
    #[test]
    fn busy_jitter_is_a_random_part_of_a_second() {
        let draws: std::collections::HashSet<_> = (0..200).map(|_| random_busy_jitter()).collect();
        assert!(draws.len() > 10, "{draws:?}");
        assert!(
            draws
                .iter()
                .all(|draw| *draw < Duration::from_millis(BUSY_JITTER_MS)),
            "{draws:?}"
        );
    }

    static JITTERED: AtomicUsize = AtomicUsize::new(0);

    fn counted_jitter() -> Duration {
        JITTERED.fetch_add(1, Ordering::SeqCst);
        Duration::ZERO
    }

    static HELD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    fn held_jitter() -> Duration {
        HELD.store(true, Ordering::SeqCst);
        Duration::from_secs(3_600)
    }

    /// Each busy refusal waits its jitter before the turn sees it, and a stop
    /// ends the wait.
    #[tokio::test]
    async fn a_busy_refusal_waits_its_jitter_first() {
        use futures::StreamExt as _;
        let (mut provider, stand, server, root) = stand_in().await;
        stand.refusing.store(true, Ordering::SeqCst);
        provider.busy_jitter = counted_jitter;
        for count in 1..=3 {
            let events = provider
                .stream(
                    recording("anthropic/claude-sonnet-5-5"),
                    CancellationToken::new(),
                )
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await;
            let error = events.into_iter().find_map(Result::err).unwrap();
            assert!(
                error.to_string().contains("as its rate limit allows"),
                "{error}"
            );
            assert_eq!(JITTERED.load(Ordering::SeqCst), count);
        }
        // Held back for an hour, the refusal comes as soon as the turn stops.
        provider.busy_jitter = held_jitter;
        let cancel = CancellationToken::new();
        let mut events = provider
            .stream(recording("anthropic/claude-sonnet-5-5"), cancel.clone())
            .await
            .unwrap();
        let next = tokio::spawn(async move { events.next().await });
        while !HELD.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        cancel.cancel();
        let event = tokio::time::timeout(Duration::from_secs(30), next)
            .await
            .expect("a stop ends the wait")
            .unwrap();
        let error = event.unwrap().unwrap_err();
        assert!(
            error.to_string().contains("as its rate limit allows"),
            "{error}"
        );
        server.abort();
        let _ = std::fs::remove_dir_all(root);
    }

    #[derive(Clone)]
    struct Grants {
        url: String,
        issued: Arc<AtomicUsize>,
        requests: Arc<AtomicUsize>,
        /// Grants up to this number are refused, as if Cloud withdrew them.
        revoked: Arc<AtomicUsize>,
    }

    /// Cloud withdrew the cached grant early: the router's 401 drops it, and
    /// the request is asked once more with a new one instead of failing until
    /// the old one expires.
    #[tokio::test]
    async fn a_withdrawn_grant_is_replaced_once_on_401() {
        use futures::StreamExt as _;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let grants = Grants {
            url: url.clone(),
            issued: Arc::default(),
            requests: Arc::default(),
            revoked: Arc::new(AtomicUsize::new(1)),
        };
        let app = Router::new()
            .route(
                "/api/v1/account/router-session",
                post(|State(state): State<Grants>| async move {
                    let number = state.issued.fetch_add(1, Ordering::SeqCst) + 1;
                    Json(json!({"token":format!("grant-{number}"), "routerUrl":state.url,
                        "expiresAt":chrono::Utc::now() + chrono::Duration::hours(1)}))
                }),
            )
            .route(
                "/models",
                get(|| async { Json(json!({"data":[{"id":"fixture-chat"}]})) }),
            )
            .route(
                "/chat/completions",
                post(
                    |State(state): State<Grants>, headers: axum::http::HeaderMap| async move {
                        state.requests.fetch_add(1, Ordering::SeqCst);
                        let number = headers["authorization"]
                            .to_str()
                            .unwrap()
                            .trim_start_matches("Bearer grant-")
                            .parse::<usize>()
                            .unwrap();
                        if number <= state.revoked.load(Ordering::SeqCst) {
                            return (
                                StatusCode::UNAUTHORIZED,
                                Json(json!({"error":{"message":"grant revoked"}})),
                            )
                                .into_response();
                        }
                        (
                            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                        )
                            .into_response()
                    },
                ),
            )
            .with_state(grants.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let root = std::env::temp_dir().join(format!("haarness-cloud-grant-{}", uuid_for_test()));
        let config = Config::new(&url, root.clone(), "fixture").unwrap();
        haarness_account::credentials::save(
            &config.path(),
            &haarness_account::Credentials {
                api_url: url.clone(),
                access_token: "signed-in".into(),
                refresh_token: "fixture".into(),
                access_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            },
        )
        .unwrap();
        let provider = HaarnessCloudProvider {
            client: Client::new(config).unwrap(),
            grant: Arc::default(),
            models: RwLock::new(None),
            listed_audio: ListedAudio::default(),
            busy_jitter: random_busy_jitter,
        };
        let request = || ProviderRequest {
            model: "fixture-chat".into(),
            system: "system".into(),
            messages: vec![haarness_core::ProviderMessage::Text {
                role: "user".into(),
                content: "hello".into(),
            }],
            tools: Vec::new(),
            reasoning_effort: None,
            output_schema: None,
            parameters: haarness_core::RequestParameters::default(),
        };
        let text = |events: Vec<CoreResult<haarness_core::ProviderEvent>>| {
            events
                .into_iter()
                .map(|event| match event {
                    Ok(haarness_core::ProviderEvent::TextDelta(text)) => Ok(text),
                    Ok(_) => Ok(String::new()),
                    Err(error) => Err(error),
                })
                .collect::<CoreResult<String>>()
        };
        let events = provider
            .stream(request(), CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert_eq!(text(events).unwrap(), "ok");
        assert_eq!(grants.issued.load(Ordering::SeqCst), 2);
        assert_eq!(grants.requests.load(Ordering::SeqCst), 2);

        // The new grant is kept for the next request.
        let events = provider
            .stream(request(), CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert_eq!(text(events).unwrap(), "ok");
        assert_eq!(grants.issued.load(Ordering::SeqCst), 2);

        // A refusal that a new grant does not cure is reported, after one retry.
        grants.revoked.store(usize::MAX, Ordering::SeqCst);
        let events = provider
            .stream(request(), CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        let error = text(events).unwrap_err();
        assert!(error.to_string().contains("HTTP 401"), "{error}");
        assert_eq!(grants.issued.load(Ordering::SeqCst), 3);
        assert_eq!(grants.requests.load(Ordering::SeqCst), 5);
        server.abort();
        let _ = std::fs::remove_dir_all(root);
    }

    fn uuid_for_test() -> String {
        format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }
}
