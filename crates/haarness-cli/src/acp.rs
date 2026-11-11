//! Agent Client Protocol adapter backed by the canonical Haarness REST/SSE server.
//!
//! Keeping ACP as a client of the workspace server preserves the single runtime
//! owner invariant: editor sessions, the TUI, and the CLI all observe the same
//! durable threads, approvals, tools, and event stream.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fmt::Write as _,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

#[cfg(test)]
use agent_client_protocol::schema::v1::{
    AudioContent, EmbeddedResource, ImageContent, McpServerStdio, TextResourceContents,
};
use agent_client_protocol::schema::{
    ProtocolVersion,
    v1::{
        AgentCapabilities, CancelNotification, CloseSessionRequest, CloseSessionResponse,
        ContentBlock, ContentChunk, Cost, Diff, EmbeddedResourceResource, Implementation,
        InitializeRequest, InitializeResponse, LoadSessionRequest, LoadSessionResponse,
        McpCapabilities, McpServer, NewSessionRequest, NewSessionResponse, PermissionOption,
        PermissionOptionKind, Plan, PlanEntry, PlanEntryPriority, PlanEntryStatus,
        PromptCapabilities, PromptRequest, PromptResponse, RequestPermissionOutcome,
        RequestPermissionRequest, RequestPermissionResponse, SessionCapabilities,
        SessionCloseCapabilities, SessionId, SessionNotification, SessionUpdate, StopReason,
        TextContent, ToolCall, ToolCallContent, ToolCallLocation, ToolCallStatus, ToolCallUpdate,
        ToolCallUpdateFields, ToolKind, UsageUpdate,
    },
};
use agent_client_protocol::{Agent, Client, ConnectionTo, Stdio};
use anyhow::{Context as _, Result};
use haarness_protocol::{
    CreateThreadRequest, Item, ItemData, McpConfigureRequest, McpOwner, PermissionReply,
    PermissionReplyDecision, RequestId, StartTurnRequest, Thread, ThreadId, Turn, TurnId,
    TurnStatus,
};
use haarness_tui::api::{ApiClient, SubscriptionEvent};
use serde_json::{Value, json};
use uuid::Uuid;

/// An MCP server of a session: Haarness's name for it, and the editor's.
type SessionServer = (String, String);

/// An MCP server an editor passed: the sessions it came with (chats made
/// within a minute or so share the start of their id, which names it), the
/// editor's name for it and its settings, and the server it was last
/// configured on, by its instance id.
struct SessionMcp {
    sessions: HashSet<String>,
    shown: String,
    config: McpConfigureRequest,
    on: Uuid,
    /// When it was last configured, once the server had taken it: a listing
    /// read before that may not show it.
    since: std::time::Instant,
    /// Held while it is configured, so a session sharing it waits for that
    /// rather than reading a listing in between.
    turn: Arc<tokio::sync::Mutex<()>>,
}

/// How long `haarness acp` waits, as it exits, for the server to take back
/// its sessions' MCP servers.
const MCP_RELEASE_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// How long setting a session up waits for one of its MCP servers to start
/// before leaving the rest of that start to the server (see
/// `Service::start_mcp`).
const MCP_START_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a prompt's answer, or a load's, waits for the chat's usage
/// before going without its usage update.
const USAGE_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Most bytes of file text, before and after together, the diffs of one
/// prompt or one load carry. The text result goes with each call regardless.
const DIFF_TEXT_LIMIT: usize = 1_000_000;

pub struct Config {
    pub client: ApiClient,
    pub workspace: PathBuf,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub agent: Option<String>,
    pub auto_approve: bool,
}

#[derive(Clone)]
struct Service {
    client: ApiClient,
    workspace: PathBuf,
    provider: Option<String>,
    model: Option<String>,
    agent: Option<String>,
    auto_approve: bool,
    active: Arc<Mutex<HashMap<String, TurnId>>>,
    /// This process, named with each MCP server it configures: the server
    /// keeps it while any process that configured it runs and has not let it
    /// go, the same chat open in a second window's included, and sees one
    /// an editor killed (SIGKILL says nothing) gone.
    owner: McpOwner,
    /// The MCP servers the sessions' editors passed, by Haarness's name for
    /// them, so a prompt can configure again one the server no longer has
    /// (see `Service::restore_mcp`).
    mcp: Arc<Mutex<HashMap<String, SessionMcp>>>,
    /// Each session's MCP servers still starting when it was set up, by
    /// session: Haarness's name and the editor's. Its next prompt waits for
    /// them (see `Service::await_mcp`).
    starting: Arc<Mutex<HashMap<String, Vec<SessionServer>>>>,
    /// A prompt's wait for its session's servers, which Cancel ends.
    waits: Arc<Mutex<HashMap<String, tokio_util::sync::CancellationToken>>>,
    /// Held by a session's prompt until its turn has ended (see
    /// `Service::prompt`).
    gates: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    /// The folder each session was last opened in (`cwd` of its
    /// `session/new` or `session/load`), as the editor spells it.
    cwds: Arc<Mutex<HashMap<String, PathBuf>>>,
}

impl Service {
    fn new(config: Config) -> Self {
        Self {
            // The person in the editor can cancel what a prompt starts.
            client: config.client.attended(),
            workspace: config.workspace,
            provider: config.provider,
            model: config.model,
            agent: config.agent,
            auto_approve: config.auto_approve,
            active: Arc::new(Mutex::new(HashMap::new())),
            owner: McpOwner {
                pid: std::process::id(),
                // Unknown, the server cannot tell when the process ends, and
                // keeps its servers until it lets them go.
                started: haarness_core::process_groups::process_start(std::process::id())
                    .unwrap_or_default(),
                rejoin: false,
            },
            mcp: Arc::new(Mutex::new(HashMap::new())),
            starting: Arc::new(Mutex::new(HashMap::new())),
            waits: Arc::new(Mutex::new(HashMap::new())),
            gates: Arc::new(Mutex::new(HashMap::new())),
            cwds: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The turn an editor's prompt starts.
    fn prompt_turn(&self, text: String, attachments: Vec<String>) -> StartTurnRequest {
        StartTurnRequest {
            request_id: RequestId::new(),
            text,
            attachments,
            resources: Vec::new(),
            synthetic: false,
            auto_approve: self.auto_approve,
            agent: None,
            provider: None,
            model: None,
            reasoning_effort: None,
            tools: Some(HashMap::from([
                ("question".into(), false),
                ("preview".into(), false),
            ])),
            command: None,
            ..Default::default()
        }
    }
}

pub async fn serve(config: Config) -> Result<()> {
    let service = Service::new(config);

    let create = service.clone();
    let load = service.clone();
    let prompt = service.clone();
    let cancel = service.clone();
    let close = service.clone();
    let served = Agent
        .builder()
        .name("haarness")
        .on_receive_request(
            async move |request: InitializeRequest, responder, _connection| {
                responder.respond(Service::initialize(request))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: NewSessionRequest, responder, connection| {
                let service = create.clone();
                connection.spawn(async move {
                    responder.respond_with_result(service.create(request).await)
                })?;
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: LoadSessionRequest, responder, connection| {
                let service = load.clone();
                let updates = connection.clone();
                connection.spawn(async move {
                    responder.respond_with_result(service.load(request, &updates).await)
                })?;
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: PromptRequest, responder, connection| {
                let service = prompt.clone();
                let updates = connection.clone();
                connection.spawn(async move {
                    responder.respond_with_result(service.prompt(request, &updates).await)
                })?;
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |notification: CancelNotification, connection| {
                let service = cancel.clone();
                connection.spawn(async move { service.cancel(&notification.session_id).await })?;
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: CloseSessionRequest, responder, connection| {
                let service = close.clone();
                connection.spawn(async move {
                    responder.respond_with_result(service.close(&request.session_id).await)
                })?;
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(Stdio::new());
    // The editor closing the connection (it quit, or stopped this process)
    // ends the sessions: their MCP servers are let go, so no process of
    // theirs runs on and no other chat is offered their tools.
    let result = tokio::select! {
        result = served => result.context("serve ACP over stdio"),
        () = stopped() => Ok(()),
    };
    service.release_all().await;
    result
}

/// A server a session had on the project's earlier server that was turned
/// off on the one serving it now (see `Service::start_mcp`).
#[derive(Debug)]
struct TurnedOff;

impl std::fmt::Display for TurnedOff {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the MCP server was turned off")
    }
}

impl std::error::Error for TurnedOff {}

/// Debug builds under the test providers wait
/// `HAARNESS_TEST_ACP_RESTORE_DELAY_MS` before configuring a session's server
/// again, so a test can have a session sharing it read the listing first.
async fn restore_delay() {
    if cfg!(debug_assertions)
        && std::env::var_os("HAARNESS_TEST_PROVIDER").is_some()
        && let Some(delay) = std::env::var("HAARNESS_TEST_ACP_RESTORE_DELAY_MS")
            .ok()
            .and_then(|delay| delay.parse().ok())
    {
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
    }
}

/// A signal that asks `haarness acp` to stop.
async fn stopped() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut signals = [
            SignalKind::terminate(),
            SignalKind::interrupt(),
            SignalKind::hangup(),
        ]
        .into_iter()
        .filter_map(|kind| signal(kind).ok())
        .collect::<Vec<_>>();
        if signals.is_empty() {
            return std::future::pending().await;
        }
        let waits = signals.iter_mut().map(|signal| Box::pin(signal.recv()));
        futures::future::select_all(waits).await;
    }
    #[cfg(not(unix))]
    {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

impl Service {
    fn initialize(_request: InitializeRequest) -> InitializeResponse {
        InitializeResponse::new(ProtocolVersion::V1)
            .agent_capabilities(
                AgentCapabilities::new()
                    .load_session(true)
                    .prompt_capabilities(
                        // `prompt_content` turns audio blocks into attachments too.
                        PromptCapabilities::new()
                            .image(true)
                            .audio(true)
                            .embedded_context(true),
                    )
                    .mcp_capabilities(McpCapabilities::new().http(true).sse(true))
                    .session_capabilities(
                        SessionCapabilities::new().close(SessionCloseCapabilities::new()),
                    ),
            )
            .agent_info(
                Implementation::new("haarness", env!("CARGO_PKG_VERSION")).title("Haarness"),
            )
    }

    async fn create(&self, request: NewSessionRequest) -> AcpResult<NewSessionResponse> {
        self.validate_scope(&request.cwd, &request.additional_directories)?;
        let server = self.client.reach_server().await.map_err(unreachable)?;
        let thread = self
            .client
            .create_thread(CreateThreadRequest {
                id: None,
                schedule: None,
                title: Some("ACP session".into()),
                parent_id: None,
                agent: self.agent.clone(),
                provider: self.provider.clone(),
                model: self.model.clone(),
                ultracode: None,
                directory: None,
            })
            .await
            .map_err(internal)?;
        self.configure_mcp(
            thread.id,
            request.mcp_servers,
            &request.cwd,
            server.server_instance_id,
        )
        .await?;
        self.cwds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(thread.id.to_string(), request.cwd);
        Ok(NewSessionResponse::new(thread.id.to_string()))
    }

    async fn load(
        &self,
        request: LoadSessionRequest,
        connection: &ConnectionTo<Client>,
    ) -> AcpResult<LoadSessionResponse> {
        self.validate_scope(&request.cwd, &request.additional_directories)?;
        let thread = thread_id(&request.session_id)?;
        let server = self.client.reach_server().await.map_err(unreachable)?;
        let chat = self.client.thread(thread).await.map_err(not_found)?;
        self.configure_mcp(
            thread,
            request.mcp_servers,
            &request.cwd,
            server.server_instance_id,
        )
        .await?;
        self.cwds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(request.session_id.to_string(), request.cwd);
        let items = self.client.items(thread).await.map_err(internal)?;
        let mut files = self.files(&chat, &request.session_id);
        let mut diffs = files.replayed_diffs(&items);
        let mut calls = HashMap::new();
        for (index, item) in items.into_iter().enumerate() {
            let diffs = diffs.remove(&index).unwrap_or_default();
            Self::replay(
                &request.session_id,
                item,
                &files,
                &mut calls,
                diffs,
                connection,
            )?;
        }
        self.report_usage(thread, &request.session_id, connection)
            .await;
        Ok(LoadSessionResponse::new())
    }

    async fn prompt(
        &self,
        request: PromptRequest,
        connection: &ConnectionTo<Client>,
    ) -> AcpResult<PromptResponse> {
        // A prompt with nothing in it fails at once, not behind a running one.
        let thread = thread_id(&request.session_id)?;
        let (text, attachments) = prompt_content(request.prompt)?;
        if text.trim().is_empty() && attachments.is_empty() {
            return Err(invalid("prompt must contain text or supported media"));
        }
        let key = request.session_id.to_string();
        // One prompt of a session at a time: a prompt sent while an earlier
        // one waits for the session's MCP servers, or runs, goes after it in
        // the order they were sent, rather than ahead of it or refused as the
        // chat is busy, and shares that wait. Cancel reaches the prompt from
        // here on.
        let gate = self
            .gates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(key.clone())
            .or_default()
            .clone();
        let held = gate.lock().await;
        let stop = tokio_util::sync::CancellationToken::new();
        self.waits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.clone(), stop.clone());
        let begun: AcpResult<Option<_>> = async {
            // The project's server may have been replaced since the last
            // prompt (an update), or stopped: the prompt goes to the one that
            // serves the project now, with the session's servers set up there.
            let server = self.client.reach_server().await.map_err(unreachable)?;
            let chat = self.client.thread(thread).await.map_err(not_found)?;
            self.restore_mcp(&request.session_id, server.server_instance_id)
                .await;
            if self
                .await_mcp(&request.session_id, &stop, connection)
                .await?
            {
                return Ok(None);
            }
            let (events, event_task) = self.client.subscribe(None);
            let turn = self
                .client
                .start_turn(thread, self.prompt_turn(text, attachments))
                .await
                .map_err(|error| {
                    event_task.abort();
                    refused(&error)
                })?;
            self.active
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(key.clone(), turn.id);
            // Cancelled while the turn was being started.
            if stop.is_cancelled() {
                let _ = self.client.interrupt(turn.id).await;
            }
            Ok(Some((
                turn,
                events,
                event_task,
                self.files(&chat, &request.session_id),
            )))
        }
        .await;
        self.waits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
        let Some((turn, mut events, event_task, mut files)) = begun? else {
            return Ok(PromptResponse::new(StopReason::Cancelled));
        };

        let mut calls = Calls::new();
        // The chats this turn starts under the session (subagents, workflow
        // agents): what each is called where the client is shown its work.
        let mut children = HashMap::<ThreadId, Child>::new();
        let mut approvals = Approvals::default();
        // Requests still waiting from before this prompt (a background
        // subagent that asked after the last one ended) come first. The event
        // stream starts with the server's recent events; those from before
        // the turn are skipped below, so each request is taken once.
        self.refresh_pending(thread, &mut approvals, &mut children)
            .await;
        // A turn that ends while the client shows a request (a background
        // subagent's, say) leaves it up until it is answered or settled, so
        // the person's answer is not lost; nothing else is shown meanwhile.
        let mut ended = None;
        let result = async {
            loop {
                if ended.is_none() {
                    self.show_next(&mut approvals, &request.session_id, connection, &calls)
                        .await;
                } else {
                    // A cancel has no turn left to interrupt.
                    self.active
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&key);
                }
                let showing = approvals.shown.is_some();
                if !showing && let Some(turn) = ended.take() {
                    break finish(turn);
                }
                let next = tokio::select! {
                    answer = async {
                        match approvals.shown.as_mut() {
                            Some((_, answer)) => answer.as_mut().await,
                            None => std::future::pending().await,
                        }
                    }, if showing => Step::Answer(answer),
                    event = events.recv() => Step::Event(event),
                };
                let event = match next {
                    Step::Answer(answer) => {
                        let Some((id, _)) = approvals.shown.take() else {
                            continue;
                        };
                        approvals.settled.insert(id.clone());
                        self.answer(&id, answer?).await?;
                        continue;
                    }
                    Step::Event(event) => event,
                };
                let Some(event) = event else {
                    break Err(internal("Haarness event stream closed during an ACP turn"));
                };
                match event {
                    // A request from the session's own chat, its subagents or
                    // the workflows it runs, outside a turn included: the client
                    // is the only one who can answer it. One is shown at a
                    // time; the others wait behind it.
                    SubscriptionEvent::Event(event)
                        if event.event_type == "permission.requested"
                            && event.created_at >= turn.created_at
                            && (event.thread_id == Some(thread)
                                || event.root_thread_id == Some(thread)) =>
                    {
                        let Some(id) = event.payload["permissionId"].as_str() else {
                            continue;
                        };
                        let child = match event.thread_id {
                            Some(id) if id != thread => Some(self.child(id, &mut children).await),
                            _ => None,
                        };
                        approvals.wait(id.to_owned(), event.payload.clone(), child);
                    }
                    // Settled by the server: answered elsewhere, or by a
                    // sibling's Reject or Always. The client's request for it is
                    // cancelled, or never sent.
                    SubscriptionEvent::Event(event)
                        if event.event_type == "permission.resolved"
                            && (event.thread_id == Some(thread)
                                || event.root_thread_id == Some(thread)) =>
                    {
                        if let Some(id) = event.payload["permissionId"].as_str() {
                            approvals.settle(id);
                        }
                    }
                    SubscriptionEvent::Event(event)
                        if event.event_type == "task.updated"
                            && event.root_thread_id == Some(thread) =>
                    {
                        name_child(&event.payload, &calls, &mut children);
                    }
                    SubscriptionEvent::Event(event)
                        if event.event_type == "item.created"
                            && event.created_at >= turn.created_at
                            && event.root_thread_id == Some(thread)
                            && event.thread_id.is_some_and(|id| id != thread) =>
                    {
                        let Some(id) = event.thread_id else { continue };
                        let item = serde_json::from_value::<Item>(event.payload.clone())
                            .map_err(internal)?;
                        let child = self.child(id, &mut children).await;
                        Self::child_item(
                            &request.session_id,
                            item,
                            &child,
                            connection,
                            &mut calls,
                            &mut files,
                        )?;
                    }
                    SubscriptionEvent::Event(event)
                        if event.thread_id == Some(thread) && event.turn_id == Some(turn.id) =>
                    {
                        if event.event_type == "turn.text_delta" {
                            if let Some(delta) = event.payload["delta"].as_str() {
                                notify(
                                    connection,
                                    &request.session_id,
                                    SessionUpdate::AgentMessageChunk(ContentChunk::new(
                                        ContentBlock::Text(TextContent::new(delta)),
                                    )),
                                )?;
                            }
                            continue;
                        }
                        if event.event_type == "turn.reasoning_delta" {
                            if let Some(delta) = event.payload["delta"].as_str() {
                                notify(
                                    connection,
                                    &request.session_id,
                                    SessionUpdate::AgentThoughtChunk(ContentChunk::new(
                                        ContentBlock::Text(TextContent::new(delta)),
                                    )),
                                )?;
                            }
                            continue;
                        }
                        if event.event_type == "item.created" {
                            let item = serde_json::from_value::<Item>(event.payload.clone())
                                .map_err(internal)?;
                            Self::live_item(
                                &request.session_id,
                                item,
                                connection,
                                &mut calls,
                                &mut files,
                            )?;
                            continue;
                        }
                        if event.event_type == "turn.finished" {
                            ended = Some(
                                serde_json::from_value::<Turn>(event.payload.clone())
                                    .map_err(internal)?,
                            );
                        }
                    }
                    // Events may have been missed: requests raised or settled
                    // meanwhile are read again from the server.
                    SubscriptionEvent::ResyncRequired(_) => {
                        if ended.is_none() {
                            let current = self.client.turn(turn.id).await.map_err(internal)?;
                            if !matches!(current.status, TurnStatus::Queued | TurnStatus::Running) {
                                ended = Some(current);
                            }
                        }
                        self.refresh_pending(thread, &mut approvals, &mut children)
                            .await;
                    }
                    SubscriptionEvent::Reconnected => {
                        self.refresh_pending(thread, &mut approvals, &mut children)
                            .await;
                    }
                    SubscriptionEvent::Event(_)
                    | SubscriptionEvent::ConnectionLost { .. }
                    | SubscriptionEvent::ServerRestarting
                    | SubscriptionEvent::ServerRestartFailed(_) => {}
                }
            }
        }
        .await;
        event_task.abort();
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&request.session_id.to_string());
        // Before the answer, and before the session's next prompt starts.
        self.report_usage(thread, &request.session_id, connection)
            .await;
        drop(held);
        result
    }

    async fn cancel(&self, session: &SessionId) -> AcpResult<()> {
        if let Some(wait) = self
            .waits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session.to_string())
        {
            wait.cancel();
        }
        let turn = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session.to_string())
            .copied();
        if let Some(turn) = turn {
            self.client.interrupt(turn).await.map_err(internal)?;
        }
        Ok(())
    }

    fn validate_scope(&self, cwd: &Path, additional: &[PathBuf]) -> AcpResult<()> {
        if !cwd.is_absolute() {
            return Err(invalid("ACP session cwd must be absolute"));
        }
        if !additional.is_empty() {
            return Err(invalid(
                "Haarness ACP does not yet support additionalDirectories",
            ));
        }
        let actual = dunce::canonicalize(cwd)
            .map_err(|error| invalid(format!("cannot resolve ACP cwd: {error}")))?;
        if actual != self.workspace {
            return Err(invalid(format!(
                "ACP cwd {} does not match the Haarness workspace {}",
                actual.display(),
                self.workspace.display()
            )));
        }
        Ok(())
    }

    /// What a prompt or a load of `session` tells the editor about files:
    /// paths are read from the folder `chat`'s tools work in (its own, or
    /// the workspace), and those in the workspace are spelled from the
    /// folder the editor opened the session in, which names the same folder
    /// (see `validate_scope`) but may reach it another way (a link, a
    /// mapped drive).
    fn files(&self, chat: &Thread, session: &SessionId) -> Files {
        let folder = chat
            .directory
            .as_deref()
            .map_or_else(|| self.workspace.clone(), PathBuf::from);
        let opened = self
            .cwds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session.to_string())
            .map(|cwd| fold(cwd));
        Files::new(folder, opened.map(|cwd| (self.workspace.clone(), cwd)))
    }

    /// Tells the editor how full the chat's context is and what it has cost
    /// (see [`usage_update`]). A server that does not answer in time leaves
    /// the editor's last numbers as they are.
    async fn report_usage(
        &self,
        thread: ThreadId,
        session: &SessionId,
        connection: &ConnectionTo<Client>,
    ) {
        let Ok(Ok(usage)) =
            tokio::time::timeout(USAGE_WAIT, self.client.thread_usage(thread)).await
        else {
            return;
        };
        if let Some(update) = usage_update(&usage) {
            let _ = notify(connection, session, SessionUpdate::UsageUpdate(update));
        }
    }

    async fn configure_mcp(
        &self,
        thread: ThreadId,
        servers: Vec<McpServer>,
        cwd: &Path,
        on: Uuid,
    ) -> AcpResult<()> {
        let session = thread.to_string();
        for server in servers {
            let (shown, config) = mcp_config(server, cwd)?;
            let name = format!("acp-{}-{shown}", &session[..8]);
            // Held from the moment the name is known, so a session sharing
            // it waits until the server has taken it.
            let held = {
                let mut known = self
                    .mcp
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(passed) = known.get_mut(&name) {
                    passed.sessions.insert(session.clone());
                    continue;
                }
                let turn = Arc::new(tokio::sync::Mutex::new(()));
                let held = turn
                    .clone()
                    .try_lock_owned()
                    .expect("a lock made just now is free");
                known.insert(
                    name.clone(),
                    SessionMcp {
                        sessions: HashSet::from([session.clone()]),
                        shown: shown.clone(),
                        config: config.clone(),
                        on,
                        since: std::time::Instant::now(),
                        turn,
                    },
                );
                held
            };
            let started = self.start_mcp(&name, &config, false).await;
            if !self.configured(&name, on) {
                // The session closed meanwhile.
                self.release_mcp(&name, config).await;
                continue;
            }
            drop(held);
            match started {
                Ok(true) => self
                    .starting
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .entry(session.clone())
                    .or_default()
                    .push((name, shown)),
                Ok(false) => {}
                Err(error) => {
                    self.forget_mcp(&name, &session).await;
                    return Err(internal(error));
                }
            }
        }
        Ok(())
    }

    /// Forget `name` for `session`, where it did not start, so the
    /// session's next load configures it again. Once no session has it, it
    /// is let go on the server: an editor retrying the session configures it
    /// again under the new chat's name, and this one would serve every chat.
    async fn forget_mcp(&self, name: &str, session: &str) {
        let released = {
            let mut known = self
                .mcp
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match known.get_mut(name) {
                Some(server) => {
                    server.sessions.remove(session);
                    server
                        .sessions
                        .is_empty()
                        .then(|| known.remove(name))
                        .flatten()
                }
                None => None,
            }
        };
        if let Some(server) = released {
            self.let_go(name, server).await;
        }
    }

    /// Let `server` (no longer tracked) go once a configure of it on its way
    /// has ended, waiting [`MCP_RELEASE_WAIT`] for that at most: let go
    /// before that configure landed, it would start again.
    async fn let_go(&self, name: &str, server: SessionMcp) {
        let _turn = tokio::time::timeout(MCP_RELEASE_WAIT, server.turn.lock()).await;
        self.release_mcp(name, server.config).await;
    }

    /// Release `name` on the server as this process's: the server goes once
    /// no process that configured it is left.
    async fn release_mcp(&self, name: &str, mut config: McpConfigureRequest) {
        config.enabled = false;
        config.owner = Some(self.owner.clone());
        if let Err(error) = self.client.configure_mcp(name, &config).await {
            tracing::debug!(server = %name, %error, "an editor's MCP server was not let go");
        }
    }

    /// The editor closed `session`: its work stops, and the MCP servers no
    /// other open session shares are let go.
    async fn close(&self, session: &SessionId) -> AcpResult<CloseSessionResponse> {
        let _ = self.cancel(session).await;
        let key = session.to_string();
        let released: Vec<(String, SessionMcp)> = {
            let mut known = self
                .mcp
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for server in known.values_mut() {
                server.sessions.remove(&key);
            }
            let unused: Vec<String> = known
                .iter()
                .filter(|(_, server)| server.sessions.is_empty())
                .map(|(name, _)| name.clone())
                .collect();
            unused
                .into_iter()
                .filter_map(|name| known.remove(&name).map(|server| (name, server)))
                .collect()
        };
        self.starting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
        futures::future::join_all(
            released
                .into_iter()
                .map(|(name, server)| async move { self.let_go(&name, server).await }),
        )
        .await;
        Ok(CloseSessionResponse::new())
    }

    /// Let every session's MCP servers go, as `haarness acp` exits, waiting
    /// [`MCP_RELEASE_WAIT`] at most.
    async fn release_all(&self) {
        let servers: Vec<(String, SessionMcp)> = self
            .mcp
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
            .collect();
        if servers.is_empty() {
            return;
        }
        let releases = servers
            .into_iter()
            .map(|(name, server)| async move { self.let_go(&name, server).await });
        let _ = tokio::time::timeout(MCP_RELEASE_WAIT, futures::future::join_all(releases)).await;
    }

    /// Record that `name` was configured on the server `on` and that the
    /// server has taken it; `false` when it is no longer tracked (its
    /// sessions closed meanwhile), and is to be let go.
    fn configured(&self, name: &str, on: Uuid) -> bool {
        let mut known = self
            .mcp
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(server) = known.get_mut(name) else {
            return false;
        };
        server.on = on;
        server.since = std::time::Instant::now();
        true
    }

    /// Before a prompt, configure each of the session's MCP servers on a new
    /// server (`on`, a restart or an update) this process has not configured
    /// it on yet: again when the server does not have it, and to become one
    /// of its owners when another process on the chat set it up there. One
    /// still starting, configured again here or by a session sharing it, is
    /// waited for and told to the editor as a new session's are (see
    /// `Service::await_mcp`). One the server it was configured on no longer
    /// lists was turned off there (by the person, or through the API): the
    /// sessions let it go, and a later restart does not bring it back. One
    /// the person disconnected is still listed, and stays as it is.
    async fn restore_mcp(&self, session: &SessionId, on: Uuid) {
        let key = session.to_string();
        let servers: Vec<String> = self
            .mcp
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(_, server)| server.sessions.contains(&key))
            .map(|(name, _)| name.clone())
            .collect();
        if servers.is_empty() {
            return;
        }
        let asked = std::time::Instant::now();
        let Ok(listed) = self.client.mcp().await else {
            return;
        };
        for name in servers {
            let Some(turn) = self
                .mcp
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&name)
                .map(|server| server.turn.clone())
            else {
                continue;
            };
            // A session sharing it that is configuring it has the turn: this
            // one decides once that is done.
            let held = turn.lock().await;
            let status = listed
                .iter()
                .find(|listed| listed.name == name)
                .map(|listed| listed.status.clone());
            let (shown, config, wait) = {
                let mut known = self
                    .mcp
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(server) = known.get_mut(&name) else {
                    continue;
                };
                // Configured by this process on this server (or seen there).
                let ours = server.on == on;
                let configured_since = ours && server.since > asked;
                match status.as_deref() {
                    // Set up there by another process on the chat (the same
                    // chat open in another window, after a restart): this one
                    // becomes an owner too, so the server stays when that one
                    // ends.
                    Some(status @ ("connected" | "connecting" | "needs_auth")) if !ours => (
                        server.shown.clone(),
                        Some(server.config.clone()),
                        status == "connecting",
                    ),
                    Some(status) => {
                        server.on = on;
                        if status != "connecting" {
                            continue;
                        }
                        (server.shown.clone(), None, true)
                    }
                    // Configured after the listing was read, for a session of
                    // this process sharing it.
                    None if configured_since => (server.shown.clone(), None, true),
                    None if ours => {
                        tracing::info!(server = %name, "an editor's MCP server was turned off; its sessions let it go");
                        known.remove(&name);
                        continue;
                    }
                    None => (server.shown.clone(), Some(server.config.clone()), false),
                }
            };
            if let Some(config) = config {
                tracing::info!(server = %name, "configuring an editor's MCP server on a new Haarness server");
                restore_delay().await;
                let started = self.start_mcp(&name, &config, true).await;
                if started.as_ref().is_err_and(anyhow::Error::is::<TurnedOff>) {
                    tracing::info!(server = %name, "an editor's MCP server was turned off; its sessions let it go");
                    self.mcp
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&name);
                    continue;
                }
                if !self.configured(&name, on) {
                    self.release_mcp(&name, config).await;
                    continue;
                }
                if matches!(started, Ok(false)) && !wait {
                    continue;
                }
            }
            drop(held);
            let mut starting = self
                .starting
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let waits = starting.entry(key.clone()).or_default();
            if !waits.iter().any(|(waited, _)| *waited == name) {
                waits.push((name, shown));
            }
        }
    }

    /// Before a prompt, wait for the session's MCP servers still starting
    /// when it was set up, as a turn waits for the servers Haarness starts
    /// itself: a start an editor asked for holds up no other chat. Cancel
    /// (`stop`) ends the wait (`true`) and the next prompt waits again. A
    /// server that did not start is told to the editor, logged and let go
    /// (see `Service::forget_mcp`); one still starting after the wait is
    /// told too and waited for again next time. Either way the prompt goes
    /// on without it.
    async fn await_mcp(
        &self,
        session: &SessionId,
        stop: &tokio_util::sync::CancellationToken,
        connection: &ConnectionTo<Client>,
    ) -> AcpResult<bool> {
        let key = session.to_string();
        let servers = self
            .starting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key)
            .unwrap_or_default();
        if servers.is_empty() {
            return Ok(stop.is_cancelled());
        }
        let mut still = Vec::new();
        let mut servers = servers.into_iter();
        let mut cancelled = false;
        while let Some((name, shown)) = servers.next() {
            // The server's tools are answered once its start has ended
            // (150 s at most, then it is waited for again next time).
            let waited = tokio::select! {
                () = stop.cancelled() => None,
                _ = self.client.mcp_tools(&name) => Some(()),
            };
            if waited.is_none() {
                cancelled = true;
                still.push((name, shown));
                still.extend(servers.by_ref());
                break;
            }
            let status = self
                .client
                .mcp()
                .await
                .ok()
                .and_then(|listed| listed.into_iter().find(|server| server.name == name));
            match status {
                Some(server) if matches!(server.status.as_str(), "connected" | "needs_auth") => {}
                // Still starting after the wait (a limit set past 150 s):
                // the next prompt waits for it again.
                Some(server) if server.status == "connecting" => {
                    notify(
                        connection,
                        session,
                        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                            TextContent::new(format!(
                                "MCP server {shown} is still starting, so this prompt runs without its tools.\n\n"
                            )),
                        ))),
                    )?;
                    still.push((name, shown));
                }
                server => {
                    let why = server
                        .and_then(|server| server.detail.or(Some(server.status)))
                        .unwrap_or_else(|| "it is no longer configured".into());
                    tracing::warn!(server = %name, reason = %why, "an editor's MCP server did not start");
                    notify(
                        connection,
                        session,
                        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                            TextContent::new(format!(
                                "MCP server {shown} did not start ({why}), so this prompt runs without its tools.\n\n"
                            )),
                        ))),
                    )?;
                    self.forget_mcp(&name, &key).await;
                }
            }
        }
        if !still.is_empty() {
            self.starting
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(key)
                .or_default()
                .extend(still);
        }
        Ok(cancelled)
    }

    /// Configure `name` and wait for its start for a moment at most: `true`
    /// when it is still starting then (a first `npx -y` download), which
    /// counts as configured for the session. Its start goes on on the
    /// server, and the session's next prompt waits for it. A server that
    /// needs authorization is configured too.
    /// `rejoin` when this process asks again for a server it had on the
    /// project's earlier server: the server refuses one turned off there
    /// since ([`TurnedOff`]).
    async fn start_mcp(
        &self,
        name: &str,
        config: &McpConfigureRequest,
        rejoin: bool,
    ) -> Result<bool> {
        let mut config = config.clone();
        let mut owner = self.owner.clone();
        owner.rejoin = rejoin;
        config.owner = Some(owner);
        match tokio::time::timeout(MCP_START_WAIT, self.client.configure_mcp(name, &config)).await {
            Ok(Ok(info)) if rejoin && info.status == "disabled" => Err(TurnedOff.into()),
            Ok(configured) => configured.map(|_| false),
            Err(_) => {
                let servers = self.client.mcp().await?;
                match servers.iter().find(|server| server.name == name) {
                    Some(server) if server.status == "connecting" => Ok(true),
                    Some(server)
                        if matches!(server.status.as_str(), "connected" | "needs_auth") =>
                    {
                        Ok(false)
                    }
                    Some(server) => anyhow::bail!(
                        "MCP server {name} did not start ({}{})",
                        server.status,
                        server
                            .detail
                            .as_deref()
                            .map(|detail| format!(": {detail}"))
                            .unwrap_or_default()
                    ),
                    None => anyhow::bail!("MCP server {name} was not configured"),
                }
            }
        }
    }

    /// The client's request for `asked`. The answer arrives through the
    /// returned future; dropping it sends `$/cancel_request`, as when the
    /// server settles the request first.
    fn ask(
        session: &SessionId,
        asked: &Asked,
        connection: &ConnectionTo<Client>,
        calls: &Calls,
    ) -> Answer {
        let payload = &asked.payload;
        let child = asked.child.as_ref();
        let operation = payload["operation"]
            .as_str()
            .unwrap_or("Approve tool use")
            .to_owned();
        // Why Auto's reviewer left it to the person, after the request.
        let operation = match serde_json::from_value::<haarness_protocol::PermissionReview>(
            payload["review"].clone(),
        ) {
            Ok(review) => format!("{operation} — {}", crate::review_note(&review)),
            Err(_) => operation,
        };
        let detail = payload.get("detail").cloned().unwrap_or(Value::Null);
        let call = asked_call(
            calls,
            child.map(|child| child.id),
            payload["callId"].as_str(),
            &detail,
        )
        .unwrap_or_else(|| asked.id.clone());
        let options = permission_options(payload);
        let mut fields = ToolCallUpdateFields::new()
            .title(child.map_or_else(|| operation.clone(), |child| child.titled(&operation)))
            .status(ToolCallStatus::Pending)
            .raw_input(detail);
        // The script the command runs, for the person answering; the call's
        // result replaces it once it runs.
        if let Some(text) = script_content(payload) {
            fields = fields.content(vec![ContentBlock::Text(TextContent::new(text)).into()]);
        }
        let request = RequestPermissionRequest::new(
            session.clone(),
            ToolCallUpdate::new(call, fields).meta(child.map(Child::meta)),
            options,
        )
        .meta(child.map(Child::meta));
        Box::pin(connection.send_request(request).block_task())
    }

    /// Passes the client's answer to the server. A request the server
    /// settled meanwhile (a Reject of a sibling rejects it too, an Always
    /// covers it, another app answered it) is gone: that is not an error.
    async fn answer(&self, id: &str, response: RequestPermissionResponse) -> AcpResult<()> {
        let decision = match response.outcome {
            RequestPermissionOutcome::Selected(selected) => match selected.option_id.0.as_ref() {
                "once" => PermissionReplyDecision::Once,
                "always" => PermissionReplyDecision::Always,
                _ => PermissionReplyDecision::Reject,
            },
            _ => PermissionReplyDecision::Reject,
        };
        match self
            .client
            .reply_permission(
                id,
                PermissionReply {
                    decision,
                    feedback: None,
                },
            )
            .await
        {
            Err(error)
                if haarness_tui::api::refusal_status(&error)
                    == Some(reqwest::StatusCode::NOT_FOUND) =>
            {
                Ok(())
            }
            result => result.map_err(internal),
        }
    }

    /// Reads the requests the server holds for the session's chats: those
    /// not taken yet wait to be shown, and those taken but no longer held
    /// (settled while events were missed) are dropped. A server that cannot
    /// be read leaves the queue as it is.
    async fn refresh_pending(
        &self,
        thread: ThreadId,
        approvals: &mut Approvals,
        children: &mut HashMap<ThreadId, Child>,
    ) {
        let Ok(pending) = self.client.pending_permissions().await else {
            return;
        };
        let mut held = Vec::new();
        for info in pending {
            if info.thread_id != thread && info.root_thread_id != Some(thread) {
                continue;
            }
            let child = if info.thread_id == thread {
                None
            } else {
                Some(self.child(info.thread_id, children).await)
            };
            let Ok(payload) = serde_json::to_value(&info) else {
                continue;
            };
            held.push(Asked {
                id: info.permission_id,
                payload,
                child,
            });
        }
        approvals.refresh(held);
    }

    /// Shows the oldest waiting request when none is up, skipping those the
    /// server no longer holds.
    async fn show_next(
        &self,
        approvals: &mut Approvals,
        session: &SessionId,
        connection: &ConnectionTo<Client>,
        calls: &Calls,
    ) {
        while approvals.shown.is_none()
            && let Some(asked) = approvals.waiting.pop_front()
        {
            if approvals.settled.contains(&asked.id) {
                continue;
            }
            // A sibling answered a moment ago may have settled this one before
            // its `permission.resolved` arrives here.
            let pending = self
                .client
                .pending_permissions()
                .await
                .map_or(true, |pending| {
                    pending.iter().any(|info| info.permission_id == asked.id)
                });
            if !pending {
                approvals.settled.insert(asked.id);
                continue;
            }
            let answer = Self::ask(session, &asked, connection, calls);
            approvals.shown = Some((asked.id, answer));
        }
    }

    /// What a chat started under the session is called, looked up once.
    /// A subagent is named after its task's `description` (see
    /// [`name_child`]), read from the chat that launched it when this prompt
    /// did not see the launch; otherwise after its chat title.
    async fn child(&self, id: ThreadId, children: &mut HashMap<ThreadId, Child>) -> Child {
        if let Some(child) = children.get(&id) {
            return child.clone();
        }
        let thread = self.client.thread(id).await.ok();
        let mut title = thread
            .as_ref()
            .map(|thread| thread.title.clone())
            .unwrap_or_default();
        if let Some(parent) = thread.and_then(|thread| thread.parent_id)
            && let Ok(items) = self.client.items(parent).await
        {
            let launch = items.iter().find_map(|item| match &item.data {
                ItemData::TaskLink {
                    call_id,
                    child_thread_id,
                }
                | ItemData::TaskState {
                    call_id,
                    child_thread_id,
                    ..
                } if *child_thread_id == id => Some(call_id.clone()),
                _ => None,
            });
            let description = items.iter().find_map(|item| match &item.data {
                ItemData::ToolCall {
                    call_id, arguments, ..
                } if Some(call_id) == launch.as_ref() => arguments["description"]
                    .as_str()
                    .filter(|description| !description.trim().is_empty())
                    .map(str::to_owned),
                _ => None,
            });
            if let Some(description) = description {
                title = description;
            }
        }
        let child = Child::new(id, &title);
        children.insert(id, child.clone());
        child
    }

    /// A child's tool calls and their results, on the session's own updates:
    /// the editor sees the work done under its session and which child did it.
    /// Its plan and messages stay its own.
    fn child_item(
        session: &SessionId,
        item: Item,
        child: &Child,
        connection: &ConnectionTo<Client>,
        calls: &mut Calls,
        files: &mut Files,
    ) -> AcpResult<()> {
        match item.data {
            ItemData::ToolCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                let call_id = child.call(&call_id);
                calls.insert(call_id.clone(), (Some(child.id), arguments.clone()));
                notify(
                    connection,
                    session,
                    SessionUpdate::ToolCall(
                        ToolCall::new(call_id, child.titled(&title(&name, &arguments)))
                            .kind(tool_kind(&name))
                            .status(ToolCallStatus::InProgress)
                            .locations(files.locations(&name, &arguments))
                            .raw_input(arguments)
                            .meta(child.meta()),
                    ),
                )
            }
            ItemData::ToolResult {
                call_id,
                name,
                output,
                is_error,
                metadata,
            } => {
                let call_id = child.call(&call_id);
                let locations = calls
                    .get(&call_id)
                    .map(|(_, arguments)| files.locations(&name, arguments))
                    .unwrap_or_default();
                let diffs = files.diffs(&name, &metadata);
                notify(
                    connection,
                    session,
                    SessionUpdate::ToolCallUpdate(
                        result_update(call_id, &output, is_error, &metadata, locations, diffs)
                            .meta(child.meta()),
                    ),
                )
            }
            _ => Ok(()),
        }
    }

    fn live_item(
        session: &SessionId,
        item: Item,
        connection: &ConnectionTo<Client>,
        calls: &mut Calls,
        files: &mut Files,
    ) -> AcpResult<()> {
        match item.data {
            ItemData::ToolCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                calls.insert(call_id.clone(), (None, arguments.clone()));
                notify(
                    connection,
                    session,
                    SessionUpdate::ToolCall(
                        ToolCall::new(call_id.clone(), title(&name, &arguments))
                            .kind(tool_kind(&name))
                            .status(ToolCallStatus::InProgress)
                            .locations(files.locations(&name, &arguments))
                            .raw_input(arguments),
                    ),
                )
            }
            ItemData::ToolResult {
                call_id,
                name,
                output,
                is_error,
                metadata,
            } => {
                let locations = calls
                    .get(&call_id)
                    .map(|(_, arguments)| files.locations(&name, arguments))
                    .unwrap_or_default();
                let diffs = files.diffs(&name, &metadata);
                notify(
                    connection,
                    session,
                    SessionUpdate::ToolCallUpdate(result_update(
                        call_id, &output, is_error, &metadata, locations, diffs,
                    )),
                )
            }
            ItemData::Todo { items } => notify(
                connection,
                session,
                SessionUpdate::Plan(Plan::new(
                    items
                        .into_iter()
                        .map(|item| {
                            PlanEntry::new(
                                item.content,
                                match item.priority.as_str() {
                                    "high" => PlanEntryPriority::High,
                                    "low" => PlanEntryPriority::Low,
                                    _ => PlanEntryPriority::Medium,
                                },
                                match item.status.as_str() {
                                    "in_progress" => PlanEntryStatus::InProgress,
                                    "completed" | "cancelled" => PlanEntryStatus::Completed,
                                    _ => PlanEntryStatus::Pending,
                                },
                            )
                        })
                        .collect(),
                )),
            ),
            ItemData::Error { message, .. } => notify(
                connection,
                session,
                SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                    TextContent::new(message),
                ))),
            ),
            _ => Ok(()),
        }
    }

    /// One stored item of the session's chat, as it was shown live. `calls`
    /// holds the locations of the calls replayed whose results have not
    /// come yet; `diffs` are the result's (see [`Files::replayed_diffs`]).
    fn replay(
        session: &SessionId,
        item: Item,
        files: &Files,
        calls: &mut HashMap<String, Vec<ToolCallLocation>>,
        diffs: Vec<ToolCallContent>,
        connection: &ConnectionTo<Client>,
    ) -> AcpResult<()> {
        let update = match item.data {
            ItemData::UserMessage {
                text,
                synthetic: false,
                ..
            } => Some(SessionUpdate::UserMessageChunk(ContentChunk::new(
                ContentBlock::Text(TextContent::new(text)),
            ))),
            ItemData::AssistantMessage { text } => Some(SessionUpdate::AgentMessageChunk(
                ContentChunk::new(ContentBlock::Text(TextContent::new(text))),
            )),
            ItemData::Reasoning { text, .. } => Some(SessionUpdate::AgentThoughtChunk(
                ContentChunk::new(ContentBlock::Text(TextContent::new(text))),
            )),
            ItemData::ToolCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                let locations = files.locations(&name, &arguments);
                calls.insert(call_id.clone(), locations.clone());
                Some(SessionUpdate::ToolCall(
                    ToolCall::new(call_id, title(&name, &arguments))
                        .kind(tool_kind(&name))
                        .status(ToolCallStatus::InProgress)
                        .locations(locations)
                        .raw_input(arguments),
                ))
            }
            ItemData::ToolResult {
                call_id,
                output,
                is_error,
                metadata,
                ..
            } => {
                let locations = calls.remove(&call_id).unwrap_or_default();
                Some(SessionUpdate::ToolCallUpdate(result_update(
                    call_id, &output, is_error, &metadata, locations, diffs,
                )))
            }
            _ => None,
        };
        if let Some(update) = update {
            notify(connection, session, update)?;
        }
        Ok(())
    }
}

/// The call a request is about, in the chat that asks (`None` for the
/// session's own): the one the server names (`named`; a batch's step,
/// `<call>:N`, is its batch's call here), or, from a server that names none,
/// the call with the request's input. Two children can make the same call.
fn asked_call(
    calls: &Calls,
    chat: Option<ThreadId>,
    named: Option<&str>,
    detail: &Value,
) -> Option<String> {
    let key = |call: &str| chat.map_or_else(|| call.to_owned(), |chat| format!("{call}@{chat}"));
    let batch = named.and_then(|call| {
        call.rsplit_once(':')
            .filter(|(_, step)| !step.is_empty() && step.bytes().all(|byte| byte.is_ascii_digit()))
            .map(|(batch, _)| batch)
    });
    [named, batch]
        .into_iter()
        .flatten()
        .map(key)
        .find(|call| calls.contains_key(call))
        .or_else(|| {
            calls
                .iter()
                .find(|(_, (owner, input))| *owner == chat && input == detail)
                .map(|(call, _)| call.clone())
        })
}

/// The script a shell request's command runs (`script` on the request), as
/// Markdown: its path, then its text in a fence longer than any run of
/// backticks in it.
fn script_content(payload: &Value) -> Option<String> {
    let script = &payload["script"];
    let path = script["path"].as_str()?;
    let content = script["content"].as_str()?;
    // The longest run of backticks in `text`: a fence or code span one
    // longer cannot be closed from inside.
    let longest = |text: &str| {
        let (mut longest, mut run) = (0, 0);
        for character in text.chars() {
            run = if character == '`' { run + 1 } else { 0 };
            longest = longest.max(run);
        }
        longest
    };
    let span = "`".repeat(longest(path) + 1);
    // Its text may not be what runs: one quiet line says why.
    match script["withheld"].as_str() {
        Some("written") => {
            return Some(format!(
                "This command writes {span} {path} {span} before running it, so its contents can't be shown."
            ));
        }
        Some(_) => {
            return Some(format!(
                "An earlier step of this command can change {span} {path} {span}, so its contents aren't shown."
            ));
        }
        None => {}
    }
    let fence = "`".repeat(longest(content).max(2) + 1);
    let mut text = format!(
        "Script: {span} {path} {span}\n\n{fence}\n{}\n{fence}",
        content.strip_suffix('\n').unwrap_or(content)
    );
    if script["truncated"] == true {
        text.push_str("\n\nOnly the start of the file is shown.");
    }
    Some(text)
}

/// What the client may answer. A request that may only be allowed once
/// (`allowAlways: false`) offers nothing longer.
fn permission_options(payload: &Value) -> Vec<PermissionOption> {
    let mut options = vec![PermissionOption::new(
        "once",
        "Allow once",
        PermissionOptionKind::AllowOnce,
    )];
    if payload["allowAlways"] != false {
        options.push(PermissionOption::new(
            "always",
            // A site in the user's Chrome is approved for this chat only.
            if payload["explicit"] == true {
                "Allow in this chat"
            } else {
                "Allow while Haarness is running"
            },
            PermissionOptionKind::AllowAlways,
        ));
    }
    options.push(PermissionOption::new(
        "reject",
        "Reject",
        PermissionOptionKind::RejectOnce,
    ));
    options
}

/// Each tool call the client was shown, by the id it was shown under: the
/// chat that made it (none for the session's own) and its arguments.
type Calls = HashMap<String, (Option<ThreadId>, Value)>;

/// The client's answer to a request for approval.
type Answer = std::pin::Pin<
    Box<dyn std::future::Future<Output = AcpResult<RequestPermissionResponse>> + Send>,
>;

/// What the prompt loop waits on next.
enum Step {
    Answer(AcpResult<RequestPermissionResponse>),
    Event(Option<SubscriptionEvent>),
}

/// A request for approval, from the session's chat or one under it.
struct Asked {
    id: String,
    payload: Value,
    child: Option<Child>,
}

/// The one request the client is asked at a time and those waiting behind
/// it. A request the server settled is never shown, and is cancelled when
/// it already is.
#[derive(Default)]
struct Approvals {
    waiting: VecDeque<Asked>,
    shown: Option<(String, Answer)>,
    settled: HashSet<String>,
    taken: HashSet<String>,
}

impl Approvals {
    fn wait(&mut self, id: String, payload: Value, child: Option<Child>) {
        if self.settled.contains(&id) || !self.taken.insert(id.clone()) {
            return;
        }
        self.waiting.push_back(Asked { id, payload, child });
    }

    /// The requests the server holds now, read after events may have been
    /// missed: new ones wait, taken ones it no longer holds are settled.
    fn refresh(&mut self, held: Vec<Asked>) {
        let ids = held
            .iter()
            .map(|asked| asked.id.clone())
            .collect::<HashSet<_>>();
        let gone = self
            .taken
            .iter()
            .filter(|id| !ids.contains(*id) && !self.settled.contains(*id))
            .cloned()
            .collect::<Vec<_>>();
        for id in gone {
            self.settle(&id);
        }
        for asked in held {
            self.wait(asked.id, asked.payload, asked.child);
        }
    }

    fn settle(&mut self, id: &str) {
        self.settled.insert(id.to_owned());
        self.waiting.retain(|asked| asked.id != id);
        if self.shown.as_ref().is_some_and(|(shown, _)| shown == id) {
            // Dropping the answer sends the client `$/cancel_request`.
            self.shown = None;
        }
    }
}

/// A chat Haarness started under the client's session: a subagent or a
/// workflow agent. Its requests and tool calls reach the client on the
/// session, titled with its name and tagged with its id in `_meta`.
#[derive(Clone)]
struct Child {
    id: ThreadId,
    title: String,
    label: String,
}

impl Child {
    fn new(id: ThreadId, title: &str) -> Self {
        let title = if title.trim().is_empty() {
            "Subagent".to_owned()
        } else {
            title.trim().to_owned()
        };
        let label = if title.chars().count() > 40 {
            let mut label = title.chars().take(39).collect::<String>();
            label.push('…');
            label
        } else {
            title.clone()
        };
        Self { id, title, label }
    }

    /// A child's tool call id as the client sees it: providers number calls
    /// per chat, so a child's could repeat one of the session's own.
    fn call(&self, call_id: &str) -> String {
        format!("{call_id}@{}", self.id)
    }

    /// `label · title`, for clients that do not read `_meta`.
    fn titled(&self, title: &str) -> String {
        format!("{} · {title}", self.label)
    }

    fn meta(&self) -> serde_json::Map<String, Value> {
        let mut meta = serde_json::Map::new();
        meta.insert(
            "haarness".into(),
            json!({"childSessionId": self.id.to_string(), "childTitle": self.title}),
        );
        meta
    }
}

/// Names a subagent after the `description` of the `task` call that started
/// it, which says what it is for better than the chat title cut from its
/// prompt.
fn name_child(payload: &Value, calls: &Calls, children: &mut HashMap<ThreadId, Child>) {
    let Some(id) = payload["childThreadId"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        .map(ThreadId)
    else {
        return;
    };
    let Some(description) = payload["callId"]
        .as_str()
        .and_then(|call| calls.get(call))
        .and_then(|(_, arguments)| arguments["description"].as_str())
        .filter(|description| !description.trim().is_empty())
    else {
        return;
    };
    children.insert(id, Child::new(id, description));
}

fn notify(
    connection: &ConnectionTo<Client>,
    session: &SessionId,
    update: SessionUpdate,
) -> AcpResult<()> {
    connection.send_notification(SessionNotification::new(session.clone(), update))
}

fn thread_id(session: &SessionId) -> AcpResult<ThreadId> {
    Uuid::parse_str(&session.to_string())
        .map(ThreadId)
        .map_err(|_| invalid(format!("invalid Haarness ACP session id: {session}")))
}

fn finish(turn: Turn) -> AcpResult<PromptResponse> {
    match turn.status {
        TurnStatus::Completed => Ok(PromptResponse::new(StopReason::EndTurn)),
        TurnStatus::Interrupted => Ok(PromptResponse::new(StopReason::Cancelled)),
        TurnStatus::Failed => {
            let error = turn.error.unwrap_or_else(|| "Haarness turn failed".into());
            // A hook's block refuses the prompt, as the server refuses one
            // before a turn starts (see `refused`).
            match haarness_core::prompt_blocked(&error) {
                Some(refusal) => Err(invalid_request(refusal)),
                None => Err(internal(error)),
            }
        }
        TurnStatus::Queued | TurnStatus::Running => Err(internal(
            "Haarness emitted turn.finished for a non-terminal turn",
        )),
    }
}

fn prompt_content(blocks: Vec<ContentBlock>) -> AcpResult<(String, Vec<String>)> {
    let mut text = String::new();
    let mut attachments = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text(content) => text.push_str(&content.text),
            ContentBlock::Image(content) if content.data.trim().is_empty() => {
                // An image sent only by its address. One written as a data
                // URL is the image itself; any other address (a web page's,
                // a file's) reaches the model the way a resource link does,
                // and Haarness never fetches it.
                let uri = content
                    .uri
                    .as_deref()
                    .map(str::trim)
                    .filter(|uri| !uri.is_empty())
                    .ok_or_else(|| invalid("an ACP image needs its data or a uri"))?;
                if let Some(rest) = uri
                    .get(..5)
                    .filter(|scheme| scheme.eq_ignore_ascii_case("data:"))
                    .and_then(|_| uri.get(5..))
                {
                    // Attachments are read by their scheme in lowercase.
                    attachments.push(format!("data:{rest}"));
                } else {
                    let _ = write!(
                        text,
                        "\n\n<resource-link name={:?} uri={uri:?} />",
                        image_link_name(uri)
                    );
                }
            }
            ContentBlock::Image(content) => attachments.push(format!(
                "data:{};base64,{}",
                content.mime_type, content.data
            )),
            ContentBlock::Audio(content) => attachments.push(format!(
                "data:{};base64,{}",
                content.mime_type, content.data
            )),
            ContentBlock::ResourceLink(resource) => {
                let _ = write!(
                    text,
                    "\n\n<resource-link name={:?} uri={:?} />",
                    resource.name, resource.uri
                );
            }
            ContentBlock::Resource(resource) => match resource.resource {
                EmbeddedResourceResource::TextResourceContents(resource) => {
                    let _ = write!(
                        text,
                        "\n\n<resource uri={:?}>\n{}\n</resource>",
                        resource.uri, resource.text
                    );
                }
                EmbeddedResourceResource::BlobResourceContents(resource) => {
                    let mime = resource.mime_type.ok_or_else(|| {
                        invalid("embedded binary ACP resources require a MIME type")
                    })?;
                    if !mime.starts_with("image/")
                        && !mime.starts_with("video/")
                        && !mime.starts_with("audio/")
                        && mime != "application/pdf"
                    {
                        return Err(invalid(format!(
                            "unsupported embedded ACP resource MIME type {mime}"
                        )));
                    }
                    attachments.push(format!("data:{mime};base64,{}", resource.blob));
                }
                _ => return Err(invalid("unsupported embedded ACP resource")),
            },
            _ => return Err(invalid("unsupported ACP prompt content block")),
        }
    }
    Ok((text, attachments))
}

/// The file name at the end of an image's address (`chart.png` for
/// `https://example.com/a/chart.png?v=2`), or `image` when it has none.
fn image_link_name(uri: &str) -> String {
    let path = url::Url::parse(uri).map_or_else(
        |_| uri.split(['?', '#']).next().unwrap_or_default().to_owned(),
        |url| url.path().to_owned(),
    );
    path.rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("image")
        .to_owned()
}

fn mcp_config(server: McpServer, cwd: &Path) -> AcpResult<(String, McpConfigureRequest)> {
    let (name, transport, command, args, env, url, headers) = match server {
        McpServer::Stdio(server) => {
            if !server.command.is_absolute() {
                return Err(invalid(format!(
                    "ACP stdio MCP command must be absolute: {}",
                    server.command.display()
                )));
            }
            (
                server.name,
                "stdio",
                Some(server.command.display().to_string()),
                server.args,
                server
                    .env
                    .into_iter()
                    .map(|entry| (entry.name, entry.value))
                    .collect(),
                None,
                BTreeMap::new(),
            )
        }
        McpServer::Http(server) => (
            server.name,
            "streamable_http",
            None,
            Vec::new(),
            BTreeMap::new(),
            Some(server.url),
            server
                .headers
                .into_iter()
                .map(|entry| (entry.name, entry.value))
                .collect(),
        ),
        McpServer::Sse(server) => (
            server.name,
            "sse",
            None,
            Vec::new(),
            BTreeMap::new(),
            Some(server.url),
            server
                .headers
                .into_iter()
                .map(|entry| (entry.name, entry.value))
                .collect(),
        ),
        _ => return Err(invalid("unsupported ACP MCP transport")),
    };
    Ok((
        slug(&name),
        McpConfigureRequest {
            transport: transport.into(),
            command,
            args,
            env,
            cwd: Some(cwd.display().to_string()),
            url,
            headers,
            headers_helper: None,
            timeout_ms: None,
            oauth: Some(false),
            oauth_client_id: None,
            oauth_scopes: Vec::new(),
            enabled: true,
            apps: None,
            owner: None,
        },
    ))
}

fn slug(name: &str) -> String {
    let slug = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if slug.trim_matches('-').is_empty() {
        "server".into()
    } else {
        slug
    }
}

fn title(name: &str, arguments: &Value) -> String {
    match name {
        "bash" => arguments["command"]
            .as_str()
            .map_or_else(|| "Run command".into(), |command| format!("Run {command}")),
        "bash_output" => arguments["id"].as_str().map_or_else(
            || "List commands".into(),
            |id| format!("Read the output of {id}"),
        ),
        "bash_stop" => arguments["id"]
            .as_str()
            .map_or_else(|| "Stop command".into(), |id| format!("Stop {id}")),
        "read" => arguments["path"]
            .as_str()
            .map_or_else(|| "Read file".into(), |path| format!("Read {path}")),
        "write" | "edit" | "multiedit" => arguments["path"]
            .as_str()
            .map_or_else(|| format!("Use {name}"), |path| format!("Edit {path}")),
        // The patch names its files; the first is shown, and how many more.
        "apply_patch" => {
            let mut files = Vec::new();
            for (file, _) in patch_files(arguments) {
                if !files.contains(&file) {
                    files.push(file);
                }
            }
            match files.as_slice() {
                [] => format!("Use {name}"),
                [file] => format!("Edit {file}"),
                [file, rest @ ..] => format!("Edit {file} and {} more", rest.len()),
            }
        }
        _ => format!("Use {name}"),
    }
}

fn tool_kind(name: &str) -> ToolKind {
    match name {
        "read" | "list" | "bash_output" => ToolKind::Read,
        "write" | "edit" | "multiedit" | "apply_patch" => ToolKind::Edit,
        "glob" | "grep" | "memory_search" | "session_search" => ToolKind::Search,
        "bash" | "execute" | "bash_stop" => ToolKind::Execute,
        "webfetch" => ToolKind::Fetch,
        "todowrite" | "task" | "goal" => ToolKind::Think,
        _ => ToolKind::Other,
    }
}

/// What the editor shows for a tool result, and its raw output. A call Auto's
/// reviewer refused shows its reason and the reviewer's note, never the text
/// written for the model.
fn shown_result(output: &str, metadata: &Value) -> (String, Value) {
    match metadata
        .get("review")
        .filter(|review| review["status"] == "blocked")
    {
        Some(review) => (
            match review["rationale"].as_str() {
                Some(reason) => format!("Stopped by Auto's reviewer: {reason}"),
                None => "Stopped by Auto's reviewer".to_owned(),
            },
            json!({"review": review}),
        ),
        None => (output.to_owned(), parse_output(output)),
    }
}

/// A tool result, live or replayed: its text, then the diffs of the files
/// it changed, and the files it worked on. A call with none keeps the
/// locations it was shown with.
fn result_update(
    call_id: String,
    output: &str,
    is_error: bool,
    metadata: &Value,
    locations: Vec<ToolCallLocation>,
    diffs: Vec<ToolCallContent>,
) -> ToolCallUpdate {
    let (shown, raw) = shown_result(output, metadata);
    let mut content = vec![ContentBlock::Text(TextContent::new(shown)).into()];
    content.extend(diffs);
    let mut fields = ToolCallUpdateFields::new()
        .status(if is_error {
            ToolCallStatus::Failed
        } else {
            ToolCallStatus::Completed
        })
        .content(content)
        .raw_output(raw);
    if !locations.is_empty() {
        fields = fields.locations(locations);
    }
    ToolCallUpdate::new(call_id, fields)
}

/// What a prompt or a load tells the editor about the files its calls work
/// on: the chat's folder, which relative paths are read from, how the
/// editor spells the workspace, and how much file text its diffs may still
/// carry (`DIFF_TEXT_LIMIT` in all).
struct Files {
    folder: PathBuf,
    /// The workspace as Haarness names it and as the editor opened it: a
    /// path in it is given the editor's spelling, so the editor finds it in
    /// its own project.
    spelling: Option<(PathBuf, PathBuf)>,
    left: usize,
}

impl Files {
    fn new(folder: PathBuf, spelling: Option<(PathBuf, PathBuf)>) -> Self {
        Self {
            folder,
            spelling,
            left: DIFF_TEXT_LIMIT,
        }
    }

    /// `path` as the file tools read it (see [`absolute`]), spelled as the
    /// editor spells the workspace when it lies in it.
    fn path(&self, path: &str) -> PathBuf {
        let path = absolute(&self.folder, path);
        let Some((workspace, opened)) = &self.spelling else {
            return path;
        };
        match path.strip_prefix(workspace) {
            Ok(rest) if rest.as_os_str().is_empty() => opened.clone(),
            Ok(rest) => opened.join(rest),
            Err(_) => path,
        }
    }

    /// The files and folders a call works on, for the editor to open and to
    /// follow (see [`named_paths`]), each once.
    fn locations(&self, name: &str, arguments: &Value) -> Vec<ToolCallLocation> {
        let mut seen = HashSet::new();
        named_paths(name, arguments)
            .iter()
            .map(|path| self.path(path))
            .filter(|path| seen.insert(path.clone()))
            .map(ToolCallLocation::new)
            .collect()
    }

    /// The diffs of a result whose `metadata.diffs` holds the before and
    /// after texts of the files it changed (`write`, `edit`, `multiedit`,
    /// `apply_patch`, and a `batch` for each of its steps), at absolute
    /// paths: a new file has no old text and a deleted one an empty new
    /// text. A binary file or a text the server cut short has none, nor has
    /// a diff larger than what is left to send; the result's text names
    /// those files still.
    fn diffs(&mut self, name: &str, metadata: &Value) -> Vec<ToolCallContent> {
        let steps = if name == "batch" {
            metadata.get("results").and_then(Value::as_array)
        } else {
            None
        };
        let recorded = std::iter::once(metadata)
            .chain(steps.into_iter().flatten().map(|step| &step["metadata"]))
            .filter_map(|metadata| metadata.get("diffs").and_then(Value::as_array))
            .flatten();
        let mut diffs = Vec::new();
        for entry in recorded {
            if self.left == 0 {
                break;
            }
            if let Some(diff) = self.diff(entry) {
                diffs.push(diff.into());
            }
        }
        diffs
    }

    /// One recorded change (a `FileDiff`) as a diff, when it holds its whole
    /// texts and they fit in what is left. Its size is known before any text
    /// is copied.
    fn diff(&mut self, entry: &Value) -> Option<Diff> {
        if entry["binary"] == true || entry["truncated"] == true {
            return None;
        }
        let file = entry["file"].as_str()?;
        let (before, after) = (entry["before"].as_str(), entry["after"].as_str());
        let (old, new) = match (entry["status"].as_str()?, before, after) {
            ("added", _, Some(after)) => (None, after),
            ("modified", Some(before), Some(after)) => (Some(before), after),
            ("deleted", Some(before), _) => (Some(before), ""),
            _ => return None,
        };
        let size = old.map_or(0, str::len) + new.len();
        if size > self.left {
            return None;
        }
        self.left -= size;
        Some(Diff::new(self.path(file), new).old_text(old.map(str::to_owned)))
    }

    /// The diffs of each tool result of a replayed chat, by its index in
    /// `items`. They are taken from the newest result back, so when the
    /// chat holds more than one load sends, its latest changes show.
    fn replayed_diffs(&mut self, items: &[Item]) -> HashMap<usize, Vec<ToolCallContent>> {
        let mut diffs = HashMap::new();
        for (index, item) in items.iter().enumerate().rev() {
            if self.left == 0 {
                break;
            }
            if let ItemData::ToolResult { name, metadata, .. } = &item.data {
                let found = self.diffs(name, metadata);
                if !found.is_empty() {
                    diffs.insert(index, found);
                }
            }
        }
        diffs
    }
}

/// The paths a call names as it wrote them: the file a file tool reads or
/// changes, or `lsp` asks about; every file a patch names (both ends of a
/// move); the folder a search or a listing looks in (`.` when it names
/// none); and those of each step of a `batch`. Other tools name none.
fn named_paths(name: &str, arguments: &Value) -> Vec<String> {
    let named = |key: &str| {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
    };
    match name {
        "read" | "write" | "edit" | "multiedit" => {
            named("path").into_iter().map(str::to_owned).collect()
        }
        "lsp" => named("filePath").into_iter().map(str::to_owned).collect(),
        "apply_patch" => patch_files(arguments)
            .into_iter()
            .flat_map(|(file, target)| std::iter::once(file).chain(target))
            .collect(),
        "glob" | "grep" | "list" => vec![named("path").unwrap_or(".").to_owned()],
        "batch" => arguments
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|step| Some((step.get("tool")?.as_str()?, step.get("parameters")?)))
            // A step's tool is named as the server reads it: a `functions.`
            // prefix and a different case still name the tool.
            .map(|(tool, parameters)| {
                let tool = tool.strip_prefix("functions.").unwrap_or(tool);
                (tool.to_ascii_lowercase(), parameters)
            })
            // A batch cannot run a batch.
            .filter(|(tool, _)| tool != "batch")
            .flat_map(|(tool, parameters)| named_paths(&tool, parameters))
            .collect(),
        _ => Vec::new(),
    }
}

/// The files an `apply_patch` call's patch works on, in its order: each
/// operation's file and, for a move, the file it becomes. None when the
/// patch does not parse.
fn patch_files(arguments: &Value) -> Vec<(String, Option<String>)> {
    let Some(Ok(operations)) = arguments
        .get("input")
        .and_then(Value::as_str)
        .map(haarness_core::patch::parse_patch)
    else {
        return Vec::new();
    };
    operations
        .into_iter()
        .map(|operation| match operation {
            haarness_core::patch::FileOp::Update { path, move_to, .. } => (path, move_to),
            operation => (operation.path().to_owned(), None),
        })
        .collect()
}

/// `path` as the file tools read it: a leading `~` is the home folder and
/// a relative path is under `folder`, with `.` and `..` folded away.
fn absolute(folder: &Path, path: &str) -> PathBuf {
    fold(&folder.join(haarness_core::expand_home(path).as_ref()))
}

/// `path` with `.` and `..` folded away, as written: links are not followed.
fn fold(path: &Path) -> PathBuf {
    let mut folded = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                folded.pop();
            }
            component => folded.push(component.as_os_str()),
        }
    }
    folded
}

/// The chat's usage as the editor's meter shows it: the context number
/// every Haarness client shows, out of the model's window, and what the
/// chat and the subagents under it have cost (the priced part, when some
/// turns have no price). None when the model states no window, as the
/// apps show no meter then either.
fn usage_update(usage: &Value) -> Option<UsageUpdate> {
    let context = &usage["context"];
    let size = context["window"].as_u64().filter(|window| *window > 0)?;
    let used = context["tokens"].as_u64()?;
    let cost = [&usage["costUsd"], &usage["descendants"]["costUsd"]]
        .into_iter()
        .filter_map(Value::as_f64)
        .reduce(|total, cost| total + cost);
    Some(UsageUpdate::new(used, size).cost(cost.map(|amount| Cost::new(amount, "USD"))))
}

fn parse_output(output: &str) -> Value {
    serde_json::from_str(output).unwrap_or_else(|_| Value::String(output.into()))
}

type AcpResult<T> = std::result::Result<T, agent_client_protocol::Error>;

fn invalid(message: impl Into<String>) -> agent_client_protocol::Error {
    agent_client_protocol::Error::invalid_params().data(json!({"error": message.into()}))
}

fn internal(error: impl std::fmt::Display) -> agent_client_protocol::Error {
    agent_client_protocol::Error::internal_error().data(error.to_string())
}

/// A prompt the server refused before its turn started (a chat whose turn
/// another window or run started, an unknown agent) is an invalid request
/// whose message is the server's own sentence, which editors show; a failure
/// of anything else is an internal error. A hook that blocks the prompt
/// fails the turn instead, and `finish` refuses it the same way.
fn refused(error: &anyhow::Error) -> agent_client_protocol::Error {
    if !haarness_tui::api::refusal_status(error).is_some_and(|status| status.is_client_error()) {
        return internal(error);
    }
    invalid_request(&error.to_string())
}

/// An invalid request whose message is `message`.
fn invalid_request(message: &str) -> agent_client_protocol::Error {
    let mut refusal = agent_client_protocol::Error::invalid_request();
    message.clone_into(&mut refusal.message);
    refusal
}

/// The project's server answers no longer and could not be started again:
/// the editor shows why, in place of the protocol's own words.
fn unreachable(error: anyhow::Error) -> agent_client_protocol::Error {
    let mut failure = agent_client_protocol::Error::internal_error();
    failure.message = format!("{error:#}");
    failure
}

fn not_found(error: impl std::fmt::Display) -> agent_client_protocol::Error {
    agent_client_protocol::Error::resource_not_found(None).data(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The server's refusal reaches the editor as its sentence; a failure of
    /// the server or the connection stays an internal error.
    #[test]
    fn a_refused_prompt_is_an_invalid_request_with_the_servers_sentence() {
        let refusal = |status| {
            anyhow::Error::new(haarness_tui::api::ApiRefusal {
                status,
                code: Some("conflict".into()),
                message: "conflict: This chat already has a turn running.".into(),
                reason: None,
                path: None,
            })
        };
        let error = refused(&refusal(reqwest::StatusCode::CONFLICT));
        assert_eq!(
            error.code,
            agent_client_protocol::Error::invalid_request().code
        );
        assert_eq!(error.message, "This chat already has a turn running.");
        let error = refused(&refusal(reqwest::StatusCode::INTERNAL_SERVER_ERROR));
        assert_eq!(
            error.code,
            agent_client_protocol::Error::internal_error().code
        );
        let error = refused(&anyhow::anyhow!("connection refused"));
        assert_eq!(
            error.code,
            agent_client_protocol::Error::internal_error().code
        );
    }

    /// An editor's prompt says a person can stop its turn, so a command's
    /// `!` snippet in it runs past the shell deadline.
    #[tokio::test]
    async fn a_prompt_s_turn_says_a_person_can_stop_it() {
        let bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
        let seen = bodies.clone();
        let app = axum::Router::new().route(
            "/v1/threads/{id}/turns",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                seen.lock().unwrap().push(body);
                async { (axum::http::StatusCode::BAD_REQUEST, "fixture") }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let service = Service::new(Config {
            client: ApiClient::new(&url, "token").unwrap(),
            workspace: PathBuf::from("/fixture"),
            provider: None,
            model: None,
            agent: None,
            auto_approve: false,
        });
        let turn = service.prompt_turn("Run the tests".into(), Vec::new());
        let _ = service.client.start_turn(ThreadId::new(), turn).await;
        server.abort();
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0]["attended"], true, "{}", bodies[0]);
        assert_eq!(bodies[0]["text"], "Run the tests");
    }

    #[test]
    fn initialize_advertises_every_prompt_block_it_accepts() {
        let response = Service::initialize(InitializeRequest::new(ProtocolVersion::V1));
        let capabilities = response.agent_capabilities;
        let prompt = capabilities.prompt_capabilities;
        assert!(prompt.image && prompt.audio && prompt.embedded_context);
        // Closing a session lets its MCP servers go.
        assert!(capabilities.session_capabilities.close.is_some());
    }

    /// A project server that could not be reached or started again reaches
    /// the editor as the reason, not as the protocol's "Internal error".
    #[test]
    fn an_unreachable_server_tells_the_editor_why() {
        let error = unreachable(
            anyhow::anyhow!("the data folder is read-only")
                .context("Haarness could not start the project's server again"),
        );
        assert_eq!(
            error.message,
            "Haarness could not start the project's server again: the data folder is read-only"
        );
    }

    #[test]
    fn converts_mixed_prompt_content() {
        let (text, attachments) = prompt_content(vec![
            ContentBlock::Text(TextContent::new("inspect")),
            ContentBlock::Resource(EmbeddedResource::new(
                EmbeddedResourceResource::TextResourceContents(TextResourceContents::new(
                    "fn main() {}",
                    "file:///tmp/main.rs",
                )),
            )),
            ContentBlock::Image(ImageContent::new("YWJj", "image/png")),
            ContentBlock::Audio(AudioContent::new("SUQz", "audio/mpeg")),
            ContentBlock::Resource(EmbeddedResource::new(
                EmbeddedResourceResource::BlobResourceContents(
                    agent_client_protocol::schema::v1::BlobResourceContents::new(
                        "JVBERi0xLjQ=",
                        "file:///tmp/report.pdf",
                    )
                    .mime_type("application/pdf"),
                ),
            )),
        ])
        .unwrap();
        assert!(text.contains("inspect"));
        assert!(text.contains("fn main() {}"));
        assert_eq!(
            attachments,
            [
                "data:image/png;base64,YWJj",
                "data:audio/mpeg;base64,SUQz",
                "data:application/pdf;base64,JVBERi0xLjQ="
            ]
        );
    }

    /// An image given only by its address reaches the model as a link to
    /// it, as a resource link does, instead of an empty image that refuses
    /// the whole prompt; an image with neither data nor an address is refused
    /// in words.
    #[test]
    fn an_image_given_only_by_its_address_becomes_a_link() {
        let (text, attachments) = prompt_content(vec![
            ContentBlock::Text(TextContent::new("compare these")),
            ContentBlock::Image(
                ImageContent::new("", "image/png").uri("https://example.com/a/chart.png?v=2"),
            ),
            ContentBlock::Image(ImageContent::new("", "image/png").uri("file:///tmp/shot.png")),
            ContentBlock::Image(ImageContent::new("", "image/png").uri("https://example.com/")),
            ContentBlock::Image(
                ImageContent::new("", "image/png").uri("DATA:image/png;base64,YWJj"),
            ),
            ContentBlock::Image(
                ImageContent::new("YWJj", "image/png").uri("https://example.com/x.png"),
            ),
        ])
        .unwrap();
        assert_eq!(
            text,
            "compare these\
             \n\n<resource-link name=\"chart.png\" uri=\"https://example.com/a/chart.png?v=2\" />\
             \n\n<resource-link name=\"shot.png\" uri=\"file:///tmp/shot.png\" />\
             \n\n<resource-link name=\"image\" uri=\"https://example.com/\" />"
        );
        assert_eq!(
            attachments,
            ["data:image/png;base64,YWJj", "data:image/png;base64,YWJj"]
        );
        for image in [
            ImageContent::new("", "image/png"),
            ImageContent::new(" ", "image/png").uri(" "),
        ] {
            let error = prompt_content(vec![ContentBlock::Image(image)]).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("an ACP image needs its data or a uri"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_request_points_at_the_call_of_the_chat_that_asks() {
        let child = ThreadId(Uuid::now_v7());
        let other = ThreadId(Uuid::now_v7());
        let same = json!({"path": "notes.md", "content": "x"});
        let calls = Calls::from([
            ("call-1".to_owned(), (None, json!({"path": "own.md"}))),
            (format!("call-1@{other}"), (Some(other), same.clone())),
            (format!("call-1@{child}"), (Some(child), same.clone())),
        ]);
        assert_eq!(
            asked_call(&calls, Some(child), None, &same),
            Some(format!("call-1@{child}"))
        );
        assert_eq!(asked_call(&calls, None, None, &same), None);
        assert_eq!(
            asked_call(&calls, None, None, &json!({"path": "own.md"})).as_deref(),
            Some("call-1")
        );
    }

    /// A server that names the asking call is believed over a call with the
    /// same input; a batch's step points at its batch.
    #[test]
    fn a_request_points_at_the_call_the_server_names() {
        let child = ThreadId(Uuid::now_v7());
        let same = json!({"path": "notes.md"});
        let calls = Calls::from([
            ("call-1".to_owned(), (None, same.clone())),
            ("call-2".to_owned(), (None, same.clone())),
            ("call-3".to_owned(), (None, json!({"tool_calls": []}))),
            (format!("call-2@{child}"), (Some(child), same.clone())),
        ]);
        assert_eq!(
            asked_call(&calls, None, Some("call-2"), &same).as_deref(),
            Some("call-2")
        );
        assert_eq!(
            asked_call(&calls, None, Some("call-3:2"), &same).as_deref(),
            Some("call-3")
        );
        assert_eq!(
            asked_call(&calls, Some(child), Some("call-2"), &same),
            Some(format!("call-2@{child}"))
        );
        // A call this client never saw: the input decides, as before.
        assert_eq!(
            asked_call(&calls, Some(child), Some("call-9"), &same),
            Some(format!("call-2@{child}"))
        );
    }

    #[test]
    fn a_refresh_after_missed_events_asks_new_requests_and_drops_settled_ones() {
        let asked = |id: &str| Asked {
            id: id.to_owned(),
            payload: json!({"permissionId": id}),
            child: None,
        };
        let mut approvals = Approvals::default();
        approvals.wait("shown".into(), json!({}), None);
        approvals.wait("waiting".into(), json!({}), None);
        let shown = approvals.waiting.pop_front().unwrap();
        approvals.shown = Some((shown.id, Box::pin(std::future::pending())));
        // `shown` was settled and `missed` raised while events were lost.
        approvals.refresh(vec![asked("waiting"), asked("missed")]);
        assert!(approvals.shown.is_none(), "a settled request is withdrawn");
        assert!(approvals.settled.contains("shown"));
        assert_eq!(
            approvals
                .waiting
                .iter()
                .map(|asked| asked.id.as_str())
                .collect::<Vec<_>>(),
            ["waiting", "missed"]
        );
        // A second read changes nothing.
        approvals.refresh(vec![asked("waiting"), asked("missed")]);
        assert_eq!(approvals.waiting.len(), 2);
    }

    #[test]
    fn a_shell_request_shows_the_script_its_command_runs() {
        assert_eq!(script_content(&json!({"detail": {"command": "ls"}})), None);
        // Neither can a backtick in the path close its code span.
        assert_eq!(
            script_content(&json!({"script": {"path": "/work/a``b.sh", "content": "x"}}))
                .unwrap()
                .lines()
                .next(),
            Some("Script: ``` /work/a``b.sh ```")
        );
        assert_eq!(
            script_content(&json!({"script": {
                "path": "/work/x.sh", "content": "echo hi\n", "truncated": false,
            }}))
            .as_deref(),
            Some("Script: ` /work/x.sh `\n\n```\necho hi\n```")
        );
        // A fence inside the script cannot close the block early.
        assert_eq!(
            script_content(&json!({"script": {
                "path": "/work/x.sh", "content": "cat <<'EOF'\n````\nEOF", "truncated": true,
            }}))
            .as_deref(),
            Some(
                "Script: ` /work/x.sh `\n\n`````\ncat <<'EOF'\n````\nEOF\n`````\n\nOnly the start of the file is shown."
            )
        );
    }

    #[test]
    fn a_script_whose_text_may_not_be_what_runs_is_one_line() {
        assert_eq!(
            script_content(&json!({"script": {
                "path": "/work/run.sh", "content": "", "withheld": "changeable",
            }}))
            .as_deref(),
            Some(
                "An earlier step of this command can change ` /work/run.sh `, so its contents aren't shown."
            )
        );
        assert_eq!(
            script_content(&json!({"script": {
                "path": "/work/gen.sh", "content": "", "withheld": "written",
            }}))
            .as_deref(),
            Some(
                "This command writes ` /work/gen.sh ` before running it, so its contents can't be shown."
            )
        );
    }

    #[test]
    fn a_request_that_may_be_allowed_only_once_offers_nothing_longer() {
        let ids = |payload: Value| {
            permission_options(&payload)
                .into_iter()
                .map(|option| option.option_id.0.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(json!({"allowAlways": false})), ["once", "reject"]);
        assert_eq!(
            ids(json!({"allowAlways": true})),
            ["once", "always", "reject"]
        );
        assert_eq!(ids(json!({})), ["once", "always", "reject"]);
    }

    #[test]
    fn maps_tool_updates_for_editor_rendering() {
        assert_eq!(tool_kind("read"), ToolKind::Read);
        assert_eq!(tool_kind("apply_patch"), ToolKind::Edit);
        assert_eq!(tool_kind("bash"), ToolKind::Execute);
        assert_eq!(tool_kind("bash_output"), ToolKind::Read);
        assert_eq!(tool_kind("bash_stop"), ToolKind::Execute);
        assert_eq!(
            title("read", &json!({"path":"src/main.rs"})),
            "Read src/main.rs"
        );
    }

    /// A patch call is titled after the file it changes, as an edit is,
    /// rather than `Use apply_patch`.
    #[test]
    fn a_patch_call_is_titled_after_its_files() {
        let patch =
            |body: &str| json!({"input": format!("*** Begin Patch\n{body}*** End Patch\n")});
        assert_eq!(
            title(
                "apply_patch",
                &patch("*** Update File: src/lib.rs\n@@\n-old\n+new\n")
            ),
            "Edit src/lib.rs"
        );
        assert_eq!(
            title(
                "apply_patch",
                &patch("*** Update File: src/old.rs\n*** Move to: src/new.rs\n@@\n-a\n+b\n")
            ),
            "Edit src/old.rs"
        );
        assert_eq!(
            title(
                "apply_patch",
                &patch(
                    "*** Add File: notes.md\n+hi\n*** Update File: notes.md\n@@\n-hi\n+ho\n*** Delete File: gone.txt\n*** Add File: b.txt\n+b\n"
                )
            ),
            "Edit notes.md and 2 more"
        );
        assert_eq!(
            title("apply_patch", &json!({"input": "not a patch"})),
            "Use apply_patch"
        );
        assert_eq!(title("apply_patch", &json!({})), "Use apply_patch");
        assert_eq!(title("edit", &json!({"path":"a.rs"})), "Edit a.rs");
    }

    /// The locations of a call in a chat working in `/work/app`.
    fn located(name: &str, arguments: &Value) -> Vec<PathBuf> {
        Files::new(PathBuf::from("/work/app"), None)
            .locations(name, arguments)
            .into_iter()
            .map(|location| location.path)
            .collect()
    }

    #[test]
    fn calls_name_the_files_they_work_on_as_absolute_paths() {
        let paths = |name: &str, arguments: Value| located(name, &arguments);
        let path = PathBuf::from;
        assert_eq!(
            paths("read", json!({"path":"src/main.rs", "offset": 10})),
            [path("/work/app/src/main.rs")]
        );
        assert_eq!(
            paths("write", json!({"path":"/tmp/out.txt", "content":"x"})),
            [path("/tmp/out.txt")]
        );
        assert_eq!(
            paths("edit", json!({"path":"./docs/../README.md"})),
            [path("/work/app/README.md")]
        );
        assert_eq!(
            paths("multiedit", json!({"path":"a.rs", "edits": []})),
            [path("/work/app/a.rs")]
        );
        assert_eq!(
            paths(
                "lsp",
                json!({"operation":"hover", "filePath":"src/lib.rs", "line":3, "character":1})
            ),
            [path("/work/app/src/lib.rs")]
        );
        // `~` is the home folder, as the file tools read it.
        let home = haarness_core::expand_home("~");
        if home != "~" {
            assert_eq!(
                paths("read", json!({"path":"~/notes.md"})),
                [Path::new(home.as_ref()).join("notes.md")]
            );
        }
        // Both ends of a move, each file once.
        let moved = "*** Begin Patch\n*** Add File: new.md\n+x\n*** Update File: src/old.rs\n*** Move to: src/new.rs\n@@\n-a\n+b\n*** Update File: new.md\n@@\n-x\n+y\n*** Delete File: /work/other/gone.txt\n*** End Patch\n";
        assert_eq!(
            paths("apply_patch", json!({"input": moved})),
            [
                path("/work/app/new.md"),
                path("/work/app/src/old.rs"),
                path("/work/app/src/new.rs"),
                path("/work/other/gone.txt"),
            ]
        );
        assert!(paths("apply_patch", json!({"input": "garbage"})).is_empty());
        // A search or a listing with no path looks in the chat's folder.
        assert_eq!(
            paths("glob", json!({"pattern":"*.rs"})),
            [path("/work/app")]
        );
        assert_eq!(
            paths("glob", json!({"pattern":"*.rs", "path": ""})),
            [path("/work/app")]
        );
        assert_eq!(
            paths("grep", json!({"pattern":"fn", "path":"src"})),
            [path("/work/app/src")]
        );
        assert_eq!(paths("list", json!({"path":"/etc"})), [path("/etc")]);
        assert!(paths("read", json!({"path": ""})).is_empty());
        assert!(paths("read", json!({})).is_empty());
        assert!(paths("bash", json!({"command":"ls", "workdir":"src"})).is_empty());
        assert!(paths("webfetch", json!({"url":"https://example.com"})).is_empty());
    }

    /// A batch works on the files its steps name.
    #[test]
    fn a_batch_names_the_files_of_its_steps() {
        let batch = json!({"tool_calls": [
            {"tool":"edit", "parameters":{"path":"a.rs", "old":"x", "new":"y"}},
            {"tool":"read", "parameters":{"path":"b.rs"}},
            {"tool":"edit", "parameters":{"path":"./a.rs", "old":"y", "new":"z"}},
            {"tool":"bash", "parameters":{"command":"ls"}},
            {"tool":"batch", "parameters":{"tool_calls":[{"tool":"read", "parameters":{"path":"c.rs"}}]}},
            {"tool":"functions.Batch", "parameters":{"tool_calls":[{"tool":"read", "parameters":{"path":"d.rs"}}]}},
            {"tool":"write"},
            // Named as a model may name it: the server still runs the tool.
            {"tool":"functions.edit", "parameters":{"path":"e.rs", "old":"x", "new":"y"}},
            {"tool":"Write", "parameters":{"path":"f.rs", "content":"x"}},
            {"tool":"functions.GLOB", "parameters":{"pattern":"*.rs", "path":"src"}},
        ]});
        assert_eq!(
            located("batch", &batch),
            [
                PathBuf::from("/work/app/a.rs"),
                PathBuf::from("/work/app/b.rs"),
                PathBuf::from("/work/app/e.rs"),
                PathBuf::from("/work/app/f.rs"),
                PathBuf::from("/work/app/src"),
            ]
        );
        assert!(located("batch", &json!({})).is_empty());
    }

    /// Paths in the workspace are spelled as the editor opened it (through
    /// a link, here), so it finds them in its own project; others stay as
    /// they are.
    #[test]
    fn paths_in_the_workspace_take_the_editor_s_spelling() {
        let mut files = Files::new(
            PathBuf::from("/private/var/work/app/sub"),
            Some((
                PathBuf::from("/private/var/work/app"),
                PathBuf::from("/var/work/app"),
            )),
        );
        let paths = |files: &Files, name: &str, arguments: Value| {
            files
                .locations(name, &arguments)
                .into_iter()
                .map(|location| location.path)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            paths(&files, "read", json!({"path":"src/main.rs"})),
            [PathBuf::from("/var/work/app/sub/src/main.rs")]
        );
        assert_eq!(
            paths(&files, "read", json!({"path":"/private/var/work/app/x.rs"})),
            [PathBuf::from("/var/work/app/x.rs")]
        );
        assert_eq!(
            paths(&files, "list", json!({"path":".."})),
            [PathBuf::from("/var/work/app")]
        );
        assert_eq!(
            paths(
                &files,
                "read",
                json!({"path":"/private/var/work/application"})
            ),
            [PathBuf::from("/private/var/work/application")]
        );
        assert_eq!(
            paths(&files, "write", json!({"path":"/tmp/out.txt"})),
            [PathBuf::from("/tmp/out.txt")]
        );
        let diffs = files.diffs(
            "write",
            &json!({"diffs":[{"file":"new.txt", "status":"added", "additions":1, "deletions":0, "after":"x"}]}),
        );
        assert_eq!(
            diff_parts(&diffs[0]).0,
            PathBuf::from("/var/work/app/sub/new.txt")
        );
    }

    fn diff_parts(content: &ToolCallContent) -> (PathBuf, Option<&str>, &str) {
        match content {
            ToolCallContent::Diff(diff) => (
                diff.path.clone(),
                diff.old_text.as_deref(),
                diff.new_text.as_str(),
            ),
            other => panic!("not a diff: {other:?}"),
        }
    }

    /// A change as the server records it in `metadata.diffs`.
    fn recorded(
        file: &str,
        status: haarness_protocol::FileDiffStatus,
        before: Option<&str>,
        after: Option<&str>,
    ) -> Value {
        serde_json::to_value(haarness_protocol::FileDiff {
            file: file.into(),
            status,
            additions: 1,
            deletions: 1,
            patch: None,
            before: before.map(str::to_owned),
            after: after.map(str::to_owned),
            binary: false,
            truncated: false,
        })
        .unwrap()
    }

    /// An edit's result carries the before and after texts the server
    /// recorded, as the editor's diffs.
    #[test]
    fn a_result_s_recorded_changes_become_diffs() {
        use haarness_protocol::FileDiffStatus::{Added, Deleted, Modified};
        let mut files = Files::new(PathBuf::from("/work/app"), None);
        let mut binary = recorded("logo.png", Modified, None, Some(""));
        binary["binary"] = json!(true);
        binary["truncated"] = json!(true);
        let mut cut = recorded("big.txt", Modified, Some("cut"), Some("cut"));
        cut["truncated"] = json!(true);
        let metadata = json!({"diffs": [
            recorded("new.txt", Added, None, Some("hello\n")),
            recorded("src/lib.rs", Modified, Some("fn a() {}\n"), Some("fn b() {}\n")),
            recorded("/work/app/gone.txt", Deleted, Some("bye\n"), None),
            binary,
            cut,
            recorded("odd.txt", Modified, None, None),
            {"not":"a diff"},
        ], "diffsTruncated": false});
        let diffs = files.diffs("multiedit", &metadata);
        let parts = diffs.iter().map(diff_parts).collect::<Vec<_>>();
        assert_eq!(
            parts,
            [
                (PathBuf::from("/work/app/new.txt"), None, "hello\n"),
                (
                    PathBuf::from("/work/app/src/lib.rs"),
                    Some("fn a() {}\n"),
                    "fn b() {}\n"
                ),
                (PathBuf::from("/work/app/gone.txt"), Some("bye\n"), ""),
            ]
        );
        assert_eq!(
            files.left,
            DIFF_TEXT_LIMIT - "hello\n".len() - 2 * "fn a() {}\n".len() - "bye\n".len()
        );
        assert!(files.diffs("write", &json!({"truncated": true})).is_empty());
        assert!(files.diffs("write", &Value::Null).is_empty());
    }

    /// A batch's steps record their changes in its result, each in its own
    /// metadata; only a batch's result is read that way.
    #[test]
    fn a_batch_s_steps_changes_become_diffs() {
        use haarness_protocol::FileDiffStatus::{Added, Modified};
        let metadata = json!({"failures": 1, "results": [
            {"tool":"edit", "output":"edited a.rs", "metadata": {"diffs": [
                recorded("a.rs", Modified, Some("x\n"), Some("y\n")),
            ]}},
            {"tool":"read", "output":"…", "metadata": null},
            {"tool":"edit", "error":"no match"},
            {"tool":"write", "output":"wrote b.rs", "metadata": {"diffs": [
                recorded("b.rs", Added, None, Some("new\n")),
            ]}},
        ]});
        let mut files = Files::new(PathBuf::from("/work/app"), None);
        let parts = files
            .diffs("batch", &metadata)
            .iter()
            .map(|diff| diff_parts(diff).0)
            .collect::<Vec<_>>();
        assert_eq!(
            parts,
            [
                PathBuf::from("/work/app/a.rs"),
                PathBuf::from("/work/app/b.rs")
            ]
        );
        assert!(files.diffs("other", &metadata).is_empty());
    }

    /// The diffs of one prompt carry a bounded amount of text: one that no
    /// longer fits is left out, and a smaller one after it still goes.
    #[test]
    fn diffs_stop_at_the_prompt_s_text_limit() {
        let mut files = Files::new(PathBuf::from("/work/app"), None);
        let big = "x".repeat(DIFF_TEXT_LIMIT / 2);
        let added = |file: &str, text: &str| json!({"diffs": [{"file": file, "status":"added", "additions":1, "deletions":0, "after": text}]});
        assert_eq!(files.diffs("write", &added("one.txt", &big)).len(), 1);
        assert_eq!(files.diffs("write", &added("two.txt", &big)).len(), 1);
        assert_eq!(files.left, 0);
        assert!(files.diffs("write", &added("three.txt", "")).is_empty());

        let mut files = Files::new(PathBuf::from("/work/app"), None);
        let most = "x".repeat(DIFF_TEXT_LIMIT - 10);
        assert_eq!(files.diffs("write", &added("one.txt", &most)).len(), 1);
        assert!(
            files
                .diffs("write", &added("two.txt", &"y".repeat(11)))
                .is_empty()
        );
        assert_eq!(files.diffs("write", &added("three.txt", "small")).len(), 1);
    }

    /// A load that cannot send every diff of the chat sends its latest ones.
    #[test]
    fn a_load_keeps_the_latest_diffs_within_the_limit() {
        let thread = ThreadId::new();
        let result = |sequence: u64, file: &str, size: usize| Item {
            id: haarness_protocol::ItemId::new(),
            thread_id: thread,
            turn_id: None,
            sequence,
            created_at: chrono::Utc::now(),
            data: ItemData::ToolResult {
                call_id: format!("call-{sequence}"),
                name: "write".into(),
                output: format!("wrote {file}"),
                is_error: false,
                metadata: json!({"diffs": [{
                    "file": file, "status": "added", "additions": 1, "deletions": 0,
                    "after": "x".repeat(size),
                }]}),
            },
        };
        let items = vec![
            result(1, "old.txt", DIFF_TEXT_LIMIT / 2),
            result(2, "middle.txt", DIFF_TEXT_LIMIT / 2),
            result(3, "new.txt", DIFF_TEXT_LIMIT / 2),
        ];
        let diffs = Files::new(PathBuf::from("/work/app"), None).replayed_diffs(&items);
        let mut kept = diffs.keys().copied().collect::<Vec<_>>();
        kept.sort_unstable();
        assert_eq!(kept, [1, 2]);
        assert_eq!(
            diff_parts(&diffs[&2][0]).0,
            PathBuf::from("/work/app/new.txt")
        );
    }

    #[test]
    fn the_chat_s_usage_becomes_the_editor_s_meter() {
        let usage = json!({
            "inputTokens": 5000, "outputTokens": 300, "costUsd": 0.25, "costIncomplete": false,
            "descendants": {"count": 1, "costUsd": 0.05},
            "context": {"tokens": 53_000, "window": 200_000, "compactAt": 160_000},
        });
        let update = usage_update(&usage).expect("a usage update");
        assert_eq!((update.used, update.size), (53_000, 200_000));
        let cost = update.cost.clone().expect("its cost");
        assert!((cost.amount - 0.30).abs() < 1e-9, "{}", cost.amount);
        assert_eq!(cost.currency, "USD");
        assert_eq!(
            serde_json::to_value(SessionUpdate::UsageUpdate(update)).unwrap()["sessionUpdate"],
            "usage_update"
        );

        // Nothing priced: no cost; only a subagent priced: its cost.
        let unpriced = json!({"costUsd": null, "descendants": {"count": 0, "costUsd": null},
            "context": {"tokens": 10, "window": 8192}});
        assert_eq!(usage_update(&unpriced).unwrap().cost, None);
        let child = json!({"costUsd": null, "descendants": {"costUsd": 0.5},
            "context": {"tokens": 10, "window": 8192}});
        let amount = usage_update(&child).unwrap().cost.unwrap().amount;
        assert!((amount - 0.5).abs() < 1e-9, "{amount}");

        // A model that states no window gets no meter.
        for context in [
            json!({"tokens": 10, "window": null}),
            json!({"tokens": 10, "window": 0}),
            json!({"tokens": 10}),
        ] {
            assert!(usage_update(&json!({"context": context})).is_none());
        }
        assert!(usage_update(&json!({})).is_none());
    }

    #[test]
    fn enforces_the_acp_absolute_stdio_mcp_command_contract() {
        let result = mcp_config(
            McpServer::Stdio(McpServerStdio::new("fixture", "npx")),
            Path::new("/tmp"),
        );
        assert!(result.is_err());
    }
}
