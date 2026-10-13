//! One server, seen from the bridge: the extension host socket that carries
//! the extension's messages in order, the activity it polls while connected,
//! and the few REST calls the extension may ask for. URLs are built from
//! validated path segments; the server's URL and token stay here.
use super::discovery::Server;
use crate::reconnect::{Backoff, Link, Pacing};
use anyhow::{Context as _, Result, bail};
use futures::{SinkExt as _, StreamExt as _};
use reqwest::Url;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest as _};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// What a relay tells the bridge.
pub(crate) enum Event {
    Connected {
        server: String,
    },
    Message {
        server: String,
        message: Value,
    },
    Activity {
        server: String,
        active: Vec<String>,
        approvals: Vec<Value>,
    },
    /// The socket ended or could not open. `problem` is set when retrying
    /// will not help until the server changes.
    Closed {
        server: String,
        problem: Option<String>,
    },
}

#[derive(Clone, Copy)]
pub(crate) struct Timing {
    pub(crate) ping: Duration,
    pub(crate) activity: Duration,
    pub(crate) pacing: Pacing,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            ping: Duration::from_secs(20),
            activity: Duration::from_secs(2),
            // 250 ms doubling to 5 s; a connection up for 30 s starts over.
            pacing: super::discovery::retry_pacing(),
        }
    }
}

/// A running relay: messages for the server, and its task.
pub(crate) struct Relay {
    outgoing: mpsc::UnboundedSender<Value>,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl Relay {
    /// Queues `message` for the server, in order; dropped while it is not connected.
    pub(crate) fn send(&self, message: Value) {
        let _ = self.outgoing.send(message);
    }

    pub(crate) fn stop(self) -> JoinHandle<()> {
        self.cancel.cancel();
        self.task
    }
}

pub(crate) fn spawn(
    server: &Server,
    hello: watch::Receiver<Value>,
    events: mpsc::UnboundedSender<Event>,
    http: reqwest::Client,
    timing: Timing,
    cancel: CancellationToken,
) -> Relay {
    let (outgoing, receiver) = mpsc::unbounded_channel();
    let task = tokio::spawn(run(
        Api::new(server, http),
        server.id.clone(),
        hello,
        receiver,
        events,
        timing,
        cancel.clone(),
    ));
    Relay {
        outgoing,
        cancel,
        task,
    }
}

async fn run(
    api: Api,
    server: String,
    hello: watch::Receiver<Value>,
    mut outgoing: mpsc::UnboundedReceiver<Value>,
    events: mpsc::UnboundedSender<Event>,
    timing: Timing,
    cancel: CancellationToken,
) {
    let mut backoff = Backoff::new(timing.pacing);
    loop {
        let link = tokio::select! {
            () = cancel.cancelled() => return,
            link = api.connect() => link,
        };
        let link = match link {
            Ok(socket) => {
                // Messages queued while disconnected answered an old socket.
                while outgoing.try_recv().is_ok() {}
                let since = tokio::time::Instant::now();
                let ended = connected(
                    &api,
                    &server,
                    socket,
                    &hello,
                    &mut outgoing,
                    &events,
                    timing,
                    &cancel,
                )
                .await;
                if cancel.is_cancelled() {
                    return;
                }
                if let Err(error) = ended {
                    tracing::info!(server = %server, %error, "extension host connection ended");
                }
                let _ = events.send(Event::Closed {
                    server: server.clone(),
                    problem: None,
                });
                Link::Ended {
                    proven: since.elapsed() >= timing.pacing.stable,
                }
            }
            Err(problem) => {
                tracing::info!(server = %server, problem = %problem.message, "could not open the extension host connection");
                let lasting = problem.lasting;
                let _ = events.send(Event::Closed {
                    server: server.clone(),
                    problem: lasting.then_some(problem.message),
                });
                if lasting {
                    // This server answers the same until it changes, which the
                    // bridge sees as a changed record or a new server instance
                    // and answers with a new relay.
                    cancel.cancelled().await;
                    return;
                }
                Link::Failed
            }
        };
        let wait = backoff.wait(link);
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[allow(clippy::too_many_arguments)] // one connection's whole context
async fn connected(
    api: &Api,
    server: &str,
    mut socket: Socket,
    hello: &watch::Receiver<Value>,
    outgoing: &mut mpsc::UnboundedReceiver<Value>,
    events: &mpsc::UnboundedSender<Event>,
    timing: Timing,
    cancel: &CancellationToken,
) -> Result<()> {
    let first = hello.borrow().clone();
    send(&mut socket, &first).await?;
    let _ = events.send(Event::Connected {
        server: server.to_owned(),
    });
    let polling = cancel.child_token();
    let _stop_polling = polling.clone().drop_guard();
    tokio::spawn(poll_activity(
        api.clone(),
        server.to_owned(),
        events.clone(),
        timing.activity,
        polling,
    ));
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + timing.ping, timing.ping);
    // Pongs that answer the bridge's own pings stay here.
    let mut pings = 0_u32;
    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                let _ = tokio::time::timeout(Duration::from_millis(300), socket.close(None)).await;
                return Ok(());
            }
            _ = ping.tick() => {
                send(&mut socket, &json!({"type":"ping"})).await?;
                pings = pings.saturating_add(1);
            }
            message = outgoing.recv() => {
                let Some(message) = message else { return Ok(()) };
                send(&mut socket, &message).await?;
            }
            frame = socket.next() => match frame {
                Some(Ok(Message::Text(text))) => {
                    let message: Value = serde_json::from_str(&text).context("the server sent invalid JSON")?;
                    if message["type"] == "pong" && pings > 0 {
                        pings -= 1;
                        continue;
                    }
                    let _ = events.send(Event::Message { server: server.to_owned(), message });
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
                Some(Ok(Message::Binary(_))) => bail!("the server sent a binary frame"),
                Some(Ok(Message::Close(_))) | None => return Ok(()),
                Some(Err(error)) => return Err(error.into()),
            },
        }
    }
}

async fn send(socket: &mut Socket, message: &Value) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(10),
        socket.send(Message::Text(message.to_string().into())),
    )
    .await
    .context("the server stopped reading")??;
    Ok(())
}

async fn poll_activity(
    api: Api,
    server: String,
    events: mpsc::UnboundedSender<Event>,
    every: Duration,
    cancel: CancellationToken,
) {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            _ = tick.tick() => {}
        }
        let polled = tokio::select! {
            () = cancel.cancelled() => return,
            polled = async { tokio::try_join!(api.active_threads(), api.approvals()) } => polled,
        };
        match polled {
            Ok((active, approvals)) => {
                let _ = events.send(Event::Activity {
                    server: server.clone(),
                    active,
                    approvals,
                });
            }
            Err(error) => tracing::debug!(server = %server, %error, "activity poll failed"),
        }
    }
}

/// A Chrome site gate, `chrome-<category>:<thread>:<origin>`, as a prompt of
/// its own lists it or as a combined prompt does (`browser_site:chrome-…`).
fn chrome_gate(pattern: &str) -> Option<&str> {
    let gate = pattern.strip_prefix("browser_site:").unwrap_or(pattern);
    gate.starts_with("chrome-").then_some(gate)
}

/// The explicit approvals waiting on a Chrome site, with only their Chrome
/// site gates. One that also waits on something else (a file outside the
/// project, say) is `answerable: false`: Chrome shows it, so people know a
/// chat is waiting, but only Haarness may answer it.
pub(crate) fn chrome_approvals(pending: &[Value]) -> Vec<Value> {
    pending
        .iter()
        .filter(|permission| permission["explicit"] == true)
        .filter_map(|permission| {
            let patterns: Vec<&str> = permission["patterns"]
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .collect();
            let gates: Vec<&str> = patterns
                .iter()
                .filter_map(|pattern| chrome_gate(pattern))
                .collect();
            if gates.is_empty() {
                return None;
            }
            Some(json!({
                "id": permission["permissionId"].as_str()?,
                "thread": permission["threadId"].as_str()?,
                "patterns": gates,
                "explicit": true,
                "answerable": gates.len() == patterns.len(),
            }))
        })
        .collect()
}

/// Why opening the socket failed; `lasting` when retrying will not help
/// until the server changes.
pub(crate) struct Problem {
    pub(crate) message: String,
    pub(crate) lasting: bool,
}

/// A server's REST API and extension host socket.
#[derive(Clone)]
pub(crate) struct Api {
    http: reqwest::Client,
    base: Url,
    token: String,
}

const REST_TIMEOUT: Duration = Duration::from_secs(5);

impl Api {
    pub(crate) fn new(server: &Server, http: reqwest::Client) -> Self {
        Self {
            http,
            base: server.base.clone(),
            token: server.token.clone(),
        }
    }

    fn url(&self, segments: &[&str]) -> Url {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .expect("a loopback http URL has a path")
            .clear()
            .push("v1")
            .extend(segments);
        url
    }

    async fn connect(&self) -> Result<Socket, Problem> {
        let mut url = self.url(&["browser", "extension", "host"]);
        url.set_query(Some("version=1"));
        url.set_scheme("ws").expect("http and ws share a URL shape");
        let mut request = url.as_str().into_client_request().map_err(|_| Problem {
            message: "invalid server address".into(),
            lasting: true,
        })?;
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", self.token)
                .parse()
                .map_err(|_| Problem {
                    message: "invalid server token".into(),
                    lasting: true,
                })?,
        );
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(32 * 1024 * 1024))
            .max_frame_size(Some(32 * 1024 * 1024));
        let attempt = tokio::time::timeout(
            Duration::from_secs(5),
            tokio_tungstenite::connect_async_with_config(request, Some(config), false),
        )
        .await;
        match attempt {
            Ok(Ok((socket, _))) => Ok(socket),
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response))) => {
                let status = response.status().as_u16();
                Err(Problem {
                    message: match status {
                        403 => "this project's Haarness server does not accept Chrome; restart it without --anonymous".into(),
                        _ => "restart this project's Haarness server".into(),
                    },
                    lasting: matches!(status, 400 | 401 | 403 | 404 | 405 | 501),
                })
            }
            Ok(Err(_)) => Err(Problem {
                message: "not reachable".into(),
                lasting: false,
            }),
            Err(_) => Err(Problem {
                message: "did not answer in time".into(),
                lasting: false,
            }),
        }
    }

    async fn request(&self, request: reqwest::RequestBuilder) -> Result<Option<Value>> {
        let response = tokio::time::timeout(REST_TIMEOUT, request.bearer_auth(&self.token).send())
            .await
            .context("Haarness did not answer in time")?
            .map_err(|_| anyhow::anyhow!("Haarness is not reachable"))?;
        let status = response.status();
        let body = tokio::time::timeout(REST_TIMEOUT, response.bytes())
            .await
            .context("Haarness did not answer in time")?
            .map_err(|_| anyhow::anyhow!("Haarness's answer was cut off"))?;
        let value = serde_json::from_slice::<Value>(&body).ok();
        if !status.is_success() {
            let message = value
                .as_ref()
                .and_then(|value| value["message"].as_str())
                .map_or_else(|| format!("Haarness answered {status}"), str::to_owned);
            bail!(message);
        }
        Ok(value)
    }

    async fn get(&self, segments: &[&str]) -> Result<Value> {
        self.request(self.http.get(self.url(segments)))
            .await?
            .context("Haarness's answer is not JSON")
    }

    pub(crate) async fn active_threads(&self) -> Result<Vec<String>> {
        let turns = self.get(&["turns", "active"]).await?;
        let mut threads: Vec<String> = turns
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|turn| turn["threadId"].as_str().map(str::to_owned))
            .collect();
        threads.sort();
        threads.dedup();
        Ok(threads)
    }

    pub(crate) async fn approvals(&self) -> Result<Vec<Value>> {
        let pending = self.get(&["permissions"]).await?;
        Ok(chrome_approvals(
            pending.as_array().map_or(&[], Vec::as_slice),
        ))
    }

    pub(crate) async fn thread(&self, id: Uuid) -> Result<Value> {
        self.get(&["threads", &id.to_string()]).await
    }

    /// Stops the chat `id` belongs to: a subagent's chat is its parent's.
    pub(crate) async fn stop(&self, id: Uuid) -> Result<()> {
        let mut root = id;
        for _ in 0..32 {
            let thread = self.thread(root).await?;
            let parent = thread["isSubagent"]
                .as_bool()
                .unwrap_or(false)
                .then(|| {
                    thread["parentId"]
                        .as_str()
                        .and_then(|parent| Uuid::parse_str(parent).ok())
                })
                .flatten();
            match parent {
                Some(parent) => root = parent,
                None => break,
            }
        }
        let mut url = self.url(&["threads", &root.to_string(), "interrupt"]);
        url.set_query(Some("resume=true"));
        self.request(self.http.post(url).json(&json!({}))).await?;
        Ok(())
    }

    pub(crate) async fn answer(&self, permission: Uuid, decision: &str) -> Result<()> {
        self.request(
            self.http
                .post(self.url(&["permissions", &permission.to_string()]))
                .json(&json!({"decision": decision})),
        )
        .await?;
        Ok(())
    }

    pub(crate) async fn hosts(&self) -> Result<Value> {
        self.get(&["browser", "hosts"]).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_chrome_approvals_reach_chrome() {
        let pending = [
            json!({"permissionId":"019a0000-0000-7000-8000-00000000000a","threadId":"t1","permission":"browser_site",
                "patterns":["chrome-browsing:t1:https://mail.example.com"],"explicit":true,"operation":"browser_site:chrome-browsing:t1:https://mail.example.com"}),
            json!({"permissionId":"b","threadId":"t1","permission":"bash","patterns":["git push"],"explicit":false}),
            json!({"permissionId":"c","threadId":"t1","permission":"browser_site","patterns":["https://example.com"]}),
            // An upload outside the project: the Chrome gate and a file gate in one prompt.
            json!({"permissionId":"d","threadId":"t2","permission":"multiple","patterns":[
                "browser_site:chrome-uploads:t2:https://a.example",
                "external_directory:/Users/a/Documents/*",
                "read:/Users/a/Documents/cv.pdf"],"explicit":true}),
            // Two Chrome gates combined are still Chrome's to answer.
            json!({"permissionId":"e","threadId":"t2","permission":"multiple","patterns":[
                "browser_site:chrome-browsing:t2:https://b.example","browser_site:chrome-uploads:t2:https://b.example"],"explicit":true}),
            json!({"permissionId":"f","threadId":"t2","permission":"bash","patterns":["bash:x"],"explicit":true}),
        ];
        assert_eq!(
            chrome_approvals(&pending),
            [
                json!({"id":"019a0000-0000-7000-8000-00000000000a","thread":"t1",
                    "patterns":["chrome-browsing:t1:https://mail.example.com"],"explicit":true,"answerable":true}),
                // Only the Chrome site reaches Chrome, never the file paths.
                json!({"id":"d","thread":"t2","patterns":["chrome-uploads:t2:https://a.example"],
                    "explicit":true,"answerable":false}),
                json!({"id":"e","thread":"t2",
                    "patterns":["chrome-browsing:t2:https://b.example","chrome-uploads:t2:https://b.example"],
                    "explicit":true,"answerable":true}),
            ]
        );
    }

    #[test]
    fn rest_urls_are_built_from_segments() {
        let base = Url::parse("http://127.0.0.1:4096/").unwrap();
        let server = Server {
            id: "s".into(),
            workspace: "/w".into(),
            version: "0".into(),
            base: base.clone(),
            token: "t".into(),
            capable: true,
            record: std::sync::Arc::new(super::super::discovery::Record {
                file: "/c/workspaces/w/runtime.json".into(),
                discovery: crate::rendezvous::RuntimeDiscovery {
                    url: "http://127.0.0.1:4096".into(),
                    token: "t".into(),
                    pid: 1,
                    workspace: Some("/w".into()),
                    executable_fingerprint: None,
                    access_fingerprint: None,
                    trace_endpoint: None,
                },
                base,
            }),
        };
        let api = Api::new(&server, reqwest::Client::new());
        assert_eq!(
            api.url(&["threads", "../../permissions/x?y", "interrupt"])
                .as_str(),
            "http://127.0.0.1:4096/v1/threads/..%2F..%2Fpermissions%2Fx%3Fy/interrupt"
        );
        assert_eq!(
            api.url(&["turns", "active"]).as_str(),
            "http://127.0.0.1:4096/v1/turns/active"
        );
    }
}
