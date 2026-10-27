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
/// A project's server that the host would have to replace cannot be while it
/// is still working, or while it cannot tell whether it is. Hosts return this
/// (as the error or its source) so the gateway can say so in plain words,
/// without the pid and shell command a person at the computer gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectBusy {
    /// Another Haarness build started the server.
    OtherBuild,
    /// The server runs with the other project configuration setting.
    OtherSetting,
}
impl ProjectBusy {
    pub const CODE: &'static str = "project_busy";
    pub const MESSAGE: &'static str = "This project is busy on a different version of Haarness. Open it on the computer, or wait until its work finishes.";
    pub const SETTING_MESSAGE: &'static str = "This project is busy with a different project configuration setting. Open it on the computer, or wait until its work finishes.";
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::OtherBuild => Self::MESSAGE,
            Self::OtherSetting => Self::SETTING_MESSAGE,
        }
    }
}
impl std::fmt::Display for ProjectBusy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
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
    /// Where a published chat's request goes (the connector names the chat in
    /// [`crate::remote_scope::HEADER`]). The chat was shared on purpose, so it
    /// may live in a workspace the host keeps from project-wide access, such
    /// as the chat home; the workspace server still confines it to that chat.
    async fn chat_target(&self, id: String, read: bool) -> anyhow::Result<WorkspaceTarget> {
        if read {
            self.read_target(id).await
        } else {
            self.target(id).await
        }
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
        // The path keeps its old name; the id is a workspace id.
        .route(
            "/v1/host/projects/{workspace_id}",
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
    if let Some(busy) = error.downcast_ref::<ProjectBusy>() {
        return (StatusCode::CONFLICT, Json(serde_json::json!({"error": {"code":ProjectBusy::CODE, "message":busy.message(), "retryable":true}}))).into_response();
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
/// Closes a project for remote access. The id is the catalog's workspace id
/// (`workspaces[].id` from `GET /v1/host/projects`), not its project id.
#[utoipa::path(delete,path="/v1/host/projects/{workspace_id}",operation_id="close_remote_project",params(("workspace_id"=String,Path,description="Workspace id: `workspaces[].id` from GET /v1/host/projects, not `projects[].id`")),responses((status=204)))]
async fn close_project(
    State(gateway): State<Gateway>,
    Path(workspace_id): Path<String>,
) -> Response {
    match gateway.host.close(workspace_id).await {
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
    let chat = crate::remote_scope::root(request.headers()).is_some();
    let mut target = match resolve(&gateway, &id, chat, read).await {
        Ok(value) => value,
        Err(error) => return failure(error),
    };
    let prefix = format!("/v1/host/workspaces/{id}/");
    let raw_path = request
        .uri()
        .path()
        .strip_prefix(&prefix)
        .unwrap_or(&path)
        .to_owned();
    let query = request.uri().query().map(str::to_owned);
    let address = |target: &WorkspaceTarget| {
        let mut url = format!("{}/{}", target.url.trim_end_matches('/'), raw_path);
        if let Some(query) = &query {
            url.push('?');
            url.push_str(query);
        }
        url
    };
    // A project's server that stopped since the host found it takes no
    // connection, and nothing has reached it: find (or start) it again,
    // once, before saying it could not be reached.
    let mut retried = false;
    if let Ok(ws) = ws {
        let upstream = loop {
            let handshake = address(&target)
                .replacen("http", "ws", 1)
                .into_client_request()
                .ok()
                .zip(format!("Bearer {}", target.token).parse().ok());
            let Some((mut handshake, auth)) = handshake else {
                return unreachable("bad upstream address");
            };
            handshake.headers_mut().insert("authorization", auth);
            match tokio_tungstenite::connect_async(handshake).await {
                Ok((socket, _)) => break socket,
                Err(_) if !retried => {
                    retried = true;
                    target = match resolve(&gateway, &id, chat, read).await {
                        Ok(value) => value,
                        Err(error) => return failure(error),
                    };
                }
                Err(error) => return unreachable(&error.to_string()),
            }
        };
        let shutdown = gateway.shutdown.clone();
        return ws
            .on_upgrade(move |socket| pipe_websocket(socket, upstream, shutdown))
            .into_response();
    }
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, 64 * 1024 * 1024).await {
        Ok(value) => value,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let mut url = address(&target);
    loop {
        let mut upstream = gateway
            .client
            .request(parts.method.clone(), &url)
            .bearer_auth(&target.token);
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
        match upstream.body(body.clone()).send().await {
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
                return result
                    .body(Body::from_stream(
                        response
                            .bytes_stream()
                            .take_until(gateway.shutdown.cancelled_owned()),
                    ))
                    .expect("upstream response");
            }
            Err(error) if error.is_connect() && !retried => {
                retried = true;
                target = match resolve(&gateway, &id, chat, read).await {
                    Ok(value) => value,
                    Err(error) => return failure(error),
                };
                url = address(&target);
            }
            Err(error) => return unreachable(&error.to_string()),
        }
    }
}

/// Where a request for workspace `id` goes: one published chat's (the
/// connector names it in [`crate::remote_scope::HEADER`]), a read's, or any.
async fn resolve(
    gateway: &Gateway,
    id: &str,
    chat: bool,
    read: bool,
) -> anyhow::Result<WorkspaceTarget> {
    if chat {
        gateway.host.chat_target(id.to_owned(), read).await
    } else if read {
        gateway.host.read_target(id.to_owned()).await
    } else {
        gateway.host.target(id.to_owned()).await
    }
}

/// The project's server did not take the request, even found again. The
/// person reading this is at another device, so it says what to do there
/// (clients name it by `project_server_unavailable`, as the front does);
/// the cause stays in this computer's log.
fn unreachable(cause: &str) -> Response {
    tracing::warn!(%cause, "project server unreachable from the remote gateway");
    (
        StatusCode::BAD_GATEWAY,
        Json(serde_json::json!({"error": {
            "code": "project_server_unavailable",
            "message": "Your computer couldn't answer. Check that Haarness is open and up to date on it, then try again.",
            "retryable": true,
        }})),
    )
        .into_response()
}

/// Carry one client WebSocket to its upstream and back until either side
/// closes or `shutdown` fires.
pub(crate) async fn pipe_websocket(
    socket: axum::extract::ws::WebSocket,
    upstream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    shutdown: tokio_util::sync::CancellationToken,
) {
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
            if upstream_sink.send(message).await.is_err() {
                break;
            }
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
            if client_sink.send(message).await.is_err() {
                break;
            }
        }
        let _ = client_sink.close().await;
    };
    tokio::select! { () = outgoing => {}, () = incoming => {}, () = shutdown.cancelled() => {} }
}

#[derive(OpenApi)]
#[openapi(paths(catalog, browse, open, close_project))]
pub struct HostApi;

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt as _;

    /// A host whose project's server is still working and would have to be
    /// replaced: another Haarness build runs it, or it runs with the other
    /// project configuration setting.
    struct Busy {
        upstream: String,
        closed: std::sync::Mutex<Vec<String>>,
        kind: std::sync::Mutex<ProjectBusy>,
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
            Err(anyhow::Error::new(*self.kind.lock().unwrap()).context(
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
            closed: std::sync::Mutex::default(),
            kind: std::sync::Mutex::new(ProjectBusy::OtherBuild),
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
        // A server of this build, busy with the other project configuration
        // setting, is not said to be another version.
        *host.kind.lock().unwrap() = ProjectBusy::OtherSetting;
        let (status, body) =
            send(&router, "POST", "/v1/host/workspaces/w1/v1/permissions/p1").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body,
            serde_json::json!({"error": {"code": "project_busy", "message": ProjectBusy::SETTING_MESSAGE, "retryable": true}})
        );
        let (status, _) = send(&router, "GET", "/v1/host/workspaces/w1/v1/threads").await;
        assert_eq!(status, StatusCode::OK, "reads still reach it");

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

    /// A host that keeps one workspace (a chat home) from project-wide access
    /// but lets a published chat in it through.
    struct ChatHome {
        upstream: String,
    }
    #[async_trait::async_trait]
    impl WorkspaceHost for ChatHome {
        async fn catalog(&self) -> anyhow::Result<WorkspaceListResponse> {
            anyhow::bail!("unused")
        }
        async fn browse(
            &self,
            _: Option<String>,
            _: BrowseOptions,
        ) -> anyhow::Result<DirectoryListing> {
            anyhow::bail!("unused")
        }
        async fn open(&self, _: String) -> anyhow::Result<WorkspaceInfo> {
            anyhow::bail!("unused")
        }
        async fn target(&self, _: String) -> anyhow::Result<WorkspaceTarget> {
            anyhow::bail!("Chats without a project stay on their computer")
        }
        async fn chat_target(&self, _: String, _: bool) -> anyhow::Result<WorkspaceTarget> {
            Ok(WorkspaceTarget {
                url: self.upstream.clone(),
                token: "home-token".into(),
            })
        }
    }

    #[tokio::test]
    async fn a_published_chat_reaches_a_workspace_kept_from_project_access() {
        let chat = uuid::Uuid::now_v7().to_string();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = format!("http://{}", listener.local_addr().unwrap());
        let expected = chat.clone();
        let server = tokio::spawn(async move {
            let app = Router::new().route(
                "/v1/threads",
                any(move |headers: axum::http::HeaderMap| async move {
                    assert_eq!(headers["authorization"], "Bearer home-token");
                    // The workspace server still confines the request to the chat.
                    assert_eq!(headers[crate::remote_scope::HEADER], expected.as_str());
                    Json(serde_json::json!([{"id": "chat"}]))
                }),
            );
            axum::serve(listener, app).await.unwrap();
        });
        let router = routes(
            Arc::new(ChatHome { upstream }),
            tokio_util::sync::CancellationToken::new(),
        );

        let (status, body) = send(&router, "GET", "/v1/host/workspaces/home/v1/threads").await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "project-wide access stays refused"
        );
        assert_eq!(body["error"]["code"], "remote_project");

        for method in ["GET", "POST"] {
            let response = router
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method(method)
                        .uri("/v1/host/workspaces/home/v1/threads")
                        .header(crate::remote_scope::HEADER, &chat)
                        .header("content-type", "application/json")
                        .body(Body::from(if method == "GET" { "" } else { "{}" }))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{method}");
        }
        server.abort();
    }

    /// A host whose project's server moved: the first address it gives takes
    /// no connection (the server stopped since it was found), later ones are
    /// where it runs now, if anywhere.
    struct Moved {
        addresses: std::sync::Mutex<Vec<String>>,
        resolved: std::sync::atomic::AtomicUsize,
    }
    #[async_trait::async_trait]
    impl WorkspaceHost for Moved {
        async fn catalog(&self) -> anyhow::Result<WorkspaceListResponse> {
            anyhow::bail!("unused")
        }
        async fn browse(
            &self,
            _: Option<String>,
            _: BrowseOptions,
        ) -> anyhow::Result<DirectoryListing> {
            anyhow::bail!("unused")
        }
        async fn open(&self, _: String) -> anyhow::Result<WorkspaceInfo> {
            anyhow::bail!("unused")
        }
        async fn close(&self, _: String) -> anyhow::Result<()> {
            anyhow::bail!("unused")
        }
        async fn target(&self, _: String) -> anyhow::Result<WorkspaceTarget> {
            let index = self
                .resolved
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let addresses = self.addresses.lock().unwrap();
            Ok(WorkspaceTarget {
                url: addresses[index.min(addresses.len() - 1)].clone(),
                token: "token".into(),
            })
        }
    }

    /// An address nothing listens on.
    async fn closed_address() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        address
    }

    #[tokio::test]
    async fn a_project_server_that_went_away_is_found_again() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let live = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let app = Router::new().route(
                "/v1/threads",
                any(|body: String| async move { Json(serde_json::json!({"body": body})) }),
            );
            axum::serve(listener, app).await.unwrap();
        });
        let host = Arc::new(Moved {
            addresses: std::sync::Mutex::new(vec![closed_address().await, live]),
            resolved: std::sync::atomic::AtomicUsize::new(0),
        });
        let router = routes(host.clone(), tokio_util::sync::CancellationToken::new());

        // A write too: nothing reached the stopped server, so it goes once.
        let (status, body) = send(&router, "POST", "/v1/host/workspaces/w/v1/threads").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["body"], "{}");
        assert_eq!(host.resolved.load(std::sync::atomic::Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn a_project_server_that_stays_gone_is_said_plainly() {
        let gone = closed_address().await;
        let host = Arc::new(Moved {
            addresses: std::sync::Mutex::new(vec![gone]),
            resolved: std::sync::atomic::AtomicUsize::new(0),
        });
        let router = routes(host.clone(), tokio_util::sync::CancellationToken::new());

        let (status, body) = send(&router, "GET", "/v1/host/workspaces/w/v1/threads").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["error"]["code"], "project_server_unavailable");
        assert_eq!(body["error"]["retryable"], true);
        let message = body["error"]["message"].as_str().unwrap();
        assert!(
            message.starts_with("Your computer couldn't answer."),
            "{message}"
        );
        assert!(!message.contains("127.0.0.1"), "no address: {message}");
        assert_eq!(
            host.resolved.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "found again once, not in a loop"
        );
    }
}
