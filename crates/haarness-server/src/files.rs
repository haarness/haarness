//! Bounded workspace file access for graphical clients. Every path is resolved
//! beneath the server workspace, or beneath a chat's own folder when the
//! request names that chat (`threadId`); edits use revisions to detect
//! external changes.
use crate::ServerState;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Query, State},
    http::StatusCode,
    routing::{get, post},
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    fs,
    io::{Read as _, Write as _},
    path::{Component, Path, PathBuf},
};
use utoipa::{IntoParams, OpenApi, ToSchema};

const LIMIT: u64 = 10 * 1024 * 1024;
type Failure = (StatusCode, String);
static EDIT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct FileQuery {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub hidden: bool,
    /// See [`FileScope::thread_id`].
    #[serde(default)]
    pub thread_id: Option<haarness_protocol::ThreadId>,
}
/// Which folder a file request is about.
#[derive(Deserialize, IntoParams, Default)]
#[serde(rename_all = "camelCase")]
pub struct FileScope {
    /// The chat whose folder the request is about: for a chat with a
    /// folder of its own, paths are relative to that folder and confined to
    /// it (a link out of it, to another chat's folder too, is refused).
    /// Omitted, or for a chat of the project folder, the server's project
    /// folder.
    #[serde(default)]
    pub thread_id: Option<haarness_protocol::ThreadId>,
}
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub directory: bool,
    pub symlink: bool,
    pub size: u64,
}
#[derive(Serialize, ToSchema)]
pub struct FileListing {
    pub entries: Vec<FileEntry>,
    pub truncated: bool,
    /// The chat folder (canonical and absolute) the entries' paths are
    /// relative to, when the listing was asked for a chat with one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
}
#[derive(Serialize, ToSchema)]
pub struct FileContent {
    pub path: String,
    pub content: String,
    pub encoding: String,
    pub mime: String,
    pub revision: String,
    pub size: u64,
    /// The chat folder (canonical and absolute) `path` is relative to, when
    /// the request named a chat with one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
}
#[derive(Deserialize, ToSchema)]
pub struct FileWrite {
    pub path: String,
    pub content: String,
    pub revision: String,
    /// utf8 (default) or base64 for binary uploads.
    #[serde(default)]
    pub encoding: Option<String>,
}
#[derive(Deserialize, ToSchema)]
pub struct FileAction {
    pub action: String,
    pub path: String,
    pub target: Option<String>,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(OpenApi)]
#[openapi(
    paths(list_files, read_file, write_file, mutate_file),
    components(schemas(FileEntry, FileListing, FileContent, FileWrite, FileAction))
)]
pub struct FilesApi;
pub fn routes() -> Router<ServerState> {
    Router::new()
        .route("/files/list", get(list_files))
        .route("/files/content", get(read_file).put(write_file))
        .route("/files/action", post(mutate_file))
        .merge(crate::preview::routes())
        .layer(DefaultBodyLimit::max(15 * 1024 * 1024))
}
/// The folder a request's paths are relative to: the chat's own folder for
/// a chat with one (as reads see it: a deleted one is not made again, one a
/// link stands in for is refused), else the server's project folder. The
/// second value names the chat folder for the response.
pub(crate) async fn root_for(
    state: &ServerState,
    thread: Option<haarness_protocol::ThreadId>,
) -> Result<(PathBuf, Option<String>), Failure> {
    let Some(thread) = thread else {
        return Ok((state.workspace.clone(), None));
    };
    match state.runtime.chat_folder(thread).await {
        // Someone deleted it: these calls never make it again.
        Ok(Some(folder)) if !folder.is_dir() => Err((StatusCode::NOT_FOUND, FOLDER_GONE.into())),
        Ok(Some(folder)) => {
            let shown = folder.display().to_string();
            Ok((folder, Some(shown)))
        }
        Ok(None) => Ok((state.workspace.clone(), None)),
        Err(haarness_core::CoreError::NotFound(message)) => {
            Err((StatusCode::NOT_FOUND, format!("not found: {message}")))
        }
        Err(error) => Err(bad(error)),
    }
}
fn bad(error: impl std::fmt::Display) -> Failure {
    (StatusCode::BAD_REQUEST, error.to_string())
}
/// A path that is not there (any more) answers 404 with a plain sentence, not
/// the operating system's "No such file or directory (os error 2)". Every
/// other error keeps its 400 and its own text.
pub(crate) const FILE_GONE: &str = "This file no longer exists";
pub(crate) const FOLDER_GONE: &str = "This folder no longer exists";
fn io(missing: &'static str) -> impl Fn(std::io::Error) -> Failure {
    move |error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            (StatusCode::NOT_FOUND, missing.into())
        } else {
            bad(error)
        }
    }
}
/// The listed path is a folder, so say so when it is gone.
fn folder(failure: Failure) -> Failure {
    if failure.0 == StatusCode::NOT_FOUND {
        (StatusCode::NOT_FOUND, FOLDER_GONE.into())
    } else {
        failure
    }
}
/// Whether a path part names Git's metadata folder as a file system may read
/// it: in any letter case (`.GIT` is `.git` on macOS and Windows) and
/// without the trailing dots and spaces Windows drops. (Unicode
/// normalization cannot make another name `.git`: no other characters
/// compose or decompose to plain ASCII.)
fn is_git(part: &std::ffi::OsStr) -> bool {
    part.to_str().is_some_and(|name| {
        name.trim_end_matches(['.', ' '])
            .eq_ignore_ascii_case(".git")
    })
}
/// `value` as a path inside the workspace, refused when it is absolute,
/// climbs out, or names Git metadata.
fn relative(value: &str) -> Result<&Path, Failure> {
    let relative = Path::new(value);
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(bad("Path must be relative to the workspace"));
    }
    if relative.components().any(|part| is_git(part.as_os_str())) {
        return Err(bad("Git metadata is not editable in the file explorer"));
    }
    Ok(relative)
}
/// Whether `canonical`, a resolved path, is inside `root` and outside `.git`.
fn inside(root: &Path, canonical: &Path) -> Result<(), Failure> {
    if !canonical.starts_with(root) {
        return Err(bad("Path escapes the workspace"));
    }
    if canonical
        .strip_prefix(root)
        .map_err(bad)?
        .components()
        .any(|part| is_git(part.as_os_str()))
    {
        return Err(bad("Git metadata is not editable in the file explorer"));
    }
    Ok(())
}
/// Make the folders that hold `value` again, after they were removed, so a
/// file can be put back there. Only folders below the nearest one that still
/// exists are made, and that one must be inside the workspace and outside
/// `.git`, as `resolve` requires of any parent.
pub(crate) fn create_parents(root: &Path, value: &str) -> Result<(), Failure> {
    let relative = relative(value)?;
    let root = dunce::canonicalize(root).map_err(bad)?;
    let parent = root
        .join(relative)
        .parent()
        .ok_or_else(|| bad("Missing parent"))?
        .to_path_buf();
    let mut existing = parent.as_path();
    // A dangling link counts as there: making a folder through it is refused below.
    while fs::symlink_metadata(existing).is_err() {
        existing = existing.parent().ok_or_else(|| bad("Missing parent"))?;
    }
    inside(
        &root,
        &dunce::canonicalize(existing).map_err(io(FOLDER_GONE))?,
    )?;
    fs::create_dir_all(&parent).map_err(bad)
}
/// `resolve` for a read: the real path the check approved, so a link swapped
/// for one that leads out of the workspace after the check is not followed
/// by the read. Renames and deletes keep `resolve`, which names the link.
pub(crate) fn resolve_for_read(root: &Path, value: &str) -> Result<PathBuf, Failure> {
    let relative = relative(value)?;
    let root = dunce::canonicalize(root).map_err(bad)?;
    let canonical = dunce::canonicalize(root.join(relative)).map_err(io(FILE_GONE))?;
    inside(&root, &canonical)?;
    Ok(canonical)
}
pub(crate) fn resolve(root: &Path, value: &str, create: bool) -> Result<PathBuf, Failure> {
    let relative = relative(value)?;
    let root = dunce::canonicalize(root).map_err(bad)?;
    let path = root.join(relative);
    let (existing, missing) = if create && !path.exists() {
        (
            path.parent().ok_or_else(|| bad("Missing parent"))?,
            FOLDER_GONE,
        )
    } else {
        (path.as_path(), FILE_GONE)
    };
    let canonical = dunce::canonicalize(existing).map_err(io(missing))?;
    inside(&root, &canonical)?;
    Ok(path)
}
/// A symlink the explorer can open as a folder: its target is a directory
/// inside the workspace, outside `.git`, and not the listed folder or one of
/// its ancestors (which would loop). Dangling links stay files.
fn browsable_link(root: &Path, listed: &Path, link: &Path) -> bool {
    let Ok(target) = dunce::canonicalize(link) else {
        return false;
    };
    target.is_dir()
        && target.starts_with(root)
        && !listed.starts_with(&target)
        && !target
            .strip_prefix(root)
            .is_ok_and(|inside| inside.components().any(|part| is_git(part.as_os_str())))
}
pub(crate) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn bytes(path: &Path) -> Result<Vec<u8>, Failure> {
    if !path.metadata().map_err(io(FILE_GONE))?.is_file() {
        return Err(bad("Only regular files can be opened"));
    }
    let mut output = Vec::new();
    fs::File::open(path)
        .map_err(io(FILE_GONE))?
        .take(LIMIT + 1)
        .read_to_end(&mut output)
        .map_err(bad)?;
    if output.len() as u64 > LIMIT {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "File exceeds 10 MiB".into()));
    }
    Ok(output)
}

#[utoipa::path(get,path="/v1/files/list",params(FileQuery),responses((status=200,body=FileListing),(status=404,description="This folder no longer exists (plain text)")))]
pub async fn list_files(
    State(state): State<ServerState>,
    Query(query): Query<FileQuery>,
) -> Result<Json<FileListing>, Failure> {
    let (base, directory) = root_for(&state, query.thread_id).await?;
    tokio::task::spawn_blocking(move || {
        let path = resolve(&base, &query.path, false).map_err(folder)?;
        let listed = dunce::canonicalize(&path).map_err(io(FOLDER_GONE))?;
        let root = dunce::canonicalize(&base).map_err(bad)?;
        let mut entries = Vec::new();
        for entry in fs::read_dir(path).map_err(io(FOLDER_GONE))? {
            let entry = entry.map_err(bad)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_git(entry.file_name().as_os_str()) || (!query.hidden && name.starts_with('.')) {
                continue;
            }
            let meta = match fs::symlink_metadata(entry.path()) {
                Ok(meta) => meta,
                // Removed while the folder was being read: not listed.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(bad(error)),
            };
            entries.push(FileEntry {
                name,
                path: entry
                    .path()
                    .strip_prefix(&base)
                    .map_err(bad)?
                    .to_string_lossy()
                    .replace('\\', "/"),
                directory: meta.is_dir()
                    || meta.is_symlink() && browsable_link(&root, &listed, &entry.path()),
                symlink: meta.is_symlink(),
                size: meta.len(),
            });
        }
        entries.sort_by(|a, b| {
            b.directory
                .cmp(&a.directory)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(Json(FileListing {
            entries,
            truncated: false,
            directory,
        }))
    })
    .await
    .map_err(bad)?
}

#[utoipa::path(get,path="/v1/files/content",params(FileQuery),responses((status=200,body=FileContent),(status=404,description="This file no longer exists (plain text)")))]
pub async fn read_file(
    State(state): State<ServerState>,
    Query(query): Query<FileQuery>,
) -> Result<Json<FileContent>, Failure> {
    let (base, directory) = root_for(&state, query.thread_id).await?;
    tokio::task::spawn_blocking(move || {
        let real = resolve_for_read(&base, &query.path)?;
        let data = bytes(&real)?;
        let revision = hash(&data);
        let size = data.len() as u64;
        let mime = match Path::new(&query.path)
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "pdf" => "application/pdf",
            "mp3" => "audio/mpeg",
            "wav" => "audio/wav",
            "mp4" => "video/mp4",
            "webm" => "video/webm",
            _ => "application/octet-stream",
        }
        .to_string();
        let text = std::str::from_utf8(&data)
            .ok()
            .filter(|text| !text.contains('\0'));
        let (content, encoding, mime) = if let Some(text) = text {
            (text.to_string(), "utf8".into(), "text/plain".into())
        } else {
            (
                base64::engine::general_purpose::STANDARD.encode(data),
                "base64".into(),
                mime,
            )
        };
        Ok(Json(FileContent {
            path: query.path,
            content,
            encoding,
            mime,
            revision,
            size,
            directory,
        }))
    })
    .await
    .map_err(bad)?
}

fn decode_write(input: &FileWrite) -> Result<Vec<u8>, Failure> {
    let data = match input.encoding.as_deref().unwrap_or("utf8") {
        "utf8" => input.content.as_bytes().to_vec(),
        "base64" => base64::engine::general_purpose::STANDARD
            .decode(&input.content)
            .map_err(|_| bad("Invalid base64 file content"))?,
        _ => return Err(bad("Unknown file encoding")),
    };
    if data.len() as u64 > LIMIT {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "File exceeds 10 MiB".into()));
    }
    Ok(data)
}

#[utoipa::path(put,path="/v1/files/content",params(FileScope),request_body=FileWrite,responses((status=200,body=FileContent)))]
pub async fn write_file(
    State(state): State<ServerState>,
    Query(scope): Query<FileScope>,
    Json(input): Json<FileWrite>,
) -> Result<Json<FileContent>, Failure> {
    let (base, directory) = root_for(&state, scope.thread_id).await?;
    let events = state.runtime.events().clone();
    let changed = changed(&input.path, scope.thread_id, directory.as_deref());
    let result = save(base, input, directory).await?;
    events.publish("files.updated", None, None, None, changed);
    Ok(result)
}

/// Save the editor's copy of a file. It waits for an agent's file tool that
/// holds the file (through its formatter), so the save is not written over;
/// the revision check then sees what the tool left.
async fn save(
    base: PathBuf,
    input: FileWrite,
    directory: Option<String>,
) -> Result<Json<FileContent>, Failure> {
    let _file = hold(&base, [input.path.as_str()]).await?;
    let _guard = EDIT.lock().await;
    tokio::task::spawn_blocking(move || {
        let path = resolve(&base, &input.path, true)?;
        let data = decode_write(&input)?;
        let current = if path.exists() {
            bytes(&path)?
        } else {
            Vec::new()
        };
        if hash(&current) != input.revision {
            return Err((
                StatusCode::CONFLICT,
                "File changed outside the editor. Reload before saving.".into(),
            ));
        }
        if fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_symlink()) {
            return Err(bad("Editing symlinks is not supported"));
        }
        let (folder, name) = Bound::parent_of(&base, &path)?;
        let permissions = fs::metadata(&path).ok().map(|meta| meta.permissions());
        let (mut file, temporary) = folder.create_temporary()?;
        let result = (|| {
            if let Some(permissions) = permissions {
                file.set_permissions(permissions).map_err(bad)?;
            }
            file.write_all(&data).map_err(bad)?;
            file.sync_all().map_err(bad)?;
            folder.rename(&temporary, &folder, &name)
        })();
        if result.is_err() {
            folder.remove_quietly(&temporary);
        }
        result?;
        Ok(Json(FileContent {
            path: input.path,
            revision: hash(&data),
            size: data.len() as u64,
            content: input.content,
            encoding: input.encoding.unwrap_or_else(|| "utf8".into()),
            mime: "text/plain".into(),
            directory,
        }))
    })
    .await
    .map_err(bad)?
}

/// Wait for an agent's file tools that hold `paths` (through their
/// formatter). Taken before `EDIT`, so a change waiting for one file holds
/// up no change of another; tools never take `EDIT`.
async fn hold<'a>(
    base: &Path,
    paths: impl IntoIterator<Item = &'a str>,
) -> Result<crate::file_lock::FileLocks, Failure> {
    crate::file_lock::lock(
        paths
            .into_iter()
            .map(|path| haarness_core::resolve_permission_path(base, path)),
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .map_err(bad)
}

/// A `files.updated` payload: the changed path, and for a chat folder the
/// chat and folder it is relative to.
fn changed(
    path: &str,
    thread: Option<haarness_protocol::ThreadId>,
    directory: Option<&str>,
) -> serde_json::Value {
    match directory {
        Some(directory) => {
            serde_json::json!({"path": path, "threadId": thread, "directory": directory})
        }
        None => serde_json::json!({"path": path}),
    }
}

#[utoipa::path(post,path="/v1/files/action",params(FileScope),request_body=FileAction,responses((status=200,body=serde_json::Value)))]
pub async fn mutate_file(
    State(state): State<ServerState>,
    Query(scope): Query<FileScope>,
    Json(input): Json<FileAction>,
) -> Result<Json<serde_json::Value>, Failure> {
    let (base, directory) = root_for(&state, scope.thread_id).await?;
    let events = state.runtime.events().clone();
    let changed = changed(&input.path, scope.thread_id, directory.as_deref());
    let result = act(base, input).await?;
    events.publish("files.updated", None, None, None, changed);
    Ok(result)
}

/// Make, rename or delete an entry. A file renamed or deleted waits for an
/// agent's file tool that holds it, whose formatter would bring it back.
async fn act(base: PathBuf, input: FileAction) -> Result<Json<serde_json::Value>, Failure> {
    let held = match input.action.as_str() {
        "rename" | "delete" => [Some(input.path.as_str()), input.target.as_deref()],
        _ => [None, None],
    };
    let _files = hold(&base, held.into_iter().flatten()).await?;
    let _guard = EDIT.lock().await;
    tokio::task::spawn_blocking(move || {
        if input.path.is_empty() || input.path == "." {
            return Err(bad("Select a workspace entry"));
        }
        let path = resolve(&base, &input.path, input.action == "mkdir")?;
        if dunce::canonicalize(&path).ok() == dunce::canonicalize(&base).ok() {
            return Err(bad("Select a workspace entry"));
        }
        // Each change is made in the folder the checks approved, held open,
        // even if a link is put in place of that folder meanwhile.
        let (folder, name) = Bound::parent_of(&base, &path)?;
        match input.action.as_str() {
            "mkdir" => folder.mkdir(&name)?,
            "rename" => {
                let target = resolve(
                    &base,
                    input
                        .target
                        .as_deref()
                        .ok_or_else(|| bad("Target required"))?,
                    true,
                )?;
                let (into, target_name) = Bound::parent_of(&base, &target)?;
                if into.kind(&target_name)?.is_some() {
                    return Err((StatusCode::CONFLICT, "Target already exists".into()));
                }
                folder.rename(&name, &into, &target_name)?;
            }
            "delete" => {
                if !input.confirm {
                    return Err(bad("Deletion requires confirmation"));
                }
                let directory = folder.kind(&name)?.ok_or_else(|| bad(FILE_GONE))?;
                folder.remove(&name, directory)?;
            }
            _ => return Err(bad("Unknown file action")),
        }
        Ok(Json(serde_json::json!({"ok":true})))
    })
    .await
    .map_err(bad)?
}

/// A folder inside the scope, held once its real place is checked: files are
/// made, renamed and removed in that folder, not wherever its path leads
/// later, so a link put in place of the folder (or of one above it) between
/// the check and the change is not followed.
struct Bound {
    held: Held,
}

enum Held {
    /// The folder itself, open (and the real path it was opened by):
    /// changes are made relative to it.
    #[cfg(unix)]
    Open(std::os::fd::OwnedFd, PathBuf),
    /// Its real path, checked again right before each change: where the
    /// system has no folder handles (Windows), or none it lets this process
    /// open (a write-only drop-box folder on macOS).
    Checked(PathBuf),
}

impl Bound {
    /// The real folder holding `path` (a path `resolve` approved), and the
    /// name of `path` in it.
    fn parent_of(root: &Path, path: &Path) -> Result<(Self, std::ffi::OsString), Failure> {
        let name = path
            .file_name()
            .ok_or_else(|| bad("Select a workspace entry"))?
            .to_owned();
        let parent = path.parent().ok_or_else(|| bad("Missing parent"))?;
        Ok((Self::open(root, parent)?, name))
    }

    fn open(root: &Path, folder: &Path) -> Result<Self, Failure> {
        let root = dunce::canonicalize(root).map_err(bad)?;
        let real = dunce::canonicalize(folder).map_err(io(FOLDER_GONE))?;
        inside(&root, &real)?;
        #[cfg(unix)]
        {
            let opened = match open_folder(&real) {
                Ok(opened) => opened,
                // Searchable but not readable: hold its checked path.
                Err(nix::errno::Errno::EACCES) => {
                    return Ok(Self {
                        held: Held::Checked(real),
                    });
                }
                Err(error) => return Err(errno(error)),
            };
            let stat = nix::sys::stat::fstat(&opened).map_err(errno)?;
            if !is_folder(stat.st_mode) {
                return Err(bad("Not a folder"));
            }
            // Where the opened folder really is, not where its path leads now.
            inside(&root, &held_path(&opened, &stat, &real)?)?;
            Ok(Self {
                held: Held::Open(opened, real),
            })
        }
        #[cfg(not(unix))]
        Ok(Self {
            held: Held::Checked(real),
        })
    }

    /// The checked path, when it still leads to the folder that was checked.
    fn still(path: &Path) -> Result<&Path, Failure> {
        if dunce::canonicalize(path).ok().as_deref() == Some(path) {
            Ok(path)
        } else {
            Err(bad("Path escapes the workspace"))
        }
    }

    /// A new, empty file of its own name, for a write to replace a file with.
    fn create_temporary(&self) -> Result<(fs::File, std::ffi::OsString), Failure> {
        let name = std::ffi::OsString::from(format!(".haarness-edit-{}", uuid::Uuid::now_v7()));
        let file = match &self.held {
            #[cfg(unix)]
            Held::Open(folder, _) => {
                use nix::{fcntl::OFlag, sys::stat::Mode};
                fs::File::from(
                    nix::fcntl::openat(
                        folder,
                        name.as_os_str(),
                        OFlag::O_CREAT
                            | OFlag::O_EXCL
                            | OFlag::O_WRONLY
                            | OFlag::O_NOFOLLOW
                            | OFlag::O_CLOEXEC,
                        Mode::from_bits_truncate(0o666),
                    )
                    .map_err(errno)?,
                )
            }
            Held::Checked(path) => fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(Self::still(path)?.join(&name))
                .map_err(bad)?,
        };
        Ok((file, name))
    }

    /// Move `name` to `target` in `into` (this folder or another held one),
    /// replacing a file there.
    fn rename(
        &self,
        name: &std::ffi::OsStr,
        into: &Self,
        target: &std::ffi::OsStr,
    ) -> Result<(), Failure> {
        match (&self.held, &into.held) {
            #[cfg(unix)]
            (Held::Open(from, _), Held::Open(to, _)) => {
                nix::fcntl::renameat(from, name, to, target).map_err(errno)
            }
            _ => fs::rename(self.path_of(name)?, into.path_of(target)?).map_err(bad),
        }
    }

    /// `name` in this folder by path, for the changes that cannot be made
    /// relative to an open folder.
    fn path_of(&self, name: &std::ffi::OsStr) -> Result<PathBuf, Failure> {
        match &self.held {
            #[cfg(unix)]
            Held::Open(folder, real) => {
                let stat = nix::sys::stat::fstat(folder).map_err(errno)?;
                Ok(held_path(folder, &stat, real)?.join(name))
            }
            Held::Checked(path) => Ok(Self::still(path)?.join(name)),
        }
    }

    fn mkdir(&self, name: &std::ffi::OsStr) -> Result<(), Failure> {
        match &self.held {
            #[cfg(unix)]
            Held::Open(folder, _) => nix::sys::stat::mkdirat(
                folder,
                name,
                nix::sys::stat::Mode::from_bits_truncate(0o777),
            )
            .map_err(errno),
            Held::Checked(path) => fs::create_dir(Self::still(path)?.join(name)).map_err(bad),
        }
    }

    /// Whether `name` is there, and whether it is a folder (a link is not).
    fn kind(&self, name: &std::ffi::OsStr) -> Result<Option<bool>, Failure> {
        match &self.held {
            #[cfg(unix)]
            Held::Open(folder, _) => {
                match nix::sys::stat::fstatat(
                    folder,
                    name,
                    nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
                ) {
                    Ok(stat) => Ok(Some(is_folder(stat.st_mode))),
                    Err(nix::errno::Errno::ENOENT) => Ok(None),
                    Err(error) => Err(errno(error)),
                }
            }
            Held::Checked(path) => match fs::symlink_metadata(Self::still(path)?.join(name)) {
                Ok(meta) => Ok(Some(meta.is_dir())),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(bad(error)),
            },
        }
    }

    fn remove(&self, name: &std::ffi::OsStr, directory: bool) -> Result<(), Failure> {
        match &self.held {
            #[cfg(unix)]
            Held::Open(folder, _) => {
                use nix::unistd::UnlinkatFlags;
                let flag = if directory {
                    UnlinkatFlags::RemoveDir
                } else {
                    UnlinkatFlags::NoRemoveDir
                };
                nix::unistd::unlinkat(folder, name, flag).map_err(errno)
            }
            Held::Checked(path) => {
                let path = Self::still(path)?.join(name);
                if directory {
                    fs::remove_dir(path).map_err(bad)
                } else {
                    fs::remove_file(path).map_err(bad)
                }
            }
        }
    }

    fn remove_quietly(&self, name: &std::ffi::OsStr) {
        let _ = self.remove(name, false);
    }
}

/// A failed call, worded as the standard library's would be.
#[cfg(unix)]
fn errno(error: nix::errno::Errno) -> Failure {
    io(FILE_GONE)(std::io::Error::from_raw_os_error(error as i32))
}

#[cfg(unix)]
fn is_folder(mode: nix::libc::mode_t) -> bool {
    (mode & nix::sys::stat::SFlag::S_IFMT.bits()) == nix::sys::stat::SFlag::S_IFDIR.bits()
}

/// Open the folder `path` names to make changes in it. Linux opens it as a
/// place only (`O_PATH`), which a folder one may write in but not read
/// (`-wx`, a drop box) allows; elsewhere it is opened for reading.
#[cfg(unix)]
fn open_folder(path: &Path) -> nix::Result<std::os::fd::OwnedFd> {
    use nix::{fcntl::OFlag, sys::stat::Mode};
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let access = OFlag::O_PATH;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let access = OFlag::O_RDONLY;
    nix::fcntl::open(
        path,
        access | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
}

/// The path the system has for the folder `opened` holds, which a link put
/// in place of `checked` since does not change. Where the system cannot say
/// (Linux without `/proc`), `checked` itself, as long as it still names that
/// very folder (the same device and inode); otherwise the change is refused.
#[cfg(unix)]
fn held_path(
    opened: &std::os::fd::OwnedFd,
    stat: &nix::sys::stat::FileStat,
    checked: &Path,
) -> Result<PathBuf, Failure> {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        let mut path = PathBuf::new();
        if nix::fcntl::fcntl(opened, nix::fcntl::FcntlArg::F_GETPATH(&mut path)).is_ok() {
            return Ok(path);
        }
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use std::os::fd::AsRawFd as _;
        if let Ok(path) = fs::read_link(format!("/proc/self/fd/{}", opened.as_raw_fd())) {
            return Ok(path);
        }
    }
    let _ = opened;
    same_folder(stat, checked)
}

/// `checked`, when it still names the folder `stat` describes.
#[cfg(unix)]
fn same_folder(stat: &nix::sys::stat::FileStat, checked: &Path) -> Result<PathBuf, Failure> {
    let named = nix::sys::stat::stat(checked).map_err(errno)?;
    if (stat.st_dev, stat.st_ino) == (named.st_dev, named.st_ino) {
        Ok(checked.to_path_buf())
    } else {
        Err(bad("Path escapes the workspace"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn a_change_waiting_for_one_file_holds_up_no_change_of_another() {
        let root =
            std::env::temp_dir().join(format!("haarness-save-other-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let root = dunce::canonicalize(&root).unwrap();
        for name in ["held.txt", "other.txt", "gone.txt"] {
            fs::write(root.join(name), "before\n").unwrap();
        }
        let input = |path: &str| -> FileWrite {
            serde_json::from_value(serde_json::json!({
                "path": path, "content": "after\n", "revision": hash(b"before\n")
            }))
            .unwrap()
        };
        // An agent's edit holds one file while its formatter runs.
        let held = crate::file_lock::lock(
            [root.join("held.txt")],
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
        let waiting = tokio::spawn(save(root.clone(), input("held.txt"), None));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let delete: FileAction = serde_json::from_value(
            serde_json::json!({"action": "delete", "path": "gone.txt", "confirm": true}),
        )
        .unwrap();
        let deleted = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let saved = save(root.clone(), input("other.txt"), None).await.unwrap();
            assert_eq!(saved.revision, hash(b"after\n"));
            act(root.clone(), delete).await.unwrap().0
        })
        .await
        .expect("a change of another file waited");
        assert_eq!(deleted, serde_json::json!({"ok": true}));
        assert!(!waiting.is_finished());
        assert_eq!(
            fs::read_to_string(root.join("other.txt")).unwrap(),
            "after\n"
        );
        assert!(!root.join("gone.txt").exists());
        // A delete of the held file waits for it too.
        let delete: FileAction = serde_json::from_value(
            serde_json::json!({"action": "delete", "path": "held.txt", "confirm": true}),
        )
        .unwrap();
        let deleting = tokio::spawn(act(root.clone(), delete));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!deleting.is_finished(), "the delete did not wait");
        drop(held);
        // The save goes first, then the delete it queued behind.
        assert_eq!(waiting.await.unwrap().unwrap().revision, hash(b"after\n"));
        assert_eq!(
            deleting.await.unwrap().unwrap().0,
            serde_json::json!({"ok": true})
        );
        assert!(!root.join("held.txt").exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn a_save_waits_for_a_file_tool_that_holds_the_file() {
        let root =
            std::env::temp_dir().join(format!("haarness-save-lock-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let root = dunce::canonicalize(&root).unwrap();
        fs::write(root.join("notes.txt"), "agent\n").unwrap();
        // An agent's edit holds the file while its formatter runs.
        let held = crate::file_lock::lock(
            [root.join("notes.txt")],
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
        let input: FileWrite = serde_json::from_value(serde_json::json!({
            "path": "notes.txt", "content": "editor\n", "revision": hash(b"agent\n")
        }))
        .unwrap();
        let saving = tokio::spawn(save(root.clone(), input, None));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!saving.is_finished(), "the save did not wait");
        fs::write(root.join("notes.txt"), "formatted\n").unwrap();
        drop(held);
        // The save then sees the formatter's copy and asks for a reload.
        let refused = saving.await.unwrap().map(|_| ()).unwrap_err();
        assert_eq!(refused.0, StatusCode::CONFLICT);
        assert_eq!(
            fs::read_to_string(root.join("notes.txt")).unwrap(),
            "formatted\n"
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn uploads_decode_binary_without_breaking_legacy_text_or_size_limits() {
        let mut input: FileWrite = serde_json::from_value(
            serde_json::json!({"path":"a", "content":"hello", "revision":""}),
        )
        .unwrap();
        assert_eq!(decode_write(&input).unwrap(), b"hello");
        input.encoding = Some("base64".into());
        input.content = "AP8B".into();
        assert_eq!(decode_write(&input).unwrap(), [0, 255, 1]);
        input.content = "not-base64".into();
        assert!(decode_write(&input).is_err());
        input.encoding = Some("utf8".into());
        input.content = "x".repeat(usize::try_from(LIMIT).unwrap() + 1);
        assert_eq!(
            decode_write(&input).unwrap_err().0,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }
    #[test]
    fn missing_paths_answer_not_found_in_plain_words_and_other_errors_stay() {
        let root = std::env::temp_dir().join(format!("haarness-gone-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let gone = |status: StatusCode, message: &str| Err::<PathBuf, _>((status, message.into()));
        assert_eq!(
            resolve(&root, "deleted.txt", false),
            gone(StatusCode::NOT_FOUND, FILE_GONE)
        );
        assert_eq!(
            resolve(&root, "deleted/new.txt", true),
            gone(StatusCode::NOT_FOUND, FOLDER_GONE)
        );
        assert_eq!(
            resolve(&root, "deleted", false).map_err(folder),
            gone(StatusCode::NOT_FOUND, FOLDER_GONE)
        );
        // Removed between resolving and reading.
        fs::write(root.join("brief.txt"), "brief").unwrap();
        let brief = resolve(&root, "brief.txt", false).unwrap();
        fs::remove_file(&brief).unwrap();
        assert_eq!(
            bytes(&brief).unwrap_err(),
            (StatusCode::NOT_FOUND, FILE_GONE.into())
        );
        fs::create_dir(root.join("folder")).unwrap();
        assert_eq!(
            bytes(&root.join("folder")).unwrap_err(),
            bad("Only regular files can be opened")
        );
        assert_eq!(
            resolve(&root, "../secret", false).unwrap_err(),
            bad("Path must be relative to the workspace")
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn paths_reject_traversal_and_external_symlinks() {
        let root = std::env::temp_dir().join(format!("haarness-files-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        assert!(resolve(&root, "../secret", false).is_err());
        assert!(resolve(&root, ".git/config", true).is_err());
        fs::create_dir(root.join(".git")).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.parent().unwrap(), root.join("escape")).unwrap();
            assert!(resolve(&root, "escape", false).is_err());
            std::os::unix::fs::symlink(root.join(".git"), root.join("metadata")).unwrap();
            assert!(resolve(&root, "metadata", false).is_err());
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn git_metadata_is_refused_in_any_spelling_the_disk_reads_as_git() {
        let root = std::env::temp_dir().join(format!("haarness-git-case-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), "x").unwrap();
        for spelling in [
            ".git",
            ".GIT",
            ".Git/config",
            ".gIt",
            ".git.",
            ".git ",
            "src/.GIT/x",
        ] {
            assert_eq!(
                relative(spelling).unwrap_err(),
                bad("Git metadata is not editable in the file explorer"),
                "{spelling}"
            );
            assert!(resolve(&root, spelling, true).is_err(), "{spelling}");
        }
        for allowed in [".github", ".gitignore", "git", "a.git"] {
            assert!(relative(allowed).is_ok(), "{allowed}");
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn a_change_lands_in_the_checked_folder_even_if_a_link_takes_its_place() {
        let base =
            std::env::temp_dir().join(format!("haarness-write-race-{}", uuid::Uuid::now_v7()));
        let root = base.join("workspace");
        let outside = base.join("outside");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let root = dunce::canonicalize(&root).unwrap();
        let held = Bound::open(&root, &root.join("sub")).unwrap();
        // Between the check and the change, the folder is moved aside and a
        // link out of the workspace put in its place.
        fs::rename(root.join("sub"), root.join("sub-real")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("sub")).unwrap();
        let (mut file, temporary) = held.create_temporary().unwrap();
        file.write_all(b"written").unwrap();
        held.rename(&temporary, &held, std::ffi::OsStr::new("w.txt"))
            .unwrap();
        held.mkdir(std::ffi::OsStr::new("made")).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("sub-real/w.txt")).unwrap(),
            "written"
        );
        assert!(root.join("sub-real/made").is_dir());
        assert_eq!(
            fs::read_dir(&outside).unwrap().count(),
            0,
            "nothing outside"
        );
        assert_eq!(
            held.kind(std::ffi::OsStr::new("w.txt")).unwrap(),
            Some(false)
        );
        assert_eq!(held.kind(std::ffi::OsStr::new("made")).unwrap(), Some(true));
        held.remove(std::ffi::OsStr::new("w.txt"), false).unwrap();
        assert!(!root.join("sub-real/w.txt").exists());
        // The link itself is no folder of the workspace to hold.
        assert_eq!(
            Bound::open(&root, &root.join("sub")).err(),
            Some(bad("Path escapes the workspace"))
        );
        // A rename between two held folders stays in them too.
        let into = Bound::open(&root, &root.join("sub-real/made")).unwrap();
        let (_, temporary) = held.create_temporary().unwrap();
        held.rename(&temporary, &into, std::ffi::OsStr::new("moved"))
            .unwrap();
        assert!(root.join("sub-real/made/moved").is_file());
        fs::remove_dir_all(base).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn a_change_lands_in_a_folder_one_may_write_in_but_not_read() {
        use std::{ffi::OsStr, os::unix::fs::PermissionsExt as _};
        let base = std::env::temp_dir().join(format!("haarness-drop-box-{}", uuid::Uuid::now_v7()));
        let root = base.join("workspace");
        let outside = base.join("outside");
        fs::create_dir_all(root.join("drop")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let root = dunce::canonicalize(&root).unwrap();
        let drop_box = root.join("drop");
        let mode = |mode| fs::set_permissions(&drop_box, fs::Permissions::from_mode(mode));
        mode(0o300).unwrap();
        let changed = (|| {
            let held = Bound::open(&root, &drop_box)?;
            let (mut file, temporary) = held.create_temporary()?;
            file.write_all(b"dropped").map_err(bad)?;
            held.rename(&temporary, &held, OsStr::new("note.txt"))?;
            held.mkdir(OsStr::new("inner"))?;
            let kinds = (
                held.kind(OsStr::new("note.txt"))?,
                held.kind(OsStr::new("inner"))?,
            );
            held.mkdir(OsStr::new("gone"))?;
            held.remove(OsStr::new("gone"), true)?;
            // The folder moved aside and a link out put in its place: a
            // change lands in the folder or is refused, never outside.
            fs::rename(&drop_box, root.join("drop-real")).map_err(bad)?;
            std::os::unix::fs::symlink(&outside, &drop_box).map_err(bad)?;
            let late = held.mkdir(OsStr::new("late"));
            Ok::<_, Failure>((kinds, late))
        })();
        let real = root.join("drop-real");
        let _ = fs::set_permissions(&real, fs::Permissions::from_mode(0o755));
        let _ = mode(0o755);
        let (kinds, late) = changed.unwrap();
        assert_eq!(kinds, (Some(false), Some(true)));
        assert_eq!(
            fs::read_to_string(real.join("note.txt")).unwrap(),
            "dropped"
        );
        assert!(real.join("inner").is_dir());
        assert!(!real.join("gone").exists());
        assert!(late.is_err() || real.join("late").is_dir(), "{late:?}");
        assert_eq!(
            fs::read_dir(&outside).unwrap().count(),
            0,
            "nothing outside"
        );
        fs::remove_dir_all(base).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn without_a_path_for_the_open_folder_the_checked_path_must_still_name_it() {
        let base =
            std::env::temp_dir().join(format!("haarness-same-folder-{}", uuid::Uuid::now_v7()));
        let outside = base.join("outside");
        fs::create_dir_all(base.join("sub")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let base = dunce::canonicalize(&base).unwrap();
        let sub = base.join("sub");
        let opened = open_folder(&sub).unwrap();
        let stat = nix::sys::stat::fstat(&opened).unwrap();
        assert_eq!(same_folder(&stat, &sub), Ok(sub.clone()));
        fs::rename(&sub, base.join("sub-real")).unwrap();
        std::os::unix::fs::symlink(&outside, &sub).unwrap();
        assert_eq!(
            same_folder(&stat, &sub),
            Err(bad("Path escapes the workspace"))
        );
        assert!(same_folder(&stat, &base.join("gone")).is_err());
        assert_eq!(
            same_folder(&stat, &base.join("sub-real")),
            Ok(base.join("sub-real"))
        );
        fs::remove_dir_all(base).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn a_read_opens_the_file_the_check_approved_even_if_the_link_changes() {
        let base =
            std::env::temp_dir().join(format!("haarness-read-race-{}", uuid::Uuid::now_v7()));
        let root = base.join("workspace");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("inside.txt"), "INSIDE").unwrap();
        fs::write(base.join("outside.txt"), "OUTSIDE").unwrap();
        std::os::unix::fs::symlink("inside.txt", root.join("notes")).unwrap();
        let checked = resolve_for_read(&root, "notes").unwrap();
        let named = resolve(&root, "notes", false).unwrap();
        // The link now leads out of the workspace, after the check passed.
        fs::remove_file(root.join("notes")).unwrap();
        std::os::unix::fs::symlink(base.join("outside.txt"), root.join("notes")).unwrap();
        assert_eq!(bytes(&checked).unwrap(), b"INSIDE");
        // The link's own name would be followed to wherever it leads now.
        assert_eq!(bytes(&named).unwrap(), b"OUTSIDE");
        assert_eq!(
            resolve_for_read(&root, "notes").unwrap_err(),
            bad("Path escapes the workspace")
        );
        fs::remove_dir_all(base).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn linked_folders_inside_the_workspace_browse_as_folders() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("haarness-links-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let root = dunce::canonicalize(&root).unwrap();
        let nested = root.join("src/nested");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        symlink(&nested, root.join("shortcut")).unwrap();
        symlink(&root, nested.join("loop")).unwrap();
        symlink(root.parent().unwrap(), root.join("outside")).unwrap();
        symlink(root.join("missing"), root.join("dangling")).unwrap();
        symlink(root.join(".git"), root.join("metadata")).unwrap();
        let listed = root.clone();
        assert!(browsable_link(&root, &listed, &root.join("shortcut")));
        assert!(!browsable_link(&root, &listed, &root.join("outside")));
        assert!(!browsable_link(&root, &listed, &root.join("dangling")));
        assert!(!browsable_link(&root, &listed, &root.join("metadata")));
        // A link back to the folder being listed (or above it) stays a file.
        assert!(!browsable_link(&root, &nested, &nested.join("loop")));
        fs::remove_dir_all(root).unwrap();
    }
}
