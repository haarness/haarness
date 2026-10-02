use std::{convert::Infallible, net::SocketAddr, sync::Arc, time::Duration};

use async_stream::stream;
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, Request, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures::{Stream, StreamExt};
use haarness_core::{AgentRuntime, CoreError, McpRegistry, SkillRegistry};
use haarness_language::{FormatterStatus, LanguageService, LanguageStatus};
use haarness_protocol::{
    AgentProfile, ApiDoc, ApiError, BackgroundTaskInfo, BtwRequest, BtwResponse, CloudAgentInput,
    CloudChatInput, CloudScheduleInput, CloudSessionInput, CommandExecutionResponse,
    CompactRequest, CreateThreadRequest, EventEnvelope, ExecuteCommandRequest, FileDiff,
    ForkThreadRequest, GoalActionRequest, GoalActionResponse, LazyToolSettingsUpdate,
    McpConfigureRequest, McpServerInfo, PTY_CAPABILITY, PendingPermissionInfo, PendingQuestionInfo,
    PermissionReply, PluginActionRequest, PluginInfo, ProviderInfo, PtyCreateRequest, PtyId,
    PtyInfo, PtyUpdateRequest, QuestionReply, QueueInputRequest, QueueInputResponse, ReviewFile,
    SCHEMA_VERSION, SESSION_ARCHIVE_CAPABILITY, ServerInfo, SessionArchive,
    SessionArchiveImportRequest, SessionArchiveImportResult, SkillInfo, StartTurnRequest, Thread,
    ThreadId, ToolCatalogResponse, ToolSettingsUpdate, Turn, TurnId, UpdateThreadRequest,
    WORKSPACE_CLEANUP_CAPABILITY, WORKSPACE_HISTORY_CAPABILITY, WorkflowId, WorkflowRun,
    WorkflowRunStatus, WorkflowStartRequest, WorkspaceCleanupRequest, WorkspaceCreateRequest,
    WorkspaceInfo, WorkspaceListResponse, WorkspaceOperationResponse, WorkspaceRefsResponse,
    WorkspaceRemoveRequest, WorkspaceResetRequest, WorkspaceResolveRefRequest,
    WorkspaceResolveRefResponse,
};
use haarness_pty::{PtyManager, PtySubscription};
use haarness_workflow_catalog::{CatalogError, WorkflowCatalog};
use haarness_workflows::WorkflowCoordinator;
use haarness_workspaces::{WorkspaceError, WorkspaceManager};
use opentelemetry::global;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_stream::wrappers::BroadcastStream;
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use utoipa::OpenApi;

use crate::auth::{AuthState, require_auth};

#[derive(Clone)]
pub struct ServerState {
    pub browser: Option<Arc<crate::browser::BrowserBroker>>,
    pub management: Option<Arc<dyn crate::ManagementRegistry>>,
    pub runtime: Arc<AgentRuntime>,
    pub mcp: Arc<dyn McpRegistry>,
    pub skills: Arc<dyn SkillRegistry>,
    pub plugins: Option<Arc<dyn crate::PluginRegistry>>,
    pub workflows: Option<Arc<WorkflowCoordinator>>,
    pub workspaces: Option<Arc<WorkspaceManager>>,
    pub workspace_id: Option<String>,
    pub workspace: std::path::PathBuf,
    pub project_trusted: bool,
    pub language: Option<Arc<LanguageService>>,
}

pub struct ServerHandle {
    pub address: SocketAddr,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    runtime: Arc<AgentRuntime>,
    workflows: Option<Arc<WorkflowCoordinator>>,
    language: Option<Arc<LanguageService>>,
    ptys: Arc<PtyManager>,
}

impl ServerHandle {
    #[must_use]
    pub fn shutdown_signal(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub async fn shutdown(self) -> anyhow::Result<()> {
        self.shutdown.cancel();
        self.wait().await
    }

    /// Wait for local signal or authenticated `/v1/shutdown`, then drain
    /// server-owned work into honest terminal states before returning.
    pub async fn wait(self) -> anyhow::Result<()> {
        self.ptys.shutdown_all();
        let workflow_result = if let Some(workflows) = &self.workflows {
            workflows.interrupt_active().await.map(|count| {
                if count > 0 {
                    tracing::info!(count, "interrupted active workflows before shutdown");
                }
            })
        } else {
            Ok(())
        };
        let turn_result = self
            .runtime
            .interrupt_active_turns("Haarness stopped")
            .await;
        if let Ok(count) = &turn_result
            && *count > 0
        {
            tracing::info!(count, "interrupted active turns before shutdown");
        }
        let server_result = self.task.await?;
        if let Some(language) = &self.language {
            language.shutdown().await;
        }
        server_result?;
        workflow_result?;
        turn_result?;
        Ok(())
    }
}

pub async fn serve(
    listener: TcpListener,
    token: &str,
    state: ServerState,
) -> anyhow::Result<ServerHandle> {
    serve_with_host(listener, token, state, None).await
}

pub async fn serve_with_host(
    listener: TcpListener,
    token: &str,
    state: ServerState,
    host: Option<Arc<dyn crate::remote::WorkspaceHost>>,
) -> anyhow::Result<ServerHandle> {
    serve_with_access(
        listener,
        token,
        state,
        host,
        crate::auth::ServerAccess::Private,
    )
    .await
}

// Retain the same async startup API as serve for existing callers.
#[allow(clippy::unused_async)]
pub async fn serve_with_access(
    listener: TcpListener,
    token: &str,
    state: ServerState,
    host: Option<Arc<dyn crate::remote::WorkspaceHost>>,
    access: crate::auth::ServerAccess,
) -> anyhow::Result<ServerHandle> {
    let address = listener.local_addr()?;
    let workflow_coordinator = state.workflows.clone();
    let language = state.language.clone();
    let ptys = Arc::new(PtyManager::new(
        state.workspace.clone(),
        state.runtime.events().clone(),
    ));
    let auth = AuthState::with_access(token, address.ip().is_loopback(), &access);
    let shutdown = CancellationToken::new();
    let protected = Router::new()
        .route("/manage/{resource}", post(manage_resource))
        .route("/cloud/agents", post(cloud_agents))
        .route("/cloud/sessions", post(cloud_sessions))
        .route("/cloud/chats", post(cloud_chats))
        .route("/cloud/schedules", post(cloud_schedules))
        .merge(crate::profiles::routes())
        .route("/info", get(info))
        .route("/stats", get(analytics_stats))
        .route("/insights", get(analytics_insights))
        .route("/providers", get(providers))
        .route("/providers/refresh", post(refresh_providers))
        .route("/agents", get(agents))
        .route("/commands", get(commands))
        .route("/lsp", get(lsp_status))
        .route("/formatter", get(formatter_status))
        .route("/tools", get(tool_catalog))
        .route("/tools/lazy", post(update_lazy_tools))
        .route("/tools/{name}", post(update_tool))
        .route("/projects", get(projects))
        .route("/workspaces", get(workspaces_list).post(create_workspace))
        .route("/workspaces/refs", get(workspace_refs))
        .route("/workspaces/resolve-ref", post(resolve_workspace_ref))
        .route(
            "/workspaces/{reference}",
            get(get_workspace).delete(remove_workspace),
        )
        .route("/workspaces/{reference}/open", post(open_workspace))
        .route("/workspaces/{reference}/reset", post(reset_workspace))
        .route("/workspaces/{reference}/cleanup", post(cleanup_workspace))
        .route("/ptys", get(list_ptys).post(create_pty))
        .route(
            "/ptys/{id}",
            get(get_pty).patch(update_pty).delete(delete_pty),
        )
        .route("/ptys/{id}/connect", get(connect_pty))
        .route("/workflows", get(workflows))
        .route("/workflows/{invocation}/source", get(workflow_source))
        .route("/workflows/run", post(start_workflow))
        .route("/workflows/runs", get(list_workflow_runs))
        .route(
            "/workflows/runs/{id}",
            get(get_workflow_run).delete(delete_workflow_run),
        )
        .route("/workflows/runs/{id}/wait", get(wait_workflow_run))
        .route("/workflows/runs/{id}/cancel", post(cancel_workflow_run))
        .route("/workflows/runs/{id}/pause", post(pause_workflow_run))
        .route("/workflows/runs/{id}/resume", post(resume_workflow_run))
        .route("/skills", get(skills))
        .route("/skills/{name}", get(load_skill))
        .route("/plugins", get(plugins).post(plugin_action))
        .route("/mcp", get(mcp_servers))
        .route("/mcp/{name}", post(configure_mcp))
        .route("/mcp/{name}/reconnect", post(reconnect_mcp))
        .route("/mcp/{name}/disconnect", post(disconnect_mcp))
        .route("/mcp/{name}/tools", get(mcp_tools))
        .route("/mcp/{name}/tools/{tool}", post(call_mcp_tool))
        .route("/mcp/{name}/resources", get(mcp_resources))
        .route(
            "/mcp/{name}/resource-templates",
            get(mcp_resource_templates),
        )
        .route("/mcp/{name}/resources/read", post(read_mcp_resource))
        .route("/mcp/{name}/prompts", get(mcp_prompts))
        .route("/mcp/{name}/prompts/{prompt}", post(get_mcp_prompt))
        .route("/threads", get(list_threads).post(create_thread))
        .route("/threads/{id}", get(get_thread).patch(update_thread))
        .route("/threads/{id}/archive", get(export_session_archive))
        .route("/threads/{id}/fork", post(fork_thread))
        .route("/threads/{id}/items", get(list_items))
        .route("/threads/{id}/todos", get(list_todos))
        .route("/threads/{id}/active", get(active_turn))
        .route("/threads/{id}/interrupt", post(interrupt_thread_tree))
        .route(
            "/threads/{id}/background",
            get(list_background_tasks).post(promote_background_tasks),
        )
        .route("/threads/{id}/pending-question", get(pending_question))
        // Twenty MiB of inline media expands to ~27 MiB in base64. Keep
        // other endpoints at their existing bounds.
        .route(
            "/threads/{id}/turns",
            post(start_turn).layer(DefaultBodyLimit::max(32 * 1024 * 1024)),
        )
        .route(
            "/threads/{id}/commands",
            post(execute_command).layer(DefaultBodyLimit::max(32 * 1024 * 1024)),
        )
        .route(
            "/threads/{id}/input",
            post(queue_input).layer(DefaultBodyLimit::max(32 * 1024 * 1024)),
        )
        .route(
            "/threads/{id}/input/queue",
            get(input_queue).post(change_input_queue),
        )
        .route("/threads/{id}/btw", post(btw))
        .route("/threads/{id}/compact", post(compact))
        .route("/threads/{id}/goal", get(get_goal).post(set_goal))
        .route("/threads/{id}/loop", get(get_loop).post(set_loop))
        .route("/threads/{id}/usage", get(thread_usage))
        .route("/threads/{id}/shell", post(run_shell))
        .route("/threads/{id}/revert", post(revert_thread))
        .route("/threads/{id}/unrevert", post(unrevert_thread))
        .route("/threads/{id}/diff", get(last_turn_diff))
        .route("/threads/{id}/diff/review", get(last_turn_review))
        .route("/vcs/diff", get(vcs_diff))
        .route("/files/search", get(search_files))
        .merge(crate::files::routes())
        .merge(crate::browser::routes())
        .route("/session-archives/import", post(import_session_archive))
        .route("/turns/active", get(active_turns))
        .route("/turns/{id}", get(get_turn))
        .route("/turns/{id}/interrupt", post(interrupt_turn))
        .route("/turns/{id}/wait", get(wait_turn))
        .route("/permissions", get(list_permissions))
        .route("/permissions/{id}", post(reply_permission))
        .route("/questions", get(list_questions))
        .route("/questions/{id}", post(reply_question))
        .route("/shutdown", post(shutdown_server))
        .route("/events", get(events))
        .layer(axum::Extension(shutdown.clone()))
        .layer(axum::Extension(Arc::clone(&ptys)))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::remote_scope::enforce,
        ))
        .route_layer(middleware::from_fn_with_state(auth, require_auth));
    let runtime = Arc::clone(&state.runtime);
    let mut app = Router::new()
        .merge(crate::sharing::routes())
        .route("/healthz", get(health))
        .route("/openapi.json", get(openapi))
        .nest("/v1", protected)
        .with_state(state)
        .layer(middleware::from_fn(trace_request));
    if let Some(host) = host {
        app = app.merge(crate::remote::routes(host, shutdown.clone()).route_layer(
            middleware::from_fn_with_state(
                AuthState::with_access(token, address.ip().is_loopback(), &access),
                require_auth,
            ),
        ));
    }
    let server_shutdown = shutdown.clone();
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(server_shutdown.cancelled_owned())
            .await
            .map_err(Into::into)
    });
    Ok(ServerHandle {
        address,
        shutdown,
        task,
        runtime,
        workflows: workflow_coordinator,
        language,
        ptys,
    })
}

struct HeaderExtractor<'a>(&'a HeaderMap);

impl opentelemetry::propagation::Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key)?.to_str().ok()
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(axum::http::HeaderName::as_str).collect()
    }
}

async fn trace_request(request: Request, next: Next) -> Response {
    let parent = global::get_text_map_propagator(|propagator| {
        propagator.extract(&HeaderExtractor(request.headers()))
    });
    let method = request.method().clone();
    let path = if request.uri().path().starts_with("/s/") {
        "/s/{token}".into()
    } else {
        request.uri().path().to_owned()
    };
    let span = tracing::info_span!(
        "http.request",
        "otel.kind" = "server",
        "http.request.method" = %method,
        "url.path" = %path,
        "http.response.status_code" = tracing::field::Empty,
    );
    let _ = span.set_parent(parent);
    let response_span = span.clone();
    async move {
        let response = next.run(request).await;
        response_span.record(
            "http.response.status_code",
            i64::from(response.status().as_u16()),
        );
        response
    }
    .instrument(span)
    .await
}

async fn shutdown_server(
    axum::Extension(shutdown): axum::Extension<CancellationToken>,
) -> StatusCode {
    shutdown.cancel();
    StatusCode::ACCEPTED
}

async fn health() -> Json<Value> {
    Json(json!({"status":"ok"}))
}
async fn openapi() -> Json<Value> {
    let mut document = ApiDoc::openapi();
    document.merge(AnalyticsApi::openapi());
    document.merge(crate::profiles::ProfilesApi::openapi());
    document.merge(crate::files::FilesApi::openapi());
    document.merge(crate::browser::BrowserApi::openapi());
    document.merge(ManagementApi::openapi());
    document.merge(crate::remote::HostApi::openapi());
    Json(serde_json::to_value(document).expect("OpenAPI serializes"))
}

#[derive(OpenApi)]
#[openapi(paths(analytics_stats, analytics_insights))]
struct AnalyticsApi;

async fn analytics_report(
    state: ServerState,
    options: haarness_insights::Options,
) -> ApiResult<haarness_insights::Report> {
    options
        .validate()
        .map_err(|e| CoreError::Invalid(e.to_string()))?;
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(|| CoreError::Invalid("shared workspace history is not configured".into()))?;
    let data = manager.data_directory().to_owned();
    tokio::task::spawn_blocking(move || {
        haarness_insights::collect(&data, &state.workspace, options)
    })
    .await
    .map_err(|e| CoreError::Storage(e.to_string()))?
    .map_err(|e| ApiFailure(CoreError::Invalid(e.to_string())))
}

#[utoipa::path(get, path = "/v1/stats", params(
    ("days" = Option<u32>, Query, description = "Rolling days selected by turn start time"),
    ("project" = Option<String>, Query, description = "Project ID, name or root"),
    ("currentProject" = Option<bool>, Query), ("limit" = Option<usize>, Query)
), responses((status = 200, body = haarness_insights::Report)))]
async fn analytics_stats(
    State(state): State<ServerState>,
    Query(options): Query<haarness_insights::Options>,
) -> ApiResult<Json<haarness_insights::Report>> {
    Ok(Json(analytics_report(state, options).await?))
}

#[derive(serde::Serialize, utoipa::ToSchema)]
struct AnalyticsDocument {
    summary: String,
    html: String,
}

#[utoipa::path(get, path = "/v1/insights", params(
    ("days" = Option<u32>, Query), ("project" = Option<String>, Query),
    ("currentProject" = Option<bool>, Query), ("limit" = Option<usize>, Query)
), responses((status = 200, body = AnalyticsDocument)))]
async fn analytics_insights(
    State(state): State<ServerState>,
    Query(options): Query<haarness_insights::Options>,
) -> ApiResult<Json<AnalyticsDocument>> {
    let report = analytics_report(state, options).await?;
    Ok(Json(AnalyticsDocument {
        summary: haarness_insights::render_text(&report),
        html: haarness_insights::render_html(&report),
    }))
}

async fn list_ptys(axum::Extension(ptys): axum::Extension<Arc<PtyManager>>) -> Json<Vec<PtyInfo>> {
    Json(ptys.list())
}

async fn create_pty(
    axum::Extension(ptys): axum::Extension<Arc<PtyManager>>,
    Json(request): Json<PtyCreateRequest>,
) -> ApiResult<(StatusCode, Json<PtyInfo>)> {
    let info = ptys.create(request)?;
    Ok((StatusCode::CREATED, Json(info)))
}

async fn get_pty(
    axum::Extension(ptys): axum::Extension<Arc<PtyManager>>,
    Path(id): Path<PtyId>,
) -> ApiResult<Json<PtyInfo>> {
    Ok(Json(ptys.get(id)?))
}

async fn update_pty(
    axum::Extension(ptys): axum::Extension<Arc<PtyManager>>,
    Path(id): Path<PtyId>,
    Json(request): Json<PtyUpdateRequest>,
) -> ApiResult<Json<PtyInfo>> {
    Ok(Json(ptys.update(id, request)?))
}

async fn delete_pty(
    axum::Extension(ptys): axum::Extension<Arc<PtyManager>>,
    Path(id): Path<PtyId>,
) -> ApiResult<StatusCode> {
    ptys.delete(id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct PtyConnectQuery {
    cursor: Option<u64>,
}

async fn connect_pty(
    axum::Extension(ptys): axum::Extension<Arc<PtyManager>>,
    Path(id): Path<PtyId>,
    Query(query): Query<PtyConnectQuery>,
    upgrade: WebSocketUpgrade,
) -> ApiResult<Response> {
    // Resolve before upgrading so unknown sessions retain the normal JSON 404
    // contract instead of failing opaquely inside an accepted WebSocket.
    let subscription = ptys.subscribe(id, query.cursor)?;
    Ok(upgrade
        .on_upgrade(move |socket| pty_socket(socket, ptys, id, subscription))
        .into_response())
}

async fn pty_socket(
    mut socket: WebSocket,
    ptys: Arc<PtyManager>,
    id: PtyId,
    mut subscription: PtySubscription,
) {
    let replay_control = json!({
        "type": "replay",
        "start": subscription.cursor.start,
        "end": subscription.cursor.end,
        "truncated": subscription.cursor.truncated,
    });
    if socket
        .send(Message::Text(replay_control.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    for chunk in subscription.replay.chunks(64 * 1024) {
        if socket
            .send(Message::Binary(chunk.to_vec().into()))
            .await
            .is_err()
        {
            return;
        }
    }
    let mut cursor = subscription.cursor.end;
    if send_pty_cursor(&mut socket, subscription.cursor.start, cursor, false)
        .await
        .is_err()
    {
        return;
    }
    if ptys
        .get(id)
        .is_ok_and(|info| info.status == haarness_protocol::PtyStatus::Exited)
    {
        let _ = socket.send(Message::Close(None)).await;
        return;
    }

    loop {
        tokio::select! {
            output = subscription.receiver.recv() => match output {
                Ok(bytes) if bytes.is_empty() => {
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                }
                Ok(bytes) => {
                    cursor = cursor.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
                    if socket.send(Message::Binary(bytes.into())).await.is_err()
                        || send_pty_cursor(&mut socket, subscription.cursor.start, cursor, false).await.is_err()
                    {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // The client reconnects with its last acknowledged cursor;
                    // the bounded session buffer fills the exact missing range.
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Binary(bytes))) => {
                    if ptys.write(id, &bytes).is_err() {
                        let _ = socket.send(Message::Close(None)).await;
                        break;
                    }
                }
                Some(Ok(Message::Ping(bytes))) => {
                    if socket.send(Message::Pong(bytes)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                Some(Ok(Message::Text(_) | Message::Pong(_))) => {}
            },
        }
    }
}

async fn send_pty_cursor(
    socket: &mut WebSocket,
    start: u64,
    end: u64,
    truncated: bool,
) -> Result<(), axum::Error> {
    socket
        .send(Message::Text(
            json!({
                "type": "cursor",
                "start": start,
                "end": end,
                "truncated": truncated,
            })
            .to_string()
            .into(),
        ))
        .await
}

async fn info(State(state): State<ServerState>) -> Json<ServerInfo> {
    let (default_provider, default_model) = state.runtime.model_defaults();
    Json(ServerInfo {
        default_agent: state.runtime.agent_default(),
        name: "haarness".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        schema_version: SCHEMA_VERSION.into(),
        server_instance_id: state.runtime.events().server_instance_id(),
        event_cursor: state.runtime.events().cursor(),
        capabilities: vec![
            "remote-chat-scope.v1".into(),
            "rest".into(),
            "providers.refresh.v1".into(),
            "sse".into(),
            "network.attach".into(),
            "attachments.inline-upload".into(),
            "files.base64-write.v1".into(),
            "mcp".into(),
            "skills".into(),
            "subagents".into(),
            "btw.v1".into(),
            "projects.v1".into(),
            "workspaces.v1".into(),
            WORKSPACE_CLEANUP_CAPABILITY.into(),
            "workflows".into(),
            "ultracode".into(),
            "goals".into(),
            "workflows.read".into(),
            "opentui".into(),
            "tool-catalog.v1".into(),
            "lazy-tools.v1".into(),
            "lsp.v1".into(),
            "formatter.v1".into(),
            "configuration-hot-reload.v1".into(),
            WORKSPACE_HISTORY_CAPABILITY.into(),
            haarness_protocol::INPUT_QUEUE_CAPABILITY.into(),
            PTY_CAPABILITY.into(),
            SESSION_ARCHIVE_CAPABILITY.into(),
        ]
        .into_iter()
        .chain(
            state
                .browser
                .as_ref()
                .into_iter()
                .flat_map(|_| ["browser.v1".into(), "browser-drafts.v1".into()]),
        )
        .chain(
            state
                .management
                .as_ref()
                .into_iter()
                .flat_map(|_| ["input-settings.v1".into(), "browser-settings.v1".into()]),
        )
        .collect(),
        workspace: Some(state.workspace.display().to_string()),
        git_branch: git_branch(&state.workspace),
        project_trusted: Some(state.project_trusted),
        default_provider,
        default_model,
    })
}

async fn lsp_status(State(state): State<ServerState>) -> Json<Vec<LanguageStatus>> {
    Json(match state.language {
        Some(language) => language.status().await,
        None => Vec::new(),
    })
}

async fn formatter_status(State(state): State<ServerState>) -> Json<Vec<FormatterStatus>> {
    Json(
        state
            .language
            .map_or_else(Vec::new, |language| language.formatter_status()),
    )
}

fn workspace_manager(state: &ServerState) -> Result<Arc<WorkspaceManager>, CoreError> {
    state
        .workspaces
        .clone()
        .ok_or_else(|| CoreError::Invalid("workspace management is unavailable".into()))
}

async fn projects(
    State(state): State<ServerState>,
) -> ApiResult<Json<Vec<haarness_protocol::ProjectInfo>>> {
    let manager = workspace_manager(&state)?;
    let current = state.workspace_id.clone();
    let response = tokio::task::spawn_blocking(move || manager.list(current.as_deref()))
        .await
        .map_err(|error| CoreError::Storage(error.to_string()))?
        .map_err(workspace_api_error)?;
    Ok(Json(response.projects))
}

async fn workspaces_list(
    State(state): State<ServerState>,
) -> ApiResult<Json<WorkspaceListResponse>> {
    let manager = workspace_manager(&state)?;
    let current = state.workspace_id.clone();
    let response = tokio::task::spawn_blocking(move || manager.list(current.as_deref()))
        .await
        .map_err(|error| CoreError::Storage(error.to_string()))?
        .map_err(workspace_api_error)?;
    Ok(Json(response))
}

async fn workspace_refs(
    State(state): State<ServerState>,
) -> ApiResult<Json<WorkspaceRefsResponse>> {
    let manager = workspace_manager(&state)?;
    let current = state
        .workspace_id
        .clone()
        .ok_or_else(|| CoreError::Invalid("current workspace is not registered".into()))?;
    let response = tokio::task::spawn_blocking(move || manager.refs(&current))
        .await
        .map_err(|error| CoreError::Storage(error.to_string()))?
        .map_err(workspace_api_error)?;
    Ok(Json(response))
}

async fn resolve_workspace_ref(
    State(state): State<ServerState>,
    Json(request): Json<WorkspaceResolveRefRequest>,
) -> ApiResult<Json<WorkspaceResolveRefResponse>> {
    let manager = workspace_manager(&state)?;
    let current = state
        .workspace_id
        .clone()
        .ok_or_else(|| CoreError::Invalid("current workspace is not registered".into()))?;
    let response = tokio::task::spawn_blocking(move || manager.resolve_ref(&current, &request))
        .await
        .map_err(|error| CoreError::Storage(error.to_string()))?
        .map_err(workspace_api_error)?;
    Ok(Json(response))
}

async fn get_workspace(
    State(state): State<ServerState>,
    Path(reference): Path<String>,
) -> ApiResult<Json<WorkspaceInfo>> {
    let manager = workspace_manager(&state)?;
    let workspace = tokio::task::spawn_blocking(move || manager.get(&reference))
        .await
        .map_err(|error| CoreError::Storage(error.to_string()))?
        .map_err(workspace_api_error)?;
    Ok(Json(workspace))
}

async fn create_workspace(
    State(state): State<ServerState>,
    Json(request): Json<WorkspaceCreateRequest>,
) -> ApiResult<(StatusCode, Json<WorkspaceOperationResponse>)> {
    let manager = workspace_manager(&state)?;
    let current = state
        .workspace_id
        .clone()
        .ok_or_else(|| CoreError::Invalid("current workspace is not registered".into()))?;
    let response = tokio::task::spawn_blocking(move || manager.create_worktree(&current, &request))
        .await
        .map_err(|error| CoreError::Storage(error.to_string()))?
        .map_err(workspace_api_error)?;
    Ok((StatusCode::CREATED, Json(response)))
}

async fn open_workspace(
    State(state): State<ServerState>,
    Path(reference): Path<String>,
) -> ApiResult<Json<WorkspaceOperationResponse>> {
    let manager = workspace_manager(&state)?;
    let workspace = tokio::task::spawn_blocking(move || manager.open_workspace(&reference))
        .await
        .map_err(|error| CoreError::Storage(error.to_string()))?
        .map_err(workspace_api_error)?;
    Ok(Json(WorkspaceOperationResponse {
        message: format!(
            "workspace selected; attach or launch Haarness with --workspace '{}'",
            workspace.path
        ),
        workspace,
    }))
}

async fn reset_workspace(
    State(state): State<ServerState>,
    Path(reference): Path<String>,
    Json(request): Json<WorkspaceResetRequest>,
) -> ApiResult<Json<WorkspaceOperationResponse>> {
    let manager = workspace_manager(&state)?;
    let response =
        tokio::task::spawn_blocking(move || manager.reset_worktree(&reference, &request))
            .await
            .map_err(|error| CoreError::Storage(error.to_string()))?
            .map_err(workspace_api_error)?;
    Ok(Json(response))
}

async fn remove_workspace(
    State(state): State<ServerState>,
    Path(reference): Path<String>,
    Json(request): Json<WorkspaceRemoveRequest>,
) -> ApiResult<Json<WorkspaceOperationResponse>> {
    let manager = workspace_manager(&state)?;
    let response =
        tokio::task::spawn_blocking(move || manager.remove_worktree(&reference, &request))
            .await
            .map_err(|error| CoreError::Storage(error.to_string()))?
            .map_err(workspace_api_error)?;
    Ok(Json(response))
}

async fn cleanup_workspace(
    State(state): State<ServerState>,
    Path(reference): Path<String>,
    Json(request): Json<WorkspaceCleanupRequest>,
) -> ApiResult<Json<WorkspaceOperationResponse>> {
    let manager = workspace_manager(&state)?;
    let response = tokio::task::spawn_blocking(move || {
        manager.cleanup_missing_workspace(&reference, &request)
    })
    .await
    .map_err(|error| CoreError::Storage(error.to_string()))?
    .map_err(workspace_api_error)?;
    Ok(Json(response))
}

fn workspace_api_error(error: WorkspaceError) -> CoreError {
    match error {
        WorkspaceError::NotFound(message) => CoreError::NotFound(message),
        WorkspaceError::Conflict(message) => CoreError::Conflict(message),
        WorkspaceError::Invalid(message) => CoreError::Invalid(message),
        WorkspaceError::Io(message) => CoreError::Storage(message),
        WorkspaceError::Git(message) => CoreError::Tool(message),
    }
}

/// Current git branch of the workspace, read directly from `.git/HEAD` with no
/// git dependency. Walks up from the workspace and follows worktree gitdir
/// files; detached heads report the short commit hash.
fn git_branch(workspace: &std::path::Path) -> Option<String> {
    let mut current = Some(workspace);
    while let Some(directory) = current {
        let git = directory.join(".git");
        if git.is_dir() {
            return read_head(&git.join("HEAD"));
        }
        if git.is_file() {
            let content = std::fs::read_to_string(&git).ok()?;
            let gitdir = content.trim().strip_prefix("gitdir: ")?;
            let gitdir_path = directory.join(gitdir);
            return read_head(&gitdir_path.join("HEAD"));
        }
        current = directory.parent();
    }
    None
}

fn read_head(head: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(head).ok()?;
    let content = content.trim();
    if let Some(reference) = content.strip_prefix("ref: refs/heads/") {
        return Some(reference.to_owned());
    }
    Some(content.get(..8).unwrap_or(content).to_owned())
}
async fn model_catalog(state: &ServerState, force: bool) -> Vec<ProviderInfo> {
    let registry = state.runtime.providers();
    let providers: Vec<_> = registry
        .list()
        .iter()
        .filter(|info| info.authenticated)
        .filter_map(|info| registry.get(&info.id))
        .collect();
    futures::future::join_all(
        providers
            .iter()
            .map(|provider| provider.refresh_models(force)),
    )
    .await;
    registry.list()
}
async fn providers(State(state): State<ServerState>) -> Json<Vec<ProviderInfo>> {
    Json(model_catalog(&state, false).await)
}
async fn refresh_providers(State(state): State<ServerState>) -> Json<Vec<ProviderInfo>> {
    Json(model_catalog(&state, true).await)
}
async fn agents(State(state): State<ServerState>) -> Json<Vec<AgentProfile>> {
    Json(state.runtime.agent_profiles())
}
/// Authoritative, deduplicated slash-command registry. Resolution is
/// case-insensitive: reserved built-ins, configured prompts, then workflows.
async fn commands(State(state): State<ServerState>) -> Json<Vec<haarness_protocol::CommandInfo>> {
    Json(crate::command::registry(&state))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolCatalogQuery {
    thread_id: Option<ThreadId>,
}

async fn tool_catalog(
    State(state): State<ServerState>,
    Query(query): Query<ToolCatalogQuery>,
) -> ApiResult<Json<ToolCatalogResponse>> {
    Ok(Json(state.runtime.tool_catalog(query.thread_id).await?))
}

async fn update_tool(
    State(state): State<ServerState>,
    Path(name): Path<String>,
    Json(request): Json<ToolSettingsUpdate>,
) -> ApiResult<Json<ToolCatalogResponse>> {
    Ok(Json(
        state
            .runtime
            .set_tool_enabled(&name, request.enabled)
            .await?,
    ))
}

async fn update_lazy_tools(
    State(state): State<ServerState>,
    Json(request): Json<LazyToolSettingsUpdate>,
) -> ApiResult<Json<ToolCatalogResponse>> {
    Ok(Json(state.runtime.set_lazy_tool_mode(request.mode).await?))
}

/// Discover workflow definitions without importing or executing their source.
async fn workflows(
    State(state): State<ServerState>,
) -> ApiResult<Json<haarness_protocol::WorkflowListResponse>> {
    if let Some(coordinator) = &state.workflows {
        return Ok(Json(coordinator.catalog().list()));
    }
    let workspace = state.workspace;
    let response =
        tokio::task::spawn_blocking(move || WorkflowCatalog::for_workspace(&workspace).list())
            .await
            .map_err(|error| CoreError::Storage(error.to_string()))?;
    Ok(Json(response))
}

/// Read only the source selected by the catalog. The path itself never comes
/// from the request, which prevents this endpoint becoming an arbitrary file
/// reader.
async fn workflow_source(
    State(state): State<ServerState>,
    Path(invocation): Path<String>,
) -> ApiResult<Json<haarness_protocol::WorkflowSourceResponse>> {
    if let Some(coordinator) = &state.workflows {
        let response = coordinator
            .catalog()
            .read_source(&invocation)
            .map_err(catalog_api_error)?;
        return Ok(Json(response));
    }
    let workspace = state.workspace;
    let response = tokio::task::spawn_blocking(move || {
        WorkflowCatalog::for_workspace(&workspace).read_source(&invocation)
    })
    .await
    .map_err(|error| CoreError::Storage(error.to_string()))?
    .map_err(catalog_api_error)?;
    Ok(Json(response))
}

fn catalog_api_error(error: CatalogError) -> CoreError {
    match error {
        CatalogError::NotFound(invocation) => CoreError::NotFound(format!("workflow {invocation}")),
        CatalogError::Read { path, message } => {
            CoreError::Storage(format!("cannot read workflow {path}: {message}"))
        }
    }
}

fn workflow_coordinator(state: &ServerState) -> Result<Arc<WorkflowCoordinator>, CoreError> {
    state
        .workflows
        .clone()
        .ok_or_else(|| CoreError::Invalid("workflow execution is unavailable".into()))
}

async fn start_workflow(
    State(state): State<ServerState>,
    Json(request): Json<WorkflowStartRequest>,
) -> ApiResult<(StatusCode, Json<WorkflowRun>)> {
    let wait = request.wait;
    let run = workflow_coordinator(&state)?.start(request).await?;
    Ok((
        if wait {
            StatusCode::OK
        } else {
            StatusCode::ACCEPTED
        },
        Json(run),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowRunsQuery {
    thread_id: Option<ThreadId>,
    limit: Option<usize>,
}

async fn list_workflow_runs(
    State(state): State<ServerState>,
    Query(query): Query<WorkflowRunsQuery>,
) -> ApiResult<Json<Vec<WorkflowRun>>> {
    Ok(Json(
        workflow_coordinator(&state)?
            .list(query.thread_id, query.limit.unwrap_or(50).clamp(1, 500))
            .await?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowRunQuery {
    after_sequence: Option<u64>,
}

async fn get_workflow_run(
    State(state): State<ServerState>,
    Path(id): Path<WorkflowId>,
    Query(query): Query<WorkflowRunQuery>,
) -> ApiResult<Json<WorkflowRun>> {
    Ok(Json(
        workflow_coordinator(&state)?
            .get(id, query.after_sequence)
            .await?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowWaitQuery {
    timeout_ms: Option<u64>,
}

async fn wait_workflow_run(
    State(state): State<ServerState>,
    Path(id): Path<WorkflowId>,
    Query(query): Query<WorkflowWaitQuery>,
) -> ApiResult<Json<WorkflowRun>> {
    let timeout = match query.timeout_ms {
        Some(0) => {
            return Err(CoreError::Invalid(
                "workflow wait timeout must be a positive integer".into(),
            )
            .into());
        }
        Some(millis) => Some(Duration::from_millis(millis.min(24 * 60 * 60 * 1_000))),
        None => None,
    };
    Ok(Json(workflow_coordinator(&state)?.wait(id, timeout).await?))
}

async fn cancel_workflow_run(
    State(state): State<ServerState>,
    Path(id): Path<WorkflowId>,
) -> ApiResult<Json<WorkflowRun>> {
    Ok(Json(workflow_coordinator(&state)?.cancel(id).await?))
}

async fn pause_workflow_run(
    State(state): State<ServerState>,
    Path(id): Path<WorkflowId>,
) -> ApiResult<Json<WorkflowRun>> {
    Ok(Json(workflow_coordinator(&state)?.pause(id).await?))
}

async fn resume_workflow_run(
    State(state): State<ServerState>,
    Path(id): Path<WorkflowId>,
) -> ApiResult<Json<WorkflowRun>> {
    Ok(Json(workflow_coordinator(&state)?.resume(id).await?))
}

async fn delete_workflow_run(
    State(state): State<ServerState>,
    Path(id): Path<WorkflowId>,
) -> ApiResult<StatusCode> {
    if workflow_coordinator(&state)?.delete(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(CoreError::NotFound(format!("workflow run {id}")).into())
    }
}

async fn plugins(State(state): State<ServerState>) -> ApiResult<Json<Vec<PluginInfo>>> {
    let Some(registry) = state.plugins else {
        return Ok(Json(Vec::new()));
    };
    let plugins = tokio::task::spawn_blocking(move || registry.list())
        .await
        .map_err(|error| CoreError::Tool(error.to_string()))?
        .map_err(|error| CoreError::Tool(error.to_string()))?;
    Ok(Json(plugins))
}

async fn plugin_action(
    State(state): State<ServerState>,
    Json(request): Json<PluginActionRequest>,
) -> ApiResult<Json<Vec<PluginInfo>>> {
    let registry = state
        .plugins
        .ok_or_else(|| CoreError::Invalid("plugin administration is unavailable".into()))?;
    let plugins = tokio::task::spawn_blocking(move || registry.apply(&request))
        .await
        .map_err(|error| CoreError::Tool(error.to_string()))?
        .map_err(|error| CoreError::Tool(error.to_string()))?;
    Ok(Json(plugins))
}
async fn skills(State(state): State<ServerState>) -> ApiResult<Json<Vec<SkillInfo>>> {
    Ok(Json(state.skills.discover().await?))
}
async fn load_skill(
    State(state): State<ServerState>,
    Path(name): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        json!({"name":name,"instructions":state.skills.load(&name).await?}),
    ))
}
async fn mcp_servers(State(state): State<ServerState>) -> ApiResult<Json<Vec<McpServerInfo>>> {
    Ok(Json(state.mcp.list().await?))
}
async fn configure_mcp(
    State(state): State<ServerState>,
    Path(name): Path<String>,
    Json(config): Json<McpConfigureRequest>,
) -> ApiResult<Json<McpServerInfo>> {
    Ok(Json(state.mcp.configure(&name, config).await?))
}
async fn reconnect_mcp(
    State(state): State<ServerState>,
    Path(name): Path<String>,
) -> ApiResult<Json<McpServerInfo>> {
    Ok(Json(state.mcp.reconnect(&name).await?))
}
async fn disconnect_mcp(
    State(state): State<ServerState>,
    Path(name): Path<String>,
) -> ApiResult<Json<McpServerInfo>> {
    Ok(Json(state.mcp.disconnect(&name).await?))
}
async fn mcp_tools(
    State(state): State<ServerState>,
    Path(name): Path<String>,
) -> ApiResult<Json<Vec<haarness_core::ToolDefinition>>> {
    Ok(Json(state.mcp.tools(&name).await?))
}
async fn call_mcp_tool(
    State(state): State<ServerState>,
    Path((name, tool)): Path<(String, String)>,
    Json(arguments): Json<Value>,
) -> ApiResult<Json<haarness_core::ToolOutput>> {
    Ok(Json(state.mcp.call(&name, &tool, arguments).await?))
}
async fn mcp_resources(
    State(state): State<ServerState>,
    Path(name): Path<String>,
) -> ApiResult<Json<Vec<Value>>> {
    Ok(Json(state.mcp.resources(&name).await?))
}
async fn mcp_resource_templates(
    State(state): State<ServerState>,
    Path(name): Path<String>,
) -> ApiResult<Json<Vec<Value>>> {
    Ok(Json(state.mcp.resource_templates(&name).await?))
}
async fn read_mcp_resource(
    State(state): State<ServerState>,
    Path(name): Path<String>,
    Json(request): Json<Value>,
) -> ApiResult<Json<Value>> {
    let uri = request
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| CoreError::Invalid("resource read requires a string uri".into()))?;
    Ok(Json(state.mcp.read_resource(&name, uri).await?))
}
async fn mcp_prompts(
    State(state): State<ServerState>,
    Path(name): Path<String>,
) -> ApiResult<Json<Vec<Value>>> {
    Ok(Json(state.mcp.prompts(&name).await?))
}
async fn get_mcp_prompt(
    State(state): State<ServerState>,
    Path((name, prompt)): Path<(String, String)>,
    Json(arguments): Json<Value>,
) -> ApiResult<Json<Value>> {
    Ok(Json(state.mcp.get_prompt(&name, &prompt, arguments).await?))
}

#[derive(Deserialize)]
struct ThreadQuery {
    #[serde(default)]
    archived: bool,
}
async fn list_threads(
    State(state): State<ServerState>,
    Query(query): Query<ThreadQuery>,
) -> ApiResult<Json<Vec<Thread>>> {
    Ok(Json(state.runtime.list_threads(query.archived).await?))
}
async fn create_thread(
    State(state): State<ServerState>,
    Json(request): Json<CreateThreadRequest>,
) -> ApiResult<(StatusCode, Json<Thread>)> {
    Ok((
        StatusCode::CREATED,
        Json(state.runtime.create_thread(request).await?),
    ))
}
async fn get_thread(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
) -> ApiResult<Json<Thread>> {
    Ok(Json(state.runtime.get_thread(id).await?))
}
#[derive(Default, Deserialize)]
struct SessionArchiveQuery {
    #[serde(default)]
    sanitize: bool,
}
async fn export_session_archive(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
    Query(query): Query<SessionArchiveQuery>,
) -> ApiResult<Json<SessionArchive>> {
    Ok(Json(
        state
            .runtime
            .export_session_archive(id, query.sanitize)
            .await?,
    ))
}
async fn import_session_archive(
    State(state): State<ServerState>,
    Json(request): Json<SessionArchiveImportRequest>,
) -> ApiResult<Json<SessionArchiveImportResult>> {
    Ok(Json(
        state
            .runtime
            .import_session_archive(request.archive)
            .await?,
    ))
}
async fn update_thread(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
    Json(request): Json<UpdateThreadRequest>,
) -> ApiResult<Json<Thread>> {
    Ok(Json(state.runtime.update_thread(id, request).await?))
}
async fn fork_thread(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
    request: Option<Json<ForkThreadRequest>>,
) -> ApiResult<(StatusCode, Json<Thread>)> {
    let request = request.map(|Json(value)| value).unwrap_or_default();
    Ok((
        StatusCode::CREATED,
        Json(state.runtime.fork_thread_request(id, request).await?),
    ))
}
#[derive(Deserialize)]
struct ItemsQuery {
    after: Option<u64>,
}
async fn list_items(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
    Query(query): Query<ItemsQuery>,
) -> ApiResult<Json<Vec<haarness_protocol::Item>>> {
    Ok(Json(state.runtime.items(id, query.after).await?))
}
async fn list_todos(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
) -> ApiResult<Json<Vec<haarness_protocol::TodoEntry>>> {
    Ok(Json(state.runtime.todos(id).await?))
}
async fn btw(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
    Json(request): Json<BtwRequest>,
) -> ApiResult<Json<BtwResponse>> {
    Ok(Json(state.runtime.btw(id, request).await?))
}
async fn active_turn(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
) -> ApiResult<Json<Option<Turn>>> {
    Ok(Json(state.runtime.active_turn(id).await?))
}
async fn pending_question(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
) -> ApiResult<Json<Option<PendingQuestionInfo>>> {
    Ok(Json(state.runtime.pending_question(id).await?))
}
async fn start_turn(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
    Json(request): Json<StartTurnRequest>,
) -> ApiResult<(StatusCode, Json<Turn>)> {
    if let Some(effort) = request.reasoning_effort.as_deref()
        && effort != "ultracode"
        && !haarness_protocol::REASONING_EFFORTS.contains(&effort)
    {
        return Err(CoreError::Invalid(format!(
            "reasoning_effort must be {}|ultracode, got {effort}",
            haarness_protocol::REASONING_EFFORTS.join("|")
        ))
        .into());
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(state.runtime.start_turn(id, request).await?),
    ))
}
async fn execute_command(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
    Json(request): Json<ExecuteCommandRequest>,
) -> ApiResult<(StatusCode, Json<CommandExecutionResponse>)> {
    let response = crate::command::execute(&state, id, request).await?;
    let status = match &response {
        CommandExecutionResponse::Turn { .. } => StatusCode::ACCEPTED,
        CommandExecutionResponse::WorkflowRun { run }
            if run.status == WorkflowRunStatus::Running =>
        {
            StatusCode::ACCEPTED
        }
        CommandExecutionResponse::WorkflowRun { .. } | CommandExecutionResponse::Text { .. } => {
            StatusCode::OK
        }
    };
    Ok((status, Json(response)))
}
async fn compact(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
    Json(request): Json<CompactRequest>,
) -> ApiResult<Json<haarness_protocol::Item>> {
    if request.summary.is_none() {
        // No caller summary: generate one with the thread's model.
        return Ok(Json(state.runtime.compact_with_llm(id).await?));
    }
    Ok(Json(state.runtime.compact(id, request).await?))
}

#[derive(Deserialize)]
#[serde(untagged)]
enum GoalRequest {
    Action(GoalActionRequest),
    Legacy { goal: Option<String> },
}
async fn get_goal(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
) -> ApiResult<Json<GoalActionResponse>> {
    Ok(Json(
        state
            .runtime
            .goal_action(id, GoalActionRequest::Status)
            .await?,
    ))
}
/// Manage the structured goal harness. The original `{ "goal": string|null }`
/// body remains accepted for backward compatibility.
async fn set_goal(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
    Json(request): Json<GoalRequest>,
) -> ApiResult<Json<GoalActionResponse>> {
    let action = match request {
        GoalRequest::Action(action) => action,
        GoalRequest::Legacy { goal } => match goal.filter(|goal| !goal.trim().is_empty()) {
            Some(goal) => GoalActionRequest::Set {
                goal,
                token_budget: None,
            },
            None => GoalActionRequest::Clear,
        },
    };
    Ok(Json(state.runtime.goal_action(id, action).await?))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoopRequest {
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    interval_seconds: Option<u64>,
    #[serde(default)]
    stop: bool,
}
async fn get_loop(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
) -> ApiResult<Json<Value>> {
    let (_, config) = state.runtime.thread_extras(id).await?;
    Ok(Json(json!({ "loop": config })))
}
/// Start (prompt + interval) or stop a recurring loop; loops expire after 7
/// days as a runaway guard.
async fn set_loop(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
    Json(request): Json<LoopRequest>,
) -> ApiResult<Json<Value>> {
    if request.stop {
        state.runtime.set_loop(id, None).await?;
        return Ok(Json(json!({ "loop": Value::Null })));
    }
    let prompt = request
        .prompt
        .filter(|prompt| !prompt.trim().is_empty())
        .ok_or_else(|| CoreError::Invalid("loop prompt is required".into()))?;
    let interval = request.interval_seconds.unwrap_or(300).clamp(30, 86_400);
    let config = haarness_protocol::LoopConfig {
        prompt,
        interval_seconds: interval,
        expires_at: chrono::Utc::now() + chrono::Duration::days(7),
    };
    state.runtime.set_loop(id, Some(config.clone())).await?;
    Ok(Json(json!({ "loop": config })))
}

#[derive(Deserialize)]
struct ShellRequest {
    command: String,
}
/// Start one shell command through the bash tool + permission path, durably
/// recorded in the thread. No provider turn is started. The command runs in a
/// detached task so a permission prompt can be delivered and answered over the
/// same client's event loop instead of deadlocking the request.
async fn run_shell(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
    Json(request): Json<ShellRequest>,
) -> ApiResult<StatusCode> {
    if request.command.trim().is_empty() {
        return Err(CoreError::Invalid("command must not be empty".into()).into());
    }
    if state.runtime.active_turn(id).await?.is_some() {
        return Err(CoreError::Conflict(
            "cannot run a shell command while a turn is active".into(),
        )
        .into());
    }
    let runtime = Arc::clone(&state.runtime);
    tokio::spawn(async move {
        if let Err(error) = runtime.run_shell(id, request.command).await {
            tracing::warn!(thread_id = %id, %error, "shell command failed");
        }
    });
    Ok(StatusCode::ACCEPTED)
}

#[derive(Default, Deserialize)]
struct RevertRequest {
    sequence: Option<u64>,
}
/// Restore the workspace to the checkpoint before the last mutating turn.
async fn revert_thread(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
    request: Option<Json<RevertRequest>>,
) -> ApiResult<Json<Thread>> {
    let request = request.map(|Json(value)| value).unwrap_or_default();
    Ok(Json(state.runtime.revert(id, request.sequence).await?))
}
async fn unrevert_thread(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
) -> ApiResult<Json<Thread>> {
    Ok(Json(state.runtime.unrevert(id).await?))
}

async fn last_turn_diff(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
) -> ApiResult<Json<Vec<FileDiff>>> {
    Ok(Json(state.runtime.diff_last_turn(id).await?))
}

#[derive(Default, Deserialize)]
struct DiffReviewQuery {
    #[serde(default)]
    summary: bool,
    path: Option<String>,
}

async fn last_turn_review(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
    Query(query): Query<DiffReviewQuery>,
) -> ApiResult<Json<Vec<FileDiff>>> {
    Ok(Json(
        state
            .runtime
            .diff_last_turn_review(id, query.summary, query.path.as_deref())
            .await?,
    ))
}

#[derive(Default, Deserialize)]
struct VcsDiffQuery {
    /// `working` (default) or `branch`.
    mode: Option<String>,
    /// Unified-context lines. Omission requests the backend's full-context
    /// review while retaining its strict byte limits.
    context: Option<u32>,
    #[serde(default)]
    summary: bool,
    path: Option<String>,
}

async fn vcs_diff(
    State(state): State<ServerState>,
    Query(query): Query<VcsDiffQuery>,
) -> ApiResult<Json<Vec<ReviewFile>>> {
    let mode = match query.mode.as_deref().unwrap_or("working") {
        "working" | "worktree" | "git" => crate::vcs::DiffMode::WorkingTree,
        "branch" => crate::vcs::DiffMode::Branch,
        value => {
            return Err(CoreError::Invalid(format!(
                "unknown diff mode '{value}'; expected working or branch"
            ))
            .into());
        }
    };
    let files = crate::vcs::diff_selected(
        &state.workspace,
        mode,
        query.context.unwrap_or(i32::MAX as u32),
        query.summary,
        query.path.as_deref(),
    )
    .await
    .map_err(|error| CoreError::Tool(format!("Git diff failed: {error}")))?;
    Ok(Json(files))
}

#[derive(Deserialize)]
struct FileSearchQuery {
    q: String,
    limit: Option<usize>,
}
/// Fuzzy workspace file search for @-mention completion: ignore-aware walk,
/// subsequence scoring, best matches first.
async fn search_files(
    State(state): State<ServerState>,
    Query(query): Query<FileSearchQuery>,
) -> ApiResult<Json<Vec<String>>> {
    let limit = query.limit.unwrap_or(8).min(50);
    let raw_query = query.q;
    let workspace = state.workspace.clone();
    let results = tokio::task::spawn_blocking(move || {
        if let Some(results) = explicit_path_suggestions(&workspace, &raw_query, limit) {
            return results;
        }
        let needle = raw_query.to_ascii_lowercase();
        let mut scored: Vec<(i64, String)> = Vec::new();
        let walker = ignore::WalkBuilder::new(&workspace)
            .hidden(true)
            .max_depth(Some(12))
            .build();
        for entry in walker.flatten().take(50_000) {
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let Ok(relative) = entry.path().strip_prefix(&workspace) else {
                continue;
            };
            let path = relative.display().to_string();
            if let Some(score) = fuzzy_score(&path.to_ascii_lowercase(), &needle) {
                scored.push((score, path));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        scored
            .into_iter()
            .take(limit)
            .map(|(_, path)| path)
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|error| CoreError::Tool(error.to_string()))?;
    Ok(Json(results))
}

/// Direct `/…`, `~/…`, `./…`, and `../…` completion. The user has explicitly
/// named the location, so reveal one directory level and preserve their path
/// notation in every result. Normal read/media permission checks still run
/// when the selection is actually used.
fn explicit_path_suggestions(
    workspace: &std::path::Path,
    query: &str,
    limit: usize,
) -> Option<Vec<String>> {
    let (expanded, exact_directory_prefix) = if query == "~" || query.starts_with("~/") {
        let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
            return Some(Vec::new());
        };
        let suffix = query.strip_prefix('~').unwrap_or_default();
        (
            home.join(suffix.trim_start_matches('/')),
            (query == "~").then(|| "~/".to_owned()),
        )
    } else if query.starts_with('/') {
        (std::path::PathBuf::from(query), None)
    } else if query == "." || query == ".." || query.starts_with("./") || query.starts_with("../") {
        (
            workspace.join(query),
            match query {
                "." => Some("./".to_owned()),
                ".." => Some("../".to_owned()),
                _ => None,
            },
        )
    } else {
        return None;
    };
    let (directory, fragment, display_prefix) = if let Some(prefix) = exact_directory_prefix {
        (expanded, String::new(), prefix)
    } else if query.ends_with('/') {
        (expanded, String::new(), query.to_owned())
    } else {
        (
            expanded
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| std::path::PathBuf::from("/")),
            expanded
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
            query
                .rfind('/')
                .map_or_else(String::new, |slash| query[..=slash].to_owned()),
        )
    };
    let fragment = fragment.to_ascii_lowercase();
    let Ok(entries) = std::fs::read_dir(&directory) else {
        // This was still an explicit path query. Do not reinterpret a typo or
        // unreadable directory as a fuzzy workspace search.
        return Some(Vec::new());
    };
    let mut matches = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') && !fragment.starts_with('.') {
                return None;
            }
            let score = fuzzy_score(&name.to_ascii_lowercase(), &fragment)?;
            let is_dir = entry.file_type().ok()?.is_dir();
            let mut display = format!("{display_prefix}{name}");
            if is_dir && !display.ends_with('/') {
                display.push('/');
            }
            Some((is_dir, score, display))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    Some(
        matches
            .into_iter()
            .take(limit)
            .map(|(_, _, path)| path)
            .collect(),
    )
}

/// Subsequence match with bonuses for substring hits and file-name hits.
fn fuzzy_score(path: &str, query: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(-i64::try_from(path.len()).unwrap_or(i64::MAX));
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let mut score = 0_i64;
    if let Some(position) = path.find(query) {
        score += 1000 - i64::try_from(position).unwrap_or(0);
    } else {
        // Subsequence: every query char in order.
        let mut chars = query.chars();
        let mut current = chars.next();
        for character in path.chars() {
            if Some(character) == current {
                current = chars.next();
            }
        }
        if current.is_some() {
            return None;
        }
        score += 100;
    }
    if name.starts_with(query) {
        score += 500;
    } else if name.contains(query) {
        score += 250;
    }
    score -= i64::try_from(path.len()).unwrap_or(0);
    Some(score)
}

#[cfg(test)]
mod file_search_tests {
    use super::explicit_path_suggestions;

    struct TestTree(std::path::PathBuf);

    impl Drop for TestTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn dot_paths_browse_relative_directories_and_preserve_the_typed_prefix() {
        let tree = TestTree(
            std::env::temp_dir().join(format!("haarness-dot-path-{}", uuid::Uuid::now_v7())),
        );
        let workspace = tree.0.join("one/two");
        std::fs::create_dir_all(workspace.join("local-dir")).unwrap();
        std::fs::create_dir_all(tree.0.join("one/sibling-dir")).unwrap();
        std::fs::write(workspace.join("local.txt"), "local").unwrap();
        std::fs::write(workspace.join(".secret"), "hidden").unwrap();

        let current = explicit_path_suggestions(&workspace, "./", 20).unwrap();
        assert!(current.contains(&"./local-dir/".to_owned()), "{current:?}");
        assert!(current.contains(&"./local.txt".to_owned()), "{current:?}");
        assert!(!current.iter().any(|path| path.contains(".secret")));
        let directory = current
            .iter()
            .position(|path| path == "./local-dir/")
            .unwrap();
        let file = current
            .iter()
            .position(|path| path == "./local.txt")
            .unwrap();
        assert!(
            directory < file,
            "directories sort before files: {current:?}"
        );

        let parent = explicit_path_suggestions(&workspace, "../sib", 20).unwrap();
        assert_eq!(parent, vec!["../sibling-dir/"]);
        let repeated = explicit_path_suggestions(&workspace, "../../", 20).unwrap();
        assert!(repeated.contains(&"../../one/".to_owned()), "{repeated:?}");

        let hidden = explicit_path_suggestions(&workspace, "./.s", 20).unwrap();
        assert!(hidden.contains(&"./.secret".to_owned()), "{hidden:?}");
        assert!(
            explicit_path_suggestions(&workspace, "../missing/", 20)
                .unwrap()
                .is_empty()
        );
        assert!(explicit_path_suggestions(&workspace, "local", 20).is_none());
    }
}

/// Aggregate token usage and cost across every turn of the thread.
async fn thread_usage(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
) -> ApiResult<Json<Value>> {
    state.runtime.get_thread(id).await?;
    let items = state.runtime.items(id, None).await?;
    let mut usage = usage_totals(&items);
    let mut all = state.runtime.list_threads(false).await?;
    all.extend(state.runtime.list_threads(true).await?);
    let mut ids = std::collections::HashSet::from([id]);
    loop {
        let before = ids.len();
        for thread in &all {
            if thread.is_subagent && thread.parent_id.is_some_and(|parent| ids.contains(&parent)) {
                ids.insert(thread.id);
            }
        }
        if before == ids.len() {
            break;
        }
    }
    ids.remove(&id);
    let count = ids.len();
    let mut child_items = Vec::new();
    for child in ids {
        child_items.extend(state.runtime.items(child, None).await?);
    }
    let mut descendants = usage_totals(&child_items);
    descendants["count"] = json!(count);
    usage["descendants"] = descendants;
    usage["latestInputTokens"] = items
        .iter()
        .rev()
        .find_map(|item| match item.data {
            haarness_protocol::ItemData::Usage { input_tokens, .. } => Some(json!(input_tokens)),
            _ => None,
        })
        .unwrap_or(Value::Null);
    Ok(Json(usage))
}

fn usage_totals(items: &[haarness_protocol::Item]) -> Value {
    let (mut input, mut output, mut cache_read, mut cache_write) = (0_u64, 0_u64, 0_u64, 0_u64);
    let (mut cost, mut priced, mut unpriced) = (0.0, false, false);
    for item in items {
        if let haarness_protocol::ItemData::Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            cost_usd,
            ..
        } = &item.data
        {
            input = input.saturating_add(*input_tokens);
            output = output.saturating_add(*output_tokens);
            cache_read = cache_read.saturating_add(*cache_read_tokens);
            cache_write = cache_write.saturating_add(*cache_write_tokens);
            if let Some(value) = cost_usd {
                cost += value;
                priced = true;
            } else {
                unpriced = true;
            }
        }
    }
    json!({"inputTokens":input,"outputTokens":output,"cacheReadTokens":cache_read,"cacheWriteTokens":cache_write,"costUsd":if priced{Some(cost)}else{None},"costIncomplete":unpriced})
}

#[derive(OpenApi)]
#[openapi(
    paths(
        manage_resource,
        cloud_agents,
        cloud_sessions,
        cloud_chats,
        cloud_schedules
    ),
    components(schemas(
        haarness_protocol::ManagementView,
        haarness_protocol::ManagementEntry,
        haarness_protocol::ManagementField
    ))
)]
struct ManagementApi;

#[utoipa::path(post, path = "/v1/manage/{resource}",
    params(("resource" = String, Path, description = "capabilities, marketplace, assets, providers, mcp, schedules, hooks, memory, profiles, or server-owned UI forms")),
    request_body = Value,
    responses((status = 200, body = Value), (status = 401, description = "Authentication required")))]
async fn manage_resource(
    State(state): State<ServerState>,
    Path(resource): Path<String>,
    Json(input): Json<Value>,
) -> ApiResult<Json<Value>> {
    let registry = state.management.as_ref().ok_or_else(|| {
        haarness_core::CoreError::Tool("Client management is unavailable on this server".into())
    })?;
    Ok(Json(registry.user_request(&resource, input).await?))
}
/// Account-authenticated Cloud agents, using this host's signed-in account.
#[utoipa::path(post, path = "/v1/cloud/agents", request_body = CloudAgentInput, responses((status = 200, body = Value), (status = 401, description = "Authentication required")))]
async fn cloud_agents(
    State(state): State<ServerState>,
    Json(input): Json<CloudAgentInput>,
) -> ApiResult<Json<Value>> {
    manage_resource(
        State(state),
        Path("cloud_agent_manage".into()),
        Json(serde_json::to_value(input).expect("Cloud input serializes")),
    )
    .await
}
/// Account-authenticated Cloud sessions, using this host's signed-in account.
#[utoipa::path(post, path = "/v1/cloud/sessions", request_body = CloudSessionInput, responses((status = 200, body = Value), (status = 401, description = "Authentication required")))]
async fn cloud_sessions(
    State(state): State<ServerState>,
    Json(input): Json<CloudSessionInput>,
) -> ApiResult<Json<Value>> {
    manage_resource(
        State(state),
        Path("cloud_agent_session".into()),
        Json(serde_json::to_value(input).expect("Cloud input serializes")),
    )
    .await
}
/// Account-authenticated Cloud chats, using this host's signed-in account.
#[utoipa::path(post, path = "/v1/cloud/chats", request_body = CloudChatInput, responses((status = 200, body = Value), (status = 401, description = "Authentication required")))]
async fn cloud_chats(
    State(state): State<ServerState>,
    Json(input): Json<CloudChatInput>,
) -> ApiResult<Json<Value>> {
    manage_resource(
        State(state),
        Path("cloud_chat".into()),
        Json(serde_json::to_value(input).expect("Cloud input serializes")),
    )
    .await
}
#[utoipa::path(post, path="/v1/cloud/schedules", request_body=CloudScheduleInput, responses((status=200,body=Value)))]
async fn cloud_schedules(
    State(state): State<ServerState>,
    Json(input): Json<CloudScheduleInput>,
) -> ApiResult<Json<Value>> {
    manage_resource(
        State(state),
        Path("cloud_schedule".into()),
        Json(serde_json::to_value(input).expect("Cloud input serializes")),
    )
    .await
}
async fn get_turn(
    State(state): State<ServerState>,
    Path(id): Path<TurnId>,
) -> ApiResult<Json<Turn>> {
    Ok(Json(state.runtime.turn(id).await?))
}
async fn active_turns(State(state): State<ServerState>) -> ApiResult<Json<Vec<Turn>>> {
    Ok(Json(state.runtime.active_turns().await?))
}
async fn list_background_tasks(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
) -> ApiResult<Json<Vec<BackgroundTaskInfo>>> {
    Ok(Json(state.runtime.background_tasks(id).await?))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BackgroundQuery {
    child_thread_id: Option<ThreadId>,
}
async fn promote_background_tasks(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
    Query(query): Query<BackgroundQuery>,
) -> ApiResult<Json<Vec<BackgroundTaskInfo>>> {
    Ok(Json(
        state
            .runtime
            .promote_subagents(id, query.child_thread_id)
            .await?,
    ))
}
async fn interrupt_turn(
    State(state): State<ServerState>,
    Path(id): Path<TurnId>,
) -> ApiResult<Json<Turn>> {
    Ok(Json(state.runtime.interrupt(id).await?))
}
async fn interrupt_thread_tree(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
) -> ApiResult<Json<Value>> {
    let thread_ids = state.runtime.thread_tree_ids(id).await?;
    let workflows = match &state.workflows {
        Some(workflows) => workflows.cancel_for_launch_threads(&thread_ids).await?,
        None => 0,
    };
    let turns_and_tasks = state.runtime.interrupt_thread_tree(id).await?;
    Ok(Json(json!({
        "threadId": id,
        "workflows": workflows,
        "turnsAndTasks": turns_and_tasks,
    })))
}
/// Every unanswered permission prompt, oldest first — including prompts from
/// child-agent threads, so clients can repopulate after reconnects.
async fn list_permissions(
    State(state): State<ServerState>,
) -> ApiResult<Json<Vec<PendingPermissionInfo>>> {
    Ok(Json(state.runtime.pending_permissions()))
}
async fn list_questions(
    State(state): State<ServerState>,
) -> ApiResult<Json<Vec<PendingQuestionInfo>>> {
    Ok(Json(state.runtime.pending_questions()))
}

/// User input that works mid-turn: with a running turn the message is queued
/// and steered into it; otherwise a normal turn starts.
async fn queue_input(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
    Json(request): Json<QueueInputRequest>,
) -> ApiResult<(StatusCode, Json<QueueInputResponse>)> {
    Ok((
        StatusCode::ACCEPTED,
        Json(state.runtime.queue_input(id, request).await?),
    ))
}

async fn input_queue(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
) -> ApiResult<Json<haarness_protocol::InputQueue>> {
    Ok(Json(state.runtime.input_queue(id).await?))
}
async fn change_input_queue(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
    Json(request): Json<haarness_protocol::InputQueueAction>,
) -> ApiResult<Json<haarness_protocol::InputQueue>> {
    Ok(Json(state.runtime.change_input_queue(id, request).await?))
}

async fn reply_permission(
    State(state): State<ServerState>,
    Path(id): Path<String>,
    Json(reply): Json<PermissionReply>,
) -> ApiResult<StatusCode> {
    state.runtime.reply_permission(&id, reply).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn reply_question(
    State(state): State<ServerState>,
    Path(id): Path<String>,
    Json(reply): Json<QuestionReply>,
) -> ApiResult<StatusCode> {
    state.runtime.reply_question(&id, reply)?;
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Deserialize)]
struct WaitQuery {
    timeout_ms: Option<u64>,
}
async fn wait_turn(
    State(state): State<ServerState>,
    Path(id): Path<TurnId>,
    Query(query): Query<WaitQuery>,
) -> ApiResult<Json<Turn>> {
    let timeout = Duration::from_millis(query.timeout_ms.unwrap_or(30_000).min(300_000));
    let turn = tokio::time::timeout(timeout, async {
        loop {
            let turn = state.runtime.turn(id).await?;
            if !matches!(
                turn.status,
                haarness_protocol::TurnStatus::Queued | haarness_protocol::TurnStatus::Running
            ) {
                break Ok::<_, CoreError>(turn);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(|_| CoreError::Conflict("turn wait timed out".into()))??;
    Ok(Json(turn))
}

async fn events(
    State(state): State<ServerState>,
    axum::Extension(shutdown): axum::Extension<CancellationToken>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // nosniff: Apple's URL loading holds a response until 512 bytes arrive
    // while it sniffs the MIME type, which would stall the first events.
    (
        [(axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff")],
        event_stream(state, shutdown, headers),
    )
}

fn event_stream(
    state: ServerState,
    shutdown: CancellationToken,
    headers: HeaderMap,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let after = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok());
    let root = crate::remote_scope::root(&headers);
    let subscription = state.runtime.events().subscribe(after);
    let output = stream! {
        // A first byte at once: Apple's URL loading, and many proxies, hold a
        // streaming response until body data arrives, so a client resuming
        // from a cursor with nothing to replay would wait for the keepalive.
        yield Ok(Event::default().comment("connected"));
        if subscription.requires_resync { let payload=json!({"schemaVersion":SCHEMA_VERSION,"type":"resync_required"}); yield Ok(Event::default().event("resync_required").data(payload.to_string())); }
        for event in subscription.replay { if crate::remote_scope::event_allowed(&event,root,&state).await { yield Ok(to_sse(&event)); } }
        let mut live=BroadcastStream::new(subscription.receiver);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                event = live.next() => match event {
                    Some(Ok(event)) => if crate::remote_scope::event_allowed(&event,root,&state).await { yield Ok(to_sse(&event)); },
                    Some(Err(_)) => yield Ok(Event::default().event("resync_required").data("{\"type\":\"resync_required\"}")),
                    None => break,
                }
            }
        }
    };
    Sse::new(output).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    )
}
fn to_sse(event: &EventEnvelope) -> Event {
    Event::default()
        .id(event.event_id.to_string())
        .event(&event.event_type)
        .data(serde_json::to_string(event).expect("event serializes"))
}

pub(crate) type ApiResult<T> = Result<T, ApiFailure>;
pub(crate) struct ApiFailure(CoreError);
impl From<CoreError> for ApiFailure {
    fn from(value: CoreError) -> Self {
        Self(value)
    }
}
impl IntoResponse for ApiFailure {
    fn into_response(self) -> Response {
        let (status, code, retryable) = match &self.0 {
            CoreError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found", false),
            CoreError::Conflict(_) => (StatusCode::CONFLICT, "conflict", true),
            CoreError::PermissionDenied(_) => (StatusCode::FORBIDDEN, "permission_denied", false),
            CoreError::PermissionRejected(_) => {
                (StatusCode::FORBIDDEN, "permission_rejected", false)
            }
            CoreError::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid_request", false),
            CoreError::Provider(_) => (StatusCode::BAD_GATEWAY, "provider_error", true),
            CoreError::ProviderBlocked(_) => (StatusCode::BAD_GATEWAY, "provider_blocked", false),
            CoreError::ProviderIncomplete(_) => {
                (StatusCode::BAD_GATEWAY, "provider_incomplete", true)
            }
            CoreError::Storage(_) => (StatusCode::INTERNAL_SERVER_ERROR, "storage_error", true),
            CoreError::Tool(_) => (StatusCode::UNPROCESSABLE_ENTITY, "tool_error", false),
            CoreError::Cancelled => (StatusCode::CONFLICT, "cancelled", false),
        };
        (
            status,
            Json(ApiError {
                code: code.into(),
                message: self.0.to_string(),
                retryable,
            }),
        )
            .into_response()
    }
}
