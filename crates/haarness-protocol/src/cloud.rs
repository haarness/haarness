//! Cloud machines, conversations, and reusable templates have distinct identities.
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CloudAgentAction {
    List,
    Get,
    Create,
    Update,
    Delete,
    SshPublicKey,
    Models,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloudAgentInput {
    pub action: CloudAgentAction,
    pub id: Option<Uuid>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub version: Option<i64>,
    pub configuration: Option<Value>,
    pub search: Option<String>,
    pub cursor: Option<Uuid>,
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CloudSessionAction {
    List,
    Get,
    Create,
    Start,
    Stop,
    Rename,
    Update,
    SaveAgent,
    SshPublicKey,
    Archive,
    Delete,
    Ask,
    Messages,
    Wait,
    Abort,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloudSessionInput {
    pub action: CloudSessionAction,
    pub session_id: Option<Uuid>,
    pub configuration: Option<Value>,
    pub version: Option<i64>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub start: Option<bool>,
    pub cloud_agent_id: Option<Uuid>,
    pub thread_id: Option<Uuid>,
    pub turn_id: Option<Uuid>,
    pub title: Option<String>,
    pub message: Option<String>,
    pub reasoning_effort: Option<String>,
    pub archived: Option<bool>,
    pub wait: Option<bool>,
    pub timeout_seconds: Option<u64>,
    pub cursor: Option<Uuid>,
    pub limit: Option<u64>,
    pub after: Option<u64>,
}

/// A chat is a root runtime thread inside a Cloud session. It shares files and compute,
/// but owns its conversation history. Chat operations never create or start a session.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CloudChatAction {
    List,
    Get,
    Create,
    Rename,
    Archive,
    Ask,
    Messages,
    Wait,
    Abort,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloudChatInput {
    pub action: CloudChatAction,
    pub session_id: Uuid,
    /// Explicit chat ID, also called threadId by the runtime. Required except list/create.
    pub chat_id: Option<Uuid>,
    pub turn_id: Option<Uuid>,
    pub title: Option<String>,
    pub message: Option<String>,
    pub reasoning_effort: Option<String>,
    pub archived: Option<bool>,
    pub wait: Option<bool>,
    pub timeout_seconds: Option<u64>,
    pub cursor: Option<Uuid>,
    pub limit: Option<u64>,
    pub after: Option<u64>,
}

/// Cloud timers execute on the Cloud worker even when this device is offline.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CloudScheduleAction {
    List,
    Get,
    Create,
    Update,
    Pause,
    Resume,
    Run,
    History,
    Delete,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloudScheduleInput {
    pub action: CloudScheduleAction,
    pub id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub chat_id: Option<Uuid>,
    pub name: Option<String>,
    pub prompt: Option<String>,
    /// First run, RFC3339 with explicit timezone offset. Required unless everySeconds is given.
    pub at: Option<chrono::DateTime<chrono::Utc>>,
    pub every_seconds: Option<i64>,
    pub version: Option<i64>,
    pub enabled: Option<bool>,
    pub cursor: Option<Uuid>,
    pub limit: Option<u64>,
}
