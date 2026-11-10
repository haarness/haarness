//! A `Stop` hook that blocks keeps a root turn going: the model answers its
//! feedback in the same turn, which ends once.
use super::*;
use haarness_core::{HookCall, HookDecision, HookPort};
use haarness_protocol::{
    InputDelivery, Item, ItemData, ItemId, QueueInputRequest, ThreadId, TurnId,
};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Echoes the last message, and delegates once for `!tool task <json>`.
struct Script;

#[async_trait]
impl ModelProvider for Script {
    fn id(&self) -> &str {
        "echo"
    }
    fn info(&self) -> ProviderInfo {
        Echo.info()
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        _: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if let Some(title) = title_fixture(&request) {
            return title;
        }
        let mut prompt = String::new();
        let mut answered = false;
        for message in &request.messages {
            match message {
                ProviderMessage::Text { role, content }
                    if role == "user" && !haarness_core::is_turn_reminder(content) =>
                {
                    prompt.clone_from(content);
                    answered = false;
                }
                ProviderMessage::ToolResult { .. } => answered = true,
                _ => {}
            }
        }
        let finish = |reason: &str| ProviderEvent::Finished {
            reason: reason.into(),
        };
        let events = match prompt.strip_prefix("!tool task ") {
            Some(arguments) if !answered => vec![
                ProviderEvent::ToolCall {
                    call_id: "delegate".into(),
                    name: "task".into(),
                    arguments: serde_json::from_str(arguments).unwrap(),
                    provider_metadata: Value::Null,
                },
                finish("tool_use"),
            ],
            Some(_) => vec![ProviderEvent::TextDelta("Delegated".into()), finish("stop")],
            None => vec![
                ProviderEvent::TextDelta(format!("Echo: {prompt}")),
                finish("stop"),
            ],
        };
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

/// Stop hooks that answer as `answers` says, run by run (the last answer
/// repeats), and record what each run was told.
#[derive(Default)]
struct StopHooks {
    answers: Vec<Option<&'static str>>,
    /// The chat and `stop_hook_active` of each run.
    runs: Mutex<Vec<(ThreadId, bool)>>,
    turn_ends: AtomicUsize,
    /// When set, the first run says it began and waits to be let go.
    gate: Option<(tokio::sync::Notify, tokio::sync::Notify)>,
}

impl StopHooks {
    fn answering(answers: &[Option<&'static str>]) -> Self {
        Self {
            answers: answers.to_vec(),
            ..Self::default()
        }
    }

    fn held(answers: &[Option<&'static str>]) -> Self {
        Self {
            gate: Some((tokio::sync::Notify::new(), tokio::sync::Notify::new())),
            ..Self::answering(answers)
        }
    }

    fn active(&self) -> Vec<bool> {
        self.runs
            .lock()
            .unwrap()
            .iter()
            .map(|(_, active)| *active)
            .collect()
    }
}

#[async_trait]
impl HookPort for StopHooks {
    async fn pre_tool_use(&self, _: &HookCall, _: &str, _: &Value) -> HookDecision {
        HookDecision::default()
    }
    async fn post_tool_use(
        &self,
        _: &HookCall,
        _: &str,
        _: &Value,
        _: &str,
        _: bool,
    ) -> HookDecision {
        HookDecision::default()
    }
    async fn user_prompt_submit(&self, _: &HookCall, _: &str) -> HookDecision {
        HookDecision::default()
    }
    async fn permission_request(
        &self,
        _: &HookCall,
        _: &str,
        _: &[String],
        _: Option<&[String]>,
        _: &Value,
    ) -> HookDecision {
        HookDecision::default()
    }
    async fn stop(&self, call: &HookCall, active: bool) -> Option<String> {
        let run = {
            let mut runs = self.runs.lock().unwrap();
            runs.push((call.thread, active));
            runs.len() - 1
        };
        if let Some((entered, release)) = self.gate.as_ref().filter(|_| run == 0) {
            entered.notify_one();
            release.notified().await;
        }
        self.answers[run.min(self.answers.len() - 1)].map(str::to_owned)
    }
    async fn turn_end(&self, _: &HookCall, _: TurnId, _: &Value) {
        self.turn_ends.fetch_add(1, Ordering::SeqCst);
    }
}

fn runtime(hooks: Arc<StopHooks>) -> (std::path::PathBuf, AgentRuntime) {
    let directory =
        std::env::temp_dir().join(format!("haarness-stop-hook-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&directory).unwrap();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Script) as Arc<dyn ModelProvider>
        ])),
        Arc::new(NativeToolRegistry::new(directory.clone())),
        EventBus::new(2048),
        directory.clone(),
        String::new(),
        RuntimeConfig::default(),
    )
    .with_hooks(hooks);
    (directory, runtime)
}

fn replies(items: &[Item]) -> Vec<&str> {
    items
        .iter()
        .filter_map(|item| match &item.data {
            ItemData::AssistantMessage { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// The messages hooks kept the turn going with: the feedback of each.
fn hook_messages(items: &[Item]) -> Vec<&str> {
    items
        .iter()
        .filter_map(|item| match &item.data {
            ItemData::UserMessage {
                text,
                synthetic: true,
                queued: false,
                hook: Some(event),
                ..
            } if event == "Stop" => Some(haarness_protocol::hook_feedback(text)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_stop_hook_that_blocks_once_gets_one_more_reply_in_the_same_turn() {
    let hooks = Arc::new(StopHooks::answering(&[Some("run the tests"), None]));
    let (directory, runtime) = runtime(hooks.clone());
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("fix the bug"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    wait_idle(&runtime, thread.id).await;
    let items = runtime.items(thread.id, None).await.unwrap();
    assert_eq!(hook_messages(&items), ["run the tests"]);
    let replies = replies(&items);
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(replies[0], "Echo: fix the bug");
    // The model read the hook's message, which says who wrote it.
    assert!(
        replies[1].contains("the person did not type")
            && replies[1].ends_with("<hook-feedback>\nrun the tests\n</hook-feedback>"),
        "{}",
        replies[1]
    );
    assert!(items.iter().all(|item| item.turn_id == Some(turn.id)));
    assert_eq!(hooks.active(), [false, true]);
    assert_eq!(hooks.turn_ends.load(Ordering::SeqCst), 1);
    remove_test_folder(directory).await;
}

#[tokio::test]
async fn a_stop_hooks_long_feedback_is_cut_as_other_hook_text_is() {
    let long: &'static str = "x".repeat(20_000).leak();
    let hooks = Arc::new(StopHooks::answering(&[Some(long), None]));
    let (directory, runtime) = runtime(hooks.clone());
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("test it"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    wait_idle(&runtime, thread.id).await;
    let items = runtime.items(thread.id, None).await.unwrap();
    let feedback = hook_messages(&items);
    assert_eq!(feedback.len(), 1);
    assert_eq!(
        feedback[0],
        format!("{}… (4000 more characters not shown)", "x".repeat(16_000))
    );
    remove_test_folder(directory).await;
}

/// A workflow takes its agent's last reply as the agent's answer: Stop
/// hooks observe the end of the agent's turn, and never send it back.
#[tokio::test]
async fn a_workflow_agents_turn_ends_whatever_its_stop_hooks_say() {
    let hooks = Arc::new(StopHooks::answering(&[Some("run the tests")]));
    let (directory, runtime) = runtime(hooks.clone());
    let chat = echo_thread(&runtime).await;
    let child = |parent: ThreadId, title: &str| CreateThreadRequest {
        title: Some(title.into()),
        parent_id: Some(parent),
        provider: Some("echo".into()),
        model: Some("echo-1".into()),
        ..Default::default()
    };
    let workflow = runtime
        .create_thread(child(chat.id, "Workflow"))
        .await
        .unwrap();
    let agent = runtime
        .create_thread(child(workflow.id, "Workflow agent"))
        .await
        .unwrap();
    let turn = runtime
        .start_workflow_agent_turn(chat.id, agent.id, turn_request("review the diff"), None)
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    wait_idle(&runtime, agent.id).await;
    let items = runtime.items(agent.id, None).await.unwrap();
    assert!(hook_messages(&items).is_empty());
    assert_eq!(replies(&items), ["Echo: review the diff"]);
    assert_eq!(*hooks.runs.lock().unwrap(), [(agent.id, false)]);
    assert_eq!(hooks.turn_ends.load(Ordering::SeqCst), 1);
    remove_test_folder(directory).await;
}

#[tokio::test]
async fn stop_hooks_that_always_block_keep_a_turn_going_eight_times_then_it_ends() {
    let hooks = Arc::new(StopHooks::answering(&[Some("keep going")]));
    let (directory, runtime) = runtime(hooks.clone());
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("loop"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    wait_idle(&runtime, thread.id).await;
    let items = runtime.items(thread.id, None).await.unwrap();
    assert_eq!(hook_messages(&items).len(), 8);
    assert_eq!(replies(&items).len(), 9);
    let active = hooks.active();
    assert_eq!(active.len(), 9);
    assert!(!active[0] && active[1..].iter().all(|active| *active));
    assert_eq!(hooks.turn_ends.load(Ordering::SeqCst), 1);
    remove_test_folder(directory).await;
}

#[tokio::test]
async fn a_message_sent_while_a_stop_hook_runs_goes_first_and_the_feedback_is_dropped() {
    let hooks = Arc::new(StopHooks::held(&[Some("not now"), None]));
    let (directory, runtime) = runtime(hooks.clone());
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("first"))
        .await
        .unwrap();
    let (entered, release) = hooks.gate.as_ref().unwrap();
    entered.notified().await;
    let sent = runtime
        .queue_input(
            thread.id,
            QueueInputRequest {
                request_id: RequestId::new(),
                text: "answer me instead".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(sent.queued, "the message steers the running turn");
    release.notify_one();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    wait_idle(&runtime, thread.id).await;
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(hook_messages(&items).is_empty());
    let replies = replies(&items);
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert!(replies[1].contains("answer me instead"), "{}", replies[1]);
    // The hooks ran again when the turn would end after it, told that no
    // hook kept it going.
    assert_eq!(hooks.active(), [false, false]);
    assert_eq!(hooks.turn_ends.load(Ordering::SeqCst), 1);
    remove_test_folder(directory).await;
}

#[tokio::test]
async fn a_message_queued_for_later_goes_first_and_the_feedback_is_dropped() {
    let hooks = Arc::new(StopHooks::held(&[Some("not now"), None]));
    let (directory, runtime) = runtime(hooks.clone());
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("first"))
        .await
        .unwrap();
    let (entered, release) = hooks.gate.as_ref().unwrap();
    entered.notified().await;
    runtime
        .queue_input(
            thread.id,
            QueueInputRequest {
                request_id: RequestId::new(),
                delivery: InputDelivery::Queue,
                text: "the next thing".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    release.notify_one();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    // The queued message then starts a turn of its own.
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while replies(&runtime.items(thread.id, None).await.unwrap()).len() < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the queued message is answered");
    wait_idle(&runtime, thread.id).await;
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(hook_messages(&items).is_empty());
    assert_eq!(replies(&items), ["Echo: first", "Echo: the next thing"]);
    assert_eq!(
        items
            .iter()
            .filter(|item| item.turn_id == Some(turn.id))
            .filter(|item| matches!(item.data, ItemData::AssistantMessage { .. }))
            .count(),
        1
    );
    assert_eq!(hooks.active(), [false, false]);
    assert_eq!(hooks.turn_ends.load(Ordering::SeqCst), 2);
    remove_test_folder(directory).await;
}

#[tokio::test]
async fn a_stop_while_a_stop_hook_runs_ends_the_turn_without_its_message() {
    let hooks = Arc::new(StopHooks::held(&[Some("never read")]));
    let (directory, runtime) = runtime(hooks.clone());
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("stop me"))
        .await
        .unwrap();
    let (entered, release) = hooks.gate.as_ref().unwrap();
    entered.notified().await;
    runtime.interrupt(turn.id).await.unwrap();
    release.notify_one();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Interrupted
    );
    wait_idle(&runtime, thread.id).await;
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(hook_messages(&items).is_empty());
    assert_eq!(replies(&items), ["Echo: stop me"]);
    assert_eq!(hooks.turn_ends.load(Ordering::SeqCst), 0);
    remove_test_folder(directory).await;
}

#[tokio::test]
async fn a_subagents_turn_never_runs_stop_hooks() {
    let hooks = Arc::new(StopHooks::answering(&[Some("check the child"), None]));
    let (directory, runtime) = runtime(hooks.clone());
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(r#"!tool task {"description":"Look","prompt":"work"}"#),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    wait_idle(&runtime, thread.id).await;
    // Only the chat's own turn ran them, once blocked and once not.
    let runs = hooks.runs.lock().unwrap().clone();
    assert_eq!(runs, [(thread.id, false), (thread.id, true)]);
    let children: Vec<ThreadId> = runtime
        .items(thread.id, None)
        .await
        .unwrap()
        .iter()
        .filter_map(|item| match &item.data {
            ItemData::TaskLink {
                child_thread_id, ..
            } => Some(*child_thread_id),
            _ => None,
        })
        .collect();
    assert_eq!(children.len(), 1);
    let child = runtime.items(children[0], None).await.unwrap();
    assert!(hook_messages(&child).is_empty());
    assert_eq!(replies(&child), ["Echo: work"]);
    assert_eq!(
        hook_messages(&runtime.items(thread.id, None).await.unwrap()),
        ["check the child"]
    );
    remove_test_folder(directory).await;
}

/// A store whose chat is stopped while the message of a Stop hook is
/// written: the stop lands after the runtime last looked, before it
/// announces the message.
struct StopsWhileWritten {
    inner: SqliteStore,
    runtime: std::sync::OnceLock<AgentRuntime>,
}

#[async_trait]
impl ThreadRepository for StopsWhileWritten {
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
        if let (ItemData::UserMessage { hook: Some(_), .. }, Some(turn)) = (&data, turn) {
            let runtime = self.runtime.get().unwrap().clone();
            tokio::spawn(async move { runtime.interrupt(turn).await });
            // The stop is asked for at once; it then waits for the turn,
            // which is still writing.
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
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

#[tokio::test]
async fn a_stop_while_a_stop_hooks_message_is_written_takes_the_message_back() {
    let store = Arc::new(StopsWhileWritten {
        inner: SqliteStore::in_memory().unwrap(),
        runtime: std::sync::OnceLock::new(),
    });
    let directory =
        std::env::temp_dir().join(format!("haarness-stop-hook-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&directory).unwrap();
    let hooks = Arc::new(StopHooks::answering(&[Some("never answered")]));
    let runtime = AgentRuntime::with_config(
        store.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Script) as Arc<dyn ModelProvider>
        ])),
        Arc::new(NativeToolRegistry::new(directory.clone())),
        EventBus::new(2048),
        directory.clone(),
        String::new(),
        RuntimeConfig::default(),
    )
    .with_hooks(hooks.clone());
    assert!(store.runtime.set(runtime.clone()).is_ok());
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("stop me later"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Interrupted
    );
    wait_idle(&runtime, thread.id).await;
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(hook_messages(&items).is_empty());
    assert_eq!(replies(&items), ["Echo: stop me later"]);
    // Nobody was told of it either.
    assert!(
        !runtime
            .events()
            .subscribe(None)
            .replay
            .iter()
            .any(|event| event.event_type == "item.created"
                && event.payload["data"]["hook"].is_string())
    );
    assert_eq!(hooks.turn_ends.load(Ordering::SeqCst), 0);
    remove_test_folder(directory).await;
}
