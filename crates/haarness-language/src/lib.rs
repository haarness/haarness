//! Workspace-scoped formatter and Language Server Protocol process host.
//!
//! The host deliberately speaks JSON-RPC directly instead of exposing LSP
//! transport types across the rest of Haarness. Servers are lazy: touching a
//! matching file starts at most one process for that configured server. A
//! missing or broken optional server never makes a successful file edit fail.

use futures::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::RwLock as SyncRwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, Notify, RwLock, oneshot};
use tokio_util::sync::CancellationToken;

const DEFAULT_DIAGNOSTIC_TIMEOUT_MS: u64 = 2_000;
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 10_000;
const MAX_MESSAGE_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_DIAGNOSTICS: usize = 100;

fn true_value() -> bool {
    true
}

fn default_diagnostic_timeout_ms() -> u64 {
    DEFAULT_DIAGNOSTIC_TIMEOUT_MS
}

fn default_request_timeout_ms() -> u64 {
    DEFAULT_REQUEST_TIMEOUT_MS
}

fn default_formatter_timeout_ms() -> u64 {
    30_000
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct LspConfig {
    pub enabled: bool,
    pub builtins: bool,
    pub diagnostic_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub servers: BTreeMap<String, LspServerConfig>,
    /// Idle clients are stopped and restarted on demand.
    pub idle_timeout_ms: u64,
    pub max_clients: usize,
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            builtins: true,
            diagnostic_timeout_ms: default_diagnostic_timeout_ms(),
            request_timeout_ms: default_request_timeout_ms(),
            servers: BTreeMap::new(),
            idle_timeout_ms: 600_000,
            max_clients: 6,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct LspServerConfig {
    pub initialize_timeout_ms: Option<u64>,
    pub request_timeout_ms: Option<u64>,
    pub diagnostic_timeout_ms: Option<u64>,
    pub enabled: bool,
    pub command: Vec<String>,
    pub extensions: Vec<String>,
    pub filenames: Vec<String>,
    pub language_ids: BTreeMap<String, String>,
    pub environment: BTreeMap<String, String>,
    pub initialization: Value,
}

impl Default for LspServerConfig {
    fn default() -> Self {
        Self {
            initialize_timeout_ms: None,
            request_timeout_ms: None,
            diagnostic_timeout_ms: None,
            enabled: true_value(),
            command: Vec::new(),
            extensions: Vec::new(),
            filenames: Vec::new(),
            language_ids: BTreeMap::new(),
            environment: BTreeMap::new(),
            initialization: Value::Object(serde_json::Map::new()),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct FormatterConfig {
    pub enabled: bool,
    pub builtins: bool,
    pub formatters: BTreeMap<String, FormatterDefinition>,
}

impl Default for FormatterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            builtins: true,
            formatters: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct FormatterDefinition {
    pub enabled: bool,
    pub command: Vec<String>,
    pub extensions: Vec<String>,
    pub filenames: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub timeout_ms: u64,
}

impl Default for FormatterDefinition {
    fn default() -> Self {
        Self {
            enabled: true_value(),
            command: Vec::new(),
            extensions: Vec::new(),
            filenames: Vec::new(),
            environment: BTreeMap::new(),
            timeout_ms: default_formatter_timeout_ms(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub server: String,
    pub file: String,
    pub range: Range,
    pub severity: Option<u8>,
    pub code: Option<String>,
    pub source: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageStatus {
    pub id: String,
    pub status: String,
    pub extensions: Vec<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatterStatus {
    pub id: String,
    pub status: String,
    pub extensions: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileReport {
    pub file: PathBuf,
    pub formatted_by: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
pub enum Operation {
    Definition,
    References,
    Hover,
    DocumentSymbols,
    WorkspaceSymbols,
    Implementation,
    PrepareCallHierarchy,
    IncomingCalls,
    OutgoingCalls,
}

impl Operation {
    fn method(self) -> &'static str {
        match self {
            Self::Definition => "textDocument/definition",
            Self::References => "textDocument/references",
            Self::Hover => "textDocument/hover",
            Self::DocumentSymbols => "textDocument/documentSymbol",
            Self::WorkspaceSymbols => "workspace/symbol",
            Self::Implementation => "textDocument/implementation",
            Self::PrepareCallHierarchy => "textDocument/prepareCallHierarchy",
            Self::IncomingCalls => "callHierarchy/incomingCalls",
            Self::OutgoingCalls => "callHierarchy/outgoingCalls",
        }
    }
}

struct ServerSlot {
    config: LspServerConfig,
    /// One running server per folder it was started for: the project
    /// folder, or a chat's own folder, whose files it then sees alone.
    clients: Mutex<BTreeMap<PathBuf, Arc<Client>>>,
    error: RwLock<Option<String>>,
    retry_after: Mutex<Option<Instant>>,
}

pub struct LanguageService {
    workspace: PathBuf,
    state: SyncRwLock<Arc<LanguageState>>,
}

struct LanguageState {
    lifecycle: Mutex<()>,
    sweeping: AtomicBool,
    closed: AtomicBool,
    lsp: LspConfig,
    formatters: BTreeMap<String, FormatterDefinition>,
    servers: BTreeMap<String, Arc<ServerSlot>>,
}

impl LanguageService {
    #[must_use]
    pub fn new(workspace: PathBuf, lsp: LspConfig, formatter: FormatterConfig) -> Self {
        let mut servers = if lsp.enabled && lsp.builtins {
            builtin_servers()
        } else {
            BTreeMap::new()
        };
        if lsp.enabled {
            for (id, config) in &lsp.servers {
                if config.enabled {
                    servers.insert(id.clone(), config.clone());
                } else {
                    servers.remove(id);
                }
            }
        }
        let mut formatters = if formatter.enabled && formatter.builtins {
            builtin_formatters()
        } else {
            BTreeMap::new()
        };
        if formatter.enabled {
            for (id, config) in formatter.formatters {
                if config.enabled {
                    formatters.insert(id, config);
                } else {
                    formatters.remove(&id);
                }
            }
        }
        let servers = servers
            .into_iter()
            .map(|(id, config)| {
                (
                    id,
                    Arc::new(ServerSlot {
                        config,
                        clients: Mutex::new(BTreeMap::new()),
                        error: RwLock::new(None),
                        retry_after: Mutex::new(None),
                    }),
                )
            })
            .collect();
        Self {
            workspace,
            state: SyncRwLock::new(Arc::new(LanguageState {
                lifecycle: Mutex::new(()),
                sweeping: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                lsp,
                formatters,
                servers,
            })),
        }
    }

    fn state(&self) -> Arc<LanguageState> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Install new formatter/LSP definitions for future calls, then stop any
    /// processes that belonged to the retired generation.
    pub async fn reload(&self, lsp: LspConfig, formatter: FormatterConfig) {
        let previous = self.replace_state(lsp, formatter);
        shutdown_language_state(&previous).await;
    }

    /// Synchronous generation swap for a wider application reload
    /// transaction. Retired processes drain asynchronously after new calls
    /// can already see the replacement definitions.
    pub fn replace_config(&self, lsp: LspConfig, formatter: FormatterConfig) {
        let previous = self.replace_state(lsp, formatter);
        tokio::spawn(async move {
            shutdown_language_state(&previous).await;
        });
    }

    fn replace_state(&self, lsp: LspConfig, formatter: FormatterConfig) -> Arc<LanguageState> {
        let replacement = Self::new(self.workspace.clone(), lsp, formatter).state();
        {
            let mut state = self
                .state
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::replace(&mut *state, replacement)
        }
    }

    #[must_use]
    pub fn active(&self) -> bool {
        let state = self.state();
        !state.servers.is_empty() || !state.formatters.is_empty()
    }

    /// Format and diagnose `files` of a call that works in `root` (the
    /// project folder, or a chat's own folder inside it).
    pub async fn process_files(
        self: &Arc<Self>,
        files: Vec<PathBuf>,
        root: &Path,
        cancel: CancellationToken,
    ) -> Vec<FileReport> {
        let futures = files.into_iter().map(|file| {
            let service = Arc::clone(self);
            let cancel = cancel.clone();
            let root = root.to_path_buf();
            async move { service.process_file(file, &root, cancel).await }
        });
        join_all(futures).await
    }

    pub async fn process_file(
        &self,
        file: PathBuf,
        root: &Path,
        cancel: CancellationToken,
    ) -> FileReport {
        let state = self.state();
        let mut report = FileReport {
            file: file.clone(),
            ..FileReport::default()
        };
        let Some(root) = self.root(root) else {
            return report;
        };
        if !file.is_file() || !inside(&root, &file) {
            return report;
        }
        for (id, formatter) in &state.formatters {
            if !matches_file(
                formatter.extensions.as_slice(),
                formatter.filenames.as_slice(),
                &file,
            ) {
                continue;
            }
            match self.run_formatter(formatter, &file, &root, &cancel).await {
                Ok(true) => report.formatted_by.push(id.clone()),
                Ok(false) => {}
                Err(error) => report.warnings.push(format!("formatter {id}: {error}")),
            }
        }
        if !state.lsp.enabled {
            return report;
        }
        for (id, slot) in Self::matching_servers(&state, &file) {
            match self.client(&state, &id, &slot, &root, &cancel).await {
                Ok(client) => match client
                    .touch(
                        &file,
                        slot.config
                            .diagnostic_timeout_ms
                            .unwrap_or(state.lsp.diagnostic_timeout_ms),
                        &cancel,
                    )
                    .await
                {
                    Ok(mut diagnostics) => report.diagnostics.append(&mut diagnostics),
                    Err(error) => report.warnings.push(format!("LSP {id}: {error}")),
                },
                Err(error) => {
                    tracing::debug!(server = id, %error, "optional LSP unavailable");
                }
            }
        }
        report.diagnostics.truncate(MAX_DIAGNOSTICS);
        report
    }

    pub async fn touch(&self, file: &Path, root: &Path, cancel: &CancellationToken) {
        let state = self.state();
        let Some(root) = self.root(root) else {
            return;
        };
        if !state.lsp.enabled || !file.is_file() || !inside(&root, file) {
            return;
        }
        for (id, slot) in Self::matching_servers(&state, file) {
            if let Ok(client) = self.client(&state, &id, &slot, &root, cancel).await {
                let _ = client.touch(file, 0, cancel).await;
            }
        }
    }

    /// Ask the language servers of `root` (where the call works) about
    /// `file`. A server started for a chat's own folder answers only with
    /// places inside it.
    #[allow(clippy::too_many_arguments)] // one query's position, text and scope
    pub async fn query(
        &self,
        operation: Operation,
        file: &Path,
        root: &Path,
        line: u32,
        character: u32,
        query: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<Value, String> {
        let state = self.state();
        let root = self
            .root(root)
            .ok_or_else(|| "the folder is not inside the project".to_owned())?;
        let chat_folder = self.canonical_workspace() != root;
        let matches = Self::matching_servers(&state, file);
        if matches.is_empty() {
            return Err("no configured language server handles this file".into());
        }
        let mut results = Vec::new();
        let mut errors = Vec::new();
        for (id, slot) in matches {
            let client = match self.client(&state, &id, &slot, &root, cancel).await {
                Ok(client) => client,
                Err(error) => {
                    errors.push(format!("{id}: {error}"));
                    continue;
                }
            };
            if let Err(error) = client.touch(file, 0, cancel).await {
                errors.push(format!("{id}: {error}"));
                continue;
            }
            let uri = file_uri(file)?;
            let params = match operation {
                Operation::WorkspaceSymbols => json!({"query": query.unwrap_or_default()}),
                Operation::References => json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": line, "character": character},
                    "context": {"includeDeclaration": true}
                }),
                Operation::IncomingCalls | Operation::OutgoingCalls => {
                    let prepared = client
                        .request(
                            Operation::PrepareCallHierarchy.method(),
                            json!({
                                "textDocument": {"uri": uri},
                                "position": {"line": line, "character": character}
                            }),
                            slot.config
                                .request_timeout_ms
                                .unwrap_or(state.lsp.request_timeout_ms),
                            cancel,
                        )
                        .await;
                    let item = match prepared {
                        Ok(Value::Array(items)) => items.into_iter().next(),
                        Ok(Value::Null) => None,
                        Ok(item) => Some(item),
                        Err(error) => {
                            errors.push(format!("{id}: {error}"));
                            continue;
                        }
                    };
                    let Some(item) = item else {
                        errors.push(format!("{id}: no call hierarchy item at this position"));
                        continue;
                    };
                    json!({"item": item})
                }
                Operation::DocumentSymbols => json!({"textDocument": {"uri": uri}}),
                _ => json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": line, "character": character}
                }),
            };
            match client
                .request(
                    operation.method(),
                    params,
                    slot.config
                        .request_timeout_ms
                        .unwrap_or(state.lsp.request_timeout_ms),
                    cancel,
                )
                .await
            {
                Ok(mut value) => {
                    if chat_folder {
                        keep_places_inside(&mut value, &root);
                    }
                    results.push(json!({"server": id, "result": value}));
                }
                Err(error) => errors.push(format!("{id}: {error}")),
            }
        }
        if results.is_empty() {
            return Err(if errors.is_empty() {
                "no language server returned a result".into()
            } else {
                errors.join("; ")
            });
        }
        Ok(json!({"results": results, "errors": errors}))
    }

    pub async fn status(&self) -> Vec<LanguageStatus> {
        let state = self.state();
        let mut result = Vec::new();
        for (id, slot) in &state.servers {
            let client = slot.clients.lock().await.values().next().cloned();
            // A server that is not installed is missing, whatever launching
            // it anyway reported.
            let missing = client.is_none()
                && !executable_available(
                    &slot.config.command,
                    &slot.config.environment,
                    &self.workspace,
                );
            let status = match &client {
                Some(client) => client.status.read().await.clone(),
                None if missing => "missing".into(),
                None if slot.error.read().await.is_some() => "error".into(),
                None => "idle".into(),
            };
            result.push(LanguageStatus {
                id: id.clone(),
                status,
                extensions: slot.config.extensions.clone(),
                error: if missing {
                    None
                } else {
                    slot.error.read().await.clone()
                },
            });
        }
        result
    }

    #[must_use]
    pub fn formatter_status(&self) -> Vec<FormatterStatus> {
        self.state()
            .formatters
            .iter()
            .map(|(id, formatter)| FormatterStatus {
                id: id.clone(),
                status: if executable_available(
                    &formatter.command,
                    &formatter.environment,
                    &self.workspace,
                ) {
                    "available".into()
                } else {
                    "missing".into()
                },
                extensions: formatter.extensions.clone(),
            })
            .collect()
    }

    pub async fn shutdown(&self) {
        shutdown_language_state(&self.state()).await;
    }

    fn matching_servers(state: &LanguageState, file: &Path) -> Vec<(String, Arc<ServerSlot>)> {
        state
            .servers
            .iter()
            .filter(|(_, slot)| {
                matches_file(
                    slot.config.extensions.as_slice(),
                    slot.config.filenames.as_slice(),
                    file,
                )
            })
            .map(|(id, slot)| (id.clone(), Arc::clone(slot)))
            .collect()
    }

    async fn client(
        &self,
        state: &Arc<LanguageState>,
        id: &str,
        slot: &Arc<ServerSlot>,
        root: &Path,
        cancel: &CancellationToken,
    ) -> Result<Lease, String> {
        let _lifecycle = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err("language server request cancelled".into()),
            guard = state.lifecycle.lock() => guard,
        };
        if state.closed.load(Ordering::SeqCst) {
            return Err("language service is shutting down".into());
        }
        if !state.sweeping.swap(true, Ordering::SeqCst) {
            let weak = Arc::downgrade(state);
            let interval = Duration::from_millis((state.lsp.idle_timeout_ms / 2).clamp(10, 60_000));
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(interval).await;
                    let Some(state) = weak.upgrade() else {
                        break;
                    };
                    let _lifecycle = state.lifecycle.lock().await;
                    if state.closed.load(Ordering::SeqCst) {
                        break;
                    }
                    reap(&state, false).await;
                }
            });
        }
        let mut current = slot.clients.lock().await;
        if let Some(client) = current.get(root)
            && client.status.read().await.as_str() == "running"
        {
            return Ok(Lease::new(Arc::clone(client)).await);
        }
        if slot.config.command.is_empty() {
            return Err("server command is empty".into());
        }
        {
            let retry_after = slot.retry_after.lock().await;
            if retry_after.is_some_and(|retry_after| retry_after > Instant::now()) {
                return Err(slot
                    .error
                    .read()
                    .await
                    .clone()
                    .unwrap_or_else(|| "server launch is in retry backoff".into()));
            }
        }
        drop(current);
        reap(state, true).await;
        let mut count = 0;
        for slot in state.servers.values() {
            count += slot.clients.lock().await.len();
        }
        if count >= state.lsp.max_clients.max(1) {
            return Err("language server capacity is busy".into());
        }
        current = slot.clients.lock().await;
        match Client::spawn(
            id.to_owned(),
            slot.config.clone(),
            root.to_path_buf(),
            slot.config
                .initialize_timeout_ms
                .or(slot.config.request_timeout_ms)
                .unwrap_or(state.lsp.request_timeout_ms),
            cancel,
        )
        .await
        {
            Ok(client) => {
                *slot.error.write().await = None;
                *slot.retry_after.lock().await = None;
                current.insert(root.to_path_buf(), Arc::clone(&client));
                Ok(Lease::new(client).await)
            }
            Err(error) => {
                *slot.error.write().await = Some(error.clone());
                *slot.retry_after.lock().await = Some(Instant::now() + Duration::from_secs(30));
                Err(error)
            }
        }
    }

    async fn run_formatter(
        &self,
        formatter: &FormatterDefinition,
        file: &Path,
        root: &Path,
        cancel: &CancellationToken,
    ) -> Result<bool, String> {
        let Some(program) = formatter.command.first() else {
            return Err("command is empty".into());
        };
        let display = file.to_string_lossy();
        let mut used_file = program.contains("$FILE");
        let program = program.replace("$FILE", &display);
        let mut command = Command::new(program);
        for argument in formatter.command.iter().skip(1) {
            used_file |= argument.contains("$FILE");
            command.arg(argument.replace("$FILE", &display));
        }
        if !used_file {
            command.arg(file);
        }
        // The server's credentials, OpenTelemetry settings and an inherited
        // chat id stay out (`private_env_names`); the configured
        // environment can still set them.
        for name in haarness_core::private_env_names() {
            command.env_remove(name);
        }
        command
            .current_dir(dunce::simplified(root))
            .envs(&formatter.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.to_string()),
        };
        let stderr = child.stderr.take().map(|mut stderr| {
            tokio::spawn(async move {
                let mut output = Vec::new();
                let _ = stderr.read_to_end(&mut output).await;
                output
            })
        });
        let timeout = Duration::from_millis(formatter.timeout_ms.max(1));
        let status = tokio::select! {
            () = cancel.cancelled() => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err("cancelled".into());
            }
            result = tokio::time::timeout(timeout, child.wait()) => {
                match result {
                    Ok(result) => result.map_err(|error| error.to_string())?,
                    Err(_) => {
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                        return Err("timed out".into());
                    }
                }
            }
        };
        let stderr = match stderr {
            Some(stderr) => stderr.await.unwrap_or_default(),
            None => Vec::new(),
        };
        if status.success() {
            return Ok(true);
        }
        let stderr = String::from_utf8_lossy(&stderr).trim().to_owned();
        Err(if stderr.is_empty() {
            format!("exited with {status}")
        } else {
            stderr
        })
    }

    fn canonical_workspace(&self) -> PathBuf {
        self.workspace
            .canonicalize()
            .unwrap_or_else(|_| self.workspace.clone())
    }

    /// `root` canonical, when it is the project folder or a folder inside it.
    fn root(&self, root: &Path) -> Option<PathBuf> {
        let root = root.canonicalize().ok()?;
        root.starts_with(self.canonical_workspace()).then_some(root)
    }
}

fn inside(root: &Path, path: &Path) -> bool {
    path.canonicalize().is_ok_and(|path| path.starts_with(root))
}

/// Leave out of a language server's answer every place (a location, a
/// link's target, a symbol, a call-hierarchy item) in a file outside `root`.
fn keep_places_inside(value: &mut Value, root: &Path) {
    match value {
        Value::Array(items) => {
            items.retain(|item| !item.as_object().is_some_and(|place| outside(place, root)));
            for item in items {
                keep_places_inside(item, root);
            }
        }
        Value::Object(place) if outside(place, root) => *value = Value::Null,
        Value::Object(place) => {
            for item in place.values_mut() {
                keep_places_inside(item, root);
            }
        }
        _ => {}
    }
}

/// Whether `place` names a file outside `root`.
fn outside(place: &serde_json::Map<String, Value>, root: &Path) -> bool {
    let nested = |key: &str| place.get(key).and_then(|value| value.get("uri"));
    place
        .get("uri")
        .or_else(|| place.get("targetUri"))
        .or_else(|| nested("location"))
        .or_else(|| nested("from"))
        .or_else(|| nested("to"))
        .and_then(Value::as_str)
        .is_some_and(|uri| {
            url::Url::parse(uri)
                .ok()
                .and_then(|url| url.to_file_path().ok())
                .is_none_or(|path| {
                    !path
                        .canonicalize()
                        .unwrap_or_else(|_| path.clone())
                        .starts_with(root)
                })
        })
}

async fn shutdown_language_state(state: &LanguageState) {
    let _lifecycle = state.lifecycle.lock().await;
    state.closed.store(true, Ordering::SeqCst);
    for slot in state.servers.values() {
        let clients = slot
            .clients
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for client in clients {
            client.shutdown().await;
        }
    }
}

// A lease covers touch + all requests in one operation, including diagnostic waits.
struct Lease {
    client: Arc<Client>,
    _guard: tokio::sync::OwnedRwLockReadGuard<()>,
}
impl Lease {
    async fn new(client: Arc<Client>) -> Self {
        let guard = client.activity.clone().read_owned().await;
        Self {
            client,
            _guard: guard,
        }
    }
}
impl std::ops::Deref for Lease {
    type Target = Client;
    fn deref(&self) -> &Client {
        &self.client
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        *self
            .client
            .used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
    }
}

// Called under lifecycle: choosing a victim and taking a new lease cannot race.
async fn reap(state: &LanguageState, capacity: bool) {
    let mut count = 0;
    let mut idle = Vec::new();
    for slot in state.servers.values() {
        let current = slot.clients.lock().await;
        for (root, client) in current.iter() {
            count += 1;
            if let Ok(guard) = client.activity.clone().try_write_owned() {
                let used = *client
                    .used
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                idle.push((used, slot.clone(), root.clone(), client.clone(), guard));
            }
        }
    }
    idle.sort_by_key(|entry| entry.0);
    for (used, slot, root, client, _guard) in idle {
        if used.elapsed() >= Duration::from_millis(state.lsp.idle_timeout_ms.max(1))
            || (capacity && count >= state.lsp.max_clients.max(1))
        {
            slot.clients.lock().await.remove(&root);
            client.shutdown().await;
            count -= 1;
        }
    }
}

struct Client {
    activity: Arc<RwLock<()>>,
    used: std::sync::Mutex<Instant>,
    id: String,
    config: LspServerConfig,
    workspace: PathBuf,
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    diagnostics: RwLock<HashMap<String, Vec<Diagnostic>>>,
    diagnostic_generations: Mutex<HashMap<String, u64>>,
    opened: Mutex<HashMap<String, i32>>,
    diagnostics_changed: Notify,
    next: AtomicU64,
    status: RwLock<String>,
}

impl Client {
    async fn spawn(
        id: String,
        config: LspServerConfig,
        workspace: PathBuf,
        timeout_ms: u64,
        cancel: &CancellationToken,
    ) -> Result<Arc<Self>, String> {
        let mut command = Command::new(&config.command[0]);
        // As for a formatter: the configured environment still wins.
        for name in haarness_core::private_env_names() {
            command.env_remove(name);
        }
        command
            .args(config.command.iter().skip(1))
            .current_dir(dunce::simplified(&workspace))
            .envs(&config.environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                format!("{} is not installed", config.command[0])
            } else {
                error.to_string()
            }
        })?;
        let stdin = child.stdin.take().ok_or("language server has no stdin")?;
        let stdout = child.stdout.take().ok_or("language server has no stdout")?;
        let client = Arc::new(Self {
            activity: Arc::new(RwLock::new(())),
            used: std::sync::Mutex::new(Instant::now()),
            id,
            config,
            workspace,
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
            pending: Mutex::new(HashMap::new()),
            diagnostics: RwLock::new(HashMap::new()),
            diagnostic_generations: Mutex::new(HashMap::new()),
            opened: Mutex::new(HashMap::new()),
            diagnostics_changed: Notify::new(),
            next: AtomicU64::new(1),
            status: RwLock::new("starting".into()),
        });
        let reader = Arc::clone(&client);
        tokio::spawn(async move { reader.read_loop(stdout).await });
        let root_uri = file_uri(&client.workspace)?;
        let process_id = std::process::id();
        if let Err(error) = client
            .request(
                "initialize",
                json!({
                    "processId": process_id,
                    "rootUri": root_uri,
                    "workspaceFolders": [{"uri": root_uri, "name": client.workspace.file_name().and_then(|name| name.to_str()).unwrap_or("workspace")}],
                    "clientInfo": {"name": "haarness", "version": env!("CARGO_PKG_VERSION")},
                    "capabilities": {
                        "workspace": {"configuration": true, "workspaceFolders": true},
                        "textDocument": {
                            "publishDiagnostics": {"relatedInformation": true, "versionSupport": true},
                            "definition": {"linkSupport": true},
                            "implementation": {"linkSupport": true},
                            "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                            "callHierarchy": {"dynamicRegistration": false}
                        }
                    },
                    "initializationOptions": client.config.initialization
                }),
                timeout_ms,
                cancel,
            )
            .await
        {
            client.force_kill().await;
            return Err(error);
        }
        if let Err(error) = client.notify("initialized", json!({})).await {
            client.force_kill().await;
            return Err(error);
        }
        *client.status.write().await = "running".into();
        Ok(client)
    }

    async fn touch(
        &self,
        file: &Path,
        diagnostic_timeout_ms: u64,
        cancel: &CancellationToken,
    ) -> Result<Vec<Diagnostic>, String> {
        let uri = file_uri(file)?;
        let text = tokio::fs::read_to_string(file)
            .await
            .map_err(|error| error.to_string())?;
        let language_id = language_id(&self.config, file);
        let generation = self
            .diagnostic_generations
            .lock()
            .await
            .get(&uri)
            .copied()
            .unwrap_or_default();
        self.diagnostics.write().await.remove(&uri);
        let mut opened = self.opened.lock().await;
        if let Some(version) = opened.get_mut(&uri) {
            *version += 1;
            self.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": {"uri": uri, "version": *version},
                    "contentChanges": [{"text": text}]
                }),
            )
            .await?;
        } else {
            opened.insert(uri.clone(), 1);
            self.notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {"uri": uri, "languageId": language_id, "version": 1, "text": text}
                }),
            )
            .await?;
        }
        drop(opened);
        self.notify(
            "textDocument/didSave",
            json!({"textDocument": {"uri": uri}}),
        )
        .await?;
        if diagnostic_timeout_ms > 0 {
            let wait = async {
                loop {
                    let changed = self.diagnostics_changed.notified();
                    if self
                        .diagnostic_generations
                        .lock()
                        .await
                        .get(&uri)
                        .copied()
                        .unwrap_or_default()
                        != generation
                    {
                        break;
                    }
                    changed.await;
                }
            };
            tokio::select! {
                () = cancel.cancelled() => return Err("cancelled".into()),
                _ = tokio::time::timeout(Duration::from_millis(diagnostic_timeout_ms), wait) => {}
            }
        }
        Ok(self
            .diagnostics
            .read()
            .await
            .get(&uri)
            .cloned()
            .unwrap_or_default())
    }

    async fn request(
        &self,
        method: &str,
        params: Value,
        timeout_ms: u64,
        cancel: &CancellationToken,
    ) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.insert(id, sender);
        if let Err(error) = self
            .send(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
            .await
        {
            self.pending.lock().await.remove(&id);
            return Err(error);
        }
        tokio::select! {
            () = cancel.cancelled() => {
                self.pending.lock().await.remove(&id);
                Err("cancelled".into())
            }
            response = tokio::time::timeout(Duration::from_millis(timeout_ms.max(1)), receiver) => {
                match response {
                    Ok(Ok(result)) => result,
                    Ok(Err(_)) => Err("language server response channel closed".into()),
                    Err(_) => {
                        self.pending.lock().await.remove(&id);
                        Err(format!("{method} timed out"))
                    }
                }
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.send(&json!({"jsonrpc":"2.0", "method":method, "params":params}))
            .await
    }

    async fn send(&self, value: &Value) -> Result<(), String> {
        let body = serde_json::to_vec(value).map_err(|error| error.to_string())?;
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
            .await
            .map_err(|error| error.to_string())?;
        stdin
            .write_all(&body)
            .await
            .map_err(|error| error.to_string())?;
        stdin.flush().await.map_err(|error| error.to_string())
    }

    async fn read_loop<R>(self: Arc<Self>, stdout: R)
    where
        R: AsyncRead + Unpin,
    {
        let mut reader = BufReader::new(stdout);
        loop {
            let message = match read_message(&mut reader).await {
                Ok(Some(message)) => message,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(server = self.id, %error, "language server stream failed");
                    break;
                }
            };
            if let Some(id) = message.get("id").and_then(Value::as_u64)
                && message.get("method").is_none()
            {
                if let Some(sender) = self.pending.lock().await.remove(&id) {
                    let result = if let Some(error) = message.get("error") {
                        Err(error.to_string())
                    } else {
                        Ok(message.get("result").cloned().unwrap_or(Value::Null))
                    };
                    let _ = sender.send(result);
                }
                continue;
            }
            let Some(method) = message.get("method").and_then(Value::as_str) else {
                continue;
            };
            if method == "textDocument/publishDiagnostics" {
                self.publish_diagnostics(&message).await;
                continue;
            }
            if let Some(id) = message.get("id").cloned() {
                let result = match method {
                    "workspace/configuration" => {
                        let count = message["params"]["items"].as_array().map_or(0, Vec::len);
                        Value::Array((0..count).map(|_| Value::Null).collect())
                    }
                    "workspace/applyEdit" => json!({
                        "applied": false,
                        "failureReason": "Haarness does not accept server-initiated workspace edits"
                    }),
                    _ => Value::Null,
                };
                let _ = self
                    .send(&json!({"jsonrpc":"2.0", "id":id, "result":result}))
                    .await;
            }
        }
        *self.status.write().await = "stopped".into();
        for (_, sender) in self.pending.lock().await.drain() {
            let _ = sender.send(Err("language server stopped".into()));
        }
    }

    async fn publish_diagnostics(&self, message: &Value) {
        let Some(uri) = message["params"]["uri"].as_str() else {
            return;
        };
        let file = url::Url::parse(uri)
            .ok()
            .and_then(|uri| uri.to_file_path().ok())
            .map_or_else(|| uri.to_owned(), |path| path.display().to_string());
        let diagnostics = message["params"]["diagnostics"]
            .as_array()
            .into_iter()
            .flatten()
            .take(MAX_DIAGNOSTICS)
            .filter_map(|item| parse_diagnostic(&self.id, &file, item))
            .collect();
        self.diagnostics
            .write()
            .await
            .insert(uri.to_owned(), diagnostics);
        let mut generations = self.diagnostic_generations.lock().await;
        let generation = generations.entry(uri.to_owned()).or_default();
        *generation = generation.saturating_add(1);
        drop(generations);
        self.diagnostics_changed.notify_waiters();
    }

    async fn shutdown(&self) {
        let cancel = CancellationToken::new();
        let _ = self.request("shutdown", Value::Null, 1_000, &cancel).await;
        let _ = self.notify("exit", Value::Null).await;
        let mut child = self.child.lock().await;
        if tokio::time::timeout(Duration::from_secs(1), child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
        }
        *self.status.write().await = "stopped".into();
    }

    async fn force_kill(&self) {
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
        *self.status.write().await = "error".into();
    }
}

async fn read_message<R>(reader: &mut BufReader<R>) -> Result<Option<Value>, String>
where
    R: AsyncRead + Unpin,
{
    let mut length = None;
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .await
            .map_err(|error| error.to_string())?;
        if read == 0 {
            return Ok(None);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some(value) = line
            .split_once(':')
            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .map(|(_, value)| value.trim())
        {
            length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| "invalid LSP Content-Length".to_owned())?,
            );
        }
    }
    let length = length.ok_or("missing LSP Content-Length")?;
    if length > MAX_MESSAGE_BYTES {
        return Err("LSP message exceeds 16 MiB".into());
    }
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn parse_diagnostic(server: &str, file: &str, item: &Value) -> Option<Diagnostic> {
    let position = |value: &Value| {
        Some(Position {
            line: u32::try_from(value["line"].as_u64()?).ok()?,
            character: u32::try_from(value["character"].as_u64()?).ok()?,
        })
    };
    Some(Diagnostic {
        server: server.to_owned(),
        file: file.to_owned(),
        range: Range {
            start: position(&item["range"]["start"])?,
            end: position(&item["range"]["end"])?,
        },
        severity: item["severity"]
            .as_u64()
            .and_then(|value| u8::try_from(value).ok()),
        code: item.get("code").and_then(|value| match value {
            Value::String(value) => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            _ => None,
        }),
        source: item["source"].as_str().map(str::to_owned),
        message: item["message"].as_str()?.to_owned(),
    })
}

fn file_uri(path: &Path) -> Result<String, String> {
    url::Url::from_file_path(path)
        .map(|url| url.to_string())
        .map_err(|()| format!("cannot convert {} to a file URI", path.display()))
}

fn executable_available(
    command: &[String],
    environment: &BTreeMap<String, String>,
    workspace: &Path,
) -> bool {
    let Some(program) = command.first() else {
        return false;
    };
    let program = program.replace("$FILE", "");
    let path = Path::new(&program);
    if path.components().count() > 1 {
        return if path.is_absolute() {
            path.is_file()
        } else {
            workspace.join(path).is_file()
        };
    }
    let search_path = environment
        .get("PATH")
        .map(std::ffi::OsString::from)
        .or_else(|| std::env::var_os("PATH"));
    search_path.is_some_and(|search_path| {
        std::env::split_paths(&search_path).any(|directory| directory.join(&program).is_file())
    })
}

fn normalized_extension(path: &Path) -> String {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map_or_else(String::new, |extension| format!(".{extension}"))
        .to_ascii_lowercase()
}

fn matches_file(extensions: &[String], filenames: &[String], file: &Path) -> bool {
    let extension = normalized_extension(file);
    let name = file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    filenames.iter().any(|candidate| candidate == name)
        || extensions.iter().any(|candidate| {
            let candidate = candidate.to_ascii_lowercase();
            candidate == extension || format!(".{candidate}") == extension
        })
}

fn language_id(config: &LspServerConfig, file: &Path) -> String {
    let extension = normalized_extension(file);
    let bare = extension.trim_start_matches('.');
    config
        .language_ids
        .get(&extension)
        .or_else(|| config.language_ids.get(bare))
        .cloned()
        .unwrap_or_else(|| match bare {
            "rs" => "rust".into(),
            "ts" => "typescript".into(),
            "tsx" => "typescriptreact".into(),
            "js" | "mjs" | "cjs" => "javascript".into(),
            "jsx" => "javascriptreact".into(),
            "py" | "pyi" => "python".into(),
            "go" => "go".into(),
            "c" | "h" => "c".into(),
            "cc" | "cpp" | "cxx" | "hpp" => "cpp".into(),
            "sh" | "bash" => "shellscript".into(),
            "yaml" | "yml" => "yaml".into(),
            "toml" => "toml".into(),
            _ => bare.to_owned(),
        })
}

fn server(command: &[&str], extensions: &[&str]) -> LspServerConfig {
    LspServerConfig {
        command: command.iter().map(|value| (*value).into()).collect(),
        extensions: extensions.iter().map(|value| (*value).into()).collect(),
        ..LspServerConfig::default()
    }
}

fn builtin_servers() -> BTreeMap<String, LspServerConfig> {
    BTreeMap::from([
        ("rust".into(), server(&["rust-analyzer"], &[".rs"])),
        (
            "typescript".into(),
            server(
                &["typescript-language-server", "--stdio"],
                &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"],
            ),
        ),
        (
            "pyright".into(),
            server(&["pyright-langserver", "--stdio"], &[".py", ".pyi"]),
        ),
        ("gopls".into(), server(&["gopls"], &[".go"])),
        (
            "clangd".into(),
            server(&["clangd"], &[".c", ".h", ".cc", ".cpp", ".cxx", ".hpp"]),
        ),
        (
            "bash".into(),
            server(&["bash-language-server", "start"], &[".sh", ".bash"]),
        ),
        (
            "yaml".into(),
            server(&["yaml-language-server", "--stdio"], &[".yaml", ".yml"]),
        ),
        (
            "taplo".into(),
            server(&["taplo", "lsp", "stdio"], &[".toml"]),
        ),
    ])
}

fn formatter(command: &[&str], extensions: &[&str]) -> FormatterDefinition {
    FormatterDefinition {
        command: command.iter().map(|value| (*value).into()).collect(),
        extensions: extensions.iter().map(|value| (*value).into()).collect(),
        ..FormatterDefinition::default()
    }
}

fn builtin_formatters() -> BTreeMap<String, FormatterDefinition> {
    BTreeMap::from([
        ("rustfmt".into(), formatter(&["rustfmt", "$FILE"], &[".rs"])),
        (
            "gofmt".into(),
            formatter(&["gofmt", "-w", "$FILE"], &[".go"]),
        ),
        (
            "prettier".into(),
            formatter(
                &["prettier", "--write", "$FILE"],
                &[
                    ".js", ".jsx", ".ts", ".tsx", ".json", ".css", ".scss", ".md", ".yaml", ".yml",
                ],
            ),
        ),
        (
            "ruff".into(),
            formatter(&["ruff", "format", "$FILE"], &[".py", ".pyi"]),
        ),
        (
            "clang-format".into(),
            formatter(
                &["clang-format", "-i", "$FILE"],
                &[".c", ".h", ".cc", ".cpp", ".cxx", ".hpp"],
            ),
        ),
        (
            "shfmt".into(),
            formatter(&["shfmt", "-w", "$FILE"], &[".sh", ".bash"]),
        ),
        ("zig".into(), formatter(&["zig", "fmt", "$FILE"], &[".zig"])),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_matching_accepts_dotted_and_bare_configuration() {
        let path = Path::new("src/main.rs");
        assert!(matches_file(&[".rs".into()], &[], path));
        assert!(matches_file(&["rs".into()], &[], path));
        assert!(!matches_file(&[".go".into()], &[], path));
    }

    #[tokio::test]
    async fn cancellation_does_not_wait_for_another_server_lifecycle() {
        let service = LanguageService::new(
            std::env::temp_dir(),
            LspConfig::default(),
            FormatterConfig::default(),
        );
        let state = service.state();
        let _held = state.lifecycle.lock().await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            service.query(
                Operation::Hover,
                Path::new("cancelled.rs"),
                &std::env::temp_dir(),
                0,
                0,
                None,
                &cancel,
            ),
        )
        .await
        .expect("cancellation must not wait for an unrelated startup or shutdown");
        assert!(result.unwrap_err().contains("cancelled"));
    }

    #[tokio::test]
    async fn a_server_that_is_not_installed_reads_as_missing_after_a_launch_attempt() {
        let workspace = std::env::temp_dir();
        let mut lsp = LspConfig {
            builtins: false,
            ..LspConfig::default()
        };
        lsp.servers.insert(
            "ghost".into(),
            LspServerConfig {
                command: vec!["haarness-test-no-such-langserver".into()],
                extensions: vec![".ghost".into()],
                ..LspServerConfig::default()
            },
        );
        let service = LanguageService::new(workspace.clone(), lsp, FormatterConfig::default());
        let error = service
            .query(
                Operation::Hover,
                &workspace.join("a.ghost"),
                &workspace,
                0,
                0,
                None,
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(
            error.contains("haarness-test-no-such-langserver is not installed"),
            "{error}"
        );
        let status = service.status().await;
        let ghost = status.iter().find(|server| server.id == "ghost").unwrap();
        assert_eq!(ghost.status, "missing");
        assert_eq!(ghost.error, None);
    }

    #[test]
    fn a_chat_folder_s_answers_keep_only_places_inside_it() {
        let root = std::env::temp_dir().join(format!("haarness-places-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let inside = url::Url::from_file_path(root.join("a.rs"))
            .unwrap()
            .to_string();
        let outside = "file:///elsewhere/b.rs";
        let mut value = json!([
            {"uri": inside, "range": {}},
            {"uri": outside, "range": {}},
            {"targetUri": outside},
            {"name": "symbol", "location": {"uri": outside}},
            {"from": {"uri": outside}, "fromRanges": []},
            {"to": {"uri": inside}, "fromRanges": []},
            {"name": "local", "kind": 12},
        ]);
        keep_places_inside(&mut value, &root);
        assert_eq!(
            value,
            json!([
                {"uri": inside, "range": {}},
                {"to": {"uri": inside}, "fromRanges": []},
                {"name": "local", "kind": 12},
            ])
        );
        let mut single = json!({"uri": outside, "range": {}});
        keep_places_inside(&mut single, &root);
        assert!(single.is_null());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn builtins_cover_the_initial_cross_platform_languages() {
        let servers = builtin_servers();
        assert!(servers.contains_key("rust"));
        assert!(servers.contains_key("typescript"));
        assert!(servers.contains_key("pyright"));
        assert!(servers.contains_key("gopls"));
        let formatters = builtin_formatters();
        assert!(formatters.contains_key("rustfmt"));
        assert!(formatters.contains_key("prettier"));
    }
}
