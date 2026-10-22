//! `SessionStart` runs for chats people start, not for the chats a chat
//! starts inside itself.
use super::*;
use haarness_core::{HookDecision, HookPort};
use haarness_protocol::ThreadId;

#[derive(Default)]
struct Starts(std::sync::Mutex<Vec<ThreadId>>);
#[async_trait]
impl HookPort for Starts {
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
    async fn session_start(&self, thread: ThreadId, _agent: &str) {
        self.0.lock().unwrap().push(thread);
    }
}

#[tokio::test]
async fn session_start_runs_for_a_chat_but_not_for_one_started_inside_it() {
    let workspace =
        std::env::temp_dir().join(format!("haarness-session-start-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).unwrap();
    let starts = Arc::new(Starts::default());
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(workspace.clone())),
        EventBus::new(256),
        workspace.clone(),
        String::new(),
        RuntimeConfig::default(),
    )
    .with_hooks(starts.clone());
    let chat = echo_thread(&runtime).await;
    // A workflow's thread and one of its agents, as the workflow host
    // creates them.
    let workflow = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Workflow".into()),
            parent_id: Some(chat.id),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .create_thread(CreateThreadRequest {
            title: Some("Workflow agent".into()),
            parent_id: Some(workflow.id),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(*starts.0.lock().unwrap(), vec![chat.id]);
    let _ = std::fs::remove_dir_all(&workspace);
}
