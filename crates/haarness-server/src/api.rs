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
use futures::Stream;
use haarness_core::{AgentRuntime, CoreError, McpRegistry, SkillRegistry};
use haarness_language::{FormatterStatus, LanguageService, LanguageStatus};
use haarness_protocol::{
    AgentProfile, ApiDoc, ApiError, BTW_STREAM_CAPABILITY, BackgroundTaskInfo, BtwEvent,
    BtwRequest, BtwResponse, CloudAgentInput, CloudChatInput, CloudScheduleInput,
    CloudWorkspaceInput, CommandExecutionResponse, CompactRequest, CreateThreadRequest,
    EventEnvelope, ExecuteCommandRequest, FileDiff, ForkThreadRequest, GoalActionRequest,
    GoalActionResponse, LazyToolSettingsUpdate, McpConfigureRequest, McpServerInfo, PTY_CAPABILITY,
    PendingPermissionInfo, PendingQuestionInfo, PermissionReply, PluginActionRequest, PluginInfo,
    ProviderInfo, PtyCreateRequest, PtyId, PtyInfo, PtyUpdateRequest, QuestionReply,
    QueueInputRequest, QueueInputResponse, ReviewFile, SCHEMA_VERSION, SESSION_ARCHIVE_CAPABILITY,
    ServerInfo, SessionArchive, SessionArchiveImportRequest, SessionArchiveImportResult, SkillInfo,
    StartTurnRequest, Thread, ThreadActivity, ThreadId, ToolCatalogResponse, ToolSettingsUpdate,
    Turn, TurnId, UpdateThreadRequest, WORKSPACE_CLEANUP_CAPABILITY, WORKSPACE_HISTORY_CAPABILITY,
    WorkflowId, WorkflowRun, WorkflowRunStatus, WorkflowStartRequest, WorkspaceCleanupRequest,
    WorkspaceCreateRequest, WorkspaceInfo, WorkspaceListResponse, WorkspaceOperationResponse,
    WorkspaceRefsResponse, WorkspaceRemoveRequest, WorkspaceResetRequest,
    WorkspaceResolveRefRequest, WorkspaceResolveRefResponse,
};
use haarness_pty::{PtyManager, PtySubscription};
use haarness_workflow_catalog::{CatalogError, WorkflowCatalog};
use haarness_workflows::WorkflowCoordinator;
use haarness_workspaces::{WorkspaceError, WorkspaceManager};
use opentelemetry::global;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;
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
    // A background workflow run is owed to the chat that launched it until
    // its result is delivered there; the runtime counts it through this.
    if let Some(workflows) = &workflow_coordinator {
        let workflows = Arc::downgrade(workflows);
        state.runtime.set_background_probe(Arc::new(move |thread| {
            workflows
                .upgrade()
                .map_or(0, |workflows| workflows.pending_deliveries(thread))
        }));
    }
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
        .route("/cloud/workspaces", post(cloud_workspaces))
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
        .route("/threads/{id}/activity", get(thread_activity))
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
        // Room for a side chat's whole thread, so one that has grown too long
        // is refused with a reason (400) rather than for its size alone (413).
        .route(
            "/threads/{id}/btw",
            post(btw).layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route(
            "/threads/{id}/btw/stream",
            post(btw_stream).layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route("/threads/{id}/compact", post(compact))
        .route("/threads/{id}/goal", get(get_goal).post(set_goal))
        .route(
            "/threads/{id}/approval-mode",
            get(get_approval_mode).put(set_approval_mode),
        )
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
        .with_state(state);
    if let Some(host) = host {
        app = app.merge(crate::remote::routes(host, shutdown.clone()).route_layer(
            middleware::from_fn_with_state(
                AuthState::with_access(token, address.ip().is_loopback(), &access),
                require_auth,
            ),
        ));
    }
    // Last, so remote host routes are traced like every other request.
    let app = app.layer(middleware::from_fn(trace_request));
    let server_shutdown = shutdown.clone();
    let task = tokio::spawn(async move {
        // Peer addresses let rejected-request log lines say who was turned away.
        let app = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
        axum::serve(
            axum::serve::ListenerExt::tap_io(listener, keep_connections_alive),
            app,
        )
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

/// Probe an accepted connection once it is idle, so one whose peer vanished
/// (a laptop that slept, a network gone without a word) ends within minutes
/// instead of when TCP gives up: an event stream waiting for that client would
/// hold its queue as long. On Linux, data left unacknowledged ends it too.
fn keep_connections_alive(connection: &mut tokio::net::TcpStream) {
    let socket = socket2::SockRef::from(&*connection);
    let probes = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(60))
        .with_interval(Duration::from_secs(15));
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    let probes = probes.with_retries(4);
    if let Err(error) = socket.set_tcp_keepalive(&probes) {
        tracing::debug!(%error, "could not set TCP keepalive on a connection");
    }
    #[cfg(target_os = "linux")]
    if let Err(error) = socket.set_tcp_user_timeout(Some(Duration::from_secs(120))) {
        tracing::debug!(%error, "could not set TCP_USER_TIMEOUT on a connection");
    }
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

/// Reads a client repeats every few milliseconds while it waits on a turn.
/// Their spans are debug-level, so the default trace filter leaves them out
/// instead of turning every `haarness run` into thousands of one-span traces.
fn polling_route(method: &axum::http::Method, route: Option<&str>) -> bool {
    method == axum::http::Method::GET
        && matches!(
            route,
            Some(
                "/healthz"
                    | "/v1/events"
                    | "/v1/turns/active"
                    | "/v1/turns/{id}"
                    | "/v1/questions"
                    | "/v1/permissions"
            )
        )
}

async fn trace_request(request: Request, next: Next) -> Response {
    let parent = global::get_text_map_propagator(|propagator| {
        propagator.extract(&HeaderExtractor(request.headers()))
    });
    let method = request.method().clone();
    // The route template, never the raw path: paths carry thread ids and share tokens.
    let route = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|path| path.as_str().to_owned());
    let name = route
        .as_deref()
        .map_or_else(|| method.to_string(), |route| format!("{method} {route}"));
    macro_rules! request_span {
        ($span:ident) => {
            tracing::$span!(
                "http.request",
                "otel.kind" = "server",
                "otel.name" = %name,
                "otel.status_code" = tracing::field::Empty,
                "http.request.method" = %method,
                "http.route" = route.as_deref(),
                "url.scheme" = "http",
                "http.response.status_code" = tracing::field::Empty,
            )
        };
    }
    let span = if polling_route(&method, route.as_deref()) {
        request_span!(debug_span)
    } else {
        request_span!(info_span)
    };
    let _ = span.set_parent(parent);
    let response_span = span.clone();
    async move {
        let response = next.run(request).await;
        let status = response.status();
        response_span.record("http.response.status_code", i64::from(status.as_u16()));
        if status.is_server_error() {
            response_span.record("otel.status_code", "ERROR");
        }
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
    Json(openapi_document())
}

/// Every route, as `sdks/generate.py` describes it from the route table and
/// this server's schemas. Only some handlers carry utoipa annotations, so
/// without it the live document would leave out threads, turns and events.
const ROUTE_REFERENCE: &str = include_str!("../../../sdks/openapi.json");

fn openapi_document() -> Value {
    let mut document = ApiDoc::openapi();
    document.merge(AnalyticsApi::openapi());
    document.merge(crate::profiles::ProfilesApi::openapi());
    document.merge(crate::files::FilesApi::openapi());
    document.merge(crate::preview::PreviewApi::openapi());
    document.merge(crate::browser::BrowserApi::openapi());
    document.merge(ManagementApi::openapi());
    document.merge(crate::remote::HostApi::openapi());
    let mut document = serde_json::to_value(document).expect("OpenAPI serializes");
    let reference: Value =
        serde_json::from_str(ROUTE_REFERENCE).expect("sdks/openapi.json is valid JSON");
    // Annotated paths and live schemas win; the reference fills the gaps.
    // What it filled is listed under `x-haarness-reference`, so
    // `sdks/tests/server.py` can export the server's own document as
    // `sdks/schema.json`: generating the SDKs from a document that already
    // holds their output would keep a removed schema forever.
    let mut filled = serde_json::Map::new();
    for (section, field) in [("paths", None), ("components", Some("schemas"))] {
        let (Some(source), Some(target)) = (
            reference
                .get(section)
                .and_then(|value| field.map_or(Some(value), |field| value.get(field)))
                .and_then(Value::as_object),
            document
                .get_mut(section)
                .and_then(|value| match field {
                    Some(field) => value.get_mut(field),
                    None => Some(value),
                })
                .and_then(Value::as_object_mut),
        ) else {
            continue;
        };
        let mut keys = Vec::new();
        for (key, value) in source {
            if !target.contains_key(key) {
                target.insert(key.clone(), value.clone());
                keys.push(Value::String(key.clone()));
            }
        }
        filled.insert(field.unwrap_or(section).to_owned(), Value::Array(keys));
    }
    document["x-haarness-reference"] = Value::Object(filled);
    document
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
    .map_err(|e| ApiFailure::from(CoreError::Invalid(e.to_string())))
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

/// How long a terminal socket that fell behind keeps taking keys before it
/// closes, so a Ctrl+C already on its way still reaches the program.
const PTY_LAG_KEY_GRACE: Duration = Duration::from_millis(250);

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
    let mut replay_control = json!({
        "type": "replay",
        "start": subscription.cursor.start,
        "end": subscription.cursor.end,
        "truncated": subscription.cursor.truncated,
    });
    // Only a truncated replay has modes to restore; older clients ignore it.
    if let Some(modes) = &subscription.cursor.modes {
        replay_control["modes"] = json!(modes);
    }
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
            // Keys first: under a flood of output the reader falls behind at
            // once, and a Ctrl+C sent meanwhile must still reach the shell.
            biased;
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
                    // Keys it already sent go to the shell before the close.
                    // They may still be in flight (Linux PTYs hand a flood
                    // over in small reads, so this lag comes at once), and a
                    // poll that only takes what is ready lost the Ctrl+C.
                    let grace = tokio::time::sleep(PTY_LAG_KEY_GRACE);
                    tokio::pin!(grace);
                    loop {
                        tokio::select! {
                            biased;
                            incoming = socket.recv() => match incoming {
                                Some(Ok(Message::Binary(bytes))) => {
                                    if ptys.write(id, &bytes).is_err() {
                                        break;
                                    }
                                }
                                Some(Ok(_)) => {}
                                Some(Err(_)) | None => break,
                            },
                            () = &mut grace => break,
                        }
                    }
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
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
            "files.preview.v1".into(),
            "mcp".into(),
            "skills".into(),
            "subagents".into(),
            "btw.v1".into(),
            BTW_STREAM_CAPABILITY.into(),
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
            haarness_protocol::APPROVAL_MODE_CAPABILITY.into(),
            haarness_protocol::APPROVAL_YOLO_CAPABILITY.into(),
            haarness_protocol::THREAD_CREATE_ID_CAPABILITY.into(),
            haarness_protocol::THREAD_DIRECTORY_CAPABILITY.into(),
            haarness_protocol::RESOURCE_MENTIONS_CAPABILITY.into(),
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
        // Chrome needs both the broker and its settings.
        .chain(
            (state.browser.is_some() && state.management.is_some())
                .then(|| "browser-extension.v1".into()),
        )
        // Auto asks a reviewer model here (`auto_review.enabled`).
        .chain(
            state
                .runtime
                .auto_review_enabled()
                .then(|| haarness_protocol::APPROVAL_REVIEW_CAPABILITY.into()),
        )
        .chain(state.management.as_ref().into_iter().flat_map(|_| {
            [
                "input-settings.v1".into(),
                "browser-settings.v1".into(),
                "usage-statistics.v1".into(),
            ]
        }))
        .collect(),
        workspace: Some(state.workspace.display().to_string()),
        git_branch: git_branch(&state.workspace),
        project_trusted: Some(state.project_trusted),
        default_provider,
        default_model,
        default_approval_mode: Some(state.runtime.approval_mode_default()),
        default_reasoning_effort: state.runtime.default_reasoning_effort(),
        pid: Some(std::process::id()),
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
/// An ordinary read waits a little for lists still loading; an explicit
/// refresh (a model picker opening, a key just saved) for all of them.
async fn providers(State(state): State<ServerState>) -> Json<Vec<ProviderInfo>> {
    Json(
        crate::models::catalog(
            &state.runtime,
            crate::models::Wait::Read(crate::models::WAIT),
        )
        .await,
    )
}
async fn refresh_providers(State(state): State<ServerState>) -> Json<Vec<ProviderInfo>> {
    Json(crate::models::catalog(&state.runtime, crate::models::Wait::All).await)
}
async fn agents(State(state): State<ServerState>) -> Json<Vec<AgentProfile>> {
    Json(state.runtime.agent_profiles())
}
/// Authoritative, deduplicated slash-command registry. Resolution is
/// case-insensitive: reserved built-ins, configured prompts, then workflows.
async fn commands(
    State(state): State<ServerState>,
    Query(query): Query<ChatListQuery>,
) -> ApiResult<Json<Vec<haarness_protocol::CommandInfo>>> {
    let folder = match query.thread_id {
        Some(thread) => crate::command::chat_folder_of(&state, thread).await?,
        None => None,
    };
    Ok(Json(crate::command::registry(&state, folder.as_deref())))
}

/// Lists that also show what one chat's own folder holds (its workflows).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatListQuery {
    /// The chat the list is for: one with a folder of its own also gets that
    /// folder's workflows.
    thread_id: Option<ThreadId>,
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
    Query(query): Query<ChatListQuery>,
) -> ApiResult<Json<haarness_protocol::WorkflowListResponse>> {
    if let Some(thread) = query.thread_id {
        let folder = crate::command::chat_folder_of(&state, thread).await?;
        if folder.is_some() {
            let state = state.clone();
            return tokio::task::spawn_blocking(move || {
                Json(crate::command::workflow_list(&state, folder.as_deref()))
            })
            .await
            .map_err(|error| CoreError::Storage(error.to_string()).into());
        }
    }
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
        .map_err(|error| ApiFailure::plain(CoreError::Tool(error.to_string())))?;
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
/// Whom a server list is for: with a chat (or, before one exists, an agent)
/// named, each server that chat cannot reach says so (`turnedOff`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpServersQuery {
    #[serde(default)]
    thread_id: Option<ThreadId>,
    #[serde(default)]
    agent: Option<String>,
}
async fn mcp_servers(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(query): Query<McpServersQuery>,
) -> ApiResult<Json<Vec<McpServerInfo>>> {
    let mut servers = state.mcp.list().await?;
    // A published chat's viewers are offered no resources to mention.
    if (query.thread_id.is_some() || query.agent.is_some())
        && crate::remote_scope::root(&headers).is_none()
    {
        let mut agents = vec![query.agent];
        // A message sent while a turn runs follows both the running turn's
        // agent and the chat's (see `queue_input`), so its list does too.
        if let Some(running) = query
            .thread_id
            .and_then(|thread| state.runtime.running_agent(thread))
        {
            agents.extend([None, Some(running)]);
        }
        let mut policies = Vec::new();
        for agent in &agents {
            policies.push(
                state
                    .runtime
                    .resource_mention_policy(query.thread_id, agent.as_deref())
                    .await?,
            );
        }
        for server in &mut servers {
            if policies
                .iter()
                .any(|policy| policy.turned_off(&server.name))
            {
                server.turned_off = Some(true);
            }
        }
    }
    Ok(Json(servers))
}
async fn configure_mcp(
    State(state): State<ServerState>,
    Path(name): Path<String>,
    Json(config): Json<McpConfigureRequest>,
) -> ApiResult<Json<McpServerInfo>> {
    Ok(Json(
        state
            .mcp
            .configure(&name, config)
            .await
            .map_err(ApiFailure::plain)?,
    ))
}
async fn reconnect_mcp(
    State(state): State<ServerState>,
    Path(name): Path<String>,
) -> ApiResult<Json<McpServerInfo>> {
    Ok(Json(
        state
            .mcp
            .reconnect(&name)
            .await
            .map_err(ApiFailure::plain)?,
    ))
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
/// A side question whose answer streams as it is written (see [`BtwEvent`]).
/// A missing chat or a bad request fails before the stream starts; a client
/// that goes away ends the provider request.
async fn btw_stream(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
    Json(request): Json<BtwRequest>,
) -> ApiResult<Response> {
    state.runtime.check_btw(id, &request).await?;
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let runtime = Arc::clone(&state.runtime);
    let token = cancel.clone();
    let work = tokio::spawn(async move {
        let progress = sender.clone();
        let result = runtime
            .btw_streaming(id, request, token, move |event| {
                let _ = progress.send(event);
            })
            .await;
        let _ = sender.send(match result {
            Ok(answer) => BtwEvent::Done {
                answer: answer.answer,
                input_tokens: answer.input_tokens,
                output_tokens: answer.output_tokens,
            },
            Err(error) => {
                let (_, code, retryable) = failure_parts(&error);
                BtwEvent::Error {
                    code: code.into(),
                    message: error.to_string(),
                    retryable,
                }
            }
        });
    });
    let guard = (AbortOnDrop(work), cancel.drop_guard());
    let output = stream! {
        let _guard = guard;
        // A first byte at once, as on /v1/events: some clients and proxies
        // hold a streaming response until its body starts.
        yield Ok::<_, Infallible>(Event::default().comment("connected"));
        while let Some(event) = receiver.recv().await {
            let last = matches!(event, BtwEvent::Done { .. } | BtwEvent::Error { .. });
            yield Ok(Event::default().data(serde_json::to_string(&event).unwrap_or_default()));
            if last {
                break;
            }
        }
    };
    Ok((
        [
            (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (
                axum::http::HeaderName::from_static("x-accel-buffering"),
                "no",
            ),
        ],
        Sse::new(output).keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keepalive"),
        ),
    )
        .into_response())
}
async fn active_turn(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
) -> ApiResult<Json<Option<Turn>>> {
    Ok(Json(state.runtime.active_turn(id).await?))
}
async fn thread_activity(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
) -> ApiResult<Json<ThreadActivity>> {
    Ok(Json(state.runtime.thread_activity(id).await?))
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
    headers: HeaderMap,
    Json(mut request): Json<StartTurnRequest>,
) -> ApiResult<(StatusCode, Json<Turn>)> {
    // A published chat follows its own Auto/Review setting; a remote client
    // cannot approve a turn's requests wholesale.
    if request.auto_approve && crate::remote_scope::root(&headers).is_some() {
        return Err(CoreError::PermissionDenied(
            "A shared chat cannot start a turn that approves its own requests".into(),
        )
        .into());
    }
    if let Some(effort) = request.reasoning_effort.as_deref()
        && effort != "ultracode"
        && effort != haarness_protocol::MODEL_DEFAULT_EFFORT
        && !haarness_protocol::REASONING_EFFORTS.contains(&effort)
    {
        return Err(CoreError::Invalid(format!(
            "reasoning_effort must be {}|{}|ultracode, got {effort}",
            haarness_protocol::MODEL_DEFAULT_EFFORT,
            haarness_protocol::REASONING_EFFORTS.join("|")
        ))
        .into());
    }
    let resources = std::mem::take(&mut request.resources);
    let agents = vec![
        state
            .runtime
            .message_agent(request.agent.as_deref(), request.command.as_deref()),
    ];
    let turn = admit(resources.is_empty(), async move {
        let _reading = read_mentions(
            &state,
            &headers,
            Mentioning {
                chat: id,
                request: request.request_id,
                agents,
            },
            resources,
            &mut request.text,
            &mut request.attachments,
        )
        .await?;
        state
            .runtime
            .start_turn(id, request)
            .await
            .map_err(|error| match error {
                // A chat busy elsewhere is said as a sentence, without the
                // category the error type adds ("conflict: …").
                CoreError::Conflict(message) => {
                    ApiFailure(CoreError::Conflict(message.clone()), Some(message))
                }
                other => other.into(),
            })
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(turn)))
}
async fn execute_command(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
    headers: HeaderMap,
    Json(mut request): Json<ExecuteCommandRequest>,
) -> ApiResult<(StatusCode, Json<CommandExecutionResponse>)> {
    let resources = std::mem::take(&mut request.resources);
    let mut text = None;
    if !resources.is_empty() {
        // Mentions join the command's argument text, as attached files do.
        text = Some(match request.arguments.take() {
            None => String::new(),
            Some(Value::String(text)) => text,
            Some(_) => {
                return Err(CoreError::Invalid(
                    "MCP resource mentions need string command arguments".into(),
                )
                .into());
            }
        });
    }
    let agents = vec![
        state
            .runtime
            .message_agent(request.agent.as_deref(), Some(&request.name)),
    ];
    // Boxed: a command's future is large.
    let response = admit(
        resources.is_empty(),
        Box::pin(async move {
            let mut reading = None;
            if let Some(mut text) = text {
                reading = read_mentions(
                    &state,
                    &headers,
                    Mentioning {
                        chat: id,
                        request: request.request_id,
                        agents,
                    },
                    resources,
                    &mut text,
                    &mut request.attachments,
                )
                .await?;
                request.arguments = Some(Value::String(text));
            }
            let response = crate::command::execute(&state, id, request).await?;
            drop(reading);
            Ok(response)
        }),
    )
    .await?;
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
/// The chat's Auto/Review setting, which the server applies for every client.
async fn get_approval_mode(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
) -> ApiResult<Json<haarness_protocol::ApprovalModeInfo>> {
    Ok(Json(state.runtime.approval_mode(id).await?))
}
async fn set_approval_mode(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
    Json(request): Json<haarness_protocol::SetApprovalModeRequest>,
) -> ApiResult<Json<haarness_protocol::ApprovalModeInfo>> {
    Ok(Json(state.runtime.set_approval_mode(id, request).await?))
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
        .ok_or_else(|| CoreError::Invalid("a loop needs a prompt".into()))?;
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
            // The error can quote the command, which can carry a credential.
            tracing::warn!(
                thread_id = %id,
                error = %crate::hooks::log_text(&error.to_string(), 300),
                "shell command failed"
            );
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
        state.project_trusted,
    )
    .await
    .map_err(|error| CoreError::Tool(format!("Git diff failed: {error}")))?;
    Ok(Json(files))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileSearchQuery {
    q: String,
    limit: Option<usize>,
    /// Search a chat's own folder: results are relative to it.
    thread_id: Option<ThreadId>,
}
/// Dependency and build folders file search leaves out of a folder outside
/// Git unless the query names them (`node_modules/react`); a listing still
/// shows them. In a Git work tree, `.gitignore` says what to leave out, as
/// files committed under such names (`cmd/dist/main.go`) are a project's.
const SEARCH_SKIPS: [&str; 8] = [
    "node_modules",
    ".venv",
    "venv",
    "target",
    "dist",
    "build",
    "__pycache__",
    ".next",
];

/// Fuzzy workspace file search for @-mention completion: ignore-aware walk,
/// subsequence scoring, best matches first.
async fn search_files(
    State(state): State<ServerState>,
    Query(query): Query<FileSearchQuery>,
) -> ApiResult<Json<Vec<String>>> {
    let limit = query.limit.unwrap_or(8).min(50);
    let raw_query = query.q;
    // A chat with a folder of its own searches that folder alone.
    let workspace = match query.thread_id {
        Some(thread) => state
            .runtime
            .chat_folder(thread)
            .await?
            .unwrap_or_else(|| state.workspace.clone()),
        None => state.workspace.clone(),
    };
    // The chat home stands alone, whatever folders hold it: neither their
    // ignore files nor a repository among them (a home folder kept in Git)
    // change what a search there finds.
    let chat_home = state
        .runtime
        .is_chat_home()
        .then(|| state.workspace.clone());
    if let Some(results) =
        explicit_path_suggestions(&workspace, &raw_query, limit, PATH_LOOKUPS).await
    {
        return Ok(Json(results));
    }
    let results = tokio::task::spawn_blocking(move || {
        let needle = raw_query.to_ascii_lowercase();
        let mut scored: Vec<(i64, String)> = Vec::new();
        // `.gitignore` and `.ignore` apply in a folder outside Git too, and
        // there a dependency or build folder is not searched unless the
        // query names it, so a big one cannot use up the walk before the
        // files a person means.
        let named = needle.clone();
        let top = workspace.clone();
        let alone = crate::tools::within_chat_home(&workspace, chat_home.as_deref());
        let in_git = workspace
            .ancestors()
            .take_while(|folder| {
                !alone || crate::tools::within_chat_home(folder, chat_home.as_deref())
            })
            .any(|folder| folder.join(".git").exists());
        let walker = ignore::WalkBuilder::new(&workspace)
            .hidden(true)
            .require_git(false)
            .parents(!alone)
            .max_depth(Some(12))
            .filter_entry(move |entry| {
                !(!in_git
                    && entry.depth() > 0
                    && entry.file_type().is_some_and(|kind| kind.is_dir())
                    && entry.file_name().to_str().is_some_and(|name| {
                        SEARCH_SKIPS.contains(&name) && !named.contains(&name.to_ascii_lowercase())
                    })
                    // Nor in a repository inside the searched folder.
                    && !entry
                        .path()
                        .ancestors()
                        .skip(1)
                        .take_while(|folder| *folder != top)
                        .any(|folder| folder.join(".git").exists()))
            })
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

/// How explicit path completion looks at the disk. A folder on a network
/// mount that cannot be reached right now, or a link into one, can hold a
/// lookup for minutes, so each gets so long and no longer; tests stand in
/// lookups that never answer.
#[derive(Clone, Copy)]
struct PathLookups {
    /// A folder's entries, each with its type as the listing gives it, so
    /// only a link needs looking at on its own.
    list: fn(&std::path::Path) -> Option<Listing>,
    /// Whether a link leads to a folder.
    follow: fn(&std::path::Path) -> bool,
    /// How long the folder may take to list.
    folder: Duration,
    /// How long its links may take to resolve, all together, once it has.
    links: Duration,
}

const PATH_LOOKUPS: PathLookups = PathLookups {
    list: list_folder,
    follow: std::path::Path::is_dir,
    folder: Duration::from_millis(1500),
    links: Duration::from_millis(500),
};

/// How many of a folder's links are resolved at once.
const LINKS_AT_ONCE: usize = 4;

/// An entry's type, as a folder listing gives it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Folder,
    Link,
    Other,
}

/// A folder's entries: each name and its type.
type Listing = Vec<(String, EntryKind)>;

fn list_folder(folder: &std::path::Path) -> Option<Listing> {
    let entries = std::fs::read_dir(folder).ok()?;
    Some(
        entries
            .filter_map(Result::ok)
            .map(|entry| {
                let kind = match entry.file_type() {
                    Ok(kind) if kind.is_symlink() => EntryKind::Link,
                    Ok(kind) if kind.is_dir() => EntryKind::Folder,
                    Ok(_) => EntryKind::Other,
                    // No type in the listing and none on a second look:
                    // followed like a link, as a path would be.
                    Err(_) => EntryKind::Link,
                };
                (entry.file_name().to_string_lossy().into_owned(), kind)
            })
            .collect(),
    )
}

/// Direct `/…`, `~/…`, `./…`, and `../…` completion. The user has explicitly
/// named the location, so reveal one directory level and preserve their path
/// notation in every result. Normal read/media permission checks still run
/// when the selection is actually used. A folder that does not list within
/// `lookups.folder` completes to nothing, and a link that has not resolved
/// within `lookups.links` after that is listed by its name, without the `/`
/// of a folder.
async fn explicit_path_suggestions(
    workspace: &std::path::Path,
    query: &str,
    limit: usize,
    lookups: PathLookups,
) -> Option<Vec<String>> {
    use futures::StreamExt as _;
    let (expanded, exact_directory_prefix) = if query == "~" || query.starts_with("~/") {
        let Some(home) = haarness_protocol::home_dir() else {
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
    let listed = {
        let folder = directory.clone();
        crate::lookups::within(
            "folder",
            &directory,
            tokio::time::Instant::now() + lookups.folder,
            move || (lookups.list)(&folder),
        )
        .await
    };
    let Some(Some(entries)) = listed else {
        // This was still an explicit path query. Do not reinterpret a typo, an
        // unreadable directory or one that does not answer as a fuzzy
        // workspace search.
        return Some(Vec::new());
    };
    let matches = entries
        .into_iter()
        .filter_map(|(name, kind)| {
            if name.starts_with('.') && !fragment.starts_with('.') {
                return None;
            }
            let score = fuzzy_score(&name.to_ascii_lowercase(), &fragment)?;
            Some((name, kind, score))
        })
        .collect::<Vec<_>>();
    // A link is followed, so a linked folder completes with its `/`.
    let deadline = tokio::time::Instant::now() + lookups.links;
    let links = matches
        .iter()
        .filter(|(_, kind, _)| *kind == EntryKind::Link)
        .map(|(name, _, _)| name.clone())
        .collect::<Vec<_>>();
    let linked_folders = futures::stream::iter(links)
        .map(|name| {
            let link = directory.join(&name);
            async move {
                let path = link.clone();
                crate::lookups::within("link", &link, deadline, move || (lookups.follow)(&path))
                    .await
                    .unwrap_or(false)
                    .then_some(name)
            }
        })
        .buffer_unordered(LINKS_AT_ONCE)
        .filter_map(std::future::ready)
        .collect::<std::collections::HashSet<_>>()
        .await;
    let mut matches = matches
        .into_iter()
        .map(|(name, kind, score)| {
            let is_dir = match kind {
                EntryKind::Folder => true,
                EntryKind::Link => linked_folders.contains(&name),
                EntryKind::Other => false,
            };
            let mut display = format!("{display_prefix}{name}");
            if is_dir && !display.ends_with('/') {
                display.push('/');
            }
            (is_dir, score, display)
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
/// Desktop's `fileScore` (`apps/desktop/src/renderer/prompt-context.ts`)
/// scores the files it keeps while a search runs the same way: change both,
/// and the scores both tests pin.
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
    use super::{
        Listing, PATH_LOOKUPS, PathLookups, explicit_path_suggestions, fuzzy_score, list_folder,
    };
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    /// The scores Desktop's port is pinned to as well.
    #[test]
    fn file_scores_match_the_desktop_port() {
        let score = |path: &str, query: &str| fuzzy_score(&path.to_ascii_lowercase(), query);
        assert_eq!(score("setup.md", "s"), Some(1492));
        assert_eq!(score("style.css", "st"), Some(1491));
        assert_eq!(score("setup.md", "st"), Some(92));
        assert_eq!(score("src/Setup.MD", "setup"), Some(1484));
        assert_eq!(score("docs/guide.md", ""), Some(-13));
        assert_eq!(score("readme.md", "q"), None);
        assert_eq!(score("É/é.md", "é"), Some(1489));
        assert_eq!(score("É.md", "é"), None);
    }

    struct TestTree(std::path::PathBuf);

    impl Drop for TestTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn dot_paths_browse_relative_directories_and_preserve_the_typed_prefix() {
        let tree = TestTree(
            std::env::temp_dir().join(format!("haarness-dot-path-{}", uuid::Uuid::now_v7())),
        );
        let workspace = tree.0.join("one/two");
        std::fs::create_dir_all(workspace.join("local-dir")).unwrap();
        std::fs::create_dir_all(tree.0.join("one/sibling-dir")).unwrap();
        std::fs::write(workspace.join("local.txt"), "local").unwrap();
        std::fs::write(workspace.join(".secret"), "hidden").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("local-dir", workspace.join("linked-dir")).unwrap();
            std::os::unix::fs::symlink("local.txt", workspace.join("linked.txt")).unwrap();
            std::os::unix::fs::symlink("missing", workspace.join("dangling")).unwrap();
        }
        let suggest =
            |query: &'static str| explicit_path_suggestions(&workspace, query, 20, PATH_LOOKUPS);

        let current = suggest("./").await.unwrap();
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
        // A link to a folder completes as one; a link to a file, or to
        // nothing, as a file.
        #[cfg(unix)]
        assert_eq!(
            current,
            [
                "./local-dir/",
                "./linked-dir/",
                "./dangling",
                "./local.txt",
                "./linked.txt"
            ]
        );

        let parent = suggest("../sib").await.unwrap();
        assert_eq!(parent, vec!["../sibling-dir/"]);
        let repeated = suggest("../../").await.unwrap();
        assert!(repeated.contains(&"../../one/".to_owned()), "{repeated:?}");

        let hidden = suggest("./.s").await.unwrap();
        assert!(hidden.contains(&"./.secret".to_owned()), "{hidden:?}");
        assert!(suggest("../missing/").await.unwrap().is_empty());
        assert!(suggest("local").await.is_none());
    }

    #[cfg(unix)]
    static LINK_RELEASED: AtomicBool = AtomicBool::new(false);
    static FOLDER_RELEASED: AtomicBool = AtomicBool::new(false);

    fn wait_for(released: &AtomicBool) {
        while !released.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Follows a link as the server does, except that one named `hung`
    /// hangs until the test lets it go, as a link into a network mount that
    /// went away can.
    #[cfg(unix)]
    fn follow_hangs(link: &Path) -> bool {
        if link.file_name().is_some_and(|name| name == "hung") {
            wait_for(&LINK_RELEASED);
        }
        link.is_dir()
    }

    /// Lists a folder as the server does, except that one named
    /// `hung-folder` hangs until the test lets it go.
    fn list_hangs(folder: &Path) -> Option<Listing> {
        if folder.file_name().is_some_and(|name| name == "hung-folder") {
            wait_for(&FOLDER_RELEASED);
        }
        list_folder(folder)
    }

    /// `query`'s completion, asked `asks` times in a row on another thread,
    /// so a completion that holds its thread fails the test instead of
    /// hanging it: the answers that came within 5 s each, and how long each
    /// took. `release` then lets the lookups that hang go.
    fn complete(
        workspace: PathBuf,
        query: &'static str,
        lookups: PathLookups,
        asks: usize,
        release: &AtomicBool,
    ) -> Vec<(Vec<String>, Duration)> {
        let (sender, answers) = std::sync::mpsc::channel();
        let asking = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            for _ in 0..asks {
                let started = Instant::now();
                let found =
                    runtime.block_on(explicit_path_suggestions(&workspace, query, 20, lookups));
                let _ = sender.send((found.unwrap(), started.elapsed()));
            }
        });
        let found = std::iter::from_fn(|| answers.recv_timeout(Duration::from_secs(5)).ok())
            .take(asks)
            .collect();
        release.store(true, Ordering::SeqCst);
        asking.join().unwrap();
        found
    }

    #[cfg(unix)]
    #[test]
    fn a_link_that_does_not_resolve_in_time_is_listed_by_its_name() {
        let tree = TestTree(
            std::env::temp_dir().join(format!("haarness-hung-link-{}", uuid::Uuid::now_v7())),
        );
        std::fs::create_dir_all(tree.0.join("local-dir")).unwrap();
        std::fs::write(tree.0.join("local.txt"), "local").unwrap();
        std::os::unix::fs::symlink("local-dir", tree.0.join("hung")).unwrap();
        std::os::unix::fs::symlink("local-dir", tree.0.join("linked-dir")).unwrap();
        let lookups = PathLookups {
            follow: follow_hangs,
            links: Duration::from_millis(200),
            ..PATH_LOOKUPS
        };
        // Asked twice while the link hangs: the first answer waits for it as
        // long as links may take, the second not at all.
        let answers = complete(tree.0.clone(), "./", lookups, 2, &LINK_RELEASED);
        assert_eq!(answers.len(), 2, "completion waited on a link that hangs");
        let (first, took) = &answers[0];
        assert_eq!(
            first,
            &["./local-dir/", "./linked-dir/", "./hung", "./local.txt"]
        );
        assert!(*took < Duration::from_secs(2), "{took:?}");
        let (second, took) = &answers[1];
        assert_eq!(second, first);
        assert!(*took < Duration::from_millis(150), "{took:?}");
    }

    #[test]
    fn a_folder_that_does_not_answer_completes_to_nothing_in_time() {
        let tree = TestTree(
            std::env::temp_dir().join(format!("haarness-hung-folder-{}", uuid::Uuid::now_v7())),
        );
        std::fs::create_dir_all(tree.0.join("hung-folder/inside")).unwrap();
        let lookups = PathLookups {
            list: list_hangs,
            folder: Duration::from_millis(200),
            ..PATH_LOOKUPS
        };
        let answers = complete(
            tree.0.clone(),
            "./hung-folder/",
            lookups,
            1,
            &FOLDER_RELEASED,
        );
        let [(found, took)] = answers.as_slice() else {
            panic!("completion waited on a folder that does not answer");
        };
        assert!(found.is_empty(), "{found:?}");
        assert!(*took < Duration::from_secs(2), "{took:?}");
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
    // The one context number every client shows, measured as compaction does.
    let context = state.runtime.context_usage(id).await?;
    usage["context"] = json!({
        "tokens": context.estimate.tokens,
        "reportedTokens": context.estimate.reported,
        "estimatedTokens": context.estimate.estimated,
        "window": context.window,
        "compactAt": context.compact_at,
    });
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
        cloud_workspaces,
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
    Ok(Json(
        registry
            .user_request(&resource, input)
            .await
            .map_err(ApiFailure::plain)?,
    ))
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
/// Account-authenticated Cloud workspaces, using this host's signed-in account.
#[utoipa::path(post, path = "/v1/cloud/workspaces", request_body = CloudWorkspaceInput, responses((status = 200, body = Value), (status = 401, description = "Authentication required")))]
async fn cloud_workspaces(
    State(state): State<ServerState>,
    Json(input): Json<CloudWorkspaceInput>,
) -> ApiResult<Json<Value>> {
    manage_resource(
        State(state),
        Path("cloud_workspace".into()),
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
/// `resume=true` is the interactive Stop: steering the agent has not read yet
/// gets an answer instead of being dropped. Programmatic stops omit it.
#[derive(Deserialize)]
struct InterruptQuery {
    #[serde(default)]
    resume: bool,
}
async fn interrupt_turn(
    State(state): State<ServerState>,
    Path(id): Path<TurnId>,
    Query(query): Query<InterruptQuery>,
) -> ApiResult<Json<Turn>> {
    Ok(Json(state.runtime.interrupt_with(id, query.resume).await?))
}
async fn interrupt_thread_tree(
    State(state): State<ServerState>,
    Path(id): Path<ThreadId>,
    Query(query): Query<InterruptQuery>,
) -> ApiResult<Json<Value>> {
    let thread_ids = state.runtime.thread_tree_ids(id).await?;
    let workflows = match &state.workflows {
        Some(workflows) => workflows.cancel_for_launch_threads(&thread_ids).await?,
        None => 0,
    };
    let turns_and_tasks = state
        .runtime
        .interrupt_thread_tree_with(id, query.resume)
        .await?;
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
    headers: HeaderMap,
    Json(mut request): Json<QueueInputRequest>,
) -> ApiResult<(StatusCode, Json<QueueInputResponse>)> {
    let resources = std::mem::take(&mut request.resources);
    // Steered, it joins the running turn; queued (or steered as that turn
    // ends) it runs as a turn of its own under the chat's agent. Both must
    // allow a mention.
    let mut agents = vec![None];
    if let Some(running) = state.runtime.running_agent(id) {
        agents.push(Some(running));
    }
    // Boxed: the admitted work is too large a future to keep on the stack.
    let response = admit(
        resources.is_empty(),
        Box::pin(async move {
            let _reading = read_mentions(
                &state,
                &headers,
                Mentioning {
                    chat: id,
                    request: request.request_id,
                    agents,
                },
                resources,
                &mut request.text,
                &mut request.attachments,
            )
            .await?;
            Ok(state.runtime.queue_input(id, request).await?)
        }),
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

/// The MCP resources a message to chat `id` mentions, read into it when it
/// arrives, so a queued message holds what was read; the chat's rules under
/// each agent the message may run with keep from it what they keep from the
/// model. A published chat's viewers cannot read the owner's MCP servers, so
/// a mention from one is refused.
///
/// The returned gate is held until the message is admitted: a retry of the
/// same request meanwhile waits for it, then finds the message taken and
/// reads nothing again.
async fn read_mentions(
    state: &ServerState,
    headers: &HeaderMap,
    message: Mentioning,
    resources: Vec<haarness_protocol::ResourceMention>,
    text: &mut String,
    attachments: &mut Vec<String>,
) -> ApiResult<Option<Reading>> {
    if resources.is_empty() {
        return Ok(None);
    }
    if crate::remote_scope::root(headers).is_some() {
        return Err(CoreError::PermissionDenied(
            "A shared chat cannot read this computer's MCP resources".into(),
        )
        .into());
    }
    let Mentioning {
        chat: id,
        request,
        agents,
    } = message;
    let reading = Reading::start(id, request).await;
    if state.runtime.request_admitted(id, request).await? {
        return Ok(Some(reading));
    }
    let mut policies = Vec::new();
    for agent in &agents {
        policies.push(
            state
                .runtime
                .resource_mention_policy(Some(id), agent.as_deref())
                .await?,
        );
    }
    crate::mcp_resources::read_mentions(
        state.mcp.as_ref(),
        &state.workspace,
        resources,
        &|server, uri| policies.iter().any(|policy| policy.denies(server, uri)),
        crate::mcp_resources::MENTION_BUDGET,
        text,
        attachments,
    )
    .await?;
    Ok(Some(reading))
}

/// The message whose mentions are read: its chat, its request, and the
/// agents it may run under (`None` for the chat's), each of whose rules
/// must allow a mention.
struct Mentioning {
    chat: ThreadId,
    request: haarness_protocol::RequestId,
    agents: Vec<Option<String>>,
}

type ReadingKey = (ThreadId, haarness_protocol::RequestId);
/// The messages whose mentions are being read and admitted, by chat and
/// request.
static READING: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashMap<ReadingKey, Arc<tokio::sync::Mutex<()>>>>,
> = std::sync::LazyLock::new(Default::default);

/// One message's reads and admission, held from the first read until it is
/// admitted; a retry of the same request waits for it.
struct Reading {
    key: ReadingKey,
    _held: tokio::sync::OwnedMutexGuard<()>,
}
impl Reading {
    async fn start(id: ThreadId, request: haarness_protocol::RequestId) -> Self {
        let gate = READING.lock().entry((id, request)).or_default().clone();
        Self {
            key: (id, request),
            _held: gate.lock_owned().await,
        }
    }
}
impl Drop for Reading {
    fn drop(&mut self) {
        let mut reading = READING.lock();
        // The map's and this one's: nobody else waits.
        if reading
            .get(&self.key)
            .is_some_and(|gate| Arc::strong_count(gate) <= 2)
        {
            reading.remove(&self.key);
        }
    }
}

/// Admits a message. One that mentions resources is read and admitted on a
/// task of its own (`plain` is false), so a client that goes away while the
/// reads run cannot lose the message; its retry with the same request id
/// finds it.
async fn admit<T: Send + 'static>(
    plain: bool,
    work: impl std::future::Future<Output = ApiResult<T>> + Send + 'static,
) -> ApiResult<T> {
    if plain {
        return work.await;
    }
    tokio::spawn(work).await.map_err(|error| {
        ApiFailure::from(CoreError::Tool(format!(
            "the message could not be admitted: {error}"
        )))
    })?
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
    headers: HeaderMap,
    Json(reply): Json<PermissionReply>,
) -> ApiResult<StatusCode> {
    // A published chat's viewers may not let agents into the owner's Chrome.
    if crate::remote_scope::root(&headers).is_some()
        && state
            .runtime
            .pending_permissions()
            .iter()
            .any(|pending| pending.permission_id == id && pending.explicit)
    {
        return Err(ApiFailure(
            CoreError::PermissionDenied("explicit approval".into()),
            Some("Answer this on the computer running Chrome.".into()),
        ));
    }
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
    // X-Accel-Buffering: nginx (and proxies that follow it) in front of a
    // server would otherwise collect the stream before passing it on.
    (
        [
            (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (
                axum::http::HeaderName::from_static("x-accel-buffering"),
                "no",
            ),
        ],
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
        // The connection takes every event as it is published, apart from
        // writing to its client: a client that reads slowly keeps its place
        // (see PendingEvents) instead of falling out of the bus's queue.
        let pending = Arc::new(parking_lot::Mutex::new(Pending::default()));
        let wake = Arc::new(tokio::sync::Notify::new());
        let _pump = AbortOnDrop(tokio::spawn(pump_events(
            subscription,
            crate::remote_scope::EventScope::new(root),
            Arc::clone(&state.runtime),
            Arc::clone(&pending),
            Arc::clone(&wake),
        )));
        loop {
            let next = {
                let mut waiting = pending.lock();
                match waiting.events.pop() {
                    Some(event) => Some(event),
                    // A queue that gave up ends the response once its
                    // `resync_required` is out: the client reconnects from
                    // its cursor.
                    None if waiting.closed || waiting.events.given_up() => break,
                    None => None,
                }
            };
            match next {
                Some(event) => {
                    let mut sent = Event::default().event(&event.name).data(event.data);
                    if let Some(id) = event.id {
                        sent = sent.id(id.to_string());
                    }
                    yield Ok(sent);
                }
                None => tokio::select! {
                    () = shutdown.cancelled() => break,
                    () = wake.notified() => {}
                },
            }
        }
    };
    Sse::new(output).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    )
}
/// What waits for one event-stream connection.
struct Pending {
    events: haarness_core::PendingEvents,
    /// The bus is gone.
    closed: bool,
}
impl Default for Pending {
    fn default() -> Self {
        Self {
            events: haarness_core::PendingEvents::new(haarness_core::PENDING_EVENTS_LIMIT),
            closed: false,
        }
    }
}

/// Aborts its task when dropped: a connection's pump ends with it.
struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Moves one connection's events from the bus into its queue as they are
/// published, the replay first: those its client may see (a published
/// chat's stream leaves out other chats' before they could break its runs
/// of deltas), joined while they wait.
async fn pump_events(
    subscription: haarness_core::EventSubscription,
    mut scope: crate::remote_scope::EventScope,
    runtime: Arc<AgentRuntime>,
    pending: Arc<parking_lot::Mutex<Pending>>,
    wake: Arc<tokio::sync::Notify>,
) {
    // False once the queue gave up: this connection is over.
    let queue = |event: EventEnvelope| {
        let pushed = pending.lock().events.push(event);
        wake.notify_one();
        match pushed {
            Ok(()) => true,
            Err(dropped) => {
                tracing::info!(
                    skipped = dropped,
                    "event stream fell behind; its client resyncs"
                );
                false
            }
        }
    };
    // The replay is bounded by what the bus keeps, and the client has read
    // none of it yet, so it never ends the connection: one larger than the
    // queue holds is answered as a cursor older than the bus is, with
    // `resync_required`, and the stream goes on with what is published next.
    let mut replay = subscription.replay.into_iter();
    while let Some(event) = replay.next() {
        if !scope.allows(&event, &runtime).await {
            continue;
        }
        let pushed = pending.lock().events.push_replay(event);
        wake.notify_one();
        if let Err(dropped) = pushed {
            tracing::info!(
                skipped = dropped.saturating_add(replay.len()),
                "event stream replay is larger than a connection holds; its client resyncs"
            );
            break;
        }
    }
    drop(replay);
    let mut receiver = subscription.receiver;
    loop {
        match receiver.recv().await {
            Ok(event) => {
                if scope.allows(&event, &runtime).await && !queue(event) {
                    return;
                }
            }
            // The bus let go of events for this connection: what waits is
            // older than the gap, and what follows newer, so neither goes out.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                let dropped = pending.lock().events.give_up();
                wake.notify_one();
                tracing::info!(
                    skipped = skipped.saturating_add(u64::try_from(dropped).unwrap_or(u64::MAX)),
                    "event stream fell behind; its client resyncs"
                );
                return;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
    pending.lock().closed = true;
    wake.notify_one();
}

pub(crate) type ApiResult<T> = Result<T, ApiFailure>;
pub(crate) struct ApiFailure(CoreError, Option<String>);
impl ApiFailure {
    /// Management answers people directly: a failed action reads as its own
    /// sentence, without the `tool error:` category prefix.
    fn plain(error: CoreError) -> Self {
        let message = match &error {
            CoreError::Tool(message) => Some(message.clone()),
            _ => None,
        };
        Self(error, message)
    }
}
impl From<CoreError> for ApiFailure {
    fn from(value: CoreError) -> Self {
        Self(value, None)
    }
}
/// The status, code and retryable flag a request failing with `error` answers with.
fn failure_parts(error: &CoreError) -> (StatusCode, &'static str, bool) {
    match error {
        CoreError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found", false),
        CoreError::Conflict(_) => (StatusCode::CONFLICT, "conflict", true),
        CoreError::PermissionDenied(_) => (StatusCode::FORBIDDEN, "permission_denied", false),
        CoreError::PermissionRejected(_) => (StatusCode::FORBIDDEN, "permission_rejected", false),
        CoreError::ReviewBlocked(_) => (StatusCode::FORBIDDEN, "review_blocked", false),
        CoreError::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid_request", false),
        CoreError::Provider(_) => (StatusCode::BAD_GATEWAY, "provider_error", true),
        CoreError::ProviderBlocked(_) => (StatusCode::BAD_GATEWAY, "provider_blocked", false),
        CoreError::ProviderIncomplete(_) => (StatusCode::BAD_GATEWAY, "provider_incomplete", true),
        CoreError::Storage(_) => (StatusCode::INTERNAL_SERVER_ERROR, "storage_error", true),
        CoreError::Tool(_) => (StatusCode::UNPROCESSABLE_ENTITY, "tool_error", false),
        CoreError::Cancelled => (StatusCode::CONFLICT, "cancelled", false),
    }
}
impl IntoResponse for ApiFailure {
    fn into_response(self) -> Response {
        let (status, code, retryable) = failure_parts(&self.0);
        (
            status,
            Json(ApiError {
                code: code.into(),
                message: self.1.unwrap_or_else(|| self.0.to_string()),
                retryable,
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod openapi_tests {
    use serde_json::Value;

    #[test]
    fn the_live_document_describes_every_route_and_resolves_every_schema() {
        let document = super::openapi_document();
        let paths = document["paths"].as_object().unwrap();
        for path in [
            "/healthz",
            "/openapi.json",
            "/v1/threads",
            "/v1/threads/{id}/turns",
            "/v1/events",
            "/v1/providers",
            "/v1/permissions",
            "/v1/profiles",
        ] {
            assert!(paths.contains_key(path), "{path} missing");
        }
        let reference: Value = serde_json::from_str(super::ROUTE_REFERENCE).unwrap();
        assert!(paths.len() >= reference["paths"].as_object().unwrap().len());
        // Fill-ins are listed; annotated routes are the server's own.
        let filled = |section: &str, key: &str| {
            document["x-haarness-reference"][section]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == key)
        };
        assert!(filled("paths", "/v1/threads"));
        assert!(!filled("paths", "/v1/profiles"));
        assert!(filled("schemas", "SessionArchiveImportResult"));
        assert!(!filled("schemas", "Thread"));
        let schemas = document["components"]["schemas"].as_object().unwrap();
        let text = document.to_string();
        for reference in text.split("\"#/components/schemas/").skip(1) {
            let name = &reference[..reference.find('"').unwrap()];
            assert!(
                schemas.contains_key(name),
                "{name} is referenced but missing"
            );
        }
    }
}

#[cfg(test)]
mod failure_tests {
    use super::{ApiFailure, CoreError};
    use axum::response::IntoResponse as _;

    async fn message(failure: ApiFailure) -> serde_json::Value {
        let response = failure.into_response();
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn management_failures_read_as_sentences() {
        let plain = message(ApiFailure::plain(CoreError::Tool(
            "/tmp/missing does not exist".into(),
        )))
        .await;
        assert_eq!(plain["message"], "/tmp/missing does not exist");
        assert_eq!(plain["code"], "tool_error");
        let invalid = message(ApiFailure::plain(CoreError::Invalid("bad".into()))).await;
        assert_eq!(invalid["message"], "invalid request: bad");
        let elsewhere = message(ApiFailure::from(CoreError::Tool("boom".into()))).await;
        assert_eq!(elsewhere["message"], "tool error: boom");
    }
}

#[cfg(test)]
mod connection_tests {
    #[tokio::test]
    async fn accepted_connections_are_probed_once_idle() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = tokio::net::TcpStream::connect(listener.local_addr().unwrap());
        let (accepted, _client) = tokio::join!(listener.accept(), client);
        let (mut accepted, _) = accepted.unwrap();
        super::keep_connections_alive(&mut accepted);
        let socket = socket2::SockRef::from(&accepted);
        assert!(socket.keepalive().unwrap());
        assert_eq!(
            socket.tcp_keepalive_time().unwrap(),
            std::time::Duration::from_secs(60)
        );
    }
}
