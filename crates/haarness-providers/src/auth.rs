//! Provider credentials and OpenAI subscription OAuth.

use std::{
    collections::BTreeMap,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use fs2::FileExt as _;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use url::Url;

const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_ISSUER: &str = "https://auth.openai.com";
const OPENAI_CODEX_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
const OPENAI_CALLBACK_PORT: u16 = 1455;
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const POLL_MARGIN: Duration = Duration::from_secs(3);
const XAI_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const XAI_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const XAI_DEVICE_URL: &str = "https://auth.x.ai/oauth2/device/code";
const XAI_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const XAI_DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const COPILOT_CLIENT_ID: &str = "Ov23li8tweQw6odWQebz";
const COPILOT_DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

pub type AuthResult<T> = Result<T, AuthError>;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("could not read provider credentials: {0}")]
    Read(#[source] std::io::Error),
    #[error("could not decode provider credentials: {0}")]
    Decode(#[source] serde_json::Error),
    #[error("could not write provider credentials: {0}")]
    Write(#[source] std::io::Error),
    #[error("could not encode provider credentials: {0}")]
    Encode(#[source] serde_json::Error),
    #[error("OpenAI sign-in failed: {0}")]
    OpenAi(String),
    #[error("xAI sign-in failed: {0}")]
    Xai(String),
    #[error("GitHub Copilot sign-in failed: {0}")]
    Copilot(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Credential {
    Typed(TypedCredential),
    Legacy(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TypedCredential {
    Api {
        key: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        metadata: BTreeMap<String, String>,
    },
    #[serde(rename = "oauth")]
    OAuth {
        access: String,
        refresh: String,
        expires: u64,
        #[serde(
            default,
            rename = "accountId",
            alias = "account_id",
            skip_serializing_if = "Option::is_none"
        )]
        account_id: Option<String>,
        #[serde(
            default,
            rename = "enterpriseUrl",
            alias = "enterprise_url",
            skip_serializing_if = "Option::is_none"
        )]
        enterprise_url: Option<String>,
    },
}

impl Credential {
    #[must_use]
    pub fn api(key: String) -> Self {
        Self::Typed(TypedCredential::Api {
            key,
            metadata: BTreeMap::new(),
        })
    }

    #[must_use]
    pub fn oauth(
        access: String,
        refresh: String,
        expires: u64,
        account_id: Option<String>,
    ) -> Self {
        Self::Typed(TypedCredential::OAuth {
            access,
            refresh,
            expires,
            account_id,
            enterprise_url: None,
        })
    }

    #[must_use]
    pub fn api_key(&self) -> Option<&str> {
        match self {
            Self::Typed(TypedCredential::Api { key, .. }) | Self::Legacy(key) => Some(key),
            Self::Typed(TypedCredential::OAuth { .. }) => None,
        }
    }

    #[must_use]
    pub fn oauth_parts(&self) -> Option<OAuthCredential<'_>> {
        match self {
            Self::Typed(TypedCredential::OAuth {
                access,
                refresh,
                expires,
                account_id,
                ..
            }) => Some(OAuthCredential {
                access,
                refresh,
                expires: *expires,
                account_id: account_id.as_deref(),
            }),
            Self::Typed(TypedCredential::Api { .. }) | Self::Legacy(_) => None,
        }
    }

    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Typed(TypedCredential::Api { .. }) => "api",
            Self::Typed(TypedCredential::OAuth { .. }) => "oauth",
            Self::Legacy(_) => "legacy api",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct OAuthCredential<'a> {
    pub access: &'a str,
    pub refresh: &'a str,
    pub expires: u64,
    pub account_id: Option<&'a str>,
}

/// Owner-only credential storage. Unknown records survive read-modify-write.
/// Disk is read for every lookup so a running server sees CLI auth changes.
#[derive(Debug)]
pub struct CredentialStore {
    path: PathBuf,
    mutation: Mutex<()>,
}

impl CredentialStore {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            mutation: Mutex::new(()),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn all_raw(&self) -> AuthResult<BTreeMap<String, serde_json::Value>> {
        if !self.path.exists() {
            return Ok(BTreeMap::new());
        }
        serde_json::from_slice(&fs::read(&self.path).map_err(AuthError::Read)?)
            .map_err(AuthError::Decode)
    }

    pub fn all(&self) -> AuthResult<BTreeMap<String, Credential>> {
        Ok(self
            .all_raw()?
            .into_iter()
            .filter_map(|(id, value)| {
                serde_json::from_value(value)
                    .ok()
                    .map(|credential| (id, credential))
            })
            .collect())
    }

    pub fn get(&self, provider: &str) -> AuthResult<Option<Credential>> {
        Ok(self
            .all_raw()?
            .get(normalize(provider))
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok()))
    }

    pub fn set(&self, provider: &str, credential: &Credential) -> AuthResult<()> {
        let _guard = self
            .mutation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _file_lock = self.lock_mutations()?;
        let provider = normalize(provider);
        let mut raw = self.all_raw()?;
        raw.remove(&format!("{provider}/"));
        raw.insert(
            provider.to_owned(),
            serde_json::to_value(credential).map_err(AuthError::Encode)?,
        );
        self.write_raw(&raw)
    }

    pub fn remove(&self, provider: &str) -> AuthResult<bool> {
        let _guard = self
            .mutation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _file_lock = self.lock_mutations()?;
        let provider = normalize(provider);
        let mut raw = self.all_raw()?;
        let removed =
            raw.remove(provider).is_some() | raw.remove(&format!("{provider}/")).is_some();
        self.write_raw(&raw)?;
        Ok(removed)
    }

    /// A late refresh must not recreate a credential removed or replaced while
    /// the network exchange was in flight.
    pub fn replace_oauth_if_current(
        &self,
        provider: &str,
        expected_refresh: &str,
        next: &Credential,
    ) -> AuthResult<bool> {
        let _guard = self
            .mutation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _file_lock = self.lock_mutations()?;
        let provider = normalize(provider);
        let mut raw = self.all_raw()?;
        let matches = raw
            .get(provider)
            .cloned()
            .and_then(|value| serde_json::from_value::<Credential>(value).ok())
            .and_then(|credential| {
                credential
                    .oauth_parts()
                    .map(|oauth| oauth.refresh == expected_refresh)
            })
            .unwrap_or(false);
        if !matches {
            return Ok(false);
        }
        raw.insert(
            provider.to_owned(),
            serde_json::to_value(next).map_err(AuthError::Encode)?,
        );
        self.write_raw(&raw)?;
        Ok(true)
    }

    fn write_raw(&self, value: &BTreeMap<String, serde_json::Value>) -> AuthResult<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(AuthError::Write)?;
        }
        let mut suffix = [0_u8; 8];
        rand::rng().fill_bytes(&mut suffix);
        let temporary = self.path.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            URL_SAFE_NO_PAD.encode(suffix)
        ));
        let result = (|| -> AuthResult<()> {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(AuthError::Write)?;
            set_private(&file)?;
            file.write_all(&serde_json::to_vec_pretty(value).map_err(AuthError::Encode)?)
                .map_err(AuthError::Write)?;
            file.sync_all().map_err(AuthError::Write)?;
            fs::rename(&temporary, &self.path).map_err(AuthError::Write)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn lock_mutations(&self) -> AuthResult<fs::File> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(AuthError::Write)?;
        }
        let mut lock_name = self.path.as_os_str().to_os_string();
        lock_name.push(".lock");
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(PathBuf::from(lock_name))
            .map_err(AuthError::Write)?;
        set_private(&lock)?;
        lock.lock_exclusive().map_err(AuthError::Write)?;
        Ok(lock)
    }
}

fn normalize(provider: &str) -> &str {
    provider.trim_end_matches('/')
}

#[cfg(unix)]
fn set_private(file: &fs::File) -> AuthResult<()> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(AuthError::Write)
}

#[cfg(not(unix))]
fn set_private(_file: &fs::File) -> AuthResult<()> {
    Ok(())
}

#[derive(Clone, Debug)]
pub struct OpenAiOAuthConfig {
    pub issuer: String,
    pub client_id: String,
    pub codex_endpoint: String,
    pub callback_port: u16,
    pub user_agent: String,
}

impl Default for OpenAiOAuthConfig {
    fn default() -> Self {
        // The issuer and callback port can be pointed at a fixture so the
        // browser and device flows are testable end to end without OpenAI.
        let issuer = std::env::var("HAARNESS_OPENAI_OAUTH_ISSUER")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| OPENAI_ISSUER.into());
        let callback_port = std::env::var("HAARNESS_OPENAI_OAUTH_CALLBACK_PORT")
            .ok()
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(OPENAI_CALLBACK_PORT);
        Self {
            issuer,
            client_id: OPENAI_CLIENT_ID.into(),
            codex_endpoint: OPENAI_CODEX_ENDPOINT.into(),
            callback_port,
            user_agent: format!("haarness/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

#[derive(Clone, Debug)]
pub struct OpenAiOAuthClient {
    client: reqwest::Client,
    config: OpenAiOAuthConfig,
}

impl Default for OpenAiOAuthClient {
    fn default() -> Self {
        Self::new(OpenAiOAuthConfig::default())
    }
}

impl OpenAiOAuthClient {
    #[must_use]
    pub fn new(config: OpenAiOAuthConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            config,
        }
    }

    #[must_use]
    pub fn codex_endpoint(&self) -> &str {
        &self.config.codex_endpoint
    }

    pub async fn start_browser_login(&self) -> AuthResult<OpenAiBrowserLogin> {
        let listeners = bind_callback(self.config.callback_port).await?;
        let redirect = format!(
            "http://localhost:{}/auth/callback",
            self.config.callback_port
        );
        let pkce = Pkce::generate();
        let state = random_url_token(32);
        let url = self.authorize_url(&redirect, &pkce, &state)?;
        let client = self.clone();
        let task = tokio::spawn(async move {
            let callback =
                tokio::time::timeout(CALLBACK_TIMEOUT, wait_for_callback(&listeners, &state))
                    .await
                    .map_err(|_| AuthError::OpenAi("browser authorization timed out".into()))??;
            let result = client
                .exchange(&callback.code, &redirect, &pkce.verifier)
                .await;
            let (status, page) = match &result {
                Ok(_) => ("200 OK", success_page()),
                Err(error) => ("400 Bad Request", error_page(&error.to_string())),
            };
            let _ = respond(callback.stream, status, &page).await;
            result
        });
        Ok(OpenAiBrowserLogin {
            url,
            task: Some(task),
        })
    }

    pub async fn start_device_login(&self) -> AuthResult<OpenAiDeviceLogin> {
        let response = self
            .client
            .post(format!(
                "{}/api/accounts/deviceauth/usercode",
                self.config.issuer.trim_end_matches('/')
            ))
            .header("User-Agent", &self.config.user_agent)
            .json(&serde_json::json!({"client_id": self.config.client_id}))
            .send()
            .await
            .map_err(request_error)?;
        let device: DeviceCodeResponse =
            decode_response(response, "start device authorization").await?;
        let interval = device.interval.parse::<u64>().unwrap_or(5).max(1);
        Ok(OpenAiDeviceLogin {
            client: self.clone(),
            verification_url: format!("{}/codex/device", self.config.issuer.trim_end_matches('/')),
            user_code: device.user_code,
            device_auth_id: device.device_auth_id,
            interval: Duration::from_secs(interval) + POLL_MARGIN,
        })
    }

    pub async fn refresh(&self, credential: OAuthCredential<'_>) -> AuthResult<Credential> {
        let response = self
            .client
            .post(format!(
                "{}/oauth/token",
                self.config.issuer.trim_end_matches('/')
            ))
            .header("User-Agent", &self.config.user_agent)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", credential.refresh),
                ("client_id", self.config.client_id.as_str()),
            ])
            .send()
            .await
            .map_err(request_error)?;
        let tokens: TokenResponse = decode_response(response, "refresh token").await?;
        Ok(tokens.credential(
            tokens
                .account_id()
                .or_else(|| credential.account_id.map(str::to_owned)),
            Some(credential.refresh),
        ))
    }

    fn authorize_url(&self, redirect: &str, pkce: &Pkce, state: &str) -> AuthResult<Url> {
        let mut url = Url::parse(&format!(
            "{}/oauth/authorize",
            self.config.issuer.trim_end_matches('/')
        ))
        .map_err(|error| AuthError::OpenAi(error.to_string()))?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.client_id)
            .append_pair("redirect_uri", redirect)
            .append_pair("scope", "openid profile email offline_access")
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("id_token_add_organizations", "true")
            .append_pair("codex_cli_simplified_flow", "true")
            .append_pair("state", state)
            .append_pair("originator", "opencode");
        Ok(url)
    }

    async fn exchange(&self, code: &str, redirect: &str, verifier: &str) -> AuthResult<Credential> {
        let response = self
            .client
            .post(format!(
                "{}/oauth/token",
                self.config.issuer.trim_end_matches('/')
            ))
            .header("User-Agent", &self.config.user_agent)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect),
                ("client_id", self.config.client_id.as_str()),
                ("code_verifier", verifier),
            ])
            .send()
            .await
            .map_err(request_error)?;
        let tokens: TokenResponse =
            decode_response(response, "exchange authorization code").await?;
        Ok(tokens.credential(tokens.account_id(), None))
    }
}

pub struct OpenAiBrowserLogin {
    pub url: Url,
    task: Option<JoinHandle<AuthResult<Credential>>>,
}

impl OpenAiBrowserLogin {
    pub async fn finish(mut self) -> AuthResult<Credential> {
        // The task stays owned by `self` while it runs: when a cancelled
        // sign-in drops this future, `Drop` aborts the task and its callback
        // listener closes, so the next attempt can use the port.
        let Some(task) = self.task.as_mut() else {
            return Err(AuthError::OpenAi(
                "browser authorization was already completed".into(),
            ));
        };
        let result = task.await;
        self.task = None;
        result.map_err(|error| AuthError::OpenAi(format!("authorization task failed: {error}")))?
    }
}

impl Drop for OpenAiBrowserLogin {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub struct OpenAiDeviceLogin {
    client: OpenAiOAuthClient,
    pub verification_url: String,
    pub user_code: String,
    device_auth_id: String,
    interval: Duration,
}

impl OpenAiDeviceLogin {
    pub async fn finish(self) -> AuthResult<Credential> {
        loop {
            let response = self
                .client
                .client
                .post(format!(
                    "{}/api/accounts/deviceauth/token",
                    self.client.config.issuer.trim_end_matches('/')
                ))
                .header("User-Agent", &self.client.config.user_agent)
                .json(&serde_json::json!({
                    "device_auth_id": self.device_auth_id,
                    "user_code": self.user_code,
                }))
                .send()
                .await
                .map_err(request_error)?;
            if response.status().is_success() {
                let code: DeviceTokenResponse = response.json().await.map_err(|error| {
                    AuthError::OpenAi(format!("invalid device response: {error}"))
                })?;
                let redirect = format!(
                    "{}/deviceauth/callback",
                    self.client.config.issuer.trim_end_matches('/')
                );
                return self
                    .client
                    .exchange(&code.authorization_code, &redirect, &code.code_verifier)
                    .await;
            }
            if response.status().as_u16() != 403 && response.status().as_u16() != 404 {
                return Err(response_error(response, "poll device authorization").await);
            }
            tokio::time::sleep(self.interval).await;
        }
    }
}

#[derive(Clone, Debug)]
pub struct XaiOAuthConfig {
    pub token_url: String,
    pub device_url: String,
    pub client_id: String,
    pub scope: String,
    pub user_agent: String,
}

impl Default for XaiOAuthConfig {
    fn default() -> Self {
        Self {
            token_url: XAI_TOKEN_URL.into(),
            device_url: XAI_DEVICE_URL.into(),
            client_id: XAI_CLIENT_ID.into(),
            scope: XAI_SCOPE.into(),
            user_agent: format!("haarness/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

#[derive(Clone, Debug)]
pub struct XaiOAuthClient {
    client: reqwest::Client,
    config: XaiOAuthConfig,
}

impl Default for XaiOAuthClient {
    fn default() -> Self {
        Self::new(XaiOAuthConfig::default())
    }
}

impl XaiOAuthClient {
    #[must_use]
    pub fn new(config: XaiOAuthConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            config,
        }
    }

    pub async fn start_device_login(&self) -> AuthResult<XaiDeviceLogin> {
        let response = self
            .client
            .post(&self.config.device_url)
            .header("Accept", "application/json")
            .header("User-Agent", &self.config.user_agent)
            .form(&[
                ("client_id", self.config.client_id.as_str()),
                ("scope", self.config.scope.as_str()),
                ("referrer", "opencode"),
            ])
            .send()
            .await
            .map_err(xai_request_error)?;
        let device: XaiDeviceCodeResponse =
            xai_decode_response(response, "start device authorization").await?;
        if device.device_code.is_empty()
            || device.user_code.is_empty()
            || device.verification_uri.is_empty()
        {
            return Err(AuthError::Xai(
                "device response is missing its code or verification URL".into(),
            ));
        }
        let interval = device.interval.unwrap_or(5).max(1);
        let expires = device.expires_in.unwrap_or(5 * 60).max(1);
        Ok(XaiDeviceLogin {
            client: self.clone(),
            verification_url: device
                .verification_uri_complete
                .unwrap_or_else(|| device.verification_uri.clone()),
            instructions_url: device.verification_uri,
            user_code: device.user_code,
            device_code: device.device_code,
            interval: Duration::from_secs(interval),
            expires: Duration::from_secs(expires),
        })
    }

    pub async fn refresh(&self, credential: OAuthCredential<'_>) -> AuthResult<Credential> {
        let response = self
            .client
            .post(&self.config.token_url)
            .header("Accept", "application/json")
            .header("User-Agent", &self.config.user_agent)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", credential.refresh),
                ("client_id", self.config.client_id.as_str()),
            ])
            .send()
            .await
            .map_err(xai_request_error)?;
        let tokens: TokenResponse = xai_decode_response(response, "refresh token").await?;
        Ok(tokens.credential(None, Some(credential.refresh)))
    }
}

pub struct XaiDeviceLogin {
    client: XaiOAuthClient,
    pub verification_url: String,
    pub instructions_url: String,
    pub user_code: String,
    device_code: String,
    interval: Duration,
    expires: Duration,
}

impl XaiDeviceLogin {
    pub async fn finish(self) -> AuthResult<Credential> {
        let deadline = tokio::time::Instant::now() + self.expires;
        let mut interval = self.interval;
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(AuthError::Xai(
                    "device authorization timed out; run login again".into(),
                ));
            }
            let response = self
                .client
                .client
                .post(&self.client.config.token_url)
                .header("Accept", "application/json")
                .header("User-Agent", &self.client.config.user_agent)
                .form(&[
                    ("grant_type", XAI_DEVICE_GRANT),
                    ("client_id", self.client.config.client_id.as_str()),
                    ("device_code", self.device_code.as_str()),
                ])
                .send()
                .await
                .map_err(xai_request_error)?;
            if response.status().is_success() {
                let tokens: TokenResponse = response.json().await.map_err(|error| {
                    AuthError::Xai(format!(
                        "device token response returned invalid JSON: {error}"
                    ))
                })?;
                return Ok(tokens.credential(None, None));
            }
            let status = response.status();
            let body = response
                .json::<serde_json::Value>()
                .await
                .unwrap_or_default();
            let error = body.get("error").and_then(serde_json::Value::as_str);
            match error {
                Some("authorization_pending") => {}
                Some("slow_down") => interval += Duration::from_secs(5),
                Some("expired_token") => {
                    return Err(AuthError::Xai(
                        "device code expired; run login again".into(),
                    ));
                }
                _ => {
                    let detail = body
                        .get("error_description")
                        .and_then(serde_json::Value::as_str)
                        .or(error)
                        .unwrap_or("device token exchange failed");
                    return Err(AuthError::Xai(format!("{detail} ({status})")));
                }
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            tokio::time::sleep((interval + POLL_MARGIN).min(remaining)).await;
        }
    }
}

#[derive(Clone, Debug)]
pub struct CopilotOAuthConfig {
    pub base_url: String,
    pub client_id: String,
    pub user_agent: String,
}

impl Default for CopilotOAuthConfig {
    fn default() -> Self {
        let domain = std::env::var("GITHUB_ENTERPRISE_URL").map_or_else(
            |_| "github.com".into(),
            |url| {
                url.trim()
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .trim_end_matches('/')
                    .to_owned()
            },
        );
        Self {
            base_url: format!("https://{domain}"),
            client_id: COPILOT_CLIENT_ID.into(),
            user_agent: format!("haarness/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CopilotOAuthClient {
    client: reqwest::Client,
    config: CopilotOAuthConfig,
}

impl Default for CopilotOAuthClient {
    fn default() -> Self {
        Self::new(CopilotOAuthConfig::default())
    }
}

impl CopilotOAuthClient {
    #[must_use]
    pub fn new(config: CopilotOAuthConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            config,
        }
    }

    pub async fn start_device_login(&self) -> AuthResult<CopilotDeviceLogin> {
        let endpoint = format!(
            "{}/login/device/code",
            self.config.base_url.trim_end_matches('/')
        );
        let response = self
            .client
            .post(endpoint)
            .header("Accept", "application/json")
            .header("User-Agent", &self.config.user_agent)
            .json(&serde_json::json!({
                "client_id":self.config.client_id,
                "scope":"read:user"
            }))
            .send()
            .await
            .map_err(copilot_request_error)?;
        let device: CopilotDeviceCodeResponse =
            copilot_decode_response(response, "start device authorization").await?;
        if device.device_code.is_empty()
            || device.user_code.is_empty()
            || device.verification_uri.is_empty()
        {
            return Err(AuthError::Copilot(
                "device response is missing its code or verification URL".into(),
            ));
        }
        Ok(CopilotDeviceLogin {
            client: self.clone(),
            verification_url: device.verification_uri,
            user_code: device.user_code,
            device_code: device.device_code,
            interval: Duration::from_secs(device.interval.unwrap_or(5).max(1)),
            expires: Duration::from_secs(device.expires_in.unwrap_or(15 * 60).max(1)),
        })
    }
}

pub struct CopilotDeviceLogin {
    client: CopilotOAuthClient,
    pub verification_url: String,
    pub user_code: String,
    device_code: String,
    interval: Duration,
    expires: Duration,
}

impl CopilotDeviceLogin {
    pub async fn finish(self) -> AuthResult<Credential> {
        let deadline = tokio::time::Instant::now() + self.expires;
        let mut interval = self.interval;
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(AuthError::Copilot(
                    "device authorization timed out; run login again".into(),
                ));
            }
            let endpoint = format!(
                "{}/login/oauth/access_token",
                self.client.config.base_url.trim_end_matches('/')
            );
            let response = self
                .client
                .client
                .post(endpoint)
                .header("Accept", "application/json")
                .header("User-Agent", &self.client.config.user_agent)
                .json(&serde_json::json!({
                    "client_id":self.client.config.client_id,
                    "device_code":self.device_code,
                    "grant_type":COPILOT_DEVICE_GRANT
                }))
                .send()
                .await
                .map_err(copilot_request_error)?;
            let status = response.status();
            let body: serde_json::Value = response.json().await.map_err(|error| {
                AuthError::Copilot(format!(
                    "device token response returned invalid JSON: {error}"
                ))
            })?;
            if status.is_success()
                && let Some(token) = body
                    .get("access_token")
                    .and_then(serde_json::Value::as_str)
                    .filter(|token| !token.is_empty())
            {
                return Ok(Credential::api(token.to_owned()));
            }
            match body.get("error").and_then(serde_json::Value::as_str) {
                Some("authorization_pending") => {}
                Some("slow_down") => interval += Duration::from_secs(5),
                Some("expired_token") => {
                    return Err(AuthError::Copilot(
                        "device code expired; run login again".into(),
                    ));
                }
                error => {
                    let detail = body
                        .get("error_description")
                        .and_then(serde_json::Value::as_str)
                        .or(error)
                        .unwrap_or("device token exchange failed");
                    return Err(AuthError::Copilot(format!("{detail} ({status})")));
                }
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            tokio::time::sleep((interval + POLL_MARGIN).min(remaining)).await;
        }
    }
}

#[must_use]
pub fn xai_access_token_expiring(access_token: &str, skew: Duration) -> bool {
    jwt_claims(access_token)
        .and_then(|claims| {
            claims
                .get("exp")
                .and_then(serde_json::Value::as_u64)
                .map(|exp| exp * 1000)
        })
        .is_some_and(|expires| {
            expires <= unix_millis().saturating_add(skew.as_millis().try_into().unwrap_or(u64::MAX))
        })
}

#[derive(Clone, Debug)]
struct Pkce {
    verifier: String,
    challenge: String,
}

impl Pkce {
    fn generate() -> Self {
        const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
        let mut random = [0_u8; 43];
        rand::rng().fill_bytes(&mut random);
        let verifier: String = random
            .iter()
            .map(|byte| char::from(CHARS[usize::from(*byte) % CHARS.len()]))
            .collect();
        Self {
            challenge: URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
            verifier,
        }
    }
}

struct BrowserCallback {
    code: String,
    stream: TcpStream,
}

/// The callback listeners: IPv4 and, where the machine has it, IPv6 loopback
/// on one port, so `localhost` reaches this sign-in whichever family the
/// browser tries first. A cancelled attempt releases the port as it shuts
/// down, so a busy port is retried for a moment before giving up.
async fn bind_callback(port: u16) -> AuthResult<Vec<TcpListener>> {
    let mut attempts = 0;
    loop {
        match bind_loopback(port).await {
            Ok(listeners) => return Ok(listeners),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse && attempts < 20 => {
                attempts += 1;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                return Err(AuthError::OpenAi(format!(
                    "another sign-in is using port {port} on this computer. Finish or close it (Codex CLI uses the same port), then try again."
                )));
            }
            Err(error) => {
                return Err(AuthError::OpenAi(format!(
                    "could not open port {port} on this computer for the browser to return to ({error})."
                )));
            }
        }
    }
}

async fn bind_loopback(port: u16) -> std::io::Result<Vec<TcpListener>> {
    let ipv4 = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    let port = ipv4.local_addr()?.port();
    match TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, port)).await {
        Ok(ipv6) => Ok(vec![ipv4, ipv6]),
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => Err(error),
        // No IPv6 loopback here: browsers reach `localhost` over IPv4.
        Err(_) => Ok(vec![ipv4]),
    }
}

/// Waits for the browser's return. Port 1455 is fixed, so any web page can
/// send a request to it: only one carrying this sign-in's `state` ends the
/// wait, with a code or the provider's refusal. Anything else is answered and
/// the wait goes on (until [`CALLBACK_TIMEOUT`]), and its text never becomes
/// the error.
async fn wait_for_callback(listeners: &[TcpListener], state: &str) -> AuthResult<BrowserCallback> {
    // Each connection is read in a task of its own while the listeners keep
    // accepting, so idle ones (a page's `<link rel=preconnect>`, a browser's
    // spare socket) never hold up the real return.
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let accepts = listeners.iter().map(|listener| Box::pin(listener.accept()));
        tokio::select! {
            (accepted, _, _) = futures::future::select_all(accepts) => {
                let (stream, _) = accepted.map_err(|error| {
                    AuthError::OpenAi(format!("callback listener failed: {error}"))
                })?;
                connections.spawn(answer_callback(stream, state.to_owned()));
            }
            Some(Ok(Some(outcome))) = connections.join_next() => return outcome,
        }
    }
}

/// Reads one connection to the callback port and answers it. `Some` when it
/// ends the sign-in.
async fn answer_callback(
    mut stream: TcpStream,
    state: String,
) -> Option<AuthResult<BrowserCallback>> {
    let mut buffer = vec![0_u8; 16 * 1024];
    let Ok(Ok(count)) = tokio::time::timeout(CALLBACK_READ_TIMEOUT, stream.read(&mut buffer)).await
    else {
        return None;
    };
    let request = String::from_utf8_lossy(&buffer[..count]);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default();
    match read_callback(target, &state) {
        CallbackRequest::Elsewhere => {
            let _ = respond(stream, "404 Not Found", "Not found").await;
            None
        }
        CallbackRequest::Foreign => {
            let _ = respond(
                stream,
                "400 Bad Request",
                &error_page("This is not the sign-in Haarness started."),
            )
            .await;
            None
        }
        CallbackRequest::Code(code) => Some(Ok(BrowserCallback { code, stream })),
        CallbackRequest::Refused(error) => {
            let _ = respond(stream, "400 Bad Request", &error_page(&error)).await;
            Some(Err(AuthError::OpenAi(error)))
        }
    }
}

/// How long one connection to the callback port may take to send its request.
const CALLBACK_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// What one request to the callback port means for the sign-in.
#[derive(Debug, PartialEq, Eq)]
enum CallbackRequest {
    /// Not the callback path: answered 404.
    Elsewhere,
    /// The callback path without this sign-in's state: answered 400.
    Foreign,
    /// This sign-in's authorization code.
    Code(String),
    /// This sign-in failed: the provider's refusal, or no code.
    Refused(String),
}

fn read_callback(target: &str, state: &str) -> CallbackRequest {
    let Ok(url) = Url::parse(&format!("http://localhost{target}")) else {
        return CallbackRequest::Elsewhere;
    };
    if url.path() != "/auth/callback" {
        return CallbackRequest::Elsewhere;
    }
    if query(&url, "state").as_deref() != Some(state) {
        return CallbackRequest::Foreign;
    }
    if let Some(error) = query(&url, "error") {
        // With this sign-in's state it comes from the provider; still only
        // printable text, and not much of it.
        let printable = |text: &str| -> String {
            text.chars()
                .filter(|character| character.is_ascii_graphic() || *character == ' ')
                .take(300)
                .collect()
        };
        let description = query(&url, "error_description")
            .map(|text| printable(&text))
            .filter(|text| !text.trim().is_empty());
        return CallbackRequest::Refused(match description {
            Some(description) => format!("{description} ({})", printable(&error)),
            None => printable(&error),
        });
    }
    match query(&url, "code") {
        Some(code) if !code.is_empty() => CallbackRequest::Code(code),
        _ => CallbackRequest::Refused("missing authorization code".into()),
    }
}

fn query(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

async fn respond(mut stream: TcpStream, status: &str, body: &str) -> AuthResult<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|error| AuthError::OpenAi(format!("callback response failed: {error}")))
}

fn success_page() -> String {
    "<!doctype html><meta charset=utf-8><title>Haarness connected</title><style>body{background:#0b0d10;color:#e8eaed;font:16px system-ui;display:grid;place-items:center;min-height:100vh;margin:0}main{padding:32px;background:#15181d;border-radius:12px}b{color:#45d483}</style><main><b>ChatGPT connected.</b><p>You can close this window and return to Haarness.</p></main>".into()
}

fn error_page(error: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><title>Haarness sign-in failed</title><style>body{{background:#0b0d10;color:#e8eaed;font:16px system-ui;display:grid;place-items:center;min-height:100vh;margin:0}}main{{padding:32px;background:#15181d;border-radius:12px}}b{{color:#ff6b6b}}</style><main><b>Sign-in failed.</b><p>{}</p></main>",
        error
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    )
}

fn random_url_token(length: usize) -> String {
    let mut bytes = vec![0_u8; length];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: String,
    user_code: String,
    interval: String,
}

#[derive(Deserialize)]
struct DeviceTokenResponse {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Deserialize)]
struct XaiDeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct CopilotDeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    id_token: Option<String>,
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

impl TokenResponse {
    fn account_id(&self) -> Option<String> {
        self.id_token
            .as_deref()
            .and_then(jwt_claims)
            .and_then(account_id)
            .or_else(|| jwt_claims(&self.access_token).and_then(account_id))
    }

    fn credential(&self, account_id: Option<String>, old_refresh: Option<&str>) -> Credential {
        Credential::oauth(
            self.access_token.clone(),
            self.refresh_token
                .clone()
                .or_else(|| old_refresh.map(str::to_owned))
                .unwrap_or_default(),
            unix_millis().saturating_add(self.expires_in.unwrap_or(3600) * 1000),
            account_id,
        )
    }
}

fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(token.split('.').nth(1)?).ok()?).ok()
}

fn account_id(claims: serde_json::Value) -> Option<String> {
    claims
        .get("chatgpt_account_id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            claims
                .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            claims
                .get("organizations")
                .and_then(serde_json::Value::as_array)
                .and_then(|organizations| organizations.first())
                .and_then(|organization| organization.get("id"))
                .and_then(serde_json::Value::as_str)
        })
        .map(str::to_owned)
}

#[must_use]
pub fn openai_residency(access_token: &str) -> Option<String> {
    let claims = jwt_claims(access_token)?;
    let residency = claims
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_compute_residency")
        .or_else(|| claims.get("chatgpt_compute_residency"))
        .and_then(serde_json::Value::as_str)?;
    (residency != "no_constraint").then(|| residency.to_owned())
}

#[must_use]
pub fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

async fn decode_response<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
    operation: &str,
) -> AuthResult<T> {
    if !response.status().is_success() {
        return Err(response_error(response, operation).await);
    }
    response
        .json()
        .await
        .map_err(|error| AuthError::OpenAi(format!("{operation} returned invalid JSON: {error}")))
}

async fn response_error(response: reqwest::Response, operation: &str) -> AuthError {
    let status = response.status();
    let detail = response
        .json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|value| {
            value
                .get("error_description")
                .or_else(|| value.get("error"))
                .and_then(|value| value.as_str().map(str::to_owned))
        });
    AuthError::OpenAi(match detail {
        Some(detail) => format!("{operation} returned {status}: {detail}"),
        None => format!("{operation} returned {status}"),
    })
}

async fn xai_decode_response<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
    operation: &str,
) -> AuthResult<T> {
    if !response.status().is_success() {
        return Err(xai_response_error(response, operation).await);
    }
    response
        .json()
        .await
        .map_err(|error| AuthError::Xai(format!("{operation} returned invalid JSON: {error}")))
}

async fn xai_response_error(response: reqwest::Response, operation: &str) -> AuthError {
    let status = response.status();
    let value = response
        .json::<serde_json::Value>()
        .await
        .unwrap_or_default();
    let detail = value
        .get("error_description")
        .and_then(serde_json::Value::as_str)
        .or_else(|| value.get("error").and_then(serde_json::Value::as_str));
    AuthError::Xai(match detail {
        Some(detail) => format!("{operation} returned {status}: {detail}"),
        None => format!("{operation} returned {status}"),
    })
}

fn request_error(error: reqwest::Error) -> AuthError {
    AuthError::OpenAi(format!("request failed: {error}"))
}

fn xai_request_error(error: reqwest::Error) -> AuthError {
    AuthError::Xai(format!("request failed: {error}"))
}

async fn copilot_decode_response<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
    operation: &str,
) -> AuthResult<T> {
    let status = response.status();
    let body = response.bytes().await.map_err(copilot_request_error)?;
    if !status.is_success() {
        let detail = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|value| {
                value
                    .get("error_description")
                    .or_else(|| value.get("error"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "request failed".into());
        return Err(AuthError::Copilot(format!(
            "{operation} returned {status}: {detail}"
        )));
    }
    serde_json::from_slice(&body)
        .map_err(|error| AuthError::Copilot(format!("{operation} returned invalid JSON: {error}")))
}

fn copilot_request_error(error: reqwest::Error) -> AuthError {
    AuthError::Copilot(format!("request failed: {error}"))
}

#[cfg(test)]
mod tests {
    use axum::{Json, Router, routing::post};
    use base64::Engine as _;
    use tokio::net::TcpListener;

    use super::{
        CallbackRequest, CopilotOAuthClient, CopilotOAuthConfig, Credential, CredentialStore,
        OpenAiOAuthClient, OpenAiOAuthConfig, openai_residency, query, read_callback,
    };

    #[tokio::test]
    async fn browser_login_uses_the_haarness_pkce_contract() {
        let probe = std::net::TcpListener::bind(("localhost", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let login = OpenAiOAuthClient::new(OpenAiOAuthConfig {
            issuer: "https://auth.openai.com".into(),
            client_id: "test-client".into(),
            codex_endpoint: "https://chatgpt.example/responses".into(),
            callback_port: port,
            user_agent: "haarness-test".into(),
        })
        .start_browser_login()
        .await
        .unwrap();
        assert_eq!(login.url.path(), "/oauth/authorize");
        assert_eq!(
            query(&login.url, "client_id").as_deref(),
            Some("test-client")
        );
        assert_eq!(
            query(&login.url, "code_challenge_method").as_deref(),
            Some("S256")
        );
        assert_eq!(query(&login.url, "originator").as_deref(), Some("opencode"));
        assert_eq!(
            query(&login.url, "codex_cli_simplified_flow").as_deref(),
            Some("true")
        );
        let redirect = format!("http://localhost:{port}/auth/callback");
        assert_eq!(
            query(&login.url, "redirect_uri").as_deref(),
            Some(redirect.as_str())
        );
        drop(login);
    }

    #[test]
    fn only_a_callback_with_this_sign_ins_state_ends_it() {
        for target in [
            "/",
            "/favicon.ico",
            "/auth/callbackx?state=st-1",
            "not a target",
        ] {
            assert_eq!(
                read_callback(target, "st-1"),
                CallbackRequest::Elsewhere,
                "{target}"
            );
        }
        for target in [
            "/auth/callback?error=access_denied&error_description=Pwned%20by%20a%20page",
            "/auth/callback?code=c&state=other",
            "/auth/callback?code=c",
        ] {
            assert_eq!(
                read_callback(target, "st-1"),
                CallbackRequest::Foreign,
                "{target}"
            );
        }
        assert_eq!(
            read_callback("/auth/callback?code=c-1&state=st-1", "st-1"),
            CallbackRequest::Code("c-1".into())
        );
        assert_eq!(
            read_callback(
                "/auth/callback?state=st-1&error=access_denied&error_description=User%20said%20no%3Cb%3E%0A",
                "st-1"
            ),
            CallbackRequest::Refused("User said no<b> (access_denied)".into())
        );
        assert_eq!(
            read_callback("/auth/callback?state=st-1", "st-1"),
            CallbackRequest::Refused("missing authorization code".into())
        );
    }

    /// Port 1455 is fixed, so a web page can reach it: its requests are
    /// answered and the sign-in keeps waiting for the real return.
    #[tokio::test]
    async fn a_web_page_cannot_end_a_browser_sign_in() {
        let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let login = OpenAiOAuthClient::new(OpenAiOAuthConfig {
            callback_port: port,
            ..OpenAiOAuthConfig::default()
        })
        .start_browser_login()
        .await
        .unwrap();
        let state = query(&login.url, "state").unwrap();
        let waiting = tokio::spawn(login.finish());
        let client = reqwest::Client::new();
        for forged in [
            "auth/callback?error=access_denied&error_description=PAGE_TEXT".to_owned(),
            "auth/callback?code=PAGE_CODE&state=PAGE_STATE".to_owned(),
            "favicon.ico".to_owned(),
        ] {
            let response = client
                .get(format!("http://127.0.0.1:{port}/{forged}"))
                .send()
                .await
                .unwrap();
            assert!(response.status().is_client_error(), "{forged}");
            assert!(
                !response.text().await.unwrap().contains("PAGE_"),
                "{forged}"
            );
        }
        // A connection that never sends a request holds nothing up.
        let silent = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!waiting.is_finished(), "a forged request ended the sign-in");
        drop(silent);
        let response = client
            .get(format!(
                "http://127.0.0.1:{port}/auth/callback?state={state}&error=access_denied"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        let error = waiting.await.unwrap().err().unwrap().to_string();
        assert!(error.contains("access_denied"), "{error}");
    }

    /// Idle connections (a page's preconnect, a browser's spare socket) are
    /// read alongside the real return, which they do not hold up.
    #[tokio::test]
    async fn idle_connections_do_not_delay_the_browser_return() {
        let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let login = OpenAiOAuthClient::new(OpenAiOAuthConfig {
            callback_port: port,
            ..OpenAiOAuthConfig::default()
        })
        .start_browser_login()
        .await
        .unwrap();
        let state = query(&login.url, "state").unwrap();
        let waiting = tokio::spawn(login.finish());
        let mut idle = Vec::new();
        for _ in 0..3 {
            idle.push(
                tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .unwrap(),
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let started = std::time::Instant::now();
        let response = reqwest::get(format!(
            "http://127.0.0.1:{port}/auth/callback?state={state}&error=access_denied"
        ))
        .await
        .unwrap();
        assert_eq!(response.status(), 400);
        let error = tokio::time::timeout(std::time::Duration::from_secs(3), waiting)
            .await
            .expect("the return is read while idle connections wait")
            .unwrap()
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("access_denied"), "{error}");
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        drop(idle);
    }

    #[tokio::test]
    async fn a_cancelled_browser_login_releases_its_callback_port() {
        let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let client = OpenAiOAuthClient::new(OpenAiOAuthConfig {
            callback_port: port,
            ..OpenAiOAuthConfig::default()
        });
        let login = client.start_browser_login().await.unwrap();
        // Cancelling a sign-in drops the future waiting for it.
        let waiting = tokio::spawn(login.finish());
        tokio::task::yield_now().await;
        waiting.abort();
        let _ = waiting.await;
        let again = client.start_browser_login().await;
        assert!(
            again.is_ok(),
            "the retry gets the port: {:?}",
            again.as_ref().err()
        );

        // A port someone else keeps says so in words.
        let held = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        let error = OpenAiOAuthClient::new(OpenAiOAuthConfig {
            callback_port: port,
            ..OpenAiOAuthConfig::default()
        })
        .start_browser_login()
        .await
        .err()
        .unwrap()
        .to_string();
        assert!(
            error.contains(&format!("another sign-in is using port {port}")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn copilot_device_login_uses_githubs_public_contract() {
        async fn device(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
            assert_eq!(body["client_id"], "fixture-client");
            assert_eq!(body["scope"], "read:user");
            Json(serde_json::json!({
                "device_code":"device-1",
                "user_code":"ABCD-EFGH",
                "verification_uri":"https://github.example/device",
                "expires_in":60,
                "interval":1
            }))
        }
        async fn token(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
            assert_eq!(body["client_id"], "fixture-client");
            assert_eq!(body["device_code"], "device-1");
            assert_eq!(body["grant_type"], super::COPILOT_DEVICE_GRANT);
            Json(serde_json::json!({"access_token":"github-token"}))
        }
        let router = Router::new()
            .route("/login/device/code", post(device))
            .route("/login/oauth/access_token", post(token));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let login = CopilotOAuthClient::new(CopilotOAuthConfig {
            base_url: format!("http://{address}"),
            client_id: "fixture-client".into(),
            user_agent: "haarness-test".into(),
        })
        .start_device_login()
        .await
        .unwrap();
        assert_eq!(login.verification_url, "https://github.example/device");
        assert_eq!(login.user_code, "ABCD-EFGH");
        assert_eq!(
            login.finish().await.unwrap().api_key(),
            Some("github-token")
        );
    }

    #[test]
    fn records_round_trip_with_haarness_names() {
        let api: Credential = serde_json::from_str(r#"{"type":"api","key":"sk-1"}"#).unwrap();
        assert_eq!(api.api_key(), Some("sk-1"));
        assert_eq!(
            serde_json::to_value(api).unwrap(),
            serde_json::json!({"type":"api","key":"sk-1"})
        );
        let oauth: Credential = serde_json::from_str(
            r#"{"type":"oauth","access":"a","refresh":"r","expires":123,"account_id":"acc"}"#,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(oauth).unwrap(),
            serde_json::json!({"type":"oauth","access":"a","refresh":"r","expires":123,"accountId":"acc"})
        );
    }

    #[test]
    fn late_refresh_cannot_restore_logout() {
        let root = std::env::temp_dir().join(format!(
            "haarness-auth-{}-{}",
            std::process::id(),
            super::unix_millis()
        ));
        let store = CredentialStore::new(root.join("auth.json"));
        store
            .set(
                "openai",
                &Credential::oauth("old".into(), "r1".into(), 1, None),
            )
            .unwrap();
        store.remove("openai").unwrap();
        assert!(
            !store
                .replace_oauth_if_current(
                    "openai",
                    "r1",
                    &Credential::oauth("new".into(), "r2".into(), 2, None)
                )
                .unwrap()
        );
        assert!(store.get("openai").unwrap().is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn extracts_compute_residency() {
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({
                "https://api.openai.com/auth": {"chatgpt_compute_residency": "eu"}
            }))
            .unwrap(),
        );
        assert_eq!(
            openai_residency(&format!("x.{claims}.y")).as_deref(),
            Some("eu")
        );
    }
}
