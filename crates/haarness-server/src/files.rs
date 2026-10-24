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
/// `value` as a path inside the workspace, refused when it is absolute,
/// climbs out, or names Git metadata.
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
    let root = root.canonicalize().map_err(bad)?;
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
    inside(&root, &existing.canonicalize().map_err(io(FOLDER_GONE))?)?;
    fs::create_dir_all(&parent).map_err(bad)
}
/// `resolve` for a read: the real path the check approved, so a link swapped
/// for one that leads out of the workspace after the check is not followed
/// by the read. Renames and deletes keep `resolve`, which names the link.
pub(crate) fn resolve_for_read(root: &Path, value: &str) -> Result<PathBuf, Failure> {
    let relative = relative(value)?;
    let root = root.canonicalize().map_err(bad)?;
    let canonical = root.join(relative).canonicalize().map_err(io(FILE_GONE))?;
    inside(&root, &canonical)?;
    Ok(canonical)
}
pub(crate) fn resolve(root: &Path, value: &str, create: bool) -> Result<PathBuf, Failure> {
    let relative = relative(value)?;
    let root = root.canonicalize().map_err(bad)?;
    let path = root.join(relative);
    let (existing, missing) = if create && !path.exists() {
        (
            path.parent().ok_or_else(|| bad("Missing parent"))?,
            FOLDER_GONE,
        )
    } else {
        (path.as_path(), FILE_GONE)
    };
    let canonical = existing.canonicalize().map_err(io(missing))?;
    inside(&root, &canonical)?;
    Ok(path)
}
/// A symlink the explorer can open as a folder: its target is a directory
/// inside the workspace, outside `.git`, and not the listed folder or one of
/// its ancestors (which would loop). Dangling links stay files.
fn browsable_link(root: &Path, listed: &Path, link: &Path) -> bool {
    let Ok(target) = link.canonicalize() else {
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
        let listed = path.canonicalize().map_err(io(FOLDER_GONE))?;
        let root = base.canonicalize().map_err(bad)?;
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
    let _guard = EDIT.lock().await;
    let events = state.runtime.events().clone();
    let changed = changed(&input.path, scope.thread_id, directory.as_deref());
    let result = tokio::task::spawn_blocking(move || {
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
        let temporary = path.with_file_name(format!(".haarness-edit-{}", uuid::Uuid::now_v7()));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(bad)?;
            if let Ok(meta) = fs::metadata(&path) {
                file.set_permissions(meta.permissions()).map_err(bad)?;
            }
            file.write_all(&data).map_err(bad)?;
            file.sync_all().map_err(bad)?;
            fs::rename(&temporary, &path).map_err(bad)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
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
    .map_err(bad)??;
    events.publish("files.updated", None, None, None, changed);
    Ok(result)
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
    let _guard = EDIT.lock().await;
    let events = state.runtime.events().clone();
    let changed = changed(&input.path, scope.thread_id, directory.as_deref());
    let result = tokio::task::spawn_blocking(move || {
        if input.path.is_empty() || input.path == "." {
            return Err(bad("Select a workspace entry"));
        }
        let path = resolve(&base, &input.path, input.action == "mkdir")?;
        if path.canonicalize().ok() == base.canonicalize().ok() {
            return Err(bad("Select a workspace entry"));
        }
        match input.action.as_str() {
            "mkdir" => fs::create_dir(path).map_err(bad)?,
            "rename" => {
                let target = resolve(
                    &base,
                    input
                        .target
                        .as_deref()
                        .ok_or_else(|| bad("Target required"))?,
                    true,
                )?;
                if target.exists() {
                    return Err((StatusCode::CONFLICT, "Target already exists".into()));
                }
                fs::rename(path, target).map_err(bad)?;
            }
            "delete" => {
                if !input.confirm {
                    return Err(bad("Deletion requires confirmation"));
                }
                if fs::symlink_metadata(&path).map_err(bad)?.is_dir() {
                    fs::remove_dir(path).map_err(bad)?;
                } else {
                    fs::remove_file(path).map_err(bad)?;
                }
            }
            _ => return Err(bad("Unknown file action")),
        }
        Ok(Json(serde_json::json!({"ok":true})))
    })
    .await
    .map_err(bad)??;
    events.publish("files.updated", None, None, None, changed);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        input.content = "x".repeat(LIMIT as usize + 1);
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
        let root = root.canonicalize().unwrap();
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
