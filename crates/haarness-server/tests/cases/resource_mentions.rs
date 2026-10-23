//! A message's `@` mentions of MCP resources are read when it arrives: the
//! stored message and the model both get what was read, a resource that
//! cannot be read, that the chat's rules keep from it or that does not
//! answer in time is named with the reason while the message still goes, a
//! queued message holds what was read when it was queued, a client that
//! leaves mid-read does not lose its message, and a published chat cannot
//! read the owner's servers.
use super::*;
use haarness_core::{PermissionDecision, PermissionRule};
use haarness_protocol::{Item, ItemData, RESOURCE_MENTIONS_CAPABILITY};
use serde_json::json;

/// Two servers that expose the same URI (`docs` and `wiki`), and a third
/// (`hidden`) whose tools a rule takes away. Reading `slow://` never answers.
/// Every read is logged.
#[derive(Default)]
struct Docs {
    reads: std::sync::Mutex<Vec<String>>,
}
#[async_trait]
impl McpRegistry for Docs {
    fn tool_snapshot(&self) -> Vec<ToolDefinition> {
        vec![]
    }
    fn resource_servers(&self) -> Vec<String> {
        vec!["docs".into(), "wiki".into(), "hidden".into()]
    }
    async fn list(&self) -> CoreResult<Vec<McpServerInfo>> {
        Ok(["docs", "wiki", "hidden"]
            .into_iter()
            .map(|name| McpServerInfo {
                name: name.into(),
                transport: "stdio".into(),
                status: "connected".into(),
                detail: None,
                scope: None,
                apps: None,
                app_tools: None,
                turned_off: None,
            })
            .collect())
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
        self.reads.lock().unwrap().push(format!("{server}:{uri}"));
        match uri {
            "slow://" => std::future::pending().await,
            "docs://setup" | "docs://private" => Ok(
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
    docs: Arc<Docs>,
    handle: haarness_server::ServerHandle,
}

/// The echo provider, and rules that keep `docs://private` and every tool
/// (so every resource) of `hidden` from the model; the agent `nodocs` has
/// none of `docs`'s tools.
async fn fixture() -> Fixture {
    fixture_with(Arc::new(Echo)).await
}

async fn fixture_with(provider: Arc<dyn ModelProvider>) -> Fixture {
    let mut config = RuntimeConfig::default();
    for (pattern, decision) in [
        // Rules match last first: the ask is the default, the denies win.
        ("mcp_resource_read:*", PermissionDecision::Ask),
        (
            "mcp_resource_read:docs:docs://private",
            PermissionDecision::Deny,
        ),
        ("mcp__hidden__*", PermissionDecision::Deny),
    ] {
        config.base_rules.push(PermissionRule {
            pattern: pattern.into(),
            decision,
        });
    }
    config.custom_agents.push(haarness_core::CustomAgent {
        id: "nodocs".into(),
        tools: [("mcp__docs__*".to_owned(), false)].into_iter().collect(),
        ..Default::default()
    });
    let runtime = Arc::new(AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![provider])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
        std::env::temp_dir(),
        String::new(),
        config,
    ));
    let docs = Arc::new(Docs::default());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = serve(
        listener,
        "mention-token",
        ServerState {
            browser: None,
            management: None,
            runtime: runtime.clone(),
            mcp: docs.clone(),
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
        docs,
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
            "<details>\n<summary>MCP resource: docs:docs://gone (not read)</summary>\n\n```\nMCP error -32002: Resource not found\n```\n\n</details>",
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
    for (path, body) in [
        (
            "input",
            json!({"requestId": RequestId::new(), "text": "Read it", "resources": [{"server": "docs", "uri": "docs://setup"}]}),
        ),
        (
            "commands",
            json!({"name": "review", "arguments": "it", "resources": [{"server": "docs", "uri": "docs://setup"}]}),
        ),
    ] {
        let response = shared(
            fixture
                .client
                .post(format!("{}/v1/threads/{}/{path}", fixture.base, thread.id)),
        )
        .json(&body)
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN, "{path}");
    }
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

#[tokio::test]
async fn the_chats_rules_keep_a_mention_from_it_without_asking_and_its_lists_say_so() {
    let fixture = fixture().await;
    let thread = fixture.thread().await;
    // The list asked for this chat says which server it cannot reach.
    let servers: Vec<McpServerInfo> = fixture
        .client
        .get(format!("{}/v1/mcp?threadId={}", fixture.base, thread.id))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let off: Vec<_> = servers
        .iter()
        .map(|server| (server.name.as_str(), server.turned_off))
        .collect();
    assert_eq!(
        off,
        [("docs", None), ("wiki", None), ("hidden", Some(true))]
    );
    let plain: Vec<McpServerInfo> = fixture
        .client
        .get(format!("{}/v1/mcp", fixture.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(plain.iter().all(|server| server.turned_off.is_none()));
    fixture
        .client
        .post(format!("{}/v1/threads/{}/turns", fixture.base, thread.id))
        .json(&json!({
            "requestId": RequestId::new(),
            "text": "Read these",
            "resources": [
                {"server": "docs", "uri": "docs://private", "name": "private"},
                {"server": "hidden", "uri": "docs://setup", "name": "setup"},
                // Asked of the model by a rule; the person's pick is not.
                {"server": "wiki", "uri": "docs://setup", "name": "setup"},
            ],
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let sent = user_messages(&fixture.items(&thread).await);
    let refused = "(not read)</summary>\n\n```\na rule keeps this resource from the model\n```";
    assert!(
        sent[0].contains(&format!("private · docs:docs://private {refused}")),
        "{sent:?}"
    );
    assert!(
        sent[0].contains(&format!("setup · hidden:docs://setup {refused}")),
        "{sent:?}"
    );
    assert!(sent[0].contains("```\n# wiki setup\n```"), "{sent:?}");
    assert!(fixture.runtime.pending_permissions().is_empty());
    fixture.handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_slow_server_has_ten_seconds_and_a_client_that_leaves_keeps_its_message() {
    let fixture = fixture().await;
    let thread = fixture.thread().await;
    let request = RequestId::new();
    let body = json!({
        "requestId": request,
        "text": "Wait for it",
        "resources": [
            {"server": "docs", "uri": "slow://", "name": "slow"},
            {"server": "docs", "uri": "docs://setup", "name": "setup"},
        ],
    });
    // The client gives up after a second; the message is still admitted.
    let started = std::time::Instant::now();
    let gave_up = fixture
        .client
        .post(format!("{}/v1/threads/{}/turns", fixture.base, thread.id))
        .timeout(Duration::from_secs(1))
        .json(&body)
        .send()
        .await;
    assert!(gave_up.is_err(), "{gave_up:?}");
    // Its retry while the reads still run waits for them, and reads nothing.
    let retried: Turn = fixture
        .client
        .post(format!("{}/v1/threads/{}/turns", fixture.base, thread.id))
        .json(&body)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retried.request_id, request);
    let sent = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let sent = user_messages(
                &fixture
                    .client
                    .get(format!("{}/v1/threads/{}/items", fixture.base, thread.id))
                    .send()
                    .await
                    .unwrap()
                    .json::<Vec<Item>>()
                    .await
                    .unwrap(),
            );
            if !sent.is_empty() {
                return sent;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the message is admitted after its client left");
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_secs(9) && waited < Duration::from_secs(15),
        "{waited:?}"
    );
    assert!(
        sent[0].contains(
            "slow · docs:slow:// (not read)</summary>\n\n```\ndid not answer within 10 s\n```"
        ),
        "{sent:?}"
    );
    assert!(sent[0].contains("# docs setup"), "{sent:?}");
    wait_idle(&fixture.runtime, thread.id).await;
    // A retry once it ran finds that turn at once, reading nothing either.
    let started = std::time::Instant::now();
    let turn: Turn = fixture
        .client
        .post(format!("{}/v1/threads/{}/turns", fixture.base, thread.id))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(turn.request_id, request);
    assert_eq!(user_messages(&fixture.items(&thread).await).len(), 1);
    assert_eq!(
        *fixture.docs.reads.lock().unwrap(),
        ["docs:slow://", "docs:docs://setup"]
    );
    fixture.handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_mention_follows_the_agent_its_message_runs_under() {
    let fixture = fixture().await;
    let thread = fixture.thread().await;
    let off = |agent: Option<&str>| {
        let url = match agent {
            Some(agent) => format!(
                "{}/v1/mcp?threadId={}&agent={agent}",
                fixture.base, thread.id
            ),
            None => format!("{}/v1/mcp?threadId={}", fixture.base, thread.id),
        };
        let client = fixture.client.clone();
        async move {
            client
                .get(url)
                .send()
                .await
                .unwrap()
                .json::<Vec<McpServerInfo>>()
                .await
                .unwrap()
                .into_iter()
                .filter(|server| server.turned_off == Some(true))
                .map(|server| server.name)
                .collect::<Vec<_>>()
        }
    };
    // The agent named wins over the chat's.
    assert_eq!(off(None).await, ["hidden"]);
    assert_eq!(off(Some("nodocs")).await, ["docs", "hidden"]);
    let body = |agent: Option<&str>| {
        json!({
            "requestId": RequestId::new(),
            "text": "Read it",
            "agent": agent,
            "resources": [{"server": "docs", "uri": "docs://setup", "name": "setup"}],
        })
    };
    for agent in [Some("nodocs"), None] {
        fixture
            .client
            .post(format!("{}/v1/threads/{}/turns", fixture.base, thread.id))
            .json(&body(agent))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        wait_idle(&fixture.runtime, thread.id).await;
    }
    let sent = user_messages(&fixture.items(&thread).await);
    assert!(
        sent[0].contains("(not read)</summary>\n\n```\na rule keeps this resource from the model"),
        "{sent:?}"
    );
    assert!(sent[1].contains("# docs setup"), "{sent:?}");
    fixture.handle.shutdown().await.unwrap();

    // Steering, a message joins the running turn's agent.
    let provider = SlowText::default();
    let fixture = fixture_with(Arc::new(provider.clone())).await;
    let thread = fixture.thread().await;
    fixture
        .client
        .post(format!("{}/v1/threads/{}/turns", fixture.base, thread.id))
        .json(&json!({"requestId": RequestId::new(), "text": "Hold on", "agent": "nodocs"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), provider.first_started.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let steer = json!({
        "requestId": RequestId::new(),
        "text": "And this",
        "resources": [{"server": "docs", "uri": "docs://setup", "name": "setup"}],
    });
    // Its retry finds it steered: nothing is read or delivered twice.
    for _ in 0..2 {
        fixture
            .client
            .post(format!("{}/v1/threads/{}/input", fixture.base, thread.id))
            .json(&steer)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    // Queued, a message runs later as a turn of its own, under the chat's
    // agent, so its mention is read.
    fixture
        .client
        .post(format!("{}/v1/threads/{}/input", fixture.base, thread.id))
        .json(&json!({
            "requestId": RequestId::new(),
            "delivery": "queue",
            "text": "Then this",
            "resources": [{"server": "docs", "uri": "docs://setup", "name": "setup"}],
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    provider.release_first.add_permits(1);
    let sent = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let sent = user_messages(&fixture.items(&thread).await);
            if sent.len() == 3 {
                return sent;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the queued message runs");
    assert!(
        sent[1].starts_with("And this")
            && sent[1].contains("a rule keeps this resource from the model"),
        "{sent:?}"
    );
    assert!(
        sent[2].starts_with("Then this") && sent[2].contains("# docs setup"),
        "{sent:?}"
    );
    assert_eq!(*fixture.docs.reads.lock().unwrap(), ["docs:docs://setup"]);
    fixture.handle.shutdown().await.unwrap();
}
