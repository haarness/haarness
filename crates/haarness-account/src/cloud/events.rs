//! Push notifications are hints; every wake reads authoritative HTTP state.
//! Subscribe before the first snapshot and resync after every connection gap.
use super::{Attachment, Cloud, MAX_RESPONSE, Remote};
use crate::safe_url;
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use std::{future::Future, time::Duration};
use tokio::{sync::watch, task::JoinHandle, time::Instant};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const IDLE: Duration = Duration::from_secs(45);
const CONNECT: Duration = Duration::from_secs(5);
const FALLBACK: Duration = Duration::from_secs(1);
const RECONCILE: Duration = Duration::from_secs(60);

pub(super) struct Updates {
    // A watch channel coalesces bursts while HTTP is in flight; no unbounded queue.
    receiver: watch::Receiver<bool>,
    task: JoinHandle<()>,
}

impl Drop for Updates {
    fn drop(&mut self) {
        // Also covers a dropped tool future, not just explicit cancellation.
        self.task.abort();
    }
}

impl Updates {
    async fn start<F: Future<Output = ()> + Send + 'static>(
        cancel: &CancellationToken,
        deadline: Instant,
        worker: impl FnOnce(watch::Sender<bool>) -> F,
    ) -> Self {
        let (sender, receiver) = watch::channel(false);
        let task = tokio::spawn(worker(sender));
        let mut updates = Self { receiver, task };
        // A slow/unsupported stream must not stall commands. When a late handshake
        // succeeds it triggers another snapshot, closing the subscription race.
        tokio::select! {
            _ = updates.receiver.changed() => {},
            () = cancel.cancelled() => {},
            () = tokio::time::sleep_until(deadline.min(Instant::now() + CONNECT)) => {},
        }
        updates
    }

    pub(super) async fn wait(&mut self, deadline: Instant, cancel: &CancellationToken) {
        let interval = if *self.receiver.borrow() && self.receiver.has_changed().is_ok() {
            RECONCILE
        } else {
            FALLBACK
        };
        tokio::select! {
            _ = self.receiver.changed(), if self.receiver.has_changed().is_ok() => {},
            () = cancel.cancelled() => {},
            () = tokio::time::sleep_until(deadline.min(Instant::now() + interval)) => {},
        }
    }

    pub(super) async fn control(cloud: Cloud, workspace: Uuid, deadline: Instant) -> Self {
        let cancel = cloud.cancel.clone();
        Self::start(&cancel, deadline, |sender| {
            Box::pin(async move {
                let mut retry = 0;
                let mut rejected = None;
                loop {
                    let connected =
                        control_connection(&cloud, workspace, &sender, &mut rejected).await;
                    sender.send_replace(false);
                    backoff(&mut retry, connected).await;
                }
            })
        })
        .await
    }

    pub(super) async fn runtime(
        cloud: Cloud,
        workspace: Uuid,
        attachment: Attachment,
        thread: Uuid,
        turn: Uuid,
        deadline: Instant,
    ) -> Self {
        let cancel = cloud.cancel.clone();
        Self::start(&cancel, deadline, |sender| {
            Box::pin(async move {
                // Unlike ordinary JSON requests, a live response has no total timeout.
                // An idle watchdog below still bounds silent/disconnected connections.
                let Ok(http) = reqwest::Client::builder()
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .connect_timeout(CONNECT)
                    .build()
                else {
                    return;
                };
                let mut initial = Some(attachment);
                let mut retry = 0;
                loop {
                    let attachment = if let Some(value) = initial.take() {
                        Ok(value)
                    } else {
                        // A reconnect obtains a fresh ticket and runtime generation.
                        Remote::attach(&cloud, workspace).await
                    };
                    let connected = match attachment {
                        Ok(attachment) => {
                            runtime_connection(&http, &attachment, thread, turn, &sender).await
                        }
                        Err(_) => false,
                    };
                    sender.send_replace(false);
                    backoff(&mut retry, connected).await;
                }
            })
        })
        .await
    }
}

async fn backoff(retry: &mut u32, connected: bool) {
    if connected {
        *retry = 0;
    }
    let seconds = 1_u64 << (*retry).min(5);
    *retry = retry.saturating_add(1);
    tokio::time::sleep(Duration::from_millis(
        seconds.min(30) * 1000 + rand::random_range(0..250),
    ))
    .await;
}

async fn control_connection(
    cloud: &Cloud,
    workspace: Uuid,
    sender: &watch::Sender<bool>,
    rejected: &mut Option<String>,
) -> bool {
    let Ok(bearer) = cloud.client.bearer(rejected.take().as_deref()).await else {
        return false;
    };
    let Ok(mut url) = cloud.client.endpoint("api/v1/events") else {
        return false;
    };
    url.query_pairs_mut()
        .append_pair("accountId", &cloud.account.to_string());
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    if url.set_scheme(scheme).is_err() {
        return false;
    }
    let Ok(mut request) = url.as_str().into_client_request() else {
        return false;
    };
    let Ok(auth) = format!("Bearer {bearer}").parse() else {
        return false;
    };
    request.headers_mut().insert("authorization", auth);
    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(64 * 1024))
        .max_frame_size(Some(64 * 1024));
    let connection =
        tokio::time::timeout(CONNECT, crate::websocket::connect(request, Some(config))).await;
    let mut socket = match connection {
        Ok(Ok((socket, _))) => socket,
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response)))
            if response.status() == reqwest::StatusCode::UNAUTHORIZED =>
        {
            *rejected = Some(bearer);
            return false;
        }
        _ => return false,
    };
    let mut ready = false;
    loop {
        match tokio::time::timeout(IDLE, socket.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let Ok(event) = serde_json::from_str::<Value>(&text) else {
                    return ready;
                };
                if event["type"] == "ready" {
                    tracing::debug!(transport = "websocket", %workspace, "Cloud event stream connected");
                    ready = true;
                }
                if ready && control_change(&event, cloud.account, workspace) {
                    sender.send_replace(true);
                }
            }
            Ok(Some(Ok(Message::Ping(bytes)))) => {
                if !matches!(
                    tokio::time::timeout(CONNECT, socket.send(Message::Pong(bytes))).await,
                    Ok(Ok(()))
                ) {
                    return ready;
                }
            }
            Ok(Some(Ok(Message::Pong(_)))) => {}
            Ok(Some(Ok(Message::Close(Some(frame))))) if u16::from(frame.code) == 4401 => {
                *rejected = Some(bearer);
                return ready;
            }
            _ => return ready,
        }
    }
}

fn control_change(event: &Value, account: Uuid, workspace: Uuid) -> bool {
    match event["type"].as_str() {
        Some("ready" | "resync_required") => true,
        Some("entity.changed")
            if event["accountId"].as_str() == Some(account.to_string().as_str()) =>
        {
            // Cloud's deprecated `session` duplicate of each event is for older clients.
            event["entity"] == "workspace"
                && event["id"].as_str().and_then(|id| id.parse::<Uuid>().ok()) == Some(workspace)
        }
        _ => false,
    }
}

async fn runtime_connection(
    http: &reqwest::Client,
    attachment: &Attachment,
    thread: Uuid,
    turn: Uuid,
    sender: &watch::Sender<bool>,
) -> bool {
    let Ok(url) = safe_url(&attachment.gateway_url).and_then(|u| {
        u.join("v1/events")
            .map_err(|e| crate::Error::Other(e.to_string()))
    }) else {
        return false;
    };
    let request = http
        .get(url)
        .bearer_auth(&attachment.ticket)
        .header("accept", "text/event-stream");
    let Ok(Ok(mut response)) = tokio::time::timeout(CONNECT, request.send()).await else {
        return false;
    };
    if !response.status().is_success()
        || response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_none_or(|v| v.split(';').next() != Some("text/event-stream"))
    {
        return false;
    }
    tracing::debug!(transport = "sse", %thread, %turn, "Cloud event stream connected");
    sender.send_replace(true);
    let mut parser = Frames::default();
    // Renew before ticket expiry even if an older gateway doesn't close the stream.
    let renewal = (attachment.expires_at - chrono::Utc::now() - chrono::Duration::seconds(15))
        .to_std()
        .unwrap_or_default();
    let renew_at = Instant::now() + renewal;
    loop {
        let chunk =
            tokio::time::timeout_at(renew_at.min(Instant::now() + IDLE), response.chunk()).await;
        match chunk {
            Ok(Ok(Some(chunk))) => {
                let Some(events) = parser.push(&chunk) else {
                    return true;
                };
                if events
                    .iter()
                    .any(|event| runtime_change(event, thread, turn))
                {
                    sender.send_replace(true);
                }
            }
            _ => return true,
        }
    }
}

fn runtime_change(event: &Value, thread: Uuid, turn: Uuid) -> bool {
    let kind = event["type"].as_str().unwrap_or_default();
    if kind == "resync_required" {
        return true;
    }
    let thread = thread.to_string();
    let relevant = event["threadId"].as_str() == Some(&thread)
        || event["rootThreadId"].as_str() == Some(&thread);
    relevant
        && (kind.starts_with("permission.")
            || matches!(kind, "turn.started" | "turn.finished" | "turn.status")
                && event["turnId"].as_str() == Some(turn.to_string().as_str()))
}

/// Bounded, incremental SSE parsing (including split UTF-8, CRLF and multiline data).
#[derive(Default)]
struct Frames {
    line: Vec<u8>,
    data: Vec<u8>,
}
impl Frames {
    fn push(&mut self, bytes: &[u8]) -> Option<Vec<Value>> {
        let mut events = Vec::new();
        for byte in bytes {
            if *byte != b'\n' {
                self.line.push(*byte);
                if self.line.len() + self.data.len() > MAX_RESPONSE {
                    return None;
                }
                continue;
            }
            if self.line.last() == Some(&b'\r') {
                self.line.pop();
            }
            if self.line.is_empty() {
                if !self.data.is_empty() {
                    events.push(serde_json::from_slice(&self.data).ok()?);
                    self.data.clear();
                }
            } else if let Some(data) = self.line.strip_prefix(b"data:") {
                let data = data.strip_prefix(b" ").unwrap_or(data);
                self.data.extend_from_slice(data);
                self.data.push(b'\n');
            }
            self.line.clear();
        }
        Some(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sse_handles_fragmented_unicode_crlf_multiline_and_bounds() {
        let input = "id: 1\r\nevent: turn.finished\r\ndata: {\"type\":\r\ndata: \"turn.finished\",\"text\":\"日本語\"}\r\n\r\n:keepalive\n\n";
        let mut parser = Frames::default();
        let events = input
            .as_bytes()
            .iter()
            .flat_map(|b| parser.push(&[*b]).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(events, [json!({"type":"turn.finished","text":"日本語"})]);
        assert!(parser.push(&vec![b'x'; MAX_RESPONSE + 1]).is_none());
    }

    #[test]
    fn only_scoped_state_changes_wake_waiters() {
        let account = Uuid::now_v7();
        let workspace = Uuid::now_v7();
        let thread = Uuid::now_v7();
        let turn = Uuid::now_v7();
        let mut event = json!({"type":"entity.changed","accountId":account,"entity":"workspace","id":workspace});
        assert!(control_change(&event, account, workspace));
        event["id"] = json!(Uuid::now_v7());
        assert!(!control_change(&event, account, workspace));
        event["id"] = json!(workspace);
        // The deprecated duplicate for older clients wakes nothing on its own.
        event["entity"] = json!("session");
        assert!(!control_change(&event, account, workspace));
        event["entity"] = json!("workspace");
        event["accountId"] = json!(Uuid::now_v7());
        assert!(!control_change(&event, account, workspace));
        let mut event =
            json!({"type":"permission.requested","threadId":Uuid::now_v7(),"rootThreadId":thread});
        assert!(runtime_change(&event, thread, turn));
        event["type"] = json!("item.delta");
        assert!(!runtime_change(&event, thread, turn));
        event["type"] = json!("turn.finished");
        event["turnId"] = json!(turn);
        assert!(runtime_change(&event, thread, turn));
        event["turnId"] = json!(Uuid::now_v7());
        assert!(!runtime_change(&event, thread, turn));
    }
}
