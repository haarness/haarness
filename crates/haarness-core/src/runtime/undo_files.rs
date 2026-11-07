//! What an undo step could not put back: the files the agent's file tools
//! changed that no checkpoint holds (ignored, or outside the folder), and
//! the files over the checkpoints' size limit that the restore left. Clients
//! show them under the undone turn, so a turn never reads as undone while
//! some of its changes stay.
//!
//! The file tools' calls are read before the workspace's undo lock is
//! taken ([`AgentRuntime::plan_undo_files`]): a long child chat never holds
//! up undo and redo in the other chats. Only their paths are kept.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use haarness_protocol::{Item, ItemData, ThreadId, TurnId, UnrestoredFile, UnrestoredReason};
use serde_json::Value;
use tracing::warn;

use super::{AgentRuntime, FolderAccess, blocking};
use crate::{ChangedFile, SnapshotStore, UndoStepFiles};

/// The most child agents' chats one undo step reads for their file tools.
const MAX_CHILD_THREADS: usize = 64;

/// The files the turns an undo step takes back changed, worked out before
/// the step: for the step back to `boundary` from `until`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UndoPlan {
    pub(super) boundary: u64,
    pub(super) until: Option<u64>,
    pub(super) changed: Vec<ChangedFile>,
}

/// A path a call changed: written (made or replaced) or removed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Change {
    Written { path: String, created: bool },
    Removed { path: String },
}

/// A child agent's chat that a call started, and when: its calls from then
/// to the end of the step are the step's.
struct ChildRun {
    thread: ThreadId,
    since: DateTime<Utc>,
}

impl AgentRuntime {
    /// [`Self::plan_undo_files`] for the step an undo of chat `id` (back to
    /// `sequence` when given) takes now. `None` when there is none, or the
    /// chat cannot be read: the undo then works it out itself.
    pub(super) async fn plan_revert_files(
        &self,
        id: ThreadId,
        sequence: Option<u64>,
    ) -> Option<UndoPlan> {
        let thread = self.repository.get_thread(id).await.ok()?;
        let items = self.repository.list_items(id, None).await.ok()?;
        let until = thread.revert.as_ref().map(|state| state.sequence);
        let boundary = undo_boundary(&items, sequence.unwrap_or(u64::MAX), until)?;
        Some(
            self.plan_undo_files(&thread, &items, (boundary, until))
                .await,
        )
    }

    /// Drop the copies of large files kept for `thread`'s pending undo, now
    /// that it is confirmed (see [`SnapshotStore::release_copies`]).
    pub(super) async fn release_undo_copies(&self, thread: &haarness_protocol::Thread) {
        let Some(revert) = &thread.revert else {
            return;
        };
        // Without snapshots no copy was kept.
        let Ok(snapshots) = self.undo_snapshots(thread.id, FolderAccess::Read).await else {
            return;
        };
        if let Err(error) = snapshots.release_copies(&revert.snapshot_id).await {
            warn!(%error, "cannot drop the copies kept for an undo");
        }
    }

    /// The files the undo step from `until` back to `boundary` would take
    /// back: those the file tools of the turns after `boundary` (up to
    /// `until`) and of the child agents they ran changed, resolved as the
    /// tools resolve them. Best effort: a chat that cannot be read is left
    /// out.
    pub(super) async fn plan_undo_files(
        &self,
        thread: &haarness_protocol::Thread,
        items: &[Item],
        (boundary, until): (u64, Option<u64>),
    ) -> UndoPlan {
        let undone = items
            .iter()
            .filter(|item| item.sequence > boundary && until.is_none_or(|end| item.sequence <= end))
            .collect::<Vec<_>>();
        let mut plan = UndoPlan {
            boundary,
            until,
            changed: Vec::new(),
        };
        // The step ends where its last item was written; a child's later
        // calls (a background child still running) are not the step's.
        let Some(end) = undone.iter().map(|item| item.created_at).max() else {
            return plan;
        };
        let (mut changes, children) = file_changes(undone.iter().copied());
        let mut seen = HashSet::new();
        let mut queue = VecDeque::from(children);
        while let Some(child) = queue.pop_front() {
            if seen.len() >= MAX_CHILD_THREADS {
                break;
            }
            if !seen.insert((child.thread, child.since)) {
                continue;
            }
            match self.repository.list_items(child.thread, None).await {
                Ok(items) => {
                    // A resumed child's earlier turns are another step's.
                    let (more, grandchildren) =
                        file_changes(items.iter().filter(|item| {
                            item.created_at >= child.since && item.created_at <= end
                        }));
                    changes.extend(more);
                    queue.extend(grandchildren);
                }
                Err(error) => {
                    warn!(child = %child.thread, %error, "cannot read a child agent's files for undo");
                }
            }
        }
        if changes.is_empty() {
            return plan;
        }
        match self.thread_workspace(thread, FolderAccess::Read).await {
            Ok(workspace) => match blocking(move || Ok(changed_files(changes, &workspace))).await {
                Ok(files) => plan.changed = files,
                Err(error) => warn!(%error, "cannot tell which files undo leaves"),
            },
            Err(error) => warn!(%error, "cannot tell which files undo leaves"),
        }
        plan
    }

    /// The files the undo step that `plan` describes left as they are:
    /// `skipped`, left for their size, and those of the plan that neither
    /// `expected` nor `desired` holds. Never fails the undo, which is done by
    /// now: what cannot be worked out is left out.
    pub(super) async fn undo_step_files(
        snapshots: &Arc<dyn SnapshotStore>,
        plan: UndoPlan,
        expected: &str,
        desired: &str,
        skipped: Vec<String>,
    ) -> UndoStepFiles {
        let mut left = Vec::new();
        if !plan.changed.is_empty() {
            match snapshots.left_out(expected, desired, plan.changed).await {
                Ok(files) => left = files,
                Err(error) => warn!(%error, "cannot tell which files undo left"),
            }
        }
        UndoStepFiles::new(
            skipped
                .into_iter()
                .map(|path| UnrestoredFile {
                    path,
                    reason: UnrestoredReason::Large,
                })
                .chain(left),
        )
    }
}

/// The step an undo now would take: back to the newest checkpoint before
/// `current` (the pending undo's boundary), whether a turn's paired
/// checkpoints or a prompt's own boundary, among items up to `limit`.
pub(super) fn undo_boundary(items: &[Item], limit: u64, current: Option<u64>) -> Option<u64> {
    let eligible = |boundary: u64| current.is_none_or(|current| boundary < current);
    items
        .iter()
        .filter(|item| item.sequence <= limit)
        .filter_map(|item| match &item.data {
            ItemData::Snapshot {
                boundary_sequence: Some(boundary),
                base_snapshot_id: Some(_),
                ..
            } => Some(*boundary),
            ItemData::UserMessage {
                synthetic: false, ..
            } => Some(item.sequence.saturating_sub(1)),
            _ => None,
        })
        .filter(|boundary| eligible(*boundary))
        .max()
}

/// A call by its turn and id.
type CallKey<'a> = (Option<TurnId>, &'a str);
/// A call's tool, arguments and when it started.
type StartedCall<'a> = (&'a str, &'a Value, DateTime<Utc>);

/// What the file-tool calls among `items` that succeeded changed, in the
/// order they ran, and the child agents' chats they started. Only names are
/// read; no call's content is copied.
fn file_changes<'a>(items: impl Iterator<Item = &'a Item>) -> (Vec<Change>, Vec<ChildRun>) {
    let mut started: HashMap<CallKey<'_>, StartedCall<'_>> = HashMap::new();
    let mut changes = Vec::new();
    let mut children = Vec::new();
    for item in items {
        match &item.data {
            ItemData::ToolCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                started.insert(
                    (item.turn_id, call_id.as_str()),
                    (name.as_str(), arguments, item.created_at),
                );
            }
            ItemData::ToolResult {
                call_id,
                is_error: false,
                metadata,
                ..
            } => {
                if let Some((name, arguments, _)) = started.get(&(item.turn_id, call_id.as_str())) {
                    changes.extend(call_changes(name, arguments, metadata));
                }
            }
            ItemData::TaskLink {
                call_id,
                child_thread_id,
            } => children.push(ChildRun {
                thread: *child_thread_id,
                // From the call that started it, which comes before the
                // child's first item.
                since: started
                    .get(&(item.turn_id, call_id.as_str()))
                    .map_or(item.created_at, |(_, _, at)| *at),
            }),
            _ => {}
        }
    }
    (changes, children)
}

fn tool_name(name: &str) -> &str {
    name.strip_prefix("functions.").unwrap_or(name)
}

/// What one call changed, by the names it gave.
fn call_changes(name: &str, arguments: &Value, metadata: &Value) -> Vec<Change> {
    let path = || {
        arguments
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    match tool_name(name) {
        // A write says whether it made the file; one that does not (an older
        // record) counts as made, so a file gone since is never named for it.
        "write" => path()
            .map(|path| Change::Written {
                path,
                created: metadata
                    .pointer("/diffs/0/status")
                    .and_then(Value::as_str)
                    .is_none_or(|status| status == "added"),
            })
            .into_iter()
            .collect(),
        "edit" | "multiedit" => path()
            .map(|path| Change::Written {
                path,
                created: false,
            })
            .into_iter()
            .collect(),
        "apply_patch" => arguments
            .get("input")
            .and_then(Value::as_str)
            .and_then(|input| crate::patch::parse_patch(input).ok())
            .unwrap_or_default()
            .into_iter()
            .flat_map(|operation| match operation {
                crate::patch::FileOp::Add { path, .. } => vec![Change::Written {
                    path,
                    created: true,
                }],
                crate::patch::FileOp::Update {
                    path,
                    move_to: Some(target),
                    ..
                } => vec![
                    Change::Removed { path },
                    Change::Written {
                        path: target,
                        created: true,
                    },
                ],
                crate::patch::FileOp::Update { path, .. } => vec![Change::Written {
                    path,
                    created: false,
                }],
                crate::patch::FileOp::Delete { path } => vec![Change::Removed { path }],
            })
            .collect(),
        // Each step that succeeded, with its own result.
        "batch" => {
            let steps = arguments.get("tool_calls").and_then(Value::as_array);
            let results = metadata.get("results").and_then(Value::as_array);
            steps
                .into_iter()
                .flatten()
                .zip(results.into_iter().flatten())
                .filter(|(_, result)| result.get("error").is_none())
                .flat_map(|(step, result)| {
                    let name = step.get("tool").and_then(Value::as_str).unwrap_or_default();
                    if tool_name(name) == "batch" {
                        return Vec::new();
                    }
                    call_changes(
                        name,
                        step.get("parameters").unwrap_or(&Value::Null),
                        result.get("metadata").unwrap_or(&Value::Null),
                    )
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// The files `changes` named, each once, as the tools resolve them in
/// `workspace`, with whether its first change made it.
fn changed_files(changes: Vec<Change>, workspace: &Path) -> Vec<ChangedFile> {
    let mut files: Vec<ChangedFile> = Vec::new();
    let mut known: HashSet<PathBuf> = HashSet::new();
    for change in changes {
        let (path, created) = match change {
            Change::Written { path, created } => (
                crate::permissions::resolve_permission_path(workspace, &path),
                created,
            ),
            Change::Removed { path } => (
                crate::permissions::resolve_permission_entry(workspace, &path),
                false,
            ),
        };
        if known.insert(path.clone()) {
            files.push(ChangedFile { path, created });
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use haarness_protocol::ItemId;
    use serde_json::json;

    fn item(sequence: u64, turn: TurnId, data: ItemData) -> Item {
        Item {
            id: ItemId::new(),
            thread_id: ThreadId::new(),
            turn_id: Some(turn),
            sequence,
            created_at: Utc::now(),
            data,
        }
    }

    fn call(sequence: u64, turn: TurnId, id: &str, name: &str, arguments: Value) -> Item {
        item(
            sequence,
            turn,
            ItemData::ToolCall {
                call_id: id.into(),
                name: name.into(),
                arguments,
                provider_metadata: Value::Null,
            },
        )
    }

    fn result(sequence: u64, turn: TurnId, id: &str, is_error: bool, metadata: Value) -> Item {
        item(
            sequence,
            turn,
            ItemData::ToolResult {
                call_id: id.into(),
                name: String::new(),
                output: String::new(),
                is_error,
                metadata,
            },
        )
    }

    #[test]
    fn the_file_tools_that_succeeded_name_what_they_changed() {
        let root =
            std::env::temp_dir().join(format!("haarness-undo-files-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let workspace = dunce::canonicalize(&root).unwrap();
        let turn = TurnId::new();
        let added = json!({"diffs": [{"file": "new.txt", "status": "added"}]});
        let replaced = json!({"diffs": [{"file": ".env", "status": "modified"}]});
        let patch = "*** Begin Patch\n*** Add File: added.txt\n+x\n*** Update File: old.rs\n*** Move to: moved.rs\n@@\n-a\n+b\n*** Delete File: gone.txt\n*** End Patch\n";
        let child = ThreadId::new();
        let items = vec![
            call(
                1,
                turn,
                "a",
                "write",
                json!({"path": ".env", "content": "x"}),
            ),
            result(2, turn, "a", false, replaced),
            call(
                3,
                turn,
                "b",
                "functions.write",
                json!({"path": "new.txt", "content": "x"}),
            ),
            result(4, turn, "b", false, added),
            // Failed: it changed nothing.
            call(
                5,
                turn,
                "c",
                "edit",
                json!({"path": "failed.txt", "old": "a", "new": "b"}),
            ),
            result(6, turn, "c", true, Value::Null),
            call(7, turn, "d", "apply_patch", json!({"input": patch})),
            result(8, turn, "d", false, Value::Null),
            call(9, turn, "e", "read", json!({"path": "read.txt"})),
            result(10, turn, "e", false, Value::Null),
            call(
                11,
                turn,
                "f",
                "batch",
                json!({"tool_calls": [
                    {"tool": "edit", "parameters": {"path": "batched.txt", "old": "a", "new": "b"}},
                    {"tool": "write", "parameters": {"path": "refused.txt", "content": "x"}},
                ]}),
            ),
            result(
                12,
                turn,
                "f",
                false,
                json!({"results": [{"tool": "edit", "output": "updated"}, {"tool": "write", "error": "denied"}]}),
            ),
            // The same file again: its first change counts.
            call(
                13,
                turn,
                "g",
                "write",
                json!({"path": "new.txt", "content": "y"}),
            ),
            result(
                14,
                turn,
                "g",
                false,
                json!({"diffs": [{"status": "modified"}]}),
            ),
            call(15, turn, "h", "task", json!({"prompt": "help"})),
            item(
                16,
                turn,
                ItemData::TaskLink {
                    call_id: "h".into(),
                    child_thread_id: child,
                },
            ),
        ];
        let (changes, children) = file_changes(items.iter());
        assert_eq!(
            children
                .iter()
                .map(|child| (child.thread, child.since))
                .collect::<Vec<_>>(),
            vec![(child, items[14].created_at)],
            "a child's calls count from the call that started it"
        );
        let files = changed_files(changes, &workspace);
        let named = files
            .iter()
            .map(|file| {
                (
                    file.path
                        .strip_prefix(&workspace)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    file.created,
                )
            })
            .collect::<Vec<_>>();
        let expect = |path: &str, created| (path.to_owned(), created);
        assert_eq!(
            named,
            vec![
                expect(".env", false),
                expect("new.txt", true),
                expect("added.txt", true),
                expect("old.rs", false),
                expect("moved.rs", true),
                expect("gone.txt", false),
                expect("batched.txt", false),
            ]
        );
        std::fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn the_step_an_undo_takes_is_the_newest_boundary_before_the_pending_one() {
        let turn = TurnId::new();
        let prompt = |sequence| {
            item(
                sequence,
                turn,
                ItemData::UserMessage {
                    text: "go".into(),
                    attachments: vec![],
                    queued: false,
                    synthetic: false,
                    command_line: None,
                },
            )
        };
        let pair = |sequence, boundary| {
            item(
                sequence,
                turn,
                ItemData::Snapshot {
                    snapshot_id: format!("after-{boundary}"),
                    file_count: 1,
                    boundary_sequence: Some(boundary),
                    base_snapshot_id: Some(format!("before-{boundary}")),
                },
            )
        };
        let items = vec![prompt(1), pair(3, 0), prompt(4), pair(6, 3), prompt(7)];
        // A text-only prompt last: its own boundary.
        assert_eq!(undo_boundary(&items, u64::MAX, None), Some(6));
        assert_eq!(undo_boundary(&items, u64::MAX, Some(6)), Some(3));
        assert_eq!(undo_boundary(&items, u64::MAX, Some(3)), Some(0));
        assert_eq!(undo_boundary(&items, u64::MAX, Some(0)), None);
        assert_eq!(undo_boundary(&items, 5, None), Some(3));
    }
}
