//! Agent Client Protocol adapter backed by the canonical Haarness REST/SSE server.
//!
//! Keeping ACP as a client of the workspace server preserves the single runtime
//! owner invariant: editor sessions, the TUI, and the CLI all observe the same
//! durable threads, approvals, tools, and event stream.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
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
        RequestPermissionOutcome, RequestPermissionRequest, SessionId, SessionNotification,
        SessionUpdate, StopReason, TextContent, ToolCall, ToolCallStatus, ToolCallUpdate,
        ToolCallUpdateFields, ToolKind,
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
                    tools: Some(HashMap::from([("question".into(), false), ("preview".into(), false)])),
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

        let mut calls = HashMap::<String, Value>::new();
        let result = async {
            loop {
                let Some(event) = events.recv().await else {
                    break Err(internal("Haarness event stream closed during an ACP turn"));
                };
                match event {
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
                        if event.event_type == "permission.requested" {
                            self.permission(
                                &request.session_id,
                                &event.payload,
                                connection,
                                &calls,
                            )
                            .await?;
                            continue;
                        }
                        if event.event_type == "turn.finished" {
                            let finished = serde_json::from_value::<Turn>(event.payload.clone())
                                .map_err(internal)?;
                            break finish(finished);
                        }
                    }
                    SubscriptionEvent::ResyncRequired => {
                        let current = self.client.turn(turn.id).await.map_err(internal)?;
                        if !matches!(current.status, TurnStatus::Queued | TurnStatus::Running) {
                            break finish(current);
                        }
                    }
                    SubscriptionEvent::Event(_)
                    | SubscriptionEvent::ConnectionLost { .. }
                    | SubscriptionEvent::Reconnected => {}
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

    async fn permission(
        &self,
        session: &SessionId,
        payload: &Value,
        connection: &ConnectionTo<Client>,
        calls: &HashMap<String, Value>,
    ) -> AcpResult<()> {
        let id = payload["permissionId"]
            .as_str()
            .ok_or_else(|| internal("permission event is missing permissionId"))?
            .to_owned();
        let operation = payload["operation"]
            .as_str()
            .unwrap_or("Approve tool use")
            .to_owned();
        let detail = payload.get("detail").cloned().unwrap_or(Value::Null);
        let call = calls
            .iter()
            .find(|(_, input)| **input == detail)
            .map_or_else(|| id.clone(), |(call, _)| call.clone());
        let response = connection
            .send_request(RequestPermissionRequest::new(
                session.clone(),
                ToolCallUpdate::new(
                    call,
                    ToolCallUpdateFields::new()
                        .title(operation)
                        .status(ToolCallStatus::Pending)
                        .raw_input(detail),
                ),
                vec![
                    PermissionOption::new("once", "Allow once", PermissionOptionKind::AllowOnce),
                    PermissionOption::new(
                        "always",
                        "Allow while Haarness is running",
                        PermissionOptionKind::AllowAlways,
                    ),
                    PermissionOption::new("reject", "Reject", PermissionOptionKind::RejectOnce),
                ],
            ))
            .block_task()
            .await?;
        let decision = match response.outcome {
            RequestPermissionOutcome::Selected(selected) => match selected.option_id.0.as_ref() {
                "once" => PermissionReplyDecision::Once,
                "always" => PermissionReplyDecision::Always,
                _ => PermissionReplyDecision::Reject,
            },
            _ => PermissionReplyDecision::Reject,
        };
        self.client
            .reply_permission(
                &id,
                PermissionReply {
                    decision,
                    feedback: None,
                },
            )
            .await
            .map_err(internal)
    }

    fn live_item(
        session: &SessionId,
        item: Item,
        connection: &ConnectionTo<Client>,
        calls: &mut HashMap<String, Value>,
    ) -> AcpResult<()> {
        match item.data {
            ItemData::ToolCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                calls.insert(call_id.clone(), arguments.clone());
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
            ItemData::Reasoning { text } => Some(SessionUpdate::AgentThoughtChunk(
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
