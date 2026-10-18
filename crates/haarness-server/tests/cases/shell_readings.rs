//! A shell command meets the rules for what it really runs: other spellings
//! of it, the commands its wrappers run and the text a shell runs again.
use super::*;
use haarness_core::{PermissionDecision as D, PermissionRule};
use haarness_protocol::{ItemData, PermissionReply, PermissionReplyDecision as Reply};
use serde_json::json;
use std::path::{Path, PathBuf};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("haarness-shell-readings-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn runtime(workspace: &Path, rules: &[(&str, D)]) -> AgentRuntime {
    let mut config = RuntimeConfig::default();
    for (pattern, decision) in rules {
        config.base_rules.push(PermissionRule {
            pattern: (*pattern).into(),
            decision: *decision,
        });
    }
    AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(workspace.to_owned())),
        EventBus::new(2048),
        workspace.to_owned(),
        String::new(),
        config,
    )
}

fn bash(command: &str) -> String {
    format!("!tool bash {}", json!({ "command": command }))
}

async fn tool_errors(runtime: &AgentRuntime, thread: &Thread) -> Vec<String> {
    runtime
        .items(thread.id, None)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item.data {
            ItemData::ToolResult {
                is_error: true,
                output,
                ..
            } => Some(output),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_deny_for_rm_refuses_rm_inside_bash_c_eval_and_wrappers() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.0.join("victim")).unwrap();
    let runtime = runtime(&fixture.0, &[("bash:rm *", D::Deny)]);
    let thread = echo_thread(&runtime).await;
    for command in [
        "bash -c 'rm -rf victim'",
        "eval \"rm -rf victim\"",
        "sudo -n rm -rf victim",
        "FOO=1 \\rm -rf victim",
    ] {
        let turn = runtime
            .start_turn(thread.id, turn_request(&bash(command)))
            .await
            .unwrap();
        assert_eq!(
            wait_done(&runtime, &turn).await.status,
            TurnStatus::Completed
        );
        assert!(fixture.0.join("victim").is_dir(), "{command} ran");
    }
    let errors = tool_errors(&runtime, &thread).await;
    assert_eq!(errors.len(), 4, "{errors:?}");
    for error in errors {
        assert!(
            error.contains("'bash:rm -rf victim' is denied by permission rule 'bash:rm *'"),
            "{error}"
        );
    }
}

#[tokio::test]
async fn an_ask_for_a_command_asks_through_an_assignment_and_shows_the_whole_command() {
    let fixture = Fixture::new();
    let runtime = runtime(&fixture.0, &[("bash:printf *", D::Ask)]);
    let thread = echo_thread(&runtime).await;
    let command = "LC_ALL=C printf asked > asked.txt";
    let turn = runtime
        .start_turn(thread.id, turn_request(&bash(command)))
        .await
        .unwrap();
    wait_pending_permissions(&runtime, 1).await;
    let pending = runtime.pending_permissions().remove(0);
    // The card shows the command as written; the request names what the
    // rule asks about.
    assert_eq!(pending.detail["command"], command);
    assert_eq!(pending.patterns, vec!["printf asked"]);
    assert_eq!(pending.always_patterns, vec!["bash:printf *"]);
    assert!(!fixture.0.join("asked.txt").exists());
    runtime
        .reply_permission(
            &pending.permission_id,
            PermissionReply {
                decision: Reply::Once,
                feedback: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("asked.txt")).unwrap(),
        "asked"
    );
}

#[tokio::test]
async fn an_allowed_wrapper_under_bash_ask_runs_without_a_new_question() {
    let fixture = Fixture::new();
    let runtime = runtime(&fixture.0, &[("bash:*", D::Ask), ("bash:nice *", D::Allow)]);
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash("nice sh -c 'printf ran > ran.txt'")),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(runtime.pending_permissions().is_empty());
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("ran.txt")).unwrap(),
        "ran"
    );
}

async fn reply(runtime: &AgentRuntime, id: &str, decision: Reply) {
    runtime
        .reply_permission(
            id,
            PermissionReply {
                decision,
                feedback: None,
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_command_too_long_to_read_asks_when_a_rule_could_be_about_what_it_runs() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.0.join("victim")).unwrap();
    let runtime = runtime(&fixture.0, &[("bash:rm *", D::Deny)]);
    let thread = echo_thread(&runtime).await;
    let command = format!("bash -c '{}rm -rf victim'", "true; ".repeat(300 * 1024 / 6));
    let turn = runtime
        .start_turn(thread.id, turn_request(&bash(&command)))
        .await
        .unwrap();
    wait_pending_permissions(&runtime, 1).await;
    let pending = runtime.pending_permissions().remove(0);
    assert_eq!(pending.patterns, vec![command]);
    reply(&runtime, &pending.permission_id, Reply::Reject).await;
    wait_done(&runtime, &turn).await;
    assert!(fixture.0.join("victim").is_dir());
}
