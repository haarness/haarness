//! Conversation-owned browser broker. A Desktop host serves one conversation
//! with Desktop's isolated Haarness browser; an extension host (Haarness for
//! Chrome, through its native-host bridge) serves every conversation of this
//! server from the user's own Chrome. The server owns routing, permissions,
//! request lifetimes, file paths and network sockets.
use crate::api::ServerState;
use crate::browser_policy::{
    BrowserPolicy, BrowserSettings, ChromeCategory, ChromePolicy, DefaultBrowser, web_origin,
};
use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{
        Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use futures::{SinkExt, StreamExt, future::BoxFuture};
use haarness_core::{
    CoreError, CoreResult, PermissionDecision, Tool, ToolContext, ToolDefinition, ToolOutput,
};
use haarness_protocol::ThreadId;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::{Mutex as AsyncMutex, mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;
const MAX_BYTES: usize = 5 * 1024 * 1024;
const MAX_HOST_MESSAGE: usize = 8 * 1024 * 1024;
/// Conversations one Chrome profile may hold tabs for at once.
const MAX_EXTENSION_THREADS: usize = 256;
/// Tabs the user took back, remembered per conversation.
const MAX_REVOKED: usize = 64;
/// Tabs of hosts that went away, remembered for their recovery text.
const MAX_RECENT_TABS: usize = 512;
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const RECOVERY: &str = "Browser disconnected or replaced. Open this conversation in Desktop, list its tabs and inspect the page before repeating any mutation; its outcome may be unknown.";
const NOT_OWNED: &str =
    "Tab does not belong to this conversation or is closed. Call browser_tabs_list.";
const NO_HAARNESS: &str = "The Haarness browser is available only while this chat is open in Haarness Desktop. Use browser \"chrome\" to work in the user's Chrome, or ask the user to open this chat in Desktop.";
const NO_CHROME: &str = "Chrome is not connected. If the user has not set up Haarness for Chrome, ask them to (Settings → Browser → Chrome, /chrome in the TUI, or `haarness browser setup`); otherwise ask them to open Chrome. Then retry.";
const CHROME_OFF: &str = "Agents may not use Chrome: it is turned off in Settings → Browser → Chrome (or /chrome in the TUI). Ask the user; do not retry.";
const CHROME_PAUSED: &str = "The user paused Haarness in Chrome. Ask them to resume it from the Haarness button in Chrome's toolbar; do not retry until they do.";
const CHROME_STOPPED: &str = "The user stopped Haarness in Chrome (they cancelled the debugging bar or pressed Stop). Ask before trying again; they can click Resume on the Haarness button in Chrome's toolbar.";
const NO_BROWSER: &str = "No browser is available to this chat. Open it in Haarness Desktop to use the Haarness browser, or set up Haarness for Chrome (/chrome in the TUI or `haarness browser setup`) to use Chrome.";
const NO_BROWSER_UNATTENDED: &str = "This scheduled run has no browser it may use unattended. Use the Haarness browser in Desktop, or pass browser \"chrome\" only if the user asked for their Chrome.";
const REVOKED: &str =
    "The user took this tab back from Haarness. Don't use it again without asking.";
const CHROME_RECOVERY: &str = "Chrome disconnected or the Haarness extension restarted. List tabs and inspect the page before repeating any mutation; its outcome may be unknown.";
const SETTINGS_CHANGED: &str = "Browser settings changed while this operation was pending. Inspect the page and request fresh authorization.";
/// What a model reads about Chrome when the user never set it up.
const CHROME_NOT_SET_UP: &str = "not set up; mention only if the user asks for their Chrome";
fn error(message: impl ToString) -> CoreError {
    CoreError::Tool(message.to_string())
}

/// The server owns what the model reads for each failure an extension
/// reports; `site-blocked` keeps the extension's message, which names the site.
fn chrome_failure(code: &str) -> Option<&'static str> {
    Some(match code {
        "paused" => CHROME_PAUSED,
        "stopped-by-user" => CHROME_STOPPED,
        "unsupported" => {
            "This browser tool is not available in Chrome; use the Haarness browser in Desktop for it."
        }
        "restricted-page" => {
            "Haarness cannot control this Chrome page (browser, extension and file pages are off limits). Navigate the tab to an http(s) site."
        }
        "not-in-group" => {
            "This tab is no longer in the chat's Chrome tab group. Call browser_tabs_list."
        }
        "background-tab" => {
            "Chrome cannot capture this tab while it is in the background of the window the user is using. Use browser_snapshot, or call browser_tabs_focus first."
        }
        _ => return None,
    })
}

/// Which browser a host drives, by its wire name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Haarness,
    Chrome,
}
impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Haarness => "haarness",
            Self::Chrome => "chrome",
        }
    }
    fn recovery(self) -> &'static str {
        match self {
            Self::Haarness => RECOVERY,
            Self::Chrome => CHROME_RECOVERY,
        }
    }
}

/// What the broker needs to know about a conversation. The daemon wiring
/// looks it up in the runtime.
#[derive(Clone, Debug, Default)]
pub struct ThreadMeta {
    pub title: String,
    pub parent: Option<ThreadId>,
    /// Part of a scheduled run (its own schedule or an ancestor's):
    /// Automatic never picks the user's Chrome for it.
    pub scheduled: bool,
}
pub type ThreadLookup =
    Arc<dyn Fn(ThreadId) -> BoxFuture<'static, Option<ThreadMeta>> + Send + Sync>;
/// The daemon's lookup: a conversation's title and parent from the runtime,
/// and whether it belongs to a scheduled run, including the subagents,
/// workflows and workflow agents such a run starts. Weak: the runtime owns
/// the tools that own the broker.
#[must_use]
pub fn runtime_threads(runtime: &Arc<haarness_core::AgentRuntime>) -> ThreadLookup {
    let runtime = Arc::downgrade(runtime);
    Arc::new(move |thread| {
        let runtime = runtime.upgrade();
        Box::pin(async move {
            let runtime = runtime?;
            let meta = runtime.get_thread(thread).await.ok()?;
            Some(ThreadMeta {
                title: meta.title,
                parent: meta.parent_id,
                scheduled: runtime.scheduled_run(thread).await,
            })
        })
    })
}
/// Live browser settings, read for every decision.
pub type SettingsSource = Arc<dyn Fn() -> CoreResult<BrowserSettings> + Send + Sync>;

pub struct BrowserBroker {
    policy: SettingsSource,
    hosts: Mutex<HashMap<ThreadId, Arc<Host>>>,
    drafts: Mutex<HashMap<uuid::Uuid, Arc<Host>>>,
    /// Chrome profiles, by the extension's browser id.
    extensions: Mutex<HashMap<String, Arc<Host>>>,
    threads: OnceLock<ThreadLookup>,
    /// The native-messaging wrapper `haarness browser setup` writes.
    chrome_wrapper: OnceLock<PathBuf>,
    recent: Mutex<VecDeque<(String, Kind)>>,
    ever_connected: AtomicBool,
    hellos: AtomicU64,
}
struct Host {
    /// Present for Chrome. A Desktop host serves one conversation.
    extension: Option<ExtensionHost>,
    policy_supported: bool,
    lease: String,
    outbound: mpsc::Sender<Value>,
    pending: Mutex<HashMap<String, oneshot::Sender<Value>>>,
    state: Mutex<Value>,
    closed: CancellationToken,
    tunnels: Mutex<HashMap<String, Arc<Tunnel>>>,
}
struct Tunnel {
    read: AsyncMutex<OwnedReadHalf>,
    write: AsyncMutex<OwnedWriteHalf>,
}
/// One Chrome profile, as its extension introduced itself.
struct ExtensionHost {
    browser_id: String,
    name: String,
    label: Option<String>,
    version: String,
    extension_version: String,
    connected_at: DateTime<Utc>,
    /// Hello order: among equally recently focused profiles, the newest wins.
    sequence: u64,
    /// Each conversation's tabs in this profile, as the extension last sent them.
    threads: Mutex<HashMap<ThreadId, Value>>,
    revoked: Mutex<HashMap<ThreadId, VecDeque<String>>>,
    status: Mutex<ExtensionStatus>,
}
#[derive(Clone, Copy, Debug, Default)]
struct ExtensionStatus {
    paused: bool,
    stopped: bool,
    focused_at: u64,
}
impl Default for BrowserBroker {
    fn default() -> Self {
        Self {
            hosts: Mutex::new(HashMap::new()),
            drafts: Mutex::new(HashMap::new()),
            extensions: Mutex::new(HashMap::new()),
            policy: Arc::new(|| Ok(BrowserSettings::default())),
            threads: OnceLock::new(),
            chrome_wrapper: OnceLock::new(),
            recent: Mutex::new(VecDeque::new()),
            ever_connected: AtomicBool::new(false),
            hellos: AtomicU64::new(0),
        }
    }
}
impl Host {
    fn kind(&self) -> Kind {
        if self.extension.is_some() {
            Kind::Chrome
        } else {
            Kind::Haarness
        }
    }
    fn tab_ids(&self) -> Vec<String> {
        let ids = |tabs: &Value| -> Vec<String> {
            tabs["tabs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|tab| tab["id"].as_str().map(str::to_owned))
                .collect()
        };
        match &self.extension {
            None => ids(&self.state.lock()),
            Some(extension) => extension.threads.lock().values().flat_map(ids).collect(),
        }
    }
    /// The reader's text for a failure outcome.
    fn failure(&self, outcome: &Value) -> String {
        let message = outcome["message"]
            .as_str()
            .unwrap_or("Browser operation failed");
        if self.extension.is_some()
            && let Some(text) = outcome["code"].as_str().and_then(chrome_failure)
        {
            return text.into();
        }
        message.into()
    }
}
impl ExtensionHost {
    fn display_name(&self) -> String {
        let name = if self.name.trim().is_empty() {
            "Chrome"
        } else {
            &self.name
        };
        match self
            .label
            .as_deref()
            .filter(|label| !label.trim().is_empty())
        {
            Some(label) => format!("{name} ({label})"),
            None => name.to_owned(),
        }
    }
    fn tabs(&self, thread: ThreadId) -> Vec<Value> {
        self.threads
            .lock()
            .get(&thread)
            .and_then(|state| state["tabs"].as_array().cloned())
            .unwrap_or_default()
    }
    fn tab(&self, thread: ThreadId, id: &str) -> Option<Value> {
        self.tabs(thread).into_iter().find(|tab| tab["id"] == id)
    }
    fn holds(&self, thread: ThreadId) -> bool {
        !self.tabs(thread).is_empty()
    }
    fn is_revoked(&self, thread: ThreadId, id: &str) -> bool {
        self.revoked
            .lock()
            .get(&thread)
            .is_some_and(|tabs| tabs.iter().any(|tab| tab == id))
    }
    fn status(&self) -> ExtensionStatus {
        *self.status.lock()
    }
    /// Paused and stopped profiles fail before anything is sent.
    fn ready(&self) -> Result<(), &'static str> {
        let status = self.status();
        if status.stopped {
            Err(CHROME_STOPPED)
        } else if status.paused {
            Err(CHROME_PAUSED)
        } else {
            Ok(())
        }
    }
    fn update_status(&self, message: &Value) {
        let mut status = self.status.lock();
        if let Some(paused) = message["paused"].as_bool() {
            status.paused = paused;
        }
        if let Some(stopped) = message["stopped"].as_bool() {
            status.stopped = stopped;
        }
        if let Some(focused_at) = message["focusedAt"].as_u64() {
            status.focused_at = focused_at;
        }
    }
    /// Replace one conversation's tabs. Anything invalid closes the host.
    fn apply_state(&self, message: &Value) -> bool {
        let Some(thread) = message["thread"]
            .as_str()
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .map(ThreadId)
        else {
            return false;
        };
        if !valid_state(&message["state"]) {
            return false;
        }
        let revoked = match &message["revoked"] {
            Value::Null => Vec::new(),
            Value::Array(ids) if ids.len() <= MAX_REVOKED => {
                let ids: Vec<String> = ids
                    .iter()
                    .filter_map(|id| id.as_str().filter(|id| valid_tab_id(id)))
                    .map(str::to_owned)
                    .collect();
                if ids.len() != message["revoked"].as_array().map_or(0, Vec::len) {
                    return false;
                }
                ids
            }
            _ => return false,
        };
        let tabs = message["state"]["tabs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        {
            let mut threads = self.threads.lock();
            if tabs.is_empty() {
                threads.remove(&thread);
            } else {
                if threads.len() >= MAX_EXTENSION_THREADS && !threads.contains_key(&thread) {
                    return false;
                }
                threads.insert(thread, message["state"].clone());
            }
        }
        let mut all = self.revoked.lock();
        if !revoked.is_empty() && (all.len() < MAX_EXTENSION_THREADS || all.contains_key(&thread)) {
            let entry = all.entry(thread).or_default();
            for id in revoked {
                if !entry.contains(&id) {
                    entry.push_back(id);
                }
            }
            while entry.len() > MAX_REVOKED {
                entry.pop_front();
            }
        }
        // A tab the user shared again belongs to the conversation again.
        if let Some(entry) = all.get_mut(&thread) {
            entry.retain(|id| !tabs.iter().any(|tab| tab["id"] == id.as_str()));
            if entry.is_empty() {
                all.remove(&thread);
            }
        }
        true
    }
}
/// What an extension says about itself in its first message.
struct Hello {
    browser_id: String,
    name: String,
    label: Option<String>,
    version: String,
    extension_version: String,
    status: ExtensionStatus,
}
impl Hello {
    fn parse(message: &Value) -> Option<Self> {
        if message["type"] != "hello" {
            return None;
        }
        let browser = &message["browser"];
        let short = |value: &Value, limit: usize| {
            value
                .as_str()
                .filter(|text| text.chars().count() <= limit)
                .map(str::to_owned)
        };
        let browser_id = uuid::Uuid::parse_str(browser["id"].as_str()?)
            .ok()?
            .to_string();
        if browser["kind"] != "chrome" {
            return None;
        }
        Some(Self {
            browser_id,
            name: short(&browser["name"], 64)?,
            label: match &browser["label"] {
                Value::Null => None,
                label => Some(short(label, 64)?),
            },
            version: short(&browser["version"], 64)?,
            extension_version: short(&message["extension"]["version"], 32)?,
            status: ExtensionStatus {
                paused: message["paused"].as_bool().unwrap_or(false),
                stopped: message["stopped"].as_bool().unwrap_or(false),
                focused_at: message["focusedAt"].as_u64().unwrap_or(0),
            },
        })
    }
}

/// One Chrome profile connected to this server, for the hosts API.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserHostInfo {
    /// Always `chrome` for Chromium browsers; `name` says which.
    pub kind: String,
    pub browser_id: String,
    pub name: String,
    pub label: Option<String>,
    pub version: String,
    pub extension_version: String,
    pub paused: bool,
    pub stopped: bool,
    pub connected_at: DateTime<Utc>,
    /// Conversations with tabs in this profile.
    pub threads: usize,
    pub tabs: usize,
    /// Tabs of the conversation named by `thread`.
    pub thread_tabs: usize,
}
/// Chrome's settings, as they apply on this server now.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChromeHostSettings {
    pub enabled: bool,
    pub default_browser: DefaultBrowser,
    /// An extension connected since this server started.
    pub ever_connected: bool,
}
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct BrowserHostsResponse {
    pub hosts: Vec<BrowserHostInfo>,
    pub chrome: ChromeHostSettings,
}

/// The HTTP(S) origin a navigation or a new tab heads for, read the way
/// Desktop normalizes a typed URL.
fn destination_origin(method: &str, args: &Value) -> Option<String> {
    if !matches!(method, "navigate" | "tabs.open") {
        return None;
    }
    let value = args["url"].as_str()?.trim();
    if value.is_empty() || value == "about:blank" {
        return None;
    }
    let scheme = value.split_once("://").is_some_and(|(scheme, _)| {
        scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
    });
    if scheme {
        return web_origin(value);
    }
    let host = value.split(['/', '?', '#']).next().unwrap_or_default();
    let host = if host.starts_with('[') {
        host.split_once(']').map_or(host, |(host, _)| host)
    } else {
        host.split(':').next().unwrap_or_default()
    }
    .to_ascii_lowercase();
    let local = host == "localhost"
        || host == "[::1"
        || (host.starts_with("127.") && host.split('.').count() == 4);
    web_origin(&format!(
        "{}://{value}",
        if local { "http" } else { "https" }
    ))
}

/// Whether `origin` is the `www.` twin of `primary` (one adds or strips a
/// leading `www.`, nothing else differs).
fn www_twin(primary: Option<&str>, origin: &str) -> bool {
    let (Some(primary), Ok(other)) = (
        primary.and_then(|p| url::Url::parse(p).ok()),
        url::Url::parse(origin),
    ) else {
        return false;
    };
    let (Some(left), Some(right)) = (primary.host_str(), other.host_str()) else {
        return false;
    };
    primary.scheme() == other.scheme()
        && primary.port_or_known_default() == other.port_or_known_default()
        && (left.strip_prefix("www.") == Some(right) || right.strip_prefix("www.") == Some(left))
}

impl BrowserBroker {
    pub fn with_policy(policy: SettingsSource) -> Arc<Self> {
        Arc::new(Self {
            policy,
            ..Self::default()
        })
    }

    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    /// Look conversations up for titles, lineage and scheduled runs. Set once
    /// by the daemon, after its runtime exists.
    pub fn set_thread_lookup(&self, lookup: ThreadLookup) {
        let _ = self.threads.set(lookup);
    }
    /// Where `haarness browser setup` writes Chrome's native-messaging
    /// wrapper, to tell "not set up" from "not connected".
    pub fn set_chrome_wrapper(&self, path: PathBuf) {
        let _ = self.chrome_wrapper.set(path);
    }
    fn host(&self, thread: ThreadId, lease: Option<&str>) -> CoreResult<Arc<Host>> {
        self.hosts
            .lock()
            .get(&thread)
            .filter(|h| !h.closed.is_cancelled() && lease.is_none_or(|id| h.lease == id))
            .cloned()
            .ok_or_else(|| error(RECOVERY))
    }
    fn draft(&self, id: uuid::Uuid, lease: &str) -> CoreResult<Arc<Host>> {
        self.drafts
            .lock()
            .get(&id)
            .filter(|host| !host.closed.is_cancelled() && host.lease == lease)
            .cloned()
            .ok_or_else(|| error(RECOVERY))
    }
    fn adopt(&self, id: uuid::Uuid, lease: &str, thread: ThreadId) -> CoreResult<()> {
        let host = self.draft(id, lease)?;
        let mut hosts = self.hosts.lock();
        if let Some((owner, _)) = hosts.iter().find(|(_, value)| Arc::ptr_eq(value, &host)) {
            return if *owner == thread {
                Ok(())
            } else {
                Err(error(
                    "This browser already belongs to another conversation",
                ))
            };
        }
        if hosts
            .get(&thread)
            .is_some_and(|value| !value.closed.is_cancelled())
        {
            return Err(error("This conversation already has a browser"));
        }
        hosts.insert(thread, host);
        // Retain the draft lease as a transport alias until the socket closes.
        // Existing page connections keep working while agent tools use the thread.
        Ok(())
    }
    /// The Desktop host attached for a conversation.
    fn desktop(&self, thread: ThreadId) -> Option<Arc<Host>> {
        self.host(thread, None).ok()
    }
    /// Connected Chrome profiles, oldest hello first.
    fn extension_hosts(&self) -> Vec<Arc<Host>> {
        let mut hosts: Vec<Arc<Host>> = self
            .extensions
            .lock()
            .values()
            .filter(|host| !host.closed.is_cancelled())
            .cloned()
            .collect();
        hosts.sort_by_key(|host| host.extension.as_ref().map_or(0, |e| e.sequence));
        hosts
    }
    async fn thread_meta(&self, thread: ThreadId) -> Option<ThreadMeta> {
        let lookup = self.threads.get()?.clone();
        lookup(thread).await
    }
    /// The conversation an extension command belongs to, so Chrome can group
    /// its tabs and title the group.
    async fn command_context(&self, thread: ThreadId) -> Value {
        let meta = self.thread_meta(thread).await.unwrap_or_default();
        json!({"thread":thread,"title":meta.title,"parent":meta.parent})
    }
    /// Remember a closed host's tabs so later calls get that browser's
    /// recovery text, then forget the host.
    fn forget(&self, host: &Arc<Host>) {
        let kind = host.kind();
        {
            let mut recent = self.recent.lock();
            for id in host.tab_ids() {
                recent.retain(|(known, _)| *known != id);
                recent.push_back((id, kind));
            }
            while recent.len() > MAX_RECENT_TABS {
                recent.pop_front();
            }
        }
        self.hosts
            .lock()
            .retain(|_, value| !Arc::ptr_eq(value, host));
        self.drafts
            .lock()
            .retain(|_, value| !Arc::ptr_eq(value, host));
        self.extensions
            .lock()
            .retain(|_, value| !Arc::ptr_eq(value, host));
    }
    fn recent_kind(&self, tab: &str) -> Option<Kind> {
        self.recent
            .lock()
            .iter()
            .rev()
            .find(|(id, _)| id == tab)
            .map(|(_, kind)| *kind)
    }
    /// The Chrome profile for a conversation: the one already holding its
    /// tabs, otherwise the most recently focused one that is not paused or
    /// stopped (the newest on a tie).
    fn chrome_host(
        &self,
        thread: ThreadId,
        settings: &BrowserSettings,
    ) -> Result<Arc<Host>, &'static str> {
        if !settings.chrome.enabled {
            return Err(CHROME_OFF);
        }
        let hosts = self.extension_hosts();
        let extension = |host: &Arc<Host>| {
            host.extension
                .as_ref()
                .map(|e| (e.status().focused_at, e.sequence))
        };
        if let Some(sticky) = hosts
            .iter()
            .filter(|host| host.extension.as_ref().is_some_and(|e| e.holds(thread)))
            .max_by_key(|host| extension(host))
        {
            sticky
                .extension
                .as_ref()
                .map_or(Ok(()), ExtensionHost::ready)?;
            return Ok(sticky.clone());
        }
        if let Some(best) = hosts
            .iter()
            .filter(|host| host.extension.as_ref().is_some_and(|e| e.ready().is_ok()))
            .max_by_key(|host| extension(host))
        {
            return Ok(best.clone());
        }
        // Every profile is paused or stopped: the latest one says why.
        match hosts.iter().max_by_key(|host| extension(host)) {
            Some(latest) => Err(latest
                .extension
                .as_ref()
                .and_then(|e| e.ready().err())
                .unwrap_or(NO_CHROME)),
            None => Err(NO_CHROME),
        }
    }
    /// Where `browser_tabs_open` goes: the model's choice, else the user's.
    async fn open_host(
        &self,
        thread: ThreadId,
        selector: Option<&str>,
        settings: &BrowserSettings,
    ) -> CoreResult<Arc<Host>> {
        let selector = match selector {
            Some("haarness") => DefaultBrowser::Haarness,
            Some("chrome") => DefaultBrowser::Chrome,
            _ => settings.default_browser,
        };
        match selector {
            DefaultBrowser::Haarness => self.desktop(thread).ok_or_else(|| error(NO_HAARNESS)),
            DefaultBrowser::Chrome => self.chrome_host(thread, settings).map_err(error),
            DefaultBrowser::Auto => {
                if let Some(host) = self.desktop(thread) {
                    return Ok(host);
                }
                if self
                    .thread_meta(thread)
                    .await
                    .is_some_and(|meta| meta.scheduled)
                {
                    return Err(error(NO_BROWSER_UNATTENDED));
                }
                if self.extension_hosts().is_empty() {
                    return Err(error(NO_BROWSER));
                }
                self.chrome_host(thread, settings).map_err(error)
            }
        }
    }
    /// Who answers `browser_tabs_list`: Desktop when attached, else the
    /// conversation's Chrome profile. `None` answers from what the server knows.
    fn list_host(&self, thread: ThreadId, settings: &BrowserSettings) -> Option<Arc<Host>> {
        self.desktop(thread)
            .or_else(|| self.chrome_host(thread, settings).ok())
    }
    /// The host that owns a tab, and the tab's generation.
    fn route_tab(
        &self,
        thread: ThreadId,
        id: &str,
        settings: &BrowserSettings,
    ) -> CoreResult<(Arc<Host>, Value)> {
        if let Some(host) = self.desktop(thread) {
            let generation = host.state.lock()["tabs"]
                .as_array()
                .and_then(|tabs| tabs.iter().find(|tab| tab["id"] == id))
                .map(|tab| tab["generation"].clone());
            if let Some(generation) = generation {
                return Ok((host, generation));
            }
        }
        let extensions = self.extension_hosts();
        for host in &extensions {
            let Some(extension) = &host.extension else {
                continue;
            };
            if let Some(tab) = extension.tab(thread, id) {
                if !settings.chrome.enabled {
                    return Err(error(CHROME_OFF));
                }
                extension.ready().map_err(error)?;
                return Ok((host.clone(), tab["generation"].clone()));
            }
        }
        if extensions.iter().any(|host| {
            host.extension
                .as_ref()
                .is_some_and(|extension| extension.is_revoked(thread, id))
        }) {
            return Err(error(REVOKED));
        }
        Err(error(
            self.recent_kind(id).map_or(NOT_OWNED, Kind::recovery),
        ))
    }
    /// The Chrome profile a prepared call was inspected on, still holding
    /// the tab for this conversation.
    fn chrome_for(
        &self,
        thread: ThreadId,
        lease: Option<&str>,
        tab: Option<&str>,
    ) -> CoreResult<Arc<Host>> {
        let host = self
            .extension_hosts()
            .into_iter()
            .find(|host| lease == Some(host.lease.as_str()))
            .ok_or_else(|| error(CHROME_RECOVERY))?;
        let extension = host
            .extension
            .as_ref()
            .ok_or_else(|| error(CHROME_RECOVERY))?;
        if let Some(id) = tab
            && extension.tab(thread, id).is_none()
        {
            return Err(error(if extension.is_revoked(thread, id) {
                REVOKED
            } else {
                NOT_OWNED
            }));
        }
        extension.ready().map_err(error)?;
        Ok(host)
    }
    /// Tabs this conversation has in every browser but `primary`, tagged.
    fn other_tabs(&self, thread: ThreadId, primary: Option<&Arc<Host>>) -> Vec<Value> {
        let other = |host: &Arc<Host>| primary.is_none_or(|primary| !Arc::ptr_eq(primary, host));
        let tag = |mut tab: Value, kind: Kind| {
            if tab.is_object() {
                tab["browser"] = json!(kind.name());
            }
            tab
        };
        let mut tabs = Vec::new();
        if let Some(host) = self.desktop(thread).filter(|host| other(host)) {
            tabs.extend(
                host.state.lock()["tabs"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|tab| tag(tab, Kind::Haarness)),
            );
        }
        for host in self.extension_hosts().iter().filter(|host| other(host)) {
            if let Some(extension) = &host.extension {
                tabs.extend(
                    extension
                        .tabs(thread)
                        .into_iter()
                        .map(|tab| tag(tab, Kind::Chrome)),
                );
            }
        }
        tabs
    }
    /// Which browsers this conversation can use now, one entry per host.
    fn browsers(&self, thread: ThreadId) -> Value {
        let enabled = (self.policy)()
            .is_ok_and(|settings| settings.policy.enabled && settings.chrome.enabled);
        let desktop = self.desktop(thread).is_some();
        let mut browsers = vec![if desktop {
            json!({"browser":"haarness","name":"Haarness browser","available":true})
        } else {
            json!({"browser":"haarness","name":"Haarness browser","available":false,"note":"only while this chat is open in Haarness Desktop"})
        }];
        let hosts = self.extension_hosts();
        for host in &hosts {
            let Some(extension) = &host.extension else {
                continue;
            };
            let note = if !enabled {
                Some("turned off in Settings → Browser → Chrome")
            } else if extension.status().stopped {
                Some("stopped by the user")
            } else if extension.status().paused {
                Some("paused by the user")
            } else {
                None
            };
            let mut entry = json!({"browser":"chrome","name":extension.display_name(),"available":note.is_none()});
            if let Some(note) = note {
                entry["note"] = json!(note);
            }
            browsers.push(entry);
        }
        if hosts.is_empty() {
            let set_up = self.ever_connected.load(Ordering::Relaxed)
                || self.chrome_wrapper.get().is_some_and(|path| path.exists());
            let note = if !set_up {
                CHROME_NOT_SET_UP
            } else if !enabled {
                "turned off in Settings → Browser → Chrome"
            } else {
                "not connected; ask the user to open Chrome"
            };
            browsers
                .push(json!({"browser":"chrome","name":"Chrome","available":false,"note":note}));
        }
        json!(browsers)
    }
    /// Tag a result with its browser; tab lists also name every browser,
    /// and `tabs.list` adds this conversation's tabs in the others.
    fn tag(
        &self,
        kind: Kind,
        host: &Arc<Host>,
        method: &str,
        thread: ThreadId,
        mut value: Value,
    ) -> Value {
        match method {
            "tabs.open" | "tabs.focus" if value.is_object() => {
                value["browser"] = json!(kind.name());
            }
            "tabs.list" | "tabs.close" if value.is_object() => {
                if let Some(tabs) = value["tabs"].as_array_mut() {
                    for tab in tabs.iter_mut().filter(|tab| tab.is_object()) {
                        tab["browser"] = json!(kind.name());
                    }
                    if method == "tabs.list" {
                        tabs.extend(self.other_tabs(thread, Some(host)));
                    }
                }
                value["browsers"] = self.browsers(thread);
            }
            _ => {}
        }
        value
    }
    /// The Chrome profiles connected to this server, for Settings and the TUI.
    pub fn hosts(&self, thread: Option<ThreadId>) -> BrowserHostsResponse {
        let (enabled, default_browser) = match (self.policy)() {
            Ok(settings) => (
                settings.policy.enabled && settings.chrome.enabled,
                settings.default_browser,
            ),
            Err(error) => {
                tracing::warn!(%error, "cannot read browser settings");
                (false, DefaultBrowser::Auto)
            }
        };
        BrowserHostsResponse {
            hosts: self
                .extension_hosts()
                .iter()
                .filter_map(|host| {
                    let extension = host.extension.as_ref()?;
                    let status = extension.status();
                    let threads = extension.threads.lock();
                    let count = |state: &Value| state["tabs"].as_array().map_or(0, Vec::len);
                    Some(BrowserHostInfo {
                        kind: "chrome".into(),
                        browser_id: extension.browser_id.clone(),
                        name: extension.name.clone(),
                        label: extension.label.clone(),
                        version: extension.version.clone(),
                        extension_version: extension.extension_version.clone(),
                        paused: status.paused,
                        stopped: status.stopped,
                        connected_at: extension.connected_at,
                        threads: threads.values().filter(|state| count(state) > 0).count(),
                        tabs: threads.values().map(count).sum(),
                        thread_tabs: thread
                            .and_then(|thread| threads.get(&thread))
                            .map_or(0, count),
                    })
                })
                .collect(),
            chrome: ChromeHostSettings {
                enabled,
                default_browser,
                ever_connected: self.ever_connected.load(Ordering::Relaxed),
            },
        }
    }
    async fn request(
        host: &Arc<Host>,
        context: &Value,
        command: Value,
        cancel: &CancellationToken,
    ) -> CoreResult<Value> {
        let recovery = host.kind().recovery();
        if host.closed.is_cancelled() {
            return Err(error(recovery));
        }
        let id = uuid::Uuid::now_v7().to_string();
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = host.pending.lock();
            if pending.len() >= 64 {
                return Err(error("Too many browser commands are pending"));
            }
            pending.insert(id.clone(), tx);
        }
        let mut message = json!({"type":"command","id":id,"command":command});
        if let (Some(message), Some(context)) = (message.as_object_mut(), context.as_object()) {
            for (key, value) in context {
                message.insert(key.clone(), value.clone());
            }
        }
        let result = async {
            host.outbound.send(message).await.map_err(|_| error(recovery))?;
            tokio::select! {
                biased;
                () = cancel.cancelled() => Err(CoreError::Cancelled),
                () = host.closed.cancelled() => Err(error(recovery)),
                result = tokio::time::timeout(Duration::from_secs(60), rx) => match result {
                    Ok(Ok(value)) if value["type"] == "success" => Ok(value["result"].clone()),
                    Ok(Ok(value)) => Err(error(host.failure(&value))),
                    _ => Err(error("Browser command timed out. Inspect the page before retrying; the action may already have run."))
                }
            }
        }.await;
        host.pending.lock().remove(&id);
        if result.is_err() {
            let _ = host.outbound.try_send(json!({"type":"cancel","id":id}));
        }
        result
    }
    async fn prepare(
        &self,
        thread: ThreadId,
        method: &str,
        mut args: Value,
        cancel: &CancellationToken,
    ) -> CoreResult<Value> {
        let settings = (self.policy)()?;
        settings.policy.ensure_enabled()?;
        // The model's browser choice routes the call; hosts never see it.
        let selector = if method == "tabs.open" {
            args.as_object_mut().and_then(|args| args.remove("browser"))
        } else {
            None
        };
        let route = if let Some(id) = args["tabID"].as_str() {
            Some(self.route_tab(thread, id, &settings)?)
        } else if method == "tabs.open" {
            let host = self
                .open_host(thread, selector.as_ref().and_then(Value::as_str), &settings)
                .await?;
            Some((host, Value::Null))
        } else if method == "tabs.list" {
            self.list_host(thread, &settings)
                .map(|host| (host, Value::Null))
        } else {
            Some((self.host(thread, None)?, Value::Null))
        };
        let Some((host, generation)) = route else {
            // No browser can serve this conversation: the tab list says why.
            args["__browser"] = json!({"lease":null,"generation":null,"target":{"resources":[],"key":"inventory"},"policy":settings.policy,"remember":[],"browser":null});
            return Ok(args);
        };
        if host.extension.is_some() {
            return self
                .prepare_chrome(
                    thread,
                    method,
                    args,
                    (host, generation),
                    &settings.chrome,
                    cancel,
                )
                .await;
        }
        let policy = settings.policy;
        if !host.policy_supported && policy != BrowserPolicy::default() {
            return Err(error(
                "Update Desktop and reconnect this conversation to enforce browser site settings.",
            ));
        }
        let mut action = args.clone();
        action["type"] = json!(method);
        let result = Self::request(
            &host,
            &Value::Null,
            json!({"action":action,"files":[],"inspect":true}),
            cancel,
        )
        .await?;
        let target = result["value"].clone();
        if !target["resources"].is_array() || !target["key"].is_string() {
            return Err(error("Invalid browser target metadata"));
        }
        let category = match method {
            "files.upload" | "files.drop" => "uploads",
            "files.get" => "downloads",
            _ => "browsing",
        };
        let mut remember = Vec::new();
        if policy.remember_approvals {
            for resource in target["resources"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if policy.decision(method, resource) == PermissionDecision::Ask {
                    let origin = url::Url::parse(resource)
                        .ok()
                        .map(|url| url.origin().ascii_serialization())
                        .unwrap_or_else(|| resource.into());
                    remember.push(format!("{category}:{origin}"));
                }
            }
            // Inventory has no site; it still needs an explicit, rememberable grant.
            if remember.is_empty()
                && target["resources"].as_array().is_some_and(Vec::is_empty)
                && policy.decision(method, "inventory") == PermissionDecision::Ask
            {
                remember.push(format!("{category}:inventory"));
            }
        }
        args["__browser"] = json!({"lease":host.lease,"generation":generation,"target":target,"policy":policy,"remember":remember,"browser":"haarness"});
        Ok(args)
    }
    /// Inspect in Chrome and gate each site the call touches: blocked sites
    /// are refused by the permission constraint, allowed ones pass, and the
    /// rest need a person's answer for this chat
    /// (`chrome-<category>:<thread>:<origin>`). Pages that are not HTTP(S)
    /// need none; Chrome refuses what is dangerous there.
    async fn prepare_chrome(
        &self,
        thread: ThreadId,
        method: &str,
        mut args: Value,
        (host, generation): (Arc<Host>, Value),
        chrome: &ChromePolicy,
        cancel: &CancellationToken,
    ) -> CoreResult<Value> {
        let context = self.command_context(thread).await;
        let mut action = args.clone();
        action["type"] = json!(method);
        let result = Self::request(
            &host,
            &context,
            json!({"action":action,"files":[],"inspect":true}),
            cancel,
        )
        .await?;
        let target = result["value"].clone();
        if !target["resources"].is_array() || !target["key"].is_string() {
            return Err(error("Invalid browser target metadata"));
        }
        let origins: Vec<String> = target["resources"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(web_origin)
            .collect();
        let primary = destination_origin(method, &args);
        let mut remember: Vec<String> = Vec::new();
        for category in ChromeCategory::for_method(method) {
            let asks = |origin: &str| chrome.decision(*category, origin) == PermissionDecision::Ask;
            for origin in &origins {
                // A www twin rides along with its site's prompt, never alone.
                let twin = primary.as_deref() != Some(origin.as_str())
                    && www_twin(primary.as_deref(), origin);
                if asks(origin) && (!twin || primary.as_deref().is_some_and(asks)) {
                    let pattern = format!("chrome-{}:{thread}:{origin}", category.name());
                    if !remember.contains(&pattern) {
                        remember.push(pattern);
                    }
                }
            }
        }
        args["__browser"] = json!({"lease":host.lease,"generation":generation,"target":target,"policy":chrome.browser_policy(),"chrome":chrome,"remember":remember,"browser":"chrome"});
        Ok(args)
    }
}

pub fn routes() -> Router<ServerState> {
    Router::new()
        .route("/threads/{id}/browser/host", get(attach))
        .route("/threads/{id}/browser/tunnel/{action}", post(tunnel))
        .route("/threads/{id}/browser/tool", post(call_browser_tool))
        .route("/browser/drafts/{id}/host", get(attach_draft))
        .route("/browser/drafts/{id}/tunnel/{action}", post(draft_tunnel))
        .route("/browser/drafts/{id}/adopt", post(adopt_draft))
        .route("/browser/extension/host", get(attach_extension))
        .route("/browser/hosts", get(list_browser_hosts))
}

async fn attach_draft(
    State(state): State<ServerState>,
    Path(id): Path<uuid::Uuid>,
    axum::Extension(shutdown): axum::Extension<CancellationToken>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some(broker) = state.browser else {
        return (StatusCode::NOT_IMPLEMENTED, "Browser unavailable").into_response();
    };
    upgrade
        .max_message_size(MAX_HOST_MESSAGE)
        .on_upgrade(move |socket| serve_host(broker, None, Some(id), socket, shutdown, true))
        .into_response()
}
#[derive(serde::Deserialize)]
struct AdoptDraft {
    thread: ThreadId,
    lease: String,
}
async fn adopt_draft(
    State(state): State<ServerState>,
    Path(id): Path<uuid::Uuid>,
    Json(input): Json<AdoptDraft>,
) -> crate::api::ApiResult<Json<Value>> {
    state.runtime.get_thread(input.thread).await?;
    state
        .browser
        .ok_or_else(|| error("Browser unavailable"))?
        .adopt(id, &input.lease, input.thread)?;
    Ok(Json(json!({"adopted":true})))
}
async fn draft_tunnel(
    State(state): State<ServerState>,
    Path((id, action)): Path<(uuid::Uuid, String)>,
    Json(input): Json<Value>,
) -> Response {
    let host = state
        .browser
        .ok_or_else(|| error("Browser unavailable"))
        .and_then(|broker| {
            broker.draft(
                id,
                input["lease"]
                    .as_str()
                    .ok_or_else(|| error("Missing browser lease"))?,
            )
        });
    let result = match host {
        Ok(host) => host_tunnel(host, &action, input).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct BrowserToolRequest {
    pub name: String,
    pub arguments: Value,
}
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserToolResponse {
    pub output: String,
    pub metadata: Value,
    pub is_error: bool,
}
#[derive(utoipa::OpenApi)]
#[openapi(paths(call_browser_tool, list_browser_hosts))]
pub struct BrowserApi;

/// Run a browser tool through the same permissions and transcript as a model call.
#[utoipa::path(post, path="/v1/threads/{id}/browser/tool", operation_id="call_browser_tool", params(("id"=String,Path)), request_body=BrowserToolRequest, responses((status=200,body=BrowserToolResponse)))]
async fn call_browser_tool(
    State(state): State<ServerState>,
    Path(thread): Path<ThreadId>,
    Json(input): Json<BrowserToolRequest>,
) -> crate::api::ApiResult<Json<BrowserToolResponse>> {
    let name = input.name.as_str();
    if !name.starts_with("browser_") {
        return Err(CoreError::Invalid("Expected browser tool".into()).into());
    }
    let cancel = CancellationToken::new();
    let _guard = cancel.clone().drop_guard();
    let result = state
        .runtime
        .run_direct_tool(thread, name, input.arguments, cancel)
        .await?;
    Ok(Json(BrowserToolResponse {
        output: result.output.text,
        metadata: result.output.metadata,
        is_error: result.is_error,
    }))
}

#[derive(serde::Deserialize)]
struct BrowserHostsQuery {
    thread: Option<ThreadId>,
}
/// The Chrome profiles connected to this server through Haarness for Chrome,
/// and how Chrome's settings apply here.
#[utoipa::path(get, path="/v1/browser/hosts", operation_id="list_browser_hosts", params(("thread"=Option<String>, Query, description="Count this conversation's tabs as threadTabs")), responses((status=200,body=BrowserHostsResponse)))]
async fn list_browser_hosts(
    State(state): State<ServerState>,
    Query(query): Query<BrowserHostsQuery>,
) -> crate::api::ApiResult<Json<BrowserHostsResponse>> {
    let broker = state
        .browser
        .ok_or_else(|| CoreError::NotFound("Browser unavailable".into()))?;
    Ok(Json(broker.hosts(query.thread)))
}

#[derive(serde::Deserialize)]
struct HostOptions {
    version: Option<u8>,
}
async fn attach(
    State(state): State<ServerState>,
    Query(options): Query<HostOptions>,
    Path(thread): Path<ThreadId>,
    axum::Extension(shutdown): axum::Extension<CancellationToken>,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !matches!(options.version.unwrap_or(1), 1 | 2) {
        return (StatusCode::BAD_REQUEST, "Unsupported browser host version").into_response();
    }
    if state.runtime.get_thread(thread).await.is_err() {
        return (StatusCode::NOT_FOUND, "Conversation not found").into_response();
    }
    let Some(broker) = state.browser else {
        return (StatusCode::NOT_IMPLEMENTED, "Browser unavailable").into_response();
    };
    upgrade
        .max_message_size(MAX_HOST_MESSAGE)
        .on_upgrade(move |socket| {
            serve_host(
                broker,
                Some(thread),
                None,
                socket,
                shutdown,
                options.version == Some(2),
            )
        })
        .into_response()
}

#[derive(serde::Deserialize)]
struct ExtensionHostOptions {
    version: Option<u8>,
}
/// Haarness for Chrome's bridge. Browsers always send Origin and the bridge
/// never does, so a web page cannot pose as Chrome; nor can anyone on a
/// server that asks for no token.
async fn attach_extension(
    State(state): State<ServerState>,
    Query(options): Query<ExtensionHostOptions>,
    headers: HeaderMap,
    anonymous: Option<axum::Extension<crate::auth::AnonymousAccess>>,
    axum::Extension(shutdown): axum::Extension<CancellationToken>,
    upgrade: WebSocketUpgrade,
) -> Response {
    if headers.contains_key(header::ORIGIN) {
        return (
            StatusCode::FORBIDDEN,
            "Browser pages cannot attach to Chrome",
        )
            .into_response();
    }
    if anonymous.is_some() {
        return (
            StatusCode::FORBIDDEN,
            "Chrome needs a Haarness server that requires its token",
        )
            .into_response();
    }
    if options.version != Some(1) {
        return (
            StatusCode::BAD_REQUEST,
            "Unsupported browser extension version",
        )
            .into_response();
    }
    let Some(broker) = state.browser else {
        return (StatusCode::NOT_IMPLEMENTED, "Browser unavailable").into_response();
    };
    let server = json!({
        "id": state.runtime.events().server_instance_id(),
        "workspace": state.workspace.display().to_string(),
        "pid": std::process::id(),
    });
    upgrade
        .max_message_size(MAX_HOST_MESSAGE)
        .on_upgrade(move |socket| serve_extension(broker, socket, shutdown, server))
        .into_response()
}
async fn serve_host(
    broker: Arc<BrowserBroker>,
    thread: Option<ThreadId>,
    draft: Option<uuid::Uuid>,
    socket: WebSocket,
    shutdown: CancellationToken,
    policy_supported: bool,
) {
    let (tx, mut rx) = mpsc::channel(128);
    let host = Arc::new(Host {
        extension: None,
        policy_supported,
        lease: uuid::Uuid::now_v7().to_string(),
        outbound: tx,
        pending: Mutex::new(HashMap::new()),
        state: Mutex::new(json!({"tabs":[],"focusedTabID":null})),
        closed: shutdown.child_token(),
        tunnels: Mutex::new(HashMap::new()),
    });
    if let Some(thread) = thread {
        if let Some(old) = broker.hosts.lock().insert(thread, host.clone()) {
            old.closed.cancel();
        }
    } else if let Some(id) = draft {
        let mut drafts = broker.drafts.lock();
        if drafts.len() >= 64 && !drafts.contains_key(&id) {
            return;
        }
        if let Some(old) = drafts.insert(id, host.clone()) {
            old.closed.cancel();
        }
    }
    let (mut writer, mut reader) = socket.split();
    if writer
        .send(Message::Text(
            json!({"type":"attached","version":1,"lease":host.lease})
                .to_string()
                .into(),
        ))
        .await
        .is_ok()
    {
        loop {
            tokio::select! {
                ()=host.closed.cancelled()=>break,
                outgoing=rx.recv()=>{let Some(value)=outgoing else {break};if writer.send(Message::Text(value.to_string().into())).await.is_err(){break}},
                incoming=reader.next()=>{match incoming {
                    Some(Ok(Message::Text(text)))=>{
                        let Ok(value)=serde_json::from_str::<Value>(&text) else {break};
                        match value["type"].as_str() {
                            Some("state")=>{if valid_state(&value["state"]){*host.state.lock()=value["state"].clone()}else{break}},
                            Some("result")=>{if let Some(id)=value["id"].as_str(){if let Some(tx)=host.pending.lock().remove(id){let _=tx.send(value["outcome"].clone());}}},
                            Some("ping")=>{let _=host.outbound.try_send(json!({"type":"pong"}));},
                            _=>break
                        }
                    },
                    Some(Ok(Message::Ping(bytes)))=>{if writer.send(Message::Pong(bytes)).await.is_err(){break}},
                    _=>break
                }}
            }
        }
    }
    host.closed.cancel();
    host.pending.lock().clear();
    host.tunnels.lock().clear();
    broker.forget(&host);
    let _ = writer.close().await;
}
/// One Chrome profile's connection. It introduces itself first (`hello`),
/// then reports each conversation's tabs (`state`), results and its paused
/// or stopped state. Unknown messages are ignored, so newer extensions keep
/// working; malformed ones close the connection.
async fn serve_extension(
    broker: Arc<BrowserBroker>,
    socket: WebSocket,
    shutdown: CancellationToken,
    server: Value,
) {
    let lease = uuid::Uuid::now_v7().to_string();
    let (mut writer, mut reader) = socket.split();
    if writer
        .send(Message::Text(
            json!({"type":"attached","version":1,"lease":lease,"server":server})
                .to_string()
                .into(),
        ))
        .await
        .is_err()
    {
        return;
    }
    let deadline = tokio::time::sleep(HELLO_TIMEOUT);
    tokio::pin!(deadline);
    let hello = loop {
        tokio::select! {
            () = shutdown.cancelled() => break None,
            () = &mut deadline => break None,
            incoming = reader.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    break serde_json::from_str::<Value>(&text)
                        .ok()
                        .and_then(|message| Hello::parse(&message));
                }
                Some(Ok(Message::Ping(bytes))) => {
                    if writer.send(Message::Pong(bytes)).await.is_err() {
                        break None;
                    }
                }
                Some(Ok(Message::Pong(_))) => {}
                _ => break None,
            }
        }
    };
    let Some(hello) = hello else {
        let _ = writer.close().await;
        return;
    };
    let (tx, mut rx) = mpsc::channel(128);
    let browser_id = hello.browser_id.clone();
    let host = Arc::new(Host {
        extension: Some(ExtensionHost {
            browser_id: hello.browser_id,
            name: hello.name,
            label: hello.label,
            version: hello.version,
            extension_version: hello.extension_version,
            connected_at: Utc::now(),
            sequence: broker.hellos.fetch_add(1, Ordering::Relaxed),
            threads: Mutex::new(HashMap::new()),
            revoked: Mutex::new(HashMap::new()),
            status: Mutex::new(hello.status),
        }),
        policy_supported: true,
        lease,
        outbound: tx,
        pending: Mutex::new(HashMap::new()),
        state: Mutex::new(json!({"tabs":[],"focusedTabID":null})),
        closed: shutdown.child_token(),
        tunnels: Mutex::new(HashMap::new()),
    });
    // The same profile reconnecting replaces its old connection.
    if let Some(old) = broker.extensions.lock().insert(browser_id, host.clone()) {
        old.closed.cancel();
    }
    broker.ever_connected.store(true, Ordering::Relaxed);
    loop {
        tokio::select! {
            () = host.closed.cancelled() => break,
            outgoing = rx.recv() => {
                let Some(value) = outgoing else { break };
                if writer.send(Message::Text(value.to_string().into())).await.is_err() {
                    break;
                }
            }
            incoming = reader.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    let Ok(value) = serde_json::from_str::<Value>(&text) else { break };
                    let Some(extension) = &host.extension else { break };
                    match value["type"].as_str() {
                        Some("state") => {
                            if !extension.apply_state(&value) {
                                break;
                            }
                        }
                        Some("result") => {
                            if let Some(id) = value["id"].as_str()
                                && let Some(tx) = host.pending.lock().remove(id)
                            {
                                let _ = tx.send(value["outcome"].clone());
                            }
                        }
                        Some("status") => extension.update_status(&value),
                        Some("ping") => {
                            let _ = host.outbound.try_send(json!({"type":"pong"}));
                        }
                        _ => {}
                    }
                }
                Some(Ok(Message::Ping(bytes))) => {
                    if writer.send(Message::Pong(bytes)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Pong(_))) => {}
                _ => break,
            }
        }
    }
    host.closed.cancel();
    host.pending.lock().clear();
    broker.forget(&host);
    let _ = writer.close().await;
}
fn valid_tab_id(id: &str) -> bool {
    id.starts_with("tab_") && id.len() == 40
}
fn valid_state(state: &Value) -> bool {
    state["tabs"].as_array().is_some_and(|tabs| {
        tabs.len() <= 32
            && tabs.iter().all(|tab| {
                tab["id"].as_str().is_some_and(valid_tab_id)
                    && tab["generation"].is_u64()
                    && tab["url"].as_str().is_some_and(|url| url.len() <= 16384)
            })
    })
}
async fn tunnel(
    State(state): State<ServerState>,
    Path((thread, action)): Path<(ThreadId, String)>,
    Json(input): Json<Value>,
) -> Response {
    let result = tunnel_request(state, thread, &action, input).await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}
async fn tunnel_request(
    state: ServerState,
    thread: ThreadId,
    action: &str,
    input: Value,
) -> CoreResult<Value> {
    let broker = state.browser.ok_or_else(|| error("Browser unavailable"))?;
    let lease = input["lease"]
        .as_str()
        .ok_or_else(|| error("Missing browser lease"))?;
    let host = broker.host(thread, Some(lease))?;
    host_tunnel(host, action, input).await
}
async fn host_tunnel(host: Arc<Host>, action: &str, input: Value) -> CoreResult<Value> {
    if action == "open" {
        if host.tunnels.lock().len() >= 128 {
            return Err(error("Browser connection limit reached"));
        }
        let hostname = input["target"]["host"]
            .as_str()
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 253
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._:%-".contains(&b))
            })
            .ok_or_else(|| error("Invalid tunnel host"))?;
        let port = input["target"]["port"]
            .as_u64()
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port > 0)
            .ok_or_else(|| error("Invalid tunnel port"))?;
        let socket = tokio::select! {()=host.closed.cancelled()=>return Err(error(RECOVERY)),value=tokio::time::timeout(Duration::from_secs(10),TcpStream::connect((hostname,port)))=>value.map_err(error)?.map_err(error)?};
        socket.set_nodelay(true).map_err(error)?;
        let (read, write) = socket.into_split();
        let id = uuid::Uuid::now_v7().to_string();
        host.tunnels.lock().insert(
            id.clone(),
            Arc::new(Tunnel {
                read: AsyncMutex::new(read),
                write: AsyncMutex::new(write),
            }),
        );
        return Ok(json!(id));
    }
    let id = input["id"]
        .as_str()
        .ok_or_else(|| error("Missing tunnel ID"))?;
    if action == "close" {
        host.tunnels.lock().remove(id);
        return Ok(Value::Null);
    }
    let socket = host
        .tunnels
        .lock()
        .get(id)
        .cloned()
        .ok_or_else(|| error("Tunnel closed"))?;
    tokio::select! {
        ()=host.closed.cancelled()=>Err(error(RECOVERY)),
        result=tokio::time::timeout(Duration::from_secs(30),async {
            match action {
                "read"=>{let mut bytes=vec![0u8;64*1024];let mut reader=socket.read.lock().await;let size=match tokio::time::timeout(Duration::from_secs(25),reader.read(&mut bytes)).await {Ok(result)=>result.map_err(error)?,Err(_)=>return Ok(json!({"data":"","eof":false}))};Ok(json!({"data":STANDARD.encode(&bytes[..size]),"eof":size==0}))},
                "write"=>{let data=input["data"].as_str().filter(|s|s.len()<=90_000).ok_or_else(||error("Invalid tunnel chunk"))?;let bytes=STANDARD.decode(data).map_err(error)?;if bytes.len()>64*1024{return Err(error("Tunnel chunk exceeds limit"));}let mut writer=socket.write.lock().await;writer.write_all(&bytes).await.map_err(error)?;if input["end"]==true{writer.shutdown().await.map_err(error)?;}Ok(Value::Null)},
                _=>Err(error("Unknown tunnel action"))
            }
        })=>result.map_err(error)?
    }
}

pub fn tools(broker: Arc<BrowserBroker>) -> Vec<Arc<dyn Tool>> {
    let catalog: Vec<Value> =
        serde_json::from_str(include_str!("browser-tools.json")).expect("compiled browser catalog");
    catalog
        .into_iter()
        .map(|entry| {
            Arc::new(BrowserTool {
                broker: broker.clone(),
                method: entry["method"].as_str().unwrap().to_owned(),
                definition: serde_json::from_value(entry).unwrap(),
            }) as Arc<dyn Tool>
        })
        .collect()
}
struct BrowserTool {
    broker: Arc<BrowserBroker>,
    method: String,
    definition: ToolDefinition,
}
#[async_trait]
impl Tool for BrowserTool {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }
    fn side_effectful(&self) -> bool {
        true
    }
    async fn prepare_arguments(
        &self,
        args: Value,
        thread: ThreadId,
        cancel: CancellationToken,
    ) -> CoreResult<Value> {
        self.broker
            .prepare(thread, &self.method, args, &cancel)
            .await
    }
    fn permission_constraint(&self, args: &Value, operation: &str) -> Option<PermissionDecision> {
        let resource = operation.strip_prefix("browser:")?.split_once(':')?.1;
        let prepared = &args["__browser"];
        if prepared["browser"] == "chrome" {
            // Pages that are not HTTP(S) have no site to gate.
            let origin = web_origin(resource)?;
            let chrome: ChromePolicy = serde_json::from_value(prepared["chrome"].clone()).ok()?;
            // A blocked www twin never blocks its site; Chrome's page filter
            // still stops a redirect to it.
            let primary = destination_origin(&self.method, args);
            let twin = primary.as_deref() != Some(origin.as_str())
                && www_twin(primary.as_deref(), &origin);
            return (chrome.method_decision(&self.method, &origin) == PermissionDecision::Deny
                && !twin)
                .then_some(PermissionDecision::Deny);
        }
        if prepared["lease"].is_null() {
            return None;
        }
        let policy: BrowserPolicy = serde_json::from_value(prepared["policy"].clone()).ok()?;
        let decision = policy.decision(&self.method, resource);
        if policy.remember_approvals && decision == PermissionDecision::Ask {
            None
        } else {
            Some(decision)
        }
    }
    /// A site in the user's own Chrome needs a person's answer, for this chat.
    fn explicit_approval(&self, _args: &Value, operation: &str) -> bool {
        operation.starts_with(haarness_core::EXPLICIT_GRANT_PREFIX)
    }
    async fn execute(&self, mut args: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        let prepared = args
            .as_object_mut()
            .and_then(|args| args.remove("__browser"))
            .ok_or_else(|| error("Browser permission preparation is missing"))?;
        let kind = if prepared["lease"].is_null() {
            None
        } else if prepared["browser"] == "chrome" {
            Some(Kind::Chrome)
        } else {
            Some(Kind::Haarness)
        };
        // Only the settings of the browser this call runs in matter: Chrome's
        // switch never interrupts the Haarness browser.
        let check_policy = || -> CoreResult<()> {
            let current = (self.broker.policy)()?;
            current.policy.ensure_enabled()?;
            let (now, then) = match kind {
                None => return Ok(()),
                Some(Kind::Haarness) => {
                    (serde_json::to_value(&current.policy), &prepared["policy"])
                }
                Some(Kind::Chrome) => {
                    if !current.chrome.enabled {
                        return Err(error(CHROME_OFF));
                    }
                    (serde_json::to_value(&current.chrome), &prepared["chrome"])
                }
            };
            if now.map_err(error)? != *then {
                return Err(error(SETTINGS_CHANGED));
            }
            Ok(())
        };
        check_policy()?;
        let Some(kind) = kind else {
            // Nothing attached: answer from what this server knows.
            let value = json!({"tabs":self.broker.other_tabs(context.thread_id, None),"focusedTabID":null,"browsers":self.broker.browsers(context.thread_id)});
            return Ok(ToolOutput {
                text: serde_json::to_string_pretty(&value).map_err(error)?,
                metadata: json!({"result":value,"attachments":[]}),
            });
        };
        let (host, request_context) = match kind {
            Kind::Haarness => (
                self.broker
                    .host(context.thread_id, prepared["lease"].as_str())?,
                Value::Null,
            ),
            Kind::Chrome => (
                self.broker.chrome_for(
                    context.thread_id,
                    prepared["lease"].as_str(),
                    args["tabID"].as_str(),
                )?,
                self.broker.command_context(context.thread_id).await,
            ),
        };
        let mut files = Vec::new();
        let mut bytes_total = 0;
        if matches!(self.method.as_str(), "files.upload" | "files.drop") {
            for raw in args["paths"]
                .as_array()
                .ok_or_else(|| error("Missing upload paths"))?
            {
                let raw = raw.as_str().ok_or_else(|| error("Invalid upload path"))?;
                let path = haarness_core::resolve_permission_path(&context.workspace, raw);
                let root = context.workspace.canonicalize().map_err(error)?;
                if !path.starts_with(&root)
                    && !context
                        .authorized_external_paths
                        .iter()
                        .any(|p| path == *p || path.starts_with(p))
                {
                    return Err(CoreError::PermissionDenied(
                        "Upload path was not authorized".into(),
                    ));
                }
                let file = tokio::fs::File::open(&path).await.map_err(error)?;
                if !file.metadata().await.map_err(error)?.is_file() {
                    return Err(error("Upload must be a regular file"));
                }
                let mut bytes = Vec::new();
                file.take((MAX_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)
                    .await
                    .map_err(error)?;
                bytes_total += bytes.len();
                if bytes_total > MAX_BYTES {
                    return Err(error("Browser uploads exceed 5 MiB total"));
                }
                files.push(json!({"id":format!("file_{}",uuid::Uuid::now_v7()),"name":path.file_name().unwrap_or_default().to_string_lossy(),"mime":"application/octet-stream","data":STANDARD.encode(bytes)}));
            }
            args["paths"] = json!(
                files
                    .iter()
                    .map(|file| file["name"].clone())
                    .collect::<Vec<_>>()
            );
        }
        args["type"] = json!(self.method);
        let mut command = json!({"action":args,"files":files,"target":prepared["target"],"policy":prepared["policy"]});
        if !prepared["generation"].is_null() {
            command["generation"] = prepared["generation"].clone();
        }
        check_policy()?;
        let request_cancel = context.cancel.child_token();
        let _request_guard = request_cancel.clone().drop_guard();
        let request = BrowserBroker::request(&host, &request_context, command, &request_cancel);
        tokio::pin!(request);
        let mut poll = tokio::time::interval(Duration::from_millis(100));
        let result = loop {
            tokio::select! {
                result = &mut request => break result?,
                _ = poll.tick() => if let Err(error) = check_policy() {
                    request_cancel.cancel();
                    let _ = request.await;
                    return Err(error);
                }
            }
        };
        check_policy()?;
        let mut value = result["value"].clone();
        let mut exported = Vec::new();
        let mut attachments = Vec::new();
        let mut total = 0;
        let files = result["files"]
            .as_array()
            .ok_or_else(|| error("Invalid browser result"))?;
        if files.len() > 8 {
            return Err(error("Too many browser files"));
        }
        let directory = context
            .workspace
            .join(".haarness-chat-attachments")
            .join(format!("browser-{}", uuid::Uuid::now_v7()));
        for file in files {
            let encoded = file["data"]
                .as_str()
                .filter(|s| s.len() < MAX_HOST_MESSAGE)
                .ok_or_else(|| error("Invalid browser file"))?;
            let bytes = STANDARD.decode(encoded).map_err(error)?;
            total += bytes.len();
            if total > MAX_BYTES {
                return Err(error("Browser result exceeds 5 MiB total"));
            }
            let root = directory
                .parent()
                .ok_or_else(|| error("Invalid browser artifact directory"))?;
            tokio::fs::create_dir_all(root).await.map_err(error)?;
            if !tokio::fs::canonicalize(root)
                .await
                .map_err(error)?
                .starts_with(context.workspace.canonicalize().map_err(error)?)
            {
                return Err(error("Browser artifact directory escapes the workspace"));
            }
            // Screenshots of signed-in pages must never reach a commit.
            let ignore = root.join(".gitignore");
            if !ignore.exists() {
                let mut options = tokio::fs::OpenOptions::new();
                options.write(true).create_new(true);
                if let Ok(mut file) = options.open(&ignore).await {
                    file.write_all(b"*\n").await.map_err(error)?;
                }
            }
            tokio::fs::create_dir_all(&directory).await.map_err(error)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                tokio::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                    .await
                    .map_err(error)?;
            }
            let name = file["name"].as_str().unwrap_or("capture");
            let safe: String = name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .take(120)
                .collect();
            let path = directory.join(format!("{}-{}", exported.len(), safe));
            let mut options = tokio::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut output = options.open(&path).await.map_err(error)?;
            output.write_all(&bytes).await.map_err(error)?;
            output.flush().await.map_err(error)?;
            let mime = file["mime"].as_str().unwrap_or("application/octet-stream");
            if matches!(mime, "image/png" | "image/jpeg" | "image/webp") {
                attachments.push(path.to_string_lossy().to_string());
            }
            let relative = format!(
                "./{}",
                path.strip_prefix(&context.workspace)
                    .map_err(error)?
                    .display()
            );
            exported.push(
                json!({"id":file["id"],"name":name,"mime":mime,"bytes":bytes.len(),"path":path,"workspaceRelativePath":relative}),
            );
        }
        if !exported.is_empty() {
            value["files"] = json!(exported);
        }
        let value = self
            .broker
            .tag(kind, &host, &self.method, context.thread_id, value);
        Ok(ToolOutput {
            text: serde_json::to_string_pretty(&value).map_err(error)?,
            metadata: json!({"result":value,"attachments":attachments,"browser":kind.name()}),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn connected(
        broker: &Arc<BrowserBroker>,
        thread: ThreadId,
    ) -> (Arc<Host>, mpsc::Receiver<Value>) {
        let (tx, rx) = mpsc::channel(128);
        let host = Arc::new(Host {
            extension: None,
            policy_supported: true,
            lease: uuid::Uuid::now_v7().to_string(),
            outbound: tx,
            pending: Mutex::new(HashMap::new()),
            state: Mutex::new(json!({"tabs":[],"focusedTabID":null})),
            closed: CancellationToken::new(),
            tunnels: Mutex::new(HashMap::new()),
        });
        broker.hosts.lock().insert(thread, host.clone());
        (host, rx)
    }
    #[tokio::test]
    async fn legacy_hosts_cannot_run_restricted_site_policies() {
        let mut policy = crate::browser_policy::BrowserPolicy::default();
        policy.defaults.browsing = haarness_core::PermissionDecision::Deny;
        let broker = BrowserBroker::with_policy(Arc::new(move || Ok(policy.clone().into())));
        let thread = ThreadId::new();
        let (tx, mut rx) = mpsc::channel(8);
        broker.hosts.lock().insert(
            thread,
            Arc::new(Host {
                extension: None,
                policy_supported: false,
                lease: "legacy".into(),
                outbound: tx,
                pending: Mutex::new(HashMap::new()),
                state: Mutex::new(json!({"tabs":[],"focusedTabID":null})),
                closed: CancellationToken::new(),
                tunnels: Mutex::new(HashMap::new()),
            }),
        );
        let error = broker
            .prepare(thread, "tabs.list", json!({}), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Update Desktop"));
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn draft_adoption_keeps_its_lease_and_cannot_replace_or_leak_to_another_thread() {
        let broker = BrowserBroker::new();
        let seed = ThreadId::new();
        let (host, _messages) = connected(&broker, seed);
        broker.hosts.lock().remove(&seed);
        let draft = uuid::Uuid::now_v7();
        broker.drafts.lock().insert(draft, host.clone());
        let target = ThreadId::new();
        assert!(broker.host(target, None).is_err());
        assert!(broker.adopt(draft, "wrong-lease", target).is_err());
        let occupied = ThreadId::new();
        connected(&broker, occupied);
        assert!(broker.adopt(draft, &host.lease, occupied).is_err());
        broker.adopt(draft, &host.lease, target).unwrap();
        broker.adopt(draft, &host.lease, target).unwrap();
        assert!(Arc::ptr_eq(&host, &broker.host(target, None).unwrap()));
        assert!(Arc::ptr_eq(
            &host,
            &broker.draft(draft, &host.lease).unwrap()
        ));
        assert!(broker.adopt(draft, &host.lease, ThreadId::new()).is_err());
        host.closed.cancel();
        assert!(broker.draft(draft, &host.lease).is_err());
        assert!(broker.host(target, None).is_err());
    }
    #[tokio::test]
    async fn cancellation_removes_request_and_never_replays() {
        let broker = BrowserBroker::new();
        let thread = ThreadId::new();
        let (host, mut messages) = connected(&broker, thread);
        let cancel = CancellationToken::new();
        let task = tokio::spawn({
            let host = host.clone();
            let cancel = cancel.clone();
            async move {
                BrowserBroker::request(
                    &host,
                    &Value::Null,
                    json!({"action":{"type":"click"}}),
                    &cancel,
                )
                .await
            }
        });
        let message = messages.recv().await.unwrap();
        assert_eq!(message["type"], "command");
        cancel.cancel();
        assert!(matches!(task.await.unwrap(), Err(CoreError::Cancelled)));
        assert!(host.pending.lock().is_empty());
        assert_eq!(messages.recv().await.unwrap()["type"], "cancel");
        assert!(messages.try_recv().is_err());
    }
    #[tokio::test]
    async fn replacement_and_foreign_conversations_cannot_execute_prepared_calls() {
        let broker = BrowserBroker::new();
        let thread = ThreadId::new();
        let (old, _) = connected(&broker, thread);
        assert!(broker.host(ThreadId::new(), None).is_err());
        let (new, _) = connected(&broker, thread);
        old.closed.cancel();
        assert!(broker.host(thread, Some(&old.lease)).is_err());
        assert_eq!(
            broker.host(thread, Some(&new.lease)).unwrap().lease,
            new.lease
        );
    }
    #[tokio::test]
    async fn prepare_captures_authoritative_target_and_ignores_supplied_metadata() {
        let broker = BrowserBroker::new();
        let thread = ThreadId::new();
        let (host, mut messages) = connected(&broker, thread);
        let task = tokio::spawn({
            let broker = broker.clone();
            async move {
                broker
                    .prepare(
                        thread,
                        "tabs.open",
                        json!({"url":"https://example.test","__browser":{"lease":"forged"}}),
                        &CancellationToken::new(),
                    )
                    .await
            }
        });
        let message = messages.recv().await.unwrap();
        let id = message["id"].as_str().unwrap();
        host.pending.lock().remove(id).unwrap().send(json!({"type":"success","result":{"value":{"resources":["https://example.test"],"key":"open"},"files":[]}})).unwrap();
        let arguments = task.await.unwrap().unwrap();
        assert_eq!(arguments["__browser"]["lease"], host.lease);
        assert_eq!(
            arguments["__browser"]["target"]["resources"][0],
            "https://example.test"
        );
    }
    #[test]
    fn catalog_is_valid_and_bounded() {
        let tools = tools(BrowserBroker::new());
        assert!(tools.len() > 35);
        for tool in tools {
            let definition = tool.definition();
            assert_eq!(definition.input_schema["type"], "object");
            jsonschema::validator_for(&definition.input_schema).unwrap();
            assert!(tool.validate_arguments(&json!({"__browser":{}})).is_err());
        }
        assert!(!valid_state(
            &json!({"tabs":[{"id":"not-a-tab","generation":0,"url":"about:blank"}]})
        ));
    }

    // ---- Chrome extension hosts ----------------------------------------

    use crate::browser_policy::{ChromeAccess, ChromeSite};
    fn tab_id() -> String {
        format!("tab_{}", uuid::Uuid::now_v7())
    }
    /// A Chrome profile connected to `broker`, focused at `focused_at`.
    fn chrome(
        broker: &Arc<BrowserBroker>,
        focused_at: u64,
        label: Option<&str>,
    ) -> (Arc<Host>, mpsc::Receiver<Value>) {
        let (tx, rx) = mpsc::channel(128);
        let id = uuid::Uuid::now_v7().to_string();
        let host = Arc::new(Host {
            extension: Some(ExtensionHost {
                browser_id: id.clone(),
                name: "Google Chrome".into(),
                label: label.map(str::to_owned),
                version: "154.0.8037.98".into(),
                extension_version: "0.1.1".into(),
                connected_at: Utc::now(),
                sequence: broker.hellos.fetch_add(1, Ordering::Relaxed),
                threads: Mutex::new(HashMap::new()),
                revoked: Mutex::new(HashMap::new()),
                status: Mutex::new(ExtensionStatus {
                    focused_at,
                    ..ExtensionStatus::default()
                }),
            }),
            policy_supported: true,
            lease: uuid::Uuid::now_v7().to_string(),
            outbound: tx,
            pending: Mutex::new(HashMap::new()),
            state: Mutex::new(json!({"tabs":[],"focusedTabID":null})),
            closed: CancellationToken::new(),
            tunnels: Mutex::new(HashMap::new()),
        });
        broker.extensions.lock().insert(id, host.clone());
        (host, rx)
    }
    fn tab(id: &str) -> Value {
        json!({"id":id,"url":"https://example.test/","title":"Example","loading":false,"canGoBack":false,"canGoForward":false,"generation":1})
    }
    fn share(host: &Host, thread: ThreadId, tabs: &[&str]) {
        assert!(host.extension.as_ref().unwrap().apply_state(&json!({
            "type":"state","thread":thread,
            "state":{"tabs":tabs.iter().map(|id| tab(id)).collect::<Vec<_>>(),"focusedTabID":null},
        })));
    }
    fn status(host: &Host, message: Value) {
        host.extension.as_ref().unwrap().update_status(&message);
    }
    /// Answer the next command a host receives.
    async fn answer(host: &Arc<Host>, messages: &mut mpsc::Receiver<Value>, value: Value) -> Value {
        answer_files(host, messages, value, json!([])).await
    }
    async fn answer_files(
        host: &Arc<Host>,
        messages: &mut mpsc::Receiver<Value>,
        value: Value,
        files: Value,
    ) -> Value {
        let message = messages.recv().await.unwrap();
        let id = message["id"].as_str().unwrap();
        host.pending
            .lock()
            .remove(id)
            .unwrap()
            .send(json!({"type":"success","result":{"value":value,"files":files}}))
            .unwrap();
        message
    }
    fn live(settings: &Arc<Mutex<BrowserSettings>>) -> Arc<BrowserBroker> {
        let settings = settings.clone();
        BrowserBroker::with_policy(Arc::new(move || Ok(settings.lock().clone())))
    }
    fn lookup(broker: &BrowserBroker, scheduled: ThreadId, parent: ThreadId) {
        broker.set_thread_lookup(Arc::new(move |thread| {
            Box::pin(async move {
                Some(ThreadMeta {
                    title: "Inbox".into(),
                    parent: Some(parent),
                    scheduled: thread == scheduled,
                })
            })
        }));
    }
    fn site(origin: &str, browsing: PermissionDecision, scripts: PermissionDecision) -> ChromeSite {
        ChromeSite {
            origin: origin.into(),
            access: ChromeAccess {
                browsing,
                scripts,
                ..ChromeAccess::default()
            },
        }
    }
    fn tool(broker: &Arc<BrowserBroker>, name: &str) -> Arc<dyn Tool> {
        tools(broker.clone())
            .into_iter()
            .find(|tool| tool.definition().name == name)
            .unwrap()
    }
    fn context(thread: ThreadId, workspace: &std::path::Path) -> ToolContext {
        ToolContext {
            execution: haarness_core::ExecutionSettings::default(),
            thread_id: thread,
            turn_id: haarness_protocol::TurnId::new(),
            workspace: workspace.to_path_buf(),
            authorized_external_paths: Vec::new(),
            cancel: CancellationToken::new(),
            invoker: None,
        }
    }
    fn message(error: CoreError) -> String {
        match error {
            CoreError::Tool(message) | CoreError::PermissionDenied(message) => message,
            other => other.to_string(),
        }
    }

    #[tokio::test]
    async fn chrome_tabs_route_by_ownership_and_carry_their_conversation() {
        let broker = BrowserBroker::new();
        let thread = ThreadId::new();
        let parent = ThreadId::new();
        lookup(&broker, ThreadId::new(), parent);
        let (host, mut messages) = chrome(&broker, 1, None);
        let id = tab_id();
        share(&host, thread, &[&id]);
        let task = tokio::spawn({
            let broker = broker.clone();
            let id = id.clone();
            async move {
                broker
                    .prepare(
                        thread,
                        "snapshot",
                        json!({"tabID":id}),
                        &CancellationToken::new(),
                    )
                    .await
            }
        });
        let inspect = answer(
            &host,
            &mut messages,
            json!({"resources":["https://example.test"],"key":"snapshot"}),
        )
        .await;
        assert_eq!(inspect["thread"], json!(thread));
        assert_eq!(inspect["title"], "Inbox");
        assert_eq!(inspect["parent"], json!(parent));
        assert_eq!(inspect["command"]["inspect"], true);
        let prepared = task.await.unwrap().unwrap();
        assert_eq!(prepared["__browser"]["browser"], "chrome");
        assert_eq!(prepared["__browser"]["lease"], host.lease);
        assert_eq!(prepared["__browser"]["generation"], 1);
        assert_eq!(
            prepared["__browser"]["remember"],
            json!([format!("chrome-browsing:{thread}:https://example.test")])
        );
        // Another conversation cannot reach the tab, and nothing is sent.
        let error = broker
            .prepare(
                ThreadId::new(),
                "snapshot",
                json!({"tabID":id}),
                &CancellationToken::new(),
            )
            .await
            .err()
            .unwrap();
        assert_eq!(message(error), NOT_OWNED);
        assert!(messages.try_recv().is_err());
    }

    #[tokio::test]
    async fn new_tabs_follow_the_selector_then_the_users_choice() {
        let settings = Arc::new(Mutex::new(BrowserSettings::default()));
        let broker = live(&settings);
        let thread = ThreadId::new();
        let scheduled = ThreadId::new();
        lookup(&broker, scheduled, ThreadId::new());
        let open = |thread, selector: Option<&'static str>| {
            let broker = broker.clone();
            let settings = settings.lock().clone();
            async move {
                broker
                    .open_host(thread, selector, &settings)
                    .await
                    .map_err(message)
            }
        };
        assert_eq!(open(thread, None).await.err().unwrap(), NO_BROWSER);
        assert_eq!(
            open(thread, Some("haarness")).await.err().unwrap(),
            NO_HAARNESS
        );
        assert_eq!(open(thread, Some("chrome")).await.err().unwrap(), NO_CHROME);
        let (profile, _) = chrome(&broker, 1, None);
        assert!(Arc::ptr_eq(&open(thread, None).await.unwrap(), &profile));
        let (desktop, _) = connected(&broker, thread);
        assert!(Arc::ptr_eq(&open(thread, None).await.unwrap(), &desktop));
        assert!(Arc::ptr_eq(
            &open(thread, Some("chrome")).await.unwrap(),
            &profile
        ));
        // A scheduled run never lands in the user's Chrome on Automatic.
        assert_eq!(
            open(scheduled, None).await.err().unwrap(),
            NO_BROWSER_UNATTENDED
        );
        assert!(Arc::ptr_eq(
            &open(scheduled, Some("chrome")).await.unwrap(),
            &profile
        ));
        // The user's default applies when the model names no browser.
        settings.lock().default_browser = DefaultBrowser::Chrome;
        assert!(Arc::ptr_eq(&open(thread, None).await.unwrap(), &profile));
        assert!(Arc::ptr_eq(
            &open(thread, Some("haarness")).await.unwrap(),
            &desktop
        ));
        settings.lock().chrome.enabled = false;
        assert_eq!(open(thread, None).await.err().unwrap(), CHROME_OFF);
        // The selector never reaches a host.
        settings.lock().default_browser = DefaultBrowser::Auto;
        let (desktop, mut messages) = connected(&broker, thread);
        let task = tokio::spawn({
            let broker = broker.clone();
            async move {
                broker
                    .prepare(
                        thread,
                        "tabs.open",
                        json!({"url":"https://example.test","browser":"haarness"}),
                        &CancellationToken::new(),
                    )
                    .await
            }
        });
        let inspect = answer(
            &desktop,
            &mut messages,
            json!({"resources":["https://example.test"],"key":"open"}),
        )
        .await;
        assert!(inspect["command"]["action"].get("browser").is_none());
        assert_eq!(inspect.get("thread"), None);
        let prepared = task.await.unwrap().unwrap();
        assert!(prepared.get("browser").is_none());
        assert_eq!(prepared["__browser"]["browser"], "haarness");
    }

    #[tokio::test]
    async fn chrome_sticks_to_a_conversations_profile_and_paused_profiles_send_nothing() {
        let settings = Arc::new(Mutex::new(BrowserSettings::default()));
        let broker = live(&settings);
        let (thread, other) = (ThreadId::new(), ThreadId::new());
        let (work, mut work_messages) = chrome(&broker, 10, Some("Work"));
        let (personal, _) = chrome(&broker, 20, Some("Personal"));
        let pick = |thread| {
            let settings = settings.lock().clone();
            broker.chrome_host(thread, &settings)
        };
        assert!(Arc::ptr_eq(&pick(thread).unwrap(), &personal));
        let id = tab_id();
        share(&work, thread, &[&id]);
        assert!(Arc::ptr_eq(&pick(thread).unwrap(), &work));
        // Equally recent: the newest connection wins.
        let (newest, _) = chrome(&broker, 20, None);
        assert!(Arc::ptr_eq(&pick(other).unwrap(), &newest));
        newest.closed.cancel();
        status(&work, json!({"paused":true}));
        assert_eq!(pick(thread).err().unwrap(), CHROME_PAUSED);
        assert!(Arc::ptr_eq(&pick(other).unwrap(), &personal));
        status(&personal, json!({"stopped":true}));
        assert_eq!(pick(other).err().unwrap(), CHROME_STOPPED);
        let error = broker
            .prepare(
                thread,
                "click",
                json!({"tabID":id,"ref":"e1"}),
                &CancellationToken::new(),
            )
            .await
            .err()
            .unwrap();
        assert_eq!(message(error), CHROME_PAUSED);
        assert!(work_messages.try_recv().is_err());
        status(&work, json!({"paused":false,"focusedAt":30}));
        settings.lock().chrome.enabled = false;
        assert_eq!(pick(thread).err().unwrap(), CHROME_OFF);
        let error = broker
            .prepare(
                thread,
                "click",
                json!({"tabID":id,"ref":"e1"}),
                &CancellationToken::new(),
            )
            .await
            .err()
            .unwrap();
        assert_eq!(message(error), CHROME_OFF);
        assert!(work_messages.try_recv().is_err());
    }

    #[test]
    fn chrome_state_is_validated_and_names_the_tabs_the_user_took_back() {
        let broker = BrowserBroker::new();
        let (host, _) = chrome(&broker, 1, None);
        let extension = host.extension.as_ref().unwrap();
        let thread = ThreadId::new();
        let (kept, taken) = (tab_id(), tab_id());
        for invalid in [
            json!({"thread":"not-a-thread","state":{"tabs":[],"focusedTabID":null}}),
            json!({"thread":thread,"state":{"tabs":[{"id":"tab_short","url":"https://example.test","generation":1}]}}),
            json!({"thread":thread,"state":{"tabs":(0..33).map(|_| tab(&tab_id())).collect::<Vec<_>>()}}),
            json!({"thread":thread,"state":{"tabs":[]},"revoked":"tab"}),
            json!({"thread":thread,"state":{"tabs":[]},"revoked":["not-a-tab"]}),
        ] {
            assert!(!extension.apply_state(&invalid), "{invalid}");
        }
        assert!(extension.apply_state(&json!({"thread":thread,"state":{"tabs":[tab(&kept)],"focusedTabID":kept},"revoked":[taken]})));
        let settings = BrowserSettings::default();
        assert_eq!(
            message(broker.route_tab(thread, &taken, &settings).err().unwrap()),
            REVOKED
        );
        assert!(broker.route_tab(thread, &kept, &settings).is_ok());
        // Shared again, it belongs to the conversation again.
        share(&host, thread, &[&kept, &taken]);
        assert!(broker.route_tab(thread, &taken, &settings).is_ok());
        // An empty list releases the conversation.
        share(&host, thread, &[]);
        assert!(!extension.holds(thread));
        for _ in 0..MAX_EXTENSION_THREADS {
            share(&host, ThreadId::new(), &[&tab_id()]);
        }
        assert!(!extension.apply_state(
            &json!({"thread":ThreadId::new(),"state":{"tabs":[tab(&tab_id())],"focusedTabID":null}})
        ));
    }

    #[test]
    fn hello_is_validated() {
        let hello = |browser: Value, extension: Value| {
            Hello::parse(
                &json!({"type":"hello","browser":browser,"extension":extension,"paused":true,"focusedAt":5}),
            )
        };
        let id = uuid::Uuid::now_v7().to_string();
        let parsed = hello(
            json!({"id":id,"kind":"chrome","name":"Microsoft Edge","label":null,"version":"140"}),
            json!({"version":"0.1.1"}),
        )
        .unwrap();
        assert!(parsed.status.paused && !parsed.status.stopped);
        assert_eq!(parsed.status.focused_at, 5);
        assert_eq!(parsed.label, None);
        let long = "x".repeat(65);
        for (browser, extension) in [
            (
                json!({"id":"profile","kind":"chrome","name":"Chrome","version":"1"}),
                json!({"version":"1"}),
            ),
            (
                json!({"id":id,"kind":"firefox","name":"Firefox","version":"1"}),
                json!({"version":"1"}),
            ),
            (
                json!({"id":id,"kind":"chrome","name":long,"version":"1"}),
                json!({"version":"1"}),
            ),
            (
                json!({"id":id,"kind":"chrome","name":"Chrome","label":long,"version":"1"}),
                json!({"version":"1"}),
            ),
            (
                json!({"id":id,"kind":"chrome","name":"Chrome","version":"1"}),
                json!({"version":"x".repeat(33)}),
            ),
            (
                json!({"id":id,"kind":"chrome","name":"Chrome"}),
                json!({"version":"1"}),
            ),
        ] {
            assert!(hello(browser.clone(), extension).is_none(), "{browser}");
        }
        assert!(Hello::parse(&json!({"type":"state"})).is_none());
    }

    #[test]
    fn closed_hosts_leave_each_browsers_recovery_text() {
        let broker = BrowserBroker::new();
        let thread = ThreadId::new();
        let (desktop, _) = connected(&broker, thread);
        let (haarness_tab, chrome_tab) = (tab_id(), tab_id());
        *desktop.state.lock() = json!({"tabs":[tab(&haarness_tab)],"focusedTabID":null});
        let (profile, _) = chrome(&broker, 1, None);
        share(&profile, thread, &[&chrome_tab]);
        broker.forget(&desktop);
        broker.forget(&profile);
        let settings = BrowserSettings::default();
        assert_eq!(
            message(
                broker
                    .route_tab(thread, &haarness_tab, &settings)
                    .err()
                    .unwrap()
            ),
            RECOVERY
        );
        assert_eq!(
            message(
                broker
                    .route_tab(thread, &chrome_tab, &settings)
                    .err()
                    .unwrap()
            ),
            CHROME_RECOVERY
        );
        assert_eq!(
            message(
                broker
                    .route_tab(thread, &tab_id(), &settings)
                    .err()
                    .unwrap()
            ),
            NOT_OWNED
        );
    }

    #[test]
    fn a_reconnected_profile_invalidates_prepared_calls() {
        let broker = BrowserBroker::new();
        let thread = ThreadId::new();
        let (old, _) = chrome(&broker, 1, None);
        let id = tab_id();
        share(&old, thread, &[&id]);
        assert!(
            broker
                .chrome_for(thread, Some(&old.lease), Some(&id))
                .is_ok()
        );
        // The same profile reconnects: a new lease, an empty inventory.
        let (new, _) = chrome(&broker, 1, None);
        old.closed.cancel();
        assert_eq!(
            message(
                broker
                    .chrome_for(thread, Some(&old.lease), Some(&id))
                    .err()
                    .unwrap()
            ),
            CHROME_RECOVERY
        );
        assert_eq!(
            message(
                broker
                    .chrome_for(thread, Some(&new.lease), Some(&id))
                    .err()
                    .unwrap()
            ),
            NOT_OWNED
        );
        share(&new, thread, &[&id]);
        assert!(
            broker
                .chrome_for(thread, Some(&new.lease), Some(&id))
                .is_ok()
        );
        assert_eq!(
            message(
                broker
                    .chrome_for(ThreadId::new(), Some(&new.lease), Some(&id))
                    .err()
                    .unwrap()
            ),
            NOT_OWNED
        );
    }

    #[tokio::test]
    async fn chrome_gates_by_category_with_www_twins_scripts_and_no_gate_off_the_web() {
        use PermissionDecision::{Allow, Ask, Deny};
        let settings = Arc::new(Mutex::new(BrowserSettings::default()));
        settings.lock().chrome.sites = vec![
            site("https://allowed.test", Allow, Allow),
            site("https://reads.test", Allow, Ask),
            site("https://blocked.test", Deny, Deny),
            site("https://www.twin.test", Deny, Deny),
        ];
        let broker = live(&settings);
        let thread = ThreadId::new();
        let (host, mut messages) = chrome(&broker, 1, None);
        let id = tab_id();
        share(&host, thread, &[&id]);
        let gate = |name: &'static str, args: Value, resources: Value| {
            let broker = broker.clone();
            let host = host.clone();
            async move {
                let tool = tool(&broker, name);
                let task = tokio::spawn({
                    let tool = tool.clone();
                    async move {
                        tool.prepare_arguments(args, thread, CancellationToken::new())
                            .await
                            .unwrap()
                    }
                });
                (tool, task, host, resources)
            }
        };
        let mut check =
            async |name: &'static str, args: Value, resources: Value| -> (Arc<dyn Tool>, Value) {
                let (tool, task, host, resources) = gate(name, args, resources).await;
                answer(
                    &host,
                    &mut messages,
                    json!({"resources":resources,"key":"k"}),
                )
                .await;
                (tool, task.await.unwrap())
            };
        let pattern = |category: &str, origin: &str| format!("chrome-{category}:{thread}:{origin}");
        // One prompt covers a site and its www twin; pages off the web need none.
        let (navigate, prepared) = check(
            "browser_navigate",
            json!({"tabID":id,"url":"example.test/inbox"}),
            json!([
                "https://example.test",
                "https://www.example.test",
                "about:blank"
            ]),
        )
        .await;
        assert_eq!(
            prepared["__browser"]["remember"],
            json!([
                pattern("browsing", "https://example.test"),
                pattern("browsing", "https://www.example.test")
            ])
        );
        assert_eq!(
            navigate.permission_constraint(&prepared, "browser:navigate:about:blank"),
            None
        );
        assert_eq!(
            navigate.permission_constraint(&prepared, "browser:navigate:https://example.test"),
            None
        );
        // A twin rides along only with its site's prompt; a blocked twin never blocks.
        let (_, prepared) = check(
            "browser_navigate",
            json!({"tabID":id,"url":"https://allowed.test"}),
            json!(["https://allowed.test", "https://www.allowed.test"]),
        )
        .await;
        assert_eq!(prepared["__browser"]["remember"], json!([]));
        let (_, prepared) = check(
            "browser_navigate",
            json!({"tabID":id,"url":"https://twin.test"}),
            json!(["https://twin.test", "https://www.twin.test"]),
        )
        .await;
        assert_eq!(
            prepared["__browser"]["remember"],
            json!([pattern("browsing", "https://twin.test")])
        );
        assert_eq!(
            navigate.permission_constraint(&prepared, "browser:navigate:https://www.twin.test"),
            None
        );
        let (_, prepared) = check(
            "browser_navigate",
            json!({"tabID":id,"url":"https://blocked.test"}),
            json!(["https://blocked.test"]),
        )
        .await;
        assert_eq!(
            navigate.permission_constraint(&prepared, "browser:navigate:https://blocked.test"),
            Some(Deny)
        );
        assert_eq!(prepared["__browser"]["remember"], json!([]));
        // Scripts ask on their own, even where browsing is allowed.
        let (evaluate, prepared) = check(
            "browser_evaluate",
            json!({"tabID":id,"script":"1"}),
            json!(["https://reads.test"]),
        )
        .await;
        assert_eq!(
            prepared["__browser"]["remember"],
            json!([pattern("scripts", "https://reads.test")])
        );
        assert_eq!(
            evaluate.permission_constraint(&prepared, "browser:evaluate:https://reads.test"),
            None
        );
        let (_, prepared) = check(
            "browser_evaluate",
            json!({"tabID":id,"script":"1"}),
            json!(["https://news.test"]),
        )
        .await;
        assert_eq!(
            prepared["__browser"]["remember"],
            json!([
                pattern("browsing", "https://news.test"),
                pattern("scripts", "https://news.test")
            ])
        );
        let (_, prepared) = check(
            "browser_evaluate",
            json!({"tabID":id,"script":"1"}),
            json!(["https://allowed.test"]),
        )
        .await;
        assert_eq!(prepared["__browser"]["remember"], json!([]));
        let (download, prepared) = check(
            "browser_files_get",
            json!({"tabID":id,"fileID":"file_1"}),
            json!(["https://news.test"]),
        )
        .await;
        assert_eq!(
            prepared["__browser"]["remember"],
            json!([pattern("downloads", "https://news.test")])
        );
        assert_eq!(prepared["__browser"]["policy"]["rememberApprovals"], true);
        assert!(download.explicit_approval(
            &prepared,
            &format!("browser_site:{}", pattern("downloads", "https://news.test"))
        ));
        assert!(!download.explicit_approval(&prepared, "browser_site:downloads:https://news.test"));
        // The inventory has no site.
        let (list, prepared) = check("browser_tabs_list", json!({}), json!([])).await;
        assert_eq!(prepared["__browser"]["remember"], json!([]));
        assert_eq!(
            list.permission_constraint(&prepared, "browser:tabs_list:inventory"),
            None
        );
    }

    #[tokio::test]
    async fn tab_lists_name_every_browser_and_answer_without_one() {
        let broker = BrowserBroker::new();
        let thread = ThreadId::new();
        let workspace =
            std::env::temp_dir().join(format!("haarness-browser-list-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&workspace).unwrap();
        let (desktop, mut messages) = connected(&broker, thread);
        let (haarness_tab, work_tab, personal_tab) = (tab_id(), tab_id(), tab_id());
        *desktop.state.lock() = json!({"tabs":[tab(&haarness_tab)],"focusedTabID":haarness_tab});
        let (work, mut work_messages) = chrome(&broker, 1, None);
        let (personal, _) = chrome(&broker, 2, Some("Personal"));
        share(&work, thread, &[&work_tab]);
        share(&personal, thread, &[&personal_tab]);
        let list = tool(&broker, "browser_tabs_list");
        let task = tokio::spawn({
            let list = list.clone();
            async move {
                list.prepare_arguments(json!({}), thread, CancellationToken::new())
                    .await
            }
        });
        answer(
            &desktop,
            &mut messages,
            json!({"resources":[],"key":"inventory"}),
        )
        .await;
        let prepared = task.await.unwrap().unwrap();
        let task = tokio::spawn({
            let list = list.clone();
            let context = context(thread, &workspace);
            async move { list.execute(prepared, context).await }
        });
        answer(
            &desktop,
            &mut messages,
            json!({"tabs":[tab(&haarness_tab)],"focusedTabID":haarness_tab}),
        )
        .await;
        let output = task.await.unwrap().unwrap();
        let value = &output.metadata["result"];
        assert_eq!(output.metadata["browser"], "haarness");
        let tabs: Vec<(&str, &str)> = value["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tab| {
                (
                    tab["id"].as_str().unwrap(),
                    tab["browser"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            tabs,
            vec![
                (haarness_tab.as_str(), "haarness"),
                (work_tab.as_str(), "chrome"),
                (personal_tab.as_str(), "chrome"),
            ]
        );
        assert_eq!(value["focusedTabID"], json!(haarness_tab));
        assert_eq!(
            value["browsers"],
            json!([
                {"browser":"haarness","name":"Haarness browser","available":true},
                {"browser":"chrome","name":"Google Chrome","available":true},
                {"browser":"chrome","name":"Google Chrome (Personal)","available":true},
            ])
        );
        // Closing a tab returns that browser's own tabs only.
        let close = tool(&broker, "browser_tabs_close");
        let task = tokio::spawn({
            let close = close.clone();
            let work_tab = work_tab.clone();
            async move {
                close
                    .prepare_arguments(json!({"tabID":work_tab}), thread, CancellationToken::new())
                    .await
            }
        });
        answer(&work, &mut work_messages, json!({"resources":[],"key":"k"})).await;
        let prepared = task.await.unwrap().unwrap();
        let task = tokio::spawn({
            let context = context(thread, &workspace);
            async move { close.execute(prepared, context).await }
        });
        answer(
            &work,
            &mut work_messages,
            json!({"tabs":[],"focusedTabID":null}),
        )
        .await;
        let output = task.await.unwrap().unwrap();
        assert_eq!(output.metadata["browser"], "chrome");
        assert_eq!(output.metadata["result"]["tabs"], json!([]));
        assert_eq!(
            output.metadata["result"]["browsers"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        // No browser at all: the list says what is possible, without an error.
        let alone = ThreadId::new();
        let fresh = BrowserBroker::new();
        let list = tool(&fresh, "browser_tabs_list");
        let prepared = list
            .prepare_arguments(json!({}), alone, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            list.permission_constraint(&prepared, "browser:tabs_list:inventory"),
            None
        );
        let output = list
            .execute(prepared, context(alone, &workspace))
            .await
            .unwrap();
        assert_eq!(
            output.metadata["result"],
            json!({"tabs":[],"focusedTabID":null,"browsers":[
                {"browser":"haarness","name":"Haarness browser","available":false,"note":"only while this chat is open in Haarness Desktop"},
                {"browser":"chrome","name":"Chrome","available":false,"note":CHROME_NOT_SET_UP},
            ]})
        );
        std::fs::remove_dir_all(workspace).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn chrome_captures_are_private_and_ignored_by_git() {
        use std::os::unix::fs::PermissionsExt as _;
        let broker = BrowserBroker::new();
        let thread = ThreadId::new();
        let workspace =
            std::env::temp_dir().join(format!("haarness-browser-capture-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&workspace).unwrap();
        let (host, mut messages) = chrome(&broker, 1, None);
        let id = tab_id();
        share(&host, thread, &[&id]);
        let screenshot = tool(&broker, "browser_screenshot");
        let task = tokio::spawn({
            let screenshot = screenshot.clone();
            let id = id.clone();
            async move {
                screenshot
                    .prepare_arguments(json!({"tabID":id}), thread, CancellationToken::new())
                    .await
            }
        });
        answer(
            &host,
            &mut messages,
            json!({"resources":["https://example.test"],"key":"shot"}),
        )
        .await;
        let prepared = task.await.unwrap().unwrap();
        let task = tokio::spawn({
            let context = context(thread, &workspace);
            async move { screenshot.execute(prepared, context).await }
        });
        let command = answer_files(
            &host,
            &mut messages,
            json!({}),
            json!([{"id":"file_1","name":"inbox.png","mime":"image/png","data":STANDARD.encode(b"png")}]),
        )
        .await;
        assert_eq!(command["thread"], json!(thread));
        let output = task.await.unwrap().unwrap();
        assert_eq!(output.metadata["browser"], "chrome");
        let path = PathBuf::from(output.metadata["attachments"][0].as_str().unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"png");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join(".haarness-chat-attachments/.gitignore"))
                .unwrap(),
            "*\n"
        );
        std::fs::remove_dir_all(workspace).unwrap();
    }

    #[tokio::test]
    async fn chromes_switch_stops_chrome_commands_but_never_the_haarness_browser() {
        let settings = Arc::new(Mutex::new(BrowserSettings::default()));
        let broker = live(&settings);
        let thread = ThreadId::new();
        let workspace =
            std::env::temp_dir().join(format!("haarness-browser-switch-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&workspace).unwrap();
        let (desktop, mut messages) = connected(&broker, thread);
        let desktop_tab = tab_id();
        *desktop.state.lock() = json!({"tabs":[tab(&desktop_tab)],"focusedTabID":null});
        let snapshot = tool(&broker, "browser_snapshot");
        let run = |args: Value| {
            let snapshot = snapshot.clone();
            let context = context(thread, &workspace);
            async move {
                let prepared = snapshot
                    .prepare_arguments(args, thread, CancellationToken::new())
                    .await?;
                snapshot.execute(prepared, context).await
            }
        };
        let task = tokio::spawn(run(json!({"tabID":desktop_tab})));
        answer(
            &desktop,
            &mut messages,
            json!({"resources":["https://example.test"],"key":"k"}),
        )
        .await;
        let command = messages.recv().await.unwrap();
        // Chrome turned off and its sites changed while Desktop works.
        {
            let mut settings = settings.lock();
            settings.chrome.enabled = false;
            settings.chrome.sites.push(site(
                "https://example.test",
                PermissionDecision::Deny,
                PermissionDecision::Deny,
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        desktop
            .pending
            .lock()
            .remove(command["id"].as_str().unwrap())
            .unwrap()
            .send(json!({"type":"success","result":{"value":{"content":"page"},"files":[]}}))
            .unwrap();
        let output = task.await.unwrap().unwrap();
        assert_eq!(output.metadata["result"]["content"], "page");
        // Chrome stops as soon as it is turned off.
        settings.lock().chrome = ChromePolicy::default();
        let (profile, mut chrome_messages) = chrome(&broker, 1, None);
        let chrome_tab = tab_id();
        share(&profile, thread, &[&chrome_tab]);
        let task = tokio::spawn(run(json!({"tabID":chrome_tab})));
        answer(
            &profile,
            &mut chrome_messages,
            json!({"resources":["https://example.test"],"key":"k"}),
        )
        .await;
        let command = chrome_messages.recv().await.unwrap();
        assert_eq!(command["command"]["policy"]["rememberApprovals"], true);
        settings.lock().chrome.enabled = false;
        assert_eq!(message(task.await.unwrap().err().unwrap()), CHROME_OFF);
        assert_eq!(
            chrome_messages.recv().await.unwrap(),
            json!({"type":"cancel","id":command["id"]})
        );
        std::fs::remove_dir_all(workspace).unwrap();
    }
}
