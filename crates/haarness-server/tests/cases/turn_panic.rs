//! A panic inside a turn fails that turn instead of leaving the chat busy.
use super::*;
use haarness_protocol::ItemData;
use serde_json::json;

/// A tool whose code panics, as a bug would.
struct Panics;
#[async_trait]
impl Tool for Panics {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "panic_fixture".into(),
            description: "Fixture whose code panics".into(),
            input_schema: json!({"type":"object","properties":{}}),
        }
    }
    fn side_effectful(&self) -> bool {
        false
    }
    async fn execute(&self, _args: Value, _context: ToolContext) -> CoreResult<ToolOutput> {
        panic!("fixture bug in panic_fixture");
    }
}
struct WithPanics(NativeToolRegistry);
impl ToolRegistry for WithPanics {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if name == "panic_fixture" {
            Some(Arc::new(Panics))
        } else {
            self.0.get(name)
        }
    }
    fn list(&self) -> Vec<ToolDefinition> {
        let mut tools = self.0.list();
        tools.push(Panics.definition());
        tools
    }
}

#[tokio::test]
async fn a_panic_in_a_turn_fails_that_turn_and_the_chat_takes_the_next_message() {
    let workspace =
        std::env::temp_dir().join(format!("haarness-turn-panic-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(WithPanics(NativeToolRegistry::new(workspace.clone()))),
        EventBus::new(256),
        workspace.clone(),
        String::new(),
        RuntimeConfig::default(),
    );
    let thread = echo_thread(&runtime).await;
    let turn = runtime
        .start_turn(thread.id, turn_request("!tool panic_fixture {}"))
        .await
        .unwrap();
    let failed = wait_done(&runtime, &turn).await;
    assert_eq!(failed.status, TurnStatus::Failed);
    let error = failed.error.unwrap_or_default();
    assert!(
        error.contains("internal error (fixture bug in panic_fixture)"),
        "{error}"
    );
    // The chat is free: the next message runs as a turn of its own.
    let next = runtime
        .start_turn(thread.id, turn_request("after the panic"))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &next).await.status,
        TurnStatus::Completed
    );
    assert!(runtime.items(thread.id, None).await.unwrap().iter().any(
        |item| matches!(&item.data, ItemData::AssistantMessage { text, .. }
                if text.contains("after the panic"))
    ));
    let _ = std::fs::remove_dir_all(&workspace);
}
