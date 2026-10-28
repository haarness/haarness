//! A command's `!` snippet in a turn a person can stop runs until it ends,
//! as the command waits for its output; a scheduled run and the model's own
//! shell calls keep the shell deadline. A snippet whose output was cut says
//! so in the prompt.
use super::*;
use haarness_core::{PermissionDecision, PermissionRule};
use haarness_protocol::{ItemData, ThreadId};
use serde_json::json;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("haarness-snippets-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn command(name: &str, template: &str) -> haarness_protocol::CommandInfo {
    haarness_protocol::CommandInfo {
        name: name.into(),
        description: format!("Fixture {name}"),
        source: None,
        agent: None,
        model: None,
        argument_hint: None,
        template: template.into(),
        plugin: None,
        execution: haarness_protocol::DefinitionExecution::default(),
    }
}

/// Shell calls are allowed and end after 300 ms unless a call says otherwise.
fn runtime(fixture: &Fixture) -> (AgentRuntime, Arc<std::sync::Mutex<Vec<ProviderRequest>>>) {
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    config.execution.shell_timeout_ms = 300;
    config.base_rules.push(PermissionRule {
        pattern: "bash:*".into(),
        decision: PermissionDecision::Allow,
    });
    config.commands.extend([
        command("slow", "Slow: !`sleep 1; printf finished` end"),
        command("forever", "Never: !`sleep 30; printf never`"),
        command("numbers", "Numbers: !`seq 3000` end"),
    ]);
    let provider = Capturing::default();
    let requests = provider.requests.clone();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider) as Arc<dyn ModelProvider>
        ])),
        Arc::new(NativeToolRegistry::new(fixture.0.clone())),
        EventBus::new(2048),
        fixture.0.clone(),
        String::new(),
        config,
    );
    (runtime, requests)
}

fn run_command(name: &str) -> StartTurnRequest {
    let mut request = turn_request("");
    request.command = Some(name.into());
    request
}

/// The prompt the model received that starts with `prefix`.
fn sent(requests: &std::sync::Mutex<Vec<ProviderRequest>>, prefix: &str) -> Option<String> {
    requests.lock().unwrap().iter().find_map(|request| {
        request.messages.iter().find_map(|message| match message {
            ProviderMessage::Text { role, content }
                if role == "user" && content.starts_with(prefix) =>
            {
                Some(content.clone())
            }
            _ => None,
        })
    })
}

#[tokio::test]
async fn a_person_s_command_waits_for_a_snippet_past_the_shell_deadline() {
    let fixture = Fixture::new();
    let (runtime, requests) = runtime(&fixture);
    let thread = echo_thread(&runtime).await.id;

    let turn = runtime
        .start_turn(thread, run_command("slow"))
        .await
        .unwrap();
    let done = wait_done(&runtime, &turn).await;
    assert_eq!(done.status, TurnStatus::Completed, "{:?}", done.error);
    assert_eq!(
        sent(&requests, "Slow:").as_deref(),
        Some("Slow: finished end")
    );

    // The model's own shell call keeps the deadline.
    let turn = runtime
        .start_turn(
            thread,
            turn_request(&format!("!tool bash {}", json!({"command": "sleep 1"}))),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let timed_out = runtime
        .items(thread, None)
        .await
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|item| match item.data {
            ItemData::ToolResult {
                name,
                output,
                is_error,
                ..
            } if name == "bash" => Some((output, is_error)),
            _ => None,
        })
        .expect("the model's shell call");
    assert!(
        timed_out.1 && timed_out.0.contains("timed out after 300 ms"),
        "{timed_out:?}"
    );
}

#[tokio::test]
async fn stop_ends_a_snippet_that_does_not_end() {
    let fixture = Fixture::new();
    let (runtime, requests) = runtime(&fixture);
    let thread = echo_thread(&runtime).await.id;

    let turn = runtime
        .start_turn(thread, run_command("forever"))
        .await
        .unwrap();
    // Well past the deadline a model's call would have.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        runtime.turn(turn.id).await.unwrap().status,
        TurnStatus::Running
    );
    let stopped = std::time::Instant::now();
    runtime.interrupt_thread_tree(thread).await.unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Interrupted
    );
    assert!(stopped.elapsed() < Duration::from_secs(5));
    assert!(sent(&requests, "Never:").is_none(), "nothing is sent");
}

#[tokio::test]
async fn a_scheduled_run_keeps_the_shell_deadline_for_a_snippet() {
    let fixture = Fixture::new();
    let (runtime, requests) = runtime(&fixture);
    let thread: ThreadId = runtime
        .create_thread(CreateThreadRequest {
            schedule: Some(haarness_protocol::ScheduleOrigin {
                id: uuid::Uuid::now_v7(),
                name: "Nightly".into(),
            }),
            title: Some("Fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;
    let mut request = run_command("slow");
    request.auto_approve = true;
    let turn = runtime.start_turn(thread, request).await.unwrap();
    let done = wait_done(&runtime, &turn).await;
    assert_eq!(done.status, TurnStatus::Failed);
    assert!(
        done.error
            .as_deref()
            .is_some_and(|error| error.contains("timed out after 300 ms")),
        "{:?}",
        done.error
    );
    assert!(sent(&requests, "Slow:").is_none());
}

#[tokio::test]
async fn a_snippet_whose_output_was_cut_says_so_in_the_prompt() {
    let fixture = Fixture::new();
    let (runtime, requests) = runtime(&fixture);
    let thread = echo_thread(&runtime).await.id;

    let turn = runtime
        .start_turn(thread, run_command("numbers"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let prompt = sent(&requests, "Numbers:").expect("the command's prompt");
    assert!(
        prompt.starts_with("Numbers: 1001\n1002\n"),
        "{}",
        &prompt[..40]
    );
    assert!(
        prompt.contains("3000\n(only the last 2000 lines or 50 KB of this output are shown; everything it printed is saved in "),
        "{}",
        &prompt[prompt.len() - 300..]
    );
    assert!(prompt.ends_with(") end"));
}
