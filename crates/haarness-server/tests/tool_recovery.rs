use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use futures::stream;
use haarness_core::{
    AgentRuntime, CoreResult, EventBus, HookDecision, HookPort, LazyToolSettings, ModelProvider,
    PermissionDecision, PermissionRule, ProviderEvent, ProviderMessage, ProviderRequest,
    ProviderStream, RuntimeConfig, StaticProviderRegistry, ToolRegistry,
};
use haarness_protocol::{
    CreateThreadRequest, ItemData, LazyToolMode, ProviderInfo, RequestId, StartTurnRequest,
    ThreadId, TurnStatus,
};
use haarness_server::NativeToolRegistry;
use haarness_store::SqliteStore;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Scripted {
    rounds: Mutex<VecDeque<Vec<ProviderEvent>>>,
    requests: Mutex<Vec<ProviderRequest>>,
}

#[async_trait]
impl ModelProvider for Scripted {
    fn id(&self) -> &str {
        "fixture"
    }
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "fixture".into(),
            name: "Recovery fixture".into(),
            authenticated: true,
            models: vec![],
        }
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        _: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        self.requests.lock().unwrap().push(request);
        let mut events = self
            .rounds
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![ProviderEvent::TextDelta("Recovered".into())]);
        events.push(ProviderEvent::Finished {
            reason: "done".into(),
        });
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

struct Fixture {
    root: PathBuf,
    runtime: AgentRuntime,
    provider: Arc<Scripted>,
}

impl Fixture {
    fn new(config: RuntimeConfig) -> Self {
        Self::with_hooks(config, None)
    }

    fn with_hooks(config: RuntimeConfig, hooks: Option<Arc<dyn HookPort>>) -> Self {
        let root = std::env::temp_dir().join(format!("haarness-recovery-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("sample.txt"), "real file contents").unwrap();
        let provider = Arc::new(Scripted::default());
        let runtime = AgentRuntime::with_config(
            Arc::new(SqliteStore::in_memory().unwrap()),
            Arc::new(StaticProviderRegistry::new(vec![
                provider.clone() as Arc<dyn ModelProvider>
            ])),
            Arc::new(NativeToolRegistry::new(root.clone())),
            EventBus::new(128),
            root.clone(),
            String::new(),
            config,
        );
        let runtime = match hooks {
            Some(hooks) => runtime.with_hooks(hooks),
            None => runtime,
        };
        Self {
            root,
            runtime,
            provider,
        }
    }

    async fn thread(&self) -> ThreadId {
        self.runtime
            .create_thread(CreateThreadRequest {
                title: Some("Tool recovery".into()),
                provider: Some("fixture".into()),
                model: Some("fixture".into()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
    }

    async fn run(&self, rounds: Vec<Vec<ProviderEvent>>) -> Vec<haarness_protocol::Item> {
        *self.provider.rounds.lock().unwrap() = rounds.into();
        let thread = self.thread().await;
        let turn = self
            .runtime
            .start_turn(
                thread,
                StartTurnRequest {
                    request_id: RequestId::new(),
                    text: "Inspect the sample file".into(),
                    attachments: vec![],
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
        tokio::time::timeout(Duration::from_secs(10), async {
            while self.runtime.active_turn(thread).await.unwrap().is_some() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("tool turn finishes without getting stuck on invalid input");
        let turn = self.runtime.turn(turn.id).await.unwrap();
        assert_eq!(turn.status, TurnStatus::Completed, "{:?}", turn.error);
        self.runtime.items(thread, None).await.unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn call(id: &str, name: &str, arguments: Value) -> ProviderEvent {
    ProviderEvent::ToolCall {
        call_id: id.into(),
        name: name.into(),
        arguments,
        provider_metadata: Value::Null,
    }
}

fn error(items: &[haarness_protocol::Item], id: &str) -> String {
    items
        .iter()
        .find_map(|item| match &item.data {
            ItemData::ToolResult {
                call_id,
                output,
                is_error: true,
                ..
            } if call_id == id => Some(output.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing error result for {id}"))
}

#[tokio::test]
async fn provider_receives_field_feedback_then_corrects_and_reads_real_file() {
    let fixture = Fixture::new(RuntimeConfig::default());
    let items = fixture
        .run(vec![
            vec![call("bad", "Read", json!({"path":42}))],
            vec![call(
                "fixed",
                "FUNCTIONS.Read",
                json!({"path":"sample.txt"}),
            )],
        ])
        .await;
    assert!(error(&items, "bad").contains("path: expected string"));
    let requests = fixture.provider.requests.lock().unwrap();
    assert!(requests[1].messages.iter().any(|message| matches!(message,
        ProviderMessage::ToolResult { name, output, is_error: true, .. }
            if name == "read" && output.contains("path: expected string") && output.contains("retry the same tool"))));
    assert!(requests[2].messages.iter().any(|message| matches!(message,
        ProviderMessage::ToolResult { name, output, is_error: false, .. }
            if name == "read" && output.contains("real file contents"))));
}

#[tokio::test]
async fn direct_and_runtime_owned_tools_validate_before_prompting_or_writing() {
    let fixture = Fixture::new(RuntimeConfig::default());
    let thread = fixture.thread().await;
    let failure = fixture
        .runtime
        .run_direct_tool(
            thread,
            "Write",
            json!({"path":"bad.txt","content":42}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(failure.contains("content: expected string"), "{failure}");
    assert!(
        fixture
            .runtime
            .items(thread, None)
            .await
            .unwrap()
            .is_empty()
    );
    let items = fixture
        .run(vec![vec![
            call("write", "Write", json!({"path":"bad.txt","content":42})),
            call("question", "Question", json!({"prompt":["wrong"]})),
            call(
                "todo",
                "Todowrite",
                json!({"todos":[{"content":"Task","status":"bogus","priority":"high"}]}),
            ),
            call("task", "Task", json!({"prompt":13})),
        ]])
        .await;
    for (id, field) in [
        ("write", "content: expected string"),
        ("question", "prompt: expected string"),
        ("todo", "todos[0].status: expected one of"),
        ("task", "prompt: expected string"),
    ] {
        assert!(error(&items, id).contains(field), "{}", error(&items, id));
    }
    assert!(fixture.runtime.pending_permissions().is_empty());
    assert!(fixture.runtime.pending_questions().is_empty());
    assert!(!fixture.root.join("bad.txt").exists());
}

#[tokio::test]
async fn batch_retains_valid_results_and_precise_invalid_child_feedback() {
    let mut config = RuntimeConfig::default();
    config.tool_availability.insert("write".into(), false);
    let fixture = Fixture::new(config);
    let items = fixture
        .run(vec![vec![call(
            "batch",
            "Batch",
            json!({"tool_calls":[
                {"tool":"Read","parameters":{"path":"sample.txt"}},
                {"tool":"Glob","parameters":{"pattern":false}},
                {"tool":"made_up","parameters":{}},
                {"tool":"FUNCTIONS.Write","parameters":{"path":"bad.txt","content":"blocked"}}
            ]}),
        )]])
        .await;
    let result = items
        .iter()
        .find_map(|item| match &item.data {
            ItemData::ToolResult {
                name,
                metadata,
                is_error: false,
                ..
            } if name == "batch" => Some(metadata),
            _ => None,
        })
        .unwrap();
    assert_eq!(result["failures"], 3);
    assert!(
        result["results"][0]["output"]
            .as_str()
            .unwrap()
            .contains("real file contents")
    );
    assert!(
        result["results"][1]["error"]
            .as_str()
            .unwrap()
            .contains("pattern: expected string")
    );
    assert!(
        result["results"][2]["error"]
            .as_str()
            .unwrap()
            .contains("Available tools")
    );
    assert!(
        result["results"][3]["error"]
            .as_str()
            .unwrap()
            .contains("disabled")
    );
    assert!(!fixture.root.join("bad.txt").exists());
}

#[tokio::test]
async fn unknown_disabled_and_deferred_errors_have_distinct_recovery_instructions() {
    let mut config = RuntimeConfig::default();
    config.tool_availability.insert("write".into(), false);
    config.base_rules.push(PermissionRule {
        pattern: "bash:*".into(),
        decision: PermissionDecision::Deny,
    });
    config.lazy_tools = LazyToolSettings {
        mode: LazyToolMode::Always,
        ..Default::default()
    };
    let fixture = Fixture::new(config);
    let items = fixture
        .run(vec![vec![
            call("unknown", "made_up", json!({})),
            call(
                "disabled",
                "FUNCTIONS.Write",
                json!({"path":"bad.txt","content":"blocked"}),
            ),
            call("denied", "Bash", json!({"command":"touch forbidden"})),
            call("deferred", "Image_Generate", json!({"prompt":"a tree"})),
            call(
                "bad-loader",
                "FUNCTIONS.Load_Tool",
                json!({"queries":[false]}),
            ),
        ]])
        .await;
    let unknown = error(&items, "unknown");
    assert!(unknown.contains("unknown tool: made_up"), "{unknown}");
    assert!(unknown.contains("Available tools"), "{unknown}");
    let suggestions = unknown.split("Available tools").nth(1).unwrap();
    assert!(
        !suggestions
            .split([',', ' ', '.'])
            .any(|name| name == "write" || name == "bash"),
        "{suggestions}"
    );
    assert!(!unknown.contains("is deferred"));
    for id in ["disabled", "denied"] {
        let failure = error(&items, id);
        assert!(
            failure.contains("disabled") && failure.contains("Do not retry"),
            "{failure}"
        );
        assert!(!failure.contains("Call load_tool"));
    }
    let deferred = error(&items, "deferred");
    assert!(
        deferred.contains("is deferred") && deferred.contains("\"queries\":[\"image_generate\"]"),
        "{deferred}"
    );
    assert!(error(&items, "bad-loader").contains("queries[0]: expected string"));
    let thread = fixture.thread().await;
    let unknown = fixture
        .runtime
        .run_direct_tool(thread, "made_up", json!({}), CancellationToken::new())
        .await
        .unwrap_err()
        .to_string();
    let suggestions = unknown.split("Available tools").nth(1).unwrap();
    assert!(
        !suggestions
            .split([',', ' '])
            .any(|name| name == "write" || name == "bash"),
        "{suggestions}"
    );
    assert!(!fixture.root.join("forbidden").exists());
}

struct Rewrite;
#[async_trait]
impl HookPort for Rewrite {
    async fn pre_tool_use(&self, _: ThreadId, tool: &str, _: &Value) -> HookDecision {
        assert_eq!(tool, "write", "hooks must see the canonical tool name");
        HookDecision {
            updated_input: Some(json!({"path":"bad.txt","content":false})),
            permission: Some(PermissionDecision::Allow),
            ..Default::default()
        }
    }
    async fn post_tool_use(
        &self,
        _: ThreadId,
        _: &str,
        _: &Value,
        _: &str,
        _: bool,
    ) -> Option<String> {
        None
    }
    async fn user_prompt_submit(&self, _: ThreadId, _: &str) -> HookDecision {
        HookDecision::default()
    }
    async fn permission_request(
        &self,
        _: ThreadId,
        _: &str,
        _: &[String],
        _: &Value,
    ) -> HookDecision {
        HookDecision::default()
    }
    async fn stop(&self, _: ThreadId) {}
}

#[tokio::test]
async fn hook_rewritten_arguments_are_revalidated_on_direct_and_provider_paths() {
    let fixture = Fixture::with_hooks(RuntimeConfig::default(), Some(Arc::new(Rewrite)));
    let thread = fixture.thread().await;
    let failure = fixture
        .runtime
        .run_direct_tool(
            thread,
            "Write",
            json!({"path":"bad.txt","content":"valid"}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(failure.contains("content: expected string"), "{failure}");
    let items = fixture
        .run(vec![vec![call(
            "rewrite",
            "Write",
            json!({"path":"bad.txt","content":"valid"}),
        )]])
        .await;
    assert!(error(&items, "rewrite").contains("content: expected string"));
    assert!(!fixture.root.join("bad.txt").exists());
}

#[test]
fn all_native_tools_advertise_compilable_validation_schemas() {
    let registry = NativeToolRegistry::new(PathBuf::from("."));
    for tool in registry.list() {
        jsonschema::validator_for(&tool.input_schema)
            .unwrap_or_else(|error| panic!("{} has an invalid schema: {error}", tool.name));
        jsonschema::validator_for(&registry.get(&tool.name).unwrap().validation_schema())
            .unwrap_or_else(|error| {
                panic!("{} has an invalid execution schema: {error}", tool.name)
            });
    }
}

#[tokio::test]
async fn documented_legacy_inputs_keep_their_existing_execution_semantics() {
    let mut config = RuntimeConfig::default();
    config.base_rules.push(PermissionRule {
        pattern: "bash:*".into(),
        decision: PermissionDecision::Allow,
    });
    let fixture = Fixture::new(config);
    let thread = fixture.thread().await;
    let result = fixture
        .runtime
        .run_direct_tool(
            thread,
            "Bash",
            json!({"command":"printf legacy-timeout-proof","timeout_ms":5000}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(!result.is_error, "{}", result.output.text);
    assert!(result.output.text.contains("legacy-timeout-proof"));
    let items = fixture
        .run(vec![vec![call(
            "todo",
            "Todowrite",
            json!({"items":[{"content":"Legacy todo","status":"pending"}]}),
        )]])
        .await;
    assert!(
        items
            .iter()
            .any(|item| matches!(&item.data, ItemData::Todo { items }
        if items.len() == 1 && items[0].content == "Legacy todo" && items[0].priority == "medium"))
    );
}

struct ShellEnv;
#[async_trait]
impl HookPort for ShellEnv {
    async fn pre_tool_use(&self, _: ThreadId, _: &str, _: &Value) -> HookDecision {
        HookDecision::default()
    }
    async fn post_tool_use(
        &self,
        _: ThreadId,
        _: &str,
        _: &Value,
        _: &str,
        _: bool,
    ) -> Option<String> {
        None
    }
    async fn user_prompt_submit(&self, _: ThreadId, _: &str) -> HookDecision {
        HookDecision::default()
    }
    async fn permission_request(
        &self,
        _: ThreadId,
        _: &str,
        _: &[String],
        _: &Value,
    ) -> HookDecision {
        HookDecision::default()
    }
    async fn stop(&self, _: ThreadId) {}
    async fn shell_env(
        &self,
        _: ThreadId,
        cwd: &std::path::Path,
    ) -> std::collections::BTreeMap<String, String> {
        std::collections::BTreeMap::from([(
            "HOOK_SHELL_ENV".to_owned(),
            format!("from-hook:{}", cwd.display()),
        )])
    }
}

#[tokio::test]
async fn shell_env_hooks_add_variables_without_touching_bash_arguments() {
    let mut config = RuntimeConfig::default();
    config.base_rules.push(PermissionRule {
        pattern: "bash:*".into(),
        decision: PermissionDecision::Allow,
    });
    let fixture = Fixture::with_hooks(config, Some(Arc::new(ShellEnv)));
    let thread = fixture.thread().await;
    let result = fixture
        .runtime
        .run_direct_tool(
            thread,
            "bash",
            json!({"command":"printf '%s|%s' \"$HOOK_SHELL_ENV\" \"$PATH\""}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(!result.is_error, "{}", result.output.text);
    assert_eq!(result.output.metadata["exit"], 0, "{}", result.output.text);
    let stdout = result.output.metadata["stdout"].as_str().unwrap();
    let (hook, path) = stdout.split_once('|').unwrap();
    assert_eq!(hook, format!("from-hook:{}", fixture.root.display()));
    assert!(
        !path.is_empty(),
        "the inherited environment must survive hook variables"
    );
    let items = fixture
        .run(vec![vec![call(
            "env",
            "bash",
            json!({"command":"printf %s \"$HOOK_SHELL_ENV\""}),
        )]])
        .await;
    let outputs: Vec<_> = items
        .iter()
        .filter_map(|item| match &item.data {
            ItemData::ToolResult {
                output, is_error, ..
            } => Some((output.clone(), *is_error)),
            _ => None,
        })
        .collect();
    assert_eq!(outputs.len(), 1, "{items:?}");
    assert!(!outputs[0].1, "{}", outputs[0].0);
    assert!(
        outputs[0]
            .0
            .contains(&format!("from-hook:{}", fixture.root.display())),
        "{}",
        outputs[0].0
    );
}
