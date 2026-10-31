//! A nested `AGENTS.md` that `read` attached stays attached once while the
//! model can see it, and comes back with the next read below its folder once
//! compaction or an undo took the result that carried it out of the
//! conversation.
use super::*;
use haarness_protocol::{CompactRequest, ItemData, ThreadId};
use serde_json::json;
use std::path::{Path, PathBuf};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("haarness-in-view-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(path.join("ws/sub")).unwrap();
        std::fs::write(path.join("ws/sub/AGENTS.md"), "SUB_FOLDER_RULES").unwrap();
        std::fs::write(path.join("ws/sub/a.txt"), "alpha").unwrap();
        std::fs::write(path.join("ws/sub/b.txt"), "beta").unwrap();
        Self(dunce::canonicalize(&path).unwrap())
    }
    fn workspace(&self) -> PathBuf {
        self.0.join("ws")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A runtime that keeps one recent turn behind a summary, with undo.
fn runtime(fixture: &Fixture) -> AgentRuntime {
    let workspace = fixture.workspace();
    let database = fixture.0.join("db.sqlite");
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    config.compaction.tail_turns = 1;
    AgentRuntime::with_config(
        Arc::new(SqliteStore::open(&database).unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(workspace.clone())),
        EventBus::new(2048),
        workspace.clone(),
        String::new(),
        config,
    )
    .with_snapshots(Arc::new(
        haarness_store::SqliteSnapshotStore::open(&database, workspace).unwrap(),
    ))
}

async fn turn(runtime: &AgentRuntime, thread: ThreadId, text: &str) {
    let turn = runtime
        .start_turn(thread, turn_request(text))
        .await
        .unwrap();
    assert_eq!(
        wait_done(runtime, &turn).await.status,
        TurnStatus::Completed
    );
}

/// A turn in which the model reads `path`: what that read attached.
async fn reads(runtime: &AgentRuntime, thread: ThreadId, path: &str) -> Vec<String> {
    turn(
        runtime,
        thread,
        &format!("!tool read {}", json!({ "path": path })),
    )
    .await;
    let items = runtime.items(thread, None).await.unwrap();
    let (output, metadata) = items
        .iter()
        .rev()
        .find_map(|item| match &item.data {
            ItemData::ToolResult {
                name,
                output,
                metadata,
                ..
            } if name == "read" => Some((output.clone(), metadata.clone())),
            _ => None,
        })
        .expect("the read's result");
    let loaded = metadata["loaded"]
        .as_array()
        .map(|paths| {
            paths
                .iter()
                .map(|path| path.as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert_eq!(
        output.contains("SUB_FOLDER_RULES"),
        !loaded.is_empty(),
        "{output}"
    );
    loaded
}

fn rules(workspace: &Path) -> Vec<String> {
    vec![
        workspace
            .join("sub")
            .join("AGENTS.md")
            .display()
            .to_string(),
    ]
}

async fn compact(runtime: &AgentRuntime, thread: ThreadId) {
    runtime
        .compact(
            thread,
            CompactRequest {
                summary: Some("Earlier work, summarized.".into()),
                files_touched: Vec::new(),
                outstanding: Vec::new(),
                todo_checkpoint: None,
                tail_start_sequence: None,
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_nested_instruction_file_comes_back_after_compaction_took_it_out_of_view() {
    let fixture = Fixture::new();
    let runtime = runtime(&fixture);
    let thread = echo_thread(&runtime).await.id;
    let rules = rules(&fixture.workspace());

    assert_eq!(reads(&runtime, thread, "sub/a.txt").await, rules);
    // In view: a read in the same folder, in a later turn, adds nothing.
    assert!(reads(&runtime, thread, "sub/b.txt").await.is_empty());
    turn(&runtime, thread, "Something else").await;
    // One recent turn stays behind the summary; both reads are summarized.
    compact(&runtime, thread).await;
    assert_eq!(reads(&runtime, thread, "sub/b.txt").await, rules);
    assert!(reads(&runtime, thread, "sub/a.txt").await.is_empty());
}

#[tokio::test]
async fn a_nested_instruction_file_still_in_the_kept_turns_is_not_attached_twice() {
    let fixture = Fixture::new();
    let runtime = runtime(&fixture);
    let thread = echo_thread(&runtime).await.id;

    assert_eq!(
        reads(&runtime, thread, "sub/a.txt").await,
        rules(&fixture.workspace())
    );
    // The turn that attached it is the one kept verbatim.
    compact(&runtime, thread).await;
    assert!(reads(&runtime, thread, "sub/b.txt").await.is_empty());
}

#[tokio::test]
async fn a_nested_instruction_file_comes_back_after_its_turn_was_undone() {
    let fixture = Fixture::new();
    let runtime = runtime(&fixture);
    let thread = echo_thread(&runtime).await.id;
    let rules = rules(&fixture.workspace());

    turn(&runtime, thread, "Hello").await;
    assert_eq!(reads(&runtime, thread, "sub/a.txt").await, rules);
    runtime.revert(thread, None).await.unwrap();
    assert_eq!(reads(&runtime, thread, "sub/b.txt").await, rules);
    assert!(reads(&runtime, thread, "sub/a.txt").await.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_or_differently_cased_instruction_file_is_attached_once() {
    let fixture = Fixture::new();
    let runtime = runtime(&fixture);
    let workspace = fixture.workspace();
    // A common setup: the folder's AGENTS.md points at its CLAUDE.md.
    std::fs::create_dir_all(workspace.join("linked")).unwrap();
    std::fs::write(workspace.join("linked/CLAUDE.md"), "SUB_FOLDER_RULES").unwrap();
    std::os::unix::fs::symlink("CLAUDE.md", workspace.join("linked/AGENTS.md")).unwrap();
    std::fs::write(workspace.join("linked/a.txt"), "alpha").unwrap();
    std::fs::write(workspace.join("linked/b.txt"), "beta").unwrap();
    let thread = echo_thread(&runtime).await.id;

    assert_eq!(
        reads(&runtime, thread, "linked/a.txt").await,
        vec![workspace.join("linked/AGENTS.md").display().to_string()]
    );
    assert!(reads(&runtime, thread, "linked/b.txt").await.is_empty());

    // On a filesystem that ignores case (macOS by default) `agents.md` is
    // found as `AGENTS.md`.
    std::fs::create_dir_all(workspace.join("cased")).unwrap();
    std::fs::write(workspace.join("cased/agents.md"), "SUB_FOLDER_RULES").unwrap();
    std::fs::write(workspace.join("cased/a.txt"), "alpha").unwrap();
    std::fs::write(workspace.join("cased/b.txt"), "beta").unwrap();
    if workspace.join("cased/AGENTS.md").is_file() {
        let thread = echo_thread(&runtime).await.id;
        assert_eq!(
            reads(&runtime, thread, "cased/a.txt").await,
            vec![workspace.join("cased/AGENTS.md").display().to_string()]
        );
        assert!(reads(&runtime, thread, "cased/b.txt").await.is_empty());
    }
}
