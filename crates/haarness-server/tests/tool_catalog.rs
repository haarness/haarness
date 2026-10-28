use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::stream;
use haarness_core::{
    AgentRuntime, CoreResult, EventBus, LazyToolSettings, ModelProvider, PermissionDecision,
    PermissionRule, ProviderEvent, ProviderMessage, ProviderRequest, ProviderStream, RuntimeConfig,
    StaticProviderRegistry, Tool, ToolContext, ToolDefinition, ToolOutput, ToolRegistry,
};
use haarness_protocol::{
    CreateThreadRequest, LazyToolMode, ModelInfo, PermissionReply, PermissionReplyDecision,
    ProviderInfo, RequestId, StartTurnRequest, TurnStatus,
};
use haarness_store::SqliteStore;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct FixtureTool {
    name: &'static str,
    description: &'static str,
}

#[async_trait]
impl Tool for FixtureTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.into(),
            description: self.description.into(),
            input_schema: json!({
                "type": "object",
                "properties": {"action": {"type": "string"}},
                "additionalProperties": false
            }),
        }
    }

    fn side_effectful(&self) -> bool {
        false
    }

    async fn execute(&self, arguments: Value, _context: ToolContext) -> CoreResult<ToolOutput> {
        Ok(ToolOutput {
            text: format!("{} executed with {arguments}", self.name),
            metadata: json!({"fixture": self.name}),
        })
    }
}

struct FixtureRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl FixtureRegistry {
    fn new() -> Self {
        let tools = [
            FixtureTool {
                name: "read",
                description: "Read a local file",
            },
            FixtureTool {
                name: "session_manage",
                description: "List and inspect durable sessions",
            },
        ]
        .into_iter()
        .map(|tool| (tool.name.into(), Arc::new(tool) as Arc<dyn Tool>))
        .collect();
        Self { tools }
    }

    fn with_counting_write(executions: Arc<AtomicUsize>) -> Self {
        let mut registry = Self::new();
        registry.tools.insert(
            "write".into(),
            Arc::new(CountingWriteTool { executions }) as Arc<dyn Tool>,
        );
        registry
    }
}

struct CountingWriteTool {
    executions: Arc<AtomicUsize>,
}

#[test]
fn namespace_resolution_preserves_exact_registrations_and_unknown_names() {
    let mut registry = FixtureRegistry::new();
    assert_eq!(registry.resolve_name("functions.read"), "read");
    assert_eq!(
        registry.resolve_name("functions.missing"),
        "functions.missing"
    );
    assert_eq!(registry.resolve_name("other.read"), "other.read");
    registry.tools.insert(
        "functions.read".into(),
        Arc::new(FixtureTool {
            name: "functions.read",
            description: "An exact registered name",
        }),
    );
    assert_eq!(registry.resolve_name("functions.read"), "functions.read");
}

#[async_trait]
impl Tool for CountingWriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write".into(),
            description: "Write a fixture file".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        }
    }

    fn side_effectful(&self) -> bool {
        true
    }

    async fn execute(&self, _arguments: Value, _context: ToolContext) -> CoreResult<ToolOutput> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput {
            text: "write executed".into(),
            metadata: Value::Null,
        })
    }
}

impl ToolRegistry for FixtureRegistry {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    fn list(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|tool| tool.definition()).collect()
    }
}

#[derive(Clone, Default)]
struct LazyProvider {
    round: Arc<std::sync::atomic::AtomicUsize>,
    schemas: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
}

#[async_trait]
impl ModelProvider for LazyProvider {
    fn id(&self) -> &str {
        "lazy"
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "lazy".into(),
            name: "Lazy fixture".into(),
            authenticated: true,
            models: vec![ModelInfo {
                id: "lazy-1".into(),
                display_name: "Lazy fixture".into(),
                reasoning: false,
                reasoning_efforts: Vec::new(),
                images: false,
                video: false,
                tools: true,
                context_window: Some(128_000),
                max_output_tokens: Some(2_000),
                pricing: None,
            }],
        }
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        use std::sync::atomic::Ordering;

        self.schemas
            .lock()
            .unwrap()
            .push(request.tools.iter().map(|tool| tool.name.clone()).collect());
        let round = self.round.fetch_add(1, Ordering::SeqCst);
        let event = match round {
            0 => ProviderEvent::ToolCall {
                call_id: "load-1".into(),
                name: "load_tool".into(),
                arguments: json!({"queries": ["sessions"]}),
                provider_metadata: Value::Null,
            },
            1 => {
                assert!(request.messages.iter().any(|message| {
                    matches!(message, ProviderMessage::ToolResult { name, is_error: false, .. }
                        if name == "load_tool")
                }));
                ProviderEvent::ToolCall {
                    call_id: "session-1".into(),
                    name: "session_manage".into(),
                    arguments: json!({"action": "list"}),
                    provider_metadata: Value::Null,
                }
            }
            _ => ProviderEvent::TextDelta("Loaded session tool completed end to end".into()),
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(event),
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }),
        ])))
    }
}

#[derive(Clone, Default)]
struct PermissionRaceProvider {
    round: Arc<AtomicUsize>,
}

#[async_trait]
impl ModelProvider for PermissionRaceProvider {
    fn id(&self) -> &str {
        "permission-race"
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "permission-race".into(),
            name: "Permission race fixture".into(),
            authenticated: true,
            models: vec![ModelInfo {
                id: "permission-race-1".into(),
                display_name: "Permission race fixture".into(),
                reasoning: false,
                reasoning_efforts: Vec::new(),
                images: false,
                video: false,
                tools: true,
                context_window: Some(128_000),
                max_output_tokens: Some(2_000),
                pricing: None,
            }],
        }
    }

    async fn stream(
        &self,
        _request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        let event = if self.round.fetch_add(1, Ordering::SeqCst) == 0 {
            ProviderEvent::ToolCall {
                call_id: "write-1".into(),
                name: "write".into(),
                arguments: json!({"path": "late.txt", "content": "must not run"}),
                provider_metadata: Value::Null,
            }
        } else {
            ProviderEvent::TextDelta("permission race handled".into())
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(event),
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }),
        ])))
    }
}

fn turn_request(text: &str) -> StartTurnRequest {
    StartTurnRequest {
        request_id: RequestId::new(),
        text: text.into(),
        attachments: Vec::new(),
        resources: Vec::new(),
        synthetic: false,
        auto_approve: true,
        agent: None,
        provider: None,
        model: None,
        reasoning_effort: None,
        tools: None,
        command: None,
    }
}

fn ask_write_config() -> RuntimeConfig {
    RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        base_rules: vec![PermissionRule {
            pattern: "edit:*".into(),
            decision: PermissionDecision::Ask,
        }],
        ..RuntimeConfig::default()
    }
}

async fn wait_done(runtime: &AgentRuntime, id: haarness_protocol::TurnId) {
    for _ in 0..500 {
        let turn = runtime.turn(id).await.unwrap();
        if !matches!(turn.status, TurnStatus::Queued | TurnStatus::Running) {
            assert_eq!(turn.status, TurnStatus::Completed, "{:?}", turn.error);
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("turn did not finish");
}

async fn wait_permission(runtime: &AgentRuntime) -> String {
    for _ in 0..500 {
        if let Some(permission) = runtime.pending_permissions().into_iter().next() {
            return permission.permission_id;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("permission prompt did not appear");
}

#[tokio::test]
async fn deferred_tool_is_loaded_then_executed_on_the_following_provider_round() {
    let provider = LazyProvider::default();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(FixtureRegistry::new()),
        EventBus::new(128),
        PathBuf::from("."),
        String::new(),
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            lazy_tools: LazyToolSettings {
                mode: LazyToolMode::Always,
                ..LazyToolSettings::default()
            },
            ..RuntimeConfig::default()
        },
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Lazy catalog E2E".into()),
            provider: Some("lazy".into()),
            model: Some("lazy-1".into()),
            ..CreateThreadRequest::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request("Inspect the sessions using session_manage"),
        )
        .await
        .unwrap();
    wait_done(&runtime, turn.id).await;

    let schemas = provider.schemas.lock().unwrap().clone();
    assert_eq!(schemas.len(), 3);
    assert!(schemas[0].contains(&"read".into()));
    assert!(schemas[0].contains(&"load_tool".into()));
    assert!(!schemas[0].contains(&"session_manage".into()));
    assert!(schemas[1].contains(&"session_manage".into()));
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(items.iter().any(|item| {
        matches!(&item.data, haarness_protocol::ItemData::ToolResult { name, is_error: false, .. }
            if name == "session_manage")
    }));
    assert!(items.iter().any(|item| {
        matches!(&item.data, haarness_protocol::ItemData::AssistantMessage { text }
            if text.contains("completed end to end"))
    }));
}

#[tokio::test]
async fn direct_tool_disabled_while_permission_is_pending_never_executes() {
    let root = std::env::temp_dir().join(format!("haarness-tool-race-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let executions = Arc::new(AtomicUsize::new(0));
    let runtime = Arc::new(AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(Vec::new())),
        Arc::new(FixtureRegistry::with_counting_write(executions.clone())),
        EventBus::new(32),
        root.clone(),
        String::new(),
        ask_write_config(),
    ));
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Direct disable race".into()),
            ..CreateThreadRequest::default()
        })
        .await
        .unwrap();
    let call = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            runtime
                .run_direct_tool(
                    thread.id,
                    "write",
                    json!({"path": "late.txt", "content": "must not run"}),
                    CancellationToken::new(),
                )
                .await
        }
    });
    let permission_id = wait_permission(&runtime).await;
    runtime
        .set_tool_enabled("write", Some(false))
        .await
        .unwrap();
    runtime
        .reply_permission(
            &permission_id,
            PermissionReply {
                decision: PermissionReplyDecision::Once,
                feedback: None,
            },
        )
        .await
        .unwrap();
    let result = call.await.unwrap().unwrap();
    assert!(result.is_error);
    assert!(result.output.text.contains("disabled"));
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert!(!root.join("late.txt").exists());
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(matches!(
        items.last().map(|item| &item.data),
        Some(haarness_protocol::ItemData::ToolResult {
            name,
            is_error: true,
            ..
        }) if name == "write"
    ));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn provider_tool_disabled_while_permission_is_pending_never_executes() {
    let root =
        std::env::temp_dir().join(format!("haarness-provider-race-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let executions = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(PermissionRaceProvider::default()) as Arc<dyn ModelProvider>;
    let runtime = Arc::new(AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![provider])),
        Arc::new(FixtureRegistry::with_counting_write(executions.clone())),
        EventBus::new(32),
        root.clone(),
        String::new(),
        ask_write_config(),
    ));
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Provider disable race".into()),
            provider: Some("permission-race".into()),
            model: Some("permission-race-1".into()),
            ..CreateThreadRequest::default()
        })
        .await
        .unwrap();
    let mut request = turn_request("write the fixture");
    request.auto_approve = false;
    let turn = runtime.start_turn(thread.id, request).await.unwrap();
    let permission_id = wait_permission(&runtime).await;
    runtime
        .set_tool_enabled("write", Some(false))
        .await
        .unwrap();
    runtime
        .reply_permission(
            &permission_id,
            PermissionReply {
                decision: PermissionReplyDecision::Once,
                feedback: None,
            },
        )
        .await
        .unwrap();
    wait_done(&runtime, turn.id).await;
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert!(!root.join("late.txt").exists());
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(items.iter().any(|item| {
        matches!(&item.data, haarness_protocol::ItemData::ToolResult {
            name,
            output,
            is_error: true,
            ..
        } if name == "write" && output.contains("disabled"))
    }));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn availability_override_is_execution_authority_and_survives_recovery() {
    let root = std::env::temp_dir().join(format!("haarness-tool-catalog-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let store = Arc::new(SqliteStore::open(root.join("state.db")).unwrap());
    let runtime = AgentRuntime::with_config(
        store.clone(),
        Arc::new(StaticProviderRegistry::new(Vec::new())),
        Arc::new(FixtureRegistry::new()),
        EventBus::new(32),
        root.clone(),
        String::new(),
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            ..RuntimeConfig::default()
        },
    );
    runtime.recover().await.unwrap();
    let thread = runtime
        .create_thread(CreateThreadRequest::default())
        .await
        .unwrap();
    runtime
        .set_tool_enabled("session_manage", Some(false))
        .await
        .unwrap();
    runtime
        .set_lazy_tool_mode(Some(LazyToolMode::Off))
        .await
        .unwrap();
    let error = runtime
        .run_direct_tool(
            thread.id,
            "session_manage",
            json!({"action": "list"}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("disabled"));

    let restarted = AgentRuntime::with_config(
        store.clone(),
        Arc::new(StaticProviderRegistry::new(Vec::new())),
        Arc::new(FixtureRegistry::new()),
        EventBus::new(32),
        root.clone(),
        String::new(),
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            lazy_tools: LazyToolSettings {
                mode: LazyToolMode::Always,
                ..LazyToolSettings::default()
            },
            ..RuntimeConfig::default()
        },
    );
    restarted.recover().await.unwrap();
    let catalog = restarted.tool_catalog(Some(thread.id)).await.unwrap();
    assert!(
        !catalog
            .tools
            .iter()
            .find(|tool| tool.id == "session_manage")
            .unwrap()
            .enabled
    );
    assert_eq!(catalog.lazy_mode, LazyToolMode::Off);

    let reset = restarted.set_lazy_tool_mode(None).await.unwrap();
    assert_eq!(reset.lazy_mode, LazyToolMode::Always);
    drop(restarted);

    let after_reset = AgentRuntime::with_config(
        store,
        Arc::new(StaticProviderRegistry::new(Vec::new())),
        Arc::new(FixtureRegistry::new()),
        EventBus::new(32),
        root.clone(),
        String::new(),
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            lazy_tools: LazyToolSettings {
                mode: LazyToolMode::Always,
                ..LazyToolSettings::default()
            },
            ..RuntimeConfig::default()
        },
    );
    after_reset.recover().await.unwrap();
    assert_eq!(
        after_reset.tool_catalog(None).await.unwrap().lazy_mode,
        LazyToolMode::Always
    );

    drop(after_reset);
    drop(runtime);
    std::fs::remove_dir_all(root).unwrap();
}

#[derive(Default)]
struct AllowedProvider {
    requests: std::sync::Mutex<Vec<ProviderRequest>>,
}
#[async_trait]
impl ModelProvider for AllowedProvider {
    fn id(&self) -> &str {
        "allowed"
    }
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id().into(),
            name: "Allowed tools fixture".into(),
            authenticated: true,
            models: Vec::new(),
        }
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        _: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        let completed = request.messages.iter().any(|message| matches!(message, ProviderMessage::ToolResult { name, is_error: false, .. } if name == "session_manage"));
        self.requests.lock().unwrap().push(request);
        Ok(Box::pin(stream::iter(vec![
            Ok(if completed {
                ProviderEvent::TextDelta("Allowed tool completed".into())
            } else {
                ProviderEvent::ToolCall {
                    call_id: "allowed-call".into(),
                    name: "session_manage".into(),
                    arguments: json!({}),
                    provider_metadata: Value::Null,
                }
            }),
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }),
        ])))
    }
}

#[tokio::test]
async fn denied_loader_restores_only_allowed_tools_and_completes_the_turn() {
    for (rules, mask) in [
        (
            vec![
                ("*", PermissionDecision::Allow),
                ("load_tool:*", PermissionDecision::Deny),
            ],
            None,
        ),
        (
            vec![
                ("*", PermissionDecision::Deny),
                ("session_manage:*", PermissionDecision::Allow),
            ],
            None,
        ),
        (
            vec![("*", PermissionDecision::Allow)],
            Some(HashMap::from([("load_tool".into(), false)])),
        ),
    ] {
        let provider = Arc::new(AllowedProvider::default());
        let runtime = AgentRuntime::with_config(
            Arc::new(SqliteStore::in_memory().unwrap()),
            Arc::new(StaticProviderRegistry::new(vec![
                provider.clone() as Arc<dyn ModelProvider>
            ])),
            Arc::new(FixtureRegistry::new()),
            EventBus::new(128),
            PathBuf::from("."),
            String::new(),
            RuntimeConfig {
                approval_mode: haarness_protocol::ApprovalMode::Review,
                base_rules: rules
                    .into_iter()
                    .map(|(pattern, decision)| PermissionRule {
                        pattern: pattern.into(),
                        decision,
                    })
                    .collect(),
                lazy_tools: LazyToolSettings {
                    mode: LazyToolMode::Always,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let thread = runtime
            .create_thread(CreateThreadRequest {
                title: Some("Allowed tool fixture".into()),
                provider: Some("allowed".into()),
                model: Some("fixture".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let mut input = turn_request("Inspect the sessions with session_manage");
        input.tools = mask;
        let turn = runtime.start_turn(thread.id, input).await.unwrap();
        wait_done(&runtime, turn.id).await;
        {
            let requests = provider.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert!(
                requests[0]
                    .tools
                    .iter()
                    .any(|tool| tool.name == "session_manage")
            );
            assert!(
                !requests[0]
                    .tools
                    .iter()
                    .any(|tool| tool.name == "load_tool")
            );
            assert!(!requests[0].system.contains("call load_tool"));
        }
        assert!(runtime.items(thread.id, None).await.unwrap().iter().any(|item| matches!(&item.data, haarness_protocol::ItemData::ToolResult { name, is_error: false, .. } if name == "session_manage")));
    }
}

/// The two resource tools as the server registers them, answering with the
/// servers the runtime scoped each call to.
struct ResourceFixtures {
    tools: HashMap<String, Arc<dyn Tool>>,
}

struct ScopedResourceTool(&'static str);

#[async_trait]
impl Tool for ScopedResourceTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.0.into(),
            description: "MCP resources".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"server": {"type": "string"}, "uri": {"type": "string"}},
                "additionalProperties": false
            }),
        }
    }

    fn side_effectful(&self) -> bool {
        false
    }

    async fn execute(&self, arguments: Value, _context: ToolContext) -> CoreResult<ToolOutput> {
        Ok(ToolOutput {
            text: arguments["__servers"].to_string(),
            metadata: Value::Null,
        })
    }
}

impl ResourceFixtures {
    fn new() -> Self {
        let mut tools = FixtureRegistry::new().tools;
        for name in ["mcp_resource_list", "mcp_resource_read"] {
            tools.insert(
                name.into(),
                Arc::new(ScopedResourceTool(name)) as Arc<dyn Tool>,
            );
        }
        Self { tools }
    }
}

impl ToolRegistry for ResourceFixtures {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    fn list(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|tool| tool.definition()).collect()
    }

    fn resource_servers(&self) -> Vec<String> {
        vec!["docs".into(), "hidden".into()]
    }

    fn tool_server(&self, name: &str) -> Option<String> {
        (name == "mcp__a__b__d").then(|| "a__b".into())
    }
}

#[derive(Clone, Default)]
struct SchemaRecorder {
    schemas: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
}

#[async_trait]
impl ModelProvider for SchemaRecorder {
    fn id(&self) -> &str {
        "schemas"
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "schemas".into(),
            name: "Schema recorder".into(),
            authenticated: true,
            models: vec![ModelInfo {
                id: "schemas-1".into(),
                display_name: "Schema recorder".into(),
                reasoning: false,
                reasoning_efforts: Vec::new(),
                images: false,
                video: false,
                tools: true,
                context_window: Some(128_000),
                max_output_tokens: Some(2_000),
                pricing: None,
            }],
        }
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        self.schemas
            .lock()
            .unwrap()
            .push(request.tools.iter().map(|tool| tool.name.clone()).collect());
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderEvent::TextDelta("done".into())),
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }),
        ])))
    }
}

fn deny(pattern: &str) -> PermissionRule {
    PermissionRule {
        pattern: pattern.into(),
        decision: PermissionDecision::Deny,
    }
}

/// A rule that takes every tool of a server away keeps its resources from
/// the model too: a read naming it is refused, a listing leaves it out, and
/// with no resource server left the resource tools are not offered.
#[tokio::test]
async fn a_server_whose_tools_are_all_denied_keeps_its_resources_from_the_model() {
    let runtime = |rules: Vec<PermissionRule>, provider: SchemaRecorder| {
        AgentRuntime::with_config(
            Arc::new(SqliteStore::in_memory().unwrap()),
            Arc::new(StaticProviderRegistry::new(vec![
                Arc::new(provider) as Arc<dyn ModelProvider>
            ])),
            Arc::new(ResourceFixtures::new()),
            EventBus::new(64),
            PathBuf::from("."),
            String::new(),
            RuntimeConfig {
                approval_mode: haarness_protocol::ApprovalMode::Review,
                base_rules: rules,
                ..RuntimeConfig::default()
            },
        )
    };
    let provider = SchemaRecorder::default();
    let partly = runtime(vec![deny("mcp__hidden__*:*")], provider.clone());
    let thread = partly
        .create_thread(CreateThreadRequest {
            title: Some("Resources".into()),
            provider: Some("schemas".into()),
            model: Some("schemas-1".into()),
            ..CreateThreadRequest::default()
        })
        .await
        .unwrap();
    let Err(refused) = partly
        .run_direct_tool(
            thread.id,
            "mcp_resource_read",
            json!({"server": "hidden", "uri": "hidden://notes"}),
            CancellationToken::new(),
        )
        .await
    else {
        panic!("a read of the denied server ran");
    };
    let refused = refused.to_string();
    assert!(
        refused.contains("MCP server \"hidden\" is turned off for this chat"),
        "{refused}"
    );
    let listed = partly
        .run_direct_tool(
            thread.id,
            "mcp_resource_list",
            json!({}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(listed.output.text, r#"["docs"]"#);
    // A rule about one tool of the server leaves its resources alone.
    let one_tool = runtime(
        vec![deny("mcp__hidden__search:*")],
        SchemaRecorder::default(),
    );
    let thread_two = one_tool
        .create_thread(CreateThreadRequest::default())
        .await
        .unwrap();
    let listed = one_tool
        .run_direct_tool(
            thread_two.id,
            "mcp_resource_list",
            json!({}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(listed.output.text, r#"["docs","hidden"]"#);
    // The tools are offered while one resource server is open, and not after.
    let turn = partly
        .start_turn(thread.id, turn_request("hello"))
        .await
        .unwrap();
    wait_done(&partly, turn.id).await;
    let closed_provider = SchemaRecorder::default();
    let closed = runtime(
        vec![deny("mcp__hidden__*:*"), deny("mcp__docs__*:*")],
        closed_provider.clone(),
    );
    let thread_three = closed
        .create_thread(CreateThreadRequest {
            provider: Some("schemas".into()),
            model: Some("schemas-1".into()),
            ..CreateThreadRequest::default()
        })
        .await
        .unwrap();
    let turn = closed
        .start_turn(thread_three.id, turn_request("hello"))
        .await
        .unwrap();
    wait_done(&closed, turn.id).await;
    let offered = provider.schemas.lock().unwrap()[0].clone();
    assert!(
        offered.contains(&"mcp_resource_read".to_owned()),
        "{offered:?}"
    );
    let offered = closed_provider.schemas.lock().unwrap()[0].clone();
    assert!(
        !offered.iter().any(|name| name.starts_with("mcp_resource_")),
        "{offered:?}"
    );
    assert!(offered.contains(&"read".to_owned()), "{offered:?}");
}

/// A server whose own name holds `__` (`a__b`) offers `mcp__a__b__d`:
/// clients see the tool under that server, not under `a`.
#[tokio::test]
async fn a_tool_is_listed_under_the_server_that_offers_it() {
    let mut fixtures = ResourceFixtures::new();
    fixtures.tools.insert(
        "mcp__a__b__d".into(),
        Arc::new(ScopedResourceTool("mcp__a__b__d")) as Arc<dyn Tool>,
    );
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(Vec::new())),
        Arc::new(fixtures),
        EventBus::new(64),
        PathBuf::from("."),
        String::new(),
        RuntimeConfig::default(),
    );
    let catalog = runtime.tool_catalog(None).await.unwrap();
    let tool = catalog
        .tools
        .iter()
        .find(|tool| tool.id == "mcp__a__b__d")
        .unwrap();
    assert_eq!(tool.source, "mcp:a__b");
}

/// A provider that, as the Anthropic and OpenAI APIs do, refuses a whole
/// request over one tool name outside `[A-Za-z0-9_-]{1,64}`, in its tools or
/// in its history. Its first response calls two tools by the names it was
/// sent; its next one is text.
#[derive(Clone, Default)]
struct StrictNames {
    requests: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
}

#[async_trait]
impl ModelProvider for StrictNames {
    fn id(&self) -> &str {
        "strict"
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "strict".into(),
            name: "Strict names".into(),
            authenticated: true,
            models: vec![ModelInfo {
                id: "strict-1".into(),
                display_name: "Strict names".into(),
                reasoning: false,
                reasoning_efforts: Vec::new(),
                images: false,
                video: false,
                tools: true,
                context_window: Some(128_000),
                max_output_tokens: Some(2_000),
                pricing: None,
            }],
        }
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        let tools: Vec<String> = request.tools.iter().map(|tool| tool.name.clone()).collect();
        let mut names = tools.clone();
        let mut answered = false;
        for message in &request.messages {
            match message {
                ProviderMessage::ToolCall { name, .. } => names.push(name.clone()),
                ProviderMessage::ToolResult { name, .. } => {
                    names.push(name.clone());
                    answered = true;
                }
                _ => {}
            }
        }
        self.requests.lock().unwrap().push(names.clone());
        if let Some(name) = names.iter().find(|name| {
            name.is_empty()
                || name.len() > 64
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        }) {
            return Err(haarness_core::CoreError::Provider(format!(
                "400 invalid_request_error: tool name {name:?} does not match ^[a-zA-Z0-9_-]{{1,64}}$"
            )));
        }
        if answered {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::TextDelta("done".into())),
                Ok(ProviderEvent::Finished {
                    reason: "stop".into(),
                }),
            ])));
        }
        // `Docs Server`'s `files.read` comes out as `Docs_Server`'s
        // `files_read` does, which keeps its name: the other has a hash.
        let docs = tools
            .iter()
            .find(|name| name.starts_with("mcp__Docs_Server__files_read_"))
            .cloned()
            .unwrap_or_default();
        let calls = [docs, "mcp__srv__what_".to_owned()]
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                Ok(ProviderEvent::ToolCall {
                    call_id: format!("call-{index}"),
                    name,
                    arguments: json!({}),
                    provider_metadata: Value::Null,
                })
            });
        Ok(Box::pin(stream::iter(calls.chain([Ok(
            ProviderEvent::Finished {
                reason: "tool_use".into(),
            },
        )]))))
    }
}

/// Tools named as providers refuse (a space, a dot, a `?`) reach a strict
/// provider under names it takes, beside a tool whose name comes out alike,
/// and its calls of them run the tools they stand for under their own names,
/// which the chat keeps; the next request sends that history the same way.
#[tokio::test]
async fn a_strict_provider_gets_names_it_takes_and_its_calls_reach_the_tools() {
    let mut fixtures = FixtureRegistry::new();
    for name in [
        "mcp__Docs Server__files.read",
        "mcp__Docs_Server__files_read",
        "mcp__srv__what?",
    ] {
        fixtures.tools.insert(
            name.into(),
            Arc::new(FixtureTool {
                name,
                description: "MCP fixture",
            }) as Arc<dyn Tool>,
        );
    }
    let provider = StrictNames::default();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(fixtures),
        EventBus::new(64),
        PathBuf::from("."),
        String::new(),
        RuntimeConfig {
            lazy_tools: LazyToolSettings {
                mode: LazyToolMode::Off,
                ..LazyToolSettings::default()
            },
            ..RuntimeConfig::default()
        },
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("strict".into()),
            model: Some("strict-1".into()),
            ..CreateThreadRequest::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request("read the docs"))
        .await
        .unwrap();
    wait_done(&runtime, turn.id).await;
    let requests = provider.requests.lock().unwrap().clone();
    // The turn's two requests (a title request may follow).
    assert!(requests.len() >= 2, "{requests:?}");
    assert!(requests[0].contains(&"mcp__Docs_Server__files_read".to_owned()));
    let results: Vec<(String, String)> = runtime
        .items(thread.id, None)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item.data {
            haarness_protocol::ItemData::ToolResult { name, output, .. } => Some((name, output)),
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        [
            (
                "mcp__Docs Server__files.read".to_owned(),
                "mcp__Docs Server__files.read executed with {}".to_owned()
            ),
            (
                "mcp__srv__what?".to_owned(),
                "mcp__srv__what? executed with {}".to_owned()
            ),
        ]
    );
}

/// A side question over a chat with calls of tools named as providers refuse
/// (`what?`) reaches a strict provider with names it takes, and names the
/// tools as the chat's turns do, so it reads their cached prefix: here the
/// tool whose name `files.read`'s comes out as is turned off, so it is in
/// the catalog but in neither request.
#[tokio::test]
async fn a_side_question_names_tools_as_the_turns_do() {
    let mut fixtures = FixtureRegistry::new();
    for name in [
        "mcp__Docs Server__files.read",
        "mcp__Docs_Server__files_read",
        "mcp__srv__what?",
    ] {
        fixtures.tools.insert(
            name.into(),
            Arc::new(FixtureTool {
                name,
                description: "MCP fixture",
            }) as Arc<dyn Tool>,
        );
    }
    let provider = StrictNames::default();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(fixtures),
        EventBus::new(64),
        PathBuf::from("."),
        String::new(),
        RuntimeConfig {
            lazy_tools: LazyToolSettings {
                mode: LazyToolMode::Off,
                ..LazyToolSettings::default()
            },
            tool_availability: HashMap::from([("mcp__Docs_Server__files_read".to_owned(), false)]),
            ..RuntimeConfig::default()
        },
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("strict".into()),
            model: Some("strict-1".into()),
            ..CreateThreadRequest::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request("read the docs"))
        .await
        .unwrap();
    wait_done(&runtime, turn.id).await;
    let turn_names = provider.requests.lock().unwrap()[0].clone();
    let docs = turn_names
        .iter()
        .find(|name| name.starts_with("mcp__Docs_Server__files_read_"))
        .cloned()
        .expect("files.read was sent under a name of its own");
    provider.requests.lock().unwrap().clear();
    let answer = runtime
        .btw(
            thread.id,
            haarness_protocol::BtwRequest {
                question: "what did it read?".into(),
                request_id: RequestId::new(),
                history: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(answer.answer, "done");
    let side = provider.requests.lock().unwrap()[0].clone();
    assert!(side.contains(&docs), "{side:?}");
    assert!(side.contains(&"mcp__srv__what_".to_owned()), "{side:?}");
}
