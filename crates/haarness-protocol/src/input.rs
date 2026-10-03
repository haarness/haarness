//! Durable follow-ups waiting for the current work to finish.
use crate::RequestId;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub const INPUT_QUEUE_CAPABILITY: &str = "input-queue.v1";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InputDelivery {
    #[default]
    Steer,
    Queue,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PendingInput {
    /// The submitting graphical client supports sandboxed file previews.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub preview: bool,
    pub id: RequestId,
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InputQueue {
    pub revision: u64,
    pub items: Vec<PendingInput>,
}

/// All edits compare the whole queue revision. A stale client must refresh,
/// never silently overwrite another client's edit or an already-delivered input.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum InputQueueAction {
    Edit {
        revision: u64,
        id: RequestId,
        text: String,
    },
    Remove {
        revision: u64,
        id: RequestId,
    },
    Reorder {
        revision: u64,
        ids: Vec<RequestId>,
    },
    Steer {
        revision: u64,
        id: RequestId,
    },
}
