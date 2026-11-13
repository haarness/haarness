use chrono::{Duration, Utc};
use haarness_core::ThreadRepository;
use haarness_insights::{Options, collect, collect_at, render_html, write_report};
use haarness_protocol::{CreateThreadRequest, ItemData, RequestId, TurnStatus};
use haarness_store::SqliteStore;
use serde_json::json;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("haarness-insights-test-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("workspaces.json"),
            json!({"projects":[
            {"id":"p1","name":"Alpha <script>alert(1)</script>","root":"/alpha"},
            {"id":"p2","name":"Beta","root":"/beta"}],"workspaces":[
            {"id":"w1","projectId":"p1","path":"/alpha"},
            {"id":"w2","projectId":"p2","path":"/beta"},
            {"id":"w3","projectId":"p1","path":"/alpha-worktree"}]})
            .to_string(),
        )
        .unwrap();
        Self(path)
    }
    fn store(&self, id: &str) -> SqliteStore {
        SqliteStore::open(self.0.join(format!("workspaces/{id}/haarness.db"))).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn turn(
    store: &SqliteStore,
    model: &str,
    input: u64,
    cost: Option<f64>,
) -> haarness_protocol::Thread {
    let thread = store
        .create_thread(CreateThreadRequest {
            title: Some("Private title".into()),
            provider: Some("test".into()),
            model: Some("old-model".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let (turn, _, _) = store
        .start_turn(
            thread.id,
            serde_json::from_value(
                json!({"requestId": RequestId::new(), "text":"SECRET PROMPT", "model":model}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    store
        .append_item(
            thread.id,
            Some(turn.id),
            ItemData::Usage {
                prompt: None,
                input_tokens: input,
                output_tokens: 10,
                cache_read_tokens: 30,
                cache_write_tokens: 5,
                cost_usd: cost,
                estimated: false,
                first_token_ms: None,
                stream_ms: None,
                stream_tokens: None,
            },
        )
        .await
        .unwrap();
    store
        .append_item(
            thread.id,
            Some(turn.id),
            ItemData::ToolCall {
                call_id: "call".into(),
                name: "edit".into(),
                arguments: json!({"secret":"DO NOT EXPORT"}),
                provider_metadata: json!(null),
            },
        )
        .await
        .unwrap();
    store.append_item(thread.id, Some(turn.id), ItemData::ToolResult { call_id: "call".into(), name: "edit".into(), output: "SECRET OUTPUT".into(), is_error: true, metadata: json!({"diffs":[{"file":"src/<bad>.rs","additions":4,"deletions":2,"before":"SECRET FILE"}]}) }).await.unwrap();
    store
        .set_turn_status(turn.id, TurnStatus::Completed, None)
        .await
        .unwrap();
    thread
}

#[tokio::test]
async fn aggregates_worktrees_models_imports_forks_deleted_usage_and_safe_reports() {
    let fixture = Fixture::new();
    let a = fixture.store("w1");
    let b = fixture.store("w2");
    let c = fixture.store("w3");
    let thread = turn(&a, "model-a", 100, Some(0.1)).await;
    a.fork_thread(thread.id, None, None).await.unwrap();
    turn(&b, "model-b", 200, None).await;
    turn(&c, "model-a", 50, Some(0.05)).await;
    let worktree_db =
        rusqlite::Connection::open(fixture.0.join("workspaces/w3/haarness.db")).unwrap();
    worktree_db.execute("UPDATE items SET data_json=json_set(data_json,'$.metadata.diffs[0].file','/alpha-worktree/src/<bad>.rs') WHERE json_extract(data_json,'$.type')='tool_result'", []).unwrap();
    // Import preserves IDs and must not duplicate billed activity across databases.
    let archive = a.export_session_archive(thread.id, false).await.unwrap();
    b.import_session_archive(archive).await.unwrap();
    // Undo removes transcript visibility, not expenditure.
    let db = rusqlite::Connection::open(fixture.0.join("workspaces/w1/haarness.db")).unwrap();
    db.execute(
        "UPDATE items SET deleted=1 WHERE json_extract(data_json,'$.type')='usage'",
        [],
    )
    .unwrap();
    let registry_before = std::fs::read(fixture.0.join("workspaces.json")).unwrap();
    let report = collect(
        &fixture.0,
        std::path::Path::new("/alpha"),
        Options::default(),
    )
    .unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.total.turns, 3);
    assert_eq!(report.total.sessions, 3);
    assert_eq!(report.total.input_tokens, 350);
    assert_eq!(report.total.output_tokens, 30);
    assert_eq!(report.total.cache_read_tokens, 90);
    assert_eq!(report.total.tokens(), 380);
    assert!((report.total.known_cost_usd - 0.15).abs() < 0.00001);
    assert_eq!(report.total.unpriced_usage_records, 1);
    assert_eq!(report.duplicate_turns_skipped, 1);
    assert!(report.detached_items_skipped > 0);
    assert_eq!(report.projects.len(), 2);
    assert_eq!(report.models.len(), 2);
    assert!(report.models.iter().all(|m| m.name != "test/old-model"));
    assert_eq!(report.total.tool_errors, 3);
    assert_eq!(report.total.additions, 12);
    assert_eq!(report.total.file_changes, 3);
    assert_eq!(
        report.files.len(),
        2,
        "same project-relative file groups across workspaces"
    );
    assert_eq!(
        std::fs::read(fixture.0.join("workspaces.json")).unwrap(),
        registry_before
    );
    let html = render_html(&report);
    assert!(html.contains("&lt;script&gt;"));
    assert!(!html.contains("<script>"));
    for secret in [
        "SECRET PROMPT",
        "DO NOT EXPORT",
        "SECRET OUTPUT",
        "SECRET FILE",
    ] {
        assert!(!html.contains(secret));
    }
    assert!(!html.contains("{{"));
    assert!(html.contains("Cost coverage is incomplete"));
    let first = write_report(&fixture.0, &html).unwrap();
    let second = write_report(&fixture.0, &html).unwrap();
    assert_ne!(first, second);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(first).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let filtered = collect(
        &fixture.0,
        std::path::Path::new("/alpha"),
        Options {
            current_project: true,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(filtered.total.turns, 2);
    assert_eq!(filtered.total.input_tokens, 150);
    let from_subdirectory = collect(
        &fixture.0,
        std::path::Path::new("/alpha/src"),
        Options {
            current_project: true,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(from_subdirectory.total.input_tokens, 150);
    let limited = collect(
        &fixture.0,
        std::path::Path::new("/"),
        Options {
            limit: Some(1),
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(limited.total.sessions, 1);
    assert_eq!(limited.matching_sessions, 3);
}

#[tokio::test]
async fn dates_unknown_cost_empty_missing_and_corrupt_databases_are_explicit() {
    let fixture = Fixture::new();
    let a = fixture.store("w1");
    turn(&a, "old", 100, None).await;
    let now = Utc::now();
    let db = rusqlite::Connection::open(fixture.0.join("workspaces/w1/haarness.db")).unwrap();
    db.execute(
        "UPDATE turns SET created_at=?1",
        [(now - Duration::days(8)).to_rfc3339()],
    )
    .unwrap();
    let report = collect_at(
        &fixture.0,
        std::path::Path::new("/"),
        Options {
            days: Some(7),
            ..Options::default()
        },
        now,
    )
    .unwrap();
    assert_eq!(report.total.turns, 0);
    assert!(report.findings[0].contains("No recorded turns"));
    assert!(
        collect(
            &fixture.0,
            std::path::Path::new("/"),
            Options {
                days: Some(0),
                ..Options::default()
            }
        )
        .is_err()
    );
    assert!(
        collect(
            &fixture.0,
            std::path::Path::new("/"),
            Options {
                project: Some("missing".into()),
                ..Options::default()
            }
        )
        .is_err()
    );
    std::fs::create_dir_all(fixture.0.join("workspaces/corrupt")).unwrap();
    std::fs::write(
        fixture.0.join("workspaces/corrupt/haarness.db"),
        "bad sqlite",
    )
    .unwrap();
    let report = collect(&fixture.0, std::path::Path::new("/"), Options::default()).unwrap();
    assert_eq!(report.warnings.len(), 1);
    assert_eq!(report.total.unpriced_usage_records, 1);
    assert!(report.total.known_cost_usd.abs() < f64::EPSILON);
}

#[tokio::test]
async fn daily_usage_groups_days_models_tools_and_subagents_without_names() {
    let fixture = Fixture::new();
    let alpha = fixture.store("w1");
    let parent = turn(&alpha, "model-a", 100, Some(0.5)).await;
    turn(&alpha, "model-a", 50, None).await;
    turn(&fixture.store("w2"), "model-b", 7, Some(0.25)).await;
    // A subagent's turn: its thread has the chat as parent.
    let child = alpha
        .create_thread(CreateThreadRequest {
            title: Some("Child title".into()),
            parent_id: Some(parent.id),
            provider: Some("test".into()),
            model: Some("model-a".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    alpha
        .start_turn(
            child.id,
            serde_json::from_value(json!({"requestId": RequestId::new(), "text":"child"})).unwrap(),
        )
        .await
        .unwrap();

    let days = haarness_insights::daily_usage(&fixture.0, 7, Utc::now()).unwrap();
    assert_eq!(days.len(), 1, "everything happened today");
    let today = &days[0];
    assert_eq!(today.day, Utc::now().format("%Y-%m-%d").to_string());
    assert_eq!(today.total.turns, 4);
    assert_eq!(today.total.input_tokens, 157);
    assert_eq!(today.subagent_turns, 1);
    assert_eq!(today.scheduled_turns, 0);
    let model_a = today
        .models
        .iter()
        .find(|row| row.model == "model-a")
        .unwrap();
    assert_eq!(
        (model_a.provider.as_str(), model_a.metrics.turns),
        ("test", 3)
    );
    assert!(!today.tools.is_empty());
    let serialized = serde_json::to_string(&days).unwrap();
    for private in [
        "Private title",
        "Child title",
        "SECRET PROMPT",
        "Alpha",
        "/alpha",
    ] {
        assert!(!serialized.contains(private), "{private} leaked");
    }
    // Older days fall outside the window.
    assert!(
        haarness_insights::daily_usage(&fixture.0, 1, Utc::now() + Duration::days(2))
            .unwrap()
            .is_empty()
    );
}

/// A subagent's turn ends inside the turn that started it. Counted as a turn
/// of its own, a subagent the chat recovered from read as a failed turn, and
/// one that failed with its chat counted that failure twice; its thread read
/// as another chat.
#[tokio::test]
async fn a_subagent_s_outcome_and_thread_count_within_the_chat_that_started_it() {
    // `status` is the turn's outcome; the usage is what its requests spent.
    async fn run(
        store: &SqliteStore,
        thread: haarness_protocol::ThreadId,
        status: Option<TurnStatus>,
    ) {
        let (turn, _, _) = store
            .start_turn(
                thread,
                serde_json::from_value(json!({"requestId": RequestId::new(), "text":"work"}))
                    .unwrap(),
            )
            .await
            .unwrap();
        store
            .append_item(
                thread,
                Some(turn.id),
                ItemData::Usage {
                    prompt: None,
                    input_tokens: 100,
                    output_tokens: 10,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    cost_usd: Some(0.01),
                    estimated: false,
                    first_token_ms: None,
                    stream_ms: None,
                    stream_tokens: None,
                },
            )
            .await
            .unwrap();
        if let Some(status) = status {
            store.set_turn_status(turn.id, status, None).await.unwrap();
        }
    }
    let fixture = Fixture::new();
    let store = fixture.store("w1");
    let thread = |parent: Option<haarness_protocol::ThreadId>| CreateThreadRequest {
        parent_id: parent,
        provider: Some("test".into()),
        model: Some("model-a".into()),
        ..Default::default()
    };
    // Chat one: the person's turn completed after its subagent failed, and a
    // subagent of a subagent was stopped.
    let recovered = store.create_thread(thread(None)).await.unwrap();
    run(&store, recovered.id, Some(TurnStatus::Completed)).await;
    let child = store
        .create_thread(thread(Some(recovered.id)))
        .await
        .unwrap();
    run(&store, child.id, Some(TurnStatus::Failed)).await;
    let grandchild = store.create_thread(thread(Some(child.id))).await.unwrap();
    run(&store, grandchild.id, Some(TurnStatus::Interrupted)).await;
    // Chat two: the credits ran out in the subagent, then in the person's
    // turn that started it.
    let failed = store.create_thread(thread(None)).await.unwrap();
    run(&store, failed.id, Some(TurnStatus::Failed)).await;
    let failed_child = store.create_thread(thread(Some(failed.id))).await.unwrap();
    run(&store, failed_child.id, Some(TurnStatus::Failed)).await;
    // A subagent still running counts nothing as running either.
    let running_child = store.create_thread(thread(Some(failed.id))).await.unwrap();
    run(&store, running_child.id, None).await;
    // Chat three: a fork names chat one as its parent, but is a chat of its
    // own whose turns are the person's.
    let fork = store
        .fork_thread(recovered.id, Some("Fork".into()), None)
        .await
        .unwrap();
    run(&store, fork.id, Some(TurnStatus::Failed)).await;

    let days = haarness_insights::daily_usage(&fixture.0, 7, Utc::now()).unwrap();
    let today = &days[0];
    assert_eq!(
        today.total.sessions, 3,
        "three chats, whatever their subagents"
    );
    assert_eq!(today.total.turns, 7, "every turn's work still counts");
    assert_eq!(today.subagent_turns, 4);
    assert_eq!(today.total.input_tokens, 700);
    assert_eq!(
        (
            today.total.completed,
            today.total.failed,
            today.total.interrupted,
            today.total.in_progress
        ),
        (1, 2, 0, 0),
        "outcomes of the three turns people started"
    );
    let model = &today.models[0];
    assert_eq!(model.metrics.turns, 7);
    assert_eq!(model.metrics.failed, 2);
    assert_eq!(
        today
            .failures
            .iter()
            .map(|row| (row.kind, row.provider.as_str(), row.model.as_str()))
            .collect::<Vec<_>>(),
        [(haarness_insights::FailureKind::Other, "test", "model-a"); 2],
        "only the person's failed turns, which stored no words"
    );

    // `haarness stats` counts outcomes the same way and says so.
    let report = collect_at(
        &fixture.0,
        std::path::Path::new("/alpha"),
        Options::default(),
        Utc::now(),
    )
    .unwrap();
    assert_eq!((report.total.completed, report.total.failed), (1, 2));
    assert_eq!(report.total.turns, 7);
    let outcomes = report
        .findings
        .iter()
        .find(|line| line.starts_with("Turn outcomes"))
        .unwrap();
    assert!(
        outcomes.starts_with("Turn outcomes: 1 completed, 2 failed, 0 interrupted"),
        "{outcomes}"
    );
    assert!(outcomes.contains("a subagent's turn is part of the one that started it"));
}

/// Each failed turn of the day is there with why it failed (read from the
/// words it stored), those words, the version that ran it and how far it got;
/// the day's tool errors and its turns by version are there too.
#[tokio::test]
async fn failed_turns_keep_their_kind_words_version_and_progress() {
    async fn failed(
        store: &SqliteStore,
        provider: &str,
        model: &str,
        error: &str,
    ) -> haarness_protocol::Turn {
        let thread = store
            .create_thread(CreateThreadRequest {
                title: Some("Private title".into()),
                provider: Some(provider.into()),
                model: Some(model.into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let (turn, _, _) = store
            .start_turn(
                thread.id,
                serde_json::from_value(
                    json!({"requestId": RequestId::new(), "text":"SECRET PROMPT"}),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        store
            .append_item(
                thread.id,
                Some(turn.id),
                ItemData::ToolCall {
                    call_id: "call".into(),
                    name: "task".into(),
                    arguments: json!({}),
                    provider_metadata: json!(null),
                },
            )
            .await
            .unwrap();
        store
            .append_item(
                thread.id,
                Some(turn.id),
                ItemData::ToolResult {
                    call_id: "call".into(),
                    name: "task".into(),
                    output: "The subagent failed: Gemini is overloaded or unavailable right now"
                        .into(),
                    is_error: true,
                    metadata: json!(null),
                },
            )
            .await
            .unwrap();
        store
            .set_turn_status(turn.id, TurnStatus::Failed, Some(error.into()))
            .await
            .unwrap();
        turn
    }
    let fixture = Fixture::new();
    let store = fixture.store("w1");
    let flash = "gemini-3-flash-preview";
    let used_up = "This API key has used up today's free allowance for Gemini 3 Flash on Gemini, which comes back at midnight Pacific time, in about 9 hours. Choose another model for this chat, or turn on billing for this API key's project in Google AI Studio.";
    let first = failed(&store, "gemini", flash, used_up).await;
    failed(&store, "gemini", flash, used_up).await;
    failed(
        &store,
        "gemini",
        flash,
        "Could not reach Gemini at generativelanguage.googleapis.com. Check the network connection and provider endpoint, then retry.",
    )
    .await;
    failed(
        &store,
        "acme-gateway",
        "secret-model",
        "Provider error: HTTP 500 Internal Server Error: SECRET STACK at /Users/sam/project/main.rs",
    )
    .await;
    turn(&store, "model-a", 10, None).await;

    let days = haarness_insights::daily_usage(&fixture.0, 7, Utc::now()).unwrap();
    let today = &days[0];
    assert_eq!((today.total.turns, today.total.failed), (5, 4));
    let failures: Vec<_> = today
        .failures
        .iter()
        .map(|row| {
            (
                row.kind.as_str(),
                row.provider.as_str(),
                row.model.as_str(),
                row.version.as_deref(),
            )
        })
        .collect();
    let version = Some(env!("CARGO_PKG_VERSION"));
    assert_eq!(
        failures,
        [
            ("quota", "gemini", flash, version),
            ("quota", "gemini", flash, version),
            ("network", "gemini", flash, version),
            ("provider_error", "acme-gateway", "secret-model", version),
        ],
        "oldest first, each on the version that ran it"
    );
    let row = &today.failures[0];
    assert_eq!(row.id, first.id.to_string());
    assert_eq!(row.at, first.created_at);
    assert_eq!(
        row.error, used_up,
        "the words as stored; the report scrubs them"
    );
    assert_eq!((row.tool_calls, row.tool_errors), (1, 1));
    assert_eq!(row.last_tool.as_deref(), Some("task"));

    let task_errors: Vec<_> = today
        .tool_errors
        .iter()
        .filter(|row| row.tool == "task")
        .collect();
    assert_eq!(task_errors.len(), 4);
    assert_eq!(
        task_errors[0].output,
        "The subagent failed: Gemini is overloaded or unavailable right now"
    );
    assert_eq!(
        today.versions,
        [haarness_insights::VersionUsage {
            version: version.map(Into::into),
            turns: 5,
            completed: 1,
            failed: 4,
            interrupted: 0,
        }]
    );

    // `haarness stats` reads no words at all.
    let report = collect_at(
        &fixture.0,
        std::path::Path::new("/alpha"),
        Options::default(),
        Utc::now(),
    )
    .unwrap();
    assert_eq!(report.total.failed, 4);
}

/// A history older than recorded versions still counts, under no version.
#[tokio::test]
async fn turns_from_before_versions_were_recorded_count_under_none() {
    let fixture = Fixture::new();
    let store = fixture.store("w1");
    turn(&store, "model-a", 10, None).await;
    drop(store);
    let path = fixture.0.join("workspaces/w1/haarness.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch("ALTER TABLE turns DROP COLUMN app_version;")
        .unwrap();
    drop(connection);
    let days = haarness_insights::daily_usage(&fixture.0, 7, Utc::now()).unwrap();
    assert_eq!(days[0].versions[0].version, None);
    assert_eq!(days[0].versions[0].turns, 1);
}

#[tokio::test]
async fn the_chat_home_s_usage_counts_under_chats_without_a_project() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.0.join("workspaces.json"),
        json!({"projects":[
        {"id":"p1","name":"Alpha","root":"/alpha"},
        {"id":"p9","name":"Chat folders","root":"/data/Chat folders"}],"workspaces":[
        {"id":"w1","projectId":"p1","path":"/alpha"},
        {"id":"w9","projectId":"p9","path":"/data/Chat folders"}]})
        .to_string(),
    )
    .unwrap();
    // Marked in its own file only, as after an older build rewrote the registry.
    std::fs::write(
        fixture.0.join("chat-home.json"),
        json!({"paths": ["/data/Chat folders"]}).to_string(),
    )
    .unwrap();
    turn(&fixture.store("w1"), "model-a", 100, None).await;
    turn(&fixture.store("w9"), "model-a", 40, None).await;
    let report = collect(
        &fixture.0,
        std::path::Path::new("/alpha"),
        Options::default(),
    )
    .unwrap();
    assert_eq!(report.total.input_tokens, 140, "its usage still counts");
    let mut names = report
        .projects
        .iter()
        .map(|project| project.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(names, ["Alpha", "Chats without a project"]);
}
