//! A compaction someone asked for is the server's own work once it starts:
//! a request that goes away (a proxy's time limit, a page left) does not
//! stop the summary, and a second one for the same chat is refused.
use super::*;
use haarness_protocol::{Item, ItemData, ThreadId};

const TOKEN: &str = "compaction-task-token";

struct Fixture {
    provider: CompactionQueueRace,
    runtime: Arc<AgentRuntime>,
    handle: haarness_server::ServerHandle,
    thread: ThreadId,
}

impl Fixture {
    async fn start() -> Self {
        let provider = CompactionQueueRace::default();
        let runtime = Arc::new(AgentRuntime::with_config(
            Arc::new(SqliteStore::in_memory().unwrap()),
            Arc::new(StaticProviderRegistry::new(vec![
                Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
            ])),
            Arc::new(EmptyToolRegistry),
            EventBus::new(256),
            std::env::temp_dir(),
            String::new(),
            haarness_core::RuntimeConfig {
                approval_mode: haarness_protocol::ApprovalMode::Review,
                compaction: haarness_core::CompactionSettings {
                    enabled: false,
                    ..Default::default()
                },
                ..Default::default()
            },
        ));
        let thread = runtime
            .create_thread(CreateThreadRequest {
                title: Some("Compaction task".into()),
                provider: Some("echo".into()),
                model: Some("echo-1".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let seed = runtime
            .start_turn(thread.id, turn_request("seed history"))
            .await
            .unwrap();
        assert_eq!(
            wait_done(&runtime, &seed).await.status,
            TurnStatus::Completed
        );
        let handle = serve(
            TcpListener::bind("127.0.0.1:0").await.unwrap(),
            TOKEN,
            ServerState {
                browser: None,
                management: None,
                runtime: Arc::clone(&runtime),
                mcp: Arc::new(NoMcp),
                skills: Arc::new(NoSkills),
                plugins: None,
                workflows: None,
                workspaces: None,
                workspace_id: None,
                workspace: std::env::temp_dir(),
                project_trusted: true,
                language: None,
            },
        )
        .await
        .unwrap();
        Self {
            provider,
            runtime,
            handle,
            thread: thread.id,
        }
    }

    fn post_compact(&self, body: serde_json::Value) -> tokio::task::JoinHandle<reqwest::Response> {
        let url = format!(
            "http://{}/v1/threads/{}/compact",
            self.handle.address, self.thread
        );
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(url)
                .bearer_auth(TOKEN)
                .json(&body)
                .send()
                .await
                .unwrap()
        })
    }

    async fn summary_started(&self) {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.provider.summary_started.acquire(),
        )
        .await
        .expect("the summary started")
        .unwrap()
        .forget();
    }

    fn summaries_asked(&self) -> usize {
        self.provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| is_compaction_request(request))
            .count()
    }

    async fn compactions(&self) -> Vec<Item> {
        self.runtime
            .items(self.thread, None)
            .await
            .unwrap()
            .into_iter()
            .filter(|item| matches!(item.data, ItemData::Compaction { .. }))
            .collect()
    }

    fn compacted_events(&self) -> usize {
        self.runtime
            .events()
            .subscribe(None)
            .replay
            .iter()
            .filter(|event| {
                event.event_type == "thread.compacted" && event.thread_id == Some(self.thread)
            })
            .count()
    }
}

async fn error_message(response: reqwest::Response) -> String {
    let body: Value = response.json().await.unwrap();
    body["message"].as_str().unwrap_or_default().to_owned()
}

#[tokio::test]
async fn a_compaction_whose_request_was_dropped_still_lands() {
    let fixture = Fixture::start().await;
    let request = fixture.post_compact(serde_json::json!({}));
    fixture.summary_started().await;

    // The caller goes away while the model is still summarizing, as when
    // a proxy cuts a long request or the page that sent it is closed.
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    tokio::time::sleep(Duration::from_millis(300)).await;
    fixture.provider.release_summary.add_permits(1);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while fixture.compactions().await.is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the summary of a dropped request never landed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let compactions = fixture.compactions().await;
    assert_eq!(compactions.len(), 1);
    assert!(matches!(
        &compactions[0].data,
        ItemData::Compaction { summary, .. } if summary.contains("lossy summary")
    ));
    assert_eq!(fixture.compacted_events(), 1, "the compaction is announced");
    assert_eq!(fixture.summaries_asked(), 1);
    fixture.handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_second_compaction_is_refused_while_the_first_summarizes() {
    let fixture = Fixture::start().await;
    let first = fixture.post_compact(serde_json::json!({}));
    fixture.summary_started().await;

    // Asked again, as a page that lost its request might: refused in plain
    // words, without a second summary or a caller's summary under it.
    for body in [
        serde_json::json!({}),
        serde_json::json!({"summary": "caller summary"}),
    ] {
        let again = tokio::time::timeout(Duration::from_secs(5), fixture.post_compact(body))
            .await
            .expect("answered at once instead of summarizing again")
            .unwrap();
        assert_eq!(again.status(), reqwest::StatusCode::CONFLICT);
        assert!(
            error_message(again)
                .await
                .contains("already being compacted"),
        );
    }
    assert_eq!(fixture.summaries_asked(), 1, "one summary, not two");

    fixture.provider.release_summary.add_permits(1);
    let first = first.await.unwrap();
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    assert_eq!(fixture.compactions().await.len(), 1);

    // Once it has landed the chat can be compacted again.
    let next = fixture.post_compact(serde_json::json!({}));
    fixture.summary_started().await;
    fixture.provider.release_summary.add_permits(1);
    assert_eq!(next.await.unwrap().status(), reqwest::StatusCode::OK);
    assert_eq!(fixture.compactions().await.len(), 2);
    assert_eq!(fixture.compacted_events(), 2);
    fixture.handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_failed_summary_lets_the_chat_be_compacted_again() {
    let fixture = Fixture::start().await;
    let first = fixture.post_compact(serde_json::json!({}));
    fixture.summary_started().await;
    // The summary fails: the provider's stream ends in an error.
    fixture.provider.release_summary.close();
    let first = first.await.unwrap();
    assert_eq!(first.status(), reqwest::StatusCode::BAD_GATEWAY);
    assert!(
        error_message(first)
            .await
            .contains("compaction fixture closed")
    );

    // Asked again, the chat is not held by the failed one.
    let again = fixture.post_compact(serde_json::json!({})).await.unwrap();
    assert_eq!(again.status(), reqwest::StatusCode::BAD_GATEWAY);
    assert!(
        error_message(again)
            .await
            .contains("compaction fixture closed")
    );
    assert_eq!(fixture.summaries_asked(), 2);
    assert!(fixture.compactions().await.is_empty());
    fixture.handle.shutdown().await.unwrap();
}

/// The chat can be compacted again: a new summary is asked for and lands.
async fn compacts_again(fixture: &Fixture) {
    let next = fixture.post_compact(serde_json::json!({}));
    fixture.summary_started().await;
    fixture.provider.release_summary.add_permits(1);
    assert_eq!(next.await.unwrap().status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn a_turn_started_during_the_summary_ends_it_at_once() {
    let fixture = Fixture::start().await;
    let request = fixture.post_compact(serde_json::json!({}));
    fixture.summary_started().await;

    // A message from another client: the summary could never be committed
    // over its turn, so it ends now instead of running on.
    let turn = fixture
        .runtime
        .start_turn(fixture.thread, turn_request("a message from the phone"))
        .await
        .unwrap();
    let answer = tokio::time::timeout(Duration::from_secs(5), request)
        .await
        .expect("the summary ended without being released")
        .unwrap();
    assert_eq!(answer.status(), reqwest::StatusCode::CONFLICT);
    assert!(error_message(answer).await.contains("thread changed"));
    assert_eq!(
        wait_done(&fixture.runtime, &turn).await.status,
        TurnStatus::Completed
    );
    assert!(fixture.compactions().await.is_empty());

    compacts_again(&fixture).await;
    assert_eq!(fixture.compactions().await.len(), 1);
    fixture.handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn stop_on_the_idle_chat_ends_the_summary() {
    let fixture = Fixture::start().await;
    let request = fixture.post_compact(serde_json::json!({}));
    fixture.summary_started().await;

    let stopped = reqwest::Client::new()
        .post(format!(
            "http://{}/v1/threads/{}/interrupt",
            fixture.handle.address, fixture.thread
        ))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert!(stopped.status().is_success());
    let answer = tokio::time::timeout(Duration::from_secs(5), request)
        .await
        .expect("Stop did not end the summary")
        .unwrap();
    assert_eq!(answer.status(), reqwest::StatusCode::CONFLICT);
    // Said as what happened, not as a bare cancellation.
    assert!(
        error_message(answer)
            .await
            .contains("compaction stopped before its summary was written")
    );
    assert!(fixture.compactions().await.is_empty());

    compacts_again(&fixture).await;
    fixture.handle.shutdown().await.unwrap();
}
