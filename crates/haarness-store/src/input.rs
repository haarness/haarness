use super::*;
use haarness_protocol::{InputQueue, InputQueueAction, PendingInput};
use rusqlite::{Transaction, TransactionBehavior};
use std::collections::HashSet;

pub(super) fn migrate(db: &Connection) -> CoreResult<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS input_queues(
        thread_id TEXT PRIMARY KEY REFERENCES threads(id), revision INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS pending_inputs(
        id TEXT PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES threads(id),
        payload TEXT NOT NULL, position INTEGER NOT NULL, state TEXT NOT NULL DEFAULT 'pending');
        CREATE INDEX IF NOT EXISTS pending_inputs_thread ON pending_inputs(thread_id,state,position);")
        .map_err(storage)
}

pub(super) fn read(db: &Connection, thread: ThreadId) -> CoreResult<InputQueue> {
    let revision = db
        .query_row(
            "SELECT revision FROM input_queues WHERE thread_id=?1",
            [thread.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(storage)?
        .unwrap_or(0);
    let mut stmt = db.prepare("SELECT payload FROM pending_inputs WHERE thread_id=?1 AND state='pending' ORDER BY position,id").map_err(storage)?;
    let rows = stmt
        .query_map([thread.to_string()], |row| row.get::<_, String>(0))
        .map_err(storage)?;
    let items = rows
        .map(|row| serde_json::from_str(&row.map_err(storage)?).map_err(storage))
        .collect::<CoreResult<_>>()?;
    Ok(InputQueue {
        revision: revision as u64,
        items,
    })
}

fn bump(db: &Connection, thread: ThreadId) -> CoreResult<()> {
    db.execute("INSERT INTO input_queues(thread_id,revision) VALUES(?1,1) ON CONFLICT(thread_id) DO UPDATE SET revision=revision+1", [thread.to_string()]).map_err(storage)?;
    Ok(())
}
pub(super) fn check_revision(db: &Connection, thread: ThreadId, revision: u64) -> CoreResult<()> {
    check(&read(db, thread)?, revision)
}
fn check(queue: &InputQueue, revision: u64) -> CoreResult<()> {
    if queue.revision != revision {
        return Err(CoreError::Conflict(
            "Pending messages changed; refresh and try again".into(),
        ));
    }
    Ok(())
}
fn pending(queue: &InputQueue, id: RequestId) -> CoreResult<&PendingInput> {
    queue
        .items
        .iter()
        .find(|item| item.id == id)
        .ok_or_else(|| CoreError::Conflict("This message has already been sent or removed".into()))
}
fn validate(input: &PendingInput) -> CoreResult<()> {
    if input.text.trim().is_empty() && input.attachments.is_empty() {
        return Err(CoreError::Invalid(
            "text or an attachment is required".into(),
        ));
    }
    Ok(())
}

pub(super) fn enqueue(
    db: &mut Connection,
    thread: ThreadId,
    input: PendingInput,
) -> CoreResult<InputQueue> {
    validate(&input)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)?;
    let owner: Option<String> = tx
        .query_row(
            "SELECT thread_id FROM pending_inputs WHERE id=?1",
            [input.id.to_string()],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)?;
    if let Some(owner) = owner {
        if owner != thread.to_string() {
            return Err(CoreError::Conflict(
                "Input id belongs to another conversation".into(),
            ));
        }
        // First admission wins, including retries after cancellation/delivery.
        return read(&tx, thread);
    }
    // A retry of a previously started ordinary turn must not run a second time.
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM turns WHERE thread_id=?1 AND request_id=?2)",
            params![thread.to_string(), input.id.to_string()],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if exists {
        return read(&tx, thread);
    }
    let position: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(position),0)+1 FROM pending_inputs WHERE thread_id=?1",
            [thread.to_string()],
            |r| r.get(0),
        )
        .map_err(storage)?;
    tx.execute(
        "INSERT INTO pending_inputs(id,thread_id,payload,position) VALUES(?1,?2,?3,?4)",
        params![
            input.id.to_string(),
            thread.to_string(),
            serde_json::to_string(&input).map_err(storage)?,
            position
        ],
    )
    .map_err(storage)?;
    bump(&tx, thread)?;
    let queue = read(&tx, thread)?;
    tx.commit().map_err(storage)?;
    Ok(queue)
}

pub(super) fn change(
    db: &mut Connection,
    thread: ThreadId,
    action: InputQueueAction,
) -> CoreResult<InputQueue> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)?;
    let queue = read(&tx, thread)?;
    match action {
        InputQueueAction::Edit {
            revision,
            id,
            text,
            attachments,
        } => {
            check(&queue, revision)?;
            let mut item = pending(&queue, id)?.clone();
            item.text = text;
            if let Some(attachments) = attachments {
                item.attachments = attachments;
            }
            validate(&item)?;
            tx.execute(
                "UPDATE pending_inputs SET payload=?2 WHERE id=?1",
                params![
                    id.to_string(),
                    serde_json::to_string(&item).map_err(storage)?
                ],
            )
            .map_err(storage)?;
        }
        InputQueueAction::Remove { revision, id } => {
            check(&queue, revision)?;
            pending(&queue, id)?;
            tx.execute(
                "UPDATE pending_inputs SET state='cancelled' WHERE id=?1",
                [id.to_string()],
            )
            .map_err(storage)?;
        }
        InputQueueAction::Reorder { revision, ids } => {
            check(&queue, revision)?;
            let expected: HashSet<_> = queue.items.iter().map(|item| item.id).collect();
            if ids.len() != expected.len()
                || ids.iter().copied().collect::<HashSet<_>>() != expected
            {
                return Err(CoreError::Invalid(
                    "Include every pending message exactly once".into(),
                ));
            }
            for (position, id) in ids.iter().enumerate() {
                tx.execute(
                    "UPDATE pending_inputs SET position=?2 WHERE id=?1",
                    params![id.to_string(), position as i64],
                )
                .map_err(storage)?;
            }
        }
        InputQueueAction::Steer { .. } => {
            return Err(CoreError::Invalid("Steering requires the runtime".into()));
        }
    }
    bump(&tx, thread)?;
    let queue = read(&tx, thread)?;
    tx.commit().map_err(storage)?;
    Ok(queue)
}

/// Keep a tombstone so retransmission cannot resurrect delivered/removed work.
pub(super) fn consume(
    tx: &Transaction<'_>,
    thread: ThreadId,
    id: RequestId,
) -> CoreResult<Option<PendingInput>> {
    let row: Option<(String, String, String)> = tx
        .query_row(
            "SELECT thread_id,state,payload FROM pending_inputs WHERE id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(storage)?;
    let Some((owner, state, payload)) = row else {
        return Ok(None);
    };
    if owner != thread.to_string() || state != "pending" {
        return Err(CoreError::Conflict(
            "This input is no longer pending".into(),
        ));
    }
    tx.execute(
        "UPDATE pending_inputs SET state='delivered' WHERE id=?1",
        [id.to_string()],
    )
    .map_err(storage)?;
    bump(tx, thread)?;
    Ok(Some(serde_json::from_str(&payload).map_err(storage)?))
}

pub(super) fn promote(
    db: &mut Connection,
    thread: ThreadId,
    turn: TurnId,
    selected: Option<(RequestId, u64)>,
) -> CoreResult<Option<Item>> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)?;
    let queue = read(&tx, thread)?;
    let input = if let Some((id, revision)) = selected {
        check(&queue, revision)?;
        pending(&queue, id)?
    } else {
        let Some(first) = queue.items.first() else {
            return Ok(None);
        };
        first
    };
    let input = consume(&tx, thread, input.id)?.expect("pending input checked in transaction");
    let now = Utc::now();
    let item = Item {
        id: ItemId::new(),
        thread_id: thread,
        turn_id: Some(turn),
        sequence: SqliteStore::next_sequence(&tx, thread)?,
        data: ItemData::UserMessage {
            text: input.text,
            attachments: input.attachments,
            queued: true,
            synthetic: false,
        },
        created_at: now,
    };
    tx.execute("INSERT INTO items(id,thread_id,turn_id,sequence,data_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![item.id.to_string(),thread.to_string(),turn.to_string(),item.sequence as i64,serde_json::to_string(&item.data).map_err(storage)?,time(now)]).map_err(storage)?;
    tx.execute(
        "UPDATE threads SET updated_at=?2 WHERE id=?1",
        params![thread.to_string(), time(now)],
    )
    .map_err(storage)?;
    tx.commit().map_err(storage)?;
    Ok(Some(item))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn message(text: &str) -> PendingInput {
        PendingInput {
            preview: None,
            reasoning_effort: None,
            id: RequestId::new(),
            text: text.into(),
            attachments: vec!["data:text/plain;base64,dGVzdA==".into()],
        }
    }
    #[tokio::test]
    async fn deferred_input_storage_is_durable_idempotent_and_thread_scoped() {
        let path = std::env::temp_dir().join(format!("haarness-input-{}.db", Uuid::now_v7()));
        let store = SqliteStore::open(&path).unwrap();
        let thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let other = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let mut original = message("first admission");
        original.preview = Some(true);
        let q = store
            .enqueue_input(thread.id, original.clone())
            .await
            .unwrap();
        let mut retry = original.clone();
        retry.text = "retry must not replace".into();
        let same = store.enqueue_input(thread.id, retry).await.unwrap();
        assert_eq!(same.revision, q.revision);
        assert_eq!(same.items[0].text, "first admission");
        assert!(
            store
                .enqueue_input(other.id, original.clone())
                .await
                .is_err()
        );
        assert!(store.input_queue(other.id).await.unwrap().items.is_empty());
        drop(store);
        let store = SqliteStore::open(&path).unwrap();
        let q = store.input_queue(thread.id).await.unwrap();
        assert_eq!(q.items[0].attachments, original.attachments);
        assert_eq!(q.items[0].preview, Some(true));
        let q = store
            .change_input_queue(
                thread.id,
                InputQueueAction::Edit {
                    revision: q.revision,
                    id: original.id,
                    text: "edited".into(),
                    attachments: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(q.items[0].attachments, original.attachments);
        assert_eq!(q.items[0].preview, Some(true));
        store
            .change_input_queue(
                thread.id,
                InputQueueAction::Remove {
                    revision: q.revision,
                    id: original.id,
                },
            )
            .await
            .unwrap();
        assert!(
            store
                .enqueue_input(thread.id, original)
                .await
                .unwrap()
                .items
                .is_empty()
        );
        drop(store);
        let _ = std::fs::remove_file(path);
    }
    #[tokio::test]
    async fn deferred_input_storage_rejects_stale_edits_and_invalid_reorders() {
        let store = SqliteStore::in_memory().unwrap();
        let thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let a = message("a");
        let b = message("b");
        store.enqueue_input(thread.id, a.clone()).await.unwrap();
        let q = store.enqueue_input(thread.id, b.clone()).await.unwrap();
        for ids in [vec![a.id], vec![a.id, a.id], vec![a.id, RequestId::new()]] {
            assert!(
                store
                    .change_input_queue(
                        thread.id,
                        InputQueueAction::Reorder {
                            revision: q.revision,
                            ids
                        }
                    )
                    .await
                    .is_err()
            );
        }
        let new = store
            .change_input_queue(
                thread.id,
                InputQueueAction::Reorder {
                    revision: q.revision,
                    ids: vec![b.id, a.id],
                },
            )
            .await
            .unwrap();
        assert_eq!(new.items[0].id, b.id);
        assert!(matches!(
            store
                .change_input_queue(
                    thread.id,
                    InputQueueAction::Edit {
                        revision: q.revision,
                        id: a.id,
                        text: "stale".into(),
                        attachments: None,
                    }
                )
                .await,
            Err(CoreError::Conflict(_))
        ));
        assert_eq!(
            store.input_queue(thread.id).await.unwrap().items[1].text,
            "a"
        );
    }
    #[tokio::test]
    async fn deferred_input_storage_concurrent_edit_has_one_winner() {
        let path = std::env::temp_dir().join(format!("haarness-input-race-{}.db", Uuid::now_v7()));
        let store = Arc::new(SqliteStore::open(&path).unwrap());
        let other = Arc::new(SqliteStore::open(&path).unwrap());
        let thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let item = message("initial");
        let q = store.enqueue_input(thread.id, item.clone()).await.unwrap();
        let action = InputQueueAction::Edit {
            revision: q.revision,
            id: item.id,
            text: "winner".into(),
            attachments: None,
        };
        let left = tokio::spawn({
            let store = store.clone();
            let action = action.clone();
            async move { store.change_input_queue(thread.id, action).await }
        });
        let right = tokio::spawn(async move { other.change_input_queue(thread.id, action).await });
        let (left, right) = (left.await.unwrap(), right.await.unwrap());
        assert_ne!(left.is_ok(), right.is_ok());
        assert_eq!(
            store.input_queue(thread.id).await.unwrap().revision,
            q.revision + 1
        );
        drop(store);
        let _ = std::fs::remove_file(path);
    }
}
