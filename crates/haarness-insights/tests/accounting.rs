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
