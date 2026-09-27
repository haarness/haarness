//! First-party account sessions shared by the terminal, Desktop and Cloud CLI.
//! No project configuration or remote runtime is allowed to choose the issuer.
pub mod billing;
pub mod cloud;
pub mod credentials;
pub mod loopback;
pub mod pkce;

use chrono::{DateTime, Utc};
pub use credentials::Credentials;
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use url::Url;

pub const SCOPE: &str = "account workspaces billing keys offline_access";
pub const CLIENT_ID: &str = "haarness-cli";
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Sign in to Haarness to continue. Run `haarness login`.")]
    NotLoggedIn,
    #[error("{0}")]
    Config(String),
    #[error("{0}")]
    Other(String),
    #[error("{message}")]
    Api { status: u16, message: String },
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("Could not reach Haarness Cloud. Check your connection and retry: {0}")]
    Http(#[from] reqwest::Error),
}

#[derive(Clone, Debug)]
pub struct Config {
    pub api_url: Url,
    pub config_dir: PathBuf,
    pub client_id: String,
    api_key: Option<ConfiguredApiKey>,
}
#[derive(Clone)]
struct ConfiguredApiKey(String);
impl std::fmt::Debug for ConfiguredApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted API key]")
    }
}
impl Config {
    pub fn from_env() -> Result<Self> {
        let dir = std::env::var_os("HAARNESS_CLOUD_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HAARNESS_DATA_DIR")
                    .map(|p| PathBuf::from(p).join("config/account"))
            })
            .unwrap_or_else(|| {
                std::env::var_os("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
                    })
                    .join("haarness-cloud")
            });
        let mut config = Self::new(
            &std::env::var("HAARNESS_CLOUD_API_URL")
                .unwrap_or_else(|_| "https://app.haarness.dev".into()),
            dir,
            &std::env::var("HAARNESS_CLOUD_CLIENT_ID").unwrap_or_else(|_| CLIENT_ID.into()),
        )?;
        if let Ok(key) = std::env::var("HAARNESS_API_KEY") {
            config = config.with_api_key(key)?;
        }
        Ok(config)
    }
    pub fn new(api: &str, config_dir: PathBuf, client_id: &str) -> Result<Self> {
        let api_url = safe_url(api)?;
        Ok(Self {
            api_url,
            config_dir,
            client_id: client_id.into(),
            api_key: None,
        })
    }
    pub fn path(&self) -> PathBuf {
        self.config_dir.join("credentials.json")
    }
    pub fn signed_in(&self) -> bool {
        self.api_key.is_some() || self.path().is_file()
    }
    /// Explicit environment/API credentials never replace a stored browser login.
    pub fn with_api_key(mut self, key: String) -> Result<Self> {
        if !(key.starts_with("hck_") || key.starts_with("hcr_"))
            || key.bytes().any(|b| b.is_ascii_whitespace())
        {
            return Err(Error::Config(
                "HAARNESS_API_KEY must be a Haarness API key".into(),
            ));
        }
        self.api_key = Some(ConfiguredApiKey(key));
        Ok(self)
    }
    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_ref().map(|key| key.0.as_str())
    }
    pub fn selected_account(&self) -> Option<String> {
        if self.api_key.is_some() {
            return None;
        }
        fs::read_to_string(self.config_dir.join("account.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .filter(|v| v["apiUrl"].as_str() == Some(self.api_url.as_str()))
            .and_then(|v| v["accountId"].as_str().map(str::to_owned))
    }
    pub async fn lock(&self) -> Result<fs::File> {
        credential_lock(&self.path()).await
    }
}

/// Only explicit local development uses HTTP. Never follow token-bearing redirects.
pub fn safe_url(value: &str) -> Result<Url> {
    let mut url = Url::parse(value).map_err(|_| Error::Config("Invalid Cloud URL".into()))?;
    if url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https"
            || (url.scheme() == "http"
                && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))))
    {
        return Err(Error::Config(
            "Cloud requires HTTPS or a local development address".into(),
        ));
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

/// The descriptor stays alive through load, remote rotation and atomic save.
/// `spawn_blocking` prevents a contended OS lock from blocking the async executor.
pub async fn credential_lock(path: &Path) -> Result<fs::File> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let dir = path
            .parent()
            .ok_or_else(|| Error::Config("Missing credential directory".into()))?;
        if !dir.exists() {
            fs::create_dir_all(dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
            }
        }
        let mut options = fs::OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path.with_extension("lock"))?;
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(file)
    })
    .await
    .map_err(|e| Error::Other(e.to_string()))?
}

#[derive(Clone)]
pub struct Client {
    pub config: Config,
    http: reqwest::Client,
}
#[derive(Deserialize)]
struct Tokens {
    access_token: String,
    expires_in: i64,
    refresh_token: Option<String>,
}
impl Client {
    pub fn new(config: Config) -> Result<Self> {
        Ok(Self {
            config,
            http: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .user_agent("haarness-account/0.1")
                .build()?,
        })
    }
    fn endpoint(&self, path: &str) -> Result<Url> {
        self.config
            .api_url
            .join(path.trim_start_matches('/'))
            .map_err(|e| Error::Config(e.to_string()))
    }
    fn validate(&self, c: &Credentials) -> Result<()> {
        if c.api_url != self.config.api_url.as_str() {
            return Err(Error::Config(
                "Saved account belongs to another Cloud service. Sign in again.".into(),
            ));
        }
        Ok(())
    }
    async fn token(&self, form: &[(&str, &str)]) -> Result<Tokens> {
        let response = self
            .http
            .post(self.endpoint("oauth/token")?)
            .form(form)
            .send()
            .await?;
        let status = response.status();
        let value: Value = response.json().await?;
        if !status.is_success() {
            if value["error"] == "invalid_grant" {
                return Err(Error::NotLoggedIn);
            }
            return Err(Error::Other(
                value["error"].as_str().unwrap_or("Sign-in failed").into(),
            ));
        }
        Ok(serde_json::from_value(value)?)
    }
    fn pair(&self, t: Tokens) -> Result<Credentials> {
        Ok(Credentials {
            api_url: self.config.api_url.to_string(),
            access_token: t.access_token,
            access_expires_at: Utc::now() + chrono::Duration::seconds(t.expires_in),
            refresh_token: t
                .refresh_token
                .ok_or_else(|| Error::Other("Missing refresh token".into()))?,
        })
    }
    pub async fn bearer(&self, rejected: Option<&str>) -> Result<String> {
        if let Some(key) = self.config.api_key() {
            return if rejected.is_some() {
                Err(Error::Api {
                    status: 401,
                    message: "API key invalid, expired or revoked. Update HAARNESS_API_KEY.".into(),
                })
            } else {
                Ok(key.into())
            };
        }
        let _lock = self.config.lock().await?;
        let mut c = credentials::load(&self.config.path())?.ok_or(Error::NotLoggedIn)?;
        self.validate(&c)?;
        if c.access_expired(Utc::now(), chrono::Duration::seconds(30))
            || rejected == Some(c.access_token.as_str())
        {
            let result = self
                .token(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", &self.config.client_id),
                    ("refresh_token", &c.refresh_token),
                ])
                .await;
            match result {
                Ok(t) => {
                    c = self.pair(t)?;
                    credentials::save(&self.config.path(), &c)?;
                }
                Err(Error::NotLoggedIn) => {
                    credentials::delete(&self.config.path())?;
                    return Err(Error::NotLoggedIn);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(c.access_token)
    }
    pub async fn request(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let url = self.endpoint(path)?;
        let id = uuid::Uuid::now_v7().to_string();
        let mut bearer = self.bearer(None).await?;
        for attempt in 0..2 {
            let mut req = self
                .http
                .request(method.clone(), url.clone())
                .bearer_auth(&bearer);
            if let Some(body) = &body {
                req = req.json(body).header("Idempotency-Key", &id);
            }
            let response = req.send().await?;
            if response.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                bearer = self.bearer(Some(&bearer)).await?;
                continue;
            }
            let status = response.status();
            let bytes = response.bytes().await?;
            if status == StatusCode::UNAUTHORIZED {
                return Err(Error::NotLoggedIn);
            }
            let value: Value = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes)?
            };
            if !status.is_success() {
                return Err(Error::Api {
                    status: status.as_u16(),
                    message: value
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .or_else(|| value.get("message").and_then(Value::as_str))
                        .unwrap_or("Cloud request failed. Retry.")
                        .into(),
                });
            }
            return Ok(value);
        }
        Err(Error::NotLoggedIn)
    }
    pub async fn status(&self) -> Result<Value> {
        if !self.config.signed_in() {
            return Ok(json!({"loggedIn":false}));
        }
        if self.config.api_key().is_some() {
            let context = self.request(Method::GET, "api/v1/key", None).await?;
            let key = &context["key"];
            return Ok(
                json!({"loggedIn":true,"authKind":"api_key","apiUrl":self.config.api_url,
                "user":{"id":key["id"],"displayName":key["label"],"email":""},
                "accounts":[{"id":key["accountId"],"name":context["accountName"],"role":"api_key"}],
                "defaultAccountId":key["accountId"],"selectedAccountId":key["accountId"],"scopes":key["scopes"]}),
            );
        }
        match self.request(Method::GET, "api/v1/me", None).await {
            Ok(mut me) => {
                me["loggedIn"] = json!(true);
                me["apiUrl"] = json!(self.config.api_url);
                me["selectedAccountId"] =
                    json!(self.config.selected_account().unwrap_or_else(|| {
                        me["defaultAccountId"].as_str().unwrap_or_default().into()
                    }));
                Ok(me)
            }
            Err(Error::NotLoggedIn) => Ok(json!({"loggedIn":false})),
            Err(e) => Err(e),
        }
    }
    pub async fn select_account(&self, id: &str) -> Result<()> {
        if self.config.api_key().is_some() {
            return if self.status().await?["selectedAccountId"] == id {
                Ok(())
            } else {
                Err(Error::Other(
                    "API keys are bound to one account. Use a key for the other account.".into(),
                ))
            };
        }
        let me = self.request(Method::GET, "api/v1/me", None).await?;
        if !me["accounts"]
            .as_array()
            .is_some_and(|a| a.iter().any(|a| a["id"] == id))
        {
            return Err(Error::Other("Account unavailable".into()));
        }
        let _lock = self.config.lock().await?;
        fs::write(
            self.config.config_dir.join("account.json"),
            serde_json::to_vec(&json!({"apiUrl":self.config.api_url,"accountId":id}))?,
        )?;
        Ok(())
    }
    pub async fn logout(&self) -> Result<()> {
        if self.config.api_key().is_some() {
            return Err(Error::Other("Unset HAARNESS_API_KEY to stop using this key. Revoke it in Cloud to disable all uses.".into()));
        }
        let _lock = self.config.lock().await?;
        let _ = fs::remove_file(self.config.config_dir.join("login-generation"));
        let stored = credentials::load(&self.config.path())?;
        credentials::delete(&self.config.path())?;
        let _ = fs::remove_file(self.config.config_dir.join("account.json"));
        if let Some(c) = stored {
            self.validate(&c)?;
            let result = self
                .http
                .post(self.endpoint("oauth/revoke")?)
                .form(&[("token", c.refresh_token)])
                .send()
                .await;
            if !matches!(result, Ok(ref r) if r.status().is_success()) {
                return Err(Error::Other("Signed out on this device. Cloud was unreachable; revoke this session from your account’s Security page when online.".into()));
            }
        }
        Ok(())
    }
    pub async fn begin_login(&self, device: bool) -> Result<Login> {
        // Snapshot under the same lock: a logout or another completed login wins over a late callback.
        let _lock = self.config.lock().await?;
        let generation = pkce::random_urlsafe(16);
        fs::write(self.config.config_dir.join("login-generation"), &generation)?;
        if device {
            let response = self
                .http
                .post(self.endpoint("oauth/device_authorization")?)
                .form(&[
                    ("client_id", self.config.client_id.as_str()),
                    ("scope", SCOPE),
                ])
                .send()
                .await?
                .error_for_status()?;
            let value: Device = response.json().await?;
            return Ok(Login {
                url: value.verification_uri_complete.clone(),
                code: Some(value.user_code.clone()),
                generation,
                flow: Flow::Device(value),
                client: self.clone(),
            });
        }
        let callback = loopback::Loopback::bind().await?;
        let redirect = callback.redirect_uri();
        let pkce = pkce::Pkce::generate();
        let state = pkce::random_urlsafe(32);
        let mut url = self.endpoint("oauth/authorize")?;
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", self.config.client_id.as_str()),
            ("redirect_uri", &redirect),
            ("scope", SCOPE),
            ("state", &state),
            ("code_challenge", &pkce.challenge),
            ("code_challenge_method", "S256"),
        ]);
        Ok(Login {
            url: url.to_string(),
            code: None,
            generation,
            flow: Flow::Browser {
                callback,
                redirect,
                verifier: pkce.verifier,
                state,
            },
            client: self.clone(),
        })
    }
    pub async fn router_session(&self) -> Result<RouterSession> {
        if let Some(key) = self.config.api_key() {
            let context = self.request(Method::GET, "api/v1/key", None).await?;
            let router_url = context["routerUrl"]
                .as_str()
                .ok_or_else(|| Error::Other("Cloud did not return a model endpoint".into()))?;
            safe_url(router_url)?;
            let expires = context["key"]["expiresAt"]
                .as_str()
                .and_then(|value| value.parse::<DateTime<Utc>>().ok())
                .unwrap_or_else(|| Utc::now() + chrono::Duration::minutes(5));
            return Ok(RouterSession {
                token: key.into(),
                router_url: router_url.into(),
                expires_at: expires.min(Utc::now() + chrono::Duration::minutes(5)),
            });
        }
        Ok(serde_json::from_value(
            self.request(
                Method::POST,
                "api/v1/account/router-session",
                Some(json!({"accountId":self.config.selected_account()})),
            )
            .await?,
        )?)
    }
}

#[derive(Deserialize)]
pub struct RouterSession {
    pub token: String,
    #[serde(rename = "routerUrl")]
    pub router_url: String,
    #[serde(rename = "expiresAt")]
    pub expires_at: DateTime<Utc>,
}
#[derive(Deserialize)]
struct Device {
    device_code: String,
    user_code: String,
    verification_uri_complete: String,
    expires_in: u64,
    interval: u64,
}
enum Flow {
    Browser {
        callback: loopback::Loopback,
        redirect: String,
        verifier: String,
        state: String,
    },
    Device(Device),
}
pub struct Login {
    pub url: String,
    pub code: Option<String>,
    generation: String,
    flow: Flow,
    client: Client,
}
impl Login {
    pub async fn finish(self) -> Result<()> {
        let client = &self.client;
        let tokens = match self.flow {
            Flow::Browser {
                callback,
                redirect,
                verifier,
                state,
            } => {
                let result = callback.wait_for_callback(&state).await?;
                client
                    .token(&[
                        ("grant_type", "authorization_code"),
                        ("client_id", &client.config.client_id),
                        ("code", &result.code),
                        ("redirect_uri", &redirect),
                        ("code_verifier", &verifier),
                    ])
                    .await?
            }
            Flow::Device(device) => {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(device.expires_in);
                let mut interval = device.interval.max(1);
                loop {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(Error::Other("Sign-in expired. Try again.".into()));
                    }
                    tokio::time::sleep(Duration::from_secs(interval)).await;
                    match client
                        .token(&[
                            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                            ("client_id", &client.config.client_id),
                            ("device_code", &device.device_code),
                        ])
                        .await
                    {
                        Ok(t) => break t,
                        Err(Error::Other(e)) if e == "authorization_pending" => {}
                        Err(Error::Other(e)) if e == "slow_down" => interval += 5,
                        Err(e) => return Err(e),
                    }
                }
            }
        };
        let c = client.pair(tokens)?;
        let _lock = client.config.lock().await?;
        if fs::read_to_string(client.config.config_dir.join("login-generation"))
            .ok()
            .as_deref()
            != Some(&self.generation)
        {
            let _ = client
                .http
                .post(client.endpoint("oauth/revoke")?)
                .form(&[("token", &c.refresh_token)])
                .send()
                .await;
            return Err(Error::Other("Sign-in cancelled or replaced".into()));
        }
        credentials::save(&client.config.path(), &c)?;
        let _ = fs::remove_file(client.config.config_dir.join("account.json"));
        Ok(())
    }
}
