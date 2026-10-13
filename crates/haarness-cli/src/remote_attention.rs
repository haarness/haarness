//! Tells Haarness Cloud what waits on the person on this computer, so a phone
//! can be told without an open stream: the approvals and questions that are
//! pending, and the chats whose last turn just finished. Only ids, categories
//! and chat titles leave this computer. Commands, paths, prompts and question
//! text stay here; the phone reads them through the relay when it opens the
//! chat, which is the only place it can answer anyway.
//!
//! Only servers that already run are watched. They are found through their
//! discovery records, the way Chrome's bridge finds them, never through the
//! host gateway, so watching never starts, replaces or keeps a project's
//! server alive. A stopped server runs no turn, so nothing of it can wait on
//! anyone.
use super::{
    Paths,
    browser::discovery::{self, Discovery, Server},
    reconnect::{Backoff, Link},
    remote_cloud::{AbortTask, Frame, REMOTE_THREAD_HEADER},
    rendezvous::{RuntimeDiscovery, comparable_workspace},
};
use anyhow::Result;
use chrono::{DateTime, TimeDelta, Utc};
use futures::{Stream, StreamExt as _};
use haarness_protocol::{
    EventEnvelope, PendingPermissionInfo, PendingQuestionInfo, ServerInfo, Thread, ThreadId, Turn,
    TurnId, TurnStatus, WorkspaceInfo,
};
use haarness_server::auth::ServerAccess;
use parking_lot::Mutex;
use reqwest::Url;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinSet, time::Instant};
use uuid::Uuid;

/// The relay feature a Cloud that stores attention announces in `ready`.
pub(crate) const FEATURE: &str = "attention.v1";

/// Cloud resolves whatever a snapshot leaves out, so these bound what one
/// snapshot carries, not what the computer may have pending.
const MAX_ITEMS: usize = 200;
const MAX_FINISHED: usize = 20;
/// Characters in a title or name, and bytes in an id, as Cloud stores them.
const MAX_TEXT: usize = 200;
const MAX_CATEGORY: usize = 64;
/// How long a finished turn is reported, in every snapshot, so a reconnect
/// cannot lose it; Cloud keeps the first report of each turn.
const FINISHED_WINDOW: TimeDelta = TimeDelta::minutes(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Servers send a keepalive every 15 s; a stream silent for longer is gone.
const STREAM_IDLE: Duration = Duration::from_secs(45);
/// The largest event read whole. A bigger one (a message carrying large
/// attachments) is skipped, keeping only its id.
const MAX_EVENT: usize = 32 * 1024 * 1024;
/// How often a finished turn is checked before it is given up on, when the
/// server does not answer the check.
const SETTLE_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Permission,
    Question,
}

/// Something waiting on the person: an approval or a question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Item {
    pub(crate) kind: Kind,
    pub(crate) id: String,
    /// The workspace id the host gateway routes by.
    pub(crate) project_id: String,
    pub(crate) project_name: String,
    /// The chat that asked: the root chat, or a helper agent's.
    pub(crate) thread_id: Uuid,
    /// The chat to open: the root of the conversation, or the published chat.
    pub(crate) root_thread_id: Uuid,
    pub(crate) turn_id: Option<Uuid>,
    pub(crate) chat_title: Option<String>,
    /// The permission category, never its command or patterns.
    pub(crate) category: Option<String>,
    pub(crate) explicit: bool,
    /// Whether Cloud can route an answer to the server that asked.
    pub(crate) actionable: bool,
    pub(crate) created_at: DateTime<Utc>,
    /// Questions only: whether a free-text answer is accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) custom: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Outcome {
    Completed,
    Failed,
}

/// A root chat whose turn finished and stayed finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_field_names)] // `finishedAt` is the wire's name
pub(crate) struct Finished {
    pub(crate) turn_id: Uuid,
    pub(crate) project_id: String,
    pub(crate) project_name: String,
    pub(crate) thread_id: Uuid,
    pub(crate) chat_title: Option<String>,
    pub(crate) status: Outcome,
    /// The chat is a scheduled run; Cloud does not announce those.
    pub(crate) scheduled: bool,
    pub(crate) finished_at: DateTime<Utc>,
}

/// Everything one attention frame says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub(crate) items: Vec<Item>,
    pub(crate) finished: Vec<Finished>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    /// How long changes gather before one snapshot goes out.
    pub(crate) debounce: Duration,
    /// How long a finished root turn must stay finished to count: goals and
    /// loops start the next turn at once.
    pub(crate) settle: Duration,
    /// How often the servers on this computer are looked for.
    pub(crate) scan: Duration,
    /// How long the first snapshot of a connection waits for the servers
    /// found at its start, including ones that are starting and have not
    /// answered yet; a partial one would resolve their items in Cloud.
    pub(crate) first: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(300),
            settle: Duration::from_secs(2),
            scan: Duration::from_secs(5),
            first: Duration::from_secs(3),
        }
    }
}

/// What a connector keeps of a server it stopped following (its project was
/// closed for remote access, or a published chat's server was replaced), so
/// following it again misses nothing and reports nothing twice.
#[derive(Clone, Debug, Default)]
struct Remembered {
    cursor: Option<u64>,
    settling: Vec<Turn>,
    finished: Vec<Done>,
    /// Root turns that ran when it was last read.
    running: HashSet<TurnId>,
}

/// By server instance.
type Memory = Arc<Mutex<HashMap<String, Remembered>>>;

/// This build's executable fingerprint as it is now.
pub(crate) type Fingerprint = Arc<dyn Fn() -> Option<String> + Send + Sync>;

pub(crate) struct Context {
    setup: Setup,
    /// Started by the first connection whose Cloud stores attention and kept
    /// for the rest of the run: it keeps reading the servers while Cloud is
    /// unreachable, so a turn that ends meanwhile is reported on reconnect.
    watcher: Mutex<Option<Watcher>>,
}

/// What the watcher works with. It holds no `Context`, so the watcher stops
/// with the connector that owns the context.
#[derive(Clone)]
struct Setup {
    paths: Paths,
    trust: bool,
    /// This build's executable as it is now. An in-place upgrade changes it
    /// while the connector runs, and the host gateway compares each server
    /// with the executable on disk at every request.
    executable: Fingerprint,
    timing: Timing,
    memory: Memory,
}

impl Context {
    /// For the connector of the workspace `paths` is scoped to, started with
    /// this project configuration setting.
    pub(crate) fn new(paths: &Paths, trust: bool, timing: Timing) -> Arc<Self> {
        Self::with_executable(
            paths,
            trust,
            timing,
            Arc::new(|| super::current_executable_fingerprint().ok()),
        )
    }

    /// As `new`, comparing servers with `executable` (tests replace the build).
    pub(crate) fn with_executable(
        paths: &Paths,
        trust: bool,
        timing: Timing,
        executable: Fingerprint,
    ) -> Arc<Self> {
        Arc::new(Self {
            setup: Setup {
                paths: paths.clone(),
                trust,
                executable,
                timing,
                memory: Arc::default(),
            },
            watcher: Mutex::new(None),
        })
    }

    /// The board the watcher for `scope` publishes, starting the watcher
    /// unless it runs. A published chat whose server moved (it was replaced)
    /// is followed afresh.
    fn board(&self, scope: Scope) -> Option<BoardReceiver> {
        let chat = match &scope {
            Scope::Host => None,
            Scope::Chat { url, token, .. } => Some((Url::parse(url).ok()?, token.clone())),
        };
        let mut watcher = self.watcher.lock();
        if let Some(running) = watcher.as_ref()
            && running.chat == chat
            && !running.task.0.is_finished()
        {
            return Some(running.board.clone());
        }
        let (publish, board) = tokio::sync::watch::channel(Board::default());
        let setup = self.setup.clone();
        let task = match (scope, chat.clone()) {
            (
                Scope::Chat {
                    project, thread, ..
                },
                Some((base, token)),
            ) => tokio::spawn(follow_chat(
                setup,
                base,
                token,
                project,
                ThreadId(thread),
                publish,
            )),
            _ => tokio::spawn(follow_host(setup, publish)),
        };
        *watcher = Some(Watcher {
            chat,
            board: board.clone(),
            task: AbortTask(task),
        });
        Some(board)
    }
}

/// What one connection reports.
pub(crate) enum Scope {
    /// Every project on this computer whose server runs.
    Host,
    /// One published chat and its child chats, on the connector's own server.
    Chat {
        url: String,
        token: String,
        project: String,
        thread: Uuid,
    },
}

/// The servers' follower, which outlives connections.
struct Watcher {
    /// The published chat's server (address and token), or None for every
    /// project.
    chat: Option<(Url, String)>,
    board: BoardReceiver,
    task: AbortTask,
}

type BoardReceiver = tokio::sync::watch::Receiver<Board>;

/// Every followed server's part of the next snapshot, as the watcher last
/// saw it.
#[derive(Clone, Default)]
struct Board {
    /// The running servers were matched to their projects at least once.
    /// Before that nothing is known, not even that nothing waits.
    scanned: bool,
    /// Every server found was matched and every one that is starting has
    /// answered, as far as a connection's first snapshot waits for them.
    ready: bool,
    /// None until that server has answered once.
    servers: Vec<(Project, Option<View>)>,
}

impl Board {
    /// Whether a connection's first snapshot can go out: a partial one would
    /// resolve the missing servers' items in Cloud.
    fn loaded(&self) -> bool {
        self.ready && self.servers.iter().all(|(_, view)| view.is_some())
    }

    fn snapshot(&self, now: DateTime<Utc>) -> Snapshot {
        snapshot(
            self.servers
                .iter()
                .filter_map(|(project, view)| Some((project, view.as_ref()?))),
            now,
        )
    }
}

/// Report attention over one relay connection until it ends: the first
/// snapshot as soon as every server found has answered (even when nothing
/// waits, so Cloud resolves what an earlier connection left), then one
/// snapshot per change, at most every `debounce`.
pub(crate) async fn watch(context: Arc<Context>, scope: Scope, send: mpsc::Sender<Frame>) {
    let timing = context.setup.timing;
    let Some(mut board) = context.board(scope) else {
        return;
    };
    let start = Instant::now();
    let mut outbox = Outbox::default();
    let mut flush_at = Some(start);
    loop {
        tokio::select! {
            changed = board.changed() => {
                if changed.is_err() {
                    return;
                }
                if outbox.first() && board.borrow().loaded() {
                    flush_at = Some(Instant::now());
                } else {
                    flush_at.get_or_insert(Instant::now() + timing.debounce);
                }
            }
            () = tokio::time::sleep_until(flush_at.unwrap_or(start)), if flush_at.is_some() => {
                let current = board.borrow_and_update().clone();
                if outbox.first() && !current.loaded() {
                    if !current.scanned {
                        // The board's next change brings the first match.
                        flush_at = None;
                        continue;
                    }
                    if Instant::now() < start + timing.first {
                        flush_at = Some(start + timing.first);
                        continue;
                    }
                }
                flush_at = None;
                if let Some(frame) = outbox.frame(current.snapshot(Utc::now()))
                    && send.send(frame).await.is_err()
                {
                    return;
                }
            }
        }
    }
}

/// Follow every project server on this computer that runs, for as long as
/// the connector runs.
async fn follow_host(setup: Setup, publish: tokio::sync::watch::Sender<Board>) {
    let timing = setup.timing;
    let (updates, mut received) = mpsc::channel(64);
    let mut discovery = Discovery::new(vec![setup.paths.cache.clone()], discovery::http_client());
    let mut projects = Projects::new(setup.paths.clone());
    let mut watching = Watching::new(publish, setup.executable.clone());
    let mut servers = Vec::new();
    let mut next_scan = Instant::now();
    loop {
        // A server that did not answer yet is asked again once its backoff
        // passed, not only at the next scan.
        let wake = discovery
            .next_retry()
            .map_or(next_scan, |retry| retry.min(next_scan));
        tokio::select! {
            () = tokio::time::sleep_until(wake) => {
                servers = discovery.scan().await;
                next_scan = Instant::now() + timing.scan;
                // A server stays found until its record goes or its process
                // ends; only then is what it was read up to forgotten.
                setup
                    .memory
                    .lock()
                    .retain(|instance, _| servers.iter().any(|server| &server.id == instance));
                watching.forget_gone(&servers);
                watching.refresh_executable();
                projects.want(&servers);
            }
            matched = projects.finished(), if projects.busy() => {
                if let Some(live) = matched {
                    watching.follow(live, &servers, &setup, &updates);
                }
            }
            Some((key, view)) = received.recv() => watching.update(&key, view),
        }
        let starting = discovery
            .waiting_since()
            .any(|since| since.elapsed() < timing.first);
        watching.publish(!projects.busy() && !starting);
    }
}

/// Follow one published chat's tree on the connector's own server.
async fn follow_chat(
    setup: Setup,
    base: Url,
    token: String,
    project: String,
    chat: ThreadId,
    publish: tokio::sync::watch::Sender<Board>,
) {
    let paths = setup.paths.clone();
    let id = project.clone();
    let lookup = tokio::task::spawn_blocking(move || {
        Ok::<_, anyhow::Error>(
            paths
                .workspace_manager()?
                .registered()?
                .into_iter()
                .find(|workspace| workspace.id == id)
                .map(|workspace| workspace.name),
        )
    });
    // A registry that cannot be read in time names the project by its id.
    let name = tokio::time::timeout(REGISTRY_TIMEOUT, lookup)
        .await
        .ok()
        .and_then(Result::ok)
        .and_then(Result::ok)
        .flatten()
        .unwrap_or_default();
    let (updates, mut received) = mpsc::channel(64);
    let mut watching = Watching::new(publish, setup.executable.clone());
    let key = String::from("chat");
    let follower = follow(
        Daemon::new(base, token, Some(chat)),
        setup.memory.clone(),
        setup.timing.settle,
        key.clone(),
        updates,
    );
    watching.watched.insert(
        key,
        Watched {
            project: Project::new(project, &name, Some(chat)),
            route: Route::Direct,
            view: None,
            _task: AbortTask(tokio::spawn(follower)),
        },
    );
    watching.scanned = true;
    watching.publish(true);
    while let Some((key, view)) = received.recv().await {
        watching.update(&key, view);
        watching.publish(true);
    }
}

struct Watched {
    project: Project,
    route: Route,
    /// None until the server has answered once.
    view: Option<View>,
    _task: AbortTask,
}

/// The servers followed, published to the connections as a `Board` when
/// something changed.
struct Watching {
    watched: HashMap<String, Watched>,
    publish: tokio::sync::watch::Sender<Board>,
    fingerprint: Fingerprint,
    /// What `fingerprint` said at the last scan.
    executable: Option<String>,
    scanned: bool,
    ready: bool,
    /// Changed since the last publish.
    dirty: bool,
}

impl Watching {
    fn new(publish: tokio::sync::watch::Sender<Board>, fingerprint: Fingerprint) -> Self {
        Self {
            watched: HashMap::new(),
            publish,
            executable: fingerprint(),
            fingerprint,
            scanned: false,
            ready: false,
            dirty: false,
        }
    }

    fn update(&mut self, key: &str, view: View) {
        if let Some(entry) = self.watched.get_mut(key) {
            entry.view = Some(view);
            self.dirty = true;
        }
    }

    /// Look at this build's executable again: after an in-place upgrade,
    /// the servers the old one started are the ones the gateway replaces.
    fn refresh_executable(&mut self) {
        let current = (self.fingerprint)();
        if current != self.executable {
            self.executable = current;
            self.dirty = true;
        }
    }

    /// Stop following the servers that are gone.
    fn forget_gone(&mut self, servers: &[Server]) {
        let before = self.watched.len();
        self.watched
            .retain(|key, _| servers.iter().any(|server| &server.id == key));
        self.dirty |= self.watched.len() != before;
    }

    /// Follow the servers Cloud can reach now, of those still found, and
    /// stop following the others.
    fn follow(
        &mut self,
        live: Live,
        servers: &[Server],
        setup: &Setup,
        updates: &mpsc::Sender<(String, View)>,
    ) {
        let live: Vec<_> = live
            .into_iter()
            .filter(|(_, server)| servers.iter().any(|found| found.id == server.id))
            .collect();
        let before = self.watched.len();
        self.watched
            .retain(|key, _| live.iter().any(|(_, server)| &server.id == key));
        self.dirty |= self.watched.len() != before || !self.scanned;
        self.scanned = true;
        for (project, server) in live {
            if let Some(entry) = self.watched.get_mut(&server.id) {
                // Renamed meanwhile.
                if entry.project != project {
                    entry.project = project;
                    self.dirty = true;
                }
                continue;
            }
            let route = if setup.paths.workspace_id.as_ref() == Some(&project.id) {
                Route::Direct
            } else {
                Route::Gateway {
                    record: server.record.discovery.clone(),
                    trust: setup.trust,
                }
            };
            let follower = follow(
                Daemon::new(server.base.clone(), server.token.clone(), None),
                setup.memory.clone(),
                setup.timing.settle,
                server.id.clone(),
                updates.clone(),
            );
            self.watched.insert(
                server.id,
                Watched {
                    project,
                    route,
                    view: None,
                    _task: AbortTask(tokio::spawn(follower)),
                },
            );
            self.dirty = true;
        }
    }

    /// Publish the board when it changed. `ready` only once matched.
    fn publish(&mut self, ready: bool) {
        let ready = ready && self.scanned;
        if !self.dirty && ready == self.ready {
            return;
        }
        self.dirty = false;
        self.ready = ready;
        let executable = self.executable.as_deref();
        let servers = self
            .watched
            .values()
            .map(|entry| {
                let view = entry.view.clone().map(|mut view| {
                    view.reachable = entry.route.reaches(view.info.as_deref(), executable);
                    view
                });
                (entry.project.clone(), view)
            })
            .collect();
        self.publish.send_replace(Board {
            scanned: self.scanned,
            ready,
            servers,
        });
    }
}

/// How long the project's name may take to read for a published chat.
const REGISTRY_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the projects are matched again when nothing seems to change:
/// a file's modification time can be too coarse to show a quick rewrite.
const REMATCH: Duration = Duration::from_secs(30);

/// A file's modification time and length.
type Stamp = Option<(std::time::SystemTime, u64)>;

fn stamp(path: &Path) -> Stamp {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

/// Works out which project each running server serves, off the watcher's
/// loop and only when the servers or the files it reads changed. It reads
/// the registry as stored and resolves only running servers' folders, so an
/// unrelated folder that is slow or offline costs nothing, and a read that
/// hangs holds up only the next match, never the snapshots or the servers'
/// updates. One read runs at a time.
struct Projects {
    paths: Paths,
    /// The servers and files the last match was made from, and when.
    basis: Option<(Vec<String>, [Stamp; 2], Instant)>,
    job: Option<tokio::task::JoinHandle<Result<Live>>>,
}

/// The running servers Cloud can reach, each with its project.
type Live = Vec<(Project, Server)>;

impl Projects {
    fn new(paths: Paths) -> Self {
        Self {
            paths,
            basis: None,
            job: None,
        }
    }

    fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// Match these servers, unless the last match stands.
    fn want(&mut self, servers: &[Server]) {
        if self.job.is_some() {
            // The next scan compares again once it ended.
            return;
        }
        let mut ids: Vec<String> = servers.iter().map(|server| server.id.clone()).collect();
        ids.sort();
        let files = [
            stamp(&self.paths.workspace_registry()),
            stamp(&super::remote::hidden_projects_file(&self.paths)),
        ];
        if self.basis.as_ref().is_some_and(|(known, stamps, at)| {
            *known == ids && *stamps == files && at.elapsed() < REMATCH
        }) {
            return;
        }
        self.basis = Some((ids, files, Instant::now()));
        let paths = self.paths.clone();
        let servers = servers.to_vec();
        self.job = Some(tokio::task::spawn_blocking(move || {
            let workspaces = paths.workspace_manager()?.registered()?;
            let hidden = super::remote::hidden_projects(&paths)?;
            Ok(reachable(servers, &workspaces, &hidden))
        }));
    }

    /// The running match's result. None when it failed (a file being
    /// rewritten): what is followed stays, and the next scan reads again.
    async fn finished(&mut self) -> Option<Live> {
        let Some(job) = self.job.as_mut() else {
            return std::future::pending().await;
        };
        let result = job.await;
        self.job = None;
        if let Ok(Ok(live)) = result {
            Some(live)
        } else {
            self.basis = None;
            None
        }
    }
}

/// A project as Cloud knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Project {
    id: String,
    name: String,
    /// A published chat: everything belongs to it, whichever child chat asked.
    chat: Option<ThreadId>,
}

impl Project {
    fn new(id: String, name: &str, chat: Option<ThreadId>) -> Self {
        let name = bounded(name, MAX_TEXT).unwrap_or_else(|| id.clone());
        Self { id, name, chat }
    }
}

/// The live servers Cloud can reach, each with the project it routes to: a
/// registered workspace (a record's folder is its workspace id) that is not
/// closed for remote access and that the server really serves.
fn reachable(servers: Vec<Server>, workspaces: &[WorkspaceInfo], hidden: &HashSet<String>) -> Live {
    servers
        .into_iter()
        .filter_map(|server| {
            let folder = server.record.file.parent()?.file_name()?.to_str()?;
            let workspace = workspaces
                .iter()
                .find(|workspace| workspace.id == folder && !hidden.contains(&workspace.id))?;
            let same_folder = comparable_workspace(Path::new(&workspace.path))
                == comparable_workspace(Path::new(&server.workspace));
            same_folder.then(|| {
                (
                    Project::new(workspace.id.clone(), &workspace.name, None),
                    server,
                )
            })
        })
        .collect()
}

/// How Cloud's requests reach a server.
enum Route {
    /// The connector's own server: the gateway hands requests to it directly.
    Direct,
    /// Another project's server, which the gateway finds with
    /// `ensure_workspace_server`.
    Gateway {
        record: RuntimeDiscovery,
        trust: bool,
    },
}

impl Route {
    /// Whether Cloud can answer the server that said `info`, while this
    /// build's executable is `executable`.
    fn reaches(&self, info: Option<&ServerInfo>, executable: Option<&str>) -> bool {
        match self {
            Self::Direct => true,
            Self::Gateway { record, trust } => {
                info.is_some_and(|info| reused_as_is(record, info, *trust, executable))
            }
        }
    }
}

/// Whether `ensure_workspace_server`, as the host gateway calls it, keeps this
/// server as it runs. It replaces any other one, and refuses to while a turn
/// runs, which is always so while something waits on the person: Cloud
/// cannot answer for such a server.
fn reused_as_is(
    record: &RuntimeDiscovery,
    info: &ServerInfo,
    trust: bool,
    executable: Option<&str>,
) -> bool {
    if super::server_compatibility_error(info).is_some() || info.project_trusted != Some(trust) {
        return false;
    }
    // A configured (open or password) server is kept whichever build runs it.
    let private = record
        .access_fingerprint
        .as_deref()
        .is_none_or(|access| access == ServerAccess::Private.fingerprint(&record.token));
    !private
        || executable
            .is_none_or(|current| record.executable_fingerprint.as_deref() == Some(current))
}

/// One server's part of the snapshot, as its follower last saw it.
#[derive(Clone, Debug, Default)]
struct View {
    /// What the server said about itself when it was first asked.
    info: Option<Arc<ServerInfo>>,
    /// Set by the watcher, which knows the build Cloud's requests reach.
    reachable: bool,
    permissions: Vec<PendingPermissionInfo>,
    questions: Vec<PendingQuestionInfo>,
    /// The chats something waits in or finished in.
    chats: HashMap<ThreadId, Chat>,
    finished: Vec<Done>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Chat {
    title: Option<String>,
    scheduled: bool,
}

impl Chat {
    fn of(thread: &Thread) -> Self {
        Self {
            title: bounded(&thread.title, MAX_TEXT)
                .filter(|title| !haarness_protocol::is_untitled_chat_title(title)),
            scheduled: thread.schedule.is_some(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Done {
    turn: TurnId,
    thread: ThreadId,
    status: Outcome,
    at: DateTime<Utc>,
}

/// The chat an item belongs to: the published chat, else its root.
fn root_of(chat: Option<ThreadId>, thread: ThreadId, root: Option<ThreadId>) -> ThreadId {
    chat.or(root).unwrap_or(thread)
}

/// `text` trimmed and cut to `max` characters; None when nothing is left.
fn bounded(text: &str, max: usize) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.chars().take(max).collect())
}

/// The category Cloud phrases an approval by. Every MCP tool is one kind of
/// ask, whatever server and tool it names.
fn category(permission: &str) -> Option<String> {
    if permission.starts_with("mcp__") {
        return Some("mcp".into());
    }
    bounded(permission, MAX_CATEGORY)
}

/// An id Cloud can store and match later; a longer one is left out rather
/// than cut, because a cut id would never resolve.
fn item_id(id: &str) -> Option<String> {
    (!id.is_empty() && id.len() <= MAX_TEXT).then(|| id.to_owned())
}

/// The snapshot of everything the servers report: items oldest first, at
/// most `MAX_ITEMS`, and the newest `MAX_FINISHED` finished turns of the
/// last `FINISHED_WINDOW`. Nothing but ids, categories, flags and titles.
fn snapshot<'a>(
    views: impl IntoIterator<Item = (&'a Project, &'a View)>,
    now: DateTime<Utc>,
) -> Snapshot {
    let mut items = Vec::new();
    let mut finished = Vec::new();
    for (project, view) in views {
        let title = |thread: ThreadId| view.chats.get(&thread).and_then(|chat| chat.title.clone());
        for permission in &view.permissions {
            let Some(id) = item_id(&permission.permission_id) else {
                continue;
            };
            let root = root_of(
                project.chat,
                permission.thread_id,
                permission.root_thread_id,
            );
            items.push(Item {
                kind: Kind::Permission,
                id,
                project_id: project.id.clone(),
                project_name: project.name.clone(),
                thread_id: permission.thread_id.0,
                root_thread_id: root.0,
                turn_id: permission.turn_id.map(|turn| turn.0),
                chat_title: title(root),
                category: category(&permission.permission),
                explicit: permission.explicit,
                actionable: view.reachable,
                created_at: permission.created_at,
                custom: None,
            });
        }
        for question in &view.questions {
            let Some(id) = item_id(&question.question_id) else {
                continue;
            };
            let root = root_of(project.chat, question.thread_id, question.root_thread_id);
            items.push(Item {
                kind: Kind::Question,
                id,
                project_id: project.id.clone(),
                project_name: project.name.clone(),
                thread_id: question.thread_id.0,
                root_thread_id: root.0,
                turn_id: question.turn_id.map(|turn| turn.0),
                chat_title: title(root),
                category: None,
                explicit: false,
                actionable: view.reachable,
                created_at: question.created_at,
                custom: Some(question.custom),
            });
        }
        for done in &view.finished {
            if now - done.at > FINISHED_WINDOW {
                continue;
            }
            finished.push(Finished {
                turn_id: done.turn.0,
                project_id: project.id.clone(),
                project_name: project.name.clone(),
                thread_id: done.thread.0,
                chat_title: title(done.thread),
                status: done.status,
                scheduled: view
                    .chats
                    .get(&done.thread)
                    .is_some_and(|chat| chat.scheduled),
                finished_at: done.at,
            });
        }
    }
    items.sort_by(|left, right| {
        (left.created_at, &left.project_id, &left.id).cmp(&(
            right.created_at,
            &right.project_id,
            &right.id,
        ))
    });
    items.truncate(MAX_ITEMS);
    finished.sort_by(|left, right| {
        right
            .finished_at
            .cmp(&left.finished_at)
            .then(left.turn_id.cmp(&right.turn_id))
    });
    finished.truncate(MAX_FINISHED);
    Snapshot { items, finished }
}

/// Numbers snapshots and holds back one that changes nothing; the first of a
/// connection always goes out.
#[derive(Default)]
struct Outbox {
    revision: u64,
    sent: Option<Snapshot>,
}

impl Outbox {
    fn first(&self) -> bool {
        self.sent.is_none()
    }

    fn frame(&mut self, snapshot: Snapshot) -> Option<Frame> {
        if self.sent.as_ref() == Some(&snapshot) {
            return None;
        }
        self.revision += 1;
        self.sent = Some(snapshot.clone());
        Some(Frame::Attention {
            revision: self.revision,
            items: snapshot.items,
            finished: snapshot.finished,
        })
    }
}

/// One server, asked directly on this computer.
#[derive(Clone)]
struct Daemon {
    http: reqwest::Client,
    base: Url,
    token: String,
    /// A published chat: the server answers for its tree only, as it does
    /// for Cloud's requests to that chat.
    scope: Option<ThreadId>,
}

impl Daemon {
    fn new(base: Url, token: String, scope: Option<ThreadId>) -> Self {
        Self {
            http: discovery::http_client(),
            base,
            token,
            scope,
        }
    }

    fn request(&self, path: &str) -> Result<reqwest::RequestBuilder> {
        let mut request = self
            .http
            .get(self.base.join(&format!("v1/{path}"))?)
            .bearer_auth(&self.token);
        if let Some(chat) = self.scope {
            request = request.header(REMOTE_THREAD_HEADER, chat.to_string());
        }
        Ok(request)
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        Ok(self
            .request(path)?
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// As `get`, with None for something the server does not have (any more).
    async fn find<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>> {
        let response = self.request(path)?.timeout(REQUEST_TIMEOUT).send().await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(response.error_for_status()?.json().await?))
    }

    /// The event stream, resumed after `cursor` when there is one.
    async fn events(&self, cursor: Option<u64>) -> Result<reqwest::Response> {
        let mut request = self.request("events")?;
        if let Some(cursor) = cursor {
            request = request.header("last-event-id", cursor.to_string());
        }
        Ok(tokio::time::timeout(REQUEST_TIMEOUT, request.send())
            .await??
            .error_for_status()?)
    }
}

/// One server-sent event, its data parsed only once its type matters.
#[derive(Debug, PartialEq, Eq)]
struct Event {
    id: Option<u64>,
    kind: String,
    data: String,
}

impl Event {
    /// None for a frame of comments only (`connected`, keepalives).
    fn parse(frame: &str) -> Option<Self> {
        let mut event = Self {
            id: None,
            kind: String::new(),
            data: String::new(),
        };
        let mut fields = false;
        for line in frame.lines() {
            let line = line.trim_end_matches('\r');
            let (name, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match name {
                "id" => event.id = value.parse().ok(),
                "event" => value.clone_into(&mut event.kind),
                "data" => {
                    if !event.data.is_empty() {
                        event.data.push('\n');
                    }
                    event.data.push_str(value);
                }
                _ => continue,
            }
            fields = true;
        }
        fields.then_some(event)
    }

    fn envelope(&self) -> Option<EventEnvelope> {
        serde_json::from_str(&self.data).ok()
    }
}

/// Splits a response body into events.
struct Events<S> {
    body: S,
    /// How long the body may send nothing at all, keepalives included.
    idle: Duration,
    buffer: Vec<u8>,
    /// Bytes already searched for a frame end.
    searched: usize,
    /// The buffer holds the rest of an event too large to read.
    skipping: bool,
}

/// Enough of a frame's start for its `id:` line, which servers write first.
const EVENT_HEAD: usize = 256;

impl<S, B> Events<S>
where
    S: Stream<Item = reqwest::Result<B>> + Unpin,
    B: AsRef<[u8]>,
{
    fn new(body: S, idle: Duration) -> Self {
        Self {
            body,
            idle,
            buffer: Vec::new(),
            searched: 0,
            skipping: false,
        }
    }

    /// The next event; None when the stream ended, broke or fell silent.
    /// An event larger than `MAX_EVENT` comes back with its id only, so the
    /// cursor still moves past it: resuming before it would read it again.
    async fn next(&mut self) -> Option<Event> {
        loop {
            let from = self.searched.saturating_sub(1);
            if let Some(end) = self.buffer[from..]
                .windows(2)
                .position(|pair| pair == b"\n\n")
                .map(|at| from + at + 2)
            {
                let frame: Vec<u8> = self.buffer.drain(..end).collect();
                self.searched = 0;
                if std::mem::take(&mut self.skipping) {
                    continue;
                }
                if let Some(event) = Event::parse(&String::from_utf8_lossy(&frame)) {
                    return Some(event);
                }
                continue;
            }
            if self.buffer.len() > MAX_EVENT {
                let skipped = (!self.skipping)
                    .then(|| {
                        let head = &self.buffer[..EVENT_HEAD];
                        Event::parse(&String::from_utf8_lossy(head))?.id
                    })
                    .flatten();
                self.skipping = true;
                // Keep the last byte: the frame's end may straddle chunks.
                self.buffer.drain(..self.buffer.len() - 1);
                if let Some(id) = skipped {
                    self.searched = self.buffer.len();
                    return Some(Event {
                        id: Some(id),
                        kind: String::new(),
                        data: String::new(),
                    });
                }
            }
            self.searched = self.buffer.len();
            let chunk = tokio::time::timeout(self.idle, self.body.next())
                .await
                .ok()??
                .ok()?;
            self.buffer.extend_from_slice(chunk.as_ref());
        }
    }
}

/// The turn of a chat a person started, not of a helper agent: the root of
/// its conversation, or the published chat itself.
fn root_turn(envelope: &EventEnvelope, chat: Option<ThreadId>) -> Option<Turn> {
    let thread = envelope.thread_id?;
    let root = chat.or(envelope.root_thread_id)?;
    if thread != root {
        return None;
    }
    serde_json::from_value::<Turn>(envelope.payload.clone()).ok()
}

fn finished(turn: &Turn) -> bool {
    matches!(turn.status, TurnStatus::Completed | TurnStatus::Failed)
}

/// A finished root turn, checked once it had time to settle.
struct Settled {
    turn: Turn,
    attempt: u32,
    /// The chat when nothing else runs in it; None when a goal or loop
    /// took over.
    checked: Result<Option<Thread>>,
}

async fn settle(daemon: Daemon, turn: Turn, delay: Duration, attempt: u32) -> Settled {
    tokio::time::sleep(delay).await;
    let checked = async {
        let thread = turn.thread_id;
        if daemon
            .get::<Option<Turn>>(&format!("threads/{thread}/active"))
            .await?
            .is_some()
        {
            return Ok(None);
        }
        Ok(Some(
            daemon.get::<Thread>(&format!("threads/{thread}")).await?,
        ))
    }
    .await;
    Settled {
        turn,
        attempt,
        checked,
    }
}

/// What reading one event asks for.
enum Step {
    Nothing,
    /// The pending approvals and questions may have changed.
    Refresh,
    /// The view changed.
    Changed,
}

struct Follower {
    daemon: Daemon,
    instance: String,
    memory: Memory,
    view: View,
    /// The first refresh succeeded; until then the view is not reported.
    loaded: bool,
    /// The last event read.
    cursor: Option<u64>,
    /// Finished root turns still settling, by chat. A chat's next finished
    /// turn replaces its last: only that one can be where the chat stopped.
    settling: HashMap<ThreadId, Turn>,
    /// Root turns that run, as far as the events read tell. When events were
    /// missed, the ones no longer running are asked about: their end may be
    /// among what was missed.
    running: HashSet<TurnId>,
    /// Events may have been missed (the server's buffer moved past the
    /// cursor, or this server is followed afresh): ask which turns run.
    missed: bool,
}

impl Follower {
    /// Read the pending approvals and questions again, and the titles of the
    /// chats they wait in.
    async fn refresh(&mut self) -> Result<()> {
        let permissions: Vec<PendingPermissionInfo> = self.daemon.get("permissions").await?;
        let questions: Vec<PendingQuestionInfo> = self.daemon.get("questions").await?;
        let chat = self.daemon.scope;
        let needed: HashSet<ThreadId> = permissions
            .iter()
            .map(|permission| root_of(chat, permission.thread_id, permission.root_thread_id))
            .chain(
                questions
                    .iter()
                    .map(|question| root_of(chat, question.thread_id, question.root_thread_id)),
            )
            .chain(self.view.finished.iter().map(|done| done.thread))
            .collect();
        self.view.chats.retain(|id, _| needed.contains(id));
        for id in needed {
            if !self.view.chats.contains_key(&id)
                && let Ok(thread) = self.daemon.get::<Thread>(&format!("threads/{id}")).await
            {
                self.view.chats.insert(id, Chat::of(&thread));
            }
        }
        self.view.permissions = permissions;
        self.view.questions = questions;
        self.loaded = true;
        Ok(())
    }

    /// Check the root turns that ended while their events were missed, and
    /// note the ones that run now, whose end a later gap could hide.
    async fn reconcile(
        &mut self,
        settle_for: Duration,
        settling: &mut JoinSet<Settled>,
    ) -> Result<()> {
        let active: Vec<Turn> = self.daemon.get("turns/active").await?;
        let ids: Vec<TurnId> = self.running.iter().copied().collect();
        for id in ids {
            if active.iter().any(|turn| turn.id == id) {
                continue;
            }
            let turn = self.daemon.find::<Turn>(&format!("turns/{id}")).await?;
            self.running.remove(&id);
            if let Some(turn) = turn.filter(finished) {
                self.check(turn, settle_for, settling);
            }
        }
        for turn in active {
            if self.running.contains(&turn.id) {
                continue;
            }
            let root = match self.daemon.scope {
                Some(chat) => turn.thread_id == chat,
                None => self
                    .daemon
                    .find::<Thread>(&format!("threads/{}", turn.thread_id))
                    .await?
                    .is_some_and(|thread| thread.parent_id.is_none()),
            };
            if root {
                self.running.insert(turn.id);
            }
        }
        self.missed = false;
        self.remember();
        Ok(())
    }

    fn read(
        &mut self,
        event: &Event,
        settle_for: Duration,
        settling: &mut JoinSet<Settled>,
    ) -> Step {
        let step = match event.kind.as_str() {
            "resync_required" => {
                // What was missed may hold a root turn's end.
                self.missed = true;
                Step::Refresh
            }
            "permission.requested"
            | "permission.resolved"
            | "question.requested"
            | "question.resolved" => Step::Refresh,
            "turn.started" => {
                if let Some(turn) = event
                    .envelope()
                    .and_then(|envelope| root_turn(&envelope, self.daemon.scope))
                {
                    self.running.insert(turn.id);
                }
                Step::Nothing
            }
            "thread.updated" => {
                let renamed = event
                    .envelope()
                    .and_then(|envelope| serde_json::from_value::<Thread>(envelope.payload).ok())
                    .is_some_and(|thread| {
                        let chat = Chat::of(&thread);
                        match self.view.chats.get_mut(&thread.id) {
                            Some(known) if *known != chat => {
                                *known = chat;
                                true
                            }
                            _ => false,
                        }
                    });
                if renamed {
                    Step::Changed
                } else {
                    Step::Nothing
                }
            }
            "turn.finished" => {
                if let Some(turn) = event
                    .envelope()
                    .and_then(|envelope| root_turn(&envelope, self.daemon.scope))
                {
                    self.running.remove(&turn.id);
                    if finished(&turn) {
                        self.check(turn, settle_for, settling);
                    }
                }
                // Its prompts were answered or withdrawn.
                Step::Refresh
            }
            _ => Step::Nothing,
        };
        if let Some(id) = event.id {
            self.cursor = Some(id);
            if let Some(remembered) = self.memory.lock().get_mut(&self.instance) {
                remembered.cursor = self.cursor;
            }
        }
        step
    }

    /// Check a finished root turn once it had time to settle, unless it is
    /// already reported or being checked.
    fn check(&mut self, turn: Turn, settle_for: Duration, settling: &mut JoinSet<Settled>) {
        if self.view.finished.iter().any(|done| done.turn == turn.id)
            || self
                .settling
                .get(&turn.thread_id)
                .is_some_and(|known| known.id == turn.id)
        {
            return;
        }
        settling.spawn(settle(self.daemon.clone(), turn.clone(), settle_for, 1));
        self.settling.insert(turn.thread_id, turn);
        self.remember();
    }

    /// Count a settled turn as finished when nothing took over, checking it
    /// again when the server did not answer. True when the view changed.
    fn settled(
        &mut self,
        settled: Settled,
        settle_for: Duration,
        settling: &mut JoinSet<Settled>,
    ) -> bool {
        let Settled {
            turn,
            attempt,
            checked,
        } = settled;
        if self
            .settling
            .get(&turn.thread_id)
            .is_none_or(|current| current.id != turn.id)
        {
            // A later turn of the chat replaced it.
            return false;
        }
        let thread = match checked {
            Err(_) if attempt < SETTLE_ATTEMPTS => {
                settling.spawn(settle(self.daemon.clone(), turn, settle_for, attempt + 1));
                return false;
            }
            Ok(thread) => thread,
            Err(_) => None,
        };
        self.settling.remove(&turn.thread_id);
        let Some(thread) = thread else {
            // Given up on, or a goal or loop took over.
            self.remember();
            return false;
        };
        self.view.chats.insert(thread.id, Chat::of(&thread));
        self.view.finished.push(Done {
            turn: turn.id,
            thread: turn.thread_id,
            status: if turn.status == TurnStatus::Failed {
                Outcome::Failed
            } else {
                Outcome::Completed
            },
            at: turn.updated_at,
        });
        let now = Utc::now();
        self.view
            .finished
            .retain(|done| now - done.at <= FINISHED_WINDOW);
        self.view
            .finished
            .sort_by(|left, right| right.at.cmp(&left.at));
        self.view.finished.truncate(MAX_FINISHED);
        self.remember();
        true
    }

    /// Keep what a later connection to this server needs.
    fn remember(&self) {
        if let Some(remembered) = self.memory.lock().get_mut(&self.instance) {
            remembered.cursor = self.cursor;
            remembered.settling = self.settling.values().cloned().collect();
            remembered.finished.clone_from(&self.view.finished);
            remembered.running.clone_from(&self.running);
        }
    }
}

/// Follow one server until the watcher drops it: its pending approvals and
/// questions, the chats they wait in, and the root turns that finished. Each
/// change goes to the watcher as the whole view.
async fn follow(
    daemon: Daemon,
    memory: Memory,
    settle_for: Duration,
    key: String,
    updates: mpsc::Sender<(String, View)>,
) {
    let mut backoff = Backoff::new(discovery::retry_pacing());
    let info = loop {
        match daemon.get::<ServerInfo>("info").await {
            Ok(info) => break info,
            Err(_) => tokio::time::sleep(backoff.wait(Link::Failed)).await,
        }
    };
    let instance = info.server_instance_id.to_string();
    let remembered = {
        let mut memory = memory.lock();
        if daemon.scope.is_some() {
            // A chat connector serves one server; earlier ones are gone.
            memory.retain(|known, _| known == &instance);
        }
        memory.entry(instance.clone()).or_default().clone()
    };
    let mut follower = Follower {
        daemon,
        instance,
        memory,
        view: View {
            finished: remembered.finished,
            ..View::default()
        },
        loaded: false,
        // A server first seen now is read from now on: turns that finished
        // before the connector watched it are not news.
        cursor: remembered
            .cursor
            .or((info.event_cursor > 0).then_some(info.event_cursor)),
        settling: HashMap::new(),
        running: remembered.running,
        missed: true,
    };
    follower.view.info = Some(Arc::new(info));
    let mut settling = JoinSet::new();
    for turn in remembered.settling {
        follower.check(turn, settle_for, &mut settling);
    }
    let report = |follower: &Follower| {
        let update = follower
            .loaded
            .then(|| (key.clone(), follower.view.clone()));
        let updates = updates.clone();
        async move {
            match update {
                Some(update) => updates.send(update).await.is_ok(),
                None => true,
            }
        }
    };
    // A refresh the server keeps failing is asked less and less often.
    let mut refreshes = Backoff::new(discovery::retry_pacing());
    loop {
        let stream = match follower.daemon.events(follower.cursor).await {
            Ok(response) => response,
            Err(_) => {
                tokio::time::sleep(backoff.wait(Link::Failed)).await;
                continue;
            }
        };
        let since = Instant::now();
        let mut events = Events::new(stream.bytes_stream(), STREAM_IDLE);
        // Read after subscribing, so nothing raised in between is missed.
        let mut refresh_at = Some(Instant::now());
        loop {
            tokio::select! {
                event = events.next() => {
                    let Some(event) = event else { break };
                    match follower.read(&event, settle_for, &mut settling) {
                        Step::Nothing => {}
                        Step::Refresh => {
                            refresh_at.get_or_insert_with(Instant::now);
                        }
                        Step::Changed => {
                            if !report(&follower).await {
                                return;
                            }
                        }
                    }
                }
                Some(settled) = settling.join_next(), if !settling.is_empty() => {
                    if let Ok(settled) = settled
                        && follower.settled(settled, settle_for, &mut settling)
                        && !report(&follower).await
                    {
                        return;
                    }
                }
                () = tokio::time::sleep_until(refresh_at.unwrap_or_else(Instant::now)), if refresh_at.is_some() => {
                    refresh_at = None;
                    if follower.refresh().await.is_err() {
                        refresh_at = Some(Instant::now() + refreshes.wait(Link::Failed));
                        continue;
                    }
                    if follower.missed
                        && follower.reconcile(settle_for, &mut settling).await.is_err()
                    {
                        // What waits is reported now; the turns are asked again.
                        refresh_at = Some(Instant::now() + refreshes.wait(Link::Failed));
                    } else {
                        refreshes = Backoff::new(discovery::retry_pacing());
                    }
                    if !report(&follower).await {
                        return;
                    }
                }
            }
        }
        // Only a stream that stayed up resets the waits: one the server
        // keeps closing at once is not reopened four times a second.
        let proven = since.elapsed() >= backoff.pacing.stable;
        tokio::time::sleep(backoff.wait(Link::Ended { proven })).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn thread_id(n: u128) -> ThreadId {
        ThreadId(Uuid::from_u128(n))
    }

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_791_800_000 + seconds, 0).unwrap()
    }

    fn permission(
        id: &str,
        category: &str,
        thread: ThreadId,
        root: Option<ThreadId>,
        created: i64,
    ) -> PendingPermissionInfo {
        serde_json::from_value(json!({
            "permissionId": id, "operation": "bash:git push origin main", "permission": category,
            "detail": {"command": "git push origin main"}, "patterns": ["git push origin main"],
            "alwaysPatterns": ["bash:git push *"], "threadId": thread, "rootThreadId": root,
            "turnId": Uuid::from_u128(7), "createdAt": at(created),
        }))
        .unwrap()
    }

    fn question(
        id: &str,
        thread: ThreadId,
        root: Option<ThreadId>,
        custom: bool,
        created: i64,
    ) -> PendingQuestionInfo {
        serde_json::from_value(json!({
            "questionId": id, "prompt": "Which database password should I use?", "choices": ["a"],
            "custom": custom, "threadId": thread, "rootThreadId": root, "createdAt": at(created),
        }))
        .unwrap()
    }

    /// A chat as its server describes it.
    fn chat(title: &str) -> Chat {
        let thread: Thread = serde_json::from_value(json!({
            "id": thread_id(1), "title": title, "parentId": null, "archived": false,
            "agent": "build", "provider": "echo", "model": "echo-1",
            "createdAt": at(0), "updatedAt": at(0),
        }))
        .unwrap();
        Chat::of(&thread)
    }

    #[test]
    fn a_snapshot_carries_categories_and_titles_but_never_commands_or_prompts() {
        let root = thread_id(1);
        let child = thread_id(2);
        let project = Project::new("ws-1".into(), "api", None);
        let view = View {
            reachable: true,
            permissions: vec![
                permission("p-mcp", "mcp__github__create_issue", child, Some(root), 2),
                permission("p-bash", "bash", root, Some(root), 1),
            ],
            questions: vec![question("q-1", child, Some(root), true, 3)],
            chats: HashMap::from([(root, chat("Fix login"))]),
            ..View::default()
        };
        let snapshot = snapshot([(&project, &view)], at(10));
        let wire = serde_json::to_value(Frame::Attention {
            revision: 1,
            items: snapshot.items.clone(),
            finished: snapshot.finished.clone(),
        })
        .unwrap();
        assert_eq!(
            wire,
            json!({"type":"attention","revision":1,"finished":[],"items":[
                {"kind":"permission","id":"p-bash","projectId":"ws-1","projectName":"api",
                 "threadId":root,"rootThreadId":root,"turnId":Uuid::from_u128(7),"chatTitle":"Fix login",
                 "category":"bash","explicit":false,"actionable":true,"createdAt":at(1)},
                {"kind":"permission","id":"p-mcp","projectId":"ws-1","projectName":"api",
                 "threadId":child,"rootThreadId":root,"turnId":Uuid::from_u128(7),"chatTitle":"Fix login",
                 "category":"mcp","explicit":false,"actionable":true,"createdAt":at(2)},
                {"kind":"question","id":"q-1","projectId":"ws-1","projectName":"api",
                 "threadId":child,"rootThreadId":root,"turnId":null,"chatTitle":"Fix login",
                 "category":null,"explicit":false,"actionable":true,"createdAt":at(3),"custom":true}]})
        );
        let text = wire.to_string();
        for secret in [
            "git push",
            "database password",
            "operation",
            "patterns",
            "detail",
            "prompt",
        ] {
            assert!(!text.contains(secret), "{secret} leaked: {text}");
        }
    }

    #[test]
    fn a_published_chat_owns_every_item_of_its_tree() {
        let shared = thread_id(10);
        let grandchild = thread_id(12);
        // A grandchild's root is the top of the conversation, which for a
        // shared child chat is not the published chat.
        let top = thread_id(9);
        let project = Project::new("ws".into(), "api", Some(shared));
        let view = View {
            permissions: vec![permission("p", "edit", grandchild, Some(top), 1)],
            questions: vec![question("q", grandchild, None, false, 2)],
            chats: HashMap::from([(shared, chat("Shared"))]),
            ..View::default()
        };
        let snapshot = snapshot([(&project, &view)], at(5));
        assert!(
            snapshot
                .items
                .iter()
                .all(|item| item.root_thread_id == shared.0
                    && item.thread_id == grandchild.0
                    && item.chat_title.as_deref() == Some("Shared"))
        );
        assert_eq!(snapshot.items[1].custom, Some(false));
    }

    #[test]
    fn snapshots_are_bounded_and_finished_turns_expire() {
        let project = Project::new("ws".into(), &"n".repeat(300), None);
        assert_eq!(project.name.chars().count(), MAX_TEXT);
        let root = thread_id(1);
        let mut view = View {
            permissions: (0..250)
                .map(|n| permission(&format!("p-{n:03}"), &"c".repeat(100), root, None, n))
                .collect(),
            chats: HashMap::from([(root, chat(&"t".repeat(300)))]),
            ..View::default()
        };
        // An id Cloud cannot store is left out, not cut.
        view.permissions
            .push(permission(&"x".repeat(201), "bash", root, None, -1));
        view.finished = (0..30)
            .map(|n| Done {
                turn: TurnId(Uuid::from_u128(100 + n)),
                thread: root,
                status: Outcome::Completed,
                at: at(i64::try_from(n).unwrap() * 60),
            })
            .collect();
        let now = at(30 * 60);
        let snapshot = snapshot([(&project, &view)], now);
        assert_eq!(snapshot.items.len(), MAX_ITEMS);
        assert_eq!(snapshot.items[0].id, "p-000", "the oldest are kept");
        assert!(
            snapshot
                .items
                .iter()
                .all(|item| item.category.as_ref().unwrap().len() == MAX_CATEGORY)
        );
        assert_eq!(
            snapshot.finished.len(),
            15,
            "only the last 15 minutes: {:?}",
            snapshot.finished
        );
        assert_eq!(
            snapshot.finished[0].finished_at,
            at(29 * 60),
            "newest first"
        );
        assert!(
            snapshot
                .finished
                .iter()
                .all(|done| now - done.finished_at <= FINISHED_WINDOW)
        );
        assert_eq!(
            snapshot.finished[0]
                .chat_title
                .as_ref()
                .unwrap()
                .chars()
                .count(),
            MAX_TEXT
        );
    }

    #[test]
    fn untitled_chats_have_no_title_and_scheduled_runs_say_so() {
        let thread: Thread = serde_json::from_value(json!({
            "id": thread_id(1), "title": "New chat", "parentId": null, "archived": false,
            "agent": "build", "provider": "echo", "model": "echo-1",
            "createdAt": at(0), "updatedAt": at(0),
            "schedule": {"id": Uuid::from_u128(5), "name": "Nightly"},
        }))
        .unwrap();
        assert_eq!(
            Chat::of(&thread),
            Chat {
                title: None,
                scheduled: true
            }
        );
        assert_eq!(category(""), None);
        assert_eq!(
            category(" cloud_schedule ").as_deref(),
            Some("cloud_schedule")
        );
    }

    #[test]
    fn the_first_snapshot_always_goes_out_and_later_ones_only_on_change() {
        let mut outbox = Outbox::default();
        assert!(outbox.first());
        let Some(Frame::Attention {
            revision: 1, items, ..
        }) = outbox.frame(Snapshot::default())
        else {
            panic!("the empty first snapshot is sent")
        };
        assert!(items.is_empty());
        assert!(!outbox.first());
        assert!(outbox.frame(Snapshot::default()).is_none());
        let changed = Snapshot {
            finished: vec![Finished {
                turn_id: Uuid::from_u128(1),
                project_id: "ws".into(),
                project_name: "api".into(),
                thread_id: Uuid::from_u128(2),
                chat_title: None,
                status: Outcome::Failed,
                scheduled: false,
                finished_at: at(0),
            }],
            ..Snapshot::default()
        };
        assert!(matches!(
            outbox.frame(changed.clone()),
            Some(Frame::Attention { revision: 2, .. })
        ));
        assert!(outbox.frame(changed).is_none());
    }

    fn envelope(
        kind: &str,
        thread: ThreadId,
        root: Option<ThreadId>,
        payload: serde_json::Value,
    ) -> EventEnvelope {
        serde_json::from_value(json!({
            "schemaVersion": "1", "serverInstanceId": Uuid::from_u128(99), "eventId": 4, "type": kind,
            "threadId": thread, "rootThreadId": root, "turnId": null, "itemId": null,
            "payload": payload, "createdAt": at(0),
        }))
        .unwrap()
    }

    fn turn(thread: ThreadId, status: &str) -> serde_json::Value {
        json!({"id": Uuid::from_u128(50), "threadId": thread, "requestId": Uuid::from_u128(51),
            "status": status, "agent": "build", "provider": "echo", "model": "echo-1",
            "error": null, "createdAt": at(0), "updatedAt": at(3)})
    }

    /// The finished turn of a chat a person started, as `Follower::read`
    /// finds it.
    fn finished_root_turn(envelope: &EventEnvelope, chat: Option<ThreadId>) -> Option<Turn> {
        root_turn(envelope, chat).filter(finished)
    }

    #[test]
    fn only_completed_or_failed_turns_of_the_root_chat_finish_it() {
        let root = thread_id(1);
        let child = thread_id(2);
        let finished = |thread, root, status| {
            finished_root_turn(
                &envelope("turn.finished", thread, root, turn(thread, status)),
                None,
            )
        };
        assert!(finished(root, Some(root), "completed").is_some());
        assert!(finished(root, Some(root), "failed").is_some());
        assert!(finished(root, Some(root), "interrupted").is_none());
        assert!(
            finished(child, Some(root), "completed").is_none(),
            "a helper agent's turn"
        );
        assert!(
            finished(root, None, "completed").is_none(),
            "an older server says no root"
        );
        // A published chat is the root of what its connector reports.
        let shared = child;
        let event = envelope(
            "turn.finished",
            shared,
            Some(root),
            turn(shared, "completed"),
        );
        assert!(finished_root_turn(&event, Some(shared)).is_some());
        let other = envelope("turn.finished", root, Some(root), turn(root, "completed"));
        assert!(finished_root_turn(&other, Some(shared)).is_none());
    }

    #[test]
    fn server_sent_frames_parse_and_comments_are_skipped() {
        assert_eq!(Event::parse(": connected\n\n"), None);
        assert_eq!(Event::parse(":keepalive\n\n"), None);
        assert_eq!(
            Event::parse("id: 12\nevent: turn.finished\ndata: {\"a\":\ndata: 1}\n\n"),
            Some(Event {
                id: Some(12),
                kind: "turn.finished".into(),
                data: "{\"a\":\n1}".into()
            })
        );
        assert_eq!(
            Event::parse("event: resync_required\r\ndata: {}\r\n\r\n").map(|event| event.kind),
            Some("resync_required".into())
        );
    }

    #[tokio::test]
    async fn events_are_split_across_chunks() {
        let chunks: Vec<reqwest::Result<&'static [u8]>> = vec![
            Ok(b": connected\n\nid: 1\nevent: a\nda"),
            Ok(b"ta: {}\n"),
            Ok(b"\nid: 2\nevent: b\ndata: {}\n\n"),
        ];
        let mut events = Events::new(futures::stream::iter(chunks), STREAM_IDLE);
        assert_eq!(
            events.next().await.map(|event| (event.id, event.kind)),
            Some((Some(1), "a".into()))
        );
        assert_eq!(
            events.next().await.map(|event| (event.id, event.kind)),
            Some((Some(2), "b".into()))
        );
        assert!(events.next().await.is_none());
    }

    #[tokio::test]
    async fn keepalives_keep_a_quiet_stream_open_and_silence_ends_it() {
        let idle = Duration::from_millis(400);
        // Keepalives for longer than the idle limit, one event, then nothing.
        let body = futures::stream::unfold(0, |sent| async move {
            match sent {
                0..12 => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Some((Ok::<_, reqwest::Error>(&b":keepalive\n\n"[..]), sent + 1))
                }
                12 => Some((Ok(&b"id: 9\nevent: a\ndata: {}\n\n"[..]), sent + 1)),
                _ => std::future::pending().await,
            }
        });
        let mut events = Events::new(Box::pin(body), idle);
        let started = Instant::now();
        assert_eq!(events.next().await.and_then(|event| event.id), Some(9));
        assert!(started.elapsed() > idle, "keepalives kept it open");
        let silent = Instant::now();
        assert!(events.next().await.is_none(), "a silent stream is gone");
        assert!(silent.elapsed() >= idle);
    }

    #[tokio::test]
    async fn an_event_too_large_to_read_is_skipped_but_its_id_is_kept() {
        let huge = format!(
            "id: 5\nevent: item.created\ndata: {{\"text\":\"{}\"}}\n\n",
            "x".repeat(MAX_EVENT)
        )
        .into_bytes();
        let (start, rest) = huge.split_at(MAX_EVENT / 2);
        let (middle, end) = rest.split_at(rest.len() - 1);
        let chunks: Vec<reqwest::Result<Vec<u8>>> = vec![
            Ok(b"id: 4\nevent: a\ndata: {}\n\n".to_vec()),
            Ok(start.to_vec()),
            Ok(middle.to_vec()),
            // The frame's end straddles chunks.
            Ok([end, b"id: 6\nevent: b\ndata: {}\n\n"].concat()),
        ];
        let mut events = Events::new(futures::stream::iter(chunks), STREAM_IDLE);
        let mut read = Vec::new();
        while let Some(event) = events.next().await {
            read.push((event.id, event.kind, event.data.len()));
        }
        assert_eq!(
            read,
            [
                (Some(4), "a".into(), 2),
                (Some(5), String::new(), 0),
                (Some(6), "b".into(), 2)
            ]
        );
    }

    fn info(schema: &str, trusted: bool) -> ServerInfo {
        serde_json::from_value(json!({
            "name": "haarness", "version": "0.1.1", "schemaVersion": schema,
            "serverInstanceId": Uuid::from_u128(1), "projectTrusted": trusted,
            "capabilities": [haarness_protocol::WORKSPACE_HISTORY_CAPABILITY, haarness_protocol::PTY_CAPABILITY],
        }))
        .unwrap()
    }

    fn record(executable: Option<&str>, access: Option<String>) -> RuntimeDiscovery {
        RuntimeDiscovery {
            url: "http://127.0.0.1:1".into(),
            token: "token".into(),
            pid: 1,
            workspace: None,
            executable_fingerprint: executable.map(str::to_owned),
            access_fingerprint: access,
            trace_endpoint: None,
        }
    }

    #[test]
    fn cloud_answers_only_servers_the_gateway_would_keep() {
        let private = Some(ServerAccess::Private.fingerprint("token"));
        let this = Some("this-build");
        assert!(reused_as_is(
            &record(this, private.clone()),
            &info("1", false),
            false,
            this
        ));
        // Another build, or a record from before fingerprints: replaced.
        assert!(!reused_as_is(
            &record(Some("old-build"), private.clone()),
            &info("1", false),
            false,
            this
        ));
        assert!(!reused_as_is(
            &record(None, None),
            &info("1", false),
            false,
            this
        ));
        // Another API schema or project setting: replaced.
        assert!(!reused_as_is(
            &record(this, private.clone()),
            &info("0", false),
            false,
            this
        ));
        assert!(!reused_as_is(
            &record(this, private),
            &info("1", true),
            false,
            this
        ));
        // A configured server is kept whichever build runs it.
        let open = Some(ServerAccess::Open.fingerprint("token"));
        assert!(reused_as_is(
            &record(Some("old-build"), open),
            &info("1", false),
            false,
            this
        ));
        assert!(Route::Direct.reaches(Some(&info("0", true)), None));
        assert!(
            !Route::Gateway {
                record: record(this, None),
                trust: false
            }
            .reaches(None, this),
            "a server that has not answered yet"
        );
    }

    #[test]
    fn only_registered_open_projects_that_a_server_really_serves_are_reachable() {
        let root = discovery::tests::temp("attention-projects");
        let (a, b, c) = (root.join("a"), root.join("b"), root.join("c"));
        for folder in [&a, &b, &c] {
            std::fs::create_dir_all(folder).unwrap();
        }
        let workspace = |id: &str, path: &Path| -> WorkspaceInfo {
            serde_json::from_value(json!({
                "id": id, "projectId": "p", "name": path.file_name().unwrap().to_str().unwrap(),
                "path": path, "kind": "local", "managed": false, "createdAt": at(0),
                "lastOpenedAt": at(0), "exists": true,
            }))
            .unwrap()
        };
        let server = |folder: &str, serves: &Path| {
            let base = Url::parse("http://127.0.0.1:1/").unwrap();
            Server {
                id: format!("instance-{folder}"),
                workspace: serves.display().to_string(),
                version: "0".into(),
                base: base.clone(),
                token: "t".into(),
                capable: false,
                record: Arc::new(discovery::Record {
                    file: root
                        .join("cache/workspaces")
                        .join(folder)
                        .join("runtime.json"),
                    discovery: record(None, None),
                    base,
                }),
            }
        };
        let workspaces = [
            workspace("wa", &a),
            workspace("wb", &b),
            workspace("wc", &c),
        ];
        let hidden = HashSet::from(["wb".to_owned()]);
        let live = reachable(
            vec![
                server("wa", &a),
                server("wb", &b),
                // Its record's folder names another project.
                server("wc", &a),
                // Not registered.
                server("wz", &c),
            ],
            &workspaces,
            &hidden,
        );
        let found: Vec<_> = live
            .iter()
            .map(|(project, server)| {
                (
                    project.id.as_str(),
                    project.name.as_str(),
                    server.id.as_str(),
                )
            })
            .collect();
        assert_eq!(found, [("wa", "a", "instance-wa")]);
        std::fs::remove_dir_all(root).unwrap();
    }
}
