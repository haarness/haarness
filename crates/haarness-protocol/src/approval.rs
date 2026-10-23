//! Per-chat approval mode, owned by the server so every client agrees.
use crate::ThreadId;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
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
    /// The reviewer is deciding a request already shown: Auto turned on
    /// while it waited. A person may still answer it.
    Reviewing,
    /// A status a newer server sends.
    #[serde(other)]
    Unknown,
}

/// The risk the reviewer rated an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewRisk {
    Low,
    Medium,
    High,
    Critical,
    /// A rating a newer server sends.
    #[serde(other)]
    Unknown,
}

/// What a note Haarness wrote itself says, so a client can word it in the
/// person's language; `rationale` carries the same in English for clients
/// that do not know the code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ReviewReason {
    /// The review took longer than its deadline.
    Timeout { seconds: u64 },
    /// The action is too long to review whole.
    TooLong,
    /// No model was available to review it.
    NoModel,
    /// The reviewer's calls failed; `detail` is the last error.
    Error { detail: String },
    /// Reviewing paused after `count` blocks in a row this turn.
    Streak { count: u32 },
    /// Reviewing paused after `count` blocks this turn.
    TurnLimit { count: u32 },
    /// The command deletes `path`, outside the project, which the person's
    /// messages do not name.
    UnnamedDeletion { path: String },
    /// The command deletes `path` (as written: a variable, what `xargs`
    /// passes, a path after a directory change), which Haarness cannot place
    /// and the person's messages do not name.
    UnplacedDeletion { path: String },
    /// The command runs `program`'s inline code (`python3 -c`, `perl -e`),
    /// which plainly deletes or names a path outside the project, and the
    /// person's messages do not ask for it.
    InlineCode { program: String },
    /// The reviewer rated the action's risk (see `risk`) and gave no reason.
    Rated,
    /// A code a newer server sends: show `rationale`.
    #[serde(other)]
    Unknown,
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
    /// One or two sentences for the person: the reviewer's own words, or
    /// Haarness's in English when `reason` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// Set when Haarness wrote the note itself.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_reason"
    )]
    pub reason: Option<ReviewReason>,
}

/// A reason this client cannot read (a known code missing a field, a value
/// of the wrong shape) is an unknown one: the note shows `rationale`.
fn lenient_reason<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ReviewReason>, D::Error> {
    Ok(Option::<Value>::deserialize(deserializer)?
        .map(|value| serde_json::from_value(value).unwrap_or(ReviewReason::Unknown)))
}

/// Decode a `review` this client cannot read as no review, never as a
/// failure of the request or item that carries it.
///
/// # Errors
/// Only when the input is not a value at all.
pub fn lenient_review<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<PermissionReview>, D::Error> {
    Ok(Option::<Value>::deserialize(deserializer)?
        .and_then(|value| serde_json::from_value(value).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_a_newer_server_sends_still_decode() {
        let review: PermissionReview = serde_json::from_value(serde_json::json!({
            "status": "escalated",
            "risk": "catastrophic",
            "reason": {"code": "budget", "cents": 3},
            "rationale": "Shown as written."
        }))
        .unwrap();
        assert_eq!(review.status, PermissionReviewStatus::Unknown);
        assert_eq!(review.risk, Some(ReviewRisk::Unknown));
        assert_eq!(review.reason, Some(ReviewReason::Unknown));
        let known: PermissionReview = serde_json::from_value(serde_json::json!({
            "status": "failed",
            "reason": {"code": "timeout", "seconds": 60}
        }))
        .unwrap();
        assert_eq!(known.reason, Some(ReviewReason::Timeout { seconds: 60 }));
        assert_eq!(
            serde_json::to_value(&known.reason).unwrap(),
            serde_json::json!({"code": "timeout", "seconds": 60})
        );
        // A known code missing its field is an unknown reason, not a failure.
        let partial: PermissionReview = serde_json::from_value(serde_json::json!({
            "status": "asked",
            "reason": {"code": "unnamed_deletion"},
            "rationale": "Shown as written."
        }))
        .unwrap();
        assert_eq!(partial.reason, Some(ReviewReason::Unknown));
        assert_eq!(partial.rationale.as_deref(), Some("Shown as written."));
    }

    #[test]
    fn a_review_a_client_cannot_read_leaves_its_request_readable() {
        let thread = ThreadId::new();
        let request = |review: serde_json::Value| {
            serde_json::json!({
                "permissionId": "p1",
                "operation": "bash:ls",
                "threadId": thread,
                "createdAt": "2026-10-23T12:00:00Z",
                "review": review,
            })
        };
        let info: crate::PendingPermissionInfo =
            serde_json::from_value(request(serde_json::json!({"reason": {"code": "timeout"}})))
                .unwrap();
        assert_eq!(info.permission_id, "p1");
        assert!(info.review.is_none());
        let info: crate::PendingPermissionInfo =
            serde_json::from_value(request(serde_json::json!("asked"))).unwrap();
        assert!(info.review.is_none());
        let info: crate::PendingPermissionInfo =
            serde_json::from_value(request(serde_json::json!({"status": "asked"}))).unwrap();
        assert_eq!(info.review.unwrap().status, PermissionReviewStatus::Asked);
        let item: crate::ItemData = serde_json::from_value(serde_json::json!({
            "type": "permission_request",
            "permission_id": "p1",
            "operation": "bash:ls",
            "detail": {},
            "review": {"status": 3},
        }))
        .unwrap();
        assert!(matches!(
            item,
            crate::ItemData::PermissionRequest { review: None, .. }
        ));
    }
}
