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
use haarness_core::AgentRuntime;
use haarness_protocol::{EventEnvelope, ThreadId};
use serde_json::{Value, json};
use std::collections::HashSet;
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
/// Which events one stream of a published chat may see: those of the chat's
/// conversation tree. The tree is read once and again only for a chat the
/// stream has not met yet (a subagent started meanwhile), so the events of
/// other chats on this computer cost no lookup each.
pub struct EventScope {
    root: Option<ThreadId>,
    tree: Option<HashSet<ThreadId>>,
    outside: HashSet<ThreadId>,
    /// How often the tree was read.
    reads: usize,
}
impl EventScope {
    #[must_use]
    pub fn new(root: Option<ThreadId>) -> Self {
        Self {
            root,
            tree: None,
            outside: HashSet::new(),
            reads: 0,
        }
    }
    /// Whether `event` reaches this stream's client, when that is known
    /// without reading the chat tree again (`None` when it is not).
    #[must_use]
    pub fn decided(&self, event: &EventEnvelope) -> Option<bool> {
        let Some(root) = self.root else {
            return Some(true);
        };
        // Published chats explicitly share project files and readable capability catalogs.
        // Their invalidations carry no unrelated conversation content.
        if event.thread_id.is_none()
            && matches!(
                event.event_type.as_str(),
                "files.updated" | "assets.updated"
            )
        {
            return Some(true);
        }
        if event.root_thread_id == Some(root) || event.thread_id == Some(root) {
            return Some(true);
        }
        let Some(thread) = event.thread_id else {
            return Some(false);
        };
        if self.outside.contains(&thread) {
            return Some(false);
        }
        self.tree
            .as_ref()
            .is_some_and(|tree| tree.contains(&thread))
            .then_some(true)
    }
    pub async fn allows(&mut self, event: &EventEnvelope, runtime: &AgentRuntime) -> bool {
        if let Some(decided) = self.decided(event) {
            return decided;
        }
        let (Some(root), Some(thread)) = (self.root, event.thread_id) else {
            return false;
        };
        // A chat never changes its parent, so one that is outside the tree
        // now stays outside; a new subagent joins at its first event.
        self.reads += 1;
        match runtime.thread_tree_ids(root).await {
            Ok(tree) => {
                let inside = tree.contains(&thread);
                self.tree = Some(tree);
                if !inside {
                    self.outside.insert(thread);
                }
                inside
            }
            Err(_) => false,
        }
    }
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
    // A list asked for one chat (`?threadId=`: commands with that chat's
    // workflows, its tools, its MCP servers) is of a chat of the tree.
    if url::form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
        .any(|(key, value)| key == "threadId" && !visible(&value))
    {
        return denied();
    }
    let permitted = match parts.as_slice() {
        [
            "threads" | "permissions" | "questions" | "info" | "events" | "providers" | "agents"
            | "commands" | "plugins" | "tools",
        ]
        | ["turns" | "commands", "active"]
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
            "/threads"
                | "/permissions"
                | "/questions"
                | "/turns/active"
                | "/commands/active"
                | "/info"
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
                        || c.starts_with("mcp-resource-mentions")
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

#[cfg(test)]
mod tests {
    use super::*;
    use haarness_core::{EventBus, StaticProviderRegistry};
    use haarness_protocol::CreateThreadRequest;
    use std::sync::Arc;

    #[tokio::test]
    async fn a_published_chats_stream_reads_its_tree_once_per_chat_it_meets() {
        let events = EventBus::new(16);
        let runtime = AgentRuntime::new(
            Arc::new(haarness_store::SqliteStore::in_memory().unwrap()),
            Arc::new(StaticProviderRegistry::new(Vec::new())),
            Arc::new(crate::NativeToolRegistry::new(std::env::temp_dir())),
            events.clone(),
        );
        let create = |parent| {
            runtime.create_thread(CreateThreadRequest {
                parent_id: parent,
                provider: Some("echo".into()),
                model: Some("echo-1".into()),
                ..Default::default()
            })
        };
        let root = create(None).await.unwrap().id;
        let child = create(Some(root)).await.unwrap().id;
        let other = create(None).await.unwrap().id;
        let event = |thread: ThreadId| {
            events.publish(
                "turn.text_delta",
                Some(thread),
                None,
                None,
                json!({"delta": "x"}),
            )
        };
        let mut scope = EventScope::new(Some(root));
        for _ in 0..50 {
            assert!(!scope.allows(&event(other), &runtime).await);
            assert!(scope.allows(&event(child), &runtime).await);
            assert!(scope.allows(&event(root), &runtime).await);
        }
        assert_eq!(
            scope.reads, 1,
            "another chat's events cost one read, not one each"
        );
        // A subagent started after the stream opened joins at its first event.
        let late = create(Some(child)).await.unwrap().id;
        assert!(scope.allows(&event(late), &runtime).await);
        assert!(!scope.allows(&event(other), &runtime).await);
        assert_eq!(scope.reads, 2);
        let mut unscoped = EventScope::new(None);
        assert!(unscoped.allows(&event(other), &runtime).await);
    }
}
