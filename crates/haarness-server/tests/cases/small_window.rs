//! A model with a small context window: a subagent whose read fills it
//! compacts its own chat, and still has the task it was given, word for
//! word, beside the read it made.
use super::*;
use haarness_protocol::ItemData;

const TASK: &str = "Read notes/retries.md and report the setting that bounds the backoff.";

/// A 32K-window model: the parent hands the task to a subagent, which reads
/// a long file and answers.
#[derive(Clone, Default)]
struct SmallWindow {
    requests: Arc<std::sync::Mutex<Vec<ProviderRequest>>>,
    /// The output limit the catalog states, as Cloud's models state one
    /// that fills half the window.
    max_output: Option<u64>,
}

#[async_trait]
impl ModelProvider for SmallWindow {
    fn id(&self) -> &str {
        "echo"
    }

    fn info(&self) -> ProviderInfo {
        let mut info = Echo.info();
        info.models[0].context_window = Some(32_768);
        info.models[0].max_output_tokens = self.max_output;
        info.models[0].tools = true;
        info
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if let Some(title) = title_fixture(&request) {
            return title;
        }
        self.requests.lock().unwrap().push(request.clone());
        let user = |text: &str| {
            request.messages.iter().any(|message| {
                matches!(message, ProviderMessage::Text { role, content } if role == "user" && content == text)
            })
        };
        let answered = |id: &str| {
            request.messages.iter().any(|message| {
                matches!(message, ProviderMessage::ToolResult { call_id, .. } if call_id == id)
            })
        };
        let event = if is_compaction_request(&request) {
            ProviderEvent::TextDelta("The subagent is reading notes about retries.".into())
        } else if user(TASK) && !answered("read-notes") {
            ProviderEvent::ToolCall {
                call_id: "read-notes".into(),
                name: "read".into(),
                arguments: serde_json::json!({"path": "notes/retries.md"}),
                provider_metadata: Value::Null,
            }
        } else if user(TASK) || request.messages.iter().any(|message| {
            matches!(message, ProviderMessage::Text { content, .. } if content.contains("<compaction-summary>"))
        }) {
            ProviderEvent::TextDelta("The backoff is bounded by max_delay.".into())
        } else if !answered("delegate") {
            ProviderEvent::ToolCall {
                call_id: "delegate".into(),
                name: "task".into(),
                arguments: serde_json::json!({"prompt": TASK, "agent": "general", "description": "Read notes"}),
                provider_metadata: Value::Null,
            }
        } else {
            ProviderEvent::TextDelta("The subagent says max_delay bounds it.".into())
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(event),
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }),
        ])))
    }
}

/// A workspace with about 48 KB of notes: more than the part of a 32K
/// window that a compaction keeps.
fn notes_workspace() -> std::path::PathBuf {
    let workspace =
        std::env::temp_dir().join(format!("haarness-small-window-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(workspace.join("notes")).unwrap();
    let notes = (0..1_400)
        .map(|line| format!("- [`retry.backoff_{line}`](#b{line}): `base * 2^n`, see `max_delay`."))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(workspace.join("notes/retries.md"), &notes).unwrap();
    workspace
}

fn small_window_runtime(provider: &SmallWindow, workspace: &std::path::Path) -> Arc<AgentRuntime> {
    Arc::new(AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(workspace.to_path_buf())),
        EventBus::new(512),
        workspace.to_path_buf(),
        String::new(),
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            auto_approve: true,
            ..RuntimeConfig::default()
        },
    ))
}

/// The first request after a compaction.
fn after_compaction(requests: &[ProviderRequest]) -> &ProviderRequest {
    requests
        .iter()
        .filter(|request| !is_compaction_request(request))
        .find(|request| {
            request.messages.iter().any(|message| {
                matches!(message, ProviderMessage::Text { content, .. } if content.contains("<compaction-summary>"))
            })
        })
        .expect("a request after the compaction")
}

#[tokio::test]
async fn a_cloud_model_whose_output_takes_half_its_window_keeps_the_request_beside_a_long_read() {
    let workspace = notes_workspace();
    let provider = SmallWindow {
        max_output: Some(16_384),
        ..SmallWindow::default()
    };
    let requests = provider.requests.clone();
    let runtime = small_window_runtime(&provider, &workspace);
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request(TASK))
        .await
        .unwrap();
    let done = wait_done(&runtime, &turn).await;
    assert_eq!(done.status, TurnStatus::Completed, "{done:?}");
    let (compactions, kept_request, cut_output) = {
        let requests = requests.lock().unwrap();
        let after = after_compaction(&requests);
        (
            requests
                .iter()
                .filter(|request| is_compaction_request(request))
                .count(),
            after.messages.iter().any(|message| {
                matches!(message, ProviderMessage::Text { role, content } if role == "user" && content == TASK)
            }),
            after.messages.iter().any(|message| {
                matches!(message, ProviderMessage::ToolResult { call_id, output, .. }
                    if call_id == "read-notes" && output.contains("characters are left out"))
            }),
        )
    };
    // One compaction: what it kept fits, so the turn does not compact again.
    assert_eq!(compactions, 1);
    assert!(kept_request);
    assert!(cut_output);
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(items.iter().any(|item| matches!(
        &item.data,
        ItemData::AssistantMessage { text } if text == "The backoff is bounded by max_delay."
    )));
    std::fs::remove_dir_all(&workspace).ok();
}

#[tokio::test]
async fn a_subagent_that_fills_a_small_window_compacts_and_keeps_its_task() {
    let workspace = notes_workspace();
    let provider = SmallWindow::default();
    let requests = provider.requests.clone();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider) as Arc<dyn ModelProvider>
        ])),
        Arc::new(NativeToolRegistry::new(workspace.clone())),
        EventBus::new(512),
        workspace.clone(),
        String::new(),
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            auto_approve: true,
            ..RuntimeConfig::default()
        },
    );
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(
            thread.id,
            turn_request("Ask a subagent about the retry notes."),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let child = runtime
        .list_threads(false)
        .await
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.parent_id == Some(thread.id))
        .expect("a subagent ran");
    let items = runtime.items(child.id, None).await.unwrap();
    assert!(
        items
            .iter()
            .any(|item| matches!(item.data, ItemData::Compaction { .. })),
        "the subagent's chat compacted"
    );
    let requests = requests.lock().unwrap();
    let after = after_compaction(&requests);
    assert!(
        after.messages.iter().any(|message| {
            matches!(message, ProviderMessage::Text { role, content } if role == "user" && content == TASK)
        }),
        "{:#?}",
        after.messages
    );
    assert!(after.messages.iter().any(|message| {
        matches!(message, ProviderMessage::ToolResult { call_id, output, .. }
            if call_id == "read-notes" && output.contains("characters are left out"))
    }));
    std::fs::remove_dir_all(&workspace).ok();
}
