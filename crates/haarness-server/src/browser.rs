//! Conversation-owned browser broker. Only an authenticated Desktop host owns Chromium;
//! the server owns permissions, request lifetimes, file paths and network sockets.
use crate::api::ServerState;
use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{
        Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures::{SinkExt, StreamExt};
use haarness_core::{CoreError, CoreResult, Tool, ToolContext, ToolDefinition, ToolOutput};
use haarness_protocol::ThreadId;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::{Mutex as AsyncMutex, mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;
const MAX_BYTES: usize = 5 * 1024 * 1024;
const MAX_HOST_MESSAGE: usize = 8 * 1024 * 1024;
const RECOVERY: &str = "Browser disconnected or replaced. Open this conversation in Desktop, list its tabs and inspect the page before repeating any mutation; its outcome may be unknown.";
fn error(message: impl ToString) -> CoreError {
    CoreError::Tool(message.to_string())
}
pub struct BrowserBroker {
    policy: Arc<dyn Fn() -> CoreResult<crate::browser_policy::BrowserPolicy> + Send + Sync>,
    hosts: Mutex<HashMap<ThreadId, Arc<Host>>>,
    drafts: Mutex<HashMap<uuid::Uuid, Arc<Host>>>,
}
struct Host {
    policy_supported: bool,
    lease: String,
    outbound: mpsc::Sender<Value>,
    pending: Mutex<HashMap<String, oneshot::Sender<Value>>>,
    state: Mutex<Value>,
    closed: CancellationToken,
    tunnels: Mutex<HashMap<String, Arc<Tunnel>>>,
}
struct Tunnel {
    read: AsyncMutex<OwnedReadHalf>,
    write: AsyncMutex<OwnedWriteHalf>,
}
impl Default for BrowserBroker {
    fn default() -> Self { Self { hosts: Mutex::new(HashMap::new()), drafts: Mutex::new(HashMap::new()), policy: Arc::new(|| Ok(crate::browser_policy::BrowserPolicy::default())) } }
}
impl BrowserBroker {
    pub fn with_policy(policy: Arc<dyn Fn() -> CoreResult<crate::browser_policy::BrowserPolicy> + Send + Sync>) -> Arc<Self> {
        Arc::new(Self { policy, ..Self::default() })
    }

    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn host(&self, thread: ThreadId, lease: Option<&str>) -> CoreResult<Arc<Host>> {
        self.hosts
            .lock()
            .get(&thread)
            .filter(|h| !h.closed.is_cancelled() && lease.is_none_or(|id| h.lease == id))
            .cloned()
            .ok_or_else(|| error(RECOVERY))
    }
    fn draft(&self, id: uuid::Uuid, lease: &str) -> CoreResult<Arc<Host>> {
        self.drafts.lock().get(&id)
            .filter(|host| !host.closed.is_cancelled() && host.lease == lease)
            .cloned().ok_or_else(|| error(RECOVERY))
    }
    fn adopt(&self, id: uuid::Uuid, lease: &str, thread: ThreadId) -> CoreResult<()> {
        let host = self.draft(id, lease)?;
        let mut hosts = self.hosts.lock();
        if let Some((owner, _)) = hosts.iter().find(|(_, value)| Arc::ptr_eq(value, &host)) {
            return if *owner == thread { Ok(()) } else { Err(error("This browser already belongs to another conversation")) };
        }
        if hosts.get(&thread).is_some_and(|value| !value.closed.is_cancelled()) {
            return Err(error("This conversation already has a browser"));
        }
        hosts.insert(thread, host);
        // Retain the draft lease as a transport alias until the socket closes.
        // Existing page connections keep working while agent tools use the thread.
        Ok(())
    }
    async fn request(
        host: &Arc<Host>,
        command: Value,
        cancel: &CancellationToken,
    ) -> CoreResult<Value> {
        if host.closed.is_cancelled() {
            return Err(error(RECOVERY));
        }
        let id = uuid::Uuid::now_v7().to_string();
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = host.pending.lock();
            if pending.len() >= 64 {
                return Err(error("Too many browser commands are pending"));
            }
            pending.insert(id.clone(), tx);
        }
        let result = async {
            host.outbound.send(json!({"type":"command","id":id,"command":command})).await.map_err(|_| error(RECOVERY))?;
            tokio::select! {
                biased;
                () = cancel.cancelled() => Err(CoreError::Cancelled),
                () = host.closed.cancelled() => Err(error(RECOVERY)),
                result = tokio::time::timeout(Duration::from_secs(60), rx) => match result {
                    Ok(Ok(value)) if value["type"] == "success" => Ok(value["result"].clone()),
                    Ok(Ok(value)) => Err(error(value["message"].as_str().unwrap_or("Browser operation failed"))),
                    _ => Err(error("Browser command timed out. Inspect the page before retrying; the action may already have run."))
                }
            }
        }.await;
        host.pending.lock().remove(&id);
        if result.is_err() {
            let _ = host.outbound.try_send(json!({"type":"cancel","id":id}));
        }
        result
    }
    async fn prepare(
        &self,
        thread: ThreadId,
        method: &str,
        mut args: Value,
        cancel: &CancellationToken,
    ) -> CoreResult<Value> {
        let policy = (self.policy)()?;
        policy.ensure_enabled()?;
        let host = self.host(thread, None)?;
        if !host.policy_supported && policy != crate::browser_policy::BrowserPolicy::default() {
            return Err(error("Update Desktop and reconnect this conversation to enforce browser site settings."));
        }
        let mut action = args.clone();
        action["type"] = json!(method);
        let generation = if let Some(id) = args["tabID"].as_str() {
            let state = host.state.lock();
            Some(state["tabs"].as_array().and_then(|tabs| tabs.iter().find(|tab| tab["id"]==id)).ok_or_else(|| error("Tab does not belong to this conversation or is closed. Call browser_tabs_list."))?["generation"].clone())
        } else {
            None
        };
        let result = Self::request(
            &host,
            json!({"action":action,"files":[],"inspect":true}),
            cancel,
        )
        .await?;
        let target = result["value"].clone();
        if !target["resources"].is_array() || !target["key"].is_string() {
            return Err(error("Invalid browser target metadata"));
        }
        let category = match method { "files.upload" | "files.drop" => "uploads", "files.get" => "downloads", _ => "browsing" };
        let mut remember = Vec::new();
        if policy.remember_approvals {
            for resource in target["resources"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                if policy.decision(method, resource) == haarness_core::PermissionDecision::Ask {
                    let origin = url::Url::parse(resource).ok().map(|url| url.origin().ascii_serialization()).unwrap_or_else(|| resource.into());
                    remember.push(format!("{category}:{origin}"));
                }
            }
            // Inventory has no site; it still needs an explicit, rememberable grant.
            if remember.is_empty() && target["resources"].as_array().is_some_and(Vec::is_empty)
                && policy.decision(method, "inventory") == haarness_core::PermissionDecision::Ask { remember.push(format!("{category}:inventory")); }
        }
        args["__browser"] = json!({"lease":host.lease,"generation":generation,"target":target,"policy":policy,"remember":remember});
        Ok(args)
    }
}

pub fn routes() -> Router<ServerState> {
    Router::new()
        .route("/threads/{id}/browser/host", get(attach))
        .route("/threads/{id}/browser/tunnel/{action}", post(tunnel))
        .route("/threads/{id}/browser/tool", post(call_browser_tool))
        .route("/browser/drafts/{id}/host", get(attach_draft))
        .route("/browser/drafts/{id}/tunnel/{action}", post(draft_tunnel))
        .route("/browser/drafts/{id}/adopt", post(adopt_draft))
}

async fn attach_draft(
    State(state): State<ServerState>,
    Path(id): Path<uuid::Uuid>,
    axum::Extension(shutdown): axum::Extension<CancellationToken>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some(broker) = state.browser else {
        return (StatusCode::NOT_IMPLEMENTED, "Browser unavailable").into_response();
    };
    upgrade.max_message_size(MAX_HOST_MESSAGE)
        .on_upgrade(move |socket| serve_host(broker, None, Some(id), socket, shutdown, true))
        .into_response()
}
#[derive(serde::Deserialize)]
struct AdoptDraft { thread: ThreadId, lease: String }
async fn adopt_draft(
    State(state): State<ServerState>, Path(id): Path<uuid::Uuid>, Json(input): Json<AdoptDraft>,
) -> crate::api::ApiResult<Json<Value>> {
    state.runtime.get_thread(input.thread).await?;
    state.browser.ok_or_else(|| error("Browser unavailable"))?.adopt(id, &input.lease, input.thread)?;
    Ok(Json(json!({"adopted":true})))
}
async fn draft_tunnel(
    State(state): State<ServerState>, Path((id, action)): Path<(uuid::Uuid, String)>, Json(input): Json<Value>,
) -> Response {
    let host = state.browser.ok_or_else(|| error("Browser unavailable"))
        .and_then(|broker| broker.draft(id, input["lease"].as_str().ok_or_else(|| error("Missing browser lease"))?));
    let result = match host { Ok(host) => host_tunnel(host, &action, input).await, Err(error) => Err(error) };
    match result { Ok(value) => Json(value).into_response(), Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response() }
}
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct BrowserToolRequest {
    pub name: String,
    pub arguments: Value,
}
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserToolResponse {
    pub output: String,
    pub metadata: Value,
    pub is_error: bool,
}
#[derive(utoipa::OpenApi)]
#[openapi(paths(call_browser_tool))]
pub struct BrowserApi;

/// Run a browser tool through the same permissions and transcript as a model call.
#[utoipa::path(post, path="/v1/threads/{id}/browser/tool", operation_id="call_browser_tool", params(("id"=String,Path)), request_body=BrowserToolRequest, responses((status=200,body=BrowserToolResponse)))]
async fn call_browser_tool(
    State(state): State<ServerState>,
    Path(thread): Path<ThreadId>,
    Json(input): Json<BrowserToolRequest>,
) -> crate::api::ApiResult<Json<BrowserToolResponse>> {
    let name = input.name.as_str();
    if !name.starts_with("browser_") {
        return Err(CoreError::Invalid("Expected browser tool".into()).into());
    }
    let cancel = CancellationToken::new();
    let _guard = cancel.clone().drop_guard();
    let result = state
        .runtime
        .run_direct_tool(thread, name, input.arguments, cancel)
        .await?;
    Ok(Json(BrowserToolResponse {
        output: result.output.text,
        metadata: result.output.metadata,
        is_error: result.is_error,
    }))
}

#[derive(serde::Deserialize)]
struct HostOptions { version: Option<u8> }
async fn attach(
    State(state): State<ServerState>,
    Query(options): Query<HostOptions>,
    Path(thread): Path<ThreadId>,
    axum::Extension(shutdown): axum::Extension<CancellationToken>,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !matches!(options.version.unwrap_or(1), 1 | 2) {
        return (StatusCode::BAD_REQUEST, "Unsupported browser host version").into_response();
    }
    if state.runtime.get_thread(thread).await.is_err() {
        return (StatusCode::NOT_FOUND, "Conversation not found").into_response();
    }
    let Some(broker) = state.browser else {
        return (StatusCode::NOT_IMPLEMENTED, "Browser unavailable").into_response();
    };
    upgrade
        .max_message_size(MAX_HOST_MESSAGE)
        .on_upgrade(move |socket| serve_host(broker, Some(thread), None, socket, shutdown, options.version == Some(2)))
        .into_response()
}
async fn serve_host(
    broker: Arc<BrowserBroker>,
    thread: Option<ThreadId>,
    draft: Option<uuid::Uuid>,
    socket: WebSocket,
    shutdown: CancellationToken,
    policy_supported: bool,
) {
    let (tx, mut rx) = mpsc::channel(128);
    let host = Arc::new(Host {
        policy_supported,
        lease: uuid::Uuid::now_v7().to_string(),
        outbound: tx,
        pending: Mutex::new(HashMap::new()),
        state: Mutex::new(json!({"tabs":[],"focusedTabID":null})),
        closed: shutdown.child_token(),
        tunnels: Mutex::new(HashMap::new()),
    });
    if let Some(thread) = thread {
        if let Some(old) = broker.hosts.lock().insert(thread, host.clone()) { old.closed.cancel(); }
    } else if let Some(id) = draft {
        let mut drafts = broker.drafts.lock();
        if drafts.len() >= 64 && !drafts.contains_key(&id) { return; }
        if let Some(old) = drafts.insert(id, host.clone()) { old.closed.cancel(); }
    }
    let (mut writer, mut reader) = socket.split();
    if writer
        .send(Message::Text(
            json!({"type":"attached","version":1,"lease":host.lease})
                .to_string()
                .into(),
        ))
        .await
        .is_ok()
    {
        loop {
            tokio::select! {
                ()=host.closed.cancelled()=>break,
                outgoing=rx.recv()=>{let Some(value)=outgoing else {break};if writer.send(Message::Text(value.to_string().into())).await.is_err(){break}},
                incoming=reader.next()=>{match incoming {
                    Some(Ok(Message::Text(text)))=>{
                        let Ok(value)=serde_json::from_str::<Value>(&text) else {break};
                        match value["type"].as_str() {
                            Some("state")=>{if valid_state(&value["state"]){*host.state.lock()=value["state"].clone()}else{break}},
                            Some("result")=>{if let Some(id)=value["id"].as_str(){if let Some(tx)=host.pending.lock().remove(id){let _=tx.send(value["outcome"].clone());}}},
                            Some("ping")=>{let _=host.outbound.try_send(json!({"type":"pong"}));},
                            _=>break
                        }
                    },
                    Some(Ok(Message::Ping(bytes)))=>{if writer.send(Message::Pong(bytes)).await.is_err(){break}},
                    _=>break
                }}
            }
        }
    }
    host.closed.cancel();
    host.pending.lock().clear();
    host.tunnels.lock().clear();
    {
        let mut hosts = broker.hosts.lock();
        hosts.retain(|_, value| !Arc::ptr_eq(value, &host));
    }
    broker.drafts.lock().retain(|_, value| !Arc::ptr_eq(value, &host));
    let _ = writer.close().await;
}
fn valid_state(state: &Value) -> bool {
    state["tabs"].as_array().is_some_and(|tabs| {
        tabs.len() <= 32
            && tabs.iter().all(|tab| {
                tab["id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("tab_") && id.len() == 40)
                    && tab["generation"].is_u64()
                    && tab["url"].as_str().is_some_and(|url| url.len() <= 16384)
            })
    })
}
async fn tunnel(
    State(state): State<ServerState>,
    Path((thread, action)): Path<(ThreadId, String)>,
    Json(input): Json<Value>,
) -> Response {
    let result = tunnel_request(state, thread, &action, input).await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}
async fn tunnel_request(
    state: ServerState,
    thread: ThreadId,
    action: &str,
    input: Value,
) -> CoreResult<Value> {
    let broker = state.browser.ok_or_else(|| error("Browser unavailable"))?;
    let lease = input["lease"]
        .as_str()
        .ok_or_else(|| error("Missing browser lease"))?;
    let host = broker.host(thread, Some(lease))?;
    host_tunnel(host, action, input).await
}
async fn host_tunnel(host: Arc<Host>, action: &str, input: Value) -> CoreResult<Value> {
    if action == "open" {
        if host.tunnels.lock().len() >= 128 {
            return Err(error("Browser connection limit reached"));
        }
        let hostname = input["target"]["host"]
            .as_str()
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 253
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._:%-".contains(&b))
            })
            .ok_or_else(|| error("Invalid tunnel host"))?;
        let port = input["target"]["port"]
            .as_u64()
            .filter(|p| *p > 0 && *p <= 65535)
            .ok_or_else(|| error("Invalid tunnel port"))? as u16;
        let socket = tokio::select! {()=host.closed.cancelled()=>return Err(error(RECOVERY)),value=tokio::time::timeout(Duration::from_secs(10),TcpStream::connect((hostname,port)))=>value.map_err(error)?.map_err(error)?};
        socket.set_nodelay(true).map_err(error)?;
        let (read, write) = socket.into_split();
        let id = uuid::Uuid::now_v7().to_string();
        host.tunnels.lock().insert(
            id.clone(),
            Arc::new(Tunnel {
                read: AsyncMutex::new(read),
                write: AsyncMutex::new(write),
            }),
        );
        return Ok(json!(id));
    }
    let id = input["id"]
        .as_str()
        .ok_or_else(|| error("Missing tunnel ID"))?;
    if action == "close" {
        host.tunnels.lock().remove(id);
        return Ok(Value::Null);
    }
    let socket = host
        .tunnels
        .lock()
        .get(id)
        .cloned()
        .ok_or_else(|| error("Tunnel closed"))?;
    tokio::select! {
        ()=host.closed.cancelled()=>Err(error(RECOVERY)),
        result=tokio::time::timeout(Duration::from_secs(30),async {
            match action {
                "read"=>{let mut bytes=vec![0u8;64*1024];let mut reader=socket.read.lock().await;let size=match tokio::time::timeout(Duration::from_secs(25),reader.read(&mut bytes)).await {Ok(result)=>result.map_err(error)?,Err(_)=>return Ok(json!({"data":"","eof":false}))};Ok(json!({"data":STANDARD.encode(&bytes[..size]),"eof":size==0}))},
                "write"=>{let data=input["data"].as_str().filter(|s|s.len()<=90_000).ok_or_else(||error("Invalid tunnel chunk"))?;let bytes=STANDARD.decode(data).map_err(error)?;if bytes.len()>64*1024{return Err(error("Tunnel chunk exceeds limit"));}let mut writer=socket.write.lock().await;writer.write_all(&bytes).await.map_err(error)?;if input["end"]==true{writer.shutdown().await.map_err(error)?;}Ok(Value::Null)},
                _=>Err(error("Unknown tunnel action"))
            }
        })=>result.map_err(error)?
    }
}

pub fn tools(broker: Arc<BrowserBroker>) -> Vec<Arc<dyn Tool>> {
    let catalog: Vec<Value> =
        serde_json::from_str(include_str!("browser-tools.json")).expect("compiled browser catalog");
    catalog
        .into_iter()
        .map(|entry| {
            Arc::new(BrowserTool {
                broker: broker.clone(),
                method: entry["method"].as_str().unwrap().to_owned(),
                definition: serde_json::from_value(entry).unwrap(),
            }) as Arc<dyn Tool>
        })
        .collect()
}
struct BrowserTool {
    broker: Arc<BrowserBroker>,
    method: String,
    definition: ToolDefinition,
}
#[async_trait]
impl Tool for BrowserTool {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }
    fn side_effectful(&self) -> bool {
        true
    }
    async fn prepare_arguments(
        &self,
        args: Value,
        thread: ThreadId,
        cancel: CancellationToken,
    ) -> CoreResult<Value> {
        self.broker
            .prepare(thread, &self.method, args, &cancel)
            .await
    }
    fn permission_constraint(&self, args: &Value, operation: &str) -> Option<haarness_core::PermissionDecision> {
        let resource = operation.strip_prefix("browser:")?.split_once(':')?.1;
        let policy: crate::browser_policy::BrowserPolicy = serde_json::from_value(args["__browser"]["policy"].clone()).ok()?;
        let decision = policy.decision(&self.method, resource);
        if policy.remember_approvals && decision == haarness_core::PermissionDecision::Ask { None } else { Some(decision) }
    }
    async fn execute(&self, mut args: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        let prepared = args
            .as_object_mut()
            .and_then(|args| args.remove("__browser"))
            .ok_or_else(|| error("Browser permission preparation is missing"))?;
        let check_policy = || -> CoreResult<()> {
            let current = (self.broker.policy)()?;
            current.ensure_enabled()?;
            if serde_json::to_value(current).map_err(error)? != prepared["policy"] {
                return Err(error("Browser settings changed while this operation was pending. Inspect the page and request fresh authorization."));
            }
            Ok(())
        };
        check_policy()?;
        let host = self
            .broker
            .host(context.thread_id, prepared["lease"].as_str())?;
        let mut files = Vec::new();
        let mut bytes_total = 0;
        if matches!(self.method.as_str(), "files.upload" | "files.drop") {
            for raw in args["paths"]
                .as_array()
                .ok_or_else(|| error("Missing upload paths"))?
            {
                let raw = raw.as_str().ok_or_else(|| error("Invalid upload path"))?;
                let path = haarness_core::resolve_permission_path(&context.workspace, raw);
                let root = context.workspace.canonicalize().map_err(error)?;
                if !path.starts_with(&root)
                    && !context
                        .authorized_external_paths
                        .iter()
                        .any(|p| path == *p || path.starts_with(p))
                {
                    return Err(CoreError::PermissionDenied(
                        "Upload path was not authorized".into(),
                    ));
                }
                let file = tokio::fs::File::open(&path).await.map_err(error)?;
                if !file.metadata().await.map_err(error)?.is_file() {
                    return Err(error("Upload must be a regular file"));
                }
                let mut bytes = Vec::new();
                file.take((MAX_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)
                    .await
                    .map_err(error)?;
                bytes_total += bytes.len();
                if bytes_total > MAX_BYTES {
                    return Err(error("Browser uploads exceed 5 MiB total"));
                }
                files.push(json!({"id":format!("file_{}",uuid::Uuid::new_v4()),"name":path.file_name().unwrap_or_default().to_string_lossy(),"mime":"application/octet-stream","data":STANDARD.encode(bytes)}));
            }
            args["paths"] = json!(
                files
                    .iter()
                    .map(|file| file["name"].clone())
                    .collect::<Vec<_>>()
            );
        }
        args["type"] = json!(self.method);
        let mut command = json!({"action":args,"files":files,"target":prepared["target"],"policy":prepared["policy"]});
        if !prepared["generation"].is_null() {
            command["generation"] = prepared["generation"].clone();
        }
        check_policy()?;
        let request_cancel = context.cancel.child_token();
        let _request_guard = request_cancel.clone().drop_guard();
        let request = BrowserBroker::request(&host, command, &request_cancel);
        tokio::pin!(request);
        let mut poll = tokio::time::interval(Duration::from_millis(100));
        let result = loop {
            tokio::select! {
                result = &mut request => break result?,
                _ = poll.tick() => if let Err(error) = check_policy() {
                    request_cancel.cancel();
                    let _ = request.await;
                    return Err(error);
                }
            }
        };
        check_policy()?;
        let mut value = result["value"].clone();
        let mut exported = Vec::new();
        let mut attachments = Vec::new();
        let mut total = 0;
        let files = result["files"]
            .as_array()
            .ok_or_else(|| error("Invalid browser result"))?;
        if files.len() > 8 {
            return Err(error("Too many browser files"));
        }
        let directory = context
            .workspace
            .join(".haarness-chat-attachments")
            .join(format!("browser-{}", uuid::Uuid::now_v7()));
        for file in files {
            let encoded = file["data"]
                .as_str()
                .filter(|s| s.len() < MAX_HOST_MESSAGE)
                .ok_or_else(|| error("Invalid browser file"))?;
            let bytes = STANDARD.decode(encoded).map_err(error)?;
            total += bytes.len();
            if total > MAX_BYTES {
                return Err(error("Browser result exceeds 5 MiB total"));
            }
            let root = directory
                .parent()
                .ok_or_else(|| error("Invalid browser artifact directory"))?;
            tokio::fs::create_dir_all(root).await.map_err(error)?;
            if !tokio::fs::canonicalize(root)
                .await
                .map_err(error)?
                .starts_with(context.workspace.canonicalize().map_err(error)?)
            {
                return Err(error("Browser artifact directory escapes the workspace"));
            }
            tokio::fs::create_dir_all(&directory).await.map_err(error)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                tokio::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                    .await
                    .map_err(error)?;
            }
            let name = file["name"].as_str().unwrap_or("capture");
            let safe: String = name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .take(120)
                .collect();
            let path = directory.join(format!("{}-{}", exported.len(), safe));
            tokio::fs::write(&path, &bytes).await.map_err(error)?;
            let mime = file["mime"].as_str().unwrap_or("application/octet-stream");
            if matches!(mime, "image/png" | "image/jpeg" | "image/webp") {
                attachments.push(path.to_string_lossy().to_string());
            }
            let relative = format!(
                "./{}",
                path.strip_prefix(&context.workspace)
                    .map_err(error)?
                    .display()
            );
            exported.push(
                json!({"id":file["id"],"name":name,"mime":mime,"bytes":bytes.len(),"path":path,"workspaceRelativePath":relative}),
            );
        }
        if !exported.is_empty() {
            value["files"] = json!(exported);
        }
        Ok(ToolOutput {
            text: serde_json::to_string_pretty(&value).map_err(error)?,
            metadata: json!({"result":value,"attachments":attachments}),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn connected(
        broker: &Arc<BrowserBroker>,
        thread: ThreadId,
    ) -> (Arc<Host>, mpsc::Receiver<Value>) {
        let (tx, rx) = mpsc::channel(128);
        let host = Arc::new(Host {
            policy_supported: true,
            lease: uuid::Uuid::now_v7().to_string(),
            outbound: tx,
            pending: Mutex::new(HashMap::new()),
            state: Mutex::new(json!({"tabs":[],"focusedTabID":null})),
            closed: CancellationToken::new(),
            tunnels: Mutex::new(HashMap::new()),
        });
        broker.hosts.lock().insert(thread, host.clone());
        (host, rx)
    }
    #[tokio::test]
    async fn legacy_hosts_cannot_run_restricted_site_policies() {
        let mut policy = crate::browser_policy::BrowserPolicy::default();
        policy.defaults.browsing = haarness_core::PermissionDecision::Deny;
        let broker = BrowserBroker::with_policy(Arc::new(move || Ok(policy.clone())));
        let thread = ThreadId::new();
        let (tx, mut rx) = mpsc::channel(8);
        broker.hosts.lock().insert(thread, Arc::new(Host {
            policy_supported: false,
            lease: "legacy".into(), outbound: tx, pending: Mutex::new(HashMap::new()),
            state: Mutex::new(json!({"tabs":[],"focusedTabID":null})), closed: CancellationToken::new(), tunnels: Mutex::new(HashMap::new()),
        }));
        let error = broker.prepare(thread, "tabs.list", json!({}), &CancellationToken::new()).await.unwrap_err();
        assert!(error.to_string().contains("Update Desktop"));
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn draft_adoption_keeps_its_lease_and_cannot_replace_or_leak_to_another_thread() {
        let broker = BrowserBroker::new();
        let seed = ThreadId::new();
        let (host, _messages) = connected(&broker, seed);
        broker.hosts.lock().remove(&seed);
        let draft = uuid::Uuid::new_v4();
        broker.drafts.lock().insert(draft, host.clone());
        let target = ThreadId::new();
        assert!(broker.host(target, None).is_err());
        assert!(broker.adopt(draft, "wrong-lease", target).is_err());
        let occupied = ThreadId::new();
        connected(&broker, occupied);
        assert!(broker.adopt(draft, &host.lease, occupied).is_err());
        broker.adopt(draft, &host.lease, target).unwrap();
        broker.adopt(draft, &host.lease, target).unwrap();
        assert!(Arc::ptr_eq(&host, &broker.host(target, None).unwrap()));
        assert!(Arc::ptr_eq(&host, &broker.draft(draft, &host.lease).unwrap()));
        assert!(broker.adopt(draft, &host.lease, ThreadId::new()).is_err());
        host.closed.cancel();
        assert!(broker.draft(draft, &host.lease).is_err());
        assert!(broker.host(target, None).is_err());
    }
    #[tokio::test]
    async fn cancellation_removes_request_and_never_replays() {
        let broker = BrowserBroker::new();
        let thread = ThreadId::new();
        let (host, mut messages) = connected(&broker, thread);
        let cancel = CancellationToken::new();
        let task = tokio::spawn({
            let host = host.clone();
            let cancel = cancel.clone();
            async move {
                BrowserBroker::request(&host, json!({"action":{"type":"click"}}), &cancel).await
            }
        });
        let message = messages.recv().await.unwrap();
        assert_eq!(message["type"], "command");
        cancel.cancel();
        assert!(matches!(task.await.unwrap(), Err(CoreError::Cancelled)));
        assert!(host.pending.lock().is_empty());
        assert_eq!(messages.recv().await.unwrap()["type"], "cancel");
        assert!(messages.try_recv().is_err());
    }
    #[tokio::test]
    async fn replacement_and_foreign_conversations_cannot_execute_prepared_calls() {
        let broker = BrowserBroker::new();
        let thread = ThreadId::new();
        let (old, _) = connected(&broker, thread);
        assert!(broker.host(ThreadId::new(), None).is_err());
        let (new, _) = connected(&broker, thread);
        old.closed.cancel();
        assert!(broker.host(thread, Some(&old.lease)).is_err());
        assert_eq!(
            broker.host(thread, Some(&new.lease)).unwrap().lease,
            new.lease
        );
    }
    #[tokio::test]
    async fn prepare_captures_authoritative_target_and_ignores_supplied_metadata() {
        let broker = BrowserBroker::new();
        let thread = ThreadId::new();
        let (host, mut messages) = connected(&broker, thread);
        let task = tokio::spawn({
            let broker = broker.clone();
            async move {
                broker
                    .prepare(
                        thread,
                        "tabs.open",
                        json!({"url":"https://example.test","__browser":{"lease":"forged"}}),
                        &CancellationToken::new(),
                    )
                    .await
            }
        });
        let message = messages.recv().await.unwrap();
        let id = message["id"].as_str().unwrap();
        host.pending.lock().remove(id).unwrap().send(json!({"type":"success","result":{"value":{"resources":["https://example.test"],"key":"open"},"files":[]}})).unwrap();
        let arguments = task.await.unwrap().unwrap();
        assert_eq!(arguments["__browser"]["lease"], host.lease);
        assert_eq!(
            arguments["__browser"]["target"]["resources"][0],
            "https://example.test"
        );
    }
    #[test]
    fn catalog_is_valid_and_bounded() {
        let tools = tools(BrowserBroker::new());
        assert!(tools.len() > 35);
        for tool in tools {
            let definition = tool.definition();
            assert_eq!(definition.input_schema["type"], "object");
            jsonschema::validator_for(&definition.input_schema).unwrap();
            assert!(tool.validate_arguments(&json!({"__browser":{}})).is_err());
        }
        assert!(!valid_state(
            &json!({"tabs":[{"id":"not-a-tab","generation":0,"url":"about:blank"}]})
        ));
    }
}
