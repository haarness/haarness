//! Durable workflow lifecycle coordinator.

use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, RwLock, Weak},
    time::Duration,
};

use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use fs2::FileExt;
use haarness_core::{CoreError, CoreResult, EventBus, WorkflowLogDraft, WorkflowRepository};
use haarness_protocol::{
    ThreadId, WorkflowArgumentType, WorkflowDefinition, WorkflowDialect as ProtocolDialect,
    WorkflowId, WorkflowLogSource, WorkflowMeta, WorkflowRun, WorkflowRunAgent,
    WorkflowRunAgentStatus, WorkflowRunDefinition, WorkflowRunStatus, WorkflowRunTime,
    WorkflowRunWarning, WorkflowStartRequest,
};
use haarness_workflow_catalog::{CatalogError, WorkflowCatalog};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;

use crate::{
    ExecutionErrorKind, HostCall, HostError, HostNotice, HostNoticeMethod, HostRequestMethod,
    WorkflowDialect, WorkflowExecutor, WorkflowHost, WorkflowInvocation,
};

/// Application services used by workflow host calls. The server adapter
/// implements this with a weak AgentRuntime reference, keeping construction
/// acyclic while preserving the normal tool/permission and agent paths.
#[async_trait]
pub trait WorkflowRuntimeHost: Send + Sync + 'static {
    async fn create_run_thread(
        &self,
        parent: Option<ThreadId>,
        title: &str,
    ) -> Result<ThreadId, HostError>;

    /// Execute one non-nested host request for a durable workflow thread.
    async fn request(
        &self,
        thread_id: ThreadId,
        call: HostCall,
    ) -> Result<Option<Value>, HostError>;

    /// Resolve a requested workflow-agent model against the launching
    /// conversation. Hosts with a live provider catalog may fall back to that
    /// conversation's current model and report the unavailable request so the
    /// coordinator can persist a durable warning.
    async fn resolve_agent_model(
        &self,
        _launch_thread_id: ThreadId,
        requested: &str,
    ) -> Result<WorkflowModelResolution, HostError> {
        Ok(WorkflowModelResolution {
            resolved: requested.to_owned(),
            unavailable_requested: None,
        })
    }

    /// Read the launching conversation's concrete child-agent defaults. The
    /// coordinator merges these only after per-call, phase, and run defaults,
    /// so persisted agent rows describe the values the runtime will execute.
    async fn workflow_agent_defaults(
        &self,
        _launch_thread_id: ThreadId,
    ) -> Result<WorkflowAgentDefaults, HostError> {
        Ok(WorkflowAgentDefaults::default())
    }

    /// Deliver a background run's terminal result to its launching thread.
    /// Implementations should preserve their normal active-turn queue
    /// semantics. The default keeps headless/test hosts source-compatible.
    async fn notify_terminal(
        &self,
        _launch_thread_id: ThreadId,
        _run: WorkflowRun,
    ) -> Result<(), HostError> {
        Ok(())
    }

    /// Release run-scoped continuations and other host resources after the
    /// isolated workflow process has settled. The default keeps lightweight
    /// test and embedding hosts source-compatible.
    async fn dispose_run_thread(&self, _thread_id: ThreadId) -> Result<(), HostError> {
        Ok(())
    }
}

/// Provider-catalog result for a workflow child-agent model request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowModelResolution {
    pub resolved: String,
    pub unavailable_requested: Option<String>,
}

/// Concrete values inherited by a workflow child agent from its launching
/// conversation when the workflow itself does not override them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowAgentDefaults {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub variant: Option<String>,
}

struct ActiveRun {
    cancellation: CancellationToken,
    _lease: WorkflowRunLease,
}

struct WorkflowRunLease {
    file: File,
}

impl WorkflowRunLease {
    fn acquire(id: WorkflowId) -> CoreResult<Self> {
        Self::try_acquire(id)?.ok_or_else(|| {
            CoreError::Conflict(format!(
                "workflow {id} is active in another Haarness process"
            ))
        })
    }

    fn try_acquire(id: WorkflowId) -> CoreResult<Option<Self>> {
        let directory = std::env::temp_dir().join("haarness-workflow-leases");
        fs::create_dir_all(&directory).map_err(|error| {
            CoreError::Storage(format!(
                "cannot create workflow lease directory {}: {error}",
                directory.display()
            ))
        })?;
        let path = directory.join(format!("{id}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| {
                CoreError::Storage(format!(
                    "cannot open workflow lease {}: {error}",
                    path.display()
                ))
            })?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self { file })),
            Err(error) if error.kind() == ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(CoreError::Storage(format!(
                "cannot lock workflow lease {}: {error}",
                path.display()
            ))),
        }
    }
}

impl Drop for WorkflowRunLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Process-local delivery bookkeeping, intentionally mirroring the lifetime
/// of the attached agent runtime. A restart must not synthesize stale workflow
/// results into a session after recovery marked the run interrupted.
struct DeliveryWatch {
    launch_thread_id: ThreadId,
    waits: usize,
    consumed: bool,
    pending_terminal: Option<WorkflowRun>,
    /// Paused: it is not working and owes its launcher nothing until it is
    /// resumed.
    paused: bool,
}

/// Durable state machine around the isolated executor.
pub struct WorkflowCoordinator {
    repository: Arc<dyn WorkflowRepository>,
    catalog: RwLock<Arc<WorkflowCatalog>>,
    executor: WorkflowExecutor,
    runtime_host: Arc<dyn WorkflowRuntimeHost>,
    events: EventBus,
    workspace: PathBuf,
    project_trusted: bool,
    active: DashMap<WorkflowId, ActiveRun>,
    state_locks: DashMap<WorkflowId, Arc<Mutex<()>>>,
    continuations: DashMap<String, (WorkflowId, String)>,
    delivery_watches: DashMap<WorkflowId, DeliveryWatch>,
}

impl WorkflowCoordinator {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        repository: Arc<dyn WorkflowRepository>,
        catalog: Arc<WorkflowCatalog>,
        executor: WorkflowExecutor,
        runtime_host: Arc<dyn WorkflowRuntimeHost>,
        events: EventBus,
        workspace: PathBuf,
        project_trusted: bool,
    ) -> Self {
        Self {
            repository,
            catalog: RwLock::new(catalog),
            executor,
            runtime_host,
            events,
            workspace,
            project_trusted,
            active: DashMap::new(),
            state_locks: DashMap::new(),
            continuations: DashMap::new(),
            delivery_watches: DashMap::new(),
        }
    }

    #[must_use]
    pub fn catalog(&self) -> Arc<WorkflowCatalog> {
        self.catalog
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Swap the discovery catalog used for future starts. Definitions for
    /// already-running workflows were materialized before launch and remain
    /// untouched.
    pub fn replace_catalog(&self, catalog: Arc<WorkflowCatalog>) {
        *self
            .catalog
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = catalog;
    }

    pub async fn recover(&self) -> CoreResult<u64> {
        let directory = self.workspace.display().to_string();
        let candidates = self.repository.running_workflow_ids(&directory).await?;
        let mut orphan_ids = Vec::new();
        let mut leases = Vec::new();
        for id in candidates {
            if let Some(lease) = WorkflowRunLease::try_acquire(id)? {
                orphan_ids.push(id);
                leases.push(lease);
            }
        }
        let recovered = self
            .repository
            .recover_interrupted_workflows(&directory, &orphan_ids)
            .await?;
        drop(leases);
        Ok(recovered)
    }

    pub async fn start(self: &Arc<Self>, request: WorkflowStartRequest) -> CoreResult<WorkflowRun> {
        let selected = usize::from(request.name.is_some())
            + usize::from(request.source.is_some())
            + usize::from(request.path.is_some());
        if selected != 1 {
            return Err(CoreError::Invalid(
                "exactly one of workflow name, source, or path is required".into(),
            ));
        }
        if request.timeout_ms == Some(0) {
            return Err(CoreError::Invalid(
                "workflow timeout must be a positive integer".into(),
            ));
        }
        let (definition, source, source_path) = self.resolve_start(&request)?;
        self.ensure_trusted(&definition, source_path.as_ref())?;
        let args_provided = definition.dialect == ProtocolDialect::Native || request.args.is_some();
        let args = workflow_args(&definition, request.args.clone())?;
        let thread_id = self
            .runtime_host
            .create_run_thread(
                request.thread_id,
                &format!("Workflow · {}", definition.meta.name),
            )
            .await
            .map_err(host_error)?;
        let now = Utc::now();
        let run = WorkflowRun {
            id: WorkflowId::new(),
            thread_id: Some(thread_id),
            directory: self.workspace.display().to_string(),
            workflow_name: definition.invocation.clone(),
            source_path: source_path.as_ref().map(|path| path.display().to_string()),
            source_hash: String::new(),
            args,
            status: WorkflowRunStatus::Running,
            phase: None,
            status_payload: None,
            logs: Vec::new(),
            warnings: Vec::new(),
            agents: Vec::new(),
            definition: run_definition(
                &definition,
                source,
                source_path.as_ref(),
                args_provided,
                &request,
            ),
            result: None,
            error: None,
            time: WorkflowRunTime {
                created: now,
                updated: now,
                started: Some(now),
                completed: None,
            },
        };
        // Acquire the cross-process lease before the durable row becomes
        // visible as running, closing the startup race with recovery.
        let lease = WorkflowRunLease::acquire(run.id)?;
        let run = self.repository.create_workflow_run(run).await?;
        if !request.wait
            && let Some(launch_thread_id) = request.thread_id
        {
            self.delivery_watches.insert(
                run.id,
                DeliveryWatch {
                    launch_thread_id,
                    waits: 0,
                    consumed: false,
                    pending_terminal: None,
                    paused: false,
                },
            );
        }
        self.append_log(
            run.id,
            format!("Started workflow {}", run.workflow_name),
            WorkflowLogSource::Platform,
        )
        .await?;
        let timeout = request
            .timeout_ms
            .map(|millis| Duration::from_millis(millis.clamp(1, 24 * 60 * 60 * 1_000)));
        if let Err(error) = self.spawn_execution(run.id, timeout, lease) {
            self.delivery_watches.remove(&run.id);
            return Err(error);
        }
        if request.wait {
            self.wait(run.id, Some(Duration::from_secs(24 * 60 * 60)))
                .await
        } else {
            self.get(run.id, None).await
        }
    }

    pub async fn list(
        &self,
        thread_id: Option<ThreadId>,
        limit: usize,
    ) -> CoreResult<Vec<WorkflowRun>> {
        self.repository
            .list_workflow_runs(
                Some(&self.workspace.display().to_string()),
                thread_id,
                limit,
            )
            .await
    }

    pub async fn get(
        &self,
        id: WorkflowId,
        after_sequence: Option<u64>,
    ) -> CoreResult<WorkflowRun> {
        let mut run = self.owned_run(id).await?;
        if let Some(after) = after_sequence {
            run.logs.retain(|entry| entry.sequence > after);
        }
        Ok(run)
    }

    pub async fn wait(&self, id: WorkflowId, timeout: Option<Duration>) -> CoreResult<WorkflowRun> {
        let future = async {
            loop {
                let run = self.get(id, None).await?;
                if terminal(run.status) || matches!(run.status, WorkflowRunStatus::Paused) {
                    return Ok(run);
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        };
        match timeout {
            // A caller-supplied timeout bounds how long this call waits; it is
            // not a workflow failure. Keep waiting nondestructive by
            // returning the latest nonterminal snapshot when the deadline
            // expires, so models can continue useful work without a red tool
            // error or destructive cancellation.
            Some(duration) => match tokio::time::timeout(duration, future).await {
                Ok(result) => result,
                Err(_) => self.get(id, None).await,
            },
            None => future.await,
        }
    }

    /// Background runs launched by `thread` that will still deliver their
    /// terminal result into it, including one being handed to it now. The
    /// runtime reads this through the probe the server installs, so a chat
    /// that launched them is not called done while they run. A paused run
    /// counts again once it is resumed.
    #[must_use]
    pub fn pending_deliveries(&self, thread: ThreadId) -> u32 {
        let pending = self
            .delivery_watches
            .iter()
            .filter(|watch| watch.launch_thread_id == thread && !watch.paused)
            .count();
        u32::try_from(pending).unwrap_or(u32::MAX)
    }

    /// Optimistically claim terminal delivery for a model-facing wait issued
    /// by the thread that launched this background run. Other sessions may
    /// inspect or wait without suppressing the launcher's notification.
    #[must_use]
    pub fn begin_launch_wait(&self, id: WorkflowId, thread_id: ThreadId) -> bool {
        let Some(mut watch) = self.delivery_watches.get_mut(&id) else {
            return false;
        };
        if watch.launch_thread_id != thread_id || watch.consumed {
            return false;
        }
        watch.waits = watch.waits.saturating_add(1);
        true
    }

    /// Finish a launcher wait. Terminal results remain claimed; timeout,
    /// cancellation, and paused/nonterminal returns release the optimistic
    /// claim. If completion raced that release, delivery is resumed here.
    pub async fn end_launch_wait(&self, id: WorkflowId, claimed: bool, terminal: bool) {
        if !claimed {
            return;
        }
        let mut deliver = None;
        let mut remove = false;
        if let Some(mut watch) = self.delivery_watches.get_mut(&id) {
            watch.waits = watch.waits.saturating_sub(1);
            if terminal {
                watch.consumed = true;
                remove = true;
            } else if watch.waits == 0
                && let Some(run) = watch.pending_terminal.take()
            {
                watch.consumed = true;
                deliver = Some((watch.launch_thread_id, run));
                remove = true;
            }
        }
        // A delivered run is forgotten only once its result was handed to the
        // launching thread, so `pending_deliveries` counts it until then.
        if let Some((launch_thread_id, run)) = deliver {
            self.deliver_terminal(launch_thread_id, run).await;
        }
        if remove {
            self.delivery_watches.remove(&id);
        }
    }

    pub async fn cancel(&self, id: WorkflowId) -> CoreResult<WorkflowRun> {
        self.transition_to_stopped(id, WorkflowRunStatus::Cancelled, "Workflow cancelled")
            .await
    }

    /// Cancel active workflows launched anywhere inside one conversation
    /// tree. Session cancellation suppresses terminal reinjection: the user
    /// asked that conversation to stop, so a cancellation notice must not
    /// start a fresh synthetic turn on the now-idle launcher.
    pub async fn cancel_for_launch_threads(
        &self,
        launch_threads: &HashSet<ThreadId>,
    ) -> CoreResult<u64> {
        let ids = self
            .active
            .iter()
            .map(|entry| *entry.key())
            .collect::<Vec<_>>();
        let mut cancelled = 0_u64;
        let mut first_error = None;
        for id in ids {
            let run = match self.owned_run(id).await {
                Ok(run) => run,
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    continue;
                }
            };
            if !run
                .definition
                .launch_thread_id
                .is_some_and(|thread_id| launch_threads.contains(&thread_id))
            {
                continue;
            }
            self.delivery_watches.remove(&id);
            match self
                .transition_to_stopped(
                    id,
                    WorkflowRunStatus::Cancelled,
                    "Workflow cancelled with its session",
                )
                .await
            {
                Ok(_) => cancelled = cancelled.saturating_add(1),
                Err(CoreError::Conflict(_)) if !self.active.contains_key(&id) => {}
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        first_error.map_or(Ok(cancelled), Err)
    }

    pub async fn pause(&self, id: WorkflowId) -> CoreResult<WorkflowRun> {
        self.transition_to_stopped(id, WorkflowRunStatus::Paused, "Workflow paused")
            .await
    }

    /// Persist every run owned by this process as interrupted before
    /// cancelling executors. This two-phase order prevents an executor racing
    /// shutdown from overwriting the resumable state as a user cancellation,
    /// while cancelling all runs promptly rather than waiting on them one by
    /// one.
    pub async fn interrupt_active(&self) -> CoreResult<u64> {
        let ids = self
            .active
            .iter()
            .map(|entry| *entry.key())
            .collect::<Vec<_>>();
        let mut interrupted = Vec::new();
        let mut first_error = None;
        for id in ids {
            let state_lock = self.state_lock(id);
            let guard = state_lock.lock().await;
            let result = async {
                let mut run = self.owned_run(id).await?;
                if run.status != WorkflowRunStatus::Running {
                    return Ok(false);
                }
                run.status = WorkflowRunStatus::Interrupted;
                run.time.updated = Utc::now();
                run.time.completed = Some(run.time.updated);
                run.error = Some("Workflow interrupted because Haarness stopped".into());
                let saved = self.repository.save_workflow_run(&run).await?;
                self.publish_updated(&saved);
                Ok(true)
            }
            .await;
            drop(guard);
            match result {
                Ok(true) => interrupted.push(id),
                Ok(false) => {}
                Err(error) => {
                    tracing::error!(run = %id, %error, "failed to persist workflow interruption");
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        for id in &interrupted {
            if let Some(active) = self.active.get(id) {
                active.cancellation.cancel();
            }
            self.delivery_watches.remove(id);
        }
        for id in &interrupted {
            if let Err(error) = self
                .append_log(
                    *id,
                    "Workflow interrupted because Haarness stopped".into(),
                    WorkflowLogSource::Platform,
                )
                .await
            {
                tracing::warn!(run = %id, %error, "failed to persist workflow interruption log");
            }
        }
        for id in &interrupted {
            if let Err(error) = self.wait_until_inactive(*id).await {
                tracing::warn!(run = %id, %error, "workflow executor did not stop promptly");
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(interrupted.len() as u64)
    }

    pub async fn resume(self: &Arc<Self>, id: WorkflowId) -> CoreResult<WorkflowRun> {
        let state_lock = self.state_lock(id);
        let guard = state_lock.lock().await;
        let mut run = self.owned_run(id).await?;
        if run.status == WorkflowRunStatus::Running {
            return Ok(run);
        }
        if !matches!(
            run.status,
            WorkflowRunStatus::Paused | WorkflowRunStatus::Interrupted
        ) {
            return Err(CoreError::Conflict(format!(
                "workflow {} is {}, not paused or interrupted",
                id,
                status_name(run.status)
            )));
        }
        self.ensure_source_trusted(
            run.definition.dialect,
            &run.definition.invocation,
            run.source_path.as_deref().map(Path::new),
        )?;
        // Hold the lease before publishing the resumed running state so a
        // concurrent process cannot recover the row in the hand-off window.
        let lease = WorkflowRunLease::acquire(id)?;
        run.status = WorkflowRunStatus::Running;
        run.error = None;
        run.result = None;
        run.time.started = Some(Utc::now());
        run.time.updated = Utc::now();
        run.time.completed = None;
        let run = self.repository.save_workflow_run(&run).await?;
        if let Some(mut watch) = self.delivery_watches.get_mut(&id) {
            watch.paused = false;
        }
        drop(guard);
        self.append_log(
            id,
            "Workflow resumed from the beginning".into(),
            WorkflowLogSource::Platform,
        )
        .await?;
        self.spawn_execution(id, None, lease)?;
        self.get(run.id, None).await
    }

    pub async fn delete(&self, id: WorkflowId) -> CoreResult<bool> {
        let state_lock = self.state_lock(id);
        let guard = state_lock.lock().await;
        match self.repository.workflow_run(id).await {
            Ok(run) if run.directory == self.workspace.display().to_string() => {}
            Ok(_) => return Err(CoreError::NotFound(format!("workflow run {id}"))),
            Err(CoreError::NotFound(_)) => return Ok(false),
            Err(error) => return Err(error),
        }
        let lease = if let Some(active) = self.active.get(&id) {
            active.cancellation.cancel();
            drop(active);
            drop(guard);
            self.wait_until_inactive(id).await?;
            WorkflowRunLease::acquire(id)?
        } else {
            let lease = WorkflowRunLease::acquire(id)?;
            drop(guard);
            lease
        };
        let _guard = state_lock.lock().await;
        match self.repository.workflow_run(id).await {
            Ok(run) if run.directory == self.workspace.display().to_string() => {}
            Ok(_) => return Err(CoreError::NotFound(format!("workflow run {id}"))),
            Err(CoreError::NotFound(_)) => return Ok(false),
            Err(error) => return Err(error),
        }
        let deleted = self.repository.delete_workflow_run(id).await?;
        drop(lease);
        if deleted {
            self.events
                .publish("workflow.deleted", None, None, None, json!({"runId": id}));
        }
        self.continuations.retain(|_, (run_id, _)| *run_id != id);
        self.delivery_watches.remove(&id);
        self.state_locks.remove(&id);
        Ok(deleted)
    }

    fn resolve_start(
        &self,
        request: &WorkflowStartRequest,
    ) -> CoreResult<(WorkflowDefinition, String, Option<PathBuf>)> {
        let catalog = self.catalog();
        let response = if let Some(name) = request.name.as_deref() {
            catalog.read_source(name)
        } else if let Some(source) = request.source.as_deref() {
            let definition = catalog
                .inspect_source(source, format!("<inline:{}>", uuid::Uuid::now_v7()))
                .map_err(catalog_error)?;
            return Ok((definition, source.to_owned(), None));
        } else {
            catalog.read_catalog_path(
                PathBuf::from(request.path.as_deref().unwrap_or_default()).as_path(),
            )
        }
        .map_err(catalog_error)?;
        let source_path = Some(PathBuf::from(&response.definition.path));
        Ok((response.definition, response.source, source_path))
    }

    fn ensure_trusted(
        &self,
        definition: &WorkflowDefinition,
        source_path: Option<&PathBuf>,
    ) -> CoreResult<()> {
        self.ensure_source_trusted(
            definition.dialect,
            &definition.invocation,
            source_path.map(PathBuf::as_path),
        )
    }

    fn ensure_source_trusted(
        &self,
        dialect: ProtocolDialect,
        invocation: &str,
        source_path: Option<&Path>,
    ) -> CoreResult<()> {
        let project_source = source_path.is_some_and(|path| self.catalog().is_project_path(path));
        if project_source && dialect == ProtocolDialect::Native && !self.project_trusted {
            return Err(CoreError::PermissionDenied(format!(
                "native project workflow {invocation} is disabled because project configuration is disabled"
            )));
        }
        if source_path.is_none() && dialect == ProtocolDialect::Native && !self.project_trusted {
            return Err(CoreError::PermissionDenied(
                "inline native workflows require project configuration; start without --disable-project-config and unset HAARNESS_DISABLE_PROJECT_CONFIG, or use claude-portable source"
                    .into(),
            ));
        }
        Ok(())
    }

    fn spawn_execution(
        self: &Arc<Self>,
        id: WorkflowId,
        timeout: Option<Duration>,
        lease: WorkflowRunLease,
    ) -> CoreResult<()> {
        if self.active.contains_key(&id) {
            return Err(CoreError::Conflict(format!(
                "workflow {id} is already running"
            )));
        }
        let cancellation = CancellationToken::new();
        self.active.insert(
            id,
            ActiveRun {
                cancellation: cancellation.clone(),
                _lease: lease,
            },
        );
        let coordinator = Arc::clone(self);
        // The run outlives the request or tool call that started it; its agent
        // turns join the starter's trace under one `workflow.run` span.
        let span = tracing::info_span!(
            "workflow.run",
            "otel.kind" = "internal",
            "haarness.workflow.run.id" = %id,
        );
        tokio::spawn(
            async move {
                coordinator.execute_saved(id, timeout, cancellation).await;
            }
            .instrument(span),
        );
        Ok(())
    }

    async fn execute_saved(
        self: Arc<Self>,
        id: WorkflowId,
        timeout: Option<Duration>,
        cancellation: CancellationToken,
    ) {
        let mut execution_thread_id = None;
        let result = async {
            let run = self.repository.workflow_run(id).await?;
            let source =
                run.definition.source.clone().ok_or_else(|| {
                    CoreError::Storage("workflow source snapshot is missing".into())
                })?;
            let thread_id = run
                .thread_id
                .ok_or_else(|| CoreError::Storage("workflow run session is missing".into()))?;
            execution_thread_id = Some(thread_id);
            let invocation = invocation_for(&run, source, timeout);
            let host: Arc<dyn WorkflowHost> = Arc::new(RunExecutionHost {
                coordinator: Arc::downgrade(&self),
                run_id: id,
                thread_id,
                meta: run.definition.meta.clone(),
            });
            self.executor
                .execute(invocation, host, cancellation)
                .await
                .map_err(|error| match error.kind {
                    ExecutionErrorKind::Cancelled => CoreError::Cancelled,
                    _ => CoreError::Tool(error.to_string()),
                })
        }
        .await;

        if let Some(thread_id) = execution_thread_id {
            if let Err(error) = self.runtime_host.dispose_run_thread(thread_id).await {
                tracing::warn!(run = %id, %error, "failed to dispose workflow host resources");
            }
        }
        self.continuations.retain(|_, (run_id, _)| *run_id != id);

        let state_lock = self.state_lock(id);
        let guard = state_lock.lock().await;
        let Ok(mut run) = self.repository.workflow_run(id).await else {
            self.active.remove(&id);
            return;
        };
        if run.status != WorkflowRunStatus::Running {
            self.active.remove(&id);
            return;
        }
        run.time.updated = Utc::now();
        run.time.completed = Some(run.time.updated);
        let terminal_message = match result {
            Ok(output) => {
                run.status = WorkflowRunStatus::Completed;
                run.result = output.value;
                run.error = None;
                format!("Completed workflow {}", run.workflow_name)
            }
            Err(CoreError::Cancelled) => {
                run.status = WorkflowRunStatus::Cancelled;
                run.error = Some("Workflow cancelled".into());
                "Workflow cancelled".into()
            }
            Err(error) => {
                run.status = WorkflowRunStatus::Failed;
                let error = error.to_string();
                run.error = Some(error.clone());
                format!("Failed workflow: {error}")
            }
        };
        let saved = self.repository.save_workflow_run(&run).await;
        drop(guard);
        match saved {
            Ok(saved) => {
                self.publish_updated(&saved);
                if let Err(error) = self
                    .append_log(id, terminal_message, WorkflowLogSource::Platform)
                    .await
                {
                    tracing::error!(run = %id, %error, "failed to persist workflow terminal log");
                }
                self.terminal_ready(saved).await;
            }
            Err(error) => tracing::error!(run = %id, %error, "failed to persist workflow result"),
        }
        self.active.remove(&id);
    }

    async fn transition_to_stopped(
        &self,
        id: WorkflowId,
        status: WorkflowRunStatus,
        message: &str,
    ) -> CoreResult<WorkflowRun> {
        let state_lock = self.state_lock(id);
        let guard = state_lock.lock().await;
        let mut run = self.owned_run(id).await?;
        if run.status == status {
            return Ok(run);
        }
        let allowed = match status {
            WorkflowRunStatus::Paused => run.status == WorkflowRunStatus::Running,
            WorkflowRunStatus::Cancelled => matches!(
                run.status,
                WorkflowRunStatus::Running
                    | WorkflowRunStatus::Paused
                    | WorkflowRunStatus::Interrupted
            ),
            _ => false,
        };
        if !allowed {
            return Err(CoreError::Conflict(format!(
                "cannot set workflow {} from {} to {}",
                id,
                status_name(run.status),
                status_name(status)
            )));
        }
        let _external_lease = if self.active.contains_key(&id) {
            None
        } else {
            Some(WorkflowRunLease::acquire(id)?)
        };
        run.status = status;
        run.time.updated = Utc::now();
        run.time.completed = (status == WorkflowRunStatus::Cancelled).then_some(run.time.updated);
        run.error = (status == WorkflowRunStatus::Cancelled).then(|| message.to_owned());
        let saved = self.repository.save_workflow_run(&run).await?;
        // Paused, the run owes nothing; cancelled, it owes its notice.
        if let Some(mut watch) = self.delivery_watches.get_mut(&id) {
            watch.paused = status == WorkflowRunStatus::Paused;
        }
        if let Some(active) = self.active.get(&id) {
            active.cancellation.cancel();
        }
        self.publish_updated(&saved);
        drop(guard);
        self.append_log(id, message.to_owned(), WorkflowLogSource::Platform)
            .await?;
        self.wait_until_inactive(id).await?;
        let run = self.get(id, None).await?;
        if status == WorkflowRunStatus::Cancelled {
            self.terminal_ready(run.clone()).await;
        }
        Ok(run)
    }

    async fn terminal_ready(&self, run: WorkflowRun) {
        let id = run.id;
        let mut deliver = None;
        let mut remove = false;
        if let Some(mut watch) = self.delivery_watches.get_mut(&id) {
            if watch.consumed {
                remove = true;
            } else if watch.waits > 0 {
                watch.pending_terminal = Some(run);
            } else {
                watch.consumed = true;
                deliver = Some((watch.launch_thread_id, run));
                remove = true;
            }
        }
        // As in `end_launch_wait`: forgotten once handed over.
        if let Some((launch_thread_id, run)) = deliver {
            self.deliver_terminal(launch_thread_id, run).await;
        }
        if remove {
            self.delivery_watches.remove(&id);
        }
    }

    async fn deliver_terminal(&self, launch_thread_id: ThreadId, run: WorkflowRun) {
        if let Err(error) = self
            .runtime_host
            .notify_terminal(launch_thread_id, run.clone())
            .await
        {
            tracing::warn!(
                run = %run.id,
                thread = %launch_thread_id,
                %error,
                "failed to deliver workflow terminal result"
            );
        }
    }

    async fn wait_until_inactive(&self, id: WorkflowId) -> CoreResult<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.active.contains_key(&id) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .map_err(|_| {
            CoreError::Conflict(format!(
                "workflow {id} did not stop its previous execution in time"
            ))
        })
    }

    async fn owned_run(&self, id: WorkflowId) -> CoreResult<WorkflowRun> {
        let run = self.repository.workflow_run(id).await?;
        if run.directory != self.workspace.display().to_string() {
            return Err(CoreError::NotFound(format!("workflow run {id}")));
        }
        Ok(run)
    }

    async fn apply_notice(&self, notice: HostNotice, meta: &WorkflowMeta) -> CoreResult<()> {
        let id = parse_workflow_id(&notice.run_id)?;
        match notice.method {
            HostNoticeMethod::Log => {
                let message = notice
                    .value
                    .as_str()
                    .map_or_else(|| notice.value.to_string(), str::to_owned);
                self.append_log(id, message, WorkflowLogSource::Workflow)
                    .await?;
            }
            HostNoticeMethod::Phase => {
                let phase = notice
                    .value
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| CoreError::Invalid("workflow phase must be a string".into()))?
                    .to_owned();
                let state_lock = self.state_lock(id);
                let guard = state_lock.lock().await;
                let mut run = self.repository.workflow_run(id).await?;
                if run.status != WorkflowRunStatus::Running {
                    return Ok(());
                }
                run.phase = Some(phase.clone());
                if !meta.phases.iter().any(|declared| declared.title == phase) {
                    run.warnings.push(WorkflowRunWarning {
                        time: Utc::now(),
                        message: format!("Workflow phase \"{phase}\" is not declared"),
                        phase: Some(phase.clone()),
                        requested_model: None,
                        resolved_model: None,
                    });
                }
                run.time.updated = Utc::now();
                let saved = self.repository.save_workflow_run(&run).await?;
                self.publish_updated(&saved);
                drop(guard);
                self.append_log(id, format!("Phase: {phase}"), WorkflowLogSource::Workflow)
                    .await?;
            }
            HostNoticeMethod::Status => {
                if !notice.value.is_object() {
                    return Err(CoreError::Invalid(
                        "workflow status payload must be an object".into(),
                    ));
                }
                let state_lock = self.state_lock(id);
                let _guard = state_lock.lock().await;
                let mut run = self.repository.workflow_run(id).await?;
                if run.status != WorkflowRunStatus::Running {
                    return Ok(());
                }
                run.status_payload = Some(notice.value);
                run.time.updated = Utc::now();
                let saved = self.repository.save_workflow_run(&run).await?;
                self.publish_updated(&saved);
            }
        }
        Ok(())
    }

    async fn append_log(
        &self,
        id: WorkflowId,
        message: String,
        source: WorkflowLogSource,
    ) -> CoreResult<()> {
        let current = self.repository.workflow_run(id).await?;
        let entry = self
            .repository
            .append_workflow_log(
                id,
                WorkflowLogDraft {
                    time: Utc::now(),
                    message,
                    phase: current.phase,
                    source: Some(source),
                },
            )
            .await?;
        self.events.publish_scoped(
            "workflow.log",
            current.thread_id,
            current.definition.launch_thread_id.or(current.thread_id),
            None,
            None,
            json!({"runId": id, "entry": entry}),
        );
        let updated = self.repository.workflow_run(id).await?;
        self.publish_updated(&updated);
        Ok(())
    }

    async fn execute_nested(
        self: &Arc<Self>,
        run_id: WorkflowId,
        thread_id: ThreadId,
        name: &str,
        args: Option<Value>,
        cancellation: CancellationToken,
    ) -> Result<Option<Value>, HostError> {
        let response = self
            .catalog()
            .read_source(name)
            .map_err(|error| HostError::new(catalog_error(error).to_string()))?;
        self.ensure_trusted(
            &response.definition,
            Some(&PathBuf::from(&response.definition.path)),
        )
        .map_err(|error| HostError::new(error.to_string()))?;
        let host: Arc<dyn WorkflowHost> = Arc::new(RunExecutionHost {
            coordinator: Arc::downgrade(self),
            run_id,
            thread_id,
            meta: response.definition.meta.clone(),
        });
        let args = workflow_args(&response.definition, args)
            .map_err(|error| HostError::new(error.to_string()))?;
        let mut invocation = WorkflowInvocation::new(run_id.to_string(), response.source);
        invocation.source_path = Some(PathBuf::from(response.definition.path));
        invocation.dialect = match response.definition.dialect {
            ProtocolDialect::Native => WorkflowDialect::Native,
            ProtocolDialect::ClaudePortable => WorkflowDialect::ClaudePortable,
        };
        invocation.args = args;
        self.executor
            .execute(invocation, host, cancellation)
            .await
            .map(|output| output.value)
            .map_err(|error| HostError::new(error.to_string()))
    }

    async fn begin_agent(&self, run_id: WorkflowId, call: &HostCall) -> Result<String, HostError> {
        let agent_id = format!("agent-{}", call.request_id);
        let state_lock = self.state_lock(run_id);
        let guard = state_lock.lock().await;
        let mut run = self
            .repository
            .workflow_run(run_id)
            .await
            .map_err(|error| HostError::new(error.to_string()))?;
        let input = call.params.as_object();
        run.agents.push(WorkflowRunAgent {
            id: agent_id.clone(),
            label: input
                .and_then(|value| value.get("label"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            agent: input
                .and_then(|value| value.get("agent"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            phase: input
                .and_then(|value| value.get("phase"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| run.phase.clone()),
            status: WorkflowRunAgentStatus::Running,
            thread_id: None,
            model: input
                .and_then(|value| value.get("model"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            variant: input
                .and_then(|value| value.get("variant"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            output: None,
            error: None,
            started_at: Utc::now(),
            completed_at: None,
        });
        run.time.updated = Utc::now();
        let saved = self
            .repository
            .save_workflow_run(&run)
            .await
            .map_err(|error| HostError::new(error.to_string()))?;
        self.publish_updated(&saved);
        drop(guard);
        self.append_log(
            run_id,
            format!("Agent {agent_id} started"),
            WorkflowLogSource::Platform,
        )
        .await
        .map_err(|error| HostError::new(error.to_string()))?;
        Ok(agent_id)
    }

    async fn apply_phase_agent_defaults(
        &self,
        run_id: WorkflowId,
        meta: &WorkflowMeta,
        params: &mut Value,
    ) -> Result<(), HostError> {
        let run = self
            .repository
            .workflow_run(run_id)
            .await
            .map_err(|error| HostError::new(error.to_string()))?;
        if let Some(prompt) = params.as_str().map(str::to_owned) {
            *params = json!({"prompt": prompt});
        }
        let Some(input) = params.as_object_mut() else {
            return Ok(());
        };
        let phase_name = input
            .get("phase")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| run.phase.clone());
        let phase = phase_name
            .as_deref()
            .and_then(|current| meta.phases.iter().find(|phase| phase.title == current));
        if !input.contains_key("agent")
            && let Some(agent) = &run.definition.agent
        {
            input.insert("agent".into(), Value::String(agent.clone()));
        }
        if !input.contains_key("model")
            && let Some(model) = phase
                .and_then(|phase| phase.model.as_ref())
                .or(run.definition.model.as_ref())
        {
            input.insert("model".into(), Value::String(model.clone()));
        }
        if !input.contains_key("variant")
            && let Some(variant) = phase
                .and_then(|phase| phase.variant.as_ref())
                .or(run.definition.variant.as_ref())
        {
            input.insert("variant".into(), Value::String(variant.clone()));
        }

        let launch_thread_id = run
            .definition
            .launch_thread_id
            .or(run.thread_id)
            .ok_or_else(|| {
                HostError::new("workflow run is missing its launch and execution sessions")
            })?;
        let launch_defaults = self
            .runtime_host
            .workflow_agent_defaults(launch_thread_id)
            .await?;
        for (key, value) in [
            ("agent", launch_defaults.agent),
            ("model", launch_defaults.model),
            ("variant", launch_defaults.variant),
        ] {
            if !input.contains_key(key)
                && let Some(value) = value
            {
                input.insert(key.into(), Value::String(value));
            }
        }

        let Some(requested) = input.get("model").and_then(Value::as_str) else {
            return Ok(());
        };
        let resolution = self
            .runtime_host
            .resolve_agent_model(launch_thread_id, requested)
            .await?;
        input.insert("model".into(), Value::String(resolution.resolved.clone()));

        if let Some(requested_model) = resolution.unavailable_requested {
            let state_lock = self.state_lock(run_id);
            let _guard = state_lock.lock().await;
            let mut current = self
                .repository
                .workflow_run(run_id)
                .await
                .map_err(|error| HostError::new(error.to_string()))?;
            current.warnings.push(WorkflowRunWarning {
                time: Utc::now(),
                message: format!(
                    "Requested model {requested_model} is unavailable; using {}",
                    resolution.resolved
                ),
                phase: phase_name,
                requested_model: Some(requested_model),
                resolved_model: Some(resolution.resolved),
            });
            current.time.updated = Utc::now();
            let saved = self
                .repository
                .save_workflow_run(&current)
                .await
                .map_err(|error| HostError::new(error.to_string()))?;
            self.publish_updated(&saved);
        }
        Ok(())
    }

    async fn finish_agent(
        &self,
        run_id: WorkflowId,
        agent_id: &str,
        result: &Result<Option<Value>, HostError>,
        cancelled: bool,
    ) -> Result<(), HostError> {
        let state_lock = self.state_lock(run_id);
        let guard = state_lock.lock().await;
        let mut run = self
            .repository
            .workflow_run(run_id)
            .await
            .map_err(|error| HostError::new(error.to_string()))?;
        let Some(agent) = run.agents.iter_mut().find(|agent| agent.id == agent_id) else {
            return Err(HostError::new("workflow agent record disappeared"));
        };
        agent.completed_at = Some(Utc::now());
        match result {
            Ok(Some(Value::Null)) => {
                agent.status = WorkflowRunAgentStatus::Failed;
                agent.error =
                    Some("Agent failed and returned null because onError was set to null".into());
            }
            Ok(value) => {
                agent.status = WorkflowRunAgentStatus::Completed;
                agent.output = value
                    .as_ref()
                    .and_then(|value| value.get("text"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                agent.thread_id = value
                    .as_ref()
                    .and_then(|value| serde_json::from_value(value.get("threadID")?.clone()).ok());
                if let Some(token) = value
                    .as_ref()
                    .and_then(|value| value.get("continuation"))
                    .and_then(Value::as_str)
                {
                    self.continuations
                        .insert(token.to_owned(), (run_id, agent_id.to_owned()));
                }
            }
            Err(error) => {
                agent.status = if cancelled {
                    WorkflowRunAgentStatus::Cancelled
                } else {
                    WorkflowRunAgentStatus::Failed
                };
                agent.error = Some(error.to_string());
            }
        }
        let status = match agent.status {
            WorkflowRunAgentStatus::Running => "running",
            WorkflowRunAgentStatus::Completed => "completed",
            WorkflowRunAgentStatus::Failed => "failed",
            WorkflowRunAgentStatus::Cancelled => "cancelled",
        };
        run.time.updated = Utc::now();
        let saved = self
            .repository
            .save_workflow_run(&run)
            .await
            .map_err(|error| HostError::new(error.to_string()))?;
        self.publish_updated(&saved);
        drop(guard);
        self.append_log(
            run_id,
            format!("Agent {agent_id} {status}"),
            WorkflowLogSource::Platform,
        )
        .await
        .map_err(|error| HostError::new(error.to_string()))
    }

    async fn restart_agent(&self, run_id: WorkflowId, agent_id: &str) -> Result<(), HostError> {
        let state_lock = self.state_lock(run_id);
        let guard = state_lock.lock().await;
        let mut run = self
            .repository
            .workflow_run(run_id)
            .await
            .map_err(|error| HostError::new(error.to_string()))?;
        let agent = run
            .agents
            .iter_mut()
            .find(|agent| agent.id == agent_id)
            .ok_or_else(|| HostError::new("workflow agent record disappeared"))?;
        agent.status = WorkflowRunAgentStatus::Running;
        agent.output = None;
        agent.error = None;
        agent.started_at = Utc::now();
        agent.completed_at = None;
        run.time.updated = Utc::now();
        let saved = self
            .repository
            .save_workflow_run(&run)
            .await
            .map_err(|error| HostError::new(error.to_string()))?;
        self.publish_updated(&saved);
        drop(guard);
        self.append_log(
            run_id,
            format!("Agent {agent_id} follow-up started"),
            WorkflowLogSource::Platform,
        )
        .await
        .map_err(|error| HostError::new(error.to_string()))
    }

    fn publish_updated(&self, run: &WorkflowRun) {
        self.events.publish_scoped(
            "workflow.updated",
            run.thread_id,
            run.definition.launch_thread_id.or(run.thread_id),
            None,
            None,
            json!({"info": run}),
        );
    }

    fn state_lock(&self, id: WorkflowId) -> Arc<Mutex<()>> {
        self.state_locks
            .entry(id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

struct RunExecutionHost {
    coordinator: Weak<WorkflowCoordinator>,
    run_id: WorkflowId,
    thread_id: ThreadId,
    meta: WorkflowMeta,
}

#[async_trait]
impl WorkflowHost for RunExecutionHost {
    async fn notice(&self, notice: HostNotice) -> Result<(), HostError> {
        let coordinator = self
            .coordinator
            .upgrade()
            .ok_or_else(|| HostError::new("workflow coordinator stopped"))?;
        if notice.run_id != self.run_id.to_string() {
            return Err(HostError::new("workflow notice run id mismatch"));
        }
        coordinator
            .apply_notice(notice, &self.meta)
            .await
            .map_err(|error| HostError::new(error.to_string()))
    }

    async fn request(&self, mut call: HostCall) -> Result<Option<Value>, HostError> {
        let coordinator = self
            .coordinator
            .upgrade()
            .ok_or_else(|| HostError::new("workflow coordinator stopped"))?;
        if call.run_id != self.run_id.to_string() {
            return Err(HostError::new("workflow host-call run id mismatch"));
        }
        if call.method == HostRequestMethod::Workflow {
            let name = call
                .params
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| HostError::new("nested workflow name is required"))?;
            let args = call.params.get("args").cloned();
            return coordinator
                .execute_nested(self.run_id, self.thread_id, name, args, call.cancellation)
                .await;
        }
        if call.method == HostRequestMethod::Agent {
            coordinator
                .apply_phase_agent_defaults(self.run_id, &self.meta, &mut call.params)
                .await?;
            let agent_id = coordinator.begin_agent(self.run_id, &call).await?;
            let cancellation = call.cancellation.clone();
            let result = coordinator.runtime_host.request(self.thread_id, call).await;
            coordinator
                .finish_agent(self.run_id, &agent_id, &result, cancellation.is_cancelled())
                .await?;
            return result;
        }
        if call.method == HostRequestMethod::AgentFollowup {
            let tracking = call
                .params
                .get("token")
                .and_then(Value::as_str)
                .and_then(|token| {
                    coordinator
                        .continuations
                        .get(token)
                        .map(|entry| entry.value().clone())
                });
            if let Some((run_id, agent_id)) = tracking
                && run_id == self.run_id
            {
                coordinator.restart_agent(run_id, &agent_id).await?;
                let cancellation = call.cancellation.clone();
                let result = coordinator.runtime_host.request(self.thread_id, call).await;
                coordinator
                    .finish_agent(run_id, &agent_id, &result, cancellation.is_cancelled())
                    .await?;
                return result;
            }
        }
        if call.method == HostRequestMethod::AgentClose {
            let token = call
                .params
                .get("token")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let result = coordinator.runtime_host.request(self.thread_id, call).await;
            if result.is_ok()
                && let Some(token) = token
            {
                coordinator.continuations.remove(&token);
            }
            return result;
        }
        coordinator.runtime_host.request(self.thread_id, call).await
    }
}

pub(crate) fn workflow_args(
    definition: &WorkflowDefinition,
    input: Option<Value>,
) -> CoreResult<Option<Value>> {
    if definition.dialect == ProtocolDialect::ClaudePortable {
        return Ok(input);
    }
    let input = input.unwrap_or_else(|| json!({}));
    let args = input.as_object().ok_or_else(|| {
        CoreError::Invalid("native workflow arguments must be a JSON object".into())
    })?;
    let mut result = serde_json::Map::new();
    for (key, definition) in &definition.meta.arguments {
        let Some(raw) = args.get(key).or(definition.default.as_ref()) else {
            continue;
        };
        let value = match definition.kind.unwrap_or(WorkflowArgumentType::String) {
            WorkflowArgumentType::Number => {
                if let Some(number) = raw.as_f64() {
                    serde_json::Number::from_f64(number)
                        .map(Value::Number)
                        .ok_or_else(|| {
                            CoreError::Invalid(format!(
                                "workflow argument {key} must be a finite number"
                            ))
                        })?
                } else if let Some(number) = raw
                    .as_str()
                    .and_then(|value| value.parse::<f64>().ok())
                    .and_then(serde_json::Number::from_f64)
                {
                    Value::Number(number)
                } else {
                    return Err(CoreError::Invalid(format!(
                        "workflow argument {key} must be a number"
                    )));
                }
            }
            WorkflowArgumentType::Boolean => match raw {
                Value::Bool(value) => Value::Bool(*value),
                Value::String(value) if value == "true" => Value::Bool(true),
                Value::String(value) if value == "false" => Value::Bool(false),
                _ => {
                    return Err(CoreError::Invalid(format!(
                        "workflow argument {key} must be a boolean"
                    )));
                }
            },
            WorkflowArgumentType::String => Value::String(match raw {
                Value::String(value) => value.clone(),
                value => value.to_string(),
            }),
        };
        result.insert(key.clone(), value);
    }
    for (key, value) in args {
        result.entry(key.clone()).or_insert_with(|| value.clone());
    }
    Ok(Some(Value::Object(result)))
}

fn run_definition(
    definition: &WorkflowDefinition,
    source: String,
    source_path: Option<&PathBuf>,
    args_provided: bool,
    request: &WorkflowStartRequest,
) -> WorkflowRunDefinition {
    WorkflowRunDefinition {
        name: definition.name.clone(),
        invocation: definition.invocation.clone(),
        launch_thread_id: request.thread_id,
        plugin_name: definition.plugin_name.clone(),
        agent: request.agent.clone(),
        model: request.model.clone(),
        variant: request.variant.clone(),
        dialect: definition.dialect,
        meta: definition.meta.clone(),
        source: Some(source),
        source_path: source_path.map(|path| path.display().to_string()),
        args_provided: Some(args_provided),
    }
}

fn invocation_for(
    run: &WorkflowRun,
    source: String,
    timeout: Option<Duration>,
) -> WorkflowInvocation {
    let mut invocation = WorkflowInvocation::new(run.id.to_string(), source);
    invocation.dialect = match run.definition.dialect {
        ProtocolDialect::Native => WorkflowDialect::Native,
        ProtocolDialect::ClaudePortable => WorkflowDialect::ClaudePortable,
    };
    invocation.source_path = run.source_path.as_ref().map(PathBuf::from);
    invocation.expected_source_sha256 = Some(run.source_hash.clone());
    invocation.args.clone_from(&run.args);
    invocation.timeout = timeout;
    invocation
}

fn parse_workflow_id(value: &str) -> CoreResult<WorkflowId> {
    uuid::Uuid::parse_str(value)
        .map(WorkflowId)
        .map_err(|error| CoreError::Invalid(format!("invalid workflow id: {error}")))
}

fn terminal(status: WorkflowRunStatus) -> bool {
    matches!(
        status,
        WorkflowRunStatus::Completed
            | WorkflowRunStatus::Failed
            | WorkflowRunStatus::Cancelled
            | WorkflowRunStatus::Interrupted
    )
}

fn status_name(status: WorkflowRunStatus) -> &'static str {
    match status {
        WorkflowRunStatus::Running => "running",
        WorkflowRunStatus::Completed => "completed",
        WorkflowRunStatus::Failed => "failed",
        WorkflowRunStatus::Cancelled => "cancelled",
        WorkflowRunStatus::Paused => "paused",
        WorkflowRunStatus::Interrupted => "interrupted",
    }
}

fn catalog_error(error: CatalogError) -> CoreError {
    match error {
        CatalogError::NotFound(name) => CoreError::NotFound(format!("workflow {name}")),
        CatalogError::Read { path, message } => {
            CoreError::Invalid(format!("workflow {path}: {message}"))
        }
    }
}

fn host_error(error: HostError) -> CoreError {
    CoreError::Tool(error.to_string())
}
