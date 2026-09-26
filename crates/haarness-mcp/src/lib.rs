//! MCP configuration, transport lifecycle, capability discovery, and tool calls.

mod executable;
mod process;
#[cfg(all(test, unix))]
mod process_tests;
mod credentials;
use credentials::FileCredentialStore;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures::StreamExt;
use haarness_core::{CoreError, CoreResult, McpRegistry, ToolDefinition, ToolOutput};
use haarness_protocol::{McpConfigureRequest, McpServerInfo};
use http::{HeaderName, HeaderValue};
use parking_lot::RwLock;
use rmcp::{
    RoleClient, ServiceExt,
    model::{
        CallToolRequestParams, ClientJsonRpcMessage, GetPromptRequestParams,
        ReadResourceRequestParams, ServerJsonRpcMessage,
    },
    transport::{
        AuthClient, AuthorizationManager, AuthorizationRequest,
        CredentialStore as OAuthCredentialStore, IntoTransport,
        StreamableHttpClientTransport, Transport, auth::OAuthState,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

#[derive(Clone, Default)]
pub struct NativeMcpRegistry {
    servers: Arc<RwLock<BTreeMap<String, Entry>>>,
    auth_dir: Option<PathBuf>,
}
#[derive(Clone)]
struct Entry {
    generation: uuid::Uuid,
    info: McpServerInfo,
    config: McpConfigureRequest,
    tools: Vec<ToolDefinition>,
    remembered: Vec<ToolDefinition>,
    identity: Option<[u8; 32]>,
    credential: Option<FileCredentialStore>,
    worker: Option<mpsc::Sender<WorkerCommand>>,
    auth_challenge: Option<String>,
}
#[derive(Clone)]
struct CatalogClient {
    servers: std::sync::Weak<RwLock<BTreeMap<String, Entry>>>,
    name: String,
    generation: uuid::Uuid,
    credential: Option<FileCredentialStore>,
}
impl rmcp::ClientHandler for CatalogClient {
    async fn on_tool_list_changed(&self, context: rmcp::service::NotificationContext<RoleClient>) {
        {
            let Some(servers) = self.servers.upgrade() else {
                return;
            };
            let mut servers = servers.write();
            let Some(entry) = servers.get_mut(&self.name) else {
                return;
            };
            if entry.generation != self.generation || !credential_valid(self.credential.as_ref()) {
                return;
            }
            entry.tools.clear();
            entry.remembered.clear();
        }
        let Ok(tools) = context.peer.list_all_tools().await else {
            return;
        };
        let Some(servers) = self.servers.upgrade() else {
            return;
        };
        let mut servers = servers.write();
        let Some(entry) = servers.get_mut(&self.name) else {
            return;
        };
        if entry.generation != self.generation
            || !credential_valid(self.credential.as_ref())
            || !matches!(entry.info.status.as_str(), "connected" | "connecting")
        {
            return;
        }
        entry.tools = definitions(&tools)
            .into_iter()
            .map(|mut tool| {
                tool.name = format!("mcp__{}__{}", self.name, tool.name);
                tool
            })
            .collect();
        entry.remembered = entry.tools.clone();
        entry.info.detail = Some(format!("{} tools discovered", entry.tools.len()));
    }
}

enum WorkerCommand {
    Call {
        tool: String,
        arguments: Value,
        reply: oneshot::Sender<CoreResult<ToolOutput>>,
    },
    Resources {
        reply: oneshot::Sender<CoreResult<Vec<Value>>>,
    },
    ResourceTemplates {
        reply: oneshot::Sender<CoreResult<Vec<Value>>>,
    },
    ReadResource {
        uri: String,
        reply: oneshot::Sender<CoreResult<Value>>,
    },
    Prompts {
        reply: oneshot::Sender<CoreResult<Vec<Value>>>,
    },
    GetPrompt {
        prompt: String,
        arguments: Value,
        reply: oneshot::Sender<CoreResult<Value>>,
    },
}

pub struct McpOAuthSession {
    state: OAuthState,
    pub authorization_url: String,
}

impl McpOAuthSession {
    pub async fn complete(mut self, callback_url: &str) -> CoreResult<()> {
        self.state
            .handle_callback_url(callback_url)
            .await
            .map_err(mcp_error)
    }
}

#[derive(Debug)]
enum ActivationError {
    Core(CoreError),
    AuthorizationRequired(Option<String>),
}

impl From<CoreError> for ActivationError {
    fn from(error: CoreError) -> Self {
        Self::Core(error)
    }
}

#[async_trait]
impl McpRegistry for NativeMcpRegistry {
    fn tool_snapshot(&self) -> Vec<ToolDefinition> {
        self.revoke_stale();
        self.servers
            .read()
            .iter()
            .flat_map(|(name, entry)| {
                if entry.info.status == "connected" {
                    return entry.tools.clone();
                }
                if matches!(entry.info.status.as_str(), "connecting" | "failed")
                    && entry.config.enabled
                    && entry.config.headers_helper.is_none()
                    && entry.identity.is_some()
                    && entry.identity
                        == schema_identity(name, &entry.config, self.auth_dir.as_deref())
                {
                    return entry.remembered.clone();
                }
                Vec::new()
            })
            .collect()
    }
    async fn list(&self) -> CoreResult<Vec<McpServerInfo>> {
        self.revoke_stale();
        Ok(self
            .servers
            .read()
            .values()
            .map(|entry| entry.info.clone())
            .collect())
    }
    async fn configure(
        &self,
        name: &str,
        config: McpConfigureRequest,
    ) -> CoreResult<McpServerInfo> {
        if !matches!(
            config.transport.as_str(),
            "stdio" | "streamable_http" | "sse"
        ) {
            return Err(CoreError::Invalid(format!(
                "unsupported MCP transport {}",
                config.transport
            )));
        }
        let info = McpServerInfo {
            name: name.into(),
            transport: config.transport.clone(),
            status: if config.enabled {
                "disconnected"
            } else {
                "disabled"
            }
            .into(),
            detail: None,
        };
        self.servers.write().insert(
            name.into(),
            Entry {
                generation: uuid::Uuid::now_v7(),
                info: info.clone(),
                config: config.clone(),
                tools: Vec::new(),
                remembered: Vec::new(),
                identity: None,
                credential: None,
                worker: None,
                auth_challenge: None,
            },
        );
        if config.enabled {
            self.reconnect(name).await
        } else {
            Ok(info)
        }
    }
    async fn reconnect(&self, name: &str) -> CoreResult<McpServerInfo> {
        self.connect(name, None).await
    }
    async fn disconnect(&self, name: &str) -> CoreResult<McpServerInfo> {
        let mut servers = self.servers.write();
        let entry = servers
            .get_mut(name)
            .ok_or_else(|| CoreError::NotFound(format!("MCP server {name}")))?;
        entry.generation = uuid::Uuid::now_v7();
        entry.worker = None;
        entry.tools.clear();
        entry.remembered.clear();
        entry.identity = None;
        entry.auth_challenge = None;
        entry.info.status = if entry.config.enabled {
            "disconnected"
        } else {
            "disabled"
        }
        .into();
        entry.info.detail = None;
        Ok(entry.info.clone())
    }
    async fn tools(&self, name: &str) -> CoreResult<Vec<ToolDefinition>> {
        self.revoke_stale();
        self.servers
            .read()
            .get(name)
            .map(|entry| entry.tools.clone())
            .ok_or_else(|| CoreError::NotFound(format!("MCP server {name}")))
    }
    async fn call(&self, name: &str, tool: &str, arguments: Value) -> CoreResult<ToolOutput> {
        let config = self
            .servers
            .read()
            .get(name)
            .map(|entry| entry.config.clone())
            .ok_or_else(|| CoreError::NotFound(format!("MCP server {name}")))?;
        let first = self.call_once(name, tool, arguments.clone()).await;
        let Err(error) = first else {
            return first;
        };
        if config.headers_helper.is_none()
            || !refreshable_helper_error(&error)
            || transport_failure(&error)
        {
            return Err(error);
        }
        self.reconnect(name).await?;
        self.call_once(name, tool, arguments).await
    }
    async fn resources(&self, name: &str) -> CoreResult<Vec<Value>> {
        self.worker_request(name, |reply| WorkerCommand::Resources { reply })
            .await
    }
    async fn resource_templates(&self, name: &str) -> CoreResult<Vec<Value>> {
        self.worker_request(name, |reply| WorkerCommand::ResourceTemplates { reply })
            .await
    }
    async fn read_resource(&self, name: &str, uri: &str) -> CoreResult<Value> {
        self.worker_request(name, |reply| WorkerCommand::ReadResource {
            uri: uri.to_owned(),
            reply,
        })
        .await
    }
    async fn prompts(&self, name: &str) -> CoreResult<Vec<Value>> {
        self.worker_request(name, |reply| WorkerCommand::Prompts { reply })
            .await
    }
    async fn get_prompt(&self, name: &str, prompt: &str, arguments: Value) -> CoreResult<Value> {
        self.worker_request(name, |reply| WorkerCommand::GetPrompt {
            prompt: prompt.to_owned(),
            arguments,
            reply,
        })
        .await
    }
}
impl NativeMcpRegistry {
    fn revoke_stale(&self) {
        for entry in self.servers.write().values_mut() {
            if !credential_valid(entry.credential.as_ref()) {
                revoke_entry(entry);
            }
        }
    }
    fn connect<'a>(
        &'a self,
        name: &'a str,
        expected: Option<uuid::Uuid>,
    ) -> futures::future::BoxFuture<'a, CoreResult<McpServerInfo>> {
        Box::pin(async move {
            let (config, generation) = {
                let mut servers = self.servers.write();
                let entry = servers
                    .get_mut(name)
                    .ok_or_else(|| CoreError::NotFound(format!("MCP server {name}")))?;
                if expected.is_some_and(|expected| {
                    entry.generation != expected || entry.info.status != "failed"
                }) {
                    return Err(CoreError::Conflict(format!(
                        "MCP server {name} changed before recovery"
                    )));
                }
                if !entry.config.enabled {
                    return Err(CoreError::Invalid(format!("MCP server {name} is disabled")));
                }
                entry.generation = uuid::Uuid::now_v7();
                entry.info.status = "connecting".into();
                entry.info.detail = None;
                entry.worker = None;
                entry.tools.clear();
                (entry.config.clone(), entry.generation)
            };
            let identity = schema_identity(name, &config, self.auth_dir.as_deref());
            let credential = (config.transport == "streamable_http" && oauth_enabled(&config, &config.headers))
                .then(|| manager_store(name, self.auth_dir.as_deref()));
            let handler = CatalogClient {
                servers: Arc::downgrade(&self.servers),
                name: name.into(),
                generation,
                credential: credential.clone(),
            };
            let activation = async {
                match config.transport.as_str() {
                    "stdio" => {
                        let result = async {
                            let program = config.command.as_deref().ok_or_else(|| {
                                CoreError::Invalid("stdio MCP requires command".into())
                            })?;
                            let resolved = executable::resolve(program, &config);
                            let mut command =
                                tokio::process::Command::new(resolved.as_ref().map_or_else(
                                    || std::ffi::OsStr::new(program),
                                    |(path, _)| path.as_os_str(),
                                ));
                            command.args(&config.args);
                            if let Some(cwd) = &config.cwd {
                                command.current_dir(cwd);
                            }
                            command.envs(
                                config
                                    .env
                                    .iter()
                                    .map(|(key, value)| (key, expand_env_reference(value))),
                            );
                            if let Some((_, path)) = resolved {
                                command.env("PATH", path);
                            }
                            let transport = process::Process::new(command).map_err(mcp_error)?;
                            let client =
                                handler.clone().serve(transport).await.map_err(mcp_error)?;
                            let tools = client.list_all_tools().await.map_err(mcp_error)?;
                            let definitions = definitions(&tools);
                            let tx = spawn_worker(client, None);
                            CoreResult::Ok((definitions, tx, None))
                        }
                        .await;
                        result.map_err(ActivationError::Core)
                    }
                    "streamable_http" => {
                        activate_remote(name, &config, credential.clone(), handler.clone())
                            .await
                    }
                    "sse" => activate_legacy_sse(name, &config, handler.clone())
                        .await
                        .map(|(tools, worker)| (tools, worker, None))
                        .map_err(ActivationError::Core),
                    _ => unreachable!(),
                }
            };
            let activated = tokio::time::timeout(
                Duration::from_millis(config.timeout_ms.unwrap_or(10_000).clamp(1, 600_000)),
                activation,
            )
            .await
            .unwrap_or_else(|_| {
                Err(ActivationError::Core(CoreError::Tool(
                    "MCP connection timed out".into(),
                )))
            });
            // A disconnect, reconfiguration or newer reconnect owns the slot now.
            let mut servers = self.servers.write();
            let entry = servers
                .get_mut(name)
                .ok_or_else(|| CoreError::NotFound(format!("MCP server {name}")))?;
            if entry.generation != generation {
                return Err(CoreError::Conflict(format!(
                    "MCP server {name} changed while connecting"
                )));
            }
            let activated = match activated {
                Ok(activated) => activated,
                Err(ActivationError::AuthorizationRequired(challenge)) => {
                    entry.tools.clear();
                    entry.remembered.clear();
                    entry.identity = None;
                    entry.worker = None;
                    entry.auth_challenge = challenge;
                    entry.info.status = "needs_auth".into();
                    entry.info.detail = Some(format!(
                        "authorization required; run `haarness mcp auth {name}`"
                    ));
                    return Ok(entry.info.clone());
                }
                Err(ActivationError::Core(error)) => {
                    entry.info.status = "failed".into();
                    entry.info.detail =
                        Some(format!("connection failed: {}", concise_error(&error)));
                    return Err(error);
                }
            };
            let (mut definitions, worker, credential) = activated;
            if !credential_valid(credential.as_ref()) {
                revoke_entry(entry);
                return Err(CoreError::Conflict(format!("MCP server {name} credentials changed while connecting")));
            }
            for tool in &mut definitions {
                tool.name = format!("mcp__{name}__{}", tool.name);
            }
            let count = definitions.len();
            entry.remembered = definitions.clone();
            entry.identity = schema_identity(name, &config, self.auth_dir.as_deref()).or(identity);
            entry.credential = credential;
            entry.tools = definitions;
            entry.worker = Some(worker);
            entry.auth_challenge = None;
            entry.info.status = "connected".into();
            entry.info.detail = Some(format!("{count} tools discovered"));
            let info = entry.info.clone();
            drop(servers);
            self.watch(name, generation);
            Ok(info)
        })
    }

    #[must_use]
    pub fn with_auth_dir(path: impl Into<PathBuf>) -> Self {
        Self {
            auth_dir: Some(path.into()),
            ..Self::default()
        }
    }

    /// Atomically replace the complete configured server generation. The
    /// candidate registry is composed and connected before this call, so API
    /// readers never observe a partly-added/partly-removed MCP set.
    pub fn replace_from(&self, candidate: &Self) {
        let entries = candidate.servers.read().clone();
        *self.servers.write() = entries.clone();
        for (name, entry) in entries {
            if entry.info.status == "connected" {
                self.watch(&name, entry.generation);
            }
        }
    }

    pub async fn begin_oauth(&self, name: &str, redirect_uri: &str) -> CoreResult<McpOAuthSession> {
        let (config, challenge) = self
            .servers
            .read()
            .get(name)
            .map(|entry| (entry.config.clone(), entry.auth_challenge.clone()))
            .ok_or_else(|| CoreError::NotFound(format!("MCP server {name}")))?;
        if config.transport != "streamable_http" {
            return Err(CoreError::Invalid(
                "OAuth is only supported for Streamable HTTP MCP servers".into(),
            ));
        }
        if config.oauth == Some(false) {
            return Err(CoreError::Invalid(format!(
                "OAuth is disabled for MCP server {name}"
            )));
        }
        let url = config
            .url
            .as_deref()
            .ok_or_else(|| CoreError::Invalid("Streamable HTTP MCP requires url".into()))?;
        let (manager, _) = authorization_manager(url, manager_store(name, self.auth_dir.as_deref())).await?;
        let mut request = AuthorizationRequest::new(redirect_uri).with_client_name("Haarness");
        if let Some(client_id) = config.oauth_client_id {
            request = request.with_preregistered_client(client_id);
        }
        if !config.oauth_scopes.is_empty() {
            request = request.with_scopes(config.oauth_scopes);
        }
        if let Some(challenge) = challenge {
            request = request.with_challenge(challenge);
        }
        let mut state = OAuthState::Unauthorized(manager);
        state
            .start_authorization(request)
            .await
            .map_err(mcp_error)?;
        let authorization_url = state.get_authorization_url().await.map_err(mcp_error)?;
        Ok(McpOAuthSession {
            state,
            authorization_url,
        })
    }

    pub async fn clear_oauth(&self, name: &str) -> CoreResult<()> {
        {
            let mut servers = self.servers.write();
            let entry = servers
                .get_mut(name)
                .ok_or_else(|| CoreError::NotFound(format!("MCP server {name}")))?;
            if entry.config.transport != "streamable_http" {
                return Err(CoreError::Invalid(
                    "OAuth is only supported for Streamable HTTP MCP servers".into(),
                ));
            }
            // Fence pending lists/connections before the credential I/O. A
            // failed clear also leaves the old account's catalog unavailable.
            entry.generation = uuid::Uuid::now_v7();
            entry.worker = None;
            entry.tools.clear();
            entry.remembered.clear();
            entry.identity = None;
            entry.auth_challenge = None;
            entry.info.status = if entry.config.enabled {
                "needs_auth"
            } else {
                "disabled"
            }
            .into();
            entry.info.detail = Some("OAuth credentials cleared".into());
        }
        manager_store(name, self.auth_dir.as_deref())
            .clear()
            .await
            .map_err(mcp_error)
    }

    async fn call_once(&self, name: &str, tool: &str, arguments: Value) -> CoreResult<ToolOutput> {
        self.revoke_stale();
        let worker = self
            .servers
            .read()
            .get(name)
            .and_then(|entry| entry.worker.clone())
            .ok_or_else(|| CoreError::Conflict(format!("MCP server {name} is not connected")))?;
        let raw = tool
            .strip_prefix(&format!("mcp__{name}__"))
            .unwrap_or(tool)
            .to_owned();
        let (reply_tx, reply_rx) = oneshot::channel();
        worker
            .send(WorkerCommand::Call {
                tool: raw,
                arguments,
                reply: reply_tx,
            })
            .await
            .map_err(|_| CoreError::Conflict(format!("MCP server {name} disconnected")))?;
        reply_rx
            .await
            .map_err(|_| CoreError::Conflict(format!("MCP server {name} disconnected")))?
    }

    async fn worker_request<T: Send + 'static>(
        &self,
        name: &str,
        command: impl FnOnce(oneshot::Sender<CoreResult<T>>) -> WorkerCommand,
    ) -> CoreResult<T> {
        self.revoke_stale();
        let worker = self
            .servers
            .read()
            .get(name)
            .and_then(|entry| entry.worker.clone())
            .ok_or_else(|| CoreError::Conflict(format!("MCP server {name} is not connected")))?;
        let (reply_tx, reply_rx) = oneshot::channel();
        worker
            .send(command(reply_tx))
            .await
            .map_err(|_| CoreError::Conflict(format!("MCP server {name} disconnected")))?;
        reply_rx
            .await
            .map_err(|_| CoreError::Conflict(format!("MCP server {name} disconnected")))?
    }

    fn watch(&self, name: &str, generation: uuid::Uuid) {
        let weak = Arc::downgrade(&self.servers);
        let auth_dir = self.auth_dir.clone();
        let name = name.to_owned();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let Some(servers) = weak.upgrade() else {
                    return;
                };
                {
                    let mut entries = servers.write();
                    let Some(entry) = entries.get_mut(&name) else {
                        return;
                    };
                    if !credential_valid(entry.credential.as_ref()) {
                        revoke_entry(entry);
                        return;
                    }
                    if entry.generation != generation || entry.info.status != "connected" {
                        return;
                    }
                    if entry
                        .worker
                        .as_ref()
                        .is_some_and(|worker| !worker.is_closed())
                    {
                        continue;
                    }
                    entry.worker = None;
                    entry.tools.clear();
                    entry.info.status = "failed".into();
                    entry.info.detail = Some("Transport closed; reconnecting".into());
                }
                drop(servers);
                tokio::time::sleep(Duration::from_secs(1)).await;
                let Some(servers) = weak.upgrade() else {
                    return;
                };
                if !servers.read().get(&name).is_some_and(|entry| {
                    entry.generation == generation && entry.info.status == "failed"
                }) {
                    return;
                }
                // Restore the connection for subsequent calls, never replay a
                // call whose side effects may already have reached the server.
                let registry = Self { servers, auth_dir };
                let _ = registry.connect(&name, Some(generation)).await;
                return;
            }
        });
    }
}
fn credential_valid(credential: Option<&FileCredentialStore>) -> bool {
    credential.is_none_or(FileCredentialStore::valid)
}
fn revoke_entry(entry: &mut Entry) {
    entry.generation = uuid::Uuid::now_v7();
    entry.worker = None;
    entry.tools.clear();
    entry.remembered.clear();
    entry.identity = None;
    entry.credential = None;
    entry.auth_challenge = None;
    entry.info.status = if entry.config.enabled { "needs_auth" } else { "disabled" }.into();
    entry.info.detail = Some("OAuth credentials cleared in another workspace".into());
}
fn schema_identity(
    name: &str,
    config: &McpConfigureRequest,
    auth_dir: Option<&Path>,
) -> Option<[u8; 32]> {
    let mut effective = config.clone();
    for value in effective
        .env
        .values_mut()
        .chain(effective.headers.values_mut())
    {
        *value = expand_env_reference(value);
    }
    let mut hash = Sha256::new();
    hash.update(serde_json::to_vec(&effective).ok()?);
    if effective.transport == "streamable_http" && oauth_enabled(&effective, &effective.headers) {
        match std::fs::read(manager_store(name, auth_dir).path) {
            Ok(credentials) => hash.update(credentials),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    Some(hash.finalize().into())
}

fn definitions(tools: &[rmcp::model::Tool]) -> Vec<ToolDefinition> {
    tools
        .iter()
        .map(|tool| ToolDefinition {
            name: tool.name.to_string(),
            description: tool.description.as_deref().unwrap_or("MCP tool").to_owned(),
            input_schema: Value::Object((*tool.input_schema).clone()),
        })
        .collect()
}

async fn activate_remote(
    name: &str,
    config: &McpConfigureRequest,
    credential: Option<FileCredentialStore>,
    handler: CatalogClient,
) -> Result<(Vec<ToolDefinition>, mpsc::Sender<WorkerCommand>, Option<FileCredentialStore>), ActivationError> {
    let url = config
        .url
        .as_deref()
        .ok_or_else(|| CoreError::Invalid("Streamable HTTP MCP requires url".into()))?;
    for attempt in 0..2 {
        let headers = resolve_headers(name, config).await?;
        let transport_config = StreamableHttpClientTransportConfig::with_uri(url)
            .custom_headers(mcp_headers(&headers)?);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(
                config.timeout_ms.unwrap_or(10_000).clamp(1, 600_000),
            ))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(mcp_error)?;
        let credential = credential.clone().filter(|_| oauth_enabled(config, &headers));
        let handler = CatalogClient { credential: credential.clone(), ..handler.clone() };
        let result = if oauth_enabled(config, &headers) {
            let (manager, authorized) = authorization_manager(url, credential.clone().ok_or_else(|| CoreError::Invalid("missing OAuth identity".into()))?).await?;
            if authorized {
                activate_streamable(
                    handler.clone(),
                    StreamableHttpClientTransport::with_client(
                        AuthClient::new(http, manager),
                        transport_config,
                    ),
                )
                .await
            } else {
                activate_streamable(
                    handler.clone(),
                    StreamableHttpClientTransport::with_client(http, transport_config),
                )
                .await
            }
        } else {
            activate_streamable(
                handler.clone(),
                StreamableHttpClientTransport::with_client(http, transport_config),
            )
            .await
        };
        match result {
            Ok((tools, worker)) => return Ok((tools, worker, credential)),
            Err(ActivationError::Core(error))
                if attempt == 0
                    && config.headers_helper.is_some()
                    && refreshable_helper_error(&error) => {}
            Err(error) => return Err(error),
        }
    }
    unreachable!("remote MCP activation loop always returns")
}

async fn activate_streamable<T, E, A>(
    handler: CatalogClient,
    transport: T,
) -> Result<(Vec<ToolDefinition>, mpsc::Sender<WorkerCommand>), ActivationError>
where
    T: IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let credential = handler.credential.clone();
    let client = match handler.serve(transport).await {
        Ok(client) => client,
        Err(error) if error.is_authorization_required() => {
            return Err(ActivationError::AuthorizationRequired(
                error.auth_challenge().map(str::to_owned),
            ));
        }
        Err(error) => return Err(ActivationError::Core(mcp_error(error))),
    };
    let tools = client
        .list_all_tools()
        .await
        .map_err(mcp_error)
        .map_err(ActivationError::Core)?;
    let definitions = definitions(&tools);
    let tx = spawn_worker(client, credential);
    Ok((definitions, tx))
}

fn spawn_worker(
    client: rmcp::service::RunningService<RoleClient, CatalogClient>,
    credential: Option<FileCredentialStore>,
) -> mpsc::Sender<WorkerCommand> {
    let (tx, mut rx) = mpsc::channel(64);
    tokio::spawn(async move {
        let peer = client.peer().clone();
        let done = client.waiting();
        tokio::pin!(done);
        loop {
            let command = tokio::select! {
                _ = &mut done => break,
                command = rx.recv() => match command { Some(command) => command, None => break },
            };
            // A queued operation must not escape through an old authenticated client.
            if !credential_valid(credential.as_ref()) { break; }
            match command {
                WorkerCommand::Call {
                    tool,
                    arguments,
                    reply,
                } => {
                    let mut params = CallToolRequestParams::new(tool);
                    params.arguments = arguments.as_object().cloned();
                    let result = peer
                        .call_tool(params)
                        .await
                        .map_err(mcp_error)
                        .and_then(to_output);
                    let _ = reply.send(result);
                }
                WorkerCommand::Resources { reply } => {
                    let result = if peer
                        .peer_info()
                        .is_some_and(|info| info.capabilities.resources.is_some())
                    {
                        peer.list_all_resources()
                            .await
                            .map_err(mcp_error)
                            .and_then(json_values)
                    } else {
                        Ok(Vec::new())
                    };
                    let _ = reply.send(result);
                }
                WorkerCommand::ResourceTemplates { reply } => {
                    let result = if peer
                        .peer_info()
                        .is_some_and(|info| info.capabilities.resources.is_some())
                    {
                        peer.list_all_resource_templates()
                            .await
                            .map_err(mcp_error)
                            .and_then(json_values)
                    } else {
                        Ok(Vec::new())
                    };
                    let _ = reply.send(result);
                }
                WorkerCommand::ReadResource { uri, reply } => {
                    let result = peer
                        .read_resource(ReadResourceRequestParams::new(uri))
                        .await
                        .map_err(mcp_error)
                        .and_then(json_value);
                    let _ = reply.send(result);
                }
                WorkerCommand::Prompts { reply } => {
                    let result = if peer
                        .peer_info()
                        .is_some_and(|info| info.capabilities.prompts.is_some())
                    {
                        peer.list_all_prompts()
                            .await
                            .map_err(mcp_error)
                            .and_then(json_values)
                    } else {
                        Ok(Vec::new())
                    };
                    let _ = reply.send(result);
                }
                WorkerCommand::GetPrompt {
                    prompt,
                    arguments,
                    reply,
                } => {
                    let mut params = GetPromptRequestParams::new(prompt);
                    params.arguments = arguments.as_object().cloned();
                    let result = peer
                        .get_prompt(params)
                        .await
                        .map_err(mcp_error)
                        .and_then(json_value);
                    let _ = reply.send(result);
                }
            }
        }
    });
    tx
}

fn json_values<T: serde::Serialize>(values: Vec<T>) -> CoreResult<Vec<Value>> {
    values.into_iter().map(json_value).collect()
}

fn json_value(value: impl serde::Serialize) -> CoreResult<Value> {
    serde_json::to_value(value).map_err(mcp_error)
}

fn oauth_enabled(config: &McpConfigureRequest, headers: &BTreeMap<String, String>) -> bool {
    if headers.keys().any(|name| name.eq_ignore_ascii_case("authorization")) { return false; }
    match config.oauth {
        Some(enabled) => enabled,
        None => {
            config.headers_helper.is_none()
                && !headers
                    .keys()
                    .any(|name| name.eq_ignore_ascii_case("authorization"))
        }
    }
}

async fn authorization_manager(
    url: &str,
    store: FileCredentialStore,
) -> CoreResult<(AuthorizationManager, bool)> {
    let mut manager = AuthorizationManager::new(url).await.map_err(mcp_error)?;
    manager.set_credential_store(store);
    let initialized = manager.initialize_from_store().await.map_err(mcp_error)?;
    Ok((manager, initialized))
}

fn manager_store(name: &str, auth_dir: Option<&Path>) -> FileCredentialStore {
    let root = auth_dir.map(Path::to_path_buf).unwrap_or_else(|| {
        std::env::temp_dir().join(format!("haarness-mcp-oauth-{}", std::process::id()))
    });
    let hex = b"0123456789abcdef";
    let mut encoded = String::with_capacity(name.len() * 2);
    for byte in name.as_bytes() {
        encoded.push(char::from(hex[usize::from(byte >> 4)]));
        encoded.push(char::from(hex[usize::from(byte & 0x0f)]));
    }
    FileCredentialStore::new(root.join(format!("mcp-{encoded}.json")))
}

fn concise_error(error: &CoreError) -> String {
    error.to_string().chars().take(512).collect()
}

async fn activate_legacy_sse(
    name: &str,
    config: &McpConfigureRequest,
    handler: CatalogClient,
) -> CoreResult<(Vec<ToolDefinition>, mpsc::Sender<WorkerCommand>)> {
    let url = config
        .url
        .as_deref()
        .ok_or_else(|| CoreError::Invalid("legacy SSE MCP requires url".into()))?;
    for attempt in 0..2 {
        let headers = resolve_headers(name, config).await?;
        let result = async {
            let transport = LegacySseTransport::connect(
                url,
                mcp_headers(&headers)?,
                Duration::from_millis(config.timeout_ms.unwrap_or(10_000).clamp(1, 600_000)),
            )
            .await?;
            let client = handler.clone().serve(transport).await.map_err(mcp_error)?;
            let tools = client.list_all_tools().await.map_err(mcp_error)?;
            let definitions = definitions(&tools);
            let tx = spawn_worker(client, None);
            CoreResult::Ok((definitions, tx))
        }
        .await;
        match result {
            Ok(activated) => return Ok(activated),
            Err(error)
                if attempt == 0
                    && config.headers_helper.is_some()
                    && refreshable_helper_error(&error) => {}
            Err(error) => return Err(error),
        }
    }
    unreachable!("legacy SSE MCP activation loop always returns")
}

#[derive(Debug)]
struct LegacySseError(String);

impl fmt::Display for LegacySseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for LegacySseError {}

struct LegacySseTransport {
    client: reqwest::Client,
    post_url: String,
    headers: http::HeaderMap,
    incoming: mpsc::Receiver<ServerJsonRpcMessage>,
    reader: JoinHandle<()>,
}

impl LegacySseTransport {
    async fn connect(
        url: &str,
        headers: HashMap<HeaderName, HeaderValue>,
        timeout: Duration,
    ) -> CoreResult<Self> {
        let headers = http::HeaderMap::from_iter(headers);
        let client = reqwest::Client::new();
        let response = client
            .get(url)
            .headers(headers.clone())
            .header(http::header::ACCEPT, "text/event-stream")
            .send()
            .await
            .map_err(mcp_error)?;
        let status = response.status();
        if !status.is_success() {
            return Err(CoreError::Tool(format!(
                "legacy MCP SSE endpoint returned HTTP {status}"
            )));
        }
        let base = response.url().clone();
        let (incoming_tx, incoming) = mpsc::channel(64);
        let (endpoint_tx, endpoint_rx) = oneshot::channel();
        let reader = tokio::spawn(async move {
            let _ = pump_legacy_sse(response, base, endpoint_tx, incoming_tx).await;
        });
        let post_url = tokio::time::timeout(timeout, endpoint_rx)
            .await
            .map_err(|_| CoreError::Tool("legacy MCP SSE endpoint event timed out".into()))?
            .map_err(|_| {
                CoreError::Tool("legacy MCP SSE stream closed before endpoint".into())
            })??;
        Ok(Self {
            client,
            post_url,
            headers,
            incoming,
            reader,
        })
    }
}

impl Transport<RoleClient> for LegacySseTransport {
    type Error = LegacySseError;

    fn send(
        &mut self,
        item: ClientJsonRpcMessage,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let client = self.client.clone();
        let post_url = self.post_url.clone();
        let headers = self.headers.clone();
        async move {
            let response = client
                .post(post_url)
                .headers(headers)
                .header(http::header::ACCEPT, "application/json, text/event-stream")
                .json(&item)
                .send()
                .await
                .map_err(|error| LegacySseError(error.to_string()))?;
            if !response.status().is_success() {
                return Err(LegacySseError(format!(
                    "legacy MCP SSE POST returned HTTP {}",
                    response.status()
                )));
            }
            Ok(())
        }
    }

    async fn receive(&mut self) -> Option<ServerJsonRpcMessage> {
        self.incoming.recv().await
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.reader.abort();
        Ok(())
    }
}

async fn pump_legacy_sse(
    response: reqwest::Response,
    base: reqwest::Url,
    endpoint_tx: oneshot::Sender<CoreResult<String>>,
    incoming: mpsc::Sender<ServerJsonRpcMessage>,
) -> CoreResult<()> {
    const MAX_EVENT_BYTES: usize = 1024 * 1024;
    let mut endpoint_tx = Some(endpoint_tx);
    let mut buffer = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        buffer.extend_from_slice(&chunk.map_err(mcp_error)?);
        if buffer.len() > MAX_EVENT_BYTES {
            return Err(CoreError::Tool(
                "legacy MCP SSE event exceeded 1 MiB".into(),
            ));
        }
        while let Some(event) = take_sse_event(&mut buffer)? {
            let Some((kind, data)) = parse_sse_event(&event) else {
                continue;
            };
            if kind == "endpoint" {
                if let Some(sender) = endpoint_tx.take() {
                    let endpoint = base
                        .join(data.trim())
                        .map(|url| url.to_string())
                        .map_err(mcp_error);
                    let failed = endpoint.is_err();
                    let _ = sender.send(endpoint);
                    if failed {
                        return Err(CoreError::Tool(
                            "legacy MCP SSE returned an invalid endpoint".into(),
                        ));
                    }
                }
                continue;
            }
            if kind.is_empty() || kind == "message" {
                let message =
                    serde_json::from_str::<ServerJsonRpcMessage>(&data).map_err(|error| {
                        CoreError::Tool(format!("invalid legacy MCP SSE JSON: {error}"))
                    })?;
                if incoming.send(message).await.is_err() {
                    return Ok(());
                }
            }
        }
    }
    if let Some(sender) = endpoint_tx {
        let _ = sender.send(Err(CoreError::Tool(
            "legacy MCP SSE stream closed before endpoint".into(),
        )));
    }
    Ok(())
}

fn take_sse_event(buffer: &mut Vec<u8>) -> CoreResult<Option<Vec<u8>>> {
    let separator = (0..buffer.len()).find_map(|index| {
        buffer
            .get(index..index + 4)
            .filter(|bytes| *bytes == b"\r\n\r\n")
            .map(|_| (index, 4))
            .or_else(|| {
                buffer
                    .get(index..index + 2)
                    .filter(|bytes| *bytes == b"\n\n")
                    .map(|_| (index, 2))
            })
    });
    let Some((index, length)) = separator else {
        return Ok(None);
    };
    let event = buffer[..index].to_vec();
    buffer.drain(..index + length);
    String::from_utf8(event.clone())
        .map_err(|error| CoreError::Tool(format!("legacy MCP SSE was not UTF-8: {error}")))?;
    Ok(Some(event))
}

fn parse_sse_event(event: &[u8]) -> Option<(String, String)> {
    let body = std::str::from_utf8(event).ok()?;
    let mut kind = String::new();
    let mut data = Vec::new();
    for line in body.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(value) = line.strip_prefix("event:") {
            value.trim_start().clone_into(&mut kind);
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value));
        }
    }
    (!data.is_empty()).then(|| (kind, data.join("\n")))
}

fn to_output(result: rmcp::model::CallToolResult) -> CoreResult<ToolOutput> {
    let mut text_parts = Vec::new();
    let mut attachments = Vec::new();
    let mut attachment_bytes = 0usize;
    for content in &result.content {
        match content {
            rmcp::model::ContentBlock::Text(text) => text_parts.push(text.text.clone()),
            rmcp::model::ContentBlock::Image(image) => {
                count_mcp_attachment(&image.data, &mut attachment_bytes)?;
                attachments.push(format!("data:{};base64,{}", image.mime_type, image.data));
                text_parts.push(format!("[{} image attachment]", image.mime_type));
            }
            rmcp::model::ContentBlock::Audio(audio) => {
                count_mcp_attachment(&audio.data, &mut attachment_bytes)?;
                attachments.push(format!("data:{};base64,{}", audio.mime_type, audio.data));
                text_parts.push(format!("[{} audio attachment]", audio.mime_type));
            }
            other => text_parts.push(serde_json::to_string(other).unwrap_or_default()),
        }
    }
    let text = text_parts.join("\n");
    if result.is_error == Some(true) {
        Err(CoreError::Tool(text))
    } else {
        let mut metadata = result.structured_content.unwrap_or(Value::Null);
        if !attachments.is_empty() {
            if !metadata.is_object() {
                metadata = serde_json::json!({"structuredContent": metadata});
            }
            metadata
                .as_object_mut()
                .expect("metadata was normalized to an object")
                .insert("attachments".into(), serde_json::json!(attachments));
        }
        Ok(ToolOutput { text, metadata })
    }
}

fn count_mcp_attachment(encoded: &str, total: &mut usize) -> CoreResult<()> {
    const MAX_MCP_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
    let decoded = STANDARD
        .decode(encoded)
        .map_err(|error| CoreError::Tool(format!("invalid MCP media base64: {error}")))?;
    *total = total.saturating_add(decoded.len());
    if *total > MAX_MCP_ATTACHMENT_BYTES {
        return Err(CoreError::Tool(
            "MCP media attachments exceed the 10 MiB request limit".into(),
        ));
    }
    Ok(())
}
fn mcp_error(error: impl std::fmt::Display) -> CoreError {
    CoreError::Tool(error.to_string())
}

fn mcp_headers(values: &BTreeMap<String, String>) -> CoreResult<HashMap<HeaderName, HeaderValue>> {
    values
        .iter()
        .map(|(name, value)| {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|error| CoreError::Invalid(format!("invalid MCP header name: {error}")))?;
            let resolved = expand_env_reference(value);
            let value = HeaderValue::from_str(&resolved).map_err(|error| {
                CoreError::Invalid(format!("invalid MCP header value: {error}"))
            })?;
            Ok((name, value))
        })
        .collect()
}

const HEADERS_HELPER_OUTPUT_LIMIT: usize = 64 * 1024;

async fn resolve_headers(
    name: &str,
    config: &McpConfigureRequest,
) -> CoreResult<BTreeMap<String, String>> {
    let Some(helper) = &config.headers_helper else {
        return Ok(config.headers.clone());
    };
    let url = config.url.as_deref().unwrap_or_default();
    let mut command = if cfg!(windows) {
        let mut command = tokio::process::Command::new("cmd.exe");
        command.args(["/d", "/s", "/c", helper]);
        command
    } else {
        let mut command = tokio::process::Command::new("sh");
        command.args(["-lc", helper]);
        command
    };
    if let Some(cwd) = &config.cwd {
        command.current_dir(cwd);
    }
    command
        .envs(&config.env)
        .env("CLAUDE_CODE_MCP_SERVER_NAME", name)
        .env("CLAUDE_CODE_MCP_SERVER_URL", url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let child = command.spawn().map_err(|_| {
        CoreError::Tool(format!(
            "headersHelper for MCP server \"{name}\" could not be executed"
        ))
    })?;
    let timeout = Duration::from_millis(config.timeout_ms.unwrap_or(10_000).clamp(1, 600_000));
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| CoreError::Tool(format!("headersHelper for MCP server \"{name}\" timed out")))?
        .map_err(|_| {
            CoreError::Tool(format!(
                "headersHelper for MCP server \"{name}\" could not be executed"
            ))
        })?;
    if output.stdout.len().saturating_add(output.stderr.len()) > HEADERS_HELPER_OUTPUT_LIMIT {
        return Err(CoreError::Tool(format!(
            "headersHelper for MCP server \"{name}\" produced more than 64 KiB of output"
        )));
    }
    if !output.status.success() {
        return Err(CoreError::Tool(format!(
            "headersHelper for MCP server \"{name}\" exited with code {}",
            output.status.code().unwrap_or(-1)
        )));
    }
    let dynamic: BTreeMap<String, String> =
        serde_json::from_slice(&output.stdout).map_err(|_| {
            CoreError::Tool(format!(
                "headersHelper for MCP server \"{name}\" returned malformed JSON"
            ))
        })?;
    let mut headers = config.headers.clone();
    for (name, value) in dynamic {
        let previous = headers
            .keys()
            .filter(|previous| previous.eq_ignore_ascii_case(&name))
            .cloned()
            .collect::<Vec<_>>();
        for previous in previous {
            headers.remove(&previous);
        }
        headers.insert(name, value);
    }
    Ok(headers)
}

fn refreshable_helper_error(error: &CoreError) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    [
        "401",
        "403",
        "unauthorized",
        "forbidden",
        "authentication",
        "disconnected",
        "connection closed",
        "transport closed",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

/// Resolve the `${VAR}` and `${VAR:-fallback}` forms used by Claude Code and
/// Haarness MCP configuration at connection time. Keeping the reference in
/// the config avoids copying bearer tokens into generated config files.
fn expand_env_reference(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            output.push_str(&rest[start..]);
            return output;
        };
        let token = &after[..end];
        let (name, fallback) = token
            .split_once(":-")
            .map_or((token, None), |(name, fallback)| (name, Some(fallback)));
        match std::env::var(name) {
            Ok(value) => output.push_str(&value),
            Err(_) => output.push_str(fallback.unwrap_or_default()),
        }
        rest = &after[end + 1..];
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };

    async fn read_http_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
        let (request, _, body) = read_http_exchange(stream).await;
        (request, body)
    }

    async fn read_http_exchange(stream: &mut TcpStream) -> (String, String, Vec<u8>) {
        let mut request = Vec::new();
        let mut scratch = [0_u8; 4096];
        let header_end = loop {
            let count = stream.read(&mut scratch).await.unwrap();
            assert!(count > 0, "fixture HTTP request closed before headers");
            request.extend_from_slice(&scratch[..count]);
            if let Some(index) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = std::str::from_utf8(&request[..header_end])
            .unwrap()
            .to_owned();
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or_default();
        while request.len() < header_end + content_length {
            let count = stream.read(&mut scratch).await.unwrap();
            assert!(count > 0, "fixture HTTP request closed before body");
            request.extend_from_slice(&scratch[..count]);
        }
        let request_line = headers.lines().next().unwrap().to_owned();
        let body = request[header_end..header_end + content_length].to_vec();
        (request_line, headers, body)
    }

    async fn write_json_response(stream: &mut TcpStream, status: &str, value: &Value) {
        let body = serde_json::to_vec(value).unwrap();
        stream
            .write_all(
                format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        stream.write_all(&body).await.unwrap();
    }

    async fn run_oauth_fixture(listener: TcpListener) {
        let address = listener.local_addr().unwrap();
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (request, headers, body) = read_http_exchange(&mut stream).await;
            let path = request.split_whitespace().nth(1).unwrap_or("/");
            let method = request.split_whitespace().next().unwrap_or("");
            let authorized = headers.lines().any(|line| {
                line.eq_ignore_ascii_case("authorization: Bearer fixture-access-token")
            });

            if method == "GET" && path.contains("oauth-protected-resource") {
                write_json_response(
                    &mut stream,
                    "200 OK",
                    &json!({
                        "resource": format!("http://{address}/mcp"),
                        "authorization_servers": [format!("http://{address}")],
                        "scopes_supported": ["mcp"]
                    }),
                )
                .await;
                continue;
            }
            if method == "GET" && path.contains(".well-known") {
                write_json_response(
                    &mut stream,
                    "200 OK",
                    &json!({
                        "issuer": format!("http://{address}"),
                        "authorization_endpoint": format!("http://{address}/authorize"),
                        "token_endpoint": format!("http://{address}/token"),
                        "code_challenge_methods_supported": ["S256"],
                        "scopes_supported": ["mcp"]
                    }),
                )
                .await;
                continue;
            }
            if method == "POST" && path == "/token" {
                let form = std::str::from_utf8(&body).unwrap();
                assert!(form.contains("code=fixture-code"));
                assert!(form.contains("code_verifier="));
                write_json_response(
                    &mut stream,
                    "200 OK",
                    &json!({
                        "access_token": "fixture-access-token",
                        "token_type": "Bearer",
                        "expires_in": 3600,
                        "scope": "mcp"
                    }),
                )
                .await;
                continue;
            }
            if path == "/mcp" && !authorized {
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer resource_metadata=\"http://{address}/.well-known/oauth-protected-resource\", scope=\"mcp\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                continue;
            }
            assert_eq!(path, "/mcp");
            let message: Value = serde_json::from_slice(&body).unwrap();
            let Some(id) = message.get("id").cloned() else {
                stream
                    .write_all(
                        b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await
                    .unwrap();
                continue;
            };
            let result = match message.get("method").and_then(Value::as_str) {
                Some("initialize") => json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "oauth-fixture", "version": "1.0.0" }
                }),
                Some("tools/list") => json!({
                    "tools": [{
                        "name": "secure_echo",
                        "description": "authorized fixture",
                        "inputSchema": { "type": "object" }
                    }]
                }),
                Some("tools/call") => json!({
                    "content": [{ "type": "text", "text": "OAuth MCP call succeeded" }],
                    "isError": false
                }),
                method => panic!("unexpected OAuth fixture method {method:?}"),
            };
            write_json_response(
                &mut stream,
                "200 OK",
                &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            )
            .await;
        }
    }

    async fn run_legacy_sse_fixture(listener: TcpListener) {
        let address = listener.local_addr().unwrap();
        let (mut stream, _) = listener.accept().await.unwrap();
        let (request, _) = read_http_request(&mut stream).await;
        assert!(request.starts_with("GET /sse "));
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\nevent: endpoint\ndata: http://{address}/messages\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let (events_tx, mut events_rx) = mpsc::unbounded_channel::<String>();
        let _writer = tokio::spawn(async move {
            while let Some(event) = events_rx.recv().await {
                stream
                    .write_all(format!("event: message\ndata: {event}\n\n").as_bytes())
                    .await
                    .unwrap();
            }
        });

        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (request, body) = read_http_request(&mut stream).await;
            assert!(request.starts_with("POST /messages "));
            let message: Value = serde_json::from_slice(&body).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            let Some(id) = message.get("id").cloned() else {
                continue;
            };
            let result = match message.get("method").and_then(Value::as_str) {
                Some("initialize") => json!({
                    "protocolVersion": message
                        .pointer("/params/protocolVersion")
                        .cloned()
                        .unwrap_or_else(|| json!("2024-11-05")),
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "legacy-fixture", "version": "1.0.0" }
                }),
                Some("tools/list") => json!({
                    "tools": [{
                        "name": "echo",
                        "description": "fixture echo",
                        "inputSchema": { "type": "object" }
                    }]
                }),
                Some("tools/call") => json!({
                    "content": [{ "type": "text", "text": "legacy SSE call succeeded" }],
                    "isError": false
                }),
                method => panic!("unexpected legacy SSE fixture method {method:?}"),
            };
            events_tx
                .send(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string())
                .unwrap();
        }
    }

    #[tokio::test]
    async fn legacy_sse_transport_discovers_and_calls_plugin_tools() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(run_legacy_sse_fixture(listener));
        let registry = NativeMcpRegistry::default();
        let info = registry
            .configure(
                "legacy",
                McpConfigureRequest {
                    transport: "sse".into(),
                    command: None,
                    args: Vec::new(),
                    env: BTreeMap::new(),
                    cwd: None,
                    url: Some(format!("http://{address}/sse")),
                    headers: BTreeMap::new(),
                    headers_helper: None,
                    timeout_ms: Some(2_000),
                    oauth: None,
                    oauth_client_id: None,
                    oauth_scopes: Vec::new(),
                    enabled: true,
                },
            )
            .await
            .unwrap();
        assert_eq!(info.status, "connected");
        assert_eq!(
            registry.tools("legacy").await.unwrap()[0].name,
            "mcp__legacy__echo"
        );
        let output = registry
            .call("legacy", "mcp__legacy__echo", json!({ "text": "hi" }))
            .await
            .unwrap();
        assert_eq!(output.text, "legacy SSE call succeeded");
        server.abort();
    }

    #[tokio::test]
    async fn oauth_logout_process() {
        if let Some(root) = std::env::var_os("HAARNESS_LOGOUT_FIXTURE") {
            manager_store("secure", Some(Path::new(&root))).clear().await.unwrap();
        }
    }

    #[tokio::test]
    async fn oauth_pkce_lifecycle_reaches_a_protected_mcp_tool() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(run_oauth_fixture(listener));
        let auth_dir =
            std::env::temp_dir().join(format!("haarness-mcp-oauth-test-{}", uuid::Uuid::now_v7()));
        let registry = NativeMcpRegistry::with_auth_dir(&auth_dir);
        let config = McpConfigureRequest {
            transport: "streamable_http".into(),
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            url: Some(format!("http://{address}/mcp")),
            headers: BTreeMap::new(),
            headers_helper: None,
            timeout_ms: Some(2_000),
            oauth: Some(true),
            oauth_client_id: Some("fixture-client".into()),
            oauth_scopes: vec!["mcp".into()],
            enabled: true,
        };
        let info = registry.configure("secure", config.clone()).await.unwrap();
        assert_eq!(info.status, "needs_auth");

        let redirect_uri = "http://127.0.0.1:19876/mcp/oauth/callback";
        let session = registry.begin_oauth("secure", redirect_uri).await.unwrap();
        let authorization = reqwest::Url::parse(&session.authorization_url).unwrap();
        assert_eq!(authorization.path(), "/authorize");
        let state = authorization
            .query_pairs()
            .find_map(|(name, value)| (name == "state").then(|| value.into_owned()))
            .expect("authorization URL has a CSRF state");
        session
            .complete(&format!("{redirect_uri}?code=fixture-code&state={state}"))
            .await
            .unwrap();

        let info = registry.reconnect("secure").await.unwrap();
        assert_eq!(info.status, "connected");
        assert_eq!(registry.tools("secure").await.unwrap().len(), 1);
        let output = registry
            .call("secure", "secure_echo", json!({}))
            .await
            .unwrap();
        assert_eq!(output.text, "OAuth MCP call succeeded");

        let sibling = NativeMcpRegistry::with_auth_dir(&auth_dir);
        sibling.configure("secure", config.clone()).await.unwrap();
        let independent = NativeMcpRegistry::with_auth_dir(&auth_dir);
        let mut explicit = config.clone();
        explicit.headers.insert("Authorization".into(), "Bearer fixture-access-token".into());
        independent.configure("secure", explicit).await.unwrap();
        let previous = manager_store("secure", Some(&auth_dir));
        let credentials = previous.load().await.unwrap().unwrap();
        // Exercise the same credential store from a separate OS process.
        let result = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::oauth_logout_process"])
            .env("HAARNESS_LOGOUT_FIXTURE", &auth_dir)
            .output().await.unwrap();
        assert!(result.status.success(), "logout subprocess failed");
        assert!(previous.save(credentials).await.is_err(), "late token refresh restored cleared credentials");
        assert!(sibling.tool_snapshot().is_empty());
        assert!(sibling.tools("secure").await.unwrap().is_empty());
        assert!(sibling.call("secure", "secure_echo", json!({})).await.is_err());
        assert_eq!(sibling.list().await.unwrap()[0].status, "needs_auth");
        assert_eq!(independent.tool_snapshot().len(), 1);
        assert!(independent.call("secure", "secure_echo", json!({})).await.is_ok());

        registry.clear_oauth("secure").await.unwrap();
        assert!(!manager_store("secure", Some(&auth_dir)).path.exists());
        assert!(registry.tool_snapshot().is_empty());
        assert!(registry.tools("secure").await.unwrap().is_empty());
        assert!(
            registry
                .call("secure", "secure_echo", json!({}))
                .await
                .is_err()
        );
        assert_eq!(
            registry.reconnect("secure").await.unwrap().status,
            "needs_auth"
        );
        assert!(registry.tool_snapshot().is_empty());
        server.abort();
        let _ = std::fs::remove_dir_all(auth_dir);
    }

    #[test]
    fn legacy_sse_parser_accepts_crlf_and_multiline_data() {
        let mut buffer =
            b"event: message\r\ndata: {\"hello\":\r\ndata: \"world\"}\r\n\r\nnext".to_vec();
        let event = take_sse_event(&mut buffer).unwrap().unwrap();
        assert_eq!(
            parse_sse_event(&event),
            Some(("message".into(), "{\"hello\":\n\"world\"}".into()))
        );
        assert_eq!(buffer, b"next");
    }

    #[tokio::test]
    async fn disabled_server_stays_disabled() {
        let registry = NativeMcpRegistry::default();
        let info = registry
            .configure(
                "fixture",
                McpConfigureRequest {
                    transport: "stdio".into(),
                    command: Some("false".into()),
                    args: vec![],
                    env: BTreeMap::new(),
                    cwd: None,
                    url: None,
                    headers: BTreeMap::new(),
                    headers_helper: None,
                    timeout_ms: None,
                    oauth: None,
                    oauth_client_id: None,
                    oauth_scopes: Vec::new(),
                    enabled: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(info.status, "disabled");
        assert!(registry.tools("fixture").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn replacement_generation_swaps_the_complete_server_set() {
        let config = McpConfigureRequest {
            transport: "stdio".into(),
            command: Some("false".into()),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            url: None,
            headers: BTreeMap::new(),
            headers_helper: None,
            timeout_ms: None,
            oauth: None,
            oauth_client_id: None,
            oauth_scopes: Vec::new(),
            enabled: false,
        };
        let live = NativeMcpRegistry::default();
        live.configure("old", config.clone()).await.unwrap();
        let candidate = NativeMcpRegistry::default();
        candidate.configure("new", config).await.unwrap();

        assert_eq!(live.list().await.unwrap()[0].name, "old");
        live.replace_from(&candidate);
        let replaced = live.list().await.unwrap();
        assert_eq!(replaced.len(), 1);
        assert_eq!(replaced[0].name, "new");
        assert!(matches!(
            live.tools("old").await,
            Err(CoreError::NotFound(_))
        ));
    }

    #[test]
    fn resolves_environment_references_without_persisting_the_secret() {
        let home = std::env::var("HOME").unwrap_or_default();
        assert_eq!(
            expand_env_reference("prefix:${HOME}"),
            format!("prefix:{home}")
        );
        assert_eq!(
            expand_env_reference("${HAARNESS_MCP_MISSING:-fallback}"),
            "fallback"
        );
    }

    #[tokio::test]
    async fn headers_helper_receives_context_and_overrides_static_headers() {
        let root =
            std::env::temp_dir().join(format!("haarness-mcp-helper-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let script = root.join("headers.sh");
        std::fs::write(
            &script,
            r#"printf '{"authorization":"Bearer dynamic","X-Name":"%s","X-URL":"%s"}' "$CLAUDE_CODE_MCP_SERVER_NAME" "$CLAUDE_CODE_MCP_SERVER_URL""#,
        )
        .unwrap();
        let config = McpConfigureRequest {
            transport: "streamable_http".into(),
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: Some(root.display().to_string()),
            url: Some("https://example.test/mcp".into()),
            headers: BTreeMap::from([
                ("Authorization".into(), "Bearer stale".into()),
                ("X-Static".into(), "kept".into()),
            ]),
            headers_helper: Some(format!("sh {}", script.display())),
            timeout_ms: Some(2_000),
            oauth: None,
            oauth_client_id: None,
            oauth_scopes: Vec::new(),
            enabled: true,
        };
        let headers = resolve_headers("weather", &config).await.unwrap();
        assert_eq!(
            headers.get("authorization").map(String::as_str),
            Some("Bearer dynamic")
        );
        assert!(!headers.contains_key("Authorization"));
        assert_eq!(headers.get("X-Static").map(String::as_str), Some("kept"));
        assert_eq!(headers.get("X-Name").map(String::as_str), Some("weather"));
        assert_eq!(
            headers.get("X-URL").map(String::as_str),
            Some("https://example.test/mcp")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dynamic_headers_refresh_only_for_auth_or_transport_failures() {
        assert!(refreshable_helper_error(&CoreError::Tool(
            "HTTP 401 Unauthorized".into()
        )));
        assert!(refreshable_helper_error(&CoreError::Conflict(
            "server disconnected".into()
        )));
        assert!(!refreshable_helper_error(&CoreError::Tool(
            "tool rejected its input".into()
        )));
    }

    #[test]
    fn rich_mcp_image_and_audio_outputs_remain_provider_visible() {
        let result = rmcp::model::CallToolResult::success(vec![
            rmcp::model::ContentBlock::text("generated media"),
            rmcp::model::ContentBlock::image("iVBORw0KGgo=", "image/png"),
            rmcp::model::ContentBlock::audio("SUQz", "audio/mpeg"),
        ]);
        let output = to_output(result).unwrap();
        assert!(output.text.contains("generated media"));
        assert_eq!(
            output.metadata["attachments"],
            serde_json::json!([
                "data:image/png;base64,iVBORw0KGgo=",
                "data:audio/mpeg;base64,SUQz"
            ])
        );
    }

    #[test]
    fn invalid_mcp_media_is_rejected_before_it_reaches_the_transcript() {
        let result = rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::image(
            "not base64",
            "image/png",
        )]);
        assert!(
            to_output(result)
                .unwrap_err()
                .to_string()
                .contains("base64")
        );
    }
}

fn transport_failure(error: &CoreError) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    [
        "disconnected",
        "connection closed",
        "transport closed",
        "broken pipe",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    fn config(log: &Path, delay: &str) -> McpConfigureRequest {
        serde_json::from_value(serde_json::json!({
            "transport": "stdio", "command": "python3",
            "args": [concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/recovery.py")],
            "env": {"CALL_LOG": log, "INIT_DELAY": delay},
            "enabled": true, "timeout_ms": 2000
        }))
        .unwrap()
    }

    async fn status(registry: &NativeMcpRegistry, expected: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if registry
                    .list()
                    .await
                    .unwrap()
                    .first()
                    .is_some_and(|info| info.status == expected)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn tool_list_changed_refreshes_connected_catalog() {
        let registry = NativeMcpRegistry::default();
        registry.configure("catalog", serde_json::from_value(serde_json::json!({
            "transport":"stdio", "command":"python3", "args":[concat!(env!("CARGO_MANIFEST_DIR"),"/tests/fixtures/catalog.py")],"enabled":true
        })).unwrap()).await.unwrap();
        assert!(
            registry
                .tool_snapshot()
                .iter()
                .any(|tool| tool.name == "mcp__catalog__old_tool")
        );
        registry
            .call("catalog", "refresh", serde_json::json!({}))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let tools = registry.tool_snapshot();
                if tools
                    .iter()
                    .any(|tool| tool.name == "mcp__catalog__new_tool")
                {
                    assert!(
                        !tools
                            .iter()
                            .any(|tool| tool.name == "mcp__catalog__old_tool")
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        registry.disconnect("catalog").await.unwrap();
    }

    #[tokio::test]
    async fn transport_exit_invalidates_tools_and_recovers_without_replaying_the_call() {
        let log = std::env::temp_dir().join(format!("haarness-mcp-{}", uuid::Uuid::now_v7()));
        let registry = NativeMcpRegistry::default();
        registry
            .configure("fixture", config(&log, "0"))
            .await
            .unwrap();
        assert!(
            registry
                .call("fixture", "crash", serde_json::json!({}))
                .await
                .is_err()
        );
        status(&registry, "failed").await;
        assert!(!registry.tool_snapshot().is_empty());
        assert!(registry.tools("fixture").await.unwrap().is_empty());
        assert!(
            registry
                .call("fixture", "ping", serde_json::json!({}))
                .await
                .is_err()
        );
        status(&registry, "connected").await;
        assert_eq!(
            registry
                .call("fixture", "ping", serde_json::json!({}))
                .await
                .unwrap()
                .text,
            "pong"
        );
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "called\n");
        registry.disconnect("fixture").await.unwrap();
        std::fs::remove_file(log).unwrap();
    }

    #[tokio::test]
    async fn disconnect_cancels_a_pending_transport_recovery() {
        let log = std::env::temp_dir().join(format!("haarness-mcp-{}", uuid::Uuid::now_v7()));
        let registry = NativeMcpRegistry::default();
        registry
            .configure("fixture", config(&log, "0"))
            .await
            .unwrap();
        assert!(
            registry
                .call("fixture", "crash", serde_json::json!({}))
                .await
                .is_err()
        );
        status(&registry, "failed").await;
        registry.disconnect("fixture").await.unwrap();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(registry.list().await.unwrap()[0].status, "disconnected");
        assert!(registry.tool_snapshot().is_empty());
        std::fs::remove_file(log).unwrap();
    }

    #[tokio::test]
    async fn late_connection_cannot_resurrect_a_disconnected_server() {
        let registry = NativeMcpRegistry::default();
        let worker = registry.clone();
        let task = tokio::spawn(async move {
            worker
                .configure("fixture", config(Path::new("unused"), "0.3"))
                .await
        });
        status(&registry, "connecting").await;
        registry.disconnect("fixture").await.unwrap();
        assert!(matches!(task.await.unwrap(), Err(CoreError::Conflict(_))));
        assert_eq!(registry.list().await.unwrap()[0].status, "disconnected");
        assert!(registry.tool_snapshot().is_empty());
    }
    #[tokio::test]
    async fn failed_list_change_invalidates_live_and_remembered_schemas() {
        let registry = NativeMcpRegistry::default();
        registry
            .configure(
                "catalog",
                serde_json::from_value(serde_json::json!({
                    "transport":"stdio", "command":"python3",
                    "args":[concat!(env!("CARGO_MANIFEST_DIR"),"/tests/fixtures/catalog.py")],
                    "env":{"FAIL_REFRESH":"1"}, "enabled":true
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        assert!(!registry.tool_snapshot().is_empty());
        registry
            .call("catalog", "refresh", serde_json::json!({}))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !registry.tool_snapshot().is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(registry.tools("catalog").await.unwrap().is_empty());
        assert!(registry.servers.read()["catalog"].remembered.is_empty());
        registry.disconnect("catalog").await.unwrap();
    }

    #[tokio::test]
    async fn reconfiguration_does_not_recall_the_previous_connections_schemas() {
        let log = std::env::temp_dir().join(format!("haarness-schema-{}", uuid::Uuid::now_v7()));
        let registry = NativeMcpRegistry::default();
        registry
            .configure("fixture", config(&log, "0"))
            .await
            .unwrap();
        assert!(!registry.tool_snapshot().is_empty());
        let mut changed = config(&log, "0");
        changed.command = Some("/missing/haarness-fixture-command".into());
        assert!(registry.configure("fixture", changed).await.is_err());
        assert!(registry.tool_snapshot().is_empty());
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(registry.tool_snapshot().is_empty());
        registry.disconnect("fixture").await.unwrap();
    }

    #[test]
    fn remembered_identity_changes_with_configuration_and_oauth_credentials() {
        let root = std::env::temp_dir().join(format!("haarness-identity-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let mut config = config(Path::new("unused"), "0");
        let first = schema_identity("fixture", &config, Some(&root)).unwrap();
        config
            .env
            .insert("AUTH_SCOPE".into(), "second-account".into());
        assert_ne!(
            Some(first),
            schema_identity("fixture", &config, Some(&root))
        );
        config.transport = "streamable_http".into();
        config.oauth = Some(true);
        let empty = schema_identity("fixture", &config, Some(&root)).unwrap();
        let path = manager_store("fixture", Some(&root)).path;
        std::fs::write(&path, "first-credential").unwrap();
        let credential = schema_identity("fixture", &config, Some(&root)).unwrap();
        assert_ne!(empty, credential);
        std::fs::write(&path, "second-credential").unwrap();
        assert_ne!(
            Some(credential),
            schema_identity("fixture", &config, Some(&root))
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
