//! First-party account sessions shared by the terminal, Desktop and Cloud CLI.
//! No project configuration or remote runtime is allowed to choose the issuer.
pub mod billing;
pub mod cloud;
pub mod credentials;
pub mod loopback;
pub mod pkce;
pub mod renewals;
pub mod routers;
pub mod websocket;

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
/// Production Haarness Cloud.
pub const PRODUCTION_API_URL: &str = "https://haarness.ai";
/// The local Cloud stack that `haarness-cloud/scripts/dev-up.sh` starts. Building with
/// `HAARNESS_CLOUD_DEV_API_URL` (and `HAARNESS_CLOUD_DEV_WEB_ORIGIN` for a separate web dev
/// server) bakes in another local stack.
pub const LOCAL_API_URL: &str = match option_env!("HAARNESS_CLOUD_DEV_API_URL") {
    Some(url) => url,
    None => "http://127.0.0.1:18080",
};
const LOCAL_WEB_ORIGIN: Option<&str> = option_env!("HAARNESS_CLOUD_DEV_WEB_ORIGIN");
/// Where a build signs in unless `HAARNESS_CLOUD_API_URL` says otherwise. Development builds
/// use the local stack, so trying a change never touches production accounts or billing;
/// release builds use production.
pub const DEFAULT_API_URL: &str = if cfg!(debug_assertions) {
    LOCAL_API_URL
} else {
    PRODUCTION_API_URL
};
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

impl Error {
    /// Whether trying again later may succeed: Cloud or the network on the
    /// way was unreachable or overloaded (408, 429, 5xx), or something other
    /// than Cloud answered (a proxy's or captive portal's page, which is not
    /// Cloud's JSON). A refused sign-in, a rejected key, a request Cloud
    /// turned down and local file problems are not.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Http(_) | Self::Json(_) => true,
            Self::Api { status, .. } => matches!(*status, 408 | 429 | 500..),
            Self::NotLoggedIn | Self::Config(_) | Self::Other(_) | Self::Io(_) => false,
        }
    }
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
        let api_url = safe_url(
            &std::env::var("HAARNESS_CLOUD_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.into()),
        )?;
        let dir = std::env::var_os("HAARNESS_CLOUD_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HAARNESS_DATA_DIR")
                    .map(|p| PathBuf::from(p).join("config/account"))
            })
            .unwrap_or_else(|| {
                let home = std::env::var_os("HAARNESS_HOME")
                    .map(PathBuf::from)
                    .filter(|home| home.is_absolute());
                default_config_dir(&api_url, home.as_deref())
            });
        let mut config = Self::new(
            api_url.as_str(),
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
    /// Where the Cloud web app lives. Production and the local stack serve it
    /// from the API origin; a separate web dev server is named in the
    /// environment, or baked into a development build next to its API.
    pub fn web_origin(&self) -> Result<Url> {
        if let Ok(url) = std::env::var("HAARNESS_CLOUD_WEB_ORIGIN") {
            return safe_url(&url);
        }
        match LOCAL_WEB_ORIGIN {
            Some(origin) if self.api_url == safe_url(LOCAL_API_URL)? => safe_url(origin),
            _ => Ok(self.api_url.clone()),
        }
    }
}

/// `~/.config/haarness-cloud` for production, or `config/account` inside a
/// `HAARNESS_HOME` install, which shares nothing with the user's own. Any
/// other Cloud service keeps its sign-in in a folder of its own, so signing
/// in to a local stack never signs a production install out, and switching
/// back needs no new sign-in.
fn default_config_dir(api_url: &Url, haarness_home: Option<&Path>) -> PathBuf {
    let base = match haarness_home {
        Some(home) => home.join("config").join("account"),
        None => std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| user_home().join(".config"))
            .join("haarness-cloud"),
    };
    if safe_url(PRODUCTION_API_URL).is_ok_and(|production| production == *api_url) {
        return base;
    }
    base.join("services").join(service_folder(api_url))
}
/// The user's home folder. Windows sets USERPROFILE, not HOME; with neither,
/// an empty path would put sign-in files inside whatever folder the process
/// started in (Desktop starts the CLI in the project).
fn user_home() -> PathBuf {
    haarness_protocol::home_dir()
        .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()))
        .unwrap_or_else(std::env::temp_dir)
}
/// `127.0.0.1-18080`, `staging.haarness.ai`: the host and any explicit port,
/// reduced to characters every file system accepts.
fn service_folder(api_url: &Url) -> String {
    let host = api_url
        .host_str()
        .unwrap_or("cloud")
        .trim_matches(['[', ']']);
    let name = match api_url.port() {
        Some(port) => format!("{host}-{port}"),
        None => host.to_owned(),
    };
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
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
        // While Cloud restarts or is overloaded a load balancer or proxy may
        // answer with its own page: keep the status, so a passing outage is
        // not taken for a refused sign-in.
        let unavailable = || Error::Api {
            status: status.as_u16(),
            message: format!(
                "Haarness Cloud could not renew the sign-in right now (HTTP {}). Retry shortly.",
                status.as_u16()
            ),
        };
        let bytes = response.bytes().await?;
        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) if !status.is_success() => return Err(unavailable()),
            Err(error) => return Err(error.into()),
        };
        if !status.is_success() {
            if value["error"] == "invalid_grant" {
                return Err(Error::NotLoggedIn);
            }
            if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
                return Err(unavailable());
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
        // Cloud spends a refresh token the moment it answers, and revokes the
        // whole sign-in when a spent one comes back. A caller that gives up
        // mid-refresh (a Stop, a timeout) must not drop the new pair before it
        // is saved, so the renewal runs on its own task (see [`renewals`]):
        // the caller returns at once, and the task still saves what Cloud
        // sent. Its requests time out after 30 s, which bounds it.
        let client = self.clone();
        let rejected = rejected.map(str::to_owned);
        renewals::spawn(async move { client.current_bearer(rejected.as_deref()).await })
            .await
            .map_err(|_| {
                Error::Other("Renewing the Haarness Cloud sign-in stopped unexpectedly".into())
            })?
    }

    /// [`Self::bearer`]'s work under the credential lock: the saved access
    /// token, renewed first when it expired or `rejected` names it.
    async fn current_bearer(&self, rejected: Option<&str>) -> Result<String> {
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
                "defaultAccountId":key["accountId"],"selectedAccountId":key["accountId"],"scopes":key["scopes"],"selectedRouterId":self.config.selected_router()?}),
            );
        }
        match self.request(Method::GET, "api/v1/me", None).await {
            Ok(mut me) => {
                me["loggedIn"] = json!(true);
                me["apiUrl"] = json!(self.config.api_url);
                me["selectedRouterId"] = json!(self.config.selected_router()?);
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
        let device_label = device_label(
            std::env::var(LOGIN_APP_ENV).is_ok_and(|app| app == "desktop"),
            &computer_name(),
        );
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
            // The link is printed and opened in a browser: one that is not
            // http or https (a local file, an app's own scheme) stops here,
            // and the parsed address is used, so no control character in it
            // or in the code reaches a terminal.
            let url = Url::parse(&value.verification_uri_complete)
                .ok()
                .filter(|url| matches!(url.scheme(), "http" | "https") && url.has_host())
                .ok_or_else(|| {
                    Error::Other(
                        "The sign-in server sent a link that is not http or https, so sign-in stopped."
                            .into(),
                    )
                })?;
            if value.user_code.contains(char::is_control) {
                return Err(Error::Other(
                    "The sign-in server sent a code with control characters, so sign-in stopped."
                        .into(),
                ));
            }
            return Ok(Login {
                url: url.to_string(),
                code: Some(value.user_code.clone()),
                generation,
                device_label,
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
            ("code_challenge_method", pkce::CHALLENGE_METHOD),
        ]);
        Ok(Login {
            url: url.to_string(),
            code: None,
            generation,
            device_label,
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
        let router_id = self.config.selected_router()?;
        if let Some(key) = self.config.api_key() {
            let context = self.request(Method::GET, "api/v1/key", None).await?;
            let selected = if let Some(id) = &router_id {
                self.request(Method::GET, &format!("api/v1/routers/{id}"), None)
                    .await?
            } else {
                context.clone()
            };
            let router_url = selected["routerUrl"]
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
                Some(json!({"accountId":self.config.selected_account(),"routerId":router_id})),
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
/// Set to `desktop` by Desktop when it runs `haarness login`, so the sign-in
/// is named after Desktop rather than the CLI.
pub const LOGIN_APP_ENV: &str = "HAARNESS_LOGIN_APP";
/// The most Cloud keeps of a sign-in's name (`cloud_domain::push::MAX_DEVICE_LABEL`).
const MAX_DEVICE_LABEL: usize = 100;

/// How Settings → Devices in Cloud names a sign-in from this computer:
/// `Haarness CLI on Studio`, or `Haarness Desktop on Studio` when Desktop signs
/// in through the CLI.
pub fn device_label(desktop: bool, computer: &str) -> String {
    let app = if desktop {
        "Haarness Desktop"
    } else {
        "Haarness CLI"
    };
    format!("{app} on {}", computer.trim())
        .chars()
        .take(MAX_DEVICE_LABEL)
        .collect()
}

/// The name Cloud shows for this computer, next to its sign-ins and its
/// shared chats: the macOS computer name, else the host name.
pub fn computer_name() -> String {
    #[cfg(target_os = "macos")]
    if let Ok(output) = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
    {
        let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if output.status.success() && !name.is_empty() {
            return name.chars().take(MAX_DEVICE_LABEL).collect();
        }
    }
    ["COMPUTERNAME", "HOSTNAME"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .chain(fs::read_to_string("/etc/hostname").ok())
        .map(|name| name.trim().to_owned())
        .find(|name| !name.is_empty())
        .map_or_else(
            || "My computer".into(),
            |name| name.chars().take(MAX_DEVICE_LABEL).collect(),
        )
}

pub struct Login {
    pub url: String,
    pub code: Option<String>,
    generation: String,
    device_label: String,
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
                        ("device_label", &self.device_label),
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
                            ("device_label", &self.device_label),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn development_builds_sign_in_to_the_local_stack() {
        // Tests build with debug assertions, like `cargo build`.
        assert_eq!(DEFAULT_API_URL, LOCAL_API_URL);
        assert!(safe_url(LOCAL_API_URL).is_ok());
        assert!(safe_url(PRODUCTION_API_URL).is_ok());
    }

    #[test]
    fn sign_ins_are_named_after_the_app_and_the_computer() {
        assert_eq!(device_label(false, "Studio"), "Haarness CLI on Studio");
        assert_eq!(
            device_label(true, " Studio\n"),
            "Haarness Desktop on Studio"
        );
        // Cloud keeps 100 characters; the label never runs past them.
        let long = device_label(true, &"é".repeat(120));
        assert_eq!(long.chars().count(), MAX_DEVICE_LABEL);
        assert!(long.starts_with("Haarness Desktop on éé"), "{long}");
        let computer = computer_name();
        assert!(!computer.trim().is_empty());
        assert!(computer.chars().count() <= MAX_DEVICE_LABEL);
    }

    #[test]
    fn each_cloud_service_keeps_its_own_sign_in() {
        let production = default_config_dir(&safe_url(PRODUCTION_API_URL).unwrap(), None);
        assert!(production.ends_with("haarness-cloud"), "{production:?}");
        let local = default_config_dir(&safe_url("http://127.0.0.1:18080").unwrap(), None);
        assert!(
            local.ends_with("haarness-cloud/services/127.0.0.1-18080"),
            "{local:?}"
        );
        assert_eq!(local.parent().unwrap().parent().unwrap(), production);
        // An isolated HAARNESS_HOME install never reads the user's own sign-in.
        let home = Path::new("/tmp/isolated");
        assert_eq!(
            default_config_dir(&safe_url(PRODUCTION_API_URL).unwrap(), Some(home)),
            home.join("config/account")
        );
        assert_eq!(
            default_config_dir(&safe_url("http://127.0.0.1:29080").unwrap(), Some(home)),
            home.join("config/account/services/127.0.0.1-29080")
        );
        assert_eq!(
            service_folder(&safe_url("https://staging.haarness.ai").unwrap()),
            "staging.haarness.ai"
        );
        assert_eq!(
            service_folder(&safe_url("http://[::1]:29080").unwrap()),
            "__1-29080"
        );
    }
}
