//! A provider that refuses an image must not break the chat: the image is
//! left out of every later request, durably, and the request is retried once.
//! A chat with more images than a provider takes in one request sends the
//! newest of them.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::stream;
use haarness_core::{
    AgentRuntime, CoreError, CoreResult, EventBus, ImageLimits, ModelProvider, ProviderEvent,
    ProviderMessage, ProviderRequest, ProviderStream, RuntimeConfig, StaticProviderRegistry,
};
use haarness_protocol::{
    CreateThreadRequest, ItemData, ProviderInfo, RequestId, StartTurnRequest, ThreadId, Turn,
    TurnStatus,
};
use haarness_server::NativeToolRegistry;
use haarness_store::SqliteStore;
use tokio_util::sync::CancellationToken;

/// A valid 1×1 PNG.
const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

/// Refuses every request that carries an image, the way Anthropic refuses
/// one over its size limit, once `refusing` is set; or, with `most`, every
/// request with more images than that.
#[derive(Default)]
struct Picky {
    refusing: AtomicBool,
    most: Mutex<Option<usize>>,
    /// The image count it tells the runtime it takes.
    declared: Mutex<Option<usize>>,
    error: Mutex<String>,
    requests: Mutex<Vec<ProviderRequest>>,
}

fn images(request: &ProviderRequest) -> usize {
    request
        .messages
        .iter()
        .map(|message| match message {
            ProviderMessage::Media { attachments, .. }
            | ProviderMessage::ToolResult { attachments, .. } => attachments.len(),
            _ => 0,
        })
        .sum()
}

#[async_trait]
impl ModelProvider for Picky {
    fn id(&self) -> &str {
        "fixture"
    }
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "fixture".into(),
            name: "Image fixture".into(),
            authenticated: true,
            models: vec![],
        }
    }
    fn image_limits(&self, _model: &str) -> ImageLimits {
        ImageLimits {
            count: *self.declared.lock().unwrap(),
            ..ImageLimits::default()
        }
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        _: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        let refuse = (self.refusing.load(Ordering::SeqCst) && images(&request) > 0)
            || self
                .most
                .lock()
                .unwrap()
                .is_some_and(|most| images(&request) > most);
        self.requests.lock().unwrap().push(request);
        if refuse {
            let error = CoreError::Provider(self.error.lock().unwrap().clone());
            return Ok(Box::pin(stream::iter(vec![Err(error)])));
        }
        Ok(Box::pin(stream::iter(
            [
                ProviderEvent::TextDelta("Answered".into()),
                ProviderEvent::Finished {
                    reason: "done".into(),
                },
            ]
            .into_iter()
            .map(Ok),
        )))
    }
}

struct Fixture {
    root: PathBuf,
    provider: Arc<Picky>,
}

impl Fixture {
    fn new(error: &str) -> Self {
        let root = std::env::temp_dir().join(format!("haarness-images-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let provider = Arc::new(Picky::default());
        error.clone_into(&mut provider.error.lock().unwrap());
        Self { root, provider }
    }

    /// A runtime over the same database file, as after a server restart.
    fn runtime(&self) -> AgentRuntime {
        AgentRuntime::with_config(
            Arc::new(SqliteStore::open(self.root.join("state.db")).unwrap()),
            Arc::new(StaticProviderRegistry::new(vec![
                self.provider.clone() as Arc<dyn ModelProvider>
            ])),
            Arc::new(NativeToolRegistry::new(self.root.clone())),
            EventBus::new(128),
            self.root.clone(),
            String::new(),
            RuntimeConfig::default(),
        )
    }

    fn requests(&self) -> Vec<ProviderRequest> {
        std::mem::take(&mut *self.provider.requests.lock().unwrap())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

async fn thread(runtime: &AgentRuntime) -> ThreadId {
    runtime
        .create_thread(CreateThreadRequest {
            title: Some("Image recovery".into()),
            provider: Some("fixture".into()),
            model: Some("fixture".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id
}

async fn turn(
    runtime: &AgentRuntime,
    thread: ThreadId,
    text: &str,
    attachments: Vec<String>,
) -> Turn {
    let turn = runtime
        .start_turn(
            thread,
            StartTurnRequest {
                request_id: RequestId::new(),
                text: text.into(),
                attachments,
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
    tokio::time::timeout(Duration::from_secs(10), async {
        while runtime.active_turn(thread).await.unwrap().is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the turn finishes");
    runtime.turn(turn.id).await.unwrap()
}

fn image(name: &str) -> String {
    format!("data:image/png;name={name};base64,{PNG}")
}

fn omissions(items: &[haarness_protocol::Item]) -> Vec<(Vec<String>, bool)> {
    items
        .iter()
        .filter_map(|item| match &item.data {
            ItemData::MediaOmitted { names, guessed, .. } => Some((names.clone(), *guessed)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_refused_image_is_left_out_retried_once_and_stays_out_after_a_restart() {
    let fixture = Fixture::new(
        "HTTP 400 Bad Request: messages.0.content.1.image.source.base64: image exceeds 5 MB maximum: 7340032 bytes > 5242880 bytes",
    );
    fixture.provider.refusing.store(true, Ordering::SeqCst);
    let runtime = fixture.runtime();
    let thread = thread(&runtime).await;

    let first = turn(&runtime, thread, "Look at this", vec![image("shot.png")]).await;
    assert_eq!(first.status, TurnStatus::Completed, "{:?}", first.error);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 2, "refused once, then retried once");
    assert_eq!(images(&requests[0]), 1);
    assert_eq!(images(&requests[1]), 0);
    let ProviderMessage::Text { content, .. } = &requests[1].messages[0] else {
        panic!("{:?}", requests[1].messages[0]);
    };
    assert!(content.contains("[Image shot.png omitted"), "{content}");
    let items = runtime.items(thread, None).await.unwrap();
    // The error named the image by its place in the request.
    assert_eq!(
        omissions(&items),
        vec![(vec!["shot.png".to_owned()], false)]
    );
    assert!(
        !items
            .iter()
            .any(|item| matches!(item.data, ItemData::Error { .. }))
    );

    let second = turn(&runtime, thread, "And now?", Vec::new()).await;
    assert_eq!(second.status, TurnStatus::Completed, "{:?}", second.error);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(images(&requests[0]), 0);
    let replayed = serde_json::to_string(&requests[0].messages[0]).unwrap();

    drop(runtime);
    let restarted = fixture.runtime();
    let third = turn(&restarted, thread, "Still there?", Vec::new()).await;
    assert_eq!(third.status, TurnStatus::Completed, "{:?}", third.error);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(images(&requests[0]), 0);
    // The same bytes as before the restart, so the prompt cache still holds.
    assert_eq!(
        serde_json::to_string(&requests[0].messages[0]).unwrap(),
        replayed
    );
}

#[tokio::test]
async fn an_unnamed_refusal_leaves_out_the_newest_images_and_retries_only_once() {
    let fixture = Fixture::new("HTTP 400 Bad Request: Could not process image");
    let runtime = fixture.runtime();
    let thread = thread(&runtime).await;
    // The provider took the first image; later it refuses any image.
    let first = turn(&runtime, thread, "One", vec![image("old.png")]).await;
    assert_eq!(first.status, TurnStatus::Completed, "{:?}", first.error);
    fixture.requests();
    fixture.provider.refusing.store(true, Ordering::SeqCst);

    let second = turn(&runtime, thread, "Two", vec![image("new.png")]).await;
    assert_eq!(second.status, TurnStatus::Failed);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 2, "one retry per turn");
    assert_eq!(images(&requests[0]), 2);
    assert_eq!(images(&requests[1]), 1, "the newest image went first");
    let items = runtime.items(thread, None).await.unwrap();
    assert_eq!(
        omissions(&items),
        vec![
            (vec!["new.png".to_owned()], true),
            (vec!["old.png".to_owned()], true)
        ]
    );

    let third = turn(&runtime, thread, "Three", Vec::new()).await;
    assert_eq!(third.status, TurnStatus::Completed, "{:?}", third.error);
    assert_eq!(images(&fixture.requests()[0]), 0);
}

#[tokio::test]
async fn a_request_too_large_for_its_images_leaves_out_the_newest_image_file() {
    let fixture = Fixture::new(
        "HTTP 413 Payload Too Large: Request exceeds the maximum allowed number of bytes.",
    );
    fixture.provider.refusing.store(true, Ordering::SeqCst);
    let runtime = fixture.runtime();
    let thread = thread(&runtime).await;
    let photo = fixture.root.join("photo.png");
    std::fs::write(&photo, base64_png()).unwrap();
    let first = turn(
        &runtime,
        thread,
        "Look",
        vec![photo.to_string_lossy().into_owned()],
    )
    .await;
    assert_eq!(first.status, TurnStatus::Completed, "{:?}", first.error);
    let items = runtime.items(thread, None).await.unwrap();
    assert_eq!(
        omissions(&items),
        vec![(vec!["photo.png".to_owned()], true)]
    );
    // The notice says the request was too large, not that an image was bad.
    assert!(items.iter().any(|item| matches!(
        item.data,
        ItemData::MediaOmitted {
            too_large: true,
            ..
        }
    )));
    assert!(
        !items
            .iter()
            .any(|item| matches!(item.data, ItemData::Compaction { .. }))
    );
}

fn base64_png() -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(PNG)
        .unwrap()
}

/// The names of the images a request carries, in order.
fn sent(request: &ProviderRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .flat_map(|message| match message {
            ProviderMessage::Media { attachments, .. }
            | ProviderMessage::ToolResult { attachments, .. } => attachments.clone(),
            _ => Vec::new(),
        })
        .map(|source| {
            let name = source.split(";name=").nth(1).unwrap_or_default();
            name.split(';').next().unwrap_or_default().to_owned()
        })
        .collect()
}

#[tokio::test]
async fn past_the_providers_image_count_the_oldest_are_noted_and_the_newest_sent() {
    let fixture = Fixture::new("HTTP 400 Bad Request: too many images in the request: 21 > 20");
    *fixture.provider.most.lock().unwrap() = Some(20);
    *fixture.provider.declared.lock().unwrap() = Some(20);
    let runtime = fixture.runtime();
    let thread = thread(&runtime).await;
    for index in 1..=20 {
        let name = format!("shot-{index:02}.png");
        let done = turn(
            &runtime,
            thread,
            &format!("Look {index}"),
            vec![image(&name)],
        )
        .await;
        assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
    }
    assert!(
        fixture
            .requests()
            .iter()
            .all(|request| images(request) <= 20)
    );

    // The 21st screenshot passes the count: the oldest nine are noted, and
    // the newest twelve, this one among them, are sent at once.
    let done = turn(&runtime, thread, "Look 21", vec![image("shot-21.png")]).await;
    assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 1, "no request was refused");
    let expected = (10..=21)
        .map(|index| format!("shot-{index:02}.png"))
        .collect::<Vec<_>>();
    assert_eq!(sent(&requests[0]), expected);
    let ProviderMessage::Text { content, .. } = &requests[0].messages[0] else {
        panic!("{:?}", requests[0].messages[0]);
    };
    assert_eq!(
        content,
        "Look 1\n[Image shot-01.png omitted to keep the request within the provider's limits. It is no longer visible: do not describe it from memory, and ask the person to attach it again if it is needed.]"
    );
    let noted = serde_json::to_string(&requests[0].messages[..36]).unwrap();

    // The next turns carry the same noted history, byte for byte, until
    // the kept images pass the count again.
    let done = turn(&runtime, thread, "Look 22", vec![image("shot-22.png")]).await;
    assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
    let requests = fixture.requests();
    assert_eq!(sent(&requests[0]).len(), 13);
    assert_eq!(
        serde_json::to_string(&requests[0].messages[..36]).unwrap(),
        noted
    );
    let items = runtime.items(thread, None).await.unwrap();
    assert!(omissions(&items).is_empty(), "nothing is left out for good");
}

#[tokio::test]
async fn a_request_refused_for_its_number_of_images_leaves_out_the_oldest() {
    let fixture = Fixture::new("HTTP 400 Bad Request: too many images in the request: 5 > 4");
    *fixture.provider.most.lock().unwrap() = Some(4);
    let runtime = fixture.runtime();
    let thread = thread(&runtime).await;
    for name in ["a", "b", "c", "d"] {
        let done = turn(&runtime, thread, name, vec![image(&format!("{name}.png"))]).await;
        assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
    }
    fixture.requests();

    let done = turn(&runtime, thread, "e", vec![image("e.png")]).await;
    assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 2, "refused once, then retried once");
    // The oldest half went; the newest image reached the model.
    assert_eq!(sent(&requests[1]), ["d.png", "e.png"]);
    let items = runtime.items(thread, None).await.unwrap();
    let omitted = items
        .iter()
        .filter_map(|item| match &item.data {
            ItemData::MediaOmitted {
                names,
                guessed,
                oldest,
                ..
            } => Some((names.clone(), *guessed, *oldest)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        omitted,
        vec![(
            vec!["a.png".to_owned(), "b.png".to_owned(), "c.png".to_owned()],
            false,
            true
        )]
    );

    // The next screenshot is sent too.
    let done = turn(&runtime, thread, "f", vec![image("f.png")]).await;
    assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
    assert_eq!(sent(&fixture.requests()[0]), ["d.png", "e.png", "f.png"]);
}

#[tokio::test]
async fn geminis_size_refusal_is_recorded_as_a_request_too_large() {
    let fixture = Fixture::new(
        r#"HTTP 400 Bad Request: {"error":{"code":400,"message":"Request payload size exceeds the limit: 20971520 bytes.","status":"INVALID_ARGUMENT"}}"#,
    );
    *fixture.provider.most.lock().unwrap() = Some(3);
    let runtime = fixture.runtime();
    let thread = thread(&runtime).await;
    for name in ["a", "b", "c", "d"] {
        let done = turn(&runtime, thread, name, vec![image(&format!("{name}.png"))]).await;
        assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
    }
    let requests = fixture.requests();
    assert_eq!(sent(requests.last().unwrap()), ["c.png", "d.png"]);
    let items = runtime.items(thread, None).await.unwrap();
    let omitted = items
        .iter()
        .filter_map(|item| match &item.data {
            ItemData::MediaOmitted {
                names,
                oldest,
                too_large,
                ..
            } => Some((names.clone(), *oldest, *too_large)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        omitted,
        vec![(vec!["a.png".to_owned(), "b.png".to_owned()], true, true)]
    );
}

#[tokio::test]
async fn a_count_refusal_the_turns_own_images_answer_for_leaves_them_out_as_too_many() {
    let fixture = Fixture::new("HTTP 400 Bad Request: too many images in the request: 3 > 1");
    *fixture.provider.most.lock().unwrap() = Some(1);
    let runtime = fixture.runtime();
    let thread = thread(&runtime).await;
    let done = turn(&runtime, thread, "a", vec![image("a.png")]).await;
    assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
    // Two of the three images are the turn's own: leaving out the older one
    // would not answer the refusal, so the newest go and the older stays.
    let done = turn(
        &runtime,
        thread,
        "b and c",
        vec![image("b.png"), image("c.png")],
    )
    .await;
    assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
    assert_eq!(sent(fixture.requests().last().unwrap()), ["a.png"]);
    let items = runtime.items(thread, None).await.unwrap();
    let omitted = items
        .iter()
        .filter_map(|item| match &item.data {
            ItemData::MediaOmitted {
                names,
                guessed,
                oldest,
                too_many,
                ..
            } => Some((names.clone(), *guessed, *oldest, *too_many)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        omitted,
        vec![(
            vec!["b.png".to_owned(), "c.png".to_owned()],
            true,
            false,
            true
        )]
    );
}
