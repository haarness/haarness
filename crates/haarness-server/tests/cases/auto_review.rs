//! Auto asks a reviewer model: it approves an eligible request once, blocks
//! it with a reason the model reads, or leaves it to a person with its note.
//! The deterministic reviewer of the test providers answers here, picked by
//! a `[review:<verdict>]` marker after the scripted call.
use super::*;
use haarness_core::{HookDecision, HookPort, PermissionDecision as D, PermissionRule};
use haarness_protocol::{
    ApprovalModeSetting, ItemData, PendingPermissionInfo, PermissionReply,
    PermissionReplyDecision as Reply, PermissionReviewStatus as Status, SetApprovalModeRequest,
    ThreadId,
};
use serde_json::json;
use std::path::{Path, PathBuf};

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("haarness-auto-review-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A runtime whose commands ask (`"bash": "ask"`, a whole-tool ask the
/// reviewer answers), changed by `configure`.
fn runtime_with(
    workspace: &Path,
    configure: impl FnOnce(&mut RuntimeConfig),
) -> (AgentRuntime, Capturing) {
    let provider = Capturing::default();
    let mut config = RuntimeConfig::default();
    config.base_rules.push(PermissionRule {
        pattern: "bash:*".into(),
        decision: D::Ask,
    });
    configure(&mut config);
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(workspace.to_path_buf())),
        EventBus::new(4096),
        workspace.to_path_buf(),
        String::new(),
        config,
    );
    (runtime, provider)
}

/// The review requests the provider received.
fn reviews(provider: &Capturing) -> usize {
    provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| {
            request
                .output_schema
                .as_ref()
                .and_then(|schema| schema.get("description"))
                .and_then(Value::as_str)
                == Some(haarness_core::auto_review::VERDICT_SCHEMA_DESCRIPTION)
        })
        .count()
}

async fn set_mode(runtime: &AgentRuntime, thread: ThreadId, mode: ApprovalModeSetting) {
    runtime
        .set_approval_mode(
            thread,
            SetApprovalModeRequest {
                mode,
                revision: None,
            },
        )
        .await
        .unwrap();
}

async fn pending(runtime: &AgentRuntime) -> PendingPermissionInfo {
    for _ in 0..500 {
        if let Some(pending) = runtime.pending_permissions().into_iter().next() {
            return pending;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no permission request was shown");
}

async fn reviewing(provider: &Capturing, count: usize) {
    for _ in 0..500 {
        if reviews(provider) >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the reviewer was not asked {count} times");
}

fn bash(command: &str, verdict: &str) -> String {
    format!(
        "!tool bash {} [review:{verdict}]",
        json!({ "command": command })
    )
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

fn results(items: &[haarness_protocol::Item]) -> Vec<(String, bool, Value)> {
    items
        .iter()
        .filter_map(|item| match &item.data {
            ItemData::ToolResult {
                output,
                is_error,
                metadata,
                ..
            } => Some((output.clone(), *is_error, metadata.clone())),
            _ => None,
        })
        .collect()
}

fn prompts(items: &[haarness_protocol::Item]) -> usize {
    items
        .iter()
        .filter(|item| matches!(item.data, ItemData::PermissionRequest { .. }))
        .count()
}

#[tokio::test]
async fn auto_approves_what_the_reviewer_allows_and_blocks_what_it_denies() {
    let workspace = Workspace::new();
    let (runtime, provider) = runtime_with(&workspace.0, |_| {});
    let thread = echo_thread(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;

    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash("printf yes > allowed.txt", "allow")),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(workspace.0.join("allowed.txt").exists());
    assert_eq!(reviews(&provider), 1);

    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash("printf no > blocked.txt", "deny")),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(!workspace.0.join("blocked.txt").exists());
    let items = runtime.items(thread.id, None).await.unwrap();
    assert_eq!(prompts(&items), 0, "Auto never shows what it reviewed");
    let (output, is_error, metadata) = results(&items).pop().unwrap();
    assert!(is_error);
    assert_eq!(
        output,
        "Auto review blocked this action (Test rule): Test verdict: deny. Do not try to reach the same result another way; ask the user if it is needed."
    );
    assert_eq!(metadata["review"]["status"], "blocked");
    assert_eq!(metadata["review"]["rule"], "Test rule");
    assert_eq!(metadata["review"]["rationale"], "Test verdict: deny.");
    assert_eq!(metadata["review"]["risk"], "critical");
    // What the reviewer saw: the person's messages and the agent's calls,
    // the action whole with where it runs; never the agent's prose or a
    // tool's output.
    let review = provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|request| request.output_schema.is_some())
        .cloned()
        .unwrap();
    assert!(review.system.starts_with("You review one action"));
    assert_eq!(review.parameters.temperature, Some(0.0));
    assert_eq!(review.parameters.max_output_tokens, Some(2048));
    assert!(review.tools.is_empty());
    let ProviderMessage::Text { content, .. } = &review.messages[0] else {
        panic!("a text prompt");
    };
    for part in [
        "<transcript>",
        "user: \"!tool bash {\\\"command\\\":\\\"printf yes \\u003e allowed.txt\\\"} [review:allow]\"",
        "agent called: \"bash\" {\"command\":\"printf yes \\u003e allowed.txt\"}",
        "<action>",
        "\"command\": \"printf no \\u003e blocked.txt\"",
        "\"permission\": \"bash\"",
        &format!("Working directory: \"{}\"", workspace.0.display()),
        "No sandbox",
    ] {
        assert!(content.contains(part), "{part} missing from {content}");
    }
    assert!(
        !content.contains("Tool finished"),
        "no agent prose: {content}"
    );
    assert!(!content.contains("exit: 0"), "no tool output: {content}");
    // An approval is once: Auto never remembers a rule.
    set_mode(&runtime, thread.id, ApprovalModeSetting::Review).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash("printf yes > allowed.txt", "allow")),
        )
        .await
        .unwrap();
    let asked = pending(&runtime).await;
    assert!(asked.review.is_none(), "Review shows a plain prompt");
    reply(&runtime, &asked.permission_id, Reply::Reject).await;
    wait_done(&runtime, &turn).await;
    assert_eq!(reviews(&provider), 2, "Review never asks the reviewer");
}

#[tokio::test]
async fn what_the_reviewer_leaves_to_a_person_carries_its_note() {
    let workspace = Workspace::new();
    let (runtime, provider) = runtime_with(&workspace.0, |_| {});
    let thread = echo_thread(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    let mut events = runtime.events().subscribe(None).receiver;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash("printf asked > asked.txt", "ask")),
        )
        .await
        .unwrap();
    let asked = pending(&runtime).await;
    let review = asked.review.clone().expect("the reviewer's note");
    assert_eq!(review.status, Status::Asked);
    assert_eq!(review.rule.as_deref(), Some("Test rule"));
    assert_eq!(review.rationale.as_deref(), Some("Test verdict: ask."));
    assert_eq!(review.risk, Some(haarness_protocol::ReviewRisk::High));
    // The stored prompt and the event carry it too.
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(items.iter().any(|item| matches!(
        &item.data,
        ItemData::PermissionRequest { review: Some(note), .. } if note.status == Status::Asked
    )));
    let mut announced = None;
    while let Ok(event) = events.try_recv() {
        if event.event_type == "permission.requested" {
            announced = Some(event.payload);
        }
    }
    assert_eq!(announced.unwrap()["review"]["status"], "asked");
    reply(&runtime, &asked.permission_id, Reply::Once).await;
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert_eq!(
        std::fs::read_to_string(workspace.0.join("asked.txt")).unwrap(),
        "asked"
    );
    assert_eq!(reviews(&provider), 1);
}

#[tokio::test]
async fn a_failed_or_late_review_asks_a_person_and_never_approves() {
    let workspace = Workspace::new();
    let (runtime, provider) = runtime_with(&workspace.0, |config| {
        config.auto_review.timeout = Duration::from_secs(1);
    });
    let thread = echo_thread(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash("printf f > failed.txt", "fail")),
        )
        .await
        .unwrap();
    let asked = pending(&runtime).await;
    let review = asked.review.clone().unwrap();
    assert_eq!(review.status, Status::Failed);
    assert!(
        review
            .rationale
            .as_deref()
            .unwrap()
            .starts_with("It could not decide"),
        "{review:?}"
    );
    // A failed call is tried once more.
    assert_eq!(reviews(&provider), 2);
    reply(&runtime, &asked.permission_id, Reply::Reject).await;
    wait_done(&runtime, &turn).await;

    // Past the deadline: the second call came after a third of it, and the
    // request still settles on time.
    let started = std::time::Instant::now();
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash("printf l > late.txt", "stall")),
        )
        .await
        .unwrap();
    let asked = pending(&runtime).await;
    assert!(started.elapsed() < Duration::from_secs(5));
    let review = asked.review.clone().unwrap();
    assert_eq!(review.status, Status::Failed);
    assert_eq!(
        review.rationale.as_deref(),
        Some("It took longer than 1 second.")
    );
    assert_eq!(reviews(&provider), 4);
    reply(&runtime, &asked.permission_id, Reply::Reject).await;
    wait_done(&runtime, &turn).await;
    assert!(!workspace.0.join("failed.txt").exists());
    assert!(!workspace.0.join("late.txt").exists());
}

#[tokio::test]
async fn after_three_blocks_in_a_row_the_rest_of_the_turn_is_a_persons() {
    let workspace = Workspace::new();
    let (runtime, provider) = runtime_with(&workspace.0, |_| {});
    let thread = echo_thread(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    let calls: Vec<Value> = (1..=4)
        .map(|index| json!({"name": "bash", "arguments": {"command": format!("printf {index} > b{index}.txt")}}))
        .collect();
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&format!("!multi {} [review:deny]", Value::Array(calls))),
        )
        .await
        .unwrap();
    let asked = pending(&runtime).await;
    let review = asked.review.clone().unwrap();
    assert_eq!(review.status, Status::Paused);
    assert_eq!(
        review.rationale.as_deref(),
        Some("It blocked 3 actions in a row, so the rest of this turn is yours to approve.")
    );
    assert_eq!(reviews(&provider), 3, "the paused request is not reviewed");
    reply(&runtime, &asked.permission_id, Reply::Reject).await;
    wait_done(&runtime, &turn).await;
    let items = runtime.items(thread.id, None).await.unwrap();
    let blocked = results(&items)
        .into_iter()
        .filter(|(_, _, metadata)| metadata["review"]["status"] == "blocked")
        .count();
    assert_eq!(blocked, 3);
    for index in 1..=4 {
        assert!(!workspace.0.join(format!("b{index}.txt")).exists());
    }
    // The next turn starts counting again.
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash("printf ok > next.txt", "allow")),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(workspace.0.join("next.txt").exists());
}

#[tokio::test]
async fn an_approval_covers_the_same_calls_later_requests() {
    let workspace = Workspace::new();
    let outside = Workspace::new();
    std::fs::write(outside.0.join("note.txt"), "outside note").unwrap();
    let (runtime, provider) = runtime_with(&workspace.0, |_| {});
    let thread = echo_thread(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    // One call, two requests: the folder outside the project, then the
    // command. The first review covers the second.
    let command = format!("cat {}/note.txt", outside.0.display());
    let turn = runtime
        .start_turn(thread.id, turn_request(&bash(&command, "allow")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let items = runtime.items(thread.id, None).await.unwrap();
    let (output, is_error, _) = results(&items).pop().unwrap();
    assert!(!is_error, "{output}");
    assert!(output.contains("outside note"), "{output}");
    assert_eq!(reviews(&provider), 1);
}

#[tokio::test]
async fn switching_to_review_mid_review_shows_the_prompt_and_never_approves() {
    let workspace = Workspace::new();
    let (runtime, provider) = runtime_with(&workspace.0, |_| {});
    let thread = echo_thread(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    let mut events = runtime.events().subscribe(None).receiver;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash("printf no > switched.txt", "stall")),
        )
        .await
        .unwrap();
    reviewing(&provider, 1).await;
    // Under review the request is registered but not shown or listed.
    assert!(runtime.pending_permissions().is_empty());
    set_mode(&runtime, thread.id, ApprovalModeSetting::Review).await;
    // The review ends at once (it would stall for minutes): the prompt is
    // there, a plain one.
    let asked = pending(&runtime).await;
    assert!(asked.review.is_none());
    let mut states = Vec::new();
    while let Ok(event) = events.try_recv() {
        if event.event_type == "turn.status" {
            states.push(
                event.payload["state"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            );
        }
    }
    assert!(
        states.iter().any(|state| state == "reviewing"),
        "clients hear that a review runs: {states:?}"
    );
    reply(&runtime, &asked.permission_id, Reply::Reject).await;
    wait_done(&runtime, &turn).await;
    assert!(!workspace.0.join("switched.txt").exists());

    // A request shown in Review is reviewed when Auto turns on: a block
    // refuses it, an approval releases it.
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash("printf no > waiting.txt", "deny")),
        )
        .await
        .unwrap();
    let asked = pending(&runtime).await;
    assert!(asked.review.is_none());
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(runtime.pending_permissions().is_empty());
    assert!(!workspace.0.join("waiting.txt").exists());
    let items = runtime.items(thread.id, None).await.unwrap();
    let (_, is_error, metadata) = results(&items).pop().unwrap();
    assert!(is_error);
    assert_eq!(metadata["review"]["status"], "blocked");
    set_mode(&runtime, thread.id, ApprovalModeSetting::Review).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash("printf yes > released.txt", "allow")),
        )
        .await
        .unwrap();
    pending(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(workspace.0.join("released.txt").exists());
    // One the reviewer leaves to a person stays shown.
    set_mode(&runtime, thread.id, ApprovalModeSetting::Review).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash("printf maybe > kept.txt", "ask")),
        )
        .await
        .unwrap();
    let asked = pending(&runtime).await;
    let before = reviews(&provider);
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    reviewing(&provider, before + 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        runtime.pending_permissions()[0].permission_id,
        asked.permission_id
    );
    reply(&runtime, &asked.permission_id, Reply::Reject).await;
    wait_done(&runtime, &turn).await;
}

#[tokio::test]
async fn stop_mid_review_cancels_the_review_and_the_request() {
    let workspace = Workspace::new();
    let (runtime, provider) = runtime_with(&workspace.0, |_| {});
    let thread = echo_thread(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash("printf no > stopped.txt", "stall")),
        )
        .await
        .unwrap();
    reviewing(&provider, 1).await;
    let started = std::time::Instant::now();
    runtime.interrupt(turn.id).await.unwrap();
    let finished = wait_done(&runtime, &turn).await;
    assert_ne!(finished.status, TurnStatus::Completed);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(runtime.pending_permissions().is_empty());
    assert!(!workspace.0.join("stopped.txt").exists());
    let items = runtime.items(thread.id, None).await.unwrap();
    assert_eq!(prompts(&items), 0);
}

#[tokio::test]
async fn requests_only_a_person_answers_are_never_reviewed() {
    let workspace = Workspace::new();
    std::fs::create_dir_all(workspace.0.join(".haarness")).unwrap();
    let (runtime, provider) = runtime_with(&workspace.0, |config| {
        config.base_rules.push(PermissionRule {
            pattern: "edit:*".into(),
            decision: D::Allow,
        });
    });
    let thread = echo_thread(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(
                r#"!tool write {"path":".haarness/haarness.json","content":"{}"} [review:allow]"#,
            ),
        )
        .await
        .unwrap();
    let asked = pending(&runtime).await;
    assert_eq!(asked.allow_always, Some(false));
    assert!(asked.review.is_none());
    assert_eq!(reviews(&provider), 0);
    reply(&runtime, &asked.permission_id, Reply::Reject).await;
    wait_done(&runtime, &turn).await;
    assert!(!workspace.0.join(".haarness/haarness.json").exists());
}

/// Refuses every request it is shown, with a reason.
struct RefusesRequests;
#[async_trait]
impl HookPort for RefusesRequests {
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
            block: Some("not on this machine".into()),
            permission: Some(D::Deny),
            ..HookDecision::default()
        }
    }
    async fn stop(&self, _: ThreadId) {}
}

#[tokio::test]
async fn hooks_and_configured_asks_decide_before_the_reviewer() {
    let workspace = Workspace::new();
    let (runtime, provider) = runtime_with(&workspace.0, |_| {});
    let runtime = runtime.with_hooks(Arc::new(RefusesRequests));
    let thread = echo_thread(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash("printf no > hooked.txt", "allow")),
        )
        .await
        .unwrap();
    wait_done(&runtime, &turn).await;
    assert!(!workspace.0.join("hooked.txt").exists());
    assert_eq!(reviews(&provider), 0);

    // An ask a person configured about one command goes to a person in Auto.
    let narrow = |config: &mut RuntimeConfig| {
        config.base_rules.push(PermissionRule {
            pattern: "bash:printf *".into(),
            decision: D::Ask,
        });
    };
    let (runtime, provider) = runtime_with(&workspace.0, narrow);
    let thread = echo_thread(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash("printf mine > narrow.txt", "allow")),
        )
        .await
        .unwrap();
    let asked = pending(&runtime).await;
    assert!(asked.review.is_none());
    assert_eq!(reviews(&provider), 0);
    reply(&runtime, &asked.permission_id, Reply::Reject).await;
    wait_done(&runtime, &turn).await;
    assert!(!workspace.0.join("narrow.txt").exists());

    // With the reviewer off, Auto approves it once, as before.
    let (runtime, provider) = runtime_with(&workspace.0, |config| {
        narrow(config);
        config.auto_review.enabled = false;
    });
    let thread = echo_thread(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash("printf mine > narrow.txt", "deny")),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(workspace.0.join("narrow.txt").exists());
    assert_eq!(reviews(&provider), 0);
    assert!(runtime.pending_permissions().is_empty());
}

#[tokio::test]
async fn a_workflows_calls_are_reviewed_and_a_persons_own_are_approved_once() {
    let workspace = Workspace::new();
    let (runtime, provider) = runtime_with(&workspace.0, |_| {});
    let thread = echo_thread(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    // The marker is in the action the reviewer sees.
    let blocked = runtime
        .run_workflow_tool(
            thread.id,
            "bash",
            json!({"command": "printf '[review:deny]' > workflow.txt"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(blocked.is_error);
    assert_eq!(blocked.output.metadata["review"]["status"], "blocked");
    assert!(
        blocked
            .output
            .text
            .starts_with("Auto review blocked this action (Test rule)"),
        "{}",
        blocked.output.text
    );
    assert!(!workspace.0.join("workflow.txt").exists());
    assert_eq!(reviews(&provider), 1);
    let own = runtime
        .run_direct_tool(
            thread.id,
            "bash",
            json!({"command": "printf '[review:deny]' > mine.txt"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(!own.is_error, "{}", own.output.text);
    assert!(workspace.0.join("mine.txt").exists());
    assert_eq!(
        reviews(&provider),
        1,
        "a person's own command is not reviewed"
    );
    // Waiting in Review, it is released when Auto turns on, as before.
    set_mode(&runtime, thread.id, ApprovalModeSetting::Review).await;
    let runtime = Arc::new(runtime);
    let waiting = tokio::spawn({
        let runtime = Arc::clone(&runtime);
        async move {
            runtime
                .run_direct_tool(
                    thread.id,
                    "bash",
                    json!({"command": "printf waited > waited.txt"}),
                    CancellationToken::new(),
                )
                .await
        }
    });
    pending(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    let own = waiting.await.unwrap().unwrap();
    assert!(!own.is_error, "{}", own.output.text);
    assert!(workspace.0.join("waited.txt").exists());
    assert_eq!(reviews(&provider), 1);
}
