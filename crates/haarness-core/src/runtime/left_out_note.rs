//! The note a turn the provider's safety check stopped leaves in its place
//! when it changed things. The turn itself is left out of later requests
//! (see [`haarness_protocol::tokens::left_out`]), but its edits and commands
//! stay done: without a word of them the model could do them again, or
//! write over files it does not know changed. The note names what changed
//! and how many commands ran, never what the turn said or what its tools
//! answered.

use std::fmt::Write as _;

use haarness_protocol::{Item, ItemData, tokens::LEFT_OUT_NOTE};
use serde_json::Value;

/// The most changed files the note names; it counts the others.
const NAMED_FILES: usize = 20;

/// The shell commands `items` (one turn's) ran: each `bash` call, and each
/// `bash` step of a `batch`.
pub(super) fn commands_run(items: &[&Item]) -> usize {
    items
        .iter()
        .map(|item| match &item.data {
            ItemData::ToolCall { name, .. } if name == "bash" => 1,
            ItemData::ToolCall {
                name, arguments, ..
            } if name == "batch" => arguments
                .get("tool_calls")
                .and_then(Value::as_array)
                .map_or(0, |steps| {
                    steps
                        .iter()
                        .filter(|step| step.get("tool").and_then(Value::as_str) == Some("bash"))
                        .count()
                }),
            _ => 0,
        })
        .sum()
}

/// The note for a left-out turn that changed `files` (relative to the
/// folder where they are inside it) and ran `commands` shell commands, or
/// `None` when it did neither: then nothing of it is left.
pub(super) fn note(files: &[String], commands: usize) -> Option<String> {
    if files.is_empty() && commands == 0 {
        return None;
    }
    let mut did = Vec::new();
    if !files.is_empty() {
        let mut named = files
            .iter()
            .take(NAMED_FILES)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if files.len() > NAMED_FILES {
            let _ = write!(named, " and {} more", files.len() - NAMED_FILES);
        }
        did.push(format!(
            "changed {} ({named})",
            if files.len() == 1 {
                "1 file".to_owned()
            } else {
                format!("{} files", files.len())
            }
        ));
    }
    if commands > 0 {
        did.push(if commands == 1 {
            "ran 1 shell command".to_owned()
        } else {
            format!("ran {commands} shell commands")
        });
    }
    Some(format!(
        "{LEFT_OUT_NOTE}A turn here was stopped by the provider's safety check and is left out of this conversation. Before it stopped, it {}. What it did stays done: read a file again before you edit it, and check before you run a command again.\n</left-out-turn>",
        did.join(" and ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str, arguments: Value) -> Item {
        Item {
            id: haarness_protocol::ItemId::new(),
            thread_id: haarness_protocol::ThreadId::new(),
            turn_id: None,
            sequence: 1,
            data: ItemData::ToolCall {
                call_id: "c".into(),
                name: name.into(),
                arguments,
                provider_metadata: Value::Null,
            },
            created_at: chrono::Utc::now(),
        }
    }

    /// A turn that changed nothing leaves nothing; one that did says what,
    /// and counts what it does not name.
    #[test]
    fn the_note_names_what_changed_and_nothing_else() {
        assert_eq!(note(&[], 0), None);
        let one = note(&["src/a.rs".into()], 0).unwrap();
        assert!(one.starts_with(LEFT_OUT_NOTE), "{one}");
        assert!(one.contains("it changed 1 file (src/a.rs). What it did stays done: read a file again before you edit it"), "{one}");
        let commands = note(&[], 2).unwrap();
        assert!(commands.contains("it ran 2 shell commands."), "{commands}");
        let many = (0..23)
            .map(|index| format!("f{index}.txt"))
            .collect::<Vec<_>>();
        let both = note(&many, 1).unwrap();
        assert!(both.contains("changed 23 files (f0.txt, f1.txt"), "{both}");
        assert!(
            both.contains("f19.txt and 3 more) and ran 1 shell command."),
            "{both}"
        );
        assert!(!both.contains("f20.txt"), "{both}");
    }

    #[test]
    fn commands_are_counted_in_and_out_of_a_batch() {
        let items = [
            call("bash", json!({"command": "git commit -m x"})),
            call("read", json!({"path": "a"})),
            call(
                "batch",
                json!({"tool_calls": [
                    {"tool": "bash", "parameters": {"command": "ls"}},
                    {"tool": "read", "parameters": {"path": "b"}},
                    {"tool": "bash", "parameters": {"command": "make"}},
                ]}),
            ),
        ];
        assert_eq!(commands_run(&items.iter().collect::<Vec<_>>()), 3);
    }
}
