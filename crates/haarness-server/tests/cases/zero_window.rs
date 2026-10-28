//! A catalog that lists a model's context window as 0 says nothing about the
//! model: compaction and the context meter treat the window as unknown
//! instead of as a limit every request is over.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The echo model, listed with a context window of 0.
#[derive(Default)]
struct ZeroWindow {
    summaries: Arc<AtomicUsize>,
}

#[async_trait]
impl ModelProvider for ZeroWindow {
    fn id(&self) -> &str {
        "echo"
    }
    fn info(&self) -> ProviderInfo {
        let mut info = Echo.info();
        info.models[0].context_window = Some(0);
        info
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if is_compaction_request(&request) {
            self.summaries.fetch_add(1, Ordering::SeqCst);
        }
        Echo.stream(request, cancel).await
    }
}

#[tokio::test]
async fn a_window_listed_as_zero_neither_compacts_every_round_nor_fills_the_meter() {
    let workspace =
        std::env::temp_dir().join(format!("haarness-zero-window-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = ZeroWindow::default();
    let summaries = provider.summaries.clone();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider) as Arc<dyn ModelProvider>
        ])),
        Arc::new(NativeToolRegistry::new(workspace.clone())),
        EventBus::new(256),
        workspace.clone(),
        String::new(),
        RuntimeConfig::default(),
    );
    let thread = echo_thread(&runtime).await.id;
    for text in ["First question", "Second question"] {
        let turn = runtime
            .start_turn(thread, turn_request(text))
            .await
            .unwrap();
        assert_eq!(
            wait_done(&runtime, &turn).await.status,
            TurnStatus::Completed
        );
    }
    assert_eq!(summaries.load(Ordering::SeqCst), 0, "nothing was compacted");
    let usage = runtime.context_usage(thread).await.unwrap();
    assert_eq!(usage.window, None);
    // As for a model that states no window: 128k, less a quarter for output.
    assert_eq!(usage.compact_at, 96_000);
    let _ = std::fs::remove_dir_all(&workspace);
}
