//! A server that has begun to shut down starts no turn: what waited at a
//! closing turn's input, a queued message and a background command's end all
//! wait for the next start instead of running while the server goes away.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use futures::stream;
use haarness_core::{
    AgentRuntime, CoreError, CoreResult, EventBus, ModelProvider, PermissionDecision,
    PermissionRule, ProviderEvent, ProviderMessage, ProviderRequest, ProviderStream, RuntimeConfig,
    StaticProviderRegistry, Tool, ToolContext, ToolDefinition, ToolOutput, ToolRegistry,
};
use haarness_protocol::{
    CreateThreadRequest, InputDelivery, ItemData, ModelInfo, ProviderInfo, QueueInputRequest,
    RefusalReason, RequestId, StartTurnRequest, ThreadId, TurnStatus,
};
use haarness_store::SqliteStore;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// Runs until it is let go, whatever Stop says, so the turn calling it is
/// still closing when the shutdown has shut its input.
struct Stubborn {
    started: Arc<tokio::sync::Semaphore>,
    release: Arc<tokio::sync::Semaphore>,
}

#[async_trait]
impl Tool for Stubborn {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "stubborn".into(),
            description: "Wait until released".into(),
            input_schema: json!({"type": "object", "additionalProperties": false}),
        }
    }

    fn side_effectful(&self) -> bool {
        false
    }

    async fn execute(&self, _arguments: Value, _context: ToolContext) -> CoreResult<ToolOutput> {
        self.started.add_permits(1);
        self.release.acquire().await.unwrap().forget();
        Ok(ToolOutput {
            text: "released".into(),
            metadata: Value::Null,
        })
    }
}

struct Registry(Arc<Stubborn>);

impl ToolRegistry for Registry {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        (name == "stubborn").then(|| self.0.clone() as Arc<dyn Tool>)
    }

    fn list(&self) -> Vec<ToolDefinition> {
        vec![self.0.definition()]
    }
}

/// Calls the stubborn tool for a message saying `wait`, and answers any
/// other; records the chat's requests (a title request has no tools).
#[derive(Clone, Default)]
struct Provider {
    requests: Arc<Mutex<Vec<Vec<ProviderMessage>>>>,
}

#[async_trait]
impl ModelProvider for Provider {
    fn id(&self) -> &str {
        "scripted"
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "scripted".into(),
            name: "Scripted".into(),
            authenticated: true,
            models: vec![ModelInfo {
                id: "scripted-1".into(),
                display_name: "Scripted".into(),
                reasoning: false,
                reasoning_efforts: Vec::new(),
                images: false,
                video: false,
                tools: true,
                context_window: Some(128_000),
                max_output_tokens: Some(2_000),
                pricing: None,
            }],
        }
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        let event = if request.tools.is_empty() {
            ProviderEvent::TextDelta("Scripted chat".into())
        } else {
            self.requests.lock().unwrap().push(request.messages.clone());
            let waits = !request
                .messages
                .iter()
                .any(|message| matches!(message, ProviderMessage::ToolResult { .. }))
                && request.messages.iter().any(|message| {
                    matches!(message, ProviderMessage::Text { role, content }
                        if role == "user" && content == "wait")
                });
            if waits {
                ProviderEvent::ToolCall {
                    call_id: "call-1".into(),
                    name: "stubborn".into(),
                    arguments: json!({}),
                    provider_metadata: Value::Null,
                }
            } else {
                ProviderEvent::TextDelta("done".into())
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

struct Fixture {
    runtime: Arc<AgentRuntime>,
    tool: Arc<Stubborn>,
    thread: ThreadId,
}

async fn fixture() -> Fixture {
    let tool = Arc::new(Stubborn {
        started: Arc::new(tokio::sync::Semaphore::new(0)),
        release: Arc::new(tokio::sync::Semaphore::new(0)),
    });
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    config.base_rules.push(PermissionRule {
        pattern: "stubborn:*".into(),
        decision: PermissionDecision::Allow,
    });
    let runtime = Arc::new(AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Provider::default()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(Registry(tool.clone())),
        EventBus::new(256),
        std::env::temp_dir(),
        String::new(),
        config,
    ));
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("scripted".into()),
            model: Some("scripted-1".into()),
            ..CreateThreadRequest::default()
        })
        .await
        .unwrap()
        .id;
    Fixture {
        runtime,
        tool,
        thread,
    }
}

fn message(text: &str, delivery: InputDelivery) -> QueueInputRequest {
    QueueInputRequest {
        delivery,
        request_id: RequestId::new(),
        text: text.into(),
        ..Default::default()
    }
}

fn stopping(error: &CoreError) -> bool {
    matches!(error, CoreError::Refused(refusal) if refusal.reason == RefusalReason::ServerStopping)
}

/// A chat running the stubborn tool whose shutdown has begun: its input is
/// shut, and its turn waits for the tool.
async fn shutting_down(fixture: &Fixture) -> tokio::task::JoinHandle<CoreResult<u64>> {
    let turn = fixture
        .runtime
        .start_turn(
            fixture.thread,
            StartTurnRequest {
                request_id: RequestId::new(),
                text: "wait".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), fixture.tool.started.acquire())
        .await
        .expect("the tool starts")
        .unwrap()
        .forget();
    let runtime = fixture.runtime.clone();
    let shutdown =
        tokio::spawn(async move { runtime.interrupt_active_turns("Haarness stopped").await });
    // The shutdown has shut the turn's input; the turn waits for its tool.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        fixture.runtime.turn(turn.id).await.unwrap().status,
        TurnStatus::Running
    );
    shutdown
}

async fn person_messages(runtime: &AgentRuntime, thread: ThreadId) -> Vec<String> {
    runtime
        .items(thread, None)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item.data {
            ItemData::UserMessage {
                text,
                synthetic: false,
                ..
            } => Some(text),
            _ => None,
        })
        .collect()
}

/// A message sent as the turn's input shut waits for the turn to end; once
/// it has, the stopping server refuses to start it as a turn of its own
/// (before, it started one that ran tools while the server went away and was
/// found interrupted at the next start). Sent again under its request id
/// after a restart, it runs.
#[tokio::test]
async fn a_message_waiting_at_a_closing_turn_starts_nothing_once_the_server_stops() {
    let fixture = fixture().await;
    let shutdown = shutting_down(&fixture).await;
    let runtime = fixture.runtime.clone();
    let thread = fixture.thread;
    let late = message("late message", InputDelivery::Steer);
    let sent = late.clone();
    let waiting = tokio::spawn(async move { runtime.queue_input(thread, sent).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !waiting.is_finished(),
        "the message waits at the shut input"
    );
    fixture.tool.release.add_permits(1);
    let answer = tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .expect("the message is answered")
        .unwrap();
    assert!(
        answer.as_ref().is_err_and(stopping),
        "{:?}",
        answer.map(|response| response.turn)
    );
    shutdown.await.unwrap().unwrap();
    assert!(fixture.runtime.active_turns().await.unwrap().is_empty());
    assert_eq!(
        person_messages(&fixture.runtime, thread).await,
        ["wait"],
        "nothing of the late message is stored"
    );
    // Any other start is refused the same way while the server stops.
    let started = fixture
        .runtime
        .start_turn(
            thread,
            StartTurnRequest {
                request_id: RequestId::new(),
                text: "another".into(),
                ..Default::default()
            },
        )
        .await;
    assert!(started.as_ref().is_err_and(stopping), "{started:?}");
}

/// A message for the queue that waited as the turn's input shut stays in
/// the queue for the next start rather than starting a turn.
#[tokio::test]
async fn a_queued_message_stays_queued_while_the_server_stops() {
    let fixture = fixture().await;
    let shutdown = shutting_down(&fixture).await;
    let runtime = fixture.runtime.clone();
    let thread = fixture.thread;
    let waiting = tokio::spawn(async move {
        runtime
            .queue_input(thread, message("queued message", InputDelivery::Queue))
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    fixture.tool.release.add_permits(1);
    let answer = tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .expect("the message is answered")
        .unwrap()
        .unwrap();
    assert!(answer.queued && answer.turn.is_none(), "{answer:?}");
    shutdown.await.unwrap().unwrap();
    let queue = fixture.runtime.input_queue(thread).await.unwrap();
    assert_eq!(
        queue
            .items
            .iter()
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>(),
        ["queued message"]
    );
    assert_eq!(person_messages(&fixture.runtime, thread).await, ["wait"]);
}
