use super::{
    Connection, CoreError, CoreResult, Item, ItemData, ItemId, OptionalExtension, RequestId,
    SqliteStore, ThreadId, TurnId, Utc, params, storage, time,
};
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
        revision: u64::try_from(revision).map_err(storage)?,
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
                    params![id.to_string(), i64::try_from(position).map_err(storage)?],
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

/// Where a person's steer went: the turn it joined and its message there.
/// A record an older server kept names neither.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Steer {
    #[serde(default)]
    pub(super) turn: Option<TurnId>,
    #[serde(default)]
    pub(super) item: Option<ItemId>,
}

/// Records that request `request` was steered into a running turn of
/// `thread` as `item`: a tombstone no queue lists, so a retry of the same
/// request is found (`has_input_admission`, `steer`) and not delivered
/// twice. False when the request was already recorded (steered, queued or
/// removed).
pub(super) fn record_steer(
    db: &Connection,
    thread: ThreadId,
    request: RequestId,
    item: &Item,
) -> CoreResult<bool> {
    let place = Steer {
        turn: item.turn_id,
        item: Some(item.id),
    };
    let inserted = db
        .execute(
            "INSERT INTO pending_inputs(id,thread_id,payload,position,state) VALUES(?1,?2,?3,0,'steered')
             ON CONFLICT(id) DO NOTHING",
            params![
                request.to_string(),
                thread.to_string(),
                serde_json::to_string(&place).map_err(storage)?
            ],
        )
        .map_err(storage)?;
    Ok(inserted == 1)
}

/// Where request `request` of `thread` went, when a person steered it into a
/// running turn.
pub(super) fn steer(
    db: &Connection,
    thread: ThreadId,
    request: RequestId,
) -> CoreResult<Option<Steer>> {
    let payload: Option<String> = db
        .query_row(
            "SELECT payload FROM pending_inputs WHERE id=?1 AND thread_id=?2 AND state='steered'",
            params![request.to_string(), thread.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    // An older server's record is `{}`: a steer that names no place.
    Ok(payload.map(|payload| serde_json::from_str(&payload).unwrap_or_default()))
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
    // A steer's record only answers a retry; it is no queued input.
    let Some((owner, state, payload)) = row.filter(|(_, state, _)| state != "steered") else {
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
            command_line: None,
            hook: None,
        },
        created_at: now,
    };
    tx.execute("INSERT INTO items(id,thread_id,turn_id,sequence,data_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![item.id.to_string(),thread.to_string(),turn.to_string(),i64::try_from(item.sequence).map_err(storage)?,serde_json::to_string(&item.data).map_err(storage)?,time(now)]).map_err(storage)?;
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
    use crate::*;
    /// A test database in a folder of its own, removed with its journal files
    /// even when the test fails.
    struct Scratch(std::path::PathBuf);
    impl Scratch {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!("haarness-{label}-{}", Uuid::now_v7()));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn database(&self) -> std::path::PathBuf {
            self.0.join("store.db")
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn message(text: &str) -> PendingInput {
        PendingInput {
            preview: None,
            reasoning_effort: None,
            id: RequestId::new(),
            text: text.into(),
            attachments: vec!["data:text/plain;base64,dGVzdA==".into()],
            ..Default::default()
        }
    }
    #[tokio::test]
    async fn deferred_input_storage_is_durable_idempotent_and_thread_scoped() {
        let scratch = Scratch::new("input");
        let path = scratch.database();
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
        let scratch = Scratch::new("input-race");
        let path = scratch.database();
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

    fn turn_request(request: RequestId, text: &str) -> StartTurnRequest {
        StartTurnRequest {
            request_id: request,
            text: text.into(),
            attachments: Vec::new(),
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
    async fn a_steer_is_recorded_once_and_never_listed_promoted_or_taken_for_a_turn() {
        let store = SqliteStore::in_memory().unwrap();
        let thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let (turn, _, _) = store
            .start_turn(thread.id, turn_request(RequestId::new(), "running"))
            .await
            .unwrap();
        let steer = RequestId::new();
        let data = || ItemData::UserMessage {
            text: "steered".into(),
            attachments: Vec::new(),
            queued: true,
            synthetic: false,
            command_line: None,
            hook: None,
        };
        // Recorded with its item; the same request again appends nothing.
        assert!(
            store
                .append_steered_input(thread.id, turn.id, steer, data())
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .append_steered_input(thread.id, turn.id, steer, data())
                .await
                .unwrap()
                .is_none()
        );
        assert!(store.has_input_admission(thread.id, steer).await.unwrap());
        // No queue lists it, so nothing promotes it.
        let queue = store.input_queue(thread.id).await.unwrap();
        assert!(queue.items.is_empty());
        assert!(
            store
                .promote_input(thread.id, turn.id, None)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .promote_input(thread.id, turn.id, Some((steer, queue.revision)))
                .await
                .is_err()
        );
        // Nor does a turn take it: see the tests below.
        let steered = store
            .list_items(thread.id, None)
            .await
            .unwrap()
            .into_iter()
            .filter(|item| matches!(&item.data, ItemData::UserMessage { text, .. } if text == "steered"))
            .count();
        assert_eq!(steered, 1);
    }

    fn user_messages(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .filter_map(|item| match &item.data {
                ItemData::UserMessage { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// A chat with a running turn and request `steer` steered into it.
    async fn steered_chat(store: &SqliteStore, steer: RequestId) -> (ThreadId, Turn, Item) {
        let thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let (turn, _, _) = store
            .start_turn(thread.id, turn_request(RequestId::new(), "running"))
            .await
            .unwrap();
        let item = store
            .append_steered_input(
                thread.id,
                turn.id,
                steer,
                ItemData::UserMessage {
                    text: "Also update the docs".into(),
                    attachments: vec!["data:text/plain;base64,dGVzdA==".into()],
                    queued: true,
                    synthetic: false,
                    command_line: None,
                    hook: None,
                },
            )
            .await
            .unwrap()
            .unwrap();
        (thread.id, turn, item)
    }

    fn taken(found: Option<SteeredInput>) -> Option<TurnId> {
        match found {
            Some(SteeredInput::Taken(turn)) => Some(turn.id),
            _ => None,
        }
    }

    #[tokio::test]
    async fn a_steer_sent_again_as_a_turn_returns_its_turn_while_it_runs_and_once_read() {
        let store = SqliteStore::in_memory().unwrap();
        let steer = RequestId::new();
        let (thread, turn, item) = steered_chat(&store, steer).await;
        let again = || store.start_turn(thread, turn_request(steer, "Also update the docs"));
        // Its turn still runs: that turn.
        assert_eq!(
            taken(store.steered_input(thread, steer).await.unwrap()),
            Some(turn.id)
        );
        let (found, message, created) = again().await.unwrap();
        assert!(!created);
        assert_eq!((found.id, message.id), (turn.id, item.id));
        // A request takes it and the model answers; the turn ends.
        for data in [
            ItemData::QueueConsumed { item_id: item.id },
            ItemData::AssistantMessage {
                text: "Docs updated".into(),
            },
        ] {
            store
                .append_item(thread, Some(turn.id), data)
                .await
                .unwrap();
        }
        store
            .set_turn_status(turn.id, TurnStatus::Completed, None)
            .await
            .unwrap();
        assert_eq!(
            taken(store.steered_input(thread, steer).await.unwrap()),
            Some(turn.id)
        );
        for _ in 0..2 {
            let (found, message, created) = again().await.unwrap();
            assert!(!created);
            assert_eq!(found.id, turn.id);
            assert_eq!(found.status, TurnStatus::Completed);
            assert_eq!(message.id, item.id);
        }
        assert!(
            store
                .turn_for_request(thread, steer)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            user_messages(&store.list_items(thread, None).await.unwrap()),
            ["running", "Also update the docs"]
        );
    }

    #[tokio::test]
    async fn a_steer_its_turn_ended_without_reading_runs_once_as_stored_when_sent_again() {
        let store = SqliteStore::in_memory().unwrap();
        let steer = RequestId::new();
        let (thread, turn, item) = steered_chat(&store, steer).await;
        // The turn fails before the model writes a word after it, as the
        // runtime records it.
        for data in [
            ItemData::QueueConsumed { item_id: item.id },
            ItemData::Error {
                message: "Queued input could not be delivered".into(),
                retryable: true,
            },
        ] {
            store
                .append_item(thread, Some(turn.id), data)
                .await
                .unwrap();
        }
        store
            .set_turn_status(turn.id, TurnStatus::Failed, Some("fixture".into()))
            .await
            .unwrap();
        assert!(matches!(
            store.steered_input(thread, steer).await.unwrap(),
            Some(SteeredInput::Unread { message, consumed: true }) if message.id == item.id
        ));
        // Sent again, it runs once, as it was stored.
        let (rerun, prompt, created) = store
            .start_turn(thread, turn_request(steer, "words a retry changed"))
            .await
            .unwrap();
        assert!(created);
        assert_ne!(rerun.id, turn.id);
        assert_eq!(rerun.request_id, steer);
        assert!(matches!(
            &prompt.data,
            ItemData::UserMessage { text, attachments, queued: false, .. }
                if text == "Also update the docs" && attachments.len() == 1
        ));
        // Then every retry is that turn.
        assert_eq!(
            taken(store.steered_input(thread, steer).await.unwrap()),
            Some(rerun.id)
        );
        let (again, _, created) = store
            .start_turn(thread, turn_request(steer, "Also update the docs"))
            .await
            .unwrap();
        assert!(!created);
        assert_eq!(again.id, rerun.id);
        assert_eq!(
            user_messages(&store.list_items(thread, None).await.unwrap()),
            ["running", "Also update the docs", "Also update the docs"]
        );
    }

    #[tokio::test]
    async fn a_steer_no_request_received_is_read_once_a_later_turn_answers() {
        let store = SqliteStore::in_memory().unwrap();
        let steer = RequestId::new();
        let (thread, turn, item) = steered_chat(&store, steer).await;
        // The server stopped first: no request took it, and nothing marks it.
        store
            .set_turn_status(turn.id, TurnStatus::Interrupted, None)
            .await
            .unwrap();
        assert!(matches!(
            store.steered_input(thread, steer).await.unwrap(),
            Some(SteeredInput::Unread {
                consumed: false,
                ..
            })
        ));
        // Undone, it is taken as it is.
        let undo = |deleted: bool| {
            store
                .connection
                .lock()
                .execute(
                    "UPDATE items SET deleted=?2 WHERE id=?1",
                    params![item.id.to_string(), deleted],
                )
                .unwrap();
        };
        undo(true);
        assert_eq!(
            taken(store.steered_input(thread, steer).await.unwrap()),
            Some(turn.id)
        );
        undo(false);
        // Every request of a later turn ends with it, so the model has read
        // it once one of them is answered.
        let (later, _, _) = store
            .start_turn(thread, turn_request(RequestId::new(), "next"))
            .await
            .unwrap();
        store
            .append_item(
                thread,
                Some(later.id),
                ItemData::AssistantMessage {
                    text: "On it".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            taken(store.steered_input(thread, steer).await.unwrap()),
            Some(turn.id)
        );
        let (found, _, created) = store
            .start_turn(thread, turn_request(steer, "Also update the docs"))
            .await
            .unwrap();
        assert!(!created);
        assert_eq!(found.id, turn.id);
    }

    #[tokio::test]
    async fn a_steer_an_older_server_recorded_is_refused_as_a_turn_rather_than_run_again() {
        let store = SqliteStore::in_memory().unwrap();
        let thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let steer = RequestId::new();
        // Servers before this one kept a steer's record without its place.
        store
            .connection
            .lock()
            .execute(
                "INSERT INTO pending_inputs(id,thread_id,payload,position,state) VALUES(?1,?2,'{}',0,'steered')",
                params![steer.to_string(), thread.id.to_string()],
            )
            .unwrap();
        assert!(store.has_input_admission(thread.id, steer).await.unwrap());
        assert!(
            store
                .steered_input(thread.id, steer)
                .await
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            store
                .start_turn(thread.id, turn_request(steer, "steered before"))
                .await,
            Err(CoreError::Conflict(_))
        ));
        assert!(store.list_items(thread.id, None).await.unwrap().is_empty());
        assert!(store.last_turn(thread.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn recovery_settles_steering_no_request_took_as_a_failed_turn_does() {
        let store = SqliteStore::in_memory().unwrap();
        let steer = RequestId::new();
        let (thread, turn, unread) = steered_chat(&store, steer).await;
        // A second steer a request already took is left as it is.
        let read = store
            .append_steered_input(
                thread,
                turn.id,
                RequestId::new(),
                ItemData::UserMessage {
                    text: "read already".into(),
                    attachments: Vec::new(),
                    queued: true,
                    synthetic: false,
                    command_line: None,
                    hook: None,
                },
            )
            .await
            .unwrap()
            .unwrap();
        store
            .append_item(
                thread,
                Some(turn.id),
                ItemData::QueueConsumed { item_id: read.id },
            )
            .await
            .unwrap();
        // A chat stopped without steering gets no such notice.
        let quiet = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        store
            .start_turn(quiet.id, turn_request(RequestId::new(), "quiet"))
            .await
            .unwrap();
        let (recovered, written) = store
            .recover_interrupted_tasks(&|_| Vec::new())
            .await
            .unwrap();
        assert_eq!(recovered, 2);
        let notice = format!(
            "{}: server restarted during active turn",
            haarness_core::UNDELIVERED_INPUT
        );
        // Clients hear the marker and the notice; both come before the
        // restart's own notice in the chat.
        assert_eq!(written.len(), 2, "{written:#?}");
        assert!(
            matches!(written[0].data, ItemData::QueueConsumed { item_id } if item_id == unread.id)
        );
        assert!(
            matches!(&written[1].data, ItemData::Error { message, retryable: true } if *message == notice)
        );
        let items = store.list_items(thread, None).await.unwrap();
        let markers = |id: ItemId| {
            items
                .iter()
                .filter(|item| matches!(item.data, ItemData::QueueConsumed { item_id } if item_id == id))
                .count()
        };
        assert_eq!((markers(unread.id), markers(read.id)), (1, 1));
        let tail: Vec<_> = items.iter().rev().take(2).map(|item| &item.data).collect();
        assert!(
            matches!(tail[0], ItemData::Error { message, .. } if message == RECOVERED_TURN_MESSAGE)
        );
        assert!(matches!(tail[1], ItemData::Error { message, .. } if *message == notice));
        assert!(!store.list_items(quiet.id, None).await.unwrap().iter().any(
            |item| matches!(&item.data, ItemData::Error { message, .. } if *message == notice)
        ));
        // Nothing runs it; sent again, it runs once.
        assert!(matches!(
            store.steered_input(thread, steer).await.unwrap(),
            Some(SteeredInput::Unread { consumed: true, .. })
        ));
        let (rerun, _, created) = store
            .start_turn(thread, turn_request(steer, "Also update the docs"))
            .await
            .unwrap();
        assert!(created && rerun.request_id == steer);
        // Another restart (under that turn) settles nothing again.
        assert!(
            store
                .recover_interrupted_tasks(&|_| Vec::new())
                .await
                .unwrap()
                .1
                .is_empty()
        );
        let items = store.list_items(thread, None).await.unwrap();
        assert_eq!(
            items
                .iter()
                .filter(|item| matches!(item.data, ItemData::QueueConsumed { item_id } if item_id == unread.id))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn a_queued_message_sent_again_as_a_turn_runs_once() {
        let store = SqliteStore::in_memory().unwrap();
        let thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let queued = message("from the queue");
        store
            .enqueue_input(thread.id, queued.clone())
            .await
            .unwrap();
        // Still waiting, it is the message the turn runs, taken off the queue.
        let (turn, prompt, created) = store
            .start_turn(thread.id, turn_request(queued.id, "sent again"))
            .await
            .unwrap();
        assert!(created);
        assert!(
            matches!(&prompt.data, ItemData::UserMessage { text, .. } if text == "from the queue")
        );
        assert!(store.input_queue(thread.id).await.unwrap().items.is_empty());
        // Once more: that turn.
        let (again, _, created) = store
            .start_turn(thread.id, turn_request(queued.id, "sent again"))
            .await
            .unwrap();
        assert!(!created);
        assert_eq!(again.id, turn.id);
        // Removed from the queue, it is refused.
        let removed = message("removed");
        let queue = store
            .enqueue_input(thread.id, removed.clone())
            .await
            .unwrap();
        store
            .change_input_queue(
                thread.id,
                InputQueueAction::Remove {
                    revision: queue.revision,
                    id: removed.id,
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .start_turn(thread.id, turn_request(removed.id, "removed"))
                .await,
            Err(CoreError::Conflict(_))
        ));
        assert_eq!(
            user_messages(&store.list_items(thread.id, None).await.unwrap()),
            ["from the queue"]
        );
    }
}
