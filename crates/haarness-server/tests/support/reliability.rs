use super::*;
use haarness_core::{CustomAgent, HookDecision, HookPort};
use haarness_protocol::{Item, ItemData, ThreadId, TurnId};
use std::path::PathBuf;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct Script {
    authenticated: bool,
    rounds: Vec<Vec<ProviderEvent>>,
    calls: AtomicUsize,
    requests: Mutex<Vec<ProviderRequest>>,
}
#[async_trait]
impl ModelProvider for Script {
    fn id(&self) -> &str {
        "echo"
    }
    fn info(&self) -> ProviderInfo {
        let mut info = Echo.info();
        info.models[0].tools = true;
        // A release named the way Claude Code agent files write it.
        let mut claude = info.models[0].clone();
        claude.id = "claude-sonnet-5-5".into();
        info.models.push(claude);
        info.authenticated = self.authenticated;
        info
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        _: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if let Some(title) = title_fixture(&request) {
            return title;
        }
        self.requests.lock().unwrap().push(request);
        let index = self
            .calls
            .fetch_add(1, Ordering::SeqCst)
            .min(self.rounds.len() - 1);
        Ok(Box::pin(stream::iter(
            self.rounds[index].clone().into_iter().map(Ok),
        )))
    }
}
#[derive(Default)]
struct Hooks {
    failures: Mutex<Vec<Value>>,
    compacted: Mutex<Vec<String>>,
    failure_gate: Option<(tokio::sync::Notify, tokio::sync::Notify)>,
}
#[async_trait]
impl HookPort for Hooks {
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
        HookDecision::default()
    }
    async fn stop(&self, _: ThreadId) {}
    async fn post_compact(&self, _: ThreadId, trigger: &str, _: &Item) {
        self.compacted.lock().unwrap().push(trigger.into());
    }
    async fn stop_failure(&self, _: ThreadId, _: TurnId, detail: &Value) {
        self.failures.lock().unwrap().push(detail.clone());
        if let Some((entered, release)) = &self.failure_gate {
            entered.notify_one();
            release.notified().await;
        }
    }
}
fn finish(reason: &str) -> ProviderEvent {
    ProviderEvent::Finished {
        reason: reason.into(),
    }
}
fn text(value: &str) -> ProviderEvent {
    ProviderEvent::TextDelta(value.into())
}
fn usage() -> ProviderEvent {
    ProviderEvent::Usage {
        input_tokens: 12,
        output_tokens: 4,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    }
}
fn setup(
    rounds: Vec<Vec<ProviderEvent>>,
    config: RuntimeConfig,
) -> (PathBuf, AgentRuntime, Arc<Script>, Arc<Hooks>) {
    setup_authenticated(rounds, config, true)
}
fn setup_authenticated(
    rounds: Vec<Vec<ProviderEvent>>,
    config: RuntimeConfig,
    authenticated: bool,
) -> (PathBuf, AgentRuntime, Arc<Script>, Arc<Hooks>) {
    let directory =
        std::env::temp_dir().join(format!("haarness-reliability-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&directory).unwrap();
    let provider = Arc::new(Script {
        authenticated,
        rounds,
        calls: AtomicUsize::new(0),
        requests: Mutex::new(vec![]),
    });
    let hooks = Arc::new(Hooks::default());
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            provider.clone() as Arc<dyn ModelProvider>
        ])),
        Arc::new(NativeToolRegistry::new(directory.clone())),
        EventBus::new(128),
        directory.clone(),
        String::new(),
        config,
    )
    .with_hooks(hooks.clone());
    (directory, runtime, provider, hooks)
}
async fn settled(runtime: &AgentRuntime, turn: &Turn) -> Turn {
    let finished = wait_done(runtime, turn).await;
    wait_idle(runtime, turn.thread_id).await;
    finished
}

#[tokio::test]
async fn reliability_recovers_missing_and_unknown_finishes_preserving_output_and_usage() {
    for reason in [None, Some("unknown"), Some("other")] {
        let mut first = vec![text("partial answer"), usage()];
        if let Some(reason) = reason {
            first.push(finish(reason));
        }
        let (directory, runtime, provider, hooks) = setup(
            vec![
                first,
                vec![text("continued answer"), usage(), finish("completed")],
            ],
            RuntimeConfig::default(),
        );
        let thread = echo_thread(&runtime).await;
        let turn = runtime
            .start_turn(thread.id, turn_request("recover"))
            .await
            .unwrap();
        assert_eq!(settled(&runtime, &turn).await.status, TurnStatus::Completed);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        let items = runtime.items(thread.id, None).await.unwrap();
        assert_eq!(
            items
                .iter()
                .filter(|item| matches!(
                    &item.data,
                    ItemData::Usage {
                        input_tokens: 12,
                        ..
                    }
                ))
                .count(),
            2
        );
        assert!(items.iter().any(|item| matches!(&item.data, ItemData::AssistantMessage {text} if text == "partial answer")));
        assert!(provider.requests.lock().unwrap()[1].messages.iter().any(|message| matches!(message, ProviderMessage::Text {content,..} if content.contains("Continue from the saved partial response"))));
        assert!(hooks.failures.lock().unwrap().is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[tokio::test]
async fn reliability_empty_retry_is_bounded_and_failure_hook_fires_once() {
    let (directory, runtime, provider, hooks) =
        setup(vec![vec![usage()]], RuntimeConfig::default());
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("recover"))
        .await
        .unwrap();
    assert_eq!(settled(&runtime, &turn).await.status, TurnStatus::Failed);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    assert_eq!(hooks.failures.lock().unwrap().len(), 1);
    assert_eq!(hooks.failures.lock().unwrap()[0]["error"], "incomplete");
    let requests = provider.requests.lock().unwrap();
    assert_eq!(
        serde_json::to_value(&requests[0].messages).unwrap(),
        serde_json::to_value(&requests[1].messages).unwrap()
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn reliability_explicit_error_preserves_text_without_retry() {
    let (directory, runtime, provider, hooks) = setup(
        vec![vec![text("partial"), usage(), finish("error")]],
        RuntimeConfig::default(),
    );
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("error"))
        .await
        .unwrap();
    assert_eq!(settled(&runtime, &turn).await.status, TurnStatus::Failed);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(hooks.failures.lock().unwrap().len(), 1);
    assert!(
        runtime.items(thread.id, None).await.unwrap().iter().any(
            |item| matches!(&item.data, ItemData::AssistantMessage {text} if text == "partial")
        )
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn reliability_cancellation_stops_recovery_without_failure_hook() {
    let (directory, runtime, provider, hooks) =
        setup(vec![vec![finish("unknown")]], RuntimeConfig::default());
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("cancel"))
        .await
        .unwrap();
    while provider.calls.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    runtime.interrupt(turn.id).await.unwrap();
    assert_eq!(
        settled(&runtime, &turn).await.status,
        TurnStatus::Interrupted
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert!(hooks.failures.lock().unwrap().is_empty());
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn reliability_incomplete_tool_calls_are_closed_without_execution() {
    let call = ProviderEvent::ToolCall {
        call_id: "unfinished".into(),
        name: "write".into(),
        arguments: serde_json::json!({"path":"must-not-exist","content":"bad"}),
        provider_metadata: Value::Null,
    };
    let (directory, runtime, provider, _) = setup(
        vec![
            vec![call, finish("unknown")],
            vec![text("recovered"), finish("completed")],
        ],
        RuntimeConfig::default(),
    );
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("recover tool"))
        .await
        .unwrap();
    assert_eq!(settled(&runtime, &turn).await.status, TurnStatus::Completed);
    assert!(!directory.join("must-not-exist").exists());
    assert!(provider.requests.lock().unwrap()[1].messages.iter().any(|message| matches!(message, ProviderMessage::ToolResult {call_id,..} if call_id == "unfinished")));
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn reliability_manual_compaction_observes_committed_item_once() {
    let (directory, runtime, _, hooks) = setup(
        vec![vec![text("summary"), finish("completed")]],
        RuntimeConfig::default(),
    );
    let thread = echo_thread(&runtime).await;
    runtime
        .compact(
            thread.id,
            haarness_protocol::CompactRequest {
                summary: Some("summary".into()),
                files_touched: vec![],
                outstanding: vec![],
                todo_checkpoint: None,
                tail_start_sequence: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(*hooks.compacted.lock().unwrap(), vec!["manual"]);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn reliability_configured_missing_model_falls_back_and_reports_in_foreground_result() {
    let call = ProviderEvent::ToolCall {
        call_id: "child".into(),
        name: "task".into(),
        arguments: serde_json::json!({"prompt":"child request","description":"child","agent":"reviewer"}),
        provider_metadata: Value::Null,
    };
    let mut config = RuntimeConfig::default();
    config.custom_agents.push(CustomAgent {
        id: "reviewer".into(),
        mode: Some("subagent".into()),
        model: Some("echo/retired-model".into()),
        ..CustomAgent::default()
    });
    let (directory, runtime, provider, _) = setup(
        vec![
            vec![call, finish("completed")],
            vec![text("child response"), finish("completed")],
        ],
        config,
    );
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("delegate"))
        .await
        .unwrap();
    assert_eq!(settled(&runtime, &turn).await.status, TurnStatus::Completed);
    assert!(
        provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.model == "echo-1")
    );
    assert!(runtime.items(thread.id,None).await.unwrap().iter().any(|item| matches!(&item.data, ItemData::ToolResult {output,..} if output.contains("Configured model") && output.contains("echo/echo-1") && output.contains("child response"))));
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn reliability_failed_summary_preserves_transcript_and_does_not_fire_compaction_hook() {
    for reason in [None, Some("unknown"), Some("error"), Some("content_filter")] {
        let mut summary = vec![text("truncated summary")];
        if let Some(reason) = reason {
            summary.push(finish(reason));
        }
        let (directory, runtime, _, hooks) = setup(
            vec![vec![text("original"), finish("done")], summary],
            RuntimeConfig::default(),
        );
        let thread = echo_thread(&runtime).await;
        let turn = runtime
            .start_turn(thread.id, turn_request("original question"))
            .await
            .unwrap();
        settled(&runtime, &turn).await;
        let before = serde_json::to_value(runtime.items(thread.id, None).await.unwrap()).unwrap();
        assert!(runtime.compact_with_llm(thread.id).await.is_err());
        assert_eq!(
            serde_json::to_value(runtime.items(thread.id, None).await.unwrap()).unwrap(),
            before
        );
        assert!(hooks.compacted.lock().unwrap().is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[tokio::test]
async fn reliability_input_arriving_during_failure_hook_starts_a_fresh_turn() {
    let (directory, runtime, _, _) = setup(
        vec![
            vec![finish("error")],
            vec![text("second answer"), finish("done")],
        ],
        RuntimeConfig::default(),
    );
    let hooks = Arc::new(Hooks {
        failure_gate: Some((tokio::sync::Notify::new(), tokio::sync::Notify::new())),
        ..Hooks::default()
    });
    let runtime = Arc::new(runtime.with_hooks(hooks.clone()));
    let thread = echo_thread(&runtime).await;
    let first = runtime
        .start_turn(thread.id, turn_request("fail first"))
        .await
        .unwrap();
    let (entered, release) = hooks.failure_gate.as_ref().unwrap();
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    let next = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            runtime
                .queue_input(
                    thread.id,
                    haarness_protocol::QueueInputRequest {
                        preview: None,
                        reasoning_effort: None,
                        delivery: haarness_protocol::InputDelivery::Steer,
                        request_id: haarness_protocol::RequestId::new(),
                        text: "follow up during cleanup".into(),
                        attachments: vec![],
                        synthetic: false,
                    },
                )
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!next.is_finished(), "admission waits for failure cleanup");
    release.notify_one();
    let next = tokio::time::timeout(Duration::from_secs(3), next)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let next = next.turn.expect("fresh turn after failure cleanup");
    assert_ne!(first.id, next.id);
    assert_eq!(settled(&runtime, &next).await.status, TurnStatus::Completed);
    assert_eq!(hooks.failures.lock().unwrap().len(), 1);
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(items.iter().any(|item| matches!(&item.data, ItemData::UserMessage {text,..} if text == "follow up during cleanup")));
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn reliability_background_fallback_notice_survives_extension_and_completed_resume() {
    let directory =
        std::env::temp_dir().join(format!("haarness-fallback-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&directory).unwrap();
    let provider = Capturing::default();
    let mut config = RuntimeConfig::default();
    config.custom_agents.push(CustomAgent {
        id: "reviewer".into(),
        mode: Some("subagent".into()),
        model: Some("echo/retired-model".into()),
        ..CustomAgent::default()
    });
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider) as Arc<dyn ModelProvider>
        ])),
        Arc::new(NativeToolRegistry::new(directory.clone())),
        EventBus::new(2048),
        directory.clone(),
        String::new(),
        config,
    );
    let thread = echo_thread(&runtime).await;
    let args = serde_json::json!({"prompt":"!tool bash {\"command\":\"sleep 0.3\"}","agent":"reviewer","background":true});
    let turn = runtime
        .start_turn(thread.id, turn_request(&format!("!tool task {args}")))
        .await
        .unwrap();
    settled(&runtime, &turn).await;
    let child = runtime.background_tasks(thread.id).await.unwrap()[0].child_thread_id;
    let args = serde_json::json!({"prompt":"include followup","task_id":child});
    let turn = runtime
        .start_turn(thread.id, turn_request(&format!("!tool task {args}")))
        .await
        .unwrap();
    settled(&runtime, &turn).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if runtime
                .background_tasks(thread.id)
                .await
                .unwrap()
                .is_empty()
                && runtime.active_turn(thread.id).await.unwrap().is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let items = runtime.items(thread.id, None).await.unwrap();
    let notices = items.iter().filter(|item| matches!(&item.data, ItemData::ToolResult {name,output,..} if name == "task" && output.contains("Configured model"))).count();
    assert_eq!(
        notices, 2,
        "launch and active continuation each disclose substitution"
    );
    assert!(items.iter().any(|item| matches!(&item.data, ItemData::UserMessage {text,synthetic:true,..} if text.contains("Configured model") && text.contains("state=\"completed\""))));
    // A fresh run with an explicit override must not inherit the prior run's warning.
    let args = serde_json::json!({"prompt":"new pass","task_id":child,"model":"echo/echo-1"});
    let turn = runtime
        .start_turn(thread.id, turn_request(&format!("!tool task {args}")))
        .await
        .unwrap();
    assert_eq!(settled(&runtime, &turn).await.status, TurnStatus::Completed);
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(items.iter().filter(|item| item.turn_id == Some(turn.id)).any(|item| matches!(&item.data, ItemData::ToolResult {name,output,is_error:false,..} if name == "task" && !output.contains("Configured model"))));
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn reliability_automatic_and_overflow_compaction_emit_hooks_after_commit() {
    for (provider, provider_id, model) in [
        (
            Arc::new(ModelAwareCompaction::default()) as Arc<dyn ModelProvider>,
            "model-aware",
            "small",
        ),
        (
            Arc::new(OverflowOnce::default()) as Arc<dyn ModelProvider>,
            "overflow",
            "overflow-1",
        ),
    ] {
        let hooks = Arc::new(Hooks::default());
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let runtime = AgentRuntime::new(
            store.clone(),
            Arc::new(StaticProviderRegistry::new(vec![provider])),
            Arc::new(EmptyToolRegistry),
            EventBus::new(128),
        )
        .with_hooks(hooks.clone());
        let thread = runtime
            .create_thread(CreateThreadRequest {
                title: Some("Compaction lifecycle fixture".into()),
                provider: Some(provider_id.into()),
                model: Some(model.into()),
                ..Default::default()
            })
            .await
            .unwrap();
        store
            .append_item(
                thread.id,
                None,
                ItemData::UserMessage {
                    text: "history ".repeat(600),
                    attachments: vec![],
                    queued: false,
                    synthetic: false,
                    command_line: None,
                },
            )
            .await
            .unwrap();
        let turn = runtime
            .start_turn(thread.id, turn_request("continue"))
            .await
            .unwrap();
        assert_eq!(settled(&runtime, &turn).await.status, TurnStatus::Completed);
        let count = runtime
            .items(thread.id, None)
            .await
            .unwrap()
            .iter()
            .filter(|item| matches!(item.data, ItemData::Compaction { .. }))
            .count();
        assert_eq!(count, 1);
        assert_eq!(*hooks.compacted.lock().unwrap(), vec!["automatic"]);
    }
}

#[tokio::test]
async fn reliability_fallback_preserves_explicit_models_and_surfaces_configuration_errors() {
    for (configured, explicit, authenticated, expected) in [
        (
            "missing-provider/model",
            None,
            true,
            Err("unknown provider"),
        ),
        ("echo/retired-model", None, false, Err("not authenticated")),
        ("bad-alias", None, true, Err("Unknown model alias")),
        (
            "missing-provider/model",
            Some("echo/manual-model"),
            true,
            Ok("manual-model"),
        ),
        // Claude Code agent files name a release without its provider, or a family.
        ("claude-sonnet-5-5", None, true, Ok("claude-sonnet-5-5")),
        ("sonnet", None, true, Ok("claude-sonnet-5-5")),
    ] {
        let mut args = serde_json::json!({"prompt":"child","agent":"reviewer"});
        if let Some(model) = explicit {
            args["model"] = serde_json::json!(model);
        }
        let call = ProviderEvent::ToolCall {
            call_id: "child".into(),
            name: "task".into(),
            arguments: args,
            provider_metadata: Value::Null,
        };
        let mut config = RuntimeConfig::default();
        config.custom_agents.push(CustomAgent {
            id: "reviewer".into(),
            mode: Some("subagent".into()),
            model: Some(configured.into()),
            ..CustomAgent::default()
        });
        let (directory, runtime, provider, _) = setup_authenticated(
            vec![
                vec![call, finish("done")],
                vec![text("finished"), finish("done")],
            ],
            config,
            authenticated,
        );
        let thread = echo_thread(&runtime).await;
        let turn = runtime
            .start_turn(thread.id, turn_request("delegate"))
            .await
            .unwrap();
        assert_eq!(settled(&runtime, &turn).await.status, TurnStatus::Completed);
        let items = runtime.items(thread.id, None).await.unwrap();
        let (output, is_error) = items
            .iter()
            .find_map(|item| match &item.data {
                ItemData::ToolResult {
                    name,
                    output,
                    is_error,
                    ..
                } if name == "task" => Some((output, *is_error)),
                _ => None,
            })
            .unwrap();
        assert!(
            !output.contains("Configured model"),
            "{configured}: {output}"
        );
        assert_eq!(is_error, expected.is_err(), "{configured}: {output}");
        match expected {
            Err(expected) => assert!(output.contains(expected), "{output}"),
            Ok(model) => assert!(
                provider
                    .requests
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|request| request.model == model),
                "{configured} should run on {model}"
            ),
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[tokio::test]
async fn reliability_recovery_keeps_completed_tool_results_without_replaying_actions() {
    let call = ProviderEvent::ToolCall {
        call_id: "completed-write".into(),
        name: "bash".into(),
        arguments: serde_json::json!({"command":"printf once >> execution-count.txt"}),
        provider_metadata: Value::Null,
    };
    let (directory, runtime, provider, _) = setup(
        vec![
            vec![call, finish("tool_calls")],
            vec![text("partial after tool"), usage(), finish("incomplete")],
            vec![text("final after recovery"), usage(), finish("done")],
        ],
        RuntimeConfig::default(),
    );
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("perform one action"))
        .await
        .unwrap();
    assert_eq!(settled(&runtime, &turn).await.status, TurnStatus::Completed);
    assert_eq!(
        std::fs::read_to_string(directory.join("execution-count.txt")).unwrap(),
        "once"
    );
    assert!(
        provider.requests.lock().unwrap()[2]
            .messages
            .iter()
            .any(|message| matches!(message,
        ProviderMessage::ToolResult {call_id,is_error:false,..} if call_id == "completed-write"))
    );
    let items = runtime.items(thread.id, None).await.unwrap();
    assert_eq!(
        items
            .iter()
            .filter(|item| matches!(&item.data, ItemData::ToolCall {name,..} if name == "bash"))
            .count(),
        1
    );
    std::fs::remove_dir_all(directory).unwrap();
}
