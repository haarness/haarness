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
    configured(workspace, rules, RuntimeConfig::default())
}

fn configured(workspace: &Path, rules: &[(&str, D)], mut config: RuntimeConfig) -> AgentRuntime {
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

async fn run(runtime: &AgentRuntime, thread: &Thread, command: &str) {
    let turn = runtime
        .start_turn(thread.id, turn_request(&bash(command)))
        .await
        .unwrap();
    assert_eq!(
        wait_done(runtime, &turn).await.status,
        TurnStatus::Completed
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
async fn under_bash_ask_an_allowed_wrapper_still_meets_a_deny_or_ask_for_what_it_runs() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.0.join("victim")).unwrap();
    let runtime = runtime(
        &fixture.0,
        &[
            ("bash:*", D::Ask),
            ("bash:nice *", D::Allow),
            ("bash:rm *", D::Deny),
            ("bash:printf *", D::Ask),
        ],
    );
    let thread = echo_thread(&runtime).await;
    run(&runtime, &thread, "nice rm -rf victim").await;
    assert!(fixture.0.join("victim").is_dir());
    let errors = tool_errors(&runtime, &thread).await;
    assert!(
        errors[0].contains("'bash:rm -rf victim' is denied by permission rule 'bash:rm *'"),
        "{errors:?}"
    );
    // The ask names the command as written and asks about the reading; an
    // Always remembers the reading's rule only, so the next printf through
    // nice runs without a question.
    let command = "nice printf one > one.txt";
    let turn = runtime
        .start_turn(thread.id, turn_request(&bash(command)))
        .await
        .unwrap();
    wait_pending_permissions(&runtime, 1).await;
    let pending = runtime.pending_permissions().remove(0);
    assert_eq!(pending.detail["command"], command);
    assert_eq!(pending.operation, format!("bash:{command}"));
    assert_eq!(pending.patterns, vec!["printf one"]);
    assert_eq!(pending.always_patterns, vec!["bash:printf *"]);
    reply(&runtime, &pending.permission_id, Reply::Always).await;
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    run(&runtime, &thread, "nice printf two > two.txt").await;
    assert!(runtime.pending_permissions().is_empty());
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("two.txt")).unwrap(),
        "two"
    );
}

#[tokio::test]
async fn an_always_answer_about_a_reading_resolves_the_same_question_in_another_chat() {
    let fixture = Fixture::new();
    let runtime = runtime(&fixture.0, &[("bash:printf *", D::Ask)]);
    let first = echo_thread(&runtime).await;
    let second = echo_thread(&runtime).await;
    let one = runtime
        .start_turn(first.id, turn_request(&bash("nice printf a > a.txt")))
        .await
        .unwrap();
    let two = runtime
        .start_turn(second.id, turn_request(&bash("nohup printf b > b.txt")))
        .await
        .unwrap();
    wait_pending_permissions(&runtime, 2).await;
    let pending = runtime
        .pending_permissions()
        .into_iter()
        .find(|pending| pending.thread_id == first.id)
        .unwrap();
    reply(&runtime, &pending.permission_id, Reply::Always).await;
    for turn in [&one, &two] {
        assert_eq!(
            wait_done(&runtime, turn).await.status,
            TurnStatus::Completed
        );
    }
    assert!(runtime.pending_permissions().is_empty());
    assert!(fixture.0.join("a.txt").exists() && fixture.0.join("b.txt").exists());
}

#[tokio::test]
async fn a_deny_for_a_reading_holds_in_auto_under_yolo_for_direct_calls_and_in_child_agents() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.0.join("victim")).unwrap();
    let denied = |errors: &[String]| {
        errors
            .iter()
            .any(|error| error.contains("is denied by permission rule 'bash:rm *'"))
    };
    // A chat in Auto.
    let auto = runtime(&fixture.0, &[("bash:rm *", D::Deny)]);
    let thread = echo_thread(&auto).await;
    let mode = auto.approval_mode(thread.id).await.unwrap();
    auto.set_approval_mode(
        thread.id,
        haarness_protocol::SetApprovalModeRequest {
            mode: haarness_protocol::ApprovalModeSetting::Auto,
            revision: Some(mode.revision),
        },
    )
    .await
    .unwrap();
    run(&auto, &thread, "nice rm -rf victim").await;
    assert!(denied(&tool_errors(&auto, &thread).await));
    // A workflow's direct call.
    let direct = auto
        .run_direct_tool(
            thread.id,
            "bash",
            json!({"command": "nohup rm -rf victim"}),
            CancellationToken::new(),
        )
        .await;
    match direct {
        Ok(result) => assert!(
            matches!(&result.item.data, ItemData::ToolResult { is_error: true, output, .. }
                if output.contains("denied")),
            "{:?}",
            result.item.data
        ),
        Err(error) => assert!(error.to_string().contains("denied"), "{error}"),
    }
    // A server that approves everything.
    let yolo = configured(
        &fixture.0,
        &[("bash:rm *", D::Deny)],
        RuntimeConfig {
            auto_approve: true,
            ..RuntimeConfig::default()
        },
    );
    let thread = echo_thread(&yolo).await;
    run(&yolo, &thread, "bash -c 'rm -rf victim'").await;
    assert!(denied(&tool_errors(&yolo, &thread).await));
    // A child agent inherits the deny for what its wrapper runs.
    let turn = yolo
        .start_turn(
            thread.id,
            turn_request(&tasks_prompt(1, "timeout 5 rm -rf victim")),
        )
        .await
        .unwrap();
    wait_done(&yolo, &turn).await;
    let child = yolo
        .list_threads(false)
        .await
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.parent_id == Some(thread.id))
        .unwrap();
    assert!(denied(&tool_errors(&yolo, &child).await));
    assert!(fixture.0.join("victim").is_dir());
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
