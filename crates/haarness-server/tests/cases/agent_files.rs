//! Agent and skill files as Claude Code writes them: an agent that lists
//! `Bash` follows its own commands, a `tools` pattern limits the agent, a
//! skill the model loads never asks while it loads (its hooks still
//! answer), an agent starts with the skills it lists, a subagent's own
//! `task` runs in the foreground, and a continued subagent keeps its agent
//! and model.
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

/// A skill as the `skill` tool loads it: its text prepared with the turn's
/// own invoker.
struct LoadSkill(&'static str);
#[async_trait]
impl Tool for LoadSkill {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "loadskill".into(),
            description: "Fixture skill".into(),
            input_schema: json!({"type":"object"}),
        }
    }
    fn side_effectful(&self) -> bool {
        false
    }
    async fn execute(&self, _args: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        let invoker = context.invoker.expect("a turn's invoker");
        let text = haarness_core::definitions::prepare(
            self.0,
            "",
            &haarness_protocol::DefinitionExecution::default(),
            &context.thread_id.to_string(),
            invoker.as_ref(),
        )
        .await?;
        Ok(ToolOutput {
            text: format!("SKILL {text}"),
            metadata: Value::Null,
        })
    }
}
struct WithSkill(NativeToolRegistry, &'static str);
impl ToolRegistry for WithSkill {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if name == "loadskill" {
            Some(Arc::new(LoadSkill(self.1)))
        } else {
            self.0.get(name)
        }
    }
    fn list(&self) -> Vec<ToolDefinition> {
        let mut tools = self.0.list();
        tools.push(LoadSkill(self.1).definition());
        tools
    }
}

const SKILL: &str = "A: !`printf ran` B: !`printf asked` C: !`printf denied > denied.txt`\n```!\nprintf one\nprintf two\n```\nEND";

#[tokio::test]
async fn a_skill_the_model_loads_names_what_it_would_ask_about_instead_of_asking() {
    for mode in [
        haarness_protocol::ApprovalMode::Review,
        haarness_protocol::ApprovalMode::Auto,
        haarness_protocol::ApprovalMode::Yolo,
    ] {
        let fixture = Fixture::new("skill-quiet");
        let mut config = RuntimeConfig {
            approval_mode: mode,
            ..RuntimeConfig::default()
        };
        config.base_rules.extend([
            rule("bash:printf asked*", haarness_core::PermissionDecision::Ask),
            rule(
                "bash:printf denied*",
                haarness_core::PermissionDecision::Deny,
            ),
        ]);
        let runtime = runtime_with(
            &fixture,
            Arc::new(Capturing::default()),
            Arc::new(WithSkill(NativeToolRegistry::new(fixture.0.clone()), SKILL)),
            config,
        );
        let chat = echo_thread(&runtime).await;
        let turn = runtime
            .start_turn(chat.id, turn_request("!tool loadskill {}"))
            .await
            .unwrap();
        assert_eq!(
            wait_done(&runtime, &turn).await.status,
            TurnStatus::Completed,
            "{mode:?}"
        );
        let items = runtime.items(chat.id, None).await.unwrap();
        assert!(
            !items
                .iter()
                .any(|item| matches!(item.data, ItemData::PermissionRequest { .. })),
            "{mode:?}: a card was raised"
        );
        let (text, failed) = tool_results(&items, "loadskill").pop().unwrap();
        assert!(!failed, "{text}");
        let asked = if mode == haarness_protocol::ApprovalMode::Yolo {
            "B: asked"
        } else {
            "B: (skipped: `printf asked` asks for approval, which loading a skill never does; call bash with it to run it)"
        };
        assert_eq!(
            text,
            format!(
                "SKILL A: ran {asked} C: (skipped: `printf denied > denied.txt` is not permitted)\none\ntwo\nEND"
            ),
            "{mode:?}"
        );
        assert!(!fixture.0.join("denied.txt").exists());
    }
}

/// Answers each request about a shell command by the command's text: allow
/// for `printf hooked-yes`, deny for `printf hooked-no`, nothing otherwise.
struct ByCommand;
#[async_trait]
impl haarness_core::HookPort for ByCommand {
    async fn pre_tool_use(
        &self,
        _: &haarness_core::HookCall,
        _: &str,
        _: &Value,
    ) -> haarness_core::HookDecision {
        haarness_core::HookDecision::default()
    }
    async fn post_tool_use(
        &self,
        _: &haarness_core::HookCall,
        _: &str,
        _: &Value,
        _: &str,
        _: bool,
    ) -> haarness_core::HookDecision {
        haarness_core::HookDecision::default()
    }
    async fn user_prompt_submit(
        &self,
        _: &haarness_core::HookCall,
        _: &str,
    ) -> haarness_core::HookDecision {
        haarness_core::HookDecision::default()
    }
    async fn permission_request(
        &self,
        _: &haarness_core::HookCall,
        _: &str,
        _: &[String],
        _: Option<&[String]>,
        detail: &Value,
    ) -> haarness_core::HookDecision {
        let permission = match detail.get("command").and_then(Value::as_str) {
            Some("printf hooked-yes") => Some(haarness_core::PermissionDecision::Allow),
            Some("printf hooked-no") => Some(haarness_core::PermissionDecision::Deny),
            _ => None,
        };
        haarness_core::HookDecision {
            permission,
            ..haarness_core::HookDecision::default()
        }
    }
}

#[tokio::test]
async fn a_permission_hook_still_answers_for_a_skill_the_model_loads() {
    let fixture = Fixture::new("skill-hooks");
    let mut config = review();
    config.base_rules.push(rule(
        "bash:printf hooked*",
        haarness_core::PermissionDecision::Ask,
    ));
    let runtime = runtime_with(
        &fixture,
        Arc::new(Capturing::default()),
        Arc::new(WithSkill(
            NativeToolRegistry::new(fixture.0.clone()),
            "Y: !`printf hooked-yes` N: !`printf hooked-no` Q: !`printf hooked-quiet`",
        )),
        config,
    )
    .with_hooks(Arc::new(ByCommand));
    let chat = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(chat.id, turn_request("!tool loadskill {}"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let items = runtime.items(chat.id, None).await.unwrap();
    assert!(
        !items
            .iter()
            .any(|item| matches!(item.data, ItemData::PermissionRequest { .. }))
    );
    let (text, failed) = tool_results(&items, "loadskill").pop().unwrap();
    assert!(!failed, "{text}");
    assert_eq!(
        text,
        "SKILL Y: hooked-yes N: (skipped: `printf hooked-no` is not permitted) Q: (skipped: `printf hooked-quiet` asks for approval, which loading a skill never does; call bash with it to run it)"
    );
}

#[tokio::test]
async fn a_person_s_own_command_still_asks_about_its_snippet() {
    let fixture = Fixture::new("command-asks");
    let mut config = review();
    config.base_rules.push(rule(
        "bash:printf asked*",
        haarness_core::PermissionDecision::Ask,
    ));
    config.commands.push(haarness_protocol::CommandInfo {
        name: "probe".into(),
        description: "Snippet".into(),
        source: None,
        agent: None,
        model: None,
        argument_hint: None,
        template: "B: !`printf asked`".into(),
        plugin: None,
        execution: haarness_protocol::DefinitionExecution::default(),
    });
    let runtime = runtime_with(
        &fixture,
        Arc::new(Capturing::default()),
        Arc::new(NativeToolRegistry::new(fixture.0.clone())),
        config,
    );
    let chat = echo_thread(&runtime).await;
    let mut request = turn_request("");
    request.command = Some("probe".into());
    let turn = runtime.start_turn(chat.id, request).await.unwrap();
    let permission = wait_permission(&runtime, chat.id).await;
    runtime
        .reply_permission(
            &permission,
            haarness_protocol::PermissionReply {
                decision: haarness_protocol::PermissionReplyDecision::Once,
                feedback: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
}

#[tokio::test]
async fn an_agent_s_preloaded_skills_ride_in_its_system_prompt_unchanged_turn_to_turn() {
    let fixture = Fixture::new("agent-preload");
    let provider = Arc::new(Capturing::default());
    let mut config = review();
    config.custom_agents.push(haarness_core::CustomAgent {
        id: "styled".into(),
        prompt: "Follow the house style.".into(),
        mode: Some("all".into()),
        skills: vec!["style".into()],
        preloaded:
            "<skill name=\"style\">\nSTYLE_BODY in ${CLAUDE_SESSION_ID} at ${CLAUDE_PROJECT_DIR}\n</skill>"
                .into(),
        ..Default::default()
    });
    let runtime = runtime_with(
        &fixture,
        provider.clone(),
        Arc::new(NativeToolRegistry::new(fixture.0.clone())),
        config,
    );
    let chat = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Styled".into()),
            agent: Some("styled".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    for text in ["FIRST styled", "SECOND styled"] {
        let turn = runtime
            .start_turn(chat.id, turn_request(text))
            .await
            .unwrap();
        assert_eq!(
            wait_done(&runtime, &turn).await.status,
            TurnStatus::Completed
        );
    }
    let requests = provider.requests.lock().unwrap().clone();
    let first = request_for(&requests, "FIRST styled");
    let second = request_for(&requests, "SECOND styled");
    assert!(
        first.system.contains(&format!(
            "Follow the house style.\n\n<skill name=\"style\">\nSTYLE_BODY in {} at {}\n</skill>",
            chat.id,
            fixture.0.display()
        )),
        "{}",
        first.system
    );
    assert_eq!(first.system, second.system);
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

/// The `task` tool's arguments as `request` offers them.
fn task_arguments(request: &ProviderRequest) -> Vec<String> {
    request
        .tools
        .iter()
        .find(|tool| tool.name == "task")
        .and_then(|tool| tool.input_schema.get("properties"))
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn a_subagent_s_children_stay_in_the_foreground_and_it_is_not_offered_background() {
    let fixture = Fixture::new("nested-foreground");
    let provider = Arc::new(Capturing::default());
    let mut config = review();
    config.execution.subagent_depth = 2;
    let runtime = Arc::new(runtime_with(
        &fixture,
        provider.clone(),
        Arc::new(NativeToolRegistry::new(fixture.0.clone())),
        config,
    ));
    let chat = echo_thread(&runtime).await;
    let slow = json!({"command": "sleep 1.5; printf GRANDCHILD_DONE"});
    let nested = json!({"prompt": format!("!tool bash {slow}"), "description": "nested"});
    let child_prompt = format!("!tool task {nested}");
    let task = json!({"prompt": child_prompt, "description": "outer"});
    let parent_prompt = format!("!tool task {task}");
    let turn = runtime
        .start_turn(chat.id, turn_request(&parent_prompt))
        .await
        .unwrap();
    // While the grandchild runs, moving the child's work to the background
    // moves nothing.
    let child = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let ids = runtime.thread_tree_ids(chat.id).await.unwrap();
            if ids.len() == 3 {
                break only_child(&runtime, chat.id).await;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        runtime
            .promote_subagents(child, None)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let results = tool_results(&runtime.items(child, None).await.unwrap(), "task");
    assert!(
        results.len() == 1 && results[0].0.contains("state=\"completed\""),
        "{results:?}"
    );
    // The chat is offered `background`; its subagent is not.
    let requests = provider.requests.lock().unwrap().clone();
    let parent = request_for(&requests, &parent_prompt);
    assert!(task_arguments(&parent).contains(&"background".to_owned()));
    let child = request_for(&requests, &child_prompt);
    let offered = task_arguments(&child);
    assert!(offered.contains(&"prompt".to_owned()), "{offered:?}");
    assert!(!offered.contains(&"background".to_owned()), "{offered:?}");
    // Nor is it told when to use it; the chat's description is as it was.
    let description = |request: &ProviderRequest| {
        request
            .tools
            .iter()
            .find(|tool| tool.name == "task")
            .map(|tool| tool.description.clone())
            .unwrap_or_default()
    };
    assert!(description(&parent).contains("background=true"));
    assert!(
        !description(&child).contains("background"),
        "{}",
        description(&child)
    );
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

    // A model its provider does not list (a deployment of its own) is kept
    // as well, as it was when the child started.
    let start =
        json!({"prompt": "OWN deployment", "description": "own", "model": "echo/own-deployment"});
    let turn = runtime
        .start_turn(chat.id, turn_request(&format!("!tool task {start}")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let mut own = None;
    for id in runtime.thread_tree_ids(chat.id).await.unwrap() {
        if id != child && runtime.get_thread(id).await.unwrap().parent_id == Some(chat.id) {
            own = Some(id);
        }
    }
    let own = own.expect("the second child");
    let again = json!({"prompt": "OWN again", "task_id": own, "description": "own"});
    let turn = runtime
        .start_turn(chat.id, turn_request(&format!("!tool task {again}")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let requests = provider.0.requests.lock().unwrap().clone();
    assert_eq!(request_for(&requests, "OWN again").model, "own-deployment");
}

#[tokio::test]
async fn a_continued_subagent_whose_provider_is_gone_says_it_moved_to_the_parent_s_model() {
    let fixture = Fixture::new("continued-gone");
    let provider = Arc::new(Capturing::default());
    let runtime = runtime_with(
        &fixture,
        provider.clone(),
        Arc::new(NativeToolRegistry::new(fixture.0.clone())),
        review(),
    );
    let chat = echo_thread(&runtime).await;
    // A child whose provider this server no longer has.
    let child = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Earlier child".into()),
            parent_id: Some(chat.id),
            agent: Some("general".into()),
            provider: Some("retired".into()),
            model: Some("old-model".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let again = json!({"prompt": "GONE again", "task_id": child.id, "description": "again"});
    let turn = runtime
        .start_turn(chat.id, turn_request(&format!("!tool task {again}")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let requests = provider.requests.lock().unwrap().clone();
    assert_eq!(request_for(&requests, "GONE again").model, "echo-1");
    let results = tool_results(&runtime.items(chat.id, None).await.unwrap(), "task");
    let (result, _) = results.last().unwrap();
    assert!(
        result.contains(
            "Configured model \"retired/old-model\" unavailable. Using parent model \"echo/echo-1\"."
        ),
        "{result}"
    );
}
