//! A request too large in bytes for its provider must not loop. PDFs and
//! other media count toward what the request carries, so earlier turns'
//! are noted before it is sent, while nothing the model read in the turn is,
//! and what fit in one request before still goes whole. A refusal for the
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
    AgentRuntime, CoreError, CoreResult, EventBus, ImageLimits, ModelProvider, ProviderEvent,
    ProviderMessage, ProviderRequest, ProviderStream, RuntimeConfig, StaticProviderRegistry,
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

/// Refuses a request whose media pass 32 MB and states that size, as
/// Anthropic does; with `unstated`, it takes 100 MB and states nothing, as
/// the Gemini API does. It also refuses every request once `refusing` holds
/// an error, and the requests `refused` names by their place: as the
/// stream's first item, or with `early` before any stream. It reads each
/// file the person names as `read <name>` with the read tool, in turn, then
/// answers. Told that a file it read was left out, it reads it again, as a
/// model follows the note.
#[derive(Default)]
struct Sized {
    unstated: AtomicBool,
    refusing: Mutex<Option<String>>,
    refused: Mutex<Vec<usize>>,
    early: AtomicBool,
    requests: Mutex<Vec<ProviderRequest>>,
    summaries: Mutex<usize>,
    reads: Mutex<usize>,
}

const NOTE: &str = "omitted to keep the request within the provider's limits";

/// What the model does next: read a file, or answer.
fn next_read(messages: &[ProviderMessage]) -> Option<String> {
    // A file it read was left out: it reads it again.
    if let Some(ProviderMessage::ToolResult {
        call_id, output, ..
    }) = messages.last()
        && output.contains(NOTE)
    {
        return call_id
            .strip_prefix("read-")
            .and_then(|rest| rest.split('#').next())
            .map(str::to_owned);
    }
    let asked = messages.iter().rposition(|message| {
        matches!(message,
            ProviderMessage::Text { role, content } | ProviderMessage::Media { role, content, .. }
                if role == "user" && content.lines().any(|line| line.starts_with("read ")))
    })?;
    let (ProviderMessage::Text { content, .. } | ProviderMessage::Media { content, .. }) =
        &messages[asked]
    else {
        return None;
    };
    let done = messages[asked..]
        .iter()
        .filter(|message| matches!(message, ProviderMessage::ToolResult { .. }))
        .count();
    content
        .lines()
        .filter_map(|line| line.strip_prefix("read "))
        .nth(done)
        .map(str::to_owned)
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
    fn image_limits(&self, _model: &str) -> ImageLimits {
        ImageLimits {
            request: (!self.unstated.load(Ordering::SeqCst))
                .then_some(32_000_000 - haarness_core::REQUEST_ALLOWANCE),
            ..ImageLimits::default()
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
        let place = self.requests.lock().unwrap().len();
        let refusal = self.refusing.lock().unwrap().clone().or_else(|| {
            let most = if self.unstated.load(Ordering::SeqCst) {
                100_000_000
            } else {
                32_000_000
            };
            (media_bytes(&request) > most || self.refused.lock().unwrap().contains(&place))
                .then(|| TOO_LARGE.to_owned())
        });
        let wanted = next_read(&request.messages);
        self.requests.lock().unwrap().push(request);
        if let Some(refusal) = refusal {
            if self.early.load(Ordering::SeqCst) {
                return Err(CoreError::Provider(refusal));
            }
            return Ok(Box::pin(stream::iter(vec![Err(CoreError::Provider(
                refusal,
            ))])));
        }
        // At most six reads, so a loop shows as a count rather than a hang.
        let read = wanted.and_then(|path| {
            let mut reads = self.reads.lock().unwrap();
            (*reads < 6).then(|| {
                *reads += 1;
                (path, *reads)
            })
        });
        Ok(match read {
            Some((path, serial)) => events(vec![ProviderEvent::ToolCall {
                call_id: format!("read-{path}#{serial}"),
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
        Self::with(RuntimeConfig::default())
    }

    fn with(config: RuntimeConfig) -> Self {
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
            config,
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
            } if call_id.starts_with("read-first.pdf#") => Some(output),
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
            "Size fixture refused this request as too large, even after the chat was compacted. The files in this chat are too large to send together: continue in a new chat, or with smaller files."
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

/// The PDFs in each request, with the notes of any left out.
fn pdfs_and_notes(requests: &[ProviderRequest]) -> Vec<(Vec<String>, usize)> {
    requests
        .iter()
        .map(|request| {
            let notes = serde_json::to_string(&request.messages)
                .unwrap()
                .matches(NOTE)
                .count();
            (sent(request), notes)
        })
        .collect()
}

#[tokio::test]
async fn what_fit_in_one_request_before_is_sent_whole_without_reading_twice() {
    // The person attaches a 10 MiB PDF and the model reads another: 28 MB.
    let fixture = Fixture::new();
    let thread = fixture.thread().await;
    let a = fixture.pdf("a.pdf", 10);
    fixture.pdf("b.pdf", 10);
    let done = fixture
        .turn(thread, "Compare this with b.pdf.\nread b.pdf", vec![a])
        .await;
    assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
    assert_eq!(
        pdfs_and_notes(&fixture.requests()),
        [
            (vec!["a.pdf".to_owned()], 0),
            (vec!["a.pdf".to_owned(), "b.pdf".to_owned()], 0)
        ]
    );
    assert_eq!(*fixture.provider.reads.lock().unwrap(), 1);

    // One 19 MiB PDF the model reads: 26.6 MB.
    let fixture = Fixture::new();
    let thread = fixture.thread().await;
    fixture.pdf("b.pdf", 19);
    let done = fixture.turn(thread, "read b.pdf", Vec::new()).await;
    assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
    assert_eq!(
        pdfs_and_notes(&fixture.requests()),
        [(vec![], 0), (vec!["b.pdf".to_owned()], 0)]
    );
    assert_eq!(*fixture.provider.reads.lock().unwrap(), 1);

    // Two 10 MiB PDFs the model reads one after the other in one turn.
    let fixture = Fixture::new();
    let thread = fixture.thread().await;
    fixture.pdf("a.pdf", 10);
    fixture.pdf("b.pdf", 10);
    let done = fixture
        .turn(thread, "read a.pdf\nread b.pdf", Vec::new())
        .await;
    assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
    assert_eq!(
        pdfs_and_notes(&fixture.requests()),
        [
            (vec![], 0),
            (vec!["a.pdf".to_owned()], 0),
            (vec!["a.pdf".to_owned(), "b.pdf".to_owned()], 0)
        ]
    );
    assert_eq!(*fixture.provider.reads.lock().unwrap(), 2);
    assert_eq!(fixture.summaries(), 0);
}

#[tokio::test]
async fn two_pdfs_read_in_one_turn_that_no_request_carries_end_it_without_rereading() {
    // 2 × 20 MiB of base64 read in one turn: nothing of this turn is noted,
    // so the model never reads them again; the refusal compacts once, and
    // the turn ends with the plain error.
    let fixture = Fixture::new();
    let thread = fixture.thread().await;
    fixture.pdf("a.pdf", 15);
    fixture.pdf("b.pdf", 15);
    let done = fixture
        .turn(thread, "read a.pdf\nread b.pdf", Vec::new())
        .await;
    assert_eq!(done.status, TurnStatus::Failed);
    assert!(
        done.error
            .unwrap()
            .ends_with("even after the chat was compacted. The files in this chat are too large to send together: continue in a new chat, or with smaller files.")
    );
    assert_eq!(*fixture.provider.reads.lock().unwrap(), 2);
    assert_eq!(fixture.summaries(), 1);
    assert!(
        pdfs_and_notes(&fixture.requests())
            .iter()
            .all(|(_, notes)| *notes == 0)
    );
}

#[tokio::test]
async fn a_request_taken_after_compacting_lets_a_later_refusal_compact_again() {
    // The first request is refused for its size and compacted for; the
    // next is taken; the one after is refused too, later in the same turn.
    let fixture = Fixture::new();
    let thread = fixture.thread().await;
    fixture.pdf("a.pdf", 1);
    *fixture.provider.refused.lock().unwrap() = vec![0, 2];
    let done = fixture.turn(thread, "read a.pdf", Vec::new()).await;
    assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
    assert_eq!(fixture.summaries(), 2);
    assert_eq!(fixture.requests().len(), 4);
}

#[tokio::test]
async fn bedrock_and_gemini_refusals_of_the_requests_bytes_compact_once_too() {
    // How the Bedrock adapter words a 413, and Gemini's 400 for a request
    // over its bytes.
    for refusal in [
        r#"Bedrock returned 413 Payload Too Large: {"message":"Request Entity Too Large"}"#,
        "HTTP 400 Bad Request: Request payload size exceeds the limit: 20971520 bytes.",
    ] {
        let fixture = Fixture::new();
        let thread = fixture.thread().await;
        *fixture.provider.refusing.lock().unwrap() = Some(refusal.into());
        let done = fixture.turn(thread, "Hello", Vec::new()).await;
        assert_eq!(done.status, TurnStatus::Failed);
        assert_eq!(fixture.summaries(), 1, "{refusal}");
        assert_eq!(
            done.error.unwrap(),
            "Size fixture refused this request as too large, even after the chat was compacted. The files in this chat are too large to send together: continue in a new chat, or with smaller files."
        );
    }
}

#[tokio::test]
async fn where_the_chat_cannot_compact_a_refusal_of_its_bytes_still_says_what_to_do() {
    let mut config = RuntimeConfig::default();
    config.compaction.enabled = false;
    let fixture = Fixture::with(config);
    let thread = fixture.thread().await;
    *fixture.provider.refusing.lock().unwrap() = Some(TOO_LARGE.into());
    let done = fixture.turn(thread, "Hello", Vec::new()).await;
    assert_eq!(done.status, TurnStatus::Failed);
    assert_eq!(fixture.summaries(), 0);
    assert_eq!(
        done.error.unwrap(),
        "Size fixture refused this request as too large. The files in this chat are too large to send together: continue in a new chat, or with smaller files."
    );
}

#[tokio::test]
async fn a_provider_that_states_no_request_size_keeps_earlier_turns_files() {
    // Three turns each attaching a 9 MiB PDF: 37.7 MB, which a provider
    // taking 100 MB carries whole, as it did before PDFs were counted.
    let fixture = Fixture::new();
    fixture.provider.unstated.store(true, Ordering::SeqCst);
    let thread = fixture.thread().await;
    for name in ["a.pdf", "b.pdf", "c.pdf"] {
        let pdf = fixture.pdf(name, 9);
        let done = fixture
            .turn(thread, &format!("Here is {name}"), vec![pdf])
            .await;
        assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
    }
    assert_eq!(
        pdfs_and_notes(&fixture.requests()),
        [
            (vec!["a.pdf".to_owned()], 0),
            (vec!["a.pdf".to_owned(), "b.pdf".to_owned()], 0),
            (
                vec!["a.pdf".to_owned(), "b.pdf".to_owned(), "c.pdf".to_owned()],
                0
            )
        ]
    );
    assert_eq!(fixture.summaries(), 0);
}
