use async_trait::async_trait;
use futures::stream;
use haarness_core::{
    AgentRuntime, CoreError, CoreResult, McpRegistry, ModelProvider, ProviderEvent,
    ProviderRequest, ProviderStream, SkillRegistry, StaticProviderRegistry, ToolDefinition,
    ToolOutput,
};
use haarness_protocol::{
    BtwEvent, BtwResponse, CreateThreadRequest, McpConfigureRequest, McpServerInfo, ModelInfo,
    ProviderInfo, SkillInfo, Thread,
};
use haarness_server::{NativeToolRegistry, ServerState, serve};
use haarness_store::SqliteStore;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// How the probe answers.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Script {
    #[default]
    Answer,
    /// The answer in three pieces.
    Pieces,
    /// A preamble and a tool call, then the answer once the call is refused.
    ToolFirst,
    /// The provider fails.
    Fail,
    /// Nothing until the request is cancelled.
    Hold,
}

struct Ended(Arc<std::sync::atomic::AtomicBool>);
impl Drop for Ended {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[derive(Clone, Default)]
struct SideQuestionProbe {
    requests: Arc<std::sync::Mutex<Vec<ProviderRequest>>>,
    script: Arc<std::sync::Mutex<Script>>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}
#[async_trait]
impl ModelProvider for SideQuestionProbe {
    fn id(&self) -> &str {
        "echo"
    }
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "echo".into(),
            name: "Side question probe".into(),
            authenticated: true,
            models: vec![ModelInfo {
                id: "echo-1".into(),
                display_name: "Echo".into(),
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
        let refused = request
            .messages
            .iter()
            .any(|message| matches!(message, haarness_core::ProviderMessage::ToolResult { .. }));
        self.requests.lock().unwrap().push(request);
        let usage = Ok(ProviderEvent::Usage {
            input_tokens: 7,
            output_tokens: 3,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        });
        let finished = || {
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            })
        };
        let script = *self.script.lock().unwrap();
        let events = match script {
            Script::Answer => vec![
                Ok(ProviderEvent::TextDelta("An isolated answer.".into())),
                usage,
                finished(),
            ],
            Script::Pieces => vec![
                Ok(ProviderEvent::TextDelta("An ".into())),
                Ok(ProviderEvent::TextDelta("isolated ".into())),
                Ok(ProviderEvent::TextDelta("answer.".into())),
                usage,
                finished(),
            ],
            Script::ToolFirst if !refused => vec![
                Ok(ProviderEvent::TextDelta("Let me look.".into())),
                Ok(ProviderEvent::ToolCall {
                    call_id: "side-call".into(),
                    name: "read".into(),
                    arguments: json!({"path": "secret.txt"}),
                    provider_metadata: Value::Null,
                }),
                finished(),
            ],
            Script::ToolFirst => vec![
                Ok(ProviderEvent::TextDelta("From context.".into())),
                usage,
                finished(),
            ],
            Script::Fail => vec![Err(CoreError::Provider("the model is overloaded".into()))],
            Script::Hold => {
                // Set when the request is cancelled, or dropped unfinished.
                let ended = Ended(self.cancelled.clone());
                return Ok(Box::pin(async_stream::stream! {
                    let _ended = ended;
                    cancel.cancelled().await;
                    yield Err(CoreError::Cancelled);
                }));
            }
        };
        Ok(Box::pin(stream::iter(events)))
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
    runtime: Arc<AgentRuntime>,
}

async fn start_server(
    workspace: PathBuf,
    database: PathBuf,
    provider: SideQuestionProbe,
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
        "btw-token",
        ServerState {
            browser: None,
            management: None,
            runtime: runtime.clone(),
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
                "Bearer btw-token".parse().unwrap(),
            );
            headers
        })
        .build()
        .unwrap();
    TestServer {
        base: format!("http://{}", handle.address),
        client,
        handle,
        runtime,
    }
}

#[tokio::test]
async fn side_questions_preserve_sessions_and_retired_coordination_routes_are_absent() {
    let root = std::env::temp_dir().join(format!("haarness-btw-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let provider = SideQuestionProbe::default();
    let server = start_server(root.clone(), root.join("state.db"), provider.clone()).await;
    let thread: Thread = server
        .client
        .post(format!("{}/v1/threads", server.base))
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
        .unwrap();
    let items = format!("{}/v1/threads/{}/items", server.base, thread.id);
    let before: Value = server
        .client
        .get(&items)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let answer: BtwResponse = server
        .client
        .post(format!("{}/v1/threads/{}/btw", server.base, thread.id))
        .json(&json!({"question":"What is the current context?"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(answer.answer, "An isolated answer.");
    assert_eq!((answer.input_tokens, answer.output_tokens), (7, 3));
    let after: Value = server
        .client
        .get(&items)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(before, after);
    assert!(
        server
            .runtime
            .active_turn(thread.id)
            .await
            .unwrap()
            .is_none()
    );
    {
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].tools.iter().any(|tool| tool.name == "read"));
        assert!(!requests[0].system.contains("BTW side question"));
        assert!(
            serde_json::to_string(&requests[0].messages)
                .unwrap()
                .contains("<btw-side-question>")
        );
    }
    for path in [
        "/v1/teams".to_owned(),
        format!("/v1/threads/{}/team", thread.id),
    ] {
        assert_eq!(
            server
                .client
                .get(format!("{}{}", server.base, path))
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
    }
    for action in ["create", "actions"] {
        assert_eq!(
            server
                .client
                .post(format!(
                    "{}/v1/threads/{}/team/{action}",
                    server.base, thread.id
                ))
                .json(&json!({}))
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
    }
    let info: Value = server
        .client
        .get(format!("{}/v1/info", server.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !info["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str() == Some("teams.v1"))
    );
    let catalog: Value = server
        .client
        .get(format!("{}/v1/tools", server.base))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!catalog.to_string().contains("team_"));
    server.handle.shutdown().await.unwrap();
    drop(server.runtime);
    std::fs::remove_dir_all(root).unwrap();
}

async fn chat(server: &TestServer) -> Thread {
    server
        .client
        .post(format!("{}/v1/threads", server.base))
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

/// The events of one streamed side question, in order.
async fn streamed(response: reqwest::Response) -> Vec<BtwEvent> {
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let body = response.text().await.unwrap();
    body.split("\n\n")
        .filter_map(|frame| {
            let data = frame
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim_start)
                .collect::<Vec<_>>()
                .join("\n");
            (!data.is_empty()).then(|| serde_json::from_str(&data).unwrap())
        })
        .collect()
}

fn texts(request: &ProviderRequest) -> Vec<(String, String)> {
    request
        .messages
        .iter()
        .filter_map(|message| match message {
            haarness_core::ProviderMessage::Text { role, content } => {
                Some((role.clone(), content.clone()))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_follow_up_sends_its_side_chat_after_the_chat_and_extends_the_question_before() {
    let root = std::env::temp_dir().join(format!("haarness-btw-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let provider = SideQuestionProbe::default();
    let server = start_server(root.clone(), root.join("state.db"), provider.clone()).await;
    let thread = chat(&server).await;
    let ask = |body: Value| {
        let server = &server;
        async move {
            server
                .client
                .post(format!("{}/v1/threads/{}/btw", server.base, thread.id))
                .json(&body)
                .send()
                .await
                .unwrap()
        }
    };
    let first: BtwResponse = ask(json!({"question": "What is the codename?"}))
        .await
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let second = ask(json!({
        "question": "Spell it backwards.",
        "history": [{"question": "What is the codename?", "answer": first.answer}],
    }))
    .await;
    assert_eq!(second.status(), 200);
    let requests = provider.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    // The follow-up is the first question's request, its answer, and the new question.
    assert_eq!(requests[1].system, requests[0].system);
    assert_eq!(requests[1].tools, requests[0].tools);
    assert!(requests[1].messages.starts_with(&requests[0].messages));
    let tail = texts(&requests[1]);
    let tail = &tail[tail.len() - 3..];
    assert_eq!(tail[0].0, "user");
    assert!(tail[0].1.contains("What is the codename?"));
    assert!(tail[0].1.contains("every tool call here is refused"));
    assert_eq!(tail[1], ("assistant".into(), "An isolated answer.".into()));
    assert_eq!(
        tail[2],
        (
            "user".into(),
            "<btw-side-question>\nSpell it backwards.\n</btw-side-question>".into()
        )
    );
    // A thread with a missing answer, or too long a thread, is refused before
    // the provider is asked.
    for history in [
        json!([{"question": "What is the codename?", "answer": "  "}]),
        Value::Array(vec![json!({"question": "q", "answer": "a"}); 101]),
    ] {
        let refused = ask(json!({"question": "Again?", "history": history})).await;
        assert_eq!(refused.status(), 400);
    }
    assert_eq!(provider.requests.lock().unwrap().len(), 2);
    server.handle.shutdown().await.unwrap();
    drop(server.runtime);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_streamed_side_question_sends_its_answer_as_it_is_written() {
    let root = std::env::temp_dir().join(format!("haarness-btw-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let provider = SideQuestionProbe::default();
    let server = start_server(root.clone(), root.join("state.db"), provider.clone()).await;
    let thread = chat(&server).await;
    let info: Value = server
        .client
        .get(format!("{}/v1/info", server.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        info["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == haarness_protocol::BTW_STREAM_CAPABILITY)
    );
    let items = format!("{}/v1/threads/{}/items", server.base, thread.id);
    let before: Value = server
        .client
        .get(&items)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let stream = format!("{}/v1/threads/{}/btw/stream", server.base, thread.id);
    let ask = |body: Value| {
        let server = &server;
        let stream = &stream;
        async move { server.client.post(stream).json(&body).send().await.unwrap() }
    };

    *provider.script.lock().unwrap() = Script::Pieces;
    let events = streamed(ask(json!({"question": "What is the context?"})).await).await;
    assert_eq!(
        events,
        vec![
            BtwEvent::Delta { text: "An ".into() },
            BtwEvent::Delta {
                text: "isolated ".into()
            },
            BtwEvent::Delta {
                text: "answer.".into()
            },
            BtwEvent::Done {
                answer: "An isolated answer.".into(),
                input_tokens: 7,
                output_tokens: 3,
            },
        ]
    );

    // An attempt after a refused tool call takes back what came before it.
    *provider.script.lock().unwrap() = Script::ToolFirst;
    let events = streamed(ask(json!({"question": "Read the secret"})).await).await;
    assert_eq!(
        events,
        vec![
            BtwEvent::Delta {
                text: "Let me look.".into()
            },
            BtwEvent::Text {
                text: String::new()
            },
            BtwEvent::Delta {
                text: "From context.".into()
            },
            BtwEvent::Done {
                answer: "From context.".into(),
                input_tokens: 7,
                output_tokens: 3,
            },
        ]
    );
    assert!(!root.join("secret.txt").exists());

    *provider.script.lock().unwrap() = Script::Fail;
    let events = streamed(ask(json!({"question": "Will this fail?"})).await).await;
    assert!(matches!(
        events.as_slice(),
        [BtwEvent::Error { code, message, retryable: true }]
            if code == "provider_error" && message.contains("the model is overloaded")
    ));

    // A missing chat, an empty question and a broken thread are HTTP errors,
    // before any stream or provider request.
    let asked = provider.requests.lock().unwrap().len();
    let missing = server
        .client
        .post(format!(
            "{}/v1/threads/{}/btw/stream",
            server.base,
            uuid::Uuid::now_v7()
        ))
        .json(&json!({"question": "Anyone?"}))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
    assert_eq!(ask(json!({"question": "  "})).await.status(), 400);
    assert_eq!(
        ask(json!({"question": "Again?", "history": [{"question": "", "answer": "a"}]}))
            .await
            .status(),
        400
    );
    assert_eq!(provider.requests.lock().unwrap().len(), asked);

    // A client that goes away ends the provider request.
    *provider.script.lock().unwrap() = Script::Hold;
    let response = ask(json!({"question": "Take your time"})).await;
    assert_eq!(response.status(), 200);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while provider.requests.lock().unwrap().len() == asked {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(response);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !provider.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the provider request was not cancelled");

    let after: Value = server
        .client
        .get(&items)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(before, after);
    server.handle.shutdown().await.unwrap();
    drop(server.runtime);
    std::fs::remove_dir_all(root).unwrap();
}
