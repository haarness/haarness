//! Calls a shutdown cut off end with a result, so the chat goes on.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use futures::stream;
use haarness_core::{
    AgentRuntime, CoreResult, EventBus, ModelProvider, PermissionDecision, PermissionRule,
    ProviderEvent, ProviderMessage, ProviderRequest, ProviderStream, RuntimeConfig,
    StaticProviderRegistry, Tool, ToolContext, ToolDefinition, ToolOutput, ToolRegistry,
};
use haarness_protocol::{
    CreateThreadRequest, ItemData, ModelInfo, ProviderInfo, RequestId, StartTurnRequest, TurnStatus,
};
use haarness_store::SqliteStore;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// Runs until it is let go, whatever Stop says: a tool that outlives the
/// shutdown's wait.
struct Stubborn {
    started: Arc<tokio::sync::Semaphore>,
    release: Arc<tokio::sync::Semaphore>,
}

#[async_trait]
impl Tool for Stubborn {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "stubborn".into(),
            description: "Wait until released".into(),
            input_schema: json!({"type": "object", "additionalProperties": false}),
        }
    }

    fn side_effectful(&self) -> bool {
        false
    }

    async fn execute(&self, _arguments: Value, _context: ToolContext) -> CoreResult<ToolOutput> {
        self.started.add_permits(1);
        self.release.acquire().await.unwrap().forget();
        Ok(ToolOutput {
            text: "released".into(),
            metadata: Value::Null,
        })
    }
}

struct Registry(Arc<Stubborn>);

impl ToolRegistry for Registry {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        (name == "stubborn").then(|| self.0.clone() as Arc<dyn Tool>)
    }

    fn list(&self) -> Vec<ToolDefinition> {
        vec![self.0.definition()]
    }
}

/// Calls the stubborn tool once, then answers; records the messages of the
/// chat's own requests (a title request has no tools).
#[derive(Clone, Default)]
struct Provider {
    requests: Arc<Mutex<Vec<Vec<ProviderMessage>>>>,
}

#[async_trait]
impl ModelProvider for Provider {
    fn id(&self) -> &str {
        "scripted"
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "scripted".into(),
            name: "Scripted".into(),
            authenticated: true,
            models: vec![ModelInfo {
                id: "scripted-1".into(),
                display_name: "Scripted".into(),
                reasoning: false,
                reasoning_efforts: Vec::new(),
                images: false,
                video: false,
                tools: true,
                context_window: Some(128_000),
                max_output_tokens: Some(2_000),
                pricing: None,
            }],
        }
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        let event = if request.tools.is_empty() {
            ProviderEvent::TextDelta("Scripted chat".into())
        } else {
            let first = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(request.messages.clone());
                requests.len() == 1
            };
            if first {
                ProviderEvent::ToolCall {
                    call_id: "call-1".into(),
                    name: "stubborn".into(),
                    arguments: json!({}),
                    provider_metadata: Value::Null,
                }
            } else {
                ProviderEvent::TextDelta("done".into())
            }
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(event),
            Ok(ProviderEvent::Finished {
                reason: "done".into(),
            }),
        ])))
    }
}

fn turn_request(text: &str) -> StartTurnRequest {
    StartTurnRequest {
        request_id: RequestId::new(),
        text: text.into(),
        ..Default::default()
    }
}

/// A turn whose tool outlives the shutdown's wait is marked interrupted,
/// which startup recovery leaves alone: its call gets its result there and
/// then, before the turn's notice, and clients can tell it from a failure.
/// The tool's own result, should it come after all, is not sent a second
/// time.
#[tokio::test]
async fn a_call_a_shutdown_outlived_ends_with_a_result() {
    let tool = Arc::new(Stubborn {
        started: Arc::new(tokio::sync::Semaphore::new(0)),
        release: Arc::new(tokio::sync::Semaphore::new(0)),
    });
    let provider = Provider::default();
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    config.base_rules.push(PermissionRule {
        pattern: "stubborn:*".into(),
        decision: PermissionDecision::Allow,
    });
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(Registry(tool.clone())),
        EventBus::new(256),
        std::env::temp_dir(),
        String::new(),
        config,
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("scripted".into()),
            model: Some("scripted-1".into()),
            ..CreateThreadRequest::default()
        })
        .await
        .unwrap();
    let turn = runtime
        .start_turn(thread.id, turn_request("wait for it"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), tool.started.acquire())
        .await
        .expect("the tool starts")
        .unwrap()
        .forget();

    runtime
        .interrupt_active_turns("Haarness stopped")
        .await
        .unwrap();
    assert_eq!(
        runtime.turn(turn.id).await.unwrap().status,
        TurnStatus::Interrupted
    );
    let items = runtime.items(thread.id, None).await.unwrap();
    let ended = items
        .iter()
        .position(|item| {
            matches!(&item.data, ItemData::ToolResult { call_id, is_error: true, output, metadata, .. }
                if call_id == "call-1" && output == haarness_core::UNFINISHED_CALL_RESULT
                    && metadata["unfinished"] == true)
        })
        .unwrap_or_else(|| panic!("the call ends: {items:?}"));
    let notice = items
        .iter()
        .position(|item| matches!(&item.data, ItemData::Error { .. }))
        .expect("the turn's notice");
    assert!(ended < notice, "{items:?}");

    // The tool returns after all; its runner records its own result.
    tool.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(10), async {
        while runtime.active_turn(thread.id).await.unwrap().is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the stubborn turn ends");
    let next = runtime
        .start_turn(thread.id, turn_request("and now?"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while matches!(
            runtime.turn(next.id).await.unwrap().status,
            TurnStatus::Queued | TurnStatus::Running
        ) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the next turn ends");
    assert_eq!(
        runtime.turn(next.id).await.unwrap().status,
        TurnStatus::Completed
    );
    let sent = provider.requests.lock().unwrap().last().cloned().unwrap();
    let results = sent
        .iter()
        .filter(|message| {
            matches!(message, ProviderMessage::ToolResult { call_id, .. } if call_id == "call-1")
        })
        .count();
    assert_eq!(results, 1, "{sent:?}");
}
