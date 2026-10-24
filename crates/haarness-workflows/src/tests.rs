use std::{
    collections::HashSet,
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use chrono::Utc;
use haarness_core::{CoreError, EventBus, ThreadRepository, WorkflowRepository};
use haarness_protocol::{
    CreateThreadRequest, ThreadId, WorkflowDialect as ProtocolDialect, WorkflowId, WorkflowMeta,
    WorkflowRun, WorkflowRunDefinition, WorkflowRunStatus, WorkflowRunTime, WorkflowStartRequest,
};
use haarness_store::SqliteStore;
use serde_json::{Value, json};
use tokio::io::BufReader;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use super::*;

#[derive(Default)]
struct RecordingHost {
    notices: Mutex<Vec<HostNotice>>,
    calls: Mutex<Vec<(HostRequestMethod, Value)>>,
}

#[async_trait]
impl WorkflowHost for RecordingHost {
    async fn notice(&self, notice: HostNotice) -> Result<(), HostError> {
        self.notices.lock().expect("notice lock").push(notice);
        Ok(())
    }

    async fn request(&self, call: HostCall) -> Result<Option<Value>, HostError> {
        self.calls
            .lock()
            .expect("call lock")
            .push((call.method, call.params.clone()));
        let value = match call.method {
            HostRequestMethod::Agent => {
                json!({"text":"first","data":{"step":1},"continuation":"agent-token"})
            }
            HostRequestMethod::AgentFollowup => {
                json!({"text":"second","data":{"step":2},"continuation":"agent-token"})
            }
            HostRequestMethod::AgentClose => Value::Null,
            HostRequestMethod::Shell => json!({"output":"shell-ok","exitCode":0}),
            HostRequestMethod::Tool => json!({"tool":"ok"}),
            HostRequestMethod::Workflow => json!({"nested":"ok"}),
        };
        Ok(Some(value))
    }
}

/// Runtime bridge for coordinator tests. It creates real durable child
/// threads, while agent calls are deterministic and can be held until the
/// coordinator cancels them.
struct CoordinatorHost {
    store: Arc<SqliteStore>,
    block_requests: usize,
    request_count: AtomicUsize,
    requests_started: Semaphore,
    calls: Mutex<Vec<(HostRequestMethod, Value)>>,
    disposed_threads: Mutex<Vec<ThreadId>>,
    notifications: Mutex<Vec<(ThreadId, WorkflowRun)>>,
    /// When set, each notification also records what the coordinator
    /// reported as pending for the launch thread while it was delivered.
    coordinator: std::sync::OnceLock<std::sync::Weak<WorkflowCoordinator>>,
    pending_while_notified: Mutex<Vec<u32>>,
    /// The chat folder every chat of this host works in, when set.
    chat_folder: std::sync::OnceLock<PathBuf>,
}

impl CoordinatorHost {
    fn new(store: Arc<SqliteStore>, block_requests: usize) -> Self {
        Self {
            store,
            block_requests,
            request_count: AtomicUsize::new(0),
            requests_started: Semaphore::new(0),
            calls: Mutex::new(Vec::new()),
            disposed_threads: Mutex::new(Vec::new()),
            notifications: Mutex::new(Vec::new()),
            coordinator: std::sync::OnceLock::new(),
            pending_while_notified: Mutex::new(Vec::new()),
            chat_folder: std::sync::OnceLock::new(),
        }
    }

    async fn wait_for_request(&self) {
        let permit = tokio::time::timeout(Duration::from_secs(5), self.requests_started.acquire())
            .await
            .expect("workflow made a host request in time")
            .expect("request semaphore stays open");
        permit.forget();
    }

    fn request_count(&self) -> usize {
        self.request_count.load(Ordering::SeqCst)
    }

    fn disposed_threads(&self) -> Vec<ThreadId> {
        self.disposed_threads
            .lock()
            .expect("disposed threads lock")
            .clone()
    }

    fn calls(&self) -> Vec<(HostRequestMethod, Value)> {
        self.calls.lock().expect("calls lock").clone()
    }

    fn notifications(&self) -> Vec<(ThreadId, WorkflowRun)> {
        self.notifications
            .lock()
            .expect("notifications lock")
            .clone()
    }

    async fn wait_for_notifications(&self, count: usize) -> Vec<(ThreadId, WorkflowRun)> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let notifications = self.notifications();
                if notifications.len() >= count {
                    return notifications;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("workflow terminal notification arrived")
    }
}

#[async_trait]
impl WorkflowRuntimeHost for CoordinatorHost {
    async fn chat_folder(&self, _thread: ThreadId) -> Option<PathBuf> {
        self.chat_folder.get().cloned()
    }

    async fn create_run_thread(
        &self,
        parent: Option<ThreadId>,
        title: &str,
    ) -> Result<ThreadId, HostError> {
        let thread = self
            .store
            .create_thread(CreateThreadRequest {
                title: Some(title.to_owned()),
                parent_id: parent,
                ..CreateThreadRequest::default()
            })
            .await
            .map_err(|error| HostError::new(error.to_string()))?;
        Ok(thread.id)
    }

    async fn request(
        &self,
        thread_id: ThreadId,
        call: HostCall,
    ) -> Result<Option<Value>, HostError> {
        self.calls
            .lock()
            .expect("calls lock")
            .push((call.method, call.params.clone()));
        let request = self.request_count.fetch_add(1, Ordering::SeqCst) + 1;
        self.requests_started.add_permits(1);
        if request <= self.block_requests {
            call.cancellation.cancelled().await;
            return Err(HostError::new("host request cancelled"));
        }
        Ok(Some(json!({
            "text": "host-ok",
            "data": {"request": request},
            "continuation": format!("continuation-{request}"),
            "threadID": thread_id,
        })))
    }

    async fn resolve_agent_model(
        &self,
        _launch_thread_id: ThreadId,
        requested: &str,
    ) -> Result<WorkflowModelResolution, HostError> {
        if requested == "missing/unavailable" {
            return Ok(WorkflowModelResolution {
                resolved: "fixture/fallback".into(),
                unavailable_requested: Some(requested.into()),
            });
        }
        Ok(WorkflowModelResolution {
            resolved: requested.into(),
            unavailable_requested: None,
        })
    }

    async fn workflow_agent_defaults(
        &self,
        _launch_thread_id: ThreadId,
    ) -> Result<WorkflowAgentDefaults, HostError> {
        Ok(WorkflowAgentDefaults {
            agent: Some("fixture-agent".into()),
            model: Some("fixture/default".into()),
            variant: Some("ultracode".into()),
        })
    }

    async fn notify_terminal(
        &self,
        launch_thread_id: ThreadId,
        run: WorkflowRun,
    ) -> Result<(), HostError> {
        if let Some(coordinator) = self.coordinator.get().and_then(std::sync::Weak::upgrade) {
            self.pending_while_notified
                .lock()
                .expect("pending lock")
                .push(coordinator.pending_deliveries(launch_thread_id));
        }
        self.notifications
            .lock()
            .expect("notifications lock")
            .push((launch_thread_id, run));
        Ok(())
    }

    async fn dispose_run_thread(&self, thread_id: ThreadId) -> Result<(), HostError> {
        self.disposed_threads
            .lock()
            .expect("disposed threads lock")
            .push(thread_id);
        Ok(())
    }
}

fn portable_coordinator(
    bun: PathBuf,
    store: Arc<SqliteStore>,
    host: Arc<CoordinatorHost>,
    workspace: PathBuf,
) -> Arc<WorkflowCoordinator> {
    Arc::new(WorkflowCoordinator::new(
        store,
        Arc::new(haarness_workflow_catalog::WorkflowCatalog::new(Vec::new())),
        WorkflowExecutor::new(ExecutorConfig {
            bun_path: Some(bun),
            execution_timeout: Some(Duration::from_secs(10)),
            ..ExecutorConfig::default()
        }),
        host,
        EventBus::new(128),
        workspace,
        false,
    ))
}

fn portable_start(source: &str) -> WorkflowStartRequest {
    WorkflowStartRequest {
        source: Some(source.to_owned()),
        args: Some(json!({"value": 7})),
        ..WorkflowStartRequest::default()
    }
}

const COMPLETING_PORTABLE_WORKFLOW: &str = r#"
export const meta = {
  name: "coordinator-complete",
  phases: ["prepare"],
}
phase("prepare")
log("portable lifecycle log")
const reply = await agent("perform work")
const followup = await reply.followup({ prompt: "verify work" })
await reply.close()
return { input: args.value, reply: reply.text, followup: followup.text }
"#;

const BLOCKING_PORTABLE_WORKFLOW: &str = r#"
export const meta = {
  name: "coordinator-blocking",
  phases: ["waiting"],
}
phase("waiting")
const reply = await agent("wait for cancellation")
log("blocking run completed")
return { reply: reply.text }
"#;

const FAILING_PORTABLE_WORKFLOW: &str = r#"
export const meta = { name: "coordinator-failure" }
throw new Error("fixture workflow failed")
"#;

#[tokio::test]
async fn session_cancellation_stops_only_matching_workflows_without_reinjection() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping coordinator fixture: Bun is unavailable");
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let store = Arc::new(SqliteStore::in_memory().expect("workflow store"));
    let first = store
        .create_thread(CreateThreadRequest::default())
        .await
        .expect("first launch thread");
    let second = store
        .create_thread(CreateThreadRequest::default())
        .await
        .expect("second launch thread");
    let host = Arc::new(CoordinatorHost::new(Arc::clone(&store), 2));
    let coordinator =
        portable_coordinator(bun, store, host.clone(), workspace.path().to_path_buf());

    let mut first_request = portable_start(BLOCKING_PORTABLE_WORKFLOW);
    first_request.thread_id = Some(first.id);
    let first_run = coordinator
        .start(first_request)
        .await
        .expect("first run starts");
    let mut second_request = portable_start(BLOCKING_PORTABLE_WORKFLOW);
    second_request.thread_id = Some(second.id);
    let second_run = coordinator
        .start(second_request)
        .await
        .expect("second run starts");
    host.wait_for_request().await;
    host.wait_for_request().await;

    assert_eq!(
        coordinator
            .cancel_for_launch_threads(&HashSet::from([first.id]))
            .await
            .expect("session workflows cancel"),
        1
    );
    assert_eq!(
        coordinator.get(first_run.id, None).await.unwrap().status,
        WorkflowRunStatus::Cancelled
    );
    assert_eq!(
        coordinator.get(second_run.id, None).await.unwrap().status,
        WorkflowRunStatus::Running
    );
    assert!(
        host.notifications().is_empty(),
        "session cancellation must not start a terminal-delivery continuation"
    );

    coordinator
        .cancel(second_run.id)
        .await
        .expect("fixture cleanup");
}

#[tokio::test]
async fn workflow_wide_agent_defaults_fallback_and_warning_are_durable() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping coordinator fixture: Bun is unavailable");
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let store = Arc::new(SqliteStore::in_memory().expect("workflow store"));
    let host = Arc::new(CoordinatorHost::new(Arc::clone(&store), 0));
    let coordinator =
        portable_coordinator(bun, store, host.clone(), workspace.path().to_path_buf());
    let completed = coordinator
        .start(WorkflowStartRequest {
            source: Some(
                r#"
export const meta = { name: "workflow-defaults" }
const reply = await agent("use workflow defaults")
return reply.text
"#
                .into(),
            ),
            agent: Some("review".into()),
            model: Some("missing/unavailable".into()),
            variant: Some("high".into()),
            wait: true,
            ..WorkflowStartRequest::default()
        })
        .await
        .expect("workflow completes with fallback model");

    assert_eq!(completed.status, WorkflowRunStatus::Completed);
    assert_eq!(completed.definition.agent.as_deref(), Some("review"));
    assert_eq!(
        completed.definition.model.as_deref(),
        Some("missing/unavailable")
    );
    assert_eq!(completed.definition.variant.as_deref(), Some("high"));
    let agent_call = host
        .calls()
        .into_iter()
        .find(|(method, _)| *method == HostRequestMethod::Agent)
        .expect("agent host call");
    assert_eq!(agent_call.1["agent"], "review");
    assert_eq!(agent_call.1["model"], "fixture/fallback");
    assert_eq!(agent_call.1["variant"], "high");
    assert_eq!(
        completed.agents[0].model.as_deref(),
        Some("fixture/fallback")
    );
    assert_eq!(completed.warnings.len(), 1);
    assert_eq!(
        completed.warnings[0].requested_model.as_deref(),
        Some("missing/unavailable")
    );
    assert_eq!(
        completed.warnings[0].resolved_model.as_deref(),
        Some("fixture/fallback")
    );
}

#[tokio::test]
async fn coordinator_uses_explicit_phase_and_nested_definition_agent_defaults() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping coordinator fixture: Bun is unavailable");
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let workflow_root = workspace.path().join(".haarness/workflows");
    fs::create_dir_all(&workflow_root).expect("workflow root");
    fs::write(
        workflow_root.join("child.js"),
        r#"
export const meta = {
  name: "child",
  phases: [{ title: "nested", model: "nested/review", variant: "nested-high" }],
}
phase("nested")
const reply = await agent({ prompt: "nested" })
return reply.text
"#,
    )
    .expect("nested portable workflow");
    let store = Arc::new(SqliteStore::in_memory().expect("workflow store"));
    let host = Arc::new(CoordinatorHost::new(Arc::clone(&store), 0));
    let coordinator = Arc::new(WorkflowCoordinator::new(
        store,
        Arc::new(haarness_workflow_catalog::WorkflowCatalog::for_workspace(
            workspace.path(),
        )),
        WorkflowExecutor::new(ExecutorConfig {
            bun_path: Some(bun),
            execution_timeout: Some(Duration::from_secs(10)),
            ..ExecutorConfig::default()
        }),
        host.clone(),
        EventBus::new(128),
        workspace.path().to_path_buf(),
        false,
    ));
    let completed = coordinator
        .start(WorkflowStartRequest {
            source: Some(
                r#"
export const meta = {
  name: "root",
  phases: [
    { title: "current", model: "root/current", variant: "root-low" },
    { title: "review", model: "missing/unavailable", variant: "root-high" },
  ],
}
phase("current")
const direct = await agent({ prompt: "direct", phase: "review" })
const nested = await workflow("child")
return { direct: direct.text, nested }
"#
                .into(),
            ),
            wait: true,
            ..WorkflowStartRequest::default()
        })
        .await
        .expect("root and nested workflows complete");

    assert_eq!(completed.status, WorkflowRunStatus::Completed);
    let calls = host
        .calls()
        .into_iter()
        .filter(|(method, _)| *method == HostRequestMethod::Agent)
        .map(|(_, input)| input)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["phase"], "review");
    assert_eq!(calls[0]["model"], "fixture/fallback");
    assert_eq!(calls[0]["variant"], "root-high");
    assert!(calls[1].get("phase").is_none());
    assert_eq!(calls[1]["model"], "nested/review");
    assert_eq!(calls[1]["variant"], "nested-high");
    assert_eq!(
        completed
            .agents
            .iter()
            .map(|agent| (
                agent.phase.as_deref(),
                agent.model.as_deref(),
                agent.variant.as_deref(),
            ))
            .collect::<Vec<_>>(),
        [
            (Some("review"), Some("fixture/fallback"), Some("root-high")),
            (Some("nested"), Some("nested/review"), Some("nested-high"),),
        ]
    );
    assert!(
        completed
            .warnings
            .iter()
            .all(|warning| !warning.message.contains("not declared")),
        "nested phases are validated against the nested definition"
    );
    assert_eq!(completed.warnings.len(), 1);
    assert_eq!(completed.warnings[0].phase.as_deref(), Some("review"));
}

#[tokio::test]
async fn coordinator_start_wait_persists_phase_log_and_result() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping coordinator fixture: Bun is unavailable");
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let store = Arc::new(SqliteStore::in_memory().expect("workflow store"));
    let host = Arc::new(CoordinatorHost::new(Arc::clone(&store), 0));
    let coordinator =
        portable_coordinator(bun, store, host.clone(), workspace.path().to_path_buf());

    let started = coordinator
        .start(portable_start(COMPLETING_PORTABLE_WORKFLOW))
        .await
        .expect("workflow starts");
    let completed = coordinator
        .wait(started.id, Some(Duration::from_secs(5)))
        .await
        .expect("workflow completes");

    assert_eq!(
        completed.status,
        WorkflowRunStatus::Completed,
        "workflow failed: {:?}; logs: {:?}",
        completed.error,
        completed.logs
    );
    assert_eq!(completed.phase.as_deref(), Some("prepare"));
    assert_eq!(
        completed.result,
        Some(json!({"input": 7, "reply": "host-ok", "followup": "host-ok"}))
    );
    assert_eq!(host.request_count(), 3);
    assert_eq!(completed.agents.len(), 1);
    assert_eq!(
        completed.agents[0].status,
        haarness_protocol::WorkflowRunAgentStatus::Completed
    );
    assert_eq!(completed.agents[0].output.as_deref(), Some("host-ok"));
    assert_eq!(completed.agents[0].thread_id, completed.thread_id);
    assert_eq!(completed.agents[0].agent.as_deref(), Some("fixture-agent"));
    assert_eq!(
        completed.agents[0].model.as_deref(),
        Some("fixture/default")
    );
    assert_eq!(completed.agents[0].variant.as_deref(), Some("ultracode"));
    assert_eq!(host.disposed_threads(), vec![completed.thread_id.unwrap()]);
    assert!(
        completed
            .logs
            .iter()
            .any(|entry| entry.message.contains("follow-up started"))
    );
    assert!(completed.logs.iter().any(|entry| {
        entry.message == "portable lifecycle log"
            && entry.source == Some(haarness_protocol::WorkflowLogSource::Workflow)
    }));
    assert!(
        completed
            .logs
            .iter()
            .any(|entry| entry.message == "Phase: prepare")
    );
    assert!(
        completed
            .logs
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
}

#[tokio::test]
async fn coordinator_lists_runs_by_launch_conversation() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping coordinator fixture: Bun is unavailable");
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let store = Arc::new(SqliteStore::in_memory().expect("workflow store"));
    let launch = store
        .create_thread(CreateThreadRequest {
            title: Some("launch".into()),
            ..CreateThreadRequest::default()
        })
        .await
        .expect("launch thread");
    let host = Arc::new(CoordinatorHost::new(Arc::clone(&store), 0));
    let coordinator = portable_coordinator(bun, store, host, workspace.path().to_path_buf());
    let completed = coordinator
        .start(WorkflowStartRequest {
            source: Some("export const meta = { name: 'scoped' }; return 'ok'".into()),
            thread_id: Some(launch.id),
            wait: true,
            ..WorkflowStartRequest::default()
        })
        .await
        .expect("scoped workflow completes");

    assert_eq!(
        coordinator
            .list(Some(launch.id), 10)
            .await
            .expect("launch-scoped runs")
            .iter()
            .map(|run| run.id)
            .collect::<Vec<_>>(),
        [completed.id]
    );
    assert!(
        coordinator
            .list(completed.thread_id, 10)
            .await
            .expect("execution-thread lookup")
            .is_empty(),
        "new runs are scoped to their launching conversation"
    );
}

#[tokio::test]
async fn coordinator_lifecycle_rejects_run_ids_owned_by_another_workspace() {
    let owner_workspace = tempfile::tempdir().expect("owner workspace");
    let foreign_workspace = tempfile::tempdir().expect("foreign workspace");
    let store = Arc::new(SqliteStore::in_memory().expect("workflow store"));
    let host = Arc::new(CoordinatorHost::new(Arc::clone(&store), 0));
    let missing_bun = PathBuf::from("/haarness-test/missing-bun");
    let owner = portable_coordinator(
        missing_bun.clone(),
        store.clone(),
        Arc::clone(&host),
        owner_workspace.path().to_path_buf(),
    );
    let foreign = portable_coordinator(
        missing_bun,
        store,
        host,
        foreign_workspace.path().to_path_buf(),
    );
    let mut zero_timeout =
        portable_start("export const meta = { name: 'invalid-timeout' }; return { ok: true }");
    zero_timeout.timeout_ms = Some(0);
    assert!(matches!(
        owner.start(zero_timeout).await,
        Err(CoreError::Invalid(_))
    ));
    let started = owner
        .start(portable_start(
            "export const meta = { name: 'owned' }; return { ok: true }",
        ))
        .await
        .expect("owner starts workflow");

    assert!(matches!(
        foreign.get(started.id, None).await,
        Err(CoreError::NotFound(_))
    ));
    assert!(matches!(
        foreign.cancel(started.id).await,
        Err(CoreError::NotFound(_))
    ));
    assert!(matches!(
        foreign.delete(started.id).await,
        Err(CoreError::NotFound(_))
    ));

    let _ = owner.wait(started.id, Some(Duration::from_secs(2))).await;
    owner.delete(started.id).await.expect("owner deletes run");
}

#[tokio::test]
async fn coordinator_runs_discovered_portable_project_source_without_native_trust() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping coordinator fixture: Bun is unavailable");
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let workflow_root = workspace.path().join(".haarness/workflows");
    fs::create_dir_all(&workflow_root).expect("workflow root");
    fs::write(
        workflow_root.join("project.js"),
        "export const meta = { name: 'project-portable' }\nreturn args.value\n",
    )
    .expect("portable source");
    let store = Arc::new(SqliteStore::in_memory().expect("workflow store"));
    let host = Arc::new(CoordinatorHost::new(Arc::clone(&store), 0));
    let coordinator = Arc::new(WorkflowCoordinator::new(
        store,
        Arc::new(haarness_workflow_catalog::WorkflowCatalog::for_workspace(
            workspace.path(),
        )),
        WorkflowExecutor::new(ExecutorConfig {
            bun_path: Some(bun),
            execution_timeout: Some(Duration::from_secs(10)),
            ..ExecutorConfig::default()
        }),
        host,
        EventBus::new(128),
        workspace.path().to_path_buf(),
        false,
    ));

    let completed = coordinator
        .start(WorkflowStartRequest {
            name: Some("project-portable".into()),
            args: Some(json!({"value":"ok"})),
            wait: true,
            ..WorkflowStartRequest::default()
        })
        .await
        .expect("restricted project workflow runs without native trust");
    assert_eq!(completed.status, WorkflowRunStatus::Completed);
    assert_eq!(completed.result, Some(json!("ok")));
}

#[tokio::test]
async fn a_chat_folder_s_native_workflow_needs_project_configuration_as_inline_source_does() {
    let home = tempfile::tempdir().expect("chat home fixture");
    let folder = home.path().join("2026-10-24/chat-ab12");
    let workflows = folder.join(".haarness/workflows");
    fs::create_dir_all(&workflows).expect("chat workflows");
    fs::write(
        workflows.join("nat.ts"),
        r#"export default workflow({ name: "nat", async run() { return "no" } })"#,
    )
    .expect("native chat workflow");
    let store = Arc::new(SqliteStore::in_memory().expect("workflow store"));
    let host = Arc::new(CoordinatorHost::new(Arc::clone(&store), 0));
    host.chat_folder.set(folder.clone()).expect("chat folder");
    let coordinator = |trusted| {
        Arc::new(WorkflowCoordinator::new(
            Arc::clone(&store) as Arc<dyn WorkflowRepository>,
            Arc::new(
                haarness_workflow_catalog::WorkflowCatalog::for_workspace(home.path())
                    .with_project_config(trusted),
            ),
            WorkflowExecutor::new(ExecutorConfig {
                bun_path: Some(std::env::current_exe().expect("test executable")),
                ..ExecutorConfig::default()
            }),
            Arc::clone(&host) as Arc<dyn WorkflowRuntimeHost>,
            EventBus::new(128),
            home.path().to_path_buf(),
            trusted,
        ))
    };
    let chat = ThreadId::new();
    for request in [
        WorkflowStartRequest {
            name: Some("nat".into()),
            thread_id: Some(chat),
            ..WorkflowStartRequest::default()
        },
        WorkflowStartRequest {
            path: Some(workflows.join("nat.ts").display().to_string()),
            thread_id: Some(chat),
            ..WorkflowStartRequest::default()
        },
    ] {
        let error = coordinator(false)
            .start(request)
            .await
            .expect_err("a native chat workflow stays gated without project configuration");
        assert!(
            matches!(&error, CoreError::PermissionDenied(message)
                if message.contains("this chat's folder") && message.contains("project configuration")),
            "{error}"
        );
    }
    // Found for that chat only.
    assert!(
        coordinator(false)
            .start(WorkflowStartRequest {
                name: Some("nat".into()),
                ..WorkflowStartRequest::default()
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn ancestor_project_native_workflow_still_requires_project_trust() {
    let repository = tempfile::tempdir().expect("repository fixture");
    fs::create_dir_all(repository.path().join(".git")).expect("worktree marker");
    let workflow_root = repository.path().join(".haarness/workflows");
    fs::create_dir_all(&workflow_root).expect("workflow root");
    fs::write(
        workflow_root.join("parent-native.ts"),
        r#"export default workflow({ name: "parent-native", async run() { return "no" } })"#,
    )
    .expect("ancestor native workflow");
    let workspace = repository.path().join("crates/child");
    fs::create_dir_all(&workspace).expect("nested workspace");
    let store = Arc::new(SqliteStore::in_memory().expect("workflow store"));
    let host = Arc::new(CoordinatorHost::new(Arc::clone(&store), 0));
    let coordinator = Arc::new(WorkflowCoordinator::new(
        store,
        Arc::new(haarness_workflow_catalog::WorkflowCatalog::for_workspace(
            &workspace,
        )),
        WorkflowExecutor::new(ExecutorConfig {
            bun_path: Some(std::env::current_exe().expect("test executable")),
            ..ExecutorConfig::default()
        }),
        host,
        EventBus::new(128),
        workspace,
        false,
    ));

    let error = coordinator
        .start(WorkflowStartRequest {
            name: Some("parent-native".into()),
            ..WorkflowStartRequest::default()
        })
        .await
        .expect_err("ancestor project native source remains gated");
    assert!(
        matches!(error, CoreError::PermissionDenied(message) if message.contains("project configuration"))
    );
}

#[tokio::test]
async fn coordinator_pause_and_resume_restart_the_portable_source() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping coordinator fixture: Bun is unavailable");
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let store = Arc::new(SqliteStore::in_memory().expect("workflow store"));
    // Only the first execution blocks. Resume must rerun the source from the
    // beginning and its second host call completes immediately.
    let host = Arc::new(CoordinatorHost::new(Arc::clone(&store), 1));
    let coordinator =
        portable_coordinator(bun, store, host.clone(), workspace.path().to_path_buf());
    let started = coordinator
        .start(portable_start(BLOCKING_PORTABLE_WORKFLOW))
        .await
        .expect("workflow starts");
    host.wait_for_request().await;

    let paused = coordinator
        .pause(started.id)
        .await
        .expect("workflow pauses");
    assert_eq!(paused.status, WorkflowRunStatus::Paused);
    assert!(paused.time.completed.is_none());
    assert_eq!(
        coordinator
            .wait(started.id, Some(Duration::from_secs(1)))
            .await
            .expect("wait observes pause")
            .status,
        WorkflowRunStatus::Paused
    );

    // pause() does not return until the cancelled executor is reaped, so an
    // immediate resume cannot race the old execution's terminal write.
    coordinator
        .resume(started.id)
        .await
        .expect("paused workflow resumes");
    let completed = coordinator
        .wait(started.id, Some(Duration::from_secs(5)))
        .await
        .expect("resumed workflow completes");
    assert_eq!(completed.status, WorkflowRunStatus::Completed);
    assert_eq!(host.request_count(), 2);
    for message in [
        "Workflow paused",
        "Workflow resumed from the beginning",
        "blocking run completed",
    ] {
        assert!(
            completed.logs.iter().any(|entry| entry.message == message),
            "missing lifecycle log {message:?}: {:?}",
            completed.logs
        );
    }
}

#[tokio::test]
async fn coordinator_resume_rechecks_native_project_trust() {
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let source_path = workspace.path().join(".haarness/workflows/native.ts");
    fs::create_dir_all(source_path.parent().expect("workflow root")).expect("workflow root");
    let source = r#"export default workflow({ name: "native", async run() { return "ok" } })"#;
    fs::write(&source_path, source).expect("native source");
    let definition = haarness_workflow_catalog::WorkflowCatalog::new(Vec::new())
        .inspect_source(source, source_path.display().to_string())
        .expect("native definition");
    assert_eq!(definition.dialect, ProtocolDialect::Native);

    let store = Arc::new(SqliteStore::in_memory().expect("workflow store"));
    let now = Utc::now();
    let created = store
        .create_workflow_run(WorkflowRun {
            id: WorkflowId::new(),
            thread_id: None,
            directory: workspace.path().display().to_string(),
            workflow_name: definition.invocation.clone(),
            source_path: Some(source_path.display().to_string()),
            source_hash: String::new(),
            args: Some(json!({})),
            status: WorkflowRunStatus::Paused,
            phase: None,
            status_payload: None,
            logs: Vec::new(),
            warnings: Vec::new(),
            agents: Vec::new(),
            definition: WorkflowRunDefinition {
                name: definition.name,
                invocation: definition.invocation,
                launch_thread_id: None,
                plugin_name: None,
                agent: None,
                model: None,
                variant: None,
                dialect: ProtocolDialect::Native,
                meta: WorkflowMeta {
                    name: definition.meta.name,
                    description: definition.meta.description,
                    when_to_use: definition.meta.when_to_use,
                    phases: definition.meta.phases,
                    arguments: definition.meta.arguments,
                },
                source: Some(source.to_owned()),
                source_path: Some(source_path.display().to_string()),
                args_provided: Some(true),
            },
            result: None,
            error: Some("Workflow paused".into()),
            time: WorkflowRunTime {
                created: now,
                updated: now,
                started: Some(now),
                completed: None,
            },
        })
        .await
        .expect("paused run is durable");
    let host = Arc::new(CoordinatorHost::new(Arc::clone(&store), 0));
    let coordinator = Arc::new(WorkflowCoordinator::new(
        store.clone(),
        Arc::new(
            haarness_workflow_catalog::WorkflowCatalog::for_workspace(workspace.path())
                .with_project_config(false),
        ),
        WorkflowExecutor::new(ExecutorConfig {
            bun_path: Some(std::env::current_exe().expect("test executable")),
            ..ExecutorConfig::default()
        }),
        host,
        EventBus::new(128),
        workspace.path().to_path_buf(),
        false,
    ));

    let error = coordinator
        .resume(created.id)
        .await
        .expect_err("untrusted restart cannot resume native project code");
    assert!(
        error.to_string().contains("project configuration"),
        "unexpected error: {error}"
    );
    assert_eq!(
        store
            .workflow_run(created.id)
            .await
            .expect("run remains durable")
            .status,
        WorkflowRunStatus::Paused
    );
}

#[tokio::test]
async fn coordinator_portable_resume_uses_snapshot_after_installed_source_changes_or_disappears() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping coordinator fixture: Bun is unavailable");
        return;
    };
    for remove_installed in [false, true] {
        let workspace = tempfile::tempdir().expect("workspace fixture");
        let workflow_root = workspace.path().join(".haarness/workflows");
        fs::create_dir_all(&workflow_root).expect("workflow root");
        let source_path = workflow_root.join("snapshot.js");
        let snapshot = r#"
export const meta = { name: "snapshot-resume" }
const reply = await agent("wait before resume")
return { source: "snapshot", request: reply.data.request }
"#;
        fs::write(&source_path, snapshot).expect("portable source");
        let store = Arc::new(SqliteStore::in_memory().expect("workflow store"));
        let host = Arc::new(CoordinatorHost::new(Arc::clone(&store), 1));
        let coordinator = Arc::new(WorkflowCoordinator::new(
            store,
            Arc::new(haarness_workflow_catalog::WorkflowCatalog::for_workspace(
                workspace.path(),
            )),
            WorkflowExecutor::new(ExecutorConfig {
                bun_path: Some(bun.clone()),
                execution_timeout: Some(Duration::from_secs(10)),
                ..ExecutorConfig::default()
            }),
            host.clone(),
            EventBus::new(128),
            workspace.path().to_path_buf(),
            false,
        ));
        let started = coordinator
            .start(WorkflowStartRequest {
                name: Some("snapshot-resume".into()),
                ..WorkflowStartRequest::default()
            })
            .await
            .expect("installed portable workflow starts");
        host.wait_for_request().await;
        coordinator
            .pause(started.id)
            .await
            .expect("workflow pauses");

        if remove_installed {
            fs::remove_file(&source_path).expect("installed source removed");
        } else {
            fs::write(
                &source_path,
                r#"export const meta = { name: "snapshot-resume" }
return { source: "mutated" }
"#,
            )
            .expect("installed source changed");
        }

        coordinator
            .resume(started.id)
            .await
            .expect("portable snapshot resumes independently of installed entry");
        let completed = coordinator
            .wait(started.id, Some(Duration::from_secs(5)))
            .await
            .expect("snapshot run completes");
        assert_eq!(completed.status, WorkflowRunStatus::Completed);
        assert_eq!(
            completed.result,
            Some(json!({"source":"snapshot","request":2})),
            "resume must use the immutable source snapshot (removed={remove_installed})"
        );
        assert_eq!(
            completed.definition.source.as_deref(),
            Some(snapshot),
            "durable source remains unchanged"
        );
    }
}

#[tokio::test]
async fn coordinator_lifecycle_retries_are_idempotent() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping coordinator fixture: Bun is unavailable");
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let store = Arc::new(SqliteStore::in_memory().expect("workflow store"));
    let host = Arc::new(CoordinatorHost::new(Arc::clone(&store), usize::MAX));
    let coordinator =
        portable_coordinator(bun, store, host.clone(), workspace.path().to_path_buf());
    let started = coordinator
        .start(portable_start(BLOCKING_PORTABLE_WORKFLOW))
        .await
        .expect("workflow starts");
    host.wait_for_request().await;

    let already_running = coordinator
        .resume(started.id)
        .await
        .expect("resuming a running workflow is idempotent");
    assert_eq!(already_running.status, WorkflowRunStatus::Running);
    assert!(
        !already_running
            .logs
            .iter()
            .any(|entry| entry.message == "Workflow resumed from the beginning")
    );

    let paused = coordinator
        .pause(started.id)
        .await
        .expect("workflow pauses");
    let paused_again = coordinator
        .pause(started.id)
        .await
        .expect("pausing a paused workflow is idempotent");
    assert_eq!(paused_again.status, WorkflowRunStatus::Paused);
    assert_eq!(
        paused_again
            .logs
            .iter()
            .filter(|entry| entry.message == "Workflow paused")
            .count(),
        1
    );
    assert_eq!(paused_again.time.updated, paused.time.updated);

    coordinator
        .resume(started.id)
        .await
        .expect("paused workflow resumes");
    host.wait_for_request().await;
    let running_again = coordinator
        .resume(started.id)
        .await
        .expect("resuming a running workflow remains idempotent");
    assert_eq!(running_again.status, WorkflowRunStatus::Running);
    assert_eq!(
        running_again
            .logs
            .iter()
            .filter(|entry| entry.message == "Workflow resumed from the beginning")
            .count(),
        1
    );

    let cancelled = coordinator
        .cancel(started.id)
        .await
        .expect("workflow cancels");
    let cancelled_again = coordinator
        .cancel(started.id)
        .await
        .expect("cancelling a cancelled workflow is idempotent");
    assert_eq!(cancelled_again.status, WorkflowRunStatus::Cancelled);
    assert_eq!(
        cancelled_again
            .logs
            .iter()
            .filter(|entry| entry.message == "Workflow cancelled")
            .count(),
        1
    );
    assert_eq!(cancelled_again.time.updated, cancelled.time.updated);
    assert!(matches!(
        coordinator.pause(started.id).await,
        Err(CoreError::Conflict(_))
    ));
    assert!(matches!(
        coordinator.resume(started.id).await,
        Err(CoreError::Conflict(_))
    ));
}

#[tokio::test]
async fn coordinator_cancel_is_terminal_and_delete_removes_the_run() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping coordinator fixture: Bun is unavailable");
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let store = Arc::new(SqliteStore::in_memory().expect("workflow store"));
    let host = Arc::new(CoordinatorHost::new(Arc::clone(&store), usize::MAX));
    let coordinator =
        portable_coordinator(bun, store, host.clone(), workspace.path().to_path_buf());
    let started = coordinator
        .start(portable_start(BLOCKING_PORTABLE_WORKFLOW))
        .await
        .expect("workflow starts");
    host.wait_for_request().await;

    let cancelled = coordinator
        .cancel(started.id)
        .await
        .expect("workflow cancels");
    assert_eq!(cancelled.status, WorkflowRunStatus::Cancelled);
    assert_eq!(cancelled.error.as_deref(), Some("Workflow cancelled"));
    assert!(cancelled.time.completed.is_some());
    assert!(
        cancelled
            .logs
            .iter()
            .any(|entry| entry.message == "Workflow cancelled")
    );
    assert!(
        coordinator
            .delete(started.id)
            .await
            .expect("delete succeeds")
    );
    assert!(coordinator.get(started.id, None).await.is_err());
    assert!(
        !coordinator
            .delete(started.id)
            .await
            .expect("delete is idempotent")
    );
}

#[tokio::test]
async fn background_completion_and_failure_notify_only_the_launch_thread() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping coordinator fixture: Bun is unavailable");
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let store = Arc::new(SqliteStore::in_memory().expect("workflow store"));
    let launch = store
        .create_thread(CreateThreadRequest::default())
        .await
        .expect("launch thread");
    let host = Arc::new(CoordinatorHost::new(Arc::clone(&store), 0));
    let coordinator = portable_coordinator(
        bun,
        Arc::clone(&store),
        host.clone(),
        workspace.path().to_path_buf(),
    );

    let mut completing = portable_start(COMPLETING_PORTABLE_WORKFLOW);
    completing.thread_id = Some(launch.id);
    let started = coordinator
        .start(completing)
        .await
        .expect("background workflow starts");
    let completed = coordinator
        .wait(started.id, Some(Duration::from_secs(5)))
        .await
        .expect("workflow completes");
    assert_eq!(completed.status, WorkflowRunStatus::Completed);
    let notifications = host.wait_for_notifications(1).await;
    assert_eq!(notifications[0].0, launch.id);
    assert_eq!(notifications[0].1.id, completed.id);
    assert_eq!(notifications[0].1.status, WorkflowRunStatus::Completed);

    let mut failing = portable_start(FAILING_PORTABLE_WORKFLOW);
    failing.thread_id = Some(launch.id);
    let failed = coordinator
        .start(failing)
        .await
        .expect("failing background workflow starts");
    let failed = coordinator
        .wait(failed.id, Some(Duration::from_secs(5)))
        .await
        .expect("failed workflow reaches a terminal state");
    assert_eq!(failed.status, WorkflowRunStatus::Failed);
    let notifications = host.wait_for_notifications(2).await;
    assert_eq!(notifications[1].0, launch.id);
    assert_eq!(notifications[1].1.id, failed.id);
    assert_eq!(notifications[1].1.status, WorkflowRunStatus::Failed);

    let mut synchronous = portable_start(COMPLETING_PORTABLE_WORKFLOW);
    synchronous.thread_id = Some(launch.id);
    synchronous.wait = true;
    assert_eq!(
        coordinator
            .start(synchronous)
            .await
            .expect("synchronous workflow completes")
            .status,
        WorkflowRunStatus::Completed
    );
    tokio::task::yield_now().await;
    assert_eq!(
        host.notifications().len(),
        2,
        "a synchronous start already consumed its terminal result"
    );
}

#[tokio::test]
async fn a_background_run_is_pending_for_its_launcher_until_its_result_is_handed_over() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping coordinator fixture: Bun is unavailable");
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let store = Arc::new(SqliteStore::in_memory().expect("workflow store"));
    let launch = store
        .create_thread(CreateThreadRequest::default())
        .await
        .expect("launch thread");
    let other = store
        .create_thread(CreateThreadRequest::default())
        .await
        .expect("other thread");
    let host = Arc::new(CoordinatorHost::new(Arc::clone(&store), usize::MAX));
    let coordinator =
        portable_coordinator(bun, store, host.clone(), workspace.path().to_path_buf());
    host.coordinator
        .set(Arc::downgrade(&coordinator))
        .expect("set once");

    let mut background = portable_start(BLOCKING_PORTABLE_WORKFLOW);
    background.thread_id = Some(launch.id);
    let run = coordinator
        .start(background)
        .await
        .expect("background workflow starts");
    host.wait_for_request().await;
    // A run without a launching thread delivers nowhere.
    let detached = coordinator
        .start(portable_start(BLOCKING_PORTABLE_WORKFLOW))
        .await
        .expect("detached workflow starts");
    host.wait_for_request().await;
    assert_eq!(coordinator.pending_deliveries(launch.id), 1);
    assert_eq!(coordinator.pending_deliveries(other.id), 0);

    coordinator.cancel(run.id).await.expect("workflow cancels");
    let notifications = host.wait_for_notifications(1).await;
    assert_eq!(notifications[0].0, launch.id);
    assert_eq!(
        *host.pending_while_notified.lock().expect("pending lock"),
        [1],
        "still pending while its result is handed over"
    );
    assert_eq!(coordinator.pending_deliveries(launch.id), 0);

    // A run cancelled with its session never reports back, so it is owed
    // nothing from the moment it is cancelled.
    let mut stopped = portable_start(BLOCKING_PORTABLE_WORKFLOW);
    stopped.thread_id = Some(launch.id);
    coordinator
        .start(stopped)
        .await
        .expect("background workflow starts");
    host.wait_for_request().await;
    assert_eq!(coordinator.pending_deliveries(launch.id), 1);
    coordinator
        .cancel_for_launch_threads(&HashSet::from([launch.id]))
        .await
        .expect("session workflows cancel");
    assert_eq!(coordinator.pending_deliveries(launch.id), 0);
    tokio::task::yield_now().await;
    assert_eq!(host.notifications().len(), 1, "no delivery after the stop");

    coordinator
        .cancel(detached.id)
        .await
        .expect("fixture cleanup");
}

/// The store, except that writing a log line fails while `fail_logs` is set.
struct FailingLogs {
    store: Arc<SqliteStore>,
    fail_logs: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl WorkflowRepository for FailingLogs {
    async fn create_workflow_run(
        &self,
        run: WorkflowRun,
    ) -> haarness_core::CoreResult<WorkflowRun> {
        self.store.create_workflow_run(run).await
    }
    async fn workflow_run(&self, id: WorkflowId) -> haarness_core::CoreResult<WorkflowRun> {
        self.store.workflow_run(id).await
    }
    async fn list_workflow_runs(
        &self,
        directory: Option<&str>,
        thread_id: Option<ThreadId>,
        limit: usize,
    ) -> haarness_core::CoreResult<Vec<WorkflowRun>> {
        self.store
            .list_workflow_runs(directory, thread_id, limit)
            .await
    }
    async fn save_workflow_run(&self, run: &WorkflowRun) -> haarness_core::CoreResult<WorkflowRun> {
        self.store.save_workflow_run(run).await
    }
    async fn append_workflow_log(
        &self,
        id: WorkflowId,
        entry: haarness_core::WorkflowLogDraft,
    ) -> haarness_core::CoreResult<haarness_protocol::WorkflowLog> {
        if self.fail_logs.load(Ordering::SeqCst) {
            return Err(CoreError::Storage("injected log failure".into()));
        }
        self.store.append_workflow_log(id, entry).await
    }
    async fn delete_workflow_run(&self, id: WorkflowId) -> haarness_core::CoreResult<bool> {
        self.store.delete_workflow_run(id).await
    }
    async fn running_workflow_ids(
        &self,
        directory: &str,
    ) -> haarness_core::CoreResult<Vec<WorkflowId>> {
        self.store.running_workflow_ids(directory).await
    }
    async fn recover_interrupted_workflows(
        &self,
        directory: &str,
        ids: &[WorkflowId],
    ) -> haarness_core::CoreResult<u64> {
        self.store
            .recover_interrupted_workflows(directory, ids)
            .await
    }
}

#[tokio::test]
async fn a_cancel_whose_log_fails_still_releases_its_launcher() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping coordinator fixture: Bun is unavailable");
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let store = Arc::new(SqliteStore::in_memory().expect("workflow store"));
    let launch = store
        .create_thread(CreateThreadRequest::default())
        .await
        .expect("launch thread");
    let host = Arc::new(CoordinatorHost::new(Arc::clone(&store), usize::MAX));
    let repository = Arc::new(FailingLogs {
        store: Arc::clone(&store),
        fail_logs: std::sync::atomic::AtomicBool::new(false),
    });
    let coordinator = Arc::new(WorkflowCoordinator::new(
        repository.clone(),
        Arc::new(haarness_workflow_catalog::WorkflowCatalog::new(Vec::new())),
        WorkflowExecutor::new(ExecutorConfig {
            bun_path: Some(bun),
            execution_timeout: Some(Duration::from_secs(10)),
            ..ExecutorConfig::default()
        }),
        host.clone(),
        EventBus::new(128),
        workspace.path().to_path_buf(),
        false,
    ));
    let mut background = portable_start(BLOCKING_PORTABLE_WORKFLOW);
    background.thread_id = Some(launch.id);
    let run = coordinator
        .start(background)
        .await
        .expect("background workflow starts");
    host.wait_for_request().await;
    assert_eq!(coordinator.pending_deliveries(launch.id), 1);

    repository.fail_logs.store(true, Ordering::SeqCst);
    let error = coordinator.cancel(run.id).await.unwrap_err();
    assert!(
        error.to_string().contains("injected log failure"),
        "{error}"
    );
    // Cancelled all the same: its notice was delivered and nothing is owed.
    assert_eq!(
        coordinator.get(run.id, None).await.unwrap().status,
        WorkflowRunStatus::Cancelled
    );
    let notifications = host.wait_for_notifications(1).await;
    assert_eq!(notifications[0].1.id, run.id);
    assert_eq!(coordinator.pending_deliveries(launch.id), 0);
}

#[tokio::test]
async fn launcher_wait_claims_terminal_and_releases_after_nonterminal_exit() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping coordinator fixture: Bun is unavailable");
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let store = Arc::new(SqliteStore::in_memory().expect("workflow store"));
    let launch = store
        .create_thread(CreateThreadRequest::default())
        .await
        .expect("launch thread");
    let host = Arc::new(CoordinatorHost::new(Arc::clone(&store), usize::MAX));
    let coordinator =
        portable_coordinator(bun, store, host.clone(), workspace.path().to_path_buf());

    let mut consumed_request = portable_start(BLOCKING_PORTABLE_WORKFLOW);
    consumed_request.thread_id = Some(launch.id);
    let consumed = coordinator
        .start(consumed_request)
        .await
        .expect("claimed workflow starts");
    host.wait_for_request().await;
    let timed_snapshot = coordinator
        .wait(consumed.id, Some(Duration::from_millis(1)))
        .await
        .expect("a wait deadline returns the latest workflow snapshot");
    assert_eq!(timed_snapshot.status, WorkflowRunStatus::Running);
    assert!(coordinator.begin_launch_wait(consumed.id, launch.id));
    let cancelled = coordinator
        .cancel(consumed.id)
        .await
        .expect("claimed workflow cancels");
    assert_eq!(cancelled.status, WorkflowRunStatus::Cancelled);
    assert!(host.notifications().is_empty());
    coordinator.end_launch_wait(consumed.id, true, true).await;
    assert!(
        host.notifications().is_empty(),
        "the launcher wait consumed the terminal result"
    );

    let mut released_request = portable_start(BLOCKING_PORTABLE_WORKFLOW);
    released_request.thread_id = Some(launch.id);
    let released = coordinator
        .start(released_request)
        .await
        .expect("released workflow starts");
    assert!(coordinator.begin_launch_wait(released.id, launch.id));
    host.wait_for_request().await;
    coordinator
        .cancel(released.id)
        .await
        .expect("released workflow cancels");
    assert!(host.notifications().is_empty());
    coordinator.end_launch_wait(released.id, true, false).await;
    let notifications = host.wait_for_notifications(1).await;
    assert_eq!(notifications[0].1.id, released.id);
    assert_eq!(notifications[0].1.status, WorkflowRunStatus::Cancelled);

    let mut paused_request = portable_start(BLOCKING_PORTABLE_WORKFLOW);
    paused_request.thread_id = Some(launch.id);
    let paused = coordinator
        .start(paused_request)
        .await
        .expect("pausable workflow starts");
    host.wait_for_request().await;
    assert_eq!(
        coordinator
            .pause(paused.id)
            .await
            .expect("workflow pauses")
            .status,
        WorkflowRunStatus::Paused
    );
    assert_eq!(host.notifications().len(), 1, "pause stays silent");
    assert!(
        coordinator
            .delete(paused.id)
            .await
            .expect("paused run deletes")
    );
    tokio::task::yield_now().await;
    assert_eq!(host.notifications().len(), 1, "delete stays silent");
}

#[tokio::test]
async fn shutdown_interrupts_all_active_workflows_without_terminal_notifications() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping coordinator fixture: Bun is unavailable");
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let store = Arc::new(SqliteStore::in_memory().expect("workflow store"));
    let launch = store
        .create_thread(CreateThreadRequest::default())
        .await
        .expect("launch thread");
    let host = Arc::new(CoordinatorHost::new(Arc::clone(&store), usize::MAX));
    let coordinator =
        portable_coordinator(bun, store, host.clone(), workspace.path().to_path_buf());
    let mut request = portable_start(BLOCKING_PORTABLE_WORKFLOW);
    request.thread_id = Some(launch.id);
    let started = coordinator.start(request).await.expect("workflow starts");
    host.wait_for_request().await;

    assert_eq!(
        coordinator
            .interrupt_active()
            .await
            .expect("shutdown interruption"),
        1
    );
    let interrupted = coordinator
        .wait(started.id, Some(Duration::from_secs(1)))
        .await
        .expect("interrupted workflow is waitable");
    assert_eq!(interrupted.status, WorkflowRunStatus::Interrupted);
    assert!(
        interrupted
            .error
            .as_deref()
            .is_some_and(|error| error.contains("Haarness stopped"))
    );
    assert!(interrupted.time.completed.is_some());
    assert_eq!(
        coordinator
            .interrupt_active()
            .await
            .expect("second shutdown is idempotent"),
        0
    );
    assert!(
        host.notifications().is_empty(),
        "process shutdown is resumable state, not a conversational result"
    );
}

#[tokio::test]
async fn coordinator_recovery_marks_orphan_running_state_without_replay() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping coordinator fixture: Bun is unavailable");
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let store = Arc::new(SqliteStore::in_memory().expect("workflow store"));
    let original_host = Arc::new(CoordinatorHost::new(Arc::clone(&store), usize::MAX));
    let original = portable_coordinator(
        bun.clone(),
        Arc::clone(&store),
        original_host.clone(),
        workspace.path().to_path_buf(),
    );
    let started = original
        .start(portable_start(BLOCKING_PORTABLE_WORKFLOW))
        .await
        .expect("workflow starts");
    original_host.wait_for_request().await;

    let recovered_host = Arc::new(CoordinatorHost::new(Arc::clone(&store), 0));
    let recovered = portable_coordinator(
        bun.clone(),
        Arc::clone(&store),
        recovered_host.clone(),
        workspace.path().to_path_buf(),
    );
    assert_eq!(
        recovered.recover().await.expect("live recovery probe"),
        0,
        "a second coordinator cannot interrupt a lease-held live run"
    );
    assert_eq!(
        recovered
            .get(started.id, None)
            .await
            .expect("live run remains inspectable")
            .status,
        WorkflowRunStatus::Running
    );
    assert!(matches!(
        recovered.pause(started.id).await,
        Err(CoreError::Conflict(_))
    ));
    assert!(matches!(
        recovered.cancel(started.id).await,
        Err(CoreError::Conflict(_))
    ));
    assert!(matches!(
        recovered.delete(started.id).await,
        Err(CoreError::Conflict(_))
    ));
    assert_eq!(
        original
            .get(started.id, None)
            .await
            .expect("foreign lifecycle attempts stay side-effect free")
            .status,
        WorkflowRunStatus::Running
    );

    let foreign_workspace = tempfile::tempdir().expect("foreign workspace fixture");
    let foreign = portable_coordinator(
        bun,
        Arc::clone(&store),
        Arc::new(CoordinatorHost::new(Arc::clone(&store), 0)),
        foreign_workspace.path().to_path_buf(),
    );
    assert_eq!(
        foreign.recover().await.expect("foreign recovery probe"),
        0,
        "recovery is scoped to the current workspace"
    );

    // Model a crash after proving the live lease: stop the fixture executor,
    // then leave its durable row in running state without a lease.
    let paused = original.pause(started.id).await.expect("fixture pauses");
    let mut orphan = paused;
    orphan.status = WorkflowRunStatus::Running;
    orphan.error = None;
    orphan.time.completed = None;
    store
        .save_workflow_run(&orphan)
        .await
        .expect("orphan running row");
    assert_eq!(recovered.recover().await.expect("recovery succeeds"), 1);
    let interrupted = recovered
        .get(started.id, None)
        .await
        .expect("recovered run remains inspectable");
    assert_eq!(interrupted.status, WorkflowRunStatus::Interrupted);
    assert!(
        interrupted
            .error
            .as_deref()
            .is_some_and(|message| message.contains("process stopped"))
    );
    assert_eq!(recovered_host.request_count(), 0, "recovery never replays");
    assert!(
        recovered_host.notifications().is_empty(),
        "restart recovery never injects an interrupted result"
    );

    assert!(original.delete(started.id).await.expect("fixture cleanup"));
}

fn bun_path() -> Option<PathBuf> {
    find_bun()
}

fn executor_with_bun(bun: PathBuf) -> WorkflowExecutor {
    WorkflowExecutor::new(ExecutorConfig {
        bun_path: Some(bun),
        execution_timeout: Some(Duration::from_secs(10)),
        ..ExecutorConfig::default()
    })
}

#[test]
fn capability_reports_bun_availability() {
    let executable = std::env::current_exe().expect("current executable");
    let executor = WorkflowExecutor::new(ExecutorConfig {
        bun_path: Some(executable.clone()),
        ..ExecutorConfig::default()
    });
    assert_eq!(executor.capability().bun_path, Some(executable));
    assert_eq!(executor.capability().blocker, None);
    assert!(executor.capability().executable);
}

#[test]
fn native_arguments_apply_declared_defaults_and_coerce_haarness_types() {
    let catalog = haarness_workflow_catalog::WorkflowCatalog::new(Vec::new());
    let definition = catalog
        .inspect_source(
            r#"export default workflow({
                name: "typed",
                arguments: {
                    count: { type: "number", default: 2 },
                    enabled: { type: "boolean", default: false },
                    label: { type: "string", default: "base" }
                },
                run() {}
            })"#,
            "typed.ts",
        )
        .unwrap();
    let args = crate::coordinator::workflow_args(
        &definition,
        Some(json!({"count":"4.5","enabled":"true","extra":[1]})),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        args,
        json!({"count":4.5,"enabled":true,"label":"base","extra":[1]})
    );
    assert!(
        crate::coordinator::workflow_args(&definition, Some(json!([1, 2])))
            .unwrap_err()
            .to_string()
            .contains("JSON object")
    );
}

#[test]
fn child_frames_require_the_known_version_and_known_fields() {
    let ready: ChildFrame =
        serde_json::from_str(r#"{"version":1,"type":"ready"}"#).expect("ready frame");
    assert!(matches!(ready, ChildFrame::Ready { .. }));
    let wrong = serde_json::from_str::<ChildFrame>(r#"{"version":2,"type":"ready"}"#).unwrap_err();
    assert!(wrong.to_string().contains("unsupported protocol version 2"));
    let unknown =
        serde_json::from_str::<ChildFrame>(r#"{"version":1,"type":"ready","surprise":true}"#)
            .unwrap_err();
    assert!(unknown.to_string().contains("unknown field"));
}

#[tokio::test]
async fn bounded_line_rejects_a_frame_before_unbounded_allocation() {
    let bytes = vec![b'x'; 65];
    let mut reader = BufReader::new(bytes.as_slice());
    let error = read_bounded_line(&mut reader, 64).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("exceeded 64 bytes"));
}

#[tokio::test]
async fn path_source_must_match_snapshot_and_hash() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let path = directory.path().join("workflow.ts");
    fs::write(&path, "export default 1\n").expect("fixture source");
    let mut invocation = WorkflowInvocation::new("wfr-source", "different\n");
    invocation.source_path = Some(path.clone());
    let error = prepare_source(invocation).await.unwrap_err();
    assert!(matches!(
        error,
        ExecutionErrorKind::InvalidInvocation(message)
            if message.contains("changed after discovery")
    ));

    let mut invocation = WorkflowInvocation::new("wfr-hash", "snapshot");
    invocation.expected_source_sha256 = Some("0".repeat(64));
    let error = prepare_source(invocation).await.unwrap_err();
    assert!(matches!(
        error,
        ExecutionErrorKind::InvalidInvocation(message)
            if message.contains("SHA-256")
    ));

    let snapshot = "export const meta = { name: 'portable' }; return 'snapshot'\n";
    fs::write(&path, "changed after discovery\n").expect("changed portable source");
    let mut invocation = WorkflowInvocation::new("wfr-portable-changed", snapshot);
    invocation.dialect = WorkflowDialect::ClaudePortable;
    invocation.source_path = Some(path.clone());
    let prepared = prepare_source(invocation)
        .await
        .expect("portable source path is diagnostic only");
    assert_eq!(prepared.source, snapshot);
    assert_eq!(prepared.source_path.as_deref(), Some(path.as_path()));

    fs::remove_file(&path).expect("remove portable source");
    let mut invocation = WorkflowInvocation::new("wfr-portable-removed", snapshot);
    invocation.dialect = WorkflowDialect::ClaudePortable;
    invocation.source_path = Some(path.clone());
    let prepared = prepare_source(invocation)
        .await
        .expect("portable snapshot survives a removed installed path");
    assert_eq!(prepared.source, snapshot);
    assert_eq!(prepared.source_path.as_deref(), Some(path.as_path()));
}

#[tokio::test]
async fn bun_native_fixture_supports_imports_host_calls_and_fanout() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping Bun fixture: Bun is unavailable");
        return;
    };
    let directory = tempfile::tempdir().expect("fixture directory");
    let helper = directory.path().join("helper.ts");
    fs::write(
        &helper,
        "export const double = (value: number) => value * 2\n",
    )
    .expect("helper source");
    let workflow = directory.path().join("workflow.ts");
    let source = r#"
import { workflow } from "@haarness-ai/plugin"
import { double } from "./helper.ts"

export default workflow({
  name: "native-fixture",
  async run(args, ctx) {
    ctx.setPhase("execute")
    ctx.setStatus({ state: "running", input: args.value })
    const fanout = await ctx.parallel([
      () => Promise.resolve(double(args.value)),
      () => Promise.reject(new Error("expected fanout failure")),
    ], { concurrencyLimit: 1 })
    const pipeline = await ctx.pipeline([1, 2], (value) => value * 2, (value, item) => value + item)
    const shell = await ctx.shell("fixture")
    const tool = await ctx.tool("read", { path: "README.md" })
    const nested = await ctx.workflow("child", { value: 1 })
    const agent = await ctx.agent({ prompt: "inspect" })
    const followup = await agent.followup({ prompt: "correct" })
    await agent.close()
    ctx.log("complete")
    return {
      fanout,
      pipeline,
      shell,
      tool,
      nested,
      agent: agent.text,
      followup: followup.text,
    }
  },
})
"#;
    fs::write(&workflow, source).expect("workflow source");
    let digest = format!("{:x}", Sha256::digest(source.as_bytes()));
    let mut invocation = WorkflowInvocation::new("wfr-native", source);
    invocation.source_path = Some(workflow);
    invocation.expected_source_sha256 = Some(digest.clone());
    invocation.args = Some(json!({"value": 4}));
    let host = Arc::new(RecordingHost::default());

    let output = executor_with_bun(bun)
        .execute(invocation, host.clone(), CancellationToken::new())
        .await
        .expect("native workflow");
    assert_eq!(output.source_sha256, digest);
    assert_eq!(output.value.as_ref().unwrap()["fanout"], json!([8, null]));
    assert_eq!(output.value.as_ref().unwrap()["pipeline"], json!([3, 6]));
    assert_eq!(output.value.as_ref().unwrap()["agent"], "first");
    assert_eq!(output.value.as_ref().unwrap()["followup"], "second");

    let notices = host.notices.lock().expect("notice lock");
    assert_eq!(notices[0].method, HostNoticeMethod::Phase);
    assert_eq!(notices[1].method, HostNoticeMethod::Status);
    assert!(notices.iter().any(|item| {
        item.method == HostNoticeMethod::Log
            && item
                .value
                .as_str()
                .is_some_and(|value| value.contains("fanout failure"))
    }));
    assert_eq!(notices.last().unwrap().value, "complete");
    assert!(
        notices
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );

    let methods = host
        .calls
        .lock()
        .expect("call lock")
        .iter()
        .map(|(method, _)| *method)
        .collect::<Vec<_>>();
    assert!(methods.contains(&HostRequestMethod::Shell));
    assert!(methods.contains(&HostRequestMethod::Tool));
    assert!(methods.contains(&HostRequestMethod::Workflow));
    assert!(methods.contains(&HostRequestMethod::Agent));
    assert!(methods.contains(&HostRequestMethod::AgentFollowup));
    assert!(methods.contains(&HostRequestMethod::AgentClose));
}

#[tokio::test]
async fn bun_portable_fixture_runs_with_restricted_globals() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping Bun fixture: Bun is unavailable");
        return;
    };
    let source = r#"
export const meta = {
  name: "portable-fixture",
  description: "restricted fixture",
}

phase("fanout")
status("Inspecting independently")
status({ state: "running", scope: args.value })
const replies = await parallel([
  () => agent("one", { label: "one", tools: { bash: false } }),
  () => agent({ prompt: "two", label: "two" }),
], { concurrencyLimit: 1 })
const followup = await replies[0].followup("verify")
await replies[0].close()
const values = await pipeline([1, 2], (value) => value * 2, (value, item) => value + item)
const empty = await parallel([])
const shellResult = await shell("fixture")
const toolResult = await tool("read", { path: "README.md" }, { timeout: 1000 })
const nested = await workflow("child", { value: 1 })
log("portable complete")
return {
  input: args.value,
  replies: replies.map((reply) => reply.text),
  followup: followup.text,
  values,
  empty,
  shellResult,
  toolResult,
  nested,
}
"#;
    let mut invocation = WorkflowInvocation::new("wfr-portable", source);
    invocation.dialect = WorkflowDialect::ClaudePortable;
    invocation.args = Some(json!({"value": "ok"}));
    let host = Arc::new(RecordingHost::default());
    let executor = WorkflowExecutor::new(ExecutorConfig {
        bun_path: Some(bun),
        execution_timeout: Some(Duration::from_secs(10)),
        ..ExecutorConfig::default()
    });

    let output = executor
        .execute(invocation, host.clone(), CancellationToken::new())
        .await
        .expect("portable workflow");
    assert_eq!(output.value.as_ref().unwrap()["input"], "ok");
    assert_eq!(
        output.value.as_ref().unwrap()["replies"],
        json!(["first", "first"])
    );
    assert_eq!(output.value.as_ref().unwrap()["followup"], "second");
    assert_eq!(output.value.as_ref().unwrap()["values"], json!([3, 6]));
    assert_eq!(output.value.as_ref().unwrap()["empty"], json!([]));
    assert_eq!(
        output.value.as_ref().unwrap()["shellResult"],
        json!({"output":"shell-ok","exitCode":0})
    );
    assert_eq!(
        output.value.as_ref().unwrap()["toolResult"],
        json!({"tool":"ok"})
    );
    assert_eq!(
        output.value.as_ref().unwrap()["nested"],
        json!({"nested":"ok"})
    );
    let notices = host.notices.lock().expect("notice lock");
    assert_eq!(notices[0].value, "fanout");
    assert_eq!(notices[1].value["message"], "Inspecting independently");
    assert_eq!(notices.last().unwrap().value, "portable complete");
    drop(notices);
    let calls = host.calls.lock().expect("call lock");
    assert_eq!(calls[0].1["label"], "one");
    assert_eq!(calls[0].1["tools"]["bash"], false);
    let methods = calls.iter().map(|(method, _)| *method).collect::<Vec<_>>();
    assert!(methods.contains(&HostRequestMethod::AgentFollowup));
    assert!(methods.contains(&HostRequestMethod::AgentClose));
    assert!(methods.contains(&HostRequestMethod::Shell));
    assert!(methods.contains(&HostRequestMethod::Tool));
    assert!(methods.contains(&HostRequestMethod::Workflow));
}

#[tokio::test]
async fn bun_portable_fixture_rejects_privileged_globals() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping Bun fixture: Bun is unavailable");
        return;
    };
    let source = r#"
export const meta = { name: "blocked" }
return process.env
"#;
    let mut invocation = WorkflowInvocation::new("wfr-blocked", source);
    invocation.dialect = WorkflowDialect::ClaudePortable;
    let executor = WorkflowExecutor::new(ExecutorConfig {
        bun_path: Some(bun),
        ..ExecutorConfig::default()
    });
    let error = executor
        .execute(
            invocation,
            Arc::new(RecordingHost::default()),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind,
        ExecutionErrorKind::Runner(message)
            if message.contains("global process is unavailable")
    ));
}

#[tokio::test]
async fn bun_portable_fixture_terminates_a_cpu_loop() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping Bun fixture: Bun is unavailable");
        return;
    };
    let source = r#"
export const meta = { name: "loop" }
while (true) {}
"#;
    let mut invocation = WorkflowInvocation::new("wfr-loop", source);
    invocation.dialect = WorkflowDialect::ClaudePortable;
    let executor = WorkflowExecutor::new(ExecutorConfig {
        bun_path: Some(bun),
        portable_watchdog_timeout: Duration::from_millis(100),
        ..ExecutorConfig::default()
    });
    let started = Instant::now();
    let error = executor
        .execute(
            invocation,
            Arc::new(RecordingHost::default()),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error.kind, ExecutionErrorKind::Runner(_)));
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn cancellation_terminates_native_child_promptly() {
    let Some(bun) = bun_path() else {
        eprintln!("skipping Bun fixture: Bun is unavailable");
        return;
    };
    let source = r#"
workflow({
  name: "cancel-fixture",
  async run() {
    await new Promise((resolve) => setTimeout(resolve, 30000))
    return "late"
  },
})
"#;
    let invocation = WorkflowInvocation::new("wfr-cancel", source);
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
    });
    let started = Instant::now();
    let error = executor_with_bun(bun)
        .execute(invocation, Arc::new(RecordingHost::default()), cancellation)
        .await
        .unwrap_err();
    assert!(matches!(error.kind, ExecutionErrorKind::Cancelled));
    assert!(started.elapsed() < Duration::from_secs(3));
}
