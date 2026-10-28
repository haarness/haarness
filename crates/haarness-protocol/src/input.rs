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

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PendingInput {
    /// The submitting client's preview support. `None` keeps the chat's
    /// recorded value, so the turn sees the same tools as earlier turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<bool>,
    /// Reasoning effort chosen with the message. `None` keeps the effort of
    /// the chat's latest prompt; `default` runs it at the model's own default
    /// over a configured level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    pub id: RequestId,
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<String>,
    /// Queued by a person who can stop the turn it starts (see
    /// `StartTurnRequest::attended`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub attended: bool,
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
        /// Replaces the message's attachments. Omitted keeps them unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attachments: Option<Vec<String>>,
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
