//! Managed models use a local account session, never a saved upstream API key.
use crate::{
    CompatibleInstance, CompatibleModelDefaults, OpenAiCompatibleProvider, ProviderHeaders,
};
use async_trait::async_trait;
use haarness_account::{Client, Config, RouterSession};
use haarness_core::{CoreError, CoreResult, ModelProvider, ProviderRequest, ProviderStream};
use haarness_protocol::{ModelInfo, ProviderInfo};
use std::sync::RwLock;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub struct HaarnessCloudProvider {
    client: Client,
    grant: Mutex<Option<(String, Option<String>, RouterSession)>>,
    models: RwLock<Vec<ModelInfo>>,
}
impl HaarnessCloudProvider {
    pub fn from_environment() -> Option<Self> {
        Some(Self {
            client: Client::new(Config::from_env().ok()?).ok()?,
            grant: Mutex::new(None),
            models: RwLock::new(Vec::new()),
        })
    }
    async fn transport(&self) -> CoreResult<OpenAiCompatibleProvider> {
        let bearer = self.client.bearer(None).await.map_err(error)?;
        let account = self.client.config.selected_account();
        let mut grant = self.grant.lock().await;
        if grant.as_ref().is_none_or(|(token, selected, g)| {
            token != &bearer
                || selected != &account
                || g.expires_at <= chrono::Utc::now() + chrono::Duration::seconds(30)
        }) {
            *grant = Some((
                bearer,
                account,
                self.client.router_session().await.map_err(error)?,
            ));
        }
        let g = &grant.as_ref().expect("grant initialized").2;
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
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: false,
            headers: ProviderHeaders::default(),
        })
        .with_http_client(http))
    }
}
fn error(e: impl std::fmt::Display) -> CoreError {
    CoreError::Provider(e.to_string())
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
            models: if signed_in {
                self.models
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            } else {
                Vec::new()
            },
        }
    }
    async fn refresh_models(&self, _force: bool) {
        if !self.client.config.signed_in() {
            self.models
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear();
            return;
        }
        match self.transport().await {
            Ok(provider) => {
                let info = provider.discover_models().await.info();
                *self
                    .models
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = info.models;
            }
            Err(_) => {
                self.models
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clear();
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
            .models
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        transport
            .with_model_info(models)
            .stream(request, cancel)
            .await
    }
}
