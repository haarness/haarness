//! A command the agent runs says it is an agent's and names the chat it runs
//! for; a person's own `!` command does not. These run in this test process,
//! whose environment is the server's: an `AI_AGENT` set there (the test run's
//! own agent) is an outer marker and is kept. A stale `HAARNESS_SESSION_ID`
//! there is covered by the shell module's tests, in a process of their own.
use super::*;
use haarness_core::{HookCall, HookDecision, HookPort, PermissionDecision, PermissionRule};
use haarness_protocol::{ItemData, ThreadId};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const SHOW: &str = r#"printf '%s|%s|%s|%s' "${AGENT-unset}" "${HAARNESS-unset}" "${AI_AGENT-unset}" "${HAARNESS_SESSION_ID-unset}""#;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("haarness-agent-shell-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What a command the agent runs for `thread` shows with [`SHOW`].
fn marked(thread: ThreadId) -> String {
    let outer = std::env::var("AI_AGENT")
        .ok()
        .filter(|value| !value.is_empty());
    format!("1|1|{}|{thread}", outer.as_deref().unwrap_or("haarness"))
}

/// What a person's own command shows with [`SHOW`]: this process's own
/// values, and no chat id.
fn unmarked() -> String {
    let inherited = |name: &str| std::env::var(name).unwrap_or_else(|_| "unset".into());
    format!(
        "{}|{}|{}|unset",
        inherited("AGENT"),
        inherited("HAARNESS"),
        inherited("AI_AGENT")
    )
}

/// A `shell.env` hook that tries to mark commands as not an agent's and to
/// name another chat.
struct Unmarking;
#[async_trait]
impl HookPort for Unmarking {
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
    async fn stop(&self, _: &HookCall) {}
    async fn shell_env(&self, _: ThreadId, _: &Path) -> BTreeMap<String, String> {
        [
            ("AGENT", "0"),
            ("HAARNESS", "0"),
            ("AI_AGENT", "hook-agent"),
            ("HAARNESS_SESSION_ID", "hook"),
            ("HOOK_ENV", "ok"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
    }
}

fn runtime(
    workspace: &Path,
    hooks: Option<Arc<dyn HookPort>>,
) -> (AgentRuntime, Arc<std::sync::Mutex<Vec<ProviderRequest>>>) {
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    config.base_rules.push(PermissionRule {
        pattern: "bash:*".into(),
        decision: PermissionDecision::Allow,
    });
    config.commands.push(haarness_protocol::CommandInfo {
        name: "marks".into(),
        description: "Shows the agent markers".into(),
        source: None,
        agent: None,
        model: None,
        argument_hint: None,
        template: format!("Marks: !`{SHOW}`"),
        plugin: None,
        execution: haarness_protocol::DefinitionExecution::default(),
    });
    let provider = Capturing::default();
    let requests = provider.requests.clone();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider) as Arc<dyn ModelProvider>
        ])),
        Arc::new(NativeToolRegistry::new(workspace.to_owned())),
        EventBus::new(2048),
        workspace.to_owned(),
        String::new(),
        config,
    );
    let runtime = match hooks {
        Some(hooks) => runtime.with_hooks(hooks),
        None => runtime,
    };
    (runtime, requests)
}

/// The outputs of the bash calls recorded in `thread`, oldest first.
async fn shell_outputs(runtime: &AgentRuntime, thread: ThreadId) -> Vec<String> {
    runtime
        .items(thread, None)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item.data {
            ItemData::ToolResult { name, output, .. } if name == "bash" => Some(output),
            _ => None,
        })
        .collect()
}

/// A turn of the model that runs `command` in `thread`.
async fn model_runs(runtime: &AgentRuntime, thread: ThreadId, command: &str) {
    let prompt = format!("!tool bash {}", json!({ "command": command }));
    let turn = runtime
        .start_turn(thread, turn_request(&prompt))
        .await
        .unwrap();
    assert_eq!(
        wait_done(runtime, &turn).await.status,
        TurnStatus::Completed
    );
}

/// The chats a turn of `parent` started.
async fn children(runtime: &AgentRuntime, parent: ThreadId) -> Vec<ThreadId> {
    runtime
        .list_threads(false)
        .await
        .unwrap()
        .into_iter()
        .filter(|thread| thread.parent_id == Some(parent))
        .map(|thread| thread.id)
        .collect()
}

#[tokio::test]
async fn agent_commands_name_their_chat_and_a_person_s_own_command_is_not_marked() {
    let fixture = Fixture::new();
    let (runtime, requests) = runtime(&fixture.0, None);
    let thread = echo_thread(&runtime).await.id;

    // A command the model runs in the foreground.
    model_runs(&runtime, thread, SHOW).await;
    assert_eq!(
        shell_outputs(&runtime, thread).await,
        [format!("exit: 0\n{}", marked(thread))]
    );

    // A slash command's !`…` block, which the agent's turn runs.
    let mut request = turn_request("");
    request.command = Some("marks".into());
    let turn = runtime.start_turn(thread, request).await.unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let expected = format!("Marks: {}", marked(thread));
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.messages.iter().any(
                |message| matches!(message, ProviderMessage::Text { role, content }
                if role == "user" && content.contains(&expected))
            )),
        "no request carried {expected}"
    );

    // A step of the model's batch call.
    let prompt = format!(
        "!tool batch {}",
        json!({ "tool_calls": [{ "tool": "bash", "parameters": { "command": SHOW } }] })
    );
    let turn = runtime
        .start_turn(thread, turn_request(&prompt))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let batch = runtime
        .items(thread, None)
        .await
        .unwrap()
        .into_iter()
        .find_map(|item| match item.data {
            ItemData::ToolResult { name, output, .. } if name == "batch" => Some(output),
            _ => None,
        })
        .expect("the batch result");
    assert!(batch.contains(&marked(thread)), "{batch}");

    // A workflow's `ctx.shell`.
    let result = runtime
        .run_workflow_tool(
            thread,
            "bash",
            json!({ "command": SHOW }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.output.metadata["stdout"], marked(thread));

    // A person's own `!` command: no markers, and no chat id.
    let item = runtime.run_shell(thread, SHOW.into()).await.unwrap();
    assert!(
        matches!(&item.data, ItemData::ToolResult { output, is_error: false, .. }
            if *output == format!("exit: 0\n{}", unmarked())),
        "{:?}",
        item.data
    );

    // A child agent's command names the child's own chat, in the foreground
    // and in the background.
    let turn = runtime
        .start_turn(thread, turn_request(&tasks_prompt(1, SHOW)))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let foreground = children(&runtime, thread).await;
    assert_eq!(foreground.len(), 1);
    assert_eq!(
        shell_outputs(&runtime, foreground[0]).await,
        [format!("exit: 0\n{}", marked(foreground[0]))]
    );
    let background = format!(
        "!tool task {}",
        json!({
            "prompt": format!("!tool bash {}", json!({ "command": SHOW })),
            "agent": "general",
            "background": true,
        })
    );
    let turn = runtime
        .start_turn(thread, turn_request(&background))
        .await
        .unwrap();
    wait_done(&runtime, &turn).await;
    let (child, outputs) = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            for child in children(&runtime, thread).await {
                let outputs = shell_outputs(&runtime, child).await;
                if child != foreground[0] && !outputs.is_empty() {
                    return (child, outputs);
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the background child ran no command");
    assert_eq!(outputs, [format!("exit: 0\n{}", marked(child))]);
}

#[tokio::test]
async fn a_shell_env_hook_cannot_unmark_an_agent_command_or_name_another_chat() {
    let fixture = Fixture::new();
    let (runtime, _) = runtime(&fixture.0, Some(Arc::new(Unmarking)));
    let thread = echo_thread(&runtime).await.id;
    let show = format!(r#"{SHOW}; printf '|%s' "$HOOK_ENV""#);
    // The hook's AI_AGENT is kept as an outer marker; its other markers and
    // its chat id are not.
    let agent = format!("1|1|hook-agent|{thread}|ok");
    model_runs(&runtime, thread, &show).await;
    assert_eq!(
        shell_outputs(&runtime, thread).await,
        [format!("exit: 0\n{agent}")]
    );
    let result = runtime
        .run_workflow_tool(
            thread,
            "bash",
            json!({ "command": show }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.output.metadata["stdout"], agent);
    // A person's own command gets the hook's variables as they are.
    let item = runtime.run_shell(thread, show).await.unwrap();
    assert!(
        matches!(&item.data, ItemData::ToolResult { output, .. }
            if output == "exit: 0\n0|0|hook-agent|hook|ok"),
        "{:?}",
        item.data
    );
}
