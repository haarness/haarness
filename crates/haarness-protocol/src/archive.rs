use std::collections::{HashMap, HashSet, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    GoalState, Item, ItemData, ItemId, LoopConfig, PermissionReview, ReviewReason, Thread,
    ThreadId, TodoCheckpoint, TodoEntry, Turn, TurnId,
};

pub const SESSION_ARCHIVE_CAPABILITY: &str = "session-archive.v1";
pub const SESSION_ARCHIVE_FORMAT: &str = "haarness.session-archive";
pub const SESSION_ARCHIVE_SCHEMA_VERSION: u32 = 1;

/// A portable conversation tree. Workspace snapshots, credentials,
/// permissions and runtime settings are intentionally host-local and
/// never enter this format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionArchive {
    pub format: String,
    pub schema_version: u32,
    pub source_version: String,
    pub exported_at: DateTime<Utc>,
    pub root_thread_id: ThreadId,
    #[serde(default)]
    pub sanitized: bool,
    pub threads: Vec<SessionArchiveThread>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionArchiveThread {
    pub thread: Thread,
    pub turns: Vec<Turn>,
    pub items: Vec<Item>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<GoalState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_config: Option<LoopConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionArchiveImportRequest {
    pub archive: SessionArchive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionArchiveImportResult {
    pub root: Thread,
    pub threads: Vec<Thread>,
    pub created: Vec<ThreadId>,
    pub merged: Vec<ThreadId>,
    /// Merged threads that came from a sanitized archive and now hold this
    /// archive's real content in place of the redaction markers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restored: Vec<ThreadId>,
}

impl SessionArchive {
    /// Reject malformed ownership graphs before any persistence adapter begins
    /// a transaction. The archive is intentionally strict so unknown future
    /// versions never get partially interpreted as version 1.
    pub fn validate(&self) -> Result<(), String> {
        if self.format != SESSION_ARCHIVE_FORMAT {
            return Err(format!(
                "unsupported session archive format: {}",
                self.format
            ));
        }
        if self.schema_version != SESSION_ARCHIVE_SCHEMA_VERSION {
            return Err(format!(
                "unsupported session archive schema version: {}; expected {}",
                self.schema_version, SESSION_ARCHIVE_SCHEMA_VERSION
            ));
        }
        if self.threads.is_empty() {
            return Err("session archive contains no threads".into());
        }

        let mut threads = HashMap::new();
        for entry in &self.threads {
            if threads.insert(entry.thread.id, entry).is_some() {
                return Err(format!(
                    "duplicate thread id in session archive: {}",
                    entry.thread.id
                ));
            }
            if entry.thread.revert.is_some() {
                return Err(format!(
                    "session archive thread {} contains host-local revert state",
                    entry.thread.id
                ));
            }
        }
        let Some(root) = threads.get(&self.root_thread_id) else {
            return Err(format!(
                "session archive root not found: {}",
                self.root_thread_id
            ));
        };
        if root.thread.parent_id.is_some() {
            return Err("session archive root must not have a parent".into());
        }

        for entry in &self.threads {
            if entry.thread.id != self.root_thread_id && entry.thread.parent_id.is_none() {
                return Err(format!(
                    "session archive child has no parent: {}",
                    entry.thread.id
                ));
            }
            if let Some(parent) = entry.thread.parent_id
                && !threads.contains_key(&parent)
            {
                return Err(format!(
                    "session archive parent {parent} is missing for {}",
                    entry.thread.id
                ));
            }
            let mut seen = HashSet::new();
            let mut current = Some(entry.thread.id);
            while let Some(id) = current {
                if !seen.insert(id) {
                    return Err(format!("session archive graph contains a cycle at {id}"));
                }
                current = threads.get(&id).and_then(|item| item.thread.parent_id);
            }
            if !seen.contains(&self.root_thread_id) {
                return Err(format!(
                    "session archive thread {} is disconnected from root {}",
                    entry.thread.id, self.root_thread_id
                ));
            }
        }

        let mut turns = HashMap::<TurnId, ThreadId>::new();
        let mut requests = HashSet::new();
        let mut items = HashMap::<ItemId, ThreadId>::new();
        let mut sequences = HashSet::new();
        for entry in &self.threads {
            for turn in &entry.turns {
                if turn.thread_id != entry.thread.id {
                    return Err(format!("turn {} belongs to a different thread", turn.id));
                }
                if turns.insert(turn.id, entry.thread.id).is_some() {
                    return Err(format!("duplicate turn id in session archive: {}", turn.id));
                }
                if !requests.insert((entry.thread.id, turn.request_id)) {
                    return Err(format!(
                        "duplicate request id in session archive thread {}: {}",
                        entry.thread.id, turn.request_id
                    ));
                }
            }
            for item in &entry.items {
                if item.thread_id != entry.thread.id {
                    return Err(format!("item {} belongs to a different thread", item.id));
                }
                if matches!(&item.data, ItemData::Snapshot { .. }) {
                    return Err(format!(
                        "item {} contains a host-local workspace snapshot",
                        item.id
                    ));
                }
                if let Some(turn_id) = item.turn_id
                    && turns.get(&turn_id) != Some(&entry.thread.id)
                {
                    return Err(format!(
                        "item {} references an unknown or foreign turn {turn_id}",
                        item.id
                    ));
                }
                if items.insert(item.id, entry.thread.id).is_some() {
                    return Err(format!("duplicate item id in session archive: {}", item.id));
                }
                if !sequences.insert((entry.thread.id, item.sequence)) {
                    return Err(format!(
                        "duplicate item sequence in session archive thread {}: {}",
                        entry.thread.id, item.sequence
                    ));
                }
            }
        }

        for entry in &self.threads {
            for item in &entry.items {
                match &item.data {
                    ItemData::QueueConsumed { item_id } if !items.contains_key(item_id) => {
                        return Err(format!(
                            "item {} references missing queued item {item_id}",
                            item.id
                        ));
                    }
                    ItemData::TaskLink {
                        child_thread_id, ..
                    }
                    | ItemData::TaskState {
                        child_thread_id, ..
                    } if !threads.contains_key(child_thread_id) => {
                        return Err(format!(
                            "item {} references missing child thread {child_thread_id}",
                            item.id
                        ));
                    }
                    ItemData::TaskState {
                        child_thread_id,
                        child_turn_id,
                        ..
                    } if turns.get(child_turn_id) != Some(child_thread_id) => {
                        return Err(format!(
                            "item {} references an unknown child turn {child_turn_id}",
                            item.id
                        ));
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Parent-before-child order independent of the JSON array order.
    pub fn ordered_thread_ids(&self) -> Result<Vec<ThreadId>, String> {
        self.validate()?;
        let mut children = HashMap::<ThreadId, Vec<ThreadId>>::new();
        for entry in &self.threads {
            if let Some(parent) = entry.thread.parent_id {
                children.entry(parent).or_default().push(entry.thread.id);
            }
        }
        for values in children.values_mut() {
            values.sort_by_key(ToString::to_string);
        }
        let mut queue = VecDeque::from([self.root_thread_id]);
        let mut ordered = Vec::with_capacity(self.threads.len());
        while let Some(id) = queue.pop_front() {
            ordered.push(id);
            queue.extend(children.remove(&id).unwrap_or_default());
        }
        Ok(ordered)
    }

    #[must_use]
    pub fn sanitize(mut self) -> Self {
        self.sanitized = true;
        for entry in &mut self.threads {
            entry.thread.title = marker("thread-title", entry.thread.id);
            if let Some(origin) = &mut entry.thread.schedule {
                origin.name = marker("schedule-name", origin.id);
            }
            if let Some(origin) = &mut entry.thread.fork_origin {
                origin.title = marker("fork-source-title", entry.thread.id);
            }
            for turn in &mut entry.turns {
                if turn.error.as_ref().is_some_and(|value| !value.is_empty()) {
                    turn.error = Some(marker("turn-error", turn.id));
                }
            }
            for item in &mut entry.items {
                sanitize_item(item);
            }
            if let Some(goal) = &mut entry.goal {
                goal.goal = marker("goal", entry.thread.id);
                goal.last_reason =
                    redact_option("goal-reason", entry.thread.id, goal.last_reason.as_deref());
                goal.paused_reason = redact_option(
                    "goal-paused",
                    entry.thread.id,
                    goal.paused_reason.as_deref(),
                );
                goal.last_blocker_reason = redact_option(
                    "goal-blocker",
                    entry.thread.id,
                    goal.last_blocker_reason.as_deref(),
                );
                for (index, subgoal) in goal.subgoals.iter_mut().enumerate() {
                    if !subgoal.is_empty() {
                        *subgoal = marker("subgoal", format!("{}-{index}", entry.thread.id));
                    }
                }
                for criterion in &mut goal.criteria {
                    criterion.text = marker("criterion", &criterion.id);
                    criterion.evidence = redact_option(
                        "criterion-evidence",
                        &criterion.id,
                        criterion.evidence.as_deref(),
                    );
                }
            }
            if let Some(config) = &mut entry.loop_config
                && !config.prompt.is_empty()
            {
                config.prompt = marker("loop-prompt", entry.thread.id);
            }
        }
        self
    }
}

/// Whether `thread` came from a sanitized archive: a sanitized export always
/// replaces the title with its marker.
#[must_use]
pub fn is_sanitized_thread(thread: &Thread) -> bool {
    thread.title == marker("thread-title", thread.id)
}

fn marker(kind: &str, id: impl std::fmt::Display) -> String {
    format!("[redacted:{kind}:{id}]")
}

fn redact(value: &str, kind: &str, id: impl std::fmt::Display) -> String {
    if value.is_empty() {
        String::new()
    } else {
        marker(kind, id)
    }
}

fn redact_option(kind: &str, id: impl std::fmt::Display, value: Option<&str>) -> Option<String> {
    value.map(|value| redact(value, kind, id))
}

fn redacted_value(kind: &str, id: impl std::fmt::Display, value: &Value) -> Value {
    if value.is_null() {
        Value::Null
    } else {
        json!({"redacted": format!("{kind}:{id}")})
    }
}

/// The reviewer's note, its rule and the path or error a reason names all
/// describe the action the note is about. What kind of note it is (status,
/// risk, reason code, counts and seconds) stays.
fn sanitize_review(review: &mut PermissionReview, id: ItemId) {
    review.rule = redact_option("permission-review-rule", id, review.rule.as_deref());
    review.rationale = redact_option("permission-review", id, review.rationale.as_deref());
    match &mut review.reason {
        Some(
            ReviewReason::UnnamedDeletion { path }
            | ReviewReason::UnplacedDeletion { path }
            | ReviewReason::UnnamedOverwrite { path }
            | ReviewReason::UnplacedOverwrite { path },
        ) => {
            *path = redact(path, "permission-review-path", id);
        }
        Some(ReviewReason::Error { detail }) => {
            *detail = redact(detail, "permission-review-error", id);
        }
        _ => {}
    }
}

fn sanitize_todos(items: &mut [TodoEntry], id: ItemId) {
    for (index, item) in items.iter_mut().enumerate() {
        item.content = redact(&item.content, "todo", format!("{id}-{index}"));
    }
}

fn sanitize_checkpoint(checkpoint: &mut Option<TodoCheckpoint>, id: ItemId) {
    if let Some(checkpoint) = checkpoint {
        sanitize_todos(&mut checkpoint.todos, id);
    }
}

#[allow(clippy::too_many_lines)]
fn sanitize_item(item: &mut Item) {
    match &mut item.data {
        ItemData::UserMessage {
            text,
            attachments,
            command_line,
            ..
        } => {
            *text = redact(text, "user-message", item.id);
            if let Some(line) = command_line {
                *line = redact(line, "command-line", item.id);
            }
            for (index, attachment) in attachments.iter_mut().enumerate() {
                *attachment = redact(attachment, "attachment", format!("{}-{index}", item.id));
            }
        }
        ItemData::AssistantMessage { text } => {
            *text = redact(text, "assistant-message", item.id);
        }
        ItemData::Reasoning { text, .. } => *text = redact(text, "reasoning", item.id),
        ItemData::ToolCall {
            arguments,
            provider_metadata,
            ..
        } => {
            *arguments = redacted_value("tool-arguments", item.id, arguments);
            *provider_metadata =
                redacted_value("tool-provider-metadata", item.id, provider_metadata);
        }
        ItemData::ToolResult {
            output, metadata, ..
        } => {
            *output = redact(output, "tool-output", item.id);
            *metadata = redacted_value("tool-metadata", item.id, metadata);
        }
        ItemData::McpAction { server, detail, .. } => {
            *server = redact(server, "mcp-server", item.id);
            *detail = redacted_value("mcp-detail", item.id, detail);
        }
        ItemData::PermissionRequest {
            operation,
            detail,
            patterns,
            always_patterns,
            review,
            ..
        } => {
            if let Some(review) = review {
                sanitize_review(review, item.id);
            }
            *operation = redact(operation, "permission-operation", item.id);
            *detail = redacted_value("permission-detail", item.id, detail);
            for (index, pattern) in patterns.iter_mut().enumerate() {
                *pattern = redact(
                    pattern,
                    "permission-pattern",
                    format!("{}-{index}", item.id),
                );
            }
            for (index, pattern) in always_patterns.iter_mut().enumerate() {
                *pattern = redact(pattern, "permission-rule", format!("{}-{index}", item.id));
            }
        }
        ItemData::Question {
            prompt, choices, ..
        } => {
            *prompt = redact(prompt, "question", item.id);
            for (index, choice) in choices.iter_mut().enumerate() {
                *choice = redact(choice, "question-choice", format!("{}-{index}", item.id));
            }
        }
        ItemData::Compaction {
            summary,
            files_touched,
            outstanding,
            todo_checkpoint,
            ..
        } => {
            *summary = redact(summary, "compaction", item.id);
            for (index, file) in files_touched.iter_mut().enumerate() {
                *file = redact(file, "compaction-file", format!("{}-{index}", item.id));
            }
            for (index, task) in outstanding.iter_mut().enumerate() {
                *task = redact(task, "compaction-task", format!("{}-{index}", item.id));
            }
            sanitize_checkpoint(todo_checkpoint, item.id);
        }
        ItemData::Plan { markdown } => *markdown = redact(markdown, "plan", item.id),
        ItemData::Review { findings } => {
            for (index, finding) in findings.iter_mut().enumerate() {
                let id = format!("{}-{index}", item.id);
                finding.file = redact_option("review-file", &id, finding.file.as_deref());
                finding.explanation = redact(&finding.explanation, "review", id);
            }
        }
        ItemData::Error { message, .. } => *message = redact(message, "error", item.id),
        ItemData::MediaOmitted { names, reason, .. } => {
            for (index, name) in names.iter_mut().enumerate() {
                *name = redact(name, "omitted-media", format!("{}-{index}", item.id));
            }
            *reason = redact(reason, "omitted-media-reason", item.id);
        }
        ItemData::Todo { items } => sanitize_todos(items, item.id),
        ItemData::QueueConsumed { .. }
        | ItemData::TaskLink { .. }
        | ItemData::TaskState { .. }
        | ItemData::Usage { .. }
        | ItemData::Snapshot { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use super::*;
    use crate::{RequestId, TurnStatus};

    fn archive() -> SessionArchive {
        let root = ThreadId::new();
        let child = ThreadId::new();
        let turn = TurnId::new();
        let item = ItemId::new();
        let now = Utc::now();
        SessionArchive {
            format: SESSION_ARCHIVE_FORMAT.into(),
            schema_version: SESSION_ARCHIVE_SCHEMA_VERSION,
            source_version: "test".into(),
            exported_at: now,
            root_thread_id: root,
            sanitized: false,
            threads: vec![
                SessionArchiveThread {
                    thread: Thread {
                        schedule: None,
                        id: child,
                        title: "child secret".into(),
                        parent_id: Some(root),
                        is_subagent: true,
                        fork_origin: None,
                        archived: false,
                        agent: "build".into(),
                        provider: "echo".into(),
                        model: "echo-1".into(),
                        ultracode: false,
                        created_at: now,
                        updated_at: now,
                        revert: None,
                    },
                    turns: Vec::new(),
                    items: Vec::new(),
                    goal: None,
                    loop_config: None,
                },
                SessionArchiveThread {
                    thread: Thread {
                        schedule: None,
                        id: root,
                        title: "root secret".into(),
                        parent_id: None,
                        is_subagent: false,
                        fork_origin: None,
                        archived: false,
                        agent: "build".into(),
                        provider: "echo".into(),
                        model: "echo-1".into(),
                        ultracode: false,
                        created_at: now,
                        updated_at: now,
                        revert: None,
                    },
                    turns: vec![Turn {
                        id: turn,
                        thread_id: root,
                        request_id: RequestId::new(),
                        status: TurnStatus::Completed,
                        agent: "build".into(),
                        provider: "echo".into(),
                        model: "echo-1".into(),
                        ultracode: false,
                        error: None,
                        created_at: now,
                        updated_at: now,
                    }],
                    items: vec![Item {
                        id: item,
                        thread_id: root,
                        turn_id: Some(turn),
                        sequence: 1,
                        data: ItemData::ToolCall {
                            call_id: "call".into(),
                            name: "bash".into(),
                            arguments: json!({"command":"secret"}),
                            provider_metadata: json!({"trace":"secret"}),
                        },
                        created_at: now,
                    }],
                    goal: None,
                    loop_config: None,
                },
            ],
        }
    }

    #[test]
    fn validates_unordered_tree_and_returns_parent_first_order() {
        let archive = archive();
        archive.validate().unwrap();
        let order = archive.ordered_thread_ids().unwrap();
        assert_eq!(order[0], archive.root_thread_id);
        assert_eq!(order.len(), 2);
    }

    #[test]
    fn rejects_unknown_versions_and_broken_ownership() {
        let mut archive = archive();
        archive.schema_version += 1;
        assert!(archive.validate().unwrap_err().contains("schema version"));
        archive.schema_version = SESSION_ARCHIVE_SCHEMA_VERSION;
        archive.threads[1].items[0].thread_id = archive.threads[0].thread.id;
        assert!(archive.validate().unwrap_err().contains("different thread"));
    }

    #[test]
    fn sanitization_redacts_content_but_preserves_graph_and_tool_shape() {
        let mut original = archive();
        original.threads[0].thread.schedule = Some(crate::ScheduleOrigin {
            id: uuid::Uuid::now_v7(),
            name: "schedule secret".into(),
        });
        let archive = original.sanitize();
        archive.validate().unwrap();
        let json = serde_json::to_string(&archive).unwrap();
        assert!(archive.sanitized);
        assert!(!json.contains("root secret"));
        assert!(!json.contains("schedule secret"));
        assert!(!json.contains("child secret"));
        assert!(!json.contains("\"command\":\"secret\""));
        assert!(json.contains("\"name\":\"bash\""));
        assert!(json.contains("redacted"));
    }

    #[test]
    fn sanitization_redacts_what_a_review_note_names_and_keeps_its_code() {
        use crate::{PermissionReviewStatus, ReviewRisk};
        let mut original = archive();
        let root = &mut original.threads[1];
        let template = root.items[0].clone();
        for (index, reason) in [
            ReviewReason::UnnamedDeletion {
                path: "/Users/someone/secret-project".into(),
            },
            ReviewReason::Error {
                detail: "401 from https://internal.example/secret-endpoint".into(),
            },
            ReviewReason::Timeout { seconds: 60 },
            ReviewReason::UnnamedOverwrite {
                path: "/Users/someone/secret-project/notes.txt".into(),
            },
            ReviewReason::UnplacedOverwrite {
                path: "/Users/someone/secret-project/\"$F\"".into(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            let mut item = template.clone();
            item.id = ItemId::new();
            item.sequence = 2 + u64::try_from(index).unwrap();
            item.data = ItemData::PermissionRequest {
                permission_id: format!("p{index}"),
                operation: "bash:rm -rf /Users/someone/secret-project".into(),
                detail: json!({"command": "rm -rf /Users/someone/secret-project"}),
                permission: "bash".into(),
                patterns: Vec::new(),
                always_patterns: Vec::new(),
                review: Some(PermissionReview {
                    status: PermissionReviewStatus::Asked,
                    risk: Some(ReviewRisk::High),
                    rule: Some("Deletes secret-project".into()),
                    rationale: Some("It deletes secret-project.".into()),
                    reason: Some(reason),
                }),
            };
            root.items.push(item);
        }
        let archive = original.sanitize();
        archive.validate().unwrap();
        let json = serde_json::to_string(&archive).unwrap();
        assert!(!json.contains("secret-project"), "{json}");
        assert!(!json.contains("secret-endpoint"), "{json}");
        let reviews: Vec<_> = archive.threads[1]
            .items
            .iter()
            .filter_map(|item| match &item.data {
                ItemData::PermissionRequest { review, .. } => review.clone(),
                _ => None,
            })
            .collect();
        assert_eq!(reviews.len(), 5);
        for review in &reviews {
            assert_eq!(review.status, PermissionReviewStatus::Asked);
            assert_eq!(review.risk, Some(ReviewRisk::High));
            assert!(review.rule.as_deref().unwrap().starts_with("[redacted:"));
        }
        assert!(matches!(
            &reviews[0].reason,
            Some(ReviewReason::UnnamedDeletion { path }) if path.starts_with("[redacted:")
        ));
        assert!(matches!(
            &reviews[1].reason,
            Some(ReviewReason::Error { detail }) if detail.starts_with("[redacted:")
        ));
        assert_eq!(
            reviews[2].reason,
            Some(ReviewReason::Timeout { seconds: 60 })
        );
        assert!(matches!(
            &reviews[3].reason,
            Some(ReviewReason::UnnamedOverwrite { path }) if path.starts_with("[redacted:")
        ));
        assert!(matches!(
            &reviews[4].reason,
            Some(ReviewReason::UnplacedOverwrite { path }) if path.starts_with("[redacted:")
        ));
    }
}
