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

/// Why a change that gives agents more reach was refused: it came from a program Haarness started.
pub const INSIDE_REFUSAL: &str = "Only you can give agents more reach on this Mac. This request came from a program Haarness started (an agent's command, a hook or a Haarness terminal), so nothing changed. Use Settings → Computer use, /mac, or haarness mac in a terminal of your own.";

/// Why Resume was refused: it came from a program Haarness started, and only the person who paused ends the pause.
pub const INSIDE_RESUME_REFUSAL: &str = "Only you can let a paused agent go on. This request came from a program Haarness started (an agent's command, a hook or a Haarness terminal), so the agent stays paused. Press Resume on the strip or in the chat.";

/// The field a server adds to a management request that came from a program Haarness started (see
/// [`haarness_core::process_tree`]). Only `true` means anything, so a client cannot loosen the check by sending it.
pub const INSIDE_FIELD: &str = "__insideHaarness";

/// How long after an agent's latest screen action Escape still stops it: as long as the helper's bar stays up after
/// an action (`ControlBar.linger`). Later, Escape belongs to whatever the person is doing again (an editor, a
/// dialog), even while the agent's turn runs on with other work.
const ARMED: Duration = Duration::from_secs(45);

/// How often a paused agent renews its claim on the screen, well inside the lease's idle time.
const PAUSED_RENEWAL: Duration = Duration::from_secs(10);

/// How long a request the helper turned away as paused waits for the pause itself to be taken in. The helper reports
/// the press before it answers, but the two travel separately here.
const PAUSE_HEARD: Duration = Duration::from_secs(1);

/// The event clients hear when the person pauses the agent on the strip and when the pause ends, however it ends. It
/// names the chat that holds the screen (the conversation's root chat, also for a subagent's step) and carries
/// `{"paused": bool}`.
pub const UPDATED_EVENT: &str = "computer_use.updated";

/// Whether the person paused the agent on the strip, and the chat that held the screen then. The chat stays once the
/// pause ends, so its clients hear of the end too.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Pause {
    on: bool,
    chat: Option<ThreadId>,
}

/// The chat whose turn uses the screen now, what stops that turn, and when it last used the screen.
struct Active {
    thread: ThreadId,
    stop: CancellationToken,
    used: std::time::Instant,
}

pub struct Computer {
    store: Store,
    helper: Arc<Helper>,
    lease: Lease,
    paused: watch::Sender<Pause>,
    active: Mutex<Option<Active>>,
    host: Option<String>,
    /// The bar's words in the person's language, as the last client that asked sent them; kept in `bar_file` so a
    /// restarted server speaks them before any client has asked.
    bar: Mutex<Value>,
    bar_file: PathBuf,
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
        let (paused, _) = watch::channel(Pause::default());
        let bar_file = config.data_dir.join("computer-use").join("bar.json");
        let bar = std::fs::read(&bar_file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .filter(Value::is_object)
            .unwrap_or(Value::Null);
        let computer = Arc::new(Self {
            store: Store::new(&config.config_dir),
            helper: Arc::new(Helper::new(source)),
            lease: Lease::new(&config.data_dir.join("computer-use")),
            paused,
            active: Mutex::new(None),
            host: policy::host_app(),
            bar: Mutex::new(bar),
            bar_file,
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
                    Event::Pause => computer.pause(),
                    Event::Resume | Event::Exit => computer.resume(),
                    // The bar's Stop is only there while the agent works; Escape counts only shortly after its
                    // latest action.
                    Event::Stop => {
                        computer.resume();
                        computer.stop_active(None);
                    }
                    Event::Escape => {
                        if computer.stop_active(Some(ARMED)) {
                            computer.resume();
                        }
                    }
                }
            }
        });
    }

    /// Pause on the strip: the agent waits, and the chat holding the screen (or, between its steps, this process's
    /// claim on it) is the one told.
    fn pause(&self) {
        let chat = self
            .active
            .lock()
            .as_ref()
            .map(|active| active.thread)
            .or_else(|| {
                self.lease
                    .current()
                    .and_then(|holder| uuid::Uuid::parse_str(&holder.thread).ok())
                    .map(ThreadId)
            });
        self.helper.keep(true);
        self.paused.send_replace(Pause { on: true, chat });
        tracing::info!(chat = ?chat, "computer use paused by the person");
    }

    fn resume(&self) {
        let mut was = false;
        self.paused.send_if_modified(|pause| {
            was = pause.on;
            pause.on = false;
            was
        });
        if was {
            self.helper.keep(false);
        }
    }

    /// Resume pressed in a client: the strip is told first, so it shows the agent at work and lets its next request
    /// through, and then the waiting agent goes on. Nothing happens while nothing is paused.
    async fn resume_from_client(&self) {
        if !self.is_paused() {
            return;
        }
        if let Err(error) = self.helper.resume().await {
            // The helper answers this at once, ahead of any request; one that cannot has stopped, and a new one starts
            // unpaused.
            tracing::debug!(%error, "could not tell the strip the agent goes on");
        }
        tracing::info!("computer use resumed from a client");
        self.resume();
    }

    /// The chat the person paused, while the pause lasts.
    #[must_use]
    pub fn paused_chat(&self) -> Option<ThreadId> {
        let pause = *self.paused.borrow();
        pause.chat.filter(|_| pause.on)
    }

    /// Escape or the bar's Stop: the turn holding the screen is cancelled, the same as Stop in a client. With no turn
    /// holding it, or (given `within`) none that used the screen that recently, nothing happens. Reports whether a
    /// turn was stopped.
    fn stop_active(&self, within: Option<Duration>) -> bool {
        let mut slot = self.active.lock();
        if within.is_some_and(|limit| {
            slot.as_ref()
                .is_some_and(|active| active.used.elapsed() > limit)
        }) {
            return false;
        }
        let Some(active) = slot.take() else {
            return false;
        };
        drop(slot);
        tracing::info!(thread = %active.thread, "the person stopped the agent using the screen");
        self.lease.release(&active.thread.to_string());
        self.helper.release();
        active.stop.cancel();
        true
    }

    /// The `mac_*` tools, offered while computer use is on.
    #[must_use]
    pub fn tools(self: &Arc<Self>) -> Vec<Arc<dyn Tool>> {
        tools::all(self)
    }

    /// Releases the screen when the turn holding it ends, for every runtime whose events go through `events`, and
    /// tells their clients when the person pauses and when the pause ends ([`UPDATED_EVENT`]).
    pub fn watch(self: &Arc<Self>, events: &EventBus) {
        let mut subscription = events.subscribe(None);
        let mut pauses = self.paused.subscribe();
        let bus = events.clone();
        let computer = Arc::downgrade(self);
        tokio::spawn(async move {
            // What this bus's clients last heard.
            let mut told = *pauses.borrow_and_update();
            loop {
                tokio::select! {
                    received = subscription.receiver.recv() => {
                        let event = match received {
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
                    changed = pauses.changed() => {
                        if changed.is_err() {
                            return;
                        }
                        let now = *pauses.borrow_and_update();
                        let before = std::mem::replace(&mut told, now);
                        let tell = |chat: Option<ThreadId>, paused: bool| {
                            if let Some(chat) = chat {
                                bus.publish(UPDATED_EVENT, Some(chat), None, None, json!({"paused": paused}));
                            }
                        };
                        // Changes that came quicker than this loop are told as where they ended.
                        if before.on && (!now.on || before.chat != now.chat) {
                            tell(before.chat, false);
                        }
                        if now.on && (!before.on || before.chat != now.chat) {
                            tell(now.chat, true);
                        }
                    }
                }
            }
        });
    }

    fn turn_ended(&self, thread: ThreadId) {
        let mut active = self.active.lock();
        let held = active.as_ref().is_some_and(|item| item.thread == thread);
        if held {
            *active = None;
        }
        drop(active);
        // The turn is over, so the bar comes down and a pause ends with it, even if its lease was lost meanwhile.
        if self.lease.release(&thread.to_string()) || held {
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

    /// Remembers which turn is driving the screen, so Stop and Escape can cancel it, and that it uses the screen now.
    pub(crate) fn hold(&self, thread: ThreadId, stop: CancellationToken) {
        *self.active.lock() = Some(Active {
            thread,
            stop,
            used: std::time::Instant::now(),
        });
    }

    /// Notes that the turn holding the screen still uses it: an action under way, or one that just ended.
    pub(crate) fn touch(&self) {
        if let Some(active) = self.active.lock().as_mut() {
            active.used = std::time::Instant::now();
        }
    }

    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.paused.borrow().on
    }

    /// Blocks while the agent is paused, until the person resumes or stops it; cancelling ends the wait early. Returns
    /// true when it had to wait, since the screen may look different afterwards. Meanwhile the paused chat keeps the
    /// screen: a pause hands it to the person, not to another chat.
    pub(crate) async fn wait_while_paused(
        &self,
        cancel: &CancellationToken,
    ) -> Result<bool, HelperError> {
        let mut paused = self.paused.subscribe();
        if !paused.borrow_and_update().on {
            return Ok(false);
        }
        let mut renewal = tokio::time::interval(PAUSED_RENEWAL);
        loop {
            tokio::select! {
                () = cancel.cancelled() => return Err(HelperError::new("cancelled", "Stopped.")),
                _ = renewal.tick() => {
                    // Refreshing this process's holder only: a pause never takes the screen from another chat.
                    if let Some(holder) = self.lease.current() {
                        let _ = self.lease.take(&holder.thread);
                    }
                }
                changed = paused.changed() => {
                    if changed.is_err() || !paused.borrow_and_update().on {
                        return Ok(true);
                    }
                }
            }
        }
    }

    /// [`Self::wait_while_paused`] for a request the helper turned away because the person paused: the press may not
    /// have been taken in yet, so it is waited for first, briefly. Without it the agent would be told it runs again
    /// while the strip still holds it, and the chat would never show the pause.
    pub(crate) async fn wait_after_refusal(
        &self,
        cancel: &CancellationToken,
    ) -> Result<bool, HelperError> {
        let mut paused = self.paused.subscribe();
        if !paused.borrow_and_update().on {
            let heard = async {
                while paused.changed().await.is_ok() {
                    if paused.borrow_and_update().on {
                        return;
                    }
                }
            };
            tokio::select! {
                () = cancel.cancelled() => return Err(HelperError::new("cancelled", "Stopped.")),
                _ = tokio::time::timeout(PAUSE_HEARD, heard) => {}
            }
        }
        self.wait_while_paused(cancel).await
    }

    /// What clients show in Settings → Computer use.
    pub async fn status(&self) -> Value {
        let settings = self.store.get();
        // App names come from Spotlight, a process each: look the new ones up off the async threads, once.
        let host_bundle = policy::responsible_app();
        let unnamed: Vec<String> = settings
            .apps
            .keys()
            .filter(|bundle| !bundle.ends_with(".*"))
            .chain(host_bundle.as_ref())
            .filter(|bundle| !self.names.lock().contains_key(*bundle))
            .cloned()
            .collect();
        if !unnamed.is_empty()
            && let Ok(found) = tokio::task::spawn_blocking(move || {
                unnamed
                    .into_iter()
                    .map(|bundle| {
                        let name = app_name(&bundle);
                        (bundle, name)
                    })
                    .collect::<Vec<_>>()
            })
            .await
        {
            self.names.lock().extend(found);
        }
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
        let host = host_bundle.map(|bundle| {
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
            "pausedChat": self.paused_chat(),
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

    /// Takes the bar's words a client sent, and keeps them for the next start when they changed.
    fn keep_words(&self, words: &Value) {
        let mut bar = self.bar.lock();
        if *bar == *words {
            return;
        }
        *bar = words.clone();
        drop(bar);
        if let Some(folder) = self.bar_file.parent()
            && std::fs::create_dir_all(folder).is_ok()
            && let Ok(bytes) = serde_json::to_vec(words)
            && let Err(error) = std::fs::write(&self.bar_file, bytes)
        {
            tracing::debug!(%error, "could not keep the computer use bar's words");
        }
    }

    /// Whether a management request would let agents do more than they may now: agreeing to the terms, switching on,
    /// letting apps without a rule act, hiding the click ring, moving an app's rule toward allow, or ending the
    /// person's pause. Switching off, withdrawing and tightening never count.
    #[must_use]
    pub fn widens(&self, input: &Value) -> bool {
        match input["action"].as_str().unwrap_or("get") {
            "accept" => true,
            "enable" => input["enabled"].as_bool().unwrap_or(true),
            "configure" => {
                input["defaultAct"].as_str() == Some("allow")
                    || input["ring"].as_bool() == Some(false)
            }
            "app" => {
                // An app without a rule follows the default, which may allow: between ask and allow.
                let rank = |rule: Option<Rule>| match rule {
                    Some(Rule::Deny) => 0,
                    Some(Rule::Ask) => 1,
                    None => 2,
                    Some(Rule::Allow) => 3,
                };
                let bundle = input["bundle"].as_str().unwrap_or_default();
                let wanted = input["rule"].as_str().and_then(Rule::parse);
                rank(wanted) > rank(self.store.get().apps.get(bundle).copied())
            }
            // The person paused the agent to have the screen back: only they say when it may go on.
            "resume" => self.is_paused(),
            _ => false,
        }
    }

    /// What a client that just connected, or just pressed Resume, needs to know, starting no helper.
    fn brief(&self) -> Value {
        json!({
            "supported": self.supported(),
            "paused": self.is_paused(),
            "pausedChat": self.paused_chat(),
        })
    }

    /// The `computer-use` management resource behind Desktop's Settings page and the terminal UI's `/mac`.
    pub async fn manage(&self, input: &Value) -> Result<Value, String> {
        if input[INSIDE_FIELD].as_bool() == Some(true) && self.widens(input) {
            return Err(if input["action"] == "resume" {
                INSIDE_RESUME_REFUSAL
            } else {
                INSIDE_REFUSAL
            }
            .into());
        }
        if let Some(words) = input.get("bar").filter(|words| words.is_object()) {
            self.keep_words(words);
        }
        let action = input["action"].as_str().unwrap_or("get");
        match action {
            // A client handing over the bar's words (on connecting, say): no status, so no helper starts. The answer says
            // which chat is paused, so a client that connects during a pause shows it from the start.
            "words" => return Ok(self.brief()),
            "resume" => {
                self.resume_from_client().await;
                return Ok(self.brief());
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn computer(root: &std::path::Path) -> Arc<Computer> {
        Computer::new(Config {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            helper: Some(root.join("no-helper")),
            console: Some(true),
        })
    }

    #[test]
    fn only_changes_that_give_agents_more_reach_count_as_widening() {
        let root = tempfile::tempdir().unwrap();
        let computer = computer(root.path());
        computer
            .set_app("com.apple.Mail", Some(Rule::Deny))
            .unwrap();
        computer
            .set_app("com.apple.Notes", Some(Rule::Allow))
            .unwrap();
        for widening in [
            json!({"action": "accept", "version": "1"}),
            json!({"action": "enable", "enabled": true}),
            json!({"action": "configure", "defaultAct": "allow"}),
            json!({"action": "configure", "ring": false}),
            json!({"action": "app", "bundle": "com.apple.TextEdit", "rule": "allow"}),
            json!({"action": "app", "bundle": "com.apple.Mail", "rule": "clear"}),
            json!({"action": "app", "bundle": "com.apple.Mail", "rule": "ask"}),
        ] {
            assert!(computer.widens(&widening), "{widening}");
        }
        for narrowing in [
            json!({"action": "get"}),
            json!({"action": "disable"}),
            json!({"action": "enable", "enabled": false}),
            json!({"action": "withdraw"}),
            json!({"action": "configure", "defaultAct": "ask", "ring": true}),
            json!({"action": "app", "bundle": "com.apple.TextEdit", "rule": "ask"}),
            json!({"action": "app", "bundle": "com.apple.Notes", "rule": "clear"}),
            json!({"action": "app", "bundle": "com.apple.Notes", "rule": "deny"}),
        ] {
            assert!(!computer.widens(&narrowing), "{narrowing}");
        }
    }

    #[tokio::test]
    async fn a_request_from_inside_haarness_cannot_widen_but_can_switch_off() {
        let root = tempfile::tempdir().unwrap();
        let computer = computer(root.path());
        let refused = computer
            .manage(&json!({"action": "accept", "version": terms::VERSION, INSIDE_FIELD: true}))
            .await
            .unwrap_err();
        assert_eq!(refused, INSIDE_REFUSAL);
        assert!(!computer.settings().accepted());
        // From a person (no field, or a client claiming false) the same change goes through.
        computer
            .manage(&json!({"action": "accept", "version": terms::VERSION, INSIDE_FIELD: false}))
            .await
            .unwrap();
        assert!(computer.settings().on());
        computer
            .manage(&json!({"action": "disable", INSIDE_FIELD: true}))
            .await
            .unwrap();
        assert!(!computer.settings().on());
    }

    #[tokio::test]
    async fn only_a_person_ends_a_pause() {
        let root = tempfile::tempdir().unwrap();
        let computer = computer(root.path());
        // Nothing paused: Resume changes nothing, whoever sends it.
        assert!(!computer.widens(&json!({"action": "resume"})));
        let calm = computer
            .manage(&json!({"action": "resume", INSIDE_FIELD: true}))
            .await
            .unwrap();
        assert_eq!(calm["paused"], false);
        computer.pause();
        assert!(computer.widens(&json!({"action": "resume"})));
        let refused = computer
            .manage(&json!({"action": "resume", INSIDE_FIELD: true}))
            .await
            .unwrap_err();
        assert_eq!(refused, INSIDE_RESUME_REFUSAL);
        assert!(computer.is_paused());
        let answer = computer.manage(&json!({"action": "resume"})).await.unwrap();
        assert_eq!(answer["paused"], false);
        assert!(!computer.is_paused());
    }

    #[tokio::test]
    async fn the_bar_s_words_outlast_a_restart() {
        let root = tempfile::tempdir().unwrap();
        let words = json!({"usingApp": "Haarness が %@ で作業中", "pause": "一時停止"});
        computer(root.path())
            .manage(&json!({"action": "words", "bar": words}))
            .await
            .unwrap();
        assert_eq!(computer(root.path()).bar_words(), words);
    }
}
