//! A published chat is a view of one conversation tree, not a machine-wide credential.
//! The connector supplies this scope over an authenticated loopback request.
use crate::api::ServerState;
use axum::{
    Json,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use haarness_protocol::{EventEnvelope, ThreadId};
use serde_json::{Value, json};
use uuid::Uuid;

pub const HEADER: &str = "x-haarness-remote-thread";
fn denied() -> Response {
    (StatusCode::FORBIDDEN,Json(json!({"error":{"code":"remote_scope","message":"This operation is outside the published chat","retryable":false}}))).into_response()
}
pub fn root(headers: &axum::http::HeaderMap) -> Option<ThreadId> {
    headers
        .get(HEADER)?
        .to_str()
        .ok()?
        .parse::<Uuid>()
        .ok()
        .map(ThreadId)
}
pub async fn event_allowed(
    event: &EventEnvelope,
    root: Option<ThreadId>,
    state: &ServerState,
) -> bool {
    let Some(root) = root else { return true };
    // Published chats explicitly share project files and readable capability catalogs.
    // Their invalidations carry no unrelated conversation content.
    if event.thread_id.is_none()
        && matches!(
            event.event_type.as_str(),
            "files.updated" | "assets.updated"
        )
    {
        return true;
    }
    if event.root_thread_id == Some(root) || event.thread_id == Some(root) {
        return true;
    }
    let Some(thread) = event.thread_id else {
        return false;
    };
    state
        .runtime
        .thread_tree_ids(root)
        .await
        .is_ok_and(|ids| ids.contains(&thread))
}
pub async fn enforce(State(state): State<ServerState>, request: Request, next: Next) -> Response {
    if !request.headers().contains_key(HEADER) {
        return next.run(request).await;
    }
    let Some(root) = root(request.headers()) else {
        return denied();
    };
    let Ok(allowed) = state.runtime.thread_tree_ids(root).await else {
        return denied();
    };
    let path = request.uri().path().trim_start_matches("/v1").to_owned();
    let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    let get = request.method() == Method::GET;
    let visible = |id: &str| {
        id.parse::<Uuid>()
            .ok()
            .map(ThreadId)
            .is_some_and(|id| allowed.contains(&id))
    };
    let permitted = match parts.as_slice() {
        [
            "threads" | "permissions" | "questions" | "info" | "events" | "providers" | "agents"
            | "commands" | "plugins" | "tools",
        ]
        | ["turns", "active"]
        | ["skills" | "mcp", ..] => get,
        ["threads", id, rest @ ..] => visible(id) && !rest.contains(&"fork"),
        ["turns", id, ..] => {
            if let Ok(id) = id.parse::<Uuid>() {
                state
                    .runtime
                    .turn(haarness_protocol::TurnId(id))
                    .await
                    .is_ok_and(|turn| allowed.contains(&turn.thread_id))
            } else {
                false
            }
        }
        ["permissions", id] => state
            .runtime
            .pending_permissions()
            .iter()
            .any(|p| p.permission_id == *id && allowed.contains(&p.thread_id)),
        ["questions", id] => state
            .runtime
            .pending_questions()
            .iter()
            .any(|q| q.question_id == *id && allowed.contains(&q.thread_id)),
        ["files" | "vcs", ..] => true,
        _ => false,
    };
    if !permitted {
        return denied();
    }
    let response = next.run(request).await;
    // Events are filtered at their source below; large file bodies remain streaming.
    if !response.status().is_success()
        || !matches!(
            path.as_str(),
            "/threads" | "/permissions" | "/questions" | "/turns/active" | "/info"
        )
    {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let Ok(body) = to_bytes(body, 16 * 1024 * 1024).await else {
        return denied();
    };
    let Ok(mut body) = serde_json::from_slice::<Value>(&body) else {
        return denied();
    };
    if let Some(rows) = body.as_array_mut() {
        rows.retain(|row| {
            row[if path == "/threads" { "id" } else { "threadId" }]
                .as_str()
                .is_some_and(visible)
        });
    }
    if path == "/info" {
        if let Some(capabilities) = body["capabilities"].as_array_mut() {
            capabilities.retain(|c| {
                !c.as_str().is_some_and(|c| {
                    c.starts_with("pty")
                        || c.starts_with("workspace")
                        || c.starts_with("session.archive")
                        || c.starts_with("browser-extension")
                })
            });
        }
        // The process id serves local Chrome discovery only.
        if let Some(info) = body.as_object_mut() {
            info.remove("pid");
        }
    }
    parts.headers.remove("content-length");
    Response::from_parts(parts, Body::from(body.to_string()))
}
