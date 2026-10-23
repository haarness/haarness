//! Per-chat approval mode, owned by the server so every client agrees.
use crate::ThreadId;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Servers advertising this answer eligible permission requests themselves
/// for chats in Auto. Clients must not auto-reply on such a server.
pub const APPROVAL_MODE_CAPABILITY: &str = "approval-mode.v1";

/// Servers advertising this ask a reviewer model in Auto (it is on there): a
/// request it leaves to a person carries [`PermissionReview`], and a call it
/// blocks fails with a `review` of status `blocked` in its result's metadata.
/// Without it, Auto approves eligible requests once.
pub const APPROVAL_REVIEW_CAPABILITY: &str = "approval-review.v1";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// Eligible requests go to a reviewer model, which approves them once,
    /// blocks them, or leaves them to a person.
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

/// Why Auto's reviewer left a request to a person, or blocked a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PermissionReviewStatus {
    /// The reviewer asked for a person's decision.
    Asked,
    /// No review happened: no model answered, the review timed out, or the
    /// action was too long to review whole.
    Failed,
    /// Reviewing paused for the rest of the turn after repeated blocks.
    Paused,
    /// The reviewer refused the call (tool results only).
    Blocked,
}

/// The risk the reviewer rated an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewRisk {
    Low,
    Medium,
    High,
    Critical,
}

/// Auto's reviewer's note: on a permission request it left to a person, and
/// in the metadata (`review`) of a tool result it blocked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PermissionReview {
    pub status: PermissionReviewStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<ReviewRisk>,
    /// A short category the reviewer named (Destructive action, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// One or two sentences for the person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}
