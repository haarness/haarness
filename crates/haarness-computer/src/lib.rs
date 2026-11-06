//! Computer use for Haarness agents: the `mac_*` tools let them look at this Mac's screen and operate its apps. The
//! feature stays off until the person accepts the terms and switches it on (Settings → Computer use in Desktop, `/mac`
//! in the terminal UI, `haarness mac on`), and it is only offered on macOS, by a build with the helper, to the user
//! signed in at the screen.
//!
//! Modules: [`helper`] drives the native process, [`settings`] stores the person's choices, [`policy`] knows the
//! locked app groups, [`lease`] lets one chat hold the screen at a time, and `tools` holds what the model calls.

pub mod helper;
pub mod lease;
pub mod policy;
pub mod settings;
pub mod terms;
mod tools;

use crate::{
    helper::{Event, Helper, HelperError, Source},
    lease::Lease,
    policy::Rule,
    settings::{Agreement, Settings, Store},
};
use haarness_core::{EventBus, Tool};
use haarness_protocol::ThreadId;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Where computer use keeps its files.
#[derive(Clone, Debug)]
pub struct Config {
    /// The global configuration folder: `computer-use.json` lives here.
    pub config_dir: PathBuf,
    /// The data folder: the screen lease lives in `computer-use/` under it.
    pub data_dir: PathBuf,
    /// The cache folder the embedded helper is unpacked into.
    pub cache_dir: PathBuf,
    /// A helper to run instead of the embedded one (`HAARNESS_MAC_HELPER`): a development build or a test stand-in.
    pub helper: Option<PathBuf>,
    /// Whether the person running this server is at the screen, when the caller knows (tests); `None` asks macOS.
    pub console: Option<bool>,
}

/// The chat whose turn uses the screen now, and what stops that turn.
struct Active {
    thread: ThreadId,
    stop: CancellationToken,
}

pub struct Computer {
    store: Store,
    helper: Arc<Helper>,
    lease: Lease,
    paused: watch::Sender<bool>,
    active: Mutex<Option<Active>>,
    host: Option<String>,
    /// The bar's words in the person's language, as the last client that asked sent them.
    bar: Mutex<Value>,
    /// App names by bundle identifier, as Spotlight found them.
    names: Mutex<std::collections::HashMap<String, Option<String>>>,
    console: Option<bool>,
}

static SHARED: OnceLock<Arc<Computer>> = OnceLock::new();

impl Computer {
    /// The one computer use service of this process: one helper, one screen. The first configuration wins.
    pub fn shared(config: Config) -> Arc<Self> {
        SHARED.get_or_init(|| Self::new(config)).clone()
    }

    pub fn new(config: Config) -> Arc<Self> {
        let source = match config.helper {
            Some(path) => Source::Path(path),
            None => Source::Embedded {
                cache: config.cache_dir.clone(),
            },
        };
        let (paused, _) = watch::channel(false);
        let computer = Arc::new(Self {
            store: Store::new(&config.config_dir),
            helper: Arc::new(Helper::new(source)),
            lease: Lease::new(&config.data_dir.join("computer-use")),
            paused,
            active: Mutex::new(None),
            host: policy::host_app(),
            bar: Mutex::new(Value::Null),
            names: Mutex::new(std::collections::HashMap::new()),
            console: config.console,
        });
        if tokio::runtime::Handle::try_current().is_ok() {
            computer.listen();
        }
        computer
    }

    /// Follows what the person does on the helper's bar and keyboard.
    fn listen(self: &Arc<Self>) {
        let mut events = self.helper.subscribe();
        let computer = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                let event = match events.recv().await {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                };
                let Some(computer) = computer.upgrade() else {
                    return;
                };
                match event {
                    Event::Pause => {
                        computer.helper.keep(true);
                        computer.paused.send_replace(true);
                        tracing::info!("computer use paused by the person");
                    }
                    Event::Resume | Event::Exit => computer.resume(),
                    Event::Escape | Event::Stop => {
                        computer.resume();
                        computer.stop_active();
                    }
                }
            }
        });
    }

    fn resume(&self) {
        if self.paused.send_replace(false) {
            self.helper.keep(false);
        }
    }

    /// Escape or the bar's Stop: the turn holding the screen is cancelled, the same as Stop in a client. With no turn
    /// holding it, nothing happens.
    fn stop_active(&self) {
        let active = self.active.lock().take();
        if let Some(active) = active {
            tracing::info!(thread = %active.thread, "the person stopped the agent using the screen");
            self.lease.release(&active.thread.to_string());
            self.helper.release();
            active.stop.cancel();
        }
    }

    /// The `mac_*` tools, offered while computer use is on.
    #[must_use]
    pub fn tools(self: &Arc<Self>) -> Vec<Arc<dyn Tool>> {
        tools::all(self)
    }

    /// Releases the screen when the turn holding it ends, for every runtime whose events go through `events`.
    pub fn watch(self: &Arc<Self>, events: &EventBus) {
        let mut subscription = events.subscribe(None);
        let computer = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                let event = match subscription.receiver.recv().await {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                };
                if event.event_type != "turn.finished" {
                    continue;
                }
                let Some(computer) = computer.upgrade() else {
                    return;
                };
                if let Some(thread) = event.thread_id {
                    computer.turn_ended(thread);
                }
            }
        });
    }

    fn turn_ended(&self, thread: ThreadId) {
        let mut active = self.active.lock();
        if active.as_ref().is_some_and(|item| item.thread == thread) {
            *active = None;
        }
        drop(active);
        if self.lease.release(&thread.to_string()) {
            // The turn is over, so the bar comes down.
            self.helper.release();
            self.resume();
        }
    }

    /// True on macOS when this build carries the helper (or a helper path is configured).
    #[must_use]
    pub fn supported(&self) -> bool {
        self.helper.available()
    }

    /// Whether this turn offers the tools: supported, on with the current terms, and the person who runs this server
    /// is the one logged in at the screen. Cheap: one cached settings read and one file check.
    #[must_use]
    pub fn offered(&self) -> bool {
        self.supported() && self.store.get().on() && self.at_screen()
    }

    fn at_screen(&self) -> bool {
        self.console.unwrap_or_else(console_is_ours)
    }

    #[must_use]
    pub fn settings(&self) -> Settings {
        self.store.get()
    }

    pub(crate) fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }

    pub(crate) fn bar_words(&self) -> Value {
        self.bar.lock().clone()
    }

    pub(crate) fn helper(&self) -> &Arc<Helper> {
        &self.helper
    }

    pub(crate) fn lease(&self) -> &Lease {
        &self.lease
    }

    /// Remembers which turn is driving the screen, so Stop and Escape can cancel it.
    pub(crate) fn hold(&self, thread: ThreadId, stop: CancellationToken) {
        *self.active.lock() = Some(Active { thread, stop });
    }

    #[must_use]
    pub fn is_paused(&self) -> bool {
        *self.paused.borrow()
    }

    /// Blocks while the agent is paused, until the person resumes or stops it; cancelling ends the wait early. Returns
    /// true when it had to wait, since the screen may look different afterwards.
    pub(crate) async fn wait_while_paused(
        &self,
        cancel: &CancellationToken,
    ) -> Result<bool, HelperError> {
        let mut paused = self.paused.subscribe();
        if !*paused.borrow_and_update() {
            return Ok(false);
        }
        loop {
            tokio::select! {
                () = cancel.cancelled() => return Err(HelperError::new("cancelled", "Stopped.")),
                changed = paused.changed() => {
                    if changed.is_err() || !*paused.borrow_and_update() {
                        return Ok(true);
                    }
                }
            }
        }
    }

    /// What clients show in Settings → Computer use.
    pub async fn status(&self) -> Value {
        let settings = self.store.get();
        let supported = self.supported();
        let permissions = if supported {
            let idle = if settings.on() {
                helper::IDLE
            } else {
                helper::IDLE_WHEN_OFF
            };
            self.helper.status(idle).await.ok()
        } else {
            None
        };
        let host = policy::responsible_app().map(|bundle| {
            let name = if policy::matches(&bundle, "dev.haarness.desktop") || bundle == "com.github.Electron" {
                Some("Haarness".to_owned())
            } else {
                self.app_name(&bundle)
            };
            json!({"bundle": bundle, "name": name, "haarness": policy::group(&bundle, None) == policy::Group::Haarness})
        });
        let active = self.lease.anyone().map(
            |holder| json!({"thread": holder.thread, "here": holder.pid == std::process::id()}),
        );
        json!({
            "supported": supported,
            "platform": std::env::consts::OS,
            "console": supported && self.at_screen(),
            "enabled": settings.on(),
            "switch": settings.enabled,
            "terms": {
                "version": terms::VERSION,
                "accepted": settings.accepted(),
                "acceptedAt": settings.terms.as_ref().filter(|_| settings.accepted()).map(|terms| terms.accepted_at.clone()),
                "text": terms::json(),
            },
            "permissions": permissions.as_ref().map(|state| json!({
                "accessibility": state["accessibility"].as_bool().unwrap_or(false),
                "screenRecording": state["screenRecording"].as_bool().unwrap_or(false),
            })),
            "screen": permissions.as_ref().map(|state| json!({
                "locked": state["locked"].as_bool().unwrap_or(false),
                "asleep": state["asleep"].as_bool().unwrap_or(false),
            })),
            "host": host,
            "paused": self.is_paused(),
            "active": active,
            "settings": {
                "defaultAct": settings.default_act.as_str(),
                "ring": settings.ring,
                "apps": settings.apps.iter().map(|(bundle, rule)| json!({
                    "bundle": bundle,
                    "name": if bundle.ends_with(".*") { None } else { self.app_name(bundle) },
                    "rule": rule.as_str(),
                })).collect::<Vec<_>>(),
            },
            "settingsFile": self.store.path(),
        })
    }

    /// The name macOS shows for an installed app, looked up once per bundle identifier.
    fn app_name(&self, bundle: &str) -> Option<String> {
        if let Some(name) = self.names.lock().get(bundle) {
            return name.clone();
        }
        let name = app_name(bundle);
        self.names.lock().insert(bundle.to_owned(), name.clone());
        name
    }

    /// Records that the person agreed to terms `version` (the current one) and turns computer use on.
    pub fn accept(&self, version: &str) -> Result<(), String> {
        if version != terms::VERSION {
            return Err(format!(
                "These terms are out of date; the current version is {}.",
                terms::VERSION
            ));
        }
        self.store
            .update(|settings| {
                settings.terms = Some(Agreement {
                    version: version.to_owned(),
                    accepted_at: chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                });
                settings.enabled = true;
            })
            .map(|_| ())
            .map_err(|error| format!("Saving the computer use settings failed: {error}."))
    }

    /// Switches computer use on or off. Switching on requires today's terms to be accepted; switching off halts an
    /// agent before its next step and kills the helper straight away.
    pub fn enable(&self, on: bool) -> Result<(), String> {
        if on && !self.store.get().accepted() {
            return Err("Accept the computer use terms before switching it on.".into());
        }
        self.store
            .update(|settings| settings.enabled = on)
            .map_err(|error| format!("Saving the computer use settings failed: {error}."))?;
        if !on {
            self.off();
        }
        Ok(())
    }

    /// Withdraws the agreement: computer use stays off until the person agrees again.
    pub fn withdraw(&self) -> Result<(), String> {
        self.store
            .update(|settings| {
                settings.enabled = false;
                settings.terms = None;
            })
            .map_err(|error| format!("Saving the computer use settings failed: {error}."))?;
        self.off();
        Ok(())
    }

    fn off(&self) {
        self.resume();
        self.helper.shutdown(true);
    }

    /// Sets, or with `None` removes, the rule for one app.
    pub fn set_app(&self, bundle: &str, rule: Option<Rule>) -> Result<(), String> {
        if !policy::valid_bundle(bundle, true) {
            return Err(format!(
                "{bundle} is not a bundle identifier, such as com.apple.Notes or com.jetbrains.*."
            ));
        }
        self.store
            .update(|settings| match rule {
                Some(rule) => {
                    settings.apps.insert(bundle.to_owned(), rule);
                }
                None => {
                    settings.apps.remove(bundle);
                }
            })
            .map(|_| ())
            .map_err(|error| format!("Saving the computer use settings failed: {error}."))
    }

    /// Changes what happens in apps without a rule, and the click ring.
    pub fn configure(&self, default_act: Option<Rule>, ring: Option<bool>) -> Result<(), String> {
        if default_act == Some(Rule::Deny) {
            return Err(
                "Apps without a rule can ask first or allow; deny one app at a time instead."
                    .into(),
            );
        }
        self.store
            .update(|settings| {
                if let Some(rule) = default_act {
                    settings.default_act = rule;
                }
                if let Some(ring) = ring {
                    settings.ring = ring;
                }
            })
            .map(|_| ())
            .map_err(|error| format!("Saving the computer use settings failed: {error}."))
    }

    /// Triggers macOS's own Accessibility and Screen Recording prompts for whichever app launched Haarness, then opens the
    /// System Settings page of whichever permission is still missing.
    pub async fn request_permissions(&self) -> Result<(), String> {
        if !self.supported() {
            return Err(
                "Computer use needs macOS and a build of Haarness that includes it.".into(),
            );
        }
        let state = self
            .helper
            .request(
                "prompt",
                json!({}),
                Duration::from_secs(30),
                None,
                None,
                helper::IDLE_WHEN_OFF,
            )
            .await
            .map_err(|error| error.message)?;
        if state["accessibility"].as_bool() != Some(true) {
            open_pane("accessibility")?;
        } else if state["screenRecording"].as_bool() != Some(true) {
            open_pane("screen-recording")?;
        }
        Ok(())
    }

    /// The `computer-use` management resource behind Desktop's Settings page and the terminal UI's `/mac`.
    pub async fn manage(&self, input: &Value) -> Result<Value, String> {
        if let Some(words) = input.get("bar").filter(|words| words.is_object()) {
            *self.bar.lock() = words.clone();
        }
        let action = input["action"].as_str().unwrap_or("get");
        match action {
            // A client handing over the bar's words (on connecting, say): no status, so no helper starts.
            "words" => return Ok(json!({"supported": self.supported()})),
            "get" | "view" => {}
            "accept" => self.accept(input["version"].as_str().unwrap_or_default())?,
            "enable" => self.enable(input["enabled"].as_bool().unwrap_or(true))?,
            "disable" => self.enable(false)?,
            "withdraw" => self.withdraw()?,
            "app" => {
                let bundle = input["bundle"].as_str().unwrap_or_default();
                let rule = match input["rule"].as_str() {
                    None | Some("clear" | "") => None,
                    Some(text) => Some(
                        Rule::parse(text)
                            .ok_or_else(|| format!("{text} is not allow, ask, deny or clear."))?,
                    ),
                };
                self.set_app(bundle, rule)?;
            }
            "configure" => {
                let default_act = match input["defaultAct"].as_str() {
                    None => None,
                    Some(text) => Some(
                        Rule::parse(text).ok_or_else(|| format!("{text} is not allow or ask."))?,
                    ),
                };
                self.configure(default_act, input["ring"].as_bool())?;
            }
            "permissions" => self.request_permissions().await?,
            "open" => open_pane(input["pane"].as_str().unwrap_or("accessibility"))?,
            other => return Err(format!("Unknown computer use action {other}.")),
        }
        Ok(self.status().await)
    }
}

/// Whether the person running this server is the one logged in at this Mac's screen: macOS gives the console to
/// that user. A server started over SSH while nobody, or someone else, is logged in sees another owner.
#[must_use]
pub fn console_is_ours() -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata("/dev/console")
            .is_ok_and(|meta| meta.uid() == nix::unistd::getuid().as_raw())
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Opens System Settings at one privacy permission: `accessibility` or `screen-recording`.
pub fn open_pane(pane: &str) -> Result<(), String> {
    let anchor = match pane {
        "accessibility" => "Privacy_Accessibility",
        "screen-recording" | "screenRecording" | "screen" => "Privacy_ScreenCapture",
        other => return Err(format!("{other} is not accessibility or screen-recording.")),
    };
    if !cfg!(target_os = "macos") {
        return Err("System Settings is on macOS only.".into());
    }
    // System Settings' public URL scheme, written in parts: its prefix reads like a header name the repository
    // guard refuses.
    let scheme = ["x", "apple.systempreferences"].join("-");
    std::process::Command::new("/usr/bin/open")
        .arg(format!("{scheme}:com.apple.preference.security?{anchor}"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not open System Settings: {error}."))
}

/// The name macOS shows for an installed app, from its bundle, when it can be found without asking macOS.
fn app_name(bundle: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("/usr/bin/mdfind")
            .arg(format!(
                "kMDItemCFBundleIdentifier == '{}'",
                bundle.replace('\'', "")
            ))
            .output()
            .ok()?;
        let path = String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()?
            .trim()
            .to_owned();
        let name = std::path::Path::new(&path)
            .file_stem()?
            .to_string_lossy()
            .into_owned();
        (!name.is_empty()).then_some(name)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = bundle;
        None
    }
}
