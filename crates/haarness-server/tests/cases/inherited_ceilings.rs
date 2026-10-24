//! Sessions of one agent send one tool list whichever chat started them; a
//! tool the parent denies is refused when called, naming the parent's rule.
use super::*;
use haarness_protocol::ItemData;
use serde_json::json;

/// The first request a child agent sent: the one whose only user message is
/// its task prompt.
fn first_request(requests: &[ProviderRequest], prompt: &str) -> ProviderRequest {
    requests
        .iter()
        .find(|request| {
            request.messages.iter().any(|message| {
                matches!(message, ProviderMessage::Text { role, content }
                    if role == "user" && content.starts_with(prompt))
            })
        })
        .cloned()
        .unwrap_or_else(|| panic!("no request for {prompt}"))
}

#[tokio::test]
async fn children_of_a_plan_chat_and_a_build_chat_send_one_tool_list() {
    const PLAN_CHILD: &str = r#"!tool image_generate {"prompt":"a lighthouse"}"#;
    const BUILD_CHILD: &str = "build child: summarize the project";
    let workspace =
        std::env::temp_dir().join(format!("haarness-inherited-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = Arc::new(Capturing::default());
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            provider.clone() as Arc<dyn ModelProvider>
        ])),
        Arc::new(NativeToolRegistry::new(workspace.clone())),
        EventBus::new(256),
        workspace.clone(),
        String::new(),
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            ..RuntimeConfig::default()
        },
    );
    let mut children = Vec::new();
    for (agent, prompt) in [("plan", PLAN_CHILD), ("build", BUILD_CHILD)] {
        let chat = runtime
            .create_thread(CreateThreadRequest {
                title: Some(format!("{agent} chat")),
                agent: Some(agent.into()),
                provider: Some("echo".into()),
                model: Some("echo-1".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let task = json!({"prompt": prompt, "agent": "general", "description": "look"});
        let turn = runtime
            .start_turn(chat.id, turn_request(&format!("!tool task {task}")))
            .await
            .unwrap();
        assert_eq!(
            wait_done(&runtime, &turn).await.status,
            TurnStatus::Completed
        );
        let child = runtime
            .thread_tree_ids(chat.id)
            .await
            .unwrap()
            .into_iter()
            .find(|id| *id != chat.id)
            .expect("the task started a child");
        children.push(child);
    }
    let requests = provider.requests.lock().unwrap().clone();
    let under_plan = first_request(&requests, PLAN_CHILD);
    let under_build = first_request(&requests, BUILD_CHILD);
    let names = |request: &ProviderRequest| {
        request
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>()
    };
    // The plan chat's ceiling denies generated media, but its child still
    // lists the tool: both children share one cached prefix.
    assert!(names(&under_plan).contains(&"image_generate".to_owned()));
    assert_eq!(names(&under_plan), names(&under_build));
    assert_eq!(under_plan.tools, under_build.tools);
    assert_eq!(under_plan.system, under_build.system);
    // Calling it is refused with the plan chat's rule, and nothing ran.
    let refused = runtime.items(children[0], None).await.unwrap();
    assert!(
        refused.iter().any(|item| matches!(&item.data,
            ItemData::ToolResult { output, is_error: true, .. }
                if output.contains("tool image_generate is denied by the rule 'media_generate:*' of the chat or agent that started this one"))),
        "{refused:?}"
    );
    assert!(runtime.pending_permissions().is_empty());
    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn the_plan_agent_started_from_a_build_chat_still_cannot_write() {
    // The build chat's boundary ends with its `*` allow; it must not reopen
    // the plan agent's own write ceiling.
    let workspace =
        std::env::temp_dir().join(format!("haarness-plan-child-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(workspace.clone())),
        EventBus::new(256),
        workspace.clone(),
        String::new(),
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            ..RuntimeConfig::default()
        },
    );
    let chat = echo_thread(&runtime).await;
    let write = json!({"path": "notes.txt", "content": "planned"});
    let task =
        json!({"prompt": format!("!tool write {write}"), "agent": "plan", "description": "plan"});
    let turn = runtime
        .start_turn(chat.id, turn_request(&format!("!tool task {task}")))
        .await
        .unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let child = runtime
        .thread_tree_ids(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|id| *id != chat.id)
        .expect("the task started a child");
    assert!(
        runtime
            .items(child, None)
            .await
            .unwrap()
            .iter()
            .any(|item| matches!(&item.data,
                ItemData::ToolResult { output, is_error: true, .. } if output.contains("denied by permission rule 'edit:*'"))),
    );
    assert!(!workspace.join("notes.txt").exists());
    let _ = std::fs::remove_dir_all(&workspace);
}
