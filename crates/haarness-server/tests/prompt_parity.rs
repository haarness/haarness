use async_trait::async_trait;
use futures::stream;
use haarness_core::{
    AgentRuntime, CoreResult, EventBus, HookDecision, HookPort, LazyToolSettings, ModelProvider,
    PermissionDecision, PermissionRule, ProviderEvent, ProviderMessage, ProviderRequest,
    ProviderStream, RuntimeConfig, StaticProviderRegistry, Tool, ToolContext, ToolDefinition,
    ToolOutput, ToolRegistry,
};
use haarness_protocol::{
    BtwRequest, CreateThreadRequest, LazyToolMode, ProviderInfo, RequestId, StartTurnRequest,
    ThreadId, TurnStatus,
};
use haarness_store::SqliteStore;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Probe {
    requests: Mutex<Vec<ProviderRequest>>,
    side_calls: AtomicUsize,
    repeat: bool,
    refuse: bool,
    hold: bool,
}
fn side(request: &ProviderRequest) -> bool {
    request.messages.iter().any(|message| matches!(message, ProviderMessage::Text { content, .. } if content.contains("<btw-side-question>")))
}
#[async_trait]
impl ModelProvider for Probe {
    fn id(&self) -> &str {
        "parity"
    }
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id().into(),
            name: "Parity fixture".into(),
            authenticated: true,
            models: Vec::new(),
        }
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        _: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        let side = side(&request);
        let has_result = request
            .messages
            .iter()
            .any(|message| matches!(message, ProviderMessage::ToolResult { .. }));
        let event = if side
            && self.refuse
            && (self.repeat || self.side_calls.fetch_add(1, Ordering::SeqCst) == 0)
        {
            ProviderEvent::ToolCall {
                call_id: "refused-write".into(),
                name: "write".into(),
                arguments: json!({}),
                provider_metadata: Value::Null,
            }
        } else if !side && self.hold && !has_result {
            ProviderEvent::ToolCall {
                call_id: "held-call".into(),
                name: "hold".into(),
                arguments: json!({"marker":"unfinished-call"}),
                provider_metadata: Value::Null,
            }
        } else {
            ProviderEvent::TextDelta(if side { "Side answer." } else { "Main answer." }.into())
        };
        self.requests.lock().unwrap().push(request);
        Ok(Box::pin(stream::iter(vec![
            Ok(event),
            Ok(ProviderEvent::Usage {
                input_tokens: 7,
                output_tokens: 3,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }),
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }),
        ])))
    }
}
#[derive(Default)]
struct Hooks {
    submits: AtomicUsize,
    prepares: AtomicUsize,
}
#[async_trait]
impl HookPort for Hooks {
    async fn prepare_request(&self, _: ThreadId, request: &mut ProviderRequest) -> CoreResult<()> {
        let count = self.prepares.fetch_add(1, Ordering::SeqCst);
        write!(request.system, "\nPrepared hook marker {count}").unwrap();
        request.reasoning_effort = Some("high".into());
        request.messages.insert(
            0,
            ProviderMessage::Text {
                role: "user".into(),
                content: "Prepared history marker".into(),
            },
        );
        Ok(())
    }
    async fn user_prompt_submit(&self, _: ThreadId, _: &str) -> HookDecision {
        self.submits.fetch_add(1, Ordering::SeqCst);
        HookDecision {
            context: Some("Submit hook marker".into()),
            ..Default::default()
        }
    }
    async fn pre_tool_use(&self, _: ThreadId, _: &str, _: &Value) -> HookDecision {
        HookDecision::default()
    }
    async fn post_tool_use(
        &self,
        _: ThreadId,
        _: &str,
        _: &Value,
        _: &str,
        _: bool,
    ) -> Option<String> {
        None
    }
    async fn permission_request(
        &self,
        _: ThreadId,
        _: &str,
        _: &[String],
        _: &Value,
    ) -> HookDecision {
        HookDecision::default()
    }
    async fn stop(&self, _: ThreadId) {}
}
struct HeldTool {
    entered: CancellationToken,
    release: CancellationToken,
    writes: Arc<AtomicUsize>,
    name: &'static str,
}
#[async_trait]
impl Tool for HeldTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.into(),
            description: "Fixture tool".into(),
            input_schema: json!({"type":"object"}),
        }
    }
    fn side_effectful(&self) -> bool {
        false
    }
    async fn execute(&self, _: Value, _: ToolContext) -> CoreResult<ToolOutput> {
        if self.name == "write" {
            self.writes.fetch_add(1, Ordering::SeqCst);
        } else {
            self.entered.cancel();
            self.release.cancelled().await;
        }
        Ok(ToolOutput {
            text: "Held tool completed".into(),
            metadata: Value::Null,
        })
    }
}
struct Registry(Vec<Arc<dyn Tool>>);
impl ToolRegistry for Registry {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.0
            .iter()
            .find(|tool| tool.definition().name == name)
            .cloned()
    }
    fn list(&self) -> Vec<ToolDefinition> {
        self.0.iter().map(|tool| tool.definition()).collect()
    }
}
fn request() -> StartTurnRequest {
    StartTurnRequest {
        request_id: RequestId::new(),
        text: "Use the held tool, then answer.".into(),
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
async fn done(runtime: &AgentRuntime, turn: haarness_protocol::TurnId) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let turn = runtime.turn(turn).await.unwrap();
            if !matches!(turn.status, TurnStatus::Running | TurnStatus::Queued) {
                assert_eq!(turn.status, TurnStatus::Completed, "{:?}", turn.error);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn side_question_reuses_prepared_context_and_settled_history_without_mutation() {
    let provider = Arc::new(Probe {
        hold: true,
        refuse: true,
        ..Default::default()
    });
    let hooks = Arc::new(Hooks::default());
    let entered = CancellationToken::new();
    let release = CancellationToken::new();
    let writes = Arc::new(AtomicUsize::new(0));
    let tools = Arc::new(Registry(
        ["hold", "write"]
            .into_iter()
            .map(|name| {
                Arc::new(HeldTool {
                    entered: entered.clone(),
                    release: release.clone(),
                    writes: writes.clone(),
                    name,
                }) as Arc<dyn Tool>
            })
            .collect(),
    ));
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            provider.clone() as Arc<dyn ModelProvider>
        ])),
        tools,
        EventBus::new(256),
        PathBuf::from("."),
        "Project instructions marker".into(),
        RuntimeConfig {
            base_rules: vec![PermissionRule {
                pattern: "*".into(),
                decision: PermissionDecision::Allow,
            }],
            lazy_tools: LazyToolSettings {
                mode: LazyToolMode::Off,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .with_hooks(hooks.clone());
    runtime.set_memory_context("Memory marker".into());
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Prompt parity".into()),
            provider: Some("parity".into()),
            model: Some("fixture".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime.start_turn(thread.id, request()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), entered.cancelled())
        .await
        .unwrap();
    let before = serde_json::to_value(runtime.items(thread.id, None).await.unwrap()).unwrap();
    let answer = runtime
        .btw(
            thread.id,
            BtwRequest {
                request_id: RequestId::new(),
                question: "Explain the current plan".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(answer.answer, "Side answer.");
    assert_eq!((answer.input_tokens, answer.output_tokens), (14, 6));
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    assert_eq!(
        before,
        serde_json::to_value(runtime.items(thread.id, None).await.unwrap()).unwrap()
    );
    assert!(runtime.active_turn(thread.id).await.unwrap().is_some());
    assert_eq!(hooks.submits.load(Ordering::SeqCst), 1);
    assert_eq!(hooks.prepares.load(Ordering::SeqCst), 1);
    {
        let requests = provider.requests.lock().unwrap();
        let main = &requests[0];
        let side = &requests[1];
        for marker in [
            "Project instructions marker",
            "Memory marker",
            "Submit hook marker",
            "Prepared hook marker 0",
        ] {
            assert!(side.system.contains(marker));
        }
        assert_eq!(main.system, side.system);
        assert_eq!(main.tools, side.tools);
        assert_eq!(main.reasoning_effort, side.reasoning_effort);
        assert_eq!(main.messages, side.messages[..side.messages.len() - 1]);
        assert!(
            !serde_json::to_string(&side.messages)
                .unwrap()
                .contains("unfinished-call")
        );
        assert!(
            requests[2].messages.iter().any(|message| matches!(
                message,
                ProviderMessage::ToolResult { is_error: true, .. }
            ))
        );
    }
    release.cancel();
    done(&runtime, turn.id).await;
    let before = serde_json::to_value(runtime.items(thread.id, None).await.unwrap()).unwrap();
    runtime
        .btw(
            thread.id,
            BtwRequest {
                request_id: RequestId::new(),
                question: "Explain the result".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        before,
        serde_json::to_value(runtime.items(thread.id, None).await.unwrap()).unwrap()
    );
    let requests = provider.requests.lock().unwrap();
    let last = requests.last().unwrap();
    assert!(last.messages.iter().any(|message| matches!(message, ProviderMessage::ToolResult { output, .. } if output == "Held tool completed")));
    assert_eq!(requests[3].system, last.system);
}

#[tokio::test]
async fn cold_side_context_and_repeated_refusals_are_bounded_and_do_not_execute() {
    let provider = Arc::new(Probe {
        refuse: true,
        repeat: true,
        ..Default::default()
    });
    let hooks = Arc::new(Hooks::default());
    let runtime = AgentRuntime::with_workspace_context(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            provider.clone() as Arc<dyn ModelProvider>
        ])),
        Arc::new(Registry(vec![])),
        EventBus::new(64),
        PathBuf::from("."),
        "Cold project marker".into(),
    )
    .with_hooks(hooks.clone());
    runtime.set_memory_context("Cold memory marker".into());
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Cold side".into()),
            provider: Some("parity".into()),
            model: Some("fixture".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        runtime
            .btw(
                thread.id,
                BtwRequest {
                    request_id: RequestId::new(),
                    question: "Explain".into()
                }
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("repeatedly")
    );
    {
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests[0].system.contains("Cold project marker"));
        assert!(requests[0].system.contains("Cold memory marker"));
    }
    assert_eq!(hooks.submits.load(Ordering::SeqCst), 0);
    assert_eq!(hooks.prepares.load(Ordering::SeqCst), 0);
    assert!(runtime.items(thread.id, None).await.unwrap().is_empty());
}
