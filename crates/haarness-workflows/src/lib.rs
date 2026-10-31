//! Native TypeScript/JavaScript workflow execution.
//!
//! Native workflows are arbitrary code. Process isolation, a cleared
//! environment, bounded IPC, and process-group cleanup reduce accidental
//! coupling, but they are not a security boundary. Embedding applications
//! must authorize native workflow sources before passing them to the executor.

mod coordinator;
pub use coordinator::{
    WorkflowAgentDefaults, WorkflowCoordinator, WorkflowModelResolution, WorkflowRuntimeHost,
    use_cache_folder,
};

use std::{
    collections::HashMap,
    ffi::OsString,
    fmt,
    future::pending,
    io,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use thiserror::Error;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, Command},
    sync::mpsc,
    task::JoinHandle,
    time::{Instant, sleep_until, timeout},
};
use tokio_util::sync::CancellationToken;

const RUNNER_SOURCE: &str = include_str!("runner.js");
const PROTOCOL_VERSION: u8 = 1;

/// Resource and process-lifecycle configuration for WorkflowExecutor.
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    pub bun_path: Option<PathBuf>,
    pub startup_timeout: Duration,
    pub execution_timeout: Option<Duration>,
    /// Synchronous VM evaluation slice for portable source. This catches CPU
    /// loops without constraining awaited host calls.
    pub portable_watchdog_timeout: Duration,
    /// Maximum transport-level silence from the runner process. Keep this
    /// separate from the VM slice: stdio scheduling and large agent responses
    /// can legitimately take longer than a JavaScript evaluation slice.
    pub runner_heartbeat_timeout: Duration,
    pub shutdown_grace: Duration,
    pub max_source_bytes: usize,
    pub max_frame_bytes: usize,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
    pub max_pending_requests: usize,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            bun_path: None,
            startup_timeout: Duration::from_secs(5),
            execution_timeout: Some(Duration::from_secs(30 * 60)),
            portable_watchdog_timeout: Duration::from_secs(1),
            runner_heartbeat_timeout: Duration::from_secs(5),
            shutdown_grace: Duration::from_millis(500),
            max_source_bytes: 2 * 1024 * 1024,
            max_frame_bytes: 4 * 1024 * 1024,
            max_stdout_bytes: 16 * 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
            max_pending_requests: 128,
        }
    }
}

/// Current availability for arbitrary native modules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeCapability {
    pub bun_path: Option<PathBuf>,
    pub executable: bool,
    pub blocker: Option<CapabilityBlocker>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityBlocker {
    BunUnavailable,
}

/// One native workflow invocation.
#[derive(Debug, Clone)]
pub struct WorkflowInvocation {
    pub run_id: String,
    pub dialect: WorkflowDialect,
    /// Durable source snapshot. Path-backed execution must match it byte-for-byte.
    pub source: String,
    /// Optional trusted installed file. Native execution canonicalizes and
    /// snapshot-checks it; portable execution uses it only as a diagnostic
    /// filename and always evaluates `source`.
    pub source_path: Option<PathBuf>,
    /// Optional lowercase or uppercase hexadecimal SHA-256 of the snapshot.
    pub expected_source_sha256: Option<String>,
    /// None preserves JavaScript undefined; Some(Value::Null) is null.
    pub args: Option<Value>,
    /// Overrides the executor's hard wall-clock timeout for this run.
    pub timeout: Option<Duration>,
}

impl WorkflowInvocation {
    #[must_use]
    pub fn new(run_id: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            dialect: WorkflowDialect::Native,
            source: source.into(),
            source_path: None,
            expected_source_sha256: None,
            args: None,
            timeout: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkflowDialect {
    #[default]
    Native,
    ClaudePortable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostNoticeMethod {
    Phase,
    Status,
    Log,
}

/// Ordered progress emitted by the child runtime.
#[derive(Debug, Clone, PartialEq)]
pub struct HostNotice {
    pub run_id: String,
    pub sequence: u64,
    pub method: HostNoticeMethod,
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostRequestMethod {
    #[serde(rename = "agent")]
    Agent,
    #[serde(rename = "agent.followup")]
    AgentFollowup,
    #[serde(rename = "agent.close")]
    AgentClose,
    #[serde(rename = "tool")]
    Tool,
    #[serde(rename = "shell")]
    Shell,
    #[serde(rename = "workflow")]
    Workflow,
}

/// A permission-checked operation requested by workflow code.
#[derive(Debug, Clone)]
pub struct HostCall {
    pub run_id: String,
    pub request_id: u64,
    pub method: HostRequestMethod,
    pub params: Value,
    /// Cancelled when AbortSignal fires or the workflow ends.
    pub cancellation: CancellationToken,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct HostError {
    message: String,
}

impl HostError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Host bridge implemented by the server/core integration layer.
///
/// Implementations must retain normal permission and ceiling checks. The
/// executor never performs shells, tools, agents, or nested workflows itself.
#[async_trait]
pub trait WorkflowHost: Send + Sync + 'static {
    async fn notice(&self, notice: HostNotice) -> Result<(), HostError>;
    async fn request(&self, call: HostCall) -> Result<Option<Value>, HostError>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionOutput {
    pub value: Option<Value>,
    pub source_sha256: String,
    pub stderr: String,
    pub stderr_truncated: bool,
}

#[derive(Debug, Error)]
pub enum ExecutionErrorKind {
    #[error("Bun is unavailable; install Bun or configure an explicit executable path")]
    BunUnavailable,
    #[error("invalid executor configuration: {0}")]
    InvalidConfiguration(String),
    #[error("invalid workflow invocation: {0}")]
    InvalidInvocation(String),
    #[error("failed to prepare workflow scratch directory: {0}")]
    Scratch(io::Error),
    #[error("failed to spawn workflow runtime: {0}")]
    Spawn(io::Error),
    #[error("workflow runtime did not become ready before the startup timeout")]
    StartupTimeout,
    #[error("workflow exceeded its execution timeout after {0}ms")]
    ExecutionTimeout(u64),
    #[error("workflow was cancelled")]
    Cancelled,
    #[error("workflow protocol error: {0}")]
    Protocol(String),
    #[error("workflow runtime error: {0}")]
    Runner(String),
    #[error("workflow host bridge failed: {0}")]
    Host(String),
    #[error("workflow runtime exited unsuccessfully: {0}")]
    ChildExit(String),
}

/// Execution failure with bounded child diagnostics.
#[derive(Debug)]
pub struct ExecutionError {
    pub kind: ExecutionErrorKind,
    pub stderr: String,
    pub stderr_truncated: bool,
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(formatter)?;
        if !self.stderr.is_empty() {
            write!(formatter, "; child stderr: {}", self.stderr.trim_end())?;
            if self.stderr_truncated {
                formatter.write_str(" [truncated]")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for ExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.kind)
    }
}

#[derive(Debug, Clone)]
pub struct WorkflowExecutor {
    config: ExecutorConfig,
}

impl WorkflowExecutor {
    #[must_use]
    pub fn new(config: ExecutorConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub fn capability(&self) -> NativeCapability {
        let bun_path = self.resolve_bun();
        let blocker = if bun_path.is_none() {
            Some(CapabilityBlocker::BunUnavailable)
        } else {
            None
        };
        NativeCapability {
            bun_path,
            executable: blocker.is_none(),
            blocker,
        }
    }

    pub async fn execute(
        &self,
        invocation: WorkflowInvocation,
        host: Arc<dyn WorkflowHost>,
        cancellation: CancellationToken,
    ) -> Result<ExecutionOutput, ExecutionError> {
        if let Err(kind) = self.validate(&invocation) {
            return Err(bare_error(kind));
        }
        let bun_path = self.resolve_bun();
        let Some(bun_path) = bun_path else {
            return Err(bare_error(ExecutionErrorKind::BunUnavailable));
        };
        let prepared = prepare_source(invocation).await.map_err(bare_error)?;
        let scratch = TempDir::new()
            .map_err(ExecutionErrorKind::Scratch)
            .map_err(bare_error)?;
        let mut command = Self::command(&bun_path, &scratch);
        let mut child = command
            .spawn()
            .map_err(ExecutionErrorKind::Spawn)
            .map_err(bare_error)?;
        // Stopped by the next server should this one die before it can.
        let _recorded = haarness_core::process_groups::recorded(child.id());
        let stdout = child.stdout.take().ok_or_else(|| {
            bare_error(ExecutionErrorKind::Protocol(
                "workflow child stdout was not piped".to_owned(),
            ))
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            bare_error(ExecutionErrorKind::Protocol(
                "workflow child stdin was not piped".to_owned(),
            ))
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            bare_error(ExecutionErrorKind::Protocol(
                "workflow child stderr was not piped".to_owned(),
            ))
        })?;
        let stderr_task = drain_stderr(stderr, self.config.max_stderr_bytes);

        let source_sha256 = prepared.sha256.clone();
        let drive_result = self
            .drive(stdout, stdin, prepared, host, cancellation)
            .await;
        let status_result = settle_child(
            &mut child,
            drive_result.is_err(),
            self.config.shutdown_grace,
        )
        .await;
        let captured = stderr_task.await.unwrap_or_else(|error| CapturedStderr {
            bytes: format!("failed to join stderr reader: {error}").into_bytes(),
            truncated: false,
        });
        let stderr = String::from_utf8_lossy(&captured.bytes).into_owned();

        let value = match drive_result {
            Ok(value) => {
                if let Err(error) = status_result {
                    return Err(ExecutionError {
                        kind: error,
                        stderr,
                        stderr_truncated: captured.truncated,
                    });
                }
                value
            }
            Err(kind) => {
                return Err(ExecutionError {
                    kind,
                    stderr,
                    stderr_truncated: captured.truncated,
                });
            }
        };
        Ok(ExecutionOutput {
            value,
            source_sha256,
            stderr,
            stderr_truncated: captured.truncated,
        })
    }

    fn validate(&self, invocation: &WorkflowInvocation) -> Result<(), ExecutionErrorKind> {
        if invocation.run_id.trim().is_empty() {
            return Err(ExecutionErrorKind::InvalidInvocation(
                "run_id must not be empty".to_owned(),
            ));
        }
        if invocation.source.len() > self.config.max_source_bytes {
            return Err(ExecutionErrorKind::InvalidInvocation(format!(
                "source exceeds {} bytes",
                self.config.max_source_bytes
            )));
        }
        if self.config.max_frame_bytes < 1024 {
            return Err(ExecutionErrorKind::InvalidConfiguration(
                "max_frame_bytes must be at least 1024".to_owned(),
            ));
        }
        if self.config.max_stdout_bytes < self.config.max_frame_bytes {
            return Err(ExecutionErrorKind::InvalidConfiguration(
                "max_stdout_bytes must be at least max_frame_bytes".to_owned(),
            ));
        }
        if self.config.max_pending_requests == 0 {
            return Err(ExecutionErrorKind::InvalidConfiguration(
                "max_pending_requests must be positive".to_owned(),
            ));
        }
        if self.config.portable_watchdog_timeout.is_zero() {
            return Err(ExecutionErrorKind::InvalidConfiguration(
                "portable_watchdog_timeout must be positive".to_owned(),
            ));
        }
        if self.config.runner_heartbeat_timeout.is_zero() {
            return Err(ExecutionErrorKind::InvalidConfiguration(
                "runner_heartbeat_timeout must be positive".to_owned(),
            ));
        }
        Ok(())
    }

    fn command(bun_path: &Path, scratch: &TempDir) -> Command {
        let mut command = Command::new(bun_path);
        command.args(["--eval", RUNNER_SOURCE]);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .current_dir(scratch.path())
            .env_clear()
            .env("HOME", scratch.path())
            .env("TMPDIR", scratch.path())
            .env("TEMP", scratch.path())
            .env("TMP", scratch.path())
            .env("LANG", "C.UTF-8")
            .env("LC_ALL", "C.UTF-8")
            .env("NO_COLOR", "1")
            .env("PATH", sanitized_path(bun_path));
        #[cfg(unix)]
        {
            command.process_group(0);
        }
        // Windows programs need to know where Windows is: without
        // `SystemRoot` Bun stops before it runs anything.
        #[cfg(windows)]
        for name in ["SystemRoot", "SystemDrive", "windir"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
    }

    fn resolve_bun(&self) -> Option<PathBuf> {
        self.config
            .bun_path
            .as_ref()
            .filter(|path| executable(path))
            .cloned()
            .or_else(find_bun)
    }

    async fn drive(
        &self,
        stdout: tokio::process::ChildStdout,
        mut stdin: tokio::process::ChildStdin,
        prepared: PreparedInvocation,
        host: Arc<dyn WorkflowHost>,
        cancellation: CancellationToken,
    ) -> Result<Option<Value>, ExecutionErrorKind> {
        let mut reader = BufReader::new(stdout);
        let mut output_bytes = 0usize;
        let ready = timeout(
            self.config.startup_timeout,
            read_child_frame(
                &mut reader,
                self.config.max_frame_bytes,
                &mut output_bytes,
                self.config.max_stdout_bytes,
            ),
        )
        .await
        .map_err(|_| ExecutionErrorKind::StartupTimeout)??;
        match ready {
            ChildFrame::Ready { .. } => {}
            frame => {
                return Err(ExecutionErrorKind::Protocol(format!(
                    "expected ready frame, received {}",
                    frame.name()
                )));
            }
        }

        let start = ParentFrame::Start {
            version: PROTOCOL_VERSION,
            dialect: prepared.dialect,
            run_id: prepared.run_id.clone(),
            source: prepared.source,
            source_path: prepared.source_path,
            args: RunnerValue::from_option(prepared.args)?,
            portable_timeout_ms: u64::try_from(self.config.portable_watchdog_timeout.as_millis())
                .unwrap_or(u64::MAX),
        };
        write_parent_frame(&mut stdin, &start, self.config.max_frame_bytes).await?;

        let execution_timeout = prepared.timeout.or(self.config.execution_timeout);
        let timeout_ms = execution_timeout.map(duration_millis);
        let deadline = execution_timeout.map(|duration| Instant::now() + duration);
        let mut portable_watchdog = (prepared.dialect == WorkflowDialect::ClaudePortable)
            .then(|| Instant::now() + self.config.runner_heartbeat_timeout);
        let (response_tx, mut response_rx) =
            mpsc::channel::<HostTaskResponse>(self.config.max_pending_requests);
        let mut outstanding = HashMap::<u64, CancellationToken>::new();
        let mut notice_sequence = 0u64;

        let result = loop {
            tokio::select! {
                () = cancellation.cancelled() => break Err(ExecutionErrorKind::Cancelled),
                () = wait_deadline(deadline) => break Err(ExecutionErrorKind::ExecutionTimeout(timeout_ms.unwrap_or(0))),
                () = wait_deadline(portable_watchdog) => {
                    break Err(ExecutionErrorKind::Runner(format!(
                        "portable workflow stopped responding for {}ms",
                        self.config.runner_heartbeat_timeout.as_millis()
                    )));
                }
                response = response_rx.recv(), if !outstanding.is_empty() => {
                    let Some(response) = response else {
                        break Err(ExecutionErrorKind::Protocol("host response channel closed".to_owned()));
                    };
                    if outstanding.remove(&response.id).is_none() {
                        continue;
                    }
                    let frame = match response.result {
                        Ok(value) => ParentFrame::Response {
                            version: PROTOCOL_VERSION,
                            id: response.id,
                            ok: true,
                            value: Some(RunnerValue::from_option(value)?),
                            error: None,
                        },
                        Err(error) => ParentFrame::Response {
                            version: PROTOCOL_VERSION,
                            id: response.id,
                            ok: false,
                            value: None,
                            error: Some(error.to_string()),
                        },
                    };
                    write_parent_frame(&mut stdin, &frame, self.config.max_frame_bytes).await?;
                }
                frame = read_child_frame(
                    &mut reader,
                    self.config.max_frame_bytes,
                    &mut output_bytes,
                    self.config.max_stdout_bytes,
                ) => {
                    let frame = frame?;
                    if portable_watchdog.is_some() {
                        portable_watchdog = Some(
                            Instant::now() + self.config.runner_heartbeat_timeout,
                        );
                    }
                    match frame {
                        ChildFrame::Heartbeat { .. } => {}
                        ChildFrame::Notice { method, value, .. } => {
                            notice_sequence = notice_sequence.saturating_add(1);
                            let notice = HostNotice {
                                run_id: prepared.run_id.clone(),
                                sequence: notice_sequence,
                                method,
                                value,
                            };
                            await_notice(host.as_ref(), notice, &cancellation, deadline, timeout_ms).await?;
                        }
                        ChildFrame::Request { id, method, params, .. } => {
                            if outstanding.contains_key(&id) {
                                break Err(ExecutionErrorKind::Protocol(format!(
                                    "duplicate host request id {id}"
                                )));
                            }
                            if outstanding.len() >= self.config.max_pending_requests {
                                let response = ParentFrame::Response {
                                    version: PROTOCOL_VERSION,
                                    id,
                                    ok: false,
                                    value: None,
                                    error: Some("too many pending workflow host requests".to_owned()),
                                };
                                write_parent_frame(&mut stdin, &response, self.config.max_frame_bytes).await?;
                                continue;
                            }
                            let call_cancellation = cancellation.child_token();
                            outstanding.insert(id, call_cancellation.clone());
                            let call = HostCall {
                                run_id: prepared.run_id.clone(),
                                request_id: id,
                                method,
                                params,
                                cancellation: call_cancellation,
                            };
                            let call_host = Arc::clone(&host);
                            let call_tx = response_tx.clone();
                            tokio::spawn(async move {
                                let result = call_host.request(call).await;
                                let _ = call_tx.send(HostTaskResponse { id, result }).await;
                            });
                        }
                        ChildFrame::Abort { id, .. } => {
                            if let Some(token) = outstanding.remove(&id) {
                                token.cancel();
                            }
                        }
                        ChildFrame::Result { value, .. } => break value.into_option(),
                        ChildFrame::Error { error, .. } => {
                            break Err(ExecutionErrorKind::Runner(error));
                        }
                        ChildFrame::Ready { .. } => {
                            break Err(ExecutionErrorKind::Protocol(
                                "received a duplicate ready frame".to_owned(),
                            ));
                        }
                    }
                }
            }
        };
        for token in outstanding.into_values() {
            token.cancel();
        }
        result
    }
}

impl Default for WorkflowExecutor {
    fn default() -> Self {
        Self::new(ExecutorConfig::default())
    }
}

#[derive(Debug)]
struct PreparedInvocation {
    run_id: String,
    dialect: WorkflowDialect,
    source: String,
    source_path: Option<PathBuf>,
    sha256: String,
    args: Option<Value>,
    timeout: Option<Duration>,
}

async fn prepare_source(
    invocation: WorkflowInvocation,
) -> Result<PreparedInvocation, ExecutionErrorKind> {
    let digest = format!("{:x}", Sha256::digest(invocation.source.as_bytes()));
    if let Some(expected) = &invocation.expected_source_sha256 {
        if !valid_sha256(expected) || !expected.eq_ignore_ascii_case(&digest) {
            return Err(ExecutionErrorKind::InvalidInvocation(
                "workflow source SHA-256 does not match its snapshot".to_owned(),
            ));
        }
    }
    let source_path = match (invocation.dialect, invocation.source_path) {
        (WorkflowDialect::Native, Some(path)) => {
            let canonical = tokio::fs::canonicalize(&path)
                .await
                .map(|path| dunce::simplified(&path).to_path_buf())
                .map_err(|error| {
                    ExecutionErrorKind::InvalidInvocation(format!(
                        "cannot canonicalize workflow source {}: {error}",
                        path.display()
                    ))
                })?;
            let bytes = tokio::fs::read(&canonical).await.map_err(|error| {
                ExecutionErrorKind::InvalidInvocation(format!(
                    "cannot read workflow source {}: {error}",
                    canonical.display()
                ))
            })?;
            if bytes != invocation.source.as_bytes() {
                return Err(ExecutionErrorKind::InvalidInvocation(format!(
                    "installed workflow source changed after discovery: {}",
                    canonical.display()
                )));
            }
            Some(canonical)
        }
        // The portable VM never imports this path. Keep it only so runtime
        // errors retain the original filename; source identity is the durable
        // snapshot and hash above, even if the installed entry changed or was
        // removed after start.
        (WorkflowDialect::ClaudePortable, path) => path,
        (WorkflowDialect::Native, None) => None,
    };
    Ok(PreparedInvocation {
        run_id: invocation.run_id,
        dialect: invocation.dialect,
        source: invocation.source,
        source_path,
        sha256: digest,
        args: invocation.args,
        timeout: invocation.timeout,
    })
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug)]
struct HostTaskResponse {
    id: u64,
    result: Result<Option<Value>, HostError>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum RunnerValue {
    Undefined,
    Json { json: String },
}

impl RunnerValue {
    fn from_option(value: Option<Value>) -> Result<Self, ExecutionErrorKind> {
        value.map_or(Ok(Self::Undefined), |value| {
            serde_json::to_string(&value)
                .map(|json| Self::Json { json })
                .map_err(|error| {
                    ExecutionErrorKind::Protocol(format!(
                        "failed to encode workflow value: {error}"
                    ))
                })
        })
    }

    fn into_option(self) -> Result<Option<Value>, ExecutionErrorKind> {
        match self {
            Self::Undefined => Ok(None),
            Self::Json { json } => serde_json::from_str(&json).map(Some).map_err(|error| {
                ExecutionErrorKind::Protocol(format!(
                    "workflow value contains invalid JSON: {error}"
                ))
            }),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ChildFrame {
    Ready {
        #[serde(rename = "version", deserialize_with = "protocol_version")]
        _version: (),
    },
    Heartbeat {
        #[serde(rename = "version", deserialize_with = "protocol_version")]
        _version: (),
    },
    Notice {
        #[serde(rename = "version", deserialize_with = "protocol_version")]
        _version: (),
        method: HostNoticeMethod,
        value: Value,
    },
    Request {
        #[serde(rename = "version", deserialize_with = "protocol_version")]
        _version: (),
        id: u64,
        method: HostRequestMethod,
        params: Value,
    },
    Abort {
        #[serde(rename = "version", deserialize_with = "protocol_version")]
        _version: (),
        id: u64,
        #[serde(rename = "reason")]
        _reason: String,
    },
    Result {
        #[serde(rename = "version", deserialize_with = "protocol_version")]
        _version: (),
        value: RunnerValue,
    },
    Error {
        #[serde(rename = "version", deserialize_with = "protocol_version")]
        _version: (),
        error: String,
    },
}

impl ChildFrame {
    const fn name(&self) -> &'static str {
        match self {
            Self::Ready { .. } => "ready",
            Self::Heartbeat { .. } => "heartbeat",
            Self::Notice { .. } => "notice",
            Self::Request { .. } => "request",
            Self::Abort { .. } => "abort",
            Self::Result { .. } => "result",
            Self::Error { .. } => "error",
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ParentFrame {
    Start {
        version: u8,
        dialect: WorkflowDialect,
        #[serde(rename = "runId")]
        run_id: String,
        source: String,
        #[serde(rename = "sourcePath", skip_serializing_if = "Option::is_none")]
        source_path: Option<PathBuf>,
        args: RunnerValue,
        #[serde(rename = "portableTimeoutMs")]
        portable_timeout_ms: u64,
    },
    Response {
        version: u8,
        id: u64,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<RunnerValue>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

fn protocol_version<'de, D>(deserializer: D) -> Result<(), D::Error>
where
    D: serde::Deserializer<'de>,
{
    let version = u8::deserialize(deserializer)?;
    if version == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(serde::de::Error::custom(format!(
            "unsupported protocol version {version}"
        )))
    }
}

async fn read_child_frame<R>(
    reader: &mut R,
    frame_limit: usize,
    output_bytes: &mut usize,
    output_limit: usize,
) -> Result<ChildFrame, ExecutionErrorKind>
where
    R: AsyncBufRead + Unpin,
{
    let line = read_bounded_line(reader, frame_limit)
        .await
        .map_err(|error| ExecutionErrorKind::Protocol(error.to_string()))?
        .ok_or_else(|| {
            ExecutionErrorKind::Protocol("workflow child closed stdout before result".to_owned())
        })?;
    *output_bytes = output_bytes.saturating_add(line.len());
    if *output_bytes > output_limit {
        return Err(ExecutionErrorKind::Protocol(format!(
            "workflow stdout exceeded {output_limit} bytes"
        )));
    }
    serde_json::from_slice(&line)
        .map_err(|error| ExecutionErrorKind::Protocol(format!("invalid child frame: {error}")))
}

async fn read_bounded_line<R>(reader: &mut R, limit: usize) -> io::Result<Option<Vec<u8>>>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(line))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        let content = newline.map_or(available, |index| &available[..index]);
        if line.len().saturating_add(content.len()) > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("workflow frame exceeded {limit} bytes"),
            ));
        }
        line.extend_from_slice(content);
        reader.consume(consumed);
        if newline.is_some() {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(line));
        }
    }
}

async fn write_parent_frame<W>(
    writer: &mut W,
    frame: &ParentFrame,
    frame_limit: usize,
) -> Result<(), ExecutionErrorKind>
where
    W: AsyncWrite + Unpin,
{
    let encoded = serde_json::to_vec(frame).map_err(|error| {
        ExecutionErrorKind::Protocol(format!("failed to encode host frame: {error}"))
    })?;
    if encoded.len() > frame_limit {
        return Err(ExecutionErrorKind::Protocol(format!(
            "host frame exceeded {frame_limit} bytes"
        )));
    }
    writer.write_all(&encoded).await.map_err(|error| {
        ExecutionErrorKind::Protocol(format!("failed to write host frame: {error}"))
    })?;
    writer.write_all(b"\n").await.map_err(|error| {
        ExecutionErrorKind::Protocol(format!("failed to terminate host frame: {error}"))
    })?;
    writer.flush().await.map_err(|error| {
        ExecutionErrorKind::Protocol(format!("failed to flush host frame: {error}"))
    })
}

async fn await_notice(
    host: &dyn WorkflowHost,
    notice: HostNotice,
    cancellation: &CancellationToken,
    deadline: Option<Instant>,
    timeout_ms: Option<u64>,
) -> Result<(), ExecutionErrorKind> {
    tokio::select! {
        result = host.notice(notice) => {
            result.map_err(|error| ExecutionErrorKind::Host(error.to_string()))
        }
        () = cancellation.cancelled() => Err(ExecutionErrorKind::Cancelled),
        () = wait_deadline(deadline) => Err(ExecutionErrorKind::ExecutionTimeout(timeout_ms.unwrap_or(0))),
    }
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

async fn wait_deadline(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        sleep_until(deadline).await;
    } else {
        pending::<()>().await;
    }
}

fn sanitized_path(bun_path: &Path) -> OsString {
    let mut paths = Vec::new();
    if let Some(parent) = bun_path.parent() {
        paths.push(parent.to_path_buf());
    }
    paths.extend([PathBuf::from("/usr/bin"), PathBuf::from("/bin")]);
    std::env::join_paths(paths).unwrap_or_else(|_| OsString::from("/usr/bin:/bin"))
}

fn find_bun() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let executable_name = if cfg!(windows) { "bun.exe" } else { "bun" };
    std::env::split_paths(&path)
        .map(|directory| directory.join(executable_name))
        .find(|candidate| executable(candidate))
}

fn executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[derive(Debug)]
struct CapturedStderr {
    bytes: Vec<u8>,
    truncated: bool,
}

fn drain_stderr(mut stderr: ChildStderr, limit: usize) -> JoinHandle<CapturedStderr> {
    tokio::spawn(async move {
        let mut captured = Vec::with_capacity(limit.min(8192));
        let mut buffer = [0u8; 8192];
        let mut truncated = false;
        loop {
            match stderr.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let remaining = limit.saturating_sub(captured.len());
                    captured.extend_from_slice(&buffer[..read.min(remaining)]);
                    truncated |= read > remaining;
                }
            }
        }
        CapturedStderr {
            bytes: captured,
            truncated,
        }
    })
}

async fn settle_child(
    child: &mut Child,
    terminate: bool,
    grace: Duration,
) -> Result<(), ExecutionErrorKind> {
    if terminate {
        terminate_child(child, grace).await;
        return Ok(());
    }
    match timeout(grace, child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => Err(ExecutionErrorKind::ChildExit(status.to_string())),
        Ok(Err(error)) => Err(ExecutionErrorKind::ChildExit(error.to_string())),
        Err(_) => {
            terminate_child(child, grace).await;
            Err(ExecutionErrorKind::ChildExit(
                "runtime did not exit after returning a result".to_owned(),
            ))
        }
    }
}

async fn terminate_child(child: &mut Child, grace: Duration) {
    let Some(id) = child.id() else {
        let _ = child.wait().await;
        return;
    };
    signal_process_group(id, false);
    if timeout(grace, child.wait()).await.is_ok() {
        return;
    }
    signal_process_group(id, true);
    let _ = child.start_kill();
    let _ = timeout(grace.max(Duration::from_millis(100)), child.wait()).await;
}

#[cfg(unix)]
fn signal_process_group(id: u32, force: bool) {
    use nix::{
        sys::signal::{Signal, killpg},
        unistd::Pid,
    };
    let signal = if force {
        Signal::SIGKILL
    } else {
        Signal::SIGTERM
    };
    if let Ok(raw) = i32::try_from(id) {
        let _ = killpg(Pid::from_raw(raw), signal);
    }
}

#[cfg(not(unix))]
fn signal_process_group(_id: u32, _force: bool) {}

fn bare_error(kind: ExecutionErrorKind) -> ExecutionError {
    ExecutionError {
        kind,
        stderr: String::new(),
        stderr_truncated: false,
    }
}

#[cfg(test)]
mod tests;
