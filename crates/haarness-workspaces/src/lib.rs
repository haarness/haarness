//! Durable project/workspace registry and conservative Git worktree lifecycle.
//!
//! The registry is shared by every workspace server under one Haarness data
//! root. Mutations hold an inter-process file lock and replace the JSON file
//! atomically. Destructive Git operations additionally require the target
//! workspace's activity lease to be free.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError, mpsc},
    time::{Duration, Instant},
};

use chrono::Utc;
use fs2::FileExt as _;
use haarness_protocol::{
    ProjectInfo, WorkspaceCleanupRequest, WorkspaceCreateRequest, WorkspaceInfo, WorkspaceKind,
    WorkspaceListResponse, WorkspaceOperationResponse, WorkspaceRefInfo, WorkspaceRefKind,
    WorkspaceRefsResponse, WorkspaceRemoveRequest, WorkspaceResetRequest,
    WorkspaceResolveRefRequest, WorkspaceResolveRefResponse,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const REGISTRY_VERSION: u32 = 1;
const WORKSPACE_ID_MARKER: &str = "haarness-workspace-id";

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("I/O error: {0}")]
    Io(String),
    #[error("Git error: {0}")]
    Git(String),
}

pub type Result<T> = std::result::Result<T, WorkspaceError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Registry {
    version: u32,
    projects: Vec<ProjectInfo>,
    workspaces: Vec<WorkspaceInfo>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            version: REGISTRY_VERSION,
            projects: Vec::new(),
            workspaces: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceManager {
    registry_path: PathBuf,
    registry_lock: PathBuf,
    managed_root: PathBuf,
    activity_root: PathBuf,
}

impl WorkspaceManager {
    /// Shared history root. Reading analytics must not refresh or mutate the registry.
    pub fn data_directory(&self) -> &Path {
        self.registry_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
    }

    pub fn new(
        registry_path: impl Into<PathBuf>,
        managed_root: impl Into<PathBuf>,
        activity_root: impl Into<PathBuf>,
    ) -> Result<Self> {
        let manager = Self {
            registry_path: registry_path.into(),
            registry_lock: PathBuf::new(),
            managed_root: managed_root.into(),
            activity_root: activity_root.into(),
        };
        let registry_lock = manager.registry_path.with_extension("lock");
        for directory in [
            manager.registry_path.parent(),
            Some(manager.managed_root.as_path()),
            Some(manager.activity_root.as_path()),
        ]
        .into_iter()
        .flatten()
        {
            fs::create_dir_all(directory).map_err(io_error)?;
        }
        Ok(Self {
            registry_lock,
            ..manager
        })
    }

    /// Register an existing directory as a local workspace. Re-registering is
    /// idempotent and refreshes its branch and last-opened timestamp.
    ///
    /// Git and the folder are consulted before the registry lock is taken, so
    /// a folder whose git stalls holds up only this call, never every other
    /// registry read. A managed checkout's identity is checked against the
    /// registry as it was read; the update refuses if that entry changed
    /// meanwhile.
    pub fn register_local(&self, path: &Path) -> Result<WorkspaceInfo> {
        let path = canonical_directory(path)?;
        let identity = project_identity(&path)?;
        let branch = git_branch(&path);
        let snapshot = self.read_registry_locked()?;
        let verified = match snapshot
            .workspaces
            .iter()
            .find(|workspace| same_path(Path::new(&workspace.path), &path))
        {
            Some(workspace) if workspace.managed => {
                let project = project_for(&snapshot, workspace)?;
                verify_workspace_identity(workspace, project)?;
                Some(managed_identity(workspace, project))
            }
            _ => None,
        };
        self.with_registry_mut(|registry| {
            let now = Utc::now();
            let project_id = if let Some(project) = registry
                .projects
                .iter_mut()
                .find(|project| same_path(Path::new(&project.root), &identity.root))
            {
                project.git = identity.git;
                project.id.clone()
            } else {
                let id = Uuid::now_v7().to_string();
                registry.projects.push(ProjectInfo {
                    id: id.clone(),
                    name: path_name(&identity.root),
                    root: identity.root.display().to_string(),
                    git: identity.git,
                    created_at: now,
                });
                id
            };
            if let Some(index) = registry
                .workspaces
                .iter()
                .position(|workspace| same_path(Path::new(&workspace.path), &path))
            {
                if registry.workspaces[index].managed {
                    let workspace = &registry.workspaces[index];
                    let project = project_for(registry, workspace)?;
                    if verified.as_ref() != Some(&managed_identity(workspace, project)) {
                        return Err(registry_changed());
                    }
                }
                let workspace = &mut registry.workspaces[index];
                workspace.last_opened_at = now;
                workspace.exists = true;
                workspace.branch.clone_from(&branch);
                if !workspace.managed {
                    workspace.project_id = project_id;
                    workspace.kind = if identity.linked {
                        WorkspaceKind::GitWorktree
                    } else {
                        WorkspaceKind::Local
                    };
                }
                return Ok(workspace.clone());
            }
            let id = Uuid::now_v7().to_string();
            let workspace = WorkspaceInfo {
                id,
                project_id,
                name: unique_name(registry, &path_name(&path)),
                path: path.display().to_string(),
                kind: if identity.linked {
                    WorkspaceKind::GitWorktree
                } else {
                    WorkspaceKind::Local
                },
                managed: false,
                branch,
                from_ref: None,
                base_commit: None,
                created_at: now,
                last_opened_at: now,
                exists: true,
            };
            registry.workspaces.push(workspace.clone());
            Ok(workspace)
        })
    }

    /// Register every directory that exists, as `register_local` does, all at
    /// once and waiting at most a second. Desktop sends every folder of its
    /// sidebar with each project-list request; one that has not answered by
    /// then (a slow network volume, one that went away, a privacy prompt
    /// nobody is there to answer) is left as registered before and returned
    /// as pending, for the caller to finish without holding its answer.
    pub fn register_existing(&self, directories: &[PathBuf]) -> Result<Registered> {
        let manager = self.clone();
        register_folders(
            directories,
            Arc::new(move |path: &Path| path.is_dir().then(|| manager.register_local(path))),
            PROBE_PATIENCE,
        )
    }

    /// Registry metadata only: startup must not probe unrelated directories.
    pub fn registered_count(&self) -> Result<usize> {
        Ok(self.read_registry_locked()?.workspaces.len())
    }

    /// The registered workspaces as stored, without looking at any folder:
    /// `exists` and `branch` are as last recorded. For callers that run
    /// often and need only ids, names and paths, such as the remote
    /// connector, since unrelated folders may be slow or offline.
    pub fn registered(&self) -> Result<Vec<WorkspaceInfo>> {
        Ok(self.read_registry_locked()?.workspaces)
    }

    /// Read the project identity last validated by register_local/open_workspace.
    pub fn project_for_workspace(&self, reference: &str) -> Result<ProjectInfo> {
        let registry = self.read_registry_locked()?;
        let workspace = resolve_workspace(&registry, reference)?;
        project_for(&registry, workspace).cloned()
    }

    /// Every project and workspace, each folder looked at for whether it
    /// still exists and its branch. The registry lock is released first, and
    /// a folder that does not answer within a second shows as last recorded
    /// (see `refresh_folders`), so a slow folder never holds up the list or
    /// another process's registry access.
    pub fn list(&self, current_workspace_id: Option<&str>) -> Result<WorkspaceListResponse> {
        let mut registry = self.read_registry_locked()?;
        refresh_existence(&mut registry);
        registry.projects.sort_by(|a, b| a.name.cmp(&b.name));
        registry.workspaces.sort_by(|a, b| {
            b.last_opened_at
                .cmp(&a.last_opened_at)
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(WorkspaceListResponse {
            projects: registry.projects,
            workspaces: registry.workspaces,
            current_workspace_id: current_workspace_id.map(str::to_owned),
        })
    }

    /// List the local branches, remote branches, and tags available as bases
    /// for a new worktree in the current workspace's owning project.
    pub fn refs(&self, current_workspace_id: &str) -> Result<WorkspaceRefsResponse> {
        let registry = self.read_registry_locked()?;
        let current = resolve_workspace(&registry, current_workspace_id)?;
        let project = project_for(&registry, current)?;
        if !project.git {
            return Err(WorkspaceError::Invalid(
                "Git refs require a Git project".into(),
            ));
        }
        let root = Path::new(&project.root);
        let default = resolve_base_ref(root, None)?;
        let refs = discover_refs(root, &default.reference)?;
        Ok(WorkspaceRefsResponse {
            default_ref: default.reference,
            default_commit: default.commit,
            refs,
        })
    }

    /// Validate and resolve a selectable/custom base to an exact commit.
    /// Missing and blank references use exactly the same default as create.
    pub fn resolve_ref(
        &self,
        current_workspace_id: &str,
        request: &WorkspaceResolveRefRequest,
    ) -> Result<WorkspaceResolveRefResponse> {
        let registry = self.read_registry_locked()?;
        let current = resolve_workspace(&registry, current_workspace_id)?;
        let project = project_for(&registry, current)?;
        if !project.git {
            return Err(WorkspaceError::Invalid(
                "Git refs require a Git project".into(),
            ));
        }
        resolve_base_ref(Path::new(&project.root), request.reference.as_deref())
    }

    pub fn get(&self, reference: &str) -> Result<WorkspaceInfo> {
        let registry = self.read_registry_locked()?;
        let mut workspace = resolve_workspace(&registry, reference)?.clone();
        // This is also used for every routed remote request. Inspect only the
        // requested directory; unrelated repositories may be slow or offline.
        refresh_workspace(&mut workspace);
        Ok(workspace)
    }

    /// Resolve and mark a workspace as most recently opened. Starting or
    /// attaching its workspace-scoped server is a client responsibility.
    pub fn open_workspace(&self, reference: &str) -> Result<WorkspaceInfo> {
        // As in `register_local`, the folder and git are consulted outside
        // the registry lock, against the registry as it was read.
        let snapshot = self.read_registry_locked()?;
        let workspace = resolve_workspace(&snapshot, reference)?.clone();
        if !Path::new(&workspace.path).is_dir() {
            return Err(WorkspaceError::Conflict(format!(
                "workspace directory is missing: {}",
                workspace.path
            )));
        }
        let verified = if workspace.managed {
            let project = project_for(&snapshot, &workspace)?;
            verify_workspace_identity(&workspace, project)?;
            Some(managed_identity(&workspace, project))
        } else {
            None
        };
        let branch = git_branch(Path::new(&workspace.path));
        self.with_registry_mut(|registry| {
            let current = resolve_workspace(registry, &workspace.id)?;
            let unchanged = current.path == workspace.path
                && current.managed == workspace.managed
                && match &verified {
                    Some(verified) => {
                        *verified == managed_identity(current, project_for(registry, current)?)
                    }
                    None => true,
                };
            if !unchanged {
                return Err(registry_changed());
            }
            let workspace = resolve_workspace_mut(registry, &workspace.id)?;
            workspace.exists = true;
            workspace.last_opened_at = Utc::now();
            workspace.branch = branch;
            Ok(workspace.clone())
        })
    }

    /// Create a new linked checkout with a new, non-tracking branch.
    pub fn create_worktree(
        &self,
        current_workspace_id: &str,
        request: &WorkspaceCreateRequest,
    ) -> Result<WorkspaceOperationResponse> {
        let _lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let current = resolve_workspace(&registry, current_workspace_id)?.clone();
        let project = registry
            .projects
            .iter()
            .find(|project| project.id == current.project_id)
            .cloned()
            .ok_or_else(|| WorkspaceError::NotFound("owning project".into()))?;
        if !project.git {
            return Err(WorkspaceError::Invalid(
                "Git worktrees require a Git project".into(),
            ));
        }
        let name = request.name.trim();
        if name.is_empty() {
            return Err(WorkspaceError::Invalid(
                "workspace name cannot be empty".into(),
            ));
        }
        if registry
            .workspaces
            .iter()
            .any(|workspace| workspace.project_id == project.id && workspace.name == name)
        {
            return Err(WorkspaceError::Conflict(format!(
                "workspace name already exists in this project: {name}"
            )));
        }

        let root = PathBuf::from(&project.root);
        let resolved_base = resolve_base_ref(&root, request.from_ref.as_deref())?;
        let from_ref = resolved_base.reference;
        let base_commit = resolved_base.commit;
        let workspace_id = Uuid::now_v7().to_string();
        let slug = slug(name);
        let branch = request.branch.as_deref().map_or_else(
            || format!("haarness/{slug}-{}", &workspace_id[..8]),
            str::to_owned,
        );
        validate_branch(&root, &branch)?;
        if git_success(
            &root,
            [
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )? {
            return Err(WorkspaceError::Conflict(format!(
                "Git branch already exists: {branch}"
            )));
        }

        let path = requested_worktree_path(
            request.path.as_deref(),
            &self.managed_root,
            &project.id,
            &format!("{slug}-{}", &workspace_id[..8]),
            &root,
        )?;
        if path.exists() {
            return Err(WorkspaceError::Conflict(format!(
                "worktree path already exists: {}",
                path.display()
            )));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        git_checked(
            &root,
            [
                "worktree",
                "add",
                "--no-track",
                "-b",
                &branch,
                path.to_string_lossy().as_ref(),
                &base_commit,
            ],
        )?;
        let canonical = canonical_directory(&path)?;
        if let Err(error) = write_workspace_identity(&canonical, &workspace_id) {
            let _ = git_checked(
                &root,
                [
                    "worktree",
                    "remove",
                    "--force",
                    canonical.to_string_lossy().as_ref(),
                ],
            );
            let _ = git_checked(&root, ["branch", "-D", &branch]);
            return Err(error);
        }
        let now = Utc::now();
        let workspace = WorkspaceInfo {
            id: workspace_id,
            project_id: project.id,
            name: name.to_owned(),
            path: canonical.display().to_string(),
            kind: WorkspaceKind::GitWorktree,
            managed: true,
            branch: Some(branch.clone()),
            from_ref: Some(from_ref),
            base_commit: Some(base_commit),
            created_at: now,
            last_opened_at: now,
            exists: true,
        };
        registry.workspaces.push(workspace.clone());
        if let Err(error) = self.write_registry(&registry) {
            let _ = git_checked(
                &root,
                [
                    "worktree",
                    "remove",
                    "--force",
                    canonical.to_string_lossy().as_ref(),
                ],
            );
            let _ = git_checked(&root, ["branch", "-D", &branch]);
            return Err(error);
        }
        Ok(WorkspaceOperationResponse {
            message: format!(
                "created worktree {} on branch {} from {}",
                canonical.display(),
                workspace.branch.as_deref().unwrap_or_default(),
                workspace.from_ref.as_deref().unwrap_or_default()
            ),
            workspace,
        })
    }

    pub fn reset_worktree(
        &self,
        reference: &str,
        request: &WorkspaceResetRequest,
    ) -> Result<WorkspaceOperationResponse> {
        if !request.confirm {
            return Err(WorkspaceError::Invalid(
                "reset requires confirm=true because it moves the worktree branch".into(),
            ));
        }
        if request.remove_untracked && !request.discard_changes {
            return Err(WorkspaceError::Invalid(
                "removeUntracked requires discardChanges=true".into(),
            ));
        }
        let _registry_lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let workspace = resolve_workspace(&registry, reference)?.clone();
        ensure_managed_worktree(&workspace)?;
        let _activity = self.acquire_inactive_guard(&workspace.id)?;
        let project = project_for(&registry, &workspace)?;
        let path = PathBuf::from(&workspace.path);
        verify_workspace_identity(&workspace, project)?;
        let status = worktree_status(&path)?;
        if status.tracked && !request.discard_changes {
            return Err(WorkspaceError::Conflict(
                "worktree has tracked changes; retry with discardChanges=true after reviewing them"
                    .into(),
            ));
        }
        let target_ref = request
            .to_ref
            .as_deref()
            .or(workspace.from_ref.as_deref())
            .unwrap_or("HEAD")
            .to_owned();
        let commit = verify_commit(Path::new(&project.root), &target_ref)?;
        ensure_reset_paths_safe(&path, &commit, request.remove_untracked)?;
        git_checked(&path, ["reset", "--hard", &commit])?;
        if request.remove_untracked {
            // Deliberately excludes ignored files (`-x` is never used).
            git_checked(&path, ["clean", "-fd"])?;
        }
        let updated = resolve_workspace_mut(&mut registry, &workspace.id)?;
        updated.from_ref = Some(target_ref.clone());
        updated.base_commit = Some(commit.clone());
        updated.branch = git_branch(&path);
        updated.last_opened_at = Utc::now();
        let updated = updated.clone();
        self.write_registry(&registry)?;
        Ok(WorkspaceOperationResponse {
            workspace: updated,
            message: format!("reset worktree to {target_ref} ({})", &commit[..12]),
        })
    }

    pub fn remove_worktree(
        &self,
        reference: &str,
        request: &WorkspaceRemoveRequest,
    ) -> Result<WorkspaceOperationResponse> {
        if !request.confirm {
            return Err(WorkspaceError::Invalid(
                "remove requires confirm=true because it deletes the checkout directory".into(),
            ));
        }
        let _registry_lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let workspace = resolve_workspace(&registry, reference)?.clone();
        ensure_managed_worktree(&workspace)?;
        let _activity = self.acquire_inactive_guard(&workspace.id)?;
        let project = project_for(&registry, &workspace)?.clone();
        let path = PathBuf::from(&workspace.path);
        if !path.exists() {
            return Err(WorkspaceError::Conflict(
                "workspace directory is missing; use stale-registration cleanup instead of worktree removal"
                    .into(),
            ));
        }
        // The registry's branch is a discovery cache, not deletion authority:
        // users may switch branches inside a worktree between registry writes.
        // Resolve the live branch while the checkout still exists and use only
        // that value for the optional `git branch -d` below.
        verify_workspace_identity(&workspace, &project)?;
        let live_branch = git_branch(&path);
        let retained_branch = live_branch.clone().or_else(|| workspace.branch.clone());
        if std::env::current_dir()
            .ok()
            .and_then(|directory| directory.canonicalize().ok())
            .is_some_and(|directory| directory.starts_with(&path))
        {
            return Err(WorkspaceError::Conflict(
                "cannot remove the workspace containing this process's current directory; run the command from another workspace"
                    .into(),
            ));
        }
        let status = worktree_status(&path)?;
        if (status.tracked || status.untracked) && !request.discard_changes {
            return Err(WorkspaceError::Conflict(
                "worktree has local changes; retry with discardChanges=true after reviewing them"
                    .into(),
            ));
        }
        let mut args = vec!["worktree", "remove"];
        if request.discard_changes {
            args.push("--force");
        }
        args.push(&workspace.path);
        git_checked(Path::new(&project.root), args)?;

        registry
            .workspaces
            .retain(|candidate| candidate.id != workspace.id);
        self.write_registry(&registry)?;
        let mut message = format!("removed worktree {}", workspace.path);
        let mut branch_deleted = false;
        if request.delete_branch {
            if let Some(branch) = live_branch.as_deref() {
                match git_checked(Path::new(&project.root), ["branch", "-d", branch]) {
                    Ok(_) => {
                        branch_deleted = true;
                        let _ = write!(&mut message, " and deleted merged branch {branch}");
                    }
                    Err(error) => {
                        let _ = write!(&mut message, "; branch {branch} was retained ({error})");
                    }
                }
            } else if let Some(branch) = retained_branch.as_deref() {
                let _ = write!(
                    &mut message,
                    "; cached branch {branch} was retained because it was not the worktree's live branch"
                );
            }
        }
        let mut removed = workspace;
        removed.exists = false;
        removed.branch = retained_branch;
        // Preserve the branch name when safe deletion was refused so clients
        // can report that partial outcome without parsing a path-bearing
        // human message. `None` on a requested deletion means Git confirmed
        // the branch is gone.
        if branch_deleted {
            removed.branch = None;
        }
        Ok(WorkspaceOperationResponse {
            workspace: removed,
            message,
        })
    }

    /// Remove only a missing managed workspace's stale registry row. This
    /// deliberately never invokes Git or deletes a filesystem path.
    pub fn cleanup_missing_workspace(
        &self,
        reference: &str,
        request: &WorkspaceCleanupRequest,
    ) -> Result<WorkspaceOperationResponse> {
        if !request.confirm {
            return Err(WorkspaceError::Invalid(
                "stale workspace cleanup requires confirm=true".into(),
            ));
        }
        let _registry_lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let workspace = resolve_workspace(&registry, reference)?.clone();
        ensure_managed_worktree(&workspace)?;
        let _activity = self.acquire_inactive_guard(&workspace.id)?;
        if Path::new(&workspace.path).exists() {
            return Err(WorkspaceError::Conflict(
                "workspace directory exists; stale-registration cleanup cannot remove a live checkout"
                    .into(),
            ));
        }
        registry
            .workspaces
            .retain(|candidate| candidate.id != workspace.id);
        self.write_registry(&registry)?;
        let mut removed = workspace;
        removed.exists = false;
        let mut message = format!("removed stale workspace record {}", removed.path);
        if let Some(branch) = removed.branch.as_deref() {
            let _ = write!(
                &mut message,
                "; branch {branch} was retained because workspace identity could not be verified"
            );
        }
        Ok(WorkspaceOperationResponse {
            workspace: removed,
            message,
        })
    }

    #[must_use]
    pub fn activity_lock_path(&self, workspace_id: &str) -> PathBuf {
        self.activity_root.join(workspace_id).join("server.lock")
    }

    fn acquire_inactive_guard(&self, workspace_id: &str) -> Result<File> {
        let path = self.activity_lock_path(workspace_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(io_error)?;
        file.try_lock_exclusive().map_err(|_| {
            WorkspaceError::Conflict(
                "workspace has an active server; stop it before reset or removal".into(),
            )
        })?;
        Ok(file)
    }

    fn with_registry_mut<T>(
        &self,
        operation: impl FnOnce(&mut Registry) -> Result<T>,
    ) -> Result<T> {
        let _lock = self.lock_registry()?;
        let mut registry = self.read_registry()?;
        let result = operation(&mut registry)?;
        self.write_registry(&registry)?;
        Ok(result)
    }

    fn read_registry_locked(&self) -> Result<Registry> {
        let _lock = self.lock_registry()?;
        self.read_registry()
    }

    fn lock_registry(&self) -> Result<File> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&self.registry_lock)
            .map_err(io_error)?;
        file.lock_exclusive().map_err(io_error)?;
        Ok(file)
    }

    fn read_registry(&self) -> Result<Registry> {
        match fs::read(&self.registry_path) {
            Ok(bytes) => {
                let registry: Registry = serde_json::from_slice(&bytes)
                    .map_err(|error| WorkspaceError::Io(error.to_string()))?;
                if registry.version != REGISTRY_VERSION {
                    return Err(WorkspaceError::Invalid(format!(
                        "unsupported workspace registry version {}",
                        registry.version
                    )));
                }
                Ok(registry)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
            Err(error) => Err(io_error(error)),
        }
    }

    fn write_registry(&self, registry: &Registry) -> Result<()> {
        let parent = self.registry_path.parent().ok_or_else(|| {
            WorkspaceError::Io("workspace registry has no parent directory".into())
        })?;
        fs::create_dir_all(parent).map_err(io_error)?;
        let temporary = parent.join(format!(".workspaces-{}.tmp", Uuid::now_v7()));
        let bytes = serde_json::to_vec_pretty(registry)
            .map_err(|error| WorkspaceError::Io(error.to_string()))?;
        let result = (|| -> std::io::Result<()> {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, &self.registry_path)?;
            // Windows cannot open a directory as a file (access denied), so
            // this failed every command there; NTFS journals the rename.
            #[cfg(unix)]
            File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(io_error)
    }
}

#[derive(Debug)]
struct ProjectIdentity {
    root: PathBuf,
    git: bool,
    linked: bool,
}

fn project_identity(path: &Path) -> Result<ProjectIdentity> {
    let top = git_optional(path, ["rev-parse", "--show-toplevel"])?;
    let Some(top) = top else {
        return Ok(ProjectIdentity {
            root: path.to_path_buf(),
            git: false,
            linked: false,
        });
    };
    let common = git_checked(path, ["rev-parse", "--git-common-dir"])?;
    let common = PathBuf::from(common.trim());
    let common = if common.is_absolute() {
        common
    } else {
        path.join(common)
    };
    let common = common.canonicalize().map_err(io_error)?;
    let root = common
        .file_name()
        .is_some_and(|name| name == ".git")
        .then(|| common.parent().map(Path::to_path_buf))
        .flatten()
        .unwrap_or_else(|| PathBuf::from(top.trim()));
    Ok(ProjectIdentity {
        linked: !same_path(Path::new(top.trim()), &root),
        root: root.canonicalize().map_err(io_error)?,
        git: true,
    })
}

fn requested_worktree_path(
    requested: Option<&str>,
    managed_root: &Path,
    project_id: &str,
    generated_name: &str,
    project_root: &Path,
) -> Result<PathBuf> {
    let path = requested.map_or_else(
        || managed_root.join(project_id).join(generated_name),
        PathBuf::from,
    );
    let absolute = if path.is_absolute() {
        path
    } else {
        project_root.join(path)
    };
    let parent = absolute.parent().ok_or_else(|| {
        WorkspaceError::Invalid("worktree path must have a parent directory".into())
    })?;
    fs::create_dir_all(parent).map_err(io_error)?;
    let parent = parent.canonicalize().map_err(io_error)?;
    let name = absolute.file_name().ok_or_else(|| {
        WorkspaceError::Invalid("worktree path must end in a directory name".into())
    })?;
    Ok(parent.join(name))
}

fn ensure_managed_worktree(workspace: &WorkspaceInfo) -> Result<()> {
    if !workspace.managed || workspace.kind != WorkspaceKind::GitWorktree {
        return Err(WorkspaceError::Invalid(
            "only Haarness-managed Git worktrees can be reset or removed".into(),
        ));
    }
    Ok(())
}

fn project_for<'a>(registry: &'a Registry, workspace: &WorkspaceInfo) -> Result<&'a ProjectInfo> {
    registry
        .projects
        .iter()
        .find(|project| project.id == workspace.project_id)
        .ok_or_else(|| WorkspaceError::NotFound("owning project".into()))
}

#[derive(Debug, Default)]
struct WorktreeStatus {
    tracked: bool,
    untracked: bool,
}

fn worktree_status(path: &Path) -> Result<WorktreeStatus> {
    let output = git_checked(path, ["status", "--porcelain=v1", "-z"])?;
    let mut status = WorktreeStatus::default();
    for record in output.split('\0').filter(|record| !record.is_empty()) {
        if record.starts_with("?? ") {
            status.untracked = true;
        } else {
            status.tracked = true;
        }
    }
    Ok(status)
}

/// `git reset --hard` can overwrite or remove an untracked file when the
/// target commit needs the same path (or a file/directory ancestor). Preserve
/// such files unless the caller explicitly requested untracked cleanup;
/// ignored paths are never covered by that request and are always protected.
fn ensure_reset_paths_safe(path: &Path, commit: &str, remove_untracked: bool) -> Result<()> {
    let targets = git_null_paths(path, ["ls-tree", "-r", "-z", "--name-only", commit])?;
    let mut protected = git_null_paths(
        path,
        [
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
        ],
    )?
    .into_iter()
    .map(|path| (path, true))
    .collect::<Vec<_>>();
    if !remove_untracked {
        protected.extend(
            git_null_paths(path, ["ls-files", "--others", "--exclude-standard", "-z"])?
                .into_iter()
                .map(|path| (path, false)),
        );
    }
    protected.sort_by(|left, right| left.0.cmp(&right.0));
    for (existing, ignored) in protected {
        if targets
            .iter()
            .any(|target| paths_component_overlap(target, &existing))
        {
            let kind = if ignored { "ignored" } else { "untracked" };
            return Err(WorkspaceError::Conflict(format!(
                "{kind} path would collide with reset target and was preserved: {}",
                existing.display()
            )));
        }
    }
    Ok(())
}

fn paths_component_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn git_null_paths<'a>(
    path: &Path,
    args: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<PathBuf>> {
    let output = git_output(path, args)?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(WorkspaceError::Git(if message.is_empty() {
            format!("Git exited with {}", output.status)
        } else {
            message
        }));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(git_path_from_bytes)
        .collect())
}

#[cfg(unix)]
fn git_path_from_bytes(path: &[u8]) -> PathBuf {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt as _};
    PathBuf::from(OsString::from_vec(path.to_vec()))
}

#[cfg(not(unix))]
fn git_path_from_bytes(path: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(path).into_owned())
}

fn write_workspace_identity(path: &Path, workspace_id: &str) -> Result<()> {
    let marker = worktree_git_dir(path)?.join(WORKSPACE_ID_MARKER);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(marker)
        .map_err(io_error)?;
    file.write_all(format!("{workspace_id}\n").as_bytes())
        .map_err(io_error)?;
    file.sync_all().map_err(io_error)
}

/// What `verify_workspace_identity` checked a managed checkout against, so an
/// update made after checking it outside the registry lock can tell whether
/// the entry changed meanwhile.
fn managed_identity(workspace: &WorkspaceInfo, project: &ProjectInfo) -> (String, String, String) {
    (
        workspace.id.clone(),
        workspace.path.clone(),
        project.root.clone(),
    )
}

fn registry_changed() -> WorkspaceError {
    WorkspaceError::Conflict("the workspace changed while it was being checked; try again".into())
}

fn verify_workspace_identity(workspace: &WorkspaceInfo, project: &ProjectInfo) -> Result<()> {
    let path = Path::new(&workspace.path);
    if !path.is_dir() {
        return Err(WorkspaceError::Conflict(
            "workspace directory is missing; managed identity cannot be verified".into(),
        ));
    }
    let identity = project_identity(path).map_err(|error| {
        WorkspaceError::Conflict(format!(
            "managed workspace identity could not be verified: {error}"
        ))
    })?;
    if !identity.git || !same_path(&identity.root, Path::new(&project.root)) {
        return Err(WorkspaceError::Conflict(
            "workspace path belongs to a different Git project; refusing destructive operation"
                .into(),
        ));
    }
    let marker = worktree_git_dir(path)
        .map_err(|error| {
            WorkspaceError::Conflict(format!(
                "managed workspace identity could not be verified: {error}"
            ))
        })?
        .join(WORKSPACE_ID_MARKER);
    let recorded = match fs::read_to_string(marker) {
        Ok(recorded) => recorded,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(WorkspaceError::Conflict(
                "workspace path is not the Haarness-managed worktree recorded in the registry"
                    .into(),
            ));
        }
        Err(error) => return Err(io_error(error)),
    };
    if recorded != format!("{}\n", workspace.id) {
        return Err(WorkspaceError::Conflict(
            "workspace identity marker does not match the registry; refusing destructive operation"
                .into(),
        ));
    }
    Ok(())
}

fn worktree_git_dir(path: &Path) -> Result<PathBuf> {
    let git_dir = PathBuf::from(git_checked(path, ["rev-parse", "--git-dir"])?.trim());
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        path.join(git_dir)
    };
    git_dir.canonicalize().map_err(io_error)
}

/// Resolve the base exactly as `create_worktree` does. An omitted or blank
/// ref records the primary checkout's branch when attached; detached HEADs
/// are recorded as the exact commit so a later checkout cannot change reset
/// semantics.
fn resolve_base_ref(root: &Path, requested: Option<&str>) -> Result<WorkspaceResolveRefResponse> {
    let explicit = requested.map(str::trim).filter(|value| !value.is_empty());
    let mut reference = explicit
        .map(str::to_owned)
        .or_else(|| git_head_branch_ref(root))
        .unwrap_or_else(|| "HEAD".to_owned());
    let commit = verify_commit(root, &reference)?;
    if explicit.is_none() && reference == "HEAD" {
        reference.clone_from(&commit);
    }
    Ok(WorkspaceResolveRefResponse { reference, commit })
}

fn discover_refs(root: &Path, default_ref: &str) -> Result<Vec<WorkspaceRefInfo>> {
    // One process for the whole listing. The `*` fields carry the object an
    // annotated tag peels to, so a repository with hundreds of refs costs one
    // git call rather than one `rev-parse` per ref.
    let output = git_checked(
        root,
        [
            "for-each-ref",
            "--format=%(refname)%00%(symref)%00%(objecttype)%00%(objectname)%00%(*objecttype)%00%(*objectname)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ],
    )?;
    let mut refs = Vec::new();
    for row in output.lines() {
        let mut columns = row.split('\0');
        let mut column = || columns.next().unwrap_or_default();
        let reference = column();
        let symbolic_target = column();
        let object_type = column();
        let object = column();
        let peeled_type = column();
        let peeled = column();
        // In particular, omit refs/remotes/<remote>/HEAD aliases: the target
        // branch is already listed and is stable without a symbolic hop.
        if !symbolic_target.is_empty() {
            continue;
        }
        let (name, kind) = if let Some(name) = reference.strip_prefix("refs/heads/") {
            (name, WorkspaceRefKind::LocalBranch)
        } else if let Some(name) = reference.strip_prefix("refs/remotes/") {
            (name, WorkspaceRefKind::RemoteBranch)
        } else if let Some(name) = reference.strip_prefix("refs/tags/") {
            (name, WorkspaceRefKind::Tag)
        } else {
            continue;
        };
        // Tags may legally point at blobs or trees. Those are not valid
        // worktree bases, so omit them instead of making all discovery fail.
        let commit = match (object_type, peeled_type) {
            ("commit", _) => object,
            ("tag", "commit") => peeled,
            _ => continue,
        };
        if commit.is_empty() {
            continue;
        }
        refs.push(WorkspaceRefInfo {
            name: name.to_owned(),
            reference: reference.to_owned(),
            kind,
            commit: commit.to_owned(),
            current: kind == WorkspaceRefKind::LocalBranch && reference == default_ref,
        });
    }
    refs.sort_by(|left, right| {
        ref_kind_rank(left.kind)
            .cmp(&ref_kind_rank(right.kind))
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(refs)
}

fn ref_kind_rank(kind: WorkspaceRefKind) -> u8 {
    match kind {
        WorkspaceRefKind::LocalBranch => 0,
        WorkspaceRefKind::RemoteBranch => 1,
        WorkspaceRefKind::Tag => 2,
    }
}

fn verify_commit(root: &Path, reference: &str) -> Result<String> {
    if reference.starts_with('-') || reference.contains('\0') {
        return Err(WorkspaceError::Invalid("unsafe Git ref".into()));
    }
    commit_for_ref(root, reference)?.ok_or_else(|| {
        WorkspaceError::Invalid(format!("Git ref does not resolve to a commit: {reference}"))
    })
}

fn commit_for_ref(root: &Path, reference: &str) -> Result<Option<String>> {
    let expression = format!("{reference}^{{commit}}");
    let output = git_output(root, ["rev-parse", "--verify", &expression])?;
    Ok(output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned()))
}

fn validate_branch(root: &Path, branch: &str) -> Result<()> {
    if branch.trim().is_empty() || branch.starts_with('-') || branch.contains('\0') {
        return Err(WorkspaceError::Invalid("invalid Git branch name".into()));
    }
    git_checked(root, ["check-ref-format", "--branch", branch]).map(|_| ())
}

fn git_branch(path: &Path) -> Option<String> {
    git_head_branch_ref(path)
        .and_then(|reference| reference.strip_prefix("refs/heads/").map(str::to_owned))
}

fn git_head_branch_ref(path: &Path) -> Option<String> {
    git_optional(path, ["symbolic-ref", "--quiet", "HEAD"])
        .ok()
        .flatten()
        .map(|value| value.trim().to_owned())
}

fn git_optional<'a>(
    path: &Path,
    args: impl IntoIterator<Item = &'a str>,
) -> Result<Option<String>> {
    let output = match git_command(path, args).output() {
        Ok(output) => output,
        // Without Git installed no folder is a repository, so plain folders
        // still open (minimal Linux servers and containers often lack Git).
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(git_start_error(error)),
    };
    if output.status.success() {
        Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
    } else {
        Ok(None)
    }
}

fn git_success<'a>(path: &Path, args: impl IntoIterator<Item = &'a str>) -> Result<bool> {
    Ok(git_output(path, args)?.status.success())
}

fn git_checked<'a>(path: &Path, args: impl IntoIterator<Item = &'a str>) -> Result<String> {
    let output = git_output(path, args)?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(WorkspaceError::Git(if message.is_empty() {
        format!("Git exited with {}", output.status)
    } else {
        message
    }))
}

fn git_output<'a>(path: &Path, args: impl IntoIterator<Item = &'a str>) -> Result<Output> {
    git_command(path, args).output().map_err(git_start_error)
}

fn git_command<'a>(path: &Path, args: impl IntoIterator<Item = &'a str>) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(path).args(args);
    command
}

fn git_start_error(error: std::io::Error) -> WorkspaceError {
    WorkspaceError::Git(if error.kind() == std::io::ErrorKind::NotFound {
        "Git is not installed; install it to use worktrees and branches".to_owned()
    } else {
        error.to_string()
    })
}

fn canonical_directory(path: &Path) -> Result<PathBuf> {
    let canonical = path.canonicalize().map_err(io_error)?;
    if !canonical.is_dir() {
        return Err(WorkspaceError::Invalid(format!(
            "workspace is not a directory: {}",
            canonical.display()
        )));
    }
    Ok(canonical)
}

fn resolve_workspace<'a>(registry: &'a Registry, reference: &str) -> Result<&'a WorkspaceInfo> {
    Ok(&registry.workspaces[workspace_index(registry, reference)?])
}

fn resolve_workspace_mut<'a>(
    registry: &'a mut Registry,
    reference: &str,
) -> Result<&'a mut WorkspaceInfo> {
    let index = workspace_index(registry, reference)?;
    Ok(&mut registry.workspaces[index])
}

/// A workspace by ID, registered path or unique name. A reference that
/// matches none of those but is an existing path (through a symlink, or with
/// `..`) names the workspace registered at its canonical location, as
/// registration stores it.
fn workspace_index(registry: &Registry, reference: &str) -> Result<usize> {
    if let Some(index) = registry
        .workspaces
        .iter()
        .position(|workspace| workspace.id == reference || workspace.path == reference)
    {
        return Ok(index);
    }
    let matches = registry
        .workspaces
        .iter()
        .enumerate()
        .filter(|(_, workspace)| workspace.name == reference)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [index] => Ok(*index),
        [] => Path::new(reference)
            .canonicalize()
            .ok()
            .and_then(|canonical| {
                registry
                    .workspaces
                    .iter()
                    .position(|workspace| same_path(Path::new(&workspace.path), &canonical))
            })
            .ok_or_else(|| WorkspaceError::NotFound(format!("workspace {reference}"))),
        _ => Err(WorkspaceError::Conflict(format!(
            "workspace name is ambiguous: {reference}; use its ID"
        ))),
    }
}

/// How long a listing waits for its folders before it shows the ones that
/// have not answered as last recorded. A folder on a network volume that went
/// away, or behind a privacy prompt nobody is there to answer, can hold one
/// file-system call for minutes; the remote connector lists every few
/// seconds, and the TUI and Desktop project lists wait on it.
const PROBE_PATIENCE: Duration = Duration::from_secs(1);

/// What looking at a registered folder found.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Folder {
    exists: bool,
    branch: Option<String>,
}

impl Folder {
    fn probe(path: &Path) -> Self {
        let exists = path.is_dir();
        Self {
            exists,
            branch: exists.then(|| head_branch(path)).flatten(),
        }
    }

    fn apply(&self, workspace: &mut WorkspaceInfo) {
        workspace.exists = self.exists;
        if self.exists {
            workspace.branch.clone_from(&self.branch);
        }
    }
}

/// Folders being looked at (listed or registered) that have not answered yet,
/// and what a listing's probe found once it answered after the listing
/// stopped waiting. A folder that hangs holds one thread, never one per call,
/// and a slow one still shows what it last answered.
#[derive(Default)]
struct Probes {
    running: HashSet<PathBuf>,
    late: HashMap<PathBuf, Folder>,
}

static PROBES: LazyLock<Mutex<Probes>> = LazyLock::new(Mutex::default);

fn probes() -> MutexGuard<'static, Probes> {
    PROBES.lock().unwrap_or_else(PoisonError::into_inner)
}

fn refresh_existence(registry: &mut Registry) {
    refresh_folders(&mut registry.workspaces, Folder::probe, PROBE_PATIENCE);
}

fn refresh_workspace(workspace: &mut WorkspaceInfo) {
    refresh_folders(
        std::slice::from_mut(workspace),
        Folder::probe,
        PROBE_PATIENCE,
    );
}

/// Look at every folder at once, each on its own thread, and wait at most
/// `patience` for them. A folder that has not answered keeps its recorded
/// `exists` and `branch`, or what its last late probe found.
fn refresh_folders(
    workspaces: &mut [WorkspaceInfo],
    probe: fn(&Path) -> Folder,
    patience: Duration,
) {
    let paths: Vec<PathBuf> = workspaces
        .iter()
        .map(|workspace| PathBuf::from(&workspace.path))
        .collect();
    // The listing stopped waiting: keep the answer for the next one.
    let found = look_at_once(&paths, Arc::new(probe), patience, |path, folder| {
        probes().late.insert(path, folder);
    });
    let mut probes = probes();
    for ((workspace, path), folder) in workspaces.iter_mut().zip(&paths).zip(found) {
        if let Some(folder) = folder {
            probes.late.remove(path);
            folder.apply(workspace);
        } else if let Some(folder) = probes.late.get(path) {
            folder.apply(workspace);
        }
    }
}

/// Run `look` on every path at once, each on its own thread, and wait at most
/// `patience` for them all. A path whose earlier look has not returned is not
/// looked at again, so a folder that hangs holds one thread, never one per
/// call. A path's answer is `None` when it did not come in time or an earlier
/// look is still out; `late` gets each answer that came after the wait ended.
fn look_at_once<T: Send + 'static>(
    paths: &[PathBuf],
    look: Arc<dyn Fn(&Path) -> T + Send + Sync>,
    patience: Duration,
    late: fn(PathBuf, T),
) -> Vec<Option<T>> {
    let (sender, answers) = mpsc::channel();
    let mut asked = 0;
    for (index, path) in paths.iter().enumerate() {
        if !probes().running.insert(path.clone()) {
            continue;
        }
        let sender = sender.clone();
        let look = Arc::clone(&look);
        let spawned = std::thread::Builder::new()
            .name("haarness-folder-probe".into())
            .spawn({
                let path = path.clone();
                move || {
                    let answer = look(&path);
                    if let Err(mpsc::SendError((_, answer))) = sender.send((index, answer)) {
                        late(path.clone(), answer);
                    }
                    probes().running.remove(&path);
                }
            });
        if spawned.is_ok() {
            asked += 1;
        } else {
            probes().running.remove(path);
        }
    }
    drop(sender);
    let deadline = Instant::now() + patience;
    let mut found: Vec<Option<T>> = std::iter::repeat_with(|| None).take(paths.len()).collect();
    while asked > 0 {
        let Ok((index, answer)) =
            answers.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        else {
            break;
        };
        asked -= 1;
        found[index] = Some(answer);
    }
    found
}

/// What registering a directory found: `None` when it is not a directory.
type Registration = Option<Result<WorkspaceInfo>>;

/// What `WorkspaceManager::register_existing` did.
#[derive(Debug, Default)]
pub struct Registered {
    /// The directories registered in time.
    pub workspaces: Vec<WorkspaceInfo>,
    /// The directories that had not answered by then, or whose earlier look
    /// in this process is still out. Their look registers them when it
    /// returns, if the process still runs.
    pub pending: Vec<PathBuf>,
}

/// Register `directories` through `register`, all at once, waiting at most
/// `patience`; see `WorkspaceManager::register_existing`.
fn register_folders(
    directories: &[PathBuf],
    register: Arc<dyn Fn(&Path) -> Registration + Send + Sync>,
    patience: Duration,
) -> Result<Registered> {
    let mut registered = Registered::default();
    let found = look_at_once(directories, register, patience, |_, _| {});
    for (directory, answer) in directories.iter().zip(found) {
        match answer {
            None => registered.pending.push(directory.clone()),
            Some(None) => {}
            Some(Some(workspace)) => registered.workspaces.push(workspace?),
        }
    }
    Ok(registered)
}

/// The branch checked out in `path`, as `git symbolic-ref HEAD` names it, read
/// from the repository's HEAD file instead of starting git once per folder
/// and listing. A HEAD this cannot read plainly (another ref storage, an
/// unusual layout) is asked of git. For display only: removal asks git
/// itself (`git_branch`).
fn head_branch(path: &Path) -> Option<String> {
    match read_head(path) {
        Some(Head::Branch(name)) => Some(name),
        Some(Head::Other) => None,
        None => git_branch(path),
    }
}

enum Head {
    /// `ref: refs/heads/<name>`.
    Branch(String),
    /// Detached, another kind of ref, or no repository at all.
    Other,
}

/// The most read from a `.git` pointer or a HEAD file: either holds one line.
const SMALL_FILE: u64 = 4096;

/// The text of `path` when `metadata` (read before opening it) says it is a
/// regular file of at most `SMALL_FILE` bytes; `None` for a device, a FIFO,
/// a folder or a larger file, so a hostile `.git` or HEAD can neither block a
/// listing nor fill its memory.
fn small_file(metadata: &fs::Metadata, path: &Path) -> Option<String> {
    if !metadata.is_file() || metadata.len() > SMALL_FILE {
        return None;
    }
    let mut text = String::new();
    File::open(path)
        .ok()?
        .take(SMALL_FILE)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

/// `None` when only git can tell.
fn read_head(path: &Path) -> Option<Head> {
    let Some(marker) = path
        .ancestors()
        .map(|folder| folder.join(".git"))
        .find(|marker| fs::symlink_metadata(marker).is_ok())
    else {
        return Some(Head::Other);
    };
    let git_dir = if marker.is_dir() {
        marker
    } else {
        // A linked worktree or submodule: `gitdir: <path>`, relative to its
        // folder. Anything else (a device, a FIFO, a huge file) is no
        // repository this or git could read, and is not handed to git.
        let Some(pointer) = small_file(&fs::metadata(&marker).ok()?, &marker) else {
            return Some(Head::Other);
        };
        let target = pointer.strip_prefix("gitdir:")?.trim();
        marker.parent()?.join(target)
    };
    let head = git_dir.join("HEAD");
    let metadata = fs::symlink_metadata(&head).ok()?;
    // Older Git links HEAD to the branch's ref. The link is read, never
    // followed: git itself would read whatever it points at to its end.
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(&head).ok()?;
        return Some(
            match target
                .to_str()
                .and_then(|target| target.strip_prefix("refs/heads/"))
            {
                Some(name) if !name.is_empty() => Head::Branch(name.to_owned()),
                _ => Head::Other,
            },
        );
    }
    let Some(content) = small_file(&metadata, &head) else {
        return Some(Head::Other);
    };
    let content = content.trim_end();
    if let Some(reference) = content.strip_prefix("ref: ") {
        return match reference.strip_prefix("refs/heads/") {
            // The reftable format keeps HEAD in its tables and this placeholder in the file.
            Some(".invalid") => None,
            Some(name) if !name.is_empty() => Some(Head::Branch(name.to_owned())),
            _ => Some(Head::Other),
        };
    }
    let detached =
        matches!(content.len(), 40 | 64) && content.bytes().all(|byte| byte.is_ascii_hexdigit());
    detached.then_some(Head::Other)
}

fn unique_name(registry: &Registry, requested: &str) -> String {
    if !registry
        .workspaces
        .iter()
        .any(|workspace| workspace.name == requested)
    {
        return requested.to_owned();
    }
    for suffix in 2_u32.. {
        let candidate = format!("{requested}-{suffix}");
        if !registry
            .workspaces
            .iter()
            .any(|workspace| workspace.name == candidate)
        {
            return candidate;
        }
    }
    unreachable!()
}

fn path_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace")
        .to_owned()
}

fn slug(value: &str) -> String {
    let slug = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned();
    if slug.is_empty() {
        "workspace".into()
    } else {
        slug
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    left.canonicalize().unwrap_or_else(|_| left.to_path_buf())
        == right.canonicalize().unwrap_or_else(|_| right.to_path_buf())
}

fn io_error(error: impl std::fmt::Display) -> WorkspaceError {
    WorkspaceError::Io(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_metadata_keeps_missing_workspaces_and_get_refreshes_the_selected_branch() {
        let fixture = Fixture::new();
        assert_eq!(fixture.manager.registered_count().unwrap(), 1);
        assert!(
            fixture
                .manager
                .project_for_workspace(&fixture.local.id)
                .unwrap()
                .git
        );
        command(&fixture.repository, ["checkout", "-b", "updated"]);
        assert_eq!(
            fixture
                .manager
                .get(&fixture.local.id)
                .unwrap()
                .branch
                .as_deref(),
            Some("updated")
        );
        let plain = fixture.root.join("plain");
        fs::create_dir(&plain).unwrap();
        let other = fixture.manager.register_local(&plain).unwrap();
        assert!(
            !fixture
                .manager
                .project_for_workspace(&other.id)
                .unwrap()
                .git
        );
        fs::remove_dir(&plain).unwrap();
        assert!(!fixture.manager.get(&other.id).unwrap().exists);
        // Legacy migration counts registered history, not just existing folders.
        assert_eq!(fixture.manager.registered_count().unwrap(), 2);
        assert_eq!(
            fixture.manager.project_for_workspace(&other.id).unwrap().id,
            other.project_id
        );
        assert!(fixture.manager.project_for_workspace("unknown").is_err());
    }

    #[test]
    fn registered_rows_are_read_as_stored_without_looking_at_any_folder() {
        let fixture = Fixture::new();
        let plain = fixture.root.join("plain");
        fs::create_dir(&plain).unwrap();
        let other = fixture.manager.register_local(&plain).unwrap();
        fs::remove_dir(&plain).unwrap();
        command(&fixture.repository, ["checkout", "-b", "updated"]);
        let row = |rows: &[WorkspaceInfo], id: &str| {
            rows.iter()
                .find(|workspace| workspace.id == id)
                .cloned()
                .unwrap()
        };
        // The folder that went and the branch that changed stay as recorded.
        let stored = fixture.manager.registered().unwrap();
        assert!(row(&stored, &other.id).exists);
        assert_eq!(
            row(&stored, &fixture.local.id).branch.as_deref(),
            Some("main")
        );
        // Listing looks at every folder.
        let listed = fixture.manager.list(None).unwrap().workspaces;
        assert!(!row(&listed, &other.id).exists);
        assert_eq!(
            row(&listed, &fixture.local.id).branch.as_deref(),
            Some("updated")
        );
    }

    #[test]
    fn project_subdirectories_are_not_mistaken_for_linked_worktrees() {
        let fixture = Fixture::new();
        let subdirectory = fixture.repository.join("src");
        fs::create_dir(&subdirectory).unwrap();
        let local = fixture.manager.register_local(&subdirectory).unwrap();
        assert_eq!(local.project_id, fixture.local.project_id);
        assert_eq!(local.kind, WorkspaceKind::Local);
        let linked = fixture.create("parallel");
        assert_eq!(linked.project_id, local.project_id);
        assert_eq!(linked.kind, WorkspaceKind::GitWorktree);
    }

    #[test]
    fn registering_after_git_init_refreshes_project_identity_without_moving_session_storage() {
        let fixture = Fixture::new();
        let root = fixture.root.join("plain");
        let child = root.join("src");
        fs::create_dir_all(&child).unwrap();
        let before = fixture.manager.register_local(&child).unwrap();
        command(&root, ["init", "-b", "main"]);
        let main = fixture.manager.register_local(&root).unwrap();
        let after = fixture.manager.register_local(&child).unwrap();
        assert_eq!(before.id, after.id);
        assert_eq!(after.project_id, main.project_id);
        let catalog = fixture.manager.list(None).unwrap();
        assert!(
            catalog
                .projects
                .iter()
                .find(|item| item.id == after.project_id)
                .unwrap()
                .git
        );
    }

    #[test]
    fn listing_reads_the_branch_from_head_as_git_names_it() {
        let fixture = Fixture::new();
        let linked = fixture.create("parallel");
        let subdirectory = fixture.repository.join("src");
        fs::create_dir(&subdirectory).unwrap();
        let plain = fixture.root.join("plain");
        fs::create_dir(&plain).unwrap();
        let detached = fixture.root.join("detached");
        command(
            &fixture.repository,
            ["worktree", "add", "--detach", detached.to_str().unwrap()],
        );
        command(&fixture.repository, ["checkout", "-b", "feature/head"]);
        for path in [
            fixture.repository.as_path(),
            Path::new(&linked.path),
            &subdirectory,
            &plain,
            &detached,
        ] {
            assert!(read_head(path).is_some(), "{path:?} needed git");
            assert_eq!(head_branch(path), git_branch(path), "{path:?}");
        }
        assert_eq!(head_branch(&subdirectory).as_deref(), Some("feature/head"));
        assert_eq!(head_branch(&detached), None);
        // The reftable format's placeholder HEAD is left to git.
        let reftable = fixture.root.join("reftable");
        fs::create_dir_all(reftable.join(".git")).unwrap();
        fs::write(reftable.join(".git/HEAD"), "ref: refs/heads/.invalid\n").unwrap();
        assert!(read_head(&reftable).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_hostile_git_file_or_head_neither_blocks_a_listing_nor_fills_its_memory() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("haarness-hostile-head-{}", Uuid::now_v7()));
        let folder = |name: &str| {
            let folder = root.join(name);
            fs::create_dir_all(folder.join("inner")).unwrap();
            folder
        };
        let fifo = |path: &Path| {
            let made = Command::new("mkfifo").arg(path).status().unwrap();
            assert!(made.success());
        };
        let zero_marker = folder("zero-marker");
        symlink("/dev/zero", zero_marker.join(".git")).unwrap();
        let fifo_marker = folder("fifo-marker");
        fifo(&fifo_marker.join(".git"));
        let fifo_head = folder("fifo-head");
        fs::create_dir(fifo_head.join(".git")).unwrap();
        fifo(&fifo_head.join(".git/HEAD"));
        let zero_head = folder("zero-head");
        fs::create_dir(zero_head.join(".git")).unwrap();
        symlink("/dev/zero", zero_head.join(".git/HEAD")).unwrap();
        let huge_head = folder("huge-head");
        fs::create_dir(huge_head.join(".git")).unwrap();
        fs::write(
            huge_head.join(".git/HEAD"),
            format!("ref: refs/heads/{}", "x".repeat(8192)),
        )
        .unwrap();
        // A HEAD that older Git links to the branch's ref still names it.
        let linked_head = folder("linked-head");
        fs::create_dir(linked_head.join(".git")).unwrap();
        symlink("refs/heads/main", linked_head.join(".git/HEAD")).unwrap();
        let cases = [
            (zero_marker, None),
            (fifo_marker, None),
            (fifo_head, None),
            (zero_head, None),
            (huge_head, None),
            (linked_head, Some("main")),
        ];
        let (sender, answers) = mpsc::channel();
        let paths: Vec<_> = cases.iter().map(|(path, _)| path.join("inner")).collect();
        std::thread::spawn(move || {
            for path in paths {
                // Each is answered from the files alone, never handed to git.
                let read = read_head(&path).is_some();
                let _ = sender.send((path.clone(), read, head_branch(&path)));
            }
        });
        for (_, expected) in cases {
            let (path, read, branch) = answers
                .recv_timeout(Duration::from_secs(30))
                .expect("a hostile .git or HEAD blocked the read");
            assert!(read, "{path:?} was handed to git");
            assert_eq!(branch.as_deref(), expected, "{path:?}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    static SLOW_STARTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static SLOW_RELEASED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// A folder named `slow` hangs until the test releases its probe (as a
    /// network volume that went away does); any other folder is looked at.
    fn gated_probe(path: &Path) -> Folder {
        use std::sync::atomic::Ordering::SeqCst;
        if path.file_name().is_none_or(|name| name != "slow") {
            return Folder::probe(path);
        }
        let probe = SLOW_STARTED.fetch_add(1, SeqCst) + 1;
        while SLOW_RELEASED.load(SeqCst) < probe {
            std::thread::sleep(Duration::from_millis(5));
        }
        Folder {
            exists: true,
            branch: Some(format!("late-{probe}")),
        }
    }

    #[test]
    fn a_folder_that_does_not_answer_shows_as_recorded_and_is_probed_once() {
        use std::sync::atomic::Ordering::SeqCst;
        let root = std::env::temp_dir().join(format!("haarness-slow-folder-{}", Uuid::now_v7()));
        let fast = root.join("fast");
        fs::create_dir_all(&fast).unwrap();
        let slow = root.join("slow");
        let entry = |path: &Path, exists: bool, branch: &str| WorkspaceInfo {
            id: Uuid::now_v7().to_string(),
            project_id: "project".into(),
            name: path_name(path),
            path: path.display().to_string(),
            kind: WorkspaceKind::Local,
            managed: false,
            branch: Some(branch.into()),
            from_ref: None,
            base_commit: None,
            created_at: Utc::now(),
            last_opened_at: Utc::now(),
            exists,
        };
        let mut workspaces = vec![entry(&fast, false, "old"), entry(&slow, true, "recorded")];
        let patience = Duration::from_secs(2);

        // The hanging folder keeps what was recorded; the other is looked at.
        let started = Instant::now();
        refresh_folders(&mut workspaces, gated_probe, patience);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(workspaces[0].exists);
        assert_eq!(workspaces[0].branch, None);
        assert_eq!(workspaces[1].branch.as_deref(), Some("recorded"));
        assert_eq!(SLOW_STARTED.load(SeqCst), 1);

        // While its probe is out, the next listing does not start another.
        refresh_folders(&mut workspaces, gated_probe, patience);
        assert_eq!(SLOW_STARTED.load(SeqCst), 1);
        assert_eq!(workspaces[1].branch.as_deref(), Some("recorded"));

        // It answers late; a listing that again waits in vain shows that answer.
        SLOW_RELEASED.store(1, SeqCst);
        let deadline = Instant::now() + Duration::from_secs(30);
        while probes().running.contains(&slow) {
            assert!(Instant::now() < deadline, "the late probe never finished");
            std::thread::sleep(Duration::from_millis(10));
        }
        refresh_folders(&mut workspaces, gated_probe, patience);
        assert_eq!(SLOW_STARTED.load(SeqCst), 2);
        assert_eq!(workspaces[1].branch.as_deref(), Some("late-1"));
        SLOW_RELEASED.store(2, SeqCst);
        fs::remove_dir_all(root).unwrap();
    }

    static REGISTER_STARTED: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);
    static REGISTER_RELEASED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    #[test]
    fn registering_sidebar_folders_does_not_wait_on_one_that_hangs() {
        use std::sync::atomic::Ordering::SeqCst;
        let fixture = Fixture::new();
        let fast = fixture.root.join("fast");
        let slow = fixture.root.join("slow");
        fs::create_dir_all(&fast).unwrap();
        fs::create_dir_all(&slow).unwrap();
        let missing = fixture.root.join("missing");
        let manager = fixture.manager.clone();
        // `slow` hangs until the test lets it go, as a network volume that
        // went away does; then it registers as usual.
        let register: Arc<dyn Fn(&Path) -> Registration + Send + Sync> =
            Arc::new(move |path: &Path| {
                if path.file_name().is_some_and(|name| name == "slow") {
                    REGISTER_STARTED.fetch_add(1, SeqCst);
                    while !REGISTER_RELEASED.load(SeqCst) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
                path.is_dir().then(|| manager.register_local(path))
            });

        let registered = register_folders(
            &[fast.clone(), slow.clone(), missing],
            Arc::clone(&register),
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(registered.pending, std::slice::from_ref(&slow));
        let names: Vec<_> = registered
            .workspaces
            .iter()
            .map(|workspace| workspace.name.as_str())
            .collect();
        assert_eq!(names, ["fast"]);
        assert_eq!(REGISTER_STARTED.load(SeqCst), 1);

        // The next request does not look at it again while it hangs.
        let again = register_folders(
            std::slice::from_ref(&slow),
            Arc::clone(&register),
            Duration::from_millis(200),
        )
        .unwrap();
        assert!(again.workspaces.is_empty());
        assert_eq!(again.pending, std::slice::from_ref(&slow));
        assert_eq!(REGISTER_STARTED.load(SeqCst), 1);

        // Once it answers, its late registration counts.
        REGISTER_RELEASED.store(true, SeqCst);
        let deadline = Instant::now() + Duration::from_secs(30);
        while probes().running.contains(&slow) {
            assert!(
                Instant::now() < deadline,
                "the late registration never finished"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let slow = fs::canonicalize(&slow).unwrap();
        assert!(
            fixture
                .manager
                .registered()
                .unwrap()
                .iter()
                .any(|workspace| Path::new(&workspace.path) == slow)
        );
    }

    struct Fixture {
        root: PathBuf,
        repository: PathBuf,
        manager: WorkspaceManager,
        local: WorkspaceInfo,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("haarness-workspace-{}", Uuid::now_v7()));
            let repository = root.join("repository");
            fs::create_dir_all(&repository).unwrap();
            command(&repository, ["init", "-b", "main"]);
            command(
                &repository,
                ["config", "user.email", "test@example.invalid"],
            );
            command(&repository, ["config", "user.name", "Haarness Test"]);
            fs::write(repository.join("tracked.txt"), "base\n").unwrap();
            command(&repository, ["add", "tracked.txt"]);
            command(&repository, ["commit", "-m", "base"]);
            let manager = WorkspaceManager::new(
                root.join("data/workspaces.json"),
                root.join("managed"),
                root.join("runtime"),
            )
            .unwrap();
            let local = manager.register_local(&repository).unwrap();
            Self {
                root,
                repository,
                manager,
                local,
            }
        }

        fn create(&self, name: &str) -> WorkspaceInfo {
            self.manager
                .create_worktree(
                    &self.local.id,
                    &WorkspaceCreateRequest {
                        name: name.into(),
                        ..WorkspaceCreateRequest::default()
                    },
                )
                .unwrap()
                .workspace
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let workspaces = self.manager.list(None).unwrap().workspaces;
            for workspace in workspaces.into_iter().filter(|workspace| workspace.managed) {
                let _ = self.manager.remove_worktree(
                    &workspace.id,
                    &WorkspaceRemoveRequest {
                        confirm: true,
                        discard_changes: true,
                        delete_branch: false,
                    },
                );
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn command<'a>(directory: &Path, args: impl IntoIterator<Item = &'a str>) {
        let output = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn real_git_lifecycle_is_isolated_and_conflict_checked() {
        let fixture = Fixture::new();
        let workspace = fixture.create("feature one");
        let path = PathBuf::from(&workspace.path);
        assert_ne!(path, fixture.repository);
        assert_eq!(
            fs::read_to_string(path.join("tracked.txt")).unwrap(),
            "base\n"
        );

        fs::write(path.join("only-worktree.txt"), "isolated\n").unwrap();
        assert!(!fixture.repository.join("only-worktree.txt").exists());
        fixture
            .manager
            .reset_worktree(
                &workspace.id,
                &WorkspaceResetRequest {
                    confirm: true,
                    discard_changes: false,
                    remove_untracked: false,
                    to_ref: None,
                },
            )
            .unwrap();
        assert_eq!(
            fs::read_to_string(path.join("only-worktree.txt")).unwrap(),
            "isolated\n",
            "ordinary untracked files survive reset unless cleanup was requested"
        );
        fixture
            .manager
            .reset_worktree(
                &workspace.id,
                &WorkspaceResetRequest {
                    confirm: true,
                    discard_changes: true,
                    remove_untracked: true,
                    to_ref: None,
                },
            )
            .unwrap();
        assert!(!path.join("only-worktree.txt").exists());

        fs::write(path.join("tracked.txt"), "changed\n").unwrap();
        let reset = fixture.manager.reset_worktree(
            &workspace.id,
            &WorkspaceResetRequest {
                confirm: true,
                ..WorkspaceResetRequest::default()
            },
        );
        assert!(matches!(reset, Err(WorkspaceError::Conflict(_))));
        fixture
            .manager
            .reset_worktree(
                &workspace.id,
                &WorkspaceResetRequest {
                    confirm: true,
                    discard_changes: true,
                    ..WorkspaceResetRequest::default()
                },
            )
            .unwrap();
        assert_eq!(
            fs::read_to_string(path.join("tracked.txt")).unwrap(),
            "base\n"
        );

        let listed = fixture.manager.list(Some(&workspace.id)).unwrap();
        assert_eq!(listed.projects.len(), 1);
        assert_eq!(listed.workspaces.len(), 2);
        assert_eq!(
            listed.current_workspace_id.as_deref(),
            Some(workspace.id.as_str())
        );
        assert_eq!(
            fixture.manager.open_workspace(&workspace.name).unwrap().id,
            workspace.id
        );

        fs::write(path.join("dirty.txt"), "keep until explicit\n").unwrap();
        let removal = fixture.manager.remove_worktree(
            &workspace.id,
            &WorkspaceRemoveRequest {
                confirm: true,
                ..WorkspaceRemoveRequest::default()
            },
        );
        assert!(matches!(removal, Err(WorkspaceError::Conflict(_))));
        fixture
            .manager
            .remove_worktree(
                &workspace.id,
                &WorkspaceRemoveRequest {
                    confirm: true,
                    discard_changes: true,
                    delete_branch: false,
                },
            )
            .unwrap();
        assert!(!path.exists());
        assert!(fixture.repository.exists());
    }

    #[test]
    fn reset_preserves_and_rejects_untracked_or_ignored_target_collisions() {
        let fixture = Fixture::new();
        fs::write(fixture.repository.join("collision"), "target collision\n").unwrap();
        fs::write(
            fixture.repository.join("ignored-collision"),
            "target ignored collision\n",
        )
        .unwrap();
        command(
            &fixture.repository,
            ["add", "collision", "ignored-collision"],
        );
        command(&fixture.repository, ["commit", "-m", "add reset targets"]);
        let target = git_checked(&fixture.repository, ["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_owned();
        let workspace = fixture.create("collision safety");
        let path = PathBuf::from(&workspace.path);
        command(&path, ["rm", "collision", "ignored-collision"]);
        fs::write(path.join(".gitignore"), "ignored-collision\n").unwrap();
        command(&path, ["add", ".gitignore"]);
        command(&path, ["commit", "-m", "remove reset targets"]);
        let head_before = git_checked(&path, ["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_owned();
        fs::write(path.join("collision"), "keep untracked\n").unwrap();
        fs::write(path.join("ignored-collision"), "keep ignored\n").unwrap();
        fs::write(path.join("safe-untracked"), "keep safe\n").unwrap();
        assert!(git_success(&path, ["check-ignore", "ignored-collision"]).unwrap());

        let collision = fixture
            .manager
            .reset_worktree(
                &workspace.id,
                &WorkspaceResetRequest {
                    confirm: true,
                    ..WorkspaceResetRequest::default()
                },
            )
            .expect_err("an untracked target collision must block reset");
        assert!(
            matches!(&collision, WorkspaceError::Conflict(message)
                if message.contains("untracked") && message.contains("collision")),
            "{collision}"
        );
        assert_eq!(
            fs::read_to_string(path.join("collision")).unwrap(),
            "keep untracked\n"
        );
        assert_eq!(
            git_checked(&path, ["rev-parse", "HEAD"]).unwrap().trim(),
            head_before
        );

        fs::remove_file(path.join("collision")).unwrap();
        let ignored = fixture
            .manager
            .reset_worktree(
                &workspace.id,
                &WorkspaceResetRequest {
                    confirm: true,
                    ..WorkspaceResetRequest::default()
                },
            )
            .expect_err("an ignored target collision must block reset");
        assert!(
            matches!(&ignored, WorkspaceError::Conflict(message)
                if message.contains("ignored") && message.contains("ignored-collision")),
            "{ignored}"
        );
        assert_eq!(
            fs::read_to_string(path.join("ignored-collision")).unwrap(),
            "keep ignored\n"
        );

        fs::remove_file(path.join("ignored-collision")).unwrap();
        fixture
            .manager
            .reset_worktree(
                &workspace.id,
                &WorkspaceResetRequest {
                    confirm: true,
                    ..WorkspaceResetRequest::default()
                },
            )
            .unwrap();
        assert_eq!(
            git_checked(&path, ["rev-parse", "HEAD"]).unwrap().trim(),
            target
        );
        assert_eq!(
            fs::read_to_string(path.join("safe-untracked")).unwrap(),
            "keep safe\n",
            "a non-colliding untracked file remains allowed and preserved"
        );
        assert!(paths_component_overlap(
            Path::new("tree"),
            Path::new("tree/item")
        ));
        assert!(!paths_component_overlap(
            Path::new("tree"),
            Path::new("treehouse/item")
        ));
    }

    #[test]
    fn active_workspace_lease_blocks_reset_and_remove() {
        let fixture = Fixture::new();
        let workspace = fixture.create("busy");
        let lock_path = fixture.manager.activity_lock_path(&workspace.id);
        fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
        let lease = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)
            .unwrap();
        lease.try_lock_exclusive().unwrap();
        let result = fixture.manager.reset_worktree(
            &workspace.id,
            &WorkspaceResetRequest {
                confirm: true,
                ..WorkspaceResetRequest::default()
            },
        );
        assert!(matches!(result, Err(WorkspaceError::Conflict(_))));
    }

    #[test]
    fn reoccupied_managed_path_cannot_be_reset_or_removed() {
        let fixture = Fixture::new();
        let workspace = fixture.create("reoccupied");
        let path = PathBuf::from(&workspace.path);
        let path_text = path.to_string_lossy().into_owned();
        assert_eq!(
            fs::read_to_string(worktree_git_dir(&path).unwrap().join(WORKSPACE_ID_MARKER)).unwrap(),
            format!("{}\n", workspace.id)
        );

        command(
            &fixture.repository,
            ["worktree", "remove", "--force", path_text.as_str()],
        );
        command(
            &fixture.repository,
            [
                "worktree",
                "add",
                "--detach",
                path_text.as_str(),
                "refs/heads/main",
            ],
        );
        assert!(
            !worktree_git_dir(&path)
                .unwrap()
                .join(WORKSPACE_ID_MARKER)
                .exists(),
            "a newly occupying worktree must not inherit the removed one's identity"
        );
        let head_before = git_checked(&path, ["rev-parse", "HEAD"]).unwrap();
        let registry_before = fs::read(&fixture.manager.registry_path).unwrap();
        let registration = fixture.manager.register_local(&path);
        assert!(
            matches!(registration, Err(WorkspaceError::Conflict(_))),
            "a replacement checkout must not inherit a stale managed workspace ID"
        );
        assert_eq!(
            fs::read(&fixture.manager.registry_path).unwrap(),
            registry_before,
            "rejected registration must not change workspace/session namespace metadata"
        );

        for error in [
            fixture
                .manager
                .open_workspace(&workspace.id)
                .expect_err("open must reject a reoccupied path"),
            fixture
                .manager
                .reset_worktree(
                    &workspace.id,
                    &WorkspaceResetRequest {
                        confirm: true,
                        discard_changes: true,
                        ..WorkspaceResetRequest::default()
                    },
                )
                .expect_err("reset must reject a reoccupied path"),
            fixture
                .manager
                .remove_worktree(
                    &workspace.id,
                    &WorkspaceRemoveRequest {
                        confirm: true,
                        discard_changes: true,
                        delete_branch: true,
                    },
                )
                .expect_err("remove must reject a reoccupied path"),
        ] {
            assert!(matches!(error, WorkspaceError::Conflict(_)), "{error}");
        }
        assert!(path.is_dir());
        assert_eq!(
            git_checked(&path, ["rev-parse", "HEAD"]).unwrap(),
            head_before
        );
        assert_eq!(fixture.manager.get(&workspace.id).unwrap().id, workspace.id);
    }

    #[test]
    fn explicit_stale_cleanup_requires_a_missing_path_and_retains_the_branch() {
        let fixture = Fixture::new();
        let workspace = fixture.create("missing checkout");
        let branch = workspace.branch.clone().unwrap();
        let path = PathBuf::from(&workspace.path);
        let path_text = path.to_string_lossy().into_owned();
        let registry_before = fs::read(&fixture.manager.registry_path).unwrap();
        assert!(matches!(
            fixture
                .manager
                .cleanup_missing_workspace(&workspace.id, &WorkspaceCleanupRequest::default()),
            Err(WorkspaceError::Invalid(_))
        ));
        let live_cleanup = fixture
            .manager
            .cleanup_missing_workspace(&workspace.id, &WorkspaceCleanupRequest { confirm: true });
        assert!(matches!(live_cleanup, Err(WorkspaceError::Conflict(_))));
        assert!(path.is_dir());
        assert_eq!(
            fs::read(&fixture.manager.registry_path).unwrap(),
            registry_before,
            "cleanup against a live checkout must preserve its registry row"
        );
        command(
            &fixture.repository,
            ["worktree", "remove", "--force", path_text.as_str()],
        );
        assert!(!path.exists());

        let ordinary_remove = fixture.manager.remove_worktree(
            &workspace.id,
            &WorkspaceRemoveRequest {
                confirm: true,
                discard_changes: true,
                delete_branch: true,
            },
        );
        assert!(matches!(ordinary_remove, Err(WorkspaceError::Conflict(_))));
        assert_eq!(fixture.manager.get(&workspace.id).unwrap().id, workspace.id);
        let removed = fixture
            .manager
            .cleanup_missing_workspace(&workspace.id, &WorkspaceCleanupRequest { confirm: true })
            .unwrap();
        assert_eq!(removed.workspace.branch.as_deref(), Some(branch.as_str()));
        assert!(removed.message.contains("stale workspace record"));
        assert!(removed.message.contains("identity could not be verified"));
        assert!(
            git_success(
                &fixture.repository,
                [
                    "show-ref",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}")
                ]
            )
            .unwrap(),
            "a cached branch must never be deleted without worktree identity proof"
        );
        assert!(matches!(
            fixture.manager.get(&workspace.id),
            Err(WorkspaceError::NotFound(_))
        ));
    }

    #[test]
    fn implicit_base_ref_survives_primary_checkout_branch_changes() {
        let fixture = Fixture::new();
        let base = git_checked(&fixture.repository, ["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_owned();
        let workspace = fixture.create("stable base");
        assert_eq!(workspace.from_ref.as_deref(), Some("refs/heads/main"));
        let worktree = PathBuf::from(&workspace.path);

        fs::write(worktree.join("tracked.txt"), "workspace commit\n").unwrap();
        command(&worktree, ["add", "tracked.txt"]);
        command(&worktree, ["commit", "-m", "workspace commit"]);

        command(&fixture.repository, ["checkout", "-b", "unrelated"]);
        fs::write(
            fixture.repository.join("tracked.txt"),
            "unrelated primary commit\n",
        )
        .unwrap();
        command(&fixture.repository, ["add", "tracked.txt"]);
        command(
            &fixture.repository,
            ["commit", "-m", "unrelated primary commit"],
        );
        let unrelated = git_checked(&fixture.repository, ["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_owned();
        assert_ne!(base, unrelated);

        fixture
            .manager
            .reset_worktree(
                &workspace.id,
                &WorkspaceResetRequest {
                    confirm: true,
                    ..WorkspaceResetRequest::default()
                },
            )
            .unwrap();
        let reset = git_checked(&worktree, ["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_owned();
        assert_eq!(reset, base);
        assert_ne!(reset, unrelated);
    }

    #[test]
    fn implicit_default_uses_the_fully_qualified_branch_when_a_tag_has_the_same_name() {
        let fixture = Fixture::new();
        let tag_commit = git_checked(&fixture.repository, ["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_owned();
        command(&fixture.repository, ["tag", "main", &tag_commit]);
        fs::write(fixture.repository.join("tracked.txt"), "new branch head\n").unwrap();
        command(&fixture.repository, ["add", "tracked.txt"]);
        command(&fixture.repository, ["commit", "-m", "advance branch"]);
        let branch_commit = git_checked(&fixture.repository, ["rev-parse", "refs/heads/main"])
            .unwrap()
            .trim()
            .to_owned();
        assert_ne!(branch_commit, tag_commit);

        let resolved = fixture
            .manager
            .resolve_ref(&fixture.local.id, &WorkspaceResolveRefRequest::default())
            .unwrap();
        assert_eq!(resolved.reference, "refs/heads/main");
        assert_eq!(resolved.commit, branch_commit);
        assert_eq!(
            fixture
                .manager
                .resolve_ref(
                    &fixture.local.id,
                    &WorkspaceResolveRefRequest {
                        reference: Some("refs/tags/main".into()),
                    },
                )
                .unwrap()
                .commit,
            tag_commit
        );

        let workspace = fixture.create("qualified default");
        assert_eq!(workspace.from_ref.as_deref(), Some("refs/heads/main"));
        assert_eq!(
            workspace.base_commit.as_deref(),
            Some(branch_commit.as_str())
        );
        assert_eq!(
            git_checked(Path::new(&workspace.path), ["rev-parse", "HEAD"])
                .unwrap()
                .trim(),
            branch_commit
        );
    }

    #[test]
    fn ref_discovery_and_resolution_cover_branches_remotes_tags_and_shas() {
        let fixture = Fixture::new();
        let commit = git_checked(&fixture.repository, ["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_owned();
        command(&fixture.repository, ["branch", "feature/local"]);
        command(
            &fixture.repository,
            ["update-ref", "refs/remotes/origin/review", &commit],
        );
        command(
            &fixture.repository,
            ["tag", "-a", "v1-test", "-m", "test tag", &commit],
        );
        command(&fixture.repository, ["tag", "tree-only", "HEAD^{tree}"]);

        let discovered = fixture.manager.refs(&fixture.local.id).unwrap();
        assert_eq!(discovered.default_ref, "refs/heads/main");
        assert_eq!(discovered.default_commit, commit);
        let main = discovered
            .refs
            .iter()
            .find(|item| item.reference == "refs/heads/main")
            .expect("current local branch");
        assert_eq!(main.kind, WorkspaceRefKind::LocalBranch);
        assert!(main.current);
        assert!(discovered.refs.iter().any(|item| {
            item.reference == "refs/heads/feature/local"
                && item.kind == WorkspaceRefKind::LocalBranch
                && !item.current
        }));
        assert!(discovered.refs.iter().any(|item| {
            item.reference == "refs/remotes/origin/review"
                && item.kind == WorkspaceRefKind::RemoteBranch
        }));
        let tag = discovered
            .refs
            .iter()
            .find(|item| item.reference == "refs/tags/v1-test")
            .expect("annotated tag");
        assert_eq!(tag.kind, WorkspaceRefKind::Tag);
        assert_eq!(tag.commit, commit, "annotated tags peel to their commit");
        assert!(
            discovered
                .refs
                .iter()
                .all(|item| item.reference != "refs/tags/tree-only"),
            "a tag on a tree is not a worktree base"
        );
        assert!(discovered.refs.iter().all(|item| !item.commit.is_empty()));

        for reference in [None, Some(""), Some("   ")] {
            let resolved = fixture
                .manager
                .resolve_ref(
                    &fixture.local.id,
                    &WorkspaceResolveRefRequest {
                        reference: reference.map(str::to_owned),
                    },
                )
                .unwrap();
            assert_eq!(resolved.reference, "refs/heads/main");
            assert_eq!(resolved.commit, commit);
        }
        for reference in ["v1-test", commit.as_str()] {
            let resolved = fixture
                .manager
                .resolve_ref(
                    &fixture.local.id,
                    &WorkspaceResolveRefRequest {
                        reference: Some(reference.to_owned()),
                    },
                )
                .unwrap();
            assert_eq!(resolved.reference, reference);
            assert_eq!(resolved.commit, commit);
        }
        let invalid = fixture.manager.resolve_ref(
            &fixture.local.id,
            &WorkspaceResolveRefRequest {
                reference: Some("missing/ref".into()),
            },
        );
        assert!(matches!(invalid, Err(WorkspaceError::Invalid(_))));

        let created = fixture
            .manager
            .create_worktree(
                &fixture.local.id,
                &WorkspaceCreateRequest {
                    name: "blank default".into(),
                    from_ref: Some("  ".into()),
                    ..WorkspaceCreateRequest::default()
                },
            )
            .unwrap()
            .workspace;
        assert_eq!(created.from_ref.as_deref(), Some("refs/heads/main"));
        assert_eq!(created.base_commit.as_deref(), Some(commit.as_str()));
    }

    #[test]
    fn detached_default_is_the_exact_commit_for_resolve_discovery_and_create() {
        let fixture = Fixture::new();
        let commit = git_checked(&fixture.repository, ["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_owned();
        command(&fixture.repository, ["checkout", "--detach", &commit]);

        let resolved = fixture
            .manager
            .resolve_ref(&fixture.local.id, &WorkspaceResolveRefRequest::default())
            .unwrap();
        assert_eq!(resolved.reference, commit);
        assert_eq!(resolved.commit, commit);
        let discovered = fixture.manager.refs(&fixture.local.id).unwrap();
        assert_eq!(discovered.default_ref, commit);
        assert_eq!(discovered.default_commit, commit);
        assert!(discovered.refs.iter().all(|item| !item.current));

        let created = fixture.create("detached default");
        assert_eq!(created.from_ref.as_deref(), Some(commit.as_str()));
        assert_eq!(created.base_commit.as_deref(), Some(commit.as_str()));
    }

    #[test]
    fn unborn_repository_ref_operations_fail_without_registry_or_worktree_side_effects() {
        let root =
            std::env::temp_dir().join(format!("haarness-workspace-unborn-{}", Uuid::now_v7()));
        let repository = root.join("repository");
        fs::create_dir_all(&repository).unwrap();
        command(&repository, ["init", "-b", "main"]);
        let manager = WorkspaceManager::new(
            root.join("data/workspaces.json"),
            root.join("managed"),
            root.join("runtime"),
        )
        .unwrap();
        let local = manager.register_local(&repository).unwrap();
        let registry_before = fs::read(&manager.registry_path).unwrap();

        let assert_unborn = |result: Result<WorkspaceResolveRefResponse>| {
            let error = result.expect_err("an unborn repository has no valid base commit");
            assert!(
                matches!(&error, WorkspaceError::Invalid(message)
                    if message == "Git ref does not resolve to a commit: refs/heads/main"),
                "{error}"
            );
        };
        assert_unborn(manager.resolve_ref(&local.id, &WorkspaceResolveRefRequest::default()));
        let refs_error = manager
            .refs(&local.id)
            .expect_err("ref discovery must reject an unborn repository");
        assert!(
            matches!(&refs_error, WorkspaceError::Invalid(message)
                if message == "Git ref does not resolve to a commit: refs/heads/main"),
            "{refs_error}"
        );
        let create_error = manager
            .create_worktree(
                &local.id,
                &WorkspaceCreateRequest {
                    name: "must-not-exist".into(),
                    ..WorkspaceCreateRequest::default()
                },
            )
            .expect_err("create must reject an unborn repository");
        assert!(
            matches!(&create_error, WorkspaceError::Invalid(message)
                if message == "Git ref does not resolve to a commit: refs/heads/main"),
            "{create_error}"
        );

        assert_eq!(fs::read(&manager.registry_path).unwrap(), registry_before);
        let listed = manager.list(None).unwrap();
        assert_eq!(listed.workspaces, vec![local]);
        assert!(
            fs::read_dir(&manager.managed_root)
                .unwrap()
                .next()
                .is_none(),
            "failed create must not leave a managed checkout directory"
        );
        let worktrees = git_checked(&repository, ["worktree", "list", "--porcelain"]).unwrap();
        assert_eq!(
            worktrees
                .lines()
                .filter(|line| line.starts_with("worktree "))
                .count(),
            1
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn removal_reports_when_safe_branch_deletion_is_refused() {
        let fixture = Fixture::new();
        let workspace = fixture.create("unmerged branch");
        let worktree = PathBuf::from(&workspace.path);
        let branch = workspace.branch.clone().expect("managed branch");
        fs::write(worktree.join("unmerged.txt"), "keep branch history\n").unwrap();
        command(&worktree, ["add", "unmerged.txt"]);
        command(&worktree, ["commit", "-m", "unmerged workspace commit"]);

        let removed = fixture
            .manager
            .remove_worktree(
                &workspace.id,
                &WorkspaceRemoveRequest {
                    confirm: true,
                    discard_changes: false,
                    delete_branch: true,
                },
            )
            .unwrap();
        assert!(!worktree.exists());
        assert_eq!(removed.workspace.branch.as_deref(), Some(branch.as_str()));
        assert!(removed.message.contains("branch"));
        assert!(removed.message.contains("retained"));
        assert!(
            git_success(
                &fixture.repository,
                [
                    "show-ref",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}")
                ]
            )
            .unwrap()
        );
    }

    #[test]
    fn removal_never_deletes_a_stale_registered_branch() {
        let fixture = Fixture::new();
        let workspace = fixture.create("switch branch");
        let worktree = PathBuf::from(&workspace.path);
        let stale_branch = workspace.branch.clone().expect("created branch");
        let live_branch = "haarness/live-removal-target";
        command(&worktree, ["switch", "-c", live_branch]);
        fs::write(worktree.join("live.txt"), "unmerged live branch\n").unwrap();
        command(&worktree, ["add", "live.txt"]);
        command(&worktree, ["commit", "-m", "live branch commit"]);

        let removed = fixture
            .manager
            .remove_worktree(
                &workspace.id,
                &WorkspaceRemoveRequest {
                    confirm: true,
                    discard_changes: false,
                    delete_branch: true,
                },
            )
            .unwrap();
        assert_eq!(removed.workspace.branch.as_deref(), Some(live_branch));
        for branch in [&stale_branch, live_branch] {
            assert!(
                git_success(
                    &fixture.repository,
                    [
                        "show-ref",
                        "--verify",
                        "--quiet",
                        &format!("refs/heads/{branch}")
                    ]
                )
                .unwrap(),
                "removal must retain {branch}"
            );
        }
    }

    #[test]
    fn local_workspaces_are_idempotent_and_never_destructible() {
        let fixture = Fixture::new();
        let again = fixture.manager.register_local(&fixture.repository).unwrap();
        assert_eq!(fixture.local.id, again.id);
        let result = fixture.manager.remove_worktree(
            &fixture.local.id,
            &WorkspaceRemoveRequest {
                confirm: true,
                discard_changes: true,
                delete_branch: false,
            },
        );
        assert!(matches!(result, Err(WorkspaceError::Invalid(_))));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_or_dotted_path_opens_the_registered_workspace() {
        let fixture = Fixture::new();
        let link = fixture.root.join("linked-repository");
        std::os::unix::fs::symlink(&fixture.repository, &link).unwrap();
        let dotted = fixture.root.join("managed/../repository");
        for reference in [&link, &dotted] {
            let reference = reference.to_str().unwrap();
            let opened = fixture.manager.open_workspace(reference).unwrap();
            assert_eq!(opened.id, fixture.local.id, "{reference}");
            assert_eq!(fixture.manager.get(reference).unwrap().id, fixture.local.id);
            assert_eq!(
                fixture.manager.project_for_workspace(reference).unwrap().id,
                fixture.local.project_id
            );
        }
        // A path to nothing registered is still not found.
        let elsewhere = fixture.root.join("managed");
        assert!(matches!(
            fixture.manager.get(elsewhere.to_str().unwrap()),
            Err(WorkspaceError::NotFound(_))
        ));
    }

    #[test]
    fn relative_custom_paths_are_stable_against_the_project_root() {
        let fixture = Fixture::new();
        let workspace = fixture
            .manager
            .create_worktree(
                &fixture.local.id,
                &WorkspaceCreateRequest {
                    name: "relative".into(),
                    path: Some("../relative-worktree".into()),
                    ..WorkspaceCreateRequest::default()
                },
            )
            .unwrap()
            .workspace;
        assert_eq!(
            PathBuf::from(&workspace.path),
            fixture
                .root
                .canonicalize()
                .unwrap()
                .join("relative-worktree")
        );
    }
}
