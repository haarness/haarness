//! Catalog transport only: uploads never apply configuration to the server.
use crate::ServerState;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use haarness_profiles::{Catalog, MAX_ARCHIVE, Record};
use serde::Deserialize;
use utoipa::OpenApi;
use uuid::Uuid;

#[derive(OpenApi)]
#[openapi(
    paths(list, upload, metadata, download, remove),
    components(schemas(Record))
)]
pub(crate) struct ProfilesApi;

pub(crate) fn routes() -> Router<ServerState> {
    Router::new()
        .route("/profiles", get(list).post(upload))
        .route("/profiles/{id}", get(metadata).delete(remove))
        .route("/profiles/{id}/download", get(download))
        .layer(DefaultBodyLimit::max(MAX_ARCHIVE))
}

fn catalog(state: &ServerState) -> Result<Catalog, Failure> {
    state
        .workspaces
        .as_ref()
        .map(|manager| Catalog::new(manager.data_directory()))
        .ok_or(Failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "profile catalog is not configured".into(),
        ))
}

struct Failure(StatusCode, String);
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({"error":self.1}))).into_response()
    }
}

async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, Failure> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| {
            Failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "profile operation failed".into(),
            )
        })?
        .map_err(|error| {
            if error.chain().any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            }) {
                Failure(StatusCode::NOT_FOUND, "profile not found".into())
            } else {
                Failure(
                    StatusCode::BAD_REQUEST,
                    "profile operation refused; check archive, limits, and storage".into(),
                )
            }
        })
}

#[derive(Deserialize)]
pub(crate) struct Filters {
    #[serde(default)]
    query: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_limit() -> usize {
    100
}

#[utoipa::path(get, path = "/v1/profiles", tag = "profiles",
    params(("query" = Option<String>, Query), ("offset" = Option<usize>, Query), ("limit" = Option<usize>, Query)),
    responses((status = 200, body = [Record]), (status = 401, description = "Bearer token required")))]
async fn list(
    State(state): State<ServerState>,
    Query(filters): Query<Filters>,
) -> Result<Json<Vec<Record>>, Failure> {
    if !(1..=100).contains(&filters.limit) || filters.query.len() > 200 {
        return Err(Failure(
            StatusCode::BAD_REQUEST,
            "limit must be 1–100; query at most 200 bytes".into(),
        ));
    }
    let catalog = catalog(&state)?;
    blocking(move || catalog.list(&filters.query, filters.offset, filters.limit))
        .await
        .map(Json)
}

#[utoipa::path(post, path = "/v1/profiles", tag = "profiles",
    request_body(content = Vec<u8>, content_type = "application/zip"),
    responses((status = 201, body = Record), (status = 400, description = "Invalid archive"), (status = 413, description = "Archive exceeds 32 MiB")))]
async fn upload(
    State(state): State<ServerState>,
    bytes: Bytes,
) -> Result<(StatusCode, Json<Record>), Failure> {
    let catalog = catalog(&state)?;
    blocking(move || catalog.put(&bytes))
        .await
        .map(|record| (StatusCode::CREATED, Json(record)))
}

#[utoipa::path(get, path = "/v1/profiles/{id}", tag = "profiles", params(("id" = Uuid, Path)),
    responses((status = 200, body = Record), (status = 404, description = "Profile not found")))]
async fn metadata(
    State(state): State<ServerState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Record>, Failure> {
    let catalog = catalog(&state)?;
    blocking(move || catalog.get(id)).await.map(Json)
}

#[utoipa::path(get, path = "/v1/profiles/{id}/download", tag = "profiles", params(("id" = Uuid, Path)),
    responses((status = 200, body = Vec<u8>, content_type = "application/zip"), (status = 404, description = "Profile not found")))]
async fn download(
    State(state): State<ServerState>,
    Path(id): Path<Uuid>,
) -> Result<Response, Failure> {
    let catalog = catalog(&state)?;
    let bytes = blocking(move || catalog.download(id)).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        bytes,
    )
        .into_response())
}

#[utoipa::path(delete, path = "/v1/profiles/{id}", tag = "profiles", params(("id" = Uuid, Path)),
    responses((status = 204, description = "Deleted"), (status = 404, description = "Profile not found")))]
async fn remove(
    State(state): State<ServerState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, Failure> {
    let catalog = catalog(&state)?;
    blocking(move || catalog.remove(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}
