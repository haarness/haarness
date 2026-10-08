//! Per-chat approval mode, owned by the server so every client agrees.
use crate::ThreadId;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Servers advertising this answer eligible permission requests themselves
/// for chats in Auto. Clients must not auto-reply on such a server.
pub const APPROVAL_MODE_CAPABILITY: &str = "approval-mode.v1";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// Eligible requests are approved once, without a prompt.
    Auto,
    #[default]
    Review,
}

/// What a client may ask for. `Inherit` clears this chat's own setting so it
/// follows the agent that delegated to it (Review at the top level).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalModeSetting {
    Auto,
    Review,
    Inherit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalModeInfo {
    pub thread_id: ThreadId,
    pub mode: ApprovalMode,
    /// The chat whose own setting decided `mode`: this one, or an agent
    /// that delegated to it.
    pub source_thread_id: ThreadId,
    /// True while this chat or a delegating agent has its own setting.
    pub explicit: bool,
    /// Opaque version of every setting that decided `mode`. Send it back
    /// with a change to refuse one made from a stale view.
    pub revision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetApprovalModeRequest {
    pub mode: ApprovalModeSetting,
    /// The `revision` the client last saw. A mismatch answers 409; omit it
    /// to apply the change regardless.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}
