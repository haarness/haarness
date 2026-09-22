use async_trait::async_trait;
use futures::{StreamExt, stream};
use haarness_core::{
    AgentRuntime, CoreError, CoreResult, EmptyToolRegistry, EventBus, ModelProvider, ProviderEvent,
    ProviderMessage, ProviderRequest, ProviderStream, RuntimeConfig, StaticProviderRegistry,
};
use haarness_protocol::{
    CreateThreadRequest, EventEnvelope, ProviderInfo, StartTurnRequest, Thread, ThreadId, Turn,
    TurnStatus, UpdateThreadRequest,
};
use haarness_store::SqliteStore;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

struct Provider {
    requests: Mutex<Vec<(ProviderRequest, CancellationToken)>>,
    title_started: Semaphore,
    main_started: Semaphore,
    title: CancellationToken,
    main: CancellationToken,
    failures: AtomicUsize,
}

impl Provider {
    fn new(title_ready: bool, main_ready: bool) -> Arc<Self> {
        let provider = Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            title_started: Semaphore::new(0),
            main_started: Semaphore::new(0),
            title: CancellationToken::new(),
            main: CancellationToken::new(),
            failures: AtomicUsize::new(0),
        });
        if title_ready {
            provider.title.cancel();
        }
        if main_ready {
            provider.main.cancel();
        }
        provider
    }
    fn titles(&self) -> Vec<(ProviderRequest, CancellationToken)> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(request, _)| request.system == haarness_core::system_prompt_for("title"))
            .cloned()
            .collect()
    }
}

#[async_trait]
impl ModelProvider for Provider {
    fn id(&self) -> &str {
        "fixture"
    }
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "fixture".into(),
            name: "Titles".into(),
            authenticated: true,
            models: vec![],
        }
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        let title = request.system == haarness_core::system_prompt_for("title");
        let broken = request.model == "broken-small";
        self.requests
            .lock()
            .unwrap()
            .push((request, cancel.clone()));
        let gate = if title {
            self.title_started.add_permits(1);
            if broken
                || self
                    .failures
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok()
            {
                return Err(CoreError::Provider("temporary title failure".into()));
            }
            self.title.clone()
        } else {
            self.main_started.add_permits(1);
            self.main.clone()
        };
        let reply = stream::once(async move {
            tokio::select! {
                () = cancel.cancelled() => Err(CoreError::Cancelled),
                () = gate.cancelled() => Ok(ProviderEvent::TextDelta(if title {
                    "<think>Choose a concise title.</think>\n\"Security review\"\nExtra text".into()
                } else { "Main answer".into() })),
            }
        });
        Ok(Box::pin(reply.chain(stream::iter(vec![Ok(
            ProviderEvent::Finished {
                reason: "completed".into(),
            },
        )]))))
    }
}

fn runtime(provider: Arc<Provider>, small_model: Option<&str>) -> (AgentRuntime, EventBus) {
    let events = EventBus::new(128);
    (
        AgentRuntime::with_config(
            Arc::new(SqliteStore::in_memory().unwrap()),
            Arc::new(StaticProviderRegistry::new(vec![
                provider as Arc<dyn ModelProvider>,
            ])),
            Arc::new(EmptyToolRegistry),
            events.clone(),
            std::env::temp_dir(),
            String::new(),
            RuntimeConfig {
                small_model: small_model.map(str::to_owned),
                ..Default::default()
            },
        ),
        events,
    )
}

async fn thread(runtime: &AgentRuntime) -> Thread {
    runtime
        .create_thread(CreateThreadRequest {
            provider: Some("fixture".into()),
            model: Some("main".into()),
            ..Default::default()
        })
        .await
        .unwrap()
}

fn request() -> StartTurnRequest {
    StartTurnRequest {
        request_id: haarness_protocol::RequestId::new(),
        text: "Analyze the code for security issues".into(),
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

async fn started(semaphore: &Semaphore) {
    tokio::time::timeout(Duration::from_secs(3), semaphore.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
}

async fn finished(runtime: &AgentRuntime, turn: &Turn) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while runtime.turn(turn.id).await.unwrap().status == TurnStatus::Running {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        runtime.turn(turn.id).await.unwrap().status,
        TurnStatus::Completed
    );
}

async fn updated(
    receiver: &mut tokio::sync::broadcast::Receiver<EventEnvelope>,
    id: ThreadId,
) -> Thread {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = receiver.recv().await.unwrap();
            if event.event_type == "thread.updated" && event.thread_id == Some(id) {
                assert_eq!(event.root_thread_id, Some(id));
                break serde_json::from_value(event.payload).unwrap();
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn first_message_titles_while_main_response_is_still_running_and_publishes_update() {
    let provider = Provider::new(true, false);
    let (runtime, events) = runtime(provider.clone(), None);
    let thread = thread(&runtime).await;
    let mut receiver = events.subscribe(None).receiver;
    let request = request();
    let turn = runtime
        .start_turn(thread.id, request.clone())
        .await
        .unwrap();
    started(&provider.main_started).await;
    let named = updated(&mut receiver, thread.id).await;
    assert_eq!(named.title, "Security review");
    assert_eq!(
        runtime.turn(turn.id).await.unwrap().status,
        TurnStatus::Running
    );
    assert_eq!(
        runtime.items(thread.id, None).await.unwrap().len(),
        1,
        "title is metadata, not transcript text"
    );
    assert_eq!(
        runtime.start_turn(thread.id, request).await.unwrap().id,
        turn.id
    );
    let titles = provider.titles();
    assert_eq!(titles.len(), 1);
    assert!(titles[0].0.tools.is_empty());
    assert!(
        matches!(&titles[0].0.messages[..], [ProviderMessage::Text { role, content }]
        if role == "user" && content == "Analyze the code for security issues")
    );
    provider.main.cancel();
    finished(&runtime, &turn).await;
    assert_eq!(
        runtime.get_thread(thread.id).await.unwrap().title,
        "Security review"
    );
}

#[tokio::test]
async fn slow_title_never_delays_turns_or_overwrites_manual_renames() {
    let provider = Provider::new(false, true);
    let (runtime, _) = runtime(provider.clone(), None);
    let thread = thread(&runtime).await;
    let first = runtime.start_turn(thread.id, request()).await.unwrap();
    started(&provider.title_started).await;
    finished(&runtime, &first).await;
    let second = runtime.start_turn(thread.id, request()).await.unwrap();
    finished(&runtime, &second).await;
    assert_eq!(
        provider.titles().len(),
        1,
        "overlapping titles are coalesced"
    );
    runtime
        .update_thread(
            thread.id,
            UpdateThreadRequest {
                title: Some("My review".into()),
                agent: Some("review".into()),
                archived: None,
                provider: None,
                model: None,
                ultracode: None,
            },
        )
        .await
        .unwrap();
    provider.title.cancel();
    let cancel = provider.titles()[0].1.clone();
    tokio::time::timeout(Duration::from_secs(3), cancel.cancelled())
        .await
        .unwrap();
    tokio::task::yield_now().await;
    let current = runtime.get_thread(thread.id).await.unwrap();
    assert_eq!(current.title, "My review");
    assert_eq!(current.agent, "review");
}

#[tokio::test]
async fn broken_small_model_falls_back_to_session_model_without_waiting_for_main() {
    let provider = Provider::new(true, false);
    let (runtime, events) = runtime(provider.clone(), Some("fixture/broken-small"));
    let thread = thread(&runtime).await;
    let mut receiver = events.subscribe(None).receiver;
    let turn = runtime.start_turn(thread.id, request()).await.unwrap();
    assert_eq!(
        updated(&mut receiver, thread.id).await.title,
        "Security review"
    );
    assert_eq!(
        provider
            .titles()
            .iter()
            .map(|(request, _)| request.model.as_str())
            .collect::<Vec<_>>(),
        vec!["broken-small", "main"]
    );
    assert_eq!(
        runtime.turn(turn.id).await.unwrap().status,
        TurnStatus::Running
    );
    provider.main.cancel();
    finished(&runtime, &turn).await;
}

#[tokio::test]
async fn transient_title_failure_retries_without_failing_the_conversation() {
    let provider = Provider::new(true, true);
    provider.failures.store(1, Ordering::SeqCst);
    let (runtime, events) = runtime(provider.clone(), None);
    let thread = thread(&runtime).await;
    let mut receiver = events.subscribe(None).receiver;
    let turn = runtime.start_turn(thread.id, request()).await.unwrap();
    finished(&runtime, &turn).await;
    assert_eq!(
        updated(&mut receiver, thread.id).await.title,
        "Security review"
    );
    assert_eq!(provider.titles().len(), 2);
    assert!(
        !runtime
            .items(thread.id, None)
            .await
            .unwrap()
            .iter()
            .any(|item| matches!(item.data, haarness_protocol::ItemData::Error { .. }))
    );
}

#[tokio::test]
async fn named_threads_children_and_synthetic_messages_do_not_start_title_jobs() {
    let provider = Provider::new(true, true);
    let (runtime, _) = runtime(provider.clone(), None);
    let parent = thread(&runtime).await;
    for (title, parent_id, synthetic) in [
        (Some("My task"), None, false),
        (None, Some(parent.id), false),
        (None, None, true),
    ] {
        let thread = runtime
            .create_thread(CreateThreadRequest {
                title: title.map(str::to_owned),
                parent_id,
                provider: Some("fixture".into()),
                model: Some("main".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let turn = runtime
            .start_turn(
                thread.id,
                StartTurnRequest {
                    synthetic,
                    ..request()
                },
            )
            .await
            .unwrap();
        finished(&runtime, &turn).await;
        assert_eq!(
            runtime.get_thread(thread.id).await.unwrap().title,
            thread.title
        );
    }
    assert!(provider.titles().is_empty());
}

struct BlockPrompt;

#[async_trait]
impl haarness_core::HookPort for BlockPrompt {
    async fn user_prompt_submit(&self, _: ThreadId, _: &str) -> haarness_core::HookDecision {
        haarness_core::HookDecision {
            block: Some("Blocked fixture".into()),
            ..Default::default()
        }
    }
    async fn pre_tool_use(
        &self,
        _: ThreadId,
        _: &str,
        _: &serde_json::Value,
    ) -> haarness_core::HookDecision {
        haarness_core::HookDecision::default()
    }
    async fn post_tool_use(
        &self,
        _: ThreadId,
        _: &str,
        _: &serde_json::Value,
        _: &str,
        _: bool,
    ) -> Option<String> {
        None
    }
    async fn permission_request(
        &self,
        _: ThreadId,
        _: &str,
        _: &serde_json::Value,
    ) -> haarness_core::HookDecision {
        haarness_core::HookDecision::default()
    }
    async fn stop(&self, _: ThreadId) {}
}

#[tokio::test]
async fn prompt_hooks_can_block_input_before_any_title_provider_request() {
    let provider = Provider::new(true, true);
    let (runtime, _) = runtime(provider.clone(), None);
    let runtime = runtime.with_hooks(Arc::new(BlockPrompt));
    let thread = thread(&runtime).await;
    let turn = runtime.start_turn(thread.id, request()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while runtime.turn(turn.id).await.unwrap().status == TurnStatus::Running {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        runtime.turn(turn.id).await.unwrap().status,
        TurnStatus::Failed
    );
    assert!(provider.requests.lock().unwrap().is_empty());
}
