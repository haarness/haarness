//! A provider that falls silent must not hold a turn forever: the request it
//! stopped answering is closed before the next one is sent, a silence before
//! any output is retried, and one after partial output continues from it.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures::{StreamExt as _, stream};
use haarness_core::{
    AgentRuntime, CoreResult, EventBus, ModelProvider, ProviderEvent, ProviderMessage,
    ProviderRequest, ProviderStream, RuntimeConfig, StaticProviderRegistry, stream_stall,
};
use haarness_protocol::{
    CreateThreadRequest, Item, ItemData, ProviderInfo, RequestId, StartTurnRequest, ThreadId, Turn,
    TurnStatus,
};
use haarness_server::NativeToolRegistry;
use haarness_store::SqliteStore;
use tokio_util::sync::CancellationToken;

/// Notes when the stream holding it is dropped, which closes the request in
/// a real provider.
struct Closed(Arc<Mutex<Option<Instant>>>);

impl Drop for Closed {
    fn drop(&mut self) {
        *self.0.lock().unwrap() = Some(Instant::now());
    }
}

/// The first request streams `before` and then stalls with the connection
/// open; later requests answer. Each request records how long before it the
/// stalled one was closed.
struct Silent {
    before: Vec<ProviderEvent>,
    closed: Arc<Mutex<Option<Instant>>>,
    requests: Mutex<Vec<(ProviderRequest, Option<Duration>)>>,
}

impl Silent {
    fn new(before: Vec<ProviderEvent>) -> Self {
        Self {
            before,
            closed: Arc::new(Mutex::new(None)),
            requests: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ModelProvider for Silent {
    fn id(&self) -> &str {
        "fixture"
    }
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "fixture".into(),
            name: "Silent fixture".into(),
            authenticated: true,
            models: vec![],
        }
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        _: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        let mut requests = self.requests.lock().unwrap();
        let closed = self.closed.lock().unwrap().map(|at| at.elapsed());
        requests.push((request, closed));
        if requests.len() == 1 {
            let guard = Closed(Arc::clone(&self.closed));
            let events = self
                .before
                .clone()
                .into_iter()
                .map(Ok)
                .chain([Err(stream_stall(Duration::from_secs(300)))]);
            return Ok(Box::pin(stream::iter(events).chain(stream::pending()).map(
                move |event| {
                    let _ = &guard;
                    event
                },
            )));
        }
        Ok(Box::pin(stream::iter(
            [
                ProviderEvent::TextDelta("Answered".into()),
                ProviderEvent::Usage {
                    input_tokens: 40,
                    output_tokens: 2,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                },
                ProviderEvent::Finished {
                    reason: "stop".into(),
                },
            ]
            .into_iter()
            .map(Ok),
        )))
    }
}

struct Fixture {
    root: PathBuf,
    provider: Arc<Silent>,
    runtime: AgentRuntime,
}

impl Fixture {
    fn new(before: Vec<ProviderEvent>) -> Self {
        let root = std::env::temp_dir().join(format!("haarness-stall-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let provider = Arc::new(Silent::new(before));
        let runtime = AgentRuntime::with_config(
            Arc::new(SqliteStore::open(root.join("state.db")).unwrap()),
            Arc::new(StaticProviderRegistry::new(vec![
                provider.clone() as Arc<dyn ModelProvider>
            ])),
            Arc::new(NativeToolRegistry::new(root.clone())),
            EventBus::new(128),
            root.clone(),
            String::new(),
            RuntimeConfig::default(),
        );
        Self {
            root,
            provider,
            runtime,
        }
    }

    async fn turn(&self) -> (ThreadId, Turn) {
        let thread = self
            .runtime
            .create_thread(CreateThreadRequest {
                title: Some("Stall".into()),
                provider: Some("fixture".into()),
                model: Some("fixture".into()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id;
        let turn = self
            .runtime
            .start_turn(
                thread,
                StartTurnRequest {
                    request_id: RequestId::new(),
                    text: "Hello".into(),
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
                },
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            while self.runtime.active_turn(thread).await.unwrap().is_some() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the turn finishes");
        (thread, self.runtime.turn(turn.id).await.unwrap())
    }

    fn requests(&self) -> Vec<(ProviderRequest, Option<Duration>)> {
        std::mem::take(&mut *self.provider.requests.lock().unwrap())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Each usage record, as whether it is a local estimate.
fn usage(items: &[Item]) -> Vec<bool> {
    items
        .iter()
        .filter_map(|item| match item.data {
            ItemData::Usage { estimated, .. } => Some(estimated),
            _ => None,
        })
        .collect()
}

fn answers(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match &item.data {
            ItemData::AssistantMessage { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn mentions_recovery(request: &ProviderRequest) -> bool {
    request.messages.iter().any(|message| {
        matches!(message, ProviderMessage::Text { content, .. }
            if content.contains("The previous provider response ended before completion"))
    })
}

#[tokio::test]
async fn a_silence_before_any_output_closes_the_request_and_retries_it() {
    let fixture = Fixture::new(Vec::new());
    let (thread, turn) = fixture.turn().await;
    assert_eq!(turn.status, TurnStatus::Completed, "{:?}", turn.error);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 2, "stalled once, then retried once");
    assert_eq!(requests[0].1, None);
    // Closed when it stalled, not when the retry replaced it two seconds of
    // backoff later.
    let closed = requests[1].1.expect("the stalled request was closed");
    assert!(closed >= Duration::from_millis(1_500), "{closed:?}");
    assert!(
        !mentions_recovery(&requests[1].0),
        "nothing to continue from"
    );
    let items = fixture.runtime.items(thread, None).await.unwrap();
    assert_eq!(answers(&items), ["Answered"]);
    // The silent attempt streamed nothing, so only the answer is counted.
    assert_eq!(usage(&items), [false]);
    assert!(
        !items
            .iter()
            .any(|item| matches!(item.data, ItemData::Error { .. }))
    );
}

#[tokio::test]
async fn a_silence_after_partial_output_keeps_it_and_continues_from_it() {
    let fixture = Fixture::new(vec![ProviderEvent::TextDelta("Partial".into())]);
    let (thread, turn) = fixture.turn().await;
    assert_eq!(turn.status, TurnStatus::Completed, "{:?}", turn.error);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 2, "stalled once, then continued once");
    // Closed before the recovery's 250 ms pause, not after it.
    let closed = requests[1].1.expect("the stalled request was closed");
    assert!(closed >= Duration::from_millis(200), "{closed:?}");
    assert!(mentions_recovery(&requests[1].0));
    let items = fixture.runtime.items(thread, None).await.unwrap();
    assert_eq!(answers(&items), ["Partial", "Answered"]);
    // One record per request: the stalled one estimated from what it
    // streamed, the continuation as the provider reported it.
    assert_eq!(usage(&items), [true, false]);
}
