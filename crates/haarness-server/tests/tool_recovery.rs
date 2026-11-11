use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use futures::stream;
use haarness_core::{
    AgentRuntime, CoreResult, EventBus, HookCall, HookDecision, HookPort, LazyToolSettings,
    ModelProvider, PermissionDecision, PermissionRule, ProviderEvent, ProviderMessage,
    ProviderRequest, ProviderStream, RuntimeConfig, StaticProviderRegistry, ToolRegistry,
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
        if !matches!(events.last(), Some(ProviderEvent::Finished { .. })) {
            events.push(ProviderEvent::Finished {
                reason: "done".into(),
            });
        }
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
        self.thread_of("fixture").await
    }

    async fn thread_of(&self, model: &str) -> ThreadId {
        self.runtime
            .create_thread(CreateThreadRequest {
                title: Some("Tool recovery".into()),
                provider: Some("fixture".into()),
                model: Some(model.into()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
    }

    async fn run(&self, rounds: Vec<Vec<ProviderEvent>>) -> Vec<haarness_protocol::Item> {
        self.run_as("fixture", rounds).await
    }

    /// A turn of a chat on `model`.
    async fn run_as(
        &self,
        model: &str,
        rounds: Vec<Vec<ProviderEvent>>,
    ) -> Vec<haarness_protocol::Item> {
        *self.provider.rounds.lock().unwrap() = rounds.into();
        let thread = self.thread_of(model).await;
        let turn = self
            .runtime
            .start_turn(
                thread,
                StartTurnRequest {
                    request_id: RequestId::new(),
                    text: "Inspect the sample file".into(),
                    attachments: vec![],
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
async fn the_patch_tool_is_offered_to_models_trained_on_it_and_still_runs_for_others() {
    let fixture = Fixture::new(RuntimeConfig::default());
    let offered = |fixture: &Fixture| {
        let requests = fixture.provider.requests.lock().unwrap();
        requests[0]
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>()
    };
    fixture.run_as("gpt-5.4", vec![]).await;
    let gpt = offered(&fixture);
    fixture.provider.requests.lock().unwrap().clear();
    // Claude, given the patch tool before, wrote diffs it could not read.
    // A patch it sends anyway is applied all the same.
    let patch = "*** Begin Patch\n*** Add File: added.txt\n+x\n*** End Patch\n";
    let items = fixture
        .run_as(
            "claude-sonnet-5-5",
            vec![vec![call("p1", "apply_patch", json!({ "input": patch }))]],
        )
        .await;
    let claude = offered(&fixture);
    assert!(gpt.iter().any(|name| name == "apply_patch"), "{gpt:?}");
    assert!(
        !claude.iter().any(|name| name == "apply_patch"),
        "{claude:?}"
    );
    for tools in [&gpt, &claude] {
        for name in ["edit", "multiedit", "write"] {
            assert!(tools.iter().any(|tool| tool == name), "{name} in {tools:?}");
        }
    }
    assert_eq!(
        tool_results(&items),
        vec![("added added.txt".to_owned(), false)]
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("added.txt")).unwrap(),
        "x\n"
    );
}

#[tokio::test]
async fn provider_receives_field_feedback_then_corrects_and_reads_real_file() {
    let fixture = Fixture::new(RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    });
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

/// A call whose arguments were not valid JSON never runs: the model is told
/// why, corrects the call, and the turn goes on. The refused call still goes
/// back to the provider as an object, as Claude requires of every tool call.
#[tokio::test]
async fn arguments_that_are_not_json_are_refused_and_the_model_retries() {
    let fixture = Fixture::new(RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    });
    // A quote the model left unescaped: no reading of it is sure.
    let broken = r#"{"path":"made.txt","content":"a"b"}"#;
    let items = fixture
        .run(vec![
            vec![call(
                "bad",
                "write",
                haarness_core::tool_call_arguments(broken),
            )],
            vec![call("fixed", "read", json!({"path":"sample.txt"}))],
        ])
        .await;
    let refusal = error(&items, "bad");
    assert!(
        refusal.contains("write tool did not run") && refusal.contains("not valid JSON"),
        "{refusal}"
    );
    assert!(
        !fixture.root.join("made.txt").exists(),
        "the call never ran"
    );
    let requests = fixture.provider.requests.lock().unwrap();
    assert!(requests[1].messages.iter().any(|message| matches!(message,
        ProviderMessage::ToolCall { call_id, arguments, .. }
            if call_id == "bad" && arguments.is_object())));
    assert!(requests[2].messages.iter().any(|message| matches!(message,
        ProviderMessage::ToolResult { name, output, is_error: false, .. }
            if name == "read" && output.contains("real file contents"))));
}

fn malformed() -> ProviderEvent {
    ProviderEvent::Finished {
        reason: "MALFORMED_FUNCTION_CALL".into(),
    }
}

fn asks_to_continue(request: &ProviderRequest) -> bool {
    matches!(request.messages.last(), Some(ProviderMessage::Text { role, content })
        if role == "user" && content.contains("ended before completion"))
}

#[tokio::test]
async fn gemini_malformed_call_alone_is_retried_with_the_recovery_note() {
    let fixture = Fixture::new(RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    });
    let items = fixture.run(vec![vec![malformed()]]).await;
    assert!(items.iter().any(
        |item| matches!(&item.data, ItemData::AssistantMessage { text } if text == "Recovered")
    ));
    assert!(
        !items
            .iter()
            .any(|item| matches!(&item.data, ItemData::Error { .. }))
    );
    let requests = fixture.provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(!asks_to_continue(&requests[0]));
    assert!(asks_to_continue(&requests[1]));
}

#[tokio::test]
async fn gemini_malformed_call_runs_the_valid_calls_before_it_and_continues() {
    let fixture = Fixture::new(RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    });
    let items = fixture
        .run(vec![vec![
            call("valid", "Read", json!({"path":"sample.txt"})),
            malformed(),
        ]])
        .await;
    assert!(items.iter().any(|item| matches!(&item.data,
        ItemData::ToolResult { call_id, output, is_error: false, .. }
            if call_id == "valid" && output.contains("real file contents"))));
    assert!(items.iter().any(
        |item| matches!(&item.data, ItemData::AssistantMessage { text } if text == "Recovered")
    ));
    let requests = fixture.provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages.iter().any(|message| matches!(message,
        ProviderMessage::ToolResult { call_id, is_error: false, .. } if call_id == "valid")));
    assert!(asks_to_continue(&requests[1]));
}

#[tokio::test]
async fn direct_and_runtime_owned_tools_validate_before_prompting_or_writing() {
    let fixture = Fixture::new(RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    });
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
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
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
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
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
            call("deferred", "Skill", json!({"name":"no-such-skill"})),
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
    // A tool left out only to keep requests small, called by its name, is
    // loaded and runs: the error is the skill tool's own, and the next
    // request carries its schema.
    let deferred = error(&items, "deferred");
    assert!(
        !deferred.contains("is deferred")
            && !deferred.contains("load_tool")
            && deferred.contains("skill"),
        "{deferred}"
    );
    {
        let requests = fixture.provider.requests.lock().unwrap();
        let offered = |index: usize| {
            requests[index]
                .tools
                .iter()
                .any(|tool| tool.name == "skill")
        };
        assert!(!offered(0) && offered(1));
    }
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
    async fn pre_tool_use(&self, _: &HookCall, tool: &str, _: &Value) -> HookDecision {
        assert_eq!(tool, "write", "hooks must see the canonical tool name");
        HookDecision {
            updated_input: Some(json!({"path":"bad.txt","content":false})),
            permission: Some(PermissionDecision::Allow),
            ..Default::default()
        }
    }
    async fn post_tool_use(
        &self,
        _: &HookCall,
        _: &str,
        _: &Value,
        _: &str,
        _: bool,
    ) -> HookDecision {
        HookDecision::default()
    }
    async fn user_prompt_submit(&self, _: &HookCall, _: &str) -> HookDecision {
        HookDecision::default()
    }
    async fn permission_request(
        &self,
        _: &HookCall,
        _: &str,
        _: &[String],
        _: Option<&[String]>,
        _: &Value,
    ) -> HookDecision {
        HookDecision::default()
    }
}

#[tokio::test]
async fn hook_rewritten_arguments_are_revalidated_on_direct_and_provider_paths() {
    let fixture = Fixture::with_hooks(
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            ..RuntimeConfig::default()
        },
        Some(Arc::new(Rewrite)),
    );
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
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
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

#[cfg(unix)]
struct ShellEnv;
#[cfg(unix)]
#[async_trait]
impl HookPort for ShellEnv {
    async fn pre_tool_use(&self, _: &HookCall, _: &str, _: &Value) -> HookDecision {
        HookDecision::default()
    }
    async fn post_tool_use(
        &self,
        _: &HookCall,
        _: &str,
        _: &Value,
        _: &str,
        _: bool,
    ) -> HookDecision {
        HookDecision::default()
    }
    async fn user_prompt_submit(&self, _: &HookCall, _: &str) -> HookDecision {
        HookDecision::default()
    }
    async fn permission_request(
        &self,
        _: &HookCall,
        _: &str,
        _: &[String],
        _: Option<&[String]>,
        _: &Value,
    ) -> HookDecision {
        HookDecision::default()
    }
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

// POSIX shell commands for the agent's shell tool, which runs cmd.exe on Windows.
#[cfg(unix)]
#[tokio::test]
async fn shell_env_hooks_add_variables_without_touching_bash_arguments() {
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
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

/// A `PermissionRequest` hook that allows, but only a rewritten command.
struct RewritingAllow;
#[async_trait]
impl HookPort for RewritingAllow {
    async fn pre_tool_use(&self, _: &HookCall, _: &str, _: &Value) -> HookDecision {
        HookDecision::default()
    }
    async fn post_tool_use(
        &self,
        _: &HookCall,
        _: &str,
        _: &Value,
        _: &str,
        _: bool,
    ) -> HookDecision {
        HookDecision::default()
    }
    async fn user_prompt_submit(&self, _: &HookCall, _: &str) -> HookDecision {
        HookDecision::default()
    }
    async fn permission_request(
        &self,
        _: &HookCall,
        _: &str,
        _: &[String],
        _: Option<&[String]>,
        _: &Value,
    ) -> HookDecision {
        HookDecision {
            permission: Some(PermissionDecision::Allow),
            updated_input: Some(json!({"command": "true"})),
            ..HookDecision::default()
        }
    }
}

#[tokio::test]
async fn a_permission_request_hook_allowing_other_input_leaves_the_prompt_to_the_person() {
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    config.base_rules.push(PermissionRule {
        pattern: "bash:*".into(),
        decision: PermissionDecision::Ask,
    });
    let fixture = Fixture::with_hooks(config, Some(Arc::new(RewritingAllow)));
    *fixture.provider.rounds.lock().unwrap() = vec![vec![call(
        "touch",
        "bash",
        json!({"command": "touch allowed-by-hook"}),
    )]]
    .into();
    let thread = fixture.thread().await;
    let turn = fixture
        .runtime
        .start_turn(
            thread,
            StartTurnRequest {
                request_id: RequestId::new(),
                text: "Touch the file".into(),
                attachments: vec![],
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
    // The hook approved `true`, not `touch allowed-by-hook`: the prompt
    // waits for a person, and the command does not run meanwhile.
    let pending = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(pending) = fixture.runtime.pending_permissions().pop() {
                return pending;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the prompt is asked");
    assert!(!fixture.root.join("allowed-by-hook").exists());
    fixture
        .runtime
        .reply_permission(
            &pending.permission_id,
            haarness_protocol::PermissionReply {
                decision: haarness_protocol::PermissionReplyDecision::Reject,
                feedback: None,
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while fixture.runtime.active_turn(thread).await.unwrap().is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the turn ends");
    assert_eq!(
        fixture.runtime.turn(turn.id).await.unwrap().status,
        TurnStatus::Completed
    );
    assert!(!fixture.root.join("allowed-by-hook").exists());
}

/// A `PermissionRequest` hook that allows, handing back the input it was
/// shown unchanged, as some hooks always fill `updatedInput`.
#[cfg(unix)]
struct EchoingAllow(std::sync::Mutex<Vec<Value>>);
#[cfg(unix)]
#[async_trait]
impl HookPort for EchoingAllow {
    async fn pre_tool_use(&self, _: &HookCall, _: &str, _: &Value) -> HookDecision {
        HookDecision::default()
    }
    async fn post_tool_use(
        &self,
        _: &HookCall,
        _: &str,
        _: &Value,
        _: &str,
        _: bool,
    ) -> HookDecision {
        HookDecision::default()
    }
    async fn user_prompt_submit(&self, _: &HookCall, _: &str) -> HookDecision {
        HookDecision::default()
    }
    async fn permission_request(
        &self,
        _: &HookCall,
        _: &str,
        _: &[String],
        _: Option<&[String]>,
        input: &Value,
    ) -> HookDecision {
        self.0.lock().unwrap().push(input.clone());
        HookDecision {
            permission: Some(PermissionDecision::Allow),
            updated_input: Some(input.clone()),
            ..HookDecision::default()
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_permission_request_hook_handing_back_the_same_input_still_allows() {
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    config.base_rules.push(PermissionRule {
        pattern: "bash:*".into(),
        decision: PermissionDecision::Ask,
    });
    let hook = Arc::new(EchoingAllow(std::sync::Mutex::new(Vec::new())));
    let fixture = Fixture::with_hooks(config, Some(hook.clone()));
    // A read through a link to a secret asks about the file it opens, in a
    // run of its own whose input names that file.
    std::fs::write(fixture.root.join(".env"), "ECHOED_SECRET=1").unwrap();
    std::os::unix::fs::symlink(".env", fixture.root.join("notes.txt")).unwrap();
    let items = fixture
        .run(vec![vec![
            call(
                "touch",
                "bash",
                json!({"command": "touch allowed-as-shown"}),
            ),
            call("read", "read", json!({"path": "notes.txt"})),
        ]])
        .await;
    assert!(fixture.runtime.pending_permissions().is_empty());
    assert!(fixture.root.join("allowed-as-shown").exists(), "{items:?}");
    assert!(
        items.iter().any(|item| matches!(&item.data,
            ItemData::ToolResult { name, output, is_error: false, .. }
                if name == "read" && output.contains("ECHOED_SECRET"))),
        "{items:?}"
    );
    // The resolved-path run was shown `.env`, and its echo still allowed it.
    assert!(
        hook.0
            .lock()
            .unwrap()
            .iter()
            .any(|input| input["path"] == ".env"),
        "{:?}",
        hook.0.lock().unwrap()
    );
}

/// A `PermissionRequest` hook that allows the first request it is asked
/// about, answering Always when `always` is set, and denies every request
/// after it. It records what Always each request offered it.
struct FirstAllow {
    always: bool,
    offered: std::sync::Mutex<Vec<Option<Vec<String>>>>,
}
impl FirstAllow {
    fn new(always: bool) -> Arc<Self> {
        Arc::new(Self {
            always,
            offered: std::sync::Mutex::new(Vec::new()),
        })
    }
}
#[async_trait]
impl HookPort for FirstAllow {
    async fn pre_tool_use(&self, _: &HookCall, _: &str, _: &Value) -> HookDecision {
        HookDecision::default()
    }
    async fn post_tool_use(
        &self,
        _: &HookCall,
        _: &str,
        _: &Value,
        _: &str,
        _: bool,
    ) -> HookDecision {
        HookDecision::default()
    }
    async fn user_prompt_submit(&self, _: &HookCall, _: &str) -> HookDecision {
        HookDecision::default()
    }
    async fn permission_request(
        &self,
        _: &HookCall,
        _: &str,
        _: &[String],
        always: Option<&[String]>,
        _: &Value,
    ) -> HookDecision {
        let mut offered = self.offered.lock().unwrap();
        offered.push(always.map(<[String]>::to_vec));
        if offered.len() > 1 {
            return HookDecision {
                block: Some("asked again".into()),
                permission: Some(PermissionDecision::Deny),
                ..HookDecision::default()
            };
        }
        HookDecision {
            permission: Some(PermissionDecision::Allow),
            always: self.always,
            ..HookDecision::default()
        }
    }
}

/// Runs `echo one`, then `echo two` in a later round, under `bash:*` ask.
async fn echo_twice(hook: Arc<FirstAllow>) -> (Fixture, Vec<haarness_protocol::Item>) {
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    config.base_rules.push(PermissionRule {
        pattern: "bash:*".into(),
        decision: PermissionDecision::Ask,
    });
    let fixture = Fixture::with_hooks(config, Some(hook));
    let items = fixture
        .run(vec![
            vec![call("one", "bash", json!({"command": "echo one"}))],
            vec![call("two", "bash", json!({"command": "echo two"}))],
        ])
        .await;
    (fixture, items)
}

fn tool_results(items: &[haarness_protocol::Item]) -> Vec<(String, bool)> {
    items
        .iter()
        .filter_map(|item| match &item.data {
            ItemData::ToolResult {
                output, is_error, ..
            } => Some((output.clone(), *is_error)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_permission_request_hooks_always_lets_the_next_command_with_its_prefix_run() {
    let hook = FirstAllow::new(true);
    let (fixture, items) = echo_twice(hook.clone()).await;
    let results = tool_results(&items);
    assert_eq!(results.len(), 2, "{items:?}");
    assert!(results.iter().all(|(_, error)| !error), "{results:?}");
    // The hook was asked once, and was told what Always would remember.
    let offered = hook.offered.lock().unwrap().clone();
    assert_eq!(offered, vec![Some(vec!["echo *".to_owned()])]);
    assert!(fixture.runtime.pending_permissions().is_empty());
}

#[tokio::test]
async fn a_permission_request_hooks_allow_without_always_is_asked_again() {
    let hook = FirstAllow::new(false);
    let (_fixture, items) = echo_twice(hook.clone()).await;
    let results = tool_results(&items);
    assert_eq!(results.len(), 2, "{items:?}");
    assert!(!results[0].1, "{results:?}");
    assert!(
        results[1].1 && results[1].0.contains("asked again"),
        "{results:?}"
    );
    assert_eq!(hook.offered.lock().unwrap().len(), 2);
}

/// A `PermissionRequest` hook that refuses every request, with its reason
/// when it gives one, or (`failing`) that fails to answer, with that error.
struct Refuses(Option<&'static str>, bool);
#[async_trait]
impl HookPort for Refuses {
    async fn pre_tool_use(&self, _: &HookCall, _: &str, _: &Value) -> HookDecision {
        HookDecision::default()
    }
    async fn post_tool_use(
        &self,
        _: &HookCall,
        _: &str,
        _: &Value,
        _: &str,
        _: bool,
    ) -> HookDecision {
        HookDecision::default()
    }
    async fn user_prompt_submit(&self, _: &HookCall, _: &str) -> HookDecision {
        HookDecision::default()
    }
    async fn permission_request(
        &self,
        _: &HookCall,
        _: &str,
        _: &[String],
        _: Option<&[String]>,
        _: &Value,
    ) -> HookDecision {
        if self.1 {
            return HookDecision {
                failure: self.0.map(str::to_owned),
                permission: Some(PermissionDecision::Deny),
                ..HookDecision::default()
            };
        }
        HookDecision {
            block: self.0.map(str::to_owned),
            permission: Some(PermissionDecision::Deny),
            ..HookDecision::default()
        }
    }
}

/// The model reads a hook's refusal as the hook's, and a person's Reject as
/// it always read.
#[tokio::test]
async fn a_hooks_refusal_reads_as_the_hooks_and_a_persons_reject_as_theirs() {
    let asks = || {
        let mut config = RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            ..RuntimeConfig::default()
        };
        config.base_rules.push(PermissionRule {
            pattern: "bash:*".into(),
            decision: PermissionDecision::Ask,
        });
        config
    };
    let echo = || vec![vec![call("one", "bash", json!({"command": "echo one"}))]];
    for (reason, failing, said, shown) in [
        (
            Some("not on this machine."),
            false,
            "A permission hook refused this action without asking the user: not on this \
             machine. Take a different approach, or ask the user if you need it.",
            json!({"refusedBy": "hook", "reason": "not on this machine."}),
        ),
        (
            None,
            false,
            "A permission hook refused this action without asking the user. Take a different \
             approach, or ask the user if you need it.",
            json!({"refusedBy": "hook", "reason": null}),
        ),
        // A hook that failed says so, for clients to tell it from a refusal.
        (
            Some("lookup timed out"),
            true,
            "A permission hook could not answer (lookup timed out), so this action was \
             refused without asking the user. Take a different approach, or ask the user if \
             you need it.",
            json!({"refusedBy": "hook", "reason": "lookup timed out", "failed": true}),
        ),
    ] {
        let fixture = Fixture::with_hooks(asks(), Some(Arc::new(Refuses(reason, failing))));
        let items = fixture.run(echo()).await;
        assert_eq!(tool_results(&items), vec![(said.to_owned(), true)]);
        // Clients show it as the hook's, from the result's metadata.
        let metadata = items
            .iter()
            .find_map(|item| match &item.data {
                ItemData::ToolResult { metadata, .. } => Some(metadata.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(metadata, shown);
        assert!(fixture.runtime.pending_permissions().is_empty());
    }
    for (feedback, said) in [
        (
            None,
            "The user rejected this action. Ask a question or take a different approach instead \
             of retrying.",
        ),
        (
            Some("not now"),
            "The user rejected this action and said: not now. Adjust your approach accordingly.",
        ),
    ] {
        let fixture = Fixture::new(asks());
        *fixture.provider.rounds.lock().unwrap() = echo().into();
        let thread = fixture.thread().await;
        fixture
            .runtime
            .start_turn(
                thread,
                StartTurnRequest {
                    request_id: RequestId::new(),
                    text: "Run it".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let asked = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(asked) = fixture.runtime.pending_permissions().into_iter().next() {
                    break asked;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the command asks a person");
        fixture
            .runtime
            .reply_permission(
                &asked.permission_id,
                haarness_protocol::PermissionReply {
                    decision: haarness_protocol::PermissionReplyDecision::Reject,
                    feedback: feedback.map(str::to_owned),
                },
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while fixture.runtime.active_turn(thread).await.unwrap().is_some() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the turn goes on after the Reject");
        let items = fixture.runtime.items(thread, None).await.unwrap();
        assert_eq!(tool_results(&items), vec![(said.to_owned(), true)]);
    }
}

#[tokio::test]
async fn a_permission_request_hook_cannot_answer_always_for_an_edit_of_haarness_config() {
    let hook = FirstAllow::new(true);
    let fixture = Fixture::with_hooks(
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            ..RuntimeConfig::default()
        },
        Some(hook.clone()),
    );
    *fixture.provider.rounds.lock().unwrap() = vec![vec![call(
        "config",
        "write",
        json!({"path": "haarness.json", "content": "{}"}),
    )]]
    .into();
    let thread = fixture.thread().await;
    fixture
        .runtime
        .start_turn(
            thread,
            StartTurnRequest {
                request_id: RequestId::new(),
                text: "Write the config".into(),
                attachments: vec![],
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
    // Only a person answers it: the hook's allow does not, and it was
    // offered no Always.
    let pending = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(pending) = fixture.runtime.pending_permissions().pop() {
                return pending;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the prompt is asked");
    assert_eq!(hook.offered.lock().unwrap().clone(), vec![None]);
    fixture
        .runtime
        .reply_permission(
            &pending.permission_id,
            haarness_protocol::PermissionReply {
                decision: haarness_protocol::PermissionReplyDecision::Reject,
                feedback: None,
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while fixture.runtime.active_turn(thread).await.unwrap().is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the turn ends");
    assert!(!fixture.root.join("haarness.json").exists());
}

/// A `PreToolUse` hook that answers every question with `answers`, copying
/// the question's input as a real hook does.
struct AnswersQuestions(Value);
#[async_trait]
impl HookPort for AnswersQuestions {
    async fn pre_tool_use(&self, _: &HookCall, tool: &str, arguments: &Value) -> HookDecision {
        if tool != "question" {
            return HookDecision::default();
        }
        let mut updated = arguments.clone();
        updated["answers"] = self.0.clone();
        HookDecision {
            updated_input: Some(updated),
            context: Some("answered from the project's policy".into()),
            ..HookDecision::default()
        }
    }
    async fn post_tool_use(
        &self,
        _: &HookCall,
        tool: &str,
        _: &Value,
        output: &str,
        _: bool,
    ) -> HookDecision {
        // PostToolUse sees the answer, as it sees any tool's output.
        HookDecision {
            context: (tool == "question")
                .then(|| format!("after: {}", output.lines().next().unwrap_or(""))),
            ..HookDecision::default()
        }
    }
    async fn user_prompt_submit(&self, _: &HookCall, _: &str) -> HookDecision {
        HookDecision::default()
    }
    async fn permission_request(
        &self,
        _: &HookCall,
        _: &str,
        _: &[String],
        _: Option<&[String]>,
        _: &Value,
    ) -> HookDecision {
        HookDecision::default()
    }
}

fn question() -> ProviderEvent {
    call(
        "ask",
        "question",
        json!({"prompt": "Which test suite should I run?", "choices": ["Unit tests", "Full suite"]}),
    )
}

#[tokio::test]
async fn a_pre_tool_use_hook_answers_a_question_without_asking() {
    let fixture = Fixture::with_hooks(
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            ..RuntimeConfig::default()
        },
        Some(Arc::new(AnswersQuestions(json!([["Unit tests"]])))),
    );
    let items = fixture.run(vec![vec![question()]]).await;
    let result = items
        .iter()
        .find_map(|item| match &item.data {
            ItemData::ToolResult {
                name,
                output,
                is_error: false,
                metadata,
                ..
            } if name == "question" => Some((output.clone(), metadata.clone())),
            _ => None,
        })
        .expect("the question has a result");
    assert!(result.0.starts_with("Unit tests"), "{}", result.0);
    assert!(result.0.contains("answered from the project's policy"));
    assert!(result.0.contains("after: Unit tests"), "{}", result.0);
    assert_eq!(result.1, json!({"answeredBy": "hook"}));
    // Nobody was asked: no question was recorded or left waiting.
    assert!(
        !items
            .iter()
            .any(|item| matches!(item.data, ItemData::Question { .. })),
        "{items:?}"
    );
    assert!(fixture.runtime.pending_questions().is_empty());
}

#[tokio::test]
async fn answers_that_do_not_fit_leave_the_question_to_the_person() {
    let fixture = Fixture::with_hooks(
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            ..RuntimeConfig::default()
        },
        Some(Arc::new(AnswersQuestions(json!([
            ["Unit tests"],
            ["Full suite"]
        ])))),
    );
    *fixture.provider.rounds.lock().unwrap() = vec![vec![question()]].into();
    let thread = fixture.thread().await;
    fixture
        .runtime
        .start_turn(
            thread,
            StartTurnRequest {
                request_id: RequestId::new(),
                text: "Ask which suite".into(),
                attachments: vec![],
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
    let pending = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(pending) = fixture.runtime.pending_questions().pop() {
                return pending;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the person is asked");
    fixture
        .runtime
        .reply_question(
            &pending.question_id,
            haarness_protocol::QuestionReply {
                answer: "Full suite".into(),
            },
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while fixture.runtime.active_turn(thread).await.unwrap().is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the turn ends");
    let items = fixture.runtime.items(thread, None).await.unwrap();
    assert!(
        items.iter().any(|item| matches!(&item.data,
            ItemData::ToolResult { name, output, metadata, .. }
                if name == "question" && output.starts_with("Full suite")
                    && output.contains("after: Full suite") && metadata.is_null())),
        "{items:?}"
    );
}

/// A `PreToolUse` hook that stops every question, by a block or a deny.
struct StopsQuestions {
    deny: bool,
}
#[async_trait]
impl HookPort for StopsQuestions {
    async fn pre_tool_use(&self, _: &HookCall, tool: &str, _: &Value) -> HookDecision {
        if tool != "question" {
            return HookDecision::default();
        }
        if self.deny {
            HookDecision {
                permission: Some(PermissionDecision::Deny),
                ..HookDecision::default()
            }
        } else {
            HookDecision {
                block: Some("questions are answered in the ticket".into()),
                ..HookDecision::default()
            }
        }
    }
    async fn post_tool_use(
        &self,
        _: &HookCall,
        _: &str,
        _: &Value,
        _: &str,
        _: bool,
    ) -> HookDecision {
        HookDecision::default()
    }
    async fn user_prompt_submit(&self, _: &HookCall, _: &str) -> HookDecision {
        HookDecision::default()
    }
    async fn permission_request(
        &self,
        _: &HookCall,
        _: &str,
        _: &[String],
        _: Option<&[String]>,
        _: &Value,
    ) -> HookDecision {
        HookDecision::default()
    }
}

#[tokio::test]
async fn a_pre_tool_use_block_or_deny_stops_a_question() {
    for (deny, reason) in [
        (
            false,
            "blocked by a PreToolUse hook: questions are answered in the ticket",
        ),
        (true, "denied by a PreToolUse hook"),
    ] {
        let fixture = Fixture::with_hooks(
            RuntimeConfig {
                approval_mode: haarness_protocol::ApprovalMode::Review,
                ..RuntimeConfig::default()
            },
            Some(Arc::new(StopsQuestions { deny })),
        );
        let items = fixture.run(vec![vec![question()]]).await;
        assert!(
            items.iter().any(|item| matches!(&item.data,
                ItemData::ToolResult { name, output, is_error: true, .. }
                    if name == "question" && output.contains(reason))),
            "{items:?}"
        );
        // Nobody was asked.
        assert!(
            !items
                .iter()
                .any(|item| matches!(item.data, ItemData::Question { .. })),
            "{items:?}"
        );
        assert!(fixture.runtime.pending_questions().is_empty());
    }
}

#[tokio::test]
async fn a_call_that_failed_the_same_way_twice_is_not_run_again_in_the_turn() {
    let fixture = Fixture::new(RuntimeConfig::default());
    let bad = || json!({"command": "touch made.txt", "bogus": 1});
    let items = fixture
        .run(vec![
            vec![call("r1", "bash", bad())],
            vec![call("r2", "bash", bad())],
            vec![call("r3", "bash", bad())],
            vec![call("r4", "bash", bad())],
            vec![call("ok", "bash", json!({"command": "echo fixed"}))],
        ])
        .await;
    for id in ["r1", "r2"] {
        let refused = error(&items, id);
        assert!(refused.contains("unexpected fields: bogus"), "{refused}");
        assert!(
            refused.contains("bash takes command (string, required), timeout (integer), workdir (string) and run_in_background (boolean)."),
            "{refused}"
        );
    }
    for id in ["r3", "r4"] {
        let refused = error(&items, id);
        assert!(
            refused.contains(
                "Not run: this exact call already failed 2 times in this turn with the same error"
            ) && refused.contains("unexpected fields: bogus")
                && refused.contains("Change the call as that error says"),
            "{refused}"
        );
    }
    assert!(tool_results(&items).contains(&("exit: 0\nfixed\n".to_owned(), false)));
    assert!(!fixture.root.join("made.txt").exists());
}

#[tokio::test]
async fn calls_written_the_way_other_agents_write_them_run_as_this_ones() {
    let fixture = Fixture::new(RuntimeConfig::default());
    let items = fixture
        .run(vec![
            vec![call(
                "named",
                "run_command",
                json!({"command": "echo by name"}),
            )],
            vec![call(
                "read",
                "read_file",
                json!({"file_path": "sample.txt"}),
            )],
            vec![call(
                "words",
                "bash",
                json!({"command": ["echo", "two words"], "description": "Say it"}),
            )],
            vec![call(
                "cmd",
                "bash",
                json!({"cmd": "echo cmd", "is_background": false}),
            )],
            vec![call(
                "nulls",
                "bash",
                json!({"command": "echo nulls", "timeout": null, "workdir": null}),
            )],
            vec![call(
                "unknown",
                "str_replace_editor",
                json!({"command": "view", "path": "sample.txt"}),
            )],
        ])
        .await;
    let results = tool_results(&items);
    for expected in [
        "exit: 0\nby name\n",
        "exit: 0\ntwo words\n",
        "exit: 0\ncmd\n",
        "exit: 0\nnulls\n",
    ] {
        assert!(
            results.contains(&(expected.to_owned(), false)),
            "{expected:?} in {results:?}"
        );
    }
    assert!(
        results
            .iter()
            .any(|(output, error)| !error && output.contains("real file contents")),
        "{results:?}"
    );
    let unknown = error(&items, "unknown");
    assert!(
        unknown.contains("read views a file, write creates one"),
        "{unknown}"
    );
}

#[tokio::test]
async fn a_command_out_of_time_is_told_the_unit_and_the_timeout_to_send() {
    let fixture = Fixture::new(RuntimeConfig::default());
    let items = fixture
        .run(vec![vec![call(
            "slow",
            "bash",
            json!({"command": "sleep 5", "timeout": 1200}),
        )]])
        .await;
    let refused = error(&items, "slow");
    assert!(
        refused.contains("command timed out after 1200 ms"),
        "{refused}"
    );
    assert!(
        refused.contains("The timeout is in milliseconds: 1200 is 1.2 seconds. To give the command 1200 seconds, send timeout 1200000."),
        "{refused}"
    );
    assert!(refused.contains("run_in_background"), "{refused}");
}

#[tokio::test]
async fn a_failed_call_names_the_fields_it_was_run_without() {
    let fixture = Fixture::new(RuntimeConfig::default());
    let items = fixture
        .run(vec![vec![call(
            "missing",
            "read",
            json!({"path": "missing.txt", "line_start": 1}),
        )]])
        .await;
    let refused = error(&items, "missing");
    assert!(refused.contains("does not exist"), "{refused}");
    assert!(
        refused.contains("(Not used: line_start. read takes path, offset and limit.)"),
        "{refused}"
    );
}
