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
    collections::{BTreeSet, HashMap, HashSet},
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
    /// found at its start; a partial one would resolve their items in Cloud.
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

/// What a connector keeps between its connections: where each server's
/// events were read up to, and the turns that finished, so a reconnect
/// (OAuth connectors reconnect every ten minutes) misses neither.
#[derive(Clone, Debug, Default)]
struct Remembered {
    resume: Option<u64>,
    finished: Vec<Done>,
}

/// By server instance.
type Memory = Arc<Mutex<HashMap<String, Remembered>>>;

pub(crate) struct Context {
    paths: Paths,
    trust: bool,
    /// This build's executable: other project servers started by another one
    /// are replaced, not reused, when Cloud routes to them.
    executable: Option<String>,
    timing: Timing,
    memory: Memory,
}

impl Context {
    /// For the connector of the workspace `paths` is scoped to, started with
    /// this project configuration setting.
    pub(crate) fn new(paths: &Paths, trust: bool, timing: Timing) -> Arc<Self> {
        Arc::new(Self {
            paths: paths.clone(),
            trust,
            executable: super::current_executable_fingerprint().ok(),
            timing,
            memory: Arc::default(),
        })
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

/// Report attention over one relay connection until it ends: the first
/// snapshot as soon as every server found at the start has answered (even
/// when nothing waits, so Cloud resolves what an earlier connection left),
/// then one snapshot per change, at most every `debounce`.
pub(crate) async fn watch(context: Arc<Context>, scope: Scope, send: mpsc::Sender<Frame>) {
    let timing = context.timing;
    let (updates, mut received) = mpsc::channel(64);
    let mut watched = HashMap::new();
    let mut discovery = Discovery::new(vec![context.paths.cache.clone()], discovery::http_client());
    let host = matches!(scope, Scope::Host);
    match scope {
        Scope::Host => {
            context.rescan(&mut discovery, &mut watched, &updates).await;
        }
        Scope::Chat {
            url,
            token,
            project,
            thread,
        } => {
            let Ok(base) = Url::parse(&url) else { return };
            let paths = context.paths.clone();
            let id = project.clone();
            let name = tokio::task::spawn_blocking(move || {
                paths
                    .workspace_manager()
                    .and_then(|manager| Ok(manager.get(&id)?.name))
            })
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
            let chat = ThreadId(thread);
            let daemon = Daemon::new(base, token, Some(chat));
            let key = String::from("chat");
            let follower = follow(
                daemon,
                Route::Direct,
                context.memory.clone(),
                timing.settle,
                key.clone(),
                updates.clone(),
            );
            watched.insert(
                key,
                Watched {
                    project: Project::new(project, &name, Some(chat)),
                    view: None,
                    _task: AbortTask(tokio::spawn(follower)),
                },
            );
        }
    }
    let start = Instant::now();
    let mut outbox = Outbox::default();
    let mut flush_at = Some(start);
    let mut scan = tokio::time::interval_at(start + timing.scan, timing.scan);
    scan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = scan.tick(), if host => {
                if context.rescan(&mut discovery, &mut watched, &updates).await {
                    flush_at.get_or_insert(Instant::now() + timing.debounce);
                }
            }
            Some((key, view)) = received.recv() => {
                let Some(entry) = watched.get_mut(&key) else { continue };
                entry.view = Some(view);
                if outbox.first() && watched.values().all(|entry| entry.view.is_some()) {
                    flush_at = Some(Instant::now());
                } else {
                    flush_at.get_or_insert(Instant::now() + timing.debounce);
                }
            }
            () = tokio::time::sleep_until(flush_at.unwrap_or(start)), if flush_at.is_some() => {
                let loaded = watched.values().all(|entry| entry.view.is_some());
                if outbox.first() && !loaded && Instant::now() < start + timing.first {
                    flush_at = Some(start + timing.first);
                    continue;
                }
                flush_at = None;
                let views = watched
                    .values()
                    .filter_map(|entry| Some((&entry.project, entry.view.as_ref()?)));
                if let Some(frame) = outbox.frame(snapshot(views, Utc::now()))
                    && send.send(frame).await.is_err()
                {
                    return;
                }
            }
        }
    }
}

struct Watched {
    project: Project,
    /// None until the server has answered once.
    view: Option<View>,
    _task: AbortTask,
}

impl Context {
    /// Follow the project servers that run now and stop following the ones
    /// that are gone. True when that changed what is followed.
    async fn rescan(
        &self,
        discovery: &mut Discovery,
        watched: &mut HashMap<String, Watched>,
        updates: &mpsc::Sender<(String, View)>,
    ) -> bool {
        let servers = discovery.scan().await;
        let paths = self.paths.clone();
        let registry = tokio::task::spawn_blocking(move || {
            Ok::<_, anyhow::Error>((
                paths.workspace_manager()?.list(None)?.workspaces,
                super::remote::hidden_projects(&paths)?,
            ))
        })
        .await;
        // Unreadable for a moment (being rewritten): keep what is followed.
        let Ok(Ok((workspaces, hidden))) = registry else {
            return false;
        };
        let live = reachable(servers, &workspaces, &hidden);
        self.memory
            .lock()
            .retain(|instance, _| live.iter().any(|(_, server)| &server.id == instance));
        let before = watched.len();
        watched.retain(|key, _| live.iter().any(|(_, server)| &server.id == key));
        let mut changed = watched.len() != before;
        for (project, server) in live {
            if watched.contains_key(&server.id) {
                continue;
            }
            let route = if self.paths.workspace_id.as_ref() == Some(&project.id) {
                Route::Direct
            } else {
                Route::Gateway {
                    record: server.record.discovery.clone(),
                    trust: self.trust,
                    executable: self.executable.clone(),
                }
            };
            let follower = follow(
                Daemon::new(server.base.clone(), server.token.clone(), None),
                route,
                self.memory.clone(),
                self.timing.settle,
                server.id.clone(),
                updates.clone(),
            );
            watched.insert(
                server.id,
                Watched {
                    project,
                    view: None,
                    _task: AbortTask(tokio::spawn(follower)),
                },
            );
            changed = true;
        }
        changed
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
fn reachable(
    servers: Vec<Server>,
    workspaces: &[WorkspaceInfo],
    hidden: &HashSet<String>,
) -> Vec<(Project, Server)> {
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
        executable: Option<String>,
    },
}

impl Route {
    fn reaches(&self, info: &ServerInfo) -> bool {
        match self {
            Self::Direct => true,
            Self::Gateway {
                record,
                trust,
                executable,
            } => reused_as_is(record, info, *trust, executable.as_deref()),
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

/// The finished turn of a chat a person started, not of a helper agent:
/// the root of its conversation, or the published chat itself.
fn finished_root_turn(envelope: &EventEnvelope, chat: Option<ThreadId>) -> Option<Turn> {
    let thread = envelope.thread_id?;
    let root = chat.or(envelope.root_thread_id)?;
    if thread != root {
        return None;
    }
    let turn = serde_json::from_value::<Turn>(envelope.payload.clone()).ok()?;
    matches!(turn.status, TurnStatus::Completed | TurnStatus::Failed).then_some(turn)
}

/// A finished root turn, checked once it had time to settle.
struct Settled {
    event: u64,
    turn: Turn,
    /// Nothing else runs in the chat: no goal or loop took over.
    idle: bool,
    thread: Option<Thread>,
}

async fn settle(daemon: Daemon, event: u64, turn: Turn, delay: Duration) -> Settled {
    tokio::time::sleep(delay).await;
    let active = daemon
        .get::<Option<Turn>>(&format!("threads/{}/active", turn.thread_id))
        .await;
    let thread = daemon
        .get::<Thread>(&format!("threads/{}", turn.thread_id))
        .await
        .ok();
    Settled {
        event,
        idle: matches!(active, Ok(None)),
        turn,
        thread,
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
    /// Finished root turns still settling, by event id.
    unsettled: BTreeSet<u64>,
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

    fn read(
        &mut self,
        event: &Event,
        settle_for: Duration,
        settling: &mut JoinSet<Settled>,
    ) -> Step {
        let step = match event.kind.as_str() {
            "resync_required"
            | "permission.requested"
            | "permission.resolved"
            | "question.requested"
            | "question.resolved" => Step::Refresh,
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
                if let (Some(id), Some(turn)) = (
                    event.id,
                    event
                        .envelope()
                        .and_then(|envelope| finished_root_turn(&envelope, self.daemon.scope)),
                ) {
                    self.unsettled.insert(id);
                    settling.spawn(settle(self.daemon.clone(), id, turn, settle_for));
                }
                // Its prompts were answered or withdrawn.
                Step::Refresh
            }
            _ => Step::Nothing,
        };
        if let Some(id) = event.id {
            self.cursor = Some(id);
            self.memory
                .lock()
                .entry(self.instance.clone())
                .or_default()
                .resume = self.resume();
        }
        step
    }

    /// Count a settled turn as finished when nothing took over. True when
    /// the view changed.
    fn settled(&mut self, settled: Settled) -> bool {
        self.unsettled.remove(&settled.event);
        let mut changed = false;
        if let Some(thread) = &settled.thread {
            let chat = Chat::of(thread);
            changed |= self.view.chats.insert(thread.id, chat.clone()) != Some(chat);
        }
        let turn = settled.turn;
        if settled.idle && !self.view.finished.iter().any(|done| done.turn == turn.id) {
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
            changed = true;
        }
        let now = Utc::now();
        self.view
            .finished
            .retain(|done| now - done.at <= FINISHED_WINDOW);
        self.view
            .finished
            .sort_by(|left, right| right.at.cmp(&left.at));
        self.view.finished.truncate(MAX_FINISHED);
        let mut memory = self.memory.lock();
        let remembered = memory.entry(self.instance.clone()).or_default();
        remembered.resume = self.resume();
        remembered.finished.clone_from(&self.view.finished);
        changed
    }

    /// Where a later connection resumes: before the oldest finished turn
    /// still settling, so it is checked again rather than lost.
    fn resume(&self) -> Option<u64> {
        self.unsettled
            .first()
            .map(|event| event.saturating_sub(1))
            .or(self.cursor)
    }
}

/// Follow one server until the watcher drops it: its pending approvals and
/// questions, the chats they wait in, and the root turns that finished. Each
/// change goes to the watcher as the whole view.
async fn follow(
    daemon: Daemon,
    route: Route,
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
        memory.get(&instance).cloned().unwrap_or_default()
    };
    let mut follower = Follower {
        daemon,
        instance,
        memory,
        view: View {
            reachable: route.reaches(&info),
            finished: remembered.finished,
            ..View::default()
        },
        loaded: false,
        cursor: remembered.resume,
        unsettled: BTreeSet::new(),
    };
    let mut settling = JoinSet::new();
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
                        && follower.settled(settled)
                        && !report(&follower).await
                    {
                        return;
                    }
                }
                () = tokio::time::sleep_until(refresh_at.unwrap_or_else(Instant::now)), if refresh_at.is_some() => {
                    refresh_at = None;
                    if follower.refresh().await.is_err() {
                        refresh_at = Some(Instant::now() + refreshes.wait(Link::Failed));
                    } else {
                        refreshes = Backoff::new(discovery::retry_pacing());
                        if !report(&follower).await {
                            return;
                        }
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
            finished: vec![],
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
        assert!(Route::Direct.reaches(&info("0", true)));
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
