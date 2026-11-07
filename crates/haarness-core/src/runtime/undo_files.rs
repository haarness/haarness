//! What an undo step could not put back: the files the agent's file tools
//! changed that no checkpoint holds (ignored, or outside the folder), and
//! the files over the checkpoints' size limit, which the restore names.
//! Clients show them under the undone turn, so a turn never reads as undone
//! while some of its changes stay.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
};

use haarness_protocol::{Item, ItemData, ThreadId, TurnId};
use serde_json::Value;
use tracing::warn;

use super::{AgentRuntime, FolderAccess, blocking};
use crate::{ChangedFile, SnapshotStore, UndoStepFiles};

/// The most child agents' chats one undo step reads for their file tools.
const MAX_CHILD_THREADS: usize = 64;

/// One file-tool call that succeeded, as the transcript keeps it.
#[derive(Debug, Clone)]
pub(super) struct FileCall {
    name: String,
    arguments: Value,
    metadata: Value,
}

/// A path a call changed: written (made or replaced) or removed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Change {
    Written { path: String, created: bool },
    Removed { path: String },
}

impl AgentRuntime {
    /// The files the undo step back to `boundary` left as they are: those
    /// `skipped` names, and those the file tools of the turns it undid (the
    /// items after `boundary`, up to `until`) and of their child agents
    /// changed that neither `expected` nor `desired` holds. Never fails the
    /// undo, which is done by now: what cannot be worked out is left out.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn undo_step_files(
        &self,
        thread: &haarness_protocol::Thread,
        snapshots: &Arc<dyn SnapshotStore>,
        items: &[Item],
        (boundary, until): (u64, Option<u64>),
        expected: &str,
        desired: &str,
        skipped: Vec<String>,
    ) -> UndoStepFiles {
        let undone = items.iter().filter(|item| {
            item.sequence > boundary && until.is_none_or(|end| item.sequence <= end)
        });
        let (mut calls, children) = file_calls(undone);
        let mut seen = HashSet::new();
        let mut queue = VecDeque::from(children);
        while let Some(child) = queue.pop_front() {
            if seen.len() >= MAX_CHILD_THREADS || !seen.insert(child) {
                continue;
            }
            match self.repository.list_items(child, None).await {
                Ok(items) => {
                    let (more, grandchildren) = file_calls(items.iter());
                    calls.extend(more);
                    queue.extend(grandchildren);
                }
                Err(error) => warn!(%child, %error, "cannot read a child agent's files for undo"),
            }
        }
        let mut left = Vec::new();
        if !calls.is_empty() {
            match self.thread_workspace(thread, FolderAccess::Read).await {
                Ok(workspace) => {
                    let changed = blocking(move || Ok(changed_files(&calls, &workspace))).await;
                    let left_out = match changed {
                        Ok(changed) => snapshots.left_out(expected, desired, changed).await,
                        Err(error) => Err(error),
                    };
                    match left_out {
                        Ok(files) => left = files,
                        Err(error) => warn!(%error, "cannot tell which files undo left"),
                    }
                }
                Err(error) => warn!(%error, "cannot tell which files undo left"),
            }
        }
        UndoStepFiles::new(skipped.into_iter().chain(left))
    }
}

/// The file-tool calls among `items` that succeeded, in the order they ran,
/// and the child agents' chats they started.
fn file_calls<'a>(items: impl Iterator<Item = &'a Item>) -> (Vec<FileCall>, Vec<ThreadId>) {
    let mut started: HashMap<(Option<TurnId>, &str), (&str, &Value)> = HashMap::new();
    let mut calls = Vec::new();
    let mut children = Vec::new();
    for item in items {
        match &item.data {
            ItemData::ToolCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                started.insert((item.turn_id, call_id.as_str()), (name.as_str(), arguments));
            }
            ItemData::ToolResult {
                call_id,
                is_error: false,
                metadata,
                ..
            } => {
                if let Some((name, arguments)) = started.get(&(item.turn_id, call_id.as_str())) {
                    calls.push(FileCall {
                        name: (*name).to_owned(),
                        arguments: (*arguments).clone(),
                        metadata: metadata.clone(),
                    });
                }
            }
            ItemData::TaskLink {
                child_thread_id, ..
            } => children.push(*child_thread_id),
            _ => {}
        }
    }
    calls.retain(|call| {
        matches!(
            tool_name(&call.name),
            "write" | "edit" | "multiedit" | "apply_patch" | "batch"
        )
    });
    (calls, children)
}

fn tool_name(name: &str) -> &str {
    name.strip_prefix("functions.").unwrap_or(name)
}

/// What each call changed, by the names it gave.
fn changes(name: &str, arguments: &Value, metadata: &Value) -> Vec<Change> {
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
                    changes(
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

/// The files `calls` changed, each once, as the tools resolve them in
/// `workspace`, with whether its first change made it.
fn changed_files(calls: &[FileCall], workspace: &Path) -> Vec<ChangedFile> {
    let mut files: Vec<ChangedFile> = Vec::new();
    let mut known: HashSet<PathBuf> = HashSet::new();
    for call in calls {
        for change in changes(&call.name, &call.arguments, &call.metadata) {
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
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
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
            item(
                15,
                turn,
                ItemData::TaskLink {
                    call_id: "h".into(),
                    child_thread_id: child,
                },
            ),
        ];
        let (calls, children) = file_calls(items.iter());
        assert_eq!(children, vec![child]);
        let files = changed_files(&calls, &workspace);
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
}
