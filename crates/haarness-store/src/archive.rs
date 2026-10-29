use std::collections::{HashMap, HashSet, VecDeque};

use chrono::Utc;
use haarness_core::{CoreError, CoreResult};
use haarness_protocol::{
    GoalState, GoalStatus, Item, ItemData, SESSION_ARCHIVE_FORMAT, SESSION_ARCHIVE_SCHEMA_VERSION,
    SessionArchive, SessionArchiveImportResult, SessionArchiveThread, ThreadId, Turn, TurnStatus,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{SqliteStore, decode_item, decode_thread, decode_turn, raw_item, raw_thread, raw_turn};

impl SqliteStore {
    pub(super) fn dump_session_archive(
        &self,
        root_id: ThreadId,
        sanitize: bool,
    ) -> CoreResult<SessionArchive> {
        let connection = self.connection.lock();
        let mut statement = connection
            .prepare(
                "SELECT id,title,parent_id,archived,agent,provider,model,created_at,updated_at,revert_sequence,revert_snapshot,revert_base_snapshot,ultracode,is_subagent,fork_origin_json,schedule_json,directory
                 FROM threads ORDER BY created_at,id",
            )
            .map_err(super::storage)?;
        let rows = statement
            .query_map([], raw_thread)
            .map_err(super::storage)?;
        let all = rows
            .map(|row| row.map_err(super::storage).and_then(decode_thread))
            .collect::<CoreResult<Vec<_>>>()?;
        let root = all
            .iter()
            .find(|thread| thread.id == root_id)
            .ok_or_else(|| CoreError::NotFound(format!("thread {root_id}")))?;

        let mut children = HashMap::<ThreadId, Vec<ThreadId>>::new();
        let by_id = all
            .iter()
            .map(|thread| (thread.id, thread))
            .collect::<HashMap<_, _>>();
        for thread in &all {
            if let Some(parent) = thread.parent_id {
                children.entry(parent).or_default().push(thread.id);
            }
        }
        for values in children.values_mut() {
            values.sort_by_key(ToString::to_string);
        }
        let mut queue = VecDeque::from([root.id]);
        let mut threads = Vec::new();
        while let Some(id) = queue.pop_front() {
            let source = *by_id.get(&id).ok_or_else(|| {
                CoreError::Storage(format!("thread {id} disappeared during export"))
            })?;
            queue.extend(children.remove(&id).unwrap_or_default());

            let limit = source.revert.as_ref().map(|revert| revert.sequence);
            let mut item_statement = connection
                .prepare(
                    "SELECT id,thread_id,turn_id,sequence,data_json,created_at FROM items
                     WHERE thread_id=?1 AND deleted=0 AND (?2 IS NULL OR sequence<=?2)
                     ORDER BY sequence",
                )
                .map_err(super::storage)?;
            let rows = item_statement
                .query_map(
                    params![
                        id.to_string(),
                        limit
                            .map(i64::try_from)
                            .transpose()
                            .map_err(super::storage)?
                    ],
                    raw_item,
                )
                .map_err(super::storage)?;
            let mut items = rows
                .map(|row| row.map_err(super::storage).and_then(decode_item))
                .collect::<CoreResult<Vec<_>>>()?;
            items.retain(|item| !matches!(&item.data, ItemData::Snapshot { .. }));
            for item in &mut items {
                normalize_task_state(item);
            }

            let referenced = items
                .iter()
                .filter_map(|item| item.turn_id)
                .collect::<HashSet<_>>();
            let mut turn_statement = connection
                .prepare(
                    "SELECT id,thread_id,request_id,status,agent,provider,model,error,created_at,updated_at,ultracode
                     FROM turns WHERE thread_id=?1 ORDER BY created_at,id",
                )
                .map_err(super::storage)?;
            let rows = turn_statement
                .query_map([id.to_string()], raw_turn)
                .map_err(super::storage)?;
            let mut turns = rows
                .map(|row| row.map_err(super::storage).and_then(decode_turn))
                .collect::<CoreResult<Vec<_>>>()?;
            turns.retain(|turn| referenced.contains(&turn.id));
            for turn in &mut turns {
                normalize_turn(turn);
            }

            let (goal, loop_config) = archive_extras(&connection, id)?;
            let mut thread = source.clone();
            if id == root_id {
                thread.parent_id = None;
                thread.is_subagent = false;
            }
            thread.revert = None;
            // A chat's own folder is a place on this computer; an imported
            // chat works in the project folder of the server it comes into.
            thread.directory = None;
            threads.push(SessionArchiveThread {
                thread,
                turns,
                items,
                goal,
                loop_config,
            });
        }

        let archive = SessionArchive {
            format: SESSION_ARCHIVE_FORMAT.into(),
            schema_version: SESSION_ARCHIVE_SCHEMA_VERSION,
            source_version: env!("CARGO_PKG_VERSION").into(),
            exported_at: Utc::now(),
            root_thread_id: root_id,
            sanitized: false,
            threads,
        };
        archive.validate().map_err(CoreError::Storage)?;
        Ok(if sanitize {
            archive.sanitize()
        } else {
            archive
        })
    }

    pub(super) fn restore_session_archive(
        &self,
        archive: SessionArchive,
    ) -> CoreResult<SessionArchiveImportResult> {
        let order = archive.ordered_thread_ids().map_err(CoreError::Invalid)?;
        let entries = archive
            .threads
            .iter()
            .map(|entry| (entry.thread.id, entry))
            .collect::<HashMap<_, _>>();
        let mut connection = self.connection.lock();
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(super::storage)?;

        // Older archives predate is_subagent. Infer only actual task links,
        // never all parent IDs (which also describe independent forks).
        let delegated = archive
            .threads
            .iter()
            .flat_map(|entry| {
                entry.items.iter().filter_map(|item| match &item.data {
                    ItemData::TaskLink {
                        child_thread_id, ..
                    }
                    | ItemData::TaskState {
                        child_thread_id, ..
                    } if entries
                        .get(child_thread_id)
                        .is_some_and(|child| child.thread.parent_id == Some(entry.thread.id)) =>
                    {
                        Some(*child_thread_id)
                    }
                    _ => None,
                })
            })
            .collect::<HashSet<_>>();
        let mut created = Vec::new();
        let mut merged = Vec::new();
        // Threads an earlier sanitized import left with redaction markers: a
        // full archive of the same chats brings their real content back.
        let mut restored = Vec::new();
        for id in &order {
            let entry = entries
                .get(id)
                .ok_or_else(|| CoreError::Invalid(format!("archive thread {id} is missing")))?;
            let current = tx
                .query_row(
                    "SELECT id,title,parent_id,archived,agent,provider,model,created_at,updated_at,revert_sequence,revert_snapshot,revert_base_snapshot,ultracode,is_subagent,fork_origin_json,schedule_json,directory
                     FROM threads WHERE id=?1",
                    [id.to_string()],
                    raw_thread,
                )
                .optional()
                .map_err(super::storage)?
                .map(decode_thread)
                .transpose()?;
            if let Some(current) = current {
                if *id != archive.root_thread_id && current.parent_id != entry.thread.parent_id {
                    return Err(CoreError::Conflict(format!(
                        "thread {id} already exists under a different parent"
                    )));
                }
                merged.push(*id);
                if !archive.sanitized && haarness_protocol::is_sanitized_thread(&current) {
                    tx.execute(
                        "UPDATE threads SET title=?2,fork_origin_json=?3,schedule_json=?4 WHERE id=?1",
                        params![
                            id.to_string(),
                            entry.thread.title,
                            entry.thread.fork_origin.as_ref().map(serde_json::to_string).transpose().map_err(super::storage)?,
                            entry.thread.schedule.as_ref().map(serde_json::to_string).transpose().map_err(super::storage)?,
                        ],
                    )
                    .map_err(super::storage)?;
                    restored.push(*id);
                }
                continue;
            }
            tx.execute(
                "INSERT INTO threads(id,title,parent_id,archived,agent,provider,model,created_at,updated_at,ultracode,is_subagent,fork_origin_json,schedule_json,directory)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                params![
                    id.to_string(),
                    entry.thread.title,
                    entry.thread.parent_id.map(|parent| parent.to_string()),
                    entry.thread.archived,
                    entry.thread.agent,
                    entry.thread.provider,
                    entry.thread.model,
                    super::time(entry.thread.created_at),
                    super::time(entry.thread.updated_at),
                    entry.thread.ultracode,
                    entry.thread.parent_id.is_some() && (entry.thread.is_subagent || delegated.contains(id)),
                    entry.thread.fork_origin.as_ref().map(serde_json::to_string).transpose().map_err(super::storage)?,
                    entry.thread.schedule.as_ref().map(serde_json::to_string).transpose().map_err(super::storage)?,
                    // The runtime names the folder an imported chat works in.
                    entry.thread.directory,
                ],
            )
            .map_err(super::storage)?;
            created.push(*id);
        }

        for id in &order {
            let entry = entries[id];
            for source in &entry.turns {
                let mut turn = source.clone();
                normalize_turn(&mut turn);
                if let Some(owner) = tx
                    .query_row(
                        "SELECT thread_id FROM turns WHERE id=?1",
                        [turn.id.to_string()],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(super::storage)?
                {
                    if owner != id.to_string() {
                        return Err(CoreError::Conflict(format!(
                            "turn {} already belongs to another thread",
                            turn.id
                        )));
                    }
                    if restored.contains(id) {
                        tx.execute(
                            "UPDATE turns SET error=?2 WHERE id=?1",
                            params![turn.id.to_string(), turn.error],
                        )
                        .map_err(super::storage)?;
                    }
                    continue;
                }
                if let Some(existing) = tx
                    .query_row(
                        "SELECT id FROM turns WHERE thread_id=?1 AND request_id=?2",
                        params![id.to_string(), turn.request_id.to_string()],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(super::storage)?
                {
                    return Err(CoreError::Conflict(format!(
                        "request {} in thread {id} already belongs to turn {existing}",
                        turn.request_id
                    )));
                }
                tx.execute(
                    "INSERT INTO turns(id,thread_id,request_id,status,agent,provider,model,error,created_at,updated_at,ultracode)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![
                        turn.id.to_string(),
                        id.to_string(),
                        turn.request_id.to_string(),
                        super::status(turn.status),
                        turn.agent,
                        turn.provider,
                        turn.model,
                        turn.error,
                        super::time(turn.created_at),
                        super::time(turn.updated_at),
                        turn.ultracode,
                    ],
                )
                .map_err(super::storage)?;
            }

            for source in &entry.items {
                let mut item = source.clone();
                normalize_task_state(&mut item);
                if let Some((owner, sequence)) = tx
                    .query_row(
                        "SELECT thread_id,sequence FROM items WHERE id=?1",
                        [item.id.to_string()],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                    )
                    .optional()
                    .map_err(super::storage)?
                {
                    if owner != id.to_string()
                        || u64::try_from(sequence).map_err(super::storage)? != item.sequence
                    {
                        return Err(CoreError::Conflict(format!(
                            "item {} already belongs to another transcript location",
                            item.id
                        )));
                    }
                    if restored.contains(id) {
                        tx.execute(
                            "UPDATE items SET data_json=?2 WHERE id=?1",
                            params![
                                item.id.to_string(),
                                serde_json::to_string(&item.data).map_err(super::storage)?,
                            ],
                        )
                        .map_err(super::storage)?;
                    }
                    continue;
                }
                if let Some(existing) = tx
                    .query_row(
                        "SELECT id FROM items WHERE thread_id=?1 AND sequence=?2",
                        params![
                            id.to_string(),
                            i64::try_from(item.sequence).map_err(super::storage)?
                        ],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(super::storage)?
                {
                    return Err(CoreError::Conflict(format!(
                        "sequence {} in thread {id} already belongs to item {existing}",
                        item.sequence
                    )));
                }
                tx.execute(
                    "INSERT INTO items(id,thread_id,turn_id,sequence,data_json,created_at,deleted)
                     VALUES(?1,?2,?3,?4,?5,?6,0)",
                    params![
                        item.id.to_string(),
                        id.to_string(),
                        item.turn_id.map(|turn| turn.to_string()),
                        i64::try_from(item.sequence).map_err(super::storage)?,
                        serde_json::to_string(&item.data).map_err(super::storage)?,
                        super::time(item.created_at),
                    ],
                )
                .map_err(super::storage)?;
            }

            if created.contains(id) || restored.contains(id) {
                let goal = entry.goal.clone().map(pause_imported_goal);
                let goal_json = goal
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(super::storage)?;
                let loop_json = entry
                    .loop_config
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(super::storage)?;
                if goal_json.is_some() || loop_json.is_some() {
                    tx.execute(
                        "INSERT INTO thread_extras(thread_id,goal,goal_json,loop_json)
                         VALUES(?1,?2,?3,?4)
                         ON CONFLICT(thread_id) DO UPDATE SET
                           goal=excluded.goal,goal_json=excluded.goal_json,loop_json=excluded.loop_json",
                        params![
                            id.to_string(),
                            goal.as_ref().map(|goal| goal.goal.as_str()),
                            goal_json,
                            loop_json,
                        ],
                    )
                    .map_err(super::storage)?;
                }
            }
        }

        tx.commit().map_err(super::storage)?;
        let mut threads = Vec::with_capacity(order.len());
        for id in order {
            let raw = connection
                .query_row(
                    "SELECT id,title,parent_id,archived,agent,provider,model,created_at,updated_at,revert_sequence,revert_snapshot,revert_base_snapshot,ultracode,is_subagent,fork_origin_json,schedule_json,directory
                     FROM threads WHERE id=?1",
                    [id.to_string()],
                    raw_thread,
                )
                .map_err(super::storage)?;
            threads.push(decode_thread(raw)?);
        }
        let root = threads
            .iter()
            .find(|thread| thread.id == archive.root_thread_id)
            .cloned()
            .ok_or_else(|| CoreError::Storage("imported archive root disappeared".into()))?;
        Ok(SessionArchiveImportResult {
            root,
            threads,
            created,
            merged,
            restored,
        })
    }
}

fn archive_extras(
    connection: &rusqlite::Connection,
    id: ThreadId,
) -> CoreResult<(Option<GoalState>, Option<haarness_protocol::LoopConfig>)> {
    let row = connection
        .query_row(
            "SELECT goal,goal_json,loop_json FROM thread_extras WHERE thread_id=?1",
            [id.to_string()],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()
        .map_err(super::storage)?;
    let Some((legacy, goal, loop_config)) = row else {
        return Ok((None, None));
    };
    let goal = if let Some(goal) = goal {
        Some(serde_json::from_str(&goal).map_err(super::storage)?)
    } else {
        legacy.map(haarness_core::legacy_goal_state)
    };
    let loop_config = loop_config
        .map(|value| serde_json::from_str(&value).map_err(super::storage))
        .transpose()?;
    Ok((goal, loop_config))
}

fn normalize_turn(turn: &mut Turn) {
    if matches!(turn.status, TurnStatus::Queued | TurnStatus::Running) {
        turn.status = TurnStatus::Interrupted;
        turn.error = Some("session archive captured this turn while it was active".into());
        turn.updated_at = Utc::now();
    }
}

fn normalize_task_state(item: &mut Item) {
    if let ItemData::TaskState { state, .. } = &mut item.data
        && matches!(
            state.as_str(),
            "running" | "background" | "waiting_permission"
        )
    {
        *state = "interrupted".into();
    }
    // An imported chat runs none of the commands it holds.
    if let ItemData::CommandState { state, .. } = &mut item.data
        && state.is_live()
    {
        *state = haarness_protocol::ShellCommandState::Interrupted;
    }
}

fn pause_imported_goal(mut goal: GoalState) -> GoalState {
    if goal.status == GoalStatus::Active {
        goal.status = GoalStatus::Paused;
        goal.paused_reason = Some("session-imported".into());
        goal.awaiting_user_input = true;
        goal.updated_at = Utc::now();
    }
    goal
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use haarness_core::ThreadRepository;
    use haarness_protocol::{
        CreateThreadRequest, LoopConfig, RequestId, StartTurnRequest, TurnStatus,
    };

    use super::*;

    fn turn_request(text: &str) -> StartTurnRequest {
        StartTurnRequest {
            request_id: RequestId::new(),
            text: text.into(),
            attachments: vec!["/private/secret.png".into()],
            resources: Vec::new(),
            synthetic: false,
            auto_approve: false,
            agent: None,
            provider: None,
            model: None,
            reasoning_effort: None,
            tools: None,
            command: None,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn exports_and_idempotently_restores_complete_session_tree() {
        let source = SqliteStore::in_memory().unwrap();
        let root = source
            .create_thread(CreateThreadRequest {
                title: Some("portable root secret".into()),
                ..CreateThreadRequest::default()
            })
            .await
            .unwrap();
        let (turn, _, _) = source
            .start_turn(root.id, turn_request("root transcript secret"))
            .await
            .unwrap();
        source
            .append_item(
                root.id,
                Some(turn.id),
                ItemData::AssistantMessage {
                    text: "root reply secret".into(),
                },
            )
            .await
            .unwrap();
        source
            .append_item(
                root.id,
                Some(turn.id),
                ItemData::Snapshot {
                    snapshot_id: "host-only-snapshot".into(),
                    file_count: 1,
                    boundary_sequence: Some(2),
                    base_snapshot_id: None,
                },
            )
            .await
            .unwrap();
        source
            .set_turn_status(turn.id, TurnStatus::Completed, None)
            .await
            .unwrap();
        source
            .set_goal(root.id, Some("finish portable archives".into()))
            .await
            .unwrap();
        source
            .set_loop(
                root.id,
                Some(LoopConfig {
                    prompt: "loop prompt secret".into(),
                    interval_seconds: 300,
                    expires_at: Utc::now() + Duration::days(1),
                }),
            )
            .await
            .unwrap();

        let child = source
            .fork_thread(root.id, Some("portable child secret".into()), None)
            .await
            .unwrap();
        source
            .append_item(
                child.id,
                None,
                ItemData::UserMessage {
                    text: "child transcript secret".into(),
                    attachments: Vec::new(),
                    queued: false,
                    synthetic: false,
                    command_line: None,
                },
            )
            .await
            .unwrap();

        let archive = source.export_session_archive(root.id, false).await.unwrap();
        assert_eq!(archive.root_thread_id, root.id);
        assert_eq!(archive.threads.len(), 2);
        assert!(archive.threads.iter().all(|entry| {
            entry
                .items
                .iter()
                .all(|item| !matches!(&item.data, ItemData::Snapshot { .. }))
        }));
        let encoded = serde_json::to_string(&archive).unwrap();
        assert!(encoded.contains("root transcript secret"));
        assert!(encoded.contains("child transcript secret"));
        assert!(!encoded.contains("host-only-snapshot"));

        let sanitized = source.export_session_archive(root.id, true).await.unwrap();
        let sanitized_json = serde_json::to_string(&sanitized).unwrap();
        assert!(sanitized.sanitized);
        assert!(!sanitized_json.contains("secret"));
        assert!(!sanitized_json.contains("/private/secret.png"));

        let target = SqliteStore::in_memory().unwrap();
        let first = target
            .import_session_archive(archive.clone())
            .await
            .unwrap();
        assert_eq!(first.root.id, root.id);
        assert_eq!(first.created.len(), 2);
        assert!(first.merged.is_empty());
        assert_eq!(
            target.get_thread(child.id).await.unwrap().parent_id,
            Some(root.id)
        );
        assert!(
            target
                .list_items(child.id, None)
                .await
                .unwrap()
                .iter()
                .any(|item| matches!(
                    &item.data,
                    ItemData::UserMessage { text, .. } if text == "child transcript secret"
                ))
        );
        let goal = target.goal_state(root.id).await.unwrap().unwrap();
        assert_eq!(goal.status, GoalStatus::Paused);
        assert_eq!(goal.paused_reason.as_deref(), Some("session-imported"));
        assert!(goal.awaiting_user_input);

        let second = target.import_session_archive(archive).await.unwrap();
        assert!(second.created.is_empty());
        assert_eq!(second.merged.len(), 2);
        assert_eq!(target.list_threads(true).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_full_archive_replaces_the_markers_of_an_earlier_sanitized_import() {
        let source = SqliteStore::in_memory().unwrap();
        let root = source
            .create_thread(CreateThreadRequest {
                title: Some("Real title".into()),
                ..CreateThreadRequest::default()
            })
            .await
            .unwrap();
        let (turn, _, _) = source
            .start_turn(root.id, turn_request("real question"))
            .await
            .unwrap();
        source
            .append_item(
                root.id,
                Some(turn.id),
                ItemData::AssistantMessage {
                    text: "real answer".into(),
                },
            )
            .await
            .unwrap();
        source
            .set_goal(root.id, Some("real goal".into()))
            .await
            .unwrap();
        let full = source.export_session_archive(root.id, false).await.unwrap();
        let sanitized = source.export_session_archive(root.id, true).await.unwrap();

        let target = SqliteStore::in_memory().unwrap();
        target
            .import_session_archive(sanitized.clone())
            .await
            .unwrap();
        assert!(
            target
                .get_thread(root.id)
                .await
                .unwrap()
                .title
                .starts_with("[redacted:")
        );
        // A sanitized archive never overwrites anything.
        let again = target.import_session_archive(sanitized).await.unwrap();
        assert!(again.restored.is_empty());

        let result = target.import_session_archive(full.clone()).await.unwrap();
        assert_eq!(result.merged, [root.id]);
        assert_eq!(result.restored, [root.id]);
        assert_eq!(result.root.title, "Real title");
        let texts = target
            .list_items(root.id, None)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|item| match item.data {
                ItemData::UserMessage { text, .. } | ItemData::AssistantMessage { text } => {
                    Some(text)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(texts, ["real question", "real answer"]);
        assert_eq!(
            target.goal_state(root.id).await.unwrap().unwrap().goal,
            "real goal"
        );
        // Real content is never touched again.
        let last = target.import_session_archive(full).await.unwrap();
        assert!(last.restored.is_empty());
    }

    #[tokio::test]
    async fn rejects_invalid_archive_before_writing_any_threads() {
        let source = SqliteStore::in_memory().unwrap();
        let root = source
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let mut archive = source.export_session_archive(root.id, false).await.unwrap();
        archive.schema_version += 1;

        let target = SqliteStore::in_memory().unwrap();
        assert!(matches!(
            target.import_session_archive(archive).await,
            Err(CoreError::Invalid(_))
        ));
        assert!(target.list_threads(true).await.unwrap().is_empty());
    }
}
