//! Managed models use a local account session, never a saved upstream API key.
use crate::{
    CompatibleInstance, CompatibleModelDefaults, OpenAiCompatibleProvider, ProviderHeaders,
};
use async_trait::async_trait;
use haarness_account::{Client, Config, RouterSession};
use haarness_core::{CoreError, CoreResult, ModelProvider, ProviderRequest, ProviderStream};
use haarness_protocol::ProviderInfo;
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub struct HaarnessCloudProvider {
    client: Client,
    grant: Mutex<Option<(String, Option<String>, Option<String>, RouterSession)>>,
    models: RwLock<Option<([u8; 32], Arc<crate::live_models::LiveModels>)>>,
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
            grant: Mutex::new(None),
            models: RwLock::new(None),
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
            message.starts_with("Your included Haarness Cloud credits are used up."),
            "{message}"
        );
        assert!(message.contains("Turn on overage"), "{message}");
        assert!(!message.contains("Nanos"), "{message}");
        let spent = CoreError::Provider(
            "HTTP 402: [insufficient_credits] insufficient credits: unit Nanos requested 1, \
             available 0, overage opt-in true"
                .into(),
        );
        assert!(
            matches!(readable(spent), CoreError::Provider(m) if m.starts_with("You're out of Haarness Cloud credits."))
        );
        let other = CoreError::Provider("HTTP 500: boom".into());
        assert!(matches!(readable(other), CoreError::Provider(m) if m == "HTTP 500: boom"));
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
impl HaarnessCloudProvider {
    pub fn from_environment() -> Option<Self> {
        Some(Self {
            client: Client::new(Config::from_env().ok()?).ok()?,
            grant: Mutex::new(None),
            models: RwLock::new(None),
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
        let bearer = self.client.bearer(None).await.map_err(error)?;
        let account = self.client.config.selected_account();
        let router = self.client.config.selected_router().map_err(error)?;
        let mut grant = self.grant.lock().await;
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
                self.client.router_session().await.map_err(error)?,
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
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: false,
            headers: ProviderHeaders::default(),
        })
        .with_http_client(http)
        .with_server_output_limit())
    }
}
fn error(e: impl std::fmt::Display) -> CoreError {
    CoreError::Provider(e.to_string())
}

/// Say what an exhausted Cloud balance means for the person reading it. The
/// router's 402 reads `[insufficient_credits] insufficient credits: unit
/// Nanos requested …`, which is accounting, not advice.
fn readable(error: CoreError) -> CoreError {
    match error {
        CoreError::Provider(message) if message.contains("[insufficient_credits]") => {
            CoreError::Provider(
                if message.contains("overage opt-in false") {
                    // Purchased credits only pay once overage is on, so "out of
                    // credits" would be wrong for someone who just bought some.
                    "Your included Haarness Cloud credits are used up. Turn on overage in \
                     Billing to spend purchased credits, or add credits or choose a plan \
                     (in the terminal: /billing)."
                } else if message.contains("frozen") {
                    "Your purchased Haarness Cloud credits are on hold. Open Billing in \
                     Haarness Cloud for details (in the terminal: /billing)."
                } else {
                    "You're out of Haarness Cloud credits. Add credits or choose a plan under \
                     Billing (in the terminal: /billing)."
                }
                .into(),
            )
        }
        other => other,
    }
}
#[async_trait]
impl ModelProvider for HaarnessCloudProvider {
    fn id(&self) -> &str {
        "haarness"
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
        let models = self
            .catalog()
            .map_or_else(Vec::new, |(_, catalog)| catalog.get(&[]));
        let stream = transport
            .with_model_info(models)
            .stream(request, cancel)
            .await
            .map_err(readable)?;
        Ok(Box::pin(futures::StreamExt::map(stream, |event| {
            event.map_err(readable)
        })))
    }
}
