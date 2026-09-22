//! SQLite workflow-run persistence.

use async_trait::async_trait;
use chrono::Utc;
use haarness_core::{CoreError, CoreResult, WorkflowLogDraft, WorkflowRepository};
use haarness_protocol::{
    ThreadId, WorkflowId, WorkflowLog, WorkflowLogSource, WorkflowRun, WorkflowRunStatus,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{SqliteStore, parse_time, storage, time};

const RUN_COLUMNS: &str = "id,thread_id,directory,workflow_name,source_path,source_snapshot,source_hash,args_json,status,phase,status_payload_json,warnings_json,agents_json,definition_json,result_json,error,created_at,updated_at,started_at,completed_at";

pub(super) fn migrate(connection: &Connection, version: i64) -> CoreResult<()> {
    if version >= 6 {
        return Ok(());
    }
    connection
        .execute_batch(
            "BEGIN;
             CREATE TABLE workflow_runs(
               id TEXT PRIMARY KEY,
               thread_id TEXT REFERENCES threads(id) ON DELETE SET NULL,
               directory TEXT NOT NULL,
               workflow_name TEXT NOT NULL,
               source_path TEXT,
               source_snapshot TEXT NOT NULL,
               source_hash TEXT NOT NULL,
               args_json TEXT,
               status TEXT NOT NULL CHECK(status IN ('running','completed','failed','cancelled','paused','interrupted')),
               phase TEXT,
               status_payload_json TEXT,
               warnings_json TEXT NOT NULL DEFAULT '[]',
               agents_json TEXT NOT NULL DEFAULT '[]',
               definition_json TEXT NOT NULL,
               result_json TEXT,
               error TEXT,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               started_at TEXT,
               completed_at TEXT
             );
             CREATE INDEX idx_workflow_runs_directory_created
               ON workflow_runs(directory, created_at DESC);
             CREATE INDEX idx_workflow_runs_thread_created
               ON workflow_runs(thread_id, created_at DESC);
             CREATE INDEX idx_workflow_runs_status
               ON workflow_runs(status);
             CREATE TABLE workflow_logs(
               run_id TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
               sequence INTEGER NOT NULL,
               time TEXT NOT NULL,
               message TEXT NOT NULL,
               phase TEXT,
               source TEXT CHECK(source IS NULL OR source IN ('platform','workflow')),
               PRIMARY KEY(run_id, sequence)
             );
             CREATE INDEX idx_workflow_logs_run_sequence
               ON workflow_logs(run_id, sequence);
             INSERT INTO schema_migrations(version) VALUES (6);
             COMMIT;",
        )
        .map_err(storage)
}

#[async_trait]
impl WorkflowRepository for SqliteStore {
    async fn create_workflow_run(&self, mut run: WorkflowRun) -> CoreResult<WorkflowRun> {
        let source = run.definition.source.as_deref().ok_or_else(|| {
            CoreError::Invalid("workflow runs require an immutable source snapshot".into())
        })?;
        if !run.logs.is_empty() {
            return Err(CoreError::Invalid(
                "new workflow-run logs must be appended through append_workflow_log".into(),
            ));
        }
        run.source_hash = hex::encode(Sha256::digest(source.as_bytes()));
        if run.source_path.is_none() {
            run.source_path.clone_from(&run.definition.source_path);
        }

        {
            let mut connection = self.connection.lock();
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage)?;
            transaction
                .execute(
                    "INSERT INTO workflow_runs(
                   id,thread_id,directory,workflow_name,source_path,source_snapshot,source_hash,
                   args_json,status,phase,status_payload_json,warnings_json,agents_json,
                   definition_json,result_json,error,created_at,updated_at,started_at,completed_at
                 ) VALUES(
                   ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20
                 )",
                    params![
                        run.id.to_string(),
                        run.thread_id.map(|id| id.to_string()),
                        run.directory,
                        run.workflow_name,
                        run.source_path,
                        source,
                        run.source_hash,
                        optional_json(run.args.as_ref())?,
                        run_status(run.status),
                        run.phase,
                        optional_json(run.status_payload.as_ref())?,
                        json(&run.warnings)?,
                        json(&run.agents)?,
                        json(&run.definition)?,
                        optional_json(run.result.as_ref())?,
                        run.error,
                        time(run.time.created),
                        time(run.time.updated),
                        run.time.started.map(time),
                        run.time.completed.map(time),
                    ],
                )
                .map_err(storage)?;
            transaction.commit().map_err(storage)?;
        }
        self.workflow_run(run.id).await
    }

    async fn workflow_run(&self, id: WorkflowId) -> CoreResult<WorkflowRun> {
        load_run(&self.connection.lock(), id)
    }

    async fn list_workflow_runs(
        &self,
        directory: Option<&str>,
        thread_id: Option<ThreadId>,
        limit: usize,
    ) -> CoreResult<Vec<WorkflowRun>> {
        let connection = self.connection.lock();
        let sql = format!(
            "SELECT {RUN_COLUMNS} FROM workflow_runs
             WHERE (?1 IS NULL OR directory=?1)
               AND (
                 ?2 IS NULL
                 OR json_extract(definition_json,'$.launchThreadId')=?2
                 OR (
                   json_extract(definition_json,'$.launchThreadId') IS NULL
                   AND thread_id=?2
                 )
               )
             ORDER BY created_at DESC, id DESC LIMIT ?3"
        );
        let mut statement = connection.prepare(&sql).map_err(storage)?;
        let rows = statement
            .query_map(
                params![
                    directory,
                    thread_id.map(|id| id.to_string()),
                    i64::try_from(limit.clamp(1, 500)).map_err(storage)?,
                ],
                raw_run,
            )
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        rows.into_iter()
            .map(|raw| decode_run(&connection, raw))
            .collect()
    }

    async fn save_workflow_run(&self, run: &WorkflowRun) -> CoreResult<WorkflowRun> {
        let affected = self
            .connection
            .lock()
            .execute(
                "UPDATE workflow_runs SET
                   status=?2,phase=?3,status_payload_json=?4,warnings_json=?5,agents_json=?6,
                   result_json=?7,error=?8,updated_at=?9,started_at=?10,completed_at=?11
                 WHERE id=?1",
                params![
                    run.id.to_string(),
                    run_status(run.status),
                    run.phase,
                    optional_json(run.status_payload.as_ref())?,
                    json(&run.warnings)?,
                    json(&run.agents)?,
                    optional_json(run.result.as_ref())?,
                    run.error,
                    time(run.time.updated),
                    run.time.started.map(time),
                    run.time.completed.map(time),
                ],
            )
            .map_err(storage)?;
        if affected == 0 {
            return Err(CoreError::NotFound(format!("workflow run {}", run.id)));
        }
        self.workflow_run(run.id).await
    }

    async fn append_workflow_log(
        &self,
        id: WorkflowId,
        entry: WorkflowLogDraft,
    ) -> CoreResult<WorkflowLog> {
        let mut connection = self.connection.lock();
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(storage)?;
        let exists = transaction
            .query_row(
                "SELECT 1 FROM workflow_runs WHERE id=?1",
                [id.to_string()],
                |_| Ok(()),
            )
            .optional()
            .map_err(storage)?
            .is_some();
        if !exists {
            return Err(CoreError::NotFound(format!("workflow run {id}")));
        }
        let sequence: i64 = transaction
            .query_row(
                "SELECT COALESCE(MAX(sequence),0)+1 FROM workflow_logs WHERE run_id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .map_err(storage)?;
        transaction
            .execute(
                "INSERT INTO workflow_logs(run_id,sequence,time,message,phase,source)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    id.to_string(),
                    sequence,
                    time(entry.time),
                    entry.message,
                    entry.phase,
                    entry.source.map(log_source),
                ],
            )
            .map_err(storage)?;
        transaction
            .execute(
                "UPDATE workflow_runs SET updated_at=?2 WHERE id=?1",
                params![id.to_string(), time(entry.time)],
            )
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
        Ok(WorkflowLog {
            sequence: u64::try_from(sequence).map_err(storage)?,
            time: entry.time,
            message: entry.message,
            phase: entry.phase,
            source: entry.source,
        })
    }

    async fn delete_workflow_run(&self, id: WorkflowId) -> CoreResult<bool> {
        self.connection
            .lock()
            .execute("DELETE FROM workflow_runs WHERE id=?1", [id.to_string()])
            .map(|affected| affected > 0)
            .map_err(storage)
    }

    async fn running_workflow_ids(&self, directory: &str) -> CoreResult<Vec<WorkflowId>> {
        let connection = self.connection.lock();
        let mut statement = connection
            .prepare("SELECT id FROM workflow_runs WHERE directory=?1 AND status='running'")
            .map_err(storage)?;
        statement
            .query_map([directory], |row| row.get::<_, String>(0))
            .map_err(storage)?
            .map(|row| workflow_id(&row.map_err(storage)?))
            .collect()
    }

    async fn recover_interrupted_workflows(
        &self,
        directory: &str,
        ids: &[WorkflowId],
    ) -> CoreResult<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        let now = Utc::now();
        let mut connection = self.connection.lock();
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(storage)?;
        let mut affected = 0_u64;
        for id in ids {
            let updated = transaction
                .execute(
                    "UPDATE workflow_runs SET
                   status='interrupted',
                   error=COALESCE(error,'Workflow run was interrupted because the Haarness process stopped.'),
                   updated_at=?3,completed_at=?3
                 WHERE id=?1 AND directory=?2 AND status='running'",
                    params![id.to_string(), directory, time(now)],
                )
                .map_err(storage)?;
            affected = affected.saturating_add(u64::try_from(updated).map_err(storage)?);
        }
        transaction.commit().map_err(storage)?;
        Ok(affected)
    }
}

#[derive(Debug)]
struct RawRun {
    id: String,
    thread_id: Option<String>,
    directory: String,
    workflow_name: String,
    source_path: Option<String>,
    source_snapshot: String,
    source_hash: String,
    args_json: Option<String>,
    status: String,
    phase: Option<String>,
    status_payload_json: Option<String>,
    warnings_json: String,
    agents_json: String,
    definition_json: String,
    result_json: Option<String>,
    error: Option<String>,
    created_at: String,
    updated_at: String,
    started_at: Option<String>,
    completed_at: Option<String>,
}

fn raw_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRun> {
    Ok(RawRun {
        id: row.get(0)?,
        thread_id: row.get(1)?,
        directory: row.get(2)?,
        workflow_name: row.get(3)?,
        source_path: row.get(4)?,
        source_snapshot: row.get(5)?,
        source_hash: row.get(6)?,
        args_json: row.get(7)?,
        status: row.get(8)?,
        phase: row.get(9)?,
        status_payload_json: row.get(10)?,
        warnings_json: row.get(11)?,
        agents_json: row.get(12)?,
        definition_json: row.get(13)?,
        result_json: row.get(14)?,
        error: row.get(15)?,
        created_at: row.get(16)?,
        updated_at: row.get(17)?,
        started_at: row.get(18)?,
        completed_at: row.get(19)?,
    })
}

fn load_run(connection: &Connection, id: WorkflowId) -> CoreResult<WorkflowRun> {
    let sql = format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE id=?1");
    let raw = connection
        .query_row(&sql, [id.to_string()], raw_run)
        .optional()
        .map_err(storage)?
        .ok_or_else(|| CoreError::NotFound(format!("workflow run {id}")))?;
    decode_run(connection, raw)
}

fn decode_run(connection: &Connection, raw: RawRun) -> CoreResult<WorkflowRun> {
    let id = workflow_id(&raw.id)?;
    let mut definition: haarness_protocol::WorkflowRunDefinition =
        serde_json::from_str(&raw.definition_json).map_err(storage)?;
    definition.source = Some(raw.source_snapshot);
    definition.source_path.clone_from(&raw.source_path);
    let mut statement = connection
        .prepare(
            "SELECT sequence,time,message,phase,source
             FROM workflow_logs WHERE run_id=?1 ORDER BY sequence",
        )
        .map_err(storage)?;
    let logs = statement
        .query_map([id.to_string()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(storage)?
        .map(|row| {
            let (sequence, timestamp, message, phase, source) = row.map_err(storage)?;
            Ok(WorkflowLog {
                sequence: u64::try_from(sequence).map_err(storage)?,
                time: parse_time(&timestamp)?,
                message,
                phase,
                source: source.as_deref().map(parse_log_source).transpose()?,
            })
        })
        .collect::<CoreResult<Vec<_>>>()?;
    Ok(WorkflowRun {
        id,
        thread_id: raw.thread_id.as_deref().map(thread_id).transpose()?,
        directory: raw.directory,
        workflow_name: raw.workflow_name,
        source_path: raw.source_path,
        source_hash: raw.source_hash,
        args: parse_optional_json(raw.args_json.as_deref())?,
        status: parse_run_status(&raw.status)?,
        phase: raw.phase,
        status_payload: parse_optional_json(raw.status_payload_json.as_deref())?,
        logs,
        warnings: serde_json::from_str(&raw.warnings_json).map_err(storage)?,
        agents: serde_json::from_str(&raw.agents_json).map_err(storage)?,
        definition,
        result: parse_optional_json(raw.result_json.as_deref())?,
        error: raw.error,
        time: haarness_protocol::WorkflowRunTime {
            created: parse_time(&raw.created_at)?,
            updated: parse_time(&raw.updated_at)?,
            started: raw.started_at.as_deref().map(parse_time).transpose()?,
            completed: raw.completed_at.as_deref().map(parse_time).transpose()?,
        },
    })
}

fn json(value: &impl Serialize) -> CoreResult<String> {
    serde_json::to_string(value).map_err(storage)
}

fn optional_json(value: Option<&serde_json::Value>) -> CoreResult<Option<String>> {
    value.map(json).transpose()
}

fn parse_optional_json(value: Option<&str>) -> CoreResult<Option<serde_json::Value>> {
    value
        .map(|value| serde_json::from_str(value).map_err(storage))
        .transpose()
}

fn workflow_id(value: &str) -> CoreResult<WorkflowId> {
    Ok(WorkflowId(Uuid::parse_str(value).map_err(storage)?))
}

fn thread_id(value: &str) -> CoreResult<ThreadId> {
    Ok(ThreadId(Uuid::parse_str(value).map_err(storage)?))
}

fn run_status(status: WorkflowRunStatus) -> &'static str {
    match status {
        WorkflowRunStatus::Running => "running",
        WorkflowRunStatus::Completed => "completed",
        WorkflowRunStatus::Failed => "failed",
        WorkflowRunStatus::Cancelled => "cancelled",
        WorkflowRunStatus::Paused => "paused",
        WorkflowRunStatus::Interrupted => "interrupted",
    }
}

fn parse_run_status(status: &str) -> CoreResult<WorkflowRunStatus> {
    match status {
        "running" => Ok(WorkflowRunStatus::Running),
        "completed" => Ok(WorkflowRunStatus::Completed),
        "failed" => Ok(WorkflowRunStatus::Failed),
        "cancelled" => Ok(WorkflowRunStatus::Cancelled),
        "paused" => Ok(WorkflowRunStatus::Paused),
        "interrupted" => Ok(WorkflowRunStatus::Interrupted),
        _ => Err(CoreError::Storage(format!(
            "unknown workflow run status {status}"
        ))),
    }
}

fn log_source(source: WorkflowLogSource) -> &'static str {
    match source {
        WorkflowLogSource::Platform => "platform",
        WorkflowLogSource::Workflow => "workflow",
    }
}

fn parse_log_source(source: &str) -> CoreResult<WorkflowLogSource> {
    match source {
        "platform" => Ok(WorkflowLogSource::Platform),
        "workflow" => Ok(WorkflowLogSource::Workflow),
        _ => Err(CoreError::Storage(format!(
            "unknown workflow log source {source}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use haarness_core::{ThreadRepository, WorkflowLogDraft, WorkflowRepository};
    use haarness_protocol::{
        CreateThreadRequest, WorkflowDialect, WorkflowId, WorkflowLogSource, WorkflowMeta,
        WorkflowRun, WorkflowRunDefinition, WorkflowRunStatus, WorkflowRunTime,
    };
    use serde_json::json;

    use crate::SqliteStore;

    fn run(status: WorkflowRunStatus) -> WorkflowRun {
        let now = Utc::now();
        WorkflowRun {
            id: WorkflowId::new(),
            thread_id: None,
            directory: "/workspace".into(),
            workflow_name: "release".into(),
            source_path: Some("/workspace/.haarness/workflows/release.ts".into()),
            source_hash: String::new(),
            args: Some(json!({"dryRun": true})),
            status,
            phase: None,
            status_payload: None,
            logs: Vec::new(),
            warnings: Vec::new(),
            agents: Vec::new(),
            definition: WorkflowRunDefinition {
                name: "release".into(),
                invocation: "release".into(),
                launch_thread_id: None,
                plugin_name: None,
                agent: None,
                model: None,
                variant: None,
                dialect: WorkflowDialect::Native,
                meta: WorkflowMeta {
                    name: "release".into(),
                    description: None,
                    when_to_use: None,
                    phases: Vec::new(),
                    arguments: std::collections::BTreeMap::default(),
                },
                source: Some("export default workflow({name:'release',run(){}})".into()),
                source_path: Some("/workspace/.haarness/workflows/release.ts".into()),
                args_provided: Some(true),
            },
            result: None,
            error: None,
            time: WorkflowRunTime {
                created: now,
                updated: now,
                started: Some(now),
                completed: None,
            },
        }
    }

    #[tokio::test]
    async fn persists_source_snapshot_and_monotonic_append_only_logs() {
        let store = SqliteStore::in_memory().unwrap();
        let created = store
            .create_workflow_run(run(WorkflowRunStatus::Running))
            .await
            .unwrap();
        assert_eq!(created.source_hash.len(), 64);
        assert_eq!(
            created.definition.source.as_deref(),
            Some("export default workflow({name:'release',run(){}})")
        );

        let first = store
            .append_workflow_log(
                created.id,
                WorkflowLogDraft {
                    time: Utc::now(),
                    message: "one".into(),
                    phase: Some("verify".into()),
                    source: Some(WorkflowLogSource::Workflow),
                },
            )
            .await
            .unwrap();
        let second = store
            .append_workflow_log(
                created.id,
                WorkflowLogDraft {
                    time: Utc::now(),
                    message: "two".into(),
                    phase: None,
                    source: Some(WorkflowLogSource::Platform),
                },
            )
            .await
            .unwrap();
        assert_eq!((first.sequence, second.sequence), (1, 2));
        let loaded = store.workflow_run(created.id).await.unwrap();
        assert_eq!(
            loaded
                .logs
                .iter()
                .map(|entry| entry.message.as_str())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
    }

    #[tokio::test]
    async fn recovery_interrupts_only_running_workflows_without_replay() {
        let store = SqliteStore::in_memory().unwrap();
        let running = store
            .create_workflow_run(run(WorkflowRunStatus::Running))
            .await
            .unwrap();
        let completed = store
            .create_workflow_run(run(WorkflowRunStatus::Completed))
            .await
            .unwrap();

        let ids = store.running_workflow_ids("/workspace").await.unwrap();
        assert_eq!(
            store
                .recover_interrupted_workflows("/workspace", &ids)
                .await
                .unwrap(),
            1
        );
        let recovered = store.workflow_run(running.id).await.unwrap();
        assert_eq!(recovered.status, WorkflowRunStatus::Interrupted);
        assert!(recovered.error.unwrap().contains("process stopped"));
        assert_eq!(
            store.workflow_run(completed.id).await.unwrap().status,
            WorkflowRunStatus::Completed
        );
    }

    #[tokio::test]
    async fn creating_a_run_without_a_source_snapshot_fails_closed() {
        let store = SqliteStore::in_memory().unwrap();
        let mut value = run(WorkflowRunStatus::Running);
        value.definition.source = None;
        assert!(store.create_workflow_run(value).await.is_err());
    }

    #[tokio::test]
    async fn run_list_scopes_to_launch_thread_with_legacy_execution_thread_fallback() {
        let store = SqliteStore::in_memory().unwrap();
        let launch_thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap()
            .id;
        let execution_thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap()
            .id;
        let mut launched = run(WorkflowRunStatus::Completed);
        launched.thread_id = Some(execution_thread);
        launched.definition.launch_thread_id = Some(launch_thread);
        let launched = store.create_workflow_run(launched).await.unwrap();

        let legacy_execution_thread = store
            .create_thread(CreateThreadRequest::default())
            .await
            .unwrap()
            .id;
        let mut legacy = run(WorkflowRunStatus::Completed);
        legacy.thread_id = Some(legacy_execution_thread);
        let legacy = store.create_workflow_run(legacy).await.unwrap();

        let launch_runs = store
            .list_workflow_runs(Some("/workspace"), Some(launch_thread), 10)
            .await
            .unwrap();
        assert_eq!(
            launch_runs.iter().map(|run| run.id).collect::<Vec<_>>(),
            [launched.id]
        );
        assert!(
            store
                .list_workflow_runs(Some("/workspace"), Some(execution_thread), 10)
                .await
                .unwrap()
                .is_empty(),
            "new rows scope by launch conversation, not the execution child"
        );
        let legacy_runs = store
            .list_workflow_runs(Some("/workspace"), Some(legacy_execution_thread), 10)
            .await
            .unwrap();
        assert_eq!(
            legacy_runs.iter().map(|run| run.id).collect::<Vec<_>>(),
            [legacy.id]
        );
    }
}
