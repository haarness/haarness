//! SQLite persistence adapter. The database is the durable source of truth.

mod approvals;
pub use approvals::RepositoryApprovals;
mod archive;
mod input;
mod snapshots;
mod workflows;
pub use snapshots::SqliteSnapshotStore;

use std::{collections::HashMap, path::Path, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use haarness_core::{CoreError, CoreResult, StrandedReport, StrandedTask, ThreadRepository};
use haarness_protocol::{
    CompactRequest, CreateThreadRequest, GoalState, Item, ItemData, ItemId, RequestId,
    SessionArchive, SessionArchiveImportResult, StartTurnRequest, Thread, ThreadId, Turn, TurnId,
    TurnStatus, UpdateThreadRequest,
};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

const RECOVERED_TURN_MESSAGE: &str = "Haarness restarted while this turn was running. Work already recorded in this session is preserved. In-flight tool calls were not replayed automatically; retry or continue when ready.";

#[derive(Clone)]
pub struct SqliteStore {
    connection: Arc<Mutex<Connection>>,
}

/// The prompt of a turn a retried start returns: read by its chat as well as
/// its turn, so it goes through the chat's item index instead of reading
/// every item in the database.
const RETRIED_TURN_PROMPT: &str =
    "SELECT id,thread_id,turn_id,sequence,data_json,created_at FROM items
     WHERE thread_id=?1 AND turn_id=?2 ORDER BY sequence LIMIT 1";

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> CoreResult<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent).map_err(storage)?;
        }
        let connection = Connection::open(path).map_err(storage)?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(storage)?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(storage)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(storage)?;
        let store = Self {
            connection: Arc::new(Mutex::new(connection)),
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn in_memory() -> CoreResult<Self> {
        let connection = Connection::open_in_memory().map_err(storage)?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(storage)?;
        let store = Self {
            connection: Arc::new(Mutex::new(connection)),
        };
        store.migrate()?;
        Ok(store)
    }

    fn upgrade_legacy_goal_if_current(
        connection: &Connection,
        id: ThreadId,
        expected_goal: &str,
        state: &GoalState,
    ) -> CoreResult<bool> {
        let json = serde_json::to_string(state).map_err(storage)?;
        let upgraded = connection
            .execute(
                "UPDATE thread_extras SET goal_json=?3
                 WHERE thread_id=?1 AND goal=?2 AND goal_json IS NULL",
                params![id.to_string(), expected_goal, json],
            )
            .map_err(storage)?;
        Ok(upgraded == 1)
    }

    /// Reconcile parent-side task checkpoints with terminal child turns.
    ///
    /// Background-task runners are process-local, while their threads, turns,
    /// and parent task checkpoints are durable. A hard stop can therefore land
    /// after the child turn becomes terminal but before its runner appends the
    /// matching parent `TaskState`. Without this startup pass, clients rebuild
    /// an agent that appears to run forever even though no process owns it.
    ///
    /// The chat that launched a background task among them was told its
    /// result would be delivered, and it never will be: `report`'s items are
    /// written there in the same transaction, unless that chat already holds
    /// the result (a delivery whose own checkpoint was lost or raced) or is
    /// archived. Returns the items written.
    fn reconcile_task_states(
        tx: &rusqlite::Transaction<'_>,
        updated_at: &str,
        report: &StrandedReport<'_>,
    ) -> CoreResult<Vec<Item>> {
        struct Checkpoint {
            parent_thread_id: ThreadId,
            parent_turn_id: Option<TurnId>,
            call_id: String,
            child_thread_id: ThreadId,
            child_turn_id: TurnId,
            state: String,
            /// The first checkpoint of this run: a result delivered after it
            /// belongs to this run.
            since: u64,
        }

        let mut latest = HashMap::<ThreadId, Checkpoint>::new();
        let mut first = HashMap::<TurnId, u64>::new();
        {
            let mut statement = tx
                .prepare(
                    "SELECT thread_id,turn_id,data_json,sequence FROM items
                     WHERE deleted=0 AND json_extract(data_json,'$.type')='task_state'
                     ORDER BY thread_id,sequence",
                )
                .map_err(storage)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })
                .map_err(storage)?;
            for row in rows {
                let (raw_parent_thread_id, raw_parent_turn_id, raw_data, sequence) =
                    row.map_err(storage)?;
                let ItemData::TaskState {
                    call_id,
                    child_thread_id,
                    child_turn_id,
                    state,
                } = serde_json::from_str(&raw_data).map_err(storage)?
                else {
                    continue;
                };
                latest.insert(
                    child_thread_id,
                    Checkpoint {
                        parent_thread_id: thread_id(&raw_parent_thread_id)?,
                        parent_turn_id: raw_parent_turn_id.as_deref().map(turn_id).transpose()?,
                        call_id,
                        child_thread_id,
                        child_turn_id,
                        state,
                        since: *first
                            .entry(child_turn_id)
                            .or_insert(u64::try_from(sequence).map_err(storage)?),
                    },
                );
            }
        }

        let mut written = Vec::new();
        for checkpoint in latest.into_values() {
            if !matches!(
                checkpoint.state.as_str(),
                "running" | "background" | "waiting_permission"
            ) {
                continue;
            }
            let child_status = tx
                .query_row(
                    "SELECT status FROM turns WHERE id=?1 AND thread_id=?2",
                    params![
                        checkpoint.child_turn_id.to_string(),
                        checkpoint.child_thread_id.to_string()
                    ],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(storage)?;
            let Some(state) = child_status.as_deref().and_then(|status| match status {
                "completed" => Some("done"),
                "failed" => Some("failed"),
                "interrupted" => Some("interrupted"),
                _ => None,
            }) else {
                continue;
            };
            let stranded = if checkpoint.state == "background" {
                let task = StrandedTask {
                    parent_thread_id: checkpoint.parent_thread_id,
                    parent_turn_id: checkpoint.parent_turn_id,
                    call_id: checkpoint.call_id.clone(),
                    child_thread_id: checkpoint.child_thread_id,
                    child_turn_id: checkpoint.child_turn_id,
                    state: state.into(),
                    description: String::new(),
                    reply: None,
                    error: None,
                };
                Self::stranded_task(tx, task, checkpoint.since)?
            } else {
                None
            };
            let data = ItemData::TaskState {
                call_id: checkpoint.call_id,
                child_thread_id: checkpoint.child_thread_id,
                child_turn_id: checkpoint.child_turn_id,
                state: state.into(),
            };
            let sequence = Self::next_sequence(tx, checkpoint.parent_thread_id)?;
            tx.execute(
                "INSERT INTO items(id,thread_id,turn_id,sequence,data_json,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    ItemId::new().to_string(),
                    checkpoint.parent_thread_id.to_string(),
                    checkpoint.parent_turn_id.map(|id| id.to_string()),
                    i64::try_from(sequence).map_err(storage)?,
                    serde_json::to_string(&data).map_err(storage)?,
                    updated_at,
                ],
            )
            .map_err(storage)?;
            tx.execute(
                "UPDATE threads SET updated_at=?2 WHERE id=?1",
                params![checkpoint.parent_thread_id.to_string(), updated_at],
            )
            .map_err(storage)?;
            if let Some(task) = stranded {
                for data in report(&task) {
                    let item = Item {
                        id: ItemId::new(),
                        thread_id: task.parent_thread_id,
                        turn_id: task.parent_turn_id,
                        sequence: Self::next_sequence(tx, task.parent_thread_id)?,
                        data,
                        created_at: parse_time(updated_at)?,
                    };
                    tx.execute(
                        "INSERT INTO items(id,thread_id,turn_id,sequence,data_json,created_at)
                         VALUES(?1,?2,?3,?4,?5,?6)",
                        params![
                            item.id.to_string(),
                            item.thread_id.to_string(),
                            item.turn_id.map(|id| id.to_string()),
                            i64::try_from(item.sequence).map_err(storage)?,
                            serde_json::to_string(&item.data).map_err(storage)?,
                            updated_at,
                        ],
                    )
                    .map_err(storage)?;
                    written.push(item);
                }
            }
        }
        Ok(written)
    }

    /// What the launching chat is told of a background task recovery found
    /// stopped, read within the recovery's transaction; none when that chat
    /// is gone or archived, or already holds the task's result.
    fn stranded_task(
        tx: &rusqlite::Transaction<'_>,
        task: StrandedTask,
        after: u64,
    ) -> CoreResult<Option<StrandedTask>> {
        let archived = tx
            .query_row(
                "SELECT archived FROM threads WHERE id=?1",
                [task.parent_thread_id.to_string()],
                |row| row.get::<_, bool>(0),
            )
            .optional()
            .map_err(storage)?;
        if archived != Some(false) {
            return Ok(None);
        }
        // A result delivered during this run, whose own checkpoint was lost
        // or came before a late background one.
        let delivered = format!("<task id=\"{}\" state=\"", task.child_thread_id);
        let held = tx
            .query_row(
                "SELECT 1 FROM items
                 WHERE thread_id=?1 AND deleted=0 AND sequence>?2
                   AND json_extract(data_json,'$.type')='user_message'
                   AND instr(json_extract(data_json,'$.text'),?3)=1
                   AND instr(json_extract(data_json,'$.text'),?4)<>1
                 LIMIT 1",
                params![
                    task.parent_thread_id.to_string(),
                    i64::try_from(after).map_err(storage)?,
                    delivered,
                    format!("{delivered}running\""),
                ],
                |_| Ok(()),
            )
            .optional()
            .map_err(storage)?
            .is_some();
        if held {
            return Ok(None);
        }
        let description = tx
            .query_row(
                "SELECT json_extract(data_json,'$.arguments.description') FROM items
                 WHERE thread_id=?1 AND deleted=0 AND json_extract(data_json,'$.type')='tool_call'
                   AND json_extract(data_json,'$.call_id')=?2
                 ORDER BY sequence DESC LIMIT 1",
                params![task.parent_thread_id.to_string(), task.call_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(storage)?
            .flatten()
            .filter(|description| !description.trim().is_empty());
        let description = match description {
            Some(description) => description.trim().to_owned(),
            None => tx
                .query_row(
                    "SELECT title FROM threads WHERE id=?1",
                    [task.child_thread_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(storage)?
                .unwrap_or_default(),
        };
        let reply = tx
            .query_row(
                "SELECT json_extract(data_json,'$.text') FROM items
                 WHERE thread_id=?1 AND turn_id=?2 AND deleted=0
                   AND json_extract(data_json,'$.type')='assistant_message'
                   AND trim(json_extract(data_json,'$.text'))<>''
                 ORDER BY sequence DESC LIMIT 1",
                params![
                    task.child_thread_id.to_string(),
                    task.child_turn_id.to_string()
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(storage)?;
        let error = tx
            .query_row(
                "SELECT error FROM turns WHERE id=?1",
                [task.child_turn_id.to_string()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(storage)?
            .flatten();
        Ok(Some(StrandedTask {
            description,
            reply,
            error,
            ..task
        }))
    }

    fn migrate(&self) -> CoreResult<()> {
        let connection = self.connection.lock();
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations(version INTEGER PRIMARY KEY);",
            )
            .map_err(storage)?;
        let version: i64 = connection
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
            .map_err(storage)?;
        if version < 1 {
            connection
                .execute_batch(
                    "BEGIN;
             CREATE TABLE IF NOT EXISTS threads(
               id TEXT PRIMARY KEY,
               title TEXT NOT NULL,
               parent_id TEXT REFERENCES threads(id),
               archived INTEGER NOT NULL DEFAULT 0,
               agent TEXT NOT NULL,
               provider TEXT NOT NULL,
               model TEXT NOT NULL,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS turns(
               id TEXT PRIMARY KEY,
               thread_id TEXT NOT NULL REFERENCES threads(id),
               request_id TEXT NOT NULL,
               status TEXT NOT NULL,
               agent TEXT NOT NULL,
               provider TEXT NOT NULL,
               model TEXT NOT NULL,
               error TEXT,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               UNIQUE(thread_id, request_id)
             );
             CREATE TABLE IF NOT EXISTS items(
               id TEXT PRIMARY KEY,
               thread_id TEXT NOT NULL REFERENCES threads(id),
               turn_id TEXT REFERENCES turns(id),
               sequence INTEGER NOT NULL,
               data_json TEXT NOT NULL,
               created_at TEXT NOT NULL,
               UNIQUE(thread_id, sequence)
             );
             CREATE INDEX IF NOT EXISTS idx_turns_thread ON turns(thread_id, created_at);
             CREATE INDEX IF NOT EXISTS idx_items_thread_sequence ON items(thread_id, sequence);
             INSERT INTO schema_migrations(version) VALUES (1);
             COMMIT;",
                )
                .map_err(storage)?;
        }
        if version < 2 {
            connection
                .execute_batch(
                    "BEGIN;
             CREATE TABLE IF NOT EXISTS permission_rules(
               workspace TEXT NOT NULL,
               pattern TEXT NOT NULL,
               decision TEXT NOT NULL,
               created_at TEXT NOT NULL,
               PRIMARY KEY(workspace, pattern)
             );
             INSERT INTO schema_migrations(version) VALUES (2);
             COMMIT;",
                )
                .map_err(storage)?;
        }
        if version < 3 {
            connection
                .execute_batch(
                    "BEGIN;
             CREATE TABLE IF NOT EXISTS snapshots(
               id TEXT PRIMARY KEY,
               thread_id TEXT NOT NULL,
               created_at TEXT NOT NULL,
               file_count INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS snapshot_files(
               snapshot_id TEXT NOT NULL,
               path TEXT NOT NULL,
               hash TEXT NOT NULL,
               PRIMARY KEY(snapshot_id, path)
             );
             CREATE TABLE IF NOT EXISTS snapshot_blobs(
               hash TEXT PRIMARY KEY,
               size INTEGER NOT NULL,
               data BLOB NOT NULL
             );
             ALTER TABLE threads ADD COLUMN revert_sequence INTEGER;
             ALTER TABLE threads ADD COLUMN revert_snapshot TEXT;
             ALTER TABLE items ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0;
             INSERT INTO schema_migrations(version) VALUES (3);
             COMMIT;",
                )
                .map_err(storage)?;
        }
        if version < 4 {
            connection
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS thread_extras(
                       thread_id TEXT PRIMARY KEY,
                       goal TEXT,
                       loop_json TEXT
                     );",
                )
                .map_err(storage)?;
            connection
                .execute(
                    "INSERT OR IGNORE INTO schema_migrations(version) VALUES (4)",
                    [],
                )
                .map_err(storage)?;
        }
        if version < 5 {
            connection
                .execute_batch(
                    "BEGIN;
             ALTER TABLE threads ADD COLUMN ultracode INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE turns ADD COLUMN ultracode INTEGER NOT NULL DEFAULT 0;
             INSERT INTO schema_migrations(version) VALUES (5);
             COMMIT;",
                )
                .map_err(storage)?;
        }
        workflows::migrate(&connection, version)?;
        if version < 7 {
            connection
                .execute_batch(
                    "BEGIN;
                     ALTER TABLE thread_extras ADD COLUMN goal_json TEXT;
                     INSERT INTO schema_migrations(version) VALUES (7);
                     COMMIT;",
                )
                .map_err(storage)?;
        }
        if version < 8 {
            connection
                .execute_batch(
                    "BEGIN;
                     ALTER TABLE threads ADD COLUMN revert_base_snapshot TEXT;
                     ALTER TABLE snapshot_files ADD COLUMN kind TEXT NOT NULL DEFAULT 'file';
                     ALTER TABLE snapshot_files ADD COLUMN mode INTEGER;
                     ALTER TABLE snapshot_files ADD COLUMN size INTEGER;
                     INSERT INTO schema_migrations(version) VALUES (8);
                     COMMIT;",
                )
                .map_err(storage)?;
        }
        // Version 9 is reserved for an unreleased, retired feature. Preserve
        // the migration number and any existing historical rows; new stores
        // no longer create its tables or indexes.
        if version < 9 {
            connection
                .execute("INSERT INTO schema_migrations(version) VALUES (9)", [])
                .map_err(storage)?;
        }
        if version < 10 {
            connection
                .execute_batch(
                    "BEGIN IMMEDIATE;
                 ALTER TABLE threads ADD COLUMN is_subagent INTEGER NOT NULL DEFAULT 0;
                 UPDATE threads SET is_subagent=1 WHERE EXISTS (
                   SELECT 1 FROM items WHERE items.thread_id=threads.parent_id
                   AND json_extract(items.data_json, '$.type') IN ('task_link','task_state')
                   AND json_extract(items.data_json, '$.child_thread_id')=threads.id
                 );
                 INSERT INTO schema_migrations(version) VALUES (10);
                 COMMIT;",
                )
                .map_err(storage)?;
        }
        if version < 11 {
            connection
                .execute_batch(
                    "BEGIN IMMEDIATE;
                 ALTER TABLE threads ADD COLUMN fork_origin_json TEXT;
                 INSERT INTO schema_migrations(version) VALUES (11);
                 COMMIT;",
                )
                .map_err(storage)?;
        }
        if version < 12 {
            connection
                .execute_batch(
                    "BEGIN IMMEDIATE;
                 ALTER TABLE threads ADD COLUMN schedule_json TEXT;
                 INSERT INTO schema_migrations(version) VALUES (12);
                 COMMIT;",
                )
                .map_err(storage)?;
        }
        input::migrate(&connection)?;
        // Runtime settings are deliberately idempotent instead of consuming
        // a numbered migration: optional adapters can ignore this extension,
        // while every SQLite database (including one upgraded through several
        // historical versions in one open) receives the durable table.
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS runtime_settings(
                   workspace TEXT NOT NULL,
                   key TEXT NOT NULL,
                   value_json TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace, key)
                 );",
            )
            .map_err(storage)?;
        Ok(())
    }

    fn start_turn_record(
        &self,
        id: ThreadId,
        mut request: StartTurnRequest,
        queue_revision: Option<u64>,
    ) -> CoreResult<(Turn, Item, bool)> {
        let mut connection = self.connection.lock();
        // IMMEDIATE, not the default DEFERRED: a deferred read-to-write
        // upgrade racing the other connection returns SQLITE_BUSY without
        // consulting the busy timeout, which surfaced as "database is
        // locked" when parallel child agents wrote items while snapshots
        // were being captured.
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(storage)?;
        if let Some(raw_turn) = tx.query_row(
            "SELECT id,thread_id,request_id,status,agent,provider,model,error,created_at,updated_at,ultracode
             FROM turns WHERE thread_id=?1 AND request_id=?2",
            params![id.to_string(), request.request_id.to_string()], raw_turn,
        ).optional().map_err(storage)? {
            let turn = decode_turn(raw_turn)?;
            let raw = tx.query_row(
                RETRIED_TURN_PROMPT,
                params![id.to_string(), turn.id.to_string()], raw_item,
            ).map_err(storage)?;
            return Ok((turn, decode_item(raw)?, false));
        }
        // A deferred request is consumed in the same transaction as its new
        // turn. Cancellation/edit races cannot lose or duplicate the prompt.
        if let Some(revision) = queue_revision {
            input::check_revision(&tx, id, revision)?;
        }
        if let Some(input) = input::consume(&tx, id, request.request_id)? {
            request.text = input.text;
            request.attachments = input.attachments;
        }
        let thread_raw = tx.query_row(
            "SELECT id,title,parent_id,archived,agent,provider,model,created_at,updated_at,revert_sequence,revert_snapshot,revert_base_snapshot,ultracode,is_subagent,fork_origin_json,schedule_json FROM threads WHERE id=?1",
            [id.to_string()], raw_thread,
        ).optional().map_err(storage)?.ok_or_else(|| CoreError::NotFound(format!("thread {id}")))?;
        let thread = decode_thread(thread_raw)?;
        let ultracode =
            thread.ultracode || request.reasoning_effort.as_deref() == Some("ultracode");
        // A genuinely new request confirms a pending revert atomically with
        // turn creation. The duplicate-request branch above returns before
        // this point, so an idempotent retry can never consume redo state.
        if let Some(revert) = &thread.revert {
            tx.execute(
                "UPDATE items SET deleted=1 WHERE thread_id=?1 AND sequence>?2",
                params![
                    id.to_string(),
                    i64::try_from(revert.sequence).map_err(storage)?
                ],
            )
            .map_err(storage)?;
            tx.execute(
                "UPDATE threads SET revert_sequence=NULL, revert_snapshot=NULL, revert_base_snapshot=NULL WHERE id=?1",
                [id.to_string()],
            )
            .map_err(storage)?;
        }
        let now = Utc::now();
        let turn = Turn {
            id: TurnId::new(),
            thread_id: id,
            request_id: request.request_id,
            status: TurnStatus::Running,
            agent: request.agent.clone().unwrap_or(thread.agent),
            provider: request.provider.clone().unwrap_or(thread.provider),
            model: request.model.clone().unwrap_or(thread.model),
            ultracode,
            error: None,
            created_at: now,
            updated_at: now,
        };
        tx.execute(
            "INSERT INTO turns(id,thread_id,request_id,status,agent,provider,model,error,created_at,updated_at,ultracode)
             VALUES(?1,?2,?3,?4,?5,?6,?7,NULL,?8,?9,?10)",
            params![turn.id.to_string(), id.to_string(), turn.request_id.to_string(), status(turn.status),
                turn.agent, turn.provider, turn.model, time(now), time(now), turn.ultracode],
        ).map_err(storage)?;
        let item = Item {
            id: ItemId::new(),
            thread_id: id,
            turn_id: Some(turn.id),
            sequence: Self::next_sequence(&tx, id)?,
            data: ItemData::UserMessage {
                text: request.text,
                attachments: request.attachments,
                queued: false,
                synthetic: request.synthetic,
            },
            created_at: now,
        };
        tx.execute(
            "INSERT INTO items(id,thread_id,turn_id,sequence,data_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![item.id.to_string(), id.to_string(), turn.id.to_string(), i64::try_from(item.sequence).map_err(storage)?,
                serde_json::to_string(&item.data).map_err(storage)?, time(now)],
        ).map_err(storage)?;
        tx.execute(
            "UPDATE threads SET updated_at=?2 WHERE id=?1",
            params![id.to_string(), time(now)],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok((turn, item, true))
    }

    fn next_sequence(connection: &Connection, thread_id: ThreadId) -> CoreResult<u64> {
        let value: i64 = connection
            .query_row(
                "SELECT COALESCE(MAX(sequence), 0) + 1 FROM items WHERE thread_id = ?1",
                [thread_id.to_string()],
                |row| row.get(0),
            )
            .map_err(storage)?;
        u64::try_from(value).map_err(|error| CoreError::Storage(error.to_string()))
    }
}

#[async_trait]
impl ThreadRepository for SqliteStore {
    async fn export_session_archive(
        &self,
        id: ThreadId,
        sanitize: bool,
    ) -> CoreResult<SessionArchive> {
        self.dump_session_archive(id, sanitize)
    }

    async fn import_session_archive(
        &self,
        archive: SessionArchive,
    ) -> CoreResult<SessionArchiveImportResult> {
        self.restore_session_archive(archive)
    }

    async fn create_thread(&self, request: CreateThreadRequest) -> CoreResult<Thread> {
        let now = Utc::now();
        let thread = Thread {
            schedule: request.schedule,
            id: request.id.unwrap_or_default(),
            // A client may still send the placeholder it knows, or nothing but
            // spaces; store this server's word for an untitled chat.
            title: request
                .title
                .filter(|title| {
                    !title.trim().is_empty() && !haarness_protocol::is_untitled_chat_title(title)
                })
                .unwrap_or_else(|| haarness_protocol::UNTITLED_CHAT_TITLE.into()),
            parent_id: request.parent_id,
            is_subagent: request.parent_id.is_some(),
            fork_origin: None,
            archived: false,
            agent: request.agent.unwrap_or_else(|| "build".into()),
            provider: request.provider.unwrap_or_else(|| "openai".into()),
            model: request.model.unwrap_or_else(|| "gpt-5.6".into()),
            ultracode: request.ultracode.unwrap_or(false),
            created_at: now,
            updated_at: now,
            revert: None,
        };
        // A client-chosen id can name an existing chat: report the conflict
        // instead of overwriting it (the runtime then returns that chat).
        let inserted = self.connection.lock().execute(
            "INSERT INTO threads(id,title,parent_id,archived,agent,provider,model,created_at,updated_at,ultracode,is_subagent,schedule_json)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) ON CONFLICT(id) DO NOTHING",
            params![thread.id.to_string(), thread.title, thread.parent_id.map(|id| id.to_string()), false,
                thread.agent, thread.provider, thread.model, time(thread.created_at), time(thread.updated_at), thread.ultracode, thread.is_subagent, thread.schedule.as_ref().map(serde_json::to_string).transpose().map_err(storage)?],
        ).map_err(storage)?;
        if inserted == 0 {
            return Err(CoreError::Conflict(format!(
                "thread {} already exists",
                thread.id
            )));
        }
        Ok(thread)
    }

    async fn list_threads(&self, include_archived: bool) -> CoreResult<Vec<Thread>> {
        let connection = self.connection.lock();
        let mut statement = connection
            .prepare(
                "SELECT id,title,parent_id,archived,agent,provider,model,created_at,updated_at,revert_sequence,revert_snapshot,revert_base_snapshot,ultracode,is_subagent,fork_origin_json,schedule_json
             FROM threads WHERE (?1 OR archived = 0) ORDER BY updated_at DESC",
            )
            .map_err(storage)?;
        let rows = statement
            .query_map([include_archived], raw_thread)
            .map_err(storage)?;
        rows.map(|row| row.map_err(storage).and_then(decode_thread))
            .collect()
    }

    async fn get_thread(&self, id: ThreadId) -> CoreResult<Thread> {
        let raw = self.connection.lock().query_row(
            "SELECT id,title,parent_id,archived,agent,provider,model,created_at,updated_at,revert_sequence,revert_snapshot,revert_base_snapshot,ultracode,is_subagent,fork_origin_json,schedule_json FROM threads WHERE id=?1",
            [id.to_string()], raw_thread,
        ).optional().map_err(storage)?.ok_or_else(|| CoreError::NotFound(format!("thread {id}")))?;
        decode_thread(raw)
    }

    async fn update_thread(
        &self,
        id: ThreadId,
        request: UpdateThreadRequest,
    ) -> CoreResult<Thread> {
        let mut current = self.get_thread(id).await?;
        if let Some(value) = request.title {
            current.title = if haarness_protocol::is_untitled_chat_title(&value) {
                haarness_protocol::UNTITLED_CHAT_TITLE.into()
            } else {
                value
            };
        }
        if let Some(value) = request.archived {
            current.archived = value;
        }
        if let Some(value) = request.agent {
            current.agent = value;
        }
        if let Some(value) = request.provider {
            current.provider = value;
        }
        if let Some(value) = request.model {
            current.model = value;
        }
        if let Some(value) = request.ultracode {
            current.ultracode = value;
        }
        current.updated_at = Utc::now();
        self.connection.lock().execute(
            "UPDATE threads SET title=?2,archived=?3,agent=?4,provider=?5,model=?6,updated_at=?7,ultracode=?8 WHERE id=?1",
            params![id.to_string(), current.title, current.archived, current.agent, current.provider, current.model, time(current.updated_at), current.ultracode],
        ).map_err(storage)?;
        Ok(current)
    }

    async fn update_generated_title(
        &self,
        id: ThreadId,
        expected: &str,
        title: &str,
    ) -> CoreResult<Option<Thread>> {
        // Chats created before the placeholder became "New chat" still store
        // the old one, and expecting either means expecting an untitled chat.
        let (expected, also) = if haarness_protocol::is_untitled_chat_title(expected) {
            (
                haarness_protocol::UNTITLED_CHAT_TITLE,
                haarness_protocol::LEGACY_UNTITLED_CHAT_TITLE,
            )
        } else {
            (expected, expected)
        };
        let raw = self.connection.lock().query_row(
            "UPDATE threads SET title=?2,updated_at=?3 WHERE id=?1 AND title IN (?4,?5)
             RETURNING id,title,parent_id,archived,agent,provider,model,created_at,updated_at,revert_sequence,revert_snapshot,revert_base_snapshot,ultracode,is_subagent,fork_origin_json,schedule_json",
            params![id.to_string(), title, time(Utc::now()), expected, also], raw_thread,
        ).optional().map_err(storage)?;
        raw.map(decode_thread).transpose()
    }

    async fn list_items(&self, id: ThreadId, after: Option<u64>) -> CoreResult<Vec<Item>> {
        let connection = self.connection.lock();
        let mut statement = connection
            .prepare(
                "SELECT id,thread_id,turn_id,sequence,data_json,created_at FROM items
             WHERE thread_id=?1 AND sequence>?2 AND deleted=0 ORDER BY sequence",
            )
            .map_err(storage)?;
        let after = i64::try_from(after.unwrap_or(0)).map_err(storage)?;
        let rows = statement
            .query_map(params![id.to_string(), after], raw_item)
            .map_err(storage)?;
        rows.map(|row| row.map_err(storage).and_then(decode_item))
            .collect()
    }

    async fn has_input_admission(&self, id: ThreadId, request: RequestId) -> CoreResult<bool> {
        let owner: Option<String> = self
            .connection
            .lock()
            .query_row(
                "SELECT thread_id FROM pending_inputs WHERE id=?1",
                [request.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage)?;
        match owner {
            Some(owner) if owner != id.to_string() => Err(CoreError::Conflict(
                "Input id belongs to another conversation".into(),
            )),
            Some(_) => Ok(true),
            None => Ok(false),
        }
    }
    async fn input_queue(&self, id: ThreadId) -> CoreResult<haarness_protocol::InputQueue> {
        let mut db = self.connection.lock();
        let tx = db.transaction().map_err(storage)?;
        input::read(&tx, id)
    }
    async fn enqueue_input(
        &self,
        id: ThreadId,
        value: haarness_protocol::PendingInput,
    ) -> CoreResult<haarness_protocol::InputQueue> {
        input::enqueue(&mut self.connection.lock(), id, value)
    }
    async fn change_input_queue(
        &self,
        id: ThreadId,
        action: haarness_protocol::InputQueueAction,
    ) -> CoreResult<haarness_protocol::InputQueue> {
        input::change(&mut self.connection.lock(), id, action)
    }
    async fn promote_input(
        &self,
        id: ThreadId,
        turn: TurnId,
        selected: Option<(RequestId, u64)>,
    ) -> CoreResult<Option<Item>> {
        input::promote(&mut self.connection.lock(), id, turn, selected)
    }
    async fn start_turn(
        &self,
        id: ThreadId,
        request: StartTurnRequest,
    ) -> CoreResult<(Turn, Item, bool)> {
        self.start_turn_record(id, request, None)
    }
    async fn start_queued_turn(
        &self,
        id: ThreadId,
        request: StartTurnRequest,
        revision: u64,
    ) -> CoreResult<(Turn, Item, bool)> {
        self.start_turn_record(id, request, Some(revision))
    }

    async fn get_turn(&self, id: TurnId) -> CoreResult<Turn> {
        let raw = self.connection.lock().query_row(
            "SELECT id,thread_id,request_id,status,agent,provider,model,error,created_at,updated_at,ultracode FROM turns WHERE id=?1",
            [id.to_string()], raw_turn,
        ).optional().map_err(storage)?.ok_or_else(|| CoreError::NotFound(format!("turn {id}")))?;
        decode_turn(raw)
    }

    async fn last_turn(&self, id: ThreadId) -> CoreResult<Option<Turn>> {
        // `idx_turns_thread` orders a thread's turns by creation.
        let raw = self
            .connection
            .lock()
            .query_row(
                "SELECT id,thread_id,request_id,status,agent,provider,model,error,created_at,updated_at,ultracode
                 FROM turns WHERE thread_id=?1 ORDER BY created_at DESC, id DESC LIMIT 1",
                [id.to_string()],
                raw_turn,
            )
            .optional()
            .map_err(storage)?;
        raw.map(decode_turn).transpose()
    }

    async fn prepare_command_input(
        &self,
        thread: ThreadId,
        turn: TurnId,
        text: String,
    ) -> CoreResult<Item> {
        let mut item = self
            .list_items(thread, None)
            .await?
            .into_iter()
            .find(|i| {
                i.turn_id == Some(turn)
                    && matches!(i.data, ItemData::UserMessage { queued: false, .. })
            })
            .ok_or_else(|| CoreError::NotFound("command input".into()))?;
        if let ItemData::UserMessage { text: current, .. } = &mut item.data {
            *current = text;
        }
        self.connection
            .lock()
            .execute(
                "UPDATE items SET data_json=?2 WHERE id=?1 AND turn_id=?3",
                params![
                    item.id.to_string(),
                    serde_json::to_string(&item.data).map_err(storage)?,
                    turn.to_string()
                ],
            )
            .map_err(storage)?;
        Ok(item)
    }

    async fn append_item(
        &self,
        thread: ThreadId,
        turn: Option<TurnId>,
        data: ItemData,
    ) -> CoreResult<Item> {
        let mut connection = self.connection.lock();
        // IMMEDIATE, not the default DEFERRED: a deferred read-to-write
        // upgrade racing the other connection returns SQLITE_BUSY without
        // consulting the busy timeout, which surfaced as "database is
        // locked" when parallel child agents wrote items while snapshots
        // were being captured.
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(storage)?;
        let now = Utc::now();
        let item = Item {
            id: ItemId::new(),
            thread_id: thread,
            turn_id: turn,
            sequence: Self::next_sequence(&tx, thread)?,
            data,
            created_at: now,
        };
        tx.execute(
            "INSERT INTO items(id,thread_id,turn_id,sequence,data_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![item.id.to_string(), thread.to_string(), turn.map(|id| id.to_string()), i64::try_from(item.sequence).map_err(storage)?,
                serde_json::to_string(&item.data).map_err(storage)?, time(now)],
        ).map_err(storage)?;
        tx.execute(
            "UPDATE threads SET updated_at=?2 WHERE id=?1",
            params![thread.to_string(), time(now)],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(item)
    }

    async fn set_turn_status(
        &self,
        id: TurnId,
        new_status: TurnStatus,
        error_value: Option<String>,
    ) -> CoreResult<Turn> {
        let now = Utc::now();
        let changed = self
            .connection
            .lock()
            .execute(
                "UPDATE turns SET status=?2,error=?3,updated_at=?4 WHERE id=?1",
                params![id.to_string(), status(new_status), error_value, time(now)],
            )
            .map_err(storage)?;
        if changed == 0 {
            return Err(CoreError::NotFound(format!("turn {id}")));
        }
        self.get_turn(id).await
    }

    async fn compact(
        &self,
        id: ThreadId,
        request: CompactRequest,
        expected_tail: Option<(ItemId, u64)>,
    ) -> CoreResult<Item> {
        let mut connection = self.connection.lock();
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(storage)?;
        let exists = tx
            .query_row(
                "SELECT 1 FROM threads WHERE id=?1",
                [id.to_string()],
                |_| Ok(()),
            )
            .optional()
            .map_err(storage)?
            .is_some();
        if !exists {
            return Err(CoreError::NotFound(format!("thread {id}")));
        }
        let current_tail = tx
            .query_row(
                "SELECT id,sequence FROM items
                 WHERE thread_id=?1 AND deleted=0 ORDER BY sequence DESC LIMIT 1",
                [id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(storage)?
            .map(|(item, sequence)| {
                Ok((item_id(&item)?, u64::try_from(sequence).map_err(storage)?))
            })
            .transpose()?;
        if current_tail != expected_tail {
            return Err(CoreError::Conflict(
                "thread changed before compaction could commit; retry compaction".into(),
            ));
        }

        let now = Utc::now();
        let item = Item {
            id: ItemId::new(),
            thread_id: id,
            turn_id: None,
            sequence: Self::next_sequence(&tx, id)?,
            data: ItemData::Compaction {
                summary: request.summary.unwrap_or_default(),
                files_touched: request.files_touched,
                outstanding: request.outstanding,
                todo_checkpoint: request.todo_checkpoint,
                tail_start_sequence: request.tail_start_sequence,
            },
            created_at: now,
        };
        tx.execute(
            "INSERT INTO items(id,thread_id,turn_id,sequence,data_json,created_at)
             VALUES(?1,?2,NULL,?3,?4,?5)",
            params![
                item.id.to_string(),
                id.to_string(),
                i64::try_from(item.sequence).map_err(storage)?,
                serde_json::to_string(&item.data).map_err(storage)?,
                time(now)
            ],
        )
        .map_err(storage)?;
        tx.execute(
            "UPDATE threads SET updated_at=?2 WHERE id=?1",
            params![id.to_string(), time(now)],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(item)
    }

    async fn fork_thread(
        &self,
        id: ThreadId,
        title: Option<String>,
        up_to_sequence: Option<u64>,
    ) -> CoreResult<Thread> {
        let parent = self.get_thread(id).await?;
        let now = Utc::now();
        let mut fork = Thread {
            schedule: None,
            id: ThreadId::new(),
            title: title.unwrap_or_else(|| fork_title(&parent.title)),
            parent_id: Some(id),
            is_subagent: false,
            fork_origin: None,
            archived: false,
            agent: parent.agent,
            provider: parent.provider,
            model: parent.model,
            ultracode: parent.ultracode,
            created_at: now,
            updated_at: now,
            revert: None,
        };
        let limit =
            up_to_sequence.map_or(i64::MAX, |value| i64::try_from(value).unwrap_or(i64::MAX));
        let mut connection = self.connection.lock();
        // IMMEDIATE, not the default DEFERRED: a deferred read-to-write
        // upgrade racing the other connection returns SQLITE_BUSY without
        // consulting the busy timeout, which surfaced as "database is
        // locked" when parallel child agents wrote items while snapshots
        // were being captured.
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(storage)?;
        tx.execute(
            "INSERT INTO threads(id,title,parent_id,archived,agent,provider,model,created_at,updated_at,ultracode,is_subagent)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![fork.id.to_string(), fork.title, id.to_string(), false,
                fork.agent, fork.provider, fork.model, time(now), time(now), fork.ultracode, fork.is_subagent],
        )
        .map_err(storage)?;
        {
            let mut statement = tx
                .prepare(
                    "SELECT id,sequence,data_json,created_at FROM items
                     WHERE thread_id=?1 AND sequence<=?2 AND deleted=0 ORDER BY sequence",
                )
                .map_err(storage)?;
            let rows = statement
                .query_map(params![id.to_string(), limit], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })
                .map_err(storage)?;
            let mut copied = Vec::new();
            for row in rows {
                let (old_id, sequence, data_json, created_at) = row.map_err(storage)?;
                copied.push((item_id(&old_id)?, sequence, data_json, created_at));
            }
            let id_map = copied
                .iter()
                .map(|(old_id, ..)| (*old_id, ItemId::new()))
                .collect::<HashMap<_, _>>();
            let mut origin = haarness_protocol::ForkOrigin {
                title: parent.title,
                sequence: 0,
                item_id: None,
            };
            for (old_id, sequence, data_json, created_at) in copied {
                let mut data: ItemData = serde_json::from_str(&data_json).map_err(storage)?;
                if let ItemData::QueueConsumed { item_id } = &mut data {
                    let Some(remapped) = id_map.get(item_id).copied() else {
                        // A corrupt/out-of-range acknowledgement must not turn
                        // into a live queued steer in the fork.
                        continue;
                    };
                    *item_id = remapped;
                }
                tx.execute(
                    "INSERT INTO items(id,thread_id,turn_id,sequence,data_json,created_at)
                     VALUES(?1,?2,NULL,?3,?4,?5)",
                    params![
                        id_map[&old_id].to_string(),
                        fork.id.to_string(),
                        sequence,
                        serde_json::to_string(&data).map_err(storage)?,
                        created_at
                    ],
                )
                .map_err(storage)?;
                origin.sequence = u64::try_from(sequence).map_err(storage)?;
                origin.item_id = Some(old_id);
            }
            tx.execute(
                "UPDATE threads SET fork_origin_json=?2 WHERE id=?1",
                params![
                    fork.id.to_string(),
                    serde_json::to_string(&origin).map_err(storage)?
                ],
            )
            .map_err(storage)?;
            fork.fork_origin = Some(origin);
        }
        tx.commit().map_err(storage)?;
        Ok(fork)
    }

    async fn save_permission_rule(
        &self,
        workspace: &str,
        rule: &haarness_core::PermissionRule,
    ) -> CoreResult<()> {
        let decision = serde_json::to_string(&rule.decision)
            .map_err(storage)?
            .trim_matches('"')
            .to_owned();
        self.connection
            .lock()
            .execute(
                "INSERT INTO permission_rules(workspace,pattern,decision,created_at)
                 VALUES(?1,?2,?3,?4)
                 ON CONFLICT(workspace,pattern) DO UPDATE SET decision=?3",
                params![workspace, rule.pattern, decision, time(Utc::now())],
            )
            .map_err(storage)?;
        Ok(())
    }

    async fn load_permission_rules(
        &self,
        workspace: &str,
    ) -> CoreResult<Vec<haarness_core::PermissionRule>> {
        let connection = self.connection.lock();
        let mut statement = connection
            .prepare(
                "SELECT pattern,decision FROM permission_rules
                 WHERE workspace=?1 ORDER BY created_at, pattern",
            )
            .map_err(storage)?;
        let rows = statement
            .query_map([workspace], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(storage)?;
        let mut rules = Vec::new();
        for row in rows {
            let (pattern, decision) = row.map_err(storage)?;
            let decision =
                serde_json::from_value(serde_json::Value::String(decision)).map_err(storage)?;
            rules.push(haarness_core::PermissionRule { pattern, decision });
        }
        Ok(rules)
    }

    async fn set_revert_state(
        &self,
        id: ThreadId,
        state: Option<(u64, String, Option<String>)>,
    ) -> CoreResult<()> {
        let (sequence, snapshot, base_snapshot) = match state {
            Some((sequence, snapshot, base_snapshot)) => (
                Some(i64::try_from(sequence).map_err(storage)?),
                Some(snapshot),
                base_snapshot,
            ),
            None => (None, None, None),
        };
        self.connection
            .lock()
            .execute(
                "UPDATE threads SET revert_sequence=?2, revert_snapshot=?3, revert_base_snapshot=?4, updated_at=?5 WHERE id=?1",
                params![id.to_string(), sequence, snapshot, base_snapshot, time(Utc::now())],
            )
            .map_err(storage)?;
        Ok(())
    }

    async fn confirm_revert(&self, id: ThreadId) -> CoreResult<bool> {
        let mut connection = self.connection.lock();
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(storage)?;
        let sequence = tx
            .query_row(
                "SELECT revert_sequence FROM threads WHERE id=?1",
                [id.to_string()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()
            .map_err(storage)?
            .ok_or_else(|| CoreError::NotFound(format!("thread {id}")))?;
        let Some(sequence) = sequence else {
            tx.commit().map_err(storage)?;
            return Ok(false);
        };
        tx.execute(
            "UPDATE items SET deleted=1 WHERE thread_id=?1 AND sequence>?2",
            params![id.to_string(), sequence],
        )
        .map_err(storage)?;
        tx.execute(
            "UPDATE threads SET revert_sequence=NULL, revert_snapshot=NULL, revert_base_snapshot=NULL, updated_at=?2 WHERE id=?1",
            params![id.to_string(), time(Utc::now())],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(true)
    }

    async fn soft_delete_items_after(&self, id: ThreadId, sequence: u64) -> CoreResult<u64> {
        let changed = self
            .connection
            .lock()
            .execute(
                "UPDATE items SET deleted=1 WHERE thread_id=?1 AND sequence>?2",
                params![id.to_string(), i64::try_from(sequence).map_err(storage)?],
            )
            .map_err(storage)?;
        u64::try_from(changed).map_err(|error| CoreError::Storage(error.to_string()))
    }

    async fn thread_extras(
        &self,
        id: ThreadId,
    ) -> CoreResult<(Option<String>, Option<haarness_protocol::LoopConfig>)> {
        let connection = self.connection.lock();
        let row: Option<(Option<String>, Option<String>, Option<String>)> = connection
            .query_row(
                "SELECT goal, goal_json, loop_json FROM thread_extras WHERE thread_id = ?1",
                [id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(storage)?;
        let (legacy_goal, goal_json, loop_json) = row.unwrap_or((None, None, None));
        let goal = goal_json
            .as_deref()
            .and_then(|json| serde_json::from_str::<GoalState>(json).ok())
            .map(|state| state.goal)
            .or(legacy_goal);
        let loop_config = loop_json
            .as_deref()
            .and_then(|json| serde_json::from_str(json).ok());
        Ok((goal, loop_config))
    }
    async fn set_goal(&self, id: ThreadId, goal: Option<String>) -> CoreResult<()> {
        self.connection
            .lock()
            .execute(
                "INSERT INTO thread_extras(thread_id, goal, goal_json) VALUES(?1, ?2, NULL)
                 ON CONFLICT(thread_id) DO UPDATE SET goal = ?2, goal_json = NULL",
                params![id.to_string(), goal],
            )
            .map_err(storage)?;
        Ok(())
    }
    async fn goal_state(&self, id: ThreadId) -> CoreResult<Option<GoalState>> {
        let connection = self.connection.lock();
        loop {
            let row: Option<(Option<String>, Option<String>)> = connection
                .query_row(
                    "SELECT goal, goal_json FROM thread_extras WHERE thread_id = ?1",
                    [id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(storage)?;
            let Some((legacy_goal, goal_json)) = row else {
                return Ok(None);
            };
            if let Some(json) = goal_json {
                return serde_json::from_str(&json).map(Some).map_err(storage);
            }
            let Some(goal) = legacy_goal else {
                return Ok(None);
            };
            let state = haarness_core::legacy_goal_state(goal.clone());
            // Compare the legacy text we actually read. Besides keeping the
            // SELECT and migration under one in-process connection guard,
            // this conditional protects against another SqliteStore/connection
            // clearing or replacing the goal between statements. On a lost
            // race, loop and return/upgrade the new canonical row instead of
            // resurrecting our stale snapshot with a freshly generated ID.
            if Self::upgrade_legacy_goal_if_current(&connection, id, &goal, &state)? {
                return Ok(Some(state));
            }
        }
    }
    async fn save_goal_state(&self, id: ThreadId, state: Option<GoalState>) -> CoreResult<()> {
        let goal = state.as_ref().map(|state| state.goal.clone());
        let json = state
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(storage)?;
        self.connection
            .lock()
            .execute(
                "INSERT INTO thread_extras(thread_id, goal, goal_json) VALUES(?1, ?2, ?3)
                 ON CONFLICT(thread_id) DO UPDATE SET goal = ?2, goal_json = ?3",
                params![id.to_string(), goal, json],
            )
            .map_err(storage)?;
        Ok(())
    }
    async fn set_loop(
        &self,
        id: ThreadId,
        config: Option<haarness_protocol::LoopConfig>,
    ) -> CoreResult<()> {
        let json = config
            .map(|config| serde_json::to_string(&config).map_err(storage))
            .transpose()?;
        self.connection
            .lock()
            .execute(
                "INSERT INTO thread_extras(thread_id, loop_json) VALUES(?1, ?2)
                 ON CONFLICT(thread_id) DO UPDATE SET loop_json = ?2",
                params![id.to_string(), json],
            )
            .map_err(storage)?;
        Ok(())
    }
    async fn runtime_setting(
        &self,
        workspace: &str,
        key: &str,
    ) -> CoreResult<Option<serde_json::Value>> {
        let raw = self
            .connection
            .lock()
            .query_row(
                "SELECT value_json FROM runtime_settings WHERE workspace=?1 AND key=?2",
                params![workspace, key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(storage)?;
        raw.map(|value| serde_json::from_str(&value).map_err(storage))
            .transpose()
    }
    async fn save_runtime_setting(
        &self,
        workspace: &str,
        key: &str,
        value: Option<serde_json::Value>,
    ) -> CoreResult<()> {
        let connection = self.connection.lock();
        if let Some(value) = value {
            connection
                .execute(
                    "INSERT INTO runtime_settings(workspace,key,value_json,updated_at)
                     VALUES(?1,?2,?3,?4)
                     ON CONFLICT(workspace,key) DO UPDATE
                     SET value_json=excluded.value_json, updated_at=excluded.updated_at",
                    params![
                        workspace,
                        key,
                        serde_json::to_string(&value).map_err(storage)?,
                        time(Utc::now())
                    ],
                )
                .map_err(storage)?;
        } else {
            connection
                .execute(
                    "DELETE FROM runtime_settings WHERE workspace=?1 AND key=?2",
                    params![workspace, key],
                )
                .map_err(storage)?;
        }
        Ok(())
    }
    async fn recover_interrupted(&self) -> CoreResult<u64> {
        Ok(self.recover_interrupted_tasks(&|_| Vec::new()).await?.0)
    }
    async fn recover_interrupted_tasks(
        &self,
        report: &StrandedReport<'_>,
    ) -> CoreResult<(u64, Vec<Item>)> {
        let mut connection = self.connection.lock();
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(storage)?;
        let orphaned = {
            let mut statement = tx
                .prepare(
                    "SELECT id,thread_id FROM turns
                     WHERE status IN ('queued','running')
                     ORDER BY created_at,id",
                )
                .map_err(storage)?;
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(storage)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(storage)?
        };
        let now = Utc::now();
        let updated_at = time(now);
        let error = "server restarted during active turn";
        for (raw_turn_id, raw_thread_id) in &orphaned {
            let turn_id = turn_id(raw_turn_id)?;
            let thread_id = thread_id(raw_thread_id)?;
            let changed = tx
                .execute(
                    "UPDATE turns SET status='interrupted',error=?2,updated_at=?3
                     WHERE id=?1 AND status IN ('queued','running')",
                    params![raw_turn_id, error, updated_at],
                )
                .map_err(storage)?;
            if changed != 1 {
                return Err(CoreError::Storage(format!(
                    "orphaned turn {turn_id} changed during recovery"
                )));
            }
            let data = ItemData::Error {
                message: RECOVERED_TURN_MESSAGE.into(),
                retryable: true,
            };
            let sequence = Self::next_sequence(&tx, thread_id)?;
            tx.execute(
                "INSERT INTO items(id,thread_id,turn_id,sequence,data_json,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    ItemId::new().to_string(),
                    raw_thread_id,
                    raw_turn_id,
                    i64::try_from(sequence).map_err(storage)?,
                    serde_json::to_string(&data).map_err(storage)?,
                    updated_at,
                ],
            )
            .map_err(storage)?;
            tx.execute(
                "UPDATE threads SET updated_at=?2 WHERE id=?1",
                params![raw_thread_id, updated_at],
            )
            .map_err(storage)?;
        }
        let written = Self::reconcile_task_states(&tx, &updated_at, report)?;
        tx.commit().map_err(storage)?;
        Ok((u64::try_from(orphaned.len()).map_err(storage)?, written))
    }
}

type RawThread = (
    String,
    String,
    Option<String>,
    bool,
    String,
    String,
    String,
    String,
    String,
    Option<i64>,
    Option<String>,
    Option<String>,
    bool,
    bool,
    Option<String>,
    Option<String>,
);
type RawTurn = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    String,
    bool,
);
type RawItem = (String, String, Option<String>, i64, String, String);

fn raw_thread(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawThread> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
    ))
}
fn raw_turn(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawTurn> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
    ))
}
fn raw_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawItem> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

/// The default name of a fork: "Fork of" its source's name, or of "New chat"
/// when the source has none (an untitled or blank title).
fn fork_title(parent: &str) -> String {
    let parent = parent.trim();
    if parent.is_empty() || haarness_protocol::is_untitled_chat_title(parent) {
        format!("Fork of {}", haarness_protocol::UNTITLED_CHAT_TITLE)
    } else {
        format!("Fork of {parent}")
    }
}

fn decode_thread(raw: RawThread) -> CoreResult<Thread> {
    let revert = match (raw.9, raw.10) {
        (Some(sequence), Some(snapshot_id)) => Some(haarness_protocol::RevertState {
            sequence: u64::try_from(sequence)
                .map_err(|error| CoreError::Storage(error.to_string()))?,
            snapshot_id,
            base_snapshot_id: raw.11,
        }),
        _ => None,
    };
    Ok(Thread {
        schedule: raw
            .15
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(storage)?,
        id: thread_id(&raw.0)?,
        // Older servers stored "New thread" for an untitled chat; every
        // client sees one placeholder.
        title: if raw.1 == haarness_protocol::LEGACY_UNTITLED_CHAT_TITLE {
            haarness_protocol::UNTITLED_CHAT_TITLE.into()
        } else {
            raw.1
        },
        parent_id: raw.2.as_deref().map(thread_id).transpose()?,
        is_subagent: raw.13,
        fork_origin: raw
            .14
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(storage)?,
        archived: raw.3,
        agent: raw.4,
        provider: raw.5,
        model: raw.6,
        ultracode: raw.12,
        created_at: parse_time(&raw.7)?,
        updated_at: parse_time(&raw.8)?,
        revert,
    })
}
fn decode_turn(raw: RawTurn) -> CoreResult<Turn> {
    Ok(Turn {
        id: turn_id(&raw.0)?,
        thread_id: thread_id(&raw.1)?,
        request_id: request_id(&raw.2)?,
        status: parse_status(&raw.3)?,
        agent: raw.4,
        provider: raw.5,
        model: raw.6,
        ultracode: raw.10,
        error: raw.7,
        created_at: parse_time(&raw.8)?,
        updated_at: parse_time(&raw.9)?,
    })
}
fn decode_item(raw: RawItem) -> CoreResult<Item> {
    Ok(Item {
        id: item_id(&raw.0)?,
        thread_id: thread_id(&raw.1)?,
        turn_id: raw.2.as_deref().map(turn_id).transpose()?,
        sequence: u64::try_from(raw.3).map_err(storage)?,
        data: serde_json::from_str(&raw.4).map_err(storage)?,
        created_at: parse_time(&raw.5)?,
    })
}

fn thread_id(value: &str) -> CoreResult<ThreadId> {
    Ok(ThreadId(Uuid::parse_str(value).map_err(storage)?))
}
fn turn_id(value: &str) -> CoreResult<TurnId> {
    Ok(TurnId(Uuid::parse_str(value).map_err(storage)?))
}
fn item_id(value: &str) -> CoreResult<ItemId> {
    Ok(ItemId(Uuid::parse_str(value).map_err(storage)?))
}
fn request_id(value: &str) -> CoreResult<RequestId> {
    Ok(RequestId(Uuid::parse_str(value).map_err(storage)?))
}
fn time(value: DateTime<Utc>) -> String {
    value.to_rfc3339()
}
fn parse_time(value: &str) -> CoreResult<DateTime<Utc>> {
    value.parse().map_err(storage)
}
fn status(value: TurnStatus) -> &'static str {
    match value {
        TurnStatus::Queued => "queued",
        TurnStatus::Running => "running",
        TurnStatus::Completed => "completed",
        TurnStatus::Failed => "failed",
        TurnStatus::Interrupted => "interrupted",
    }
}
fn parse_status(value: &str) -> CoreResult<TurnStatus> {
    match value {
        "queued" => Ok(TurnStatus::Queued),
        "running" => Ok(TurnStatus::Running),
        "completed" => Ok(TurnStatus::Completed),
        "failed" => Ok(TurnStatus::Failed),
        "interrupted" => Ok(TurnStatus::Interrupted),
        _ => Err(CoreError::Storage(format!("unknown turn status {value}"))),
    }
}
fn storage(error: impl std::fmt::Display) -> CoreError {
    CoreError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_retried_turn_start_finds_its_prompt_through_the_chat_index() {
        let store = SqliteStore::in_memory().unwrap();
        let thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let request = StartTurnRequest {
            request_id: RequestId::new(),
            text: "the prompt".into(),
            attachments: Vec::new(),
            synthetic: false,
            auto_approve: false,
            agent: None,
            provider: None,
            model: None,
            reasoning_effort: None,
            tools: None,
            command: None,
        };
        let (turn, prompt, created) = store.start_turn(thread.id, request.clone()).await.unwrap();
        assert!(created);
        let (again, retried, created) = store.start_turn(thread.id, request).await.unwrap();
        assert!(!created);
        assert_eq!((again.id, retried.id), (turn.id, prompt.id));
        let plan = store
            .connection
            .lock()
            .prepare(&format!("EXPLAIN QUERY PLAN {RETRIED_TURN_PROMPT}"))
            .unwrap()
            .query_map(params![thread.id.to_string(), turn.id.to_string()], |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join("\n");
        assert!(plan.contains("idx_items_thread_sequence"), "{plan}");
        assert!(!plan.contains("SCAN items"), "{plan}");
    }

    #[tokio::test]
    async fn a_chosen_thread_id_is_stored_once_and_never_overwritten() {
        let store = SqliteStore::in_memory().unwrap();
        let id = ThreadId::new();
        let created = store
            .create_thread(CreateThreadRequest {
                id: Some(id),
                title: Some("First".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(created.id, id);
        let again = store
            .create_thread(CreateThreadRequest {
                id: Some(id),
                title: Some("Second".into()),
                ..Default::default()
            })
            .await;
        assert!(matches!(again, Err(CoreError::Conflict(_))), "{again:?}");
        assert_eq!(store.get_thread(id).await.unwrap().title, "First");
        assert_eq!(store.list_threads(true).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn delegated_threads_are_distinct_from_forks_after_reload_and_rename() {
        let store = SqliteStore::in_memory().unwrap();
        let root = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let child = store
            .create_thread(CreateThreadRequest {
                parent_id: Some(root.id),
                ..Default::default()
            })
            .await
            .unwrap();
        let fork = store
            .fork_thread(
                root.id,
                Some("Renamed independent conversation".into()),
                None,
            )
            .await
            .unwrap();
        let fork_of_child = store.fork_thread(child.id, None, None).await.unwrap();
        assert!(!root.is_subagent);
        assert!(child.is_subagent);
        assert!(!fork.is_subagent);
        assert!(!fork_of_child.is_subagent);
        let archive = store.export_session_archive(child.id, false).await.unwrap();
        assert!(!archive.threads[0].thread.is_subagent);
        let restored = SqliteStore::in_memory().unwrap();
        restored.import_session_archive(archive).await.unwrap();
        assert!(!restored.get_thread(child.id).await.unwrap().is_subagent);
        assert_eq!(fork.parent_id, Some(root.id));
        for thread in store.list_threads(true).await.unwrap() {
            assert_eq!(thread.is_subagent, thread.id == child.id);
            assert_eq!(
                store.get_thread(thread.id).await.unwrap().is_subagent,
                thread.is_subagent
            );
        }
    }

    #[tokio::test]
    async fn migration_classifies_old_task_links_without_hiding_forks() {
        let directory =
            std::env::temp_dir().join(format!("haarness-thread-kinds-{}", Uuid::now_v7()));
        let path = directory.join("sessions.db");
        let store = SqliteStore::open(&path).unwrap();
        let root = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let child = store
            .create_thread(CreateThreadRequest {
                parent_id: Some(root.id),
                ..Default::default()
            })
            .await
            .unwrap();
        store
            .append_item(
                root.id,
                None,
                ItemData::TaskLink {
                    call_id: "delegation".into(),
                    child_thread_id: child.id,
                },
            )
            .await
            .unwrap();
        let fork = store
            .fork_thread(root.id, Some("No title heuristic".into()), None)
            .await
            .unwrap();
        store
            .connection
            .lock()
            .execute_batch(
                "ALTER TABLE threads DROP COLUMN is_subagent;
             ALTER TABLE threads DROP COLUMN fork_origin_json; ALTER TABLE threads DROP COLUMN schedule_json;
             DELETE FROM schema_migrations WHERE version>=10;",
            )
            .unwrap();
        drop(store);
        let store = SqliteStore::open(&path).unwrap();
        assert!(store.get_thread(child.id).await.unwrap().is_subagent);
        assert!(!store.get_thread(fork.id).await.unwrap().is_subagent);
        assert!(!store.get_thread(root.id).await.unwrap().is_subagent);
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn generated_titles_never_overwrite_manual_renames_or_unrelated_settings() {
        let store = SqliteStore::in_memory().unwrap();
        let thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        store
            .update_thread(
                thread.id,
                UpdateThreadRequest {
                    title: Some("My title".into()),
                    model: Some("selected-model".into()),
                    archived: None,
                    agent: None,
                    provider: None,
                    ultracode: None,
                },
            )
            .await
            .unwrap();
        assert!(
            store
                .update_generated_title(
                    thread.id,
                    haarness_protocol::UNTITLED_CHAT_TITLE,
                    "Generated title"
                )
                .await
                .unwrap()
                .is_none()
        );
        let retained = store.get_thread(thread.id).await.unwrap();
        assert_eq!(retained.title, "My title");
        assert_eq!(retained.model, "selected-model");
        let generated = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        assert_eq!(
            store
                .update_generated_title(
                    generated.id,
                    haarness_protocol::UNTITLED_CHAT_TITLE,
                    "First title"
                )
                .await
                .unwrap()
                .unwrap()
                .title,
            "First title"
        );
        assert!(
            store
                .update_generated_title(
                    generated.id,
                    haarness_protocol::UNTITLED_CHAT_TITLE,
                    "Late title"
                )
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn an_untitled_chat_is_a_new_chat_even_in_rows_older_servers_wrote() {
        let store = SqliteStore::in_memory().unwrap();
        let fresh = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        assert_eq!(fresh.title, "New chat");
        // An older client still sends the placeholder it knows.
        let sent = store
            .create_thread(CreateThreadRequest {
                title: Some("New thread".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(sent.title, "New chat");
        let spaces = store
            .create_thread(CreateThreadRequest {
                title: Some("   ".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(spaces.title, "New chat");
        let renamed = store
            .update_thread(
                spaces.id,
                UpdateThreadRequest {
                    title: Some("New thread".into()),
                    archived: None,
                    agent: None,
                    provider: None,
                    model: None,
                    ultracode: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(renamed.title, "New chat");
        let legacy = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let blank = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        {
            let connection = store.connection.lock();
            connection
                .execute(
                    "UPDATE threads SET title='New thread' WHERE id=?1",
                    params![legacy.id.to_string()],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE threads SET title='   ' WHERE id=?1",
                    params![blank.id.to_string()],
                )
                .unwrap();
        }
        assert_eq!(store.get_thread(legacy.id).await.unwrap().title, "New chat");
        for parent in [fresh.id, legacy.id, blank.id] {
            let fork = store.fork_thread(parent, None, None).await.unwrap();
            assert_eq!(fork.title, "Fork of New chat");
        }
        let named = store
            .create_thread(CreateThreadRequest {
                title: Some("Fix the login flow".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            store.fork_thread(named.id, None, None).await.unwrap().title,
            "Fork of Fix the login flow"
        );
        // A row older servers wrote is still untitled to the title generator.
        assert_eq!(
            store
                .update_generated_title(legacy.id, haarness_protocol::UNTITLED_CHAT_TITLE, "Named")
                .await
                .unwrap()
                .unwrap()
                .title,
            "Named"
        );
    }

    #[tokio::test]
    async fn persists_idempotent_turn_and_monotonic_items() {
        let store = SqliteStore::in_memory().unwrap();
        let thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let request = StartTurnRequest {
            request_id: RequestId::new(),
            text: "hello".into(),
            attachments: vec![],
            synthetic: false,
            auto_approve: false,
            agent: None,
            provider: None,
            model: None,
            reasoning_effort: None,
            tools: None,
            command: None,
        };
        let first = store.start_turn(thread.id, request.clone()).await.unwrap();
        let second = store.start_turn(thread.id, request).await.unwrap();
        assert_eq!(first.0.id, second.0.id);
        assert_eq!(first.1.id, second.1.id);
        let appended = store
            .append_item(
                thread.id,
                Some(first.0.id),
                ItemData::AssistantMessage { text: "hi".into() },
            )
            .await
            .unwrap();
        assert!(appended.sequence > first.1.sequence);
    }

    #[tokio::test]
    async fn compaction_compare_and_append_rejects_a_stale_transcript_tail() {
        let store = SqliteStore::in_memory().unwrap();
        let thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let summarized_tail = store
            .append_item(
                thread.id,
                None,
                ItemData::UserMessage {
                    text: "summarized input".into(),
                    attachments: Vec::new(),
                    queued: false,
                    synthetic: false,
                },
            )
            .await
            .unwrap();
        let concurrent_writer = store
            .append_item(
                thread.id,
                None,
                ItemData::AssistantMessage {
                    text: "arrived after the summary snapshot".into(),
                },
            )
            .await
            .unwrap();

        let stale = store
            .compact(
                thread.id,
                CompactRequest {
                    summary: Some("stale summary".into()),
                    files_touched: Vec::new(),
                    outstanding: Vec::new(),
                    todo_checkpoint: None,
                    tail_start_sequence: None,
                },
                Some((summarized_tail.id, summarized_tail.sequence)),
            )
            .await;
        assert!(matches!(stale, Err(CoreError::Conflict(_))));
        let items = store.list_items(thread.id, None).await.unwrap();
        assert_eq!(items.len(), 2);
        assert!(
            !items
                .iter()
                .any(|item| matches!(item.data, ItemData::Compaction { .. })),
            "a stale summary cannot create a truncation boundary"
        );

        let committed = store
            .compact(
                thread.id,
                CompactRequest {
                    summary: Some("current summary".into()),
                    files_touched: Vec::new(),
                    outstanding: Vec::new(),
                    todo_checkpoint: None,
                    tail_start_sequence: None,
                },
                Some((concurrent_writer.id, concurrent_writer.sequence)),
            )
            .await
            .unwrap();
        assert!(matches!(committed.data, ItemData::Compaction { .. }));
    }

    #[tokio::test]
    async fn scheduled_origin_survives_rename_reload_and_archive_but_not_forks() {
        let directory =
            std::env::temp_dir().join(format!("haarness-schedule-origin-{}", Uuid::now_v7()));
        let path = directory.join("sessions.db");
        let store = SqliteStore::open(&path).unwrap();
        let origin = haarness_protocol::ScheduleOrigin {
            id: Uuid::now_v7(),
            name: "Daily check".into(),
        };
        let run = store
            .create_thread(CreateThreadRequest {
                schedule: Some(origin.clone()),
                ..Default::default()
            })
            .await
            .unwrap();
        store
            .update_thread(
                run.id,
                UpdateThreadRequest {
                    title: Some("Renamed conversation".into()),
                    archived: None,
                    agent: None,
                    provider: None,
                    model: None,
                    ultracode: None,
                },
            )
            .await
            .unwrap();
        assert!(
            store
                .fork_thread(run.id, None, None)
                .await
                .unwrap()
                .schedule
                .is_none()
        );
        drop(store);
        let store = SqliteStore::open(&path).unwrap();
        let loaded = store.get_thread(run.id).await.unwrap();
        assert_eq!(loaded.title, "Renamed conversation");
        assert_eq!(loaded.schedule.as_ref().unwrap().id, origin.id);
        assert_eq!(
            store
                .list_threads(false)
                .await
                .unwrap()
                .iter()
                .find(|thread| thread.id == run.id)
                .unwrap()
                .schedule
                .as_ref()
                .unwrap()
                .id,
            origin.id
        );
        let archive = store.export_session_archive(run.id, false).await.unwrap();
        let imported = SqliteStore::in_memory().unwrap();
        imported.import_session_archive(archive).await.unwrap();
        assert_eq!(
            imported
                .get_thread(run.id)
                .await
                .unwrap()
                .schedule
                .unwrap()
                .name,
            origin.name
        );
        // Upgrading an old database must preserve its conversations without inventing origins.
        store.connection.lock().execute_batch("ALTER TABLE threads DROP COLUMN schedule_json; DELETE FROM schema_migrations WHERE version=12;").unwrap();
        drop(store);
        let store = SqliteStore::open(&path).unwrap();
        assert!(store.get_thread(run.id).await.unwrap().schedule.is_none());
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn fork_origin_preserves_exact_source_boundary_through_reload_and_archives() {
        let directory =
            std::env::temp_dir().join(format!("haarness-fork-origin-{}", Uuid::now_v7()));
        let path = directory.join("sessions.db");
        let store = SqliteStore::open(&path).unwrap();
        let source = store
            .create_thread(CreateThreadRequest {
                title: Some("Source title secret".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let mut source_items = Vec::new();
        for text in ["first", "selected reply", "later reply"] {
            source_items.push(
                store
                    .append_item(
                        source.id,
                        None,
                        ItemData::AssistantMessage { text: text.into() },
                    )
                    .await
                    .unwrap(),
            );
        }
        let fork = store
            .fork_thread(source.id, None, Some(source_items[1].sequence))
            .await
            .unwrap();
        let origin = fork.fork_origin.as_ref().unwrap();
        assert_eq!(origin.title, source.title);
        assert_eq!(origin.sequence, source_items[1].sequence);
        assert_eq!(origin.item_id, Some(source_items[1].id));
        let own = store
            .append_item(
                fork.id,
                None,
                ItemData::AssistantMessage {
                    text: "new continuation".into(),
                },
            )
            .await
            .unwrap();
        store
            .update_thread(
                fork.id,
                UpdateThreadRequest {
                    title: Some("Renamed fork".into()),
                    archived: None,
                    agent: None,
                    provider: None,
                    model: None,
                    ultracode: None,
                },
            )
            .await
            .unwrap();
        let nested = store.fork_thread(fork.id, None, None).await.unwrap();
        assert_eq!(nested.parent_id, Some(fork.id));
        assert_eq!(nested.fork_origin.as_ref().unwrap().item_id, Some(own.id));
        let whole = store.fork_thread(source.id, None, Some(999)).await.unwrap();
        assert_eq!(
            whole.fork_origin.unwrap().sequence,
            source_items[2].sequence
        );
        let empty = store.fork_thread(source.id, None, Some(0)).await.unwrap();
        assert_eq!(empty.fork_origin.as_ref().unwrap().sequence, 0);
        assert_eq!(empty.fork_origin.as_ref().unwrap().item_id, None);
        drop(store);
        let store = SqliteStore::open(&path).unwrap();
        let restored = store.get_thread(fork.id).await.unwrap();
        assert_eq!(restored.title, "Renamed fork");
        assert_eq!(
            serde_json::to_value(&restored.fork_origin).unwrap(),
            serde_json::to_value(&fork.fork_origin).unwrap()
        );
        let archive = store.export_session_archive(fork.id, false).await.unwrap();
        let imported = SqliteStore::in_memory().unwrap();
        imported.import_session_archive(archive).await.unwrap();
        assert_eq!(
            serde_json::to_value(imported.get_thread(fork.id).await.unwrap().fork_origin).unwrap(),
            serde_json::to_value(&fork.fork_origin).unwrap()
        );
        let sanitized = store.export_session_archive(fork.id, true).await.unwrap();
        assert!(
            !serde_json::to_string(&sanitized)
                .unwrap()
                .contains("Source title secret")
        );
        // Upgrade an older database without inventing a boundary for its forks.
        store.connection.lock().execute_batch("ALTER TABLE threads DROP COLUMN fork_origin_json; ALTER TABLE threads DROP COLUMN schedule_json; DELETE FROM schema_migrations WHERE version>=11;").unwrap();
        drop(store);
        let store = SqliteStore::open(&path).unwrap();
        let legacy = store.get_thread(fork.id).await.unwrap();
        assert_eq!(legacy.parent_id, Some(source.id));
        assert!(legacy.fork_origin.is_none());
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn fork_copies_items_and_detaches_turns() {
        let store = SqliteStore::in_memory().unwrap();
        let thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let (turn, user_item, _) = store
            .start_turn(
                thread.id,
                StartTurnRequest {
                    request_id: RequestId::new(),
                    text: "first".into(),
                    attachments: vec![],
                    synthetic: false,
                    auto_approve: false,
                    agent: None,
                    provider: None,
                    model: None,
                    reasoning_effort: None,
                    tools: None,
                    command: None,
                },
            )
            .await
            .unwrap();
        store
            .append_item(
                thread.id,
                Some(turn.id),
                ItemData::AssistantMessage { text: "hi".into() },
            )
            .await
            .unwrap();

        let fork = store.fork_thread(thread.id, None, None).await.unwrap();
        assert_eq!(fork.parent_id, Some(thread.id));
        assert_eq!(fork.title, "Fork of New chat");
        assert_eq!(fork.provider, thread.provider);

        let parent_items = store.list_items(thread.id, None).await.unwrap();
        let fork_items = store.list_items(fork.id, None).await.unwrap();
        assert_eq!(parent_items.len(), fork_items.len());
        for (parent, copy) in parent_items.iter().zip(&fork_items) {
            assert_eq!(parent.sequence, copy.sequence);
            assert_ne!(parent.id, copy.id, "fork items need fresh identities");
            assert!(copy.turn_id.is_none(), "fork items detach from turns");
        }

        let partial = store
            .fork_thread(thread.id, Some("Early".into()), Some(user_item.sequence))
            .await
            .unwrap();
        assert_eq!(partial.title, "Early");
        assert_eq!(store.list_items(partial.id, None).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn fork_remaps_queue_consumed_item_references() {
        let store = SqliteStore::in_memory().unwrap();
        let thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let queued = store
            .append_item(
                thread.id,
                None,
                ItemData::UserMessage {
                    text: "queued steer".into(),
                    attachments: Vec::new(),
                    queued: true,
                    synthetic: false,
                },
            )
            .await
            .unwrap();
        store
            .append_item(
                thread.id,
                None,
                ItemData::QueueConsumed { item_id: queued.id },
            )
            .await
            .unwrap();

        let fork = store.fork_thread(thread.id, None, None).await.unwrap();
        let items = store.list_items(fork.id, None).await.unwrap();
        let copied_queued = items
            .iter()
            .find(|item| matches!(item.data, ItemData::UserMessage { queued: true, .. }))
            .unwrap();
        let copied_reference = items.iter().find_map(|item| match item.data {
            ItemData::QueueConsumed { item_id } => Some(item_id),
            _ => None,
        });
        assert_eq!(copied_reference, Some(copied_queued.id));
        assert_ne!(copied_reference, Some(queued.id));
    }

    #[tokio::test]
    async fn permission_rules_persist_per_workspace() {
        use haarness_core::{PermissionDecision, PermissionRule};
        let store = SqliteStore::in_memory().unwrap();
        let rule = PermissionRule {
            pattern: "bash:cargo test*".into(),
            decision: PermissionDecision::Allow,
        };
        store.save_permission_rule("/ws/a", &rule).await.unwrap();
        store
            .save_permission_rule(
                "/ws/a",
                &PermissionRule {
                    pattern: "write:docs/**".into(),
                    decision: PermissionDecision::Allow,
                },
            )
            .await
            .unwrap();
        // Same pattern again upgrades in place instead of duplicating.
        store
            .save_permission_rule(
                "/ws/a",
                &PermissionRule {
                    pattern: "bash:cargo test*".into(),
                    decision: PermissionDecision::Deny,
                },
            )
            .await
            .unwrap();

        let rules = store.load_permission_rules("/ws/a").await.unwrap();
        assert_eq!(rules.len(), 2);
        assert!(rules.iter().any(|entry| entry.pattern == "bash:cargo test*"
            && entry.decision == PermissionDecision::Deny));
        assert!(
            store
                .load_permission_rules("/ws/other")
                .await
                .unwrap()
                .is_empty(),
            "rules are workspace-scoped"
        );
    }

    #[tokio::test]
    async fn migrations_record_every_version() {
        let store = SqliteStore::in_memory().unwrap();
        let versions: Vec<i64> = {
            let connection = store.connection.lock();
            let mut statement = connection
                .prepare("SELECT version FROM schema_migrations ORDER BY version")
                .unwrap();
            let rows = statement.query_map([], |row| row.get(0)).unwrap();
            rows.map(Result::unwrap).collect()
        };
        assert_eq!(versions, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    }

    #[tokio::test]
    async fn retired_schema_preserves_existing_sessions_without_creating_new_coordination_tables() {
        let root =
            std::env::temp_dir().join(format!("haarness-retired-schema-{}", uuid::Uuid::now_v7()));
        let path = root.join("state.db");
        let store = SqliteStore::open(&path).unwrap();
        let thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        {
            let connection = store.connection.lock();
            let count: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name='teams'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 0);
            // Recreate an old development-only table to verify that upgrading
            // ignores historical metadata and retains ordinary session data.
            connection.execute_batch(r#"CREATE TABLE teams(id TEXT PRIMARY KEY, data_json TEXT); INSERT INTO teams VALUES ('legacy', '{"audit":true}');"#).unwrap();
        }
        drop(store);
        let reopened = SqliteStore::open(&path).unwrap();
        assert_eq!(reopened.get_thread(thread.id).await.unwrap().id, thread.id);
        let retained: String = reopened
            .connection
            .lock()
            .query_row("SELECT data_json FROM teams WHERE id='legacy'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(retained, r#"{"audit":true}"#);
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn legacy_text_goal_lazily_upgrades_to_structured_state() {
        let store = SqliteStore::in_memory().unwrap();
        let thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        store
            .set_goal(thread.id, Some("legacy objective".into()))
            .await
            .unwrap();

        let state = store.goal_state(thread.id).await.unwrap().unwrap();
        assert_eq!(state.goal, "legacy objective");
        assert_eq!(state.status, haarness_protocol::GoalStatus::Active);
        let goal_json: Option<String> = store
            .connection
            .lock()
            .query_row(
                "SELECT goal_json FROM thread_extras WHERE thread_id=?1",
                [thread.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(goal_json.is_some(), "the upgrade is durable");
    }

    #[tokio::test]
    async fn stale_legacy_upgrade_cannot_resurrect_clear_or_replacement() {
        let store = SqliteStore::in_memory().unwrap();
        let thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();

        store
            .set_goal(thread.id, Some("legacy objective".into()))
            .await
            .unwrap();
        let stale = haarness_core::legacy_goal_state("legacy objective".into());
        store.save_goal_state(thread.id, None).await.unwrap();
        let upgraded = {
            let connection = store.connection.lock();
            SqliteStore::upgrade_legacy_goal_if_current(
                &connection,
                thread.id,
                "legacy objective",
                &stale,
            )
            .unwrap()
        };
        assert!(!upgraded, "a concurrent clear wins the migration race");
        assert!(store.goal_state(thread.id).await.unwrap().is_none());

        store
            .set_goal(thread.id, Some("legacy objective".into()))
            .await
            .unwrap();
        let replacement = haarness_core::legacy_goal_state("replacement objective".into());
        let replacement_id = replacement.goal_id.clone();
        store
            .save_goal_state(thread.id, Some(replacement))
            .await
            .unwrap();
        let upgraded = {
            let connection = store.connection.lock();
            SqliteStore::upgrade_legacy_goal_if_current(
                &connection,
                thread.id,
                "legacy objective",
                &stale,
            )
            .unwrap()
        };
        assert!(
            !upgraded,
            "a concurrent replacement wins the migration race"
        );
        let canonical = store.goal_state(thread.id).await.unwrap().unwrap();
        assert_eq!(canonical.goal_id, replacement_id);
        assert_eq!(canonical.goal, "replacement objective");
    }

    #[tokio::test]
    async fn recovery_reconciles_terminal_children_left_background_in_the_parent() {
        let store = SqliteStore::in_memory().unwrap();
        let parent = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let child = store
            .create_thread(CreateThreadRequest {
                parent_id: Some(parent.id),
                ..Default::default()
            })
            .await
            .unwrap();
        let request = |text: &str| StartTurnRequest {
            request_id: RequestId::new(),
            text: text.into(),
            attachments: vec![],
            synthetic: false,
            auto_approve: false,
            agent: None,
            provider: None,
            model: None,
            reasoning_effort: None,
            tools: None,
            command: None,
        };
        let parent_turn = store
            .start_turn(parent.id, request("launch child"))
            .await
            .unwrap()
            .0;
        store
            .set_turn_status(parent_turn.id, TurnStatus::Completed, None)
            .await
            .unwrap();
        let child_turn = store
            .start_turn(child.id, request("long-running child"))
            .await
            .unwrap()
            .0;
        store
            .append_item(
                parent.id,
                Some(parent_turn.id),
                ItemData::TaskState {
                    call_id: "call-background".into(),
                    child_thread_id: child.id,
                    child_turn_id: child_turn.id,
                    state: "background".into(),
                },
            )
            .await
            .unwrap();
        // Reproduce a forced shutdown from an older server: the child turn is
        // terminal, but its process-local task runner never updated the parent.
        store
            .set_turn_status(child_turn.id, TurnStatus::Interrupted, None)
            .await
            .unwrap();

        assert_eq!(store.recover_interrupted().await.unwrap(), 0);
        let states = store
            .list_items(parent.id, None)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|item| match item.data {
                ItemData::TaskState {
                    child_thread_id,
                    state,
                    ..
                } if child_thread_id == child.id => Some(state),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(states, ["background", "interrupted"]);

        assert_eq!(store.recover_interrupted().await.unwrap(), 0);
        let reconciled = store
            .list_items(parent.id, None)
            .await
            .unwrap()
            .into_iter()
            .filter(|item| {
                matches!(
                    &item.data,
                    ItemData::TaskState {
                        child_thread_id,
                        state,
                        ..
                    } if *child_thread_id == child.id && state == "interrupted"
                )
            })
            .count();
        assert_eq!(reconciled, 1, "recovery must be idempotent");
    }

    #[tokio::test]
    async fn recovery_names_the_background_tasks_whose_result_never_arrived() {
        let store = SqliteStore::in_memory().unwrap();
        let request = |text: &str| StartTurnRequest {
            request_id: RequestId::new(),
            text: text.into(),
            attachments: vec![],
            synthetic: false,
            auto_approve: false,
            agent: None,
            provider: None,
            model: None,
            reasoning_effort: None,
            tools: None,
            command: None,
        };
        // An idle chat whose background subagent was running, and a busy one
        // whose foreground subagent was.
        let idle = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let busy = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let idle_turn = store
            .start_turn(idle.id, request("launch in the background"))
            .await
            .unwrap()
            .0;
        store
            .set_turn_status(idle_turn.id, TurnStatus::Completed, None)
            .await
            .unwrap();
        let busy_turn = store
            .start_turn(busy.id, request("launch in the foreground"))
            .await
            .unwrap()
            .0;
        let mut children = Vec::new();
        for (parent, turn, state) in [
            (idle.id, idle_turn.id, "background"),
            (busy.id, busy_turn.id, "running"),
        ] {
            let child = store
                .create_thread(CreateThreadRequest {
                    parent_id: Some(parent),
                    ..Default::default()
                })
                .await
                .unwrap();
            let child_turn = store
                .start_turn(child.id, request("child work"))
                .await
                .unwrap()
                .0;
            store
                .append_item(
                    parent,
                    Some(turn),
                    ItemData::TaskState {
                        call_id: format!("call-{state}"),
                        child_thread_id: child.id,
                        child_turn_id: child_turn.id,
                        state: state.into(),
                    },
                )
                .await
                .unwrap();
            children.push((child.id, child_turn.id));
        }

        store
            .append_item(
                idle.id,
                Some(idle_turn.id),
                ItemData::ToolCall {
                    call_id: "call-background".into(),
                    name: "task".into(),
                    arguments: serde_json::json!({"description": " Audit the parser ", "prompt": "child work"}),
                    provider_metadata: serde_json::Value::Null,
                },
            )
            .await
            .unwrap();

        let seen = std::sync::Mutex::new(Vec::new());
        let report = |task: &StrandedTask| {
            seen.lock().unwrap().push(task.clone());
            vec![ItemData::Error {
                message: format!("NOTICE {}", task.description),
                retryable: false,
            }]
        };
        let (stopped, written) = store.recover_interrupted_tasks(&report).await.unwrap();
        assert_eq!(stopped, 3, "both children and the busy chat's turn");
        assert_eq!(
            *seen.lock().unwrap(),
            [StrandedTask {
                parent_thread_id: idle.id,
                parent_turn_id: Some(idle_turn.id),
                call_id: "call-background".into(),
                child_thread_id: children[0].0,
                child_turn_id: children[0].1,
                state: "interrupted".into(),
                description: "Audit the parser".into(),
                reply: None,
                error: Some("server restarted during active turn".into()),
            }],
            "only a background task is owed a result; a foreground one's chat was stopped with it"
        );
        // Written by the recovery itself, with the task's final checkpoint: a
        // crash right after it cannot lose the report.
        assert_eq!(written.len(), 1);
        let items = store.list_items(idle.id, None).await.unwrap();
        let tail = items
            .iter()
            .rev()
            .take(2)
            .map(|item| item.data.clone())
            .collect::<Vec<_>>();
        assert!(
            matches!(&tail[0], ItemData::Error { message, .. } if message == "NOTICE Audit the parser")
        );
        assert!(matches!(&tail[1], ItemData::TaskState { state, .. } if state == "interrupted"));
        assert_eq!(items.last().unwrap().id, written[0].id);
        let (_, again) = store.recover_interrupted_tasks(&report).await.unwrap();
        assert!(again.is_empty(), "a task is reported once");
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn recovery_does_not_report_a_result_the_chat_already_holds_or_an_archived_chat() {
        let store = SqliteStore::in_memory().unwrap();
        let request = |text: &str| StartTurnRequest {
            request_id: RequestId::new(),
            text: text.into(),
            attachments: vec![],
            synthetic: false,
            auto_approve: false,
            agent: None,
            provider: None,
            model: None,
            reasoning_effort: None,
            tools: None,
            command: None,
        };
        let mut chats = Vec::new();
        for _ in 0..2 {
            let parent = store
                .create_thread(CreateThreadRequest::default())
                .await
                .unwrap();
            let parent_turn = store
                .start_turn(parent.id, request("launch"))
                .await
                .unwrap()
                .0;
            store
                .set_turn_status(parent_turn.id, TurnStatus::Completed, None)
                .await
                .unwrap();
            let child = store
                .create_thread(CreateThreadRequest {
                    parent_id: Some(parent.id),
                    ..Default::default()
                })
                .await
                .unwrap();
            let child_turn = store
                .start_turn(child.id, request("child work"))
                .await
                .unwrap()
                .0;
            store
                .append_item(
                    parent.id,
                    Some(parent_turn.id),
                    ItemData::TaskState {
                        call_id: "call".into(),
                        child_thread_id: child.id,
                        child_turn_id: child_turn.id,
                        state: "background".into(),
                    },
                )
                .await
                .unwrap();
            store
                .set_turn_status(child_turn.id, TurnStatus::Completed, None)
                .await
                .unwrap();
            chats.push((parent, parent_turn, child));
        }
        // The first chat got the result, but its "done" checkpoint was lost
        // (or a late background one followed it).
        let (held, held_turn, held_child) = &chats[0];
        store
            .append_item(
                held.id,
                None,
                ItemData::UserMessage {
                    text: format!(
                        "<task id=\"{}\" state=\"completed\">\nDONE\n</task>",
                        held_child.id
                    ),
                    attachments: vec![],
                    queued: false,
                    synthetic: true,
                },
            )
            .await
            .unwrap();
        store
            .append_item(
                held.id,
                Some(held_turn.id),
                ItemData::TaskState {
                    call_id: "call".into(),
                    child_thread_id: held_child.id,
                    child_turn_id: store.last_turn(held_child.id).await.unwrap().unwrap().id,
                    state: "background".into(),
                },
            )
            .await
            .unwrap();
        // The second was archived.
        store
            .update_thread(
                chats[1].0.id,
                haarness_protocol::UpdateThreadRequest {
                    title: None,
                    archived: Some(true),
                    agent: None,
                    provider: None,
                    model: None,
                    ultracode: None,
                },
            )
            .await
            .unwrap();

        let reports = std::sync::Mutex::new(0);
        let report = |_: &StrandedTask| {
            *reports.lock().unwrap() += 1;
            vec![ItemData::Error {
                message: "NOTICE".into(),
                retryable: false,
            }]
        };
        let (_, written) = store.recover_interrupted_tasks(&report).await.unwrap();
        assert!(written.is_empty());
        assert_eq!(*reports.lock().unwrap(), 0);
        for (parent, _, child) in &chats {
            let done = store
                .list_items(parent.id, None)
                .await
                .unwrap()
                .into_iter()
                .any(|item| {
                    matches!(&item.data, ItemData::TaskState { child_thread_id, state, .. }
                    if child_thread_id == &child.id && state == "done")
                });
            assert!(done, "the checkpoint is still reconciled");
        }
    }

    #[tokio::test]
    async fn recovery_never_replays_active_work() {
        let store = SqliteStore::in_memory().unwrap();
        let thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let (turn, _, _) = store
            .start_turn(
                thread.id,
                StartTurnRequest {
                    request_id: RequestId::new(),
                    text: "side effect".into(),
                    attachments: vec![],
                    synthetic: false,
                    auto_approve: false,
                    agent: None,
                    provider: None,
                    model: None,
                    reasoning_effort: None,
                    tools: None,
                    command: None,
                },
            )
            .await
            .unwrap();
        store
            .append_item(
                thread.id,
                Some(turn.id),
                ItemData::AssistantMessage {
                    text: "work recorded before restart".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(store.recover_interrupted().await.unwrap(), 1);
        let recovered = store.get_turn(turn.id).await.unwrap();
        assert_eq!(recovered.status, TurnStatus::Interrupted);
        assert_eq!(
            recovered.error.as_deref(),
            Some("server restarted during active turn")
        );
        let items = store.list_items(thread.id, None).await.unwrap();
        assert!(items.iter().any(|item| {
            matches!(
                &item.data,
                ItemData::AssistantMessage { text }
                    if text == "work recorded before restart"
            )
        }));
        let recovery_notices = items
            .iter()
            .filter(|item| item.turn_id == Some(turn.id))
            .filter_map(|item| match &item.data {
                ItemData::Error { message, retryable } => Some((message, retryable)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(recovery_notices.len(), 1);
        assert_eq!(recovery_notices[0].0, RECOVERED_TURN_MESSAGE);
        assert!(*recovery_notices[0].1);

        assert_eq!(store.recover_interrupted().await.unwrap(), 0);
        let items = store.list_items(thread.id, None).await.unwrap();
        assert_eq!(
            items
                .iter()
                .filter(|item| matches!(&item.data, ItemData::Error { .. }))
                .count(),
            1,
            "recovery is idempotent and never duplicates its transcript notice"
        );
    }

    #[tokio::test]
    async fn recovery_records_one_notice_for_every_orphaned_turn_only() {
        let store = SqliteStore::in_memory().unwrap();
        let first_thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let second_thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let completed_thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap();
        let request = |text: &str| StartTurnRequest {
            request_id: RequestId::new(),
            text: text.into(),
            attachments: vec![],
            synthetic: false,
            auto_approve: false,
            agent: None,
            provider: None,
            model: None,
            reasoning_effort: None,
            tools: None,
            command: None,
        };
        let first = store
            .start_turn(first_thread.id, request("first"))
            .await
            .unwrap()
            .0;
        let second = store
            .start_turn(second_thread.id, request("second"))
            .await
            .unwrap()
            .0;
        store
            .set_turn_status(second.id, TurnStatus::Queued, None)
            .await
            .unwrap();
        let completed = store
            .start_turn(completed_thread.id, request("completed"))
            .await
            .unwrap()
            .0;
        store
            .set_turn_status(completed.id, TurnStatus::Completed, None)
            .await
            .unwrap();

        assert_eq!(store.recover_interrupted().await.unwrap(), 2);
        for (thread, turn) in [(first_thread, first), (second_thread, second)] {
            assert_eq!(
                store.get_turn(turn.id).await.unwrap().status,
                TurnStatus::Interrupted
            );
            let notices = store
                .list_items(thread.id, None)
                .await
                .unwrap()
                .into_iter()
                .filter(|item| {
                    item.turn_id == Some(turn.id)
                        && matches!(
                            &item.data,
                            ItemData::Error { message, retryable }
                                if message == RECOVERED_TURN_MESSAGE && *retryable
                        )
                })
                .count();
            assert_eq!(notices, 1);
        }
        assert_eq!(
            store.get_turn(completed.id).await.unwrap().status,
            TurnStatus::Completed
        );
        assert!(
            store
                .list_items(completed_thread.id, None)
                .await
                .unwrap()
                .iter()
                .all(|item| !matches!(&item.data, ItemData::Error { .. }))
        );
    }
}
