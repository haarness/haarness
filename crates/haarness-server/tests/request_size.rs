//! A request too large in bytes for its provider must not loop. PDFs and
//! other media count toward the request's budget the way images do, so the
//! earlier ones are noted before the request is sent; a refusal for the
//! request's bytes compacts the chat once, and then the turn ends with an
//! error that says what to do.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::stream;
use haarness_core::{
    AgentRuntime, CoreError, CoreResult, EventBus, ModelProvider, ProviderEvent, ProviderMessage,
    ProviderRequest, ProviderStream, RuntimeConfig, StaticProviderRegistry,
};
use haarness_protocol::{
    CreateThreadRequest, ItemData, ProviderInfo, RequestId, StartTurnRequest, ThreadId, Turn,
    TurnStatus,
};
use haarness_server::NativeToolRegistry;
use haarness_store::SqliteStore;
use serde_json::json;
use tokio_util::sync::CancellationToken;

const MIB: u64 = 1024 * 1024;

/// What Anthropic says to a request over its 32 MB.
const TOO_LARGE: &str =
    "HTTP 413 Payload Too Large: Request exceeds the maximum allowed number of bytes.";

/// Refuses a request whose media pass 32 MB, as Anthropic does, or every
/// request once `refusing` holds an error: as the stream's first item, or
/// with `early` before any stream. It reads a file the person names as
/// `read <name>` with the read tool, then answers.
#[derive(Default)]
struct Sized {
    refusing: Mutex<Option<String>>,
    early: AtomicBool,
    requests: Mutex<Vec<ProviderRequest>>,
    summaries: Mutex<usize>,
}

fn attachments(message: &ProviderMessage) -> &[String] {
    match message {
        ProviderMessage::Media { attachments, .. }
        | ProviderMessage::ToolResult { attachments, .. } => attachments,
        _ => &[],
    }
}

/// The base64 bytes of a request's media.
fn media_bytes(request: &ProviderRequest) -> usize {
    request
        .messages
        .iter()
        .flat_map(attachments)
        .map(|source| haarness_core::attachments::sent_bytes(source))
        .sum()
}

/// The names of the files a request carries, in order.
fn sent(request: &ProviderRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .flat_map(attachments)
        .map(|source| haarness_core::attachments::display_name(source))
        .collect()
}

fn is_summary(request: &ProviderRequest) -> bool {
    matches!(
        request.messages.last(),
        Some(ProviderMessage::Text { role, content })
            if role == "user" && content.starts_with("Summarize this conversation")
    )
}

fn events(mut events: Vec<ProviderEvent>) -> ProviderStream {
    events.push(ProviderEvent::Finished {
        reason: "done".into(),
    });
    Box::pin(stream::iter(events.into_iter().map(Ok)))
}

#[async_trait]
impl ModelProvider for Sized {
    fn id(&self) -> &str {
        "fixture"
    }
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "fixture".into(),
            name: "Size fixture".into(),
            authenticated: true,
            models: vec![],
        }
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        _: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if is_summary(&request) {
            *self.summaries.lock().unwrap() += 1;
            return Ok(events(vec![ProviderEvent::TextDelta(
                "What happened".into(),
            )]));
        }
        let refusal = self
            .refusing
            .lock()
            .unwrap()
            .clone()
            .or_else(|| (media_bytes(&request) > 32_000_000).then(|| TOO_LARGE.to_owned()));
        let last = request.messages.last().cloned();
        self.requests.lock().unwrap().push(request);
        if let Some(refusal) = refusal {
            if self.early.load(Ordering::SeqCst) {
                return Err(CoreError::Provider(refusal));
            }
            return Ok(Box::pin(stream::iter(vec![Err(CoreError::Provider(
                refusal,
            ))])));
        }
        let wanted = match &last {
            Some(ProviderMessage::Text { role, content }) if role == "user" => content
                .lines()
                .find_map(|line| line.strip_prefix("read "))
                .map(str::to_owned),
            _ => None,
        };
        Ok(match wanted {
            Some(path) => events(vec![ProviderEvent::ToolCall {
                call_id: format!("read-{path}"),
                name: "read".into(),
                arguments: json!({ "path": path }),
                provider_metadata: serde_json::Value::Null,
            }]),
            None => events(vec![ProviderEvent::TextDelta("Answered".into())]),
        })
    }
}

struct Fixture {
    root: PathBuf,
    provider: Arc<Sized>,
    runtime: AgentRuntime,
}

impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("haarness-request-size-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let provider = Arc::new(Sized::default());
        let runtime = AgentRuntime::with_config(
            Arc::new(SqliteStore::open(root.join("state.db")).unwrap()),
            Arc::new(StaticProviderRegistry::new(vec![
                provider.clone() as Arc<dyn ModelProvider>
            ])),
            Arc::new(NativeToolRegistry::new(root.clone())),
            EventBus::new(256),
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

    /// A three-page PDF of `mib` MiB in the workspace: large in bytes, small
    /// in tokens, as a scan is.
    fn pdf(&self, name: &str, mib: u64) -> String {
        use std::io::Write as _;
        let path = self.root.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(b"%PDF-1.7\n").unwrap();
        for page in 1..=3 {
            writeln!(file, "{page} 0 obj << /Type /Page >> endobj").unwrap();
        }
        file.set_len(mib * MIB).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn requests(&self) -> Vec<ProviderRequest> {
        std::mem::take(&mut *self.provider.requests.lock().unwrap())
    }

    fn summaries(&self) -> usize {
        std::mem::take(&mut *self.provider.summaries.lock().unwrap())
    }

    async fn thread(&self) -> ThreadId {
        self.runtime
            .create_thread(CreateThreadRequest {
                title: Some("Request size".into()),
                provider: Some("fixture".into()),
                model: Some("fixture".into()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
    }

    async fn turn(&self, thread: ThreadId, text: &str, attachments: Vec<String>) -> Turn {
        let turn = self
            .runtime
            .start_turn(
                thread,
                StartTurnRequest {
                    request_id: RequestId::new(),
                    text: text.into(),
                    attachments,
                    ..Default::default()
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
        self.runtime.turn(turn.id).await.unwrap()
    }

    async fn compactions(&self, thread: ThreadId) -> usize {
        self.runtime
            .items(thread, None)
            .await
            .unwrap()
            .iter()
            .filter(|item| matches!(item.data, ItemData::Compaction { .. }))
            .count()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn root_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

#[tokio::test]
async fn two_large_pdfs_the_model_read_in_consecutive_turns_fit_in_one_request() {
    let fixture = Fixture::new();
    let thread = fixture.thread().await;
    let first = fixture.pdf("first.pdf", 15);
    let second = fixture.pdf("second.pdf", 15);

    let done = fixture
        .turn(thread, &format!("read {}", root_name(&first)), Vec::new())
        .await;
    assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
    fixture.requests();

    // 2 × 20 MiB of base64 pass 32 MB; the older PDF is noted instead.
    let done = fixture
        .turn(thread, &format!("read {}", root_name(&second)), Vec::new())
        .await;
    assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 2, "the read, then the answer: none refused");
    assert_eq!(sent(&requests[1]), ["second.pdf"]);
    // The model reads where the older one went and how to see it again.
    let read = requests[1]
        .messages
        .iter()
        .find_map(|message| match message {
            ProviderMessage::ToolResult {
                call_id, output, ..
            } if call_id == "read-first.pdf" => Some(output),
            _ => None,
        })
        .unwrap();
    assert!(
        read.starts_with("PDF read successfully: ")
            && read.contains("\n[PDF /")
            && read.ends_with("/first.pdf omitted to keep the request within the provider's limits. It is no longer visible: do not describe it from memory, and read the file again if it is needed.]"),
        "{read}"
    );
    assert_eq!(fixture.summaries(), 0);
    assert_eq!(fixture.compactions(thread).await, 0);

    // The next turn carries the same cut.
    let done = fixture.turn(thread, "Thanks", Vec::new()).await;
    assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(sent(&requests[0]), ["second.pdf"]);
    assert_eq!(fixture.summaries(), 0);
}

#[tokio::test]
async fn two_large_pdfs_attached_in_consecutive_turns_fit_in_one_request() {
    let fixture = Fixture::new();
    let thread = fixture.thread().await;
    let first = fixture.pdf("first.pdf", 15);
    let second = fixture.pdf("second.pdf", 15);

    let done = fixture
        .turn(thread, "Summarise this", vec![first.clone()])
        .await;
    assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
    fixture.requests();

    let done = fixture.turn(thread, "And this one", vec![second]).await;
    assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 1, "none refused");
    // The PDF the person just attached reaches the model.
    assert_eq!(sent(&requests[0]), ["second.pdf"]);
    let ProviderMessage::Text { content, .. } = &requests[0].messages[0] else {
        panic!("{:?}", requests[0].messages[0]);
    };
    assert_eq!(
        content,
        &format!(
            "Summarise this\n[PDF {first} omitted to keep the request within the provider's limits. It is no longer visible: do not describe it from memory, and read the file again if it is needed.]"
        )
    );
    assert_eq!(fixture.summaries(), 0);
    assert_eq!(fixture.compactions(thread).await, 0);
}

#[tokio::test]
async fn a_request_still_too_large_after_compacting_ends_the_turn_with_one_summary() {
    // Refused as the stream's first item, and before any stream.
    for early in [false, true] {
        let fixture = Fixture::new();
        fixture.provider.early.store(early, Ordering::SeqCst);
        let thread = fixture.thread().await;
        let done = fixture.turn(thread, "Hello", Vec::new()).await;
        assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
        fixture.requests();

        *fixture.provider.refusing.lock().unwrap() = Some(TOO_LARGE.into());
        let done = fixture.turn(thread, "Again", Vec::new()).await;
        assert_eq!(done.status, TurnStatus::Failed);
        assert_eq!(fixture.summaries(), 1, "compacted once");
        assert_eq!(fixture.requests().len(), 2, "refused before and after");
        assert_eq!(fixture.compactions(thread).await, 1);
        let error = done.error.unwrap();
        assert_eq!(
            error,
            "Size fixture refused this request as too large, even after the chat was compacted. Send smaller or fewer files, or continue in a new chat."
        );
        let items = fixture.runtime.items(thread, None).await.unwrap();
        let errors = items
            .iter()
            .filter_map(|item| match &item.data {
                ItemData::Error { message, .. } => Some(message.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(errors, [error.as_str()]);

        // The next turn tries once more, no more than that.
        let done = fixture.turn(thread, "Once more", Vec::new()).await;
        assert_eq!(done.status, TurnStatus::Failed);
        assert_eq!(fixture.summaries(), 1);
        assert_eq!(fixture.requests().len(), 2);
    }
}

#[tokio::test]
async fn a_context_too_long_in_tokens_still_compacts_up_to_four_times() {
    let fixture = Fixture::new();
    let thread = fixture.thread().await;
    *fixture.provider.refusing.lock().unwrap() =
        Some("HTTP 400 Bad Request: prompt is too long: 210000 tokens > 200000 maximum".into());
    let done = fixture.turn(thread, "Hello", Vec::new()).await;
    assert_eq!(done.status, TurnStatus::Failed);
    assert_eq!(fixture.summaries(), 4);
    assert!(
        done.error.unwrap().contains("prompt is too long"),
        "the provider's own words"
    );
}
