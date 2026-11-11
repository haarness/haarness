//! Agent files as Claude Code writes them: an agent that lists `Bash`
//! follows its own commands, a `tools` pattern limits the agent, a
//! subagent's own `task` runs in the foreground, and a continued subagent
//! keeps its agent and model.
use super::*;
use haarness_protocol::ItemData;
use serde_json::json;
use std::collections::HashMap;

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("haarness-{name}-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path).unwrap();
        // Servers run in their canonical folder (the temporary one is a
        // symlink on macOS).
        Self(dunce::canonicalize(&path).unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn runtime_with(
    fixture: &Fixture,
    provider: Arc<dyn ModelProvider>,
    tools: Arc<dyn ToolRegistry>,
    config: RuntimeConfig,
) -> AgentRuntime {
    AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![provider])),
        tools,
        EventBus::new(512),
        fixture.0.clone(),
        String::new(),
        config,
    )
}

fn review() -> RuntimeConfig {
    RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    }
}

fn rule(
    pattern: &str,
    decision: haarness_core::PermissionDecision,
) -> haarness_core::PermissionRule {
    haarness_core::PermissionRule {
        pattern: pattern.into(),
        decision,
    }
}

/// The first request whose newest user message starts with `prompt`.
fn request_for(requests: &[ProviderRequest], prompt: &str) -> ProviderRequest {
    requests
        .iter()
        .find(|request| {
            request.messages.iter().any(|message| {
                matches!(message, ProviderMessage::Text { role, content }
                    if role == "user" && content.starts_with(prompt))
            })
        })
        .cloned()
        .unwrap_or_else(|| panic!("no request for {prompt}"))
}

fn tool_results(items: &[haarness_protocol::Item], tool: &str) -> Vec<(String, bool)> {
    items
        .iter()
        .filter_map(|item| match &item.data {
            ItemData::ToolResult {
                name,
                output,
                is_error,
                ..
            } if name == tool => Some((output.clone(), *is_error)),
            _ => None,
        })
        .collect()
}

async fn only_child(
    runtime: &AgentRuntime,
    parent: haarness_protocol::ThreadId,
) -> haarness_protocol::ThreadId {
    let mut children = Vec::new();
    for id in runtime.thread_tree_ids(parent).await.unwrap() {
        if runtime.get_thread(id).await.unwrap().parent_id == Some(parent) {
            children.push(id);
        }
    }
    assert_eq!(children.len(), 1, "{children:?}");
    children[0]
}

#[tokio::test]
async fn an_agent_that_lists_bash_can_follow_and_stop_its_commands() {
    let fixture = Fixture::new("agent-bash-follow");
    let provider = Arc::new(Capturing::default());
    let mut config = review();
    config.custom_agents.push(haarness_core::CustomAgent {
        id: "runner".into(),
        prompt: "Run things.".into(),
        mode: Some("subagent".into()),
        // `tools: Bash, Read` in a Claude Code agent file.
        tools: HashMap::from([
            ("*".into(), false),
            ("bash".into(), true),
            ("read".into(), true),
        ]),
        ..Default::default()
    });
    let runtime = runtime_with(
        &fixture,
        provider.clone(),
        Arc::new(NativeToolRegistry::new(fixture.0.clone())),
        config,
    );
    let chat = echo_thread(&runtime).await;
    let task = json!({"prompt": "RUNNER_CHILD go", "agent": "runner", "description": "run"});
    let turn = runtime
        .start_turn(chat.id, turn_request(&format!("!tool task {task}")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let requests = provider.requests.lock().unwrap().clone();
    let names: Vec<String> = request_for(&requests, "RUNNER_CHILD")
        .tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    for listed in ["bash", "bash_output", "bash_stop", "read"] {
        assert!(names.contains(&listed.to_owned()), "{listed}: {names:?}");
    }
    assert!(!names.contains(&"grep".to_owned()), "{names:?}");
}

#[tokio::test]
async fn a_tools_pattern_limits_the_agent_even_when_every_ask_is_approved() {
    let fixture = Fixture::new("agent-tool-pattern");
    let mut config = review();
    config.custom_agents.push(haarness_core::CustomAgent {
        id: "echoer".into(),
        prompt: "Echo things.".into(),
        mode: Some("subagent".into()),
        // `tools: Bash(printf:*)` in a Claude Code agent file.
        tools: HashMap::from([("*".into(), false), ("bash".into(), true)]),
        ceiling: vec![
            rule("bash:*", haarness_core::PermissionDecision::Deny),
            rule("bash:printf *", haarness_core::PermissionDecision::Allow),
        ],
        ..Default::default()
    });
    let runtime = runtime_with(
        &fixture,
        Arc::new(Capturing::default()),
        Arc::new(NativeToolRegistry::new(fixture.0.clone())),
        config,
    );
    let chat = echo_thread(&runtime).await;
    for command in [
        "printf LIMITED_OK",
        "printf x && touch breach.txt",
        "touch breach.txt",
    ] {
        let bash = json!({ "command": command });
        let task = json!({"prompt": format!("!tool bash {bash}"), "agent": "echoer", "description": "echo"});
        let mut request = turn_request(&format!("!tool task {task}"));
        request.auto_approve = true;
        let turn = runtime.start_turn(chat.id, request).await.unwrap();
        assert_eq!(
            wait_done(&runtime, &turn).await.status,
            TurnStatus::Completed
        );
    }
    assert!(!fixture.0.join("breach.txt").exists());
    let mut results = Vec::new();
    for child in runtime.thread_tree_ids(chat.id).await.unwrap() {
        results.extend(tool_results(
            &runtime.items(child, None).await.unwrap(),
            "bash",
        ));
    }
    assert!(
        results
            .iter()
            .any(|(output, failed)| !failed && output.contains("LIMITED_OK")),
        "{results:?}"
    );
    assert_eq!(
        results
            .iter()
            .filter(|(output, failed)| *failed && output.contains("bash:*"))
            .count(),
        2,
        "{results:?}"
    );
    assert!(runtime.pending_permissions().is_empty());
}

#[tokio::test]
async fn a_subagent_s_own_task_runs_in_the_foreground() {
    let fixture = Fixture::new("nested-task");
    let mut config = review();
    config.execution.subagent_depth = 2;
    let runtime = runtime_with(
        &fixture,
        Arc::new(Capturing::default()),
        Arc::new(NativeToolRegistry::new(fixture.0.clone())),
        config,
    );
    let chat = echo_thread(&runtime).await;
    let nested =
        json!({"prompt": "GRANDCHILD answer", "description": "nested", "background": true});
    let task = json!({"prompt": format!("!tool task {nested}"), "description": "outer"});
    let turn = runtime
        .start_turn(chat.id, turn_request(&format!("!tool task {task}")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let child = only_child(&runtime, chat.id).await;
    let results = tool_results(&runtime.items(child, None).await.unwrap(), "task");
    assert_eq!(results.len(), 1, "{results:?}");
    assert!(results[0].0.contains("state=\"completed\""), "{results:?}");
    assert!(results[0].0.contains("GRANDCHILD answer"), "{results:?}");
    assert!(runtime.background_tasks(child).await.unwrap().is_empty());
}

/// Echo, with a second model to tell runs apart.
struct TwoModels(Capturing);
#[async_trait]
impl ModelProvider for TwoModels {
    fn id(&self) -> &str {
        "echo"
    }
    fn info(&self) -> ProviderInfo {
        let mut info = self.0.info();
        let mut second = info.models[0].clone();
        second.id = "echo-2".into();
        info.models.push(second);
        info
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        self.0.stream(request, cancel).await
    }
}

#[tokio::test]
async fn a_continued_subagent_keeps_its_own_agent_and_model() {
    let fixture = Fixture::new("continued-child");
    let provider = Arc::new(TwoModels(Capturing::default()));
    let runtime = runtime_with(
        &fixture,
        provider.clone(),
        Arc::new(NativeToolRegistry::new(fixture.0.clone())),
        review(),
    );
    let chat = echo_thread(&runtime).await;
    let start = json!({"prompt": "FIRST pass", "agent": "explore", "description": "look", "model": "echo/echo-2"});
    let turn = runtime
        .start_turn(chat.id, turn_request(&format!("!tool task {start}")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let child = only_child(&runtime, chat.id).await;
    let again = json!({"prompt": "SECOND pass", "task_id": child, "description": "look again"});
    let turn = runtime
        .start_turn(chat.id, turn_request(&format!("!tool task {again}")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let requests = provider.0.requests.lock().unwrap().clone();
    assert_eq!(request_for(&requests, "FIRST pass").model, "echo-2");
    assert_eq!(request_for(&requests, "SECOND pass").model, "echo-2");
    let last = runtime.get_thread(child).await.unwrap();
    assert_eq!(last.agent, "explore");

    // Another agent with the same task_id is refused, and the child does not run.
    let turns_before = requests.len();
    let other =
        json!({"prompt": "THIRD pass", "task_id": child, "agent": "general", "description": "x"});
    let turn = runtime
        .start_turn(chat.id, turn_request(&format!("!tool task {other}")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let results = tool_results(&runtime.items(chat.id, None).await.unwrap(), "task");
    let (refusal, failed) = results.last().unwrap();
    assert!(*failed, "{refusal}");
    assert!(
        refusal.contains(&format!(
            "task_id {child} is a \"explore\" subagent and continues as one"
        )),
        "{refusal}"
    );
    let requests = provider.0.requests.lock().unwrap().clone();
    assert!(
        !requests[turns_before..]
            .iter()
            .any(|request| request.messages.iter().any(|message| matches!(message,
                ProviderMessage::Text { role, content } if role == "user" && content.starts_with("THIRD pass")))),
        "the refused continuation ran"
    );
    // A call that names a model moves the child to it.
    let moved = json!({"prompt": "FOURTH pass", "task_id": child, "model": "echo/echo-1", "description": "x"});
    let turn = runtime
        .start_turn(chat.id, turn_request(&format!("!tool task {moved}")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let requests = provider.0.requests.lock().unwrap().clone();
    assert_eq!(request_for(&requests, "FOURTH pass").model, "echo-1");
}
