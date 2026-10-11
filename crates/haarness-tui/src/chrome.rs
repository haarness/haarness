//! `/chrome`: agents use the person's own Chrome — their sign-ins, their
//! network — through the Haarness for Chrome extension. The server says which
//! browsers are connected and how agents may use Chrome; this computer's setup
//! (whether Chrome can start Haarness, and the extension folder to load) comes
//! from `haarness browser status --json`. That spawns a process and reads the
//! disk, so it runs when the dialog opens and after setup, never on a timer.
//! The server is asked again every 2 s while the dialog waits for Chrome.
//!
//! Every request runs on a worker task, so the dialog keeps painting while a
//! setup copies the extension or a server is slow to answer.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use haarness_protocol::ThreadId;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::ApiClient;
use crate::state::{App, Overlay};

/// How often an open dialog asks the server again: quickly while it waits for
/// Chrome to connect, slowly once it has (Chrome may pause or go away).
const WAITING_REFRESH: Duration = Duration::from_secs(2);
const CONNECTED_REFRESH: Duration = Duration::from_secs(10);
/// Setup copies the extension folder; a status read takes a moment.
const LOCAL_TIMEOUT: Duration = Duration::from_secs(60);
/// What to run on the computer where Chrome is.
pub(crate) const SETUP_COMMAND: &str = "haarness browser setup";

/// One browser connected through Haarness for Chrome (`GET /v1/browser/hosts`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Host {
    /// The product: "Google Chrome", "Microsoft Edge", "Brave".
    pub name: String,
    /// The profile label the person gave it in the extension.
    pub label: Option<String>,
    pub extension_version: Option<String>,
    pub paused: bool,
    pub stopped: bool,
    /// Tabs the chat asked about has in this browser.
    pub thread_tabs: u64,
}

impl Host {
    /// "Google Chrome (Work)".
    pub(crate) fn title(&self) -> String {
        let name = if self.name.trim().is_empty() {
            "Chrome"
        } else {
            self.name.trim()
        };
        match self.label.as_deref().map(str::trim) {
            Some(label) if !label.is_empty() => format!("{name} ({label})"),
            _ => name.to_owned(),
        }
    }
}

/// How agents may use Chrome, as the server applies it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Access {
    /// Whether agents may use Chrome in this project, all things considered.
    pub enabled: bool,
    /// The "Let agents use Chrome" switch every project shares. Older
    /// servers leave it out.
    pub global_enabled: Option<bool>,
    /// Why Chrome is off although that switch is on: `browser-control`
    /// (agent browser control is off) or `project`.
    pub blocked_by: Option<String>,
    /// `auto`, `haarness` or `chrome`.
    pub default_browser: String,
    /// Some browser connected since this server started.
    pub ever_connected: bool,
}

impl Default for Access {
    fn default() -> Self {
        Self {
            enabled: true,
            global_enabled: None,
            blocked_by: None,
            default_browser: "auto".into(),
            ever_connected: false,
        }
    }
}

/// Why agents may not use Chrome in this project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Off {
    /// "Let agents use Chrome" is off; turning it on lets them in.
    Switch,
    /// Agent browser control is off, which keeps agents out of every
    /// browser, Chrome included. The switch here cannot change that.
    BrowserControl,
    /// The project's own settings turn Chrome off.
    Project,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Hosts {
    pub hosts: Vec<Host>,
    pub chrome: Access,
}

/// Why the server cannot say anything about Chrome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Unavailable {
    /// 403: this connection reaches the project from elsewhere (a remote or
    /// published chat), and Chrome only works on the server's computer.
    Remote,
    /// 404 or 501: a server from before Chrome support, or one without a
    /// browser to offer.
    Unsupported,
    Failed(String),
}

/// `haarness browser status --json` and `haarness browser setup --json`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Setup {
    pub web_store_url: Option<String>,
    /// The person removed the registration; only an explicit setup restores it.
    pub removed: bool,
    pub warnings: Vec<String>,
    pub browsers: Vec<SetupBrowser>,
    /// The unpacked extension copy to load, when this build carries one.
    pub extension: Option<ExtensionCopy>,
    /// What setup would copy `extension` from; `None` when this installation
    /// has no Haarness for Chrome of its own.
    pub extension_source: Option<ExtensionSource>,
    /// The servers on this computer that Chrome reaches (`status` only).
    pub connections: Vec<Connection>,
}

/// `extensionSource`: where setup finds Haarness for Chrome to copy.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct ExtensionSource {
    /// `embedded`, `bundle`, `desktop`, …
    pub kind: String,
}

/// A server `haarness browser status` found on this computer.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Connection {
    /// Its `serverInstanceId`.
    pub server_id: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct SetupBrowser {
    pub name: String,
    /// `chrome://extensions`, `edge://extensions`, …
    pub extensions_page: Option<String>,
    pub registered: bool,
    /// False for Flatpak and Snap profiles, which `note` explains.
    pub supported: bool,
    pub note: Option<String>,
}

impl Default for SetupBrowser {
    fn default() -> Self {
        Self {
            name: String::new(),
            extensions_page: None,
            registered: false,
            supported: true,
            note: None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct ExtensionCopy {
    pub path: String,
    pub version: Option<String>,
}

impl Setup {
    /// The browsers that can start Haarness now.
    pub(crate) fn ready(&self) -> impl Iterator<Item = &SetupBrowser> {
        self.browsers
            .iter()
            .filter(|browser| !self.removed && browser.registered && browser.supported)
    }

    pub(crate) fn registered(&self) -> bool {
        self.ready().next().is_some()
    }

    /// Profiles Haarness found but cannot reach, such as a Snap Chromium.
    pub(crate) fn unsupported(&self) -> impl Iterator<Item = &SetupBrowser> {
        self.browsers.iter().filter(|browser| !browser.supported)
    }

    /// Chrome on this computer reaches the server with this instance id.
    pub(crate) fn reaches(&self, server_id: &str) -> bool {
        self.connections
            .iter()
            .any(|connection| connection.server_id == server_id)
    }
}

/// The CLI's own English note that this installation has no Haarness for
/// Chrome to add. The dialog says that itself, translated, and what to do.
pub(crate) fn not_included(warning: &str) -> bool {
    warning.contains("not included in this installation")
}

/// One way this project's settings tighten Chrome, from `browser-settings`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Restriction {
    /// Chrome is turned off for this project.
    Off,
    /// A category's default is stricter here: `browsing` … `scripts`, and
    /// `ask` or `deny`.
    Default { category: String, decision: String },
    /// One site is stricter here.
    Site {
        origin: String,
        category: String,
        decision: String,
    },
    /// An older server's English sentence, shown as it came.
    Text(String),
}

impl Restriction {
    fn read(value: &Value) -> Option<Self> {
        let text = |key: &str| value[key].as_str().map(str::to_owned);
        match value["kind"].as_str()? {
            "off" => Some(Self::Off),
            "default" => Some(Self::Default {
                category: text("category")?,
                decision: text("decision")?,
            }),
            "site" => Some(Self::Site {
                origin: text("origin")?,
                category: text("category")?,
                decision: text("decision")?,
            }),
            // A kind this client does not know yet says nothing it can word.
            _ => None,
        }
    }

    /// Turns Chrome off for the project, in either form a server sends.
    fn is_off(&self) -> bool {
        match self {
            Self::Off => true,
            Self::Text(text) => {
                text.trim_end_matches('.') == "Chrome is turned off for this project"
            }
            _ => false,
        }
    }
}

/// A Chrome permission category in words (an English catalog key).
pub(crate) fn category_label(category: &str) -> Option<&'static str> {
    Some(match category {
        "browsing" => "Browsing",
        "downloads" => "Downloads",
        "uploads" => "Uploads",
        "scripts" => "Scripts",
        _ => return None,
    })
}

/// A decision in the words the Chrome site table uses (an English catalog
/// key).
pub(crate) fn decision_label(decision: &str) -> Option<&'static str> {
    Some(match decision {
        "allow" => "Always allow",
        "ask" => "Requires approval",
        "deny" => "Block",
        _ => return None,
    })
}

/// The Chrome settings, from `browser-settings`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    /// The global "Let agents use Chrome" switch, which is what this dialog
    /// shows and changes. A project can still turn Chrome off under it.
    pub enabled: bool,
    pub default_browser: String,
    /// What this project's settings tightened.
    pub restrictions: Vec<Restriction>,
}

impl Settings {
    fn from_view(chrome: &Value) -> Self {
        // No global layer yet means the defaults, which let agents in.
        let global = &chrome["globalSettings"];
        let effective = &chrome["settings"];
        let restrictions = match chrome["restrictions"].as_array() {
            Some(restrictions) => restrictions.iter().filter_map(Restriction::read).collect(),
            None => chrome["projectRestrictions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|text| !text.trim().is_empty())
                .map(|text| Restriction::Text(text.to_owned()))
                .collect(),
        };
        Self {
            enabled: global["enabled"].as_bool().unwrap_or(true),
            default_browser: global["defaultBrowser"]
                .as_str()
                .or_else(|| effective["defaultBrowser"].as_str())
                .unwrap_or("auto")
                .to_owned(),
            restrictions,
        }
    }
}

#[cfg(test)]
pub(crate) fn tests_settings_from_view(chrome: &Value) -> Settings {
    Settings::from_view(chrome)
}

/// A complete global `browser.chrome`, as a fresh install has it. The server
/// replaces the whole value on every write, so a change starts from this
/// with whatever the global layer already says laid over it.
fn default_settings() -> Value {
    json!({
        "enabled": true,
        "defaults": {"browsing": "ask", "downloads": "ask", "uploads": "ask", "scripts": "ask"},
        "defaultBrowser": "auto",
        "sites": [],
    })
}

/// Lay `layer` over `base`, object by object.
fn overlay(base: &mut Value, layer: &Value) {
    match (base, layer) {
        (Value::Object(base), Value::Object(layer)) => {
            for (key, value) in layer {
                match base.get_mut(key) {
                    Some(existing) if existing.is_object() && value.is_object() => {
                        overlay(existing, value);
                    }
                    _ => {
                        base.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (base, layer) => *base = layer.clone(),
    }
}

/// The values "Browser for new agent tabs" cycles through, in order.
pub(crate) const DEFAULT_BROWSERS: [&str; 3] = ["auto", "haarness", "chrome"];

/// A default browser in words (an English catalog key).
pub(crate) fn default_browser_label(value: &str) -> &'static str {
    match value {
        "haarness" => "Haarness browser",
        "chrome" => "Chrome",
        _ => "Automatic",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocalAction {
    Status,
    Setup,
}

impl LocalAction {
    const fn name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Setup => "setup",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    Enabled(bool),
    DefaultBrowser(&'static str),
}

/// A request on a worker task and the channel its answer comes back on.
struct Pending<T> {
    receiver: tokio::sync::oneshot::Receiver<T>,
    task: tokio::task::JoinHandle<()>,
}

/// Where a [`Pending`] request stands on this frame.
enum Delivery<T> {
    Waiting,
    Done(T),
    /// The worker went away without answering.
    Lost,
}

/// Take a finished request out of `slot`, with the key it was made under.
fn settle<K: Copy, T: Send + 'static>(
    slot: &mut Option<(K, Pending<T>)>,
) -> Option<(K, Delivery<T>)> {
    let (key, request) = slot.as_mut()?;
    let key = *key;
    match request.take() {
        Delivery::Waiting => None,
        delivery => {
            *slot = None;
            Some((key, delivery))
        }
    }
}

type HostsRequest = (ThreadId, Pending<std::result::Result<Hosts, Unavailable>>);
type LocalRequest = (LocalAction, Pending<std::result::Result<Setup, String>>);
/// Whether Chrome here reaches the project's server, with the local status
/// read that answered it (and, run from `/chrome setup`, whether to set up).
type LocateRequest = (bool, Pending<(bool, std::result::Result<Setup, String>)>);
type SettingsRequest = (
    Option<Change>,
    Pending<std::result::Result<Settings, String>>,
);

impl<T: Send + 'static> Pending<T> {
    fn spawn(work: impl std::future::Future<Output = T> + Send + 'static) -> Self {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = sender.send(work.await);
        });
        Self { receiver, task }
    }

    fn take(&mut self) -> Delivery<T> {
        match self.receiver.try_recv() {
            Ok(value) => Delivery::Done(value),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => Delivery::Waiting,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Delivery::Lost,
        }
    }
}

/// What the dialog shows, most decisive first.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum View<'a> {
    /// The server has not answered yet.
    Checking,
    Unavailable(&'a Unavailable),
    /// Agents may not use Chrome, and why.
    Off(Off),
    Connected(&'a [Host]),
    /// No browser is connected, and this terminal is not on the computer
    /// that would run Chrome for this project.
    Elsewhere,
    LocalChecking,
    LocalFailed(LocalAction, &'a str),
    SettingUp,
    /// Chrome cannot start Haarness on this computer yet.
    NotSetUp {
        removed: bool,
    },
    /// Chrome can start Haarness; the extension has not connected.
    Waiting(&'a Setup),
}

#[derive(Default)]
pub(crate) struct Chrome {
    /// The chat `hosts` counted tabs for.
    pub thread: Option<ThreadId>,
    pub hosts: Option<std::result::Result<Hosts, Unavailable>>,
    /// This computer's setup; `None` until read.
    pub local: Option<Setup>,
    /// The last local command that failed, and why.
    pub local_error: Option<(LocalAction, String)>,
    pub settings: Option<Settings>,
    /// Why the last settings change failed.
    pub settings_error: Option<String>,
    /// This terminal runs on the computer whose Chrome the project would use.
    pub here: bool,
    /// A saved switch changed what the server will report; until it says,
    /// the dialog does not guess whether agents can use Chrome now.
    recheck: bool,
    hosts_request: Option<HostsRequest>,
    local_request: Option<LocalRequest>,
    settings_request: Option<SettingsRequest>,
    locate_request: Option<LocateRequest>,
    last_refresh: Option<Instant>,
    /// The `haarness` executable that sets Chrome up; the running one unless a
    /// test says otherwise.
    pub program: Option<PathBuf>,
}

impl Chrome {
    /// Nothing is in flight.
    #[cfg(test)]
    pub(crate) fn idle(&self) -> bool {
        self.hosts_request.is_none()
            && self.local_request.is_none()
            && self.settings_request.is_none()
            && self.locate_request.is_none()
    }

    pub(crate) fn busy_local(&self) -> Option<LocalAction> {
        self.local_request.as_ref().map(|(action, _)| *action)
    }

    pub(crate) fn saving(&self) -> Option<Change> {
        self.settings_request
            .as_ref()
            .and_then(|(change, _)| *change)
    }

    /// The two settings this dialog changes, as the server last reported
    /// them (a settings read or write updates that report at once): the
    /// shared "Let agents use Chrome" switch, which is not whether agents
    /// may use Chrome in this project, and the browser for new agent tabs.
    pub(crate) fn access(&self) -> Option<(bool, &str)> {
        match &self.hosts {
            Some(Ok(hosts)) => Some((
                hosts
                    .chrome
                    .global_enabled
                    .or_else(|| self.settings.as_ref().map(|settings| settings.enabled))
                    .unwrap_or(hosts.chrome.enabled),
                hosts.chrome.default_browser.as_str(),
            )),
            _ => None,
        }
    }

    /// Why agents may not use Chrome in this project, when they may not.
    pub(crate) fn off(&self) -> Option<Off> {
        let Some(Ok(hosts)) = &self.hosts else {
            return None;
        };
        if hosts.chrome.enabled {
            return None;
        }
        // Turning the switch on is the first step whatever else is off.
        if self.access().is_some_and(|(switch, _)| !switch) {
            return Some(Off::Switch);
        }
        match hosts.chrome.blocked_by.as_deref() {
            Some("browser-control") => Some(Off::BrowserControl),
            Some("project") => Some(Off::Project),
            // An older server only says Chrome is off. With the switch on,
            // either this project turned it off, which its restrictions
            // say, or agent browser control is off.
            _ if self
                .settings
                .as_ref()
                .is_some_and(|settings| settings.restrictions.iter().any(Restriction::is_off)) =>
            {
                Some(Off::Project)
            }
            _ => Some(Off::BrowserControl),
        }
    }

    /// Tabs `thread` has across every connected browser.
    pub(crate) fn thread_tabs(&self, thread: ThreadId) -> u64 {
        match &self.hosts {
            Some(Ok(hosts)) if self.thread == Some(thread) => {
                hosts.hosts.iter().map(|host| host.thread_tabs).sum()
            }
            _ => 0,
        }
    }

    pub(crate) fn view(&self) -> View<'_> {
        let hosts = match &self.hosts {
            None => return View::Checking,
            Some(Err(problem)) => return View::Unavailable(problem),
            Some(Ok(_)) if self.recheck => return View::Checking,
            Some(Ok(hosts)) => hosts,
        };
        if let Some(off) = self.off() {
            return View::Off(off);
        }
        if !hosts.hosts.is_empty() {
            return View::Connected(&hosts.hosts);
        }
        if !self.here {
            // Asking this computer whether its Chrome reaches the project.
            if self.locate_request.is_some() {
                return View::LocalChecking;
            }
            return View::Elsewhere;
        }
        if self.busy_local() == Some(LocalAction::Setup) {
            return View::SettingUp;
        }
        if let Some((action, error)) = &self.local_error {
            return View::LocalFailed(*action, error);
        }
        match &self.local {
            None => View::LocalChecking,
            Some(setup) if !setup.registered() => View::NotSetUp {
                removed: setup.removed,
            },
            Some(setup) => View::Waiting(setup),
        }
    }

    /// The extension folder to paste into Chrome's Load unpacked picker.
    pub(crate) fn extension_path(&self) -> Option<&str> {
        self.local
            .as_ref()
            .and_then(|setup| setup.extension.as_ref())
            .map(|extension| extension.path.as_str())
            .filter(|path| !path.is_empty())
    }

    pub(crate) fn web_store_url(&self) -> Option<&str> {
        self.local
            .as_ref()
            .and_then(|setup| setup.web_store_url.as_deref())
            .filter(|url| url.starts_with("https://"))
    }

    /// This installation has no Haarness for Chrome to add (a build without
    /// it, before the Web Store listing), so nothing will connect until a
    /// release or Desktop provides it.
    pub(crate) fn nothing_to_add(&self) -> bool {
        self.web_store_url().is_none()
            && self.extension_path().is_none()
            && self
                .local
                .as_ref()
                .is_none_or(|setup| setup.extension_source.is_none())
    }

    fn program(&self) -> Option<PathBuf> {
        if self.program.is_some() {
            return self.program.clone();
        }
        // A test binary is not `haarness`: running it would run the tests.
        if cfg!(test) {
            return None;
        }
        std::env::current_exe().ok()
    }
}

/// `/status`: one line on whether agents can use Chrome in this chat.
pub(crate) fn status_line(
    result: &std::result::Result<Hosts, Unavailable>,
    language: crate::locale::Language,
) -> String {
    let hosts = match result {
        Ok(hosts) => hosts,
        Err(Unavailable::Remote) => {
            return language
                .text("Works on the computer running this project")
                .to_owned();
        }
        Err(Unavailable::Unsupported) => {
            return language.text("Not supported by this server").to_owned();
        }
        Err(Unavailable::Failed(_)) => return language.text("Unavailable").to_owned(),
    };
    if !hosts.chrome.enabled {
        return language.text("Turned off").to_owned();
    }
    let Some(first) = hosts.hosts.first() else {
        return language.text("Not connected · /chrome").to_owned();
    };
    if hosts.hosts.iter().all(|host| host.stopped) {
        return language.text("Stopped in Chrome").to_owned();
    }
    if hosts.hosts.iter().all(|host| host.paused || host.stopped) {
        return language.text("Paused in Chrome").to_owned();
    }
    let mut text = if hosts.hosts.len() == 1 {
        language.format("Connected · {browser}", &[("browser", &first.title())])
    } else {
        language.format(
            "{count} browsers connected",
            &[("count", &hosts.hosts.len().to_string())],
        )
    };
    let tabs: u64 = hosts.hosts.iter().map(|host| host.thread_tabs).sum();
    if tabs > 0 {
        text.push_str(" · ");
        text.push_str(&tab_count(tabs, language));
    }
    text
}

/// "1 tab in this chat", "3 tabs in this chat".
pub(crate) fn tab_count(tabs: u64, language: crate::locale::Language) -> String {
    if tabs == 1 {
        language.text("1 tab in this chat").to_owned()
    } else {
        language.format("{count} tabs in this chat", &[("count", &tabs.to_string())])
    }
}

/// Ask the server about Chrome, treating the refusals that mean "not here"
/// as an answer rather than an error.
pub(crate) async fn fetch_hosts(
    client: &ApiClient,
    thread: ThreadId,
) -> std::result::Result<Hosts, Unavailable> {
    client.browser_hosts(Some(thread)).await.map_err(|error| {
        match crate::api::refusal_status(&error).map(|s| s.as_u16()) {
            Some(403) => Unavailable::Remote,
            Some(404 | 501) => Unavailable::Unsupported,
            _ => Unavailable::Failed(error.to_string()),
        }
    })
}

/// Read the Chrome settings, or change one of them. Chrome settings are
/// global (one Chrome serves every project), so a change is written to the
/// global layer whole: the global value as stored, with the one field changed.
async fn settings_request(
    client: ApiClient,
    change: Option<Change>,
) -> std::result::Result<Settings, String> {
    let read = client
        .manage(
            "browser-settings",
            &json!({"action": "get", "scope": "global"}),
        )
        .await
        .map_err(|error| error.to_string())?;
    let data = &read["data"];
    let Some(chrome) = data.get("chrome").filter(|chrome| chrome.is_object()) else {
        return Err(
            "This Haarness server cannot change Chrome settings. Update Haarness and restart this project's server."
                .into(),
        );
    };
    let Some(change) = change else {
        return Ok(Settings::from_view(chrome));
    };
    let mut complete = default_settings();
    if chrome["globalSettings"].is_object() {
        overlay(&mut complete, &chrome["globalSettings"]);
    }
    match change {
        Change::Enabled(enabled) => complete["enabled"] = json!(enabled),
        Change::DefaultBrowser(browser) => complete["defaultBrowser"] = json!(browser),
    }
    let saved = client
        .manage(
            "browser-settings",
            &json!({
                "action": "set",
                "scope": "global",
                "chrome": complete,
                "revision": data["revision"],
            }),
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(Settings::from_view(&saved["data"]["chrome"]))
}

/// Run `haarness browser <status|setup> --json` and read its report.
async fn local_request(
    program: PathBuf,
    action: LocalAction,
) -> std::result::Result<Setup, String> {
    let mut command = tokio::process::Command::new(&program);
    command
        .args(["browser", action.name(), "--json"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(LOCAL_TIMEOUT, command.output())
        .await
        .map_err(|_| "Haarness took too long to answer".to_owned())?
        .map_err(|error| error.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report = serde_json::from_str::<Value>(stdout.trim()).ok();
    if output.status.success()
        && let Some(report) = report.as_ref().filter(|report| report.is_object())
    {
        return serde_json::from_value(report.clone()).map_err(|error| error.to_string());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(report
        .as_ref()
        .and_then(|report| {
            report["error"]
                .as_str()
                .or_else(|| report["message"].as_str())
        })
        .map(str::to_owned)
        .or_else(|| {
            stderr
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(|line| line.strip_prefix("error: ").unwrap_or(line).to_owned())
        })
        .unwrap_or_else(|| {
            format!(
                "haarness browser {} failed ({})",
                action.name(),
                output.status
            )
        }))
}

/// Whether this computer's Chrome reaches the project's server, for a
/// terminal attached to it by address: `browser status` lists the servers
/// Chrome finds on this computer, and the project's own `/v1/info` says
/// which one it is. A loopback address alone would also match a tunnel to
/// another computer. The status read comes back too, to show without
/// running it again.
async fn locate(client: ApiClient, program: PathBuf) -> (bool, std::result::Result<Setup, String>) {
    let info = async {
        tokio::time::timeout(Duration::from_secs(5), client.info())
            .await
            .ok()
            .and_then(Result::ok)
    };
    let (info, status) = tokio::join!(info, local_request(program, LocalAction::Status));
    let here = match (&info, &status) {
        (Some(info), Ok(setup)) => setup.reaches(&info.server_instance_id.to_string()),
        _ => false,
    };
    (here, status)
}

/// `/chrome [setup]`.
pub(crate) fn parse(arguments: &str) -> std::result::Result<bool, String> {
    match arguments.trim().to_ascii_lowercase().as_str() {
        "" | "status" => Ok(false),
        "setup" | "set-up" | "install" => Ok(true),
        other => Err(format!(
            "Unknown option {other} · use /chrome or /chrome setup"
        )),
    }
}

impl App {
    pub(crate) fn chrome_command(&mut self, arguments: &str, client: &ApiClient) -> Result<()> {
        let setup = parse(arguments).map_err(anyhow::Error::msg)?;
        self.open_chrome(client);
        if setup {
            if self.chrome.here {
                self.request_chrome_local(LocalAction::Setup);
            } else if let Some((then_setup, _)) = &mut self.chrome.locate_request {
                *then_setup = true;
            }
        }
        Ok(())
    }

    /// Open the dialog and ask everything it shows afresh.
    pub(crate) fn open_chrome(&mut self, client: &ApiClient) {
        self.open_overlay(Overlay::Chrome);
        let ssh = crate::connect::over_ssh();
        self.chrome.here = client.supports_local_workspace_switch() && !ssh;
        self.chrome.settings_error = None;
        self.chrome.recheck = false;
        self.request_chrome_hosts(client);
        self.request_chrome_settings(None, client);
        if self.chrome.here {
            self.request_chrome_local(LocalAction::Status);
        } else if !ssh {
            // Attached by address, this terminal may still be on the
            // computer that runs the project.
            self.request_chrome_locate(client);
        }
    }

    fn request_chrome_locate(&mut self, client: &ApiClient) {
        if let Some((_, previous)) = self.chrome.locate_request.take() {
            previous.task.abort();
        }
        let Some(program) = self.chrome.program() else {
            return;
        };
        let client = client.clone();
        self.chrome.locate_request = Some((
            false,
            Pending::spawn(async move { locate(client, program).await }),
        ));
    }

    fn request_chrome_hosts(&mut self, client: &ApiClient) {
        if let Some((_, previous)) = self.chrome.hosts_request.take() {
            previous.task.abort();
        }
        let client = client.clone();
        let thread = self.thread.id;
        self.chrome.hosts_request = Some((
            thread,
            Pending::spawn(async move { fetch_hosts(&client, thread).await }),
        ));
        self.chrome.last_refresh = Some(Instant::now());
    }

    /// Read the settings (`None`) or change one. A change is never replaced
    /// by a read; a read gives way to a change.
    fn request_chrome_settings(&mut self, change: Option<Change>, client: &ApiClient) {
        if let Some((previous_change, previous)) = self.chrome.settings_request.take() {
            if previous_change.is_some() && change.is_none() {
                self.chrome.settings_request = Some((previous_change, previous));
                return;
            }
            previous.task.abort();
        }
        if change.is_some() {
            self.chrome.settings_error = None;
        }
        let client = client.clone();
        self.chrome.settings_request = Some((
            change,
            Pending::spawn(async move { settings_request(client, change).await }),
        ));
    }

    /// Run the local command. Setup is never interrupted; a status read gives
    /// way to it.
    pub(crate) fn request_chrome_local(&mut self, action: LocalAction) {
        if self.chrome.busy_local() == Some(LocalAction::Setup) {
            return;
        }
        let Some(program) = self.chrome.program() else {
            self.chrome.local_error = Some((
                action,
                "Haarness could not find its own program to set up Chrome".into(),
            ));
            return;
        };
        if let Some((_, previous)) = self.chrome.local_request.take() {
            previous.task.abort();
        }
        self.chrome.local_error = None;
        self.chrome.local_request = Some((
            action,
            Pending::spawn(async move { local_request(program, action).await }),
        ));
    }

    /// Frame-tick hook: take delivery of finished requests and keep an open
    /// dialog current.
    pub(crate) fn poll_chrome(&mut self, client: &ApiClient) {
        let open = matches!(self.overlay, Overlay::Chrome);
        let mut refresh = false;
        if let Some((thread, delivery)) = settle(&mut self.chrome.hosts_request) {
            match delivery {
                // A poll that could not reach the server changes nothing it
                // knew; a refusal is an answer.
                Delivery::Done(Err(Unavailable::Failed(_)))
                    if matches!(self.chrome.hosts, Some(Ok(_))) => {}
                Delivery::Done(result) => {
                    self.chrome.thread = Some(thread);
                    self.chrome.hosts = Some(result);
                    self.chrome.recheck = false;
                }
                Delivery::Waiting | Delivery::Lost => {}
            }
        }
        if let Some((change, delivery)) = settle(&mut self.chrome.settings_request) {
            match delivery {
                Delivery::Done(Ok(settings)) => {
                    // Show what was saved now rather than when the hosts
                    // are next asked for. Whether agents may use Chrome in
                    // this project is the server's to say: a project or
                    // agent browser control can keep it off under the
                    // switch.
                    if let Some(Ok(hosts)) = &mut self.chrome.hosts {
                        hosts.chrome.global_enabled = Some(settings.enabled);
                        hosts
                            .chrome
                            .default_browser
                            .clone_from(&settings.default_browser);
                        if matches!(change, Some(Change::Enabled(_))) {
                            self.chrome.recheck = true;
                        }
                    }
                    self.chrome.settings = Some(settings);
                    if change.is_some() {
                        // The server's view of Chrome follows the new settings.
                        refresh = true;
                        if !open {
                            self.notice = "Chrome settings saved".into();
                        }
                    }
                }
                Delivery::Done(Err(error)) if change.is_some() => {
                    if !open {
                        self.notice.clone_from(&error);
                    }
                    self.chrome.settings_error = Some(error);
                }
                // A failed read changes nothing it knew.
                Delivery::Done(Err(_)) | Delivery::Waiting | Delivery::Lost => {}
            }
        }
        if let Some((action, delivery)) = settle(&mut self.chrome.local_request) {
            match delivery {
                Delivery::Done(Ok(setup)) => {
                    if action == LocalAction::Setup {
                        refresh = true;
                        if !open {
                            self.notice = if setup.registered() {
                                "Chrome can start Haarness · /chrome shows how to add it".into()
                            } else {
                                "Chrome setup did not finish · /chrome".into()
                            };
                        }
                    }
                    self.chrome.local = Some(setup);
                }
                Delivery::Done(Err(error)) => {
                    if action == LocalAction::Setup && !open {
                        self.notice.clone_from(&error);
                    }
                    self.chrome.local_error = Some((action, error));
                }
                Delivery::Waiting | Delivery::Lost => {}
            }
        }
        // Not reached from here, the dialog stays with "elsewhere".
        if let Some((then_setup, Delivery::Done((true, status)))) =
            settle(&mut self.chrome.locate_request)
        {
            self.chrome.here = true;
            match status {
                Ok(setup) => self.chrome.local = Some(setup),
                Err(error) => self.chrome.local_error = Some((LocalAction::Status, error)),
            }
            if then_setup {
                self.request_chrome_local(LocalAction::Setup);
            }
        }
        if refresh {
            self.request_chrome_hosts(client);
            return;
        }
        if !open || self.chrome.hosts_request.is_some() {
            return;
        }
        let interval = match &self.chrome.hosts {
            Some(Ok(hosts)) if hosts.hosts.is_empty() => WAITING_REFRESH,
            _ => CONNECTED_REFRESH,
        };
        if self
            .chrome
            .last_refresh
            .is_none_or(|at| at.elapsed() >= interval)
        {
            self.request_chrome_hosts(client);
        }
    }

    /// Enter in the dialog: the one obvious next step for its state.
    pub(crate) fn chrome_primary(&mut self, client: &ApiClient) {
        match self.chrome.view() {
            View::Off(Off::Switch) => self.toggle_chrome(client),
            // The switch here cannot help; look again once the person has
            // changed what keeps Chrome off.
            View::Off(Off::BrowserControl | Off::Project) => {
                self.request_chrome_hosts(client);
                self.request_chrome_settings(None, client);
            }
            View::NotSetUp { .. } => self.request_chrome_local(LocalAction::Setup),
            View::LocalFailed(action, _) => self.request_chrome_local(action),
            View::Waiting(_) if self.chrome.nothing_to_add() => {
                // Desktop may have copied the extension here since.
                self.request_chrome_local(LocalAction::Status);
                self.request_chrome_hosts(client);
            }
            View::SettingUp | View::LocalChecking | View::Checking => {}
            View::Unavailable(_) | View::Connected(_) | View::Elsewhere | View::Waiting(_) => {
                self.request_chrome_hosts(client);
            }
        }
    }

    /// `e`: let agents use Chrome, or stop them.
    pub(crate) fn toggle_chrome(&mut self, client: &ApiClient) {
        if self.chrome.saving().is_some() {
            return;
        }
        let Some((enabled, _)) = self.chrome.access() else {
            return;
        };
        if matches!(self.chrome.hosts, Some(Err(_))) {
            return;
        }
        self.request_chrome_settings(Some(Change::Enabled(!enabled)), client);
    }

    /// `d`: the next browser for new agent tabs.
    pub(crate) fn cycle_chrome_browser(&mut self, client: &ApiClient) {
        if self.chrome.saving().is_some() || matches!(self.chrome.hosts, Some(Err(_))) {
            return;
        }
        let Some((_, current)) = self.chrome.access() else {
            return;
        };
        let index = DEFAULT_BROWSERS
            .iter()
            .position(|browser| *browser == current)
            .unwrap_or(0);
        let next = DEFAULT_BROWSERS[(index + 1) % DEFAULT_BROWSERS.len()];
        self.request_chrome_settings(Some(Change::DefaultBrowser(next)), client);
    }

    /// `c`: the folder to paste into Load unpacked once Chrome can start
    /// Haarness, otherwise the command that sets it up.
    pub(crate) fn copy_chrome_detail(&mut self) {
        match self.chrome.view() {
            View::Waiting(_) => {
                if let Some(path) = self.chrome.extension_path().map(str::to_owned) {
                    crate::terminal::copy_to_clipboard(&path);
                    self.notice = "Folder path copied".into();
                }
            }
            View::Elsewhere | View::NotSetUp { .. } | View::LocalFailed(..) => {
                crate::terminal::copy_to_clipboard(SETUP_COMMAND);
                self.notice = "Command copied".into();
            }
            _ => {}
        }
    }

    /// `o`: the extension's Chrome Web Store page, once it has one.
    pub(crate) fn open_chrome_store(&mut self) {
        if !matches!(self.chrome.view(), View::Waiting(_)) {
            return;
        }
        if let Some(url) = self.chrome.web_store_url().map(str::to_owned) {
            self.notice = if crate::terminal::open_browser(&url) {
                "Opened the Chrome Web Store".into()
            } else {
                "Could not open a browser here".into()
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_open_the_dialog_or_start_setup() {
        for value in ["", " ", "status", "STATUS"] {
            assert_eq!(parse(value), Ok(false), "{value}");
        }
        for value in ["setup", "set-up", "install"] {
            assert_eq!(parse(value), Ok(true), "{value}");
        }
        assert!(parse("later").unwrap_err().contains("/chrome setup"));
    }

    #[test]
    fn hosts_and_setup_reports_read_with_defaults_for_what_is_missing() {
        let hosts: Hosts = serde_json::from_value(json!({
            "hosts": [{"kind":"chrome","browserId":"b","name":"Google Chrome","label":"Work",
                "version":"154.0.8037.98","extensionVersion":"0.1.1","paused":false,"stopped":false,
                "connectedAt":"2026-10-12T00:00:00Z","threads":2,"tabs":5,"threadTabs":1}],
            "chrome": {"enabled": true, "defaultBrowser": "chrome", "everConnected": true}
        }))
        .unwrap();
        assert_eq!(hosts.hosts[0].title(), "Google Chrome (Work)");
        assert_eq!(hosts.hosts[0].thread_tabs, 1);
        assert_eq!(hosts.chrome.default_browser, "chrome");
        // An older server's answer without the settings block still reads.
        let bare: Hosts = serde_json::from_value(json!({"hosts": []})).unwrap();
        assert!(bare.chrome.enabled);
        assert_eq!(bare.chrome.default_browser, "auto");

        let setup: Setup = serde_json::from_value(json!({
            "hostName":"ai.haarness.browser","extensionId":"accdjhmcgljbadpeanajllmcembhkomn",
            "webStoreUrl":null,"executable":"/x/haarness","wrapper":"/d/browser/chrome/haarness-browser-host",
            "removed":false,"warnings":[],
            "browsers":[
                {"browser":"chrome","name":"Google Chrome","extensionsPage":"chrome://extensions",
                 "manifest":"/m.json","detected":true,"registered":true,"supported":true,"note":null},
                {"browser":"chromium","name":"Chromium","extensionsPage":"chrome://extensions",
                 "manifest":null,"detected":true,"registered":false,"supported":false,"note":"Snap"}
            ],
            "extension":{"path":"/d/browser/chrome/Haarness for Chrome","version":"0.1.1-preview.1"}
        }))
        .unwrap();
        assert!(setup.registered());
        assert_eq!(setup.ready().count(), 1);
        assert_eq!(setup.unsupported().count(), 1);
        let removed = Setup {
            removed: true,
            ..setup.clone()
        };
        assert!(!removed.registered());
    }

    #[test]
    fn a_settings_change_keeps_every_other_global_value() {
        let mut complete = default_settings();
        overlay(
            &mut complete,
            &json!({"enabled": false, "defaults": {"scripts": "deny"},
                "sites": [{"origin": "https://a.test", "browsing": "allow"}]}),
        );
        assert_eq!(complete["enabled"], false);
        assert_eq!(complete["defaults"]["scripts"], "deny");
        assert_eq!(complete["defaults"]["browsing"], "ask");
        assert_eq!(complete["defaultBrowser"], "auto");
        assert_eq!(complete["sites"][0]["origin"], "https://a.test");
    }

    #[test]
    fn the_view_follows_the_server_then_this_computer() {
        let mut chrome = Chrome::default();
        assert_eq!(chrome.view(), View::Checking);
        chrome.hosts = Some(Err(Unavailable::Remote));
        assert_eq!(chrome.view(), View::Unavailable(&Unavailable::Remote));
        chrome.hosts = Some(Ok(Hosts::default()));
        assert_eq!(chrome.view(), View::Elsewhere);
        chrome.here = true;
        assert_eq!(chrome.view(), View::LocalChecking);
        chrome.local = Some(Setup::default());
        assert_eq!(chrome.view(), View::NotSetUp { removed: false });
        chrome.local_error = Some((LocalAction::Setup, "no".into()));
        assert_eq!(chrome.view(), View::LocalFailed(LocalAction::Setup, "no"));
        chrome.local_error = None;
        chrome.local = Some(Setup {
            browsers: vec![SetupBrowser {
                name: "Google Chrome".into(),
                registered: true,
                ..SetupBrowser::default()
            }],
            ..Setup::default()
        });
        assert!(matches!(chrome.view(), View::Waiting(_)));
        chrome.hosts = Some(Ok(Hosts {
            hosts: vec![Host::default()],
            chrome: Access::default(),
        }));
        assert!(matches!(chrome.view(), View::Connected(_)));
        if let Some(Ok(hosts)) = &mut chrome.hosts {
            hosts.chrome.enabled = false;
        }
        assert_eq!(chrome.view(), View::Off(Off::Switch));
    }

    #[test]
    fn status_reads_as_one_line() {
        let language = crate::locale::Language::English;
        let host = |paused, stopped, tabs| Host {
            name: "Google Chrome".into(),
            paused,
            stopped,
            thread_tabs: tabs,
            ..Host::default()
        };
        let hosts = |hosts| {
            Ok(Hosts {
                hosts,
                chrome: Access::default(),
            })
        };
        assert_eq!(
            status_line(&hosts(vec![host(false, false, 2)]), language),
            "Connected · Google Chrome · 2 tabs in this chat"
        );
        assert_eq!(
            status_line(
                &hosts(vec![host(false, false, 0), host(true, false, 1)]),
                language
            ),
            "2 browsers connected · 1 tab in this chat"
        );
        assert_eq!(
            status_line(&hosts(vec![host(true, false, 0)]), language),
            "Paused in Chrome"
        );
        assert_eq!(
            status_line(&hosts(vec![host(false, true, 0)]), language),
            "Stopped in Chrome"
        );
        assert_eq!(
            status_line(&hosts(Vec::new()), language),
            "Not connected · /chrome"
        );
        assert_eq!(
            status_line(&Err(Unavailable::Unsupported), language),
            "Not supported by this server"
        );
    }
}
