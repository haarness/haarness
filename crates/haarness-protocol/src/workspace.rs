use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A durable project groups one primary checkout and any linked Git worktrees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
    /// Primary checkout for a Git project, or the registered directory for a
    /// non-Git project.
    pub root: String,
    pub git: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    Local,
    GitWorktree,
}

/// A concrete directory with its own server, session database, and runtime
/// lease. `managed` means Haarness created it and may reset/remove it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInfo {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub path: String,
    pub kind: WorkspaceKind,
    pub managed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_opened_at: DateTime<Utc>,
    /// Refreshed when the registry is read; missing directories remain visible
    /// so users can explicitly clean up stale metadata.
    pub exists: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceListResponse {
    pub projects: Vec<ProjectInfo>,
    pub workspaces: Vec<WorkspaceInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_workspace_id: Option<String>,
}

/// The Git namespace a discoverable workspace base belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceRefKind {
    LocalBranch,
    RemoteBranch,
    Tag,
}

/// One named Git ref that can be selected as a new workspace's base.
/// `reference` is the unambiguous, fully-qualified value clients should send
/// back; `name` is the concise label intended for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRefInfo {
    pub name: String,
    pub reference: String,
    pub kind: WorkspaceRefKind,
    pub commit: String,
    /// True only for the local branch used by an omitted/blank `fromRef`.
    pub current: bool,
}

/// Discoverable named bases for the current workspace's owning project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRefsResponse {
    /// Stable value recorded by create when `fromRef` is omitted or blank:
    /// the primary checkout's current branch, or its exact detached commit.
    pub default_ref: String,
    pub default_commit: String,
    pub refs: Vec<WorkspaceRefInfo>,
}

/// Resolve an arbitrary Git ref to the commit a workspace would start from.
/// Missing and blank values deliberately use create's default-base behavior.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceResolveRefRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceResolveRefResponse {
    pub reference: String,
    pub commit: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceCreateRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceResetRequest {
    /// Required acknowledgement that commits on the worktree branch may be
    /// moved back to its base ref.
    #[serde(default)]
    pub confirm: bool,
    /// Permit discarding tracked working-tree modifications.
    #[serde(default)]
    pub discard_changes: bool,
    /// Permit deleting untracked (but never ignored) files.
    #[serde(default)]
    pub remove_untracked: bool,
    /// Override the workspace's recorded base ref for this reset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_ref: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRemoveRequest {
    /// Required acknowledgement that the checkout directory will be removed.
    #[serde(default)]
    pub confirm: bool,
    /// Permit removal when tracked or untracked changes exist.
    #[serde(default)]
    pub discard_changes: bool,
    /// Delete the worktree branch after removal. Git's safe `-d` check still
    /// refuses an unmerged branch.
    #[serde(default)]
    pub delete_branch: bool,
}

/// Explicitly remove a stale managed-workspace registration after its checkout
/// has disappeared. This is intentionally a distinct endpoint so an older
/// server cannot interpret cleanup as ordinary worktree deletion.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceCleanupRequest {
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceOperationResponse {
    pub workspace: WorkspaceInfo,
    pub message: String,
}
