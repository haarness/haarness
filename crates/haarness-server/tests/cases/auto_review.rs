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
    runtime_with_tools(workspace, Vec::new(), configure)
}

/// [`runtime_with`] with `extra` tools beside the native ones.
fn runtime_with_tools(
    workspace: &Path,
    extra: Vec<Arc<dyn Tool>>,
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
        Arc::new(WithFixtures {
            native: NativeToolRegistry::new(workspace.to_path_buf()),
            extra,
        }),
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

struct WithFixtures {
    native: NativeToolRegistry,
    extra: Vec<Arc<dyn Tool>>,
}
impl ToolRegistry for WithFixtures {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.extra
            .iter()
            .find(|tool| tool.definition().name == name)
            .cloned()
            .or_else(|| self.native.get(name))
    }
    fn list(&self) -> Vec<ToolDefinition> {
        let mut tools = self.native.list();
        tools.extend(self.extra.iter().map(|tool| tool.definition()));
        tools
    }
}

/// A tool only a person answers: every call asks, like a Cloud action that
/// spends money (`forced`), or names a site in the person's Chrome
/// (`explicit`).
struct PersonOnly {
    name: &'static str,
    explicit: bool,
}
#[async_trait]
impl Tool for PersonOnly {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.into(),
            description: "Fixture a person answers".into(),
            input_schema: json!({"type":"object","properties":{}}),
        }
    }
    fn side_effectful(&self) -> bool {
        true
    }
    fn permission_constraint(&self, _args: &Value, _operation: &str) -> Option<D> {
        (!self.explicit).then_some(D::Ask)
    }
    fn explicit_approval(&self, _args: &Value, _operation: &str) -> bool {
        self.explicit
    }
    async fn execute(&self, _args: Value, _context: ToolContext) -> CoreResult<ToolOutput> {
        Ok(ToolOutput {
            text: "done".into(),
            metadata: json!({}),
        })
    }
}

async fn running(runtime: &AgentRuntime, count: usize) {
    for _ in 0..500 {
        if runtime.auto_reviews_running() == count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "expected {count} running reviews, saw {}",
        runtime.auto_reviews_running()
    );
}

#[tokio::test]
async fn spending_and_chrome_requests_are_never_reviewed() {
    let workspace = Workspace::new();
    let (runtime, provider) = runtime_with_tools(
        &workspace.0,
        vec![
            Arc::new(PersonOnly {
                name: "spend_fixture",
                explicit: false,
            }),
            Arc::new(PersonOnly {
                name: "chrome_fixture",
                explicit: true,
            }),
        ],
        |_| {},
    );
    let thread = echo_thread(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    for tool in ["spend_fixture", "chrome_fixture"] {
        let turn = runtime
            .start_turn(
                thread.id,
                turn_request(&format!("!tool {tool} {{}} [review:allow]")),
            )
            .await
            .unwrap();
        let asked = pending(&runtime).await;
        assert!(asked.review.is_none(), "{tool}");
        assert_eq!(reviews(&provider), 0, "{tool} reached the reviewer");
        reply(&runtime, &asked.permission_id, Reply::Reject).await;
        wait_done(&runtime, &turn).await;
    }
}

#[tokio::test]
async fn twenty_blocks_in_a_turn_pause_reviewing_even_without_three_in_a_row() {
    let workspace = Workspace::new();
    let (runtime, provider) = runtime_with(&workspace.0, |_| {});
    let thread = echo_thread(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    // Deny, deny, allow: never three in a row. The verdict is the marker in
    // each call, the last the reviewer sees.
    let calls: Vec<Value> = (0..30)
        .map(|index| {
            let verdict = if index % 3 == 2 { "allow" } else { "deny" };
            json!({"name": "bash", "arguments": {
                "command": format!("printf '[review:{verdict}]' > t{index}.txt")
            }})
        })
        .collect();
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&format!("!multi {}", Value::Array(calls))),
        )
        .await
        .unwrap();
    let asked = pending(&runtime).await;
    let review = asked.review.clone().unwrap();
    assert_eq!(review.status, Status::Paused);
    assert_eq!(
        review.rationale.as_deref(),
        Some("It blocked 20 actions this turn, so the rest of it is yours to approve.")
    );
    // Twenty blocks and nine approvals: the thirtieth request is a person's.
    assert_eq!(reviews(&provider), 29);
    reply(&runtime, &asked.permission_id, Reply::Reject).await;
    wait_done(&runtime, &turn).await;
    assert!(workspace.0.join("t2.txt").exists());
    assert!(!workspace.0.join("t0.txt").exists());
    assert!(!workspace.0.join("t29.txt").exists());
}

#[tokio::test]
async fn a_small_model_that_fails_twice_is_skipped_for_a_while() {
    let workspace = Workspace::new();
    let (runtime, provider) = runtime_with(&workspace.0, |config| {
        config.auto_review.small_model = Some("echo/echo-small".into());
    });
    let thread = echo_thread(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    let models = || {
        provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.output_schema.is_some())
            .map(|request| request.model.clone())
            .collect::<Vec<_>>()
    };
    for (round, expected) in [
        // The small model twice, then the chat's own model twice.
        (0, vec!["echo-small", "echo-small", "echo-1", "echo-1"]),
        // Benched: only the chat's model.
        (
            1,
            vec![
                "echo-small",
                "echo-small",
                "echo-1",
                "echo-1",
                "echo-1",
                "echo-1",
            ],
        ),
    ] {
        let turn = runtime
            .start_turn(
                thread.id,
                turn_request(&bash(&format!("printf {round} > f{round}.txt"), "fail")),
            )
            .await
            .unwrap();
        let asked = pending(&runtime).await;
        assert_eq!(asked.review.as_ref().unwrap().status, Status::Failed);
        assert_eq!(models(), expected);
        reply(&runtime, &asked.permission_id, Reply::Reject).await;
        wait_done(&runtime, &turn).await;
    }
}

#[tokio::test]
async fn a_child_agents_review_shows_its_task_and_counts_toward_its_root_chat() {
    let workspace = Workspace::new();
    let (runtime, provider) = runtime_with(&workspace.0, |config| {
        // One child at a time, so the order of the blocks is known.
        config.execution.subagent_concurrency = 1;
    });
    let thread = echo_thread(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    let child = |name: &str| {
        let calls: Vec<Value> = (1..=2)
            .map(|index| {
                json!({"name": "bash", "arguments": {
                    "command": format!("printf x > {name}{index}.txt")
                }})
            })
            .collect();
        json!({"name": "task", "arguments": {
            "agent": "general",
            "description": format!("child {name}"),
            "prompt": format!("!multi {} [review:deny]", Value::Array(calls)),
        }})
    };
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&format!("!multi {}", json!([child("a"), child("b")]))),
        )
        .await
        .unwrap();
    // Two blocks in one child and one in the other make three in a row for
    // the root chat: the fourth request is a person's.
    let asked = pending(&runtime).await;
    assert_ne!(asked.thread_id, thread.id);
    let review = asked.review.clone().unwrap();
    assert_eq!(review.status, Status::Paused, "{review:?}");
    assert_eq!(reviews(&provider), 3);
    let prompt = provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .find(|request| request.output_schema.is_some())
        .map(|request| match &request.messages[0] {
            ProviderMessage::Text { content, .. } => content.clone(),
            _ => String::new(),
        })
        .unwrap();
    assert!(prompt.contains("<delegated_task>"), "{prompt}");
    assert!(prompt.contains("parent agent: \"!multi"), "{prompt}");
    reply(&runtime, &asked.permission_id, Reply::Reject).await;
    wait_done(&runtime, &turn).await;
    for name in ["a1", "a2", "b1", "b2"] {
        assert!(!workspace.0.join(format!("{name}.txt")).exists());
    }
}

#[tokio::test]
async fn a_quick_toggle_back_to_auto_reviews_a_waiting_request_again() {
    let workspace = Workspace::new();
    let (runtime, provider) = runtime_with(&workspace.0, |_| {});
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash("printf t > toggled.txt", "stall")),
        )
        .await
        .unwrap();
    let asked = pending(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    running(&runtime, 1).await;
    // Out of Auto and straight back: whichever of the switch and the ended
    // review takes the gate first, one review runs again.
    set_mode(&runtime, thread.id, ApprovalModeSetting::Review).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    reviewing(&provider, 2).await;
    running(&runtime, 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(reviews(&provider), 2);
    assert_eq!(runtime.pending_permissions().len(), 1);
    reply(&runtime, &asked.permission_id, Reply::Reject).await;
    wait_done(&runtime, &turn).await;
    running(&runtime, 0).await;
    assert!(!workspace.0.join("toggled.txt").exists());
}

#[tokio::test]
async fn a_review_ends_when_its_request_is_answered_or_rejected_with_a_sibling() {
    let workspace = Workspace::new();
    let (runtime, provider) = runtime_with(&workspace.0, |config| {
        config.base_rules.push(PermissionRule {
            pattern: "bash:printf mine*".into(),
            decision: D::Ask,
        });
    });
    let thread = echo_thread(&runtime).await;
    // A person answers a request while the review Auto started runs.
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash("printf raced > raced.txt", "stall")),
        )
        .await
        .unwrap();
    let asked = pending(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    running(&runtime, 1).await;
    reply(&runtime, &asked.permission_id, Reply::Once).await;
    running(&runtime, 0).await;
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert_eq!(
        std::fs::read_to_string(workspace.0.join("raced.txt")).unwrap(),
        "raced"
    );
    assert_eq!(reviews(&provider), 1);

    // A person rejects a request in the same chat as one under review: both
    // are rejected, and that review ends.
    let task = json!({"name": "task", "arguments": {
        "agent": "general",
        "description": "child under review",
        "prompt": bash("printf child > child.txt", "stall"),
    }});
    let mine = json!({"name": "bash", "arguments": {"command": "printf mine > mine.txt"}});
    let turn = runtime
        .start_turn(
            thread.id,
            // The person's ask first: a task's result is taken in call order,
            // so its child runs beside the ask.
            turn_request(&format!("!multi {}", json!([mine, task]))),
        )
        .await
        .unwrap();
    running(&runtime, 1).await;
    let asked = pending(&runtime).await;
    assert_eq!(asked.thread_id, thread.id, "the person's own ask");
    reply(&runtime, &asked.permission_id, Reply::Reject).await;
    running(&runtime, 0).await;
    wait_done(&runtime, &turn).await;
    assert!(!workspace.0.join("child.txt").exists());
    assert!(!workspace.0.join("mine.txt").exists());
    assert!(runtime.pending_permissions().is_empty());
}

#[tokio::test]
async fn a_step_the_reviewer_blocks_inside_a_batch_keeps_its_note() {
    let workspace = Workspace::new();
    let (runtime, _provider) = runtime_with(&workspace.0, |_| {});
    let thread = echo_thread(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    let calls = json!({"tool_calls": [
        {"tool": "bash", "parameters": {"command": "printf '[review:deny]' > batch.txt"}},
        {"tool": "bash", "parameters": {"command": "printf '[review:allow]' > fine.txt"}},
    ]});
    let turn = runtime
        .start_turn(thread.id, turn_request(&format!("!tool batch {calls}")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let items = runtime.items(thread.id, None).await.unwrap();
    let (_, _, metadata) = results(&items)
        .into_iter()
        .find(|(_, _, metadata)| metadata.get("results").is_some())
        .expect("the batch result");
    let blocked = &metadata["results"][0];
    assert_eq!(blocked["metadata"]["review"]["status"], "blocked");
    assert_eq!(
        blocked["metadata"]["review"]["rationale"],
        "Test verdict: deny."
    );
    assert!(
        blocked["error"]
            .as_str()
            .unwrap()
            .starts_with("Auto review blocked this action")
    );
    assert!(metadata["results"][1].get("error").is_none());
    assert!(!workspace.0.join("batch.txt").exists());
    assert!(workspace.0.join("fine.txt").exists());
}

#[tokio::test]
async fn turning_auto_on_with_a_card_shown_says_so_and_keeps_the_note() {
    let workspace = Workspace::new();
    let (runtime, provider) = runtime_with(&workspace.0, |_| {});
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request(&bash("printf a > kept.txt", "ask")))
        .await
        .unwrap();
    let asked = pending(&runtime).await;
    assert!(asked.review.is_none());
    let mut events = runtime.events().subscribe(None).receiver;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    reviewing(&provider, 1).await;
    // The reviewer leaves it to the person: the card is announced again,
    // first as being reviewed, then with the note, and the listing agrees.
    for _ in 0..500 {
        if runtime.pending_permissions()[0]
            .review
            .as_ref()
            .is_some_and(|review| review.status == Status::Asked)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let listed = runtime.pending_permissions();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].permission_id, asked.permission_id);
    assert_eq!(
        listed[0].review.as_ref().unwrap().rationale.as_deref(),
        Some("Test verdict: ask.")
    );
    let mut announced = Vec::new();
    let mut states = Vec::new();
    while let Ok(event) = events.try_recv() {
        match event.event_type.as_str() {
            "permission.requested" => {
                assert_eq!(event.payload["permissionId"], asked.permission_id.as_str());
                announced.push(event.payload["review"]["status"].clone());
            }
            "turn.status" => states.push(event.payload["state"].clone()),
            _ => {}
        }
    }
    assert_eq!(announced, [json!("reviewing"), json!("asked")]);
    assert!(states.contains(&json!("reviewing")), "{states:?}");
    assert_eq!(
        states.last(),
        Some(&json!("waiting_permission")),
        "{states:?}"
    );
    reply(&runtime, &asked.permission_id, Reply::Once).await;
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );

    // Approved by the reviewer, the card goes; a person's late click finds
    // nothing, which clients take as already settled.
    set_mode(&runtime, thread.id, ApprovalModeSetting::Review).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request(&bash("printf b > late.txt", "allow")),
        )
        .await
        .unwrap();
    let asked = pending(&runtime).await;
    set_mode(&runtime, thread.id, ApprovalModeSetting::Auto).await;
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let late = runtime
        .reply_permission(
            &asked.permission_id,
            PermissionReply {
                decision: Reply::Reject,
                feedback: None,
            },
        )
        .await;
    assert!(matches!(late, Err(CoreError::NotFound(_))), "{late:?}");
    assert!(workspace.0.join("late.txt").exists());
}
