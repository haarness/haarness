use super::*;
use haarness_protocol::{InputDelivery, ItemData, QueueInputRequest};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Filtered {
    calls: AtomicUsize,
    entered: tokio::sync::Notify,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl ModelProvider for Filtered {
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
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        let release = self.release.clone();
        Ok(Box::pin(async_stream::try_stream! {
            yield ProviderEvent::ReasoningDelta("partial reasoning".into());
            yield ProviderEvent::TextDelta("partial answer".into());
            yield ProviderEvent::ToolCall { call_id: "blocked-call".into(), name: "write".into(),
                arguments: serde_json::json!({"path":"must-not-exist", "content":"blocked"}), provider_metadata: Value::Null };
            release.notified().await;
            yield ProviderEvent::Usage { input_tokens: 12, output_tokens: 4, cache_read_tokens: 0, cache_write_tokens: 0 };
            yield ProviderEvent::Finished { reason: "content_filter".into() };
        }))
    }
}

#[tokio::test]
async fn filtered_turn_preserves_partial_output_and_usage_without_tools_or_queue_retry() {
    let workspace = std::env::temp_dir().join(format!("haarness-filter-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = Arc::new(Filtered {
        calls: AtomicUsize::new(0),
        entered: tokio::sync::Notify::new(),
        release: Arc::new(tokio::sync::Notify::new()),
    });
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            provider.clone() as Arc<dyn ModelProvider>
        ])),
        Arc::new(NativeToolRegistry::new(workspace.clone())),
        EventBus::new(128),
        workspace.clone(),
        String::new(),
        RuntimeConfig::default(),
    );
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("filtered fixture"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
        .await
        .unwrap();
    runtime
        .queue_input(
            thread.id,
            QueueInputRequest {
                preview: None,
                reasoning_effort: None,
                delivery: InputDelivery::Steer,
                request_id: RequestId::new(),
                text: "queued input".into(),
                attachments: vec![],
                resources: Vec::new(),
                synthetic: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    provider.release.notify_one();
    let finished = wait_done(&runtime, &turn).await;
    assert_eq!(finished.status, TurnStatus::Failed);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "content filter triggered automatic retry"
    );
    assert!(!workspace.join("must-not-exist").exists());
    assert!(runtime.active_turns().await.unwrap().is_empty());
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(items.iter().any(
        |item| matches!(&item.data, ItemData::AssistantMessage { text, .. } if text == "partial answer")
    ));
    assert!(items.iter().any(|item| matches!(
        &item.data,
        ItemData::Usage {
            input_tokens: 12,
            output_tokens: 4,
            ..
        }
    )));
    assert!(items.iter().any(|item| matches!(&item.data, ItemData::ToolResult { call_id, is_error: true, .. } if call_id == "blocked-call")));
    assert!(items.iter().any(|item| matches!(&item.data, ItemData::Error { message, retryable: false, turn_left_out: true } if message.starts_with("Stopped by the provider's safety check (content_filter). The model won't see this request again"))));
    std::fs::remove_dir_all(workspace).unwrap();
}

/// Stops any reply to a request whose last message names `TRIP` with
/// Anthropic's `refusal`, after a few words, and answers anything else;
/// records what each request of the chat sent.
#[derive(Default)]
struct RefusesTrips {
    sent: std::sync::Mutex<Vec<Vec<ProviderMessage>>>,
}

#[async_trait]
impl ModelProvider for RefusesTrips {
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
        self.sent.lock().unwrap().push(request.messages.clone());
        let tripped = request
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                ProviderMessage::Text { role, content } if role == "user" => {
                    Some(content.contains("TRIP"))
                }
                _ => None,
            })
            == Some(true);
        let (text, finish) = if tripped {
            ("PARTIAL WORDS", "refusal")
        } else {
            ("REPLY", "end_turn")
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderEvent::TextDelta(text.into())),
            Ok(ProviderEvent::Finished {
                reason: finish.into(),
            }),
        ])))
    }
}

/// The turn a safety stop ended stays in the chat but not in what later
/// requests send: a reworded message goes without the stopped one and the
/// words that streamed before the stop.
#[tokio::test]
async fn a_safety_stop_leaves_its_turn_out_of_the_next_request() {
    let provider = Arc::new(RefusesTrips::default());
    let runtime = AgentRuntime::new(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            provider.clone() as Arc<dyn ModelProvider>
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    );
    let thread = echo_thread(&runtime).await;
    for (text, status) in [
        ("first question", TurnStatus::Completed),
        ("TRIP the check", TurnStatus::Failed),
        ("reworded", TurnStatus::Completed),
    ] {
        let turn = runtime
            .start_turn(thread.id, turn_request(text))
            .await
            .unwrap();
        assert_eq!(wait_done(&runtime, &turn).await.status, status, "{text}");
    }
    let texts = |messages: &[ProviderMessage]| -> Vec<String> {
        messages
            .iter()
            .filter_map(|message| match message {
                ProviderMessage::Text { content, .. }
                    if !haarness_core::is_turn_reminder(content) =>
                {
                    Some(content.clone())
                }
                _ => None,
            })
            .collect()
    };
    let sent = provider.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 3);
    assert_eq!(texts(&sent[2]), ["first question", "REPLY", "reworded"]);
    // The chat itself is as it was.
    let items = runtime.items(thread.id, None).await.unwrap();
    for kept in ["TRIP the check", "PARTIAL WORDS"] {
        assert!(
            items.iter().any(|item| matches!(&item.data,
                ItemData::UserMessage { text, .. } | ItemData::AssistantMessage { text, .. } if text == kept)),
            "{kept}: {items:#?}"
        );
    }
    let stops = items
        .iter()
        .filter_map(|item| match &item.data {
            ItemData::Error {
                message,
                turn_left_out,
                ..
            } => Some((message.as_str(), *turn_left_out)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        stops,
        [(
            "Stopped by the provider's safety check (refusal). The model won't see this request again, so you can edit it or pick another model.",
            true
        )]
    );
}

#[test]
fn provider_finish_reasons_separate_policy_stops_from_ordinary_completion() {
    for reason in [
        "content_filter",
        "content-filter",
        "SAFETY",
        "RECITATION",
        "PROHIBITED_CONTENT",
        "SPII",
        "refusal",
        "content_filtered",
        "guardrail_intervened",
    ] {
        assert!(matches!(
            ProviderEvent::finish_error(reason),
            Some(CoreError::ProviderBlocked(_))
        ));
    }
    for reason in [
        "stop",
        "length",
        "tool_calls",
        "completed",
        "MAX_TOKENS",
        "STOP",
        "end_turn",
        "tool_use",
        "max_tokens",
    ] {
        assert!(ProviderEvent::finish_error(reason).is_none());
    }
}
