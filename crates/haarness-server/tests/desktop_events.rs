use haarness_core::{AgentRuntime, EmptyToolRegistry, EventBus, StaticProviderRegistry};
use haarness_protocol::{CreateThreadRequest, ItemData, RequestId, StartTurnRequest, TurnStatus};
use haarness_store::SqliteStore;
use std::sync::Arc;

#[tokio::test]
async fn created_and_forked_sessions_are_discoverable_through_event_replay() {
    let runtime = AgentRuntime::new(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    );
    let original = runtime
        .create_thread(CreateThreadRequest::default())
        .await
        .unwrap();
    let fork = runtime
        .fork_thread(original.id, Some("Forked session".into()), None)
        .await
        .unwrap();
    let events = runtime.events().subscribe(None).replay;
    for thread in [original, fork] {
        let created = events
            .iter()
            .filter(|event| {
                event.event_type == "thread.created" && event.thread_id == Some(thread.id)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            created.len(),
            1,
            "every creation must have exactly one observable event"
        );
        assert_eq!(created[0].payload["id"], thread.id.to_string());
        assert_eq!(created[0].payload["title"], thread.title);
    }
}

#[tokio::test]
async fn terminal_failure_is_published_live_before_completion_and_replayed_once() {
    let runtime = AgentRuntime::new(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest::default())
        .await
        .unwrap();
    let mut subscription = runtime.events().subscribe(None);
    let turn = runtime
        .start_turn(
            thread.id,
            StartTurnRequest {
                request_id: RequestId::new(),
                text: "hi".into(),
                attachments: vec![],
                resources: Vec::new(),
                synthetic: false,
                auto_approve: false,
                agent: None,
                provider: None,
                model: None,
                reasoning_effort: None,
                tools: None,
                command: None,
            },
        )
        .await
        .unwrap();
    let live = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let mut events = Vec::new();
        loop {
            let event = subscription.receiver.recv().await.unwrap();
            let finished = event.event_type == "turn.finished";
            events.push(event);
            if finished {
                return events;
            }
        }
    })
    .await
    .expect("failed turn must finish promptly");
    assert_eq!(
        runtime.turn(turn.id).await.unwrap().status,
        TurnStatus::Failed
    );
    let errors: Vec<_> = live
        .iter()
        .filter(|event| {
            event.event_type == "item.created" && event.payload["data"]["type"] == "error"
        })
        .collect();
    assert_eq!(
        errors.len(),
        1,
        "connected clients need the failure without a REST reload"
    );
    let error = errors[0];
    assert_eq!(error.thread_id, Some(thread.id));
    assert_eq!(error.root_thread_id, Some(thread.id));
    assert_eq!(error.turn_id, Some(turn.id));
    assert!(error.event_id < live.last().unwrap().event_id);
    let items = runtime.items(thread.id, None).await.unwrap();
    let item = items
        .iter()
        .find(|item| matches!(item.data, ItemData::Error { .. }))
        .unwrap();
    assert_eq!(error.item_id, Some(item.id));
    assert_eq!(error.payload, serde_json::to_value(item).unwrap());
    let replay = runtime.events().subscribe(None).replay;
    assert_eq!(
        replay
            .iter()
            .filter(|event| event.item_id == Some(item.id))
            .count(),
        1
    );
}
