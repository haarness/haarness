//! Calls a shutdown cut off end with a result, so the chat goes on.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use futures::stream;
use haarness_core::{
    AgentRuntime, CoreResult, EventBus, ModelProvider, PermissionDecision, PermissionRule,
    ProviderEvent, ProviderMessage, ProviderRequest, ProviderStream, RuntimeConfig,
    StaticProviderRegistry, Tool, ToolContext, ToolDefinition, ToolOutput, ToolRegistry,
};
use haarness_protocol::{
    CreateThreadRequest, ItemData, ModelInfo, ProviderInfo, RequestId, StartTurnRequest, TurnStatus,
};
use haarness_store::SqliteStore;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// Runs until it is let go, whatever Stop says: a tool that outlives the
/// shutdown's wait.
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

/// Calls the stubborn tool once, then answers; records the messages of the
/// chat's own requests (a title request has no tools).
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
            let first = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(request.messages.clone());
                requests.len() == 1
            };
            if first {
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

fn turn_request(text: &str) -> StartTurnRequest {
    StartTurnRequest {
        request_id: RequestId::new(),
        text: text.into(),
        ..Default::default()
    }
}

/// A turn whose tool outlives the shutdown's wait is marked interrupted,
/// which startup recovery leaves alone: its call gets its result there and
/// then, before the turn's notice, and clients can tell it from a failure.
/// The tool's own result, should it come after all, is not sent a second
/// time.
#[tokio::test]
async fn a_call_a_shutdown_outlived_ends_with_a_result() {
    let tool = Arc::new(Stubborn {
        started: Arc::new(tokio::sync::Semaphore::new(0)),
        release: Arc::new(tokio::sync::Semaphore::new(0)),
    });
    let provider = Provider::default();
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    config.base_rules.push(PermissionRule {
        pattern: "stubborn:*".into(),
        decision: PermissionDecision::Allow,
    });
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(Registry(tool.clone())),
        EventBus::new(256),
        std::env::temp_dir(),
        String::new(),
        config,
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("scripted".into()),
            model: Some("scripted-1".into()),
            ..CreateThreadRequest::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request("wait for it"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), tool.started.acquire())
        .await
        .expect("the tool starts")
        .unwrap()
        .forget();

    runtime
        .interrupt_active_turns("Haarness stopped")
        .await
        .unwrap();
    assert_eq!(
        runtime.turn(turn.id).await.unwrap().status,
        TurnStatus::Interrupted
    );
    let items = runtime.items(thread.id, None).await.unwrap();
    let ended = items
        .iter()
        .position(|item| {
            matches!(&item.data, ItemData::ToolResult { call_id, is_error: true, output, metadata, .. }
                if call_id == "call-1" && output == haarness_core::UNFINISHED_CALL_RESULT
                    && metadata["unfinished"] == true)
        })
        .unwrap_or_else(|| panic!("the call ends: {items:?}"));
    let notice = items
        .iter()
        .position(|item| matches!(&item.data, ItemData::Error { .. }))
        .expect("the turn's notice");
    assert!(ended < notice, "{items:?}");

    // The tool returns after all; its runner records its own result.
    tool.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(10), async {
        while runtime.active_turn(thread.id).await.unwrap().is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the stubborn turn ends");
    let next = runtime
        .start_turn(thread.id, turn_request("and now?"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while matches!(
            runtime.turn(next.id).await.unwrap().status,
            TurnStatus::Queued | TurnStatus::Running
        ) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the next turn ends");
    assert_eq!(
        runtime.turn(next.id).await.unwrap().status,
        TurnStatus::Completed
    );
    let sent = provider.requests.lock().unwrap().last().cloned().unwrap();
    let results = sent
        .iter()
        .filter(|message| {
            matches!(message, ProviderMessage::ToolResult { call_id, .. } if call_id == "call-1")
        })
        .count();
    assert_eq!(results, 1, "{sent:?}");
}

/// Waits for its turn's Stop, as a well-behaved tool does.
struct Patient {
    started: Arc<tokio::sync::Semaphore>,
}

#[async_trait]
impl Tool for Patient {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "patient".into(),
            description: "Wait for Stop".into(),
            input_schema: json!({"type": "object", "additionalProperties": false}),
        }
    }

    fn side_effectful(&self) -> bool {
        true
    }

    async fn execute(&self, _arguments: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        self.started.add_permits(1);
        context.cancel.cancelled().await;
        Err(haarness_core::CoreError::Cancelled)
    }
}

/// A read, which runs beside the reads next to it: at once, or, `held`,
/// as its stubborn tool does. `ran` counts its starts.
struct Read {
    name: &'static str,
    held: Option<Arc<Stubborn>>,
    ran: Arc<tokio::sync::Semaphore>,
}

impl Read {
    fn new(name: &'static str, held: Option<Arc<Stubborn>>) -> Arc<Self> {
        Arc::new(Self {
            name,
            held,
            ran: Arc::new(tokio::sync::Semaphore::new(0)),
        })
    }
}

#[async_trait]
impl Tool for Read {
    fn parallel_safe(&self, _arguments: &Value) -> bool {
        true
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.into(),
            description: "Read beside other reads".into(),
            input_schema: json!({"type": "object", "additionalProperties": false}),
        }
    }

    fn side_effectful(&self) -> bool {
        false
    }

    async fn execute(&self, arguments: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        self.ran.add_permits(1);
        match &self.held {
            Some(tool) => tool.execute(arguments, context).await,
            None => Ok(ToolOutput {
                text: "read".into(),
                metadata: Value::Null,
            }),
        }
    }
}

struct Calls(Vec<Arc<dyn Tool>>);

impl ToolRegistry for Calls {
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

/// Its first request asks for `calls` and, when `cut`, breaks off before
/// it finishes; later requests answer.
#[derive(Clone)]
struct Batch {
    calls: Vec<&'static str>,
    cut: bool,
    requests: Arc<Mutex<usize>>,
}

#[async_trait]
impl ModelProvider for Batch {
    fn id(&self) -> &str {
        "scripted"
    }

    fn info(&self) -> ProviderInfo {
        Provider::default().info()
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        let first = !request.tools.is_empty() && {
            let mut requests = self.requests.lock().unwrap();
            *requests += 1;
            *requests == 1
        };
        let mut events = if first {
            self.calls
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    Ok(ProviderEvent::ToolCall {
                        call_id: format!("call-{index}"),
                        name: (*name).into(),
                        arguments: json!({}),
                        provider_metadata: Value::Null,
                    })
                })
                .collect::<Vec<_>>()
        } else {
            vec![Ok(ProviderEvent::TextDelta("done".into()))]
        };
        if !(first && self.cut) {
            events.push(Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }));
        }
        Ok(Box::pin(stream::iter(events)))
    }
}

/// A runtime with `patient`, `stubborn` (let go at once), and the reads
/// `glance` (at once) and `wait` (held by `held`).
fn batch_runtime(provider: Batch, patient: Arc<Patient>, tools: &BatchTools) -> AgentRuntime {
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    for pattern in ["patient:*", "stubborn:*", "glance:*", "wait:*"] {
        config.base_rules.push(PermissionRule {
            pattern: pattern.into(),
            decision: PermissionDecision::Allow,
        });
    }
    let stubborn = Arc::new(Stubborn {
        started: Arc::new(tokio::sync::Semaphore::new(0)),
        release: Arc::new(tokio::sync::Semaphore::new(1)),
    });
    AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider) as Arc<dyn ModelProvider>
        ])),
        Arc::new(Calls(vec![
            patient as Arc<dyn Tool>,
            stubborn,
            tools.glance.clone(),
            tools.wait.clone(),
        ])),
        EventBus::new(256),
        std::env::temp_dir(),
        String::new(),
        config,
    )
}

/// The reads of a batch runtime, and the stubborn tool `wait` waits on.
struct BatchTools {
    glance: Arc<Read>,
    wait: Arc<Read>,
    held: Arc<Stubborn>,
}

impl Default for BatchTools {
    fn default() -> Self {
        let held = Arc::new(Stubborn {
            started: Arc::new(tokio::sync::Semaphore::new(0)),
            release: Arc::new(tokio::sync::Semaphore::new(0)),
        });
        Self {
            glance: Read::new("glance", None),
            wait: Read::new("wait", Some(held.clone())),
            held,
        }
    }
}

async fn chat(runtime: &AgentRuntime) -> haarness_protocol::ThreadId {
    runtime
        .create_thread(CreateThreadRequest {
            provider: Some("scripted".into()),
            model: Some("scripted-1".into()),
            ..CreateThreadRequest::default()
        })
        .await
        .unwrap()
        .id
}

/// Whether each call's stored result says it never ran, by call id.
async fn not_run(
    runtime: &AgentRuntime,
    thread: haarness_protocol::ThreadId,
) -> Vec<(String, bool)> {
    runtime
        .items(thread, None)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item.data {
            ItemData::ToolResult {
                call_id, metadata, ..
            } => Some((call_id, haarness_core::not_run(&metadata))),
            _ => None,
        })
        .collect()
}

async fn settled(runtime: &AgentRuntime, turn: haarness_protocol::TurnId) -> TurnStatus {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status = runtime.turn(turn).await.unwrap().status;
            if !matches!(status, TurnStatus::Queued | TurnStatus::Running) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the turn ends")
}

/// The results of calls that never started say so, so a rebuild of the
/// chat's loads does not count them: a round that broke off before its calls
/// ran, and the calls Stop reached before they began, one at a time or a
/// read that would have run beside others.
#[tokio::test]
async fn calls_that_never_started_are_marked_not_run() {
    let patient = Arc::new(Patient {
        started: Arc::new(tokio::sync::Semaphore::new(0)),
    });
    let cut = batch_runtime(
        Batch {
            calls: vec!["stubborn"],
            cut: true,
            requests: Arc::default(),
        },
        patient.clone(),
        &BatchTools::default(),
    );
    let thread = chat(&cut).await;
    let turn = cut.start_turn(thread, turn_request("go")).await.unwrap();
    assert_eq!(settled(&cut, turn.id).await, TurnStatus::Completed);
    assert_eq!(not_run(&cut, thread).await, [("call-0".to_owned(), true)]);

    for next in ["stubborn", "glance"] {
        let stopped = batch_runtime(
            Batch {
                calls: vec!["patient", next],
                cut: false,
                requests: Arc::default(),
            },
            patient.clone(),
            &BatchTools::default(),
        );
        let thread = chat(&stopped).await;
        let turn = stopped
            .start_turn(thread, turn_request("go"))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), patient.started.acquire())
            .await
            .expect("the first call starts")
            .unwrap()
            .forget();
        stopped.interrupt(turn.id).await.unwrap();
        settled(&stopped, turn.id).await;
        // The runner records the results as it winds down.
        tokio::time::timeout(Duration::from_secs(10), async {
            while stopped.active_turn(thread).await.unwrap().is_some()
                || not_run(&stopped, thread).await.len() < 2
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("both calls get a result");
        assert_eq!(
            not_run(&stopped, thread).await,
            [("call-0".to_owned(), false), ("call-1".to_owned(), true)],
            "the running call was stopped; the next one ({next}) never began"
        );
    }
}

/// A shutdown the turn outlives marks as not run only the calls that had
/// not begun: a read that ran beside the one still running began, even
/// though its result, which follows that one's, was not stored yet.
#[tokio::test]
async fn reads_a_shutdown_outlived_count_as_begun() {
    let tools = BatchTools::default();
    let runtime = batch_runtime(
        Batch {
            calls: vec!["wait", "glance", "patient"],
            cut: false,
            requests: Arc::default(),
        },
        Arc::new(Patient {
            started: Arc::new(tokio::sync::Semaphore::new(0)),
        }),
        &tools,
    );
    let thread = chat(&runtime).await;
    let turn = runtime
        .start_turn(thread, turn_request("go"))
        .await
        .unwrap();
    for (read, ran) in [("wait", &tools.held.started), ("glance", &tools.glance.ran)] {
        tokio::time::timeout(Duration::from_secs(10), ran.acquire())
            .await
            .unwrap_or_else(|_| panic!("{read} starts"))
            .unwrap()
            .forget();
    }

    runtime
        .interrupt_active_turns("Haarness stopped")
        .await
        .unwrap();
    assert_eq!(
        runtime.turn(turn.id).await.unwrap().status,
        TurnStatus::Interrupted
    );
    assert_eq!(
        not_run(&runtime, thread).await,
        [
            ("call-0".to_owned(), false),
            ("call-1".to_owned(), false),
            ("call-2".to_owned(), true),
        ],
        "both reads began; the call after them never did"
    );

    // Let the read go, so the turn's runner winds down.
    tools.held.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(10), async {
        while runtime.active_turn(thread).await.unwrap().is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the turn's runner ends");
}
