//! `haarness browser native-host`: the bridge Chrome starts for Haarness for
//! Chrome. It finds the Haarness servers on this computer, opens one extension
//! host connection to each once the extension says hello, and relays between
//! them. stdout carries native-messaging frames and nothing else; the log is
//! `<data>/logs/browser-host.log`, without message contents or tokens.
//!
//! It stops when Chrome closes stdin, and when the executable its wrapper runs
//! changes, so Chrome starts the new one; it never reconnects after either.
use super::{
    PROTOCOL,
    discovery::{self, DataRoot, Discovery, Server},
    files::{Downloads, Guard, Uploads},
    framing,
    manifest::{self, Os},
    relay::{self, Event, Relay, Timing},
    setup::{self, ExtensionCopy},
};
use crate::rendezvous::executable_fingerprint;
use anyhow::Result;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, watch},
    task::JoinHandle,
    time::Instant,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Decides whether the program Chrome would start now is still this one.
pub(crate) struct Watch {
    wrapper: Option<PathBuf>,
    exe: PathBuf,
    initial: Option<String>,
}

impl Watch {
    /// Follows the wrapper when there is one, else this executable.
    pub(crate) fn new(wrapper: Option<PathBuf>, exe: PathBuf) -> Self {
        let wrapper = wrapper.filter(|wrapper| setup::wrapper_exe(wrapper).is_some());
        let mut watch = Self {
            wrapper,
            exe,
            initial: None,
        };
        watch.initial = watch.current();
        watch
    }

    fn current(&self) -> Option<String> {
        let target = match &self.wrapper {
            Some(wrapper) => setup::wrapper_exe(wrapper)?,
            None => self.exe.clone(),
        };
        executable_fingerprint(&target).ok()
    }

    pub(crate) fn changed(&self) -> bool {
        self.current() != self.initial
    }
}

pub(crate) struct Config {
    /// The bridge's own root first (see `discovery::data_roots`).
    pub(crate) roots: Vec<DataRoot>,
    pub(crate) uploads: PathBuf,
    /// `<data>/browser/chrome/Haarness for Chrome`.
    pub(crate) extension: PathBuf,
    pub(crate) watch: Watch,
    pub(crate) home: Option<PathBuf>,
    pub(crate) http: reqwest::Client,
    /// How often to look for servers, check the watch and expire uploads.
    pub(crate) tick: Duration,
    pub(crate) timing: Timing,
    pub(crate) alive: fn(u32) -> bool,
    /// More folders a download may never come from (Desktop's own data).
    pub(crate) private: Vec<PathBuf>,
}

/// Where Haarness Desktop keeps its window state and saved server credentials.
fn desktop_data(
    home: Option<&Path>,
    env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = env("HAARNESS_DESKTOP_DATA")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    for base in [
        home.map(|home| home.join("Library/Application Support")),
        env("APPDATA").map(PathBuf::from),
        home.map(|home| home.join(".config")),
    ]
    .into_iter()
    .flatten()
    {
        folders.extend(["haarness", "Haarness"].map(|name| base.join(name)));
    }
    folders
        .into_iter()
        .filter(|folder| folder.is_absolute())
        .map(|folder| discovery::canonical(&folder))
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Exit {
    /// Chrome closed the port.
    Closed,
    /// The executable Chrome starts changed.
    Replaced,
}

/// The production entry: Chrome's stdin and stdout, the environment's roots.
pub(crate) async fn main() -> Result<()> {
    let home = discovery::home_dir();
    let env = |key: &str| std::env::var_os(key);
    let roots = discovery::data_roots(&env, home.as_deref());
    let Some(own) = roots.first().cloned() else {
        eprintln!("haarness browser native-host: no Haarness data folder");
        std::process::exit(1);
    };
    let _log = init_log(&own.data);
    std::panic::set_hook(Box::new(|info| {
        tracing::error!(%info, "browser bridge panicked");
        eprintln!("haarness browser native-host panicked: {info}");
    }));
    let os = Os::current();
    let chrome = own.data.join("browser").join("chrome");
    let exe = std::env::current_exe().unwrap_or_default();
    let config = Config {
        private: desktop_data(home.as_deref(), &env),
        uploads: own.cache.join("browser").join("uploads"),
        extension: chrome.join(super::EXTENSION_FOLDER),
        watch: Watch::new(Some(chrome.join(manifest::wrapper_file_name(os))), exe),
        home,
        http: discovery::http_client(),
        tick: Duration::from_secs(2),
        timing: Timing::default(),
        alive: discovery::pid_alive,
        roots,
    };
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        "browser bridge starting"
    );
    let exit = run(config, tokio::io::stdin(), tokio::io::stdout()).await;
    tracing::info!(?exit, pid = std::process::id(), "browser bridge stopping");
    // A blocking stdin read would hold up an orderly runtime shutdown.
    std::process::exit(0)
}

/// Appends to `<data>/logs/browser-host.log`, starting over past 5 MiB.
fn init_log(data: &Path) -> Option<()> {
    let folder = data.join("logs");
    fs::create_dir_all(&folder).ok()?;
    let path = folder.join("browser-host.log");
    if fs::metadata(&path).is_ok_and(|metadata| metadata.len() > 5 * 1024 * 1024) {
        let _ = fs::rename(&path, folder.join("browser-host.log.1"));
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    // Other crates stay at warnings whatever HAARNESS_LOG says: their debug
    // output can carry message contents.
    let level = std::env::var("HAARNESS_LOG")
        .ok()
        .filter(|level| {
            matches!(
                level.as_str(),
                "trace" | "debug" | "info" | "warn" | "error"
            )
        })
        .unwrap_or_else(|| "info".into());
    let filter = tracing_subscriber::EnvFilter::new(format!("warn,haarness={level}"));
    tracing_subscriber::fmt()
        .with_writer(std::sync::Mutex::new(file))
        .with_ansi(false)
        .with_env_filter(filter)
        .try_init()
        .ok()
}

enum Inbound {
    Message(Value),
    Closed,
}

enum Discovered {
    Rescan,
    Forget(String),
}

#[derive(Clone, PartialEq, Eq)]
enum Link {
    Connecting,
    Connected,
    Problem(String),
}

struct Slot {
    server: Server,
    link: Link,
}

const NEEDS_RESTART: &str = "restart this project's Haarness server";

struct Bridge {
    out: mpsc::UnboundedSender<Value>,
    http: reqwest::Client,
    timing: Timing,
    cancel: CancellationToken,
    events: mpsc::UnboundedSender<Event>,
    discover: mpsc::UnboundedSender<Discovered>,
    /// The extension's hello as servers get it, once it said one.
    hello: watch::Sender<Value>,
    greeted: bool,
    /// The extension speaks another protocol: nothing is relayed.
    mismatched: bool,
    servers: BTreeMap<String, Slot>,
    relays: HashMap<String, Relay>,
    stopping: Vec<JoinHandle<()>>,
    listed: Option<Value>,
    activity: HashMap<String, Value>,
    /// The approvals last sent per server: the only ones `answer` may answer.
    approvals: HashMap<String, HashSet<String>>,
    uploads: Uploads,
    downloads: Downloads,
    guard: Guard,
    extension_path: PathBuf,
    extension: Option<ExtensionCopy>,
}

/// Runs the bridge on `input`/`output` until Chrome closes the port or the
/// executable changes.
pub(crate) async fn run<R, W>(config: Config, input: R, output: W) -> Exit
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (out, outgoing) = mpsc::unbounded_channel();
    let writer = tokio::spawn(framing::write_messages(output, outgoing));
    let (inbound_sender, mut inbound) = mpsc::unbounded_channel();
    tokio::spawn(read_messages(input, inbound_sender));
    let cancel = CancellationToken::new();

    let uploads = match Uploads::new(config.uploads.clone()) {
        Ok(uploads) => uploads,
        Err(error) => {
            tracing::error!(%error, "cannot prepare the uploads folder");
            let fallback =
                std::env::temp_dir().join(format!("haarness-browser-uploads-{}", Uuid::now_v7()));
            Uploads::new(fallback).expect("a temporary folder is writable")
        }
    };
    let caches: Vec<PathBuf> = config.roots.iter().map(|root| root.cache.clone()).collect();
    let mut forbidden: Vec<PathBuf> = config
        .roots
        .iter()
        .flat_map(|root| root.planes().map(Path::to_path_buf))
        .collect();
    forbidden.push(uploads.root().to_path_buf());
    forbidden.extend(config.private.iter().cloned());
    let (discover, discover_requests) = mpsc::unbounded_channel();
    let (found_sender, mut found) = mpsc::unbounded_channel();
    tokio::spawn(discover_servers(
        Discovery::new(caches, config.http.clone()).with_liveness(config.alive),
        config.tick,
        discover_requests,
        found_sender,
        cancel.clone(),
    ));
    let (events, mut relay_events) = mpsc::unbounded_channel();
    let mut bridge = Bridge {
        out,
        http: config.http.clone(),
        timing: config.timing,
        cancel: cancel.clone(),
        events,
        discover,
        hello: watch::Sender::new(Value::Null),
        greeted: false,
        mismatched: false,
        servers: BTreeMap::new(),
        relays: HashMap::new(),
        stopping: Vec::new(),
        listed: None,
        activity: HashMap::new(),
        approvals: HashMap::new(),
        uploads,
        downloads: Downloads::default(),
        guard: Guard {
            forbidden,
            home: config.home.as_deref().map(discovery::canonical),
        },
        extension: setup::extension_copy(&config.extension),
        extension_path: config.extension.clone(),
    };
    bridge.send_bridge();
    let mut tick = tokio::time::interval_at(Instant::now() + config.tick, config.tick);
    let exit = loop {
        tokio::select! {
            message = inbound.recv() => match message {
                Some(Inbound::Message(message)) => bridge.extension_message(message),
                Some(Inbound::Closed) | None => break Exit::Closed,
            },
            Some(servers) = found.recv() => bridge.discovered(servers),
            Some(event) = relay_events.recv() => bridge.relay_event(event),
            _ = tick.tick() => {
                if config.watch.changed() {
                    tracing::info!("the executable Chrome starts changed; stopping so Chrome starts the new one");
                    break Exit::Replaced;
                }
                bridge.uploads.sweep(Instant::now());
                let extension = setup::extension_copy(&bridge.extension_path);
                if extension != bridge.extension {
                    bridge.extension = extension;
                    bridge.send_bridge();
                }
            }
        }
    };
    cancel.cancel();
    let stopping: Vec<_> = bridge
        .relays
        .drain()
        .map(|(_, relay)| relay.stop())
        .chain(bridge.stopping.drain(..))
        .collect();
    let _ = tokio::time::timeout(
        Duration::from_millis(600),
        futures::future::join_all(stopping),
    )
    .await;
    bridge.uploads.clear();
    drop(bridge);
    let _ = tokio::time::timeout(Duration::from_millis(300), writer).await;
    exit
}

async fn read_messages<R: AsyncRead + Unpin>(
    mut input: R,
    inbound: mpsc::UnboundedSender<Inbound>,
) {
    loop {
        match framing::read_frame(&mut input).await {
            Ok(Some(frame)) => match serde_json::from_slice::<Value>(&frame) {
                Ok(message) if message.is_object() => {
                    if inbound.send(Inbound::Message(message)).is_err() {
                        return;
                    }
                }
                _ => tracing::warn!(
                    bytes = frame.len(),
                    "ignoring a message that is not a JSON object"
                ),
            },
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(%error, "the extension's port broke");
                break;
            }
        }
    }
    let _ = inbound.send(Inbound::Closed);
}

async fn discover_servers(
    mut discovery: Discovery,
    every: Duration,
    mut requests: mpsc::UnboundedReceiver<Discovered>,
    found: mpsc::UnboundedSender<Vec<Server>>,
    cancel: CancellationToken,
) {
    loop {
        let servers = tokio::select! {
            () = cancel.cancelled() => return,
            servers = discovery.scan() => servers,
        };
        if found.send(servers).is_err() {
            return;
        }
        let next = discovery
            .next_retry()
            .map_or(Instant::now() + every, |retry| {
                retry.min(Instant::now() + every)
            });
        let request = tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep_until(next) => None,
            request = requests.recv() => request,
        };
        let mut handle = |request| {
            if let Discovered::Forget(id) = request {
                discovery.forget(&id);
            }
        };
        if let Some(request) = request {
            handle(request);
        }
        while let Ok(request) = requests.try_recv() {
            handle(request);
        }
    }
}

/// A field as the extension sent it, to echo back in the reply.
fn field(message: &Value, key: &str) -> Value {
    message.get(key).cloned().unwrap_or(Value::Null)
}

impl Bridge {
    fn send(&self, message: Value) {
        let _ = self.out.send(message);
    }

    fn send_bridge(&self) {
        self.send(json!({
            "type": "bridge",
            "protocol": PROTOCOL,
            "version": env!("CARGO_PKG_VERSION"),
            "uploadsRoot": self.uploads.root().display().to_string(),
            "extension": self.extension,
        }));
    }

    fn send_servers(&mut self) {
        let servers: Vec<Value> = self
            .servers
            .values()
            .map(|slot| {
                let mut entry = json!({
                    "id": slot.server.id,
                    "workspace": slot.server.workspace,
                    "name": slot.server.name(),
                    "version": slot.server.version,
                });
                let (state, error) = match &slot.link {
                    Link::Connecting => ("connecting", None),
                    Link::Connected => ("connected", None),
                    Link::Problem(problem) => ("error", Some(problem.clone())),
                };
                entry["state"] = json!(state);
                if let Some(error) = error {
                    entry["error"] = json!(error);
                }
                entry
            })
            .collect();
        let message = json!({"type": "servers", "servers": servers});
        if self.listed.as_ref() != Some(&message) {
            self.listed = Some(message.clone());
            self.send(message);
        }
    }

    fn connected(&self, server: &str) -> Option<&Server> {
        self.servers
            .get(server)
            .filter(|slot| slot.link == Link::Connected)
            .map(|slot| &slot.server)
    }

    fn start_relay(&mut self, server: &Server) {
        if !self.greeted
            || self.mismatched
            || !server.capable
            || self.relays.contains_key(&server.id)
        {
            return;
        }
        let relay = relay::spawn(
            server,
            self.hello.subscribe(),
            self.events.clone(),
            self.http.clone(),
            self.timing,
            self.cancel.child_token(),
        );
        self.relays.insert(server.id.clone(), relay);
    }

    fn stop_relay(&mut self, server: &str) {
        if let Some(relay) = self.relays.remove(server) {
            self.stopping.retain(|task| !task.is_finished());
            self.stopping.push(relay.stop());
        }
    }

    fn discovered(&mut self, servers: Vec<Server>) {
        let live: HashSet<String> = servers.iter().map(|server| server.id.clone()).collect();
        let gone: Vec<String> = self
            .servers
            .keys()
            .filter(|id| !live.contains(*id))
            .cloned()
            .collect();
        for id in gone {
            tracing::info!(server = %id, "Haarness server gone");
            self.servers.remove(&id);
            self.stop_relay(&id);
            self.server_quiet(&id);
            self.activity.remove(&id);
        }
        for server in servers {
            let link = if server.capable {
                Link::Connecting
            } else {
                Link::Problem(NEEDS_RESTART.into())
            };
            match self.servers.get_mut(&server.id) {
                None => {
                    tracing::info!(server = %server.id, workspace = %server.workspace, capable = server.capable, "Haarness server found");
                    self.servers.insert(
                        server.id.clone(),
                        Slot {
                            server: server.clone(),
                            link,
                        },
                    );
                }
                // Its record changed: the old address or token is no good, and
                // a server that refused before may accept now.
                Some(slot)
                    if slot.server.base != server.base || slot.server.token != server.token =>
                {
                    tracing::info!(server = %server.id, "Haarness server record changed");
                    *slot = Slot {
                        server: server.clone(),
                        link,
                    };
                    self.stop_relay(&server.id);
                    self.server_quiet(&server.id);
                }
                Some(_) => {}
            }
            self.start_relay(&server);
        }
        self.send_servers();
    }

    /// An explicit rescan also asks the servers that refused Chrome again.
    fn retry_refused(&mut self) {
        let refused: Vec<String> = self
            .servers
            .iter()
            .filter(|(id, slot)| {
                slot.server.capable
                    && matches!(slot.link, Link::Problem(_))
                    && self.relays.contains_key(*id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in refused {
            self.stop_relay(&id);
            if let Some(slot) = self.servers.get_mut(&id) {
                slot.link = Link::Connecting;
                let server = slot.server.clone();
                self.start_relay(&server);
            }
        }
        self.send_servers();
    }

    /// Nothing more will come from this server's last connection.
    fn server_quiet(&mut self, server: &str) {
        self.uploads.server_gone(server, Instant::now());
        self.approvals.remove(server);
        if self.activity.get(server).is_some_and(|activity| {
            activity["active"] != json!([]) || activity["approvals"] != json!([])
        }) {
            let quiet = json!({"type":"activity","server":server,"active":[],"approvals":[]});
            self.activity.insert(server.to_owned(), quiet.clone());
            self.send(quiet);
        }
    }

    fn relay_event(&mut self, event: Event) {
        match event {
            Event::Connected { server } => {
                if let Some(slot) = self.servers.get_mut(&server) {
                    tracing::info!(server = %server, "connected to the extension host endpoint");
                    slot.link = Link::Connected;
                    self.send_servers();
                }
            }
            Event::Message { server, message } => {
                if self.servers.contains_key(&server) {
                    self.server_message(&server, message);
                }
            }
            Event::Activity {
                server,
                active,
                approvals,
            } => {
                if self.connected(&server).is_none() {
                    return;
                }
                // Shown in Chrome, but only Chrome's own may be answered there.
                let ids: HashSet<String> = approvals
                    .iter()
                    .filter(|approval| approval["answerable"] == true)
                    .filter_map(|approval| approval["id"].as_str().map(str::to_owned))
                    .collect();
                let activity = json!({"type":"activity","server":server,"active":active,"approvals":approvals});
                if self.activity.get(&server) != Some(&activity) {
                    self.activity.insert(server.clone(), activity.clone());
                    self.approvals.insert(server, ids);
                    self.send(activity);
                }
            }
            Event::Closed { server, problem } => {
                let Some(slot) = self.servers.get_mut(&server) else {
                    return;
                };
                // A refusal that lasts is answered when the server changes;
                // asking it who it is again would only repeat the answer.
                let lasting = problem.is_some();
                slot.link = problem.map_or(Link::Connecting, Link::Problem);
                self.server_quiet(&server);
                self.send_servers();
                if !lasting {
                    let _ = self.discover.send(Discovered::Forget(server));
                    let _ = self.discover.send(Discovered::Rescan);
                }
            }
        }
    }

    fn server_message(&mut self, server: &str, mut message: Value) {
        if message["type"] == "cancel"
            && let Some(id) = message["id"].as_str()
        {
            self.uploads.finished(server, id, Instant::now());
        }
        if message["type"] == "command" {
            let id = message["id"].as_str().unwrap_or_default().to_owned();
            let thread = message["thread"].as_str().unwrap_or_default().to_owned();
            let method = message
                .pointer("/command/action/type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            self.downloads
                .relayed(server, &id, &thread, &method, SystemTime::now());
            if let Some(command) = message.get_mut("command")
                && let Err(reason) = self.uploads.stage(server, &id, command)
            {
                tracing::warn!(server = %server, %reason, "could not stage an upload");
                self.uploads.finished(server, &id, Instant::now());
                if let Some(relay) = self.relays.get(server) {
                    relay.send(json!({"type":"result","id":id,"outcome":{
                        "type":"failure","code":"upload-failed",
                        "message":format!("Haarness could not pass the file to Chrome: {reason}")}}));
                }
                return;
            }
        }
        self.send(json!({"type":"from-server","server":server,"message":message}));
    }

    fn extension_message(&mut self, message: Value) {
        let kind = message["type"].as_str().unwrap_or_default().to_owned();
        if kind == "hello" {
            self.hello(message);
            return;
        }
        if self.mismatched {
            return;
        }
        match kind.as_str() {
            "to-server" => self.relay_to_server(message),
            "status" => self.status(&message),
            "stop" | "thread-info" | "answer" => self.request(&kind, &message),
            "download" => self.download(&message),
            "rescan" => {
                let _ = self.discover.send(Discovered::Rescan);
                self.retry_refused();
            }
            _ => tracing::debug!(kind = %kind, "ignoring an extension message"),
        }
    }

    fn hello(&mut self, mut message: Value) {
        if message["protocol"].as_u64() != Some(PROTOCOL) {
            tracing::warn!(protocol = %message["protocol"], "the extension speaks another protocol");
            self.mismatched = true;
            for server in self.relays.keys().cloned().collect::<Vec<_>>() {
                self.stop_relay(&server);
            }
            self.send_bridge();
            return;
        }
        self.mismatched = false;
        if !message["browser"].is_object() || !message["extension"].is_object() {
            tracing::warn!("ignoring a hello without browser or extension");
            return;
        }
        if let Some(object) = message.as_object_mut() {
            object.remove("protocol");
        }
        let changed_browser =
            self.greeted && self.hello.borrow()["browser"]["id"] != message["browser"]["id"];
        tracing::info!(
            browser = %message["browser"]["name"],
            extension = %message["extension"]["version"],
            "the extension said hello"
        );
        self.hello.send_replace(message);
        self.greeted = true;
        if changed_browser {
            for server in self.relays.keys().cloned().collect::<Vec<_>>() {
                self.stop_relay(&server);
            }
        }
        for server in self
            .servers
            .values()
            .map(|slot| slot.server.clone())
            .collect::<Vec<_>>()
        {
            self.start_relay(&server);
        }
    }

    fn status(&mut self, message: &Value) {
        if !self.greeted {
            return;
        }
        let mut status = json!({"type": "status"});
        self.hello.send_modify(|hello| {
            for key in ["paused", "stopped", "focusedAt"] {
                if let Some(value) = message.get(key) {
                    hello[key] = value.clone();
                    status[key] = value.clone();
                }
            }
        });
        for (server, relay) in &self.relays {
            if self.connected(server).is_some() {
                relay.send(status.clone());
            }
        }
    }

    fn relay_to_server(&mut self, message: Value) {
        let server = message["server"].as_str().unwrap_or_default().to_owned();
        let Some(mut inner) = message.get("message").cloned().filter(Value::is_object) else {
            return;
        };
        if self.connected(&server).is_none() {
            tracing::debug!(server = %server, "dropping a message for a server that is not connected");
            return;
        }
        // The bridge says hello for the extension.
        if inner["type"] == "hello" {
            return;
        }
        if inner["type"] == "result" {
            let id = inner["id"].as_str().unwrap_or_default().to_owned();
            self.downloads.resolve(&server, &mut inner, &self.guard);
            self.uploads.finished(&server, &id, Instant::now());
        }
        if let Some(relay) = self.relays.get(&server) {
            relay.send(inner);
        }
    }

    fn download(&mut self, message: &Value) {
        let server = message["server"].as_str().unwrap_or_default();
        let thread = message["thread"].as_str().unwrap_or_default();
        let command = message["command"].as_str().unwrap_or_default();
        let Some(path) = message["path"].as_str().map(PathBuf::from) else {
            return;
        };
        if !self.servers.contains_key(server) || Uuid::parse_str(thread).is_err() {
            return;
        }
        if !self.downloads.announce(server, thread, command, &path) {
            tracing::warn!(server = %server, "ignoring a download announced for a command this bridge did not relay");
        }
    }

    /// `stop`, `thread-info` and `answer`: validated here, answered when the
    /// server has.
    fn request(&self, kind: &str, message: &Value) {
        let (reply_type, subject_key) = match kind {
            "stop" => ("stopped", "thread"),
            "thread-info" => ("thread", "thread"),
            _ => ("answered", "permission"),
        };
        let mut reply = json!({"type": reply_type, "server": field(message, "server")});
        reply[subject_key] = field(message, subject_key);
        let failed = move |mut reply: Value, error: &str| {
            reply["ok"] = json!(false);
            reply["error"] = json!(error);
            if reply_type == "thread" {
                reply["title"] = Value::Null;
                reply["archived"] = Value::Null;
                reply["parent"] = Value::Null;
                if let Some(object) = reply.as_object_mut() {
                    object.remove("ok");
                }
            }
            reply
        };
        let server_id = message["server"].as_str().unwrap_or_default();
        let Some(server) = self.connected(server_id) else {
            self.send(failed(reply, "This Haarness server is not connected."));
            return;
        };
        let Some(subject) = message[subject_key]
            .as_str()
            .and_then(|value| Uuid::parse_str(value).ok())
        else {
            self.send(failed(reply, "That is not a valid id."));
            return;
        };
        let decision = message["decision"].as_str().unwrap_or_default().to_owned();
        if kind == "answer" {
            if !matches!(decision.as_str(), "always" | "once" | "reject") {
                self.send(failed(reply, "That is not a valid answer."));
                return;
            }
            let waiting = self
                .approvals
                .get(server_id)
                .is_some_and(|ids| ids.contains(&subject.to_string()));
            if !waiting {
                self.send(failed(
                    reply,
                    "Chrome can only answer a Chrome approval that is waiting.",
                ));
                return;
            }
        }
        let api = relay::Api::new(server, self.http.clone());
        let out = self.out.clone();
        let kind = kind.to_owned();
        tokio::spawn(async move {
            let outcome = match kind.as_str() {
                "stop" => api.stop(subject).await.map(|()| None),
                "thread-info" => api.thread(subject).await.map(Some),
                _ => api.answer(subject, &decision).await.map(|()| None),
            };
            let reply = match outcome {
                Ok(Some(thread)) => {
                    reply["title"] = field(&thread, "title");
                    reply["archived"] = field(&thread, "archived");
                    let subagent = thread["isSubagent"].as_bool().unwrap_or(false);
                    reply["parent"] = if subagent {
                        field(&thread, "parentId")
                    } else {
                        Value::Null
                    };
                    reply
                }
                Ok(None) => {
                    reply["ok"] = json!(true);
                    reply
                }
                Err(error) => failed(reply, &error.to_string()),
            };
            let _ = out.send(reply);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        browser::{
            discovery::tests::{info, temp, write_record},
            framing::tests::read_message,
        },
        reconnect::Pacing,
    };
    use axum::{
        Json, Router,
        extract::{Path as UrlPath, Query, State, ws::WebSocketUpgrade},
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::{get, post},
    };
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use futures::{SinkExt as _, StreamExt as _};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::io::{AsyncWriteExt as _, DuplexStream};

    const TOKEN: &str = "fixture-token";
    const ROOT_THREAD: &str = "019a0000-0000-7000-8000-000000000001";
    const SUBAGENT: &str = "019a0000-0000-7000-8000-000000000002";
    const PERMISSION: &str = "019a0000-0000-7000-8000-00000000000a";
    const COMBINED: &str = "019a0000-0000-7000-8000-00000000000c";

    /// A Haarness server as the bridge sees it: `/v1/info`, the extension
    /// host socket, and the REST routes the extension may ask for.
    #[derive(Clone)]
    struct Fake {
        sockets: Arc<AtomicUsize>,
        /// Upgrade requests, accepted or not.
        attempts: Arc<AtomicUsize>,
        /// `/v1/info` requests.
        infos: Arc<AtomicUsize>,
        /// Refuse the extension host socket with this status.
        refuse: Arc<parking_lot::Mutex<Option<StatusCode>>>,
        received: mpsc::UnboundedSender<Value>,
        to_host: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<Value>>>,
        rest: Arc<parking_lot::Mutex<Vec<String>>>,
        permissions: Arc<parking_lot::Mutex<Value>>,
    }

    struct Server {
        url: String,
        sockets: Arc<AtomicUsize>,
        attempts: Arc<AtomicUsize>,
        infos: Arc<AtomicUsize>,
        refuse: Arc<parking_lot::Mutex<Option<StatusCode>>>,
        received: mpsc::UnboundedReceiver<Value>,
        to_host: mpsc::UnboundedSender<Value>,
        rest: Arc<parking_lot::Mutex<Vec<String>>>,
        permissions: Arc<parking_lot::Mutex<Value>>,
    }

    impl Server {
        async fn next(&mut self, kind: &str) -> Value {
            loop {
                let message = tokio::time::timeout(Duration::from_secs(5), self.received.recv())
                    .await
                    .unwrap_or_else(|_| panic!("the server got no {kind}"))
                    .unwrap();
                if message["type"] == kind {
                    return message;
                }
            }
        }
    }

    fn authorized(headers: &HeaderMap) -> bool {
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            == Some(&format!("Bearer {TOKEN}"))
    }

    async fn host(
        State(fake): State<Fake>,
        headers: HeaderMap,
        Query(query): Query<HashMap<String, String>>,
        upgrade: WebSocketUpgrade,
    ) -> axum::response::Response {
        fake.attempts.fetch_add(1, Ordering::SeqCst);
        if let Some(status) = *fake.refuse.lock() {
            return status.into_response();
        }
        if !authorized(&headers) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        if headers.contains_key("origin") {
            return StatusCode::FORBIDDEN.into_response();
        }
        if query.get("version").map(String::as_str) != Some("1") {
            return StatusCode::BAD_REQUEST.into_response();
        }
        fake.sockets.fetch_add(1, Ordering::SeqCst);
        upgrade
            .max_message_size(64 * 1024 * 1024)
            .on_upgrade(move |socket| async move {
                use axum::extract::ws::Message;
                let (mut sink, mut stream) = socket.split();
                let attached = json!({"type":"attached","version":1,"lease":"lease-1",
                    "server":{"id":"server-a","workspace":"/work/project","pid":std::process::id()}});
                if sink.send(Message::Text(attached.to_string().into())).await.is_err() {
                    return;
                }
                let mut to_host = fake.to_host.lock().await;
                loop {
                    tokio::select! {
                        message = stream.next() => match message {
                            Some(Ok(Message::Text(text))) => {
                                let value: Value = serde_json::from_str(&text).unwrap();
                                if value["type"] == "ping" {
                                    let _ = sink.send(Message::Text(json!({"type":"pong"}).to_string().into())).await;
                                }
                                let _ = fake.received.send(value);
                            }
                            Some(Ok(_)) => {}
                            _ => {
                                let _ = fake.received.send(json!({"type":"socket-closed"}));
                                return;
                            }
                        },
                        message = to_host.recv() => {
                            let Some(message) = message else { return };
                            if message["type"] == "close-me" {
                                let _ = fake.received.send(json!({"type":"socket-closed"}));
                                return;
                            }
                            if sink.send(Message::Text(message.to_string().into())).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            })
    }

    async fn fake_server(capable: bool) -> Server {
        let (received_sender, received) = mpsc::unbounded_channel();
        let (to_host, to_host_receiver) = mpsc::unbounded_channel();
        let fake = Fake {
            sockets: Arc::new(AtomicUsize::new(0)),
            attempts: Arc::default(),
            infos: Arc::default(),
            refuse: Arc::default(),
            received: received_sender,
            to_host: Arc::new(tokio::sync::Mutex::new(to_host_receiver)),
            rest: Arc::default(),
            permissions: Arc::new(parking_lot::Mutex::new(json!([]))),
        };
        let identity = info(
            "server-a",
            "/work/project",
            Some(std::process::id()),
            capable,
        );
        let app = Router::new()
            .route(
                "/v1/info",
                get(move |State(fake): State<Fake>| async move {
                    fake.infos.fetch_add(1, Ordering::SeqCst);
                    Json(identity)
                }),
            )
            .route("/v1/browser/extension/host", get(host))
            .route(
                "/v1/turns/active",
                get(|| async { Json(json!([{"id":"turn","threadId": SUBAGENT}, {"id":"turn2","threadId": SUBAGENT}])) }),
            )
            .route(
                "/v1/permissions",
                get(|State(fake): State<Fake>| async move { Json(fake.permissions.lock().clone()) }),
            )
            .route(
                "/v1/permissions/{id}",
                post(|State(fake): State<Fake>, headers: HeaderMap, UrlPath(id): UrlPath<String>, Json(body): Json<Value>| async move {
                    assert!(authorized(&headers));
                    fake.rest.lock().push(format!("answer {id} {}", body["decision"]));
                    StatusCode::NO_CONTENT
                }),
            )
            .route(
                "/v1/threads/{id}",
                get(|State(fake): State<Fake>, UrlPath(id): UrlPath<String>| async move {
                    fake.rest.lock().push(format!("thread {id}"));
                    match id.as_str() {
                        SUBAGENT => Ok(Json(json!({"id":SUBAGENT,"title":"Research","archived":false,"parentId":ROOT_THREAD,"isSubagent":true}))),
                        ROOT_THREAD => Ok(Json(json!({"id":ROOT_THREAD,"title":"Buy socks","archived":false,"parentId":null,"isSubagent":false}))),
                        _ => Err((StatusCode::NOT_FOUND, Json(json!({"code":"not_found","message":"Thread not found","retryable":false})))),
                    }
                }),
            )
            .route(
                "/v1/threads/{id}/interrupt",
                post(|State(fake): State<Fake>, UrlPath(id): UrlPath<String>, Query(query): Query<HashMap<String, String>>| async move {
                    fake.rest.lock().push(format!("interrupt {id} resume={}", query.get("resume").map_or("", String::as_str)));
                    Json(json!({"threadId": id}))
                }),
            )
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Server {
            url,
            sockets: fake.sockets,
            attempts: fake.attempts,
            infos: fake.infos,
            refuse: fake.refuse,
            received,
            to_host,
            rest: fake.rest,
            permissions: fake.permissions,
        }
    }

    struct Extension {
        to_bridge: DuplexStream,
        from_bridge: DuplexStream,
    }

    impl Extension {
        async fn send(&mut self, message: Value) {
            for frame in framing::encode(&message) {
                self.to_bridge.write_all(&frame).await.unwrap();
            }
        }

        async fn next(&mut self, kind: &str) -> Value {
            self.next_matching(kind, |_| true).await
        }

        async fn next_matching(&mut self, kind: &str, matches: impl Fn(&Value) -> bool) -> Value {
            loop {
                let message = tokio::time::timeout(
                    Duration::from_secs(5),
                    read_message(&mut self.from_bridge),
                )
                .await
                .unwrap_or_else(|_| panic!("the extension got no {kind}"))
                .expect("the bridge is still writing");
                if message["type"] == kind && matches(&message) {
                    return message;
                }
            }
        }
    }

    fn fast_timing() -> Timing {
        Timing {
            ping: Duration::from_millis(200),
            activity: Duration::from_millis(100),
            pacing: Pacing {
                retry: Duration::from_millis(50),
                retry_max: Duration::from_millis(200),
                recover_for: Duration::ZERO,
                recover_max: Duration::from_millis(200),
                stable: Duration::from_secs(30),
                drops: u32::MAX,
                cooldown: Duration::from_millis(200),
                cooldown_max: Duration::from_millis(200),
            },
        }
    }

    fn bridge(root: &Path, watch: Watch) -> (Extension, JoinHandle<Exit>) {
        let (to_bridge, input) = tokio::io::duplex(8 * 1024 * 1024);
        let (output, from_bridge) = tokio::io::duplex(8 * 1024 * 1024);
        let data_root = DataRoot {
            data: root.join("data"),
            config: root.join("config"),
            cache: root.join("cache"),
            state: root.join("state"),
        };
        let config = Config {
            uploads: root.join("cache/browser/uploads"),
            extension: root.join("data/browser/chrome/Haarness for Chrome"),
            roots: vec![data_root],
            watch,
            home: Some(root.join("home")),
            http: discovery::http_client(),
            tick: Duration::from_millis(100),
            timing: fast_timing(),
            alive: |pid| pid == std::process::id(),
            private: vec![root.join("desktop")],
        };
        let task = tokio::spawn(run(config, input, output));
        (
            Extension {
                to_bridge,
                from_bridge,
            },
            task,
        )
    }

    fn hello(protocol: u64) -> Value {
        json!({"type":"hello","protocol":protocol,
            "browser":{"id":"019a0000-0000-4000-8000-0000000000bb","kind":"chrome","name":"Google Chrome","label":null,"version":"154.0.8037.98"},
            "extension":{"version":"0.1.1"},"paused":false,"stopped":false,"focusedAt":1})
    }

    fn unwatched(root: &Path) -> Watch {
        let exe = root.join("haarness-exe");
        fs::write(&exe, b"x").unwrap();
        Watch::new(None, exe)
    }

    #[tokio::test]
    async fn the_bridge_relays_between_chrome_and_every_capable_server() {
        let root = temp("bridge");
        let extension_copy = root.join("data/browser/chrome/Haarness for Chrome");
        fs::create_dir_all(&extension_copy).unwrap();
        fs::write(
            extension_copy.join("manifest.json"),
            br#"{"version":"0.1.1.1","version_name":"0.1.1-preview.1"}"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("home/Downloads")).unwrap();
        let mut server = fake_server(true).await;
        write_record(
            &root.join("cache"),
            "project",
            &server.url,
            TOKEN,
            std::process::id(),
            "/work/project",
        );
        let old_identity = info("server-old", "/work/old", None, false);
        let (old_url, _) = discovery::tests::fake_info(old_identity, "old-token").await;
        write_record(
            &root.join("cache"),
            "old",
            &old_url,
            "old-token",
            std::process::id(),
            "/work/old",
        );
        let (mut chrome, task) = bridge(&root, unwatched(&root));

        // On start: what the bridge is, then the servers it found.
        let hello_bridge = chrome.next("bridge").await;
        let uploads = discovery::canonical(&root.join("cache/browser/uploads"));
        assert_eq!(
            hello_bridge,
            json!({"type":"bridge","protocol":1,"version":env!("CARGO_PKG_VERSION"),
                "uploadsRoot": uploads.display().to_string(),
                "extension":{"path": extension_copy.display().to_string(),"version":"0.1.1-preview.1"}})
        );
        let servers = chrome.next("servers").await;
        assert_eq!(
            servers["servers"],
            json!([
                {"id":"server-a","workspace":"/work/project","name":"project","version":"0.1.1","state":"connecting"},
                {"id":"server-old","workspace":"/work/old","name":"old","version":"0.1.1","state":"error","error":"restart this project's Haarness server"}
            ])
        );
        assert_eq!(
            server.sockets.load(Ordering::SeqCst),
            0,
            "no socket before the extension says hello"
        );
        let text = serde_json::to_string(&servers).unwrap();
        assert!(
            !text.contains(TOKEN) && !text.contains(&server.url),
            "{text}"
        );

        // After hello: one socket, which says hello first without the bridge's protocol field.
        chrome.send(hello(1)).await;
        let first = server.next("hello").await;
        let mut expected = hello(1);
        expected.as_object_mut().unwrap().remove("protocol");
        assert_eq!(first, expected);
        chrome
            .next_matching("servers", |message| {
                message["servers"][0]["state"] == "connected"
            })
            .await;
        let attached = chrome.next("from-server").await;
        assert_eq!(attached["server"], "server-a");
        assert_eq!(attached["message"]["type"], "attached");

        // Messages to the server arrive in order.
        for index in 0..3 {
            chrome
                .send(json!({"type":"to-server","server":"server-a","message":{"type":"state","thread":ROOT_THREAD,"index":index,"state":{"tabs":[],"focusedTabID":null}}}))
                .await;
        }
        for index in 0..3 {
            assert_eq!(server.next("state").await["index"], index);
        }
        // Status reaches every server and the hello used for reconnects.
        chrome
            .send(json!({"type":"status","paused":true,"stopped":false,"focusedAt":5}))
            .await;
        assert_eq!(
            server.next("status").await,
            json!({"type":"status","paused":true,"stopped":false,"focusedAt":5})
        );

        // An upload: Chrome gets a private path under uploadsRoot instead of the bytes.
        server
            .to_host
            .send(json!({"type":"command","id":"cmd-upload","thread":ROOT_THREAD,"title":"Buy socks","parent":null,
                "command":{"action":{"type":"files.upload","tabID":"tab_1","ref":"e1","paths":["a.txt"]},
                "files":[{"id":"file_00000000-0000-0000-0000-000000000001","name":"a.txt","mime":"text/plain","data":STANDARD.encode("hello")}]}}))
            .unwrap();
        let command = chrome.next("from-server").await;
        let reference = &command["message"]["command"]["files"][0];
        let staged = PathBuf::from(reference["path"].as_str().unwrap());
        assert!(staged.starts_with(&uploads));
        assert_eq!(fs::read(&staged).unwrap(), b"hello");
        assert!(reference.get("data").is_none());
        assert_eq!(command["message"]["title"], "Buy socks");
        chrome
            .send(json!({"type":"to-server","server":"server-a","message":{"type":"result","id":"cmd-upload","outcome":{"type":"success","result":{"value":null,"files":[]}}}}))
            .await;
        assert_eq!(server.next("result").await["id"], "cmd-upload");
        assert!(staged.exists(), "kept for ten minutes after the result");

        // A message over 1 MB reaches Chrome in chunks and joins back whole.
        let big = "x".repeat(1_500_000);
        server.to_host.send(json!({"type":"command","id":"cmd-big","thread":ROOT_THREAD,"command":{"action":{"type":"evaluate"},"files":[],"big":big}})).unwrap();
        let joined = chrome.next("from-server").await;
        assert_eq!(
            joined["message"]["command"]["big"].as_str().unwrap().len(),
            1_500_000
        );

        // A download Chrome finished during a relayed click becomes a base64 file for files.get.
        server.to_host.send(json!({"type":"command","id":"cmd-click","thread":ROOT_THREAD,"command":{"action":{"type":"click"},"files":[]}})).unwrap();
        chrome
            .next_matching("from-server", |message| {
                message["message"]["id"] == "cmd-click"
            })
            .await;
        let download = root.join("home/Downloads/report.csv");
        fs::write(&download, b"a,b\n").unwrap();
        chrome
            .send(json!({"type":"download","server":"server-a","thread":ROOT_THREAD,"command":"cmd-click","path":download.display().to_string(),"url":"https://example.com/report.csv"}))
            .await;
        server.to_host.send(json!({"type":"command","id":"cmd-get","thread":ROOT_THREAD,"command":{"action":{"type":"files.get","tabID":"tab_1","fileID":"file_x"},"files":[]}})).unwrap();
        chrome
            .next_matching("from-server", |message| {
                message["message"]["id"] == "cmd-get"
            })
            .await;
        chrome
            .send(json!({"type":"to-server","server":"server-a","message":{"type":"result","id":"cmd-get","outcome":{"type":"success","result":{"value":{},"files":[
                {"id":"file_x","name":"report.csv","mime":"text/csv","bytes":4,"path":download.display().to_string()}]}}}}))
            .await;
        let result = server.next("result").await;
        assert_eq!(
            result["outcome"]["result"]["files"][0]["data"],
            STANDARD.encode("a,b\n")
        );
        assert!(
            result["outcome"]["result"]["files"][0]
                .get("path")
                .is_none()
        );

        // Activity: active chats and the explicit Chrome approvals waiting.
        *server.permissions.lock() = json!([
            {"permissionId":PERMISSION,"threadId":SUBAGENT,"permission":"browser_site","patterns":["chrome-browsing:019a:https://shop.example"],"explicit":true},
            {"permissionId":"019a0000-0000-7000-8000-00000000000b","threadId":SUBAGENT,"permission":"bash","patterns":["rm -rf /"]},
            {"permissionId":COMBINED,"threadId":SUBAGENT,"permission":"multiple","explicit":true,
                "patterns":["browser_site:chrome-uploads:019a:https://shop.example","external_directory:/home/a/Documents/*"]}
        ]);
        let activity = chrome
            .next_matching("activity", |message| message["approvals"] != json!([]))
            .await;
        assert_eq!(
            activity,
            json!({"type":"activity","server":"server-a","active":[SUBAGENT],
            "approvals":[
                {"id":PERMISSION,"thread":SUBAGENT,"patterns":["chrome-browsing:019a:https://shop.example"],"explicit":true,"answerable":true},
                {"id":COMBINED,"thread":SUBAGENT,"patterns":["chrome-uploads:019a:https://shop.example"],"explicit":true,"answerable":false}
            ]})
        );
        // Only a waiting Chrome approval may be answered from Chrome: not
        // another kind, and not one that also waits on something else.
        for other in ["019a0000-0000-7000-8000-00000000000b", COMBINED] {
            chrome.send(json!({"type":"answer","server":"server-a","permission":other,"decision":"always"})).await;
            let refused = chrome.next("answered").await;
            assert_eq!(refused["ok"], false);
            assert_eq!(refused["permission"], other);
        }
        chrome.send(json!({"type":"answer","server":"server-a","permission":PERMISSION,"decision":"always"})).await;
        assert_eq!(
            chrome.next("answered").await,
            json!({"type":"answered","server":"server-a","permission":PERMISSION,"ok":true})
        );
        // Stop interrupts the whole chat a subagent belongs to.
        chrome
            .send(json!({"type":"stop","server":"server-a","thread":SUBAGENT}))
            .await;
        assert_eq!(
            chrome.next("stopped").await,
            json!({"type":"stopped","server":"server-a","thread":SUBAGENT,"ok":true})
        );
        chrome
            .send(json!({"type":"thread-info","server":"server-a","thread":SUBAGENT}))
            .await;
        assert_eq!(
            chrome.next("thread").await,
            json!({"type":"thread","server":"server-a","thread":SUBAGENT,"title":"Research","archived":false,"parent":ROOT_THREAD})
        );
        {
            let rest = server.rest.lock();
            assert!(
                rest.contains(&format!("answer {PERMISSION} \"always\"")),
                "{rest:?}"
            );
            assert!(
                rest.contains(&format!("interrupt {ROOT_THREAD} resume=true")),
                "{rest:?}"
            );
            assert!(
                !rest
                    .iter()
                    .any(|line| line.contains("00000000000b") || line.contains(COMBINED)),
                "{rest:?}"
            );
        }
        // Nothing reaches a REST path unless the server and the id check out.
        for (request, error) in [
            (
                json!({"type":"stop","server":"server-x","thread":SUBAGENT}),
                "not connected",
            ),
            (
                json!({"type":"stop","server":"server-old","thread":SUBAGENT}),
                "not connected",
            ),
            (
                json!({"type":"stop","server":"server-a","thread":"../../permissions/x"}),
                "valid id",
            ),
            (
                json!({"type":"thread-info","server":"server-a","thread":42}),
                "valid id",
            ),
        ] {
            chrome.send(request.clone()).await;
            let reply = chrome
                .next_matching(
                    if request["type"] == "stop" {
                        "stopped"
                    } else {
                        "thread"
                    },
                    |_| true,
                )
                .await;
            assert!(reply["error"].as_str().unwrap().contains(error), "{reply}");
            assert_eq!(reply["server"], request["server"]);
        }
        chrome.send(json!({"type":"thread-info","server":"server-a","thread":"019a0000-0000-7000-8000-0000000000ff"})).await;
        assert_eq!(chrome.next("thread").await["error"], "Thread not found");

        // Chrome closes the port: sockets close, staged uploads go, no reconnect.
        let sockets = server.sockets.load(Ordering::SeqCst);
        assert_eq!(sockets, 1);
        let closed_at = std::time::Instant::now();
        drop(chrome);
        let exit = tokio::time::timeout(Duration::from_millis(1500), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(exit, Exit::Closed);
        assert!(closed_at.elapsed() < Duration::from_millis(1500));
        server.next("socket-closed").await;
        assert!(!staged.exists());
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(server.sockets.load(Ordering::SeqCst), sockets);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_closed_socket_is_reopened_and_says_hello_with_the_latest_status() {
        let root = temp("bridge-reconnect");
        let mut server = fake_server(true).await;
        write_record(
            &root.join("cache"),
            "project",
            &server.url,
            TOKEN,
            std::process::id(),
            "/work/project",
        );
        let (mut chrome, task) = bridge(&root, unwatched(&root));
        chrome.send(hello(1)).await;
        server.next("hello").await;
        chrome
            .send(json!({"type":"status","paused":false,"stopped":true,"focusedAt":9}))
            .await;
        server.next("status").await;
        // The server drops the socket; the bridge comes back with the cached hello.
        server.to_host.send(json!({"type":"close-me"})).unwrap();
        server.next("socket-closed").await;
        chrome
            .next_matching("servers", |message| {
                message["servers"][0]["state"] == "connecting"
            })
            .await;
        let again = server.next("hello").await;
        assert_eq!(again["stopped"], true);
        assert_eq!(again["focusedAt"], 9);
        assert_eq!(server.sockets.load(Ordering::SeqCst), 2);
        drop(chrome);
        assert_eq!(task.await.unwrap(), Exit::Closed);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_lasting_refusal_waits_for_the_server_to_change() {
        let root = temp("bridge-refused");
        let mut server = fake_server(true).await;
        *server.refuse.lock() = Some(StatusCode::FORBIDDEN);
        let url = server.url.clone();
        let record = |token: &str| {
            write_record(
                &root.join("cache"),
                "project",
                &url,
                token,
                std::process::id(),
                "/work/project",
            );
        };
        record(TOKEN);
        let (mut chrome, task) = bridge(&root, unwatched(&root));
        chrome.send(hello(1)).await;
        let refused = chrome
            .next_matching("servers", |message| {
                message["servers"][0]["state"] == "error"
            })
            .await;
        assert!(
            refused["servers"][0]["error"]
                .as_str()
                .unwrap()
                .contains("does not accept Chrome"),
            "{refused}"
        );
        // Retrying every few seconds would only hear the same answer.
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(server.attempts.load(Ordering::SeqCst), 1);
        assert_eq!(server.infos.load(Ordering::SeqCst), 1, "no rediscovery");

        // A changed record (here, a new token) is asked again.
        record("rotated-token");
        let attempts = server.attempts.clone();
        let attempted = |count| {
            let attempts = attempts.clone();
            async move {
                tokio::time::timeout(Duration::from_secs(5), async {
                    while attempts.load(Ordering::SeqCst) < count {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                .expect("the bridge asked again");
            }
        };
        attempted(2).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(server.attempts.load(Ordering::SeqCst), 2);

        // So is a refused server when the extension asks to look again.
        *server.refuse.lock() = None;
        record(TOKEN);
        attempted(3).await;
        chrome
            .next_matching("servers", |message| {
                message["servers"][0]["state"] == "connected"
            })
            .await;
        server.next("hello").await;
        assert_eq!(server.sockets.load(Ordering::SeqCst), 1);
        *server.refuse.lock() = Some(StatusCode::NOT_IMPLEMENTED);
        server.to_host.send(json!({"type":"close-me"})).unwrap();
        chrome
            .next_matching("servers", |message| {
                message["servers"][0]["state"] == "error"
            })
            .await;
        let before = server.attempts.load(Ordering::SeqCst);
        *server.refuse.lock() = None;
        chrome.send(json!({"type":"rescan"})).await;
        attempted(before + 1).await;
        chrome
            .next_matching("servers", |message| {
                message["servers"][0]["state"] == "connected"
            })
            .await;
        drop(chrome);
        assert_eq!(task.await.unwrap(), Exit::Closed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn desktop_data_is_off_limits_for_downloads() {
        let base = std::env::temp_dir().join(format!("haarness-desktop-data-{}", Uuid::now_v7()));
        let desktop = base.join("desktop");
        let home = base.join("home");
        let env =
            |key: &str| (key == "HAARNESS_DESKTOP_DATA").then(|| desktop.clone().into_os_string());
        let folders = desktop_data(Some(&home), &env);
        for folder in [
            desktop.clone(),
            home.join("Library/Application Support").join("haarness"),
            home.join("Library/Application Support").join("Haarness"),
            home.join(".config").join("haarness"),
        ] {
            assert!(
                folders.contains(&folder),
                "{} in {folders:?}",
                folder.display()
            );
        }
    }

    #[tokio::test]
    async fn another_protocol_gets_only_the_bridge_message() {
        let root = temp("bridge-protocol");
        let server = fake_server(true).await;
        write_record(
            &root.join("cache"),
            "project",
            &server.url,
            TOKEN,
            std::process::id(),
            "/work/project",
        );
        let (mut chrome, task) = bridge(&root, unwatched(&root));
        chrome.next("bridge").await;
        chrome.send(hello(2)).await;
        assert_eq!(chrome.next("bridge").await["protocol"], 1);
        chrome
            .send(json!({"type":"to-server","server":"server-a","message":{"type":"state"}}))
            .await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(server.sockets.load(Ordering::SeqCst), 0);
        drop(chrome);
        assert_eq!(task.await.unwrap(), Exit::Closed);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn the_bridge_stops_when_the_executable_its_wrapper_runs_changes() {
        let root = temp("bridge-replaced");
        let exe = root.join("bin/haarness");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, b"build one").unwrap();
        let wrapper = root.join("haarness-browser-host");
        fs::write(&wrapper, manifest::shell_wrapper(&exe, &[])).unwrap();
        let (mut chrome, task) = bridge(
            &root,
            Watch::new(Some(wrapper.clone()), PathBuf::from("/nonexistent")),
        );
        chrome.next("bridge").await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!task.is_finished());
        // A rebuild or upgrade replaces the file Chrome would start.
        let next = root.join("bin/haarness.new");
        fs::write(&next, b"build two, longer").unwrap();
        fs::rename(&next, &exe).unwrap();
        let exit = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(exit, Exit::Replaced);
        drop(chrome);
        fs::remove_dir_all(root).unwrap();
    }
}
