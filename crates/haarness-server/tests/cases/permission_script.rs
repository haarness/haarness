//! A shell request that asks a person carries the script its command runs,
//! for the card to show: display only, so its patterns, its Always rules
//! and its answers are what they were without it.
use super::*;
use haarness_core::{PermissionDecision as D, PermissionRule};
use haarness_protocol::{PermissionReply, PermissionReplyDecision as Reply};
use serde_json::json;
use std::path::{Path, PathBuf};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "haarness-permission-script-{}",
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn runtime(workspace: &Path, config: RuntimeConfig) -> AgentRuntime {
    let mut config = config;
    config.base_rules.push(PermissionRule {
        pattern: "bash:*".into(),
        decision: D::Ask,
    });
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

fn bash(arguments: serde_json::Value) -> String {
    format!("!tool bash {arguments}")
}

async fn answer(runtime: &AgentRuntime, id: &str, decision: Reply) {
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

/// The last `permission.requested` event's payload.
fn requested(runtime: &AgentRuntime) -> serde_json::Value {
    runtime
        .events()
        .subscribe(None)
        .replay
        .into_iter()
        .rev()
        .find(|event| event.event_type == "permission.requested")
        .map(|event| event.payload)
        .unwrap()
}

#[tokio::test]
async fn a_shell_request_carries_the_script_its_command_runs_and_nothing_else_changes() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.0.join("scripts")).unwrap();
    std::fs::write(
        fixture.0.join("scripts/deploy.sh"),
        "printf deployed > deployed.txt\n",
    )
    .unwrap();
    let runtime = runtime(&fixture.0, RuntimeConfig::default());
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash(json!({"command": "cd scripts && sh deploy.sh"}))),
        )
        .await
        .unwrap();
    wait_pending_permissions(&runtime, 1).await;
    let pending = runtime.pending_permissions().remove(0);
    let expected = haarness_protocol::PermissionScript {
        path: fixture.0.join("scripts/deploy.sh").display().to_string(),
        content: "printf deployed > deployed.txt\n".into(),
        truncated: false,
    };
    assert_eq!(pending.script.as_ref(), Some(&expected));
    // The listing and the live event carry the same script.
    assert_eq!(
        serde_json::to_value(&pending).unwrap()["script"],
        json!({"path": expected.path, "content": expected.content, "truncated": false})
    );
    assert_eq!(
        requested(&runtime)["script"]["path"],
        expected.path.as_str()
    );
    // Shown, never asked about: one request, with the patterns and rules the
    // command has without it.
    assert_eq!(pending.permission, "bash");
    assert_eq!(pending.patterns, vec!["sh deploy.sh"]);
    assert_eq!(pending.always_patterns, vec!["bash:sh *"]);
    answer(&runtime, &pending.permission_id, Reply::Once).await;
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("scripts/deployed.txt")).unwrap(),
        "deployed"
    );
}

#[tokio::test]
async fn a_command_that_runs_no_readable_script_carries_none() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("prod.env"), "TOKEN=fixture-only\n").unwrap();
    std::fs::create_dir_all(fixture.0.join("sub")).unwrap();
    std::fs::write(fixture.0.join("sub/x.sh"), "echo from-sub\n").unwrap();
    let runtime = runtime(&fixture.0, RuntimeConfig::default());
    let thread = echo_thread(&runtime).await;
    for arguments in [
        json!({"command": "printf hi"}),
        json!({"command": "source prod.env"}),
        json!({"command": "bash missing.sh"}),
        json!({"command": "bash -c 'sh sub/x.sh'"}),
    ] {
        let turn = runtime
            .start_turn(thread.id, turn_request(&bash(arguments.clone())))
            .await
            .unwrap();
        wait_pending_permissions(&runtime, 1).await;
        let pending = runtime.pending_permissions().remove(0);
        assert_eq!(pending.script, None, "{arguments}");
        assert!(
            serde_json::to_value(&pending)
                .unwrap()
                .get("script")
                .is_none(),
            "{arguments}"
        );
        assert!(requested(&runtime)["script"].is_null(), "{arguments}");
        answer(&runtime, &pending.permission_id, Reply::Reject).await;
        wait_done(&runtime, &turn).await;
    }
    // The command's working folder is where a relative script is looked for.
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash(json!({"command": "bash x.sh", "workdir": "sub"}))),
        )
        .await
        .unwrap();
    wait_pending_permissions(&runtime, 1).await;
    let pending = runtime.pending_permissions().remove(0);
    assert_eq!(
        pending.script.map(|script| script.content).as_deref(),
        Some("echo from-sub\n")
    );
    answer(&runtime, &pending.permission_id, Reply::Reject).await;
    wait_done(&runtime, &turn).await;
}

#[tokio::test]
async fn an_always_answer_for_a_script_remembers_the_command_as_before() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("x.sh"), "printf ran >> ran.txt\n").unwrap();
    let runtime = runtime(&fixture.0, RuntimeConfig::default());
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash(json!({"command": "sh x.sh"}))),
        )
        .await
        .unwrap();
    wait_pending_permissions(&runtime, 1).await;
    let pending = runtime.pending_permissions().remove(0);
    assert!(pending.script.is_some());
    answer(&runtime, &pending.permission_id, Reply::Always).await;
    wait_done(&runtime, &turn).await;
    // The script changed since; the remembered rule is about the command,
    // so it runs without a question, as it would have without the card.
    std::fs::write(fixture.0.join("x.sh"), "printf again >> ran.txt\n").unwrap();
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash(json!({"command": "sh x.sh"}))),
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
        "ranagain"
    );
}

#[tokio::test]
async fn a_slash_command_shell_block_carries_its_script_too() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("context.sh"), "printf CONTEXT_FROM_SCRIPT\n").unwrap();
    let mut config = RuntimeConfig::default();
    config.commands.push(haarness_protocol::CommandInfo {
        name: "probe".into(),
        description: "Dynamic context".into(),
        source: None,
        agent: None,
        model: None,
        argument_hint: None,
        template: "Context: !`sh context.sh`".into(),
        plugin: None,
        execution: haarness_protocol::DefinitionExecution::default(),
    });
    let runtime = runtime(&fixture.0, config);
    let thread = echo_thread(&runtime).await;
    let mut request = turn_request("");
    request.command = Some("probe".into());
    let turn = runtime.start_turn(thread.id, request).await.unwrap();
    wait_pending_permissions(&runtime, 1).await;
    let pending = runtime.pending_permissions().remove(0);
    assert_eq!(
        pending.script.map(|script| script.content).as_deref(),
        Some("printf CONTEXT_FROM_SCRIPT\n")
    );
    answer(&runtime, &pending.permission_id, Reply::Once).await;
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
}

#[tokio::test]
async fn a_script_the_chat_may_not_read_or_a_credential_file_is_not_shown() {
    let fixture = Fixture::new();
    for folder in ["secrets", "private", "open"] {
        std::fs::create_dir_all(fixture.0.join(folder)).unwrap();
        std::fs::write(fixture.0.join(folder).join("run.sh"), "echo run\n").unwrap();
    }
    std::fs::write(fixture.0.join(".envrc"), "export TOKEN=fixture-only\n").unwrap();
    std::fs::create_dir_all(fixture.0.join("keys")).unwrap();
    std::fs::write(fixture.0.join("keys/id_rsa"), "PRIVATE KEY\n").unwrap();
    let mut config = RuntimeConfig::default();
    for (pattern, decision) in [("read:secrets/*", D::Deny), ("read:private/*", D::Ask)] {
        config.base_rules.push(PermissionRule {
            pattern: pattern.into(),
            decision,
        });
    }
    let runtime = runtime(&fixture.0, config);
    let thread = echo_thread(&runtime).await;
    let absolute = fixture.0.join("private/run.sh").display().to_string();
    for (command, shown) in [
        // A configured read deny, and an ask, by either spelling.
        ("bash secrets/run.sh", false),
        ("bash private/run.sh", false),
        (absolute.as_str(), false),
        // Credentials and the files that export them, whatever the rules.
        ("source ./.envrc", false),
        ("sh keys/id_rsa", false),
        ("bash open/run.sh", true),
    ] {
        let turn = runtime
            .start_turn(
                thread.id,
                turn_request(&bash(json!({ "command": command }))),
            )
            .await
            .unwrap();
        wait_pending_permissions(&runtime, 1).await;
        let pending = runtime.pending_permissions().remove(0);
        assert_eq!(pending.script.is_some(), shown, "{command}");
        assert_eq!(requested(&runtime)["script"].is_null(), !shown, "{command}");
        answer(&runtime, &pending.permission_id, Reply::Reject).await;
        wait_done(&runtime, &turn).await;
    }
}

/// The tool errors in a chat, oldest first.
async fn tool_errors(runtime: &AgentRuntime, thread: &Thread) -> Vec<String> {
    runtime
        .items(thread.id, None)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item.data {
            haarness_protocol::ItemData::ToolResult {
                is_error: true,
                output,
                ..
            } => Some(output),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_script_that_changed_after_it_was_shown_does_not_run() {
    let fixture = Fixture::new();
    let script = fixture.0.join("x.sh");
    let runtime = runtime(&fixture.0, RuntimeConfig::default());
    let thread = echo_thread(&runtime).await;
    for (change, decision) in [("edit", Reply::Once), ("remove", Reply::Always)] {
        std::fs::write(&script, "printf shown > shown.txt\n").unwrap();
        let turn = runtime
            .start_turn(
                thread.id,
                turn_request(&bash(json!({"command": "sh x.sh"}))),
            )
            .await
            .unwrap();
        wait_pending_permissions(&runtime, 1).await;
        let pending = runtime.pending_permissions().remove(0);
        assert!(pending.script.is_some());
        // While the person reads the card, the file changes, or goes.
        if change == "edit" {
            std::fs::write(&script, "printf swapped > swapped.txt\n").unwrap();
        } else {
            std::fs::remove_file(&script).unwrap();
        }
        answer(&runtime, &pending.permission_id, decision).await;
        assert_eq!(
            wait_done(&runtime, &turn).await.status,
            TurnStatus::Completed
        );
        let errors = tool_errors(&runtime, &thread).await;
        assert_eq!(
            errors.last().map(String::as_str),
            Some(
                "x.sh changed after it was shown to the user; run the command again to show the new version."
            ),
            "{change}: {errors:?}"
        );
        assert!(!fixture.0.join("swapped.txt").exists(), "{change}");
        assert!(!fixture.0.join("shown.txt").exists(), "{change}");
    }
    // Unchanged, it runs, as without a card.
    std::fs::write(&script, "printf shown > shown.txt\n").unwrap();
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash(json!({"command": "sh x.sh"}))),
        )
        .await
        .unwrap();
    // The Always above covers `sh *` now, so it runs without a card.
    wait_done(&runtime, &turn).await;
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("shown.txt")).unwrap(),
        "shown"
    );
}

#[tokio::test]
async fn a_slash_command_shell_block_whose_script_changed_does_not_run() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("context.sh"), "printf CONTEXT\n").unwrap();
    let mut config = RuntimeConfig::default();
    config.commands.push(haarness_protocol::CommandInfo {
        name: "probe".into(),
        description: "Dynamic context".into(),
        source: None,
        agent: None,
        model: None,
        argument_hint: None,
        template: "Context: !`sh context.sh`".into(),
        plugin: None,
        execution: haarness_protocol::DefinitionExecution::default(),
    });
    let runtime = runtime(&fixture.0, config);
    let thread = echo_thread(&runtime).await;
    let mut request = turn_request("");
    request.command = Some("probe".into());
    let turn = runtime.start_turn(thread.id, request).await.unwrap();
    wait_pending_permissions(&runtime, 1).await;
    let pending = runtime.pending_permissions().remove(0);
    assert!(pending.script.is_some());
    std::fs::write(
        fixture.0.join("context.sh"),
        "printf swapped > swapped.txt\n",
    )
    .unwrap();
    answer(&runtime, &pending.permission_id, Reply::Once).await;
    wait_done(&runtime, &turn).await;
    assert!(!fixture.0.join("swapped.txt").exists());
}

/// Counts the events on this thread whose message is `wanted`: the runtime
/// of a `#[tokio::test]` runs its turns on the test's own thread.
struct Messages {
    wanted: &'static str,
    seen: Arc<std::sync::atomic::AtomicUsize>,
}

impl tracing::Subscriber for Messages {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        if message.0 == self.wanted {
            self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[tokio::test]
async fn auto_reads_no_script_since_it_shows_no_card() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("x.sh"), "printf ran >> ran.txt\n").unwrap();
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let _capture = tracing::subscriber::set_default(Messages {
        wanted: "reading the script a command runs for its permission prompt",
        seen: Arc::clone(&seen),
    });
    let runtime = runtime(&fixture.0, RuntimeConfig::default());
    let thread = echo_thread(&runtime).await;
    let mode = |setting| {
        let runtime = &runtime;
        async move {
            let revision = runtime.approval_mode(thread.id).await.unwrap().revision;
            runtime
                .set_approval_mode(
                    thread.id,
                    haarness_protocol::SetApprovalModeRequest {
                        mode: setting,
                        revision: Some(revision),
                    },
                )
                .await
                .unwrap();
        }
    };
    mode(haarness_protocol::ApprovalModeSetting::Auto).await;
    // Tests on other threads may have cached what this thread's capture
    // wants to see as unwanted: ask every call site again.
    tracing::callsite::rebuild_interest_cache();
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash(json!({"command": "sh x.sh"}))),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("ran.txt")).unwrap(),
        "ran"
    );
    assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 0);
    // In Review the same command shows its script, read once.
    mode(haarness_protocol::ApprovalModeSetting::Review).await;
    tracing::callsite::rebuild_interest_cache();
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash(json!({"command": "sh x.sh"}))),
        )
        .await
        .unwrap();
    wait_pending_permissions(&runtime, 1).await;
    let pending = runtime.pending_permissions().remove(0);
    assert!(pending.script.is_some());
    assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 1);
    answer(&runtime, &pending.permission_id, Reply::Reject).await;
    wait_done(&runtime, &turn).await;
}
