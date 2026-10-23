//! A message's `@` mentions of MCP resources are read when it arrives: the
//! stored message and the model both get what was read, a resource that
//! cannot be read is named with the reason while the message still goes, a
//! queued message holds what was read when it was queued, and a published
//! chat cannot read the owner's servers.
use super::*;
use haarness_protocol::{Item, ItemData, RESOURCE_MENTIONS_CAPABILITY};
use serde_json::json;

/// Two servers that expose the same URI, and nothing else.
struct Docs;
#[async_trait]
impl McpRegistry for Docs {
    fn tool_snapshot(&self) -> Vec<ToolDefinition> {
        vec![]
    }
    fn resource_servers(&self) -> Vec<String> {
        vec!["docs".into(), "wiki".into()]
    }
    async fn list(&self) -> CoreResult<Vec<McpServerInfo>> {
        Ok(vec![])
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
        Ok(vec![])
    }
    async fn call(&self, _: &str, _: &str, _: Value) -> CoreResult<ToolOutput> {
        Err(CoreError::NotFound("fixture".into()))
    }
    async fn resources(&self, _: &str) -> CoreResult<Vec<Value>> {
        Ok(vec![json!({"uri": "docs://setup", "name": "setup"})])
    }
    async fn resource_templates(&self, _: &str) -> CoreResult<Vec<Value>> {
        Ok(vec![])
    }
    async fn read_resource(&self, server: &str, uri: &str) -> CoreResult<Value> {
        match uri {
            "docs://setup" => Ok(
                json!({"contents": [{"uri": uri, "mimeType": "text/markdown", "text": format!("# {server} setup")}]}),
            ),
            _ => Err(CoreError::Tool(
                "MCP error -32002: Resource not found".into(),
            )),
        }
    }
    async fn prompts(&self, _: &str) -> CoreResult<Vec<Value>> {
        Ok(vec![])
    }
    async fn get_prompt(&self, _: &str, _: &str, _: Value) -> CoreResult<Value> {
        Err(CoreError::NotFound("fixture".into()))
    }
}

struct Fixture {
    base: String,
    client: reqwest::Client,
    runtime: Arc<AgentRuntime>,
    handle: haarness_server::ServerHandle,
}

async fn fixture() -> Fixture {
    let runtime = Arc::new(AgentRuntime::new(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Echo) as Arc<dyn ModelProvider>
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = serve(
        listener,
        "mention-token",
        ServerState {
            browser: None,
            management: None,
            runtime: runtime.clone(),
            mcp: Arc::new(Docs),
            skills: Arc::new(NoSkills),
            plugins: None,
            workflows: None,
            workspaces: None,
            workspace_id: None,
            workspace: std::env::temp_dir(),
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
                "Bearer mention-token".parse().unwrap(),
            );
            headers
        })
        .build()
        .unwrap();
    Fixture {
        base: format!("http://{}", handle.address),
        client,
        runtime,
        handle,
    }
}

impl Fixture {
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
    async fn items(&self, thread: &Thread) -> Vec<Item> {
        wait_idle(&self.runtime, thread.id).await;
        self.client
            .get(format!("{}/v1/threads/{}/items", self.base, thread.id))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}

fn user_messages(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match &item.data {
            ItemData::UserMessage { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn replies(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match &item.data {
            ItemData::AssistantMessage { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_mentioned_resource_is_read_into_the_message_and_reaches_the_model() {
    let fixture = fixture().await;
    let info: ServerInfo = fixture
        .client
        .get(format!("{}/v1/info", fixture.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        info.capabilities
            .iter()
            .any(|capability| capability == RESOURCE_MENTIONS_CAPABILITY)
    );
    let thread = fixture.thread().await;
    fixture
        .client
        .post(format!("{}/v1/threads/{}/turns", fixture.base, thread.id))
        .json(&json!({
            "requestId": RequestId::new(),
            "text": "Compare these",
            "resources": [
                {"server": "docs", "uri": "docs://setup", "name": "setup"},
                {"server": "wiki", "uri": "docs://setup", "name": "setup"},
                {"server": "docs", "uri": "docs://gone"},
            ],
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let items = fixture.items(&thread).await;
    let sent = user_messages(&items);
    assert_eq!(
        sent,
        [[
            "Compare these",
            "<details>\n<summary>MCP resource: setup · docs:docs://setup</summary>\n\n```\n# docs setup\n```\n\n</details>",
            "<details>\n<summary>MCP resource: setup · wiki:docs://setup</summary>\n\n```\n# wiki setup\n```\n\n</details>",
            "<details>\n<summary>MCP resource: docs:docs://gone (not read)</summary>\n\n```\nCould not read MCP resource docs:docs://gone: MCP error -32002: Resource not found\n```\n\n</details>",
        ]
        .join("\n\n")]
    );
    // The model read the same message: the echo provider repeats it.
    let reply = replies(&items).pop().unwrap();
    assert!(
        reply.contains("# docs setup") && reply.contains("# wiki setup"),
        "{reply}"
    );
    fixture.handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_queued_message_holds_what_was_read_when_it_was_queued() {
    let fixture = fixture().await;
    let thread = fixture.thread().await;
    for (delivery, text) in [("steer", "Now"), ("queue", "Later")] {
        fixture
            .client
            .post(format!("{}/v1/threads/{}/input", fixture.base, thread.id))
            .json(&json!({
                "requestId": RequestId::new(),
                "delivery": delivery,
                "text": text,
                "resources": [{"server": "wiki", "uri": "docs://setup", "name": "setup"}],
            }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        wait_idle(&fixture.runtime, thread.id).await;
    }
    let sent = user_messages(&fixture.items(&thread).await);
    assert_eq!(sent.len(), 2, "{sent:?}");
    for (text, typed) in sent.iter().zip(["Now", "Later"]) {
        assert!(
            text.starts_with(&format!(
                "{typed}\n\n<details>\n<summary>MCP resource: setup · wiki:docs://setup</summary>"
            )),
            "{text}"
        );
    }
    fixture.handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_published_chat_cannot_mention_resources_and_is_not_offered_them() {
    let fixture = fixture().await;
    let thread = fixture.thread().await;
    let shared = |request: reqwest::RequestBuilder| {
        request.header("x-haarness-remote-thread", thread.id.to_string())
    };
    let info: Value = shared(fixture.client.get(format!("{}/v1/info", fixture.base)))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !info["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|capability| capability == RESOURCE_MENTIONS_CAPABILITY),
        "{info}"
    );
    let response = shared(
        fixture
            .client
            .post(format!("{}/v1/threads/{}/turns", fixture.base, thread.id)),
    )
    .json(&json!({
        "requestId": RequestId::new(),
        "text": "Read it",
        "resources": [{"server": "docs", "uri": "docs://setup"}],
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    // A command's typed (non-string) arguments cannot take a mention.
    let response = fixture
        .client
        .post(format!(
            "{}/v1/threads/{}/commands",
            fixture.base, thread.id
        ))
        .json(&json!({
            "name": "review",
            "arguments": {"focus": "all"},
            "resources": [{"server": "docs", "uri": "docs://setup"}],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(user_messages(&fixture.items(&thread).await).is_empty());
    fixture.handle.shutdown().await.unwrap();
}
