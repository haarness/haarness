//! An authenticated host gateway over independently owned directory daemons.
//! Only the composition root can resolve an opaque workspace ID to an upstream.
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, Query, Request, State, WebSocketUpgrade, ws::Message},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use futures::{SinkExt, StreamExt};
use haarness_protocol::{WorkspaceInfo, WorkspaceListResponse};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_tungstenite::tungstenite::{Message as UpstreamMessage, client::IntoClientRequest};
use utoipa::{OpenApi, ToSchema};

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryListing {
    pub path: String,
    pub parent: Option<String>,
    pub directories: Vec<DirectoryEntry>,
    pub next_cursor: Option<String>,
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseOptions {
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub hidden: bool,
    pub limit: Option<usize>,
    pub after: Option<String>,
}
#[derive(Serialize, ToSchema)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
}
/// This value never crosses the host API boundary.
pub struct WorkspaceTarget {
    pub url: String,
    pub token: String,
}
#[async_trait::async_trait]
pub trait WorkspaceHost: Send + Sync {
    async fn catalog(&self) -> anyhow::Result<WorkspaceListResponse>;
    async fn browse(
        &self,
        path: Option<String>,
        options: BrowseOptions,
    ) -> anyhow::Result<DirectoryListing>;
    async fn open(&self, path: String) -> anyhow::Result<WorkspaceInfo>;
    async fn target(&self, id: String) -> anyhow::Result<WorkspaceTarget>;
}
#[derive(Clone)]
struct Gateway {
    host: Arc<dyn WorkspaceHost>,
    client: reqwest::Client,
    shutdown: tokio_util::sync::CancellationToken,
}
pub fn routes(
    host: Arc<dyn WorkspaceHost>,
    shutdown: tokio_util::sync::CancellationToken,
) -> Router {
    Router::new()
        .route("/v1/host/projects", get(catalog))
        .route("/v1/host/directories", get(browse))
        .route("/v1/host/open", post(open))
        .route("/v1/host/workspaces/{id}/{*path}", any(forward))
        .with_state(Gateway {
            host,
            shutdown,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("HTTP client"),
        })
}
fn failure(error: anyhow::Error) -> Response {
    (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": {"code":"remote_project", "message":error.to_string(), "retryable":false}}))).into_response()
}
#[utoipa::path(get, path="/v1/host/projects", responses((status=200, body=WorkspaceListResponse)))]
async fn catalog(State(gateway): State<Gateway>) -> Response {
    match gateway.host.catalog().await {
        Ok(value) => Json(value).into_response(),
        Err(error) => failure(error),
    }
}
#[derive(Deserialize, ToSchema)]
struct Browse {
    path: Option<String>,
    #[serde(default)]
    prefix: String,
    #[serde(default)]
    hidden: bool,
    limit: Option<usize>,
    after: Option<String>,
}
#[utoipa::path(get, path="/v1/host/directories", params(("path"=Option<String>,Query),("prefix"=Option<String>,Query),("hidden"=Option<bool>,Query),("limit"=Option<usize>,Query),("after"=Option<String>,Query)), responses((status=200, body=DirectoryListing)))]
async fn browse(State(gateway): State<Gateway>, Query(input): Query<Browse>) -> Response {
    match gateway
        .host
        .browse(
            input.path,
            BrowseOptions {
                prefix: input.prefix,
                hidden: input.hidden,
                limit: input.limit,
                after: input.after,
            },
        )
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => failure(error),
    }
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct Open {
    path: String,
}
#[utoipa::path(post, path="/v1/host/open", request_body=Open, responses((status=200, body=WorkspaceInfo)))]
async fn open(State(gateway): State<Gateway>, Json(input): Json<Open>) -> Response {
    match gateway.host.open(input.path).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => failure(error),
    }
}
async fn forward(
    State(gateway): State<Gateway>,
    Path((id, path)): Path<(String, String)>,
    ws: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
    request: Request,
) -> Response {
    // Route only the directory API, never another gateway or an arbitrary URL.
    if !path.starts_with("v1/")
        || path.starts_with("v1/host/")
        || path.split('/').any(|part| part == ".." || part == ".")
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let target = match gateway.host.target(id.clone()).await {
        Ok(value) => value,
        Err(error) => return failure(error),
    };
    let prefix = format!("/v1/host/workspaces/{id}/");
    let raw_path = request.uri().path().strip_prefix(&prefix).unwrap_or(&path);
    let mut url = format!("{}/{}", target.url.trim_end_matches('/'), raw_path);
    if let Some(query) = request.uri().query() {
        url.push('?');
        url.push_str(query);
    }
    if let Ok(ws) = ws {
        let upstream_url = url.replacen("http", "ws", 1);
        let mut handshake = match upstream_url.into_client_request() {
            Ok(value) => value,
            Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
        };
        let Ok(auth) = format!("Bearer {}", target.token).parse() else {
            return StatusCode::BAD_GATEWAY.into_response();
        };
        handshake.headers_mut().insert("authorization", auth);
        let upstream = match tokio_tungstenite::connect_async(handshake).await {
            Ok((socket, _)) => socket,
            Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
        };
        return ws.on_upgrade(move |socket| async move {
            let (mut client_sink, mut client_stream) = socket.split();
            let (mut upstream_sink, mut upstream_stream) = upstream.split();
            let outgoing = async {
                while let Some(Ok(message)) = client_stream.next().await {
                    let message = match message {
                        Message::Text(value) => UpstreamMessage::Text(value.to_string().into()),
                        Message::Binary(value) => UpstreamMessage::Binary(value),
                        Message::Ping(value) => UpstreamMessage::Ping(value),
                        Message::Pong(value) => UpstreamMessage::Pong(value),
                        Message::Close(_) => break,
                    };
                    if upstream_sink.send(message).await.is_err() { break; }
                }
                let _ = upstream_sink.close().await;
            };
            let incoming = async {
                while let Some(Ok(message)) = upstream_stream.next().await {
                    let message = match message {
                        UpstreamMessage::Text(value) => Message::Text(value.to_string().into()),
                        UpstreamMessage::Binary(value) => Message::Binary(value),
                        UpstreamMessage::Ping(value) => Message::Ping(value),
                        UpstreamMessage::Pong(value) => Message::Pong(value),
                        UpstreamMessage::Close(_) | UpstreamMessage::Frame(_) => break,
                    };
                    if client_sink.send(message).await.is_err() { break; }
                }
                let _ = client_sink.close().await;
            };
            tokio::select! { () = outgoing => {}, () = incoming => {}, () = gateway.shutdown.cancelled() => {} }
        }).into_response();
    }
    let (parts, body) = request.into_parts();
    let mut upstream = gateway
        .client
        .request(parts.method, url)
        .bearer_auth(target.token);
    for name in ["content-type", "accept", "last-event-id"] {
        if let Some(value) = parts.headers.get(name) {
            upstream = upstream.header(name, value);
        }
    }
    let body = match to_bytes(body, 64 * 1024 * 1024).await {
        Ok(value) => value,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    match upstream.body(body).send().await {
        Ok(response) => {
            let mut result = Response::builder().status(response.status());
            for name in [
                "content-type",
                "cache-control",
                "content-disposition",
                "x-accel-buffering",
            ] {
                if let Some(value) = response.headers().get(name) {
                    result = result.header(name, value);
                }
            }
            result
                .body(Body::from_stream(
                    response
                        .bytes_stream()
                        .take_until(gateway.shutdown.cancelled_owned()),
                ))
                .expect("upstream response")
        }
        Err(_) => StatusCode::BAD_GATEWAY.into_response(),
    }
}

#[derive(OpenApi)]
#[openapi(paths(catalog, browse, open))]
pub struct HostApi;
