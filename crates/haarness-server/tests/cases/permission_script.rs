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
