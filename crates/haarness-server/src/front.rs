//! Another address for a project whose server already runs. A project has one
//! server: it owns the chats, terminals, schedules and the store, and every
//! client finds it through the project's runtime record. A `haarness serve`
//! started while that server runs listens through this front instead of
//! becoming a second owner. The front admits requests by its own address and
//! credentials, as a server listening there would, then hands each one to the
//! project's server as a local client holding that server's token.
use crate::auth::{ANONYMOUS_HEADER, AnonymousAccess, AuthState, ServerAccess, require_auth};
use crate::remote::{ProjectBusy, WorkspaceTarget, pipe_websocket};
use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::{
        Request, State,
        ws::{WebSocketUpgrade, rejection::WebSocketUpgradeRejection},
    },
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
};
use futures::StreamExt;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};
use tokio_util::sync::CancellationToken;

/// The largest request body the front holds: twice the server's largest
/// upload (a turn's attachments, a profile or session archive), as the host
/// gateway allows.
const BODY_LIMIT: usize = 64 * 1024 * 1024;

/// Where the project's server is. The composition root knows how to find it
/// (its runtime record) and how to start one when none answers.
#[async_trait::async_trait]
pub trait Upstream: Send + Sync {
    /// The project's server for the next request.
    async fn current(&self) -> anyhow::Result<WorkspaceTarget>;
    /// A project server that answers, after `unreachable` did not: the one
    /// that replaced it, or a new one.
    async fn recover(&self, unreachable: &WorkspaceTarget) -> anyhow::Result<WorkspaceTarget>;
}

pub struct FrontHandle {
    pub address: SocketAddr,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl FrontHandle {
    #[must_use]
    pub fn shutdown_signal(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// Wait until the listener has closed and its open streams have ended.
    pub async fn wait(self) -> anyhow::Result<()> {
        self.task.await?
    }
}

#[derive(Clone)]
struct Front {
    upstream: Arc<dyn Upstream>,
    client: reqwest::Client,
    shutdown: CancellationToken,
}

/// Serve `listener` in front of the project's server until
/// [`FrontHandle::shutdown_signal`] fires or a client asks this address to
/// shut down.
pub fn serve(
    listener: TcpListener,
    token: &str,
    access: &ServerAccess,
    upstream: Arc<dyn Upstream>,
) -> anyhow::Result<FrontHandle> {
    let address = listener.local_addr()?;
    let auth = AuthState::with_access(token, address.ip().is_loopback(), access);
    let shutdown = CancellationToken::new();
    let front = Front {
        upstream,
        shutdown: shutdown.clone(),
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            // The project's server is always on this computer.
            .no_proxy()
            .build()?,
    };
    let app = Router::new()
        .route("/v1/shutdown", post(stop))
        .fallback(forward)
        .layer(middleware::from_fn_with_state(auth, admit))
        .with_state(front);
    let server_shutdown = shutdown.clone();
    let task = tokio::spawn(async move {
        let app = app.into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, app)
            .with_graceful_shutdown(server_shutdown.cancelled_owned())
            .await
            .map_err(Into::into)
    });
    Ok(FrontHandle {
        address,
        shutdown,
        task,
    })
}

/// What the server leaves open (its health check, API document and share
/// links) stays open here. The API needs this address's credentials, checked
/// as the server checks its own, and only those requests reach the server
/// with its token.
fn protected(path: &str) -> bool {
    path == "/v1" || path.starts_with("/v1/")
}

async fn admit(State(auth): State<AuthState>, request: Request, next: Next) -> Response {
    if protected(request.uri().path()) {
        require_auth(State(auth), request, next).await
    } else {
        next.run(request).await
    }
}

/// Closes this address only. The project's server also serves the TUI,
/// Desktop and whoever else found it, and keeps their work going; `haarness
/// serve --stop` stops it.
async fn stop(State(front): State<Front>) -> StatusCode {
    front.shutdown.cancel();
    StatusCode::ACCEPTED
}

fn unavailable(error: &anyhow::Error, admitted: bool) -> Response {
    tracing::warn!(error = %format!("{error:#}"), "project server unavailable to the front");
    if let Some(busy) = error.downcast_ref::<ProjectBusy>() {
        return (StatusCode::CONFLICT, Json(serde_json::json!({"error": {"code": ProjectBusy::CODE, "message": busy.message(), "retryable": true}}))).into_response();
    }
    let message = if admitted {
        format!("{error:#}")
    } else {
        "This project's Haarness server is not reachable".to_owned()
    };
    (StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": {"code": "project_server_unavailable", "message": message, "retryable": true}}))).into_response()
}

/// Headers that describe one connection, never the request it carries.
fn hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// The request's headers as the project's server should see them: from a
/// local client on its own address. The front has already judged the
/// browser's Origin against its own address, so a browser request keeps an
/// Origin, now the server's own; credentials are the server's token, and only
/// on an admitted API request.
fn forwarded_headers(
    headers: &HeaderMap,
    target: &WorkspaceTarget,
    admitted: bool,
    anonymous: bool,
) -> HeaderMap {
    let mut forwarded = HeaderMap::new();
    for (name, value) in headers {
        if hop_by_hop(name)
            || name == header::HOST
            || name == header::AUTHORIZATION
            || name == header::CONTENT_LENGTH
            || name == header::ORIGIN
            || name.as_str() == ANONYMOUS_HEADER
            || name.as_str().starts_with("sec-websocket-")
        {
            continue;
        }
        forwarded.append(name.clone(), value.clone());
    }
    if headers.contains_key(header::ORIGIN)
        && let Some(origin) = url::Url::parse(&target.url)
            .ok()
            .and_then(|url| HeaderValue::from_str(&url.origin().ascii_serialization()).ok())
    {
        forwarded.insert(header::ORIGIN, origin);
    }
    if admitted {
        if let Ok(value) = HeaderValue::from_str(&format!("Bearer {}", target.token)) {
            forwarded.insert(header::AUTHORIZATION, value);
        }
        if anonymous {
            forwarded.insert(
                HeaderName::from_static(ANONYMOUS_HEADER),
                HeaderValue::from_static("1"),
            );
        }
    }
    forwarded
}

async fn forward(
    State(front): State<Front>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
    request: Request,
) -> Response {
    let admitted = protected(request.uri().path());
    let anonymous = request.extensions().get::<AnonymousAccess>().is_some();
    let path = request.uri().path_and_query().map_or_else(
        || request.uri().path().to_owned(),
        |value| value.as_str().to_owned(),
    );
    let target = match front.upstream.current().await {
        Ok(target) => target,
        Err(error) => return unavailable(&error, admitted),
    };
    if let Ok(ws) = ws {
        return forward_websocket(
            front,
            ws,
            request.headers(),
            &path,
            target,
            admitted,
            anonymous,
        )
        .await;
    }
    let (parts, body) = request.into_parts();
    let Ok(body) = to_bytes(body, BODY_LIMIT).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let send = |target: &WorkspaceTarget, body: Bytes| {
        front
            .client
            .request(
                parts.method.clone(),
                format!("{}{path}", target.url.trim_end_matches('/')),
            )
            .headers(forwarded_headers(
                &parts.headers,
                target,
                admitted,
                anonymous,
            ))
            .body(body)
            .send()
    };
    let response = match send(&target, body.clone()).await {
        // The server stopped or was replaced since its record was read.
        Err(error) if error.is_connect() => match front.upstream.recover(&target).await {
            Ok(target) => send(&target, body).await,
            Err(error) => return unavailable(&error, admitted),
        },
        other => other,
    };
    let response = match response {
        Ok(response) => response,
        Err(error) => return unavailable(&error.into(), admitted),
    };
    let status = response.status();
    let headers = response.headers().clone();
    let mut result = Response::new(Body::from_stream(
        response
            .bytes_stream()
            .take_until(front.shutdown.cancelled_owned()),
    ));
    *result.status_mut() = status;
    for (name, value) in &headers {
        if !hop_by_hop(name) {
            result.headers_mut().append(name.clone(), value.clone());
        }
    }
    result
}

async fn forward_websocket(
    front: Front,
    ws: WebSocketUpgrade,
    headers: &HeaderMap,
    path: &str,
    target: WorkspaceTarget,
    admitted: bool,
    anonymous: bool,
) -> Response {
    let connect = |target: &WorkspaceTarget| {
        let url = format!("{}{path}", target.url.trim_end_matches('/')).replacen("http", "ws", 1);
        let handshake = url.into_client_request().map(|mut handshake| {
            handshake
                .headers_mut()
                .extend(forwarded_headers(headers, target, admitted, anonymous));
            handshake
        });
        async move { tokio_tungstenite::connect_async(handshake?).await }
    };
    let upstream = match connect(&target).await {
        Err(tungstenite::Error::Io(_)) => match front.upstream.recover(&target).await {
            Ok(target) => connect(&target).await,
            Err(error) => return unavailable(&error, admitted),
        },
        other => other,
    };
    match upstream {
        Ok((upstream, _)) => {
            let shutdown = front.shutdown.clone();
            ws.on_upgrade(move |socket| pipe_websocket(socket, upstream, shutdown))
                .into_response()
        }
        // The server refused the upgrade: say what it said.
        Err(tungstenite::Error::Http(refusal)) => {
            let status = refusal.status();
            (status, refusal.into_body().unwrap_or_default()).into_response()
        }
        Err(error) => unavailable(&error.into(), admitted),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::ws::Message,
        routing::{any, get},
    };
    use futures::SinkExt;
    use std::sync::Mutex;

    /// A stand-in for the project's server: it requires `upstream-token` on
    /// /v1 and reports what it received.
    async fn upstream_server() -> (String, CancellationToken) {
        async fn echo(headers: HeaderMap, request: Request) -> Response {
            let authorized = headers
                .get(header::AUTHORIZATION)
                .is_some_and(|value| value == "Bearer upstream-token");
            if request.uri().path().starts_with("/v1/") && !authorized {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            let pick = |name: &str| {
                headers
                    .get(name)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned)
            };
            Json(serde_json::json!({
                "path": request.uri().to_string(),
                "method": request.method().as_str(),
                "authorization": pick("authorization"),
                "origin": pick("origin"),
                "anonymous": pick(ANONYMOUS_HEADER),
                "custom": pick("x-custom"),
                "body": String::from_utf8_lossy(&to_bytes(request.into_body(), 1024).await.unwrap()),
            }))
            .into_response()
        }
        async fn socket(upgrade: WebSocketUpgrade, headers: HeaderMap) -> Response {
            if headers
                .get(header::AUTHORIZATION)
                .is_none_or(|value| value != "Bearer upstream-token")
            {
                return (StatusCode::FORBIDDEN, "token required").into_response();
            }
            upgrade.on_upgrade(|mut socket| async move {
                while let Some(Ok(Message::Text(text))) = socket.recv().await {
                    let reply = format!("echo {}", text.as_str());
                    if socket.send(Message::Text(reply.into())).await.is_err() {
                        break;
                    }
                }
            })
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let shutdown = CancellationToken::new();
        let app = Router::new()
            .route("/v1/socket", get(socket))
            .fallback(any(echo));
        let stop = shutdown.clone();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(stop.cancelled_owned())
                .await
        });
        (url, shutdown)
    }

    /// Hands out `first` until it fails, then `next`.
    struct Fixed {
        first: String,
        next: String,
        recovered: Mutex<Vec<String>>,
    }
    #[async_trait::async_trait]
    impl Upstream for Fixed {
        async fn current(&self) -> anyhow::Result<WorkspaceTarget> {
            Ok(WorkspaceTarget {
                url: self.first.clone(),
                token: "upstream-token".into(),
            })
        }
        async fn recover(&self, unreachable: &WorkspaceTarget) -> anyhow::Result<WorkspaceTarget> {
            self.recovered.lock().unwrap().push(unreachable.url.clone());
            Ok(WorkspaceTarget {
                url: self.next.clone(),
                token: "upstream-token".into(),
            })
        }
    }

    async fn front(
        access: &ServerAccess,
        first: &str,
        next: &str,
    ) -> (String, FrontHandle, Arc<Fixed>) {
        let upstream = Arc::new(Fixed {
            first: first.into(),
            next: next.into(),
            recovered: Mutex::default(),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let handle = serve(listener, "front-token", access, upstream.clone()).unwrap();
        (format!("http://{}", handle.address), handle, upstream)
    }

    #[tokio::test]
    async fn the_api_needs_the_fronts_credentials_and_reaches_the_server_with_its_token() {
        let (upstream, _stop) = upstream_server().await;
        let (url, handle, _) = front(&ServerAccess::Private, &upstream, &upstream).await;
        let client = reqwest::Client::new();

        let refused = client.get(format!("{url}/v1/info")).send().await.unwrap();
        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
        // The server's own token is not this address's credential.
        let refused = client
            .get(format!("{url}/v1/info"))
            .bearer_auth("upstream-token")
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);

        let seen: serde_json::Value = client
            .post(format!("{url}/v1/threads?limit=2"))
            .bearer_auth("front-token")
            .header("x-custom", "kept")
            .header(ANONYMOUS_HEADER, "1")
            .body("hello")
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(seen["path"], "/v1/threads?limit=2");
        assert_eq!(seen["method"], "POST");
        assert_eq!(seen["authorization"], "Bearer upstream-token");
        assert_eq!(seen["custom"], "kept");
        assert_eq!(seen["body"], "hello");
        // A client may give up its access; the server hears of it.
        assert_eq!(seen["anonymous"], "1");
        let seen: serde_json::Value = client
            .get(format!("{url}/v1/info"))
            .bearer_auth("front-token")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(seen["anonymous"], serde_json::Value::Null);

        // The health check and share links stay open, and get no token.
        let seen: serde_json::Value = client
            .get(format!("{url}/healthz"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(seen["authorization"], serde_json::Value::Null);
        handle.shutdown_signal().cancel();
        handle.wait().await.unwrap();
    }

    #[tokio::test]
    async fn an_open_front_marks_its_requests_anonymous_and_gives_the_server_its_own_origin() {
        let (upstream, _stop) = upstream_server().await;
        let (url, handle, _) = front(&ServerAccess::Open, &upstream, &upstream).await;
        let port = handle.address.port();
        let seen: serde_json::Value = reqwest::Client::new()
            .get(format!("{url}/v1/info"))
            .header("origin", format!("http://127.0.0.1:{port}"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(seen["anonymous"], "1");
        assert_eq!(seen["origin"], upstream);
        // Another local site is refused here, as the server would refuse it.
        let refused = reqwest::Client::new()
            .get(format!("{url}/v1/info"))
            .header("origin", "http://127.0.0.1:1")
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        handle.shutdown_signal().cancel();
        handle.wait().await.unwrap();
    }

    #[tokio::test]
    async fn shutting_down_the_front_leaves_the_server_running() {
        let (upstream, _stop) = upstream_server().await;
        let (url, handle, _) = front(&ServerAccess::Private, &upstream, &upstream).await;
        let accepted = reqwest::Client::new()
            .post(format!("{url}/v1/shutdown"))
            .bearer_auth("front-token")
            .send()
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::ACCEPTED);
        tokio::time::timeout(Duration::from_secs(5), handle.wait())
            .await
            .expect("front stops")
            .unwrap();
        let alive = reqwest::Client::new()
            .get(format!("{upstream}/healthz"))
            .send()
            .await
            .unwrap();
        assert!(alive.status().is_success());
    }

    #[tokio::test]
    async fn a_server_that_went_away_is_found_again() {
        let (upstream, _stop) = upstream_server().await;
        // A port nothing listens on: the record of a server that stopped.
        let gone = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", listener.local_addr().unwrap())
        };
        let (url, handle, fixed) = front(&ServerAccess::Private, &gone, &upstream).await;
        let seen: serde_json::Value = reqwest::Client::new()
            .post(format!("{url}/v1/turns"))
            .bearer_auth("front-token")
            .body("kept for the retry")
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(seen["body"], "kept for the retry");
        assert_eq!(*fixed.recovered.lock().unwrap(), vec![gone]);
        handle.shutdown_signal().cancel();
        handle.wait().await.unwrap();
    }

    #[tokio::test]
    async fn websockets_reach_the_server_with_its_token() {
        let (upstream, _stop) = upstream_server().await;
        let (url, handle, _) = front(&ServerAccess::Private, &upstream, &upstream).await;
        let mut request = format!("{}/v1/socket", url.replacen("http", "ws", 1))
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert(header::AUTHORIZATION, "Bearer front-token".parse().unwrap());
        let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        socket
            .send(tungstenite::Message::Text("ping".into()))
            .await
            .unwrap();
        let reply = socket.next().await.unwrap().unwrap();
        assert_eq!(reply.into_text().unwrap().as_str(), "echo ping");
        drop(socket);
        handle.shutdown_signal().cancel();
        handle.wait().await.unwrap();
    }
}
