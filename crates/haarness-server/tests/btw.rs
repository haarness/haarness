use async_trait::async_trait;
use futures::stream;
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
use std::{path::PathBuf, sync::Arc};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
struct SideQuestionProbe {
    requests: Arc<std::sync::Mutex<Vec<ProviderRequest>>>,
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
        _: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        self.requests.lock().unwrap().push(request);
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderEvent::TextDelta("An isolated answer.".into())),
            Ok(ProviderEvent::Usage {
                input_tokens: 7,
                output_tokens: 3,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }),
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }),
        ])))
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
