//! Two creates of one chosen chat id that both find no chat yet: the store
//! keeps the first insert, and the runtime answers the second with that chat.
//! And a steer's retry that checked for its turn before the steer that found
//! the chat idle stored it: it gets that turn, not a second message.
use super::*;
use haarness_core::PermissionRule;
use haarness_protocol::{
    CompactRequest, GoalState, Item, ItemData, ItemId, ThreadId, TurnId, UpdateThreadRequest,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Barrier;

/// A store whose first two lookups of one id wait for each other, so
/// neither create has inserted when the other checks whether the chat
/// exists. It finds no turn by request and no admitted input, as a retry
/// that checked before the first request was stored does.
struct RacingCreates {
    inner: SqliteStore,
    id: ThreadId,
    checked: Barrier,
    lookups: AtomicUsize,
    inserts: AtomicUsize,
}

#[async_trait]
impl ThreadRepository for RacingCreates {
    async fn create_thread(&self, request: CreateThreadRequest) -> CoreResult<Thread> {
        if request.id == Some(self.id) {
            self.inserts.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.create_thread(request).await
    }

    async fn list_threads(&self, include_archived: bool) -> CoreResult<Vec<Thread>> {
        self.inner.list_threads(include_archived).await
    }

    async fn get_thread(&self, id: ThreadId) -> CoreResult<Thread> {
        let found = self.inner.get_thread(id).await;
        if id == self.id && self.lookups.fetch_add(1, Ordering::SeqCst) < 2 {
            self.checked.wait().await;
        }
        found
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
        self.inner.goal_state(id).await
    }

    async fn save_goal_state(&self, id: ThreadId, state: Option<GoalState>) -> CoreResult<()> {
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
async fn concurrent_creates_that_both_find_no_chat_make_one_chat() {
    let id = ThreadId::new();
    let repository = Arc::new(RacingCreates {
        inner: SqliteStore::in_memory().unwrap(),
        id,
        checked: Barrier::new(2),
        lookups: AtomicUsize::new(0),
        inserts: AtomicUsize::new(0),
    });
    let runtime = AgentRuntime::new(
        repository.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Echo) as Arc<dyn ModelProvider>
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    );
    let request = || CreateThreadRequest {
        id: Some(id),
        provider: Some("echo".into()),
        model: Some("echo-1".into()),
        ..Default::default()
    };
    let (left, right) = tokio::join!(
        runtime.create_thread(request()),
        runtime.create_thread(request())
    );
    let (left, right) = (left.unwrap(), right.unwrap());
    assert_eq!((left.id, right.id), (id, id));
    assert_eq!(left.created_at, right.created_at, "one chat, read back");
    // Both checked before either inserted, so both inserted: the store
    // refused the second, and the runtime answered it with the first's chat.
    assert_eq!(repository.inserts.load(Ordering::SeqCst), 2);
    assert_eq!(runtime.list_threads(true).await.unwrap().len(), 1);
    let created = runtime
        .events()
        .subscribe(Some(0))
        .replay
        .into_iter()
        .filter(|event| event.event_type == "thread.created")
        .filter_map(|event| event.thread_id)
        .collect::<Vec<_>>();
    assert_eq!(created, [id], "one created event");
}

#[tokio::test]
async fn a_steers_retry_that_checked_before_its_turn_was_stored_gets_that_turn() {
    let provider = SlowText::default();
    let runtime = AgentRuntime::new(
        Arc::new(RacingCreates {
            inner: SqliteStore::in_memory().unwrap(),
            id: ThreadId::new(),
            checked: Barrier::new(2),
            lookups: AtomicUsize::new(0),
            inserts: AtomicUsize::new(0),
        }),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
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
    let steer = haarness_protocol::QueueInputRequest {
        preview: None,
        reasoning_effort: None,
        delivery: haarness_protocol::InputDelivery::Steer,
        request_id: RequestId::new(),
        text: "Go".into(),
        attachments: Vec::new(),
        resources: Vec::new(),
        synthetic: false,
        ..Default::default()
    };
    let started = runtime.queue_input(thread.id, steer.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), provider.first_started.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let retried = runtime.queue_input(thread.id, steer).await.unwrap();
    assert!(!retried.queued && retried.item.is_none(), "{retried:?}");
    assert_eq!(
        retried.turn.map(|turn| turn.id),
        started.turn.map(|turn| turn.id)
    );
    provider.release_first.add_permits(1);
    wait_idle(&runtime, thread.id).await;
    let sent = runtime
        .items(thread.id, None)
        .await
        .unwrap()
        .into_iter()
        .filter(|item| matches!(item.data, ItemData::UserMessage { .. }))
        .count();
    assert_eq!(sent, 1);
}
