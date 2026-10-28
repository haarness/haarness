use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::stream;
use haarness_core::{
    AgentRuntime, CoreResult, EmptyToolRegistry, EventBus, ModelProvider, PermissionRule,
    ProviderEvent, ProviderMessage, ProviderRequest, ProviderStream, RuntimeConfig,
    StaticProviderRegistry, ThreadRepository,
};
use haarness_protocol::{
    CompactRequest, CreateThreadRequest, GoalActionRequest, GoalState, GoalStatus, Item, ItemData,
    ItemId, QueueInputRequest, RequestId, StartTurnRequest, Thread, ThreadId, Turn, TurnId,
    TurnStatus, UpdateThreadRequest,
};
use haarness_server::NativeToolRegistry;
use haarness_store::SqliteStore;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct CriteriaProvider;

#[async_trait]
impl ModelProvider for CriteriaProvider {
    fn id(&self) -> &str {
        "criteria"
    }

    fn info(&self) -> haarness_protocol::ProviderInfo {
        haarness_protocol::ProviderInfo {
            id: "criteria".into(),
            name: "Criteria fixture".into(),
            authenticated: true,
            models: vec![haarness_protocol::ModelInfo {
                id: "criteria-1".into(),
                display_name: "Criteria fixture".into(),
                reasoning: false,
                reasoning_efforts: Vec::new(),
                images: false,
                video: false,
                tools: true,
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
        let has_result = request
            .messages
            .iter()
            .any(|message| matches!(message, ProviderMessage::ToolResult { .. }));
        let mut events = if has_result {
            vec![Ok(ProviderEvent::TextDelta("criteria handled".into()))]
        } else {
            vec![Ok(ProviderEvent::ToolCall {
                call_id: "criteria-call".into(),
                name: "propose_goal_criteria".into(),
                arguments: serde_json::json!({
                    "criteria": ["first condition", "second condition"],
                    "prompt": "Approve these criteria?",
                    "label": "Completion criteria"
                }),
                provider_metadata: serde_json::Value::Null,
            })]
        };
        // A tiny report keeps goal accounting deterministic; without one the
        // runtime would charge its local estimate of the whole prompt.
        events.push(Ok(ProviderEvent::Usage {
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        }));
        events.push(Ok(ProviderEvent::Finished {
            reason: "done".into(),
        }));
        Ok(Box::pin(stream::iter(events)))
    }
}

#[derive(Clone)]
struct BlockingTextProvider {
    requests: Arc<std::sync::Mutex<Vec<ProviderRequest>>>,
    started: Arc<Semaphore>,
    release: Arc<Semaphore>,
}

#[derive(Clone)]
struct FailOnceProvider {
    fail_first: Arc<AtomicBool>,
    requests: Arc<std::sync::Mutex<Vec<ProviderRequest>>>,
    started: Arc<Semaphore>,
    release: Arc<Semaphore>,
}

#[derive(Clone)]
struct ChildFailureProvider {
    calls: Arc<AtomicUsize>,
    requests: Arc<std::sync::Mutex<Vec<ProviderRequest>>>,
    started: Arc<Semaphore>,
    release: Arc<Semaphore>,
}

impl Default for ChildFailureProvider {
    fn default() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            requests: Arc::new(std::sync::Mutex::new(Vec::new())),
            started: Arc::new(Semaphore::new(0)),
            release: Arc::new(Semaphore::new(0)),
        }
    }
}

#[async_trait]
impl ModelProvider for ChildFailureProvider {
    fn id(&self) -> &str {
        "child-failure"
    }

    fn info(&self) -> haarness_protocol::ProviderInfo {
        haarness_protocol::ProviderInfo {
            id: "child-failure".into(),
            name: "Child failure fixture".into(),
            authenticated: true,
            models: vec![haarness_protocol::ModelInfo {
                id: "child-failure-1".into(),
                display_name: "Child failure fixture".into(),
                reasoning: false,
                reasoning_efforts: Vec::new(),
                images: false,
                video: false,
                tools: true,
                context_window: None,
                max_output_tokens: None,
                pricing: None,
            }],
        }
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if request
            .messages
            .iter()
            .any(|message| matches!(message, ProviderMessage::ToolResult { .. }))
        {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::TextDelta(
                    "parent observed child result".into(),
                )),
                Ok(ProviderEvent::Finished {
                    reason: "done".into(),
                }),
            ])));
        }
        let last_user = request
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                ProviderMessage::Text { role, content } if role == "user" => Some(content.as_str()),
                _ => None,
            });
        if last_user.is_some_and(|content| content.contains("spawn failing child")) {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::ToolCall {
                    call_id: "child-call".into(),
                    name: "task".into(),
                    arguments: serde_json::json!({
                        "prompt": "child provider work",
                        "agent": "general"
                    }),
                    provider_metadata: serde_json::Value::Null,
                }),
                Ok(ProviderEvent::Finished {
                    reason: "tool_calls".into(),
                }),
            ])));
        }

        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request);
        self.started.add_permits(1);
        tokio::select! {
            permit = self.release.acquire() => {
                permit.expect("test semaphore stays open").forget();
            }
            () = cancel.cancelled() => return Err(haarness_core::CoreError::Cancelled),
        }
        if call < 2 {
            return Err(haarness_core::CoreError::Provider(format!(
                "forced child provider failure {}",
                call + 1
            )));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderEvent::TextDelta(
                "fresh child turn recovered".into(),
            )),
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }),
        ])))
    }
}

impl Default for FailOnceProvider {
    fn default() -> Self {
        Self {
            fail_first: Arc::new(AtomicBool::new(true)),
            requests: Arc::new(std::sync::Mutex::new(Vec::new())),
            started: Arc::new(Semaphore::new(0)),
            release: Arc::new(Semaphore::new(0)),
        }
    }
}

#[async_trait]
impl ModelProvider for FailOnceProvider {
    fn id(&self) -> &str {
        "fail-once"
    }

    fn info(&self) -> haarness_protocol::ProviderInfo {
        haarness_protocol::ProviderInfo {
            id: "fail-once".into(),
            name: "Fail-once fixture".into(),
            authenticated: true,
            models: vec![haarness_protocol::ModelInfo {
                id: "fail-once-1".into(),
                display_name: "Fail-once fixture".into(),
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
        self.requests.lock().unwrap().push(request);
        self.started.add_permits(1);
        self.release
            .acquire()
            .await
            .expect("test semaphore stays open")
            .forget();
        if self.fail_first.swap(false, Ordering::SeqCst) {
            return Err(haarness_core::CoreError::Provider(
                "forced provider failure".into(),
            ));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderEvent::TextDelta("recovered".into())),
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }),
        ])))
    }
}

impl Default for BlockingTextProvider {
    fn default() -> Self {
        Self {
            requests: Arc::new(std::sync::Mutex::new(Vec::new())),
            started: Arc::new(Semaphore::new(0)),
            release: Arc::new(Semaphore::new(0)),
        }
    }
}

#[async_trait]
impl ModelProvider for BlockingTextProvider {
    fn id(&self) -> &str {
        "blocking"
    }

    fn info(&self) -> haarness_protocol::ProviderInfo {
        haarness_protocol::ProviderInfo {
            id: "blocking".into(),
            name: "Blocking fixture".into(),
            authenticated: true,
            models: vec![haarness_protocol::ModelInfo {
                id: "blocking-1".into(),
                display_name: "Blocking fixture".into(),
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
        self.requests.lock().unwrap().push(request);
        self.started.add_permits(1);
        self.release
            .acquire()
            .await
            .expect("test semaphore stays open")
            .forget();
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderEvent::TextDelta("provider reply".into())),
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }),
        ])))
    }
}

async fn wait_provider_call(provider: &BlockingTextProvider) {
    tokio::time::timeout(Duration::from_secs(2), provider.started.acquire())
        .await
        .expect("provider call starts")
        .expect("test semaphore stays open")
        .forget();
}

async fn wait_fail_once_provider_call(provider: &FailOnceProvider) {
    tokio::time::timeout(Duration::from_secs(2), provider.started.acquire())
        .await
        .expect("provider call starts")
        .expect("test semaphore stays open")
        .forget();
}

async fn wait_child_provider_call(provider: &ChildFailureProvider) {
    tokio::time::timeout(Duration::from_secs(2), provider.started.acquire())
        .await
        .expect("child provider call starts")
        .expect("test semaphore stays open")
        .forget();
}

async fn wait_done(runtime: &AgentRuntime, turn: &Turn) -> Turn {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let current = runtime.turn(turn.id).await.unwrap();
            if !matches!(current.status, TurnStatus::Queued | TurnStatus::Running) {
                return current;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("turn completes")
}

async fn criteria_runtime() -> (Arc<SqliteStore>, AgentRuntime, Thread) {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let runtime = AgentRuntime::with_config(
        store.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(CriteriaProvider) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(std::env::temp_dir())),
        EventBus::new(64),
        std::env::temp_dir(),
        String::new(),
        RuntimeConfig {
            goal_max_turns: Some(0),
            ..Default::default()
        },
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Goal concurrency".into()),
            provider: Some("criteria".into()),
            model: Some("criteria-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .set_goal(thread.id, Some("original objective".into()))
        .await
        .unwrap();
    (store, runtime, thread)
}

async fn start_criteria_prompt(runtime: &AgentRuntime, thread: ThreadId) -> (Turn, String) {
    let turn = runtime
        .start_turn(
            thread,
            StartTurnRequest {
                request_id: RequestId::new(),
                text: "start".into(),
                attachments: Vec::new(),
                resources: Vec::new(),
                synthetic: false,
                auto_approve: false,
                agent: None,
                provider: Some("criteria".into()),
                model: Some("criteria-1".into()),
                reasoning_effort: None,
                tools: None,
                command: None,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let question = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(question) = runtime.pending_questions().first() {
                return question.question_id.clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("criteria question appears");
    (turn, question)
}

async fn answer_criteria(runtime: &AgentRuntime, question_id: &str, turn: &Turn) {
    runtime
        .reply_question(
            question_id,
            haarness_protocol::QuestionReply {
                answer: String::new(),
            },
        )
        .unwrap();
    assert_eq!(wait_done(runtime, turn).await.status, TurnStatus::Completed);
}

#[tokio::test]
async fn interrupt_cancels_before_a_provider_returns_its_stream_handle() {
    let provider = BlockingTextProvider::default();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
        std::env::temp_dir(),
        String::new(),
        RuntimeConfig::default(),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Interrupt provider connection".into()),
            provider: Some("blocking".into()),
            model: Some("blocking-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(
            thread.id,
            StartTurnRequest {
                request_id: RequestId::new(),
                text: "provider has not returned a stream yet".into(),
                attachments: Vec::new(),
                resources: Vec::new(),
                synthetic: false,
                auto_approve: false,
                agent: None,
                provider: None,
                model: None,
                reasoning_effort: None,
                tools: None,
                command: None,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    wait_provider_call(&provider).await;

    runtime.interrupt(turn.id).await.unwrap();
    tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            if runtime.active_turn(thread.id).await.unwrap().is_none() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancellation drops a blocked provider connection future");
    assert_eq!(
        runtime.turn(turn.id).await.unwrap().status,
        TurnStatus::Interrupted
    );
}

#[tokio::test]
async fn criteria_approval_does_not_resurrect_a_cleared_goal() {
    let (_store, runtime, thread) = criteria_runtime().await;
    let (turn, question) = start_criteria_prompt(&runtime, thread.id).await;
    runtime
        .goal_action(thread.id, GoalActionRequest::Clear)
        .await
        .unwrap();

    answer_criteria(&runtime, &question, &turn).await;

    assert!(runtime.goal_state(thread.id).await.unwrap().is_none());
    let stale_result = runtime
        .items(thread.id, None)
        .await
        .unwrap()
        .into_iter()
        .find_map(|item| match item.data {
            ItemData::ToolResult {
                name,
                output,
                is_error: true,
                ..
            } if name == "propose_goal_criteria" => Some(output),
            _ => None,
        })
        .expect("stale proposal returns a tool error");
    assert!(stale_result.contains("goal was cleared"));
}

#[tokio::test]
async fn criteria_approval_does_not_overwrite_a_replacement_goal() {
    let (_store, runtime, thread) = criteria_runtime().await;
    let original_id = runtime
        .goal_state(thread.id)
        .await
        .unwrap()
        .unwrap()
        .goal_id;
    let (turn, question) = start_criteria_prompt(&runtime, thread.id).await;
    runtime
        .set_goal(thread.id, Some("replacement objective".into()))
        .await
        .unwrap();
    let replacement_id = runtime
        .goal_state(thread.id)
        .await
        .unwrap()
        .unwrap()
        .goal_id;
    assert_ne!(replacement_id, original_id);

    answer_criteria(&runtime, &question, &turn).await;

    let state = runtime.goal_state(thread.id).await.unwrap().unwrap();
    assert_eq!(state.goal_id, replacement_id);
    assert_eq!(state.goal, "replacement objective");
    assert!(state.criteria.is_empty());
    let stale_result = runtime
        .items(thread.id, None)
        .await
        .unwrap()
        .into_iter()
        .find_map(|item| match item.data {
            ItemData::ToolResult {
                name,
                output,
                is_error: true,
                ..
            } if name == "propose_goal_criteria" => Some(output),
            _ => None,
        })
        .expect("stale proposal returns a tool error");
    assert!(stale_result.contains("goal was replaced"));
}

#[tokio::test]
async fn criteria_approval_does_not_resume_a_paused_goal() {
    let (_store, runtime, thread) = criteria_runtime().await;
    let (turn, question) = start_criteria_prompt(&runtime, thread.id).await;
    runtime
        .goal_action(thread.id, GoalActionRequest::Pause)
        .await
        .unwrap();

    answer_criteria(&runtime, &question, &turn).await;

    let state = runtime.goal_state(thread.id).await.unwrap().unwrap();
    assert_eq!(state.status, GoalStatus::Paused);
    assert!(state.criteria.is_empty());
}

#[tokio::test]
async fn criteria_approval_applies_to_the_latest_same_goal_snapshot() {
    let (store, runtime, thread) = criteria_runtime().await;
    let (turn, question) = start_criteria_prompt(&runtime, thread.id).await;
    let mut current = store.goal_state(thread.id).await.unwrap().unwrap();
    current.tokens_used = 321;
    current.time_used_seconds = 17;
    store
        .save_goal_state(thread.id, Some(current))
        .await
        .unwrap();

    answer_criteria(&runtime, &question, &turn).await;

    let state = runtime.goal_state(thread.id).await.unwrap().unwrap();
    // The approval applies to the latest snapshot: its 321 tokens plus the
    // two each later round reports, never the stale snapshot plus those.
    assert!(
        (321..1_000).contains(&state.tokens_used),
        "{}",
        state.tokens_used
    );
    assert!(state.time_used_seconds >= 17);
    assert_eq!(state.criteria.len(), 2);
    assert!(!state.needs_criteria);
}

#[tokio::test]
async fn queued_user_steering_does_not_consume_goal_continuation_budget() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let provider = BlockingTextProvider::default();
    let runtime = AgentRuntime::with_config(
        store.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
        std::env::temp_dir(),
        String::new(),
        RuntimeConfig {
            goal_max_turns: Some(1),
            ..Default::default()
        },
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Queued goal steering".into()),
            provider: Some("blocking".into()),
            model: Some("blocking-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .set_goal(thread.id, Some("negotiated objective".into()))
        .await
        .unwrap();
    let mut near_limit = store.goal_state(thread.id).await.unwrap().unwrap();
    near_limit.needs_criteria = false;
    near_limit.turns_used = 1;
    store
        .save_goal_state(thread.id, Some(near_limit))
        .await
        .unwrap();

    let turn = runtime
        .start_turn(
            thread.id,
            StartTurnRequest {
                request_id: RequestId::new(),
                text: "initial work".into(),
                attachments: Vec::new(),
                resources: Vec::new(),
                synthetic: false,
                auto_approve: false,
                agent: None,
                provider: Some("blocking".into()),
                model: Some("blocking-1".into()),
                reasoning_effort: None,
                tools: None,
                command: None,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    wait_provider_call(&provider).await;
    let queued = runtime
        .queue_input(
            thread.id,
            QueueInputRequest {
                preview: None,
                reasoning_effort: None,
                delivery: haarness_protocol::InputDelivery::Steer,
                request_id: RequestId::new(),
                text: "steer with new evidence".into(),
                attachments: Vec::new(),
                resources: Vec::new(),
                synthetic: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(queued.queued);
    provider.release.add_permits(1);

    wait_provider_call(&provider).await;
    let during_steering = runtime.goal_state(thread.id).await.unwrap().unwrap();
    assert_eq!(during_steering.status, GoalStatus::Active);
    assert_eq!(
        during_steering.turns_used, 1,
        "queued user input does not consume the last autonomous turn"
    );
    {
        let requests = provider.requests.lock().unwrap();
        let steered = requests.get(1).expect("queued input gets a provider round");
        let latest_user = steered
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                ProviderMessage::Text { role, content } if role == "user" => Some(content),
                _ => None,
            })
            .expect("steered request has user context");
        assert!(latest_user.contains("<goal-harness>"));
        assert!(latest_user.contains("steer with new evidence"));
    }

    provider.release.add_permits(1);
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let finished = runtime.goal_state(thread.id).await.unwrap().unwrap();
    assert_eq!(finished.status, GoalStatus::Paused);
    assert_eq!(finished.turns_used, 1);
    assert_eq!(provider.requests.lock().unwrap().len(), 2);
}

struct StatusRaceRepository {
    inner: Arc<SqliteStore>,
    block_next_goal_read: AtomicBool,
    goal_read_captured: Semaphore,
    release_goal_read: Semaphore,
    goal_state_saves: AtomicUsize,
    block_next_turn_start: AtomicBool,
    turn_start_captured: Semaphore,
    release_turn_start: Semaphore,
    fail_next_goal_state: AtomicBool,
    block_next_terminal_status: AtomicBool,
    terminal_status_captured: Semaphore,
    release_terminal_status: Semaphore,
}

impl StatusRaceRepository {
    fn new(inner: Arc<SqliteStore>) -> Self {
        Self {
            inner,
            block_next_goal_read: AtomicBool::new(false),
            goal_read_captured: Semaphore::new(0),
            release_goal_read: Semaphore::new(0),
            goal_state_saves: AtomicUsize::new(0),
            block_next_turn_start: AtomicBool::new(false),
            turn_start_captured: Semaphore::new(0),
            release_turn_start: Semaphore::new(0),
            fail_next_goal_state: AtomicBool::new(false),
            block_next_terminal_status: AtomicBool::new(false),
            terminal_status_captured: Semaphore::new(0),
            release_terminal_status: Semaphore::new(0),
        }
    }
}

#[async_trait]
impl ThreadRepository for StatusRaceRepository {
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
        request: UpdateThreadRequest,
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
        if self.block_next_turn_start.swap(false, Ordering::SeqCst) {
            self.turn_start_captured.add_permits(1);
            self.release_turn_start
                .acquire()
                .await
                .expect("test semaphore stays open")
                .forget();
        }
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
        self.inner.append_item(thread, turn, data).await
    }

    async fn set_turn_status(
        &self,
        id: TurnId,
        status: TurnStatus,
        error: Option<String>,
    ) -> CoreResult<Turn> {
        if matches!(status, TurnStatus::Completed | TurnStatus::Failed)
            && self
                .block_next_terminal_status
                .swap(false, Ordering::SeqCst)
        {
            self.terminal_status_captured.add_permits(1);
            self.release_terminal_status
                .acquire()
                .await
                .expect("test semaphore stays open")
                .forget();
        }
        self.inner.set_turn_status(id, status, error).await
    }

    async fn compact(
        &self,
        id: ThreadId,
        request: CompactRequest,
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

    async fn save_permission_rule(&self, workspace: &str, rule: &PermissionRule) -> CoreResult<()> {
        self.inner.save_permission_rule(workspace, rule).await
    }

    async fn load_permission_rules(&self, workspace: &str) -> CoreResult<Vec<PermissionRule>> {
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

    async fn goal_state(&self, id: ThreadId) -> CoreResult<Option<GoalState>> {
        if self.fail_next_goal_state.swap(false, Ordering::SeqCst) {
            return Err(haarness_core::CoreError::Storage(
                "forced goal-state read failure".into(),
            ));
        }
        let state = self.inner.goal_state(id).await?;
        if self.block_next_goal_read.swap(false, Ordering::SeqCst) {
            self.goal_read_captured.add_permits(1);
            self.release_goal_read
                .acquire()
                .await
                .expect("test semaphore stays open")
                .forget();
        }
        Ok(state)
    }

    async fn save_goal_state(&self, id: ThreadId, state: Option<GoalState>) -> CoreResult<()> {
        self.goal_state_saves.fetch_add(1, Ordering::SeqCst);
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

#[tokio::test]
async fn goal_status_cannot_resurrect_a_concurrent_pause() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let repository = Arc::new(StatusRaceRepository::new(store.clone()));
    let runtime = Arc::new(AgentRuntime::with_config(
        repository.clone(),
        Arc::new(StaticProviderRegistry::new(Vec::new())),
        Arc::new(EmptyToolRegistry),
        EventBus::new(16),
        std::env::temp_dir(),
        String::new(),
        RuntimeConfig::default(),
    ));
    let thread = runtime
        .create_thread(CreateThreadRequest::default())
        .await
        .unwrap();
    runtime
        .set_goal(thread.id, Some("original objective".into()))
        .await
        .unwrap();
    repository.goal_state_saves.store(0, Ordering::SeqCst);
    repository
        .block_next_goal_read
        .store(true, Ordering::SeqCst);

    let status_runtime = runtime.clone();
    let status = tokio::spawn(async move {
        status_runtime
            .goal_action(thread.id, GoalActionRequest::Status)
            .await
    });
    tokio::time::timeout(
        Duration::from_secs(1),
        repository.goal_read_captured.acquire(),
    )
    .await
    .expect("status read is captured")
    .expect("test semaphore stays open")
    .forget();

    let mut paused = store.goal_state(thread.id).await.unwrap().unwrap();
    paused.status = GoalStatus::Paused;
    paused.paused_reason = Some("concurrent pause".into());
    store
        .save_goal_state(thread.id, Some(paused))
        .await
        .unwrap();
    repository.release_goal_read.add_permits(1);
    status.await.unwrap().unwrap();

    let canonical = store.goal_state(thread.id).await.unwrap().unwrap();
    assert_eq!(canonical.status, GoalStatus::Paused);
    assert_eq!(canonical.paused_reason.as_deref(), Some("concurrent pause"));
    assert_eq!(
        repository.goal_state_saves.load(Ordering::SeqCst),
        0,
        "status is a pure read and cannot write its captured snapshot back"
    );
}

fn blocking_turn_request(text: &str) -> StartTurnRequest {
    StartTurnRequest {
        request_id: RequestId::new(),
        text: text.into(),
        attachments: Vec::new(),
        resources: Vec::new(),
        synthetic: false,
        auto_approve: false,
        agent: None,
        provider: Some("blocking".into()),
        model: Some("blocking-1".into()),
        reasoning_effort: None,
        tools: None,
        command: None,
        ..Default::default()
    }
}

fn fail_once_turn_request(text: &str) -> StartTurnRequest {
    StartTurnRequest {
        request_id: RequestId::new(),
        text: text.into(),
        attachments: Vec::new(),
        resources: Vec::new(),
        synthetic: false,
        auto_approve: false,
        agent: None,
        provider: Some("fail-once".into()),
        model: Some("fail-once-1".into()),
        reasoning_effort: None,
        tools: None,
        command: None,
        ..Default::default()
    }
}

async fn runner_mutation_runtime() -> (
    Arc<SqliteStore>,
    Arc<StatusRaceRepository>,
    Arc<AgentRuntime>,
    BlockingTextProvider,
    Thread,
) {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let repository = Arc::new(StatusRaceRepository::new(store.clone()));
    let provider = BlockingTextProvider::default();
    let runtime = Arc::new(AgentRuntime::with_config(
        repository.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
        std::env::temp_dir(),
        String::new(),
        RuntimeConfig {
            goal_max_turns: Some(1),
            ..Default::default()
        },
    ));
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Runner mutation race".into()),
            provider: Some("blocking".into()),
            model: Some("blocking-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .set_goal(thread.id, Some("original objective".into()))
        .await
        .unwrap();
    let mut negotiated = store.goal_state(thread.id).await.unwrap().unwrap();
    negotiated.needs_criteria = false;
    store
        .save_goal_state(thread.id, Some(negotiated))
        .await
        .unwrap();
    (store, repository, runtime, provider, thread)
}

async fn suspend_runner_goal_mutation(
    repository: &StatusRaceRepository,
    provider: &BlockingTextProvider,
) {
    wait_provider_call(provider).await;
    repository
        .block_next_goal_read
        .store(true, Ordering::SeqCst);
    provider.release.add_permits(1);
    tokio::time::timeout(
        Duration::from_secs(2),
        repository.goal_read_captured.acquire(),
    )
    .await
    .expect("runner goal read is captured")
    .expect("test semaphore stays open")
    .forget();
}

#[tokio::test]
async fn clear_waits_for_runner_goal_mutation_and_cannot_be_resurrected() {
    let (_store, repository, runtime, provider, thread) = runner_mutation_runtime().await;
    let turn = runtime
        .start_turn(thread.id, blocking_turn_request("work"))
        .await
        .unwrap();
    suspend_runner_goal_mutation(&repository, &provider).await;

    let action_runtime = runtime.clone();
    let mut clear = tokio::spawn(async move {
        action_runtime
            .goal_action(thread.id, GoalActionRequest::Clear)
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut clear)
            .await
            .is_err(),
        "clear waits until the runner's captured read is durably resolved"
    );
    repository.release_goal_read.add_permits(1);
    clear.await.unwrap().unwrap();
    provider.release.add_permits(8);
    wait_done(&runtime, &turn).await;

    assert!(runtime.goal_state(thread.id).await.unwrap().is_none());
}

#[tokio::test]
async fn replacement_waits_for_runner_goal_mutation_and_stays_canonical() {
    let (_store, repository, runtime, provider, thread) = runner_mutation_runtime().await;
    let original_id = runtime
        .goal_state(thread.id)
        .await
        .unwrap()
        .unwrap()
        .goal_id;
    let turn = runtime
        .start_turn(thread.id, blocking_turn_request("work"))
        .await
        .unwrap();
    suspend_runner_goal_mutation(&repository, &provider).await;

    let action_runtime = runtime.clone();
    let mut replace = tokio::spawn(async move {
        action_runtime
            .goal_action(
                thread.id,
                GoalActionRequest::Set {
                    goal: "replacement objective".into(),
                    token_budget: None,
                },
            )
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut replace)
            .await
            .is_err(),
        "replacement waits until the runner's captured read is durably resolved"
    );
    repository.release_goal_read.add_permits(1);
    let replacement = replace.await.unwrap().unwrap().state.unwrap();
    assert_ne!(replacement.goal_id, original_id);
    provider.release.add_permits(8);
    wait_done(&runtime, &turn).await;

    let canonical = runtime.goal_state(thread.id).await.unwrap().unwrap();
    assert_eq!(canonical.goal_id, replacement.goal_id);
    assert_eq!(canonical.goal, "replacement objective");
}

#[tokio::test]
async fn simultaneous_turn_starts_admit_exactly_one_runner() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let repository = Arc::new(StatusRaceRepository::new(store));
    let provider = BlockingTextProvider::default();
    let runtime = Arc::new(AgentRuntime::with_config(
        repository.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(32),
        std::env::temp_dir(),
        String::new(),
        RuntimeConfig::default(),
    ));
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Admission race".into()),
            provider: Some("blocking".into()),
            model: Some("blocking-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    repository
        .block_next_turn_start
        .store(true, Ordering::SeqCst);

    let first_runtime = runtime.clone();
    let first = tokio::spawn(async move {
        first_runtime
            .start_turn(thread.id, blocking_turn_request("first"))
            .await
    });
    tokio::time::timeout(
        Duration::from_secs(1),
        repository.turn_start_captured.acquire(),
    )
    .await
    .expect("first durable start is captured")
    .expect("test semaphore stays open")
    .forget();

    let second_runtime = runtime.clone();
    let mut second = tokio::spawn(async move {
        second_runtime
            .start_turn(thread.id, blocking_turn_request("second"))
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut second)
            .await
            .is_err(),
        "second request waits behind the thread admission reservation"
    );
    repository.release_turn_start.add_permits(1);
    let admitted = first.await.unwrap().unwrap();
    let rejected = second.await.unwrap().unwrap_err();
    assert!(
        rejected
            .to_string()
            .contains("This chat already has a turn running"),
        "{rejected}"
    );

    wait_provider_call(&provider).await;
    provider.release.add_permits(1);
    assert_eq!(
        wait_done(&runtime, &admitted).await.status,
        TurnStatus::Completed
    );
    let user_messages = runtime
        .items(thread.id, None)
        .await
        .unwrap()
        .into_iter()
        .filter(|item| matches!(item.data, ItemData::UserMessage { .. }))
        .count();
    assert_eq!(user_messages, 1);
}

#[tokio::test]
async fn queue_input_retries_the_start_admission_race_without_dropping_text() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let repository = Arc::new(StatusRaceRepository::new(store));
    let provider = BlockingTextProvider::default();
    let runtime = Arc::new(AgentRuntime::with_config(
        repository.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(32),
        std::env::temp_dir(),
        String::new(),
        RuntimeConfig::default(),
    ));
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Queue admission race".into()),
            provider: Some("blocking".into()),
            model: Some("blocking-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    repository
        .block_next_turn_start
        .store(true, Ordering::SeqCst);

    let first_runtime = runtime.clone();
    let first = tokio::spawn(async move {
        first_runtime
            .start_turn(thread.id, blocking_turn_request("first"))
            .await
    });
    tokio::time::timeout(
        Duration::from_secs(1),
        repository.turn_start_captured.acquire(),
    )
    .await
    .expect("first durable start is captured")
    .expect("test semaphore stays open")
    .forget();

    let queue_runtime = runtime.clone();
    let mut queued = tokio::spawn(async move {
        queue_runtime
            .queue_input(
                thread.id,
                QueueInputRequest {
                    preview: None,
                    reasoning_effort: None,
                    delivery: haarness_protocol::InputDelivery::Steer,
                    request_id: RequestId::new(),
                    text: "must not be dropped".into(),
                    attachments: Vec::new(),
                    resources: Vec::new(),
                    synthetic: false,
                    ..Default::default()
                },
            )
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut queued)
            .await
            .is_err(),
        "queue-or-start waits for the in-flight admission"
    );
    repository.release_turn_start.add_permits(1);
    let admitted = first.await.unwrap().unwrap();
    let queued = queued.await.unwrap().unwrap();
    assert!(queued.queued);
    assert_eq!(
        queued.item.as_ref().and_then(|item| match &item.data {
            ItemData::UserMessage { text, .. } => Some(text.as_str()),
            _ => None,
        }),
        Some("must not be dropped")
    );

    provider.release.add_permits(8);
    assert_eq!(
        wait_done(&runtime, &admitted).await.status,
        TurnStatus::Completed
    );
    assert!(
        runtime
            .items(thread.id, None)
            .await
            .unwrap()
            .iter()
            .any(|item| {
                matches!(
                    &item.data,
                    ItemData::UserMessage { text, .. } if text == "must not be dropped"
                )
            })
    );
}

#[tokio::test]
async fn active_retry_with_the_same_request_id_returns_the_existing_turn() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let provider = BlockingTextProvider::default();
    let runtime = AgentRuntime::with_config(
        store,
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(32),
        std::env::temp_dir(),
        String::new(),
        RuntimeConfig::default(),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Active idempotency".into()),
            provider: Some("blocking".into()),
            model: Some("blocking-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let request = blocking_turn_request("one durable request");
    let admitted = runtime
        .start_turn(thread.id, request.clone())
        .await
        .unwrap();
    wait_provider_call(&provider).await;

    let retry = runtime.start_turn(thread.id, request).await.unwrap();
    assert_eq!(retry.id, admitted.id);
    assert_eq!(retry.request_id, admitted.request_id);
    assert_eq!(
        runtime
            .items(thread.id, None)
            .await
            .unwrap()
            .into_iter()
            .filter(|item| matches!(item.data, ItemData::UserMessage { .. }))
            .count(),
        1,
        "an active idempotent retry creates no duplicate item"
    );
    assert_eq!(provider.requests.lock().unwrap().len(), 1);

    provider.release.add_permits(1);
    assert_eq!(
        wait_done(&runtime, &admitted).await.status,
        TurnStatus::Completed
    );
}

#[tokio::test]
async fn launch_setup_failure_finishes_the_durable_turn_and_clears_active() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let repository = Arc::new(StatusRaceRepository::new(store.clone()));
    let events = EventBus::new(32);
    let runtime = AgentRuntime::with_config(
        repository.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(BlockingTextProvider::default()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        events.clone(),
        std::env::temp_dir(),
        String::new(),
        RuntimeConfig::default(),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Launch failure".into()),
            provider: Some("blocking".into()),
            model: Some("blocking-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    repository
        .fail_next_goal_state
        .store(true, Ordering::SeqCst);
    let request = blocking_turn_request("fails during launch setup");

    let error = runtime
        .start_turn(thread.id, request.clone())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("forced goal-state read failure"));
    assert!(runtime.active_turn(thread.id).await.unwrap().is_none());

    let (durable, _, created) = store.start_turn(thread.id, request).await.unwrap();
    assert!(!created, "retry resolves the existing durable request");
    assert_eq!(durable.status, TurnStatus::Failed);
    assert!(
        durable
            .error
            .as_deref()
            .is_some_and(|message| message.contains("turn launch failed"))
    );
    assert!(
        runtime
            .items(thread.id, None)
            .await
            .unwrap()
            .iter()
            .any(|item| {
                matches!(
                    &item.data,
                    ItemData::Error { message, .. } if message.contains("turn launch failed")
                )
            })
    );
    let lifecycle = events
        .subscribe(None)
        .replay
        .into_iter()
        .filter(|event| matches!(event.event_type.as_str(), "turn.started" | "turn.finished"))
        .map(|event| event.event_type)
        .collect::<Vec<_>>();
    assert_eq!(lifecycle, ["turn.started", "turn.finished"]);
}

#[tokio::test]
async fn input_at_the_closing_boundary_starts_fresh_after_terminal_removal() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let repository = Arc::new(StatusRaceRepository::new(store));
    let provider = BlockingTextProvider::default();
    let runtime = Arc::new(AgentRuntime::with_config(
        repository.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(32),
        std::env::temp_dir(),
        String::new(),
        RuntimeConfig::default(),
    ));
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Closing boundary".into()),
            provider: Some("blocking".into()),
            model: Some("blocking-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let first = runtime
        .start_turn(thread.id, blocking_turn_request("first"))
        .await
        .unwrap();
    wait_provider_call(&provider).await;
    repository
        .block_next_terminal_status
        .store(true, Ordering::SeqCst);
    provider.release.add_permits(1);
    tokio::time::timeout(
        Duration::from_secs(2),
        repository.terminal_status_captured.acquire(),
    )
    .await
    .expect("runner reaches the terminal boundary")
    .expect("test semaphore stays open")
    .forget();

    let queue_runtime = runtime.clone();
    let mut next = tokio::spawn(async move {
        queue_runtime
            .queue_input(
                thread.id,
                QueueInputRequest {
                    preview: None,
                    reasoning_effort: None,
                    delivery: haarness_protocol::InputDelivery::Steer,
                    request_id: RequestId::new(),
                    text: "fresh after close".into(),
                    attachments: Vec::new(),
                    resources: Vec::new(),
                    synthetic: false,
                    ..Default::default()
                },
            )
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut next)
            .await
            .is_err(),
        "input waits while the matching active turn is terminal but not removed"
    );
    repository.release_terminal_status.add_permits(1);
    let next = next.await.unwrap().unwrap();
    assert!(!next.queued);
    let second = next
        .turn
        .expect("closing-boundary input starts a fresh turn");
    assert_ne!(second.id, first.id);

    wait_provider_call(&provider).await;
    provider.release.add_permits(1);
    assert_eq!(
        wait_done(&runtime, &second).await.status,
        TurnStatus::Completed
    );
    assert_eq!(
        runtime.turn(first.id).await.unwrap().status,
        TurnStatus::Completed
    );
}

#[tokio::test]
async fn queued_input_is_retried_after_an_inflight_provider_error() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let provider = FailOnceProvider::default();
    let runtime = AgentRuntime::with_config(
        store,
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
        std::env::temp_dir(),
        String::new(),
        RuntimeConfig::default(),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Queued error recovery".into()),
            provider: Some("fail-once".into()),
            model: Some("fail-once-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, fail_once_turn_request("initial request"))
        .await
        .unwrap();
    wait_fail_once_provider_call(&provider).await;

    let queued = runtime
        .queue_input(
            thread.id,
            QueueInputRequest {
                preview: None,
                reasoning_effort: None,
                delivery: haarness_protocol::InputDelivery::Steer,
                request_id: RequestId::new(),
                text: "accepted while the provider is blocked".into(),
                attachments: Vec::new(),
                resources: Vec::new(),
                synthetic: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(queued.queued);
    let queued_id = queued.item.expect("queued item is durable").id;
    provider.release.add_permits(1);

    wait_fail_once_provider_call(&provider).await;
    {
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2, "the failed runner retries exactly once");
        assert!(requests[1].messages.iter().any(|message| matches!(
            message,
            ProviderMessage::Text { role, content }
                if role == "user" && content.contains("accepted while the provider is blocked")
        )));
    }
    provider.release.add_permits(1);
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(runtime.items(thread.id, None).await.unwrap().iter().any(
        |item| matches!(item.data, ItemData::QueueConsumed { item_id } if item_id == queued_id)
    ));
}

#[tokio::test]
async fn input_at_error_cleanup_waits_until_goal_and_turn_are_terminal() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let repository = Arc::new(StatusRaceRepository::new(store.clone()));
    let provider = FailOnceProvider::default();
    let events = EventBus::new(64);
    let runtime = Arc::new(AgentRuntime::with_config(
        repository.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(EmptyToolRegistry),
        events.clone(),
        std::env::temp_dir(),
        String::new(),
        RuntimeConfig::default(),
    ));
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Error cleanup ordering".into()),
            provider: Some("fail-once".into()),
            model: Some("fail-once-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .set_goal(thread.id, Some("preserve failure ordering".into()))
        .await
        .unwrap();
    let mut goal = store.goal_state(thread.id).await.unwrap().unwrap();
    goal.needs_criteria = false;
    store.save_goal_state(thread.id, Some(goal)).await.unwrap();

    let first = runtime
        .start_turn(thread.id, fail_once_turn_request("fail this turn"))
        .await
        .unwrap();
    wait_fail_once_provider_call(&provider).await;
    repository
        .block_next_terminal_status
        .store(true, Ordering::SeqCst);
    provider.release.add_permits(1);
    tokio::time::timeout(
        Duration::from_secs(2),
        repository.terminal_status_captured.acquire(),
    )
    .await
    .expect("error runner reaches terminal persistence")
    .expect("test semaphore stays open")
    .forget();
    assert_eq!(
        runtime.goal_state(thread.id).await.unwrap().unwrap().status,
        GoalStatus::Paused,
        "the old runner pauses its goal before exposing the active slot"
    );

    let queue_runtime = runtime.clone();
    let mut next = tokio::spawn(async move {
        queue_runtime
            .queue_input(
                thread.id,
                QueueInputRequest {
                    preview: None,
                    reasoning_effort: None,
                    delivery: haarness_protocol::InputDelivery::Steer,
                    request_id: RequestId::new(),
                    text: "start only after failure is durable".into(),
                    attachments: Vec::new(),
                    resources: Vec::new(),
                    synthetic: false,
                    ..Default::default()
                },
            )
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut next)
            .await
            .is_err(),
        "new input waits for old failure terminalization and removal"
    );
    repository.release_terminal_status.add_permits(1);
    let next = next.await.unwrap().unwrap();
    assert!(!next.queued);
    let second = next.turn.expect("input starts a fresh turn");
    assert_ne!(second.id, first.id);

    wait_fail_once_provider_call(&provider).await;
    provider.release.add_permits(1);
    assert_eq!(
        wait_done(&runtime, &second).await.status,
        TurnStatus::Completed
    );
    assert_eq!(
        runtime.turn(first.id).await.unwrap().status,
        TurnStatus::Failed
    );
    assert_eq!(
        runtime.goal_state(thread.id).await.unwrap().unwrap().status,
        GoalStatus::Paused
    );

    let lifecycle = events
        .subscribe(None)
        .replay
        .into_iter()
        .filter(|event| matches!(event.event_type.as_str(), "turn.started" | "turn.finished"))
        .collect::<Vec<_>>();
    let old_finished = lifecycle
        .iter()
        .position(|event| event.event_type == "turn.finished" && event.turn_id == Some(first.id))
        .expect("old failure event exists");
    let new_started = lifecycle
        .iter()
        .position(|event| event.event_type == "turn.started" && event.turn_id == Some(second.id))
        .expect("new start event exists");
    assert!(old_finished < new_started);
}

#[tokio::test]
async fn child_failure_recovers_queued_input_before_releasing_turn_admission() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let repository = Arc::new(StatusRaceRepository::new(store));
    let provider = ChildFailureProvider::default();
    let events = EventBus::new(128);
    let runtime = Arc::new(AgentRuntime::with_config(
        repository.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(std::env::temp_dir())),
        events.clone(),
        std::env::temp_dir(),
        String::new(),
        RuntimeConfig::default(),
    ));
    let root = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Child queue failure".into()),
            provider: Some("child-failure".into()),
            model: Some("child-failure-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let root_turn = runtime
        .start_turn(
            root.id,
            StartTurnRequest {
                request_id: RequestId::new(),
                text: "spawn failing child".into(),
                attachments: Vec::new(),
                resources: Vec::new(),
                synthetic: false,
                auto_approve: false,
                agent: None,
                provider: Some("child-failure".into()),
                model: Some("child-failure-1".into()),
                reasoning_effort: None,
                tools: None,
                command: None,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    wait_child_provider_call(&provider).await;
    let child = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(child) = runtime
                .list_threads(false)
                .await
                .unwrap()
                .into_iter()
                .find(|thread| thread.parent_id == Some(root.id))
            {
                break child;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("child thread becomes visible");
    let failed_child_turn = runtime
        .active_turn(child.id)
        .await
        .unwrap()
        .expect("child turn is active");

    let queued = runtime
        .queue_input(
            child.id,
            QueueInputRequest {
                preview: None,
                reasoning_effort: None,
                delivery: haarness_protocol::InputDelivery::Steer,
                request_id: RequestId::new(),
                text: "queued into failing child".into(),
                attachments: Vec::new(),
                resources: Vec::new(),
                synthetic: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(queued.queued);
    let queued_id = queued.item.expect("queued child item is durable").id;
    provider.release.add_permits(1);

    wait_child_provider_call(&provider).await;
    {
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2, "child failure gets one bounded retry");
        assert!(requests[1].messages.iter().any(|message| matches!(
            message,
            ProviderMessage::Text { role, content }
                if role == "user" && content.contains("queued into failing child")
        )));
    }
    repository
        .block_next_terminal_status
        .store(true, Ordering::SeqCst);
    provider.release.add_permits(1);
    tokio::time::timeout(
        Duration::from_secs(2),
        repository.terminal_status_captured.acquire(),
    )
    .await
    .expect("failed child reaches terminal persistence")
    .expect("test semaphore stays open")
    .forget();

    let queue_runtime = runtime.clone();
    let mut next = tokio::spawn(async move {
        queue_runtime
            .queue_input(
                child.id,
                QueueInputRequest {
                    preview: None,
                    reasoning_effort: None,
                    delivery: haarness_protocol::InputDelivery::Steer,
                    request_id: RequestId::new(),
                    text: "fresh turn after child failure".into(),
                    attachments: Vec::new(),
                    resources: Vec::new(),
                    synthetic: false,
                    ..Default::default()
                },
            )
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut next)
            .await
            .is_err(),
        "child admission stays closed until the failed turn is durable"
    );
    repository.release_terminal_status.add_permits(1);
    let next = next.await.unwrap().unwrap();
    assert!(!next.queued);
    let recovered_turn = next.turn.expect("fresh child turn starts");
    assert_ne!(recovered_turn.id, failed_child_turn.id);
    assert_eq!(
        runtime.turn(failed_child_turn.id).await.unwrap().status,
        TurnStatus::Failed
    );

    wait_child_provider_call(&provider).await;
    provider.release.add_permits(1);
    assert_eq!(
        wait_done(&runtime, &recovered_turn).await.status,
        TurnStatus::Completed
    );
    assert_eq!(
        wait_done(&runtime, &root_turn).await.status,
        TurnStatus::Completed
    );
    let root_items = runtime.items(root.id, None).await.unwrap();
    assert!(
        root_items.iter().any(|item| matches!(
            &item.data,
            ItemData::ToolResult {
                name,
                output,
                is_error: true,
                ..
            } if name == "task"
                && output == "Provider error: forced child provider failure 2"
        )),
        "a child failure must become an error tool result for the parent"
    );
    assert!(
        root_items.iter().any(|item| matches!(
            &item.data,
            ItemData::AssistantMessage { text }
                if text == "parent observed child result"
        )),
        "the parent provider must receive the failed task result and continue its loop"
    );

    let items = runtime.items(child.id, None).await.unwrap();
    assert_eq!(
        items
            .iter()
            .filter(|item| {
                matches!(item.data, ItemData::QueueConsumed { item_id } if item_id == queued_id)
            })
            .count(),
        1,
        "the failed child retry acknowledges the queued message exactly once"
    );
    assert!(!items.iter().any(|item| matches!(
        &item.data,
        ItemData::Error {
            message,
            retryable: true
        } if message.contains("Queued input could not be delivered")
    )));

    let lifecycle = events
        .subscribe(None)
        .replay
        .into_iter()
        .filter(|event| matches!(event.event_type.as_str(), "turn.started" | "turn.finished"))
        .collect::<Vec<_>>();
    let old_finished = lifecycle
        .iter()
        .position(|event| {
            event.event_type == "turn.finished" && event.turn_id == Some(failed_child_turn.id)
        })
        .expect("failed child finish event exists");
    let new_started = lifecycle
        .iter()
        .position(|event| {
            event.event_type == "turn.started" && event.turn_id == Some(recovered_turn.id)
        })
        .expect("fresh child start event exists");
    assert!(old_finished < new_started);
}

#[tokio::test]
async fn child_start_event_exposes_the_active_cancellation_token() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let provider = ChildFailureProvider::default();
    let events = EventBus::new(64);
    let mut subscription = events.subscribe(None);
    let runtime = AgentRuntime::with_config(
        store,
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(std::env::temp_dir())),
        events,
        std::env::temp_dir(),
        String::new(),
        RuntimeConfig::default(),
    );
    let root = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Child start interrupt".into()),
            provider: Some("child-failure".into()),
            model: Some("child-failure-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let root_turn = runtime
        .start_turn(
            root.id,
            StartTurnRequest {
                request_id: RequestId::new(),
                text: "spawn failing child".into(),
                attachments: Vec::new(),
                resources: Vec::new(),
                synthetic: false,
                auto_approve: false,
                agent: None,
                provider: Some("child-failure".into()),
                model: Some("child-failure-1".into()),
                reasoning_effort: None,
                tools: None,
                command: None,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let child_started = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = subscription.receiver.recv().await.unwrap();
            if event.event_type == "turn.started" && event.thread_id != Some(root.id) {
                break event;
            }
        }
    })
    .await
    .expect("child turn start is published");
    let child_turn_id = child_started.turn_id.expect("child event has a turn");
    let child_thread_id = child_started.thread_id.expect("child event has a thread");

    assert_eq!(
        runtime.interrupt(child_turn_id).await.unwrap().status,
        TurnStatus::Interrupted
    );
    wait_child_provider_call(&provider).await;
    assert_eq!(
        wait_done(&runtime, &root_turn).await.status,
        TurnStatus::Completed
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        runtime.turn(child_turn_id).await.unwrap().status,
        TurnStatus::Interrupted,
        "the cancelled child cannot overwrite its terminal status"
    );
    assert!(
        runtime
            .active_turn(child_thread_id)
            .await
            .unwrap()
            .is_none()
    );
}
