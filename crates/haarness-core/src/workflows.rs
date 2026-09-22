//! Durable workflow-run repository boundary.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use haarness_protocol::{ThreadId, WorkflowId, WorkflowLog, WorkflowLogSource, WorkflowRun};

use crate::CoreResult;

/// A log entry before the repository assigns its per-run sequence number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowLogDraft {
    pub time: DateTime<Utc>,
    pub message: String,
    pub phase: Option<String>,
    pub source: Option<WorkflowLogSource>,
}

/// Durable workflow state. Execution and transition validation belong to the
/// workflow coordinator; this port only persists authoritative snapshots and
/// append-only logs.
#[async_trait]
pub trait WorkflowRepository: Send + Sync {
    /// Insert a run and its immutable source snapshot. Implementations derive
    /// and return `source_hash` from that snapshot instead of trusting input.
    async fn create_workflow_run(&self, run: WorkflowRun) -> CoreResult<WorkflowRun>;
    async fn workflow_run(&self, id: WorkflowId) -> CoreResult<WorkflowRun>;
    async fn list_workflow_runs(
        &self,
        directory: Option<&str>,
        thread_id: Option<ThreadId>,
        limit: usize,
    ) -> CoreResult<Vec<WorkflowRun>>;
    /// Save mutable run state. Immutable identity, source, directory and
    /// definition fields are deliberately not replaced.
    async fn save_workflow_run(&self, run: &WorkflowRun) -> CoreResult<WorkflowRun>;
    async fn append_workflow_log(
        &self,
        id: WorkflowId,
        entry: WorkflowLogDraft,
    ) -> CoreResult<WorkflowLog>;
    async fn delete_workflow_run(&self, id: WorkflowId) -> CoreResult<bool>;
    /// Return every running workflow in one workspace. The coordinator uses
    /// these ids to probe process leases before declaring a run orphaned.
    async fn running_workflow_ids(&self, directory: &str) -> CoreResult<Vec<WorkflowId>>;
    /// Mark only the supplied, lease-proven orphan rows interrupted. Both the
    /// workspace and current `running` status are rechecked transactionally.
    async fn recover_interrupted_workflows(
        &self,
        directory: &str,
        ids: &[WorkflowId],
    ) -> CoreResult<u64>;
}
