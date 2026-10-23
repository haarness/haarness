//! Child agents started together share a prompt prefix. With a provider that
//! writes its prompt cache explicitly, the first child's request goes alone
//! and its siblings follow once its response begins, so they read the cache
//! it wrote instead of each writing it again.

use std::{
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::{StreamExt as _, stream};
use haarness_core::{
    AgentRuntime, CoreResult, EventBus, ModelProvider, ProviderEvent, ProviderMessage,
    ProviderRequest, ProviderStream, RuntimeConfig, StaticProviderRegistry,
};
use haarness_protocol::{
    CreateThreadRequest, ModelInfo, ProviderInfo, RequestId, StartTurnRequest, ThreadId, Turn,
    TurnStatus,
};
use haarness_server::{NativeToolRegistry, WorkflowRuntimeAdapter};
use haarness_store::SqliteStore;
use haarness_workflows::{HostCall, HostRequestMethod, WorkflowRuntimeHost as _};
use serde_json::json;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

const CHILDREN: usize = 4;

/// Logs each child request as it goes out and holds the first child's
/// response until the test releases it. `!tasks <n>` makes a root turn
/// delegate `n` tasks in one round.
struct Fanout {
    explicit: bool,
    log: Arc<StdMutex<Vec<String>>>,
    held: AtomicBool,
    release: Arc<Semaphore>,
}

impl Fanout {
    fn new(explicit: bool) -> Arc<Self> {
        Arc::new(Self {
            explicit,
            log: Arc::default(),
            held: AtomicBool::new(false),
            release: Arc::new(Semaphore::new(0)),
        })
    }

    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    async fn wait_for_sends(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while self
                .log()
                .iter()
                .filter(|entry| entry.starts_with("send"))
                .count()
                < count
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("expected {count} sends, saw {:?}", self.log()));
    }
}

fn reply(text: String) -> ProviderStream {
    Box::pin(stream::iter(vec![
        Ok(ProviderEvent::TextDelta(text)),
        Ok(ProviderEvent::Finished {
            reason: "stop".into(),
        }),
    ]))
}

#[async_trait]
impl ModelProvider for Fanout {
    fn id(&self) -> &str {
        "fanout"
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "fanout".into(),
            name: "Fan-out fixture".into(),
            authenticated: true,
            models: vec![ModelInfo {
                id: "fixture".into(),
                display_name: "Fixture".into(),
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

    fn explicit_prompt_cache_scope(&self, _model: &str) -> Option<String> {
        self.explicit.then(|| "fixture-account".into())
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        let prompt = request
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                ProviderMessage::Text { role, content } if role == "user" => Some(content.clone()),
                _ => None,
            })
            .unwrap_or_default();
        if let Some(count) = prompt.strip_prefix("!tasks ") {
            if request
                .messages
                .iter()
                .any(|message| matches!(message, ProviderMessage::ToolResult { .. }))
            {
                return Ok(reply("children finished".into()));
            }
            let calls = (0..count.trim().parse::<usize>().unwrap())
                .map(|index| {
                    Ok(ProviderEvent::ToolCall {
                        call_id: format!("task-{index}"),
                        name: "task".into(),
                        arguments: json!({
                            "prompt": format!("child {index}"),
                            "description": format!("Child {index}"),
                            "agent": "general",
                        }),
                        provider_metadata: serde_json::Value::Null,
                    })
                })
                .chain([Ok(ProviderEvent::Finished {
                    reason: "tool_calls".into(),
                })])
                .collect::<Vec<_>>();
            return Ok(Box::pin(stream::iter(calls)));
        }
        self.log.lock().unwrap().push(format!("send {prompt}"));
        if self.held.swap(true, Ordering::SeqCst) {
            return Ok(reply(format!("done {prompt}")));
        }
        let (log, release) = (Arc::clone(&self.log), Arc::clone(&self.release));
        Ok(Box::pin(
            stream::once(async move {
                release.acquire().await.unwrap().forget();
                log.lock().unwrap().push(format!("respond {prompt}"));
                Ok(ProviderEvent::TextDelta(format!("done {prompt}")))
            })
            .chain(stream::iter([Ok(ProviderEvent::Finished {
                reason: "stop".into(),
            })])),
        ))
    }
}

/// The runtime's workspace folder, removed when the test ends, also when it fails.
struct Workspace(std::path::PathBuf);

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn runtime(provider: &Arc<Fanout>) -> (Arc<AgentRuntime>, Workspace) {
    let workspace =
        std::env::temp_dir().join(format!("haarness-prefix-warmup-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = Arc::new(AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new([
            Arc::clone(provider) as Arc<dyn ModelProvider>
        ])),
        Arc::new(NativeToolRegistry::new(workspace.clone())),
        EventBus::new(1024),
        workspace.clone(),
        String::new(),
        RuntimeConfig::default(),
    ));
    (runtime, Workspace(workspace))
}

async fn root_thread(runtime: &AgentRuntime) -> ThreadId {
    runtime
        .create_thread(CreateThreadRequest {
            title: Some("Fixture".into()),
            provider: Some("fanout".into()),
            model: Some("fixture".into()),
            ..CreateThreadRequest::default()
        })
        .await
        .unwrap()
        .id
}

async fn start(runtime: &AgentRuntime, thread: ThreadId, text: &str) -> Turn {
    runtime
        .start_turn(
            thread,
            StartTurnRequest {
                request_id: RequestId::new(),
                text: text.into(),
                attachments: vec![],
                resources: Vec::new(),
                synthetic: false,
                auto_approve: false,
                agent: None,
                provider: None,
                model: None,
                reasoning_effort: None,
                tools: None,
                command: None,
            },
        )
        .await
        .unwrap()
}

async fn wait_done(runtime: &AgentRuntime, turn: &Turn) -> TurnStatus {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let current = runtime.turn(turn.id).await.unwrap();
            if !matches!(current.status, TurnStatus::Queued | TurnStatus::Running) {
                return current.status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("turn finished")
}

/// One leader went first; every sibling went only after its response began.
fn assert_one_leader(log: &[String]) {
    assert_eq!(log.len(), CHILDREN + 1, "{log:?}");
    let leader = log[0].strip_prefix("send ").expect("a send first");
    assert_eq!(log[1], format!("respond {leader}"), "{log:?}");
    assert!(
        log[2..].iter().all(|entry| entry.starts_with("send ")),
        "{log:?}"
    );
}

#[tokio::test]
async fn a_task_batch_sends_one_child_first_and_the_rest_once_its_response_begins() {
    let provider = Fanout::new(true);
    let (runtime, _workspace) = runtime(&provider);
    let thread = root_thread(&runtime).await;
    let turn = start(&runtime, thread, &format!("!tasks {CHILDREN}")).await;
    provider.wait_for_sends(1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(provider.log().len(), 1, "siblings must wait for the leader");
    provider.release.add_permits(1);
    assert_eq!(wait_done(&runtime, &turn).await, TurnStatus::Completed);
    assert_one_leader(&provider.log());
}

#[tokio::test]
async fn a_workflow_fan_out_sends_one_agent_first_and_the_rest_once_its_response_begins() {
    let provider = Fanout::new(true);
    let (runtime, _workspace) = runtime(&provider);
    let root = root_thread(&runtime).await;
    let host = WorkflowRuntimeAdapter::new();
    host.attach(&runtime);
    let run_thread = host
        .create_run_thread(Some(root), "Workflow · fan-out fixture")
        .await
        .unwrap();
    let agents = (0..CHILDREN)
        .map(|index| {
            let host = Arc::clone(&host);
            tokio::spawn(async move {
                host.request(
                    run_thread,
                    HostCall {
                        run_id: "fan-out".into(),
                        request_id: index as u64,
                        method: HostRequestMethod::Agent,
                        params: json!({"prompt": format!("child {index}")}),
                        cancellation: CancellationToken::new(),
                    },
                )
                .await
            })
        })
        .collect::<Vec<_>>();
    provider.wait_for_sends(1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(provider.log().len(), 1, "siblings must wait for the leader");
    provider.release.add_permits(1);
    for agent in agents {
        let reply = tokio::time::timeout(Duration::from_secs(10), agent)
            .await
            .expect("agent finished")
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(reply["text"].as_str().unwrap().starts_with("done child"));
    }
    assert_one_leader(&provider.log());
}

#[tokio::test]
async fn children_of_a_provider_without_explicit_caching_all_go_at_once() {
    let provider = Fanout::new(false);
    let (runtime, _workspace) = runtime(&provider);
    let thread = root_thread(&runtime).await;
    let turn = start(&runtime, thread, &format!("!tasks {CHILDREN}")).await;
    // The first child's response is still held, and nobody waits on it.
    provider.wait_for_sends(CHILDREN).await;
    provider.release.add_permits(1);
    assert_eq!(wait_done(&runtime, &turn).await, TurnStatus::Completed);
}

#[tokio::test]
async fn separate_conversations_never_wait_on_each_other() {
    let provider = Fanout::new(true);
    let (runtime, _workspace) = runtime(&provider);
    let (first, second) = (root_thread(&runtime).await, root_thread(&runtime).await);
    let first = start(&runtime, first, "child a").await;
    provider.wait_for_sends(1).await;
    let second = start(&runtime, second, "child b").await;
    provider.wait_for_sends(2).await;
    provider.release.add_permits(1);
    assert_eq!(wait_done(&runtime, &first).await, TurnStatus::Completed);
    assert_eq!(wait_done(&runtime, &second).await, TurnStatus::Completed);
}
