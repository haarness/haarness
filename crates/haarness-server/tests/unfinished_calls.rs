//! Calls a shutdown cut off, or a failed turn left, end with a result, and
//! a tool that never stops does not keep its chat: the chat goes on.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::{StreamExt, stream};
use haarness_core::{
    AgentRuntime, CoreError, CoreResult, EventBus, ModelProvider, PermissionDecision,
    PermissionRule, ProviderEvent, ProviderMessage, ProviderRequest, ProviderStream, RuntimeConfig,
    StaticProviderRegistry, ThreadRepository, Tool, ToolContext, ToolDefinition, ToolOutput,
    ToolRegistry,
};
use haarness_protocol::{
    CreateThreadRequest, Item, ItemData, ItemId, ModelInfo, ProviderInfo, RequestId,
    StartTurnRequest, Thread, ThreadId, Turn, TurnId, TurnStatus,
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
/// it finishes, or, when `stop`, stops the turn as Stop does once they are
/// in, before any runs. Its second asks for `then`, numbering the ids from
/// zero again, as some providers do. Later requests answer.
#[derive(Clone, Default)]
struct Batch {
    calls: Vec<&'static str>,
    then: Vec<&'static str>,
    cut: bool,
    stop: bool,
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
        cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        let round = if request.tools.is_empty() {
            0
        } else {
            let mut requests = self.requests.lock().unwrap();
            *requests += 1;
            *requests
        };
        let first = round == 1;
        let calls = match round {
            1 => self.calls.as_slice(),
            2 => self.then.as_slice(),
            _ => &[],
        };
        let mut events = if calls.is_empty() {
            vec![Ok(ProviderEvent::TextDelta("done".into()))]
        } else {
            calls
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
        };
        if !(first && self.cut) {
            events.push(Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }));
        }
        // Read to its end, the response stops its turn: after its calls
        // are in, before the first of them runs.
        let stop = (first && self.stop).then_some(cancel);
        Ok(Box::pin(stream::iter(events).chain(stream::poll_fn(
            move |_| {
                if let Some(cancel) = &stop {
                    cancel.cancel();
                }
                std::task::Poll::Ready(None)
            },
        ))))
    }
}

/// A runtime with `patient`, `stubborn` (let go at once), and the reads
/// `glance` (at once) and `wait` (held by `held`).
fn batch_runtime(provider: Batch, patient: Arc<Patient>, tools: &BatchTools) -> AgentRuntime {
    batch_runtime_on(
        Arc::new(SqliteStore::in_memory().unwrap()),
        provider,
        patient,
        tools,
        Vec::new(),
    )
}

/// A batch runtime over `store`, with `more` tools besides.
fn batch_runtime_on(
    store: Arc<dyn ThreadRepository>,
    provider: Batch,
    patient: Arc<Patient>,
    tools: &BatchTools,
    more: Vec<Arc<dyn Tool>>,
) -> AgentRuntime {
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    let stubborn = Arc::new(Stubborn {
        started: Arc::new(tokio::sync::Semaphore::new(0)),
        release: Arc::new(tokio::sync::Semaphore::new(1)),
    });
    let mut all = vec![
        patient as Arc<dyn Tool>,
        stubborn,
        tools.glance.clone(),
        tools.wait.clone(),
    ];
    all.extend(more);
    for tool in &all {
        config.base_rules.push(PermissionRule {
            pattern: format!("{}:*", tool.definition().name),
            decision: PermissionDecision::Allow,
        });
    }
    AgentRuntime::with_config(
        store,
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider) as Arc<dyn ModelProvider>
        ])),
        Arc::new(Calls(all)),
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
            ..Batch::default()
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
                ..Batch::default()
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
            ..Batch::default()
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

/// Stop that comes once a round's calls are in, before any runs, leaves
/// each of them marked as not run: the delegation, started apart from the
/// others before the first runs, as well as the read after it.
#[tokio::test]
async fn calls_stop_reached_before_any_ran_are_marked_not_run() {
    let tools = BatchTools::default();
    let runtime = batch_runtime(
        Batch {
            calls: vec!["task", "glance"],
            stop: true,
            ..Batch::default()
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
    settled(&runtime, turn.id).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while runtime.active_turn(thread).await.unwrap().is_some()
            || not_run(&runtime, thread).await.len() < 2
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("both calls get a result");
    assert_eq!(
        not_run(&runtime, thread).await,
        [("call-0".to_owned(), true), ("call-1".to_owned(), true)],
        "Stop came before either began"
    );
    assert_eq!(
        tools.glance.ran.available_permits(),
        0,
        "the read never ran"
    );
}

/// A call made under an id an earlier round's call used, as providers that
/// number each response's calls afresh do, has not begun until it does: a
/// shutdown the turn outlives marks it as not run.
#[tokio::test]
async fn a_call_made_again_under_its_id_has_not_begun() {
    let tools = BatchTools::default();
    let runtime = batch_runtime(
        Batch {
            calls: vec!["glance", "glance"],
            then: vec!["wait", "patient"],
            ..Batch::default()
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
    // The second round's read is running; the call after it waits.
    tokio::time::timeout(Duration::from_secs(10), tools.held.started.acquire())
        .await
        .expect("the second round's read starts")
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
    assert_eq!(
        not_run(&runtime, thread).await,
        [
            ("call-0".to_owned(), false),
            ("call-1".to_owned(), false),
            ("call-0".to_owned(), false),
            ("call-1".to_owned(), true),
        ],
        "the first round's call-1 ran; the second round's had not begun"
    );

    tools.held.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(10), async {
        while runtime.active_turn(thread).await.unwrap().is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the turn's runner ends");
}

/// A runtime with the stubborn tool and the scripted provider over `store`.
fn stubborn_runtime(
    store: Arc<SqliteStore>,
    tool: Arc<Stubborn>,
    provider: Provider,
) -> AgentRuntime {
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    config.base_rules.push(PermissionRule {
        pattern: "stubborn:*".into(),
        decision: PermissionDecision::Allow,
    });
    AgentRuntime::with_config(
        store,
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider) as Arc<dyn ModelProvider>
        ])),
        Arc::new(Registry(tool)),
        EventBus::new(256),
        std::env::temp_dir(),
        String::new(),
        config,
    )
}

/// A person's steering a turn that outlived the shutdown's wait never read
/// takes its place in the history there and then, and the turn's notice asks
/// for it again, once. After a restart the chat's next request ends with the
/// next message, not with the old steering.
#[tokio::test]
async fn steering_a_turn_the_shutdown_outlived_never_read_is_settled_with_it() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let tool = Arc::new(Stubborn {
        started: Arc::new(tokio::sync::Semaphore::new(0)),
        release: Arc::new(tokio::sync::Semaphore::new(0)),
    });
    let runtime = stubborn_runtime(store.clone(), tool.clone(), Provider::default());
    let thread = chat(&runtime).await;
    let turn = runtime
        .start_turn(thread, turn_request("wait for it"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), tool.started.acquire())
        .await
        .expect("the tool starts")
        .unwrap()
        .forget();
    let steer = haarness_protocol::QueueInputRequest {
        request_id: RequestId::new(),
        delivery: haarness_protocol::InputDelivery::Steer,
        text: "keep the public names".into(),
        ..Default::default()
    };
    let steered = runtime.queue_input(thread, steer).await.unwrap();
    let message = steered.item.expect("steered into the running turn");
    let mut events = runtime.events().subscribe(None);

    runtime
        .interrupt_active_turns("Haarness stopped")
        .await
        .unwrap();
    assert_eq!(
        runtime.turn(turn.id).await.unwrap().status,
        TurnStatus::Interrupted
    );
    let items = runtime.items(thread, None).await.unwrap();
    let markers = items
        .iter()
        .filter(|item| matches!(item.data, ItemData::QueueConsumed { item_id } if item_id == message.id))
        .count();
    assert_eq!(markers, 1, "{items:#?}");
    let notices = items
        .iter()
        .filter_map(|item| match &item.data {
            ItemData::Error { message, .. } => Some(message.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].starts_with(&format!(
            "Haarness stopped {}. Work already recorded is preserved. Tools are not replayed automatically.",
            haarness_core::UNREAD_WHILE_WORKING
        )),
        "{notices:?}"
    );
    let heard = std::iter::from_fn(|| events.receiver.try_recv().ok()).any(|event| {
        event.event_type == "item.created" && event.payload["data"]["type"] == "queue_consumed"
    });
    assert!(heard, "clients hear the steering took its place");

    // The server starts again over the same chats; nothing runs by itself,
    // and the next message's requests end with it.
    let provider = Provider::default();
    let tool = Arc::new(Stubborn {
        started: Arc::new(tokio::sync::Semaphore::new(0)),
        release: Arc::new(tokio::sync::Semaphore::new(1)),
    });
    let restarted = stubborn_runtime(store.clone(), tool, provider.clone());
    restarted.recover().await.unwrap();
    assert!(restarted.active_turn(thread).await.unwrap().is_none());
    let next = restarted
        .start_turn(thread, turn_request("next task"))
        .await
        .unwrap();
    assert_eq!(settled(&restarted, next.id).await, TurnStatus::Completed);
    let sent = provider.requests.lock().unwrap().first().cloned().unwrap();
    let said = sent
        .iter()
        .filter_map(|message| match message {
            ProviderMessage::Text { role, content } if role == "user" => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(said.last(), Some(&"next task"), "{said:?}");
    assert_eq!(
        said.iter()
            .filter(|content| content.contains("keep the public names"))
            .count(),
        1,
        "{said:?}"
    );
    let items = restarted.items(thread, None).await.unwrap();
    let markers = items
        .iter()
        .filter(|item| matches!(item.data, ItemData::QueueConsumed { item_id } if item_id == message.id))
        .count();
    assert_eq!(markers, 1, "recovery settles nothing twice");
}

/// A store that keeps everything as SQLite does, except that the first tool
/// result it is given cannot be written, as on a disk that is full.
struct FailsFirstResult {
    inner: SqliteStore,
    failed: AtomicBool,
}

#[async_trait]
impl ThreadRepository for FailsFirstResult {
    async fn create_thread(&self, request: CreateThreadRequest) -> CoreResult<Thread> {
        self.inner.create_thread(request).await
    }
    async fn list_threads(&self, include_archived: bool) -> CoreResult<Vec<Thread>> {
        self.inner.list_threads(include_archived).await
    }
    async fn get_thread(&self, id: ThreadId) -> CoreResult<Thread> {
        self.inner.get_thread(id).await
    }
    async fn update_thread(
        &self,
        id: ThreadId,
        request: haarness_protocol::UpdateThreadRequest,
    ) -> CoreResult<Thread> {
        self.inner.update_thread(id, request).await
    }
    async fn list_items(&self, id: ThreadId, after: Option<u64>) -> CoreResult<Vec<Item>> {
        self.inner.list_items(id, after).await
    }
    async fn start_turn(
        &self,
        id: ThreadId,
        request: StartTurnRequest,
    ) -> CoreResult<(Turn, Item, bool)> {
        self.inner.start_turn(id, request).await
    }
    async fn get_turn(&self, id: TurnId) -> CoreResult<Turn> {
        self.inner.get_turn(id).await
    }
    async fn append_item(
        &self,
        thread: ThreadId,
        turn: Option<TurnId>,
        data: ItemData,
    ) -> CoreResult<Item> {
        if matches!(data, ItemData::ToolResult { .. }) && !self.failed.swap(true, Ordering::SeqCst)
        {
            return Err(CoreError::Storage("database or disk is full".into()));
        }
        self.inner.append_item(thread, turn, data).await
    }
    async fn set_turn_status(
        &self,
        id: TurnId,
        status: TurnStatus,
        error: Option<String>,
    ) -> CoreResult<Turn> {
        self.inner.set_turn_status(id, status, error).await
    }
    async fn compact(
        &self,
        id: ThreadId,
        request: haarness_protocol::CompactRequest,
        expected_tail: Option<(ItemId, u64)>,
    ) -> CoreResult<Item> {
        self.inner.compact(id, request, expected_tail).await
    }
    async fn fork_thread(
        &self,
        id: ThreadId,
        title: Option<String>,
        up_to_sequence: Option<u64>,
    ) -> CoreResult<Thread> {
        self.inner.fork_thread(id, title, up_to_sequence).await
    }
    async fn set_revert_state(
        &self,
        id: ThreadId,
        state: Option<(u64, String, Option<String>)>,
    ) -> CoreResult<()> {
        self.inner.set_revert_state(id, state).await
    }
    async fn soft_delete_items_after(&self, id: ThreadId, sequence: u64) -> CoreResult<u64> {
        self.inner.soft_delete_items_after(id, sequence).await
    }
    async fn discard_item(&self, id: ThreadId, item: ItemId) -> CoreResult<()> {
        self.inner.discard_item(id, item).await
    }
    async fn save_permission_rule(
        &self,
        workspace: &str,
        rule: &haarness_core::PermissionRule,
    ) -> CoreResult<()> {
        self.inner.save_permission_rule(workspace, rule).await
    }
    async fn load_permission_rules(
        &self,
        workspace: &str,
    ) -> CoreResult<Vec<haarness_core::PermissionRule>> {
        self.inner.load_permission_rules(workspace).await
    }
    async fn recover_interrupted(&self) -> CoreResult<u64> {
        self.inner.recover_interrupted().await
    }
    async fn thread_extras(
        &self,
        id: ThreadId,
    ) -> CoreResult<(Option<String>, Option<haarness_protocol::LoopConfig>)> {
        self.inner.thread_extras(id).await
    }
    async fn set_goal(&self, id: ThreadId, goal: Option<String>) -> CoreResult<()> {
        self.inner.set_goal(id, goal).await
    }
    async fn goal_state(&self, id: ThreadId) -> CoreResult<Option<haarness_protocol::GoalState>> {
        self.inner.goal_state(id).await
    }
    async fn save_goal_state(
        &self,
        id: ThreadId,
        state: Option<haarness_protocol::GoalState>,
    ) -> CoreResult<()> {
        self.inner.save_goal_state(id, state).await
    }
    async fn set_loop(
        &self,
        id: ThreadId,
        config: Option<haarness_protocol::LoopConfig>,
    ) -> CoreResult<()> {
        self.inner.set_loop(id, config).await
    }
}

/// Each tool result stored in `thread`: its call id, whether it says the
/// call did not finish, and whether it says the call never ran.
async fn results(runtime: &AgentRuntime, thread: ThreadId) -> Vec<(String, bool, bool)> {
    runtime
        .items(thread, None)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item.data {
            ItemData::ToolResult {
                call_id, metadata, ..
            } => Some((
                call_id,
                metadata["unfinished"] == true,
                haarness_core::not_run(&metadata),
            )),
            _ => None,
        })
        .collect()
}

/// Waits until the chat's turn has wound down and takes a message again.
async fn free(runtime: &AgentRuntime, thread: ThreadId, within: Duration) {
    tokio::time::timeout(within, async {
        while runtime.active_turn(thread).await.unwrap().is_some() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the turn's runner ends and frees the chat");
}

/// A turn that fails once its calls are stored (the first result could not
/// be written) answers each of them as a shutdown would, before its notice:
/// the call that ran did not finish, the one after it never began. Without
/// a result a client draws a call as running for good. The chat's next
/// message is answered.
#[tokio::test]
async fn a_turn_that_fails_after_its_calls_were_stored_ends_them() {
    let tools = BatchTools::default();
    let runtime = batch_runtime_on(
        Arc::new(FailsFirstResult {
            inner: SqliteStore::in_memory().unwrap(),
            failed: AtomicBool::new(false),
        }),
        Batch {
            calls: vec!["stubborn", "glance"],
            ..Batch::default()
        },
        Arc::new(Patient {
            started: Arc::new(tokio::sync::Semaphore::new(0)),
        }),
        &tools,
        Vec::new(),
    );
    let thread = chat(&runtime).await;
    let turn = runtime
        .start_turn(thread, turn_request("go"))
        .await
        .unwrap();
    assert_eq!(settled(&runtime, turn.id).await, TurnStatus::Failed);
    free(&runtime, thread, Duration::from_secs(10)).await;
    assert_eq!(
        results(&runtime, thread).await,
        [
            ("call-0".to_owned(), true, false),
            ("call-1".to_owned(), true, true),
        ],
        "the call that ran did not finish; the next one never began"
    );
    let items = runtime.items(thread, None).await.unwrap();
    let last_result = items
        .iter()
        .rposition(|item| matches!(item.data, ItemData::ToolResult { .. }))
        .unwrap();
    let notice = items
        .iter()
        .position(|item| matches!(item.data, ItemData::Error { .. }))
        .expect("the turn's notice");
    assert!(last_result < notice, "{items:#?}");
    assert_eq!(
        tools.glance.ran.available_permits(),
        0,
        "the read never ran"
    );

    let next = runtime
        .start_turn(thread, turn_request("and now?"))
        .await
        .unwrap();
    assert_eq!(settled(&runtime, next.id).await, TurnStatus::Completed);
}

/// Asks for `stubborn` and, while its response still streams, stops the
/// turn as Stop does: the call is stored and never runs. Its later requests
/// answer.
#[derive(Clone, Default)]
struct StopsMidAnswer {
    requests: Arc<Mutex<usize>>,
}

#[async_trait]
impl ModelProvider for StopsMidAnswer {
    fn id(&self) -> &str {
        "scripted"
    }

    fn info(&self) -> ProviderInfo {
        Provider::default().info()
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        let first = !request.tools.is_empty() && {
            let mut requests = self.requests.lock().unwrap();
            *requests += 1;
            *requests == 1
        };
        if !first {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::TextDelta("done".into())),
                Ok(ProviderEvent::Finished {
                    reason: "done".into(),
                }),
            ])));
        }
        let call = ProviderEvent::ToolCall {
            call_id: "call-0".into(),
            name: "stubborn".into(),
            arguments: json!({}),
            provider_metadata: Value::Null,
        };
        Ok(Box::pin(stream::iter(vec![Ok(call)]).chain(
            stream::poll_fn(move |_| {
                cancel.cancel();
                std::task::Poll::Pending
            }),
        )))
    }
}

/// Stop while the model is still answering, after it made a call, ends that
/// call as one that never ran, rather than leaving it without a result.
#[tokio::test]
async fn a_call_made_before_a_stop_mid_answer_is_ended() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let tool = Arc::new(Stubborn {
        started: Arc::new(tokio::sync::Semaphore::new(0)),
        release: Arc::new(tokio::sync::Semaphore::new(1)),
    });
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    config.base_rules.push(PermissionRule {
        pattern: "stubborn:*".into(),
        decision: PermissionDecision::Allow,
    });
    let runtime = AgentRuntime::with_config(
        store,
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(StopsMidAnswer::default()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(Registry(tool.clone())),
        EventBus::new(256),
        std::env::temp_dir(),
        String::new(),
        config,
    );
    let thread = chat(&runtime).await;
    let turn = runtime
        .start_turn(thread, turn_request("go"))
        .await
        .unwrap();
    assert_eq!(settled(&runtime, turn.id).await, TurnStatus::Interrupted);
    free(&runtime, thread, Duration::from_secs(10)).await;
    assert_eq!(
        results(&runtime, thread).await,
        [("call-0".to_owned(), true, true)]
    );
    assert_eq!(tool.started.available_permits(), 0, "the call never ran");
    let next = runtime
        .start_turn(thread, turn_request("and now?"))
        .await
        .unwrap();
    assert_eq!(settled(&runtime, next.id).await, TurnStatus::Completed);
}

/// A read that never looks at Stop no longer keeps its chat: once it has
/// gone on past the time a stopped command gets, its call ends as stopped
/// (it ran: not marked as never run, nor as cut off by a restart) and the
/// chat takes its next message.
#[tokio::test]
async fn stop_frees_a_chat_whose_tool_never_stops() {
    let tools = BatchTools::default();
    let runtime = batch_runtime(
        Batch {
            calls: vec!["wait"],
            ..Batch::default()
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
    tokio::time::timeout(Duration::from_secs(10), tools.held.started.acquire())
        .await
        .expect("the read starts")
        .unwrap()
        .forget();
    let stopped = std::time::Instant::now();
    runtime.interrupt(turn.id).await.unwrap();
    free(&runtime, thread, Duration::from_secs(20)).await;
    assert!(
        stopped.elapsed() >= Duration::from_secs(5),
        "the tool had a command's time to end first: {:?}",
        stopped.elapsed()
    );
    assert_eq!(
        results(&runtime, thread).await,
        [("call-0".to_owned(), false, false)]
    );
    let items = runtime.items(thread, None).await.unwrap();
    assert!(
        items.iter().any(|item| matches!(
            &item.data,
            ItemData::ToolResult { call_id, is_error: true, output, .. }
                if call_id == "call-0" && output != "released"
        )),
        "{items:#?}"
    );

    let next = runtime
        .start_turn(thread, turn_request("and now?"))
        .await
        .unwrap();
    assert_eq!(settled(&runtime, next.id).await, TurnStatus::Completed);
}

/// Takes its time once its turn is stopped, as a command does while its
/// processes end, then answers in words of its own.
struct Tidy {
    started: Arc<tokio::sync::Semaphore>,
}

#[async_trait]
impl Tool for Tidy {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "tidy".into(),
            description: "Tidy up slowly after Stop".into(),
            input_schema: json!({"type": "object", "additionalProperties": false}),
        }
    }

    fn side_effectful(&self) -> bool {
        false
    }

    async fn execute(&self, _arguments: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        self.started.add_permits(1);
        context.cancel.cancelled().await;
        tokio::time::sleep(Duration::from_secs(5)).await;
        Err(CoreError::Tool("tidied up after the stop".into()))
    }
}

/// A tool that ends its work on Stop, taking as long as a stopped command
/// may, answers its call itself: Stop does not cut it short.
#[tokio::test]
async fn a_tool_that_tidies_up_after_stop_answers_its_call_itself() {
    let tidy = Arc::new(Tidy {
        started: Arc::new(tokio::sync::Semaphore::new(0)),
    });
    let runtime = batch_runtime_on(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Batch {
            calls: vec!["tidy"],
            ..Batch::default()
        },
        Arc::new(Patient {
            started: Arc::new(tokio::sync::Semaphore::new(0)),
        }),
        &BatchTools::default(),
        vec![tidy.clone() as Arc<dyn Tool>],
    );
    let thread = chat(&runtime).await;
    let turn = runtime
        .start_turn(thread, turn_request("go"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), tidy.started.acquire())
        .await
        .expect("the tool starts")
        .unwrap()
        .forget();
    runtime.interrupt(turn.id).await.unwrap();
    free(&runtime, thread, Duration::from_secs(20)).await;
    let items = runtime.items(thread, None).await.unwrap();
    assert!(
        items.iter().any(|item| matches!(
            &item.data,
            ItemData::ToolResult { call_id, output, .. }
                if call_id == "call-0" && output.contains("tidied up after the stop")
        )),
        "{items:#?}"
    );
}

/// A shutdown, not the wait for a stopped tool, ends a turn it outlives:
/// the turn's runner adds nothing after it, so the chat keeps one result for
/// the call and one notice, however long the server takes to go.
#[tokio::test]
async fn a_shutdown_alone_ends_a_turn_whose_tool_never_stops() {
    let tool = Arc::new(Stubborn {
        started: Arc::new(tokio::sync::Semaphore::new(0)),
        release: Arc::new(tokio::sync::Semaphore::new(0)),
    });
    let runtime = stubborn_runtime(
        Arc::new(SqliteStore::in_memory().unwrap()),
        tool.clone(),
        Provider::default(),
    );
    let thread = chat(&runtime).await;
    runtime
        .start_turn(thread, turn_request("wait for it"))
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
    // Past the time a stopped tool is given, with the tool still at work.
    tokio::time::sleep(Duration::from_millis(7_500)).await;
    let items = runtime.items(thread, None).await.unwrap();
    assert_eq!(
        results(&runtime, thread).await,
        [("call-1".to_owned(), true, false)],
        "{items:#?}"
    );
    let notices = items
        .iter()
        .filter(|item| matches!(item.data, ItemData::Error { .. }))
        .count();
    assert_eq!(notices, 1, "{items:#?}");
}
