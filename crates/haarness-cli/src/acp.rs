//! Agent Client Protocol adapter backed by the canonical Haarness REST/SSE server.
//!
//! Keeping ACP as a client of the workspace server preserves the single runtime
//! owner invariant: editor sessions, the TUI, and the CLI all observe the same
//! durable threads, approvals, tools, and event stream.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[cfg(test)]
use agent_client_protocol::schema::v1::{
    AudioContent, EmbeddedResource, ImageContent, McpServerStdio, TextResourceContents,
};
use agent_client_protocol::schema::{
    ProtocolVersion,
    v1::{
        AgentCapabilities, CancelNotification, ContentBlock, ContentChunk,
        EmbeddedResourceResource, Implementation, InitializeRequest, InitializeResponse,
        LoadSessionRequest, LoadSessionResponse, McpCapabilities, McpServer, NewSessionRequest,
        NewSessionResponse, PermissionOption, PermissionOptionKind, Plan, PlanEntry,
        PlanEntryPriority, PlanEntryStatus, PromptCapabilities, PromptRequest, PromptResponse,
        RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse, SessionId,
        SessionNotification, SessionUpdate, StopReason, TextContent, ToolCall, ToolCallStatus,
        ToolCallUpdate, ToolCallUpdateFields, ToolKind,
    },
};
use agent_client_protocol::{Agent, Client, ConnectionTo, Stdio};
use anyhow::{Context as _, Result};
use haarness_protocol::{
    CreateThreadRequest, Item, ItemData, McpConfigureRequest, PermissionReply,
    PermissionReplyDecision, RequestId, StartTurnRequest, ThreadId, Turn, TurnId, TurnStatus,
};
use haarness_tui::api::{ApiClient, SubscriptionEvent};
use serde_json::{Value, json};
use uuid::Uuid;

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
    mcp: Arc<Mutex<HashSet<String>>>,
}

pub async fn serve(config: Config) -> Result<()> {
    let service = Service {
        client: config.client,
        workspace: config.workspace,
        provider: config.provider,
        model: config.model,
        agent: config.agent,
        auto_approve: config.auto_approve,
        active: Arc::new(Mutex::new(HashMap::new())),
        mcp: Arc::new(Mutex::new(HashSet::new())),
    };

    let create = service.clone();
    let load = service.clone();
    let prompt = service.clone();
    let cancel = service;
    Agent
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
        .connect_to(Stdio::new())
        .await
        .context("serve ACP over stdio")
}

impl Service {
    fn initialize(_request: InitializeRequest) -> InitializeResponse {
        InitializeResponse::new(ProtocolVersion::V1)
            .agent_capabilities(
                AgentCapabilities::new()
                    .load_session(true)
                    .prompt_capabilities(
                        PromptCapabilities::new().image(true).embedded_context(true),
                    )
                    .mcp_capabilities(McpCapabilities::new().http(true).sse(true)),
            )
            .agent_info(
                Implementation::new("haarness", env!("CARGO_PKG_VERSION")).title("Haarness"),
            )
    }

    async fn create(&self, request: NewSessionRequest) -> AcpResult<NewSessionResponse> {
        self.validate_scope(&request.cwd, &request.additional_directories)?;
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
            })
            .await
            .map_err(internal)?;
        self.configure_mcp(thread.id, request.mcp_servers, &request.cwd)
            .await?;
        Ok(NewSessionResponse::new(thread.id.to_string()))
    }

    async fn load(
        &self,
        request: LoadSessionRequest,
        connection: &ConnectionTo<Client>,
    ) -> AcpResult<LoadSessionResponse> {
        self.validate_scope(&request.cwd, &request.additional_directories)?;
        let thread = thread_id(&request.session_id)?;
        self.client.thread(thread).await.map_err(not_found)?;
        self.configure_mcp(thread, request.mcp_servers, &request.cwd)
            .await?;
        for item in self.client.items(thread).await.map_err(internal)? {
            Self::replay(&request.session_id, item, connection)?;
        }
        Ok(LoadSessionResponse::new())
    }

    async fn prompt(
        &self,
        request: PromptRequest,
        connection: &ConnectionTo<Client>,
    ) -> AcpResult<PromptResponse> {
        let thread = thread_id(&request.session_id)?;
        self.client.thread(thread).await.map_err(not_found)?;
        let (text, attachments) = prompt_content(request.prompt)?;
        if text.trim().is_empty() && attachments.is_empty() {
            return Err(invalid("prompt must contain text or supported media"));
        }

        let (mut events, event_task) = self.client.subscribe(None);
        let turn = self
            .client
            .start_turn(
                thread,
                StartTurnRequest {
                    request_id: RequestId::new(),
                    text,
                    attachments,
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
                },
            )
            .await
            .map_err(|error| {
                event_task.abort();
                internal(error)
            })?;
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(request.session_id.to_string(), turn.id);

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
        let result = async {
            loop {
                self.show_next(&mut approvals, &request.session_id, connection, &calls)
                    .await;
                let showing = approvals.shown.is_some();
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
                            Self::live_item(&request.session_id, item, connection, &mut calls)?;
                            continue;
                        }
                        if event.event_type == "turn.finished" {
                            let finished = serde_json::from_value::<Turn>(event.payload.clone())
                                .map_err(internal)?;
                            break finish(finished);
                        }
                    }
                    // Events may have been missed: requests raised or settled
                    // meanwhile are read again from the server.
                    SubscriptionEvent::ResyncRequired => {
                        let current = self.client.turn(turn.id).await.map_err(internal)?;
                        if !matches!(current.status, TurnStatus::Queued | TurnStatus::Running) {
                            break finish(current);
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
        result
    }

    async fn cancel(&self, session: &SessionId) -> AcpResult<()> {
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
        let actual = cwd
            .canonicalize()
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

    async fn configure_mcp(
        &self,
        thread: ThreadId,
        servers: Vec<McpServer>,
        cwd: &Path,
    ) -> AcpResult<()> {
        for server in servers {
            let (name, config) = mcp_config(server, cwd)?;
            let name = format!("acp-{}-{name}", &thread.to_string()[..8]);
            let fresh = self
                .mcp
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(name.clone());
            if fresh {
                if let Err(error) = self.client.configure_mcp(&name, &config).await {
                    self.mcp
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&name);
                    return Err(internal(error));
                }
            }
        }
        Ok(())
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
        let detail = payload.get("detail").cloned().unwrap_or(Value::Null);
        let call = asked_call(calls, child.map(|child| child.id), &detail)
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
                            .raw_input(arguments)
                            .meta(child.meta()),
                    ),
                )
            }
            ItemData::ToolResult {
                call_id,
                output,
                is_error,
                ..
            } => notify(
                connection,
                session,
                SessionUpdate::ToolCallUpdate(
                    ToolCallUpdate::new(
                        child.call(&call_id),
                        ToolCallUpdateFields::new()
                            .status(if is_error {
                                ToolCallStatus::Failed
                            } else {
                                ToolCallStatus::Completed
                            })
                            .content(vec![
                                ContentBlock::Text(TextContent::new(output.clone())).into(),
                            ])
                            .raw_output(parse_output(&output)),
                    )
                    .meta(child.meta()),
                ),
            ),
            _ => Ok(()),
        }
    }

    fn live_item(
        session: &SessionId,
        item: Item,
        connection: &ConnectionTo<Client>,
        calls: &mut Calls,
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
                            .raw_input(arguments),
                    ),
                )
            }
            ItemData::ToolResult {
                call_id,
                output,
                is_error,
                ..
            } => notify(
                connection,
                session,
                SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                    call_id,
                    ToolCallUpdateFields::new()
                        .status(if is_error {
                            ToolCallStatus::Failed
                        } else {
                            ToolCallStatus::Completed
                        })
                        .content(vec![
                            ContentBlock::Text(TextContent::new(output.clone())).into(),
                        ])
                        .raw_output(parse_output(&output)),
                )),
            ),
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

    fn replay(session: &SessionId, item: Item, connection: &ConnectionTo<Client>) -> AcpResult<()> {
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
            } => Some(SessionUpdate::ToolCall(
                ToolCall::new(call_id, title(&name, &arguments))
                    .kind(tool_kind(&name))
                    .status(ToolCallStatus::InProgress)
                    .raw_input(arguments),
            )),
            ItemData::ToolResult {
                call_id,
                output,
                is_error,
                ..
            } => Some(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                call_id,
                ToolCallUpdateFields::new()
                    .status(if is_error {
                        ToolCallStatus::Failed
                    } else {
                        ToolCallStatus::Completed
                    })
                    .content(vec![
                        ContentBlock::Text(TextContent::new(output.clone())).into(),
                    ])
                    .raw_output(parse_output(&output)),
            ))),
            _ => None,
        };
        if let Some(update) = update {
            notify(connection, session, update)?;
        }
        Ok(())
    }
}

/// The call a request is about, in the chat that asks (`None` for the
/// session's own): two children can make the same call.
fn asked_call(calls: &Calls, chat: Option<ThreadId>, detail: &Value) -> Option<String> {
    calls
        .iter()
        .find(|(_, (owner, input))| *owner == chat && input == detail)
        .map(|(call, _)| call.clone())
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
    let fence = "`".repeat(longest(content).max(2) + 1);
    let span = "`".repeat(longest(path) + 1);
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
        TurnStatus::Failed => Err(internal(
            turn.error.unwrap_or_else(|| "Haarness turn failed".into()),
        )),
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
        "read" => arguments["path"]
            .as_str()
            .map_or_else(|| "Read file".into(), |path| format!("Read {path}")),
        "write" | "edit" | "multiedit" | "apply_patch" => arguments["path"]
            .as_str()
            .map_or_else(|| format!("Use {name}"), |path| format!("Edit {path}")),
        _ => format!("Use {name}"),
    }
}

fn tool_kind(name: &str) -> ToolKind {
    match name {
        "read" | "list" => ToolKind::Read,
        "write" | "edit" | "multiedit" | "apply_patch" => ToolKind::Edit,
        "glob" | "grep" | "memory_search" | "session_search" => ToolKind::Search,
        "bash" | "execute" => ToolKind::Execute,
        "webfetch" => ToolKind::Fetch,
        "todowrite" | "task" | "goal" => ToolKind::Think,
        _ => ToolKind::Other,
    }
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

fn not_found(error: impl std::fmt::Display) -> agent_client_protocol::Error {
    agent_client_protocol::Error::resource_not_found(None).data(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            asked_call(&calls, Some(child), &same),
            Some(format!("call-1@{child}"))
        );
        assert_eq!(asked_call(&calls, None, &same), None);
        assert_eq!(
            asked_call(&calls, None, &json!({"path": "own.md"})).as_deref(),
            Some("call-1")
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
        assert_eq!(
            title("read", &json!({"path":"src/main.rs"})),
            "Read src/main.rs"
        );
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
