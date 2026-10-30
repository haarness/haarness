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
        Self(dunce::canonicalize(&path).unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn runtime(workspace: &Path, rules: &[(&str, D)]) -> AgentRuntime {
    configured(
        workspace,
        rules,
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            ..RuntimeConfig::default()
        },
    )
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

// POSIX shell commands for the agent's shell tool, which runs cmd.exe on Windows.
#[cfg(unix)]
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

// POSIX shell commands for the agent's shell tool, which runs cmd.exe on Windows.
#[cfg(unix)]
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
            approval_mode: haarness_protocol::ApprovalMode::Review,
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

/// More wrappers than are read: what the last one runs stays unread.
fn stacked(command: &str) -> String {
    format!("{}{command}", "nice ".repeat(17))
}

async fn scheduled_thread(runtime: &AgentRuntime) -> Thread {
    runtime
        .create_thread(CreateThreadRequest {
            schedule: Some(haarness_protocol::ScheduleOrigin {
                id: uuid::Uuid::now_v7(),
                name: "Nightly".into(),
            }),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap()
}

/// The turn a schedule starts: it approves its own requests.
fn scheduled(text: &str) -> StartTurnRequest {
    let mut request = turn_request(text);
    request.auto_approve = true;
    request
}

async fn auto(runtime: &AgentRuntime, thread: &Thread) {
    let mode = runtime.approval_mode(thread.id).await.unwrap();
    runtime
        .set_approval_mode(
            thread.id,
            haarness_protocol::SetApprovalModeRequest {
                mode: haarness_protocol::ApprovalModeSetting::Auto,
                revision: Some(mode.revision),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_command_too_long_to_read_is_refused_by_a_deny_in_auto_yolo_schedules_and_children() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.0.join("victim")).unwrap();
    let refused = |errors: &[String]| {
        errors.iter().any(|error| {
            error.contains("Haarness could not read all of")
                && error.contains("permission rule 'bash:rm *' may apply")
        })
    };
    let padded = format!("bash -c '{}rm -rf victim'", "true; ".repeat(300 * 1024 / 6));
    let runtime = runtime(&fixture.0, &[("bash:rm *", D::Deny)]);
    // Each in a chat of its own: 300 KiB of history would be compacted
    // before the next call.
    for command in [stacked("rm -rf victim"), padded] {
        let thread = echo_thread(&runtime).await;
        auto(&runtime, &thread).await;
        run(&runtime, &thread, &command).await;
        let errors = tool_errors(&runtime, &thread).await;
        assert!(refused(&errors), "{errors:?}");
        // The refusal names the command clipped, not 300 KiB of it.
        assert!(errors.iter().all(|error| error.len() < 1024));
    }
    let schedule = scheduled_thread(&runtime).await;
    let turn = runtime
        .start_turn(schedule.id, scheduled(&bash(&stacked("rm -rf victim"))))
        .await
        .unwrap();
    wait_done(&runtime, &turn).await;
    assert!(refused(&tool_errors(&runtime, &schedule).await));
    // Under yolo, and in a child agent that has only its parent's deny.
    let yolo = configured(
        &fixture.0,
        &[("bash:rm *", D::Deny)],
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            auto_approve: true,
            ..RuntimeConfig::default()
        },
    );
    let thread = echo_thread(&yolo).await;
    run(&yolo, &thread, &stacked("rm -rf victim")).await;
    assert!(refused(&tool_errors(&yolo, &thread).await));
    let turn = yolo
        .start_turn(
            thread.id,
            turn_request(&tasks_prompt(1, &stacked("rm -rf victim"))),
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
    assert!(refused(&tool_errors(&yolo, &child).await));
    assert!(fixture.0.join("victim").is_dir());
}

#[tokio::test]
async fn a_command_too_long_to_read_under_an_ask_waits_for_a_person_in_auto_and_yolo() {
    let fixture = Fixture::new();
    let command = stacked("printf hi > hi.txt");
    for config in [
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            ..RuntimeConfig::default()
        },
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            auto_approve: true,
            ..RuntimeConfig::default()
        },
    ] {
        let yolo = config.auto_approve;
        let runtime = configured(&fixture.0, &[("bash:printf *", D::Ask)], config);
        let thread = echo_thread(&runtime).await;
        auto(&runtime, &thread).await;
        let turn = runtime
            .start_turn(thread.id, turn_request(&bash(&command)))
            .await
            .unwrap();
        wait_pending_permissions(&runtime, 1).await;
        let pending = runtime.pending_permissions().remove(0);
        // Only a person answers, once: no Always, and the card says why.
        assert_eq!(pending.allow_always, Some(false), "yolo {yolo}");
        assert_eq!(pending.detail["__readLimit"], "printf *");
        assert_eq!(pending.detail["command"], command.as_str());
        assert_eq!(pending.operation, format!("bash:{command}"));
        reply(&runtime, &pending.permission_id, Reply::Once).await;
        assert_eq!(
            wait_done(&runtime, &turn).await.status,
            TurnStatus::Completed
        );
        assert_eq!(
            std::fs::read_to_string(fixture.0.join("hi.txt")).unwrap(),
            "hi"
        );
        std::fs::remove_file(fixture.0.join("hi.txt")).unwrap();
    }
    // A schedule cannot ask, so it is refused at once.
    let runtime = runtime(&fixture.0, &[("bash:printf *", D::Ask)]);
    let schedule = scheduled_thread(&runtime).await;
    let turn = runtime
        .start_turn(schedule.id, scheduled(&bash(&command)))
        .await
        .unwrap();
    wait_done(&runtime, &turn).await;
    assert!(runtime.pending_permissions().is_empty());
    let errors = tool_errors(&runtime, &schedule).await;
    assert!(
        errors.iter().any(|error| error
            .contains("needs the user's approval every time, and this scheduled run cannot ask")),
        "{errors:?}"
    );
    assert!(!fixture.0.join("hi.txt").exists());
}

// POSIX shell commands for the agent's shell tool, which runs cmd.exe on Windows.
#[cfg(unix)]
#[tokio::test]
async fn a_line_too_large_to_read_asks_a_person_after_an_always_and_keeps_its_outside_paths() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let nested = format!("echo {}x{}", "$(echo ".repeat(1500), ")".repeat(1500));
    let line = format!("{nested}; printf done > done.txt");
    // An Always on `echo`, then the line in Auto: only a person answers.
    let review = runtime(&fixture.0, &[("bash:echo *", D::Ask)]);
    let thread = echo_thread(&review).await;
    let turn = review
        .start_turn(thread.id, turn_request(&bash("echo hi")))
        .await
        .unwrap();
    wait_pending_permissions(&review, 1).await;
    let pending = review.pending_permissions().remove(0);
    reply(&review, &pending.permission_id, Reply::Always).await;
    wait_done(&review, &turn).await;
    auto(&review, &thread).await;
    let turn = review
        .start_turn(thread.id, turn_request(&bash(&line)))
        .await
        .unwrap();
    wait_pending_permissions(&review, 1).await;
    let pending = review.pending_permissions().remove(0);
    assert_eq!(pending.permission, "bash");
    assert_eq!(pending.allow_always, Some(false));
    // The card names the bash rule that may apply.
    assert_eq!(pending.detail["__readLimit"], "echo *");
    assert!(pending.operation.len() < 1024);
    reply(&review, &pending.permission_id, Reply::Reject).await;
    wait_done(&review, &turn).await;
    assert!(!fixture.0.join("done.txt").exists());
    // A bash deny refuses it.
    let denied = runtime(&fixture.0, &[("bash:git push *", D::Deny)]);
    let thread = echo_thread(&denied).await;
    run(&denied, &thread, &line).await;
    let errors = tool_errors(&denied, &thread).await;
    assert!(
        errors
            .iter()
            .any(|error| error.contains("Haarness could not read all of")),
        "{errors:?}"
    );
    // A path outside the project after the limit still asks.
    let plain = runtime(&fixture.0, &[]);
    let thread = echo_thread(&plain).await;
    let touching = format!("{nested}; touch {}/x", outside.0.display());
    let turn = plain
        .start_turn(thread.id, turn_request(&bash(&touching)))
        .await
        .unwrap();
    wait_pending_permissions(&plain, 1).await;
    let pending = plain.pending_permissions().remove(0);
    assert_eq!(pending.permission, "external_directory");
    assert!(
        pending
            .patterns
            .iter()
            .any(|pattern| pattern.starts_with(&outside.0.display().to_string())),
        "{:?}",
        pending.patterns
    );
    reply(&plain, &pending.permission_id, Reply::Reject).await;
    wait_done(&plain, &turn).await;
    assert!(!outside.0.join("x").exists());
}
