use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::RequestId;

/// The most earlier questions one side question may carry.
pub const BTW_HISTORY_LIMIT: usize = 100;
/// The most bytes of earlier questions and answers one side question may carry.
pub const BTW_HISTORY_BYTES: usize = 1024 * 1024;
/// The server streams side questions (`POST /v1/threads/{id}/btw/stream`) and
/// takes their side chat's earlier questions (`history`).
pub const BTW_STREAM_CAPABILITY: &str = "btw.stream.v1";

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BtwRequest {
    pub question: String,
    #[serde(default)]
    pub request_id: RequestId,
    /// Earlier questions of the same side chat and their answers, oldest
    /// first. The server keeps no side chat: a follow-up carries its thread,
    /// and it is sent after the chat's own history, so each follow-up reads
    /// back the previous question's request. At most 100.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<BtwTurn>,
}

/// One finished question of a side chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BtwTurn {
    pub question: String,
    pub answer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BtwResponse {
    pub answer: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// What `POST /v1/threads/{id}/btw/stream` sends, one per event: `delta`
/// extends the answer, `text` replaces it (an attempt after a refused tool
/// call starts over), and the stream ends with `done` or `error`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BtwEvent {
    Delta {
        text: String,
    },
    Text {
        text: String,
    },
    Done {
        answer: String,
        #[serde(rename = "inputTokens")]
        input_tokens: u64,
        #[serde(rename = "outputTokens")]
        output_tokens: u64,
    },
    /// The same code, message and retryable flag a failed `POST /btw` answers with.
    Error {
        code: String,
        message: String,
        retryable: bool,
    },
}
