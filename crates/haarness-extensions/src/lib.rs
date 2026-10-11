//! Trusted TypeScript/JavaScript extensions in disposable Bun subprocesses.
//! Rust owns tool schemas, permission routing, cancellation and generation swaps.
use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use haarness_core::{
    CoreError, CoreResult, HookDecision, HookPort, PermissionDecision, ProviderRequest, Tool,
    ToolContext, ToolDefinition, ToolOutput, ToolRegistry,
};
use haarness_protocol::ThreadId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;

pub const TYPES: &str = include_str!("api.ts");
const RUNNER: &str = include_str!("runner.ts");

#[cfg(test)]
mod tests;

/// One registered extension. JSON accepts the object form written by
/// `extension install`, a bare source string, or a
/// `[source, options]` tuple; it always serializes as the object form.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(from = "EntryRepr")]
pub struct Entry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub source: PathBuf,
    #[serde(default)]
    pub options: Value,
    #[serde(default = "yes")]
    pub enabled: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryObject {
    #[serde(default)]
    name: Option<String>,
    source: PathBuf,
    #[serde(default)]
    options: Value,
    #[serde(default = "yes")]
    enabled: bool,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum EntryRepr {
    Source(PathBuf),
    Tuple(PathBuf, Value),
    Object(EntryObject),
}
impl From<EntryRepr> for Entry {
    fn from(value: EntryRepr) -> Self {
        match value {
            EntryRepr::Source(source) => Self {
                name: None,
                source,
                options: Value::Null,
                enabled: true,
            },
            EntryRepr::Tuple(source, options) => Self {
                name: None,
                source,
                options,
                enabled: true,
            },
            EntryRepr::Object(entry) => Self {
                name: entry.name,
                source: entry.source,
                options: entry.options,
                enabled: entry.enabled,
            },
        }
    }
}
const fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    #[serde(skip)]
    pub log_stderr: bool,
    /// Legacy fallback for all deadlines; zero disables the deadline.
    pub timeout_ms: u64,
    pub init_timeout_ms: Option<u64>,
    pub hook_timeout_ms: Option<u64>,
    pub tool_timeout_ms: Option<u64>,
    pub payload_max_bytes: usize,
    pub entries: Vec<Entry>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            log_stderr: false,
            timeout_ms: 30_000,
            init_timeout_ms: None,
            hook_timeout_ms: None,
            tool_timeout_ms: None,
            payload_max_bytes: 2 * 1024 * 1024,
            entries: Vec::new(),
        }
    }
}
impl Config {
    pub fn read(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).context("invalid extensions.json")?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }
}

/// Resolve only an explicitly installed file/package; never search workspace node_modules.
pub fn entrypoint(path: &Path) -> Result<PathBuf> {
    let path = path
        .canonicalize()
        .with_context(|| format!("extension not found: {}", path.display()))?;
    if path.is_file() {
        return Ok(path);
    }
    let manifest = path.join("package.json");
    if manifest.is_file() {
        let value: Value = serde_json::from_slice(&std::fs::read(manifest)?)?;
        for candidate in [
            value.pointer("/haarness/extension"),
            value.get("module"),
            value.get("main"),
            value.get("exports"),
        ] {
            if let Some(candidate) = candidate.and_then(Value::as_str) {
                let resolved = path.join(candidate).canonicalize()?;
                if !resolved.starts_with(&path) || !resolved.is_file() {
                    bail!("extension entry must be a file inside its package");
                }
                return Ok(resolved);
            }
        }
    }
    for name in ["index.ts", "index.mjs", "index.js"] {
        if path.join(name).is_file() {
            return entrypoint(&path.join(name));
        }
    }
    bail!("extension package needs haarness.extension in package.json or index.ts/index.js")
}

pub fn bun() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("HAARNESS_BUN") {
        return PathBuf::from(path)
            .canonicalize()
            .context("HAARNESS_BUN must name an existing Bun executable");
    }
    for root in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let path = root.join(if cfg!(windows) { "bun.exe" } else { "bun" });
        if path.is_file() {
            return Ok(path.canonicalize()?);
        }
    }
    bail!(
        "TypeScript extensions require Bun on PATH (or HAARNESS_BUN); ordinary Haarness use does not require Bun"
    )
}

struct Process {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    _scratch: tempfile::TempDir,
    payload_max_bytes: usize,
}
impl Drop for Process {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(id) = self.child.id().and_then(|id| i32::try_from(id).ok()) {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(id),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        let _ = self.child.start_kill();
    }
}
impl Process {
    fn spawn(workspace: &Path, log_stderr: bool, payload_max_bytes: usize) -> Result<Self> {
        let scratch = tempfile::tempdir()?;
        let runner = scratch.path().join("runner.ts");
        std::fs::write(&runner, RUNNER)?;
        let bunfig = scratch.path().join("bunfig.toml");
        let preload = scratch.path().join("preload.ts");
        std::fs::write(&preload, "export {}")?;
        std::fs::write(
            &bunfig,
            format!("preload = [{}]\n", serde_json::to_string(&preload)?),
        )?;
        let mut flag = std::ffi::OsString::from("--config=");
        flag.push(&bunfig);
        let mut command = Command::new(bun()?);
        for name in haarness_core::private_env_names() {
            command.env_remove(name);
        }
        command
            .arg(flag)
            .arg("--no-install")
            .arg("--no-env-file")
            .arg(&runner)
            .current_dir(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if log_stderr {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .kill_on_drop(true);
        // A project bunfig or NODE_OPTIONS must not add preloads behind the user's back.
        command.env_remove("NODE_OPTIONS").env_remove("BUN_OPTIONS");
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().context("start extension host")?;
        Ok(Self {
            input: child.stdin.take().context("extension stdin")?,
            output: BufReader::new(child.stdout.take().context("extension stdout")?),
            child,
            _scratch: scratch,
            payload_max_bytes,
        })
    }
    async fn exchange(&mut self, mut request: Value) -> Result<Value> {
        request["id"] = json!(1);
        let mut bytes = serde_json::to_vec(&request)?;
        if bytes.len() > self.payload_max_bytes {
            bail!("extension request exceeds {} bytes", self.payload_max_bytes);
        }
        bytes.push(b'\n');
        self.input.write_all(&bytes).await?;
        self.input.flush().await?;
        let mut response = Vec::new();
        (&mut self.output)
            .take(self.payload_max_bytes.saturating_add(1) as u64)
            .read_until(b'\n', &mut response)
            .await?;
        if response.len() > self.payload_max_bytes {
            bail!("extension response exceeds {} bytes", self.payload_max_bytes);
        }
        let value: Value = serde_json::from_slice(&response)
            .context("invalid extension response (do not write directly to stdout)")?;
        if value["id"] != 1 {
            bail!("extension protocol id mismatch");
        }
        if value.get("error").is_none() && value.get("result").is_none() {
            bail!("missing extension result");
        }
        Ok(value)
    }
}

fn unpack(value: Value) -> Result<Value> {
    if let Some(error) = value.get("error") {
        bail!("extension: {error}");
    }
    if let Some(warning) = value.get("warning").and_then(Value::as_str) {
        tracing::warn!(warning, "extension observer failed; the hook still ran");
    }
    value
        .get("result")
        .cloned()
        .context("missing extension result")
}

async fn deadline(milliseconds: u64) {
    if milliseconds == 0 { std::future::pending::<()>().await; }
    else { tokio::time::sleep(Duration::from_millis(milliseconds)).await; }
}

struct Host {
    process: Mutex<Option<Process>>,
    hook_timeout_ms: u64,
    tool_timeout_ms: u64,
    name: String,
    hooks: Vec<String>,
}
impl Host {
    async fn call(&self, request: Value, cancel: &CancellationToken) -> CoreResult<Value> {
        let timeout_ms = if request["method"] == "tool" { self.tool_timeout_ms } else { self.hook_timeout_ms };
        let operation = async {
            let mut slot = self.process.lock().await;
            // Take ownership: cancellation, timeout or a dropped caller kills the process.
            let mut process = slot.take().ok_or_else(|| {
                CoreError::Tool(format!(
                    "extension {} stopped; reload or restart the server",
                    self.name
                ))
            })?;
            let result = process
                .exchange(request)
                .await
                .map_err(|error| CoreError::Tool(error.to_string()));
            if result.is_ok() {
                *slot = Some(process);
            }
            result
                .and_then(|value| unpack(value).map_err(|error| CoreError::Tool(error.to_string())))
        };
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(CoreError::Cancelled),
            () = deadline(timeout_ms) => Err(CoreError::Tool(format!("extension {} timed out after {timeout_ms} ms", self.name))),
            result = operation => result,
        }
    }
    fn registered(&self, name: &str) -> bool {
        self.hooks.iter().any(|hook| hook == name)
    }
    async fn hook(&self, name: &str, input: Value) -> CoreResult<Value> {
        if !self.registered(name) && !self.registered("event") {
            return Ok(json!({}));
        }
        self.call(
            json!({"method":"hook", "name":name, "input":input}),
            &CancellationToken::new(),
        )
        .await
    }
}

struct ExtensionTool {
    host: Arc<Host>,
    definition: ToolDefinition,
    local: String,
}
#[async_trait]
impl Tool for ExtensionTool {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }
    fn side_effectful(&self) -> bool {
        true
    }
    async fn execute(&self, arguments: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        let validator = jsonschema::validator_for(&self.definition.input_schema)
            .map_err(|error| CoreError::Invalid(error.to_string()))?;
        if !validator.is_valid(&arguments) {
            return Err(CoreError::Invalid(format!(
                "arguments do not match {} schema",
                self.definition.name
            )));
        }
        let result = self.host.call(json!({"method":"tool", "name":self.local, "arguments":arguments, "context":{"threadId":context.thread_id,"turnId":context.turn_id}}), &context.cancel).await?;
        serde_json::from_value(result)
            .map_err(|error| CoreError::Tool(format!("invalid extension tool result: {error}")))
    }
}

pub struct Extensions {
    hosts: Vec<Arc<Host>>,
    tools: BTreeMap<String, Arc<dyn Tool>>,
    workspace: PathBuf,
}
impl Extensions {
    pub async fn load(config: &Config, workspace: &Path) -> Result<Arc<Self>> {
        let mut result = Self {
            hosts: Vec::new(),
            tools: BTreeMap::new(),
            workspace: workspace.to_path_buf(),
        };
        if !config.enabled {
            return Ok(Arc::new(result));
        }
        if config.payload_max_bytes == 0 || config.payload_max_bytes == usize::MAX {
            bail!("extension payload_max_bytes must be positive and smaller than usize::MAX");
        }
        for entry in config.entries.iter().filter(|entry| entry.enabled) {
            if !entry.source.is_absolute() {
                bail!("extension source must be an absolute path");
            }
            let source = entrypoint(&entry.source)?;
            let mut process = Process::spawn(workspace, config.log_stderr, config.payload_max_bytes)?;
            let manifest = tokio::select! {
                () = deadline(config.init_timeout_ms.unwrap_or(config.timeout_ms)) => bail!("extension initialization timed out"),
                result = process.exchange(json!({"method":"load","source":source,"context":{"workspace":workspace,"extension":source,"options":entry.options}})) => result?,
            };
            let manifest = unpack(manifest)?;
            let name = manifest["name"]
                .as_str()
                .context("extension needs a name")?
                .to_owned();
            validate_name(&name)?;
            if result.hosts.iter().any(|host| host.name == name) {
                bail!(
                    "duplicate extension name: {name}; another installed extension already uses it, so change `name` in {}",
                    source.display()
                );
            }
            let host = Arc::new(Host {
                process: Mutex::new(Some(process)),
                hook_timeout_ms: config.hook_timeout_ms.unwrap_or(config.timeout_ms),
                tool_timeout_ms: config.tool_timeout_ms.unwrap_or(config.timeout_ms),
                name: name.clone(),
                hooks: serde_json::from_value(manifest["hooks"].clone())?,
            });
            let definitions: Vec<ToolDefinition> =
                serde_json::from_value(manifest["tools"].clone())?;
            for mut definition in definitions {
                validate_name(&definition.name)?;
                if definition.description.is_empty()
                    || definition.input_schema.get("type") != Some(&json!("object"))
                {
                    bail!("extension tool needs a description and object JSON Schema");
                }
                jsonschema::validator_for(&definition.input_schema)
                    .context("invalid extension tool schema")?;
                let local = definition.name.clone();
                definition.name = format!("ext__{name}__{local}");
                result.tools.insert(
                    definition.name.clone(),
                    Arc::new(ExtensionTool {
                        host: host.clone(),
                        definition,
                        local,
                    }),
                );
            }
            result.hosts.push(host);
        }
        Ok(Arc::new(result))
    }
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.hosts.iter().map(|host| host.name.clone()).collect()
    }
    /// Combine extension tools with the native registry and apply
    /// `tool.definition` rewrites to every tool known at composition time.
    /// Tools that appear later (lazy MCP servers) keep their own definitions.
    pub async fn wrap_tools(
        self: &Arc<Self>,
        inner: Arc<dyn ToolRegistry>,
    ) -> Result<Arc<dyn ToolRegistry>> {
        for name in self.tools.keys() {
            if inner.get(name).is_some() {
                bail!("extension tool collision: {name}");
            }
        }
        let definers: Vec<&Arc<Host>> = self
            .hosts
            .iter()
            .filter(|host| host.registered("tool.definition"))
            .collect();
        let mut overrides = BTreeMap::new();
        if !definers.is_empty() {
            let mut definitions = inner.list();
            definitions.extend(self.tools.values().map(|tool| tool.definition()));
            for definition in definitions {
                let mut current = definition.clone();
                for host in &definers {
                    let value = host
                        .hook(
                            "tool.definition",
                            json!({"toolID": current.name, "description": current.description, "parameters": current.input_schema}),
                        )
                        .await
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    if let Some(description) = value["description"].as_str() {
                        description.clone_into(&mut current.description);
                    }
                    if let Some(parameters) = value.get("parameters").filter(|value| value.is_object()) {
                        current.input_schema = parameters.clone();
                    }
                }
                if current.description == definition.description
                    && current.input_schema == definition.input_schema
                {
                    continue;
                }
                if current.description.is_empty()
                    || current.input_schema.get("type") != Some(&json!("object"))
                {
                    bail!(
                        "tool.definition must keep a description and an object JSON Schema for {}",
                        definition.name
                    );
                }
                jsonschema::validator_for(&current.input_schema)
                    .with_context(|| format!("invalid tool.definition schema for {}", definition.name))?;
                overrides.insert(definition.name.clone(), current);
            }
        }
        if self.tools.is_empty() && overrides.is_empty() {
            return Ok(inner);
        }
        Ok(Arc::new(CombinedTools {
            inner,
            extensions: self.clone(),
            overrides,
        }))
    }
    /// Deliver the effective configuration summary to `config` hooks. Called
    /// once per loaded generation by the composition root.
    pub async fn notify_config(&self, config: &Value) {
        for host in &self.hosts {
            if !host.registered("config") && !host.registered("event") {
                continue;
            }
            let mut input = config.clone();
            if let Some(object) = input.as_object_mut() {
                object
                    .entry("workspace")
                    .or_insert_with(|| json!(self.workspace));
            }
            if let Err(error) = host.hook("config", input).await {
                tracing::warn!(extension = host.name, %error, "extension config hook failed");
            }
        }
    }
    #[must_use]
    pub fn wrap_hooks(self: &Arc<Self>, inner: Option<Arc<dyn HookPort>>) -> Arc<dyn HookPort> {
        Arc::new(CombinedHooks {
            inner,
            extensions: self.clone(),
        })
    }
}
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains("__")
        || name.len() > 24
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        bail!(
            "extension/tool names need 1-24 lowercase letters, digits or single underscores (double underscores are reserved)"
        );
    }
    Ok(())
}
struct CombinedTools {
    inner: Arc<dyn ToolRegistry>,
    extensions: Arc<Extensions>,
    /// Model-facing definitions rewritten by `tool.definition` hooks.
    overrides: BTreeMap<String, ToolDefinition>,
}
impl ToolRegistry for CombinedTools {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        let tool = self
            .extensions
            .tools
            .get(name)
            .cloned()
            .or_else(|| self.inner.get(name))?;
        Some(match self.overrides.get(name) {
            Some(definition) => Arc::new(RedefinedTool {
                inner: tool,
                definition: definition.clone(),
            }),
            None => tool,
        })
    }
    fn list(&self) -> Vec<ToolDefinition> {
        let mut tools = self.inner.list();
        tools.extend(self.extensions.tools.values().map(|tool| tool.definition()));
        for tool in &mut tools {
            if let Some(definition) = self.overrides.get(&tool.name) {
                tool.description.clone_from(&definition.description);
                tool.input_schema.clone_from(&definition.input_schema);
            }
        }
        tools
    }
}
/// A tool whose advertised definition was rewritten by an extension. Every
/// execution concern, including argument validation, stays with the tool.
struct RedefinedTool {
    inner: Arc<dyn Tool>,
    definition: ToolDefinition,
}
#[async_trait]
impl Tool for RedefinedTool {
    fn parallel_safe(&self, arguments: &Value) -> bool {
        self.inner.parallel_safe(arguments)
    }
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }
    fn validation_schema(&self) -> Value {
        self.inner.validation_schema()
    }
    fn validate_arguments(&self, arguments: &Value) -> CoreResult<()> {
        self.inner.validate_arguments(arguments)
    }
    fn side_effectful(&self) -> bool {
        self.inner.side_effectful()
    }
    fn side_effectful_for(&self, arguments: &Value) -> bool {
        self.inner.side_effectful_for(arguments)
    }
    async fn prepare_arguments(
        &self,
        arguments: Value,
        thread: ThreadId,
        cancel: CancellationToken,
    ) -> CoreResult<Value> {
        self.inner.prepare_arguments(arguments, thread, cancel).await
    }
    fn permission_constraint(&self, arguments: &Value, operation: &str) -> Option<PermissionDecision> {
        self.inner.permission_constraint(arguments, operation)
    }
    async fn execute(&self, arguments: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        self.inner.execute(arguments, context).await
    }
}
struct CombinedHooks {
    inner: Option<Arc<dyn HookPort>>,
    extensions: Arc<Extensions>,
}
impl CombinedHooks {
    async fn decision(
        &self,
        name: &str,
        mut input: Value,
        mut decision: HookDecision,
    ) -> HookDecision {
        if decision.block.is_some() || decision.permission == Some(PermissionDecision::Deny) {
            return decision;
        }
        for host in &self.extensions.hosts {
            if let Some(arguments) = &decision.updated_input {
                input["arguments"] = arguments.clone();
            }
            match host.hook(name, input.clone()).await {
                Ok(value) => {
                    if let Some(block) = value["block"].as_str() {
                        decision.block = Some(block.into());
                        decision.permission = Some(PermissionDecision::Deny);
                    }
                    if let Some(context) = value["context"].as_str() {
                        append(&mut decision.context, context);
                    }
                    if let Some(arguments) = value.get("arguments") {
                        decision.updated_input = Some(arguments.clone());
                    }
                }
                Err(error) => {
                    decision.block = Some(error.to_string());
                    decision.permission = Some(PermissionDecision::Deny);
                }
            }
            if decision.block.is_some() {
                break;
            }
        }
        decision
    }
}
fn append(target: &mut Option<String>, text: &str) {
    if let Some(target) = target {
        target.push('\n');
        target.push_str(text);
    } else {
        *target = Some(text.into());
    }
}
#[async_trait]
impl HookPort for CombinedHooks {
    async fn pre_tool_use(&self, thread: ThreadId, tool: &str, arguments: &Value) -> HookDecision {
        let prior = if let Some(inner) = &self.inner {
            inner.pre_tool_use(thread, tool, arguments).await
        } else {
            HookDecision::default()
        };
        self.decision(
            "tool.before",
            json!({"threadId":thread,"tool":tool,"arguments":arguments}),
            prior,
        )
        .await
    }
    async fn post_tool_use(
        &self,
        thread: ThreadId,
        tool: &str,
        arguments: &Value,
        output: &str,
        is_error: bool,
    ) -> Option<String> {
        let prior = if let Some(inner) = &self.inner {
            inner
                .post_tool_use(thread, tool, arguments, output, is_error)
                .await
        } else {
            None
        };
        let result = self.decision("tool.after", json!({"threadId":thread,"tool":tool,"arguments":arguments,"output":output,"isError":is_error}), HookDecision { context: prior, ..HookDecision::default() }).await;
        let mut context = result.context;
        if let Some(error) = result.block {
            append(&mut context, &error);
        }
        context
    }
    async fn user_prompt_submit(&self, thread: ThreadId, prompt: &str) -> HookDecision {
        let prior = if let Some(inner) = &self.inner {
            inner.user_prompt_submit(thread, prompt).await
        } else {
            HookDecision::default()
        };
        self.decision("prompt", json!({"threadId":thread,"prompt":prompt}), prior)
            .await
    }
    async fn permission_request(
        &self,
        thread: ThreadId,
        permission: &str,
        detail: &Value,
    ) -> HookDecision {
        // Extensions answer first; command hooks only see
        // prompts that extensions leave open.
        for host in &self.extensions.hosts {
            let input = json!({"threadId":thread,"permission":permission,"detail":detail});
            match host.hook("permission.ask", input).await {
                Ok(value) => match value["status"].as_str() {
                    Some("allow") => {
                        return HookDecision {
                            permission: Some(PermissionDecision::Allow),
                            ..HookDecision::default()
                        };
                    }
                    Some("deny") => {
                        return HookDecision {
                            block: Some(
                                value["reason"]
                                    .as_str()
                                    .unwrap_or("denied by an extension permission.ask hook")
                                    .to_owned(),
                            ),
                            permission: Some(PermissionDecision::Deny),
                            ..HookDecision::default()
                        };
                    }
                    _ => {}
                },
                Err(error) => {
                    return HookDecision {
                        block: Some(error.to_string()),
                        permission: Some(PermissionDecision::Deny),
                        ..HookDecision::default()
                    };
                }
            }
        }
        if let Some(inner) = &self.inner {
            inner.permission_request(thread, permission, detail).await
        } else {
            HookDecision::default()
        }
    }
    async fn stop(&self, thread: ThreadId) {
        if let Some(inner) = &self.inner {
            inner.stop(thread).await;
        }
        for host in &self.extensions.hosts {
            if let Err(error) = host.hook("stop", json!({"threadId":thread})).await {
                tracing::warn!(%error, "extension stop hook failed");
            }
        }
    }
    async fn prepare_request(
        &self,
        thread: ThreadId,
        request: &mut ProviderRequest,
    ) -> CoreResult<()> {
        if let Some(inner) = &self.inner {
            inner.prepare_request(thread, request).await?;
        }
        for host in &self.extensions.hosts {
            let value = host.hook("chat", json!({"threadId":thread,"model":request.model,"system":request.system,"reasoningEffort":request.reasoning_effort})).await?;
            if let Some(context) = value["context"].as_str() {
                request.system.push('\n');
                request.system.push_str(context);
            }
            if let Some(effort) = value["reasoningEffort"].as_str() {
                request.reasoning_effort = Some(effort.into());
            }
        }
        for host in &self.extensions.hosts {
            let parameters = &request.parameters;
            let value = host.hook("chat.params", json!({
                "threadId":thread,"model":request.model,
                "temperature":parameters.temperature,"topP":parameters.top_p,"topK":parameters.top_k,
                "maxOutputTokens":parameters.max_output_tokens,"reasoningEffort":request.reasoning_effort,
                "options":parameters.options,
            })).await?;
            let parameters = &mut request.parameters;
            // An explicit null clears a value; an absent key keeps it.
            if let Some(temperature) = value.get("temperature") {
                parameters.temperature = temperature.as_f64();
            }
            if let Some(top_p) = value.get("topP") {
                parameters.top_p = top_p.as_f64();
            }
            if let Some(top_k) = value.get("topK") {
                parameters.top_k = top_k.as_u64().and_then(|value| u32::try_from(value).ok());
            }
            if let Some(limit) = value.get("maxOutputTokens") {
                parameters.max_output_tokens = limit.as_u64();
            }
            if let Some(effort) = value["reasoningEffort"].as_str() {
                request.reasoning_effort = Some(effort.into());
            }
            if let Some(options) = value["options"].as_object() {
                for (key, value) in options {
                    request.parameters.options.insert(key.clone(), value.clone());
                }
            }
        }
        for host in &self.extensions.hosts {
            let value = host.hook("chat.headers", json!({"threadId":thread,"model":request.model,"headers":request.parameters.headers})).await?;
            if let Some(headers) = value["headers"].as_object() {
                for (name, value) in headers {
                    if let Some(value) = value.as_str() {
                        request.parameters.headers.insert(name.clone(), value.to_owned());
                    }
                }
            }
        }
        Ok(())
    }
    async fn session_start(&self, thread: ThreadId, agent: &str) {
        if let Some(inner) = &self.inner {
            inner.session_start(thread, agent).await;
        }
        self.observe("session.start", json!({"threadId":thread,"agent":agent,"directory":self.extensions.workspace})).await;
    }
    async fn session_end(&self, thread: ThreadId, reason: &str) {
        if let Some(inner) = &self.inner {
            inner.session_end(thread, reason).await;
        }
        self.observe("session.end", json!({"threadId":thread,"reason":reason,"directory":self.extensions.workspace})).await;
    }
    async fn post_compact(&self, thread: ThreadId, trigger: &str, item: &haarness_protocol::Item) {
        if let Some(inner) = &self.inner {
            inner.post_compact(thread, trigger, item).await;
        }
        self.observe(
            "session.compacted",
            json!({"threadId":thread,"trigger":trigger,"compaction":item}),
        )
        .await;
    }
    async fn stop_failure(
        &self,
        thread: ThreadId,
        turn: haarness_protocol::TurnId,
        detail: &Value,
    ) {
        if let Some(inner) = &self.inner {
            inner.stop_failure(thread, turn, detail).await;
        }
        self.observe(
            "chat.turn.failed",
            json!({"threadId":thread,"turnId":turn,"detail":detail}),
        )
        .await;
    }
    async fn turn_end(&self, thread: ThreadId, turn: haarness_protocol::TurnId, detail: &Value) {
        if let Some(inner) = &self.inner {
            inner.turn_end(thread, turn, detail).await;
        }
        let mut input = json!({"threadId":thread,"turnId":turn});
        if let (Some(target), Some(extra)) = (input.as_object_mut(), detail.as_object()) {
            for (key, value) in extra {
                target.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        self.observe("chat.turn.end", input).await;
    }
    async fn notification(&self, thread: ThreadId, kind: &str, message: &str) {
        if let Some(inner) = &self.inner {
            inner.notification(thread, kind, message).await;
        }
    }
    async fn config_change(&self, detail: &Value) {
        if let Some(inner) = &self.inner {
            inner.config_change(detail).await;
        }
    }
    async fn command_execute_before(
        &self,
        thread: ThreadId,
        command: &str,
        arguments: &str,
    ) -> HookDecision {
        let prior = if let Some(inner) = &self.inner {
            inner.command_execute_before(thread, command, arguments).await
        } else {
            HookDecision::default()
        };
        self.decision(
            "command.execute.before",
            json!({"threadId":thread,"command":command,"arguments":arguments}),
            prior,
        )
        .await
    }
    async fn shell_env(&self, thread: ThreadId, cwd: &Path) -> BTreeMap<String, String> {
        let mut env = if let Some(inner) = &self.inner {
            inner.shell_env(thread, cwd).await
        } else {
            BTreeMap::new()
        };
        for host in &self.extensions.hosts {
            match host.hook("shell.env", json!({"threadId":thread,"cwd":cwd})).await {
                Ok(value) => {
                    if let Some(map) = value["env"].as_object() {
                        for (key, value) in map {
                            if let Some(value) = value.as_str() {
                                env.insert(key.clone(), value.to_owned());
                            }
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(extension = host.name, %error, "extension shell.env hook failed");
                }
            }
        }
        env
    }
}
impl CombinedHooks {
    /// Deliver an observation-only lifecycle event to every extension.
    async fn observe(&self, name: &str, input: Value) {
        for host in &self.extensions.hosts {
            if let Err(error) = host.hook(name, input.clone()).await {
                tracing::warn!(extension = host.name, hook = name, %error, "extension hook failed");
            }
        }
    }
}
