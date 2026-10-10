//! Bounded workspace file access for graphical clients. Every path is resolved
//! beneath the server workspace; edits use revisions to detect external changes.
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
pub struct FileQuery {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub hidden: bool,
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
}
#[derive(Serialize, ToSchema)]
pub struct FileContent {
    pub path: String,
    pub content: String,
    pub encoding: String,
    pub mime: String,
    pub revision: String,
    pub size: u64,
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
fn bad(error: impl std::fmt::Display) -> Failure {
    (StatusCode::BAD_REQUEST, error.to_string())
}
pub(crate) fn resolve(root: &Path, value: &str, create: bool) -> Result<PathBuf, Failure> {
    let relative = Path::new(value);
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(bad("Path must be relative to the workspace"));
    }
    if relative.components().any(|part| part.as_os_str() == ".git") {
        return Err(bad("Git metadata is not editable in the file explorer"));
    }
    let root = root.canonicalize().map_err(bad)?;
    let path = root.join(relative);
    let existing = if create && !path.exists() {
        path.parent().ok_or_else(|| bad("Missing parent"))?
    } else {
        path.as_path()
    };
    let canonical = existing.canonicalize().map_err(bad)?;
    if !canonical.starts_with(&root) {
        return Err(bad("Path escapes the workspace"));
    }
    if canonical
        .strip_prefix(&root)
        .map_err(bad)?
        .components()
        .any(|part| part.as_os_str() == ".git")
    {
        return Err(bad("Git metadata is not editable in the file explorer"));
    }
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
            .is_ok_and(|inside| inside.components().any(|part| part.as_os_str() == ".git"))
}
pub(crate) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn bytes(path: &Path) -> Result<Vec<u8>, Failure> {
    if !path.metadata().map_err(bad)?.is_file() {
        return Err(bad("Only regular files can be opened"));
    }
    let mut output = Vec::new();
    fs::File::open(path)
        .map_err(bad)?
        .take(LIMIT + 1)
        .read_to_end(&mut output)
        .map_err(bad)?;
    if output.len() as u64 > LIMIT {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "File exceeds 10 MiB".into()));
    }
    Ok(output)
}

#[utoipa::path(get,path="/v1/files/list",params(FileQuery),responses((status=200,body=FileListing)))]
pub async fn list_files(
    State(state): State<ServerState>,
    Query(query): Query<FileQuery>,
) -> Result<Json<FileListing>, Failure> {
    tokio::task::spawn_blocking(move || {
        let path = resolve(&state.workspace, &query.path, false)?;
        let listed = path.canonicalize().map_err(bad)?;
        let root = state.workspace.canonicalize().map_err(bad)?;
        let mut entries = Vec::new();
        for entry in fs::read_dir(path).map_err(bad)? {
            let entry = entry.map_err(bad)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".git" || (!query.hidden && name.starts_with('.')) {
                continue;
            }
            let meta = fs::symlink_metadata(entry.path()).map_err(bad)?;
            entries.push(FileEntry {
                name,
                path: entry
                    .path()
                    .strip_prefix(&state.workspace)
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
        }))
    })
    .await
    .map_err(bad)?
}

#[utoipa::path(get,path="/v1/files/content",params(FileQuery),responses((status=200,body=FileContent)))]
pub async fn read_file(
    State(state): State<ServerState>,
    Query(query): Query<FileQuery>,
) -> Result<Json<FileContent>, Failure> {
    tokio::task::spawn_blocking(move || {
        let path = resolve(&state.workspace, &query.path, false)?;
        let data = bytes(&path)?;
        let revision = hash(&data);
        let size = data.len() as u64;
        let mime = match path
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

#[utoipa::path(put,path="/v1/files/content",request_body=FileWrite,responses((status=200,body=FileContent)))]
pub async fn write_file(
    State(state): State<ServerState>,
    Json(input): Json<FileWrite>,
) -> Result<Json<FileContent>, Failure> {
    let _guard = EDIT.lock().await;
    let events = state.runtime.events().clone();
    let changed_path = input.path.clone();
    let result = tokio::task::spawn_blocking(move || {
        let path = resolve(&state.workspace, &input.path, true)?;
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
        }))
    })
    .await
    .map_err(bad)??;
    events.publish(
        "files.updated",
        None,
        None,
        None,
        serde_json::json!({"path": changed_path}),
    );
    Ok(result)
}

#[utoipa::path(post,path="/v1/files/action",request_body=FileAction,responses((status=200,body=serde_json::Value)))]
pub async fn mutate_file(
    State(state): State<ServerState>,
    Json(input): Json<FileAction>,
) -> Result<Json<serde_json::Value>, Failure> {
    let _guard = EDIT.lock().await;
    let events = state.runtime.events().clone();
    let changed_path = input.path.clone();
    let result = tokio::task::spawn_blocking(move || {
        if input.path.is_empty() || input.path == "." {
            return Err(bad("Select a workspace entry"));
        }
        let path = resolve(&state.workspace, &input.path, input.action == "mkdir")?;
        if path.canonicalize().ok() == state.workspace.canonicalize().ok() {
            return Err(bad("Select a workspace entry"));
        }
        match input.action.as_str() {
            "mkdir" => fs::create_dir(path).map_err(bad)?,
            "rename" => {
                let target = resolve(
                    &state.workspace,
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
    events.publish(
        "files.updated",
        None,
        None,
        None,
        serde_json::json!({"path": changed_path}),
    );
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
