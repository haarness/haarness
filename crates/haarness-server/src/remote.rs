//! An authenticated host gateway over independently owned directory daemons.
//! Only the composition root can resolve an opaque workspace ID to an upstream.
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, Query, Request, State, WebSocketUpgrade, ws::Message},
    http::{Method, StatusCode},
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
/// A project whose server another Haarness build started cannot be replaced
/// while that server is still working. Hosts return this (as the error or
/// its source) so the gateway can say so in plain words, without the pid and
/// shell command a person at the computer gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectBusy;
impl ProjectBusy {
    pub const CODE: &'static str = "project_busy";
    pub const MESSAGE: &'static str = "This project is busy on a different version of Haarness. Open it on the computer, or wait until its work finishes.";
}
impl std::fmt::Display for ProjectBusy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(Self::MESSAGE)
    }
}
impl std::error::Error for ProjectBusy {}
#[async_trait::async_trait]
pub trait WorkspaceHost: Send + Sync {
    async fn catalog(&self) -> anyhow::Result<WorkspaceListResponse>;
    async fn browse(
        &self,
        path: Option<String>,
        options: BrowseOptions,
    ) -> anyhow::Result<DirectoryListing>;
    async fn open(&self, path: String) -> anyhow::Result<WorkspaceInfo>;
    async fn close(&self, _id: String) -> anyhow::Result<()> {
        anyhow::bail!("Closing advertised projects is not supported by this host")
    }
    async fn target(&self, id: String) -> anyhow::Result<WorkspaceTarget>;
    /// Where a read (a plain GET or HEAD, never a WebSocket) may go. A host may
    /// let reads reach a server it cannot take over, such as one another
    /// Haarness build runs while it is busy ([`ProjectBusy`]), so the project
    /// can still be viewed; everything else still goes through `target`.
    async fn read_target(&self, id: String) -> anyhow::Result<WorkspaceTarget> {
        self.target(id).await
    }
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
        .route(
            "/v1/host/projects/{id}",
            axum::routing::delete(close_project),
        )
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
    if error.downcast_ref::<ProjectBusy>().is_some() {
        return (StatusCode::CONFLICT, Json(serde_json::json!({"error": {"code":ProjectBusy::CODE, "message":ProjectBusy::MESSAGE, "retryable":true}}))).into_response();
    }
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
#[utoipa::path(delete,path="/v1/host/projects/{id}",operation_id="close_remote_project",params(("id"=String,Path)),responses((status=204)))]
async fn close_project(State(gateway): State<Gateway>, Path(id): Path<String>) -> Response {
    match gateway.host.close(id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
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
    let read = ws.is_err() && matches!(*request.method(), Method::GET | Method::HEAD);
    let target = if read {
        gateway.host.read_target(id.clone()).await
    } else {
        gateway.host.target(id.clone()).await
    };
    let target = match target {
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
    for name in [
        "content-type",
        "accept",
        "last-event-id",
        "x-haarness-remote-thread",
    ] {
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
#[openapi(paths(catalog, browse, open, close_project))]
pub struct HostApi;

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt as _;

    /// A host whose project runs on another Haarness build that is still working.
    struct Busy {
        upstream: String,
        closed: std::sync::Mutex<Vec<String>>,
    }
    #[async_trait::async_trait]
    impl WorkspaceHost for Busy {
        async fn catalog(&self) -> anyhow::Result<WorkspaceListResponse> {
            anyhow::bail!("unused")
        }
        async fn browse(
            &self,
            _: Option<String>,
            _: BrowseOptions,
        ) -> anyhow::Result<DirectoryListing> {
            anyhow::bail!("Folder is unavailable")
        }
        async fn open(&self, _: String) -> anyhow::Result<WorkspaceInfo> {
            anyhow::bail!("unused")
        }
        async fn close(&self, id: String) -> anyhow::Result<()> {
            self.closed.lock().unwrap().push(id);
            Ok(())
        }
        async fn target(&self, _: String) -> anyhow::Result<WorkspaceTarget> {
            Err(anyhow::Error::new(ProjectBusy).context(
                "Haarness server 4242 needs an upgrade (server executable differs from this client build) but still owns 1 active turn(s); wait for them to finish or stop it explicitly with `haarness --workspace '/private/project' serve --stop`",
            ))
        }
        async fn read_target(&self, _: String) -> anyhow::Result<WorkspaceTarget> {
            Ok(WorkspaceTarget {
                url: self.upstream.clone(),
                token: "upstream-token".into(),
            })
        }
    }

    async fn send(router: &Router, method: &str, uri: &str) -> (StatusCode, serde_json::Value) {
        let response = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(if method == "GET" { "" } else { "{}" }))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
        )
    }

    #[tokio::test]
    async fn a_busy_project_of_another_build_is_readable_and_refuses_the_rest_in_plain_words() {
        // The other build's server, which only reads may reach.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let app = Router::new().route(
                "/v1/threads",
                get(|headers: axum::http::HeaderMap| async move {
                    assert_eq!(headers["authorization"], "Bearer upstream-token");
                    Json(serde_json::json!([{"id": "chat"}]))
                }),
            );
            axum::serve(listener, app).await.unwrap();
        });
        let host = Arc::new(Busy {
            upstream,
            closed: Default::default(),
        });
        let router = routes(host.clone(), tokio_util::sync::CancellationToken::new());

        let (status, body) = send(&router, "GET", "/v1/host/workspaces/w1/v1/threads").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!([{"id": "chat"}]));

        let (status, body) =
            send(&router, "POST", "/v1/host/workspaces/w1/v1/permissions/p1").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body,
            serde_json::json!({"error": {"code": "project_busy", "message": ProjectBusy::MESSAGE, "retryable": true}})
        );
        let text = body.to_string();
        assert!(
            !text.contains("4242") && !text.contains("serve --stop"),
            "{text}"
        );

        // Other host failures keep their code and words.
        let (status, body) = send(&router, "GET", "/v1/host/directories").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "remote_project");
        assert_eq!(body["error"]["message"], "Folder is unavailable");

        // Closing still takes the workspace id at the same path.
        let response = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("DELETE")
                    .uri("/v1/host/projects/w1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(*host.closed.lock().unwrap(), ["w1"]);
        server.abort();
    }
}
