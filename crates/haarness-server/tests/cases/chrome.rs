//! Sites in the user's own Chrome need a person's answer, for one chat; and
//! Chrome extension hosts end to end through the HTTP server.
use super::*;
use futures::SinkExt as _;
use haarness_core::{HookDecision, HookPort, PermissionDecision as D, PermissionRule};
use haarness_protocol::{ItemData, PermissionReply, PermissionReplyDecision as Reply};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest as _};

const SITE: &str = "https://mail.example.test";

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("haarness-chrome-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A Chrome page tool as the broker prepares it: navigating needs this
/// chat's approval for one site.
struct ChromeSite {
    executions: Arc<AtomicUsize>,
}
#[async_trait]
impl Tool for ChromeSite {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "browser_navigate".into(),
            description: "Fixture navigation in Chrome".into(),
            input_schema: json!({"type":"object","properties":{"url":{"type":"string"}}}),
        }
    }
    fn side_effectful(&self) -> bool {
        true
    }
    async fn prepare_arguments(
        &self,
        mut args: Value,
        thread: haarness_protocol::ThreadId,
        _cancel: CancellationToken,
    ) -> CoreResult<Value> {
        args["__browser"] = json!({
            "browser": "chrome",
            "target": {"resources": [SITE]},
            "remember": [format!("chrome-browsing:{thread}:{SITE}")],
        });
        Ok(args)
    }
    fn explicit_approval(&self, _args: &Value, operation: &str) -> bool {
        operation.starts_with("browser_site:chrome-")
    }
    async fn execute(&self, _args: Value, _context: ToolContext) -> CoreResult<ToolOutput> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput {
            text: "navigated".into(),
            metadata: json!({"browser":"chrome"}),
        })
    }
}
struct WithChrome {
    native: NativeToolRegistry,
    chrome: Arc<dyn Tool>,
}
impl ToolRegistry for WithChrome {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if name == "browser_navigate" {
            Some(self.chrome.clone())
        } else {
            self.native.get(name)
        }
    }
    fn list(&self) -> Vec<ToolDefinition> {
        let mut tools = self.native.list();
        tools.push(self.chrome.definition());
        tools
    }
}
/// Hooks that would approve everything.
struct ApproveAll;
#[async_trait]
impl HookPort for ApproveAll {
    async fn pre_tool_use(
        &self,
        _: haarness_protocol::ThreadId,
        _: &str,
        _: &Value,
    ) -> HookDecision {
        HookDecision {
            permission: Some(D::Allow),
            ..HookDecision::default()
        }
    }
    async fn post_tool_use(
        &self,
        _: haarness_protocol::ThreadId,
        _: &str,
        _: &Value,
        _: &str,
        _: bool,
    ) -> Option<String> {
        None
    }
    async fn user_prompt_submit(&self, _: haarness_protocol::ThreadId, _: &str) -> HookDecision {
        HookDecision::default()
    }
    async fn permission_request(
        &self,
        _: haarness_protocol::ThreadId,
        _: &str,
        _: &[String],
        _: &Value,
    ) -> HookDecision {
        HookDecision {
            permission: Some(D::Allow),
            ..HookDecision::default()
        }
    }
    async fn stop(&self, _: haarness_protocol::ThreadId) {}
}

struct Chrome {
    runtime: Arc<AgentRuntime>,
    store: Arc<SqliteStore>,
    executions: Arc<AtomicUsize>,
    workspace: Workspace,
}
fn chrome(config: RuntimeConfig, hooks: bool) -> Chrome {
    let workspace = Workspace::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let mut runtime = AgentRuntime::with_config(
        store.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(WithChrome {
            native: NativeToolRegistry::new(workspace.0.clone()),
            chrome: Arc::new(ChromeSite {
                executions: executions.clone(),
            }),
        }),
        EventBus::new(256),
        workspace.0.clone(),
        String::new(),
        config,
    );
    if hooks {
        runtime = runtime.with_hooks(Arc::new(ApproveAll));
    }
    Chrome {
        runtime: Arc::new(runtime),
        store,
        executions,
        workspace,
    }
}
const NAVIGATE: &str = r#"!tool browser_navigate {"url":"https://mail.example.test"}"#;
async fn explicit_prompt(
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
    panic!("no Chrome approval was requested");
}
async fn answer(runtime: &AgentRuntime, id: &str, decision: Reply) {
    runtime
        .reply_permission(
            id,
            PermissionReply {
                decision,
                feedback: None,
            },
        )
        .await
        .unwrap();
}
async fn set_auto(runtime: &AgentRuntime, thread: haarness_protocol::ThreadId) {
    runtime
        .set_approval_mode(
            thread,
            haarness_protocol::SetApprovalModeRequest {
                mode: haarness_protocol::ApprovalModeSetting::Auto,
                revision: None,
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn chrome_sites_ask_in_auto_and_turning_auto_on_never_answers_them() {
    let fixture = chrome(RuntimeConfig::default(), false);
    let runtime = &fixture.runtime;
    let thread = echo_thread(runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request(NAVIGATE))
        .await
        .unwrap();
    let pending = explicit_prompt(runtime, thread.id).await;
    assert!(pending.explicit);
    assert_ne!(pending.allow_always, Some(false));
    assert_eq!(
        pending.patterns,
        vec![format!("chrome-browsing:{}:{SITE}", thread.id)]
    );
    assert!(runtime.events().subscribe(None).replay.iter().any(|event| {
        event.event_type == "permission.requested" && event.payload["explicit"] == true
    }));
    // Auto approves the requests waiting in a chat, but never this one.
    set_auto(runtime, thread.id).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(runtime.pending_permissions().len(), 1);
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 0);
    answer(runtime, &pending.permission_id, Reply::Once).await;
    assert_eq!(
        wait_done(runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 1);
    // A chat already in Auto still asks, and "once" remembered nothing.
    let turn = runtime
        .start_turn(thread.id, turn_request(NAVIGATE))
        .await
        .unwrap();
    let pending = explicit_prompt(runtime, thread.id).await;
    answer(runtime, &pending.permission_id, Reply::Reject).await;
    wait_done(runtime, &turn).await;
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 1);
    // The direct path asks too.
    let direct = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            runtime
                .run_direct_tool(
                    thread.id,
                    "browser_navigate",
                    json!({"url":SITE}),
                    CancellationToken::new(),
                )
                .await
        }
    });
    let pending = explicit_prompt(runtime, thread.id).await;
    assert!(pending.explicit);
    answer(runtime, &pending.permission_id, Reply::Once).await;
    assert!(!direct.await.unwrap().unwrap().is_error);
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn hooks_and_configured_allow_rules_never_approve_chrome_sites() {
    let mut config = RuntimeConfig::default();
    for pattern in ["*", "browser_site:*", "browser_site:chrome-*"] {
        config.base_rules.push(PermissionRule {
            pattern: pattern.into(),
            decision: D::Allow,
        });
    }
    let fixture = chrome(config, true);
    let runtime = &fixture.runtime;
    let thread = echo_thread(runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request(NAVIGATE))
        .await
        .unwrap();
    let pending = explicit_prompt(runtime, thread.id).await;
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 0);
    answer(runtime, &pending.permission_id, Reply::Once).await;
    assert_eq!(
        wait_done(runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let direct = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            runtime
                .run_direct_tool(
                    thread.id,
                    "browser_navigate",
                    json!({"url":SITE}),
                    CancellationToken::new(),
                )
                .await
        }
    });
    let pending = explicit_prompt(runtime, thread.id).await;
    answer(runtime, &pending.permission_id, Reply::Reject).await;
    assert!(direct.await.unwrap().unwrap().is_error);
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 1);

    // Configured rules may still refuse.
    let mut config = RuntimeConfig::default();
    config.base_rules.push(PermissionRule {
        pattern: "browser_site:chrome-*".into(),
        decision: D::Deny,
    });
    let fixture = chrome(config, true);
    let runtime = &fixture.runtime;
    let thread = echo_thread(runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request(NAVIGATE))
        .await
        .unwrap();
    assert_eq!(
        wait_done(runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(runtime.pending_permissions().is_empty());
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_yolo_server_still_asks_and_remembers_nothing() {
    let config = RuntimeConfig {
        auto_approve: true,
        ..RuntimeConfig::default()
    };
    let fixture = chrome(config, false);
    let runtime = &fixture.runtime;
    let thread = echo_thread(runtime).await;
    for _ in 0..2 {
        let turn = runtime
            .start_turn(thread.id, turn_request(NAVIGATE))
            .await
            .unwrap();
        let pending = explicit_prompt(runtime, thread.id).await;
        answer(runtime, &pending.permission_id, Reply::Once).await;
        assert_eq!(
            wait_done(runtime, &turn).await.status,
            TurnStatus::Completed
        );
    }
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 2);
    // A `--yolo` client turn has a person at hand: it asks too.
    let mut request = turn_request(NAVIGATE);
    request.auto_approve = true;
    let turn = runtime.start_turn(thread.id, request).await.unwrap();
    let pending = explicit_prompt(runtime, thread.id).await;
    answer(runtime, &pending.permission_id, Reply::Once).await;
    assert_eq!(
        wait_done(runtime, &turn).await.status,
        TurnStatus::Completed
    );
}

#[tokio::test]
async fn allow_in_this_chat_covers_that_chat_only_and_is_never_saved() {
    let fixture = chrome(RuntimeConfig::default(), false);
    let runtime = &fixture.runtime;
    let thread = echo_thread(runtime).await;
    let other = echo_thread(runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request(NAVIGATE))
        .await
        .unwrap();
    let pending = explicit_prompt(runtime, thread.id).await;
    // "Always for this folder" acts as "Allow in this chat".
    answer(runtime, &pending.permission_id, Reply::AlwaysWorkspace).await;
    assert_eq!(
        wait_done(runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let turn = runtime
        .start_turn(thread.id, turn_request(NAVIGATE))
        .await
        .unwrap();
    assert_eq!(
        wait_done(runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 2);
    assert!(
        fixture
            .store
            .load_permission_rules(&fixture.workspace.0.display().to_string())
            .await
            .unwrap()
            .is_empty()
    );
    let turn = runtime
        .start_turn(other.id, turn_request(NAVIGATE))
        .await
        .unwrap();
    let pending = explicit_prompt(runtime, other.id).await;
    assert_eq!(
        pending.patterns,
        vec![format!("chrome-browsing:{}:{SITE}", other.id)]
    );
    answer(runtime, &pending.permission_id, Reply::Reject).await;
    wait_done(runtime, &turn).await;
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 2);
}

/// A scheduled run's chat, as the scheduler creates it.
async fn scheduled_thread(runtime: &AgentRuntime) -> Thread {
    runtime
        .create_thread(CreateThreadRequest {
            schedule: Some(haarness_protocol::ScheduleOrigin {
                id: uuid::Uuid::now_v7(),
                name: "Morning inbox".into(),
            }),
            title: Some("Fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap()
}
/// A chat started inside another: a subagent, a workflow's thread or one of
/// its agents.
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
const CANNOT_ASK: &str = "Chrome needs the user's approval for https://mail.example.test and this run cannot ask. Ask the user to allow it in an interactive chat, then retry.";
async fn failed_at_once(runtime: &AgentRuntime, thread: haarness_protocol::ThreadId) -> bool {
    runtime
        .items(thread, None)
        .await
        .unwrap()
        .iter()
        .any(|item| {
            matches!(&item.data, ItemData::ToolResult { output, is_error: true, .. }
                if output.contains(CANNOT_ASK))
        })
}

#[tokio::test]
async fn a_scheduled_run_and_everything_it_starts_fail_at_once_instead_of_waiting() {
    let fixture = chrome(RuntimeConfig::default(), false);
    let runtime = &fixture.runtime;
    let root = scheduled_thread(runtime).await;
    // The run itself, as the scheduler starts it.
    let mut request = turn_request(NAVIGATE);
    request.auto_approve = true;
    request.tools = Some(std::collections::HashMap::from([(
        "question".into(),
        false,
    )]));
    let turn = runtime.start_turn(root.id, request).await.unwrap();
    assert_eq!(
        wait_done(runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(failed_at_once(runtime, root.id).await);
    // A subagent it delegates to through the task tool, whatever its turn's
    // own flags say.
    let task = json!({"prompt": NAVIGATE, "agent": "general", "description": "open the inbox"});
    let turn = runtime
        .start_turn(root.id, turn_request(&format!("!tool task {task}")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let mut subagents = Vec::new();
    for id in runtime.thread_tree_ids(root.id).await.unwrap() {
        if id != root.id {
            subagents.push(id);
        }
    }
    assert_eq!(subagents.len(), 1, "{subagents:?}");
    assert!(failed_at_once(runtime, subagents[0]).await);
    // A workflow it starts: the run's thread and an agent under it.
    let workflow = child_thread(runtime, subagents[0]).await;
    let agent = child_thread(runtime, workflow.id).await;
    let turn = runtime
        .start_turn(agent.id, turn_request(NAVIGATE))
        .await
        .unwrap();
    assert_eq!(
        wait_done(runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(failed_at_once(runtime, agent.id).await);
    // The workflow's own tool calls (ctx.tool, the browser tool API) take
    // the direct path, and fail the same way.
    let direct = runtime
        .run_direct_tool(
            workflow.id,
            "browser_navigate",
            json!({"url":SITE}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(direct.is_error);
    assert!(
        direct.output.text.contains(CANNOT_ASK),
        "{}",
        direct.output.text
    );
    assert!(runtime.pending_permissions().is_empty());
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 0);
    for thread in [root.id, subagents[0], workflow.id, agent.id] {
        assert!(runtime.scheduled_run(thread).await);
    }
    assert!(!runtime.scheduled_run(echo_thread(runtime).await.id).await);
}

#[tokio::test]
async fn yolo_clients_and_their_workflow_agents_still_ask() {
    // ACP --yolo, and the agents a --yolo chat's workflow starts, approve
    // their own requests with the question tool off; a person still answers.
    let fixture = chrome(RuntimeConfig::default(), false);
    let runtime = &fixture.runtime;
    let chat = echo_thread(runtime).await;
    let workflow = child_thread(runtime, chat.id).await;
    let agent = child_thread(runtime, workflow.id).await;
    for thread in [chat.id, agent.id] {
        let mut request = turn_request(NAVIGATE);
        request.auto_approve = true;
        request.tools = Some(std::collections::HashMap::from([(
            "question".into(),
            false,
        )]));
        let turn = runtime.start_turn(thread, request).await.unwrap();
        let pending = explicit_prompt(runtime, thread).await;
        assert!(pending.explicit);
        answer(runtime, &pending.permission_id, Reply::Once).await;
        assert_eq!(
            wait_done(runtime, &turn).await.status,
            TurnStatus::Completed
        );
    }
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 2);
    // So does the workflow's own call.
    let direct = tokio::spawn({
        let runtime = runtime.clone();
        let workflow = workflow.id;
        async move {
            runtime
                .run_direct_tool(
                    workflow,
                    "browser_navigate",
                    json!({"url":SITE}),
                    CancellationToken::new(),
                )
                .await
        }
    });
    let pending = explicit_prompt(runtime, workflow.id).await;
    answer(runtime, &pending.permission_id, Reply::Once).await;
    assert!(!direct.await.unwrap().unwrap().is_error);
    assert_eq!(fixture.executions.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_published_chat_cannot_answer_chrome_sites() {
    let fixture = chrome(RuntimeConfig::default(), false);
    let runtime = fixture.runtime.clone();
    let thread = echo_thread(&runtime).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = serve(
        listener,
        "chrome-token",
        ServerState {
            browser: None,
            management: None,
            runtime: runtime.clone(),
            mcp: Arc::new(NoMcp),
            skills: Arc::new(NoSkills),
            plugins: None,
            workflows: None,
            workspaces: None,
            workspace_id: None,
            workspace: fixture.workspace.0.clone(),
            project_trusted: false,
            language: None,
        },
    )
    .await
    .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request(NAVIGATE))
        .await
        .unwrap();
    let pending = explicit_prompt(&runtime, thread.id).await;
    let url = format!(
        "http://{}/v1/permissions/{}",
        handle.address, pending.permission_id
    );
    let client = reqwest::Client::new();
    let refused = client
        .post(&url)
        .bearer_auth("chrome-token")
        .header("x-haarness-remote-thread", thread.id.to_string())
        .json(&json!({"decision":"once"}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::FORBIDDEN);
    let body: Value = refused.json().await.unwrap();
    assert_eq!(
        body["message"],
        "Answer this on the computer running Chrome."
    );
    let info: Value = client
        .get(format!("http://{}/v1/info", handle.address))
        .bearer_auth("chrome-token")
        .header("x-haarness-remote-thread", thread.id.to_string())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(info.get("pid").is_none(), "{info}");
    // Nor can they reach Chrome's hosts or endpoint.
    for path in ["/v1/browser/hosts", "/v1/browser/extension/host?version=1"] {
        let denied = client
            .get(format!("http://{}{path}", handle.address))
            .bearer_auth("chrome-token")
            .header("x-haarness-remote-thread", thread.id.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN, "{path}");
    }
    assert_eq!(runtime.pending_permissions().len(), 1);
    let answered = client
        .post(&url)
        .bearer_auth("chrome-token")
        .json(&json!({"decision":"once"}))
        .send()
        .await
        .unwrap();
    assert_eq!(answered.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn other_site_approvals_still_follow_auto_and_always() {
    // A Haarness browser site gate is an ordinary prompt: Auto answers it.
    struct DesktopSite;
    #[async_trait]
    impl Tool for DesktopSite {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "browser_navigate".into(),
                description: "Fixture navigation in the Haarness browser".into(),
                input_schema: json!({"type":"object","properties":{"url":{"type":"string"}}}),
            }
        }
        fn side_effectful(&self) -> bool {
            true
        }
        async fn prepare_arguments(
            &self,
            mut args: Value,
            _thread: haarness_protocol::ThreadId,
            _cancel: CancellationToken,
        ) -> CoreResult<Value> {
            args["__browser"] =
                json!({"target":{"resources":[SITE]},"remember":[format!("browsing:{SITE}")]});
            Ok(args)
        }
        async fn execute(&self, _args: Value, _context: ToolContext) -> CoreResult<ToolOutput> {
            Ok(ToolOutput {
                text: "navigated".into(),
                metadata: json!({}),
            })
        }
    }
    let workspace = Workspace::new();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(WithChrome {
            native: NativeToolRegistry::new(workspace.0.clone()),
            chrome: Arc::new(DesktopSite),
        }),
        EventBus::new(256),
        workspace.0.clone(),
        String::new(),
        RuntimeConfig::default(),
    );
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request(NAVIGATE))
        .await
        .unwrap();
    let pending = explicit_prompt(&runtime, thread.id).await;
    assert!(!pending.explicit);
    set_auto(&runtime, thread.id).await;
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(runtime.pending_permissions().is_empty());
}

// ---------------------------------------------------------------------------
// Extension hosts through the server.

struct Server {
    handle: haarness_server::ServerHandle,
    runtime: Arc<AgentRuntime>,
    base: String,
    _workspace: Workspace,
}
async fn chrome_server(access: haarness_server::auth::ServerAccess) -> Server {
    let workspace = Workspace::new();
    let broker = haarness_server::browser::BrowserBroker::new();
    let runtime: Arc<AgentRuntime> = Arc::new(AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(workspace.0.clone()).with_browser(broker.clone())),
        EventBus::new(256),
        workspace.0.clone(),
        String::new(),
        RuntimeConfig::default(),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = haarness_server::api::serve_with_access(
        listener,
        "chrome-token",
        ServerState {
            browser: Some(broker.clone()),
            management: None,
            runtime: runtime.clone(),
            mcp: Arc::new(NoMcp),
            skills: Arc::new(NoSkills),
            plugins: None,
            workflows: None,
            workspaces: None,
            workspace_id: None,
            workspace: workspace.0.clone(),
            project_trusted: false,
            language: None,
        },
        None,
        access,
    )
    .await
    .unwrap();
    broker.set_thread_lookup(haarness_server::browser::runtime_threads(&runtime));
    let base = format!("{}", handle.address);
    Server {
        handle,
        runtime,
        base,
        _workspace: workspace,
    }
}
type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
async fn extension_socket(
    base: &str,
    version: &str,
    origin: Option<&str>,
) -> Result<Socket, tungstenite::Error> {
    let mut request = format!("ws://{base}/v1/browser/extension/host?version={version}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("authorization", "Bearer chrome-token".parse().unwrap());
    if let Some(origin) = origin {
        request
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
    }
    tokio_tungstenite::connect_async(request)
        .await
        .map(|(socket, _)| socket)
}
fn refused_with(result: Result<Socket, tungstenite::Error>) -> u16 {
    match result {
        Err(tungstenite::Error::Http(response)) => response.status().as_u16(),
        Err(error) => panic!("unexpected error {error}"),
        Ok(_) => panic!("connection was accepted"),
    }
}
async fn receive(socket: &mut Socket) -> Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("a message from the server")
        {
            Some(Ok(tungstenite::Message::Text(text))) => {
                return serde_json::from_str(&text).unwrap();
            }
            Some(Ok(tungstenite::Message::Ping(_) | tungstenite::Message::Pong(_))) => {}
            other => panic!("socket ended: {other:?}"),
        }
    }
}
async fn send(socket: &mut Socket, value: Value) {
    socket
        .send(tungstenite::Message::Text(value.to_string().into()))
        .await
        .unwrap();
}
async fn respond(socket: &mut Socket, command: &Value, value: Value) {
    send(
        socket,
        json!({"type":"result","id":command["id"],"outcome":{"type":"success","result":{"value":value,"files":[]}}}),
    )
    .await;
}

#[tokio::test]
async fn a_chrome_extension_host_serves_tools_through_the_server() {
    let server = chrome_server(haarness_server::auth::ServerAccess::Private).await;
    let client = reqwest::Client::new();
    let get = |path: &str| {
        client
            .get(format!("http://{}{path}", server.base))
            .bearer_auth("chrome-token")
            .send()
    };
    let info: Value = get("/v1/info").await.unwrap().json().await.unwrap();
    assert_eq!(info["pid"], std::process::id());
    // Browser pages always send Origin; the bridge never does.
    assert_eq!(
        refused_with(extension_socket(&server.base, "1", Some("http://localhost:9")).await),
        403
    );
    assert_eq!(
        refused_with(extension_socket(&server.base, "2", None).await),
        400
    );

    let thread = echo_thread(&server.runtime).await;
    let tab = format!("tab_{}", uuid::Uuid::now_v7());
    let mut socket = extension_socket(&server.base, "1", None).await.unwrap();
    let attached = receive(&mut socket).await;
    assert_eq!(attached["type"], "attached");
    assert_eq!(attached["version"], 1);
    assert_eq!(attached["server"]["pid"], std::process::id());
    assert_eq!(
        attached["server"]["id"],
        json!(server.runtime.events().server_instance_id())
    );
    assert!(attached["lease"].is_string());
    let browser = uuid::Uuid::now_v7().to_string();
    send(
        &mut socket,
        json!({"type":"hello","browser":{"id":browser,"kind":"chrome","name":"Google Chrome","label":"Work","version":"154.0.8037.98"},"extension":{"version":"0.1.1"},"paused":false,"stopped":false,"focusedAt":1,"future":"ignored"}),
    )
    .await;
    send(&mut socket, json!({"type":"novel","x":1})).await;
    send(
        &mut socket,
        json!({"type":"state","thread":thread.id,"state":{"tabs":[{"id":tab,"url":"https://example.test/","title":"Example","loading":false,"canGoBack":false,"canGoForward":false,"generation":3}],"focusedTabID":tab}}),
    )
    .await;
    send(&mut socket, json!({"type":"ping"})).await;
    assert_eq!(receive(&mut socket).await["type"], "pong");
    let hosts: Value = get(&format!("/v1/browser/hosts?thread={}", thread.id))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hosts["hosts"][0]["browserId"], browser);
    assert_eq!(hosts["hosts"][0]["label"], "Work");
    assert_eq!(hosts["hosts"][0]["threadTabs"], 1);
    assert_eq!(hosts["chrome"]["everConnected"], true);
    assert_eq!(hosts["chrome"]["defaultBrowser"], "auto");

    // A page tool: inspect, ask this chat, run.
    let tool = |name: &'static str, arguments: Value| {
        let client = client.clone();
        let url = format!(
            "http://{}/v1/threads/{}/browser/tool",
            server.base, thread.id
        );
        tokio::spawn(async move {
            client
                .post(url)
                .bearer_auth("chrome-token")
                .json(&json!({"name":name,"arguments":arguments}))
                .send()
                .await
                .unwrap()
        })
    };
    let call = tool("browser_snapshot", json!({"tabID":tab}));
    let inspect = receive(&mut socket).await;
    assert_eq!(inspect["type"], "command");
    assert_eq!(inspect["thread"], json!(thread.id));
    assert_eq!(inspect["title"], "Fixture");
    assert_eq!(inspect["parent"], Value::Null);
    assert_eq!(inspect["command"]["inspect"], true);
    respond(
        &mut socket,
        &inspect,
        json!({"resources":["https://example.test"],"key":"snapshot"}),
    )
    .await;
    let pending = explicit_prompt(&server.runtime, thread.id).await;
    assert!(pending.explicit);
    assert_eq!(
        pending.patterns,
        vec![format!(
            "chrome-browsing:{}:https://example.test",
            thread.id
        )]
    );
    answer(&server.runtime, &pending.permission_id, Reply::Always).await;
    let execute = receive(&mut socket).await;
    assert_eq!(execute["thread"], json!(thread.id));
    assert_eq!(execute["command"]["generation"], 3);
    assert_eq!(execute["command"]["policy"]["rememberApprovals"], true);
    respond(&mut socket, &execute, json!({"content":"Example page"})).await;
    let result: Value = call.await.unwrap().json().await.unwrap();
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(result["metadata"]["browser"], "chrome");

    // The chat's answer covers its next call; the tab list names browsers.
    let call = tool("browser_tabs_list", json!({}));
    let inspect = receive(&mut socket).await;
    respond(
        &mut socket,
        &inspect,
        json!({"resources":[],"key":"inventory"}),
    )
    .await;
    let execute = receive(&mut socket).await;
    respond(
        &mut socket,
        &execute,
        json!({"tabs":[{"id":tab,"url":"https://example.test/","title":"Example","loading":false,"canGoBack":false,"canGoForward":false,"generation":3}],"focusedTabID":tab}),
    )
    .await;
    let result: Value = call.await.unwrap().json().await.unwrap();
    let listed = &result["metadata"]["result"];
    assert_eq!(listed["tabs"][0]["browser"], "chrome");
    assert_eq!(listed["browsers"][0]["browser"], "haarness");
    assert_eq!(listed["browsers"][0]["available"], false);
    assert_eq!(listed["browsers"][1]["name"], "Google Chrome (Work)");
    assert_eq!(listed["browsers"][1]["available"], true);

    // Failure codes read as the server's own text.
    let call = tool("browser_snapshot", json!({"tabID":tab}));
    let inspect = receive(&mut socket).await;
    send(
        &mut socket,
        json!({"type":"result","id":inspect["id"],"outcome":{"type":"failure","code":"restricted-page","message":"raw"}}),
    )
    .await;
    let failed: Value = call.await.unwrap().json().await.unwrap();
    // A failed command is cancelled in case it is still running.
    let cancel = receive(&mut socket).await;
    assert_eq!(cancel, json!({"type":"cancel","id":inspect["id"]}));
    assert!(
        failed["message"]
            .as_str()
            .unwrap()
            .contains("Haarness cannot control this Chrome page"),
        "{failed}"
    );

    // Paused in Chrome: nothing is sent.
    send(
        &mut socket,
        json!({"type":"status","paused":true,"stopped":false,"focusedAt":2}),
    )
    .await;
    send(&mut socket, json!({"type":"ping"})).await;
    assert_eq!(receive(&mut socket).await["type"], "pong");
    let paused: Value = tool("browser_snapshot", json!({"tabID":tab}))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        paused["message"]
            .as_str()
            .unwrap()
            .contains("paused Haarness in Chrome"),
        "{paused}"
    );

    // Gone: the tab's browser says how to recover.
    socket.close(None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let hosts: Value = get("/v1/browser/hosts")
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hosts["hosts"], json!([]));
    let gone: Value = tool("browser_snapshot", json!({"tabID":tab}))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        gone["message"]
            .as_str()
            .unwrap()
            .contains("Chrome disconnected"),
        "{gone}"
    );
    // With no browser at all, the tab list says what is possible.
    let empty: Value = tool("browser_tabs_list", json!({}))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(empty["isError"], false, "{empty}");
    assert_eq!(empty["metadata"]["result"]["tabs"], json!([]));
    assert_eq!(
        empty["metadata"]["result"]["browsers"][1]["available"],
        false
    );
    server.handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn automatic_keeps_whatever_a_scheduled_run_starts_out_of_chrome() {
    let server = chrome_server(haarness_server::auth::ServerAccess::Private).await;
    let mut socket = extension_socket(&server.base, "1", None).await.unwrap();
    assert_eq!(receive(&mut socket).await["type"], "attached");
    send(
        &mut socket,
        json!({"type":"hello","browser":{"id":uuid::Uuid::now_v7().to_string(),"kind":"chrome","name":"Google Chrome","label":null,"version":"154.0.8037.98"},"extension":{"version":"0.1.1"},"paused":false,"stopped":false,"focusedAt":1}),
    )
    .await;
    send(&mut socket, json!({"type":"ping"})).await;
    assert_eq!(receive(&mut socket).await["type"], "pong");
    let client = reqwest::Client::new();
    let open = |thread: haarness_protocol::ThreadId| {
        let client = client.clone();
        let url = format!("http://{}/v1/threads/{thread}/browser/tool", server.base);
        tokio::spawn(async move {
            client
                .post(url)
                .bearer_auth("chrome-token")
                .json(
                    &json!({"name":"browser_tabs_open","arguments":{"url":"https://example.test"}}),
                )
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        })
    };
    // The run, the subagent it delegates to, a workflow that subagent
    // starts and the workflow's agent: none lands in the user's Chrome.
    let run = scheduled_thread(&server.runtime).await;
    let subagent = child_thread(&server.runtime, run.id).await;
    let workflow = child_thread(&server.runtime, subagent.id).await;
    let agent = child_thread(&server.runtime, workflow.id).await;
    for thread in [run.id, subagent.id, workflow.id, agent.id] {
        let refused = open(thread).await.unwrap();
        assert!(
            refused["message"]
                .as_str()
                .unwrap_or_default()
                .contains("This scheduled run has no browser it may use unattended."),
            "{refused}"
        );
    }
    send(&mut socket, json!({"type":"ping"})).await;
    assert_eq!(receive(&mut socket).await["type"], "pong");
    // An interactive chat's subagent still goes to Chrome.
    let chat = echo_thread(&server.runtime).await;
    let helper = child_thread(&server.runtime, chat.id).await;
    let call = open(helper.id);
    let inspect = receive(&mut socket).await;
    assert_eq!(inspect["type"], "command");
    assert_eq!(inspect["thread"], json!(helper.id));
    assert_eq!(inspect["parent"], json!(chat.id));
    send(
        &mut socket,
        json!({"type":"result","id":inspect["id"],"outcome":{"type":"failure","code":"restricted-page","message":"raw"}}),
    )
    .await;
    call.await.unwrap();
    server.handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn one_conversation_too_many_never_closes_chrome() {
    let server = chrome_server(haarness_server::auth::ServerAccess::Private).await;
    let mut socket = extension_socket(&server.base, "1", None).await.unwrap();
    assert_eq!(receive(&mut socket).await["type"], "attached");
    send(
        &mut socket,
        json!({"type":"hello","browser":{"id":uuid::Uuid::now_v7().to_string(),"kind":"chrome","name":"Google Chrome","label":null,"version":"154.0.8037.98"},"extension":{"version":"0.1.1"},"paused":false,"stopped":false,"focusedAt":1}),
    )
    .await;
    let state = |thread: haarness_protocol::ThreadId| {
        let tab = format!("tab_{}", uuid::Uuid::now_v7());
        json!({"type":"state","thread":thread,"state":{"tabs":[{"id":tab,"url":"https://example.test/","title":"Example","loading":false,"canGoBack":false,"canGoForward":false,"generation":1}],"focusedTabID":tab}})
    };
    for _ in 0..257 {
        send(&mut socket, state(haarness_protocol::ThreadId::new())).await;
    }
    // The connection every conversation shares is still up.
    send(&mut socket, json!({"type":"ping"})).await;
    assert_eq!(receive(&mut socket).await["type"], "pong");
    let hosts: Value = reqwest::Client::new()
        .get(format!("http://{}/v1/browser/hosts", server.base))
        .bearer_auth("chrome-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hosts["hosts"][0]["threads"], 256, "{hosts}");
    assert_eq!(hosts["chrome"]["globalEnabled"], true);
    assert_eq!(hosts["chrome"]["blockedBy"], Value::Null);
    server.handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn chrome_extension_hosts_need_a_hello_and_a_token() {
    let server = chrome_server(haarness_server::auth::ServerAccess::Private).await;
    let mut socket = extension_socket(&server.base, "1", None).await.unwrap();
    assert_eq!(receive(&mut socket).await["type"], "attached");
    // Anything but a valid hello first closes the connection.
    send(&mut socket, json!({"type":"hello","browser":{"id":"not-a-uuid","kind":"chrome","name":"Chrome","label":null,"version":"1"},"extension":{"version":"1"}})).await;
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match socket.next().await {
                Some(Ok(tungstenite::Message::Close(_)) | Err(_)) | None => return true,
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(closed);
    server.handle.shutdown().await.unwrap();
    let open = chrome_server(haarness_server::auth::ServerAccess::Open).await;
    assert_eq!(
        refused_with(extension_socket(&open.base, "1", None).await),
        403
    );
    open.handle.shutdown().await.unwrap();
}
