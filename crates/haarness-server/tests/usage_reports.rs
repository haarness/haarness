//! Usage is the provider's report when it sends one, and the local estimate,
//! marked `estimated`, only when it sends none: on every exit of a round. A
//! request that finished cleanly also records when its first content came
//! and how long its output streamed.
use async_trait::async_trait;
use futures::{StreamExt, stream};
use haarness_core::{
    AgentRuntime, CoreError, CoreResult, McpRegistry, ModelProvider, ProviderEvent,
    ProviderRequest, ProviderStream, SkillRegistry, StaticProviderRegistry, ToolDefinition,
    ToolOutput,
};
use haarness_protocol::{
    BtwResponse, CreateThreadRequest, McpConfigureRequest, McpServerInfo, ModelInfo, ProviderInfo,
    SkillInfo, Thread,
};
use haarness_server::{NativeToolRegistry, ServerState, serve};
use haarness_store::SqliteStore;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// One request's events, each sent after its pause.
type Script = Vec<(Duration, ProviderEvent)>;

/// Answers each request with the next scripted event list.
#[derive(Clone, Default)]
struct ScriptedProvider {
    scripts: Arc<std::sync::Mutex<VecDeque<Script>>>,
    /// Set once a request has sent its first event.
    answered: Arc<AtomicBool>,
}

impl ScriptedProvider {
    fn push(&self, events: Vec<ProviderEvent>) {
        self.push_paced(events.into_iter().map(|event| (0, event)).collect());
    }

    fn push_paced(&self, events: Vec<(u64, ProviderEvent)>) {
        self.scripts.lock().unwrap().push_back(
            events
                .into_iter()
                .map(|(pause, event)| (Duration::from_millis(pause), event))
                .collect(),
        );
    }
}

#[async_trait]
impl ModelProvider for ScriptedProvider {
    fn id(&self) -> &str {
        "echo"
    }
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "echo".into(),
            name: "Scripted usage".into(),
            authenticated: true,
            models: vec![ModelInfo {
                id: "echo-1".into(),
                display_name: "Echo".into(),
                reasoning: false,
                reasoning_efforts: Vec::new(),
                images: false,
                video: false,
                tools: true,
                context_window: Some(200_000),
                max_output_tokens: None,
                pricing: None,
            }],
        }
    }
    async fn stream(&self, _: ProviderRequest, _: CancellationToken) -> CoreResult<ProviderStream> {
        let events = self.scripts.lock().unwrap().pop_front().unwrap_or_else(|| {
            vec![
                (Duration::ZERO, ProviderEvent::TextDelta("Done.".into())),
                (
                    Duration::ZERO,
                    ProviderEvent::Finished {
                        reason: "stop".into(),
                    },
                ),
            ]
        });
        let answered = self.answered.clone();
        Ok(Box::pin(stream::iter(events).then(
            move |(pause, event)| {
                let answered = answered.clone();
                async move {
                    tokio::time::sleep(pause).await;
                    answered.store(true, Ordering::SeqCst);
                    Ok(event)
                }
            },
        )))
    }
}

fn usage(input_tokens: u64, output_tokens: u64) -> ProviderEvent {
    ProviderEvent::Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    }
}

fn finished() -> ProviderEvent {
    ProviderEvent::Finished {
        reason: "stop".into(),
    }
}

struct NoMcp;

#[async_trait]
impl McpRegistry for NoMcp {
    fn tool_snapshot(&self) -> Vec<ToolDefinition> {
        Vec::new()
    }
    async fn list(&self) -> CoreResult<Vec<McpServerInfo>> {
        Ok(Vec::new())
    }
    async fn configure(&self, _: &str, _: McpConfigureRequest) -> CoreResult<McpServerInfo> {
        Err(CoreError::NotFound("fixture".into()))
    }
    async fn reconnect(&self, _: &str) -> CoreResult<McpServerInfo> {
        Err(CoreError::NotFound("fixture".into()))
    }
    async fn disconnect(&self, _: &str) -> CoreResult<McpServerInfo> {
        Err(CoreError::NotFound("fixture".into()))
    }
    async fn tools(&self, _: &str) -> CoreResult<Vec<ToolDefinition>> {
        Ok(Vec::new())
    }
    async fn call(&self, _: &str, _: &str, _: Value) -> CoreResult<ToolOutput> {
        Err(CoreError::NotFound("fixture".into()))
    }
    async fn resources(&self, _: &str) -> CoreResult<Vec<Value>> {
        Ok(Vec::new())
    }
    async fn resource_templates(&self, _: &str) -> CoreResult<Vec<Value>> {
        Ok(Vec::new())
    }
    async fn read_resource(&self, _: &str, _: &str) -> CoreResult<Value> {
        Err(CoreError::NotFound("fixture".into()))
    }
    async fn prompts(&self, _: &str) -> CoreResult<Vec<Value>> {
        Ok(Vec::new())
    }
    async fn get_prompt(&self, _: &str, _: &str, _: Value) -> CoreResult<Value> {
        Err(CoreError::NotFound("fixture".into()))
    }
}

struct NoSkills;

#[async_trait]
impl SkillRegistry for NoSkills {
    async fn discover(&self) -> CoreResult<Vec<SkillInfo>> {
        Ok(Vec::new())
    }
    async fn load(&self, name: &str) -> CoreResult<String> {
        Err(CoreError::NotFound(name.into()))
    }
}

struct TestServer {
    base: String,
    client: reqwest::Client,
    handle: haarness_server::ServerHandle,
}

async fn start_server(
    workspace: PathBuf,
    database: PathBuf,
    provider: ScriptedProvider,
) -> TestServer {
    let store = Arc::new(SqliteStore::open(database).unwrap());
    let providers = Arc::new(StaticProviderRegistry::new(vec![
        Arc::new(provider) as Arc<dyn ModelProvider>
    ]));
    let tools = Arc::new(NativeToolRegistry::new(workspace.clone()));
    let runtime = Arc::new(AgentRuntime::with_workspace(
        store,
        providers,
        tools,
        haarness_core::EventBus::new(256),
        workspace.clone(),
    ));
    runtime.recover().await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = serve(
        listener,
        "usage-token",
        ServerState {
            browser: None,
            management: None,
            runtime,
            mcp: Arc::new(NoMcp),
            skills: Arc::new(NoSkills),
            plugins: None,
            workflows: None,
            workspaces: None,
            workspace_id: None,
            workspace,
            project_trusted: false,
            language: None,
        },
    )
    .await
    .unwrap();
    let client = reqwest::Client::builder()
        .default_headers({
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::AUTHORIZATION,
                "Bearer usage-token".parse().unwrap(),
            );
            headers
        })
        .build()
        .unwrap();
    TestServer {
        base: format!("http://{}", handle.address),
        client,
        handle,
    }
}

impl TestServer {
    async fn thread(&self) -> Thread {
        self.client
            .post(format!("{}/v1/threads", self.base))
            .json(&CreateThreadRequest {
                provider: Some("echo".into()),
                model: Some("echo-1".into()),
                ..Default::default()
            })
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    /// A chat with a title, so no title request takes a script meanwhile.
    async fn titled_thread(&self) -> Thread {
        self.client
            .post(format!("{}/v1/threads", self.base))
            .json(&CreateThreadRequest {
                title: Some("Scripted chat".into()),
                provider: Some("echo".into()),
                model: Some("echo-1".into()),
                ..Default::default()
            })
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    /// Starts a turn and returns its id.
    async fn start(&self, thread: &Thread, text: &str) -> String {
        let turn: Value = self
            .client
            .post(format!("{}/v1/threads/{}/turns", self.base, thread.id))
            .json(&json!({"requestId": uuid::Uuid::now_v7(), "text": text}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        turn["id"].as_str().unwrap().to_owned()
    }

    /// Runs one turn to its end and returns the thread's usage items.
    async fn turn(&self, thread: &Thread, text: &str) -> Vec<Value> {
        let id = self.start(thread, text).await;
        self.wait(thread, &id).await
    }

    /// Waits for a started turn to end and returns the thread's usage items.
    async fn wait(&self, thread: &Thread, id: &str) -> Vec<Value> {
        self.client
            .get(format!("{}/v1/turns/{id}/wait?timeout_ms=20000", self.base))
            .send()
            .await
            .unwrap();
        let items: Value = self
            .client
            .get(format!("{}/v1/threads/{}/items", self.base, thread.id))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        items
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["data"]["type"] == "usage")
            .map(|item| item["data"].clone())
            .collect()
    }

    async fn context(&self, thread: &Thread) -> Value {
        let usage: Value = self
            .client
            .get(format!("{}/v1/threads/{}/usage", self.base, thread.id))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        usage["context"].clone()
    }
}

async fn server(name: &str) -> (TestServer, ScriptedProvider, PathBuf) {
    let root = std::env::temp_dir().join(format!("haarness-usage-{name}-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let provider = ScriptedProvider::default();
    let server = start_server(root.clone(), root.join("state.db"), provider.clone()).await;
    (server, provider, root)
}

#[tokio::test]
async fn reported_usage_is_recorded_and_anchors_the_context() {
    let (server, provider, root) = server("reported").await;
    let thread = server.thread().await;
    provider.push(vec![
        ProviderEvent::TextDelta("Hello.".into()),
        usage(1_000, 20),
        finished(),
    ]);
    let usages = server.turn(&thread, "hi").await;
    assert_eq!(usages.len(), 1);
    assert_eq!(
        (
            usages[0]["input_tokens"].as_u64(),
            usages[0]["output_tokens"].as_u64()
        ),
        (Some(1_000), Some(20))
    );
    assert!(usages[0].get("estimated").is_none(), "{:?}", usages[0]);
    assert!(usages[0]["first_token_ms"].is_u64(), "{:?}", usages[0]);
    assert_eq!(usages[0]["stream_ms"], 0, "{:?}", usages[0]);
    assert_eq!(server.context(&thread).await["reportedTokens"], 1_020);
    server.handle.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn a_cost_the_provider_charged_is_the_turn_cost() {
    // The scripted model has no price: only the charge can price the turn.
    let (server, provider, root) = server("charged").await;
    let thread = server.thread().await;
    provider.push(vec![
        ProviderEvent::TextDelta("Hello.".into()),
        usage(1_000, 20),
        ProviderEvent::Cost { usd: 0.0085 },
        finished(),
    ]);
    provider.push(vec![
        ProviderEvent::TextDelta("Hello again.".into()),
        usage(1_100, 20),
        finished(),
    ]);
    server.turn(&thread, "first").await;
    let usages = server.turn(&thread, "second").await;
    assert_eq!(usages.len(), 2, "{usages:?}");
    assert_eq!(usages[0]["cost_usd"], 0.0085, "{:?}", usages[0]);
    assert!(usages[1]["cost_usd"].is_null(), "{:?}", usages[1]);
    server.handle.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn without_a_report_the_local_estimate_is_recorded_and_marked() {
    let (server, provider, root) = server("estimated").await;
    let thread = server.thread().await;
    // No usage at all, then an all-zero report (a gateway that strips usage).
    provider.push(vec![
        ProviderEvent::TextDelta("An answer without any usage report.".into()),
        finished(),
    ]);
    provider.push(vec![
        ProviderEvent::TextDelta("Another answer.".into()),
        usage(0, 0),
        finished(),
    ]);
    server.turn(&thread, "first").await;
    let usages = server.turn(&thread, "second").await;
    assert_eq!(usages.len(), 2, "{usages:?}");
    for usage in &usages {
        assert_eq!(usage["estimated"], true, "{usage:?}");
        let prompt = &usage["prompt"];
        let estimated_input = [
            "system_tokens",
            "tool_tokens",
            "history_tokens",
            "media_tokens",
        ]
        .iter()
        .map(|key| prompt[key].as_u64().unwrap_or(0))
        .sum::<u64>();
        assert_eq!(usage["input_tokens"].as_u64(), Some(estimated_input));
        assert!(usage["output_tokens"].as_u64().unwrap() > 0);
    }
    // Estimates never anchor: the context is estimated from the history.
    let context = server.context(&thread).await;
    assert_eq!(context["reportedTokens"], 0, "{context:?}");
    assert!(
        context["estimatedTokens"].as_u64().unwrap() > 0,
        "{context:?}"
    );
    server.handle.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn usage_received_before_a_failure_is_kept() {
    let (server, provider, root) = server("failed").await;
    let thread = server.thread().await;
    provider.push(vec![
        usage(4_200, 9),
        ProviderEvent::ToolCall {
            call_id: String::new(),
            name: "read".into(),
            arguments: json!({}),
            provider_metadata: Value::Null,
        },
        finished(),
    ]);
    let usages = server.turn(&thread, "go").await;
    assert_eq!(usages.len(), 1, "{usages:?}");
    assert_eq!(usages[0]["input_tokens"], 4_200);
    assert!(usages[0].get("estimated").is_none());
    // A refused request says nothing about the provider's pace.
    assert!(usages[0].get("first_token_ms").is_none(), "{usages:?}");
    assert!(usages[0].get("stream_ms").is_none(), "{usages:?}");
    server.handle.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn a_finished_request_records_its_first_token_time_and_streaming_span() {
    let (server, provider, root) = server("timed").await;
    let thread = server.thread().await;
    provider.push_paced(vec![
        (300, ProviderEvent::ReasoningDelta("Thinking.".into())),
        (200, ProviderEvent::TextDelta("Hello ".into())),
        (250, ProviderEvent::TextDelta("there.".into())),
        // The provider's output count holds reasoning it never streamed.
        (100, usage(1_000, 5_000)),
        (0, finished()),
    ]);
    let usages = server.turn(&thread, "hi").await;
    assert_eq!(usages.len(), 1, "{usages:?}");
    let first = usages[0]["first_token_ms"].as_u64().unwrap();
    let stream = usages[0]["stream_ms"].as_u64().unwrap();
    // Measured from the request to the reasoning, then to the last text: the
    // usage report and the finish carry no content and do not extend it.
    assert!((300..10_000).contains(&first), "{usages:?}");
    assert!((450..10_000).contains(&stream), "{usages:?}");
    // Output speed divides what streamed, not the provider's 5,000.
    let streamed = usages[0]["stream_tokens"].as_u64().unwrap();
    assert!((1..50).contains(&streamed), "{usages:?}");
    server.handle.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).ok();
}

/// A thought records how long the model took over it, from the request (or
/// the text before it) to the end of its reasoning, so a client can say
/// "Thought for 3s".
#[tokio::test]
async fn reasoning_records_how_long_the_model_thought() {
    let (server, provider, root) = server("thought").await;
    let thread = server.titled_thread().await;
    provider.push_paced(vec![
        (
            300,
            ProviderEvent::ReasoningDelta("Reading the request.".into()),
        ),
        (400, ProviderEvent::ReasoningDelta(" Deciding.".into())),
        (300, ProviderEvent::TextDelta("First answer.".into())),
        (600, ProviderEvent::ReasoningDelta("Second thought.".into())),
        (200, ProviderEvent::TextDelta(" Second answer.".into())),
        (0, finished()),
    ]);
    let id = server.start(&thread, "think").await;
    server.wait(&thread, &id).await;
    let items: Value = server
        .client
        .get(format!("{}/v1/threads/{}/items", server.base, thread.id))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thoughts = items
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["data"]["type"] == "reasoning")
        .map(|item| item["data"]["duration_ms"].as_u64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(thoughts.len(), 2, "{items:?}");
    // From the request to the end of the first reasoning, then from the
    // first text to the end of the second.
    assert!((700..10_000).contains(&thoughts[0]), "{thoughts:?}");
    assert!((600..10_000).contains(&thoughts[1]), "{thoughts:?}");
    server.handle.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).ok();
}

/// Each request after a tool round times its own thought, from when it was
/// sent: the time the first request and its tool took never counts again.
#[tokio::test]
async fn a_thought_after_a_tool_round_counts_from_its_own_request() {
    let (server, provider, root) = server("thought-rounds").await;
    let thread = server.titled_thread().await;
    provider.push_paced(vec![
        (700, ProviderEvent::ReasoningDelta("Reading first.".into())),
        (
            100,
            ProviderEvent::ToolCall {
                call_id: "call-1".into(),
                name: "read".into(),
                arguments: json!({"path": "missing.txt"}),
                provider_metadata: Value::Null,
            },
        ),
        (
            0,
            ProviderEvent::Finished {
                reason: "tool_use".into(),
            },
        ),
    ]);
    provider.push_paced(vec![
        (200, ProviderEvent::ReasoningDelta("Then this.".into())),
        (100, ProviderEvent::TextDelta("Done.".into())),
        (0, finished()),
    ]);
    let id = server.start(&thread, "think twice").await;
    server.wait(&thread, &id).await;
    let items: Value = server
        .client
        .get(format!("{}/v1/threads/{}/items", server.base, thread.id))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thoughts = items
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["data"]["type"] == "reasoning")
        .map(|item| item["data"]["duration_ms"].as_u64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(thoughts.len(), 2, "{items:?}");
    // The first counts its wait for the first token and its own stream; the
    // second only its own request's 300 ms, not the 800 ms before it.
    assert!((700..10_000).contains(&thoughts[0]), "{thoughts:?}");
    assert!((200..700).contains(&thoughts[1]), "{thoughts:?}");
    server.handle.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn a_tool_call_is_timed_from_when_it_started_not_when_its_arguments_were_whole() {
    let (server, provider, root) = server("tool-timed").await;
    let thread = server.thread().await;
    // As Anthropic streams a call: its block starts, its arguments stream as
    // fragments the adapter holds, and the whole call arrives at the end.
    provider.push_paced(vec![
        (300, ProviderEvent::ContentStarted),
        (
            1_500,
            ProviderEvent::ToolCall {
                call_id: "call-1".into(),
                name: "read".into(),
                arguments: json!({"path": "missing.txt"}),
                provider_metadata: Value::Null,
            },
        ),
        (0, usage(900, 1_200)),
        (
            0,
            ProviderEvent::Finished {
                reason: "tool_use".into(),
            },
        ),
    ]);
    let usages = server.turn(&thread, "read it").await;
    assert_eq!(usages.len(), 2, "{usages:?}");
    let first = usages[0]["first_token_ms"].as_u64().unwrap();
    let stream = usages[0]["stream_ms"].as_u64().unwrap();
    assert!((300..1_500).contains(&first), "{usages:?}");
    assert!((1_400..10_000).contains(&stream), "{usages:?}");
    server.handle.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn a_stopped_request_is_not_timed() {
    let (server, provider, root) = server("stopped").await;
    let thread = server.thread().await;
    provider.push_paced(vec![
        (0, ProviderEvent::TextDelta("A start ".into())),
        (60_000, ProviderEvent::TextDelta("never sent".into())),
        (0, finished()),
    ]);
    let id = server.start(&thread, "go").await;
    for _ in 0..200 {
        if provider.answered.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(provider.answered.load(Ordering::SeqCst));
    server
        .client
        .post(format!("{}/v1/turns/{id}/interrupt", server.base))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let usages = server.wait(&thread, &id).await;
    // The streamed start is still counted, as an estimate, without timing.
    assert_eq!(usages.len(), 1, "{usages:?}");
    assert_eq!(usages[0]["estimated"], true, "{usages:?}");
    assert!(usages[0].get("first_token_ms").is_none(), "{usages:?}");
    server.handle.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn a_stopped_answer_keeps_what_streamed() {
    let (server, provider, root) = server("stopped-text").await;
    let thread = server.thread().await;
    provider.push_paced(vec![
        (
            0,
            ProviderEvent::ReasoningDelta("Thinking it over. ".into()),
        ),
        (
            0,
            ProviderEvent::TextDelta("The printing press began ".into()),
        ),
        (0, ProviderEvent::TextDelta("in Mainz".into())),
        (60_000, ProviderEvent::TextDelta(" and never this".into())),
        (0, finished()),
    ]);
    let id = server.start(&thread, "go").await;
    for _ in 0..200 {
        if provider.answered.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Let the first three events land before stopping.
    tokio::time::sleep(Duration::from_millis(300)).await;
    server
        .client
        .post(format!("{}/v1/turns/{id}/interrupt", server.base))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    server.wait(&thread, &id).await;
    let items: Value = server
        .client
        .get(format!("{}/v1/threads/{}/items", server.base, thread.id))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let kinds: Vec<(String, String)> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            let data = &item["data"];
            let text = data["text"]
                .as_str()
                .or(data["message"].as_str())
                .unwrap_or_default();
            (
                data["type"].as_str().unwrap_or_default().to_owned(),
                text.to_owned(),
            )
        })
        .collect();
    let position = |kind: &str| kinds.iter().position(|(item, _)| item == kind);
    assert!(
        kinds.contains(&("reasoning".into(), "Thinking it over. ".into())),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&(
            "assistant_message".into(),
            "The printing press began in Mainz".into()
        )),
        "{kinds:?}"
    );
    // Clients read the stop from the closing item, after what streamed.
    assert_eq!(
        kinds.last().map(|(kind, _)| kind.as_str()),
        Some("error"),
        "{kinds:?}"
    );
    assert!(
        position("reasoning") < position("assistant_message"),
        "{kinds:?}"
    );
    server.handle.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn side_questions_count_the_last_report_of_a_stream_once() {
    let (server, provider, root) = server("btw").await;
    let thread = server.thread().await;
    // Gemini repeats cumulative usage on every chunk.
    provider.push(vec![
        usage(50, 1),
        ProviderEvent::TextDelta("Short ".into()),
        usage(50, 3),
        ProviderEvent::TextDelta("answer.".into()),
        usage(50, 4),
        finished(),
    ]);
    let answer: BtwResponse = server
        .client
        .post(format!("{}/v1/threads/{}/btw", server.base, thread.id))
        .json(&json!({"question": "Quick?"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!((answer.input_tokens, answer.output_tokens), (50, 4));
    server.handle.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).ok();
}
