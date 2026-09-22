//! Stable workflow catalog and durable-run wire contracts.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::ThreadId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(transparent)]
pub struct WorkflowId(pub Uuid);

impl WorkflowId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for WorkflowId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for WorkflowId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowArgumentType {
    String,
    Number,
    Boolean,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowArgument {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<WorkflowArgumentType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPhase {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowMeta {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<WorkflowPhase>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub arguments: BTreeMap<String, WorkflowArgument>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum WorkflowDialect {
    Native,
    ClaudePortable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowFinding {
    pub line: u64,
    pub rule: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDefinition {
    pub name: String,
    pub invocation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_name: Option<String>,
    pub dialect: WorkflowDialect,
    pub path: String,
    #[serde(default)]
    pub legacy: bool,
    pub meta: WorkflowMeta,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<WorkflowFinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowInvalidDefinition {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_name: Option<String>,
    #[serde(default)]
    pub legacy: bool,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowListResponse {
    pub workflows: Vec<WorkflowDefinition>,
    pub invalid: Vec<WorkflowInvalidDefinition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSourceResponse {
    pub definition: WorkflowDefinition,
    pub source: String,
}

/// Start one durable workflow run. Exactly one of `name`, `source`, or `path`
/// must be supplied. Named runs use the statically discovered catalog; inline
/// and explicit-path runs are intended for the model-facing workflow tool and
/// still pass through the coordinator's trust and permission boundary.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStartRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    /// Workflow-wide child-agent defaults. Individual `ctx.agent` calls and
    /// phase model/variant metadata retain higher precedence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// Launching conversation. The coordinator creates a dedicated workflow
    /// child session so tool and agent activity stays inspectable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
    /// Wait for a terminal state before returning. REST/TUI starts normally
    /// leave this false; the CLI can request synchronous execution.
    #[serde(default)]
    pub wait: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    Paused,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowLogSource {
    Platform,
    Workflow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowLog {
    pub sequence: u64,
    pub time: DateTime<Utc>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<WorkflowLogSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunWarning {
    pub time: DateTime<Utc>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunAgentStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunAgent {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    pub status: WorkflowRunAgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunDefinition {
    pub name: String,
    pub invocation: String,
    /// Conversation that launched this run. Execution itself happens in the
    /// run's dedicated `thread_id`; this scope keeps progress notifications
    /// attached to the conversation that requested the workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_name: Option<String>,
    /// Immutable workflow-wide child-agent defaults captured at launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    pub dialect: WorkflowDialect,
    pub meta: WorkflowMeta,
    /// Immutable source snapshot used by restart-from-beginning resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args_provided: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunTime {
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRun {
    pub id: WorkflowId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
    pub directory: String,
    pub workflow_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    /// SHA-256 of the immutable source snapshot.
    pub source_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    pub status: WorkflowRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_payload: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logs: Vec<WorkflowLog>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<WorkflowRunWarning>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<WorkflowRunAgent>,
    pub definition: WorkflowRunDefinition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub time: WorkflowRunTime,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{WorkflowDialect, WorkflowMeta, WorkflowPhase};

    #[test]
    fn workflow_meta_uses_the_public_camel_case_contract() {
        let meta: WorkflowMeta = serde_json::from_value(json!({
            "name": "release",
            "whenToUse": "when shipping",
            "phases": [{"title": "verify", "model": "openai/gpt-5.6"}],
            "arguments": {"dryRun": {"type": "boolean", "default": true}}
        }))
        .unwrap();
        assert_eq!(meta.when_to_use.as_deref(), Some("when shipping"));
        assert_eq!(
            meta.phases,
            vec![WorkflowPhase {
                title: "verify".into(),
                detail: None,
                model: Some("openai/gpt-5.6".into()),
                variant: None,
            }]
        );
        assert_eq!(
            serde_json::to_value(WorkflowDialect::ClaudePortable).unwrap(),
            "claude-portable"
        );
    }
}
