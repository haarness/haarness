//! Runs nobody watches never wait for a person: a schedule's requests that
//! always ask are refused at once, and a workflow's own tool calls follow the
//! turn that approves its own requests.
use super::*;
use haarness_core::{PermissionDecision as D, PermissionRule};
use haarness_protocol::{ItemData, PermissionReply, PermissionReplyDecision as Reply};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

const REFUSED: &str = "needs the user's approval every time, and this scheduled run cannot ask";

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("haarness-unattended-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A tool whose every call asks, like a Cloud action that spends money.
struct AlwaysAsks {
    executions: Arc<AtomicUsize>,
}
#[async_trait]
impl Tool for AlwaysAsks {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "spend_fixture".into(),
            description: "Fixture action that always asks".into(),
            input_schema: json!({"type":"object","properties":{}}),
        }
    }
    fn side_effectful(&self) -> bool {
        true
    }
    fn permission_constraint(&self, _args: &Value, _operation: &str) -> Option<D> {
        Some(D::Ask)
    }
    async fn execute(&self, _args: Value, _context: ToolContext) -> CoreResult<ToolOutput> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput {
            text: "spent".into(),
            metadata: json!({}),
        })
    }
}

/// A tool that holds its turn open until released.
struct Hold(Arc<tokio::sync::Notify>);
#[async_trait]
impl Tool for Hold {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "hold_fixture".into(),
            description: "Fixture that waits".into(),
            input_schema: json!({"type":"object","properties":{}}),
        }
    }
    fn side_effectful(&self) -> bool {
        false
    }
    async fn execute(&self, _args: Value, _context: ToolContext) -> CoreResult<ToolOutput> {
        self.0.notified().await;
        Ok(ToolOutput {
            text: "released".into(),
            metadata: json!({}),
        })
    }
}

struct WithFixtures {
    native: NativeToolRegistry,
    extra: Vec<Arc<dyn Tool>>,
}
impl ToolRegistry for WithFixtures {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.extra
            .iter()
            .find(|tool| tool.definition().name == name)
            .cloned()
            .or_else(|| self.native.get(name))
    }
    fn list(&self) -> Vec<ToolDefinition> {
        let mut tools = self.native.list();
        tools.extend(self.extra.iter().map(|tool| tool.definition()));
        tools
    }
}

struct Unattended {
    runtime: Arc<AgentRuntime>,
    executions: Arc<AtomicUsize>,
    hold: Arc<tokio::sync::Notify>,
    _workspace: Workspace,
}
/// `bash` asks under these rules, so an approval is visible in each test.
fn unattended() -> Unattended {
    let workspace = Workspace::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let hold = Arc::new(tokio::sync::Notify::new());
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    config.base_rules.push(PermissionRule {
        pattern: "bash:*".into(),
        decision: D::Ask,
    });
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(WithFixtures {
            native: NativeToolRegistry::new(workspace.0.clone()),
            extra: vec![
                Arc::new(AlwaysAsks {
                    executions: executions.clone(),
                }),
                Arc::new(Hold(hold.clone())),
            ],
        }),
        EventBus::new(256),
        workspace.0.clone(),
        String::new(),
        config,
    );
    Unattended {
        runtime: Arc::new(runtime),
        executions,
        hold,
        _workspace: workspace,
    }
}

async fn scheduled_thread(runtime: &AgentRuntime) -> Thread {
    runtime
        .create_thread(CreateThreadRequest {
            schedule: Some(haarness_protocol::ScheduleOrigin {
                id: uuid::Uuid::now_v7(),
                name: "Nightly report".into(),
            }),
            title: Some("Fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap()
}
async fn child_thread(runtime: &AgentRuntime, parent: haarness_protocol::ThreadId) -> Thread {
    runtime
        .create_thread(CreateThreadRequest {
            title: Some("Fixture child".into()),
            parent_id: Some(parent),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap()
}
/// The turn a schedule (or `run --yolo`) starts: it approves its own requests.
fn approving(text: &str) -> StartTurnRequest {
    let mut request = turn_request(text);
    request.auto_approve = true;
    request.tools = Some(std::collections::HashMap::from([(
        "question".into(),
        false,
    )]));
    request
}
async fn refused_at_once(runtime: &AgentRuntime, thread: haarness_protocol::ThreadId) -> bool {
    runtime
        .items(thread, None)
        .await
        .unwrap()
        .iter()
        .any(|item| {
            matches!(&item.data, ItemData::ToolResult { output, is_error: true, .. }
                if output.contains(REFUSED))
        })
}
async fn pending_for(
    runtime: &AgentRuntime,
    thread: haarness_protocol::ThreadId,
) -> haarness_protocol::PendingPermissionInfo {
    for _ in 0..500 {
        if let Some(pending) = runtime
            .pending_permissions()
            .into_iter()
            .find(|pending| pending.thread_id == thread)
        {
            return pending;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no approval was requested");
}
/// Who makes a direct call: a workflow's own `ctx.tool`/`ctx.shell`, or a
/// person (`!`, the browser tool API).
#[derive(Clone, Copy)]
enum Caller {
    Workflow,
    Person,
}
async fn direct(
    runtime: &AgentRuntime,
    caller: Caller,
    thread: haarness_protocol::ThreadId,
    tool: &str,
    arguments: Value,
) -> haarness_core::DirectToolResult {
    match caller {
        Caller::Workflow => {
            runtime
                .run_workflow_tool(thread, tool, arguments, CancellationToken::new())
                .await
        }
        Caller::Person => {
            runtime
                .run_direct_tool(thread, tool, arguments, CancellationToken::new())
                .await
        }
    }
    .unwrap()
}
/// A direct call in `thread` that must ask: rejected once it does.
async fn still_asks(
    runtime: &Arc<AgentRuntime>,
    caller: Caller,
    thread: haarness_protocol::ThreadId,
    tool: &str,
) {
    let call = tokio::spawn({
        let runtime = runtime.clone();
        let tool = tool.to_owned();
        let arguments = if tool == "bash" {
            json!({"command":"echo asked"})
        } else {
            json!({})
        };
        async move { direct(&runtime, caller, thread, &tool, arguments).await }
    });
    let pending = pending_for(runtime, thread).await;
    runtime
        .reply_permission(
            &pending.permission_id,
            PermissionReply {
                decision: Reply::Reject,
                feedback: None,
            },
        )
        .await
        .unwrap();
    assert!(call.await.unwrap().is_error);
}
/// Start `request` in `thread` holding its turn open, create a workflow's
/// thread and an agent's under it as the workflow host does while that turn
/// runs, then let the turn end.
async fn workflow_started_by(
    fixture: &Unattended,
    thread: haarness_protocol::ThreadId,
    request: StartTurnRequest,
) -> (Thread, Thread) {
    let runtime = &fixture.runtime;
    let turn = runtime.start_turn(thread, request).await.unwrap();
    let workflow = child_thread(runtime, thread).await;
    let agent = child_thread(runtime, workflow.id).await;
    fixture.hold.notify_one();
    assert_eq!(
        wait_done(runtime, &turn).await.status,
        TurnStatus::Completed
    );
    (workflow, agent)
}
const HOLD: &str = "!tool hold_fixture {}";

#[tokio::test]
async fn a_scheduled_run_refuses_a_request_that_always_asks_instead_of_waiting() {
    let fixture = unattended();
    let runtime = &fixture.runtime;
    let root = scheduled_thread(runtime).await;
    let turn = runtime
        .start_turn(root.id, approving("!tool spend_fixture {}"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(refused_at_once(runtime, root.id).await);
    // A workflow another run starts is refused the same way, also after
    // that run's turn ended.
    let other = scheduled_thread(runtime).await;
    let (workflow, _) = workflow_started_by(&fixture, other.id, approving(HOLD)).await;
    let refused = direct(
        runtime,
        Caller::Workflow,
        workflow.id,
        "spend_fixture",
        json!({}),
    )
    .await;
    assert!(refused.is_error);
    assert!(
        refused.output.text.contains(REFUSED),
        "{}",
        refused.output.text
    );
    assert!(runtime.pending_permissions().is_empty());
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 0);
    // An ordinary chat still asks a person.
    let chat = echo_thread(runtime).await;
    let turn = runtime
        .start_turn(chat.id, approving("!tool spend_fixture {}"))
        .await
        .unwrap();
    let pending = pending_for(runtime, chat.id).await;
    assert_eq!(pending.allow_always, Some(false));
    runtime
        .reply_permission(
            &pending.permission_id,
            PermissionReply {
                decision: Reply::Once,
                feedback: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_workflow_started_by_a_scheduled_run_approves_its_own_tool_calls_once() {
    let fixture = unattended();
    let runtime = &fixture.runtime;
    let root = scheduled_thread(runtime).await;
    let (workflow, agent) = workflow_started_by(&fixture, root.id, approving(HOLD)).await;
    // The run's turn has ended: nobody watches its workflow either.
    for thread in [workflow.id, agent.id] {
        let call = direct(
            runtime,
            Caller::Workflow,
            thread,
            "bash",
            json!({"command":"echo unattended"}),
        )
        .await;
        assert!(!call.is_error, "{}", call.output.text);
        assert!(call.output.text.contains("unattended"));
    }
    assert!(runtime.pending_permissions().is_empty());
    // A person's own `!` command in the schedule's chat is still asked.
    still_asks(runtime, Caller::Person, root.id, "bash").await;
    // A workflow under an ordinary chat asks too.
    let chat = echo_thread(runtime).await;
    still_asks(
        runtime,
        Caller::Workflow,
        child_thread(runtime, chat.id).await.id,
        "bash",
    )
    .await;
}

#[tokio::test]
async fn a_person_in_a_scheduled_chat_or_its_fork_is_asked() {
    // The run is over; someone opens its chat, or forks it, and works there.
    let fixture = unattended();
    let runtime = &fixture.runtime;
    let root = scheduled_thread(runtime).await;
    let turn = runtime
        .start_turn(root.id, approving("nightly summary"))
        .await
        .unwrap();
    wait_done(runtime, &turn).await;
    let fork = runtime.fork_thread(root.id, None, None).await.unwrap();
    for thread in [root.id, fork.id] {
        // A workflow the person's turn starts asks, and so does what always
        // asks: neither is approved nor refused on the run's behalf.
        let (workflow, agent) = workflow_started_by(&fixture, thread, turn_request(HOLD)).await;
        for workflow_thread in [workflow.id, agent.id] {
            still_asks(runtime, Caller::Workflow, workflow_thread, "bash").await;
        }
        still_asks(runtime, Caller::Workflow, workflow.id, "spend_fixture").await;
        // The person's `!` command and their model call that always asks.
        still_asks(runtime, Caller::Person, thread, "bash").await;
        let turn = runtime
            .start_turn(thread, turn_request("!tool spend_fixture {}"))
            .await
            .unwrap();
        let pending = pending_for(runtime, thread).await;
        runtime
            .reply_permission(
                &pending.permission_id,
                PermissionReply {
                    decision: Reply::Once,
                    feedback: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            wait_done(runtime, &turn).await.status,
            TurnStatus::Completed
        );
        assert!(!refused_at_once(runtime, thread).await);
    }
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_workflow_follows_the_turn_that_approves_its_own_requests() {
    // `haarness run --yolo`, ACP or a `--yolo` terminal UI: the chat's turn
    // approves its own requests, and so does a workflow it starts, also
    // after the turn ends.
    let fixture = unattended();
    let runtime = &fixture.runtime;
    let chat = echo_thread(runtime).await;
    let (workflow, agent) = workflow_started_by(&fixture, chat.id, approving(HOLD)).await;
    for thread in [workflow.id, agent.id] {
        let call = direct(
            runtime,
            Caller::Workflow,
            thread,
            "bash",
            json!({"command":"echo followed"}),
        )
        .await;
        assert!(!call.is_error, "{}", call.output.text);
    }
    // The request that always asks is still a person's.
    still_asks(runtime, Caller::Workflow, workflow.id, "spend_fixture").await;
    // So is the person's own `!` command in that chat.
    still_asks(runtime, Caller::Person, chat.id, "bash").await;
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 0);
}
