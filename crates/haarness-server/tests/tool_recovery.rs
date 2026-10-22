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
    let fixture = Fixture::new(RuntimeConfig::default());
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
    let fixture = Fixture::new(RuntimeConfig::default());
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
        _: Option<&[String]>,
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
        _: Option<&[String]>,
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

/// A `PermissionRequest` hook that allows, but only a rewritten command.
struct RewritingAllow;
#[async_trait]
impl HookPort for RewritingAllow {
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
        _: Option<&[String]>,
        _: &Value,
    ) -> HookDecision {
        HookDecision {
            permission: Some(PermissionDecision::Allow),
            updated_input: Some(json!({"command": "true"})),
            ..HookDecision::default()
        }
    }
    async fn stop(&self, _: ThreadId) {}
}

#[tokio::test]
async fn a_permission_request_hook_allowing_other_input_leaves_the_prompt_to_the_person() {
    let mut config = RuntimeConfig::default();
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
struct EchoingAllow(std::sync::Mutex<Vec<Value>>);
#[async_trait]
impl HookPort for EchoingAllow {
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
    async fn stop(&self, _: ThreadId) {}
}

#[cfg(unix)]
#[tokio::test]
async fn a_permission_request_hook_handing_back_the_same_input_still_allows() {
    let mut config = RuntimeConfig::default();
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
    async fn stop(&self, _: ThreadId) {}
}

/// Runs `echo one`, then `echo two` in a later round, under `bash:*` ask.
async fn echo_twice(hook: Arc<FirstAllow>) -> (Fixture, Vec<haarness_protocol::Item>) {
    let mut config = RuntimeConfig::default();
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

#[tokio::test]
async fn a_permission_request_hook_cannot_answer_always_for_an_edit_of_haarness_config() {
    let hook = FirstAllow::new(true);
    let fixture = Fixture::with_hooks(RuntimeConfig::default(), Some(hook.clone()));
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
    async fn pre_tool_use(&self, _: ThreadId, tool: &str, arguments: &Value) -> HookDecision {
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
        _: ThreadId,
        tool: &str,
        _: &Value,
        output: &str,
        _: bool,
    ) -> Option<String> {
        // PostToolUse sees the answer, as it sees any tool's output.
        (tool == "question").then(|| format!("after: {}", output.lines().next().unwrap_or("")))
    }
    async fn user_prompt_submit(&self, _: ThreadId, _: &str) -> HookDecision {
        HookDecision::default()
    }
    async fn permission_request(
        &self,
        _: ThreadId,
        _: &str,
        _: &[String],
        _: Option<&[String]>,
        _: &Value,
    ) -> HookDecision {
        HookDecision::default()
    }
    async fn stop(&self, _: ThreadId) {}
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
        RuntimeConfig::default(),
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
        RuntimeConfig::default(),
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
    async fn pre_tool_use(&self, _: ThreadId, tool: &str, _: &Value) -> HookDecision {
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
        _: Option<&[String]>,
        _: &Value,
    ) -> HookDecision {
        HookDecision::default()
    }
    async fn stop(&self, _: ThreadId) {}
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
            RuntimeConfig::default(),
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
