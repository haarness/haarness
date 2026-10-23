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
        |item| matches!(&item.data, ItemData::AssistantMessage { text } if text == "partial answer")
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
    assert!(items.iter().any(|item| matches!(&item.data, ItemData::Error { message, retryable: false } if message.contains("safety check stopped this response"))));
    std::fs::remove_dir_all(workspace).unwrap();
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
