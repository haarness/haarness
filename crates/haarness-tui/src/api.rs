//! Strict server client: every piece of state comes from REST/SSE.

use anyhow::{Context as _, Result};
use futures::{SinkExt, StreamExt};
use haarness_protocol::{
    AgentProfile, BtwEvent, BtwRequest, BtwResponse, BtwTurn, CommandExecutionResponse,
    CompactRequest, CreateThreadRequest, EventEnvelope, ExecuteCommandRequest, FileDiff, Item,
    LazyToolSettingsUpdate, McpServerInfo, PermissionReply, PtyCreateRequest, PtyId, PtyInfo,
    PtyStatus, PtyUpdateRequest, QuestionReply, ReviewFile, SessionArchive,
    SessionArchiveImportRequest, SessionArchiveImportResult, SkillInfo, StartTurnRequest, Thread,
    ToolCatalogResponse, ToolSettingsUpdate, Turn, TurnId, UpdateThreadRequest,
};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use std::time::Duration;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
};
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;

/// A loaded review: the files to show, plus what the server had to leave out
/// to stay inside its bounds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Review {
    pub files: Vec<FileDiff>,
    /// Changed paths the server did not return at all. `None` means the
    /// review is complete.
    pub omitted_files: Option<u64>,
}

/// Turn a failed review request into something the reader can act on. The
/// interesting distinction is between "the server is not there" and "the
/// server is there and this review is too big for one request".
fn review_error(error: reqwest::Error) -> anyhow::Error {
    if error.is_timeout() {
        return anyhow::anyhow!(
            "no response within {}s — this working tree is too large to review in one request; commit or stash unrelated changes, or narrow the review",
            REVIEW_TIMEOUT.as_secs()
        );
    }
    if error.is_connect() {
        return anyhow::anyhow!("cannot reach the Haarness server — is the daemon still running?");
    }
    if error.is_decode() {
        return anyhow::anyhow!("the server sent a review this client could not read: {error}");
    }
    anyhow::Error::new(error).context("review request failed")
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LspStatus {
    pub id: String,
    pub status: String,
    pub error: Option<String>,
    /// The files it serves (`.sh`, `.bash`).
    #[serde(default)]
    pub extensions: Vec<String>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatterStatus {
    pub id: String,
    pub status: String,
}

/// Worktree creation/reset/removal run local Git commands and can legitimately
/// take longer than the ordinary API deadline on a large checkout. A timed-out
/// client may otherwise retry after the server already committed the mutation.
const WORKSPACE_MUTATION_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// A review reads the whole working tree and builds a patch for every change,
/// so it is the one read that can legitimately outlast the ordinary API
/// deadline on a large repository. The blanket 15 s deadline turned a slow
/// review into reqwest's bare "error sending request", which told the reader
/// nothing about what had gone wrong.
const REVIEW_TIMEOUT: Duration = Duration::from_secs(120);

/// A request to an MCP server waits while the server is on its first start,
/// which may take 2 minutes for a local server (a first `npx -y` download),
/// before it gets its own answer: past the ordinary 15 s deadline.
const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(150);

#[derive(Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerAuth {
    #[default]
    None,
    Bearer {
        token: String,
    },
    Basic {
        username: String,
        password: String,
    },
}
impl ServerAuth {
    pub fn bearer(token: &str) -> Self {
        if token.is_empty() {
            Self::None
        } else {
            Self::Bearer {
                token: token.into(),
            }
        }
    }
    pub fn basic(username: &str, password: &str) -> Result<Self> {
        let username = if username.trim().is_empty() {
            "haarness"
        } else {
            username.trim()
        };
        anyhow::ensure!(
            !username.contains(':') && !username.chars().any(char::is_control),
            "Username cannot contain colons or control characters"
        );
        Ok(if password.is_empty() {
            Self::None
        } else {
            Self::Basic {
                username: username.into(),
                password: password.into(),
            }
        })
    }
    fn header(&self) -> Option<String> {
        use base64::{Engine, engine::general_purpose::STANDARD};
        match self {
            Self::None => None,
            Self::Bearer { token } => Some(format!("Bearer {token}")),
            Self::Basic { username, password } => Some(format!(
                "Basic {}",
                STANDARD.encode(format!("{username}:{password}"))
            )),
        }
    }
}

#[derive(Clone)]
pub struct EndpointCredentials {
    pub server: String,
    pub auth: ServerAuth,
}

impl EndpointCredentials {
    #[must_use]
    pub fn new(server: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            server: server.into(),
            auth: ServerAuth::bearer(&token.into()),
        }
    }
}

pub type EndpointResolver = Arc<dyn Fn() -> Option<EndpointCredentials> + Send + Sync>;

/// Starts the local workspace server again after it stopped or crashed, and
/// returns where it now listens. Only a TUI on this computer's own workspace
/// server has one; an attached remote server is someone else's to restart.
pub type ServerRestarter =
    Arc<dyn Fn() -> futures::future::BoxFuture<'static, Result<EndpointCredentials>> + Send + Sync>;

/// Silence after which the event stream, or a terminal's socket, counts as
/// lost: three of the keepalives a server sends every 15 s. A connection
/// that died without closing (the machine slept, the network changed, a
/// proxy froze) otherwise waits forever, and the TUI shows nothing new.
const STREAM_IDLE: Duration = Duration::from_secs(45);
/// An event stream that stays open this long, or brings an event, proves
/// the connection works. One a proxy accepts and drops at once proves
/// nothing: its retries keep backing off, and "Reconnected" waits for one
/// that holds, so it says so once per outage.
const STREAM_PROVEN: Duration = Duration::from_secs(10);

/// Failed reconnects before a local client starts its server again: long
/// enough for a server that is merely restarting to come back on its own.
const RESTART_AFTER_ATTEMPTS: u32 = 3;
/// After a failed restart, reconnect this many more times before another.
const RESTART_RETRY_ATTEMPTS: u32 = 5;

#[derive(Clone)]
struct ApiEndpoint {
    base: String,
    auth: ServerAuth,
    client: reqwest::Client,
    stream_client: reqwest::Client,
}

impl ApiEndpoint {
    fn new(server: &str, auth: ServerAuth) -> Result<Self> {
        let url = url::Url::parse(server).context("invalid Haarness server URL")?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            anyhow::bail!("Haarness server URL must use http or https and include a host");
        }
        if !url.username().is_empty() || url.password().is_some() {
            anyhow::bail!(
                "Haarness server URL must not contain credentials; use --username and --password"
            );
        }
        let mut headers = HeaderMap::new();
        if let Some(value) = auth.header() {
            headers.insert(AUTHORIZATION, HeaderValue::from_str(&value)?);
        }
        let stream_headers = headers.clone();
        Ok(Self {
            base: server.trim_end_matches('/').into(),
            auth,
            client: reqwest::Client::builder()
                .default_headers(headers)
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(15))
                .build()?,
            stream_client: reqwest::Client::builder()
                .default_headers(stream_headers)
                .connect_timeout(Duration::from_secs(3))
                .build()?,
        })
    }

    fn matches(&self, credentials: &EndpointCredentials) -> bool {
        self.base == credentials.server.trim_end_matches('/') && self.auth == credentials.auth
    }
}

#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)] // independent settings of one client, not a state machine
pub struct ApiClient {
    endpoint: Arc<RwLock<ApiEndpoint>>,
    endpoint_resolver: Option<EndpointResolver>,
    server_restarter: Option<ServerRestarter>,
    upload_attachments: bool,
    local_workspace_switching: bool,
    /// `haarness --yolo`: approve this client's own requests, never the
    /// server's (see [`ApiClient::with_auto_approve`]).
    auto_approve: bool,
    /// A person is at this client and can stop what it starts (see
    /// [`ApiClient::attended`]).
    attended: bool,
    /// This client shows the chat's background work and follows it up (see
    /// [`ApiClient::follows_background`]).
    follows_background: bool,
    pty_events: tokio::sync::broadcast::Sender<PtyClientEvent>,
    pty_connections: Arc<Mutex<HashMap<PtyId, PtyClientConnection>>>,
    /// Silence after which a connection counts as lost (see [`STREAM_IDLE`]).
    stream_idle: Duration,
}

#[derive(Clone)]
struct PtyClientConnection {
    generation: uuid::Uuid,
    cancel: CancellationToken,
    input: tokio::sync::mpsc::Sender<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub enum PtyClientEvent {
    Connected(PtyId),
    /// A replay follows. A truncated one (older output is gone) comes with
    /// the escape sequences that restore the modes that output left on.
    Replay {
        id: PtyId,
        truncated: bool,
        modes: Option<String>,
    },
    Output {
        id: PtyId,
        bytes: Vec<u8>,
    },
    Disconnected(PtyId),
    Exited(PtyId),
}

#[derive(Debug)]
pub enum SubscriptionEvent {
    Event(Box<EventEnvelope>),
    /// The replay cursor fell outside the server buffer or the server process
    /// changed. Callers must replace authoritative REST-backed state. It
    /// carries the new server's info when the change was found by reading it.
    ResyncRequired(Option<Box<haarness_protocol::ServerInfo>>),
    ConnectionLost {
        attempt: u32,
    },
    /// The local server is gone and this client is starting it again.
    ServerRestarting,
    /// Starting it again failed; reconnecting continues and retries later.
    ServerRestartFailed(String),
    Reconnected,
}

fn parse_sse_frame(frame: &[u8]) -> Option<SubscriptionEvent> {
    let frame = String::from_utf8_lossy(frame);
    let mut event_name = None;
    let mut data = String::new();
    for raw in frame.lines() {
        let line = raw.trim_end_matches('\r');
        if let Some(value) = line.strip_prefix("event:") {
            event_name = Some(value.trim());
        } else if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.trim_start());
        }
    }
    if event_name == Some("resync_required")
        || serde_json::from_str::<serde_json::Value>(&data)
            .ok()
            .and_then(|value| value["type"].as_str().map(str::to_owned))
            .as_deref()
            == Some("resync_required")
    {
        return Some(SubscriptionEvent::ResyncRequired(None));
    }
    serde_json::from_str(&data)
        .ok()
        .map(Box::new)
        .map(SubscriptionEvent::Event)
}

/// The event of one streamed side question frame; comments and keepalives
/// carry none.
fn btw_frame(frame: &[u8]) -> Option<BtwEvent> {
    let frame = String::from_utf8_lossy(frame);
    let data = frame
        .lines()
        .filter_map(|line| line.trim_end_matches('\r').strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str(&data).ok()
}

fn encode_url_component(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                vec![byte as char]
            } else {
                format!("%{byte:02X}").chars().collect()
            }
        })
        .collect()
}

fn compatible_server_info(
    info: &haarness_protocol::ServerInfo,
    expected_workspace: Option<&str>,
) -> bool {
    info.name == "haarness"
        && info.schema_version == haarness_protocol::SCHEMA_VERSION
        && info
            .capabilities
            .iter()
            .any(|capability| capability == haarness_protocol::WORKSPACE_HISTORY_CAPABILITY)
        && info
            .capabilities
            .iter()
            .any(|capability| capability == haarness_protocol::PTY_CAPABILITY)
        && expected_workspace.is_none_or(|expected| info.workspace.as_deref() == Some(expected))
}

impl ApiClient {
    pub async fn insights(&self, query: &[(String, String)]) -> Result<serde_json::Value> {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(query.iter().map(|(key, value)| (key, value)))
            .finish();
        Ok(checked_response(
            self.request(reqwest::Method::GET, &format!("/v1/insights?{query}"))
                .timeout(Duration::from_secs(120))
                .sent()
                .await?,
        )
        .await?
        .json()
        .await?)
    }

    pub fn new(server: &str, token: &str) -> Result<Self> {
        Self::with_auth(server, ServerAuth::bearer(token))
    }

    pub fn with_auth(server: &str, auth: ServerAuth) -> Result<Self> {
        let (pty_events, _) = tokio::sync::broadcast::channel(256);
        Ok(Self {
            endpoint: Arc::new(RwLock::new(ApiEndpoint::new(server, auth)?)),
            endpoint_resolver: None,
            server_restarter: None,
            upload_attachments: false,
            local_workspace_switching: false,
            auto_approve: false,
            attended: false,
            follows_background: false,
            pty_events,
            pty_connections: Arc::new(Mutex::new(HashMap::new())),
            stream_idle: STREAM_IDLE,
        })
    }

    /// A shorter silence limit, for tests.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_stream_idle(mut self, idle: Duration) -> Self {
        self.stream_idle = idle;
        self
    }

    #[must_use]
    pub(crate) fn supports_local_workspace_switch(&self) -> bool {
        self.local_workspace_switching
    }

    #[must_use]
    pub fn with_endpoint_resolver(mut self, resolver: EndpointResolver) -> Self {
        self.endpoint_resolver = Some(resolver);
        self.local_workspace_switching = true;
        self
    }

    #[must_use]
    pub fn with_server_restarter(mut self, restarter: ServerRestarter) -> Self {
        self.server_restarter = Some(restarter);
        self
    }

    /// `haarness --yolo` for one terminal: every turn this client starts
    /// approves its own requests once (`autoApprove`), and the TUI answers
    /// the rest in the conversation it shows. The server stays as it was, so
    /// other chats keep asking and nothing is approved after the TUI closes
    /// but the turns it started.
    #[must_use]
    pub fn with_auto_approve(mut self, auto_approve: bool) -> Self {
        self.auto_approve = auto_approve;
        self
    }

    #[must_use]
    pub fn auto_approves(&self) -> bool {
        self.auto_approve
    }

    /// A person is at this client (the terminal UI, an editor over ACP) and
    /// can stop the turns it starts: its messages and commands say so
    /// (`attended`), and their `!` snippets then run without the shell
    /// deadline. `haarness run` and other scripts leave it off.
    #[must_use]
    pub fn attended(mut self) -> Self {
        self.attended = true;
        self
    }

    /// This client shows the chat's background work and follows it up (the
    /// terminal UI): its messages and commands say so (`followsBackground`),
    /// so an agent's shell command that runs long moves to the background
    /// instead of holding the reply. `haarness run` and other scripts leave
    /// it off, and their commands stay in the foreground as before.
    #[must_use]
    pub fn follows_background(mut self) -> Self {
        self.follows_background = true;
        self
    }

    /// Refresh a remote ticket without enabling local project switching.
    #[must_use]
    pub fn with_remote_endpoint_resolver(mut self, resolver: EndpointResolver) -> Self {
        self.endpoint_resolver = Some(resolver);
        self.local_workspace_switching = false;
        self
    }

    /// Explicit attach clients may run on a different machine from the
    /// server. Inline their bounded local media so the server never has to
    /// dereference a client-only filesystem path.
    #[must_use]
    pub fn with_attachment_uploads(mut self) -> Self {
        self.upload_attachments = true;
        self
    }

    /// Whether this client sends the files it attaches itself, read from
    /// this computer, as data URLs; otherwise the server reads an attached
    /// path from its own disk.
    #[must_use]
    pub fn uploads_attachments(&self) -> bool {
        self.upload_attachments
    }

    async fn attachments(&self, sources: Vec<String>) -> Result<Vec<String>> {
        if !self.upload_attachments {
            return Ok(sources);
        }
        crate::media::inline_sources(sources).await
    }

    fn endpoint(&self) -> ApiEndpoint {
        let current = self
            .endpoint
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if !self.local_workspace_switching {
            if let Some(credentials) = self
                .endpoint_resolver
                .as_ref()
                .and_then(|resolve| resolve())
            {
                // A native Cloud renewal can change credentials, never the trusted gateway path.
                if credentials.server.trim_end_matches('/') == current.base
                    && !current.matches(&credentials)
                {
                    if let Ok(next) = ApiEndpoint::new(&credentials.server, credentials.auth) {
                        self.replace_endpoint(next.clone());
                        return next;
                    }
                }
            }
        }
        current
    }

    fn replace_endpoint(&self, endpoint: ApiEndpoint) {
        *self
            .endpoint
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = endpoint;
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let endpoint = self.endpoint();
        endpoint
            .client
            .request(method, format!("{}{path}", endpoint.base))
            .headers(trace_headers())
    }

    async fn rediscover_endpoint(
        &self,
        expected_workspace: Option<&str>,
    ) -> Option<haarness_protocol::ServerInfo> {
        let credentials = (self.endpoint_resolver.as_ref()?)()?;
        if self.endpoint().matches(&credentials) {
            return None;
        }
        let candidate = ApiEndpoint::new(&credentials.server, credentials.auth.clone()).ok()?;
        let info = candidate
            .client
            .get(format!("{}/v1/info", candidate.base))
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<haarness_protocol::ServerInfo>()
            .await
            .ok()?;
        if !compatible_server_info(&info, expected_workspace) {
            return None;
        }
        self.replace_endpoint(candidate);
        Some(info)
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        Ok(
            checked_response(self.request(reqwest::Method::GET, path).sent().await?)
                .await?
                .json()
                .await?,
        )
    }
    /// A request to an MCP server, given [`MCP_REQUEST_TIMEOUT`].
    fn mcp_request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.request(method, path).timeout(MCP_REQUEST_TIMEOUT)
    }
    async fn get_mcp<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        Ok(
            checked_response(self.mcp_request(reqwest::Method::GET, path).sent().await?)
                .await?
                .json()
                .await?,
        )
    }
    pub async fn file_content(&self, path: &str) -> Result<serde_json::Value> {
        let mut encoded: url::form_urlencoded::Serializer<'_, String> =
            url::form_urlencoded::Serializer::new(String::new());
        let query = encoded.append_pair("path", path).finish();
        self.get(&format!("/v1/files/content?{query}")).await
    }
    pub fn server_url(&self) -> String {
        self.endpoint().base
    }
    pub(crate) fn credentials(&self) -> EndpointCredentials {
        let endpoint = self.endpoint();
        EndpointCredentials {
            server: endpoint.base,
            auth: endpoint.auth,
        }
    }
    pub(crate) fn host_client(&self) -> Result<Self> {
        let credentials = self.credentials();
        let server = credentials
            .server
            .split("/v1/host/workspaces/")
            .next()
            .unwrap_or(&credentials.server);
        Ok(Self::with_auth(server, credentials.auth)?.with_attachment_uploads())
    }
    pub(crate) async fn host_projects(&self) -> Result<serde_json::Value> {
        self.get("/v1/host/projects").await
    }
    pub(crate) async fn host_open(&self, path: &str) -> Result<serde_json::Value> {
        Ok(checked_response(
            self.request(reqwest::Method::POST, "/v1/host/open")
                .json(&serde_json::json!({"path":path}))
                .sent()
                .await?,
        )
        .await?
        .json()
        .await?)
    }
    pub async fn info(&self) -> Result<haarness_protocol::ServerInfo> {
        self.get("/v1/info").await
    }

    /// `GET /v1/browser/hosts`: the browsers connected to this server through
    /// Haarness for Chrome, with each one's tab count for `thread`, and how
    /// agents may use Chrome. Polled, so it gives up quickly.
    pub(crate) async fn browser_hosts(
        &self,
        thread: Option<haarness_protocol::ThreadId>,
    ) -> Result<crate::chrome::Hosts> {
        let path = thread.map_or_else(
            || "/v1/browser/hosts".to_owned(),
            |thread| format!("/v1/browser/hosts?thread={thread}"),
        );
        Ok(checked_response(
            self.request(reqwest::Method::GET, &path)
                .timeout(Duration::from_secs(5))
                .sent()
                .await?,
        )
        .await?
        .json()
        .await?)
    }

    pub async fn input_delivery(&self) -> Result<haarness_protocol::InputDelivery> {
        if !self
            .info()
            .await?
            .capabilities
            .iter()
            .any(|c| c == "input-settings.v1")
        {
            return Ok(haarness_protocol::InputDelivery::Steer);
        }
        let value = self
            .manage("input-settings", &serde_json::json!({"action":"get"}))
            .await?;
        Ok(serde_json::from_value(value["data"]["delivery"].clone())?)
    }

    pub async fn manage(
        &self,
        resource: &str,
        input: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        anyhow::ensure!(
            resource.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
            "Invalid management resource"
        );
        Ok(checked_response(
            self.request(reqwest::Method::POST, &format!("/v1/manage/{resource}"))
                .json(input)
                .sent()
                .await?,
        )
        .await?
        .json()
        .await?)
    }

    pub async fn thread_usage(&self, id: haarness_protocol::ThreadId) -> Result<serde_json::Value> {
        self.get(&format!("/v1/threads/{id}/usage")).await
    }
    pub async fn lsp_status(&self) -> Result<Vec<LspStatus>> {
        self.get("/v1/lsp").await
    }
    pub async fn formatter_status(&self) -> Result<Vec<FormatterStatus>> {
        self.get("/v1/formatter").await
    }
    pub async fn workspaces(&self) -> Result<haarness_protocol::WorkspaceListResponse> {
        self.get("/v1/workspaces").await
    }
    pub async fn workspace_refs(&self) -> Result<haarness_protocol::WorkspaceRefsResponse> {
        self.get("/v1/workspaces/refs").await
    }
    pub async fn resolve_workspace_ref(
        &self,
        request: &haarness_protocol::WorkspaceResolveRefRequest,
    ) -> Result<haarness_protocol::WorkspaceResolveRefResponse> {
        Ok(checked_response(
            self.request(reqwest::Method::POST, "/v1/workspaces/resolve-ref")
                .json(request)
                .sent()
                .await?,
        )
        .await?
        .json()
        .await?)
    }
    pub async fn create_workspace(
        &self,
        request: &haarness_protocol::WorkspaceCreateRequest,
    ) -> Result<haarness_protocol::WorkspaceOperationResponse> {
        Ok(checked_response(
            self.request(reqwest::Method::POST, "/v1/workspaces")
                .timeout(WORKSPACE_MUTATION_TIMEOUT)
                .json(request)
                .sent()
                .await?,
        )
        .await?
        .json()
        .await?)
    }
    pub async fn open_workspace(
        &self,
        reference: &str,
    ) -> Result<haarness_protocol::WorkspaceOperationResponse> {
        Ok(checked_response(
            self.request(
                reqwest::Method::POST,
                &format!("/v1/workspaces/{}/open", encode_url_component(reference)),
            )
            .sent()
            .await?,
        )
        .await?
        .json()
        .await?)
    }
    pub async fn reset_workspace(
        &self,
        reference: &str,
        request: &haarness_protocol::WorkspaceResetRequest,
    ) -> Result<haarness_protocol::WorkspaceOperationResponse> {
        Ok(checked_response(
            self.request(
                reqwest::Method::POST,
                &format!("/v1/workspaces/{}/reset", encode_url_component(reference)),
            )
            .timeout(WORKSPACE_MUTATION_TIMEOUT)
            .json(request)
            .sent()
            .await?,
        )
        .await?
        .json()
        .await?)
    }
    pub async fn remove_workspace(
        &self,
        reference: &str,
        request: &haarness_protocol::WorkspaceRemoveRequest,
    ) -> Result<haarness_protocol::WorkspaceOperationResponse> {
        Ok(checked_response(
            self.request(
                reqwest::Method::DELETE,
                &format!("/v1/workspaces/{}", encode_url_component(reference)),
            )
            .timeout(WORKSPACE_MUTATION_TIMEOUT)
            .json(request)
            .sent()
            .await?,
        )
        .await?
        .json()
        .await?)
    }
    pub async fn cleanup_workspace(
        &self,
        reference: &str,
        request: &haarness_protocol::WorkspaceCleanupRequest,
    ) -> Result<haarness_protocol::WorkspaceOperationResponse> {
        Ok(checked_response(
            self.request(
                reqwest::Method::POST,
                &format!("/v1/workspaces/{}/cleanup", encode_url_component(reference)),
            )
            .timeout(WORKSPACE_MUTATION_TIMEOUT)
            .json(request)
            .sent()
            .await?,
        )
        .await?
        .json()
        .await?)
    }
    pub async fn threads(&self) -> Result<Vec<Thread>> {
        self.get("/v1/threads").await
    }
    pub async fn threads_including_archived(&self) -> Result<Vec<Thread>> {
        self.get("/v1/threads?archived=true").await
    }
    pub async fn thread(&self, id: haarness_protocol::ThreadId) -> Result<Thread> {
        self.get(&format!("/v1/threads/{id}")).await
    }
    pub async fn export_session_archive(
        &self,
        id: haarness_protocol::ThreadId,
        sanitize: bool,
    ) -> Result<SessionArchive> {
        self.get(&format!("/v1/threads/{id}/archive?sanitize={sanitize}"))
            .await
    }
    pub async fn import_session_archive(
        &self,
        archive: SessionArchive,
    ) -> Result<SessionArchiveImportResult> {
        Ok(checked_response(
            self.request(reqwest::Method::POST, "/v1/session-archives/import")
                .json(&SessionArchiveImportRequest { archive })
                .sent()
                .await?,
        )
        .await?
        .json()
        .await?)
    }
    pub async fn providers(&self) -> Result<Vec<haarness_protocol::ProviderInfo>> {
        self.get("/v1/providers").await
    }
    pub async fn refresh_providers(&self) -> Result<Vec<haarness_protocol::ProviderInfo>> {
        let response = self
            .request(reqwest::Method::POST, "/v1/providers/refresh")
            .sent()
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return self.providers().await;
        }
        Ok(checked_response(response).await?.json().await?)
    }
    pub async fn agents(&self) -> Result<Vec<AgentProfile>> {
        self.get("/v1/agents").await
    }
    pub async fn commands(&self) -> Result<Vec<haarness_protocol::CommandInfo>> {
        self.get("/v1/commands").await
    }
    /// Statically discovered workflow definitions. Discovery is server-side
    /// so the TUI never imports or evaluates workflow source while browsing.
    pub async fn workflows(&self) -> Result<haarness_protocol::WorkflowListResponse> {
        self.get("/v1/workflows").await
    }
    pub async fn workflow_source(
        &self,
        invocation: &str,
    ) -> Result<haarness_protocol::WorkflowSourceResponse> {
        self.get(&format!(
            "/v1/workflows/{}/source",
            encode_url_component(invocation)
        ))
        .await
    }
    /// Start one durable workflow attached to the current conversation.
    pub async fn start_workflow(
        &self,
        request: haarness_protocol::WorkflowStartRequest,
    ) -> Result<haarness_protocol::WorkflowRun> {
        Ok(self
            .request(reqwest::Method::POST, "/v1/workflows/run")
            .json(&request)
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    /// Recent durable workflow runs, newest first.
    pub async fn workflow_runs(&self, limit: usize) -> Result<Vec<haarness_protocol::WorkflowRun>> {
        self.get(&format!("/v1/workflows/runs?limit={limit}")).await
    }
    /// Read an authoritative run snapshot. `after_sequence` lets callers
    /// request only newer log rows without changing the rest of the shape.
    pub async fn workflow_run(
        &self,
        id: haarness_protocol::WorkflowId,
        after_sequence: Option<u64>,
    ) -> Result<haarness_protocol::WorkflowRun> {
        let suffix = after_sequence
            .map(|sequence| format!("?afterSequence={sequence}"))
            .unwrap_or_default();
        self.get(&format!("/v1/workflows/runs/{id}{suffix}")).await
    }
    async fn workflow_run_action(
        &self,
        id: haarness_protocol::WorkflowId,
        action: &str,
    ) -> Result<haarness_protocol::WorkflowRun> {
        Ok(self
            .request(
                reqwest::Method::POST,
                &format!("/v1/workflows/runs/{id}/{action}"),
            )
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    pub async fn cancel_workflow(
        &self,
        id: haarness_protocol::WorkflowId,
    ) -> Result<haarness_protocol::WorkflowRun> {
        self.workflow_run_action(id, "cancel").await
    }
    pub async fn pause_workflow(
        &self,
        id: haarness_protocol::WorkflowId,
    ) -> Result<haarness_protocol::WorkflowRun> {
        self.workflow_run_action(id, "pause").await
    }
    pub async fn resume_workflow(
        &self,
        id: haarness_protocol::WorkflowId,
    ) -> Result<haarness_protocol::WorkflowRun> {
        self.workflow_run_action(id, "resume").await
    }
    pub async fn delete_workflow(&self, id: haarness_protocol::WorkflowId) -> Result<()> {
        self.request(reqwest::Method::DELETE, &format!("/v1/workflows/runs/{id}"))
            .sent()
            .await?
            .checked()
            .await?;
        Ok(())
    }
    pub async fn goal(
        &self,
        id: haarness_protocol::ThreadId,
    ) -> Result<Option<haarness_protocol::GoalState>> {
        let value: serde_json::Value = self.get(&format!("/v1/threads/{id}/goal")).await?;
        if value["state"].is_null() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_value(value["state"].clone())?))
    }
    pub async fn goal_action(
        &self,
        id: haarness_protocol::ThreadId,
        action: &haarness_protocol::GoalActionRequest,
    ) -> Result<haarness_protocol::GoalActionResponse> {
        Ok(self
            .request(reqwest::Method::POST, &format!("/v1/threads/{id}/goal"))
            .json(action)
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    /// The chat's recurring loop, if one is running.
    pub async fn get_loop(&self, id: haarness_protocol::ThreadId) -> Result<serde_json::Value> {
        let response: serde_json::Value = self.get(&format!("/v1/threads/{id}/loop")).await?;
        Ok(response["loop"].clone())
    }
    pub async fn set_loop(
        &self,
        id: haarness_protocol::ThreadId,
        body: serde_json::Value,
    ) -> Result<serde_json::Value> {
        Ok(self
            .request(reqwest::Method::POST, &format!("/v1/threads/{id}/loop"))
            .json(&body)
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    pub async fn items(&self, id: haarness_protocol::ThreadId) -> Result<Vec<Item>> {
        self.get(&format!("/v1/threads/{id}/items")).await
    }
    pub async fn todos(
        &self,
        id: haarness_protocol::ThreadId,
    ) -> Result<Vec<haarness_protocol::TodoEntry>> {
        self.get(&format!("/v1/threads/{id}/todos")).await
    }
    pub async fn active_turn(&self, id: haarness_protocol::ThreadId) -> Result<Option<Turn>> {
        self.get(&format!("/v1/threads/{id}/active")).await
    }
    /// Process-wide activity snapshot used after attach, session changes, and
    /// event-stream resynchronization. A client never owns these turns.
    pub async fn active_turns(&self) -> Result<Vec<Turn>> {
        self.get("/v1/turns/active").await
    }
    pub async fn background_tasks(
        &self,
        id: haarness_protocol::ThreadId,
    ) -> Result<Vec<haarness_protocol::BackgroundTaskInfo>> {
        self.get(&format!("/v1/threads/{id}/background")).await
    }
    /// The agents' shell commands of a chat and the chats under it
    /// (`background-commands.v1`); none from a server before it.
    pub async fn shell_commands(
        &self,
        id: haarness_protocol::ThreadId,
    ) -> Result<Vec<haarness_protocol::ShellCommandInfo>> {
        match self.get(&format!("/v1/threads/{id}/commands")).await {
            Err(error)
                if refusal_status(&error).is_some_and(|status| {
                    matches!(
                        status,
                        reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::METHOD_NOT_ALLOWED
                    )
                }) =>
            {
                Ok(Vec::new())
            }
            listed => listed,
        }
    }
    pub async fn promote_background_tasks(
        &self,
        id: haarness_protocol::ThreadId,
        child: Option<haarness_protocol::ThreadId>,
    ) -> Result<Vec<haarness_protocol::BackgroundTaskInfo>> {
        let query = child.map_or_else(String::new, |child| format!("?childThreadId={child}"));
        Ok(self
            .request(
                reqwest::Method::POST,
                &format!("/v1/threads/{id}/background{query}"),
            )
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    /// The running shell commands of every chat.
    pub async fn active_shell_commands(&self) -> Result<Vec<haarness_protocol::ShellCommandInfo>> {
        self.get("/v1/commands/active").await
    }
    /// A page of what a command printed: from `cursor`, or with `tail` the
    /// last `limit` bytes.
    pub async fn shell_command_output(
        &self,
        id: haarness_protocol::ThreadId,
        command: &str,
        cursor: Option<u64>,
        tail: bool,
        limit: u64,
    ) -> Result<haarness_protocol::ShellCommandOutput> {
        let query = [
            Some(format!("limit={limit}")),
            cursor.map(|cursor| format!("cursor={cursor}")),
            tail.then(|| "tail=true".to_owned()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("&");
        self.get(&format!(
            "/v1/threads/{id}/commands/{}/output?{query}",
            encode_url_component(command)
        ))
        .await
    }
    /// Stop a command for the person; it reads `stopping` until it has
    /// exited.
    pub async fn stop_shell_command(
        &self,
        id: haarness_protocol::ThreadId,
        command: &str,
    ) -> Result<haarness_protocol::ShellCommandInfo> {
        Ok(self
            .request(
                reqwest::Method::POST,
                &format!(
                    "/v1/threads/{id}/commands/{}/stop",
                    encode_url_component(command)
                ),
            )
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    /// Send the chat tree's running foreground commands, or the one named,
    /// to the background; answers the commands moved.
    pub async fn send_commands_to_background(
        &self,
        id: haarness_protocol::ThreadId,
        command: Option<&str>,
    ) -> Result<Vec<haarness_protocol::ShellCommandInfo>> {
        let query = command.map_or_else(String::new, |command| {
            format!("?commandId={}", encode_url_component(command))
        });
        Ok(self
            .request(
                reqwest::Method::POST,
                &format!("/v1/threads/{id}/commands/background{query}"),
            )
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    pub async fn turn(&self, id: TurnId) -> Result<Turn> {
        self.get(&format!("/v1/turns/{id}")).await
    }
    pub async fn create_thread(&self, request: CreateThreadRequest) -> Result<Thread> {
        Ok(self
            .request(reqwest::Method::POST, "/v1/threads")
            .json(&request)
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    pub async fn update_thread(
        &self,
        id: haarness_protocol::ThreadId,
        request: UpdateThreadRequest,
    ) -> Result<Thread> {
        Ok(self
            .request(reqwest::Method::PATCH, &format!("/v1/threads/{id}"))
            .json(&request)
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    pub async fn fork_thread(&self, id: haarness_protocol::ThreadId) -> Result<Thread> {
        self.fork_thread_with(id, haarness_protocol::ForkThreadRequest::default())
            .await
    }
    pub async fn fork_thread_with(
        &self,
        id: haarness_protocol::ThreadId,
        request: haarness_protocol::ForkThreadRequest,
    ) -> Result<Thread> {
        Ok(self
            .request(reqwest::Method::POST, &format!("/v1/threads/{id}/fork"))
            .json(&request)
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    pub async fn compact(
        &self,
        id: haarness_protocol::ThreadId,
        request: CompactRequest,
    ) -> Result<Item> {
        Ok(self
            .request(reqwest::Method::POST, &format!("/v1/threads/{id}/compact"))
            .json(&request)
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    pub async fn mcp(&self) -> Result<Vec<McpServerInfo>> {
        self.get("/v1/mcp").await
    }
    /// The MCP servers, each saying whether chat `thread` can reach it
    /// under `agent` (`turned_off`).
    pub async fn mcp_for(
        &self,
        thread: haarness_protocol::ThreadId,
        agent: &str,
    ) -> Result<Vec<McpServerInfo>> {
        let agent: String = url::form_urlencoded::byte_serialize(agent.as_bytes()).collect();
        self.get(&format!("/v1/mcp?threadId={thread}&agent={agent}"))
            .await
    }
    pub async fn configure_mcp(
        &self,
        name: &str,
        request: &haarness_protocol::McpConfigureRequest,
    ) -> Result<McpServerInfo> {
        Ok(self
            .request(
                reqwest::Method::POST,
                &format!("/v1/mcp/{}", encode_url_component(name)),
            )
            .json(request)
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    pub async fn reconnect_mcp(&self, name: &str) -> Result<McpServerInfo> {
        Ok(self
            .mcp_request(
                reqwest::Method::POST,
                &format!("/v1/mcp/{}/reconnect", encode_url_component(name)),
            )
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    pub async fn disconnect_mcp(&self, name: &str) -> Result<McpServerInfo> {
        Ok(self
            .request(
                reqwest::Method::POST,
                &format!("/v1/mcp/{}/disconnect", encode_url_component(name)),
            )
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    pub async fn mcp_tools(&self, name: &str) -> Result<Vec<serde_json::Value>> {
        self.get_mcp(&format!("/v1/mcp/{}/tools", encode_url_component(name)))
            .await
    }
    pub async fn tool_catalog(
        &self,
        thread_id: Option<haarness_protocol::ThreadId>,
    ) -> Result<ToolCatalogResponse> {
        let path = thread_id.map_or_else(
            || "/v1/tools".to_owned(),
            |id| format!("/v1/tools?threadId={id}"),
        );
        self.get(&path).await
    }
    pub async fn update_tool(
        &self,
        name: &str,
        enabled: Option<bool>,
    ) -> Result<ToolCatalogResponse> {
        Ok(self
            .request(
                reqwest::Method::POST,
                &format!("/v1/tools/{}", encode_url_component(name)),
            )
            .json(&ToolSettingsUpdate { enabled })
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    pub async fn update_lazy_tools(
        &self,
        mode: Option<haarness_protocol::LazyToolMode>,
    ) -> Result<ToolCatalogResponse> {
        Ok(self
            .request(reqwest::Method::POST, "/v1/tools/lazy")
            .json(&LazyToolSettingsUpdate { mode })
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    pub async fn call_mcp_tool(
        &self,
        name: &str,
        tool: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        Ok(self
            .mcp_request(
                reqwest::Method::POST,
                &format!(
                    "/v1/mcp/{}/tools/{}",
                    encode_url_component(name),
                    encode_url_component(tool)
                ),
            )
            .json(arguments)
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    pub async fn mcp_resources(&self, name: &str) -> Result<Vec<serde_json::Value>> {
        self.get_mcp(&format!("/v1/mcp/{}/resources", encode_url_component(name)))
            .await
    }
    pub async fn mcp_resource_templates(&self, name: &str) -> Result<Vec<serde_json::Value>> {
        self.get_mcp(&format!(
            "/v1/mcp/{}/resource-templates",
            encode_url_component(name)
        ))
        .await
    }
    pub async fn read_mcp_resource(&self, name: &str, uri: &str) -> Result<serde_json::Value> {
        Ok(self
            .mcp_request(
                reqwest::Method::POST,
                &format!("/v1/mcp/{}/resources/read", encode_url_component(name)),
            )
            .json(&serde_json::json!({"uri": uri}))
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    pub async fn mcp_prompts(&self, name: &str) -> Result<Vec<serde_json::Value>> {
        self.get_mcp(&format!("/v1/mcp/{}/prompts", encode_url_component(name)))
            .await
    }
    pub async fn get_mcp_prompt(
        &self,
        name: &str,
        prompt: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        Ok(self
            .mcp_request(
                reqwest::Method::POST,
                &format!(
                    "/v1/mcp/{}/prompts/{}",
                    encode_url_component(name),
                    encode_url_component(prompt)
                ),
            )
            .json(arguments)
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    pub async fn search_files(&self, query: &str, limit: usize) -> Result<Vec<String>> {
        let encoded: String = query
            .bytes()
            .flat_map(|byte| {
                if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/') {
                    vec![byte as char]
                } else {
                    format!("%{byte:02X}").chars().collect()
                }
            })
            .collect();
        Ok(self
            .request(
                reqwest::Method::GET,
                &format!("/v1/files/search?q={encoded}&limit={limit}"),
            )
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    pub async fn revert(&self, id: haarness_protocol::ThreadId) -> Result<Thread> {
        self.revert_to(id, None).await
    }
    /// Undo one mutation epoch. `sequence` bounds how far back that step may
    /// reach, so walking to a chosen point in the timeline is a loop of
    /// ordinary, conflict-checked undos rather than a second code path.
    pub async fn revert_to(
        &self,
        id: haarness_protocol::ThreadId,
        sequence: Option<u64>,
    ) -> Result<Thread> {
        let mut request = self.request(reqwest::Method::POST, &format!("/v1/threads/{id}/revert"));
        if let Some(sequence) = sequence {
            request = request.json(&serde_json::json!({ "sequence": sequence }));
        }
        Ok(checked_response(request.sent().await?)
            .await?
            .json()
            .await?)
    }
    pub async fn unrevert(&self, id: haarness_protocol::ThreadId) -> Result<Thread> {
        Ok(checked_response(
            self.request(reqwest::Method::POST, &format!("/v1/threads/{id}/unrevert"))
                .sent()
                .await?,
        )
        .await?
        .json()
        .await?)
    }
    pub async fn last_turn_diff(&self, id: haarness_protocol::ThreadId) -> Result<Review> {
        let files: Vec<FileDiff> = self.review_get(&format!("/v1/threads/{id}/diff")).await?;
        Ok(Review {
            files,
            omitted_files: None,
        })
    }
    pub async fn vcs_diff(&self, mode: &str) -> Result<Review> {
        self.vcs_diff_with_context(mode, None).await
    }
    /// `context` bounds the unified context lines per hunk. `None` asks for
    /// the server's whole-file review, which is what the viewer's "all" step
    /// means.
    pub async fn vcs_diff_with_context(&self, mode: &str, context: Option<u32>) -> Result<Review> {
        let path = match context {
            Some(context) => format!(
                "/v1/vcs/diff?mode={}&context={context}",
                encode_url_component(mode)
            ),
            None => format!("/v1/vcs/diff?mode={}", encode_url_component(mode)),
        };
        let files: Vec<ReviewFile> = self.review_get(&path).await?;
        Ok(Review {
            // The bound is a property of the response, and the server repeats
            // it on every entry so an array can carry it at all.
            omitted_files: files.first().and_then(|file| file.omitted_files),
            files: files.into_iter().map(|file| file.diff).collect(),
        })
    }
    /// A review GET: the long deadline, and a failure a reader can act on
    /// instead of reqwest's generic transport text.
    async fn review_get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self
            .request(reqwest::Method::GET, path)
            .timeout(REVIEW_TIMEOUT)
            .send()
            .await
            .map_err(review_error)?;
        checked_response(response)
            .await?
            .json()
            .await
            .map_err(review_error)
    }
    pub async fn run_shell(&self, id: haarness_protocol::ThreadId, command: &str) -> Result<()> {
        self.request(reqwest::Method::POST, &format!("/v1/threads/{id}/shell"))
            .json(&serde_json::json!({ "command": command }))
            .sent()
            .await?
            .checked()
            .await?;
        Ok(())
    }
    pub async fn ptys(&self) -> Result<Vec<PtyInfo>> {
        self.get("/v1/ptys").await
    }
    pub async fn pty(&self, id: PtyId) -> Result<PtyInfo> {
        self.get(&format!("/v1/ptys/{id}")).await
    }
    pub async fn create_pty(&self, request: &PtyCreateRequest) -> Result<PtyInfo> {
        Ok(checked_response(
            self.request(reqwest::Method::POST, "/v1/ptys")
                .json(request)
                .sent()
                .await?,
        )
        .await?
        .json()
        .await?)
    }
    pub async fn update_pty(&self, id: PtyId, request: &PtyUpdateRequest) -> Result<PtyInfo> {
        Ok(checked_response(
            self.request(reqwest::Method::PATCH, &format!("/v1/ptys/{id}"))
                .json(request)
                .sent()
                .await?,
        )
        .await?
        .json()
        .await?)
    }
    pub async fn delete_pty(&self, id: PtyId) -> Result<()> {
        checked_response(
            self.request(reqwest::Method::DELETE, &format!("/v1/ptys/{id}"))
                .sent()
                .await?,
        )
        .await?;
        self.disconnect_pty(id);
        Ok(())
    }
    #[must_use]
    pub fn subscribe_ptys(&self) -> tokio::sync::broadcast::Receiver<PtyClientEvent> {
        self.pty_events.subscribe()
    }
    pub fn connect_pty(&self, id: PtyId, cursor: u64) {
        let mut connections = self
            .pty_connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if connections.contains_key(&id) {
            return;
        }
        let cancel = CancellationToken::new();
        let generation = uuid::Uuid::now_v7();
        let (input, input_rx) = tokio::sync::mpsc::channel(128);
        connections.insert(
            id,
            PtyClientConnection {
                generation,
                cancel: cancel.clone(),
                input,
            },
        );
        let client = self.clone();
        tokio::spawn(async move {
            client
                .run_pty_connection(id, cursor, input_rx, cancel)
                .await;
            let mut connections = client
                .pty_connections
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if connections
                .get(&id)
                .is_some_and(|connection| connection.generation == generation)
            {
                connections.remove(&id);
            }
        });
    }
    pub fn disconnect_pty(&self, id: PtyId) {
        if let Some(connection) = self
            .pty_connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id)
        {
            connection.cancel.cancel();
        }
    }
    pub fn disconnect_all_ptys(&self) {
        let connections = self
            .pty_connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
            .map(|(_, connection)| connection)
            .collect::<Vec<_>>();
        for connection in connections {
            connection.cancel.cancel();
        }
    }
    pub async fn write_pty(&self, id: PtyId, bytes: Vec<u8>) -> Result<()> {
        let input = self
            .pty_connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
            .map(|connection| connection.input.clone())
            .context("terminal is not connected")?;
        input
            .send(bytes)
            .await
            .context("terminal connection closed")
    }
    pub async fn btw(
        &self,
        id: haarness_protocol::ThreadId,
        question: String,
        history: Vec<BtwTurn>,
    ) -> Result<BtwResponse> {
        Ok(checked_response(
            self.request(reqwest::Method::POST, &format!("/v1/threads/{id}/btw"))
                // A side question waits for the whole answer, which takes
                // longer than the usual request deadline.
                .timeout(Duration::from_secs(310))
                .json(&BtwRequest {
                    question,
                    request_id: haarness_protocol::RequestId::new(),
                    history,
                })
                .sent()
                .await?,
        )
        .await?
        .json()
        .await?)
    }

    /// A side question whose answer streams as it is written: `delta` and
    /// `text` events, then `done` or `error`. Dropping the stream stops the
    /// answer. A connection silent for three keepalives counts as lost.
    pub async fn btw_stream(
        &self,
        id: haarness_protocol::ThreadId,
        question: String,
        history: Vec<BtwTurn>,
    ) -> Result<futures::stream::BoxStream<'static, Result<BtwEvent>>> {
        let endpoint = self.endpoint();
        let response = checked_response(
            endpoint
                .stream_client
                .post(format!("{}/v1/threads/{id}/btw/stream", endpoint.base))
                .headers(trace_headers())
                .json(&BtwRequest {
                    question,
                    request_id: haarness_protocol::RequestId::new(),
                    history,
                })
                .sent()
                .await?,
        )
        .await?;
        let idle = self.stream_idle;
        let state = (response.bytes_stream().boxed(), Vec::new(), false);
        Ok(
            futures::stream::unfold(state, move |(mut bytes, mut buffer, ended)| async move {
                if ended {
                    return None;
                }
                loop {
                    if let Some(index) = buffer.windows(2).position(|window| window == b"\n\n") {
                        let frame: Vec<u8> = buffer.drain(..index + 2).collect();
                        if let Some(event) = btw_frame(&frame) {
                            let last =
                                matches!(event, BtwEvent::Done { .. } | BtwEvent::Error { .. });
                            return Some((Ok(event), (bytes, buffer, last)));
                        }
                        continue;
                    }
                    let failure = match tokio::time::timeout(idle, bytes.next()).await {
                        Ok(Some(Ok(chunk))) => {
                            buffer.extend_from_slice(&chunk);
                            continue;
                        }
                        Ok(Some(Err(error))) => anyhow::Error::new(error.without_url())
                            .context("The side question's connection failed"),
                        Ok(None) => anyhow::anyhow!("The answer was cut off."),
                        Err(_) => anyhow::anyhow!("The side question's connection went quiet."),
                    };
                    return Some((Err(failure), (bytes, buffer, true)));
                }
            })
            .boxed(),
        )
    }
    pub async fn skills(&self) -> Result<Vec<SkillInfo>> {
        self.get("/v1/skills").await
    }
    pub async fn plugins(&self) -> Result<Vec<haarness_protocol::PluginInfo>> {
        self.get("/v1/plugins").await
    }
    pub async fn plugin_action(
        &self,
        request: haarness_protocol::PluginActionRequest,
    ) -> Result<Vec<haarness_protocol::PluginInfo>> {
        Ok(self
            .request(reqwest::Method::POST, "/v1/plugins")
            .json(&request)
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    pub async fn start_turn(
        &self,
        id: haarness_protocol::ThreadId,
        mut request: StartTurnRequest,
    ) -> Result<Turn> {
        // The server's request and turn spans join this span's trace, so one
        // prompt is one trace from the client through every model and tool call.
        let span = tracing::info_span!("haarness.client.turn", "otel.kind" = "client");
        async {
            request.auto_approve |= self.auto_approve;
            request.attended |= self.attended;
            request.follows_background |= self.follows_background;
            request.attachments = self.attachments(request.attachments).await?;
            Ok(self
                .request(reqwest::Method::POST, &format!("/v1/threads/{id}/turns"))
                .json(&request)
                .sent()
                .await?
                .checked()
                .await?
                .json()
                .await?)
        }
        .instrument(span)
        .await
    }
    pub async fn execute_command(
        &self,
        id: haarness_protocol::ThreadId,
        mut request: ExecuteCommandRequest,
    ) -> Result<CommandExecutionResponse> {
        request.attended |= self.attended;
        request.follows_background |= self.follows_background;
        request.attachments = self.attachments(request.attachments).await?;
        Ok(self
            .request(reqwest::Method::POST, &format!("/v1/threads/{id}/commands"))
            .json(&request)
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    pub async fn interrupt(&self, id: TurnId) -> Result<Turn> {
        Ok(self
            .request(reqwest::Method::POST, &format!("/v1/turns/{id}/interrupt"))
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    /// Stop every turn, delegated task, and workflow in one conversation tree.
    /// `resume` is only for a person pressing Stop: steering they sent that
    /// the agent has not read yet is then answered by a continuation turn
    /// instead of being dropped. Programmatic stops leave it off.
    pub async fn interrupt_session(
        &self,
        id: haarness_protocol::ThreadId,
        resume: bool,
    ) -> Result<()> {
        let path = if resume {
            format!("/v1/threads/{id}/interrupt?resume=true")
        } else {
            format!("/v1/threads/{id}/interrupt")
        };
        self.request(reqwest::Method::POST, &path)
            .sent()
            .await?
            .checked()
            .await?;
        Ok(())
    }
    pub async fn shutdown_server(&self) -> Result<()> {
        self.request(reqwest::Method::POST, "/v1/shutdown")
            .sent()
            .await?
            .checked()
            .await?;
        Ok(())
    }
    /// Every unanswered permission prompt on the server — used to repopulate
    /// pending approvals after reconnects and thread switches.
    pub async fn pending_permissions(
        &self,
    ) -> Result<Vec<haarness_protocol::PendingPermissionInfo>> {
        self.get("/v1/permissions").await
    }
    /// Every unanswered question on the server. Root/thread metadata lets the
    /// TUI hydrate child-agent prompts after reconnecting or switching views.
    pub async fn pending_questions(&self) -> Result<Vec<haarness_protocol::PendingQuestionInfo>> {
        self.get("/v1/questions").await
    }
    /// Send input that works mid-turn: a running turn queues and steers it,
    /// an idle thread starts a normal turn.
    pub async fn input_queue(
        &self,
        id: haarness_protocol::ThreadId,
    ) -> Result<haarness_protocol::InputQueue> {
        self.get(&format!("/v1/threads/{id}/input/queue")).await
    }
    pub async fn change_input_queue(
        &self,
        id: haarness_protocol::ThreadId,
        mut action: haarness_protocol::InputQueueAction,
    ) -> Result<haarness_protocol::InputQueue> {
        // Attachments added while editing are local paths; a remote server
        // needs them uploaded the same way a fresh message's are.
        if let haarness_protocol::InputQueueAction::Edit {
            attachments: Some(attachments),
            ..
        } = &mut action
        {
            *attachments = self.attachments(std::mem::take(attachments)).await?;
        }
        Ok(self
            .request(
                reqwest::Method::POST,
                &format!("/v1/threads/{id}/input/queue"),
            )
            .json(&action)
            .sent()
            .await?
            // The queue tells a 409 (it moved on) apart by reqwest's status.
            .error_for_status()?
            .json()
            .await?)
    }
    pub async fn queue_input(
        &self,
        id: haarness_protocol::ThreadId,
        mut request: haarness_protocol::QueueInputRequest,
    ) -> Result<haarness_protocol::QueueInputResponse> {
        request.attended |= self.attended;
        request.follows_background |= self.follows_background;
        request.attachments = self.attachments(request.attachments).await?;
        Ok(self
            .request(reqwest::Method::POST, &format!("/v1/threads/{id}/input"))
            .json(&request)
            .sent()
            .await?
            .checked()
            .await?
            .json()
            .await?)
    }
    pub async fn approval_mode(
        &self,
        id: haarness_protocol::ThreadId,
    ) -> Result<haarness_protocol::ApprovalModeInfo> {
        self.get(&format!("/v1/threads/{id}/approval-mode")).await
    }
    pub async fn set_approval_mode(
        &self,
        id: haarness_protocol::ThreadId,
        request: &haarness_protocol::SetApprovalModeRequest,
    ) -> Result<haarness_protocol::ApprovalModeInfo> {
        Ok(self
            .request(
                reqwest::Method::PUT,
                &format!("/v1/threads/{id}/approval-mode"),
            )
            .json(request)
            .sent()
            .await?
            // The caller tells a 409 (changed elsewhere) apart by reqwest's status.
            .error_for_status()?
            .json()
            .await?)
    }
    pub async fn reply_permission(&self, id: &str, reply: PermissionReply) -> Result<()> {
        self.request(reqwest::Method::POST, &format!("/v1/permissions/{id}"))
            .json(&reply)
            .sent()
            .await?
            .checked()
            .await?;
        Ok(())
    }
    pub async fn reply_question(&self, id: &str, reply: QuestionReply) -> Result<()> {
        self.request(reqwest::Method::POST, &format!("/v1/questions/{id}"))
            .json(&reply)
            .sent()
            .await?
            .checked()
            .await?;
        Ok(())
    }

    async fn run_pty_connection(
        &self,
        id: PtyId,
        mut cursor: u64,
        mut input: tokio::sync::mpsc::Receiver<Vec<u8>>,
        cancel: CancellationToken,
    ) {
        use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

        let mut attempt = 0_u32;
        loop {
            if cancel.is_cancelled() {
                return;
            }
            let endpoint = self.endpoint();
            let mut url = match url::Url::parse(&endpoint.base) {
                Ok(url) => url,
                Err(_) => return,
            };
            let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
            if url.set_scheme(scheme).is_err() {
                return;
            }
            let path = format!("{}/v1/ptys/{id}/connect", url.path().trim_end_matches('/'));
            url.set_path(&path);
            url.set_query(Some(&format!("cursor={cursor}")));
            let mut request = match url.as_str().into_client_request() {
                Ok(request) => request,
                Err(_) => return,
            };
            if let Some(value) = endpoint.auth.header().and_then(|value| value.parse().ok()) {
                request.headers_mut().insert("authorization", value);
            }
            // An attached server behind HTTPS is checked as its HTTP API is.
            // A proxy that accepts and never answers is given up after the
            // silence limit, and closing the terminal ends the wait.
            let connected = tokio::select! {
                () = cancel.cancelled() => return,
                connected = tokio::time::timeout(
                    self.stream_idle,
                    haarness_account::websocket::connect(request, None),
                ) => connected,
            };
            let socket = match connected {
                Ok(Ok((socket, _))) => socket,
                failed => {
                    if let Ok(Err(error)) = &failed
                        && error.to_string().contains("404")
                    {
                        let _ = self.pty_events.send(PtyClientEvent::Exited(id));
                        return;
                    }
                    attempt = attempt.saturating_add(1);
                    let delay = Duration::from_millis(250 * u64::from(attempt.min(8)));
                    tokio::select! {
                        () = cancel.cancelled() => return,
                        () = tokio::time::sleep(delay) => {}
                    }
                    continue;
                }
            };
            attempt = 0;
            let _ = self.pty_events.send(PtyClientEvent::Connected(id));
            let (mut writer, mut reader) = socket.split();
            // A socket that died without closing brings nothing, and the
            // keys sent on it go nowhere: ask now and then, and connect
            // again from the cursor once nothing has come for `stream_idle`.
            let mut heard = tokio::time::Instant::now();
            let mut ask = tokio::time::interval(self.stream_idle / 3);
            ask.tick().await;
            loop {
                tokio::select! {
                    () = cancel.cancelled() => {
                        let _ = writer.send(Message::Close(None)).await;
                        return;
                    }
                    _ = ask.tick() => {
                        if heard.elapsed() >= self.stream_idle
                            || !matches!(
                                tokio::time::timeout(
                                    self.stream_idle,
                                    writer.send(Message::Ping(Vec::new().into())),
                                )
                                .await,
                                Ok(Ok(()))
                            )
                        {
                            break;
                        }
                    }
                    bytes = input.recv() => match bytes {
                        Some(bytes) => {
                            if writer.send(Message::Binary(bytes.into())).await.is_err() {
                                break;
                            }
                        }
                        None => return,
                    },
                    message = reader.next() => {
                        if matches!(message, Some(Ok(_))) {
                            heard = tokio::time::Instant::now();
                        }
                        match message {
                        Some(Ok(Message::Binary(bytes))) => {
                            let _ = self.pty_events.send(PtyClientEvent::Output {
                                id,
                                bytes: bytes.to_vec(),
                            });
                        }
                        Some(Ok(Message::Text(text))) => {
                            if let Ok(control) = serde_json::from_str::<serde_json::Value>(&text) {
                                match control["type"].as_str() {
                                    Some("replay") => {
                                        let _ = self.pty_events.send(PtyClientEvent::Replay {
                                            id,
                                            truncated: control["truncated"].as_bool().unwrap_or(false),
                                            modes: control["modes"].as_str().map(str::to_owned),
                                        });
                                    }
                                    Some("cursor") => {
                                        cursor = control["end"].as_u64().unwrap_or(cursor);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Some(Ok(Message::Ping(bytes))) => {
                            if writer.send(Message::Pong(bytes)).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(Message::Pong(_))) => {}
                        Some(Ok(Message::Close(_) | Message::Frame(_)) | Err(_)) | None => break,
                        }
                    }
                }
            }
            let _ = self.pty_events.send(PtyClientEvent::Disconnected(id));
            match self.pty(id).await {
                Ok(info) if info.status == PtyStatus::Exited => {
                    let _ = self.pty_events.send(PtyClientEvent::Exited(id));
                    return;
                }
                Err(error) if refusal_status(&error) == Some(reqwest::StatusCode::NOT_FOUND) => {
                    let _ = self.pty_events.send(PtyClientEvent::Exited(id));
                    return;
                }
                _ => {}
            }
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(Duration::from_millis(250)) => {}
            }
        }
    }

    pub fn subscribe(
        &self,
        initial: Option<(uuid::Uuid, u64)>,
    ) -> (
        tokio::sync::mpsc::Receiver<SubscriptionEvent>,
        tokio::task::JoinHandle<()>,
    ) {
        self.follow(initial, false)
    }

    /// [`Self::subscribe`] after the previous subscription was lost: the new
    /// one says `Reconnected` with the first byte the server sends.
    pub fn subscribe_again(
        &self,
        initial: Option<(uuid::Uuid, u64)>,
    ) -> (
        tokio::sync::mpsc::Receiver<SubscriptionEvent>,
        tokio::task::JoinHandle<()>,
    ) {
        self.follow(initial, true)
    }

    fn follow(
        &self,
        initial: Option<(uuid::Uuid, u64)>,
        lost: bool,
    ) -> (
        tokio::sync::mpsc::Receiver<SubscriptionEvent>,
        tokio::task::JoinHandle<()>,
    ) {
        let this = self.clone();
        let (tx, receiver) = tokio::sync::mpsc::channel(256);
        let task = tokio::spawn(async move {
            let (mut instance, mut cursor) = initial
                .map(|(instance, cursor)| (Some(instance), Some(cursor)))
                .unwrap_or((None, None));
            let mut expected_workspace = None;
            let mut disconnected = lost;
            let mut attempt = 0_u32;
            loop {
                let info = match this.info().await {
                    Ok(info) if compatible_server_info(&info, expected_workspace.as_deref()) => {
                        Some(info)
                    }
                    _ => {
                        this.rediscover_endpoint(expected_workspace.as_deref())
                            .await
                    }
                };
                if let Some(info) = info {
                    if expected_workspace.is_none() {
                        expected_workspace.clone_from(&info.workspace);
                    }
                    let restarted =
                        instance.is_some_and(|previous| previous != info.server_instance_id);
                    instance = Some(info.server_instance_id);
                    if restarted {
                        cursor = None;
                        let resync = SubscriptionEvent::ResyncRequired(Some(Box::new(info)));
                        if tx.send(resync).await.is_err() {
                            return;
                        }
                    }
                }
                let endpoint = this.endpoint();
                let mut request = endpoint
                    .stream_client
                    .get(format!("{}/v1/events", endpoint.base));
                if let Some(cursor) = cursor {
                    request = request.header("Last-Event-ID", cursor.to_string());
                }
                // A server that accepts and never answers is as gone as one
                // that refuses.
                let response = match tokio::time::timeout(this.stream_idle, request.send())
                    .await
                    .map(|sent| sent.and_then(reqwest::Response::error_for_status))
                {
                    Ok(Ok(response)) => response,
                    Ok(Err(_)) | Err(_) => {
                        attempt = attempt.saturating_add(1);
                        disconnected = true;
                        if tx
                            .send(SubscriptionEvent::ConnectionLost { attempt })
                            .await
                            .is_err()
                        {
                            return;
                        }
                        if let Some(restart) = this.server_restarter.as_ref().filter(|_| {
                            attempt >= RESTART_AFTER_ATTEMPTS
                                && (attempt - RESTART_AFTER_ATTEMPTS)
                                    .is_multiple_of(RESTART_RETRY_ATTEMPTS)
                        }) {
                            if tx.send(SubscriptionEvent::ServerRestarting).await.is_err() {
                                return;
                            }
                            match restart().await {
                                Ok(credentials) => {
                                    if !this.endpoint().matches(&credentials)
                                        && let Ok(next) =
                                            ApiEndpoint::new(&credentials.server, credentials.auth)
                                    {
                                        this.replace_endpoint(next);
                                    }
                                    // Reconnect at once; the new server's
                                    // instance id triggers the resync.
                                    continue;
                                }
                                Err(error) => {
                                    tracing::warn!(%error, "could not restart the workspace server");
                                    if tx
                                        .send(SubscriptionEvent::ServerRestartFailed(format!(
                                            "{error:#}"
                                        )))
                                        .await
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                            }
                        }
                        tokio::time::sleep(Duration::from_secs(
                            2_u64.saturating_pow(attempt.min(3)).min(5),
                        ))
                        .await;
                        continue;
                    }
                };
                let opened = tokio::time::Instant::now();
                let mut proven = false;
                // The stream held (10 s, or an event): its retries start over.
                macro_rules! prove {
                    () => {
                        if !proven {
                            proven = true;
                            attempt = 0;
                        }
                    };
                }
                let mut stream = response.bytes_stream();
                let mut buffer = Vec::new();
                // A server ends the stream of a client that fell too far
                // behind straight after `resync_required`; the state is being
                // refreshed already, so that end reconnects quietly.
                let mut resynced = false;
                let ended = loop {
                    let wait = if proven {
                        this.stream_idle
                    } else {
                        STREAM_PROVEN
                            .saturating_sub(opened.elapsed())
                            .min(this.stream_idle)
                    };
                    let chunk = match tokio::time::timeout(wait, stream.next()).await {
                        Ok(Some(Ok(chunk))) => chunk,
                        Err(_) if !proven && opened.elapsed() >= STREAM_PROVEN => {
                            prove!();
                            continue;
                        }
                        // The server ended it.
                        Ok(None) => break true,
                        // Failed, or silent for `stream_idle`: lost.
                        Ok(Some(Err(_))) | Err(_) => break false,
                    };
                    // The server answers again: the outage is over for the
                    // person at once, a keepalive comment being enough.
                    if disconnected && !chunk.is_empty() {
                        disconnected = false;
                        if tx.send(SubscriptionEvent::Reconnected).await.is_err() {
                            return;
                        }
                    }
                    buffer.extend_from_slice(&chunk);
                    while let Some(index) = buffer.windows(2).position(|window| window == b"\n\n") {
                        let frame: Vec<_> = buffer.drain(..index + 2).collect();
                        if let Some(event) = parse_sse_frame(&frame) {
                            prove!();
                            match event {
                                SubscriptionEvent::Event(event) => {
                                    if instance.is_some_and(|previous| {
                                        previous != event.server_instance_id
                                    }) {
                                        cursor = None;
                                        if tx
                                            .send(SubscriptionEvent::ResyncRequired(None))
                                            .await
                                            .is_err()
                                        {
                                            return;
                                        }
                                    }
                                    instance = Some(event.server_instance_id);
                                    if cursor.is_some_and(|seen| event.event_id <= seen) {
                                        continue;
                                    }
                                    cursor = Some(event.event_id);
                                    resynced = false;
                                    if tx.send(SubscriptionEvent::Event(event)).await.is_err() {
                                        return;
                                    }
                                }
                                SubscriptionEvent::ResyncRequired(info) => {
                                    resynced = true;
                                    if tx
                                        .send(SubscriptionEvent::ResyncRequired(info))
                                        .await
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                                SubscriptionEvent::ConnectionLost { .. }
                                | SubscriptionEvent::ServerRestarting
                                | SubscriptionEvent::ServerRestartFailed(_)
                                | SubscriptionEvent::Reconnected => {}
                            }
                        }
                    }
                };
                if ended && resynced {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                // A stream that held was a connection, and its loss a new
                // outage, retried at once; one dropped at once was not, and
                // backs off as a refused one does.
                if proven {
                    attempt = 0;
                }
                attempt = attempt.saturating_add(1);
                disconnected = true;
                if tx
                    .send(SubscriptionEvent::ConnectionLost { attempt })
                    .await
                    .is_err()
                {
                    return;
                }
                tokio::time::sleep(if proven {
                    Duration::from_millis(250)
                } else {
                    Duration::from_secs(2_u64.saturating_pow(attempt.min(3)).min(5))
                })
                .await;
            }
        });
        (receiver, task)
    }
}

/// A request the Haarness server refused. It reads as the server's own
/// message (`invalid request: Unknown agent: nope`), not reqwest's status
/// line, whose URL carries the daemon's port and record ids. Callers that
/// branch on the outcome read `status` and `code`.
#[derive(Debug)]
pub struct ApiRefusal {
    pub status: reqwest::StatusCode,
    pub code: Option<String>,
    pub message: String,
}

impl std::fmt::Display for ApiRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(without_category(&self.message))
    }
}

impl std::error::Error for ApiRefusal {}

/// The server's error code (`not_found`, …) of a refused request.
#[must_use]
pub fn refusal_code(error: &anyhow::Error) -> Option<&str> {
    error
        .downcast_ref::<ApiRefusal>()
        .and_then(|refusal| refusal.code.as_deref())
}

/// The HTTP status of a refused request, whichever way it was checked.
#[must_use]
pub fn refusal_status(error: &anyhow::Error) -> Option<reqwest::StatusCode> {
    error
        .downcast_ref::<ApiRefusal>()
        .map(|refusal| refusal.status)
        .or_else(|| {
            error
                .downcast_ref::<reqwest::Error>()
                .and_then(reqwest::Error::status)
        })
}

/// Preserve the server's structured error instead of reducing every failure
/// to reqwest's generic "HTTP status client error". A request the server
/// refused in plain text (its file routes: "Path must be relative to the
/// workspace") keeps that sentence. A body without one (an older gateway, a
/// proxy: nothing, a reason phrase, an HTML page, any 5xx in plain text)
/// reads as its status in plain words; the status, path and body go to the
/// log. Query strings and authorization headers are deliberately excluded.
async fn checked_response(response: reqwest::Response) -> Result<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let path = response.url().path().to_owned();
    let body = response
        .text()
        .await
        .context("read Haarness API error response")?;
    // `{"code", "message"}`, or the same inside `"error"`, as gateways write it.
    let value = serde_json::from_str::<serde_json::Value>(&body).ok();
    let record = value.as_ref().map(|value| {
        value
            .get("error")
            .filter(|error| error.is_object())
            .unwrap_or(value)
    });
    let field = |name: &str| {
        record
            .and_then(|record| record.get(name))
            .and_then(serde_json::Value::as_str)
    };
    let code = field("code").map(str::to_owned);
    // The sign-in check answers every refused credential with the same
    // "Authentication required", which never says that what was sent was
    // refused: a 401 in plain text reads as its status.
    let plain = (value.is_none()
        && status.is_client_error()
        && status != reqwest::StatusCode::UNAUTHORIZED)
        .then(|| body.trim())
        .filter(|text| !text.is_empty())
        .map(|text| text.chars().take(300).collect::<String>());
    let said = field("message")
        .or_else(|| field("error"))
        .or(plain.as_deref());
    let relayed = path.contains("/remote-hosts/") || path.contains("/remote-sessions/");
    let message = match refusal_words(status, code.as_deref(), said, relayed) {
        Some(words) => {
            let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
            let detail = compact.chars().take(512).collect::<String>();
            tracing::warn!(%status, path, ?code, detail, "the server refused a request without saying why");
            words.to_owned()
        }
        None => said.unwrap_or_default().to_owned(),
    };
    Err(ApiRefusal {
        status,
        code,
        message,
    }
    .into())
}

/// Failures in plain words: the same words Desktop and the phones use.
pub(crate) const COMPUTER_UNANSWERED: &str = "Your computer couldn't answer. Check that Haarness is open and up to date on it, then try again.";
pub(crate) const COMPUTER_UNREACHABLE: &str =
    "Your computer isn't reachable. Open Haarness on it, then try again.";
pub(crate) const NOT_ANSWERING: &str = "Haarness isn't answering. Try again in a moment.";
const SERVER_FAILED: &str = "Something went wrong on our side. Try again in a moment.";
const TOO_SLOW: &str = "This took too long. Try again in a moment.";
const TOO_MANY: &str = "Too many requests. Wait a moment, then try again.";
const NO_ACCESS: &str = "You don't have access to this.";
const GONE: &str = "This is no longer available. It may have been removed.";
const TOO_LARGE: &str = "This is too large to send.";
const SIGN_IN_REFUSED: &str = "The server refused the token or password. Check it, then try again.";
const NOT_THROUGH: &str = "That didn't go through. Try again.";
pub(crate) const CANNOT_CONNECT: &str =
    "Can't connect. Check your internet connection and try again.";
pub(crate) const TIMED_OUT: &str = "This took too long. Check your connection, then try again.";

/// The plain words for a refusal, or `None` when the server's own sentence
/// says it. `relayed`: Cloud relayed the request to another of your
/// computers, so a gateway status is that computer's.
fn refusal_words(
    status: reqwest::StatusCode,
    code: Option<&str>,
    said: Option<&str>,
    relayed: bool,
) -> Option<&'static str> {
    if matches!(code, Some("bad_gateway" | "project_server_unavailable")) {
        return Some(COMPUTER_UNANSWERED);
    }
    let offline = said.is_some_and(|said| {
        said.to_ascii_lowercase()
            .contains("offline or its connection changed")
    });
    if code == Some("remote_offline") || (status == reqwest::StatusCode::CONFLICT && offline) {
        return Some(COMPUTER_UNREACHABLE);
    }
    if said.is_some_and(|said| !says_only_its_status(said)) {
        return None;
    }
    Some(match status.as_u16() {
        502..=504 if relayed => COMPUTER_UNANSWERED,
        502..=504 => NOT_ANSWERING,
        500..=599 => SERVER_FAILED,
        // Through Cloud's relay a 401 is Cloud's sign-in, not this server's.
        401 if !relayed => SIGN_IN_REFUSED,
        408 => TOO_SLOW,
        429 => TOO_MANY,
        403 => NO_ACCESS,
        404 | 410 => GONE,
        413 => TOO_LARGE,
        _ => NOT_THROUGH,
    })
}

/// A message that says no more than a status: nothing, "HTTP 502", a reason
/// phrase ("Bad Gateway", "502 Bad Gateway", in any case), or an HTML page.
fn says_only_its_status(said: &str) -> bool {
    const REASONS: [&str; 18] = [
        "bad request",
        "unauthorized",
        "forbidden",
        "not found",
        "method not allowed",
        "request timeout",
        "conflict",
        "gone",
        "payload too large",
        "content too large",
        "unprocessable entity",
        "unprocessable content",
        "too many requests",
        "internal server error",
        "not implemented",
        "bad gateway",
        "service unavailable",
        "gateway timeout",
    ];
    let said = said.trim();
    if said.starts_with('<') {
        return true;
    }
    let words = said
        .trim_end_matches(['.', '!'])
        .to_ascii_lowercase()
        .replace("time-out", "timeout")
        .replace("time out", "timeout")
        .replace('-', " ");
    let words = words
        .strip_prefix("http")
        .map_or(words.as_str(), |rest| {
            rest.trim_start_matches(|c: char| c == '/' || c == '.' || c.is_ascii_digit())
        })
        .trim_start()
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim();
    words.is_empty() || REASONS.contains(&words)
}

/// A request that got no answer at all: refused, reset, timed out, or no
/// route. reqwest's own text names the daemon's address and the request's
/// path and says nothing a person can act on, so this says what to do, and
/// what reqwest said goes to the log.
#[derive(Debug)]
pub struct Unanswered {
    message: &'static str,
}

impl std::fmt::Display for Unanswered {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for Unanswered {}

/// Whether `error` is a request that got no answer (see [`Unanswered`]).
#[must_use]
pub fn unanswered(error: &anyhow::Error) -> bool {
    error.downcast_ref::<Unanswered>().is_some()
}

fn unanswered_error(error: reqwest::Error) -> anyhow::Error {
    // A request this client could not build is a bug, not a lost connection.
    if error.is_builder() {
        return error.into();
    }
    let mut detail = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        detail.push_str(": ");
        detail.push_str(&cause.to_string());
        source = cause.source();
    }
    tracing::warn!(detail, "a request to the server got no answer");
    // A server on this computer that does not answer is Haarness's, not the
    // internet's.
    let local = error
        .url()
        .and_then(reqwest::Url::host)
        .is_some_and(|host| match host {
            url::Host::Domain(name) => name == "localhost",
            url::Host::Ipv4(address) => address.is_loopback(),
            url::Host::Ipv6(address) => address.is_loopback(),
        });
    let message = if error.is_timeout() {
        TIMED_OUT
    } else if local {
        NOT_ANSWERING
    } else {
        CANNOT_CONNECT
    };
    Unanswered { message }.into()
}

/// `send`, with a request that got no answer in plain words: see
/// [`Unanswered`].
trait SendPlain {
    async fn sent(self) -> Result<reqwest::Response>;
}

impl SendPlain for reqwest::RequestBuilder {
    async fn sent(self) -> Result<reqwest::Response> {
        self.send().await.map_err(unanswered_error)
    }
}

/// The server prefixes each message with its error category ("tool error:
/// …"); the category is already in `code`, and people act on the sentence.
/// "not found:" and "permission denied:" stay: there the category is the news
/// ("not found: command /nope").
fn without_category(message: &str) -> &str {
    [
        "tool error: ",
        "invalid request: ",
        "conflict: ",
        "provider error: ",
        "storage error: ",
    ]
    .iter()
    .find_map(|prefix| message.strip_prefix(prefix))
    .unwrap_or(message)
}

/// `error_for_status`, keeping the server's message: see `checked_response`.
trait CheckedStatus {
    async fn checked(self) -> Result<reqwest::Response>;
}

impl CheckedStatus for reqwest::Response {
    async fn checked(self) -> Result<reqwest::Response> {
        checked_response(self).await
    }
}

/// W3C trace context for the caller's current span, so the server's spans join
/// the client's trace. Empty when tracing is off (the global propagator is a no-op).
fn trace_headers() -> reqwest::header::HeaderMap {
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;
    struct Injector<'a>(&'a mut reqwest::header::HeaderMap);
    impl opentelemetry::propagation::Injector for Injector<'_> {
        fn set(&mut self, key: &str, value: String) {
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                reqwest::header::HeaderValue::from_str(&value),
            ) {
                self.0.insert(name, value);
            }
        }
    }
    let mut headers = reqwest::header::HeaderMap::new();
    let context = tracing::Span::current().context();
    opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut Injector(&mut headers));
    });
    headers
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::{
        ApiClient, ApiEndpoint, EndpointCredentials, EndpointResolver, REVIEW_TIMEOUT, ServerAuth,
        SubscriptionEvent, compatible_server_info, parse_sse_frame, review_error,
    };

    /// A review that runs out of time has to name the thing the reader can
    /// change, not the transport that gave up.
    #[tokio::test]
    async fn a_timed_out_review_explains_itself_instead_of_naming_reqwest() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        // Accept, then never answer: exactly the shape of a review the server
        // is still computing when the client's deadline passes.
        let _server = tokio::spawn(async move {
            let _socket = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let error = reqwest::Client::builder()
            .timeout(Duration::from_millis(250))
            .build()
            .unwrap()
            .get(format!("http://{address}/v1/vcs/diff?mode=git"))
            .send()
            .await
            .expect_err("the request must time out");
        assert!(error.is_timeout());

        let message = review_error(error).to_string();
        assert!(
            message.contains(&format!("{}s", REVIEW_TIMEOUT.as_secs()))
                && message.contains("too large"),
            "{message}"
        );
        assert!(!message.contains("error sending request"), "{message}");
    }

    #[tokio::test]
    // The WebSocket handshake callback requires its library's response type.
    #[allow(clippy::result_large_err)]
    async fn remote_project_terminal_preserves_route_and_password_auth() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let id = haarness_protocol::PtyId(uuid::Uuid::now_v7());
        let auth = ServerAuth::basic("alex", "日本:password").unwrap();
        let expected = auth.header().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let _socket = tokio_tungstenite::accept_hdr_async(
                stream,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                    assert_eq!(
                        request.uri().path(),
                        format!("/v1/host/workspaces/project-id/v1/ptys/{id}/connect")
                    );
                    assert_eq!(request.uri().query(), Some("cursor=42"));
                    assert_eq!(
                        request.headers()["authorization"].to_str().unwrap(),
                        expected
                    );
                    Ok(response)
                },
            )
            .await
            .unwrap();
        });
        let client = ApiClient::with_auth(
            &format!("http://{address}/v1/host/workspaces/project-id"),
            auth,
        )
        .unwrap();
        client.connect_pty(id, 42);
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        client.disconnect_all_ptys();
    }

    /// The server closes a terminal socket whose reader fell behind; the
    /// terminal must come back from the last output it showed, neither losing
    /// nor repeating any.
    #[tokio::test]
    #[allow(clippy::result_large_err)]
    async fn a_terminal_socket_the_server_closes_reconnects_from_its_last_cursor() {
        use futures::{SinkExt as _, StreamExt as _};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let id = haarness_protocol::PtyId(uuid::Uuid::now_v7());
        let info = serde_json::to_string(&haarness_protocol::PtyInfo {
            id,
            title: "shell".into(),
            command: "sh".into(),
            args: vec![],
            cwd: "/".into(),
            status: haarness_protocol::PtyStatus::Running,
            pid: None,
            cols: 80,
            rows: 24,
            created: false,
            directory: None,
        })
        .unwrap();
        let (queries, mut asked) = tokio::sync::mpsc::unbounded_channel::<String>();
        let _server = tokio::spawn(async move {
            let control = |value: serde_json::Value| Message::Text(value.to_string().into());
            let mut sockets = 0;
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut head = [0_u8; 2048];
                let read = stream.peek(&mut head).await.unwrap();
                let head = String::from_utf8_lossy(&head[..read]).to_ascii_lowercase();
                if !head.contains("upgrade: websocket") {
                    // The status check after a close: still running.
                    let mut request = Vec::new();
                    let mut chunk = [0_u8; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        let read = stream.read(&mut chunk).await.unwrap();
                        if read == 0 {
                            break;
                        }
                        request.extend_from_slice(&chunk[..read]);
                    }
                    stream
                        .write_all(response("application/json", &info).as_bytes())
                        .await
                        .unwrap();
                    continue;
                }
                let queries = queries.clone();
                let mut socket = tokio_tungstenite::accept_hdr_async(
                    stream,
                    move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                          response| {
                        let _ = queries.send(request.uri().query().unwrap_or_default().into());
                        Ok(response)
                    },
                )
                .await
                .unwrap();
                sockets += 1;
                if sockets > 1 {
                    socket
                        .send(control(serde_json::json!({"type":"replay","start":0,"end":6,"truncated":false})))
                        .await
                        .unwrap();
                    socket
                        .send(control(serde_json::json!({"type":"cursor","start":0,"end":6,"truncated":false})))
                        .await
                        .unwrap();
                    while socket.next().await.is_some() {}
                    return;
                }
                socket
                    .send(control(
                        serde_json::json!({"type":"replay","start":0,"end":0,"truncated":false}),
                    ))
                    .await
                    .unwrap();
                for (bytes, end) in [("abc", 3), ("def", 6)] {
                    socket
                        .send(Message::Binary(bytes.as_bytes().to_vec().into()))
                        .await
                        .unwrap();
                    socket
                        .send(control(serde_json::json!({"type":"cursor","start":0,"end":end,"truncated":false})))
                        .await
                        .unwrap();
                }
                // The reader fell behind: the server lets go.
                socket.send(Message::Close(None)).await.unwrap();
            }
        });
        let client = ApiClient::new(&format!("http://{address}"), "token").unwrap();
        let mut events = client.subscribe_ptys();
        client.connect_pty(id, 0);
        for expected in ["cursor=0", "cursor=6"] {
            let query = tokio::time::timeout(Duration::from_secs(10), asked.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(query, expected);
        }
        let mut shown = Vec::new();
        let mut connected = 0;
        while connected < 2 {
            match tokio::time::timeout(Duration::from_secs(10), events.recv())
                .await
                .unwrap()
                .unwrap()
            {
                super::PtyClientEvent::Output { bytes, .. } => shown.extend(bytes),
                super::PtyClientEvent::Connected(_) => connected += 1,
                super::PtyClientEvent::Exited(_) => panic!("the terminal is still running"),
                _ => {}
            }
        }
        assert_eq!(shown, b"abcdef");
        client.disconnect_all_ptys();
    }

    fn fixture_server_info(
        server_instance_id: uuid::Uuid,
        workspace: &str,
    ) -> haarness_protocol::ServerInfo {
        haarness_protocol::ServerInfo {
            name: "haarness".into(),
            version: "test".into(),
            schema_version: haarness_protocol::SCHEMA_VERSION.into(),
            server_instance_id,
            event_cursor: 0,
            capabilities: vec![
                haarness_protocol::WORKSPACE_HISTORY_CAPABILITY.into(),
                haarness_protocol::PTY_CAPABILITY.into(),
            ],
            workspace: Some(workspace.into()),
            git_branch: None,
            default_provider: None,
            default_model: None,
            default_approval_mode: None,
            default_reasoning_effort: None,
            default_agent: None,
            project_trusted: Some(true),
            pid: None,
        }
    }

    fn response(content_type: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    async fn serve_responses(
        listener: tokio::net::TcpListener,
        responses: Vec<String>,
    ) -> Vec<String> {
        let mut requests = Vec::new();
        for response in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..read]);
                if read == 0 || request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            socket.write_all(response.as_bytes()).await.unwrap();
            requests.push(String::from_utf8(request).unwrap());
        }
        requests
    }

    #[test]
    fn parses_resync_and_regular_sse_frames() {
        assert!(matches!(
            parse_sse_frame(b"event: resync_required\ndata: {\"type\":\"resync_required\"}\n\n"),
            Some(SubscriptionEvent::ResyncRequired(None))
        ));
        let event = haarness_protocol::EventEnvelope {
            schema_version: haarness_protocol::SCHEMA_VERSION.into(),
            server_instance_id: uuid::Uuid::now_v7(),
            event_id: 17,
            event_type: "todo.updated".into(),
            thread_id: None,
            root_thread_id: None,
            turn_id: None,
            item_id: None,
            payload: serde_json::json!({"todos": []}),
            created_at: chrono::Utc::now(),
        };
        let frame = format!(
            "id: 17\nevent: todo.updated\ndata: {}\n\n",
            serde_json::to_string(&event).unwrap()
        );
        assert!(matches!(
            parse_sse_frame(frame.as_bytes()),
            Some(SubscriptionEvent::Event(parsed)) if parsed.event_id == 17
        ));
    }

    #[test]
    fn endpoint_rotation_includes_credentials_and_rejects_incompatible_servers() {
        let endpoint =
            ApiEndpoint::new("http://127.0.0.1:1234/", ServerAuth::bearer("old-token")).unwrap();
        assert!(endpoint.matches(&EndpointCredentials::new(
            "http://127.0.0.1:1234",
            "old-token"
        )));
        assert!(!endpoint.matches(&EndpointCredentials::new(
            "http://127.0.0.1:1234",
            "new-token"
        )));

        let mut info = fixture_server_info(uuid::Uuid::now_v7(), "/workspace");
        assert!(compatible_server_info(&info, Some("/workspace")));
        assert!(!compatible_server_info(&info, Some("/other")));
        info.capabilities.clear();
        assert!(!compatible_server_info(&info, Some("/workspace")));
    }

    #[tokio::test]
    async fn api_errors_read_as_the_server_message_and_keep_their_status() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for (status, body) in [
                (
                    "404 Not Found",
                    r#"{"code":"missing_route","message":"workspace history API is unavailable","retryable":false}"#,
                ),
                ("502 Bad Gateway", "upstream\n  went away"),
                (
                    "502 Bad Gateway",
                    r#"{"error":{"code":"project_server_unavailable","message":"This project's Haarness server is not reachable","retryable":true}}"#,
                ),
                (
                    "503 Service Unavailable",
                    r#"{"error":"Service Unavailable"}"#,
                ),
                ("413 Payload Too Large", "<html><body>413</body></html>"),
                ("502 Bad Gateway", ""),
                (
                    "409 Conflict",
                    r#"{"error":{"code":"conflict","message":"Remote host is offline or its connection changed. Reconnect and check the current state before retrying.","retryable":false}}"#,
                ),
                (
                    "400 Bad Request",
                    "\n  Path must be relative to the workspace\n",
                ),
                ("401 Unauthorized", "Authentication required"),
                ("500 Internal Server Error", "thread 'main' panicked"),
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                loop {
                    let read = socket.read(&mut chunk).await.unwrap();
                    request.extend_from_slice(&chunk[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                requests.push(String::from_utf8(request).unwrap());
            }
            requests
        });

        let client = ApiClient::new(&format!("http://{address}"), "test-token").unwrap();
        let error = client.vcs_diff("working").await.unwrap_err();
        assert_eq!(error.to_string(), "workspace history API is unavailable");
        assert_eq!(
            super::refusal_status(&error),
            Some(reqwest::StatusCode::NOT_FOUND)
        );
        assert_eq!(
            error
                .downcast_ref::<super::ApiRefusal>()
                .and_then(|refusal| refusal.code.as_deref()),
            Some("missing_route")
        );

        // A body without a sentence reads as its status in plain words, never
        // the status line, the path, the daemon's address or token; the
        // status stays for callers that branch on it.
        let error = client.agents().await.unwrap_err();
        assert_eq!(error.to_string(), super::NOT_ANSWERING);
        assert_eq!(
            super::refusal_status(&error),
            Some(reqwest::StatusCode::BAD_GATEWAY)
        );
        let error = error.to_string();
        assert!(!error.contains("test-token") && !error.contains(&address.to_string()));
        // A computer's gateway that could not reach its project's server.
        let error = client.agents().await.unwrap_err();
        assert_eq!(error.to_string(), super::COMPUTER_UNANSWERED);
        assert_eq!(
            super::refusal_code(&error),
            Some("project_server_unavailable")
        );
        // A reason phrase or a proxy's page is no sentence either.
        assert_eq!(
            client.agents().await.unwrap_err().to_string(),
            super::NOT_ANSWERING
        );
        assert_eq!(
            client.agents().await.unwrap_err().to_string(),
            "This is too large to send."
        );
        // Through Cloud's relay, a bare gateway status is the other
        // computer's, and Cloud's offline answer says it is not reachable.
        let relayed = ApiClient::new(
            &format!("http://{address}/api/v1/remote-hosts/h1/runtime"),
            "test-token",
        )
        .unwrap();
        assert_eq!(
            relayed.agents().await.unwrap_err().to_string(),
            super::COMPUTER_UNANSWERED
        );
        assert_eq!(
            relayed.agents().await.unwrap_err().to_string(),
            super::COMPUTER_UNREACHABLE
        );
        // A refusal the server wrote in plain text is its sentence; a wrong
        // token says it was refused; a 5xx in plain text is no sentence.
        let error = client.agents().await.unwrap_err();
        assert_eq!(error.to_string(), "Path must be relative to the workspace");
        assert_eq!(
            super::refusal_status(&error),
            Some(reqwest::StatusCode::BAD_REQUEST)
        );
        let error = client.agents().await.unwrap_err();
        assert_eq!(error.to_string(), super::SIGN_IN_REFUSED);
        assert_eq!(
            super::refusal_status(&error),
            Some(reqwest::StatusCode::UNAUTHORIZED)
        );
        assert_eq!(
            client.agents().await.unwrap_err().to_string(),
            super::SERVER_FAILED
        );
        let requests = server.await.unwrap();
        assert!(requests[0].starts_with("GET /v1/vcs/diff?mode=working "));
        assert!(requests[5].starts_with("GET /api/v1/remote-hosts/h1/runtime/v1/agents "));
    }

    /// A request that got no answer says what to do: the daemon's address
    /// and reqwest's transport text go to the log.
    #[tokio::test]
    async fn a_request_without_an_answer_reads_as_plain_words() {
        let error = ApiClient::new("http://127.0.0.1:1", "test-token")
            .unwrap()
            .agents()
            .await
            .unwrap_err();
        assert!(super::unanswered(&error));
        // A server on this computer is Haarness's; one elsewhere needs the
        // connection checked.
        assert_eq!(format!("{error:#}"), super::NOT_ANSWERING);
        let error = ApiClient::new("http://unreachable.invalid", "test-token")
            .unwrap()
            .agents()
            .await
            .unwrap_err();
        assert_eq!(format!("{error:#}"), super::CANNOT_CONNECT);
    }

    #[test]
    fn a_status_line_is_no_sentence() {
        for said in [
            "",
            "HTTP 502",
            "Bad Gateway",
            "502 Bad Gateway",
            "bad gateway",
            "HTTP/1.1 504 Gateway Time-out",
            "<html><body><h1>502 Bad Gateway</h1></body></html>",
            "Service Unavailable.",
        ] {
            assert!(super::says_only_its_status(said), "{said:?}");
        }
        for said in [
            "upstream went away",
            "Unknown agent: nope",
            "conflict: the chat changed",
            "Not found: command /nope",
        ] {
            assert!(!super::says_only_its_status(said), "{said:?}");
        }
        let words = |status: u16, code: Option<&str>, said: Option<&str>, relayed: bool| {
            super::refusal_words(
                reqwest::StatusCode::from_u16(status).unwrap(),
                code,
                said,
                relayed,
            )
        };
        assert_eq!(
            words(
                502,
                Some("bad_gateway"),
                Some("Your computer could not complete this request."),
                true
            ),
            Some(super::COMPUTER_UNANSWERED)
        );
        assert_eq!(
            words(409, Some("remote_offline"), None, true),
            Some(super::COMPUTER_UNREACHABLE)
        );
        assert_eq!(words(504, None, None, false), Some(super::NOT_ANSWERING));
        assert_eq!(
            words(500, None, Some("Internal Server Error"), false),
            Some(super::SERVER_FAILED)
        );
        assert_eq!(words(408, None, None, false), Some(super::TOO_SLOW));
        assert_eq!(words(429, None, None, false), Some(super::TOO_MANY));
        assert_eq!(
            words(403, None, Some("Forbidden"), false),
            Some(super::NO_ACCESS)
        );
        assert_eq!(words(410, None, None, false), Some(super::GONE));
        assert_eq!(words(400, None, None, false), Some(super::NOT_THROUGH));
        assert_eq!(
            words(401, None, Some("Unauthorized"), false),
            Some(super::SIGN_IN_REFUSED)
        );
        // Through Cloud's relay a bare 401 is Cloud's sign-in.
        assert_eq!(words(401, None, None, true), Some(super::NOT_THROUGH));
        assert_eq!(
            words(
                401,
                Some("unauthorized"),
                Some("Sign in to Haarness again."),
                true
            ),
            None
        );
        // The server's own sentence is the server's, whatever its status.
        assert_eq!(
            words(
                502,
                Some("provider_error"),
                Some("The model is overloaded"),
                false
            ),
            None
        );
    }

    #[tokio::test]
    async fn subscription_follows_a_replacement_local_daemon_and_resyncs_rest_state() {
        let old_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let old_address = old_listener.local_addr().unwrap();
        let new_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let new_address = new_listener.local_addr().unwrap();
        let old_instance = uuid::Uuid::now_v7();
        let new_instance = uuid::Uuid::now_v7();
        let old_info =
            serde_json::to_string(&fixture_server_info(old_instance, "/workspace")).unwrap();
        let new_info =
            serde_json::to_string(&fixture_server_info(new_instance, "/workspace")).unwrap();
        let event = haarness_protocol::EventEnvelope {
            schema_version: haarness_protocol::SCHEMA_VERSION.into(),
            server_instance_id: new_instance,
            event_id: 1,
            event_type: "thread.updated".into(),
            thread_id: None,
            root_thread_id: None,
            turn_id: None,
            item_id: None,
            payload: serde_json::json!({}),
            created_at: chrono::Utc::now(),
        };
        let event_body = format!(
            "id: 1\nevent: thread.updated\ndata: {}\n\n",
            serde_json::to_string(&event).unwrap()
        );
        let old_server = tokio::spawn(serve_responses(
            old_listener,
            vec![
                response("application/json", &old_info),
                response("text/event-stream", ""),
            ],
        ));
        let new_server = tokio::spawn(serve_responses(
            new_listener,
            vec![
                response("application/json", &new_info),
                response("text/event-stream", &event_body),
                response("application/json", &new_info),
            ],
        ));
        let replacement = Arc::new(Mutex::new(None::<EndpointCredentials>));
        let resolver: EndpointResolver = {
            let replacement = replacement.clone();
            Arc::new(move || replacement.lock().unwrap().clone())
        };
        let client = ApiClient::new(&format!("http://{old_address}"), "old-token")
            .unwrap()
            .with_endpoint_resolver(resolver);
        let (mut events, event_task) = client.subscribe(None);

        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), events.recv())
                .await
                .unwrap(),
            Some(SubscriptionEvent::ConnectionLost { attempt: 1 })
        ));
        *replacement.lock().unwrap() = Some(EndpointCredentials::new(
            format!("http://{new_address}"),
            "new-token",
        ));

        let mut resync = false;
        let mut reconnected = false;
        let mut received_event = false;
        while !(resync && reconnected && received_event) {
            match tokio::time::timeout(Duration::from_secs(3), events.recv())
                .await
                .unwrap()
                .unwrap()
            {
                SubscriptionEvent::ResyncRequired(_) => resync = true,
                SubscriptionEvent::Reconnected => reconnected = true,
                SubscriptionEvent::Event(event) if event.server_instance_id == new_instance => {
                    received_event = true;
                }
                SubscriptionEvent::Event(_)
                | SubscriptionEvent::ConnectionLost { .. }
                | SubscriptionEvent::ServerRestarting
                | SubscriptionEvent::ServerRestartFailed(_) => {}
            }
        }
        event_task.abort();
        let _ = event_task.await;
        assert_eq!(
            client.info().await.unwrap().server_instance_id,
            new_instance
        );

        let old_requests = old_server.await.unwrap();
        let new_requests = new_server.await.unwrap();
        assert_eq!(old_requests.len(), 2);
        assert_eq!(new_requests.len(), 3);
        assert!(
            old_requests
                .iter()
                .all(|request| request.contains("authorization: Bearer old-token"))
        );
        assert!(
            new_requests
                .iter()
                .all(|request| request.contains("authorization: Bearer new-token"))
        );
    }

    #[tokio::test]
    async fn a_stream_the_server_ends_after_resync_required_reconnects_quietly() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let instance = uuid::Uuid::now_v7();
        let info = serde_json::to_string(&fixture_server_info(instance, "/workspace")).unwrap();
        let event = haarness_protocol::EventEnvelope {
            schema_version: haarness_protocol::SCHEMA_VERSION.into(),
            server_instance_id: instance,
            event_id: 9,
            event_type: "thread.updated".into(),
            thread_id: None,
            root_thread_id: None,
            turn_id: None,
            item_id: None,
            payload: serde_json::json!({}),
            created_at: chrono::Utc::now(),
        };
        let server = tokio::spawn(serve_responses(
            listener,
            vec![
                response("application/json", &info),
                // A client too far behind: told to resync, then the response ends.
                response(
                    "text/event-stream",
                    "event: resync_required\ndata: {\"type\":\"resync_required\"}\n\n",
                ),
                response("application/json", &info),
                response(
                    "text/event-stream",
                    &format!(
                        "id: 9\nevent: thread.updated\ndata: {}\n\n",
                        serde_json::to_string(&event).unwrap()
                    ),
                ),
            ],
        ));
        let client = ApiClient::new(&format!("http://{address}"), "token").unwrap();
        let (mut events, event_task) = client.subscribe(None);
        let mut seen = Vec::new();
        while !matches!(seen.last(), Some(SubscriptionEvent::Event(_))) {
            seen.push(
                tokio::time::timeout(Duration::from_secs(3), events.recv())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        event_task.abort();
        let _ = event_task.await;
        assert!(
            matches!(
                seen.as_slice(),
                [SubscriptionEvent::ResyncRequired(_), SubscriptionEvent::Event(event)] if event.event_id == 9
            ),
            "no connection-lost notice between the resync and the next stream: {seen:?}"
        );
        assert_eq!(server.await.unwrap().len(), 4);
    }

    #[tokio::test]
    async fn a_local_client_restarts_its_stopped_server_and_resyncs() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let instance = uuid::Uuid::now_v7();
        let info = serde_json::to_string(&fixture_server_info(instance, "/workspace")).unwrap();
        let server = tokio::spawn(serve_responses(
            listener,
            vec![
                response("application/json", &info),
                // A stream that brings an event: the connection works again.
                response("text/event-stream", &sse_event(instance, 1)),
            ],
        ));
        let restarts = Arc::new(Mutex::new(0_u32));
        let restarter: super::ServerRestarter = {
            let restarts = restarts.clone();
            Arc::new(move || {
                *restarts.lock().unwrap() += 1;
                Box::pin(async move {
                    Ok(EndpointCredentials::new(
                        format!("http://{address}"),
                        "new-token",
                    ))
                })
            })
        };
        // Nothing listens on port 1 any more: the server this TUI used is gone.
        let client = ApiClient::new("http://127.0.0.1:1", "old-token")
            .unwrap()
            .with_endpoint_resolver(Arc::new(|| None))
            .with_server_restarter(restarter);
        let (mut events, event_task) = client.subscribe(Some((uuid::Uuid::now_v7(), 7)));

        let mut seen = Vec::new();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(15), events.recv())
                .await
                .unwrap()
                .unwrap();
            match event {
                SubscriptionEvent::ConnectionLost { attempt } => {
                    seen.push(format!("lost {attempt}"))
                }
                SubscriptionEvent::ServerRestarting => seen.push("restarting".into()),
                SubscriptionEvent::ResyncRequired(_) => seen.push("resync".into()),
                SubscriptionEvent::Reconnected => break,
                SubscriptionEvent::Event(_) | SubscriptionEvent::ServerRestartFailed(_) => {}
            }
        }
        event_task.abort();
        let _ = event_task.await;
        assert_eq!(
            seen,
            ["lost 1", "lost 2", "lost 3", "restarting", "resync"],
            "restarts only after a few failed reconnects"
        );
        assert_eq!(*restarts.lock().unwrap(), 1);
        let requests = server.await.unwrap();
        assert!(
            requests
                .iter()
                .all(|request| request.contains("authorization: Bearer new-token"))
        );
    }

    #[tokio::test]
    async fn rediscovery_rejects_a_different_workspace() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let info = serde_json::to_string(&fixture_server_info(
            uuid::Uuid::now_v7(),
            "/different-workspace",
        ))
        .unwrap();
        let server = tokio::spawn(serve_responses(
            listener,
            vec![response("application/json", &info)],
        ));
        let resolver: EndpointResolver = Arc::new(move || {
            Some(EndpointCredentials::new(
                format!("http://{address}"),
                "replacement-token",
            ))
        });
        let client = ApiClient::new("http://127.0.0.1:1", "old-token")
            .unwrap()
            .with_endpoint_resolver(resolver);

        assert!(
            client
                .rediscover_endpoint(Some("/expected-workspace"))
                .await
                .is_none()
        );
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(client.info().await.is_err());
    }

    #[test]
    fn only_local_resolver_clients_advertise_in_process_workspace_switching() {
        let plain = ApiClient::new("http://127.0.0.1:1", "token").unwrap();
        assert!(!plain.supports_local_workspace_switch());
        let resolver: EndpointResolver = Arc::new(|| None);
        let local = plain.with_endpoint_resolver(resolver);
        assert!(local.supports_local_workspace_switch());
        let remote = local.with_remote_endpoint_resolver(Arc::new(|| None));
        assert!(!remote.supports_local_workspace_switch());
    }

    /// One request off a socket: its head, which says what it asks for.
    async fn request_head(socket: &mut tokio::net::TcpStream) -> String {
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = socket.read(&mut chunk).await.unwrap_or(0);
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
        }
        String::from_utf8_lossy(&request).into_owned()
    }

    fn sse_event(instance: uuid::Uuid, id: u64) -> String {
        let event = haarness_protocol::EventEnvelope {
            schema_version: haarness_protocol::SCHEMA_VERSION.into(),
            server_instance_id: instance,
            event_id: id,
            event_type: "thread.updated".into(),
            thread_id: None,
            root_thread_id: None,
            turn_id: None,
            item_id: None,
            payload: serde_json::json!({}),
            created_at: chrono::Utc::now(),
        };
        format!(
            "id: {id}\ndata: {}\n\n",
            serde_json::to_string(&event).unwrap()
        )
    }

    /// A proxy that accepts the event stream and drops it at once: the TUI
    /// once asked for it four times a second, saying "Reconnected" each time.
    #[tokio::test]
    async fn an_event_stream_dropped_at_once_backs_off_and_never_says_reconnected() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let info = serde_json::to_string(&fixture_server_info(uuid::Uuid::now_v7(), "/w")).unwrap();
        let streams = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server = tokio::spawn({
            let streams = streams.clone();
            async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let head = request_head(&mut socket).await;
                    let answer = if head.starts_with("GET /v1/events ") {
                        streams.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n".to_owned()
                    } else {
                        response("application/json", &info)
                    };
                    let _ = socket.write_all(answer.as_bytes()).await;
                }
            }
        });
        let client = ApiClient::new(&format!("http://{address}"), "token").unwrap();
        let (mut events, task) = client.subscribe(None);
        let mut seen = Vec::new();
        let until = tokio::time::Instant::now() + Duration::from_secs(4);
        while let Ok(Some(event)) = tokio::time::timeout_at(until, events.recv()).await {
            seen.push(event);
        }
        task.abort();
        server.abort();
        assert!(
            !seen
                .iter()
                .any(|event| matches!(event, SubscriptionEvent::Reconnected)),
            "{seen:?}"
        );
        let attempts: Vec<_> = seen
            .iter()
            .filter_map(|event| match event {
                SubscriptionEvent::ConnectionLost { attempt } => Some(*attempt),
                _ => None,
            })
            .collect();
        assert_eq!(attempts, [1, 2], "{seen:?}");
        assert!(streams.load(std::sync::atomic::Ordering::SeqCst) <= 3);
    }

    /// A stream that went silent without closing, as after a sleep on another
    /// network: given up after the silence limit, and followed again from
    /// the last event it brought.
    #[tokio::test]
    async fn a_silent_event_stream_connects_again_from_its_cursor() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let instance = uuid::Uuid::now_v7();
        let info = serde_json::to_string(&fixture_server_info(instance, "/w")).unwrap();
        let (cursors, mut asked) = tokio::sync::mpsc::unbounded_channel::<Option<String>>();
        let server = tokio::spawn(async move {
            let mut open_sockets = Vec::new();
            let mut next = 5;
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let head = request_head(&mut socket).await;
                if !head.starts_with("GET /v1/events ") {
                    let _ = socket
                        .write_all(response("application/json", &info).as_bytes())
                        .await;
                    continue;
                }
                let cursor = head.lines().find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("last-event-id:")
                        .map(|value| value.trim().to_owned())
                });
                let _ = cursors.send(cursor);
                let body = sse_event(instance, next);
                next += 1;
                let _ = socket
                    .write_all(
                        format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n{body}")
                            .as_bytes(),
                    )
                    .await;
                // Then nothing, and the connection stays open.
                open_sockets.push(socket);
            }
        });
        let client = ApiClient::new(&format!("http://{address}"), "token")
            .unwrap()
            .with_stream_idle(Duration::from_millis(400));
        let (mut events, task) = client.subscribe(None);
        let mut ids = Vec::new();
        let mut lost = false;
        let mut reconnected = false;
        while ids.len() < 2 || !reconnected {
            match tokio::time::timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("the stream came back")
                .unwrap()
            {
                SubscriptionEvent::Event(event) => ids.push(event.event_id),
                SubscriptionEvent::ConnectionLost { .. } => lost = true,
                SubscriptionEvent::Reconnected => reconnected = true,
                _ => {}
            }
        }
        task.abort();
        server.abort();
        assert_eq!(ids, [5, 6]);
        assert!(lost);
        assert_eq!(asked.recv().await.unwrap(), None);
        assert_eq!(asked.recv().await.unwrap().as_deref(), Some("5"));
    }

    /// A terminal socket that stopped answering, as after a sleep: given up
    /// once nothing has come for the silence limit, pings included, and
    /// connected again from its cursor.
    #[tokio::test]
    #[allow(clippy::result_large_err)]
    async fn a_terminal_socket_that_stops_answering_connects_again_from_its_cursor() {
        use futures::SinkExt as _;
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let id = haarness_protocol::PtyId(uuid::Uuid::now_v7());
        let info = serde_json::to_string(&haarness_protocol::PtyInfo {
            id,
            title: "shell".into(),
            command: "sh".into(),
            args: vec![],
            cwd: "/".into(),
            status: haarness_protocol::PtyStatus::Running,
            pid: None,
            cols: 80,
            rows: 24,
            created: false,
            directory: None,
        })
        .unwrap();
        let (queries, mut asked) = tokio::sync::mpsc::unbounded_channel::<String>();
        let server = tokio::spawn(async move {
            let mut open_sockets = Vec::new();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut head = [0_u8; 2048];
                let read = stream.peek(&mut head).await.unwrap();
                if !String::from_utf8_lossy(&head[..read])
                    .to_ascii_lowercase()
                    .contains("upgrade: websocket")
                {
                    request_head(&mut stream).await;
                    let _ = stream
                        .write_all(response("application/json", &info).as_bytes())
                        .await;
                    continue;
                }
                let queries = queries.clone();
                let mut socket = tokio_tungstenite::accept_hdr_async(
                    stream,
                    move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                          response| {
                        let _ = queries.send(request.uri().query().unwrap_or_default().into());
                        Ok(response)
                    },
                )
                .await
                .unwrap();
                socket
                    .send(Message::Text(
                        serde_json::json!({"type":"cursor","start":0,"end":3,"truncated":false})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                // Never read again: its pings go unanswered.
                open_sockets.push(socket);
            }
        });
        let client = ApiClient::new(&format!("http://{address}"), "token")
            .unwrap()
            .with_stream_idle(Duration::from_millis(600));
        client.connect_pty(id, 0);
        for expected in ["cursor=0", "cursor=3"] {
            let query = tokio::time::timeout(Duration::from_secs(5), asked.recv())
                .await
                .expect("the terminal connected again")
                .unwrap();
            assert_eq!(query, expected);
        }
        client.disconnect_all_ptys();
        server.abort();
    }

    /// The connection is back as soon as the server sends anything, a
    /// keepalive comment included: "Reconnected" no longer waited for an
    /// event or 10 s of an open stream, which left the indicator red.
    #[tokio::test]
    async fn the_outage_ends_with_the_first_byte_the_server_sends() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let instance = uuid::Uuid::now_v7();
        let info = serde_json::to_string(&fixture_server_info(instance, "/w")).unwrap();
        let server = tokio::spawn(async move {
            let mut streams = 0;
            let mut open_sockets = Vec::new();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let head = request_head(&mut socket).await;
                if !head.starts_with("GET /v1/events ") {
                    let _ = socket
                        .write_all(response("application/json", &info).as_bytes())
                        .await;
                    continue;
                }
                streams += 1;
                let body = if streams == 1 {
                    // A stream that held, then dropped.
                    sse_event(instance, 1)
                } else {
                    ": connected\n\n".to_owned()
                };
                let _ = socket
                    .write_all(
                        format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n{body}")
                            .as_bytes(),
                    )
                    .await;
                if streams == 1 {
                    continue;
                }
                open_sockets.push(socket);
            }
        });
        let client = ApiClient::new(&format!("http://{address}"), "token").unwrap();
        let (mut events, task) = client.subscribe(None);
        let mut lost = None;
        loop {
            match tokio::time::timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("the outage ended")
                .unwrap()
            {
                SubscriptionEvent::ConnectionLost { .. } => {
                    lost = Some(std::time::Instant::now());
                }
                SubscriptionEvent::Reconnected => break,
                _ => {}
            }
        }
        let back = lost.expect("the first stream dropped").elapsed();
        assert!(back < Duration::from_secs(3), "{back:?}");
        task.abort();
        server.abort();
    }

    /// A proxy that accepts a terminal's connection and never answers it:
    /// the connect is given up after the silence limit and tried again, and
    /// closing the terminal ends the wait.
    #[tokio::test]
    async fn a_terminal_connect_that_never_answers_is_given_up_and_ends_with_the_terminal() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server = tokio::spawn({
            let accepted = accepted.clone();
            async move {
                let mut held = Vec::new();
                loop {
                    let (socket, _) = listener.accept().await.unwrap();
                    accepted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    held.push(socket);
                }
            }
        });
        let client = ApiClient::new(&format!("http://{address}"), "token")
            .unwrap()
            .with_stream_idle(Duration::from_millis(300));
        let id = haarness_protocol::PtyId(uuid::Uuid::now_v7());
        client.connect_pty(id, 0);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while accepted.load(std::sync::atomic::Ordering::SeqCst) < 2
            && std::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(
            accepted.load(std::sync::atomic::Ordering::SeqCst) >= 2,
            "a connect that never answered held the terminal"
        );
        client.disconnect_all_ptys();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let after = accepted.load(std::sync::atomic::Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), after);
        server.abort();
    }

    /// The event task, started again after it ended by itself, begins from
    /// a fresh snapshot.
    #[tokio::test]
    async fn a_subscription_started_again_resyncs_first() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let instance = uuid::Uuid::now_v7();
        let info = serde_json::to_string(&fixture_server_info(instance, "/w")).unwrap();
        let server = tokio::spawn(serve_responses(
            listener,
            vec![response("application/json", &info)],
        ));
        let client = ApiClient::new(&format!("http://{address}"), "token").unwrap();
        let (mut events, task) = client.subscribe(Some((uuid::Uuid::nil(), 0)));
        // With the info it read, so the TUI need not read it again.
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), events.recv())
                .await
                .unwrap(),
            Some(SubscriptionEvent::ResyncRequired(Some(info))) if info.server_instance_id == instance
        ));
        task.abort();
        server.abort();
    }

    /// The event task started again after the previous one ended by itself:
    /// the indicator that loss raised clears with the server's first byte.
    #[tokio::test]
    async fn a_subscription_started_again_says_reconnected_once_the_server_answers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let info = serde_json::to_string(&fixture_server_info(uuid::Uuid::now_v7(), "/w")).unwrap();
        let server = tokio::spawn(async move {
            let mut open_sockets = Vec::new();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let head = request_head(&mut socket).await;
                if head.starts_with("GET /v1/events ") {
                    let _ = socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n: connected\n\n",
                        )
                        .await;
                    open_sockets.push(socket);
                } else {
                    let _ = socket
                        .write_all(response("application/json", &info).as_bytes())
                        .await;
                }
            }
        });
        let client = ApiClient::new(&format!("http://{address}"), "token").unwrap();
        let (mut events, task) = client.subscribe_again(Some((uuid::Uuid::nil(), 0)));
        let mut seen = Vec::new();
        while !seen
            .iter()
            .any(|event| matches!(event, SubscriptionEvent::Reconnected))
        {
            seen.push(
                tokio::time::timeout(Duration::from_secs(5), events.recv())
                    .await
                    .unwrap_or_else(|_| panic!("never reconnected: {seen:?}"))
                    .unwrap(),
            );
        }
        assert!(
            matches!(seen[0], SubscriptionEvent::ResyncRequired(_)),
            "{seen:?}"
        );
        task.abort();
        server.abort();
    }
}
