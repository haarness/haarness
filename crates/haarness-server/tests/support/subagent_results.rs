//! What a parent agent reads as a subagent's result when the child stopped
//! at its output limit, wrote nothing, or was stopped by the provider's
//! safety check.
use super::*;
use haarness_protocol::{Item, ItemData, ThreadId, TurnId};

/// Parent turns follow the `!tool <name> <json>` dialect; a child's prompt
/// names how its run ends.
struct Children;

fn usage(output_tokens: u64) -> ProviderEvent {
    ProviderEvent::Usage {
        input_tokens: 100,
        output_tokens,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    }
}

fn finish(reason: &str) -> ProviderEvent {
    ProviderEvent::Finished {
        reason: reason.into(),
    }
}

#[async_trait]
impl ModelProvider for Children {
    fn id(&self) -> &str {
        "echo"
    }
    fn info(&self) -> ProviderInfo {
        Echo.info()
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        _: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if let Some(title) = title_fixture(&request) {
            return title;
        }
        let mut prompt = "";
        let mut answered = false;
        for message in &request.messages {
            match message {
                ProviderMessage::Text { role, content } if role == "user" => {
                    prompt = content.as_str();
                    answered = false;
                }
                ProviderMessage::ToolResult { .. } => answered = true,
                _ => {}
            }
        }
        let events = if let Some(script) = prompt.trim().strip_prefix("!tool ") {
            if answered {
                vec![
                    ProviderEvent::TextDelta("Parent finished".into()),
                    finish("stop"),
                ]
            } else {
                let (name, arguments) = script.split_once(' ').unwrap();
                vec![
                    ProviderEvent::ToolCall {
                        call_id: "parent-call".into(),
                        name: name.into(),
                        arguments: serde_json::from_str(arguments).unwrap(),
                        provider_metadata: Value::Null,
                    },
                    finish("tool_use"),
                ]
            }
        } else {
            match prompt.trim() {
                "answer" => vec![
                    ProviderEvent::TextDelta("first answer".into()),
                    finish("stop"),
                ],
                "blank" => vec![ProviderEvent::TextDelta("  \n".into()), finish("stop")],
                // Reasoning uses up the output limit before any reply.
                "limit" => vec![
                    ProviderEvent::ReasoningDelta("thinking".into()),
                    usage(4096),
                    finish("max_tokens"),
                ],
                // Narration and a call, then the limit (or a round that
                // writes nothing) in the next round.
                "narrate" | "narrate then stop" if !answered => vec![
                    ProviderEvent::TextDelta("Let me look.".into()),
                    ProviderEvent::ToolCall {
                        call_id: "child-call".into(),
                        name: "missing_tool".into(),
                        arguments: serde_json::json!({}),
                        provider_metadata: Value::Null,
                    },
                    finish("tool_use"),
                ],
                "narrate" => vec![
                    ProviderEvent::ReasoningDelta("thinking".into()),
                    usage(4096),
                    finish("length"),
                ],
                "narrate then stop" => vec![
                    ProviderEvent::ReasoningDelta("thinking".into()),
                    finish("stop"),
                ],
                "block" => vec![
                    ProviderEvent::TextDelta("partial".into()),
                    finish("refusal"),
                ],
                // A delivered background result, and anything else.
                _ => vec![ProviderEvent::TextDelta("noted".into()), finish("stop")],
            }
        };
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

fn runtime() -> (std::path::PathBuf, AgentRuntime) {
    let directory =
        std::env::temp_dir().join(format!("haarness-subagent-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&directory).unwrap();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Children) as Arc<dyn ModelProvider>
        ])),
        Arc::new(NativeToolRegistry::new(directory.clone())),
        EventBus::new(2048),
        directory.clone(),
        String::new(),
        RuntimeConfig::default(),
    );
    (directory, runtime)
}

/// Runs one parent turn that delegates with `arguments` and returns the
/// turn with the parent's items.
async fn delegate(
    runtime: &AgentRuntime,
    thread: ThreadId,
    arguments: &Value,
) -> (TurnId, Vec<Item>) {
    let turn = runtime
        .start_turn(thread, turn_request(&format!("!tool task {arguments}")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(runtime, &turn).await.status,
        TurnStatus::Completed
    );
    wait_idle(runtime, thread).await;
    (turn.id, runtime.items(thread, None).await.unwrap())
}

/// The `task` result of `turn`: its text and whether it is an error.
fn task_result(items: &[Item], turn: TurnId) -> (String, bool) {
    items
        .iter()
        .filter(|item| item.turn_id == Some(turn))
        .find_map(|item| match &item.data {
            ItemData::ToolResult {
                name,
                output,
                is_error,
                ..
            } if name == "task" => Some((output.clone(), *is_error)),
            _ => None,
        })
        .expect("the turn has a task result")
}

#[tokio::test]
async fn a_subagent_that_writes_an_answer_returns_it_unchanged() {
    let (directory, runtime) = runtime();
    let thread = echo_thread(&runtime).await;
    let (turn, items) =
        delegate(&runtime, thread.id, &serde_json::json!({"prompt":"answer"})).await;
    let (output, is_error) = task_result(&items, turn);
    assert!(!is_error);
    assert!(
        output.contains("<task_result>\nfirst answer\n</task_result>"),
        "{output}"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_subagent_stopped_at_its_output_limit_says_so_instead_of_an_empty_result() {
    let (directory, runtime) = runtime();
    let thread = echo_thread(&runtime).await;
    let (turn, items) = delegate(&runtime, thread.id, &serde_json::json!({"prompt":"limit"})).await;
    let (output, is_error) = task_result(&items, turn);
    assert!(!is_error);
    assert!(
        output.contains(
            "The subagent stopped at its output limit (4096 output tokens) before writing a reply."
        ),
        "{output}"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_subagent_whose_last_round_hit_the_limit_marks_its_earlier_text() {
    let (directory, runtime) = runtime();
    let thread = echo_thread(&runtime).await;
    let (turn, items) = delegate(
        &runtime,
        thread.id,
        &serde_json::json!({"prompt":"narrate"}),
    )
    .await;
    let (output, _) = task_result(&items, turn);
    assert!(
        output.contains(
            "The subagent stopped at its output limit (4096 output tokens) before it finished. Its last text:\n\nLet me look."
        ),
        "{output}"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_subagent_whose_last_round_wrote_nothing_does_not_pass_off_earlier_text_as_its_answer() {
    let (directory, runtime) = runtime();
    let thread = echo_thread(&runtime).await;
    let (turn, items) = delegate(
        &runtime,
        thread.id,
        &serde_json::json!({"prompt":"narrate then stop"}),
    )
    .await;
    let (output, _) = task_result(&items, turn);
    assert!(
        output.contains(
            "<task_result>\nThe subagent ended without writing a reply (finish reason: stop). Its last text:\n\nLet me look.\n</task_result>"
        ),
        "{output}"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_blank_subagent_reply_reads_as_no_reply() {
    let (directory, runtime) = runtime();
    let thread = echo_thread(&runtime).await;
    let (turn, items) = delegate(&runtime, thread.id, &serde_json::json!({"prompt":"blank"})).await;
    let (output, _) = task_result(&items, turn);
    assert!(
        output.contains("The subagent ended without writing a reply (finish reason: stop)."),
        "{output}"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_resumed_subagent_never_returns_its_previous_runs_reply() {
    let (directory, runtime) = runtime();
    let thread = echo_thread(&runtime).await;
    let (_, items) = delegate(&runtime, thread.id, &serde_json::json!({"prompt":"answer"})).await;
    let child = items
        .iter()
        .find_map(|item| match &item.data {
            ItemData::TaskLink {
                child_thread_id, ..
            } => Some(*child_thread_id),
            _ => None,
        })
        .unwrap();
    let (turn, items) = delegate(
        &runtime,
        thread.id,
        &serde_json::json!({"prompt":"limit","task_id":child}),
    )
    .await;
    let (output, _) = task_result(&items, turn);
    assert!(!output.contains("first answer"), "{output}");
    assert!(output.contains("before writing a reply"), "{output}");
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_subagent_stopped_by_the_safety_check_tells_its_parent_not_to_retry_it() {
    let (directory, runtime) = runtime();
    let thread = echo_thread(&runtime).await;
    let (turn, items) = delegate(&runtime, thread.id, &serde_json::json!({"prompt":"block"})).await;
    let (output, is_error) = task_result(&items, turn);
    assert!(is_error);
    assert!(
        output.contains("The provider's safety check stopped this subagent (refusal).")
            && output.contains("do not retry it as is")
            && output.contains("Partial reply before the stop:\npartial"),
        "{output}"
    );
    // That wording is for a person, who can send a follow-up.
    assert!(!output.contains("Send a follow-up message"), "{output}");
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_background_subagent_stopped_by_the_safety_check_delivers_the_same_advice() {
    let (directory, runtime) = runtime();
    let thread = echo_thread(&runtime).await;
    delegate(
        &runtime,
        thread.id,
        &serde_json::json!({"prompt":"block","background":true}),
    )
    .await;
    let delivered = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let items = runtime.items(thread.id, None).await.unwrap();
            if let Some(text) = items.iter().find_map(|item| match &item.data {
                ItemData::UserMessage {
                    text,
                    synthetic: true,
                    ..
                } if text.contains("state=\"error\"") => Some(text.clone()),
                _ => None,
            }) {
                break text;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(delivered.contains("do not retry it as is"), "{delivered}");
    assert!(
        !delivered.contains("Send a follow-up message"),
        "{delivered}"
    );
    wait_idle(&runtime, thread.id).await;
    std::fs::remove_dir_all(directory).unwrap();
}
