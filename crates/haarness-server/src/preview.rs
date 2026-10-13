//! Immutable preview versions live outside the workspace and never touch its Git history.
use crate::{ServerState, files};
use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
};
use haarness_core::{CoreError, CoreResult, Tool, ToolContext, ToolDefinition, ToolOutput};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use utoipa::{IntoParams, OpenApi, ToSchema};

const LIMIT: usize = 5 * 1024 * 1024;
type Failure = (StatusCode, String);
fn bad(error: impl std::fmt::Display) -> Failure {
    (StatusCode::BAD_REQUEST, error.to_string())
}
fn failure(error: Failure) -> CoreError {
    CoreError::Tool(error.1)
}
fn directory(workspace: &Path) -> Result<PathBuf, Failure> {
    let root = std::env::var_os("HAARNESS_DATA_DIR")
        .map(|root| PathBuf::from(root).join("data"))
        // HAARNESS_HOME keeps everything under one folder.
        .or_else(|| {
            std::env::var_os("HAARNESS_HOME")
                .map(PathBuf::from)
                .filter(|root| root.is_absolute())
                .map(|root| root.join("data"))
        })
        .or_else(|| {
            directories::ProjectDirs::from("dev", "haarness", "haarness")
                .map(|p| p.data_local_dir().to_owned())
        })
        .ok_or_else(|| bad("Preview storage is unavailable"))?;
    Ok(root.join("preview").join(files::hash(
        workspace
            .canonicalize()
            .map_err(bad)?
            .to_string_lossy()
            .as_bytes(),
    )))
}
fn validate(hash: &str) -> Result<(), Failure> {
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad("Invalid preview version"));
    }
    Ok(())
}
fn record(workspace: &Path, data: &[u8]) -> Result<String, Failure> {
    if data.len() > LIMIT {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "Preview exceeds 5 MiB".into(),
        ));
    }
    let hash = files::hash(data);
    let dir = directory(workspace)?;
    fs::create_dir_all(&dir).map_err(bad)?;
    // Write a complete new blob before publishing it. Concurrent previews of identical bytes are safe.
    let temporary = dir.join(format!(".{}", uuid::Uuid::now_v7()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(bad)?;
        file.write_all(data).map_err(bad)?;
        file.sync_all().map_err(bad)?;
        fs::rename(&temporary, dir.join(&hash)).map_err(bad)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    // Bound storage; re-previewing identical content refreshes its retention age.
    let mut versions = fs::read_dir(&dir)
        .map_err(bad)?
        .filter_map(Result::ok)
        .filter(|e| validate(&e.file_name().to_string_lossy()).is_ok())
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect::<Vec<_>>();
    versions.sort_by_key(|entry| entry.0);
    for (_, path) in versions.iter().take(versions.len().saturating_sub(500)) {
        let _ = fs::remove_file(path);
    }
    Ok(hash)
}
fn load(workspace: &Path, hash: &str) -> Result<Vec<u8>, Failure> {
    validate(hash)?;
    let mut data = Vec::new();
    fs::File::open(directory(workspace)?.join(hash))
        .map_err(|_| {
            (
                StatusCode::NOT_FOUND,
                "This preview version is no longer available".into(),
            )
        })?
        .take(LIMIT as u64 + 1)
        .read_to_end(&mut data)
        .map_err(bad)?;
    if data.len() > LIMIT || files::hash(&data) != hash {
        return Err(bad("Preview version failed integrity verification"));
    }
    Ok(data)
}
fn kind(path: &str) -> Option<&'static str> {
    match Path::new(path)
        .extension()?
        .to_str()?
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => Some("html"),
        "svg" => Some("svg"),
        "md" | "markdown" | "mdx" | "mdown" | "mkd" | "mkdn" => Some("markdown"),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "bmp" | "ico" => Some("image"),
        _ => None,
    }
}
fn content(path: String, data: Vec<u8>) -> files::FileContent {
    use base64::Engine;
    let revision = files::hash(&data);
    let size = data.len() as u64;
    let (value, encoding) = match String::from_utf8(data.clone()) {
        Ok(text) if !text.contains('\0') => (text, "utf8"),
        _ => (
            base64::engine::general_purpose::STANDARD.encode(data),
            "base64",
        ),
    };
    let mime = match Path::new(&path)
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        _ => "text/plain",
    };
    files::FileContent {
        path,
        content: value,
        encoding: encoding.into(),
        mime: mime.into(),
        revision,
        size,
    }
}

pub struct PreviewTool(pub PathBuf);
#[async_trait]
impl Tool for PreviewTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "preview".into(),
            description: "Show or refresh a workspace HTML, Markdown, SVG or image file beside the conversation. Call after creating or updating a visual file. Saves the exact version for later comparison and restore. Read-only; files must be inside the workspace and at most 5 MiB. HTML runs sandboxed: inline scripts and CSS plus https://unpkg.com libraries are allowed, network requests and navigation are blocked.".into(),
            input_schema: json!({"type":"object","properties":{"filePath":{"type":"string"},"mode":{"type":"string","enum":["preview","code"]},"title":{"type":"string"}},"required":["filePath"],"additionalProperties":false}),
        }
    }
    fn side_effectful(&self) -> bool {
        false
    }
    fn parallel_safe(&self, _: &serde_json::Value) -> bool {
        true
    }
    async fn execute(
        &self,
        arguments: serde_json::Value,
        _: ToolContext,
    ) -> CoreResult<ToolOutput> {
        self.validate_arguments(&arguments)?;
        let root = self
            .0
            .canonicalize()
            .map_err(|e| CoreError::Tool(e.to_string()))?;
        tokio::task::spawn_blocking(move || {
            let requested = Path::new(arguments["filePath"].as_str().unwrap());
            let relative = if requested.is_absolute() { requested.strip_prefix(&root).map_err(|_| bad("Preview must be inside the workspace"))? } else { requested };
            let path = relative.to_string_lossy().replace('\\', "/");
            // The model may have named a file that never existed.
            let missing = |error: Failure| if error.0 == StatusCode::NOT_FOUND { bad(format!("File not found: {path}")) } else { error };
            let target = files::resolve(&root, &path, false).map_err(missing)?;
            let kind = kind(&path).ok_or_else(|| bad("Preview supports HTML, Markdown, SVG and images"))?;
            let data = files::bytes(&target).map_err(missing)?;
            if kind != "image" && !std::str::from_utf8(&data).is_ok_and(|text| !text.contains('\0')) { return Err(bad("Preview requires UTF-8 text")); }
            let version = record(&root, &data)?;
            Ok(ToolOutput {
                text: format!("Showing {path} in the file panel. HTML network access is limited to https://unpkg.com; embed data and use inline CSS/JavaScript."),
                metadata: json!({"preview":{"path":path,"kind":kind,"version":version,"size":data.len(),"mode":arguments["mode"].as_str().unwrap_or("preview"),"title":arguments["title"].as_str().unwrap_or(&path)}}),
            })
        }).await.map_err(|e| CoreError::Tool(e.to_string()))?.map_err(failure)
    }
}

#[derive(Deserialize, IntoParams, ToSchema)]
pub struct PreviewVersionQuery {
    pub path: String,
    pub hash: String,
}
#[derive(Deserialize, ToSchema)]
pub struct PreviewRestoreRequest {
    pub path: String,
    pub hash: String,
    pub revision: String,
}
#[derive(Serialize, ToSchema)]
pub struct PreviewRestoreResult {
    pub file: files::FileContent,
    pub backup: String,
}
#[derive(OpenApi)]
#[openapi(
    paths(preview_version, current_preview, restore_preview),
    components(schemas(
        PreviewVersionQuery,
        PreviewRestoreRequest,
        PreviewRestoreResult,
        PreviewCurrent
    ))
)]
pub struct PreviewApi;
pub fn routes() -> Router<ServerState> {
    Router::new()
        .route("/files/preview", get(preview_version))
        .route("/files/preview/current", get(current_preview))
        .route("/files/preview/restore", post(restore_preview))
}
#[derive(Serialize, ToSchema)]
pub struct PreviewCurrent {
    pub revision: Option<String>,
}
#[utoipa::path(get,path="/v1/files/preview/current",params(files::FileQuery),responses((status=200,body=PreviewCurrent)))]
pub async fn current_preview(
    State(state): State<ServerState>,
    Query(query): Query<files::FileQuery>,
) -> Result<Json<PreviewCurrent>, Failure> {
    tokio::task::spawn_blocking(move || {
        // A file that is gone, or whose folder is gone, has no revision.
        let revision = match files::resolve(&state.workspace, &query.path, true)
            .and_then(|path| files::bytes(&path))
        {
            Ok(data) => Some(files::hash(&data)),
            Err((StatusCode::NOT_FOUND, _)) => None,
            Err(error) => return Err(error),
        };
        Ok(Json(PreviewCurrent { revision }))
    })
    .await
    .map_err(bad)?
}
#[utoipa::path(get,path="/v1/files/preview",params(PreviewVersionQuery),responses((status=200,body=files::FileContent),(status=404,description="This preview version is no longer available (plain text)")))]
pub async fn preview_version(
    State(state): State<ServerState>,
    Query(query): Query<PreviewVersionQuery>,
) -> Result<Json<files::FileContent>, Failure> {
    tokio::task::spawn_blocking(move || {
        // Versions are workspace-scoped; the display path still follows the explorer contract.
        // A version is kept outside the workspace, so it still opens once its folder is gone.
        match files::resolve(&state.workspace, &query.path, true) {
            Ok(_) | Err((StatusCode::NOT_FOUND, _)) => {}
            Err(error) => return Err(error),
        }
        Ok(Json(content(
            query.path,
            load(&state.workspace, &query.hash)?,
        )))
    })
    .await
    .map_err(bad)?
}
#[utoipa::path(post,path="/v1/files/preview/restore",request_body=PreviewRestoreRequest,responses((status=200,body=PreviewRestoreResult)))]
pub async fn restore_preview(
    State(state): State<ServerState>,
    Json(input): Json<PreviewRestoreRequest>,
) -> Result<Json<PreviewRestoreResult>, Failure> {
    let root = state.workspace.clone();
    let path = input.path.clone();
    let hash = input.hash.clone();
    let revision = input.revision.clone();
    let (restored, backup) = tokio::task::spawn_blocking(move || {
        // A version whose folder was removed is put back into a new one,
        // as `current_preview` calls it deleted, not unavailable.
        let target = match files::resolve(&root, &path, true) {
            Ok(target) => Some(target),
            Err((StatusCode::NOT_FOUND, _)) => None,
            Err(error) => return Err(error),
        };
        if target
            .as_ref()
            .is_some_and(|target| fs::symlink_metadata(target).is_ok_and(|m| m.is_symlink()))
        {
            return Err(bad("Restoring symlinks is not supported"));
        }
        let data = match &target {
            Some(target) if target.exists() => files::bytes(target)?,
            _ => Vec::new(),
        };
        if files::hash(&data) != revision {
            return Err((
                StatusCode::CONFLICT,
                "File changed. Compare again before restoring.".into(),
            ));
        }
        let restored = content(path.clone(), load(&root, &hash)?);
        let backup = record(&root, &data)?;
        if target.is_none() {
            files::create_parents(&root, &path)?;
        }
        Ok((restored, backup))
    })
    .await
    .map_err(bad)??;
    // The existing editor's shared lock, revision check, symlink policy and atomic replace
    // recheck the current file after backup. A concurrent edit always aborts the restore.
    let file = files::write_file(
        State(state),
        Json(files::FileWrite {
            path: input.path,
            content: restored.content,
            encoding: Some(restored.encoding),
            revision: input.revision,
        }),
    )
    .await?;
    Ok(Json(PreviewRestoreResult {
        file: file.0,
        backup,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions_are_workspace_scoped_bounded_and_verified() {
        let root = std::env::temp_dir().join(format!("haarness-preview-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join("one")).unwrap();
        fs::create_dir_all(root.join("two")).unwrap();
        let one = root.join("one");
        let two = root.join("two");
        let hash = record(&one, b"<h1>first</h1>").unwrap();
        assert_eq!(load(&one, &hash).unwrap(), b"<h1>first</h1>");
        assert_eq!(record(&one, b"<h1>first</h1>").unwrap(), hash);
        assert!(load(&two, &hash).is_err());
        assert!(load(&one, "../secret").is_err());
        assert!(record(&one, &vec![0; LIMIT + 1]).is_err());
        fs::write(directory(&one).unwrap().join(&hash), "corrupt").unwrap();
        assert!(load(&one, &hash).is_err());
        fs::remove_dir_all(directory(&one).unwrap()).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
