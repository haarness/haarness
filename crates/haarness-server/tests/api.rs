use async_trait::async_trait;
use fs2::FileExt as _;
use futures::{StreamExt, stream};
use haarness_core::{
    AgentRuntime, CoreError, CoreResult, EmptyToolRegistry, EventBus, McpRegistry, ModelProvider,
    ProviderEvent, ProviderMessage, ProviderRequest, ProviderStream, RuntimeConfig, SkillRegistry,
    StaticProviderRegistry, ThreadRepository, Tool, ToolContext, ToolDefinition, ToolOutput,
    ToolRegistry,
};
use haarness_protocol::{
    CreateThreadRequest, GoalActionRequest, GoalStatus, McpConfigureRequest, McpServerInfo,
    ModelInfo, ProviderInfo, RequestId, ServerInfo, SkillInfo, StartTurnRequest, Thread, Turn,
    TurnStatus, WORKSPACE_CLEANUP_CAPABILITY, WORKSPACE_HISTORY_CAPABILITY, WorkflowRun,
    WorkflowRunStatus,
};
use haarness_server::{NativeToolRegistry, ServerState, WorkflowRuntimeAdapter, serve};
use haarness_store::SqliteStore;
use haarness_workflow_catalog::WorkflowCatalog;
use haarness_workflows::{ExecutorConfig, WorkflowCoordinator, WorkflowExecutor};
use haarness_workspaces::WorkspaceManager;
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[path = "support/content_filter.rs"]
mod content_filter;
#[path = "support/reliability.rs"]
mod reliability;

fn git_fixture_command<'a>(directory: &std::path::Path, args: impl IntoIterator<Item = &'a str>) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// These fixtures script main-turn requests. Auxiliary titles must not consume
// their main-stream counters or gates; dedicated title tests exercise that path.
fn title_fixture(request: &ProviderRequest) -> Option<CoreResult<ProviderStream>> {
    if request.system != haarness_core::system_prompt_for("title") {
        return None;
    }
    let text = request
        .messages
        .iter()
        .find_map(|message| match message {
            ProviderMessage::Text { role, content } if role == "user" => Some(content.as_str()),
            _ => None,
        })
        .unwrap_or("Conversation");
    Some(Ok(Box::pin(stream::iter(vec![
        Ok(ProviderEvent::TextDelta(format!("Echo: {text}"))),
        Ok(ProviderEvent::Finished {
            reason: "completed".into(),
        }),
    ]))))
}

struct Echo;
#[async_trait]
impl ModelProvider for Echo {
    fn id(&self) -> &str {
        "echo"
    }
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "echo".into(),
            name: "Echo".into(),
            authenticated: true,
            models: vec![ModelInfo {
                id: "echo-1".into(),
                display_name: "Echo".into(),
                reasoning: false,
                reasoning_efforts: Vec::new(),
                images: false,
                video: false,
                tools: false,
                context_window: None,
                max_output_tokens: None,
                pricing: None,
            }],
        }
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if let Some(title) = title_fixture(&request) {
            return title;
        }
        let prompt = request
            .messages
            .into_iter()
            .rev()
            .find_map(|message| match message {
                ProviderMessage::Text { role, content } if role == "user" => Some(content),
                _ => None,
            })
            .unwrap();
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderEvent::TextDelta(format!("Echo: {prompt}"))),
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }),
        ])))
    }
}

/// Exercises the agent loop beyond Haarness's former hidden 32-round cap.
/// The missing tool is intentional: failed tools still need to be returned to
/// the model and must not turn a long, recoverable session into a provider
/// failure.
#[derive(Clone, Default)]
struct LongToolLoop {
    rounds: Arc<std::sync::atomic::AtomicUsize>,
    guarded: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl ModelProvider for LongToolLoop {
    fn id(&self) -> &str {
        "echo"
    }

    fn info(&self) -> ProviderInfo {
        let mut info = Echo.info();
        info.models[0].tools = true;
        info
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        use std::sync::atomic::Ordering;

        if let Some(title) = title_fixture(&request) {
            return title;
        }

        let round = self.rounds.fetch_add(1, Ordering::SeqCst) + 1;
        let guard = request.messages.iter().any(|message| {
            matches!(message, ProviderMessage::Text { role, content }
                if role == "user"
                    && content.starts_with("The configured maximum number of tool steps"))
        });
        let mut events = if guard {
            if !request.tools.is_empty() {
                return Err(CoreError::Invalid(
                    "tool-step guard request still exposed ordinary tools".into(),
                ));
            }
            self.guarded.store(true, Ordering::SeqCst);
            vec![Ok(ProviderEvent::TextDelta(
                "Finished cleanly at the configured guard".into(),
            ))]
        } else if round <= 40 {
            vec![Ok(ProviderEvent::ToolCall {
                call_id: format!("long-loop-{round}"),
                name: "missing_fixture_tool".into(),
                arguments: serde_json::json!({"round": round}),
                provider_metadata: Value::Null,
            })]
        } else {
            vec![Ok(ProviderEvent::TextDelta(
                "Recovered after forty tool rounds".into(),
            ))]
        };
        events.push(Ok(ProviderEvent::Finished {
            reason: "done".into(),
        }));
        Ok(Box::pin(stream::iter(events)))
    }
}
/// Records every `ProviderRequest` it receives and speaks the scripted echo
/// dialect: `!tool <name> <json>` emits a tool call on the first round and a
/// completion text once a tool result is present. `!preface` additionally
/// emits assistant prose before that first-round call.
#[derive(Clone, Default)]
struct Capturing {
    requests: Arc<std::sync::Mutex<Vec<ProviderRequest>>>,
    reasoning_efforts: Vec<String>,
}

impl Capturing {
    fn with_reasoning_efforts(efforts: &[&str]) -> Self {
        Self {
            reasoning_efforts: efforts.iter().map(|effort| (*effort).to_owned()).collect(),
            ..Self::default()
        }
    }
}
#[async_trait]
impl ModelProvider for Capturing {
    fn id(&self) -> &str {
        "echo"
    }
    fn info(&self) -> ProviderInfo {
        let mut info = Echo.info();
        let model = &mut info.models[0];
        model.reasoning = !self.reasoning_efforts.is_empty();
        model.reasoning_efforts.clone_from(&self.reasoning_efforts);
        info
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if let Some(title) = title_fixture(&request) {
            return title;
        }
        self.requests.lock().unwrap().push(request.clone());
        let mut last_user_index = None;
        let mut last_tool_result = None;
        for (index, message) in request.messages.iter().enumerate() {
            match message {
                ProviderMessage::Text { role, .. } if role == "user" => {
                    last_user_index = Some(index);
                }
                ProviderMessage::ToolResult { .. } => last_tool_result = Some(index),
                _ => {}
            }
        }
        let prompt = last_user_index
            .and_then(|index| match &request.messages[index] {
                ProviderMessage::Text { content, .. } => Some(content.clone()),
                _ => None,
            })
            .unwrap_or_default();
        // Goal doctrine is prefixed inside the latest user message to preserve
        // provider role alternation. The fixture still recognizes the
        // human-authored directive that follows the hidden envelope.
        let scripted_prompt = prompt
            .rsplit_once("</goal-harness>\n\n")
            .map_or(prompt.as_str(), |(_, suffix)| suffix);
        let events = if let Some(script) = scripted_prompt.trim_start().strip_prefix("!preface ") {
            if last_tool_result > last_user_index {
                vec![Ok(ProviderEvent::TextDelta("Tool finished".into()))]
            } else {
                let (name, rest) = script
                    .trim()
                    .split_once(char::is_whitespace)
                    .unwrap_or((script.trim(), ""));
                let arguments: Value =
                    serde_json::from_str(rest.trim()).unwrap_or(serde_json::json!({}));
                vec![
                    Ok(ProviderEvent::ReasoningDelta("Inspecting".into())),
                    Ok(ProviderEvent::TextDelta(String::new())),
                    Ok(ProviderEvent::ReasoningDelta(" safely".into())),
                    Ok(ProviderEvent::TextDelta(
                        "I’ll inspect the repository first.".into(),
                    )),
                    Ok(ProviderEvent::ToolCall {
                        call_id: "cap-call-1".into(),
                        name: name.to_owned(),
                        arguments,
                        provider_metadata: Value::Null,
                    }),
                ]
            }
        } else if let Some(script) = scripted_prompt.trim_start().strip_prefix("!tool ") {
            if last_tool_result > last_user_index {
                vec![Ok(ProviderEvent::TextDelta("Tool finished".into()))]
            } else {
                let (name, rest) = script
                    .trim()
                    .split_once(char::is_whitespace)
                    .unwrap_or((script.trim(), ""));
                let arguments: Value =
                    serde_json::from_str(rest.trim()).unwrap_or(serde_json::json!({}));
                vec![Ok(ProviderEvent::ToolCall {
                    call_id: "cap-call-1".into(),
                    name: name.to_owned(),
                    arguments,
                    provider_metadata: Value::Null,
                })]
            }
        } else if let Some(script) = scripted_prompt.trim_start().strip_prefix("!multi ") {
            // Emit several tool calls in one assistant round:
            // `!multi [{"name":...,"arguments":{...}}, ...]`
            if last_tool_result > last_user_index {
                vec![Ok(ProviderEvent::TextDelta("All tools finished".into()))]
            } else {
                let calls: Vec<Value> = serde_json::from_str(script.trim()).unwrap();
                calls
                    .iter()
                    .enumerate()
                    .map(|(index, call)| {
                        Ok(ProviderEvent::ToolCall {
                            call_id: format!("cap-call-{}", index + 1),
                            name: call["name"].as_str().unwrap().to_owned(),
                            arguments: call["arguments"].clone(),
                            provider_metadata: Value::Null,
                        })
                    })
                    .collect()
            }
        } else {
            vec![Ok(ProviderEvent::TextDelta(format!("Echo: {prompt}")))]
        };
        let mut events = events;
        events.push(Ok(ProviderEvent::Finished {
            reason: "done".into(),
        }));
        Ok(Box::pin(stream::iter(events)))
    }
}

/// Holds the first text-only response open after its provider-request
/// snapshot has been captured. A queued message can then be inserted at a
/// deterministic point in the stream and must trigger a second round.
#[derive(Clone)]
struct SlowText {
    requests: Arc<std::sync::Mutex<Vec<ProviderRequest>>>,
    first_started: Arc<tokio::sync::Semaphore>,
    release_first: Arc<tokio::sync::Semaphore>,
}

impl Default for SlowText {
    fn default() -> Self {
        Self {
            requests: Arc::new(std::sync::Mutex::new(Vec::new())),
            first_started: Arc::new(tokio::sync::Semaphore::new(0)),
            release_first: Arc::new(tokio::sync::Semaphore::new(0)),
        }
    }
}

#[async_trait]
impl ModelProvider for SlowText {
    fn id(&self) -> &str {
        "echo"
    }

    fn info(&self) -> ProviderInfo {
        Echo.info()
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if let Some(title) = title_fixture(&request) {
            return title;
        }
        let request_number = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request);
            requests.len()
        };
        if request_number == 1 {
            self.first_started.add_permits(1);
            let release = Arc::clone(&self.release_first);
            let delayed_text = stream::once(async move {
                let permit = release
                    .acquire_owned()
                    .await
                    .map_err(|_| CoreError::Provider("slow-stream fixture closed".into()))?;
                permit.forget();
                Ok(ProviderEvent::TextDelta("First response".into()))
            });
            let finished = stream::iter(vec![Ok(ProviderEvent::Finished {
                reason: "done".into(),
            })]);
            Ok(Box::pin(delayed_text.chain(finished)))
        } else {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::TextDelta("Follow-up response".into())),
                Ok(ProviderEvent::Finished {
                    reason: "done".into(),
                }),
            ])))
        }
    }
}

#[derive(Clone)]
struct BlockingWriteTool {
    started: Arc<tokio::sync::Semaphore>,
}

#[async_trait]
impl Tool for BlockingWriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "blocking_write".into(),
            description: "Write a fixture file, then wait for cancellation".into(),
            input_schema: serde_json::json!({"type":"object","additionalProperties":false}),
        }
    }

    fn side_effectful(&self) -> bool {
        true
    }

    async fn execute(&self, _arguments: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        tokio::fs::write(context.workspace.join("active-write.txt"), "written\n")
            .await
            .unwrap();
        self.started.add_permits(1);
        context.cancel.cancelled().await;
        Err(CoreError::Cancelled)
    }
}

struct BlockingWriteRegistry(Arc<BlockingWriteTool>);

impl ToolRegistry for BlockingWriteRegistry {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        (name == "blocking_write").then(|| self.0.clone() as Arc<dyn Tool>)
    }

    fn list(&self) -> Vec<ToolDefinition> {
        vec![self.0.definition()]
    }
}

#[derive(Default)]
struct WorkflowMarker;

#[async_trait]
impl Tool for WorkflowMarker {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "workflow".into(),
            description: "workflow marker".into(),
            input_schema: serde_json::json!({"type":"object"}),
        }
    }

    fn side_effectful(&self) -> bool {
        false
    }

    async fn execute(&self, _arguments: Value, _context: ToolContext) -> CoreResult<ToolOutput> {
        Ok(ToolOutput {
            text: "workflow marker executed".into(),
            metadata: Value::Null,
        })
    }
}

struct WorkflowAdvertisingRegistry {
    native: NativeToolRegistry,
    workflow: Arc<WorkflowMarker>,
}

impl WorkflowAdvertisingRegistry {
    fn new(workspace: &std::path::Path) -> Self {
        Self {
            native: NativeToolRegistry::new(workspace.to_path_buf()),
            workflow: Arc::new(WorkflowMarker),
        }
    }
}

impl ToolRegistry for WorkflowAdvertisingRegistry {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if name == "workflow" {
            Some(self.workflow.clone())
        } else {
            self.native.get(name)
        }
    }

    fn list(&self) -> Vec<ToolDefinition> {
        let mut tools = self.native.list();
        tools.push(self.workflow.definition());
        tools
    }
}

/// Forces auto-compaction after a large tool-producing response, then holds
/// the summarizer open. Queued input can land in the exact window between the
/// summary snapshot and a possible Compaction commit.
#[derive(Clone)]
struct CompactionQueueRace {
    requests: Arc<std::sync::Mutex<Vec<ProviderRequest>>>,
    summary_started: Arc<tokio::sync::Semaphore>,
    release_summary: Arc<tokio::sync::Semaphore>,
    main_rounds: Arc<std::sync::Mutex<usize>>,
    blocked_main_round: Option<usize>,
    main_started: Arc<tokio::sync::Semaphore>,
    release_main: Arc<tokio::sync::Semaphore>,
}

impl Default for CompactionQueueRace {
    fn default() -> Self {
        Self {
            requests: Arc::new(std::sync::Mutex::new(Vec::new())),
            summary_started: Arc::new(tokio::sync::Semaphore::new(0)),
            release_summary: Arc::new(tokio::sync::Semaphore::new(0)),
            main_rounds: Arc::new(std::sync::Mutex::new(0)),
            blocked_main_round: None,
            main_started: Arc::new(tokio::sync::Semaphore::new(0)),
            release_main: Arc::new(tokio::sync::Semaphore::new(0)),
        }
    }
}

impl CompactionQueueRace {
    fn blocking_main_round(round: usize) -> Self {
        Self {
            blocked_main_round: Some(round),
            ..Self::default()
        }
    }
}

fn is_compaction_request(request: &ProviderRequest) -> bool {
    matches!(
        request.messages.last(),
        Some(ProviderMessage::Text { role, content })
            if role == "user"
                && content.starts_with("Summarize this conversation for continuation:")
    )
}

#[derive(Clone, Default)]
struct OverflowOnce {
    requests: Arc<std::sync::Mutex<Vec<ProviderRequest>>>,
    main_rounds: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl ModelProvider for OverflowOnce {
    fn id(&self) -> &str {
        "overflow"
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "overflow".into(),
            name: "Overflow fixture".into(),
            authenticated: true,
            models: vec![ModelInfo {
                id: "overflow-1".into(),
                display_name: "Overflow fixture".into(),
                reasoning: false,
                reasoning_efforts: Vec::new(),
                images: true,
                video: false,
                tools: false,
                context_window: Some(1_000_000),
                max_output_tokens: Some(8_192),
                pricing: None,
            }],
        }
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if let Some(title) = title_fixture(&request) {
            return title;
        }
        let compacting = is_compaction_request(&request);
        self.requests.lock().unwrap().push(request);
        if compacting {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::TextDelta("overflow-safe summary".into())),
                Ok(ProviderEvent::Finished {
                    reason: "done".into(),
                }),
            ])));
        }
        let round = self
            .main_rounds
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if round == 0 {
            return Ok(Box::pin(stream::iter(vec![Err(CoreError::Provider(
                "HTTP 413 Payload Too Large: maximum context length exceeded".into(),
            ))])));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderEvent::TextDelta(
                "recovered after compaction".into(),
            )),
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }),
        ])))
    }
}

#[derive(Clone, Default)]
struct ModelAwareCompaction {
    requests: Arc<std::sync::Mutex<Vec<ProviderRequest>>>,
}

#[async_trait]
impl ModelProvider for ModelAwareCompaction {
    fn id(&self) -> &str {
        "model-aware"
    }

    fn info(&self) -> ProviderInfo {
        let model = |id: &str, context_window, max_output_tokens| ModelInfo {
            id: id.into(),
            display_name: id.into(),
            reasoning: false,
            reasoning_efforts: Vec::new(),
            images: false,
            video: false,
            tools: false,
            context_window: Some(context_window),
            max_output_tokens: Some(max_output_tokens),
            pricing: None,
        };
        ProviderInfo {
            id: "model-aware".into(),
            name: "Model-aware fixture".into(),
            authenticated: true,
            models: vec![model("small", 1_000, 200), model("large", 10_000, 2_000)],
        }
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if let Some(title) = title_fixture(&request) {
            return title;
        }
        let compacting = is_compaction_request(&request);
        self.requests.lock().unwrap().push(request);
        let text = if compacting {
            "model-aware summary"
        } else {
            "normal response"
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderEvent::TextDelta(text.into())),
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }),
        ])))
    }
}

#[async_trait]
impl ModelProvider for CompactionQueueRace {
    fn id(&self) -> &str {
        "echo"
    }

    fn info(&self) -> ProviderInfo {
        let mut info = Echo.info();
        info.models[0].context_window = Some(256);
        info.models[0].tools = true;
        info
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if let Some(title) = title_fixture(&request) {
            return title;
        }
        let compaction = is_compaction_request(&request);
        self.requests.lock().unwrap().push(request);
        if compaction {
            self.summary_started.add_permits(1);
            let release = Arc::clone(&self.release_summary);
            let delayed_summary = stream::once(async move {
                let permit = release
                    .acquire_owned()
                    .await
                    .map_err(|_| CoreError::Provider("compaction fixture closed".into()))?;
                permit.forget();
                Ok(ProviderEvent::TextDelta("lossy summary".into()))
            });
            let finished = stream::iter(vec![Ok(ProviderEvent::Finished {
                reason: "done".into(),
            })]);
            return Ok(Box::pin(delayed_summary.chain(finished)));
        }

        let main_round = {
            let mut rounds = self.main_rounds.lock().unwrap();
            *rounds += 1;
            *rounds
        };
        if self.blocked_main_round == Some(main_round) {
            self.main_started.add_permits(1);
            let release = Arc::clone(&self.release_main);
            let delayed_text = stream::once(async move {
                let permit = release
                    .acquire_owned()
                    .await
                    .map_err(|_| CoreError::Provider("main-stream fixture closed".into()))?;
                permit.forget();
                Ok(ProviderEvent::TextDelta("Blocked main response".into()))
            });
            let finished = stream::iter(vec![Ok(ProviderEvent::Finished {
                reason: "done".into(),
            })]);
            return Ok(Box::pin(delayed_text.chain(finished)));
        }
        let mut events = if main_round == 1 {
            vec![
                Ok(ProviderEvent::TextDelta("x".repeat(4_096))),
                Ok(ProviderEvent::ToolCall {
                    call_id: "compaction-get-goal".into(),
                    name: "get_goal".into(),
                    arguments: serde_json::json!({}),
                    provider_metadata: Value::Null,
                }),
            ]
        } else {
            vec![Ok(ProviderEvent::TextDelta(
                "Main follow-up processed exact steering".into(),
            ))]
        };
        events.push(Ok(ProviderEvent::Finished {
            reason: "done".into(),
        }));
        Ok(Box::pin(stream::iter(events)))
    }
}

/// Holds the initial response open so a real queued steering message starts
/// the next logical goal turn. That turn performs one tool round, letting the
/// test inspect both its call request and tool-result follow-up request.
#[derive(Clone)]
struct QueuedGoalToolRound {
    requests: Arc<std::sync::Mutex<Vec<ProviderRequest>>>,
    first_started: Arc<tokio::sync::Semaphore>,
    release_first: Arc<tokio::sync::Semaphore>,
}

impl Default for QueuedGoalToolRound {
    fn default() -> Self {
        Self {
            requests: Arc::new(std::sync::Mutex::new(Vec::new())),
            first_started: Arc::new(tokio::sync::Semaphore::new(0)),
            release_first: Arc::new(tokio::sync::Semaphore::new(0)),
        }
    }
}

#[async_trait]
impl ModelProvider for QueuedGoalToolRound {
    fn id(&self) -> &str {
        "echo"
    }

    fn info(&self) -> ProviderInfo {
        Echo.info()
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if let Some(title) = title_fixture(&request) {
            return title;
        }
        let request_number = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request);
            requests.len()
        };
        if request_number == 1 {
            self.first_started.add_permits(1);
            let release = Arc::clone(&self.release_first);
            let delayed_text = stream::once(async move {
                let permit = release
                    .acquire_owned()
                    .await
                    .map_err(|_| CoreError::Provider("queued-goal fixture closed".into()))?;
                permit.forget();
                Ok(ProviderEvent::TextDelta("Initial response".into()))
            });
            let finished = stream::iter(vec![Ok(ProviderEvent::Finished {
                reason: "done".into(),
            })]);
            return Ok(Box::pin(delayed_text.chain(finished)));
        }
        let mut events = if request_number == 2 {
            vec![
                Ok(ProviderEvent::TextDelta(
                    "Checking the queued goal status.".into(),
                )),
                Ok(ProviderEvent::ToolCall {
                    call_id: "queued-goal-status".into(),
                    name: "get_goal".into(),
                    arguments: serde_json::json!({}),
                    provider_metadata: Value::Null,
                }),
            ]
        } else {
            vec![Ok(ProviderEvent::TextDelta(format!(
                "Follow-up response {request_number}"
            )))]
        };
        events.push(Ok(ProviderEvent::Finished {
            reason: "done".into(),
        }));
        Ok(Box::pin(stream::iter(events)))
    }
}

/// The first response ends normally; the autonomous continuation then calls a
/// tool. Its result follow-up exercises the synthetic doctrine boundary rather
/// than the ordinary user-message prefix path.
#[derive(Clone, Default)]
struct ContinuationGoalToolRound {
    requests: Arc<std::sync::Mutex<Vec<ProviderRequest>>>,
}

#[async_trait]
impl ModelProvider for ContinuationGoalToolRound {
    fn id(&self) -> &str {
        "echo"
    }

    fn info(&self) -> ProviderInfo {
        Echo.info()
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if let Some(title) = title_fixture(&request) {
            return title;
        }
        let request_number = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request);
            requests.len()
        };
        let mut events = match request_number {
            1 => vec![Ok(ProviderEvent::TextDelta("Initial response".into()))],
            2 => vec![
                Ok(ProviderEvent::TextDelta(
                    "Checking during the continuation.".into(),
                )),
                Ok(ProviderEvent::ToolCall {
                    call_id: "continuation-goal-status".into(),
                    name: "get_goal".into(),
                    arguments: serde_json::json!({}),
                    provider_metadata: Value::Null,
                }),
            ],
            _ => vec![Ok(ProviderEvent::TextDelta(
                "Continuation tool finished".into(),
            ))],
        };
        events.push(Ok(ProviderEvent::Finished {
            reason: "done".into(),
        }));
        Ok(Box::pin(stream::iter(events)))
    }
}

/// Every logical goal turn does exactly one `get_goal` call and then emits a
/// text response. This is the provider-level shape of the runaway transcript
/// we guard against: status polling is not autonomous progress.
#[derive(Clone, Default)]
struct GoalIntrospectionOnly {
    requests: Arc<std::sync::Mutex<Vec<ProviderRequest>>>,
}

/// Reads canonical status on every logical turn but reports a distinct
/// checkpoint each time. Status inspection plus changing output is not the
/// same stalled turn and must not trip the repeat breaker.
#[derive(Clone, Default)]
struct GoalIntrospectionWithProgress {
    requests: Arc<std::sync::Mutex<Vec<ProviderRequest>>>,
}

/// Repeats the same missing-tool failure and assistant response. Failed calls
/// are observations, not progress, so they remain eligible for loop breaking.
#[derive(Clone, Default)]
struct GoalRepeatedToolFailure {
    requests: Arc<std::sync::Mutex<Vec<ProviderRequest>>>,
}

/// Reproduces the reported failure without relying on a live model: the first
/// logical turn reads the goal and stops, the bootstrap retry asks a real
/// question, then ends with a plain-text request instead of proposing
/// criteria. Once the user answers, Haarness must settle instead of spinning.
#[derive(Clone, Default)]
struct GoalQuestionThenWaits {
    requests: Arc<std::sync::Mutex<Vec<ProviderRequest>>>,
}

#[async_trait]
impl ModelProvider for GoalQuestionThenWaits {
    fn id(&self) -> &str {
        "echo"
    }

    fn info(&self) -> ProviderInfo {
        Echo.info()
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if let Some(title) = title_fixture(&request) {
            return title;
        }
        let request_number = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request);
            requests.len()
        };
        let event = match request_number {
            1 => ProviderEvent::ToolCall {
                call_id: "goal-question-status".into(),
                name: "get_goal".into(),
                arguments: serde_json::json!({}),
                provider_metadata: Value::Null,
            },
            2 => ProviderEvent::TextDelta("Hi! What would you like help with?".into()),
            3 => ProviderEvent::ToolCall {
                call_id: "goal-question-user".into(),
                name: "question".into(),
                arguments: serde_json::json!({
                    "prompt": "What issue are you seeing?",
                    "choices": ["Share an error message"]
                }),
                provider_metadata: Value::Null,
            },
            _ => ProviderEvent::TextDelta(
                "Please paste the full error message and the triggering command.".into(),
            ),
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(event),
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }),
        ])))
    }
}

#[async_trait]
impl ModelProvider for GoalIntrospectionOnly {
    fn id(&self) -> &str {
        "echo"
    }

    fn info(&self) -> ProviderInfo {
        Echo.info()
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if let Some(title) = title_fixture(&request) {
            return title;
        }
        let request_number = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request.clone());
            requests.len()
        };
        let latest_user = request.messages.iter().rposition(
            |message| matches!(message, ProviderMessage::Text { role, .. } if role == "user"),
        );
        let answered_after_latest_user = request.messages.iter().enumerate().any(|(index, message)| {
            latest_user.is_some_and(|user| index > user)
                && matches!(message, ProviderMessage::ToolResult { name, .. } if name == "get_goal")
        });
        let event = if answered_after_latest_user {
            ProviderEvent::TextDelta("Please provide more direction.".into())
        } else {
            ProviderEvent::ToolCall {
                call_id: format!("goal-status-{request_number}"),
                name: "get_goal".into(),
                arguments: serde_json::json!({}),
                provider_metadata: Value::Null,
            }
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(event),
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }),
        ])))
    }
}

#[async_trait]
impl ModelProvider for GoalIntrospectionWithProgress {
    fn id(&self) -> &str {
        "echo"
    }

    fn info(&self) -> ProviderInfo {
        Echo.info()
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if let Some(title) = title_fixture(&request) {
            return title;
        }
        let request_number = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request.clone());
            requests.len()
        };
        let latest_user = request.messages.iter().rposition(
            |message| matches!(message, ProviderMessage::Text { role, .. } if role == "user"),
        );
        let answered_after_latest_user = request.messages.iter().enumerate().any(
            |(index, message)| {
                latest_user.is_some_and(|user| index > user)
                    && matches!(message, ProviderMessage::ToolResult { name, .. } if name == "get_goal")
            },
        );
        let event = if answered_after_latest_user {
            ProviderEvent::TextDelta(format!(
                "Finished distinct checkpoint {}.",
                request_number / 2
            ))
        } else {
            ProviderEvent::ToolCall {
                call_id: format!("goal-progress-status-{request_number}"),
                name: "get_goal".into(),
                arguments: serde_json::json!({}),
                provider_metadata: Value::Null,
            }
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(event),
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }),
        ])))
    }
}

#[async_trait]
impl ModelProvider for GoalRepeatedToolFailure {
    fn id(&self) -> &str {
        "echo"
    }

    fn info(&self) -> ProviderInfo {
        Echo.info()
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if let Some(title) = title_fixture(&request) {
            return title;
        }
        let request_number = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request.clone());
            requests.len()
        };
        let latest_user = request.messages.iter().rposition(
            |message| matches!(message, ProviderMessage::Text { role, .. } if role == "user"),
        );
        let failed_after_latest_user = request.messages.iter().enumerate().any(
            |(index, message)| {
                latest_user.is_some_and(|user| index > user)
                    && matches!(message, ProviderMessage::ToolResult { name, .. } if name == "missing_goal_tool")
            },
        );
        let event = if failed_after_latest_user {
            ProviderEvent::TextDelta("The same tool is still unavailable.".into())
        } else {
            ProviderEvent::ToolCall {
                call_id: format!("goal-failed-tool-{request_number}"),
                name: "missing_goal_tool".into(),
                arguments: serde_json::json!({"path":"same"}),
                provider_metadata: Value::Null,
            }
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(event),
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }),
        ])))
    }
}

async fn wait_done(runtime: &AgentRuntime, turn: &Turn) -> Turn {
    for _ in 0..1_000 {
        let current = runtime.turn(turn.id).await.unwrap();
        if !matches!(current.status, TurnStatus::Queued | TurnStatus::Running) {
            return current;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("turn did not finish");
}

/// Idle with nothing queued. Each queued prompt starts as its own turn just
/// after the previous one closes, so idle alone can be momentary.
async fn wait_drained(runtime: &AgentRuntime, thread_id: haarness_protocol::ThreadId) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while runtime.active_turn(thread_id).await.unwrap().is_some()
            || !runtime
                .input_queue(thread_id)
                .await
                .unwrap()
                .items
                .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("queue drains");
}

async fn wait_idle(runtime: &AgentRuntime, thread_id: haarness_protocol::ThreadId) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while runtime.active_turn(thread_id).await.unwrap().is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("session becomes idle");
}

#[tokio::test]
async fn interrupt_cancels_a_silent_provider_stream_without_waiting_for_an_event() {
    let provider = SlowText::default();
    let runtime = AgentRuntime::new(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Interrupt silent provider".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request("wait forever"))
        .await
        .unwrap();
    let started = tokio::time::timeout(Duration::from_secs(1), provider.first_started.acquire())
        .await
        .expect("provider starts")
        .expect("test semaphore remains open");
    started.forget();

    assert_eq!(
        tokio::time::timeout(Duration::from_millis(250), runtime.interrupt(turn.id))
            .await
            .expect("interrupt persistence is prompt")
            .unwrap()
            .status,
        TurnStatus::Interrupted
    );
    tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            if runtime.active_turn(thread.id).await.unwrap().is_none() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("silent provider runner observes cancellation immediately");
}

#[tokio::test]
async fn undo_rejects_an_active_text_turn_without_cancelling_it() {
    let root = std::env::temp_dir().join(format!(
        "haarness-active-text-undo-{}",
        uuid::Uuid::now_v7()
    ));
    let workspace = root.join("ws");
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    let database = root.join("db.sqlite");
    let store = Arc::new(SqliteStore::open(&database).unwrap());
    let snapshots =
        Arc::new(haarness_store::SqliteSnapshotStore::open(&database, workspace.clone()).unwrap());
    let provider = SlowText::default();
    let runtime = AgentRuntime::with_config(
        store,
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(workspace.clone())),
        EventBus::new(64),
        workspace.clone(),
        String::new(),
        {
            let mut config = RuntimeConfig::default();
            config.base_rules.push(haarness_core::PermissionRule {
                pattern: "write:*".into(),
                decision: haarness_core::PermissionDecision::Allow,
            });
            config
        },
    )
    .with_snapshots(snapshots);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Active text undo".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .run_direct_tool(
            thread.id,
            "write",
            serde_json::json!({"path":"keep.txt","content":"keep\n"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request("cancel this text turn"))
        .await
        .unwrap();
    let started = tokio::time::timeout(Duration::from_secs(1), provider.first_started.acquire())
        .await
        .expect("provider starts")
        .expect("test semaphore stays open");
    started.forget();
    let user_boundary = runtime
        .items(thread.id, None)
        .await
        .unwrap()
        .into_iter()
        .find(|item| {
            item.turn_id == Some(turn.id)
                && matches!(
                    item.data,
                    haarness_protocol::ItemData::UserMessage {
                        synthetic: false,
                        ..
                    }
                )
        })
        .unwrap()
        .sequence
        .saturating_sub(1);

    let error = tokio::time::timeout(Duration::from_millis(250), runtime.revert(thread.id, None))
        .await
        .expect("busy undo is rejected promptly")
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot revert while a turn is running"),
        "{error}"
    );
    assert_eq!(
        runtime.turn(turn.id).await.unwrap().status,
        TurnStatus::Running,
        "undo does not double as an implicit stop"
    );
    runtime.interrupt_thread_tree(thread.id).await.unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Interrupted
    );
    wait_idle(&runtime, thread.id).await;
    let reverted = runtime.revert(thread.id, None).await.unwrap();
    assert_eq!(
        reverted.revert.as_ref().unwrap().sequence,
        user_boundary,
        "the cancelled user turn, not the previous mutation, owns the undo boundary"
    );
    assert_eq!(
        tokio::fs::read_to_string(workspace.join("keep.txt"))
            .await
            .unwrap(),
        "keep\n",
        "a no-op active turn must not roll back an older file change"
    );
    let last = runtime.diff_last_turn(thread.id).await.unwrap();
    assert_eq!(
        last.iter()
            .map(|file| file.file.as_str())
            .collect::<Vec<_>>(),
        ["keep.txt"],
        "the undone text turn changed nothing, so the last change is the earlier write"
    );
    runtime.unrevert(thread.id).await.unwrap();
    assert!(workspace.join("keep.txt").exists());
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn undo_rejects_an_active_mutation_until_explicitly_stopped() {
    let root = std::env::temp_dir().join(format!(
        "haarness-active-write-undo-{}",
        uuid::Uuid::now_v7()
    ));
    let workspace = root.join("ws");
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    let database = root.join("db.sqlite");
    let store = Arc::new(SqliteStore::open(&database).unwrap());
    let snapshots =
        Arc::new(haarness_store::SqliteSnapshotStore::open(&database, workspace.clone()).unwrap());
    let tool = Arc::new(BlockingWriteTool {
        started: Arc::new(tokio::sync::Semaphore::new(0)),
    });
    let runtime = AgentRuntime::with_config(
        store,
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(BlockingWriteRegistry(tool.clone())),
        EventBus::new(64),
        workspace.clone(),
        String::new(),
        {
            let mut config = RuntimeConfig::default();
            config.base_rules.push(haarness_core::PermissionRule {
                pattern: "blocking_write:*".into(),
                decision: haarness_core::PermissionDecision::Allow,
            });
            config
        },
    )
    .with_snapshots(snapshots);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Active mutation undo".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request(r"!tool blocking_write {}"))
        .await
        .unwrap();
    let started = tokio::time::timeout(Duration::from_secs(1), tool.started.acquire())
        .await
        .expect("mutating tool starts")
        .expect("test semaphore stays open");
    started.forget();
    assert!(workspace.join("active-write.txt").exists());

    let error = tokio::time::timeout(Duration::from_millis(250), runtime.revert(thread.id, None))
        .await
        .expect("busy undo is rejected promptly")
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot revert while a turn is running"),
        "{error}"
    );
    assert_eq!(
        runtime.turn(turn.id).await.unwrap().status,
        TurnStatus::Running,
        "undo leaves the active mutation alone"
    );
    assert!(workspace.join("active-write.txt").exists());
    runtime.interrupt_thread_tree(thread.id).await.unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Interrupted
    );
    wait_idle(&runtime, thread.id).await;
    runtime.revert(thread.id, None).await.unwrap();
    assert!(
        !workspace.join("active-write.txt").exists(),
        "undo after explicit stop removes the interrupted turn's write"
    );
    runtime.unrevert(thread.id).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(workspace.join("active-write.txt"))
            .await
            .unwrap(),
        "written\n"
    );
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn restart_recovery_suspends_an_orphaned_goal_instead_of_leaving_it_running() {
    let runtime = AgentRuntime::new(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Recover goal".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .goal_action(
            thread.id,
            GoalActionRequest::Set {
                goal: "finish safely".into(),
                token_budget: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(runtime.recover().await.unwrap(), 0);
    let state = runtime.goal_state(thread.id).await.unwrap().unwrap();
    assert_eq!(state.status, GoalStatus::Paused);
    assert!(state.awaiting_user_input);
    assert_eq!(state.paused_reason.as_deref(), Some("server-restarted"));
    assert!(runtime.events().subscribe(None).replay.iter().any(|event| {
        event.event_type == "goal.updated"
            && event.payload["message"]
                .as_str()
                .is_some_and(|message| message.contains("server restarted"))
    }));
}

#[tokio::test]
async fn detached_sse_client_and_thread_switch_leave_turn_owned_by_server_until_interrupt() {
    let provider = SlowText::default();
    let runtime = Arc::new(AgentRuntime::new(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = serve(
        listener,
        "detach-token",
        ServerState {
            browser: None,
            management: None,
            runtime,
            mcp: Arc::new(NoMcp),
            skills: Arc::new(NoSkills),
            plugins: None,
            workflows: None,
            workspaces: None,
            workspace_id: None,
            workspace: std::env::temp_dir(),
            project_trusted: false,
            language: None,
        },
    )
    .await
    .unwrap();
    let base = format!("http://{}", handle.address);
    let authenticated_client = || {
        reqwest::Client::builder()
            .default_headers({
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert(
                    reqwest::header::AUTHORIZATION,
                    "Bearer detach-token".parse().unwrap(),
                );
                headers
            })
            .build()
            .unwrap()
    };

    // This client represents the TUI currently attached to the first thread.
    // The response headers prove that the server has built this connection's
    // independent event subscription before it is deliberately dropped.
    let departing = authenticated_client();
    let thread: Thread = departing
        .post(format!("{base}/v1/threads"))
        .json(&CreateThreadRequest {
            title: Some("Detached turn".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let response = departing
        .get(format!("{base}/v1/events"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream")),
        "the departing client established an SSE response"
    );
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::X_CONTENT_TYPE_OPTIONS)
            .and_then(|value| value.to_str().ok()),
        Some("nosniff"),
        "Apple clients buffer a sniffed stream until 512 bytes arrive"
    );
    let mut event_stream = response.bytes_stream();
    let first = tokio::time::timeout(Duration::from_secs(2), event_stream.next())
        .await
        .expect("a stream with nothing to replay still sends a first byte at once")
        .unwrap()
        .unwrap();
    assert!(first.starts_with(b":"), "the first frame is an SSE comment");

    let turn: Turn = departing
        .post(format!("{base}/v1/threads/{}/turns", thread.id))
        .json(&turn_request("keep running after I leave"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(turn.status, TurnStatus::Running);
    let started = tokio::time::timeout(Duration::from_secs(1), provider.first_started.acquire())
        .await
        .expect("provider starts")
        .expect("test semaphore remains open");
    started.forget();

    // Dropping both the SSE body and its client must only detach an observer;
    // it must not be wired to the server-owned turn cancellation token.
    drop(event_stream);
    drop(departing);
    tokio::task::yield_now().await;

    let replacement = authenticated_client();
    let active: Vec<Turn> = replacement
        .get(format!("{base}/v1/turns/active"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        active.iter().map(|turn| turn.id).collect::<Vec<_>>(),
        [turn.id]
    );

    // Selecting/creating another conversation is a client concern. It cannot
    // change ownership of the first thread's active turn.
    let switched_to: Thread = replacement
        .post(format!("{base}/v1/threads"))
        .json(&CreateThreadRequest {
            title: Some("Replacement view".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let _: Thread = replacement
        .get(format!("{base}/v1/threads/{}", switched_to.id))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let active_for_original: Option<Turn> = replacement
        .get(format!("{base}/v1/threads/{}/active", thread.id))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(active_for_original.map(|active| active.id), Some(turn.id));

    // Cancellation remains an explicit API operation, independent of either
    // client's attachment or currently selected thread.
    let interrupted: Turn = replacement
        .post(format!("{base}/v1/turns/{}/interrupt", turn.id))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(interrupted.status, TurnStatus::Interrupted);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let active: Vec<Turn> = replacement
                .get(format!("{base}/v1/turns/active"))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap();
            if active.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("interrupted turn leaves the authoritative active snapshot");
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn default_agent_loop_is_not_cut_off_after_thirty_two_tool_rounds() {
    use std::sync::atomic::Ordering;

    let provider = LongToolLoop::default();
    let runtime = AgentRuntime::new(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Unbounded tool loop".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request("finish the long task"))
        .await
        .unwrap();
    let finished = wait_done(&runtime, &turn).await;

    assert_eq!(
        finished.status,
        TurnStatus::Completed,
        "{:?}",
        finished.error
    );
    assert_eq!(provider.rounds.load(Ordering::SeqCst), 41);
    assert!(
        runtime
            .items(thread.id, None)
            .await
            .unwrap()
            .iter()
            .any(|item| {
                matches!(&item.data, haarness_protocol::ItemData::AssistantMessage { text }
                if text == "Recovered after forty tool rounds")
            })
    );
}

#[tokio::test]
async fn explicit_tool_round_guard_requests_a_tool_free_final_response() {
    use std::sync::atomic::Ordering;

    let workspace =
        std::env::temp_dir().join(format!("haarness-round-guard-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = LongToolLoop::default();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(workspace.clone())),
        EventBus::new(64),
        workspace.clone(),
        String::new(),
        RuntimeConfig {
            max_rounds: Some(2),
            ..RuntimeConfig::default()
        },
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Explicit round guard".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request("respect my explicit guard"))
        .await
        .unwrap();
    let finished = wait_done(&runtime, &turn).await;

    assert_eq!(
        finished.status,
        TurnStatus::Completed,
        "{:?}",
        finished.error
    );
    assert_eq!(provider.rounds.load(Ordering::SeqCst), 3);
    assert!(provider.guarded.load(Ordering::SeqCst));
    let _ = std::fs::remove_dir_all(workspace);
}

fn queue_consumption_marker(
    items: &[haarness_protocol::Item],
    queued_item_id: haarness_protocol::ItemId,
) -> &haarness_protocol::Item {
    items
        .iter()
        .find(|item| {
            matches!(
                item.data,
                haarness_protocol::ItemData::QueueConsumed { item_id }
                    if item_id == queued_item_id
            )
        })
        .expect("queued message has a durable consumption marker")
}

fn turn_request(text: &str) -> StartTurnRequest {
    StartTurnRequest {
        request_id: RequestId::new(),
        text: text.into(),
        attachments: vec![],
        synthetic: false,
        auto_approve: false,
        agent: None,
        provider: None,
        model: None,
        reasoning_effort: None,
        tools: None,
        command: None,
    }
}

fn assert_goal_doctrine_precedes_tool_pair(
    request: &ProviderRequest,
    objective: &str,
    tool_name: &str,
) {
    let doctrine = request
        .messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| match message {
            ProviderMessage::Text { role, content }
            | ProviderMessage::Media { role, content, .. }
                if role == "user" && content.contains("<goal-harness>") =>
            {
                Some((index, content))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        doctrine.len(),
        1,
        "each request carries exactly one goal envelope"
    );
    assert!(doctrine[0].1.contains(objective));
    let (call_index, call_id) = request
        .messages
        .iter()
        .enumerate()
        .find_map(|(index, message)| match message {
            ProviderMessage::ToolCall { call_id, name, .. } if name == tool_name => {
                Some((index, call_id))
            }
            _ => None,
        })
        .expect("request replays the tool call");
    let (result_index, result_is_error) = request
        .messages
        .iter()
        .enumerate()
        .find_map(|(index, message)| match message {
            ProviderMessage::ToolResult {
                call_id: result_id,
                is_error,
                ..
            } if result_id == call_id => Some((index, *is_error)),
            _ => None,
        })
        .expect("request replays the matching tool result");
    assert!(
        !result_is_error,
        "the advertised goal tool executes cleanly"
    );
    assert!(
        doctrine[0].0 < call_index && call_index < result_index,
        "goal context must precede, never split, the tool call/result pair"
    );
}

struct NoMcp;
#[async_trait]
impl McpRegistry for NoMcp {
    fn tool_snapshot(&self) -> Vec<ToolDefinition> {
        vec![]
    }
    async fn list(&self) -> CoreResult<Vec<McpServerInfo>> {
        Ok(vec![])
    }
    async fn configure(&self, _: &str, _: McpConfigureRequest) -> CoreResult<McpServerInfo> {
        Err(CoreError::NotFound("fixture".into()))
    }
    async fn reconnect(&self, _: &str) -> CoreResult<McpServerInfo> {
        Err(CoreError::NotFound("fixture".into()))
    }
    async fn disconnect(&self, _: &str) -> CoreResult<McpServerInfo> {
        Err(CoreError::NotFound("fixture".into()))
    }
    async fn tools(&self, _: &str) -> CoreResult<Vec<ToolDefinition>> {
        Ok(vec![])
    }
    async fn call(&self, _: &str, _: &str, _: Value) -> CoreResult<ToolOutput> {
        Err(CoreError::NotFound("fixture".into()))
    }
    async fn resources(&self, _: &str) -> CoreResult<Vec<Value>> {
        Ok(vec![])
    }
    async fn resource_templates(&self, _: &str) -> CoreResult<Vec<Value>> {
        Ok(vec![])
    }
    async fn read_resource(&self, _: &str, _: &str) -> CoreResult<Value> {
        Err(CoreError::NotFound("fixture".into()))
    }
    async fn prompts(&self, _: &str) -> CoreResult<Vec<Value>> {
        Ok(vec![])
    }
    async fn get_prompt(&self, _: &str, _: &str, _: Value) -> CoreResult<Value> {
        Err(CoreError::NotFound("fixture".into()))
    }
}
struct NoSkills;
#[async_trait]
impl SkillRegistry for NoSkills {
    async fn discover(&self) -> CoreResult<Vec<SkillInfo>> {
        Ok(vec![])
    }
    async fn load(&self, name: &str) -> CoreResult<String> {
        Err(CoreError::NotFound(name.into()))
    }
}

#[tokio::test]
async fn workflow_catalog_rest_is_read_only_and_never_executes_source() {
    let workspace =
        std::env::temp_dir().join(format!("haarness-workflow-rest-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(workspace.join(".haarness/workflows")).unwrap();
    let source = r#"export default workflow({ name: "demo", description: "static", run() { throw new Error("must not execute") } })"#;
    std::fs::write(workspace.join(".haarness/workflows/demo.ts"), source).unwrap();
    let runtime = Arc::new(AgentRuntime::new(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(Vec::new())),
        Arc::new(EmptyToolRegistry),
        EventBus::new(16),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = serve(
        listener,
        "workflow-token",
        ServerState {
            browser: None,
            management: None,
            runtime,
            mcp: Arc::new(NoMcp),
            skills: Arc::new(NoSkills),
            plugins: None,
            workflows: None,
            workspaces: None,
            workspace_id: None,
            workspace: workspace.clone(),
            project_trusted: false,
            language: None,
        },
    )
    .await
    .unwrap();
    let base = format!("http://{}", handle.address);
    let client = reqwest::Client::new();
    let list = client
        .get(format!("{base}/v1/workflows"))
        .bearer_auth("workflow-token")
        .send()
        .await
        .unwrap();
    assert!(list.status().is_success());
    let list: haarness_protocol::WorkflowListResponse = list.json().await.unwrap();
    assert!(
        list.workflows
            .iter()
            .any(|workflow| workflow.invocation == "demo")
    );
    let commands: Vec<haarness_protocol::CommandInfo> = client
        .get(format!("{base}/v1/commands"))
        .bearer_auth("workflow-token")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let command = commands
        .iter()
        .find(|command| command.name == "demo")
        .expect("workflow joins the shared command registry");
    assert_eq!(command.source.as_deref(), Some("workflow"));
    assert!(command.description.contains("static"));

    let source_response = client
        .get(format!("{base}/v1/workflows/demo/source"))
        .bearer_auth("workflow-token")
        .send()
        .await
        .unwrap();
    assert!(source_response.status().is_success());
    let source_response: haarness_protocol::WorkflowSourceResponse =
        source_response.json().await.unwrap();
    assert_eq!(source_response.source, source);

    // P0 deliberately exposes no start endpoint until the isolated executor
    // and lifecycle coordinator exist.
    assert!(
        !client
            .post(format!("{base}/v1/workflows/run"))
            .bearer_auth("workflow-token")
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    handle.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(workspace);
}

#[tokio::test]
async fn workflow_lifecycle_rest_runs_portable_source_and_returns_durable_logs() {
    let workspace =
        std::env::temp_dir().join(format!("haarness-workflow-api-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(workspace.join(".haarness/workflows")).unwrap();
    std::fs::write(
        workspace.join(".haarness/workflows/portable.js"),
        r#"
export const meta = { name: "portable-api", phases: ["verify"] }
phase("verify")
log("portable API fixture")
return { ok: true, input: args.input }
"#,
    )
    .unwrap();
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let events = EventBus::new(32);
    let runtime = Arc::new(AgentRuntime::new(
        store.clone(),
        Arc::new(StaticProviderRegistry::new(Vec::new())),
        Arc::new(EmptyToolRegistry),
        events.clone(),
    ));
    let host = WorkflowRuntimeAdapter::new();
    host.attach(&runtime);
    let executor = WorkflowExecutor::new(ExecutorConfig::default());
    if !executor.capability().executable {
        eprintln!("skipping workflow lifecycle API fixture: Bun is unavailable");
        let _ = std::fs::remove_dir_all(workspace);
        return;
    }
    let workflows = Arc::new(WorkflowCoordinator::new(
        store,
        Arc::new(WorkflowCatalog::for_workspace(&workspace)),
        executor,
        host,
        events,
        workspace.clone(),
        true,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = serve(
        listener,
        "workflow-lifecycle-token",
        ServerState {
            browser: None,
            management: None,
            runtime,
            mcp: Arc::new(NoMcp),
            skills: Arc::new(NoSkills),
            plugins: None,
            workflows: Some(workflows),
            workspaces: None,
            workspace_id: None,
            workspace: workspace.clone(),
            project_trusted: true,
            language: None,
        },
    )
    .await
    .unwrap();
    let base = format!("http://{}", handle.address);
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{base}/v1/workflows/run"))
        .bearer_auth("workflow-lifecycle-token")
        .json(&serde_json::json!({
            "name": "portable-api",
            "args": {"input": "typed"},
            "wait": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let run: WorkflowRun = response.json().await.unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Completed);
    assert_eq!(run.phase.as_deref(), Some("verify"));
    assert_eq!(run.result.as_ref().unwrap()["input"], "typed");
    assert!(
        run.logs
            .iter()
            .any(|entry| entry.message == "portable API fixture")
    );
    assert_eq!(run.source_hash.len(), 64);

    let listed: Vec<WorkflowRun> = client
        .get(format!("{base}/v1/workflows/runs?limit=10"))
        .bearer_auth("workflow-lifecycle-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.first().map(|value| value.id), Some(run.id));
    let incremental: WorkflowRun = client
        .get(format!(
            "{base}/v1/workflows/runs/{}?afterSequence=1",
            run.id
        ))
        .bearer_auth("workflow-lifecycle-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(incremental.logs.iter().all(|entry| entry.sequence > 1));
    let invalid_wait = client
        .get(format!(
            "{base}/v1/workflows/runs/{}/wait?timeoutMs=0",
            run.id
        ))
        .bearer_auth("workflow-lifecycle-token")
        .send()
        .await
        .unwrap();
    assert_eq!(invalid_wait.status(), reqwest::StatusCode::BAD_REQUEST);
    let deleted = client
        .delete(format!("{base}/v1/workflows/runs/{}", run.id))
        .bearer_auth("workflow-lifecycle-token")
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), reqwest::StatusCode::NO_CONTENT);

    handle.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(workspace);
}

#[tokio::test]
async fn shared_command_registry_advertises_exactly_what_the_dispatcher_executes() {
    let workspace =
        std::env::temp_dir().join(format!("haarness-command-fabric-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(workspace.join(".haarness/workflows")).unwrap();
    for name in ["demo", "release"] {
        std::fs::write(
            workspace.join(format!(".haarness/workflows/{name}.js")),
            format!(
                r#"export const meta = {{ name: "{name}" }}
return {{ workflow: "{name}", input: args.input }}
"#
            ),
        )
        .unwrap();
    }
    let executor = WorkflowExecutor::new(ExecutorConfig::default());
    if !executor.capability().executable {
        eprintln!("skipping command-fabric workflow fixture: Bun is unavailable");
        let _ = std::fs::remove_dir_all(workspace);
        return;
    }

    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let events = EventBus::new(128);
    let provider = Capturing::with_reasoning_efforts(&["high"]);
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
    ]));
    let mut config = haarness_core::RuntimeConfig::default();
    config.commands.push(haarness_protocol::CommandInfo {
        execution: Default::default(),
        name: "DeMo".into(),
        description: "configured collision winner".into(),
        source: None,
        agent: Some("review".into()),
        model: None,
        argument_hint: None,
        template: "Configured demo: $ARGUMENTS".into(),
        plugin: None,
    });
    config.commands.push(haarness_protocol::CommandInfo {
        execution: Default::default(),
        name: "prompt".into(),
        description: "configured prompt".into(),
        source: None,
        agent: None,
        model: None,
        argument_hint: Some("<text>".into()),
        template: "Prompt command: $ARGUMENTS".into(),
        plugin: None,
    });
    // Even a configured collision cannot replace the reserved manager.
    config.commands.push(haarness_protocol::CommandInfo {
        execution: Default::default(),
        name: "WORKFLOW".into(),
        description: "must lose".into(),
        source: None,
        agent: None,
        model: None,
        argument_hint: None,
        template: "wrong".into(),
        plugin: None,
    });
    let runtime = Arc::new(AgentRuntime::with_config(
        store.clone(),
        providers,
        Arc::new(EmptyToolRegistry),
        events.clone(),
        workspace.clone(),
        String::new(),
        config,
    ));
    let host = WorkflowRuntimeAdapter::new();
    host.attach(&runtime);
    let workflows = Arc::new(WorkflowCoordinator::new(
        store,
        Arc::new(WorkflowCatalog::for_workspace(&workspace)),
        executor,
        host,
        events,
        workspace.clone(),
        true,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = serve(
        listener,
        "command-token",
        ServerState {
            browser: None,
            management: None,
            runtime: runtime.clone(),
            mcp: Arc::new(NoMcp),
            skills: Arc::new(NoSkills),
            plugins: None,
            workflows: Some(workflows),
            workspaces: None,
            workspace_id: None,
            workspace: workspace.clone(),
            project_trusted: true,
            language: None,
        },
    )
    .await
    .unwrap();
    let base = format!("http://{}", handle.address);
    let client = reqwest::Client::builder()
        .default_headers({
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::AUTHORIZATION,
                "Bearer command-token".parse().unwrap(),
            );
            headers
        })
        .build()
        .unwrap();
    let info: ServerInfo = client
        .get(format!("{base}/v1/info"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        info.capabilities
            .iter()
            .any(|capability| capability == WORKSPACE_HISTORY_CAPABILITY)
    );
    assert!(
        info.capabilities
            .iter()
            .any(|capability| capability == WORKSPACE_CLEANUP_CAPABILITY)
    );
    assert!(
        info.capabilities
            .iter()
            .any(|capability| capability == "lsp.v1")
    );
    assert!(
        info.capabilities
            .iter()
            .any(|capability| capability == "formatter.v1")
    );
    let lsp: Vec<serde_json::Value> = client
        .get(format!("{base}/v1/lsp"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let formatters: Vec<serde_json::Value> = client
        .get(format!("{base}/v1/formatter"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(lsp.is_empty());
    assert!(formatters.is_empty());
    let thread: Thread = client
        .post(format!("{base}/v1/threads"))
        .json(&CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    let commands: Vec<haarness_protocol::CommandInfo> = client
        .get(format!("{base}/v1/commands"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let manager = commands
        .iter()
        .filter(|command| command.name.eq_ignore_ascii_case("workflow"))
        .collect::<Vec<_>>();
    assert_eq!(manager.len(), 1);
    assert_eq!(manager[0].source.as_deref(), Some("builtin"));
    let goal_manager = commands
        .iter()
        .filter(|command| command.name.eq_ignore_ascii_case("goal"))
        .collect::<Vec<_>>();
    assert_eq!(goal_manager.len(), 1);
    assert_eq!(goal_manager[0].source.as_deref(), Some("builtin"));
    let demo = commands
        .iter()
        .filter(|command| command.name.eq_ignore_ascii_case("demo"))
        .collect::<Vec<_>>();
    assert_eq!(demo.len(), 1, "case-insensitive collisions deduplicate");
    assert_eq!(demo[0].source.as_deref(), Some("configured"));
    assert_eq!(
        commands
            .iter()
            .find(|command| command.name.eq_ignore_ascii_case("release"))
            .and_then(|command| command.source.as_deref()),
        Some("workflow")
    );

    let prompt_response: haarness_protocol::CommandExecutionResponse = client
        .post(format!("{base}/v1/threads/{}/commands", thread.id))
        .json(&serde_json::json!({
            "name": "PrOmPt",
            "arguments": "preserve this tail"
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let haarness_protocol::CommandExecutionResponse::Turn { turn } = prompt_response else {
        panic!("configured prompt command did not return a tagged turn")
    };
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(runtime.items(thread.id, None).await.unwrap().iter().any(
        |item| matches!(&item.data, haarness_protocol::ItemData::UserMessage { text, .. }
            if text == "Prompt command: preserve this tail")
    ));

    let collision: haarness_protocol::CommandExecutionResponse = client
        .post(format!("{base}/v1/threads/{}/commands", thread.id))
        .json(&serde_json::json!({"name": "demo", "arguments": "winner"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let haarness_protocol::CommandExecutionResponse::Turn { turn } = collision else {
        panic!("configured command must win its workflow collision")
    };
    assert_eq!(turn.agent, "review");
    wait_done(&runtime, &turn).await;

    let workflow_response: haarness_protocol::CommandExecutionResponse = client
        .post(format!("{base}/v1/threads/{}/commands", thread.id))
        .json(&serde_json::json!({
            "name": "ReLeAsE",
            "arguments": "typed input",
            "agent": "plan",
            "provider": "echo",
            "model": "echo-1",
            "variant": "high",
            "wait": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let haarness_protocol::CommandExecutionResponse::WorkflowRun { run } = workflow_response else {
        panic!("advertised workflow did not return a tagged durable run")
    };
    assert_eq!(run.status, WorkflowRunStatus::Completed);
    assert_eq!(run.result.as_ref().unwrap()["input"], "typed input");
    assert_eq!(run.definition.agent.as_deref(), Some("plan"));
    assert_eq!(run.definition.model.as_deref(), Some("echo/echo-1"));
    assert_eq!(run.definition.variant.as_deref(), Some("high"));
    let release_run_id = run.id;

    // The explicit manager can still address the workflow hidden by the
    // configured `/demo` collision, without changing direct-command truth.
    let explicit: haarness_protocol::CommandExecutionResponse = client
        .post(format!("{base}/v1/threads/{}/commands", thread.id))
        .json(&serde_json::json!({
            "name": "WORKFLOW",
            "arguments": "run DEMO explicit",
            "wait": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(matches!(
        explicit,
        haarness_protocol::CommandExecutionResponse::WorkflowRun { run }
            if run.result.as_ref().unwrap()["workflow"] == "demo"
    ));

    let listed: haarness_protocol::CommandExecutionResponse = client
        .post(format!("{base}/v1/threads/{}/commands", thread.id))
        .json(&serde_json::json!({"name": "workflow", "arguments": "list"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(matches!(
        listed,
        haarness_protocol::CommandExecutionResponse::Text { text }
            if text.contains("/release") && text.contains(&release_run_id.to_string())
    ));

    let inspected: haarness_protocol::CommandExecutionResponse = client
        .post(format!("{base}/v1/threads/{}/commands", thread.id))
        .json(&serde_json::json!({
            "name": "workflow",
            "arguments": format!("SHOW {release_run_id}")
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(matches!(
        inspected,
        haarness_protocol::CommandExecutionResponse::WorkflowRun { run }
            if run.id == release_run_id
    ));

    let other: Thread = client
        .post(format!("{base}/v1/threads"))
        .json(&CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let foreign = client
        .post(format!("{base}/v1/threads/{}/commands", other.id))
        .json(&serde_json::json!({
            "name": "workflow",
            "arguments": format!("inspect {release_run_id}")
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), reqwest::StatusCode::NOT_FOUND);

    let removed: haarness_protocol::CommandExecutionResponse = client
        .post(format!("{base}/v1/threads/{}/commands", thread.id))
        .json(&serde_json::json!({
            "name": "workflow",
            "arguments": format!("rm {release_run_id}")
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(matches!(
        removed,
        haarness_protocol::CommandExecutionResponse::Text { text }
            if text.contains("Deleted workflow release")
    ));

    let unknown = client
        .post(format!("{base}/v1/threads/{}/commands", thread.id))
        .json(&serde_json::json!({"name": "missing"}))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);
    let error: haarness_protocol::ApiError = unknown.json().await.unwrap();
    assert!(error.message.contains("available:"), "{error:?}");

    let goal_response: haarness_protocol::CommandExecutionResponse = client
        .post(format!("{base}/v1/threads/{}/commands", thread.id))
        .json(&serde_json::json!({
            "name": "goal",
            "arguments": "block all flaky tests",
            "variant": "high"
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let haarness_protocol::CommandExecutionResponse::Turn { turn } = goal_response else {
        panic!("literal goal objective did not launch a turn")
    };
    assert_eq!(
        runtime.goal_state(thread.id).await.unwrap().unwrap().goal,
        "block all flaky tests",
        "only an exact bare control verb is reserved"
    );
    wait_done(&runtime, &turn).await;
    assert_eq!(
        provider
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .reasoning_effort
            .as_deref(),
        Some("high"),
        "the configured variant reaches normal goal turns"
    );

    handle.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(workspace);
}

#[tokio::test]
async fn authenticated_rest_turn_runs_end_to_end() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(Echo) as Arc<dyn ModelProvider>
    ]));
    let runtime = Arc::new(AgentRuntime::new(
        store,
        providers,
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = serve(
        listener,
        "fixture-token",
        ServerState {
            browser: None,
            management: None,
            runtime,
            mcp: Arc::new(NoMcp),
            skills: Arc::new(NoSkills),
            plugins: None,
            workflows: None,
            workspaces: None,
            workspace_id: None,
            workspace: std::env::temp_dir(),
            project_trusted: false,
            language: None,
        },
    )
    .await
    .unwrap();
    let base = format!("http://{}", handle.address);
    let client = reqwest::Client::new();
    assert!(
        client
            .get(format!("{base}/healthz"))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert_eq!(
        client
            .get(format!("{base}/v1/info"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let client = reqwest::Client::builder()
        .default_headers({
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::AUTHORIZATION,
                "Bearer fixture-token".parse().unwrap(),
            );
            headers
        })
        .build()
        .unwrap();
    let thread: Thread = client
        .post(format!("{base}/v1/threads"))
        .json(&CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let turn: Turn = client
        .post(format!("{base}/v1/threads/{}/turns", thread.id))
        .json(&StartTurnRequest {
            request_id: RequestId::new(),
            text: "tested".into(),
            attachments: vec![],
            synthetic: false,
            auto_approve: false,
            agent: None,
            provider: None,
            model: None,
            reasoning_effort: None,
            tools: None,
            command: None,
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let finished: Turn = client
        .get(format!("{base}/v1/turns/{}/wait?timeout_ms=5000", turn.id))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(finished.status, TurnStatus::Completed);
    let items: Vec<haarness_protocol::Item> = client
        .get(format!("{base}/v1/threads/{}/items", thread.id))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(items.iter().any(|item|matches!(&item.data,haarness_protocol::ItemData::AssistantMessage{text}if text=="Echo: tested")));
    let todos: Vec<haarness_protocol::TodoEntry> = client
        .get(format!("{base}/v1/threads/{}/todos", thread.id))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(todos.is_empty());
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn creating_a_chat_with_a_chosen_id_twice_returns_the_first_chat() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(Echo) as Arc<dyn ModelProvider>
    ]));
    let runtime = Arc::new(AgentRuntime::new(
        store,
        providers,
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = serve(
        listener,
        "fixture-token",
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
            workspace: std::env::temp_dir(),
            project_trusted: false,
            language: None,
        },
    )
    .await
    .unwrap();
    let base = format!("http://{}", handle.address);
    let client = reqwest::Client::new();
    let info: ServerInfo = client
        .get(format!("{base}/v1/info"))
        .bearer_auth("fixture-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        info.capabilities
            .iter()
            .any(|capability| capability == haarness_protocol::THREAD_CREATE_ID_CAPABILITY)
    );
    let id = haarness_protocol::ThreadId::new();
    let create = |title: &'static str| {
        client
            .post(format!("{base}/v1/threads"))
            .bearer_auth("fixture-token")
            .json(&serde_json::json!({"id": id, "title": title, "provider": "echo", "model": "echo-1"}))
            .send()
    };
    let first = create("Fix login").await.unwrap();
    assert_eq!(first.status(), reqwest::StatusCode::CREATED);
    let first: Thread = first.json().await.unwrap();
    assert_eq!(first.id, id);
    assert_eq!(first.title, "Fix login");
    // A retry (a lost response, a restarted worker) gets the same chat back.
    let retried = create("Something else").await.unwrap();
    assert_eq!(retried.status(), reqwest::StatusCode::CREATED);
    let retried: Thread = retried.json().await.unwrap();
    assert_eq!(retried.id, id);
    assert_eq!(retried.title, "Fix login");
    assert_eq!(retried.created_at, first.created_at);
    // Concurrent creates that both find no chat: `cases/create_race.rs`.
    let threads = runtime.list_threads(true).await.unwrap();
    assert_eq!(threads.len(), 1, "{threads:?}");
    let created = runtime
        .events()
        .subscribe(Some(0))
        .replay
        .into_iter()
        .filter(|event| event.event_type == "thread.created")
        .filter_map(|event| event.thread_id)
        .collect::<Vec<_>>();
    assert_eq!(created, [id], "one created event");
    handle.shutdown().await.unwrap();
}

#[path = "cases/create_race.rs"]
mod create_race;

#[tokio::test]
async fn forked_thread_continues_with_parent_history() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let provider = Capturing::default();
    let requests = provider.requests.clone();
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(provider) as Arc<dyn ModelProvider>
    ]));
    let runtime = AgentRuntime::new(
        store.clone(),
        providers,
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Fork fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request("first"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );

    let fork = runtime.fork_thread(thread.id, None, None).await.unwrap();
    assert_eq!(
        runtime.items(fork.id, None).await.unwrap().len(),
        runtime.items(thread.id, None).await.unwrap().len(),
        "fork must copy the parent history"
    );

    let fork_turn = runtime
        .start_turn(fork.id, turn_request("second"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &fork_turn).await.status,
        TurnStatus::Completed
    );
    let last = requests.lock().unwrap().last().cloned().unwrap();
    let texts: Vec<String> = last
        .messages
        .iter()
        .filter_map(|message| match message {
            ProviderMessage::Text { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect();
    assert!(texts.iter().any(|text| text == "first"));
    assert!(texts.iter().any(|text| text == "Echo: first"));
    assert!(texts.iter().any(|text| text == "second"));
}

#[tokio::test]
async fn binary_read_attachment_is_persisted_and_replayed_to_the_provider() {
    let root =
        std::env::temp_dir().join(format!("haarness-binary-read-e2e-{}", uuid::Uuid::now_v7()));
    let workspace = root.join("ws");
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    let pdf = workspace.join("report.pdf");
    tokio::fs::write(&pdf, b"%PDF-1.4\n1 0 obj<<>>endobj\n%%EOF\n")
        .await
        .unwrap();

    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let provider = Capturing::default();
    let requests = provider.requests.clone();
    let runtime = AgentRuntime::with_config(
        store,
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider) as Arc<dyn ModelProvider>
        ])),
        Arc::new(NativeToolRegistry::new(workspace.clone())),
        EventBus::new(64),
        workspace.clone(),
        String::new(),
        RuntimeConfig::default(),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Binary read fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!tool read {"path":"report.pdf"}"#),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );

    let canonical = pdf.canonicalize().unwrap().display().to_string();
    {
        let captured = requests.lock().unwrap();
        assert_eq!(captured.len(), 2, "read completes in one tool round");
        assert!(captured[1].messages.iter().any(|message| matches!(
            message,
            ProviderMessage::ToolResult {
                name,
                attachments,
                is_error: false,
                ..
            } if name == "read" && attachments == &[canonical.as_str()]
        )));
    }

    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(items.iter().any(|item| matches!(
        &item.data,
        haarness_protocol::ItemData::ToolResult { metadata, .. }
            if metadata["attachments"][0] == canonical
                && metadata["mime"] == "application/pdf"
    )));
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn semantic_fork_remaps_consumed_queue_and_excludes_selected_prompt() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let provider = Capturing::default();
    let requests = provider.requests.clone();
    let runtime = AgentRuntime::new(
        store.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider) as Arc<dyn ModelProvider>
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Semantic fork fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let queued = store
        .append_item(
            thread.id,
            None,
            haarness_protocol::ItemData::UserMessage {
                text: "queued historical steer".into(),
                attachments: Vec::new(),
                queued: true,
                synthetic: false,
            },
        )
        .await
        .unwrap();
    store
        .append_item(
            thread.id,
            None,
            haarness_protocol::ItemData::QueueConsumed { item_id: queued.id },
        )
        .await
        .unwrap();
    store
        .append_item(
            thread.id,
            None,
            haarness_protocol::ItemData::AssistantMessage {
                text: "answer after steer".into(),
            },
        )
        .await
        .unwrap();
    let selected = store
        .append_item(
            thread.id,
            None,
            haarness_protocol::ItemData::UserMessage {
                text: "edit this selected prompt".into(),
                attachments: vec!["/tmp/reference.png".into()],
                queued: false,
                synthetic: false,
            },
        )
        .await
        .unwrap();

    let mismatch = runtime
        .fork_thread_request(
            thread.id,
            haarness_protocol::ForkThreadRequest {
                title: None,
                up_to_sequence: Some(selected.sequence),
                before_item_id: Some(selected.id),
            },
        )
        .await
        .unwrap_err();
    assert!(mismatch.to_string().contains("immediately before"));

    let fork = runtime
        .fork_thread_request(
            thread.id,
            haarness_protocol::ForkThreadRequest {
                title: None,
                up_to_sequence: Some(selected.sequence.saturating_sub(1)),
                before_item_id: Some(selected.id),
            },
        )
        .await
        .unwrap();
    let copied = runtime.items(fork.id, None).await.unwrap();
    assert!(!copied.iter().any(|item| matches!(
        &item.data,
        haarness_protocol::ItemData::UserMessage { text, .. }
            if text == "edit this selected prompt"
    )));
    let copied_queued = copied
        .iter()
        .find(|item| {
            matches!(
                item.data,
                haarness_protocol::ItemData::UserMessage { queued: true, .. }
            )
        })
        .unwrap();
    assert!(copied.iter().any(|item| matches!(
        item.data,
        haarness_protocol::ItemData::QueueConsumed { item_id }
            if item_id == copied_queued.id && item_id != queued.id
    )));

    let turn = runtime
        .start_turn(fork.id, turn_request("replacement prompt"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let request = requests.lock().unwrap().last().cloned().unwrap();
    let texts = request
        .messages
        .iter()
        .filter_map(|message| match message {
            ProviderMessage::Text { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let queued_position = texts
        .iter()
        .position(|text| text.contains("queued historical steer"))
        .unwrap();
    let answer_position = texts
        .iter()
        .position(|text| *text == "answer after steer")
        .unwrap();
    let replacement_position = texts
        .iter()
        .position(|text| *text == "replacement prompt")
        .unwrap();
    assert!(queued_position < answer_position);
    assert!(answer_position < replacement_position);
}

#[tokio::test]
async fn reasoning_effort_reaches_the_provider() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let provider = Capturing::default();
    let requests = provider.requests.clone();
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(provider) as Arc<dyn ModelProvider>
    ]));
    let runtime = AgentRuntime::new(
        store.clone(),
        providers,
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Effort fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut request = turn_request("think hard");
    request.reasoning_effort = Some("high".into());
    let turn = runtime.start_turn(thread.id, request).await.unwrap();
    wait_done(&runtime, &turn).await;
    assert_eq!(
        requests.lock().unwrap()[0].reasoning_effort.as_deref(),
        Some("high")
    );
}

#[tokio::test]
async fn workflow_schema_is_primary_intent_gated_and_children_require_explicit_opt_in() {
    let workspace = std::env::temp_dir();
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let provider = Capturing::with_reasoning_efforts(&["high"]);
    let requests = provider.requests.clone();
    let runtime = AgentRuntime::new(
        store,
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider) as Arc<dyn ModelProvider>
        ])),
        Arc::new(WorkflowAdvertisingRegistry::new(&workspace)),
        EventBus::new(128),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    let ordinary = runtime
        .start_turn(thread.id, turn_request("analyze this code"))
        .await
        .unwrap();
    wait_done(&runtime, &ordinary).await;
    assert!(
        requests.lock().unwrap()[0]
            .tools
            .iter()
            .all(|tool| tool.name != "workflow")
    );

    let explicit = runtime
        .start_turn(thread.id, turn_request("use a workflow for this audit"))
        .await
        .unwrap();
    wait_done(&runtime, &explicit).await;
    assert!(
        requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .tools
            .iter()
            .any(|tool| tool.name == "workflow")
    );

    let mut ultracode = turn_request("broad audit");
    ultracode.reasoning_effort = Some("ultracode".into());
    let ultracode = runtime.start_turn(thread.id, ultracode).await.unwrap();
    wait_done(&runtime, &ultracode).await;
    assert!(
        requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .tools
            .iter()
            .any(|tool| tool.name == "workflow")
    );

    let child_prompt = "ordinary delegated audit";
    let task = serde_json::json!({
        "prompt": child_prompt,
        "agent": "general",
        "description": "gate child workflow",
    });
    let delegated = runtime
        .start_turn(thread.id, turn_request(&format!("!tool task {task}")))
        .await
        .unwrap();
    wait_done(&runtime, &delegated).await;
    let child_request = requests
        .lock()
        .unwrap()
        .iter()
        .find(|request| {
            request.messages.iter().any(|message| {
                matches!(message, ProviderMessage::Text { role, content }
                    if role == "user" && content == child_prompt)
            })
        })
        .cloned()
        .expect("captured delegated request");
    assert!(
        child_request
            .tools
            .iter()
            .all(|tool| tool.name != "workflow"),
        "ordinary child agents must not recursively orchestrate"
    );
}

#[tokio::test]
async fn standing_ultracode_is_durable_and_resolves_the_strongest_advertised_effort() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let provider = Capturing::with_reasoning_efforts(&["low", "high", "xhigh", "max"]);
    let requests = provider.requests.clone();
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(provider) as Arc<dyn ModelProvider>
    ]));
    let runtime = AgentRuntime::new(
        store,
        providers,
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Ultracode fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ultracode: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request("orchestrate this"))
        .await
        .unwrap();
    assert!(turn.ultracode);
    wait_done(&runtime, &turn).await;

    let request = requests.lock().unwrap()[0].clone();
    assert_eq!(request.reasoning_effort.as_deref(), Some("max"));
    assert!(request.system.contains("Ultracode mode is enabled"));
    assert!(runtime.get_thread(thread.id).await.unwrap().ultracode);
    assert!(
        runtime
            .fork_thread(thread.id, None, None)
            .await
            .unwrap()
            .ultracode
    );
}

#[tokio::test]
async fn one_shot_ultracode_uses_provider_default_without_advertised_efforts() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let provider = Capturing::default();
    let requests = provider.requests.clone();
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(provider) as Arc<dyn ModelProvider>
    ]));
    let runtime = AgentRuntime::new(
        store,
        providers,
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("One-shot fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut request = turn_request("one shot");
    request.reasoning_effort = Some("ultracode".into());
    let turn = runtime.start_turn(thread.id, request).await.unwrap();
    assert!(turn.ultracode);
    wait_done(&runtime, &turn).await;

    let request = requests.lock().unwrap()[0].clone();
    assert!(request.reasoning_effort.is_none());
    assert!(request.system.contains("Ultracode mode is enabled"));
    assert!(
        !runtime.get_thread(thread.id).await.unwrap().ultracode,
        "one-shot transport must not mutate the standing thread preference"
    );
}

#[tokio::test]
async fn prompt_keyword_enables_ultracode_for_only_the_current_turn() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let provider = Capturing::with_reasoning_efforts(&["low", "high", "max"]);
    let requests = provider.requests.clone();
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(provider) as Arc<dyn ModelProvider>
    ]));
    let runtime = AgentRuntime::new(
        store,
        providers,
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Keyword fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    let turn = runtime
        .start_turn(thread.id, turn_request("please ULTRACODE: audit this"))
        .await
        .unwrap();
    assert!(turn.ultracode);
    wait_done(&runtime, &turn).await;
    assert_eq!(
        requests.lock().unwrap()[0].reasoning_effort.as_deref(),
        Some("max")
    );
    assert!(
        requests.lock().unwrap()[0]
            .system
            .contains("Ultracode mode is enabled")
    );
    assert!(!runtime.get_thread(thread.id).await.unwrap().ultracode);

    let ordinary = runtime
        .start_turn(thread.id, turn_request("/ultracode is a command"))
        .await
        .unwrap();
    assert!(!ordinary.ultracode);
    wait_done(&runtime, &ordinary).await;
    assert!(requests.lock().unwrap()[1].reasoning_effort.is_none());
    assert!(
        !requests.lock().unwrap()[1]
            .system
            .contains("Ultracode mode is enabled")
    );
}

#[tokio::test]
async fn todo_tool_records_durable_state() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
    ]));
    let events = EventBus::new(64);
    let runtime = AgentRuntime::new(
        store,
        providers,
        Arc::new(NativeToolRegistry::new(std::env::current_dir().unwrap())),
        events.clone(),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Todo fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(
                r#"!tool todowrite {"todos":[{"content":"write tests","status":"in_progress","priority":"high"},{"content":"ship","status":"pending","priority":"low"}]}"#,
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let items = runtime.items(thread.id, None).await.unwrap();
    let todo = items
        .iter()
        .find_map(|item| match &item.data {
            haarness_protocol::ItemData::Todo { items } => Some(items.clone()),
            _ => None,
        })
        .expect("todo item persisted");
    assert_eq!(todo.len(), 2);
    assert_eq!(todo[0].content, "write tests");
    assert_eq!(todo[0].status, "in_progress");
    assert_eq!(todo[0].priority, "high");
    assert_eq!(runtime.todos(thread.id).await.unwrap(), todo);
    assert!(events.subscribe(None).replay.iter().any(|event| {
        event.event_type == "todo.updated"
            && event.payload["threadId"] == serde_json::json!(thread.id)
            && event.payload["todos"]
                .as_array()
                .is_some_and(|todos| todos.len() == 2)
    }));
    let result_ok = items.iter().any(|item| {
        matches!(&item.data, haarness_protocol::ItemData::ToolResult { output, is_error, .. }
            if output.contains("Todo list updated (2 items)") && !is_error)
    });
    assert!(result_ok, "tool result should confirm the todo update");

    let read = runtime
        .start_turn(thread.id, turn_request("!tool todoread {}"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &read).await.status,
        TurnStatus::Completed
    );
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(items.iter().any(|item| {
        matches!(&item.data, haarness_protocol::ItemData::ToolResult { name, output, is_error, .. }
            if name == "todoread" && output.contains("write tests") && !is_error)
    }));

    let legacy = runtime
        .start_turn(
            thread.id,
            turn_request(
                r#"!tool todo {"items":[{"content":"legacy item","status":"cancelled"}]}"#,
            ),
        )
        .await
        .unwrap();
    wait_done(&runtime, &legacy).await;
    let todo = runtime.todos(thread.id).await.unwrap();
    assert_eq!(todo[0].status, "cancelled");
    assert_eq!(todo[0].priority, "medium");
}

#[tokio::test]
async fn todo_tools_obey_permission_policy() {
    for (policy, invocation, body) in [
        (
            "todowrite:*",
            "todo",
            r#"{"items":[{"content":"blocked","status":"pending","priority":"high"}]}"#,
        ),
        (
            "todo:*",
            "todowrite",
            r#"{"todos":[{"content":"blocked","status":"pending","priority":"high"}]}"#,
        ),
    ] {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let provider = Capturing::default();
        let requests = Arc::clone(&provider.requests);
        let providers = Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider) as Arc<dyn ModelProvider>
        ]));
        let mut config = haarness_core::RuntimeConfig::default();
        config.base_rules.push(haarness_core::PermissionRule {
            pattern: policy.into(),
            decision: haarness_core::PermissionDecision::Deny,
        });
        let runtime = AgentRuntime::with_config(
            store,
            providers,
            Arc::new(NativeToolRegistry::new(std::env::current_dir().unwrap())),
            EventBus::new(64),
            std::env::current_dir().unwrap(),
            String::new(),
            config,
        );
        let thread = echo_thread(&runtime).await;
        let turn = runtime
            .start_turn(
                thread.id,
                turn_request(&format!("!tool {invocation} {body}")),
            )
            .await
            .unwrap();
        wait_done(&runtime, &turn).await;
        assert!(runtime.todos(thread.id).await.unwrap().is_empty());
        assert!(
            requests.lock().unwrap().iter().all(|request| {
                request
                    .tools
                    .iter()
                    .all(|tool| !matches!(tool.name.as_str(), "todo" | "todowrite"))
            }),
            "{policy} must hide both todo mutation aliases from the provider"
        );
        assert!(runtime.items(thread.id, None).await.unwrap().iter().any(|item| {
            matches!(&item.data, haarness_protocol::ItemData::ToolResult { name, output, is_error, .. }
                if name == invocation
                    && *is_error
                    && output.contains("disabled") && output.contains("Do not retry"))
        }), "{policy} did not govern its {invocation} alias");
    }
}

async fn wait_permission(runtime: &AgentRuntime, thread: haarness_protocol::ThreadId) -> String {
    for _ in 0..400 {
        let items = runtime.items(thread, None).await.unwrap();
        if let Some(id) = items.iter().find_map(|item| match &item.data {
            haarness_protocol::ItemData::PermissionRequest { permission_id, .. } => {
                Some(permission_id.clone())
            }
            _ => None,
        }) {
            return id;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no permission request appeared");
}

fn workspace_runtime(workspace: &std::path::Path) -> (Arc<SqliteStore>, AgentRuntime) {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
    ]));
    let runtime = AgentRuntime::with_config(
        store.clone(),
        providers,
        Arc::new(haarness_server::NativeToolRegistry::new(
            workspace.to_path_buf(),
        )),
        EventBus::new(64),
        workspace.to_path_buf(),
        String::new(),
        {
            // These fixtures exercise the approval flow; parity defaults
            // auto-run mutations, so tighten via config like a user would.
            let mut config = haarness_core::RuntimeConfig::default();
            for pattern in ["write:*", "edit:*", "apply_patch:*", "bash:*"] {
                config.base_rules.push(haarness_core::PermissionRule {
                    pattern: pattern.into(),
                    decision: haarness_core::PermissionDecision::Ask,
                });
            }
            config
        },
    );
    (store, runtime)
}

async fn echo_thread(runtime: &AgentRuntime) -> Thread {
    runtime
        .create_thread(CreateThreadRequest {
            // Explicit title: keeps fire-and-forget auto-titling out of tests
            // that assert on captured provider requests.
            title: Some("Fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn provider_namespaces_resolve_before_batch_planning_and_permission_checks() {
    use haarness_protocol::ItemData;
    use serde_json::json;
    let workspace =
        std::env::temp_dir().join(format!("haarness-tool-names-{}", uuid::Uuid::now_v7()));
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    tokio::fs::write(workspace.join("sample.txt"), "namespace proof")
        .await
        .unwrap();
    let mut config = RuntimeConfig::default();
    config.tool_availability.insert("write".into(), false);
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(workspace.clone())),
        EventBus::new(128),
        workspace.clone(),
        String::new(),
        config,
    );
    let thread = echo_thread(&runtime).await;
    let calls = json!({"tool_calls":[
        {"tool":"functions.list","parameters":{"path":"."}},
        {"tool":"functions.glob","parameters":{"pattern":"*.txt","path":"."}},
        {"tool":"functions.missing","parameters":{}},
        {"tool":"functions.batch","parameters":{"tool_calls":[]}},
        {"tool":"functions.write","parameters":{"path":"blocked.txt","content":"must not execute"}},
        {"tool":"functions.read","parameters":{"path":"sample.txt"}}
    ]});
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&format!("!tool functions.batch {calls}")),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let items = runtime.items(thread.id, None).await.unwrap();
    let result = items
        .iter()
        .find_map(|item| match &item.data {
            ItemData::ToolResult {
                name,
                metadata,
                is_error: false,
                ..
            } if name == "batch" => Some(metadata),
            _ => None,
        })
        .expect("prefixed batch must execute");
    assert_eq!(result["failures"], 3);
    assert!(
        result["results"][0]["output"]
            .as_str()
            .unwrap()
            .contains("sample.txt")
    );
    assert!(
        result["results"][1]["output"]
            .as_str()
            .unwrap()
            .contains("sample.txt")
    );
    assert!(
        result["results"][2]["error"]
            .as_str()
            .unwrap()
            .contains("unknown tool")
    );
    assert!(
        result["results"][3]["error"]
            .as_str()
            .unwrap()
            .contains("cannot be invoked")
    );
    assert!(
        result["results"][4]["error"]
            .as_str()
            .unwrap()
            .contains("disabled")
    );
    assert!(
        result["results"][5]["output"]
            .as_str()
            .unwrap()
            .contains("namespace proof")
    );
    assert!(!workspace.join("blocked.txt").exists());
    assert_eq!(result["results"][5]["tool"], "read");

    let direct = runtime
        .run_direct_tool(
            thread.id,
            "functions.read",
            json!({"path":"sample.txt"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        matches!(direct.item.data, ItemData::ToolResult { name, is_error: false, .. } if name == "read")
    );
    let blocked = runtime
        .run_direct_tool(
            thread.id,
            "functions.write",
            json!({"path":"blocked.txt","content":"no"}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(blocked.to_string().contains("disabled"));
    assert!(!workspace.join("blocked.txt").exists());
    let _ = tokio::fs::remove_dir_all(workspace).await;
}

#[tokio::test]
async fn namespaced_mutations_still_wait_for_permission() {
    for script in [
        r#"!tool functions.write {"path":"approved.txt","content":"hi"}"#,
        r#"!tool functions.batch {"tool_calls":[{"tool":"functions.write","parameters":{"path":"approved.txt","content":"hi"}}]}"#,
        r#"!tool Write {"path":"approved.txt","content":"hi"}"#,
        r#"!tool FUNCTIONS.Batch {"tool_calls":[{"tool":"FUNCTIONS.Write","parameters":{"path":"approved.txt","content":"hi"}}]}"#,
    ] {
        let workspace = std::env::temp_dir().join(format!(
            "haarness-namespaced-permission-{}",
            uuid::Uuid::now_v7()
        ));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let (_store, runtime) = workspace_runtime(&workspace);
        let thread = echo_thread(&runtime).await;
        let turn = runtime
            .start_turn(thread.id, turn_request(script))
            .await
            .unwrap();
        let permission = wait_permission(&runtime, thread.id).await;
        assert!(!workspace.join("approved.txt").exists());
        runtime
            .reply_permission(
                &permission,
                haarness_protocol::PermissionReply {
                    decision: haarness_protocol::PermissionReplyDecision::Once,
                    feedback: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            wait_done(&runtime, &turn).await.status,
            TurnStatus::Completed
        );
        assert_eq!(
            tokio::fs::read_to_string(workspace.join("approved.txt"))
                .await
                .unwrap(),
            "hi"
        );
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }
}

#[tokio::test]
async fn mutating_tools_prompt_and_approval_executes_them() {
    let workspace = std::env::temp_dir().join(format!("haarness-perm-{}", uuid::Uuid::now_v7()));
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    let (_store, runtime) = workspace_runtime(&workspace);
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!tool write {"path":"approved.txt","content":"hi"}"#),
        )
        .await
        .unwrap();

    let permission = wait_permission(&runtime, thread.id).await;
    runtime
        .reply_permission(
            &permission,
            haarness_protocol::PermissionReply {
                decision: haarness_protocol::PermissionReplyDecision::Once,
                feedback: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert_eq!(
        tokio::fs::read_to_string(workspace.join("approved.txt"))
            .await
            .unwrap(),
        "hi"
    );
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(items.iter().any(|item| matches!(&item.data,
        haarness_protocol::ItemData::ToolResult { is_error, .. } if !is_error)));
    tokio::fs::remove_dir_all(workspace).await.unwrap();
}

#[tokio::test]
async fn rejected_permissions_surface_feedback_and_write_nothing() {
    let workspace = std::env::temp_dir().join(format!("haarness-reject-{}", uuid::Uuid::now_v7()));
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    let (_store, runtime) = workspace_runtime(&workspace);
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!tool write {"path":"denied.txt","content":"no"}"#),
        )
        .await
        .unwrap();

    let permission = wait_permission(&runtime, thread.id).await;
    runtime
        .reply_permission(
            &permission,
            haarness_protocol::PermissionReply {
                decision: haarness_protocol::PermissionReplyDecision::Reject,
                feedback: Some("use the docs directory instead".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(!workspace.join("denied.txt").exists());
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(items.iter().any(|item| matches!(&item.data,
        haarness_protocol::ItemData::ToolResult { is_error, output, .. }
            if *is_error && output.contains("use the docs directory instead"))));
    tokio::fs::remove_dir_all(workspace).await.unwrap();
}

#[tokio::test]
async fn always_workspace_approval_persists_and_stops_prompting() {
    let workspace = std::env::temp_dir().join(format!("haarness-always-{}", uuid::Uuid::now_v7()));
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    let (store, runtime) = workspace_runtime(&workspace);
    let thread = echo_thread(&runtime).await;
    let script = r#"!tool write {"path":"repeat.txt","content":"1"}"#;
    let turn = runtime
        .start_turn(thread.id, turn_request(script))
        .await
        .unwrap();
    let permission = wait_permission(&runtime, thread.id).await;
    runtime
        .reply_permission(
            &permission,
            haarness_protocol::PermissionReply {
                decision: haarness_protocol::PermissionReplyDecision::AlwaysWorkspace,
                feedback: None,
            },
        )
        .await
        .unwrap();
    wait_done(&runtime, &turn).await;

    let persisted = haarness_core::ThreadRepository::load_permission_rules(
        store.as_ref(),
        &workspace.display().to_string(),
    )
    .await
    .unwrap();
    assert_eq!(persisted.len(), 1);
    assert!(persisted[0].pattern.starts_with("edit:"));

    // The same operation on a fresh thread completes without a prompt.
    let second_thread = echo_thread(&runtime).await;
    let second = runtime
        .start_turn(second_thread.id, turn_request(script))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &second).await.status,
        TurnStatus::Completed
    );
    let items = runtime.items(second_thread.id, None).await.unwrap();
    assert!(
        !items.iter().any(|item| matches!(
            &item.data,
            haarness_protocol::ItemData::PermissionRequest { .. }
        )),
        "session rule from AlwaysWorkspace should suppress further prompts"
    );
    tokio::fs::remove_dir_all(workspace).await.unwrap();
}

#[tokio::test]
async fn llm_compaction_shrinks_context_but_retains_recent_turns_and_exact_todos() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let provider = Capturing::default();
    let requests = provider.requests.clone();
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(provider) as Arc<dyn ModelProvider>
    ]));
    let runtime = AgentRuntime::new(
        store.clone(),
        providers,
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Compaction fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    for prompt in [
        "oldest question",
        "recent question one",
        "recent question two",
    ] {
        let turn = runtime
            .start_turn(thread.id, turn_request(prompt))
            .await
            .unwrap();
        wait_done(&runtime, &turn).await;
    }
    store
        .append_item(
            thread.id,
            None,
            haarness_protocol::ItemData::Todo {
                items: vec![haarness_protocol::TodoEntry {
                    content: "finish exact compaction tests".into(),
                    status: "in_progress".into(),
                    priority: "high".into(),
                }],
            },
        )
        .await
        .unwrap();

    let item = runtime.compact_with_llm(thread.id).await.unwrap();
    assert!(matches!(&item.data,
        haarness_protocol::ItemData::Compaction { summary, .. }
            if summary.contains("Echo:")));

    let second = runtime
        .start_turn(thread.id, turn_request("next question"))
        .await
        .unwrap();
    wait_done(&runtime, &second).await;
    let last = requests.lock().unwrap().last().cloned().unwrap();
    let texts: Vec<String> = last
        .messages
        .iter()
        .filter_map(|message| match message {
            ProviderMessage::Text { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect();
    assert!(
        texts[0].contains("<compaction-summary>"),
        "summary leads the compacted request: {texts:?}"
    );
    assert!(
        texts[0].contains("finish exact compaction tests"),
        "the exact todo checkpoint survives compaction: {texts:?}"
    );
    assert!(!texts.iter().any(|text| text == "oldest question"));
    assert!(texts.iter().any(|text| text == "recent question one"));
    assert!(texts.iter().any(|text| text == "Echo: recent question one"));
    assert!(texts.iter().any(|text| text == "recent question two"));
    assert!(texts.iter().any(|text| text == "Echo: recent question two"));
    assert!(texts.iter().any(|text| text == "next question"));
}

/// Answers "high demand" (HTTP 503) the first time, then normally.
#[derive(Default)]
struct BusyOnce {
    rounds: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl ModelProvider for BusyOnce {
    fn id(&self) -> &str {
        "busy"
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "busy".into(),
            name: "Busy fixture".into(),
            authenticated: true,
            models: vec![ModelInfo {
                id: "busy-1".into(),
                display_name: "Busy fixture".into(),
                reasoning: false,
                reasoning_efforts: Vec::new(),
                images: false,
                video: false,
                tools: false,
                context_window: Some(100_000),
                max_output_tokens: Some(4_096),
                pricing: None,
            }],
        }
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if let Some(title) = title_fixture(&request) {
            return title;
        }
        if self
            .rounds
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            == 0
        {
            return Ok(Box::pin(stream::iter(vec![Err(CoreError::Provider(
                "HTTP 503 Service Unavailable: This model is currently experiencing high demand."
                    .into(),
            ))])));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderEvent::TextDelta(
                "answered after a busy moment".into(),
            )),
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }),
        ])))
    }
}

#[tokio::test]
async fn a_busy_provider_is_retried_instead_of_failing_the_turn() {
    let provider = BusyOnce::default();
    let rounds = provider.rounds.clone();
    let events = EventBus::new(256);
    let runtime = AgentRuntime::new(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider) as Arc<dyn ModelProvider>
        ])),
        Arc::new(EmptyToolRegistry),
        events.clone(),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Busy provider".into()),
            provider: Some("busy".into()),
            model: Some("busy-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request("hello"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert_eq!(rounds.load(std::sync::atomic::Ordering::SeqCst), 2);
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(items.iter().any(|item| matches!(
        &item.data,
        haarness_protocol::ItemData::AssistantMessage { text } if text == "answered after a busy moment"
    )));
    // The person saw that it was retrying rather than a silent pause.
    let retrying = events.subscribe(None).replay.iter().any(|event| {
        event.event_type == "turn.status" && event.payload.to_string().contains("retrying")
    });
    assert!(retrying, "a turn.status retrying event is published");
}

/// Answers a structured-output turn with scripted rounds, in order.
struct ScriptedStructured {
    rounds: Arc<std::sync::atomic::AtomicUsize>,
    answers: Vec<ProviderEvent>,
}

#[async_trait]
impl ModelProvider for ScriptedStructured {
    fn id(&self) -> &str {
        "scripted"
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "scripted".into(),
            name: "Scripted fixture".into(),
            authenticated: true,
            models: vec![ModelInfo {
                id: "scripted-1".into(),
                display_name: "Scripted fixture".into(),
                reasoning: false,
                reasoning_efforts: Vec::new(),
                images: false,
                video: false,
                tools: true,
                context_window: Some(100_000),
                max_output_tokens: Some(4_096),
                pricing: None,
            }],
        }
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if let Some(title) = title_fixture(&request) {
            return title;
        }
        let round = self
            .rounds
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let answer = self.answers[round.min(self.answers.len() - 1)].clone();
        Ok(Box::pin(stream::iter(vec![
            Ok(answer),
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }),
        ])))
    }
}

#[tokio::test]
async fn structured_output_in_the_wrong_shape_is_asked_for_again() {
    let schema = serde_json::json!({"type":"object","properties":{"count":{"type":"integer"}},"required":["count"],"additionalProperties":false});
    let run = |answers: Vec<ProviderEvent>| {
        let schema = schema.clone();
        async move {
            let rounds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let runtime = AgentRuntime::new(
                Arc::new(SqliteStore::in_memory().unwrap()),
                Arc::new(StaticProviderRegistry::new(vec![
                    Arc::new(ScriptedStructured {
                        rounds: rounds.clone(),
                        answers,
                    }) as Arc<dyn ModelProvider>,
                ])),
                Arc::new(EmptyToolRegistry),
                EventBus::new(256),
            );
            let thread = runtime
                .create_thread(CreateThreadRequest {
                    title: Some("Structured".into()),
                    provider: Some("scripted".into()),
                    model: Some("scripted-1".into()),
                    ..Default::default()
                })
                .await
                .unwrap();
            let turn = runtime
                .start_workflow_agent_turn(
                    thread.id,
                    thread.id,
                    turn_request("count"),
                    Some(schema),
                )
                .await
                .unwrap();
            let status = wait_done(&runtime, &turn).await.status;
            let items = runtime.items(thread.id, None).await.unwrap();
            (
                status,
                rounds.load(std::sync::atomic::Ordering::SeqCst),
                items,
            )
        }
    };
    // Prose, then the wrong type, then a valid answer: two corrective rounds.
    let (status, rounds, items) = run(vec![
        ProviderEvent::TextDelta("There are two.".into()),
        ProviderEvent::StructuredOutput(serde_json::json!({"count": "two"})),
        ProviderEvent::StructuredOutput(serde_json::json!({"count": 2})),
    ])
    .await;
    assert_eq!(status, TurnStatus::Completed);
    assert_eq!(rounds, 3);
    let corrections = items
        .iter()
        .filter(|item| matches!(&item.data, haarness_protocol::ItemData::UserMessage { synthetic: true, text, .. } if text.contains("structured")))
        .count();
    assert_eq!(corrections, 2);
    assert!(items.iter().any(|item| matches!(
        &item.data,
        haarness_protocol::ItemData::AssistantMessage { text } if text == r#"{"count":2}"#
    )));
    // A model that keeps answering in prose fails after two corrections.
    let (status, rounds, _) = run(vec![ProviderEvent::TextDelta("Two.".into())]).await;
    assert_eq!(status, TurnStatus::Failed);
    assert_eq!(rounds, 3);
}

#[tokio::test]
async fn context_overflow_compacts_once_and_replays_without_binary_media() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let provider = OverflowOnce::default();
    let requests = provider.requests.clone();
    let runtime = AgentRuntime::new(
        store,
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider) as Arc<dyn ModelProvider>
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Overflow recovery".into()),
            provider: Some("overflow".into()),
            model: Some("overflow-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    // Attachments must exist when the message is sent.
    let image = std::env::temp_dir().join(format!("large-reference-{}.png", uuid::Uuid::now_v7()));
    std::fs::write(&image, b"\x89PNG").unwrap();
    let mut request = turn_request("inspect the attachment");
    request.attachments = vec![image.to_string_lossy().into_owned()];
    let turn = runtime.start_turn(thread.id, request).await.unwrap();

    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(items.iter().any(|item| matches!(
        &item.data,
        haarness_protocol::ItemData::Compaction { summary, .. }
            if summary == "overflow-safe summary"
    )));
    assert!(items.iter().any(|item| matches!(
        &item.data,
        haarness_protocol::ItemData::AssistantMessage { text }
            if text == "recovered after compaction"
    )));

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3, "main, summary, replay");
    assert!(is_compaction_request(&requests[1]));
    assert!(
        requests[1]
            .messages
            .iter()
            .all(|message| !matches!(message, ProviderMessage::Media { .. }))
    );
    assert!(requests[1].messages.iter().any(|message| matches!(
        message,
        ProviderMessage::Text { content, .. }
            if content.contains("Attachments omitted during compaction")
    )));
    assert!(
        requests[2]
            .messages
            .iter()
            .all(|message| !matches!(message, ProviderMessage::Media { .. }))
    );
    assert!(requests[2].messages.iter().any(|message| matches!(
        message,
        ProviderMessage::Text { content, .. }
            if content.contains("Attachments from the compacted history are not replayed")
    )));
}

#[tokio::test]
async fn automatic_compaction_uses_each_models_context_and_output_limits() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let provider = ModelAwareCompaction::default();
    let requests = provider.requests.clone();
    let runtime = AgentRuntime::new(
        store.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider) as Arc<dyn ModelProvider>
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(128),
    );
    let create = |model: &str| CreateThreadRequest {
        title: Some(format!("{model} compaction")),
        provider: Some("model-aware".into()),
        model: Some(model.into()),
        ..Default::default()
    };
    let small = runtime.create_thread(create("small")).await.unwrap();
    let large = runtime.create_thread(create("large")).await.unwrap();
    for thread in [&small, &large] {
        store
            .append_item(
                thread.id,
                None,
                haarness_protocol::ItemData::UserMessage {
                    text: format!("old oversized history {}", "x".repeat(4_000)),
                    attachments: Vec::new(),
                    queued: false,
                    synthetic: false,
                },
            )
            .await
            .unwrap();
    }

    let small_turn = runtime
        .start_turn(small.id, turn_request("small model continuation"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &small_turn).await.status,
        TurnStatus::Completed
    );
    let large_turn = runtime
        .start_turn(large.id, turn_request("large model continuation"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &large_turn).await.status,
        TurnStatus::Completed
    );

    assert!(
        runtime
            .items(small.id, None)
            .await
            .unwrap()
            .iter()
            .any(|item| { matches!(item.data, haarness_protocol::ItemData::Compaction { .. }) })
    );
    assert!(
        !runtime
            .items(large.id, None)
            .await
            .unwrap()
            .iter()
            .any(|item| { matches!(item.data, haarness_protocol::ItemData::Compaction { .. }) })
    );
    let requests = requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.model == "small" && is_compaction_request(request))
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.model == "large" && is_compaction_request(request))
            .count(),
        0
    );
    let replay = requests
        .iter()
        .rev()
        .find(|request| request.model == "small" && !is_compaction_request(request))
        .unwrap();
    assert!(replay.messages.iter().any(|message| matches!(
        message,
        ProviderMessage::Text { content, .. } if content == "small model continuation"
    )));
    assert!(!replay.messages.iter().any(|message| matches!(
        message,
        ProviderMessage::Text { content, .. } if content.contains("old oversized history")
    )));
}

#[tokio::test]
async fn manual_compaction_confirms_revert_and_checkpoints_boundary_todos() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let runtime = AgentRuntime::new(
        store.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Revert then compact".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let kept = store
        .append_item(
            thread.id,
            None,
            haarness_protocol::ItemData::Todo {
                items: vec![haarness_protocol::TodoEntry {
                    content: "keep boundary task".into(),
                    status: "pending".into(),
                    priority: "medium".into(),
                }],
            },
        )
        .await
        .unwrap();
    store
        .append_item(
            thread.id,
            None,
            haarness_protocol::ItemData::Todo {
                items: vec![haarness_protocol::TodoEntry {
                    content: "discard reverted task".into(),
                    status: "in_progress".into(),
                    priority: "high".into(),
                }],
            },
        )
        .await
        .unwrap();
    store
        .set_revert_state(
            thread.id,
            Some((kept.sequence, "fixture-snapshot".into(), None)),
        )
        .await
        .unwrap();

    let compacted = runtime.compact_with_llm(thread.id).await.unwrap();

    assert!(
        runtime
            .get_thread(thread.id)
            .await
            .unwrap()
            .revert
            .is_none()
    );
    assert_eq!(
        runtime.todos(thread.id).await.unwrap()[0].content,
        "keep boundary task"
    );
    let haarness_protocol::ItemData::Compaction {
        todo_checkpoint: Some(checkpoint),
        ..
    } = compacted.data
    else {
        panic!("compaction omitted exact todo checkpoint");
    };
    assert_eq!(checkpoint.revision, kept.sequence);
    assert_eq!(checkpoint.todos[0].content, "keep boundary task");
    assert!(
        !runtime
            .items(thread.id, None)
            .await
            .unwrap()
            .iter()
            .any(|item| {
                matches!(&item.data, haarness_protocol::ItemData::Todo { items }
            if items.iter().any(|todo| todo.content == "discard reverted task"))
            })
    );
}

#[tokio::test]
async fn queued_input_during_summarization_cancels_compaction_until_exact_delivery() {
    let provider = CompactionQueueRace::default();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(128),
        std::env::temp_dir(),
        String::new(),
        haarness_core::RuntimeConfig {
            compaction: haarness_core::CompactionSettings {
                enabled: true,
                threshold: 0.75,
                reserve_output: Some(0),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Compaction queue race".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request("produce enough context to compact"))
        .await
        .unwrap();

    let summary_started =
        tokio::time::timeout(Duration::from_secs(2), provider.summary_started.acquire())
            .await
            .expect("auto-compaction summarizer started")
            .unwrap();
    summary_started.forget();

    let exact_steering = "keep this exact queued steering, including punctuation: [alpha/42]!";
    let queued = runtime
        .queue_input(
            thread.id,
            haarness_protocol::QueueInputRequest {
                preview: None,
                reasoning_effort: None,
                delivery: haarness_protocol::InputDelivery::Steer,
                request_id: RequestId::new(),
                text: exact_steering.into(),
                attachments: Vec::new(),
                synthetic: false,
            },
        )
        .await
        .unwrap();
    assert!(
        queued.queued,
        "summarization does not block queue persistence"
    );
    let queued_item = queued.item.expect("queued item is returned");
    let while_summary_is_blocked = runtime.items(thread.id, None).await.unwrap();
    assert!(
        !while_summary_is_blocked.iter().any(|item| matches!(
            item.data,
            haarness_protocol::ItemData::QueueConsumed { item_id }
                if item_id == queued_item.id
        )),
        "the queue item cannot be marked consumed before a main request receives it"
    );
    assert!(
        !while_summary_is_blocked
            .iter()
            .any(|item| matches!(item.data, haarness_protocol::ItemData::Compaction { .. })),
        "the blocked summarizer has not committed a lossy boundary"
    );

    provider.release_summary.add_permits(1);
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );

    {
        let requests = provider.requests.lock().unwrap();
        let main_requests = requests
            .iter()
            .filter(|request| !is_compaction_request(request))
            .collect::<Vec<_>>();
        assert_eq!(
            requests.len(),
            3,
            "one initial request, one discarded summary, and one exact follow-up"
        );
        assert_eq!(main_requests.len(), 2);
        let delivered = main_requests[1]
            .messages
            .iter()
            .find_map(|message| match message {
                ProviderMessage::Text { role, content }
                    if role == "user" && content.contains(exact_steering) =>
                {
                    Some(content)
                }
                _ => None,
            })
            .expect("the follow-up main request contains the exact queued text");
        assert!(
            delivered.contains("The user sent the following message while you were working:"),
            "queued steering retains its live-input reminder: {delivered:?}"
        );
        assert!(
            !main_requests[1].messages.iter().any(|message| matches!(
                message,
                ProviderMessage::Text { content, .. }
                    if content.contains("Previous context summary:")
                        || content.contains("lossy summary")
            )),
            "the discarded summary must not replace exact queued input"
        );
    }

    let items = runtime.items(thread.id, None).await.unwrap();
    let marker = queue_consumption_marker(&items, queued_item.id);
    assert!(
        marker.sequence > queued_item.sequence,
        "consumption is durably ordered after queue persistence"
    );
    assert!(
        !items
            .iter()
            .any(|item| matches!(item.data, haarness_protocol::ItemData::Compaction { .. })),
        "the in-flight lossy summary was discarded instead of committed"
    );
}

#[tokio::test]
async fn manual_llm_compaction_rejects_a_turn_completed_during_summarization() {
    let provider = CompactionQueueRace::default();
    let runtime = Arc::new(AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(128),
        std::env::temp_dir(),
        String::new(),
        haarness_core::RuntimeConfig {
            compaction: haarness_core::CompactionSettings {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        },
    ));
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Completed during summary".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let seed = runtime
        .start_turn(thread.id, turn_request("seed history"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &seed).await.status,
        TurnStatus::Completed
    );

    let compacting = tokio::spawn({
        let runtime = Arc::clone(&runtime);
        async move { runtime.compact_with_llm(thread.id).await }
    });
    let summary_started =
        tokio::time::timeout(Duration::from_secs(2), provider.summary_started.acquire())
            .await
            .expect("manual summarizer started")
            .unwrap();
    summary_started.forget();

    let intervening = runtime
        .start_turn(
            thread.id,
            turn_request("this entire turn completed while the stale summary waited"),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &intervening).await.status,
        TurnStatus::Completed
    );
    provider.release_summary.add_permits(1);
    assert!(matches!(
        compacting.await.unwrap(),
        Err(CoreError::Conflict(_))
    ));

    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(items.iter().any(|item| matches!(
        &item.data,
        haarness_protocol::ItemData::UserMessage { text, .. }
            if text == "this entire turn completed while the stale summary waited"
    )));
    assert!(
        !items
            .iter()
            .any(|item| matches!(item.data, haarness_protocol::ItemData::Compaction { .. })),
        "a stale summary cannot truncate a turn that finished before commit"
    );
}

#[tokio::test]
async fn direct_tool_writer_during_llm_summary_invalidates_the_stale_compaction() {
    let workspace = std::env::temp_dir();
    let provider = CompactionQueueRace::default();
    let runtime = Arc::new(AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(haarness_server::NativeToolRegistry::new(workspace.clone())),
        EventBus::new(128),
        workspace,
        String::new(),
        haarness_core::RuntimeConfig {
            compaction: haarness_core::CompactionSettings {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        },
    ));
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Direct writer during summary".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let seed = runtime
        .start_turn(thread.id, turn_request("seed history"))
        .await
        .unwrap();
    wait_done(&runtime, &seed).await;

    let compacting = tokio::spawn({
        let runtime = Arc::clone(&runtime);
        async move { runtime.compact_with_llm(thread.id).await }
    });
    let summary_started =
        tokio::time::timeout(Duration::from_secs(2), provider.summary_started.acquire())
            .await
            .expect("manual summarizer started")
            .unwrap();
    summary_started.forget();
    let direct_result = runtime
        .run_shell(thread.id, "printf direct-writer-during-summary".into())
        .await
        .unwrap();
    assert!(matches!(
        &direct_result.data,
        haarness_protocol::ItemData::ToolResult { output, .. }
            if output.contains("direct-writer-during-summary")
    ));

    provider.release_summary.add_permits(1);
    assert!(matches!(
        compacting.await.unwrap(),
        Err(CoreError::Conflict(_))
    ));
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(
        !items
            .iter()
            .any(|item| matches!(item.data, haarness_protocol::ItemData::Compaction { .. })),
        "a direct writer invalidates the stale summary instead of being truncated"
    );
}

#[tokio::test]
async fn rest_compaction_rejects_active_turn_and_preserves_queued_steering() {
    let provider = CompactionQueueRace::blocking_main_round(3);
    let runtime = Arc::new(AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(256),
        std::env::temp_dir(),
        String::new(),
        haarness_core::RuntimeConfig {
            compaction: haarness_core::CompactionSettings {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        },
    ));
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("REST compaction race".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let seed = runtime
        .start_turn(thread.id, turn_request("seed history"))
        .await
        .unwrap();
    wait_done(&runtime, &seed).await;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = serve(
        listener,
        "compaction-race-token",
        ServerState {
            browser: None,
            management: None,
            runtime: Arc::clone(&runtime),
            mcp: Arc::new(NoMcp),
            skills: Arc::new(NoSkills),
            plugins: None,
            workflows: None,
            workspaces: None,
            workspace_id: None,
            workspace: std::env::temp_dir(),
            project_trusted: true,
            language: None,
        },
    )
    .await
    .unwrap();
    let compact_url = format!("http://{}/v1/threads/{}/compact", handle.address, thread.id);
    let llm_compaction = tokio::spawn({
        let compact_url = compact_url.clone();
        async move {
            reqwest::Client::new()
                .post(compact_url)
                .bearer_auth("compaction-race-token")
                .json(&serde_json::json!({}))
                .send()
                .await
                .unwrap()
        }
    });
    let summary_started =
        tokio::time::timeout(Duration::from_secs(2), provider.summary_started.acquire())
            .await
            .expect("REST compaction summarizer started")
            .unwrap();
    summary_started.forget();

    let active_turn = runtime
        .start_turn(
            thread.id,
            turn_request("keep this prompt while compaction waits"),
        )
        .await
        .unwrap();
    let main_started =
        tokio::time::timeout(Duration::from_secs(2), provider.main_started.acquire())
            .await
            .expect("the intervening main stream started")
            .unwrap();
    main_started.forget();
    let exact_steering = "queued while manual compaction raced: preserve {this/exact}!";
    let queued = runtime
        .queue_input(
            thread.id,
            haarness_protocol::QueueInputRequest {
                preview: None,
                reasoning_effort: None,
                delivery: haarness_protocol::InputDelivery::Steer,
                request_id: RequestId::new(),
                text: exact_steering.into(),
                attachments: Vec::new(),
                synthetic: false,
            },
        )
        .await
        .unwrap();
    let queued_item = queued.item.expect("active turn accepted queued steering");

    provider.release_summary.add_permits(1);
    assert_eq!(
        llm_compaction.await.unwrap().status(),
        reqwest::StatusCode::CONFLICT,
        "LLM compaction must recheck active admission after summarization"
    );
    let supplied_summary = reqwest::Client::new()
        .post(&compact_url)
        .bearer_auth("compaction-race-token")
        .json(&serde_json::json!({"summary":"caller summary"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        supplied_summary.status(),
        reqwest::StatusCode::CONFLICT,
        "caller-supplied compaction is also forbidden during an active turn"
    );

    let blocked_items = runtime.items(thread.id, None).await.unwrap();
    assert!(
        !blocked_items.iter().any(|item| matches!(
            item.data,
            haarness_protocol::ItemData::QueueConsumed { item_id }
                if item_id == queued_item.id
        )),
        "rejected compaction cannot falsely consume queued steering"
    );
    assert!(
        !blocked_items
            .iter()
            .any(|item| matches!(item.data, haarness_protocol::ItemData::Compaction { .. }))
    );

    provider.release_main.add_permits(1);
    assert_eq!(
        wait_done(&runtime, &active_turn).await.status,
        TurnStatus::Completed
    );
    {
        let requests = provider.requests.lock().unwrap();
        assert!(requests.iter().any(|request| request.messages.iter().any(
            |message| matches!(message, ProviderMessage::Text { role, content }
                if role == "user"
                    && content.contains("The user sent the following message while you were working:")
                    && content.contains(exact_steering))
        )));
    }
    let items = runtime.items(thread.id, None).await.unwrap();
    queue_consumption_marker(&items, queued_item.id);
    assert!(
        !items
            .iter()
            .any(|item| matches!(item.data, haarness_protocol::ItemData::Compaction { .. })),
        "neither rejected REST compaction can create a truncation boundary"
    );
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn auto_title_default_threads() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
    ]));
    let runtime = AgentRuntime::new(
        store,
        providers,
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(thread.title, "New chat");
    let turn = runtime
        .start_turn(thread.id, turn_request("name this thread"))
        .await
        .unwrap();
    wait_done(&runtime, &turn).await;

    let mut titled = None;
    for _ in 0..300 {
        let current = runtime.get_thread(thread.id).await.unwrap();
        if current.title != "New chat" {
            titled = Some(current.title);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let title = titled.expect("auto-title should rename the default thread");
    assert!(title.starts_with("Echo:"), "{title}");
    assert!(title.chars().count() <= 50);
}

#[tokio::test]
async fn revert_restores_files_and_unrevert_redoes() {
    let root = std::env::temp_dir().join(format!("haarness-undo-{}", uuid::Uuid::now_v7()));
    let workspace = root.join("ws");
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    tokio::fs::write(workspace.join("existing.txt"), "before\n")
        .await
        .unwrap();
    let database = root.join("db.sqlite");
    let store = Arc::new(SqliteStore::open(&database).unwrap());
    let snapshots =
        Arc::new(haarness_store::SqliteSnapshotStore::open(&database, workspace.clone()).unwrap());
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
    ]));
    let events = EventBus::new(64);
    let runtime = AgentRuntime::with_config(
        store,
        providers,
        Arc::new(haarness_server::NativeToolRegistry::new(workspace.clone())),
        events.clone(),
        workspace.clone(),
        String::new(),
        {
            let mut config = haarness_core::RuntimeConfig::default();
            // Auto-approve writes so the fixture needs no permission dance.
            config.base_rules.push(haarness_core::PermissionRule {
                pattern: "write:*".into(),
                decision: haarness_core::PermissionDecision::Allow,
            });
            config
        },
    )
    .with_snapshots(snapshots);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Undo fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let initial_todo = runtime
        .start_turn(
            thread.id,
            turn_request(
                r#"!tool todowrite {"todos":[{"content":"keep this","status":"in_progress","priority":"high"}]}"#,
            ),
        )
        .await
        .unwrap();
    wait_done(&runtime, &initial_todo).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(
                r#"!multi [{"name":"todowrite","arguments":{"todos":[{"content":"future task","status":"pending","priority":"low"}]}},{"name":"write","arguments":{"path":"created.txt","content":"turn output"}}]"#,
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(workspace.join("created.txt").exists());
    assert_eq!(
        runtime.todos(thread.id).await.unwrap()[0].content,
        "future task"
    );

    let reverted = runtime.revert(thread.id, Some(u64::MAX)).await.unwrap();
    assert!(reverted.revert.is_some(), "revert state recorded");
    assert!(events.subscribe(None).replay.iter().any(|event| {
        event.event_type == "todo.updated"
            && event.thread_id == Some(thread.id)
            && event.root_thread_id == Some(thread.id)
    }));
    assert!(
        !workspace.join("created.txt").exists(),
        "the turn's file mutation is undone"
    );
    assert_eq!(
        tokio::fs::read_to_string(workspace.join("existing.txt"))
            .await
            .unwrap(),
        "before\n"
    );
    // Items remain until the next turn confirms.
    assert!(!runtime.items(thread.id, None).await.unwrap().is_empty());
    assert_eq!(
        runtime.todos(thread.id).await.unwrap()[0].content,
        "keep this",
        "current todo state follows the revert boundary even while future transcript items remain"
    );
    let fork = runtime.fork_thread(thread.id, None, None).await.unwrap();
    assert_eq!(
        runtime.todos(fork.id).await.unwrap()[0].content,
        "keep this",
        "forking a pending revert must not resurrect the future todo state"
    );
    assert!(
        !runtime
            .items(fork.id, None)
            .await
            .unwrap()
            .iter()
            .any(|item| {
                matches!(&item.data, haarness_protocol::ItemData::Todo { items }
            if items.iter().any(|todo| todo.content == "future task"))
            })
    );

    let restored = runtime.unrevert(thread.id).await.unwrap();
    assert!(restored.revert.is_none());
    assert!(
        workspace.join("created.txt").exists(),
        "unrevert brings the change back"
    );
    assert_eq!(
        runtime.todos(thread.id).await.unwrap()[0].content,
        "future task",
        "unrevert restores the future todo snapshot too"
    );

    let second = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!tool write {"path":"second.txt","content":"second turn"}"#),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &second).await.status,
        TurnStatus::Completed
    );
    let last_diff = runtime.diff_last_turn(thread.id).await.unwrap();
    assert!(
        last_diff.iter().any(|file| {
            file.file == "second.txt"
                && file.status == haarness_protocol::FileDiffStatus::Added
                && file.after.as_deref() == Some("second turn")
        }),
        "last-turn diff is selected from the explicit checkpoint pair: {last_diff:?}"
    );

    runtime.revert(thread.id, None).await.unwrap();
    assert!(workspace.join("created.txt").exists());
    assert!(!workspace.join("second.txt").exists());
    runtime.revert(thread.id, None).await.unwrap();
    assert!(!workspace.join("created.txt").exists());
    assert!(!workspace.join("second.txt").exists());

    let first_redo = runtime.unrevert(thread.id).await.unwrap();
    assert!(first_redo.revert.is_some(), "redo advances one checkpoint");
    assert!(workspace.join("created.txt").exists());
    assert!(!workspace.join("second.txt").exists());
    let second_redo = runtime.unrevert(thread.id).await.unwrap();
    assert!(
        second_redo.revert.is_none(),
        "final redo clears the boundary"
    );
    assert!(workspace.join("created.txt").exists());
    assert!(workspace.join("second.txt").exists());
    tokio::fs::remove_dir_all(root).await.unwrap();
}

/// An echo-provider runtime with snapshots over `workspace` that writes
/// without asking.
fn undo_fixture_runtime(root: &std::path::Path, workspace: &std::path::Path) -> AgentRuntime {
    let database = root.join("db.sqlite");
    let store = Arc::new(SqliteStore::open(&database).unwrap());
    let snapshots = Arc::new(
        haarness_store::SqliteSnapshotStore::open(&database, workspace.to_path_buf()).unwrap(),
    );
    AgentRuntime::with_config(
        store,
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(haarness_server::NativeToolRegistry::new(
            workspace.to_path_buf(),
        )),
        EventBus::new(64),
        workspace.to_path_buf(),
        String::new(),
        {
            let mut config = haarness_core::RuntimeConfig::default();
            config.base_rules.push(haarness_core::PermissionRule {
                pattern: "write:*".into(),
                decision: haarness_core::PermissionDecision::Allow,
            });
            config
        },
    )
    .with_snapshots(snapshots)
}

#[tokio::test]
async fn last_turn_diff_skips_unchanged_turns_and_stops_at_the_undo_boundary() {
    let root = std::env::temp_dir().join(format!("haarness-last-diff-{}", uuid::Uuid::now_v7()));
    let workspace = root.join("ws");
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    let runtime = undo_fixture_runtime(&root, &workspace);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Last turn fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    for request in [
        r#"!tool write {"path":"first.txt","content":"one"}"#,
        r#"!tool write {"path":"second.txt","content":"two"}"#,
        // Records a checkpoint pair, but changes nothing.
        r#"!tool write {"path":"second.txt","content":"two"}"#,
    ] {
        let turn = runtime
            .start_turn(thread.id, turn_request(request))
            .await
            .unwrap();
        assert_eq!(
            wait_done(&runtime, &turn).await.status,
            TurnStatus::Completed
        );
    }
    let last = |summary: bool, path: Option<&'static str>| {
        let runtime = runtime.clone();
        async move {
            runtime
                .diff_last_turn_review(thread.id, summary, path)
                .await
                .unwrap()
                .into_iter()
                .map(|file| (file.file, file.status, file.after))
                .collect::<Vec<_>>()
        }
    };
    let added = |file: &str, after: Option<&str>| {
        (
            file.to_owned(),
            haarness_protocol::FileDiffStatus::Added,
            after.map(str::to_owned),
        )
    };
    assert_eq!(
        last(false, None).await,
        [added("second.txt", Some("two"))],
        "a turn that changed nothing does not hide the one before it"
    );
    assert_eq!(last(true, None).await, [added("second.txt", None)]);
    assert_eq!(
        last(false, Some("second.txt")).await,
        [added("second.txt", Some("two"))]
    );
    assert!(last(false, Some("first.txt")).await.is_empty());

    // Undo steps over the unchanged turn first; the files and the diff stay.
    runtime.revert(thread.id, None).await.unwrap();
    assert!(workspace.join("second.txt").exists());
    assert_eq!(last(true, None).await, [added("second.txt", None)]);
    // The undone turn's file is gone, and so is it from the diff.
    runtime.revert(thread.id, None).await.unwrap();
    assert!(!workspace.join("second.txt").exists());
    assert_eq!(
        last(false, None).await,
        [added("first.txt", Some("one"))],
        "an undone turn is not the last turn"
    );
    assert_eq!(runtime.diff_last_turn(thread.id).await.unwrap().len(), 1);
    runtime.revert(thread.id, None).await.unwrap();
    assert!(last(false, None).await.is_empty(), "everything is undone");

    // Redo brings each turn's diff back with its files.
    runtime.unrevert(thread.id).await.unwrap();
    assert_eq!(last(true, None).await, [added("first.txt", None)]);
    runtime.unrevert(thread.id).await.unwrap();
    runtime.unrevert(thread.id).await.unwrap();
    assert!(
        runtime
            .get_thread(thread.id)
            .await
            .unwrap()
            .revert
            .is_none()
    );
    assert_eq!(last(true, None).await, [added("second.txt", None)]);
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_undo_or_redo_changes_nothing_and_can_be_retried() {
    use std::os::unix::fs::PermissionsExt;

    /// Gives the locked directory its permissions back, so cleanup works
    /// even when an assertion fails.
    struct Unlock(std::path::PathBuf);
    impl Drop for Unlock {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
    let lock = |path: &std::path::Path, mode| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    let root = std::env::temp_dir().join(format!("haarness-undo-fail-{}", uuid::Uuid::now_v7()));
    let workspace = root.join("ws");
    tokio::fs::create_dir_all(workspace.join("locked"))
        .await
        .unwrap();
    tokio::fs::write(workspace.join("a.txt"), "before\n")
        .await
        .unwrap();
    tokio::fs::write(workspace.join("locked/b.txt"), "before\n")
        .await
        .unwrap();
    let runtime = undo_fixture_runtime(&root, &workspace);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Failed undo fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(
                r#"!multi [{"name":"write","arguments":{"path":"a.txt","content":"after\n"}},{"name":"write","arguments":{"path":"locked/b.txt","content":"after\n"}}]"#,
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let read = |path: &str| std::fs::read_to_string(workspace.join(path)).unwrap();
    let unlock = Unlock(workspace.join("locked"));
    lock(&unlock.0, 0o555);
    if std::fs::write(unlock.0.join("probe"), "").is_ok() {
        // Root writes everywhere; there is nothing to fail.
        return;
    }

    // `a.txt` is restored first, then `locked/b.txt` cannot be: undo puts
    // `a.txt` back and the chat stays where it was.
    let error = runtime.revert(thread.id, None).await.unwrap_err();
    assert!(
        error.to_string().contains("no files were changed"),
        "{error}"
    );
    assert!(
        runtime
            .get_thread(thread.id)
            .await
            .unwrap()
            .revert
            .is_none()
    );
    assert_eq!(
        (read("a.txt"), read("locked/b.txt")),
        ("after\n".into(), "after\n".into())
    );

    // Once the cause is fixed the same undo goes through.
    lock(&unlock.0, 0o755);
    let reverted = runtime.revert(thread.id, None).await.unwrap();
    assert!(reverted.revert.is_some());
    assert_eq!(
        (read("a.txt"), read("locked/b.txt")),
        ("before\n".into(), "before\n".into())
    );

    // Redo fails the same way without losing the pending undo.
    lock(&unlock.0, 0o555);
    let error = runtime.unrevert(thread.id).await.unwrap_err();
    assert!(
        error.to_string().contains("no files were changed"),
        "{error}"
    );
    let state = |thread: Thread| {
        thread
            .revert
            .map(|state| (state.sequence, state.snapshot_id, state.base_snapshot_id))
    };
    assert_eq!(
        state(runtime.get_thread(thread.id).await.unwrap()),
        state(reverted)
    );
    assert_eq!(
        (read("a.txt"), read("locked/b.txt")),
        ("before\n".into(), "before\n".into())
    );
    lock(&unlock.0, 0o755);
    assert!(runtime.unrevert(thread.id).await.unwrap().revert.is_none());
    assert_eq!(
        (read("a.txt"), read("locked/b.txt")),
        ("after\n".into(), "after\n".into())
    );
    drop(unlock);
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn next_turn_after_revert_soft_deletes_undone_items() {
    let root = std::env::temp_dir().join(format!("haarness-undo2-{}", uuid::Uuid::now_v7()));
    let workspace = root.join("ws");
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    let database = root.join("db.sqlite");
    let store = Arc::new(SqliteStore::open(&database).unwrap());
    let snapshots =
        Arc::new(haarness_store::SqliteSnapshotStore::open(&database, workspace.clone()).unwrap());
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
    ]));
    let events = EventBus::new(64);
    let runtime = AgentRuntime::with_config(
        store,
        providers,
        Arc::new(haarness_server::NativeToolRegistry::new(workspace.clone())),
        events.clone(),
        workspace.clone(),
        String::new(),
        {
            let mut config = haarness_core::RuntimeConfig::default();
            config.base_rules.push(haarness_core::PermissionRule {
                pattern: "write:*".into(),
                decision: haarness_core::PermissionDecision::Allow,
            });
            config
        },
    )
    .with_snapshots(snapshots);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Undo confirm fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let mutation_request = turn_request(r#"!tool write {"path":"scrap.txt","content":"x"}"#);
    let turn = runtime
        .start_turn(thread.id, mutation_request.clone())
        .await
        .unwrap();
    wait_done(&runtime, &turn).await;
    let before = runtime.items(thread.id, None).await.unwrap().len();

    runtime.revert(thread.id, None).await.unwrap();
    let duplicate = runtime
        .start_turn(thread.id, mutation_request)
        .await
        .unwrap();
    assert_eq!(duplicate.id, turn.id);
    assert!(
        runtime
            .get_thread(thread.id)
            .await
            .unwrap()
            .revert
            .is_some(),
        "an idempotent retry must not confirm the pending revert"
    );
    runtime.unrevert(thread.id).await.unwrap();
    assert!(
        workspace.join("scrap.txt").exists(),
        "redo remains available"
    );
    runtime.revert(thread.id, None).await.unwrap();
    let second = runtime
        .start_turn(thread.id, turn_request("fresh direction"))
        .await
        .unwrap();
    wait_done(&runtime, &second).await;
    assert!(
        runtime
            .get_thread(thread.id)
            .await
            .unwrap()
            .revert
            .is_none(),
        "a genuinely new turn confirms the revert"
    );
    assert!(events.subscribe(None).replay.iter().any(|event| {
        event.event_type == "thread.updated"
            && event.turn_id == Some(second.id)
            && event.payload["revert"].is_null()
    }));
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(
        items.len() < before,
        "undone history leaves the listing ({} -> {})",
        before,
        items.len()
    );
    assert!(
        items.iter().any(
            |item| matches!(&item.data, haarness_protocol::ItemData::UserMessage { text, .. }
                if text == "fresh direction")
        ),
        "the confirming turn is present"
    );
    assert!(
        !items.iter().any(|item| {
            matches!(&item.data, haarness_protocol::ItemData::ToolCall { name, .. } if name == "write")
        }),
        "the undone tool call is hidden"
    );
    let fork = runtime.fork_thread(thread.id, None, None).await.unwrap();
    assert!(
        !runtime.items(fork.id, None).await.unwrap().iter().any(|item| {
            matches!(&item.data, haarness_protocol::ItemData::ToolCall { name, .. } if name == "write")
        }),
        "forking after revert confirmation must not copy soft-deleted history"
    );
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn direct_tool_after_undo_commits_the_boundary_before_mutating() {
    let root = std::env::temp_dir().join(format!("haarness-direct-undo-{}", uuid::Uuid::now_v7()));
    let workspace = root.join("ws");
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    let database = root.join("db.sqlite");
    let store = Arc::new(SqliteStore::open(&database).unwrap());
    let snapshots =
        Arc::new(haarness_store::SqliteSnapshotStore::open(&database, workspace.clone()).unwrap());
    let runtime = AgentRuntime::with_config(
        store,
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(haarness_server::NativeToolRegistry::new(workspace.clone())),
        EventBus::new(64),
        workspace.clone(),
        String::new(),
        {
            let mut config = haarness_core::RuntimeConfig::default();
            config.base_rules.push(haarness_core::PermissionRule {
                pattern: "write:*".into(),
                decision: haarness_core::PermissionDecision::Allow,
            });
            config
        },
    )
    .with_snapshots(snapshots);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Direct undo commit fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!tool write {"path":"undone.txt","content":"old"}"#),
        )
        .await
        .unwrap();
    wait_done(&runtime, &turn).await;
    runtime.revert(thread.id, None).await.unwrap();
    assert!(!workspace.join("undone.txt").exists());

    runtime
        .run_direct_tool(
            thread.id,
            "write",
            serde_json::json!({"path": "replacement.txt", "content": "new"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        runtime
            .get_thread(thread.id)
            .await
            .unwrap()
            .revert
            .is_none(),
        "a direct mutation atomically consumes redo state"
    );
    assert!(!workspace.join("undone.txt").exists());
    assert_eq!(
        tokio::fs::read_to_string(workspace.join("replacement.txt"))
            .await
            .unwrap(),
        "new"
    );
    assert!(
        runtime.unrevert(thread.id).await.is_err(),
        "redo is unavailable after the direct action commits undo"
    );
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn duplicate_request_id_is_not_replayed() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(Echo) as Arc<dyn ModelProvider>
    ]));
    let runtime = AgentRuntime::new(
        store,
        providers,
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let request = StartTurnRequest {
        request_id: RequestId::new(),
        text: "once".into(),
        attachments: vec![],
        synthetic: false,
        auto_approve: false,
        agent: None,
        provider: None,
        model: None,
        reasoning_effort: None,
        tools: None,
        command: None,
    };
    let first = runtime
        .start_turn(thread.id, request.clone())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    let second = runtime.start_turn(thread.id, request).await.unwrap();
    assert_eq!(first.id, second.id);
    let items = runtime.items(thread.id, None).await.unwrap();
    assert_eq!(
        items
            .iter()
            .filter(|item| matches!(item.data, haarness_protocol::ItemData::UserMessage { .. }))
            .count(),
        1
    );
}

// ---------------------------------------------------------------------------
// P0: parallel child agents, cross-thread permission visibility, steering.
// ---------------------------------------------------------------------------

/// Runtime wired like the fixtures above: real tools, capturing provider,
/// large event ring so tests can assert on the full event history.
fn p0_runtime(workspace: &std::path::Path, allow_bash: bool) -> (AgentRuntime, Capturing) {
    let provider = Capturing::default();
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
    ]));
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        providers,
        Arc::new(haarness_server::NativeToolRegistry::new(
            workspace.to_path_buf(),
        )),
        EventBus::new(2048),
        workspace.to_path_buf(),
        String::new(),
        {
            let mut config = haarness_core::RuntimeConfig::default();
            if !allow_bash {
                // Parity defaults auto-run bash; these tests exercise the
                // prompt flow, so tighten it back via config.
                config.base_rules.push(haarness_core::PermissionRule {
                    pattern: "bash:*".into(),
                    decision: haarness_core::PermissionDecision::Ask,
                });
            }
            config
        },
    );
    (runtime, provider)
}

/// `!multi` prompt spawning `count` task children that each run `command`.
fn tasks_prompt(count: usize, command: &str) -> String {
    let calls: Vec<Value> = (0..count)
        .map(|_| {
            serde_json::json!({
                "name": "task",
                "arguments": {
                    "prompt": format!("!tool bash {}", serde_json::json!({"command": command})),
                    "agent": "general",
                }
            })
        })
        .collect();
    format!("!multi {}", serde_json::to_string(&calls).unwrap())
}

async fn wait_pending_permissions(runtime: &AgentRuntime, count: usize) {
    for _ in 0..500 {
        if runtime.pending_permissions().len() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "expected {count} pending permissions, saw {}",
        runtime.pending_permissions().len()
    );
}

async fn wait_pending_questions(runtime: &AgentRuntime, count: usize) {
    for _ in 0..500 {
        if runtime.pending_questions().len() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "expected {count} pending questions, saw {}",
        runtime.pending_questions().len()
    );
}

#[tokio::test]
async fn parallel_child_questions_hydrate_and_resolve_independently_over_rest() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, true);
    let runtime = Arc::new(runtime);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let calls = serde_json::json!([
        {
            "name": "task",
            "arguments": {
                "prompt": format!(
                    "!tool question {}",
                    serde_json::json!({"prompt":"Choose alpha","choices":["a1","a2"]})
                ),
                "agent": "general"
            }
        },
        {
            "name": "task",
            "arguments": {
                "prompt": format!(
                    "!tool question {}",
                    serde_json::json!({"prompt":"Choose beta","choices":["b1","b2"],"custom":false})
                ),
                "agent": "general"
            }
        }
    ]);
    let turn = runtime
        .start_turn(thread.id, turn_request(&format!("!multi {calls}")))
        .await
        .unwrap();
    wait_pending_questions(&runtime, 2).await;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = serve(
        listener,
        "question-token",
        ServerState {
            browser: None,
            management: None,
            runtime: Arc::clone(&runtime),
            mcp: Arc::new(NoMcp),
            skills: Arc::new(NoSkills),
            plugins: None,
            workflows: None,
            workspaces: None,
            workspace_id: None,
            workspace,
            project_trusted: false,
            language: None,
        },
    )
    .await
    .unwrap();
    let base = format!("http://{}", handle.address);
    let client = reqwest::Client::builder()
        .default_headers({
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::AUTHORIZATION,
                "Bearer question-token".parse().unwrap(),
            );
            headers
        })
        .build()
        .unwrap();

    let pending: Vec<haarness_protocol::PendingQuestionInfo> = client
        .get(format!("{base}/v1/questions"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pending.len(), 2);
    assert!(
        pending
            .windows(2)
            .all(|pair| pair[0].created_at <= pair[1].created_at)
    );
    assert!(
        pending
            .iter()
            .all(|question| question.root_thread_id == Some(thread.id))
    );
    assert_ne!(pending[0].thread_id, pending[1].thread_id);
    assert!(
        pending
            .iter()
            .all(|question| question.thread_id != thread.id)
    );
    assert!(
        pending
            .iter()
            .find(|question| question.prompt == "Choose alpha")
            .is_some_and(|question| question.custom),
        "an omitted custom flag defaults to a free-form answer"
    );
    assert!(
        pending
            .iter()
            .find(|question| question.prompt == "Choose beta")
            .is_some_and(|question| !question.custom),
        "an explicit custom=false remains authoritative"
    );

    let child_pending: Option<haarness_protocol::PendingQuestionInfo> = client
        .get(format!(
            "{base}/v1/threads/{}/pending-question",
            pending[0].thread_id
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        child_pending.map(|question| question.question_id),
        Some(pending[0].question_id.clone())
    );

    client
        .post(format!("{base}/v1/questions/{}", pending[0].question_id))
        .json(&haarness_protocol::QuestionReply {
            answer: pending[0].choices[0].clone(),
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let remaining: Vec<haarness_protocol::PendingQuestionInfo> = client
        .get(format!("{base}/v1/questions"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(remaining.len(), 1, "answering one advances the queue");
    assert_eq!(remaining[0].question_id, pending[1].question_id);

    client
        .post(format!("{base}/v1/questions/{}", remaining[0].question_id))
        .json(&haarness_protocol::QuestionReply {
            answer: remaining[0].choices[1].clone(),
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let done = wait_done(&runtime, &turn).await;
    assert_eq!(done.status, TurnStatus::Completed);
    assert!(runtime.pending_questions().is_empty());

    let resolved: Vec<_> = runtime
        .events()
        .subscribe(None)
        .replay
        .into_iter()
        .filter(|event| event.event_type == "question.resolved")
        .collect();
    assert_eq!(resolved.len(), 2);
    assert!(resolved.iter().all(|event| {
        event.root_thread_id == Some(thread.id)
            && event.thread_id.is_some_and(|child| child != thread.id)
            && event.payload["resolution"] == "answered"
    }));
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupt_resolves_every_pending_child_question() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, true);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let calls = serde_json::json!([
        {
            "name": "task",
            "arguments": {
                "prompt": "!tool question {\"prompt\":\"First?\"}",
                "agent": "general"
            }
        },
        {
            "name": "task",
            "arguments": {
                "prompt": "!tool question {\"prompt\":\"Second?\"}",
                "agent": "general"
            }
        }
    ]);
    let turn = runtime
        .start_turn(thread.id, turn_request(&format!("!multi {calls}")))
        .await
        .unwrap();
    wait_pending_questions(&runtime, 2).await;

    runtime.interrupt(turn.id).await.unwrap();
    for _ in 0..100 {
        if runtime.pending_questions().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        runtime.pending_questions().is_empty(),
        "interrupt must not leave stale prompts"
    );
    let cancelled: Vec<_> = runtime
        .events()
        .subscribe(None)
        .replay
        .into_iter()
        .filter(|event| {
            event.event_type == "question.resolved" && event.payload["resolution"] == "cancelled"
        })
        .collect();
    assert_eq!(cancelled.len(), 2);
    assert!(
        cancelled
            .iter()
            .all(|event| event.root_thread_id == Some(thread.id))
    );
}

#[tokio::test]
async fn parallel_task_children_run_concurrently_and_persist_in_order() {
    let workspace = std::env::temp_dir().join(format!(
        "haarness-parallel-children-{}",
        uuid::Uuid::now_v7()
    ));
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    let (runtime, _) = p0_runtime(&workspace, true);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    // Each child waits for all three start markers. Serial execution would
    // deadlock the first child until its tool timeout; concurrent execution
    // releases the barrier and lets every child finish without relying on a
    // host-speed benchmark.
    let barrier =
        "[ ! -f child-1-started ] || [ ! -f child-2-started ] || [ ! -f child-3-started ]";
    let calls = (1..=3)
        .map(|index| {
            let command = format!(
                "touch child-{index}-started; while {barrier}; do sleep 0.02; done; touch child-{index}-finished"
            );
            serde_json::json!({
                "name": "task",
                "arguments": {
                    "prompt": format!(
                        "!tool bash {}",
                        serde_json::json!({"command": command, "timeout": 10_000})
                    ),
                    "agent": "general",
                }
            })
        })
        .collect::<Vec<_>>();
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&format!(
                "!multi {}",
                serde_json::to_string(&calls).unwrap()
            )),
        )
        .await
        .unwrap();
    let done = wait_done(&runtime, &turn).await;
    assert_eq!(done.status, TurnStatus::Completed);
    for index in 1..=3 {
        assert!(
            workspace.join(format!("child-{index}-finished")).exists(),
            "every child crossed the all-started barrier"
        );
    }

    let items = runtime.items(thread.id, None).await.unwrap();
    let results: Vec<(&str, bool)> = items
        .iter()
        .filter_map(|item| match &item.data {
            haarness_protocol::ItemData::ToolResult {
                call_id, is_error, ..
            } => Some((call_id.as_str(), *is_error)),
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        vec![
            ("cap-call-1", false),
            ("cap-call-2", false),
            ("cap-call-3", false)
        ],
        "results persist in call order and all succeed"
    );

    // Child liveness reached the parent thread as task.updated events with
    // three distinct child thread ids.
    let events = runtime.events().subscribe(None).replay;
    let child_threads: std::collections::HashSet<String> = events
        .iter()
        .filter(|event| event.event_type == "task.updated")
        .filter_map(|event| event.payload["childThreadId"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(child_threads.len(), 3, "three children reported progress");
    let durable_links: std::collections::HashSet<String> = items
        .iter()
        .filter_map(|item| match &item.data {
            haarness_protocol::ItemData::TaskLink {
                child_thread_id, ..
            } => Some(child_thread_id.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(
        durable_links, child_threads,
        "every live child update has a durable parent-task navigation link"
    );
    assert!(
        events
            .iter()
            .filter(|event| event.event_type == "task.updated")
            .all(|event| event.thread_id == Some(thread.id)
                && event.root_thread_id == Some(thread.id)),
        "task.updated events land on the parent thread with root attribution"
    );
    tokio::fs::remove_dir_all(workspace).await.unwrap();
}

#[tokio::test]
async fn workspace_tools_match_haarness_and_do_not_serialize_across_threads() {
    let root = std::env::temp_dir().join(format!(
        "haarness-concurrent-mutations-{}",
        uuid::Uuid::now_v7()
    ));
    let workspace = root.join("ws");
    tokio::fs::create_dir_all(workspace.join("crates/demo/src"))
        .await
        .unwrap();
    tokio::fs::write(
        workspace.join("crates/demo/src/lib.rs"),
        "pub fn demo() {}\n",
    )
    .await
    .unwrap();
    let database = root.join("db.sqlite");
    let store = Arc::new(SqliteStore::open(&database).unwrap());
    let snapshots =
        Arc::new(haarness_store::SqliteSnapshotStore::open(&database, workspace.clone()).unwrap());
    let runtime = AgentRuntime::with_config(
        store,
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(haarness_server::NativeToolRegistry::new(workspace.clone())),
        EventBus::new(2048),
        workspace.clone(),
        String::new(),
        RuntimeConfig::default(),
    );
    let runtime = Arc::new(runtime.with_snapshots(snapshots));

    let first = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let first_arguments = serde_json::json!({
        "command": "touch mutation-started; sleep 30; touch mutation-finished",
        "timeout_ms": 60_000,
    });
    let first_turn = runtime
        .start_turn(
            first.id,
            turn_request(&format!("!tool bash {first_arguments}")),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !workspace.join("mutation-started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("first mutation should start");

    let inspection = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let inspect_arguments = serde_json::json!({
        "command": "find crates -maxdepth 3 -type f | sort && printf '\\n--- workflows ---\\n' && find . -maxdepth 2 -type f | sort",
        "timeout_ms": 10_000,
    });
    let inspect_turn = runtime
        .start_turn(
            inspection.id,
            turn_request(&format!("!tool bash {inspect_arguments}")),
        )
        .await
        .unwrap();
    let inspect_done =
        tokio::time::timeout(Duration::from_secs(10), wait_done(&runtime, &inspect_turn))
            .await
            .expect("read-only inspection must not queue behind a mutation");
    assert_eq!(inspect_done.status, TurnStatus::Completed);
    assert!(
        runtime
            .items(inspection.id, None)
            .await
            .unwrap()
            .iter()
            .any(|item| matches!(
                &item.data,
                haarness_protocol::ItemData::ToolResult { output, is_error: false, .. }
                    if output.contains("crates/demo/src/lib.rs")
            ))
    );

    let concurrent = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let concurrent_arguments = serde_json::json!({
        "command": "touch concurrent-started; sleep 0.2; touch concurrent-finished",
        "timeout_ms": 10_000,
    });
    let concurrent_turn = runtime
        .start_turn(
            concurrent.id,
            turn_request(&format!("!tool bash {concurrent_arguments}")),
        )
        .await
        .unwrap();
    let concurrent_done = tokio::time::timeout(
        Duration::from_secs(10),
        wait_done(&runtime, &concurrent_turn),
    )
    .await
    .expect("a mutation in another session must not wait for the first turn");
    assert_eq!(concurrent_done.status, TurnStatus::Completed);
    assert!(
        workspace.join("concurrent-finished").exists(),
        "the second workspace mutation completed"
    );
    assert!(
        !workspace.join("mutation-finished").exists(),
        "the first mutation was still running when the second completed"
    );

    runtime.interrupt_thread_tree(first.id).await.unwrap();
    let first_done =
        tokio::time::timeout(Duration::from_secs(10), wait_done(&runtime, &first_turn))
            .await
            .expect("interrupt stops the first mutation promptly");
    assert_eq!(first_done.status, TurnStatus::Interrupted);
    wait_idle(&runtime, first.id).await;
    wait_idle(&runtime, concurrent.id).await;
    for thread_id in [first.id, concurrent.id] {
        assert_eq!(
            runtime
                .items(thread_id, None)
                .await
                .unwrap()
                .iter()
                .filter(|item| matches!(item.data, haarness_protocol::ItemData::Snapshot { .. }))
                .count(),
            2,
            "each mutating turn retains an independent before/after checkpoint pair"
        );
    }

    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn child_provider_schema_hides_recursive_and_categorically_denied_tools() {
    let workspace = std::env::temp_dir();
    let provider = Capturing::default();
    let requests = provider.requests.clone();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider) as Arc<dyn ModelProvider>
        ])),
        Arc::new(haarness_server::NativeToolRegistry::new(workspace.clone())),
        EventBus::new(256),
        workspace,
        String::new(),
        RuntimeConfig {
            base_rules: vec![haarness_core::PermissionRule {
                pattern: "webfetch:*".into(),
                decision: haarness_core::PermissionDecision::Deny,
            }],
            ..RuntimeConfig::default()
        },
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!tool task {"prompt":"inspect child tools","agent":"general"}"#),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );

    let requests = requests.lock().unwrap();
    let root = requests.first().expect("root provider request");
    assert!(root.tools.iter().any(|tool| tool.name == "task"));
    let child = requests
        .iter()
        .find(|request| {
            request.messages.iter().any(|message| {
                matches!(message, ProviderMessage::Text { role, content }
                    if role == "user" && content == "inspect child tools")
            })
        })
        .expect("child provider request");
    assert!(child.tools.iter().any(|tool| tool.name == "read"));
    assert!(
        child.tools.iter().all(|tool| tool.name != "task"),
        "default child agents must not be offered recursive task spawning"
    );
    assert!(
        child.tools.iter().all(|tool| tool.name != "webfetch"),
        "categorically denied tools must be absent from the provider schema"
    );
}

#[tokio::test]
async fn assistant_preface_is_persisted_before_its_tool_activity() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, true);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!preface bash {"command":"true"}"#),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let items = runtime.items(thread.id, None).await.unwrap();
    let reasoning = items
        .iter()
        .filter_map(|item| match &item.data {
            haarness_protocol::ItemData::Reasoning { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(reasoning, ["Inspecting safely"]);
    let preface = items
        .iter()
        .find(|item| {
            matches!(&item.data,
            haarness_protocol::ItemData::AssistantMessage { text }
                if text == "I’ll inspect the repository first.")
        })
        .expect("assistant preface");
    let call = items
        .iter()
        .find(|item| {
            matches!(&item.data,
            haarness_protocol::ItemData::ToolCall { name, .. } if name == "bash")
        })
        .expect("bash call");
    assert!(
        preface.sequence < call.sequence,
        "streamed prose must be durable before the tool call it introduces"
    );
}

#[tokio::test]
async fn blank_task_id_creates_a_new_child_instead_of_failing_validation() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, true);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let prompt = serde_json::json!({
        "prompt": "inspect with a fresh child",
        "agent": "review",
        "description": "blank continuation regression",
        "task_id": "   ",
    });
    let turn = runtime
        .start_turn(thread.id, turn_request(&format!("!tool task {prompt}")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(
        items
            .iter()
            .any(|item| matches!(&item.data, haarness_protocol::ItemData::TaskLink { .. }))
    );
    assert!(items.iter().any(|item| matches!(&item.data,
        haarness_protocol::ItemData::ToolResult { name, is_error, .. }
            if name == "task" && !is_error)));
}

#[tokio::test]
async fn invented_task_id_placeholders_create_fresh_children() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, true);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let calls = serde_json::json!([
        {
            "name": "task",
            "arguments": {
                "prompt": "audit server security",
                "agent": "review",
                "task_id": "server-sec"
            }
        },
        {
            "name": "task",
            "arguments": {
                "prompt": "audit execution security",
                "agent": "review",
                "task_id": "01K00000000000000000000001"
            }
        },
        {
            "name": "task",
            "arguments": {
                "prompt": "audit credential security",
                "agent": "review",
                "task_id": "new"
            }
        },
        {
            "name": "task",
            "arguments": {
                "prompt": "audit dependency security",
                "agent": "review",
                "task_id": "00000000-0000-1000-8000-000000000000"
            }
        }
    ]);
    let prompt = format!("!multi {calls}");
    let turn = runtime
        .start_turn(thread.id, turn_request(&prompt))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let children = runtime
        .list_threads(false)
        .await
        .unwrap()
        .into_iter()
        .filter(|candidate| candidate.parent_id == Some(thread.id))
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 4);
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(items.iter().all(|item| !matches!(
        &item.data,
        haarness_protocol::ItemData::ToolResult {
            name,
            output,
            is_error: true,
            ..
        } if name == "task" && output.contains("task_id must be a child thread ID")
    )));
}

#[tokio::test]
async fn background_subagent_returns_immediately_then_delivers_into_the_parent() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, true);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let prompt = serde_json::json!({
        "prompt": "!tool bash {\"command\":\"sleep 2\"}",
        "agent": "general",
        "description": "slow independent check",
        "background": true,
    });
    let turn = runtime
        .start_turn(thread.id, turn_request(&format!("!tool task {prompt}")))
        .await
        .unwrap();

    let root = tokio::time::timeout(Duration::from_millis(750), wait_done(&runtime, &turn))
        .await
        .expect("the parent should not wait for the two-second child");
    assert_eq!(root.status, TurnStatus::Completed);
    let running = runtime.background_tasks(thread.id).await.unwrap();
    assert_eq!(running.len(), 1);
    assert!(running[0].background);
    let child = running[0].child_thread_id;

    let update = serde_json::json!({
        "prompt": "include this follow-up context in the same child",
        "agent": "general",
        "description": "extend existing check",
        "task_id": child,
    });
    let update_turn = runtime
        .start_turn(thread.id, turn_request(&format!("!tool task {update}")))
        .await
        .unwrap();
    let updated_parent = tokio::time::timeout(
        Duration::from_millis(750),
        wait_done(&runtime, &update_turn),
    )
    .await
    .expect("adding context to a running child should also return immediately");
    assert_eq!(updated_parent.status, TurnStatus::Completed);
    let still_running = runtime.background_tasks(thread.id).await.unwrap();
    assert_eq!(still_running.len(), 1);
    assert_eq!(still_running[0].child_thread_id, child);

    tokio::time::timeout(Duration::from_secs(7), async {
        loop {
            if runtime
                .background_tasks(thread.id)
                .await
                .unwrap()
                .is_empty()
                && runtime.active_turn(thread.id).await.unwrap().is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("background completion and parent delivery should drain");

    let items = runtime.items(thread.id, None).await.unwrap();
    let launch = items
        .iter()
        .find(|item| {
            matches!(&item.data,
                haarness_protocol::ItemData::ToolResult { name, output, is_error, .. }
                    if name == "task" && !is_error && output.contains("state=\"running\""))
        })
        .expect("background launch result is durable");
    let delivered = items
        .iter()
        .find(|item| {
            matches!(&item.data,
                haarness_protocol::ItemData::UserMessage { text, synthetic, .. }
                    if *synthetic
                        && text.contains(&child.to_string())
                        && text.contains("state=\"completed\""))
        })
        .expect("completion is injected as a synthetic parent message");
    assert!(
        launch.sequence < delivered.sequence,
        "the tool result must be durable before automatic delivery"
    );
    assert!(items.iter().any(|item| matches!(&item.data,
        haarness_protocol::ItemData::TaskState { child_thread_id, state, .. }
            if *child_thread_id == child && state == "background")));
    assert!(items.iter().any(|item| matches!(&item.data,
        haarness_protocol::ItemData::TaskState { child_thread_id, state, .. }
            if *child_thread_id == child && state == "done")));
    assert!(items.iter().any(|item| {
        item.sequence > delivered.sequence
            && matches!(
                &item.data,
                haarness_protocol::ItemData::AssistantMessage { .. }
            )
    }));
    let children = runtime
        .list_threads(false)
        .await
        .unwrap()
        .into_iter()
        .filter(|candidate| candidate.parent_id == Some(thread.id))
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 1, "task_id continuation reuses the child");
    let child_items = runtime.items(child, None).await.unwrap();
    assert!(child_items.iter().any(|item| matches!(&item.data,
        haarness_protocol::ItemData::UserMessage { text, .. }
            if text == "include this follow-up context in the same child")));
}

#[tokio::test]
async fn interrupting_the_parent_stops_background_children_without_reinjecting() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, true);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let prompt = serde_json::json!({
        "prompt": "!tool bash {\"command\":\"sleep 5\"}",
        "agent": "general",
        "description": "cancel with parent",
        "background": true,
    });
    let launch = runtime
        .start_turn(thread.id, turn_request(&format!("!tool task {prompt}")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &launch).await.status,
        TurnStatus::Completed
    );
    let child = runtime
        .background_tasks(thread.id)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("background child");

    // Model a later in-session operation that the user explicitly cancels.
    // Session cancellation owns earlier background work too.
    let current = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!tool bash {"command":"sleep 5"}"#),
        )
        .await
        .unwrap();
    runtime.interrupt(current.id).await.unwrap();

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if runtime
                .background_tasks(thread.id)
                .await
                .unwrap()
                .is_empty()
                && runtime
                    .active_turn(child.child_thread_id)
                    .await
                    .unwrap()
                    .is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("parent cancellation should drain its background child");

    assert_eq!(
        runtime.turn(child.child_turn_id).await.unwrap().status,
        TurnStatus::Interrupted
    );
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(items.iter().any(|item| matches!(&item.data,
        haarness_protocol::ItemData::TaskState { child_thread_id, state, .. }
            if *child_thread_id == child.child_thread_id && state == "cancelled")));
    assert!(
        items.iter().all(|item| !matches!(&item.data,
        haarness_protocol::ItemData::UserMessage { text, synthetic, .. }
            if *synthetic
                && text.contains(&child.child_thread_id.to_string())
                && text.contains("state=\"cancelled\""))),
        "an explicit parent stop must not start a synthetic cancellation turn"
    );
}

#[tokio::test]
async fn targeted_task_abort_stops_only_the_owned_child() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, true);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let calls = serde_json::json!([
        {"name":"task","arguments":{"prompt":"!tool bash {\"command\":\"sleep 5\"}","agent":"general","background":true,"description":"abort me"}},
        {"name":"task","arguments":{"prompt":"!tool bash {\"command\":\"sleep 5\"}","agent":"general","background":true,"description":"keep me"}}
    ]);
    let launch = runtime
        .start_turn(thread.id, turn_request(&format!("!multi {calls}")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &launch).await.status,
        TurnStatus::Completed
    );
    let tasks = runtime.background_tasks(thread.id).await.unwrap();
    assert_eq!(tasks.len(), 2);
    let aborted = tasks[0].clone();
    let sibling = tasks[1].clone();

    runtime
        .abort_child_task(thread.id, aborted.child_thread_id)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let remaining = runtime.background_tasks(thread.id).await.unwrap();
            if remaining.len() == 1 {
                assert_eq!(remaining[0].child_thread_id, sibling.child_thread_id);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("targeted cancellation should remove only one child");
    assert_eq!(
        runtime.turn(aborted.child_turn_id).await.unwrap().status,
        TurnStatus::Interrupted
    );
    assert!(
        runtime
            .active_turn(sibling.child_thread_id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        runtime
            .abort_child_task(sibling.child_thread_id, sibling.child_thread_id)
            .await,
        Err(haarness_core::CoreError::PermissionDenied(_) | haarness_core::CoreError::NotFound(_))
    ));
    runtime.interrupt_thread_tree(thread.id).await.unwrap();
}

#[tokio::test]
async fn interrupting_an_idle_root_stops_its_still_running_background_tree() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, true);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let prompt = serde_json::json!({
        "prompt": "!tool bash {\"command\":\"sleep 5\"}",
        "agent": "general",
        "description": "idle root cancellation",
        "background": true,
    });
    let launch = runtime
        .start_turn(thread.id, turn_request(&format!("!tool task {prompt}")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &launch).await.status,
        TurnStatus::Completed
    );
    assert!(runtime.active_turn(thread.id).await.unwrap().is_none());
    let child = runtime
        .background_tasks(thread.id)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("background child remains active after the root turn");

    assert!(runtime.interrupt_thread_tree(thread.id).await.unwrap() > 0);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if runtime
                .background_tasks(thread.id)
                .await
                .unwrap()
                .is_empty()
                && runtime
                    .active_turn(child.child_thread_id)
                    .await
                    .unwrap()
                    .is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("idle-root cancellation should drain its background tree");
    assert_eq!(
        runtime.turn(child.child_turn_id).await.unwrap().status,
        TurnStatus::Interrupted
    );
}

#[tokio::test]
async fn completed_task_id_continues_the_same_durable_child_session() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, true);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let first = serde_json::json!({
        "prompt": "first child pass",
        "agent": "general",
        "description": "first pass",
    });
    let first_turn = runtime
        .start_turn(thread.id, turn_request(&format!("!tool task {first}")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &first_turn).await.status,
        TurnStatus::Completed
    );
    let child = runtime
        .items(thread.id, None)
        .await
        .unwrap()
        .into_iter()
        .find_map(|item| match item.data {
            haarness_protocol::ItemData::TaskLink {
                child_thread_id, ..
            } => Some(child_thread_id),
            _ => None,
        })
        .expect("first child link");

    let second = serde_json::json!({
        "prompt": "second child pass",
        "description": "continue completed child",
        "task_id": child,
    });
    let second_turn = runtime
        .start_turn(thread.id, turn_request(&format!("!tool task {second}")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &second_turn).await.status,
        TurnStatus::Completed
    );

    let children = runtime
        .list_threads(false)
        .await
        .unwrap()
        .into_iter()
        .filter(|candidate| candidate.parent_id == Some(thread.id))
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, child);
    let child_items = runtime.items(child, None).await.unwrap();
    let prompts = child_items
        .iter()
        .filter_map(|item| match &item.data {
            haarness_protocol::ItemData::UserMessage { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(prompts, ["first child pass", "second child pass"]);
}

#[tokio::test]
async fn running_foreground_subagent_can_be_promoted_without_restart() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, true);
    let runtime = Arc::new(runtime);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = serve(
        listener,
        "background-token",
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
            workspace: workspace.clone(),
            project_trusted: false,
            language: None,
        },
    )
    .await
    .unwrap();
    let base = format!("http://{}", handle.address);
    let client = reqwest::Client::builder()
        .default_headers({
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::AUTHORIZATION,
                "Bearer background-token".parse().unwrap(),
            );
            headers
        })
        .build()
        .unwrap();
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let prompt = serde_json::json!({
        "prompt": "!tool bash {\"command\":\"sleep 1\"}",
        "agent": "general",
        "description": "promotable check",
    });
    let turn = runtime
        .start_turn(thread.id, turn_request(&format!("!tool task {prompt}")))
        .await
        .unwrap();
    let foreground = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let tasks = runtime.background_tasks(thread.id).await.unwrap();
            if let Some(task) = tasks.first() {
                break task.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("foreground child should become inspectable");
    assert!(!foreground.background);
    let listed: Vec<haarness_protocol::BackgroundTaskInfo> = client
        .get(format!("{base}/v1/threads/{}/background", thread.id))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed, vec![foreground.clone()]);

    let promoted: Vec<haarness_protocol::BackgroundTaskInfo> = client
        .post(format!(
            "{base}/v1/threads/{}/background?childThreadId={}",
            thread.id, foreground.child_thread_id
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(promoted.len(), 1);
    assert!(promoted[0].background);
    let root = tokio::time::timeout(Duration::from_millis(750), wait_done(&runtime, &turn))
        .await
        .expect("promotion should release the parent before the child finishes");
    assert_eq!(root.status, TurnStatus::Completed);

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if runtime
                .background_tasks(thread.id)
                .await
                .unwrap()
                .is_empty()
                && runtime.active_turn(thread.id).await.unwrap().is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("promoted child should complete and deliver");
    let children = runtime
        .list_threads(false)
        .await
        .unwrap()
        .into_iter()
        .filter(|candidate| candidate.parent_id == Some(thread.id))
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 1, "promotion must not restart the child");
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelling_one_foreground_subagent_returns_a_terminal_notice_without_a_retry_error() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, true);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let prompt = serde_json::json!({
        "prompt": "!tool bash {\"command\":\"sleep 5\"}",
        "agent": "general",
        "description": "cancel only this child",
    });
    let parent = runtime
        .start_turn(thread.id, turn_request(&format!("!tool task {prompt}")))
        .await
        .unwrap();
    let child = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(child) = runtime
                .background_tasks(thread.id)
                .await
                .unwrap()
                .first()
                .cloned()
            {
                break child;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("foreground child should start");
    runtime.interrupt(child.child_turn_id).await.unwrap();
    let done = tokio::time::timeout(Duration::from_secs(3), wait_done(&runtime, &parent))
        .await
        .expect("parent should receive child cancellation");
    assert_eq!(done.status, TurnStatus::Completed);
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(items.iter().any(|item| matches!(&item.data,
        haarness_protocol::ItemData::ToolResult { name, output, is_error, .. }
            if name == "task"
                && !is_error
                && output.contains("state=\"cancelled\"")
                && output.contains("Do not relaunch"))));
}

#[tokio::test]
async fn child_bash_permission_surfaces_globally_and_answer_unblocks() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, false);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request(&tasks_prompt(1, "true")))
        .await
        .unwrap();
    wait_pending_permissions(&runtime, 1).await;
    let pending = runtime.pending_permissions();
    assert_eq!(pending[0].operation, "bash:true");
    assert_eq!(
        pending[0].root_thread_id,
        Some(thread.id),
        "child permission carries the root thread"
    );
    assert_ne!(
        pending[0].thread_id, thread.id,
        "the prompt itself lives on the child thread"
    );
    runtime
        .reply_permission(
            &pending[0].permission_id,
            haarness_protocol::PermissionReply {
                decision: haarness_protocol::PermissionReplyDecision::Once,
                feedback: None,
            },
        )
        .await
        .unwrap();
    let done = wait_done(&runtime, &turn).await;
    assert_eq!(done.status, TurnStatus::Completed);
    assert!(runtime.pending_permissions().is_empty());
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(
        items.iter().any(|item| matches!(&item.data,
            haarness_protocol::ItemData::ToolResult { name, is_error, .. }
                if name == "task" && !is_error)),
        "the child task completed successfully after approval"
    );
}

#[tokio::test]
async fn always_reply_resolves_all_matching_sibling_permissions() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, false);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request(&tasks_prompt(3, "true")))
        .await
        .unwrap();
    wait_pending_permissions(&runtime, 3).await;
    let pending = runtime.pending_permissions();
    runtime
        .reply_permission(
            &pending[0].permission_id,
            haarness_protocol::PermissionReply {
                decision: haarness_protocol::PermissionReplyDecision::Always,
                feedback: None,
            },
        )
        .await
        .unwrap();
    let done = wait_done(&runtime, &turn).await;
    assert_eq!(done.status, TurnStatus::Completed);
    assert!(
        runtime.pending_permissions().is_empty(),
        "one Always answer covered every identical sibling prompt"
    );
    let resolved = runtime
        .events()
        .subscribe(None)
        .replay
        .iter()
        .filter(|event| event.event_type == "permission.resolved")
        .count();
    assert_eq!(
        resolved, 3,
        "every resolution (direct + cascaded) is announced to clients"
    );
    let items = runtime.items(thread.id, None).await.unwrap();
    let ok_tasks = items
        .iter()
        .filter(|item| {
            matches!(&item.data,
            haarness_protocol::ItemData::ToolResult { name, is_error, .. }
                if name == "task" && !is_error)
        })
        .count();
    assert_eq!(ok_tasks, 3);
}

#[tokio::test]
async fn reject_cascades_to_sibling_permissions_and_turn_survives() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, false);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let calls = serde_json::json!([
        {"name":"task","arguments":{"prompt":"!tool bash {\"command\":\"true\"}","agent":"general"}},
        {"name":"task","arguments":{"prompt":"!tool bash {\"command\":\"false\"}","agent":"general"}},
    ]);
    let turn = runtime
        .start_turn(thread.id, turn_request(&format!("!multi {calls}")))
        .await
        .unwrap();
    wait_pending_permissions(&runtime, 2).await;
    let pending = runtime.pending_permissions();
    runtime
        .reply_permission(
            &pending[0].permission_id,
            haarness_protocol::PermissionReply {
                decision: haarness_protocol::PermissionReplyDecision::Reject,
                feedback: Some("do not run shell commands".into()),
            },
        )
        .await
        .unwrap();
    let done = wait_done(&runtime, &turn).await;
    assert_eq!(
        done.status,
        TurnStatus::Completed,
        "a rejection feeds error results back; the turn still completes"
    );
    assert!(
        runtime.pending_permissions().is_empty(),
        "rejecting one prompt rejected its sibling too"
    );
    // The rejection lands inside each child thread as a failed bash call (the
    // child model then course-corrects and finishes), so neither child hangs.
    let children: Vec<Thread> = runtime
        .list_threads(false)
        .await
        .unwrap()
        .into_iter()
        .filter(|candidate| candidate.parent_id == Some(thread.id))
        .collect();
    assert_eq!(children.len(), 2);
    for child in children {
        let child_items = runtime.items(child.id, None).await.unwrap();
        assert!(
            child_items.iter().any(|item| matches!(&item.data,
                haarness_protocol::ItemData::ToolResult { name, is_error, output, .. }
                    if name == "bash" && *is_error
                        && output.contains("rejected")
                        && output.contains("do not run shell commands"))),
            "child {} recorded the rejection",
            child.id
        );
    }
}

#[tokio::test]
async fn queued_message_during_text_stream_runs_in_a_follow_up_round() {
    let workspace = std::env::temp_dir();
    let provider = SlowText::default();
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
    ]));
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        providers,
        Arc::new(haarness_server::NativeToolRegistry::new(workspace.clone())),
        EventBus::new(256),
        workspace,
        String::new(),
        haarness_core::RuntimeConfig::default(),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Slow stream fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request("start slowly"))
        .await
        .unwrap();

    let started = tokio::time::timeout(Duration::from_secs(1), provider.first_started.acquire())
        .await
        .expect("the first provider stream started")
        .unwrap();
    started.forget();
    let response = runtime
        .queue_input(
            thread.id,
            haarness_protocol::QueueInputRequest {
                preview: None,
                reasoning_effort: None,
                delivery: haarness_protocol::InputDelivery::Steer,
                request_id: RequestId::new(),
                text: "arrived during streaming".into(),
                attachments: vec![],
                synthetic: false,
            },
        )
        .await
        .unwrap();
    assert!(response.queued, "the active stream accepted the message");
    let queued_item = response.item.expect("the queued item is returned");
    provider.release_first.add_permits(1);

    let done = wait_done(&runtime, &turn).await;
    assert_eq!(done.status, TurnStatus::Completed);
    {
        let requests = provider.requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            2,
            "a text-only round continues when input arrived after its snapshot"
        );
        assert!(
            requests[1].messages.iter().any(|message| {
                matches!(message, ProviderMessage::Text { role, content }
                    if role == "user" && content.contains("arrived during streaming"))
            }),
            "the second provider snapshot contains the queued message"
        );
    }

    let items = runtime.items(thread.id, None).await.unwrap();
    let marker = queue_consumption_marker(&items, queued_item.id);
    assert_eq!(marker.turn_id, Some(turn.id));
    assert!(
        items.iter().any(|item| matches!(&item.data,
            haarness_protocol::ItemData::AssistantMessage { text }
                if text == "Follow-up response")),
        "the follow-up provider round completed"
    );
}

#[tokio::test]
async fn queued_message_steers_into_running_turn() {
    let workspace = std::env::temp_dir();
    let (runtime, provider) = p0_runtime(&workspace, true);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!tool bash {"command":"sleep 1"}"#),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let response = runtime
        .queue_input(
            thread.id,
            haarness_protocol::QueueInputRequest {
                preview: None,
                reasoning_effort: None,
                delivery: haarness_protocol::InputDelivery::Steer,
                request_id: RequestId::new(),
                text: "hi".into(),
                attachments: vec![],
                synthetic: false,
            },
        )
        .await
        .unwrap();
    assert!(response.queued, "input during an active turn queues");
    let queued_item = response.item.expect("the queued item is returned");
    let done = wait_done(&runtime, &turn).await;
    assert_eq!(done.status, TurnStatus::Completed);

    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(
        items.iter().any(|item| matches!(&item.data,
            haarness_protocol::ItemData::UserMessage { text, queued, .. }
                if text == "hi" && *queued)),
        "the queued message keeps its durable provenance"
    );
    let marker = queue_consumption_marker(&items, queued_item.id);
    assert_eq!(marker.turn_id, Some(turn.id));
    assert!(
        !items.iter().any(|item| matches!(&item.data,
        haarness_protocol::ItemData::UserMessage { queued: true, .. }
            if !items.iter().any(|candidate| matches!(
                candidate.data,
                haarness_protocol::ItemData::QueueConsumed { item_id }
                    if item_id == item.id
            )))),
        "completed turn has no queued message still awaiting consumption"
    );
    let steered = provider.requests.lock().unwrap().iter().any(|request| {
        request.messages.iter().any(|message| {
            matches!(message, ProviderMessage::Text { role, content }
                if role == "user"
                    && content.contains("<system-reminder>")
                    && content.contains("hi"))
        })
    });
    assert!(steered, "the model saw the wrapped steering message");
    let events = runtime.events().subscribe(None).replay;
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "turn.message_queued"
                && event.item_id == Some(queued_item.id)),
        "turn.message_queued published"
    );
    let queued_event = events
        .iter()
        .find(|event| {
            event.event_type == "turn.message_queued" && event.item_id == Some(queued_item.id)
        })
        .unwrap();
    let consumed_event = events
        .iter()
        .find(|event| event.event_type == "item.created" && event.item_id == Some(marker.id))
        .expect("consumption marker is published to reconnecting clients");
    assert!(
        queued_event.event_id < consumed_event.event_id,
        "acceptance is announced before consumption"
    );
    let event_item: haarness_protocol::Item =
        serde_json::from_value(consumed_event.payload.clone()).unwrap();
    assert!(matches!(
        event_item.data,
        haarness_protocol::ItemData::QueueConsumed { item_id }
            if item_id == queued_item.id
    ));
}

#[tokio::test]
async fn queue_input_on_idle_thread_starts_a_turn() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, true);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let response = runtime
        .queue_input(
            thread.id,
            haarness_protocol::QueueInputRequest {
                preview: None,
                reasoning_effort: None,
                delivery: haarness_protocol::InputDelivery::Steer,
                request_id: RequestId::new(),
                text: "hello there".into(),
                attachments: vec![],
                synthetic: false,
            },
        )
        .await
        .unwrap();
    assert!(!response.queued);
    let turn = response.turn.expect("a turn started");
    let done = wait_done(&runtime, &turn).await;
    assert_eq!(done.status, TurnStatus::Completed);
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(items.iter().any(|item| matches!(&item.data,
        haarness_protocol::ItemData::AssistantMessage { text }
            if text.contains("hello there"))));
}

#[tokio::test]
async fn turn_status_transitions_are_published() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, true);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request(r#"!tool bash {"command":"true"}"#))
        .await
        .unwrap();
    wait_done(&runtime, &turn).await;
    let states: Vec<String> = runtime
        .events()
        .subscribe(None)
        .replay
        .iter()
        .filter(|event| event.event_type == "turn.status")
        .filter_map(|event| event.payload["state"].as_str().map(str::to_owned))
        .collect();
    let position = |needle: &str| states.iter().position(|state| state == needle);
    let (thinking, tool, done_state) = (position("thinking"), position("tool"), position("done"));
    assert!(
        thinking.is_some() && tool.is_some() && done_state.is_some(),
        "expected thinking → tool → done transitions, saw {states:?}"
    );
    assert!(thinking < tool && tool < done_state, "ordered: {states:?}");
}

// ---------------------------------------------------------------------------
// Permission engine regression coverage.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn default_ruleset_never_prompts_for_bash_or_writes() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, true);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!tool bash {"command":"git log --oneline -1"}"#),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(
        !items.iter().any(|item| matches!(
            item.data,
            haarness_protocol::ItemData::PermissionRequest { .. }
        )),
        "parity defaults run bash without prompting"
    );
}

#[tokio::test]
async fn always_answer_generalizes_to_the_command_prefix() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, false); // bash asks via config
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!tool bash {"command":"git status --short"}"#),
        )
        .await
        .unwrap();
    wait_pending_permissions(&runtime, 1).await;
    let pending = runtime.pending_permissions();
    assert_eq!(pending[0].permission, "bash");
    assert_eq!(pending[0].patterns, vec!["git status --short"]);
    assert_eq!(pending[0].always_patterns, vec!["bash:git status *"]);
    runtime
        .reply_permission(
            &pending[0].permission_id,
            haarness_protocol::PermissionReply {
                decision: haarness_protocol::PermissionReplyDecision::Always,
                feedback: None,
            },
        )
        .await
        .unwrap();
    wait_done(&runtime, &turn).await;

    // The generalized rule now covers the bare command and other variants.
    let second = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!tool bash {"command":"git status"}"#),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &second).await.status,
        TurnStatus::Completed
    );
    let prompts = runtime
        .items(thread.id, None)
        .await
        .unwrap()
        .iter()
        .filter(|item| {
            matches!(
                item.data,
                haarness_protocol::ItemData::PermissionRequest { .. }
            )
        })
        .count();
    assert_eq!(prompts, 1, "only the first variant prompted");
}

#[tokio::test]
async fn command_chains_prompt_once_with_a_pattern_per_command() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, false);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let _turn = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!tool bash {"command":"echo hi && git log -1"}"#),
        )
        .await
        .unwrap();
    wait_pending_permissions(&runtime, 1).await;
    let pending = runtime.pending_permissions();
    assert_eq!(pending.len(), 1, "one prompt covers the whole chain");
    assert_eq!(pending[0].patterns, vec!["echo hi", "git log -1"]);
    assert_eq!(
        pending[0].always_patterns,
        vec!["bash:echo *", "bash:git log *"]
    );
    runtime
        .reply_permission(
            &pending[0].permission_id,
            haarness_protocol::PermissionReply {
                decision: haarness_protocol::PermissionReplyDecision::Once,
                feedback: None,
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn redirects_outside_the_workspace_ask_even_when_bash_is_allowed() {
    let workspace = std::env::temp_dir().join(format!("haarness-extdir-{}", uuid::Uuid::now_v7()));
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    let (runtime, _) = p0_runtime(&workspace, true); // bash fully allowed
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let target = std::env::temp_dir().join("haarness-extdir-target.txt");
    let command = format!("echo hi > {}", target.display());
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&format!(
                "!tool bash {}",
                serde_json::json!({"command": command})
            )),
        )
        .await
        .unwrap();
    wait_pending_permissions(&runtime, 1).await;
    let pending = runtime.pending_permissions();
    assert_eq!(pending[0].permission, "external_directory");
    assert!(
        pending[0].patterns[0].ends_with("/*"),
        "{:?}",
        pending[0].patterns
    );
    runtime
        .reply_permission(
            &pending[0].permission_id,
            haarness_protocol::PermissionReply {
                decision: haarness_protocol::PermissionReplyDecision::Once,
                feedback: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let _ = tokio::fs::remove_file(&target).await;
    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn external_list_asks_then_executes_after_allow_once() {
    let workspace =
        std::env::temp_dir().join(format!("haarness-list-workspace-{}", uuid::Uuid::now_v7()));
    let external =
        std::env::temp_dir().join(format!("haarness-list-external-{}", uuid::Uuid::now_v7()));
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    tokio::fs::create_dir_all(&external).await.unwrap();
    tokio::fs::write(external.join("visible.txt"), "approved\n")
        .await
        .unwrap();
    let (runtime, _) = p0_runtime(&workspace, true);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&format!(
                "!tool list {}",
                serde_json::json!({"path": external})
            )),
        )
        .await
        .unwrap();

    wait_pending_permissions(&runtime, 1).await;
    let pending = runtime.pending_permissions();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].permission, "external_directory");
    assert!(pending[0].patterns[0].ends_with("/*"));
    runtime
        .reply_permission(
            &pending[0].permission_id,
            haarness_protocol::PermissionReply {
                decision: haarness_protocol::PermissionReplyDecision::Once,
                feedback: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(items.iter().any(|item| matches!(
        &item.data,
        haarness_protocol::ItemData::ToolResult { name, output, is_error, .. }
            if name == "list" && !is_error && output.contains("visible.txt")
    )));

    tokio::fs::remove_dir_all(external).await.unwrap();
    tokio::fs::remove_dir_all(workspace).await.unwrap();
}

#[tokio::test]
async fn env_reads_ask_while_env_example_stays_silent() {
    let workspace = std::env::temp_dir().join(format!("haarness-env-{}", uuid::Uuid::now_v7()));
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    tokio::fs::write(workspace.join(".env"), "SECRET=1\n")
        .await
        .unwrap();
    tokio::fs::write(workspace.join(".env.example"), "SECRET=\n")
        .await
        .unwrap();
    let (runtime, _) = p0_runtime(&workspace, true);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    // .env.example reads silently.
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!tool read {"path":".env.example"}"#),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(runtime.pending_permissions().is_empty());
    // .env asks.
    let _turn = runtime
        .start_turn(thread.id, turn_request(r#"!tool read {"path":".env"}"#))
        .await
        .unwrap();
    wait_pending_permissions(&runtime, 1).await;
    let pending = runtime.pending_permissions();
    assert_eq!(pending[0].permission, "read");
    runtime
        .reply_permission(
            &pending[0].permission_id,
            haarness_protocol::PermissionReply {
                decision: haarness_protocol::PermissionReplyDecision::Once,
                feedback: None,
            },
        )
        .await
        .unwrap();
    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn child_agents_cannot_escalate_past_a_parent_deny() {
    let workspace = std::env::temp_dir();
    let provider = Capturing::default();
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
    ]));
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        providers,
        Arc::new(haarness_server::NativeToolRegistry::new(workspace.clone())),
        EventBus::new(2048),
        workspace.clone(),
        String::new(),
        {
            let mut config = haarness_core::RuntimeConfig::default();
            config.base_rules.push(haarness_core::PermissionRule {
                pattern: "bash:git push*".into(),
                decision: haarness_core::PermissionDecision::Deny,
            });
            config
        },
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request(&tasks_prompt(1, "git push origin")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let children: Vec<Thread> = runtime
        .list_threads(false)
        .await
        .unwrap()
        .into_iter()
        .filter(|candidate| candidate.parent_id == Some(thread.id))
        .collect();
    assert_eq!(children.len(), 1);
    let child_items = runtime.items(children[0].id, None).await.unwrap();
    assert!(
        child_items.iter().any(|item| matches!(&item.data,
            haarness_protocol::ItemData::ToolResult { name, is_error, output, .. }
                if name == "bash" && *is_error && output.contains("denied by permission rule"))),
        "the child inherited the parent's deny"
    );
}

#[tokio::test]
async fn yolo_auto_approves_everything_and_records_rules() {
    let workspace = std::env::temp_dir();
    let provider = Capturing::default();
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
    ]));
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        providers,
        Arc::new(haarness_server::NativeToolRegistry::new(workspace.clone())),
        EventBus::new(2048),
        workspace.clone(),
        String::new(),
        {
            let mut config = haarness_core::RuntimeConfig::default();
            config.base_rules.push(haarness_core::PermissionRule {
                pattern: "bash:*".into(),
                decision: haarness_core::PermissionDecision::Ask,
            });
            config.auto_approve = true;
            config
        },
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request(r#"!tool bash {"command":"true"}"#))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(runtime.pending_permissions().is_empty());
    let requested = runtime
        .events()
        .subscribe(None)
        .replay
        .iter()
        .filter(|event| event.event_type == "permission.requested")
        .count();
    assert_eq!(requested, 0, "--yolo never prompts");
}

#[tokio::test]
async fn one_turns_auto_approval_never_approves_other_chats() {
    let workspace = std::env::temp_dir();
    let provider = Capturing::default();
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
    ]));
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        providers,
        Arc::new(haarness_server::NativeToolRegistry::new(workspace.clone())),
        EventBus::new(2048),
        workspace.clone(),
        String::new(),
        {
            let mut config = haarness_core::RuntimeConfig::default();
            config.base_rules.push(haarness_core::PermissionRule {
                pattern: "bash:*".into(),
                decision: haarness_core::PermissionDecision::Ask,
            });
            config
        },
    );
    let chat = || {
        runtime.create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
    };
    // A scheduled run (or `run --yolo`) approves its own turn...
    let scheduled = chat().await.unwrap();
    let mut request = turn_request(r#"!tool bash {"command":"true"}"#);
    request.auto_approve = true;
    let turn = runtime.start_turn(scheduled.id, request).await.unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(runtime.pending_permissions().is_empty());
    // ...but the same command in an ordinary chat still asks.
    let interactive = chat().await.unwrap();
    runtime
        .start_turn(
            interactive.id,
            turn_request(r#"!tool bash {"command":"true"}"#),
        )
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while runtime.pending_permissions().is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the interactive chat ran without asking"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(runtime.pending_permissions()[0].thread_id, interactive.id);
}

// ---------------------------------------------------------------------------
// P3: custom slash commands and custom agents.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn custom_command_expands_server_side_and_switches_agent() {
    let workspace = std::env::temp_dir();
    let provider = Capturing::default();
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
    ]));
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        providers,
        Arc::new(EmptyToolRegistry),
        EventBus::new(256),
        workspace.clone(),
        String::new(),
        {
            let mut config = haarness_core::RuntimeConfig::default();
            config.commands.push(haarness_protocol::CommandInfo {
                execution: Default::default(),
                name: "review".into(),
                description: "Review something".into(),
                source: None,
                agent: Some("review".into()),
                model: None,
                argument_hint: Some("[focus]".into()),
                template: "Review the following with care: $ARGUMENTS".into(),
                plugin: None,
            });
            config.custom_agents.push(haarness_core::CustomAgent {
                id: "pirate".into(),
                description: "Talks like a pirate".into(),
                prompt: "Respond like a pirate captain reviewing code.".into(),
                mode: None,
                model: None,
                tools: std::collections::HashMap::new(),
                permissions: Vec::new(),
                ..Default::default()
            });
            config
        },
    );
    // Custom agents surface in the profile list.
    assert!(
        runtime
            .agent_profiles()
            .iter()
            .any(|profile| profile.id == "pirate")
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut request = turn_request("the error handling");
    request.command = Some("review".into());
    let turn = runtime.start_turn(thread.id, request).await.unwrap();
    assert_eq!(turn.agent, "review", "frontmatter agent applies");
    wait_done(&runtime, &turn).await;
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(
        items.iter().any(|item| matches!(&item.data,
            haarness_protocol::ItemData::UserMessage { text, .. }
                if text == "Review the following with care: the error handling")),
        "template expanded server-side"
    );

    // A custom agent's prompt reaches the provider as the system prompt.
    let mut request = turn_request("ahoy");
    request.agent = Some("pirate".into());
    let turn = runtime.start_turn(thread.id, request).await.unwrap();
    wait_done(&runtime, &turn).await;
    let systems: Vec<String> = provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.system.clone())
        .collect();
    assert!(
        systems
            .iter()
            .any(|system| system.contains("pirate captain reviewing code")),
        "custom agent prompt used; saw {systems:?}"
    );
    let system = systems.last().expect("captured custom-agent system prompt");
    assert!(system.contains("The exact model ID is echo/echo-1"));
    assert!(system.contains(&format!("Working directory: {}", workspace.display())));
    assert!(system.contains(&format!("Platform: {}", std::env::consts::OS)));
    assert!(system.contains(&format!("Architecture: {}", std::env::consts::ARCH)));
    assert!(system.contains(&format!(
        "Today's date: {}",
        thread.created_at.format("%Y-%m-%d")
    )));
    assert!(system.contains("Use native tool calls only"));
    assert_eq!(
        system.matches("pirate captain reviewing code").count(),
        1,
        "custom role guidance is layered exactly once"
    );
    assert!(
        !system.contains("Work autonomously, preserve unrelated user changes"),
        "a custom role must not inherit conflicting default build guidance"
    );
    // Unknown command errors cleanly.
    let mut request = turn_request("x");
    request.command = Some("nope".into());
    let error = runtime.start_turn(thread.id, request).await.unwrap_err();
    assert!(matches!(error, haarness_core::CoreError::NotFound(_)));
}

// ---------------------------------------------------------------------------
// P7 (early): standing goals and loop configuration.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn goals_persist_and_precede_the_real_user_prompt() {
    let workspace = std::env::temp_dir();
    let provider = Capturing::default();
    let events = EventBus::new(256);
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
    ]));
    let runtime = AgentRuntime::with_config(
        store.clone(),
        providers,
        Arc::new(EmptyToolRegistry),
        events.clone(),
        workspace.clone(),
        String::new(),
        haarness_core::RuntimeConfig::default(),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Goal parity".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .set_goal(thread.id, Some("ship the release notes".into()))
        .await
        .unwrap();
    // This test exercises the ordinary continuation limit, independently of
    // the new-goal criteria bootstrap. A persisted, already-negotiated goal
    // has no bootstrap retry circuit breaker.
    let mut negotiated = runtime.goal_state(thread.id).await.unwrap().unwrap();
    negotiated.needs_criteria = false;
    store
        .save_goal_state(thread.id, Some(negotiated))
        .await
        .unwrap();
    let (goal, _) = runtime.thread_extras(thread.id).await.unwrap();
    assert_eq!(goal.as_deref(), Some("ship the release notes"));

    let turn = runtime
        .start_turn(thread.id, turn_request("progress?"))
        .await
        .unwrap();
    wait_done(&runtime, &turn).await;
    {
        let requests = provider.requests.lock().unwrap();
        let first = requests.first().expect("provider received the goal turn");
        assert!(
            !first.system.contains("ship the release notes"),
            "user goal data is never elevated into the system prompt"
        );
        let users = first
            .messages
            .iter()
            .filter_map(|message| match message {
                ProviderMessage::Text { role, content } if role == "user" => Some(content),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            users.len(),
            1,
            "initial doctrine preserves role alternation"
        );
        assert!(users[0].starts_with("<goal-harness>"));
        assert!(users[0].contains("ship the release notes"));
        assert!(users[0].ends_with("progress?"), "real user text stays last");
        assert_eq!(
            requests.len(),
            21,
            "initial response plus twenty autonomous continuations"
        );
    }
    let lifecycle = events
        .subscribe(None)
        .replay
        .into_iter()
        .filter(|event| event.event_type.starts_with("goal.logical_turn."))
        .collect::<Vec<_>>();
    let started = lifecycle
        .iter()
        .filter(|event| event.event_type == "goal.logical_turn.started")
        .collect::<Vec<_>>();
    let finished = lifecycle
        .iter()
        .filter(|event| event.event_type == "goal.logical_turn.finished")
        .collect::<Vec<_>>();
    assert_eq!(started.len(), 21);
    assert_eq!(finished.len(), 21);
    assert_eq!(started[0].payload["logicalTurn"], 0);
    assert_eq!(started[20].payload["logicalTurn"], 20);
    assert_eq!(started[0].payload["continuation"], false);
    assert_eq!(started[20].payload["continuation"], true);
    assert_eq!(finished[20].payload["phase"], "finished");
    assert!(finished[20].payload["finishedAt"].as_i64().is_some());
    let state = runtime.goal_state(thread.id).await.unwrap().unwrap();
    assert_eq!(state.status, haarness_protocol::GoalStatus::Paused);
    assert_eq!(state.turns_used, 20);
    let ordinary = runtime
        .start_turn(thread.id, turn_request("ordinary paused turn"))
        .await
        .unwrap();
    wait_done(&runtime, &ordinary).await;
    {
        let requests = provider.requests.lock().unwrap();
        let last = requests.last().unwrap();
        let latest_user = last
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                ProviderMessage::Text { role, content } if role == "user" => Some(content),
                _ => None,
            })
            .expect("ordinary turn has a user message");
        assert_eq!(latest_user, "ordinary paused turn");
    }

    runtime.set_goal(thread.id, None).await.unwrap();
    let (goal, _) = runtime.thread_extras(thread.id).await.unwrap();
    assert!(goal.is_none());
}

#[tokio::test]
async fn provider_only_sees_goal_tools_while_a_goal_is_active() {
    let workspace = std::env::temp_dir();
    let provider = Capturing::default();
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let runtime = AgentRuntime::with_config(
        store.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(haarness_server::NativeToolRegistry::new(workspace.clone())),
        EventBus::new(64),
        workspace,
        String::new(),
        haarness_core::RuntimeConfig {
            goal_max_turns: Some(0),
            ..Default::default()
        },
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Goal tool visibility".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    let ordinary = runtime
        .start_turn(thread.id, turn_request("ordinary request"))
        .await
        .unwrap();
    wait_done(&runtime, &ordinary).await;
    let ordinary_request_count = provider.requests.lock().unwrap().len();
    {
        let requests = provider.requests.lock().unwrap();
        let tools = requests[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        assert!(
            !tools
                .iter()
                .any(|name| matches!(*name, "get_goal" | "update_goal" | "propose_goal_criteria"))
        );
    }

    runtime
        .set_goal(thread.id, Some("finish the active objective".into()))
        .await
        .unwrap();
    let mut state = runtime.goal_state(thread.id).await.unwrap().unwrap();
    state.needs_criteria = false;
    store.save_goal_state(thread.id, Some(state)).await.unwrap();
    let goal_turn = runtime
        .start_turn(thread.id, turn_request("continue the goal"))
        .await
        .unwrap();
    wait_done(&runtime, &goal_turn).await;

    let requests = provider.requests.lock().unwrap();
    let tools = requests[ordinary_request_count]
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<std::collections::HashSet<_>>();
    assert!(tools.contains("get_goal"));
    assert!(tools.contains("update_goal"));
    assert!(tools.contains("propose_goal_criteria"));
}

#[tokio::test]
async fn ordinary_goal_tool_followup_keeps_current_doctrine() {
    let provider = Capturing::default();
    let events = EventBus::new(128);
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let runtime = AgentRuntime::with_config(
        store.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(std::env::temp_dir())),
        events.clone(),
        std::env::temp_dir(),
        String::new(),
        haarness_core::RuntimeConfig {
            goal_max_turns: Some(1),
            ..Default::default()
        },
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Goal doctrine tool round".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .set_goal(thread.id, Some("keep the release objective current".into()))
        .await
        .unwrap();
    let mut negotiated = runtime.goal_state(thread.id).await.unwrap().unwrap();
    negotiated.needs_criteria = false;
    store
        .save_goal_state(thread.id, Some(negotiated))
        .await
        .unwrap();

    let turn = runtime
        .start_turn(thread.id, turn_request("!tool get_goal {}"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );

    let requests = provider.requests.lock().unwrap();
    assert!(requests.len() >= 2);
    assert!(requests[0].messages.iter().any(|message| {
        matches!(message, ProviderMessage::Text { role, content }
            if role == "user"
                && content.contains("<goal-harness>")
                && content.contains("keep the release objective current"))
    }));
    assert_goal_doctrine_precedes_tool_pair(
        &requests[1],
        "keep the release objective current",
        "get_goal",
    );
    drop(requests);

    let started = events
        .subscribe(None)
        .replay
        .into_iter()
        .filter(|event| event.event_type == "goal.logical_turn.started")
        .count();
    assert_eq!(
        started, 2,
        "a tool-result provider round is not a new logical goal turn"
    );
}

#[tokio::test]
async fn queued_goal_steering_tool_followup_keeps_current_doctrine() {
    let provider = QueuedGoalToolRound::default();
    let events = EventBus::new(128);
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let runtime = AgentRuntime::with_config(
        store.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(std::env::temp_dir())),
        events.clone(),
        std::env::temp_dir(),
        String::new(),
        haarness_core::RuntimeConfig {
            goal_max_turns: Some(1),
            ..Default::default()
        },
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Queued goal doctrine tool round".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .set_goal(thread.id, Some("keep queued work aligned".into()))
        .await
        .unwrap();
    let mut negotiated = runtime.goal_state(thread.id).await.unwrap().unwrap();
    negotiated.needs_criteria = false;
    store
        .save_goal_state(thread.id, Some(negotiated))
        .await
        .unwrap();

    let turn = runtime
        .start_turn(thread.id, turn_request("begin"))
        .await
        .unwrap();
    let started = tokio::time::timeout(Duration::from_secs(1), provider.first_started.acquire())
        .await
        .unwrap()
        .unwrap();
    started.forget();
    let queued = runtime
        .queue_input(
            thread.id,
            haarness_protocol::QueueInputRequest {
                preview: None,
                reasoning_effort: None,
                delivery: haarness_protocol::InputDelivery::Steer,
                request_id: RequestId::new(),
                text: "steer through the status tool".into(),
                attachments: Vec::new(),
                synthetic: false,
            },
        )
        .await
        .unwrap();
    assert!(queued.queued);
    provider.release_first.add_permits(1);
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );

    let requests = provider.requests.lock().unwrap();
    assert!(requests.len() >= 3);
    let steered_user = requests[1]
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            ProviderMessage::Text { role, content } if role == "user" => Some(content),
            _ => None,
        })
        .expect("queued steering reaches the provider");
    assert!(steered_user.contains("<goal-harness>"));
    assert!(steered_user.contains("keep queued work aligned"));
    assert!(steered_user.contains("steer through the status tool"));
    assert_goal_doctrine_precedes_tool_pair(&requests[2], "keep queued work aligned", "get_goal");
    drop(requests);

    let started = events
        .subscribe(None)
        .replay
        .into_iter()
        .filter(|event| event.event_type == "goal.logical_turn.started")
        .count();
    assert_eq!(
        started, 3,
        "initial, queued, and continuation turns start once each"
    );
}

#[tokio::test]
async fn autonomous_goal_continuation_tool_followup_reinserts_doctrine_at_boundary() {
    let provider = ContinuationGoalToolRound::default();
    let events = EventBus::new(128);
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let runtime = AgentRuntime::with_config(
        store.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(std::env::temp_dir())),
        events.clone(),
        std::env::temp_dir(),
        String::new(),
        haarness_core::RuntimeConfig {
            goal_max_turns: Some(1),
            ..Default::default()
        },
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Continuation goal doctrine tool round".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .set_goal(thread.id, Some("keep continuation work aligned".into()))
        .await
        .unwrap();
    let mut negotiated = runtime.goal_state(thread.id).await.unwrap().unwrap();
    negotiated.needs_criteria = false;
    store
        .save_goal_state(thread.id, Some(negotiated))
        .await
        .unwrap();

    let turn = runtime
        .start_turn(thread.id, turn_request("begin"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );

    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    let continuation_doctrine = requests[1]
        .messages
        .iter()
        .filter(|message| {
            matches!(message, ProviderMessage::Text { role, content }
                if role == "user"
                    && content.contains("<goal-harness>")
                    && content.contains("keep continuation work aligned"))
        })
        .count();
    assert_eq!(continuation_doctrine, 1);
    assert_goal_doctrine_precedes_tool_pair(
        &requests[2],
        "keep continuation work aligned",
        "get_goal",
    );
    drop(requests);

    let started = events
        .subscribe(None)
        .replay
        .into_iter()
        .filter(|event| event.event_type == "goal.logical_turn.started")
        .count();
    assert_eq!(started, 2, "the follow-up stays in its continuation");
}

#[tokio::test]
async fn answered_goal_question_does_not_start_an_unbounded_criteria_loop() {
    let provider = GoalQuestionThenWaits::default();
    let events = EventBus::new(64);
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(std::env::temp_dir())),
        events.clone(),
        std::env::temp_dir(),
        String::new(),
        haarness_core::RuntimeConfig::default(),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Criteria bootstrap guard".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .set_goal(thread.id, Some("ambiguous objective".into()))
        .await
        .unwrap();

    let turn = runtime
        .start_turn(thread.id, turn_request("start"))
        .await
        .unwrap();
    let question = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(question) = runtime.pending_questions().into_iter().next() {
                break question;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("goal asks the scripted question");
    runtime
        .reply_question(
            &question.question_id,
            haarness_protocol::QuestionReply {
                answer: "Share an error message".into(),
            },
        )
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );

    assert_eq!(
        provider.requests.lock().unwrap().len(),
        4,
        "the answered question receives one response without another autonomous turn"
    );
    let state = runtime.goal_state(thread.id).await.unwrap().unwrap();
    assert_eq!(state.status, haarness_protocol::GoalStatus::Paused);
    assert_eq!(state.turns_used, 1);
    assert!(state.needs_criteria);
    assert!(state.awaiting_user_input);
    assert!(
        state
            .paused_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("agree on its completion criteria"))
    );
    let update = events
        .subscribe(None)
        .replay
        .into_iter()
        .rev()
        .find(|event| event.event_type == "goal.updated")
        .expect("pause publishes canonical goal state");
    assert_eq!(update.payload["state"]["status"], "paused");
    assert!(
        update.payload["message"]
            .as_str()
            .is_some_and(|message| message.contains("waiting for you"))
    );

    let replay_before_followup = events.subscribe(None).replay.len();
    let followup = runtime
        .start_turn(thread.id, turn_request("the error is E42"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &followup).await.status,
        TurnStatus::Completed
    );
    let requests = provider.requests.lock().unwrap();
    let followup_request = requests.get(4).expect("follow-up reaches the provider");
    let latest_user = followup_request
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            ProviderMessage::Text { role, content } if role == "user" => Some(content),
            _ => None,
        })
        .expect("follow-up has user context");
    assert!(latest_user.starts_with("<goal-harness>"));
    assert!(latest_user.ends_with("the error is E42"));
    drop(requests);
    let followup_events = events
        .subscribe(None)
        .replay
        .into_iter()
        .skip(replay_before_followup)
        .collect::<Vec<_>>();
    let (resumed, resumed_event) = followup_events
        .iter()
        .enumerate()
        .find(|(_, event)| {
            event.event_type == "goal.updated" && event.payload["state"]["status"] == "active"
        })
        .expect("ordinary user input resumes the runtime wait");
    assert_eq!(resumed_event.payload["state"]["awaitingUserInput"], false);
    assert!(resumed_event.payload["state"]["pausedReason"].is_null());
    let turn_started = followup_events
        .iter()
        .position(|event| event.event_type == "turn.started")
        .expect("follow-up turn starts");
    assert!(
        resumed < turn_started,
        "resume is published before the turn"
    );
}

#[tokio::test]
async fn goal_pauses_after_repeated_get_goal_only_turns() {
    let provider = GoalIntrospectionOnly::default();
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let runtime = AgentRuntime::with_config(
        store.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(std::env::temp_dir())),
        EventBus::new(64),
        std::env::temp_dir(),
        String::new(),
        haarness_core::RuntimeConfig::default(),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Goal status loop guard".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .set_goal(thread.id, Some("already negotiated objective".into()))
        .await
        .unwrap();
    let mut negotiated = runtime.goal_state(thread.id).await.unwrap().unwrap();
    negotiated.needs_criteria = false;
    store
        .save_goal_state(thread.id, Some(negotiated))
        .await
        .unwrap();

    let turn = runtime
        .start_turn(thread.id, turn_request("continue"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );

    assert_eq!(
        provider.requests.lock().unwrap().len(),
        4,
        "two get_goal + text logical turns are enough to detect the loop"
    );
    let state = runtime.goal_state(thread.id).await.unwrap().unwrap();
    assert_eq!(state.status, haarness_protocol::GoalStatus::Paused);
    assert_eq!(state.turns_used, 1);
    assert!(
        state
            .paused_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("same no-progress goal turn repeated"))
    );
}

#[tokio::test]
async fn productive_get_goal_turns_with_different_text_do_not_false_pause() {
    let provider = GoalIntrospectionWithProgress::default();
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let runtime = AgentRuntime::with_config(
        store.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(std::env::temp_dir())),
        EventBus::new(64),
        std::env::temp_dir(),
        String::new(),
        haarness_core::RuntimeConfig {
            goal_max_turns: Some(2),
            ..Default::default()
        },
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Goal status progress".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .set_goal(thread.id, Some("already negotiated objective".into()))
        .await
        .unwrap();
    let mut negotiated = runtime.goal_state(thread.id).await.unwrap().unwrap();
    negotiated.needs_criteria = false;
    store
        .save_goal_state(thread.id, Some(negotiated))
        .await
        .unwrap();

    let turn = runtime
        .start_turn(thread.id, turn_request("continue"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert_eq!(provider.requests.lock().unwrap().len(), 6);
    let state = runtime.goal_state(thread.id).await.unwrap().unwrap();
    assert_eq!(state.status, haarness_protocol::GoalStatus::Paused);
    assert_eq!(state.turns_used, 2);
    assert!(
        state
            .paused_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("automatic continuation limit"))
    );
}

#[tokio::test]
async fn repeated_failed_tool_calls_do_not_evade_goal_loop_breaker() {
    let provider = GoalRepeatedToolFailure::default();
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let runtime = AgentRuntime::with_config(
        store.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
        std::env::temp_dir(),
        String::new(),
        haarness_core::RuntimeConfig::default(),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Goal failed-tool loop".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .set_goal(thread.id, Some("already negotiated objective".into()))
        .await
        .unwrap();
    let mut negotiated = runtime.goal_state(thread.id).await.unwrap().unwrap();
    negotiated.needs_criteria = false;
    store
        .save_goal_state(thread.id, Some(negotiated))
        .await
        .unwrap();

    let turn = runtime
        .start_turn(thread.id, turn_request("continue"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert_eq!(provider.requests.lock().unwrap().len(), 4);
    let state = runtime.goal_state(thread.id).await.unwrap().unwrap();
    assert_eq!(state.status, haarness_protocol::GoalStatus::Paused);
    assert_eq!(state.turns_used, 1);
    assert!(
        state
            .paused_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("same no-progress goal turn repeated"))
    );
}

#[tokio::test]
async fn ordinary_input_does_not_resume_manual_budget_or_synthetic_goal_pauses() {
    let provider = Capturing::default();
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let runtime = AgentRuntime::with_config(
        store.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
        std::env::temp_dir(),
        String::new(),
        haarness_core::RuntimeConfig::default(),
    );

    let manual = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Manual pause".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .set_goal(manual.id, Some("manual objective".into()))
        .await
        .unwrap();
    runtime
        .goal_action(manual.id, haarness_protocol::GoalActionRequest::Pause)
        .await
        .unwrap();
    let turn = runtime
        .start_turn(manual.id, turn_request("ordinary manual input"))
        .await
        .unwrap();
    wait_done(&runtime, &turn).await;
    let manual_state = runtime.goal_state(manual.id).await.unwrap().unwrap();
    assert_eq!(manual_state.status, haarness_protocol::GoalStatus::Paused);
    assert!(!manual_state.awaiting_user_input);

    let budget = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Budget pause".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .set_goal(budget.id, Some("budget objective".into()))
        .await
        .unwrap();
    let mut budget_state = runtime.goal_state(budget.id).await.unwrap().unwrap();
    budget_state.status = haarness_protocol::GoalStatus::BudgetLimited;
    budget_state.paused_reason = Some("token budget exhausted".into());
    budget_state.awaiting_user_input = false;
    store
        .save_goal_state(budget.id, Some(budget_state))
        .await
        .unwrap();
    let turn = runtime
        .start_turn(budget.id, turn_request("ordinary budget input"))
        .await
        .unwrap();
    wait_done(&runtime, &turn).await;
    assert_eq!(
        runtime.goal_state(budget.id).await.unwrap().unwrap().status,
        haarness_protocol::GoalStatus::BudgetLimited
    );

    let synthetic = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Synthetic input".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .set_goal(synthetic.id, Some("waiting objective".into()))
        .await
        .unwrap();
    let mut waiting = runtime.goal_state(synthetic.id).await.unwrap().unwrap();
    waiting.status = haarness_protocol::GoalStatus::Paused;
    waiting.paused_reason = Some("waiting for user direction".into());
    waiting.awaiting_user_input = true;
    store
        .save_goal_state(synthetic.id, Some(waiting))
        .await
        .unwrap();
    let mut request = turn_request("synthetic workflow input");
    request.synthetic = true;
    let turn = runtime.start_turn(synthetic.id, request).await.unwrap();
    wait_done(&runtime, &turn).await;
    let synthetic_state = runtime.goal_state(synthetic.id).await.unwrap().unwrap();
    assert_eq!(
        synthetic_state.status,
        haarness_protocol::GoalStatus::Paused
    );
    assert!(synthetic_state.awaiting_user_input);

    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    for (request, expected) in requests.iter().zip([
        "ordinary manual input",
        "ordinary budget input",
        "synthetic workflow input",
    ]) {
        let latest_user = request
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                ProviderMessage::Text { role, content } if role == "user" => Some(content),
                _ => None,
            })
            .unwrap();
        assert_eq!(latest_user, expected, "paused goals do not inject doctrine");
    }
}

#[tokio::test]
async fn server_goal_command_rejects_an_active_turn_before_mutating_state() {
    let provider = SlowText::default();
    let runtime = Arc::new(AgentRuntime::new(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    ));
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Active goal preflight".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request("keep working"))
        .await
        .unwrap();
    let started = tokio::time::timeout(Duration::from_secs(1), provider.first_started.acquire())
        .await
        .unwrap()
        .unwrap();
    started.forget();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = serve(
        listener,
        "goal-preflight-token",
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
            workspace: std::env::temp_dir(),
            project_trusted: true,
            language: None,
        },
    )
    .await
    .unwrap();
    let response = reqwest::Client::new()
        .post(format!(
            "http://{}/v1/threads/{}/commands",
            handle.address, thread.id
        ))
        .bearer_auth("goal-preflight-token")
        .json(&serde_json::json!({
            "name": "goal",
            "arguments": "replacement objective"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    assert!(runtime.goal_state(thread.id).await.unwrap().is_none());

    provider.release_first.add_permits(1);
    wait_done(&runtime, &turn).await;
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn replacing_a_goal_mid_turn_resets_the_goal_boundary() {
    let provider = SlowText::default();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(128),
        std::env::temp_dir(),
        String::new(),
        haarness_core::RuntimeConfig {
            goal_max_turns: Some(1),
            ..Default::default()
        },
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Goal replacement".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .set_goal(thread.id, Some("old objective".into()))
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request("work on old objective"))
        .await
        .unwrap();
    let started = tokio::time::timeout(Duration::from_secs(1), provider.first_started.acquire())
        .await
        .unwrap()
        .unwrap();
    started.forget();
    runtime
        .set_goal(thread.id, Some("new objective".into()))
        .await
        .unwrap();
    runtime
        .queue_input(
            thread.id,
            haarness_protocol::QueueInputRequest {
                preview: None,
                reasoning_effort: None,
                delivery: haarness_protocol::InputDelivery::Steer,
                request_id: RequestId::new(),
                text: "begin the new objective".into(),
                attachments: Vec::new(),
                synthetic: false,
            },
        )
        .await
        .unwrap();
    provider.release_first.add_permits(1);
    wait_done(&runtime, &turn).await;

    let state = runtime.goal_state(thread.id).await.unwrap().unwrap();
    assert_eq!(state.goal, "new objective");
    assert_eq!(
        state.turns_used, 1,
        "old response was not charged as a new continuation"
    );
    let requests = provider.requests.lock().unwrap();
    let first_new_user = requests[1]
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            ProviderMessage::Text { role, content } if role == "user" => Some(content),
            _ => None,
        })
        .unwrap();
    let objective_position = first_new_user.find("new objective").unwrap();
    let user_position = first_new_user.rfind("begin the new objective").unwrap();
    assert!(
        objective_position < user_position,
        "goal doctrine must precede the replacement user input: {first_new_user}"
    );
}

#[tokio::test]
async fn goal_tools_negotiate_criteria_complete_and_report_canonical_state() {
    let workspace = std::env::temp_dir();
    let provider = Capturing::default();
    let events = EventBus::new(256);
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider) as Arc<dyn ModelProvider>
        ])),
        Arc::new(haarness_server::NativeToolRegistry::new(workspace.clone())),
        events.clone(),
        workspace,
        String::new(),
        haarness_core::RuntimeConfig {
            goal_max_turns: Some(1),
            ..Default::default()
        },
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Goal tools".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .set_goal(thread.id, Some("prove the release".into()))
        .await
        .unwrap();
    let propose = serde_json::json!({
        "criteria": ["tests pass", "binary starts"],
        "prompt": "Approve the release criteria?",
        "label": "Release criteria"
    });
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&format!("!tool propose_goal_criteria {propose}")),
        )
        .await
        .unwrap();
    let question = loop {
        if let Some(question) = runtime.pending_questions().first() {
            break question.clone();
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert!(question.multiple);
    assert!(question.custom);
    assert_eq!(question.choices, ["tests pass", "binary starts"]);
    runtime
        .reply_question(
            &question.question_id,
            haarness_protocol::QuestionReply {
                // Empty/headless submissions approve the
                // proposed set instead of deadlocking goal bootstrap.
                answer: String::new(),
            },
        )
        .unwrap();
    wait_done(&runtime, &turn).await;
    let state = runtime.goal_state(thread.id).await.unwrap().unwrap();
    assert_eq!(state.criteria.len(), 2);
    assert!(!state.needs_criteria);

    runtime
        .goal_action(thread.id, haarness_protocol::GoalActionRequest::Resume)
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request("!tool get_goal {}"))
        .await
        .unwrap();
    wait_done(&runtime, &turn).await;
    let items = runtime.items(thread.id, None).await.unwrap();
    let output = items
        .iter()
        .rev()
        .find_map(|item| match &item.data {
            haarness_protocol::ItemData::ToolResult {
                name,
                output,
                is_error: false,
                ..
            } if name == "get_goal" => Some(output),
            _ => None,
        })
        .expect("get_goal tool result");
    assert!(output.contains("\"objective\": \"prove the release\""));
    assert!(!output.contains("\"goal\": \"prove the release\""));
    runtime
        .goal_action(thread.id, haarness_protocol::GoalActionRequest::Resume)
        .await
        .unwrap();

    let completion = serde_json::json!({
        "status": "complete",
        "reason": "release verified",
        "next_step_question": {
            "title": "What next?",
            "edit": {"label":"Edit","description":"Refine this objective."},
            "new": {"label":"New","description":"Start another objective."},
            "clear": {"label":"Clear","description":"Remove the goal."},
            "dismiss": {"label":"Done","description":"Close this dialog."}
        },
        "criteria_evidence": [
            {"index":1,"evidence":"cargo test passed"},
            {"index":2,"evidence":"smoke test passed"}
        ]
    });
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&format!("!tool update_goal {completion}")),
        )
        .await
        .unwrap();
    wait_done(&runtime, &turn).await;
    assert_eq!(
        runtime.goal_state(thread.id).await.unwrap().unwrap().status,
        haarness_protocol::GoalStatus::Complete
    );
    let completion_event = events
        .subscribe(None)
        .replay
        .into_iter()
        .rev()
        .find(|event| event.event_type == "goal.updated" && event.payload["completion"].is_object())
        .expect("completion metadata is emitted");
    assert_eq!(
        completion_event.payload["completion"]["title"],
        "What next?"
    );
}

#[tokio::test]
async fn loop_config_persists_and_expires() {
    let workspace = std::env::temp_dir();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Echo) as Arc<dyn ModelProvider>
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
        workspace.clone(),
        String::new(),
        haarness_core::RuntimeConfig::default(),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let config = haarness_protocol::LoopConfig {
        prompt: "check the queue".into(),
        interval_seconds: 300,
        expires_at: chrono::Utc::now() + chrono::Duration::days(7),
    };
    runtime.set_loop(thread.id, Some(config)).await.unwrap();
    let (_, stored) = runtime.thread_extras(thread.id).await.unwrap();
    let stored = stored.expect("loop stored");
    assert_eq!(stored.prompt, "check the queue");
    assert_eq!(stored.interval_seconds, 300);
    runtime.set_loop(thread.id, None).await.unwrap();
    let (_, stored) = runtime.thread_extras(thread.id).await.unwrap();
    assert!(stored.is_none());
}

// ---------------------------------------------------------------------------
// P5: lifecycle hooks gate the runtime.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pre_tool_use_hook_blocks_and_post_tool_use_annotates() {
    let workspace = std::env::temp_dir();
    let provider = Capturing::default();
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
    ]));
    let hooks = haarness_server::ConfiguredHooks::new(
        haarness_server::ConfiguredHooks::parse_block(&serde_json::json!({
            "PreToolUse": [{"matcher": "Bash", "hooks": [
                // Block any rm; allow everything else via plain exit 0.
                {"command": "grep -q '\"command\":\"rm' && { echo 'no deletions' >&2; exit 2; } || true"},
            ]}],
            "PostToolUse": [{"matcher": "Bash", "hooks": [
                {"command": r#"echo '{"additionalContext":"reviewed by hook"}'"#},
            ]}],
        })),
        workspace.clone(),
    );
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        providers,
        Arc::new(haarness_server::NativeToolRegistry::new(workspace.clone())),
        EventBus::new(256),
        workspace.clone(),
        String::new(),
        haarness_core::RuntimeConfig::default(),
    )
    .with_hooks(Arc::new(hooks));
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    // Blocked command.
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!tool bash {"command":"rm -rf whatever"}"#),
        )
        .await
        .unwrap();
    wait_done(&runtime, &turn).await;
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(
        items.iter().any(|item| matches!(&item.data,
            haarness_protocol::ItemData::ToolResult { output, is_error, .. }
                if *is_error && output.contains("no deletions"))),
        "PreToolUse blocked the rm with the hook's reason"
    );
    // Allowed command gets the PostToolUse annotation.
    let turn = runtime
        .start_turn(thread.id, turn_request(r#"!tool bash {"command":"true"}"#))
        .await
        .unwrap();
    wait_done(&runtime, &turn).await;
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(
        items.iter().any(|item| matches!(&item.data,
            haarness_protocol::ItemData::ToolResult { output, is_error, .. }
                if !is_error && output.contains("reviewed by hook"))),
        "PostToolUse context reached the tool result"
    );
}

#[tokio::test]
async fn workspace_rest_lifecycle_uses_real_git_and_enforces_activity_leases() {
    let root =
        std::env::temp_dir().join(format!("haarness-workspace-api-{}", uuid::Uuid::now_v7()));
    let repository = root.join("repository");
    std::fs::create_dir_all(&repository).unwrap();
    git_fixture_command(&repository, ["init", "-b", "main"]);
    git_fixture_command(&repository, ["config", "user.email", "api@example.invalid"]);
    git_fixture_command(&repository, ["config", "user.name", "Haarness API Test"]);
    git_fixture_command(&repository, ["commit", "--allow-empty", "-m", "base"]);
    let base_commit = String::from_utf8(
        std::process::Command::new("git")
            .args(["-C", repository.to_str().unwrap(), "rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    git_fixture_command(&repository, ["branch", "feature/local"]);
    git_fixture_command(
        &repository,
        [
            "update-ref",
            "refs/remotes/origin/review",
            base_commit.as_str(),
        ],
    );
    git_fixture_command(
        &repository,
        ["tag", "-a", "api-v1", "-m", "api tag", base_commit.as_str()],
    );

    let manager = Arc::new(
        WorkspaceManager::new(
            root.join("data/workspaces.json"),
            root.join("managed"),
            root.join("runtime"),
        )
        .unwrap(),
    );
    let local = manager.register_local(&repository).unwrap();
    let (runtime, _) = p0_runtime(&repository, true);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = serve(
        listener,
        "workspace-api-token",
        ServerState {
            browser: None,
            management: None,
            runtime: Arc::new(runtime),
            mcp: Arc::new(NoMcp),
            skills: Arc::new(NoSkills),
            plugins: None,
            workflows: None,
            workspaces: Some(manager.clone()),
            workspace_id: Some(local.id.clone()),
            workspace: repository.clone(),
            project_trusted: true,
            language: None,
        },
    )
    .await
    .unwrap();
    let base = format!("http://{}", handle.address);
    let client = reqwest::Client::new();

    let initial = client
        .get(format!("{base}/v1/workspaces"))
        .bearer_auth("workspace-api-token")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<haarness_protocol::WorkspaceListResponse>()
        .await
        .unwrap();
    assert_eq!(initial.workspaces.len(), 1);
    assert_eq!(
        initial.current_workspace_id.as_deref(),
        Some(local.id.as_str())
    );

    let refs = client
        .get(format!("{base}/v1/workspaces/refs"))
        .bearer_auth("workspace-api-token")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<haarness_protocol::WorkspaceRefsResponse>()
        .await
        .unwrap();
    assert_eq!(refs.default_ref, "refs/heads/main");
    assert_eq!(refs.default_commit, base_commit);
    assert!(refs.refs.iter().any(|item| {
        item.reference == "refs/heads/main"
            && item.kind == haarness_protocol::WorkspaceRefKind::LocalBranch
            && item.current
    }));
    assert!(refs.refs.iter().any(|item| {
        item.reference == "refs/heads/feature/local"
            && item.kind == haarness_protocol::WorkspaceRefKind::LocalBranch
    }));
    assert!(refs.refs.iter().any(|item| {
        item.reference == "refs/remotes/origin/review"
            && item.kind == haarness_protocol::WorkspaceRefKind::RemoteBranch
    }));
    assert!(refs.refs.iter().any(|item| {
        item.reference == "refs/tags/api-v1"
            && item.kind == haarness_protocol::WorkspaceRefKind::Tag
    }));

    let default_ref = client
        .post(format!("{base}/v1/workspaces/resolve-ref"))
        .bearer_auth("workspace-api-token")
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<haarness_protocol::WorkspaceResolveRefResponse>()
        .await
        .unwrap();
    assert_eq!(default_ref.reference, "refs/heads/main");
    assert_eq!(default_ref.commit, base_commit);
    for reference in ["api-v1", base_commit.as_str()] {
        let resolved = client
            .post(format!("{base}/v1/workspaces/resolve-ref"))
            .bearer_auth("workspace-api-token")
            .json(&haarness_protocol::WorkspaceResolveRefRequest {
                reference: Some(reference.to_owned()),
            })
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json::<haarness_protocol::WorkspaceResolveRefResponse>()
            .await
            .unwrap();
        assert_eq!(resolved.reference, reference);
        assert_eq!(resolved.commit, base_commit);
    }
    let invalid_ref = client
        .post(format!("{base}/v1/workspaces/resolve-ref"))
        .bearer_auth("workspace-api-token")
        .json(&haarness_protocol::WorkspaceResolveRefRequest {
            reference: Some("missing/ref".into()),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(invalid_ref.status(), reqwest::StatusCode::BAD_REQUEST);
    let error = invalid_ref
        .json::<haarness_protocol::ApiError>()
        .await
        .unwrap();
    assert_eq!(error.code, "invalid_request");

    let created = client
        .post(format!("{base}/v1/workspaces"))
        .bearer_auth("workspace-api-token")
        .json(&haarness_protocol::WorkspaceCreateRequest {
            name: "api-feature".into(),
            from_ref: None,
            branch: None,
            path: Some("../api-feature".into()),
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<haarness_protocol::WorkspaceOperationResponse>()
        .await
        .unwrap();
    let feature = created.workspace;
    assert_eq!(
        feature.from_ref.as_deref(),
        Some(default_ref.reference.as_str())
    );
    assert_eq!(
        feature.base_commit.as_deref(),
        Some(default_ref.commit.as_str())
    );
    assert_eq!(
        std::path::PathBuf::from(&feature.path),
        std::fs::canonicalize(root.join("api-feature")).unwrap()
    );
    assert!(std::path::Path::new(&feature.path).is_dir());

    let opened = client
        .post(format!("{base}/v1/workspaces/{}/open", feature.id))
        .bearer_auth("workspace-api-token")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<haarness_protocol::WorkspaceOperationResponse>()
        .await
        .unwrap();
    assert_eq!(opened.workspace.id, feature.id);

    let cleanup_live = client
        .post(format!("{base}/v1/workspaces/{}/cleanup", feature.id))
        .bearer_auth("workspace-api-token")
        .json(&haarness_protocol::WorkspaceCleanupRequest { confirm: true })
        .send()
        .await
        .unwrap();
    assert_eq!(cleanup_live.status(), reqwest::StatusCode::CONFLICT);
    assert!(std::path::Path::new(&feature.path).is_dir());
    assert_eq!(manager.get(&feature.id).unwrap().id, feature.id);

    let lease_path = manager.activity_lock_path(&feature.id);
    std::fs::create_dir_all(lease_path.parent().unwrap()).unwrap();
    let lease = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lease_path)
        .unwrap();
    lease.try_lock_exclusive().unwrap();
    let blocked = client
        .post(format!("{base}/v1/workspaces/{}/reset", feature.id))
        .bearer_auth("workspace-api-token")
        .json(&haarness_protocol::WorkspaceResetRequest {
            confirm: true,
            ..haarness_protocol::WorkspaceResetRequest::default()
        })
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), reqwest::StatusCode::CONFLICT);
    let error = blocked.json::<haarness_protocol::ApiError>().await.unwrap();
    assert_eq!(error.code, "conflict");
    drop(lease);

    git_fixture_command(
        std::path::Path::new(&feature.path),
        ["commit", "--allow-empty", "-m", "ahead"],
    );
    client
        .post(format!("{base}/v1/workspaces/{}/reset", feature.id))
        .bearer_auth("workspace-api-token")
        .json(&haarness_protocol::WorkspaceResetRequest {
            confirm: true,
            ..haarness_protocol::WorkspaceResetRequest::default()
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let feature_head = std::process::Command::new("git")
        .args(["-C", &feature.path, "rev-parse", "HEAD"])
        .output()
        .unwrap();
    let main_head = std::process::Command::new("git")
        .args(["-C", repository.to_str().unwrap(), "rev-parse", "main"])
        .output()
        .unwrap();
    assert_eq!(feature_head.stdout, main_head.stdout);

    let removed = client
        .delete(format!("{base}/v1/workspaces/{}", feature.id))
        .bearer_auth("workspace-api-token")
        .json(&haarness_protocol::WorkspaceRemoveRequest {
            confirm: true,
            discard_changes: false,
            delete_branch: true,
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<haarness_protocol::WorkspaceOperationResponse>()
        .await
        .unwrap();
    assert!(!removed.workspace.exists);
    assert!(!std::path::Path::new(&feature.path).exists());
    assert_eq!(manager.list(None).unwrap().workspaces.len(), 1);

    let stale = client
        .post(format!("{base}/v1/workspaces"))
        .bearer_auth("workspace-api-token")
        .json(&haarness_protocol::WorkspaceCreateRequest {
            name: "api-stale".into(),
            from_ref: None,
            branch: None,
            path: Some("../api-stale".into()),
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<haarness_protocol::WorkspaceOperationResponse>()
        .await
        .unwrap()
        .workspace;
    let stale_branch = stale.branch.clone().unwrap();
    git_fixture_command(
        &repository,
        ["worktree", "remove", "--force", stale.path.as_str()],
    );
    let cleaned = client
        .post(format!("{base}/v1/workspaces/{}/cleanup", stale.id))
        .bearer_auth("workspace-api-token")
        .json(&haarness_protocol::WorkspaceCleanupRequest { confirm: true })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<haarness_protocol::WorkspaceOperationResponse>()
        .await
        .unwrap();
    assert!(!cleaned.workspace.exists);
    assert_eq!(
        cleaned.workspace.branch.as_deref(),
        Some(stale_branch.as_str())
    );
    assert_eq!(manager.list(None).unwrap().workspaces.len(), 1);
    let branch = format!("refs/heads/{stale_branch}");
    let retained = std::process::Command::new("git")
        .args([
            "-C",
            repository.to_str().unwrap(),
            "show-ref",
            "--verify",
            "--quiet",
            branch.as_str(),
        ])
        .status()
        .unwrap();
    assert!(retained.success());

    handle.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[derive(Clone)]
struct DeferredGate {
    requests: Arc<std::sync::Mutex<Vec<String>>>,
    ready: Arc<tokio::sync::Semaphore>,
    release: Arc<tokio::sync::Semaphore>,
}
impl Default for DeferredGate {
    fn default() -> Self {
        Self {
            requests: Default::default(),
            ready: Arc::new(tokio::sync::Semaphore::new(0)),
            release: Arc::new(tokio::sync::Semaphore::new(0)),
        }
    }
}
#[async_trait]
impl ModelProvider for DeferredGate {
    fn id(&self) -> &str {
        "echo"
    }
    fn info(&self) -> ProviderInfo {
        Echo.info()
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if let Some(title) = title_fixture(&request) {
            return title;
        }
        let text = request
            .messages
            .iter()
            .filter_map(|message| match message {
                ProviderMessage::Text { role, content } if role == "user" => Some(content.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let failing = text.contains("fail original");
        let index = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(text);
            requests.len()
        };
        self.ready.add_permits(1);
        let permit = tokio::select! {biased; ()=cancel.cancelled()=>return Err(CoreError::Cancelled), permit=self.release.acquire()=>permit.unwrap()};
        permit.forget();
        if failing {
            return Err(CoreError::Provider("fixture failure".into()));
        }
        let events = if index == 1 {
            vec![
                Ok(ProviderEvent::ToolCall {
                    call_id: "boundary-tool".into(),
                    name: "missing-fixture-tool".into(),
                    arguments: serde_json::json!({}),
                    provider_metadata: Value::Null,
                }),
                Ok(ProviderEvent::Finished {
                    reason: "tool_calls".into(),
                }),
            ]
        } else {
            vec![
                Ok(ProviderEvent::TextDelta(format!(
                    "Finished logical task {index}"
                ))),
                Ok(ProviderEvent::Finished {
                    reason: "done".into(),
                }),
            ]
        };
        Ok(Box::pin(stream::iter(events)))
    }
}
impl DeferredGate {
    async fn wait(&self) {
        tokio::time::timeout(Duration::from_secs(10), self.ready.acquire())
            .await
            .expect("provider reached boundary")
            .unwrap()
            .forget();
    }
    fn next(&self) {
        self.release.add_permits(1);
    }
}
fn deferred_request(text: &str) -> haarness_protocol::QueueInputRequest {
    haarness_protocol::QueueInputRequest {
        preview: None,
        reasoning_effort: None,
        delivery: haarness_protocol::InputDelivery::Queue,
        request_id: RequestId::new(),
        text: text.into(),
        attachments: vec![],
        synthetic: false,
    }
}
fn deferred_runtime(provider: DeferredGate, store: Arc<SqliteStore>) -> AgentRuntime {
    AgentRuntime::new(
        store,
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider) as Arc<dyn ModelProvider>
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(128),
    )
}

#[tokio::test]
async fn deferred_queue_waits_for_idle_steers_first_and_delivers_one_at_a_time() {
    let provider = DeferredGate::default();
    let runtime = deferred_runtime(
        provider.clone(),
        Arc::new(SqliteStore::in_memory().unwrap()),
    );
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("original implementation"))
        .await
        .unwrap();
    provider.wait().await;
    let first = deferred_request("deferred documentation");
    let second = deferred_request("deferred release notes");
    runtime.queue_input(thread.id, first.clone()).await.unwrap();
    runtime
        .queue_input(thread.id, second.clone())
        .await
        .unwrap();
    // Retries never duplicate a deferred prompt.
    runtime.queue_input(thread.id, first.clone()).await.unwrap();
    assert_eq!(runtime.input_queue(thread.id).await.unwrap().items.len(), 2);
    let mut steer = deferred_request("steer use sqlite");
    steer.delivery = haarness_protocol::InputDelivery::Steer;
    runtime.queue_input(thread.id, steer).await.unwrap();
    let before = runtime.items(thread.id, None).await.unwrap();
    assert!(!before.iter().any(|i|matches!(&i.data,haarness_protocol::ItemData::UserMessage{text,..} if text.contains("deferred"))));
    provider.next();
    provider.wait().await;
    let requests = provider.requests.lock().unwrap().clone();
    assert!(requests[1].contains("steer use sqlite"));
    assert!(!requests[1].contains("deferred documentation"));
    assert!(!requests[1].contains("deferred release notes"));
    provider.next();
    provider.wait().await;
    let requests = provider.requests.lock().unwrap().clone();
    assert!(requests[2].contains("deferred documentation"));
    assert!(!requests[2].contains("deferred release notes"));
    assert_eq!(runtime.input_queue(thread.id).await.unwrap().items.len(), 1);
    // Each queued prompt starts its own turn once the previous one closes.
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let documentation = runtime.active_turn(thread.id).await.unwrap().unwrap();
    assert_ne!(documentation.id, turn.id);
    provider.next();
    provider.wait().await;
    assert!(provider.requests.lock().unwrap()[3].contains("deferred release notes"));
    assert_eq!(
        wait_done(&runtime, &documentation).await.status,
        TurnStatus::Completed
    );
    let notes = runtime.active_turn(thread.id).await.unwrap().unwrap();
    assert_ne!(notes.id, documentation.id);
    provider.next();
    assert_eq!(
        wait_done(&runtime, &notes).await.status,
        TurnStatus::Completed
    );
    assert!(
        runtime
            .input_queue(thread.id)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    runtime.queue_input(thread.id, first).await.unwrap();
    assert!(runtime.active_turn(thread.id).await.unwrap().is_none());
    assert_eq!(provider.requests.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn deferred_queue_stop_parks_work_and_send_resumes_selected_message() {
    let provider = DeferredGate::default();
    let runtime = deferred_runtime(
        provider.clone(),
        Arc::new(SqliteStore::in_memory().unwrap()),
    );
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("long original"))
        .await
        .unwrap();
    provider.wait().await;
    runtime
        .queue_input(thread.id, deferred_request("later first"))
        .await
        .unwrap();
    let second = deferred_request("later second");
    runtime
        .queue_input(thread.id, second.clone())
        .await
        .unwrap();
    runtime.interrupt(turn.id).await.unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Interrupted
    );
    wait_idle(&runtime, thread.id).await;
    let queue = runtime.input_queue(thread.id).await.unwrap();
    assert_eq!(queue.items.len(), 2);
    runtime
        .change_input_queue(
            thread.id,
            haarness_protocol::InputQueueAction::Steer {
                revision: queue.revision,
                id: second.request_id,
            },
        )
        .await
        .unwrap();
    provider.wait().await;
    let requests = provider.requests.lock().unwrap().clone();
    assert!(requests[1].contains("later second"));
    assert!(!requests[1].contains("later first"));
    provider.next();
    provider.wait().await;
    provider.next();
    wait_idle(&runtime, thread.id).await;
    assert!(
        runtime
            .input_queue(thread.id)
            .await
            .unwrap()
            .items
            .is_empty()
    );
}

#[tokio::test]
async fn deferred_queue_can_be_edited_reordered_removed_and_promoted_while_busy() {
    let provider = DeferredGate::default();
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let runtime = deferred_runtime(provider.clone(), store);
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("original"))
        .await
        .unwrap();
    provider.wait().await;
    let one = deferred_request("one");
    let two = deferred_request("two");
    let three = deferred_request("three");
    for input in [&one, &two, &three] {
        runtime.queue_input(thread.id, input.clone()).await.unwrap();
    }
    let q = runtime.input_queue(thread.id).await.unwrap();
    let q = runtime
        .change_input_queue(
            thread.id,
            haarness_protocol::InputQueueAction::Edit {
                revision: q.revision,
                id: one.request_id,
                text: "edited one".into(),
                attachments: None,
            },
        )
        .await
        .unwrap();
    let stale = q.revision - 1;
    assert!(matches!(
        runtime
            .change_input_queue(
                thread.id,
                haarness_protocol::InputQueueAction::Remove {
                    revision: stale,
                    id: one.request_id
                }
            )
            .await,
        Err(CoreError::Conflict(_))
    ));
    let q = runtime
        .change_input_queue(
            thread.id,
            haarness_protocol::InputQueueAction::Reorder {
                revision: q.revision,
                ids: vec![three.request_id, two.request_id, one.request_id],
            },
        )
        .await
        .unwrap();
    let q = runtime
        .change_input_queue(
            thread.id,
            haarness_protocol::InputQueueAction::Remove {
                revision: q.revision,
                id: two.request_id,
            },
        )
        .await
        .unwrap();
    runtime
        .change_input_queue(
            thread.id,
            haarness_protocol::InputQueueAction::Steer {
                revision: q.revision,
                id: one.request_id,
            },
        )
        .await
        .unwrap();
    provider.next();
    provider.wait().await;
    let texts = provider.requests.lock().unwrap().clone();
    assert!(texts[1].contains("edited one"));
    assert!(!texts[1].contains("three"));
    assert!(!texts[1].contains("two"));
    provider.next();
    provider.wait().await;
    assert!(provider.requests.lock().unwrap()[2].contains("three"));
    provider.next();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
}

#[tokio::test]
async fn deferred_queue_runs_after_a_failed_turn_and_pauses_when_a_queued_prompt_fails() {
    let provider = DeferredGate::default();
    let runtime = deferred_runtime(
        provider.clone(),
        Arc::new(SqliteStore::in_memory().unwrap()),
    );
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("fail original"))
        .await
        .unwrap();
    provider.wait().await;
    let mut input = deferred_request("first later");
    runtime.queue_input(thread.id, input.clone()).await.unwrap();
    runtime
        .queue_input(thread.id, deferred_request("untouched later"))
        .await
        .unwrap();
    provider.next();
    assert_eq!(wait_done(&runtime, &turn).await.status, TurnStatus::Failed);
    // A failed turn does not strand what was queued behind it.
    provider.wait().await;
    assert!(provider.requests.lock().unwrap()[1].contains("first later"));
    let queued = runtime.active_turn(thread.id).await.unwrap().unwrap();
    // The fixture fails every request that carries the failed message, so
    // the queued prompt fails too. Then the rest of the queue waits.
    provider.next();
    assert_eq!(
        wait_done(&runtime, &queued).await.status,
        TurnStatus::Failed
    );
    wait_idle(&runtime, thread.id).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(runtime.active_turn(thread.id).await.unwrap().is_none());
    // Retrying a delivered admission never wakes the paused queue.
    input.delivery = haarness_protocol::InputDelivery::Steer;
    input.text = "retry with different mode".into();
    runtime.queue_input(thread.id, input).await.unwrap();
    assert!(runtime.active_turn(thread.id).await.unwrap().is_none());
    let queue = runtime.input_queue(thread.id).await.unwrap();
    assert_eq!(queue.items.len(), 1);
    assert_eq!(queue.items[0].text, "untouched later");
}

#[tokio::test]
async fn stop_answers_unread_steering_and_keeps_the_queue_paused_until_a_new_message() {
    let provider = DeferredGate::default();
    let runtime = deferred_runtime(
        provider.clone(),
        Arc::new(SqliteStore::in_memory().unwrap()),
    );
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("long refactor"))
        .await
        .unwrap();
    provider.wait().await;
    runtime
        .queue_input(thread.id, deferred_request("later changelog"))
        .await
        .unwrap();
    let mut steer = deferred_request("keep the public names");
    steer.delivery = haarness_protocol::InputDelivery::Steer;
    runtime.queue_input(thread.id, steer).await.unwrap();
    runtime
        .interrupt_thread_tree_with(thread.id, true)
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Interrupted
    );
    // The steering the agent never read is answered next, not dropped.
    provider.wait().await;
    let request = provider.requests.lock().unwrap()[1].clone();
    assert!(request.contains("keep the public names"));
    assert!(request.contains("The user stopped your previous work"));
    let continuation = runtime.active_turn(thread.id).await.unwrap().unwrap();
    provider.next();
    assert_eq!(
        wait_done(&runtime, &continuation).await.status,
        TurnStatus::Completed
    );
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(!items.iter().any(|item| matches!(&item.data, haarness_protocol::ItemData::Error { message, .. } if message.contains("Queued input could not be delivered"))));
    // Answering the steering does not drain the queue the stop paused.
    wait_idle(&runtime, thread.id).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(runtime.active_turn(thread.id).await.unwrap().is_none());
    assert_eq!(runtime.input_queue(thread.id).await.unwrap().items.len(), 1);
    // A new message runs first, then the paused queue continues.
    let fresh = runtime
        .start_turn(thread.id, turn_request("something new"))
        .await
        .unwrap();
    provider.wait().await;
    provider.next();
    assert_eq!(
        wait_done(&runtime, &fresh).await.status,
        TurnStatus::Completed
    );
    provider.wait().await;
    assert!(provider.requests.lock().unwrap()[3].contains("later changelog"));
    provider.next();
    wait_drained(&runtime, thread.id).await;
}

#[tokio::test]
async fn programmatic_stop_does_not_answer_unread_steering() {
    let provider = DeferredGate::default();
    let runtime = deferred_runtime(
        provider.clone(),
        Arc::new(SqliteStore::in_memory().unwrap()),
    );
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("long refactor"))
        .await
        .unwrap();
    provider.wait().await;
    let mut steer = deferred_request("keep the public names");
    steer.delivery = haarness_protocol::InputDelivery::Steer;
    runtime.queue_input(thread.id, steer).await.unwrap();
    runtime.interrupt_thread_tree(thread.id).await.unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Interrupted
    );
    wait_idle(&runtime, thread.id).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(runtime.active_turn(thread.id).await.unwrap().is_none());
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(items.iter().any(|item| matches!(&item.data, haarness_protocol::ItemData::Error { message, .. } if message.contains("Queued input could not be delivered"))));
}

#[tokio::test]
async fn deferred_queue_idle_submission_starts_work_immediately() {
    let runtime = AgentRuntime::new(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Echo) as Arc<dyn ModelProvider>
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    );
    let thread = echo_thread(&runtime).await;
    let request = deferred_request("idle queued task");
    let response = runtime
        .queue_input(thread.id, request.clone())
        .await
        .unwrap();
    let turn = response.turn.expect("idle queue starts its first prompt");
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    runtime.queue_input(thread.id, request).await.unwrap();
    assert!(
        runtime
            .input_queue(thread.id)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let items = runtime.items(thread.id, None).await.unwrap();
    assert_eq!(items.iter().filter(|item|matches!(&item.data,haarness_protocol::ItemData::UserMessage{text,..} if text=="idle queued task")).count(),1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_idle_queue_admissions_deliver_every_prompt_once() {
    let runtime = Arc::new(AgentRuntime::new(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Echo) as Arc<dyn ModelProvider>
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(256),
    ));
    for _ in 0..16 {
        let thread = echo_thread(&runtime).await;
        let barrier = Arc::new(tokio::sync::Barrier::new(8));
        let mut tasks = tokio::task::JoinSet::new();
        for index in 0..8 {
            let runtime = runtime.clone();
            let barrier = barrier.clone();
            tasks.spawn(async move {
                barrier.wait().await;
                runtime
                    .queue_input(
                        thread.id,
                        deferred_request(&format!("parallel prompt {index}")),
                    )
                    .await
                    .unwrap();
            });
        }
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
        wait_drained(&runtime, thread.id).await;
        assert!(
            runtime
                .input_queue(thread.id)
                .await
                .unwrap()
                .items
                .is_empty()
        );
        let items = runtime.items(thread.id, None).await.unwrap();
        for index in 0..8 {
            let expected = format!("parallel prompt {index}");
            assert_eq!(items.iter().filter(|item| matches!(&item.data, haarness_protocol::ItemData::UserMessage { text, .. } if text == &expected)).count(), 1);
        }
    }
}

#[tokio::test]
async fn stopping_with_deferred_input_resolves_the_cancelled_permission_for_clients() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, false);
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!tool bash {"command":"printf stopped-permission"}"#),
        )
        .await
        .unwrap();
    wait_pending_permissions(&runtime, 1).await;
    let permission = runtime.pending_permissions()[0].permission_id.clone();
    runtime
        .queue_input(
            thread.id,
            deferred_request("parked after approval cancellation"),
        )
        .await
        .unwrap();
    runtime.interrupt(turn.id).await.unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Interrupted
    );
    wait_idle(&runtime, thread.id).await;
    assert!(runtime.pending_permissions().is_empty());
    let events = runtime.events().subscribe(None).replay;
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "permission.resolved"
                && event.payload["permissionId"] == permission
                && event.payload["status"] == "cancelled")
            .count(),
        1
    );
    assert_eq!(runtime.input_queue(thread.id).await.unwrap().items.len(), 1);
}

#[tokio::test]
async fn stop_answers_steering_sent_into_a_request_the_stop_cancelled() {
    use haarness_protocol::ItemData;
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, false);
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!tool bash {"command":"printf waits-for-approval"}"#),
        )
        .await
        .unwrap();
    wait_pending_permissions(&runtime, 1).await;
    let mut steer = deferred_request("actually explain the plan");
    steer.delivery = haarness_protocol::InputDelivery::Steer;
    runtime.queue_input(thread.id, steer).await.unwrap();
    // The cancelled approval lets the runner send the steering to the model
    // before the stop lands, so it is consumed but never answered.
    runtime
        .interrupt_thread_tree_with(thread.id, true)
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Interrupted
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let items = runtime.items(thread.id, None).await.unwrap();
            if items.iter().any(|item| matches!(&item.data, ItemData::AssistantMessage { text } if text.contains("stopped your previous work")))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a continuation answers the steering");
    wait_idle(&runtime, thread.id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_task_resumptions_recheck_the_child_after_waiting_for_capacity() {
    use haarness_protocol::ItemData;
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, true);
    let parent = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let initial = runtime
        .start_turn(
            parent.id,
            turn_request(r#"!tool task {"agent":"general","prompt":"Initial completed child"}"#),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &initial).await.status,
        TurnStatus::Completed
    );
    let child = runtime
        .items(parent.id, None)
        .await
        .unwrap()
        .into_iter()
        .find_map(|item| match item.data {
            ItemData::TaskLink {
                child_thread_id, ..
            } => Some(child_thread_id),
            _ => None,
        })
        .unwrap();

    // Occupy the four global slots so all three continuations observe the
    // same idle child before awaiting capacity in the old implementation.
    let blocker = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let calls: Vec<Value> = (0..4)
        .map(|_| {
            serde_json::json!({
                "name": "task", "arguments": {"agent":"general", "background":true,
                "prompt":"!tool bash {\"command\":\"sleep 1\"}"}
            })
        })
        .collect();
    let blocking = runtime
        .start_turn(
            blocker.id,
            turn_request(&format!("!multi {}", serde_json::json!(calls))),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &blocking).await.status,
        TurnStatus::Completed
    );
    assert_eq!(runtime.background_tasks(blocker.id).await.unwrap().len(), 4);

    let prompts: Vec<String> = (0..3)
        .map(|index| format!("!tool bash {{\"command\":\"sleep 0.3; printf resumed-{index}\"}}"))
        .collect();
    let calls: Vec<Value> = prompts
        .iter()
        .map(|prompt| {
            serde_json::json!({
                "name":"task", "arguments":{"agent":"general", "task_id":child, "prompt":prompt}
            })
        })
        .collect();
    let resumed = runtime
        .start_turn(
            parent.id,
            turn_request(&format!("!multi {}", serde_json::json!(calls))),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &resumed).await.status,
        TurnStatus::Completed
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if runtime
                .background_tasks(parent.id)
                .await
                .unwrap()
                .is_empty()
                && runtime.active_turn(parent.id).await.unwrap().is_none()
                && runtime.active_turn(child).await.unwrap().is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("all continuations and their single delivery settle");
    let errors: Vec<String> = runtime
        .items(parent.id, None)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item.data {
            ItemData::ToolResult {
                name,
                is_error: true,
                output,
                ..
            } if name == "task" => Some(output),
            _ => None,
        })
        .collect();
    assert!(
        errors.is_empty(),
        "concurrent same-child resumptions failed: {errors:?}"
    );
    let items = runtime.items(child, None).await.unwrap();
    for prompt in &prompts {
        assert_eq!(
            items
                .iter()
                .filter(
                    |item| matches!(&item.data, ItemData::UserMessage {text, ..} if text == prompt)
                )
                .count(),
            1,
            "every continuation must be admitted once: {prompt}"
        );
    }
    assert_eq!(
        runtime
            .list_threads(false)
            .await
            .unwrap()
            .into_iter()
            .filter(|thread| thread.parent_id == Some(parent.id))
            .count(),
        1,
        "an explicit task_id must reuse the same child rather than silently spawning duplicates"
    );
}

#[path = "cases/permission_parity.rs"]
mod permission_parity;

#[path = "cases/chrome.rs"]
mod chrome;

#[tokio::test]
async fn workspace_file_routes_answer_a_missing_file_with_404_in_plain_words() {
    let workspace =
        std::env::temp_dir().join(format!("haarness-files-gone-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(workspace.join("docs")).unwrap();
    std::fs::write(workspace.join("docs/readme.md"), "# Readme").unwrap();
    // Servers run in their canonical folder (the temporary one is a symlink on macOS).
    let workspace = workspace.canonicalize().unwrap();
    let runtime = Arc::new(AgentRuntime::new(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Echo) as Arc<dyn ModelProvider>
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    ));
    let handle = serve(
        TcpListener::bind("127.0.0.1:0").await.unwrap(),
        "files-fixture",
        ServerState {
            browser: None,
            management: None,
            runtime,
            mcp: Arc::new(NoMcp),
            skills: Arc::new(NoSkills),
            plugins: None,
            workflows: None,
            workspaces: None,
            workspace_id: None,
            workspace: workspace.clone(),
            project_trusted: false,
            language: None,
        },
    )
    .await
    .unwrap();
    let base = format!("http://{}", handle.address);
    let http = reqwest::Client::new();
    let get = |path: &str| {
        http.get(format!("{base}/v1/files/{path}"))
            .bearer_auth("files-fixture")
            .send()
    };
    let answer = |response: reqwest::Response| async move {
        (response.status(), response.text().await.unwrap())
    };
    let gone = |message: &str| (reqwest::StatusCode::NOT_FOUND, message.to_owned());
    assert_eq!(
        answer(get("content?path=docs/deleted.md").await.unwrap()).await,
        gone("This file no longer exists")
    );
    assert_eq!(
        answer(get("list?path=deleted").await.unwrap()).await,
        gone("This folder no longer exists")
    );
    // A missing file has no current revision, whether or not its folder went too.
    for path in ["docs/deleted.html", "deleted/index.html"] {
        let (status, body) =
            answer(get(&format!("preview/current?path={path}")).await.unwrap()).await;
        assert_eq!(status, reqwest::StatusCode::OK, "{path}: {body}");
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["revision"],
            Value::Null
        );
    }
    // Versions live outside the workspace: only the version itself can be gone.
    let unknown = "0".repeat(64);
    assert_eq!(
        answer(
            get(&format!("preview?path=deleted/index.html&hash={unknown}"))
                .await
                .unwrap()
        )
        .await,
        gone("This preview version is no longer available")
    );
    // Other failures keep their 400 and their words.
    assert_eq!(
        answer(get("content?path=../outside.txt").await.unwrap()).await,
        (
            reqwest::StatusCode::BAD_REQUEST,
            "Path must be relative to the workspace".to_owned()
        )
    );
    assert_eq!(
        answer(get("content?path=docs").await.unwrap()).await,
        (
            reqwest::StatusCode::BAD_REQUEST,
            "Only regular files can be opened".to_owned()
        )
    );
    let listed: Value = get("list?path=docs").await.unwrap().json().await.unwrap();
    assert_eq!(listed["entries"][0]["path"], "docs/readme.md");
    handle.shutdown().await.unwrap();
    std::fs::remove_dir_all(workspace).unwrap();
}

/// Where `preview` keeps a workspace's versions, as the server finds it.
fn preview_versions(workspace: &std::path::Path) -> std::path::PathBuf {
    use sha2::Digest as _;
    let root = std::env::var_os("HAARNESS_DATA_DIR")
        .map(|root| std::path::PathBuf::from(root).join("data"))
        .or_else(|| {
            std::env::var_os("HAARNESS_HOME")
                .map(std::path::PathBuf::from)
                .filter(|root| root.is_absolute())
                .map(|root| root.join("data"))
        })
        .unwrap_or_else(|| {
            directories::ProjectDirs::from("dev", "haarness", "haarness")
                .unwrap()
                .data_local_dir()
                .to_owned()
        });
    let key = sha2::Sha256::digest(
        workspace
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .as_bytes(),
    );
    root.join("preview").join(format!("{key:x}"))
}

#[tokio::test]
async fn a_preview_whose_folder_was_removed_is_restored_into_a_new_folder() {
    let base = std::env::temp_dir().join(format!("haarness-preview-gone-{}", uuid::Uuid::now_v7()));
    let (workspace, outside) = (base.join("workspace"), base.join("outside"));
    std::fs::create_dir_all(workspace.join("site")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(workspace.join("site/index.html"), "<h1>first</h1>").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, workspace.join("link")).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    let shown = haarness_server::preview::PreviewTool(workspace.clone())
        .execute(
            serde_json::json!({"filePath": "site/index.html"}),
            ToolContext {
                execution: haarness_core::ExecutionSettings::default(),
                thread_id: haarness_protocol::ThreadId::new(),
                turn_id: haarness_protocol::TurnId::new(),
                workspace: workspace.clone(),
                authorized_external_paths: Vec::new(),
                cancel: CancellationToken::new(),
                invoker: None,
            },
        )
        .await
        .unwrap();
    let version = shown.metadata["preview"]["version"]
        .as_str()
        .unwrap()
        .to_owned();
    std::fs::remove_dir_all(workspace.join("site")).unwrap();
    let runtime = Arc::new(AgentRuntime::new(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Echo) as Arc<dyn ModelProvider>
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    ));
    let handle = serve(
        TcpListener::bind("127.0.0.1:0").await.unwrap(),
        "preview-fixture",
        ServerState {
            browser: None,
            management: None,
            runtime,
            mcp: Arc::new(NoMcp),
            skills: Arc::new(NoSkills),
            plugins: None,
            workflows: None,
            workspaces: None,
            workspace_id: None,
            workspace: workspace.clone(),
            project_trusted: false,
            language: None,
        },
    )
    .await
    .unwrap();
    let base_url = format!("http://{}", handle.address);
    let http = reqwest::Client::new();
    // The file and its folder are gone: deleted, so Restore is offered.
    let current: Value = http
        .get(format!(
            "{base_url}/v1/files/preview/current?path=site/index.html"
        ))
        .bearer_auth("preview-fixture")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(current["revision"], Value::Null);
    let empty = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(b""));
    let restore = |path: &str| {
        http.post(format!("{base_url}/v1/files/preview/restore"))
            .bearer_auth("preview-fixture")
            .json(&serde_json::json!({"path": path, "hash": version, "revision": empty}))
            .send()
    };
    let restored = restore("site/index.html").await.unwrap();
    assert_eq!(restored.status(), reqwest::StatusCode::OK);
    let restored: Value = restored.json().await.unwrap();
    assert_eq!(restored["file"]["revision"], version.as_str());
    assert_eq!(restored["backup"], empty.as_str());
    assert_eq!(
        std::fs::read_to_string(workspace.join("site/index.html")).unwrap(),
        "<h1>first</h1>"
    );
    // Folders are made only inside the workspace and outside Git metadata.
    #[cfg(unix)]
    {
        let escaped = restore("link/gone/index.html").await.unwrap();
        assert_eq!(escaped.status(), reqwest::StatusCode::BAD_REQUEST);
        assert_eq!(escaped.text().await.unwrap(), "Path escapes the workspace");
        assert!(!outside.join("gone").exists());
    }
    let git = restore(".git/gone/index.html").await.unwrap();
    assert_eq!(git.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(!workspace.join(".git").exists());
    handle.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(preview_versions(&workspace));
    std::fs::remove_dir_all(base).unwrap();
}

#[tokio::test]
async fn published_remote_chat_cannot_read_or_control_other_conversations() {
    let workspace =
        std::env::temp_dir().join(format!("haarness-remote-scope-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = Arc::new(AgentRuntime::new(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Echo) as Arc<dyn ModelProvider>
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    ));
    let handle = serve(
        TcpListener::bind("127.0.0.1:0").await.unwrap(),
        "remote-fixture",
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
            workspace: workspace.clone(),
            project_trusted: false,
            language: None,
        },
    )
    .await
    .unwrap();
    let base = format!("http://{}", handle.address);
    let http = reqwest::Client::new();
    let mut chats = vec![];
    for _ in 0..2 {
        chats.push(
            http.post(format!("{base}/v1/threads"))
                .bearer_auth("remote-fixture")
                .json(&serde_json::json!({"provider":"echo","model":"echo-1"}))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json::<Thread>()
                .await
                .unwrap(),
        );
    }
    let shared = &chats[0];
    let private = &chats[1];
    let scoped = reqwest::Client::builder()
        .default_headers({
            let mut h = reqwest::header::HeaderMap::new();
            h.insert("authorization", "Bearer remote-fixture".parse().unwrap());
            h.insert(
                "x-haarness-remote-thread",
                shared.id.to_string().parse().unwrap(),
            );
            h
        })
        .build()
        .unwrap();
    let rows = scoped
        .get(format!("{base}/v1/threads"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<Vec<Thread>>()
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, shared.id);
    for path in [
        format!("/threads/{}", private.id),
        format!("/threads/{}/items", private.id),
        "/workspaces".into(),
        "/ptys".into(),
    ] {
        assert_eq!(
            scoped
                .get(format!("{base}/v1{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::FORBIDDEN,
            "{path}"
        );
    }
    for path in [
        "/threads".into(),
        "/shutdown".into(),
        format!("/threads/{}/turns", private.id),
        format!("/threads/{}/fork", shared.id),
    ] {
        assert_eq!(
            scoped
                .post(format!("{base}/v1{path}"))
                .json(&serde_json::json!({}))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::FORBIDDEN,
            "{path}"
        );
    }
    // A shared chat follows its own Auto/Review setting.
    let wholesale = scoped
        .post(format!("{base}/v1/threads/{}/turns", shared.id))
        .json(
            &serde_json::json!({"requestId": RequestId::new(), "text": "hi", "autoApprove": true}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(wholesale.status(), reqwest::StatusCode::FORBIDDEN);
    let stream = scoped
        .get(format!("{base}/v1/events"))
        .send()
        .await
        .unwrap();
    let mut stream = stream.bytes_stream();
    scoped.put(format!("{base}/v1/files/content"))
        .json(&serde_json::json!({"path":"shared-file-proof.txt","content":"shared","revision":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}))
        .send().await.unwrap().error_for_status().unwrap();
    let private_turn = runtime
        .start_turn(private.id, turn_request("PRIVATE_MARKER"))
        .await
        .unwrap();
    wait_done(&runtime, &private_turn).await;
    let shared_turn = scoped
        .post(format!("{base}/v1/threads/{}/turns", shared.id))
        .json(&turn_request("SHARED_MARKER"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<Turn>()
        .await
        .unwrap();
    wait_done(&runtime, &shared_turn).await;
    let mut seen = String::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !seen.contains("SHARED_MARKER") {
            let bytes = stream.next().await.unwrap().unwrap();
            seen.push_str(&String::from_utf8_lossy(&bytes));
        }
    })
    .await
    .unwrap();
    assert!(seen.contains("files.updated"));
    assert!(seen.contains("shared-file-proof.txt"));
    assert!(!seen.contains("PRIVATE_MARKER"));
    assert!(!seen.contains(&private.id.to_string()));
    assert_eq!(
        scoped
            .get(format!("{base}/v1/turns/{}", private_turn.id))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    assert!(
        scoped
            .get(format!("{base}/v1/turns/{}", shared_turn.id))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let info = scoped
        .get(format!("{base}/v1/info"))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(
        !info["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "pty.v1")
    );
    drop(stream);
    handle.shutdown().await.unwrap();
    std::fs::remove_dir_all(workspace).unwrap();
}

#[tokio::test]
async fn preview_tool_and_prompt_are_opt_in_for_graphical_turns() {
    let workspace =
        std::env::temp_dir().join(format!("haarness-preview-gating-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = Capturing::default();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(workspace.clone())),
        EventBus::new(64),
        workspace.clone(),
        String::new(),
        RuntimeConfig::default(),
    );
    let thread = echo_thread(&runtime).await;
    for preview in [None, Some(true), Some(false)] {
        let mut request = turn_request("Return an answer");
        request.tools =
            preview.map(|enabled| std::collections::HashMap::from([("preview".into(), enabled)]));
        let turn = runtime.start_turn(thread.id, request).await.unwrap();
        wait_done(&runtime, &turn).await;
        wait_idle(&runtime, thread.id).await;
        let requests = provider.requests.lock().unwrap();
        let request = requests.last().unwrap();
        assert_eq!(
            request.tools.iter().any(|tool| tool.name == "preview"),
            preview == Some(true)
        );
        assert_eq!(
            request.system.contains("The user's client can preview"),
            preview == Some(true)
        );
    }
    runtime
        .set_tool_enabled("preview", Some(false))
        .await
        .unwrap();
    let mut request = turn_request("Disabled preview must not be advertised");
    request.tools = Some(std::collections::HashMap::from([("preview".into(), true)]));
    let turn = runtime.start_turn(thread.id, request).await.unwrap();
    wait_done(&runtime, &turn).await;
    let requests = provider.requests.lock().unwrap();
    let request = requests.last().unwrap();
    assert!(!request.tools.iter().any(|tool| tool.name == "preview"));
    assert!(!request.system.contains("The user's client can preview"));
    std::fs::remove_dir_all(workspace).unwrap();
}

#[tokio::test]
async fn a_chat_keeps_its_preview_setting_when_a_client_does_not_say() {
    let workspace =
        std::env::temp_dir().join(format!("haarness-preview-sticky-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = Capturing::default();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(workspace.clone())),
        EventBus::new(64),
        workspace.clone(),
        String::new(),
        RuntimeConfig::default(),
    );
    let thread = echo_thread(&runtime).await;
    // Desktop says yes, a phone says nothing, the terminal says no, then the
    // phone again: each silent turn matches the turn before it.
    for (preview, expected) in [
        (Some(true), true),
        (None, true),
        (Some(false), false),
        (None, false),
    ] {
        let mut request = turn_request("Return an answer");
        request.tools =
            preview.map(|enabled| std::collections::HashMap::from([("preview".into(), enabled)]));
        let turn = runtime.start_turn(thread.id, request).await.unwrap();
        wait_done(&runtime, &turn).await;
        wait_idle(&runtime, thread.id).await;
        let requests = provider.requests.lock().unwrap();
        let request = requests.last().unwrap();
        assert_eq!(
            request.tools.iter().any(|tool| tool.name == "preview"),
            expected,
            "{preview:?}"
        );
    }
    std::fs::remove_dir_all(workspace).unwrap();
}

fn effort_runtime(workspace: &std::path::Path) -> (AgentRuntime, Capturing) {
    let provider = Capturing::with_reasoning_efforts(&["low", "high"]);
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(workspace.to_path_buf())),
        EventBus::new(256),
        workspace.to_path_buf(),
        String::new(),
        RuntimeConfig::default(),
    );
    (runtime, provider)
}

fn queued(text: &str, effort: Option<&str>) -> haarness_protocol::QueueInputRequest {
    haarness_protocol::QueueInputRequest {
        preview: None,
        reasoning_effort: effort.map(str::to_owned),
        delivery: haarness_protocol::InputDelivery::Steer,
        request_id: RequestId::new(),
        text: text.into(),
        attachments: Vec::new(),
        synthetic: false,
    }
}

#[tokio::test]
async fn queued_and_deferred_turns_keep_the_chats_reasoning_effort() {
    let workspace =
        std::env::temp_dir().join(format!("haarness-effort-carry-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).unwrap();
    let (runtime, provider) = effort_runtime(&workspace);
    let thread = echo_thread(&runtime).await;
    let last_effort = || {
        provider
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .reasoning_effort
            .clone()
    };
    let mut request = turn_request("First prompt");
    request.reasoning_effort = Some("high".into());
    let turn = runtime.start_turn(thread.id, request).await.unwrap();
    wait_done(&runtime, &turn).await;
    wait_idle(&runtime, thread.id).await;
    assert_eq!(last_effort().as_deref(), Some("high"));

    // A message sent without an effort (a phone, an older client) keeps it.
    let response = runtime
        .queue_input(thread.id, queued("No effort given", None))
        .await
        .unwrap();
    wait_done(&runtime, response.turn.as_ref().unwrap()).await;
    wait_idle(&runtime, thread.id).await;
    assert_eq!(last_effort().as_deref(), Some("high"));

    // One that says which effort it wants gets it.
    let response = runtime
        .queue_input(thread.id, queued("Low please", Some("low")))
        .await
        .unwrap();
    wait_done(&runtime, response.turn.as_ref().unwrap()).await;
    wait_idle(&runtime, thread.id).await;
    assert_eq!(last_effort().as_deref(), Some("low"));

    // A deferred message carries its own effort through the queue.
    let mut deferred = queued("Deferred", Some("high"));
    deferred.delivery = haarness_protocol::InputDelivery::Queue;
    runtime.queue_input(thread.id, deferred).await.unwrap();
    wait_drained(&runtime, thread.id).await;
    assert_eq!(last_effort().as_deref(), Some("high"));
    std::fs::remove_dir_all(workspace).unwrap();
}

#[tokio::test]
async fn auto_approves_once_without_a_prompt_and_review_asks() {
    let workspace =
        std::env::temp_dir().join(format!("haarness-approval-auto-{}", uuid::Uuid::now_v7()));
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    let (_store, runtime) = workspace_runtime(&workspace);
    let thread = echo_thread(&runtime).await;
    let initial = runtime.approval_mode(thread.id).await.unwrap();
    assert_eq!(initial.mode, haarness_protocol::ApprovalMode::Review);
    assert!(!initial.explicit);
    let auto = runtime
        .set_approval_mode(
            thread.id,
            haarness_protocol::SetApprovalModeRequest {
                mode: haarness_protocol::ApprovalModeSetting::Auto,
                revision: Some(initial.revision.clone()),
            },
        )
        .await
        .unwrap();
    assert_eq!(auto.mode, haarness_protocol::ApprovalMode::Auto);
    assert!(auto.explicit);
    assert_ne!(auto.revision, initial.revision);

    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!tool write {"path":"auto.txt","content":"hi"}"#),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("auto.txt")).unwrap(),
        "hi"
    );
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(
        !items.iter().any(|item| matches!(
            item.data,
            haarness_protocol::ItemData::PermissionRequest { .. }
        )),
        "Auto must not show a prompt"
    );
    // Auto answers once; it never remembers a rule.
    runtime
        .set_approval_mode(
            thread.id,
            haarness_protocol::SetApprovalModeRequest {
                mode: haarness_protocol::ApprovalModeSetting::Review,
                revision: None,
            },
        )
        .await
        .unwrap();
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!tool write {"path":"auto.txt","content":"again"}"#),
        )
        .await
        .unwrap();
    let permission = wait_permission(&runtime, thread.id).await;
    runtime
        .reply_permission(
            &permission,
            haarness_protocol::PermissionReply {
                decision: haarness_protocol::PermissionReplyDecision::Reject,
                feedback: None,
            },
        )
        .await
        .unwrap();
    wait_done(&runtime, &turn).await;
    assert_eq!(
        std::fs::read_to_string(workspace.join("auto.txt")).unwrap(),
        "hi"
    );

    // A change made from a stale view is refused.
    let stale = runtime
        .set_approval_mode(
            thread.id,
            haarness_protocol::SetApprovalModeRequest {
                mode: haarness_protocol::ApprovalModeSetting::Auto,
                revision: Some(auto.revision),
            },
        )
        .await;
    assert!(matches!(stale, Err(CoreError::Conflict(_))), "{stale:?}");
    tokio::fs::remove_dir_all(workspace).await.unwrap();
}

#[tokio::test]
async fn turning_auto_on_answers_requests_already_waiting() {
    let workspace = std::env::temp_dir().join(format!(
        "haarness-approval-release-{}",
        uuid::Uuid::now_v7()
    ));
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    let (_store, runtime) = workspace_runtime(&workspace);
    let thread = echo_thread(&runtime).await;
    let other = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!tool write {"path":"waiting.txt","content":"hi"}"#),
        )
        .await
        .unwrap();
    let other_turn = runtime
        .start_turn(
            other.id,
            turn_request(r#"!tool write {"path":"other.txt","content":"hi"}"#),
        )
        .await
        .unwrap();
    wait_pending_permissions(&runtime, 2).await;
    runtime
        .set_approval_mode(
            thread.id,
            haarness_protocol::SetApprovalModeRequest {
                mode: haarness_protocol::ApprovalModeSetting::Auto,
                revision: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(workspace.join("waiting.txt").exists());
    // Another chat in Review keeps its prompt.
    let pending = runtime.pending_permissions();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].thread_id, other.id);
    assert!(
        runtime.events().subscribe(None).replay.iter().any(|event| {
            event.event_type == "approval_mode.changed"
                && event.thread_id == Some(thread.id)
                && event.payload["mode"] == "auto"
        }),
        "clients hear about the change"
    );
    runtime.interrupt(other_turn.id).await.unwrap();
    tokio::fs::remove_dir_all(workspace).await.unwrap();
}

#[tokio::test]
async fn delegated_agents_follow_the_chats_mode_and_forks_keep_their_own() {
    let workspace =
        std::env::temp_dir().join(format!("haarness-approval-child-{}", uuid::Uuid::now_v7()));
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    let (runtime, _) = p0_runtime(&workspace, false);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .set_approval_mode(
            thread.id,
            haarness_protocol::SetApprovalModeRequest {
                mode: haarness_protocol::ApprovalModeSetting::Auto,
                revision: None,
            },
        )
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request(&tasks_prompt(1, "true")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(runtime.pending_permissions().is_empty());
    let child = runtime
        .list_threads(true)
        .await
        .unwrap()
        .into_iter()
        .find(|item| item.is_subagent && item.parent_id == Some(thread.id))
        .expect("the task ran in a child chat");
    let inherited = runtime.approval_mode(child.id).await.unwrap();
    assert_eq!(inherited.mode, haarness_protocol::ApprovalMode::Auto);
    assert_eq!(inherited.source_thread_id, thread.id);
    let set = |id, mode| {
        runtime.set_approval_mode(
            id,
            haarness_protocol::SetApprovalModeRequest {
                mode,
                revision: None,
            },
        )
    };
    // The agent keeps its own Auto only until its chat is set to Review.
    set(child.id, haarness_protocol::ApprovalModeSetting::Auto)
        .await
        .unwrap();
    let mut events = runtime.events().subscribe(None).receiver;
    set(thread.id, haarness_protocol::ApprovalModeSetting::Review)
        .await
        .unwrap();
    let reviewed = runtime.approval_mode(child.id).await.unwrap();
    assert_eq!(reviewed.mode, haarness_protocol::ApprovalMode::Review);
    assert_eq!(reviewed.source_thread_id, thread.id);
    let mut notified = Vec::new();
    while let Ok(event) = events.try_recv() {
        if event.event_type == "approval_mode.changed" {
            notified.push(event.thread_id);
        }
    }
    assert!(
        notified.contains(&Some(child.id)),
        "the agent hears of the change"
    );
    // Under a Review chat an agent cannot be switched to Auto on its own.
    assert!(matches!(
        set(child.id, haarness_protocol::ApprovalModeSetting::Auto).await,
        Err(haarness_core::CoreError::Conflict(_))
    ));
    let fork = runtime.fork_thread(thread.id, None, None).await.unwrap();
    assert_eq!(
        runtime.approval_mode(fork.id).await.unwrap().mode,
        haarness_protocol::ApprovalMode::Review
    );
    tokio::fs::remove_dir_all(workspace).await.unwrap();
}
