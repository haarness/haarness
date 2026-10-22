//! Outbound Cloud connector. Local runtime and provider credentials never cross the relay.
use super::{
    Paths, ensure_workspace_server_for_remote,
    reconnect::{Backoff, Link, Outage, Pacing},
    remote_attention,
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use clap::{Args, Subcommand};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path, sync::Arc, time::Duration};
use tokio::{
    sync::{Semaphore, mpsc},
    task::JoinSet,
};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use uuid::Uuid;

const CHUNK: usize = 32 * 1024;
const WINDOW: usize = 8;
const MAX_BODY: usize = 16 * 1024 * 1024;
/// Scopes a request to one published chat tree (`haarness-server`'s
/// `remote_scope`): the server answers only for that chat and its children.
pub(crate) const REMOTE_THREAD_HEADER: &str = "x-haarness-remote-thread";
#[derive(Args)]
pub struct RemoteArgs {
    #[command(subcommand)]
    command: RemoteCommand,
}
#[derive(Subcommand)]
enum RemoteCommand {
    /// Connect this machine to Cloud. Keep running under a service supervisor for unattended use.
    Run {
        /// Name Cloud shows for this computer.
        #[arg(long, default_value = "My computer")]
        name: String,
        /// Publish only this chat and its child chats, instead of host-wide project access.
        #[arg(long = "chat")]
        session: Option<Uuid>,
    },
    /// Start a background connector for this login. Does not install an OS startup service.
    Enable {
        /// Name Cloud shows for this computer.
        #[arg(long, default_value = "My computer")]
        name: String,
        /// Publish only this chat and its child chats, instead of host-wide project access.
        #[arg(long = "chat")]
        session: Option<Uuid>,
    },
    /// Show local connector IDs and whether their processes are running.
    Status {
        /// Print the connectors as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Stop a local connector without revoking its registration or stopping local work.
    Disable {
        /// Connector id, as `remote status` shows it.
        id: Uuid,
    },
    /// Forget a stopped connector identity so the next enable registers new access.
    Forget {
        /// Connector id, as `remote status` shows it.
        id: Uuid,
    },
    /// List machines connected to the selected Cloud account.
    Hosts,
    /// List explicitly published chats.
    Sessions,
    /// Revoke Cloud access without deleting any local files or conversations.
    Revoke {
        /// Connector id, as `remote status` shows it.
        id: Uuid,
    },
}
// Kept wire-compatible with cloud-contracts::remote::RelayFrame; protocol fixtures test both ends.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Frame {
    Ready {
        version: u32,
        /// Optional frames this relay accepts, such as `attention.v1`. A
        /// relay that does not list one drops the socket when it arrives.
        #[serde(default)]
        features: Vec<String>,
    },
    CatalogChanged,
    /// What waits on the person here, sent only to a relay announcing
    /// `attention.v1` (see `remote_attention`).
    Attention {
        revision: u64,
        items: Vec<remote_attention::Item>,
        finished: Vec<remote_attention::Finished>,
        /// The chats still working, only for a relay announcing
        /// `attention.working`: one without it drops a frame with this.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working: Option<Vec<Uuid>>,
    },
    Open {
        id: Uuid,
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: String,
        websocket: bool,
        thread: Option<Uuid>,
    },
    Head {
        id: Uuid,
        status: u16,
        headers: Vec<(String, String)>,
    },
    Data {
        id: Uuid,
        data: String,
        #[serde(default)]
        text: bool,
    },
    End {
        id: Uuid,
    },
    Error {
        id: Uuid,
        message: String,
    },
    Cancel {
        id: Uuid,
    },
    Ack {
        id: Uuid,
    },
}
#[derive(Clone)]
struct Target {
    url: String,
    token: String,
    project: String,
    thread: Option<Uuid>,
}
struct Active {
    cancel: tokio_util::sync::CancellationToken,
    credit: Arc<Semaphore>,
    input: mpsc::Sender<Frame>,
}
pub async fn run(paths: &Paths, workspace: &Path, trust: bool, args: RemoteArgs) -> Result<()> {
    let client = super::account::client()?;
    match args.command {
        RemoteCommand::Forget { id } => {
            let file =
                local_file(paths, id)?.context("This host has no local connector identity")?;
            ensure!(
                !running(&file)?,
                "Disable this connector before forgetting it"
            );
            client
                .request(
                    reqwest::Method::DELETE,
                    &format!("/api/v1/remote-hosts/{id}"),
                    None,
                )
                .await
                .or_else(|error| match error {
                    haarness_account::Error::Api { status: 404, .. } => Ok(Value::Null),
                    other => Err(other),
                })?;
            std::fs::remove_file(file)?;
            println!(
                "Remote access revoked and local identity forgotten. The next enable registers a new host."
            );
            return Ok(());
        }
        RemoteCommand::Status { json } => {
            let connectors = local_status(paths)?;
            if json {
                println!("{}", json!({"items": connectors}));
            } else if connectors.is_empty() {
                println!(
                    "No remote connectors on this computer. Start one with `haarness remote enable`."
                );
            } else {
                for connector in connectors {
                    print!("{}", connector_line(&connector));
                }
            }
            return Ok(());
        }
        RemoteCommand::Disable { id } => {
            let file =
                local_file(paths, id)?.context("This host has no local connector identity")?;
            std::fs::write(file.with_extension("stop"), "stop")?;
            for _ in 0..150 {
                if !running(&file)? {
                    println!("Connector stopped. Local work continues.");
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            anyhow::bail!("Connector did not stop within 15 seconds; inspect its log")
        }
        RemoteCommand::Enable { name, session } => {
            let status = client.status().await?;
            let account = status["selectedAccountId"]
                .as_str()
                .context("Sign in to Haarness to continue. Run `haarness login`.")?;
            let file = identity_file(paths, &client, account, workspace, session);
            if running(&file)? {
                println!("Connector is already running");
                return Ok(());
            }
            // Enabling is the person asking to connect this computer again: if
            // Cloud removed its earlier registration (Remove access), register
            // a new one instead of failing on the old identity.
            let earlier = std::fs::read(&file)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .and_then(|identity| identity["id"].as_str().map(str::to_owned));
            if let Some(id) = earlier {
                let removed = matches!(
                    client
                        .request(
                            reqwest::Method::GET,
                            &format!("/api/v1/remote-hosts/{id}"),
                            None
                        )
                        .await,
                    Err(haarness_account::Error::Api { status: 404, .. })
                );
                if removed {
                    std::fs::remove_file(&file)?;
                    println!(
                        "This computer was removed from Haarness Cloud; registering it again."
                    );
                }
            }
            let log = file.with_extension("log");
            spawn_connector(paths, workspace, &file, &name, session)
                .await
                .map_err(|error| anyhow::anyhow!("{error}; inspect {}", log.display()))?;
            println!(
                "Connector started. Use `haarness remote status` for its host ID. Log: {}",
                log.display()
            );
            return Ok(());
        }
        RemoteCommand::Hosts => {
            println!(
                "{}",
                super::account::list_all(
                    &client,
                    &format!("/api/v1/remote-hosts{}", account_query(&client))
                )
                .await?
            );
            return Ok(());
        }
        RemoteCommand::Sessions => {
            println!(
                "{}",
                super::account::list_all(
                    &client,
                    &format!("/api/v1/remote-sessions{}", account_query(&client))
                )
                .await?
            );
            return Ok(());
        }
        RemoteCommand::Revoke { id } => {
            client
                .request(
                    reqwest::Method::DELETE,
                    &format!("/api/v1/remote-hosts/{id}"),
                    None,
                )
                .await?;
            println!("Remote access revoked. Local files and chats are retained.");
            return Ok(());
        }
        RemoteCommand::Run { name, session } => {
            let status = client.status().await?;
            let account = status["selectedAccountId"]
                .as_str()
                .context("Sign in and select a Cloud account first")?;
            let discovery = ensure_workspace_server_for_remote(paths, workspace, trust).await?;
            let project = paths
                .workspace_id
                .clone()
                .context("Workspace is not registered")?;
            let mut target = Target {
                url: discovery.url,
                token: discovery.token,
                project,
                thread: session,
            };
            let http = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .build()?;
            verify_runtime(&http, &target).await?;
            let title = if let Some(thread) = session {
                let response = http
                    .get(format!("{}/v1/threads/{thread}", target.url))
                    .bearer_auth(&target.token)
                    .send()
                    .await?
                    .error_for_status()?
                    .json::<Value>()
                    .await?;
                Some(
                    response["title"]
                        .as_str()
                        .unwrap_or("Remote chat")
                        .to_owned(),
                )
            } else {
                None
            };
            let mode = if session.is_some() {
                "sessions"
            } else {
                "host"
            };
            let file = identity_file(paths, &client, account, workspace, session);
            std::fs::create_dir_all(paths.config_dir())?;
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(file.with_extension("lock"))?;
            fs2::FileExt::try_lock_exclusive(&lock)
                .context("This remote connector is already running")?;
            let id = match std::fs::read_to_string(&file) {
                Ok(value) => serde_json::from_str::<Value>(&value)?["id"]
                    .as_str()
                    .context("Invalid remote identity")?
                    .parse::<Uuid>()?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Uuid::now_v7(),
                Err(error) => return Err(error.into()),
            };
            // Keep what an earlier run learned (the publication) so status
            // can show the chat's link while this run reconnects.
            let mut identity = std::fs::read(&file)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .filter(Value::is_object)
                .unwrap_or_else(|| json!({}));
            for (key, value) in [
                ("id", json!(id)),
                ("workspace", json!(workspace)),
                ("chatId", json!(session)),
                ("accountId", json!(account)),
                ("name", json!(name)),
                ("apiUrl", json!(client.config.api_url)),
                ("pid", json!(std::process::id())),
            ] {
                identity[key] = value;
            }
            // Why an earlier run gave up no longer applies once this one starts.
            if let Some(fields) = identity.as_object_mut() {
                fields.remove("error");
            }
            super::write_private_json(&file, &identity)?;
            let stop_file = file.with_extension("stop");
            if stop_file.exists() {
                std::fs::remove_file(&stop_file)?;
            }
            client.request(reqwest::Method::POST,"/api/v1/remote-hosts",Some(json!({"id":id,"accountId":account,"name":name,"mode":mode,"metadata":{"platform":std::env::consts::OS,"version":env!("CARGO_PKG_VERSION")}}))).await?;
            if let Some(thread) = session {
                let published = client
                    .request(
                        reqwest::Method::POST,
                        &format!("/api/v1/remote-hosts/{id}/sessions"),
                        Some(json!({"projectId":target.project,"threadId":thread,"title":title})),
                    )
                    .await?;
                let publication = published["id"].as_str().unwrap_or_default();
                identity["publicationId"] = json!(publication);
                identity["title"] = published["title"].clone();
                super::write_private_json(&file, &identity)?;
                println!("Published chat {publication}");
                if let Ok(url) = chat_url(&client, publication) {
                    println!("Open it from any browser: {url}");
                }
            }
            println!("Remote host {id}. Connecting to {}", client.config.api_url);
            let stop = tokio_util::sync::CancellationToken::new();
            let signal = stop.clone();
            tokio::spawn(async move {
                let _ = super::wait_for_shutdown(signal.clone()).await;
                signal.cancel();
            });
            let local_stop = stop.clone();
            let watcher = tokio::spawn(async move {
                loop {
                    tokio::select! {()=local_stop.cancelled()=>break,()=tokio::time::sleep(Duration::from_millis(200))=>{if stop_file.exists(){local_stop.cancel();break}}}
                }
            });
            let _watcher = AbortTask(watcher);
            let mut shown_title = identity["title"].as_str().unwrap_or_default().to_owned();
            let attention =
                remote_attention::Context::new(paths, trust, remote_attention::Timing::default());
            let mut relay = client
                .config
                .api_url
                .join(&format!("api/v1/remote-hosts/{id}/connect"))?;
            relay
                .set_scheme(if relay.scheme() == "https" {
                    "wss"
                } else {
                    "ws"
                })
                .map_err(|()| anyhow::anyhow!("Invalid Cloud URL"))?;
            let mut reconnect = Reconnect::new(Pacing::default(), |line| eprintln!("{line}"));
            // The local server is waited for on its own pacing; reaching it
            // costs Cloud nothing.
            let mut local = Backoff::new(LOCAL_PACING);
            let ended: Result<()> = async {
                loop {
                    // Daemons can be replaced by a local client upgrade or restarted after a crash.
                    // Resolve the current address/token before each transport generation.
                    let runtime = tokio::select! {
                        () = stop.cancelled() => break,
                        result = local_runtime(paths, workspace, trust, &http, &mut target) => result,
                    };
                    if let Err(error) = runtime {
                        if error.downcast_ref::<OutdatedRuntime>().is_some() {
                            return Err(error);
                        }
                        reconnect
                            .outage
                            .failed(&format!("Waiting for the local runtime: {error}"));
                        let delay = local.wait(Link::Failed);
                        tokio::select! {()=stop.cancelled()=>break,()=tokio::time::sleep(delay)=>{}}
                        continue;
                    }
                    local = Backoff::new(LOCAL_PACING);
                    let link = tokio::select! {
                        () = stop.cancelled() => break,
                        // Boxed: a whole relay connection lives in this future.
                        link = Box::pin(reconnect_step(&client, &relay, &target, &http, &attention, &mut reconnect)) => link?,
                        () = follow_title(&client, &http, &target, id, &file, &mut shown_title) => continue,
                    };
                    let delay = reconnect.backoff.wait(link);
                    tokio::select! {()=stop.cancelled()=>break,()=tokio::time::sleep(delay)=>{}}
                }
                Ok(())
            }
            .await;
            if let Err(error) = ended {
                // The process ends here; status reads why from the identity.
                record_exit(&file, &error);
                return Err(error);
            }
            println!("Remote connector stopped. Local work continues.");
        }
    }
    Ok(())
}
/// Keeps a shared chat's name in Cloud in step with the chat. A chat shared
/// right after its first message is still "New chat" when published; its
/// real title arrives later, and a rename can come at any time.
async fn follow_title(
    client: &haarness_account::Client,
    http: &reqwest::Client,
    target: &Target,
    host: Uuid,
    identity: &Path,
    shown: &mut String,
) {
    let Some(thread) = target.thread else {
        return std::future::pending().await;
    };
    loop {
        tokio::time::sleep(Duration::from_secs(10)).await;
        let current = async {
            http.get(format!("{}/v1/threads/{thread}", target.url))
                .bearer_auth(&target.token)
                .send()
                .await?
                .error_for_status()?
                .json::<Value>()
                .await
        }
        .await;
        let Some(title) = current
            .ok()
            .and_then(|thread| {
                thread["title"]
                    .as_str()
                    .map(|title| title.trim().chars().take(200).collect::<String>())
            })
            .filter(|title| !title.is_empty() && title != shown)
        else {
            continue;
        };
        let published = client
            .request(
                reqwest::Method::POST,
                &format!("/api/v1/remote-hosts/{host}/sessions"),
                Some(json!({"projectId": target.project, "threadId": thread, "title": title})),
            )
            .await;
        if published.is_ok() {
            if let Some(mut saved) = std::fs::read(identity)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            {
                saved["title"] = json!(title);
                let _ = super::write_private_json(identity, &saved);
            }
            *shown = title;
        }
    }
}
async fn verify_runtime(http: &reqwest::Client, target: &Target) -> Result<()> {
    let info = tokio::time::timeout(Duration::from_secs(5), async {
        http.get(format!("{}/v1/info", target.url))
            .bearer_auth(&target.token)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await
    })
    .await
    .context("This project's Haarness server did not answer in time")??;
    if !info["capabilities"]
        .as_array()
        .is_some_and(|values| values.iter().any(|v| v == "remote-chat-scope.v1"))
    {
        return Err(OutdatedRuntime.into());
    }
    Ok(())
}
/// This project's server is from a build that cannot confine a published
/// chat's requests; waiting does not change that, only restarting it does.
#[derive(Debug)]
struct OutdatedRuntime;
impl std::fmt::Display for OutdatedRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Restart this project's Haarness server with the current version before enabling remote access")
    }
}
impl std::error::Error for OutdatedRuntime {}
/// Waiting for the local server: 1 s, doubling to 30 s.
const LOCAL_PACING: Pacing = Pacing {
    retry: Duration::from_secs(1),
    retry_max: Duration::from_secs(30),
    recover_for: Duration::ZERO,
    recover_max: Duration::from_secs(30),
    stable: Duration::from_secs(60),
    drops: u32::MAX,
    cooldown: Duration::from_secs(30),
    cooldown_max: Duration::from_secs(30),
};
/// Find this project's server (starting it when none runs) and check it can
/// serve the relay. A server that is starting, being replaced or too busy to
/// answer is waited for; only an [`OutdatedRuntime`] ends the connector.
async fn local_runtime(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    http: &reqwest::Client,
    target: &mut Target,
) -> Result<()> {
    let discovery = ensure_workspace_server_for_remote(paths, workspace, trust).await?;
    target.url = discovery.url;
    target.token = discovery.token;
    verify_runtime(http, target).await
}
/// What a connector carries from one attempt to reach the relay to the next.
struct Reconnect {
    backoff: Backoff,
    outage: Outage,
    /// A bearer the relay refused, renewed before the next attempt.
    rejected: Option<String>,
}
impl Reconnect {
    fn new(pacing: Pacing, say: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self {
            backoff: Backoff::new(pacing),
            outage: Outage::new(say),
            rejected: None,
        }
    }
}
/// One attempt to reach the relay once the local server answers: renew the
/// bearer when it expired or was refused, connect, and serve until the
/// connection ends. What waiting can fix (Cloud unreachable or overloaded, or
/// a proxy answering for it) is a failed link, said once per outage; only an
/// answer no retry changes (signed out, access revoked, a protocol this build
/// does not speak) ends the connector.
async fn reconnect_step(
    client: &haarness_account::Client,
    relay: &reqwest::Url,
    target: &Target,
    http: &reqwest::Client,
    attention: &Arc<remote_attention::Context>,
    state: &mut Reconnect,
) -> Result<Link> {
    let bearer = match client.bearer(state.rejected.as_deref()).await {
        Ok(bearer) => bearer,
        Err(error) if error.is_transient() => {
            state.outage.failed(&format!(
                "Could not reach Haarness Cloud to renew this computer's sign-in: {}; reconnecting",
                renewal_failure(&error)
            ));
            return Ok(Link::Failed);
        }
        Err(error) => return Err(error.into()),
    };
    let reauthorized = state.rejected.take().is_some();
    let link = relay_link(
        relay_handshake(relay, &bearer)?,
        target,
        http,
        state.backoff.pacing.stable,
        RELAY_SILENCE,
        attention,
        &mut state.outage,
    )
    .await?;
    if link == Link::Unauthorized {
        // One retry with a fresh token; a second refusal will not pass either.
        ensure!(
            !reauthorized,
            "Remote authentication rejected; sign in again or replace the expired/revoked API key"
        );
        state.rejected = Some(bearer);
    }
    Ok(link)
}
/// Why renewing the sign-in failed, without repeating "Could not reach".
fn renewal_failure(error: &haarness_account::Error) -> String {
    let haarness_account::Error::Http(error) = error else {
        return error.to_string();
    };
    // reqwest names the request; the reason (refused, reset, DNS) is below it.
    std::iter::successors(Some(error as &(dyn std::error::Error + 'static)), |error| {
        error.source()
    })
    .map(ToString::to_string)
    .collect::<Vec<_>>()
    .join(": ")
}
/// The relay announced a protocol this build does not speak; reconnecting
/// cannot fix that, only an update can.
#[derive(Debug)]
struct Unsupported(u64);
impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Haarness Cloud now uses remote protocol version {}, which this version of Haarness does not support. Update Haarness to use remote control again.",
            self.0
        )
    }
}
impl std::error::Error for Unsupported {}
/// The connection ended from this side for a reason that says nothing about
/// the relay: it stayed silent past the heartbeat, did not take a frame in
/// time (the network stalled), or the local server changed. Before the
/// connection proved itself this is a failed attempt, not a drop, so a flaky
/// network never earns the long waits meant for a relay that keeps dropping.
#[derive(Debug)]
struct Interrupted(&'static str);
impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Interrupted {}
/// How long the relay may stay silent before the connection counts as
/// stalled. The connector pings five times in that period, and Cloud answers
/// each ping.
const RELAY_SILENCE: Duration = Duration::from_secs(25);
/// How long one frame may take to leave; a stalled network fills the socket's
/// buffer and the send then waits for as long as TCP keeps retrying.
const RELAY_SEND: Duration = Duration::from_secs(10);
/// How long a new connection may wait for the relay's `ready`.
const RELAY_READY: Duration = Duration::from_secs(10);
const RELAY_VERSION: u64 = 1;
/// The upgrade header that names the relay features this connector
/// understands, comma-separated. Cloud lists features in `ready` only when
/// asked, because connectors built before features parse `ready` strictly.
const RELAY_FEATURES_HEADER: &str = "x-haarness-relay-features";
/// The relay's WebSocket upgrade: the bearer, and the features to offer.
fn relay_handshake(
    url: &reqwest::Url,
    bearer: &str,
) -> Result<tokio_tungstenite::tungstenite::handshake::client::Request> {
    let mut handshake = url.as_str().into_client_request()?;
    let headers = handshake.headers_mut();
    headers.insert("authorization", format!("Bearer {bearer}").parse()?);
    headers.insert(
        RELAY_FEATURES_HEADER,
        format!(
            "{}, {}",
            remote_attention::FEATURE,
            remote_attention::WORKING_FEATURE
        )
        .parse()?,
    );
    Ok(handshake)
}
/// The version and optional features a relay's first frame announces, when
/// it is a `ready` frame. Read loosely: a newer relay may say more.
fn relay_ready(frame: &str) -> Option<(u64, Vec<String>)> {
    let frame = serde_json::from_str::<Value>(frame).ok()?;
    if frame["type"] != "ready" {
        return None;
    }
    let features = frame["features"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|feature| feature.as_str().map(str::to_owned))
        .collect();
    Some((frame["version"].as_u64()?, features))
}
/// One connection to the relay, from the handshake until it ends. Only an
/// answer that reconnecting cannot change is an error. During an outage the
/// failures go to `outage`, and the first connection that proves itself says
/// how many attempts it took.
async fn relay_link(
    handshake: tokio_tungstenite::tungstenite::handshake::client::Request,
    target: &Target,
    http: &reqwest::Client,
    stable: Duration,
    silence: Duration,
    attention: &Arc<remote_attention::Context>,
    outage: &mut Outage,
) -> Result<Link> {
    let attempt = tokio::time::timeout(
        Duration::from_secs(10),
        haarness_account::websocket::connect(
            handshake,
            Some(
                tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                    .max_message_size(Some(24 * 1024 * 1024))
                    .max_frame_size(Some(24 * 1024 * 1024)),
            ),
        ),
    );
    let socket = match attempt.await {
        Ok(Ok((socket, _))) => socket,
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response)))
            if response.status() == reqwest::StatusCode::UNAUTHORIZED =>
        {
            return Ok(Link::Unauthorized);
        }
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response)))
            if matches!(response.status().as_u16(), 403 | 404) =>
        {
            match refusal(&response) {
                Some(reason) => {
                    anyhow::bail!("Haarness Cloud refused this computer's connection: {reason}")
                }
                None => anyhow::bail!("Remote host access was revoked or is no longer allowed"),
            }
        }
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response))) => {
            outage.failed(&match refusal(&response) {
                Some(reason) => format!(
                    "Haarness Cloud refused the connection ({}): {reason}; reconnecting",
                    response.status()
                ),
                None => format!(
                    "Could not reach Haarness Cloud: HTTP error: {}; reconnecting",
                    response.status()
                ),
            });
            return Ok(Link::Failed);
        }
        Ok(Err(error)) => {
            outage.failed(&format!(
                "Could not reach Haarness Cloud: {error}; reconnecting"
            ));
            return Ok(Link::Failed);
        }
        Err(_) => {
            outage.failed("Haarness Cloud did not answer in time; reconnecting");
            return Ok(Link::Failed);
        }
    };
    // During an outage the connection says so once it proves itself.
    if !outage.ongoing() {
        outage.say("Remote host connected");
    }
    let since = tokio::time::Instant::now();
    let (served, mut requests) = tokio::sync::watch::channel(false);
    let session = connected(
        socket,
        target.clone(),
        http.clone(),
        served,
        attention.clone(),
        silence,
    );
    tokio::pin!(session);
    let proof = async {
        tokio::select! {
            () = tokio::time::sleep(stable) => {}
            // A request routed here proves it at once. The sender goes with
            // the connection, which then ends this select anyway.
            served = async { requests.wait_for(|served| *served).await.map(|_| ()) } => {
                if served.is_err() {
                    std::future::pending::<()>().await;
                }
            }
        }
    };
    let mut announced = false;
    let ended = tokio::select! {
        ended = &mut session => ended,
        () = proof => {
            announced = true;
            reconnected(outage);
            session.await
        }
    };
    let proven = announced || *requests.borrow() || since.elapsed() >= stable;
    if proven && !announced {
        reconnected(outage);
    }
    let interrupted = matches!(&ended, Err(error) if error.is::<Interrupted>());
    match ended {
        Err(error) if error.is::<Unsupported>() => return Err(error),
        // A connection that worked ending is news; one that never proved
        // itself is another failed attempt of the outage.
        Err(error) if proven => outage.say(&format!("Remote connection ended: {error}")),
        Err(error) => outage.failed(&format!("Remote connection ended: {error}; reconnecting")),
        Ok(()) if proven => {}
        Ok(()) => outage.failed("Remote connection ended; reconnecting"),
    }
    if interrupted && !proven {
        return Ok(Link::Failed);
    }
    Ok(Link::Ended { proven })
}
/// What Cloud said when it refused the relay's upgrade: its error's
/// `message`, on one line and clipped, so the connector's output and
/// `remote status` can name the reason. Nothing when it said nothing.
fn refusal(
    response: &tokio_tungstenite::tungstenite::http::Response<Option<Vec<u8>>>,
) -> Option<String> {
    let body: Value = serde_json::from_slice(response.body().as_deref()?).ok()?;
    let message = body["message"]
        .as_str()
        .or_else(|| body["error"]["message"].as_str())?;
    let message: String = message
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(200)
        .collect();
    let message = message.trim();
    (!message.is_empty()).then(|| message.to_owned())
}
/// A connection proved itself: say how long the outage before it lasted.
fn reconnected(outage: &mut Outage) {
    if let Some((attempts, took)) = outage.recovered() {
        let took = if took < Duration::from_secs(120) {
            format!("{} s", took.as_secs())
        } else {
            format!("{} min", took.as_secs() / 60)
        };
        outage.say(&format!(
            "Remote host reconnected after {attempts} attempts in {took}"
        ));
    }
}
/// Keep why the connector gave up in its identity, so status can say so
/// after the process is gone.
fn record_exit(file: &Path, error: &anyhow::Error) {
    if let Some(mut identity) = std::fs::read(file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(Value::is_object)
    {
        identity["error"] = json!(error.to_string());
        let _ = super::write_private_json(file, &identity);
    }
}
async fn connected(
    mut socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    target: Target,
    http: reqwest::Client,
    served: tokio::sync::watch::Sender<bool>,
    attention: Arc<remote_attention::Context>,
    silence: Duration,
) -> Result<()> {
    // The relay has as long to say it is ready as it may stay silent later,
    // at most 10 s; a stall right after connecting is no drop either.
    let ready = tokio::time::timeout(silence.min(RELAY_READY), socket.next())
        .await
        .map_err(|_| Interrupted("Relay did not say it was ready in time"))?
        .context("Relay closed")??;
    let ready = match &ready {
        Message::Text(value) => relay_ready(value),
        _ => None,
    };
    let features = match ready {
        Some((RELAY_VERSION, features)) => features,
        Some((version, _)) => return Err(Unsupported(version).into()),
        None => anyhow::bail!("Relay did not announce its protocol"),
    };
    let (send, mut outgoing) = mpsc::channel::<Frame>(128);
    // An older relay drops the socket on a frame it does not know, and one
    // before `attention.working` drops a frame that lists working chats.
    let working = features
        .iter()
        .any(|feature| feature == remote_attention::WORKING_FEATURE);
    let _attention = features
        .iter()
        .any(|feature| feature == remote_attention::FEATURE)
        .then(|| {
            let scope = match target.thread {
                Some(thread) => remote_attention::Scope::Chat {
                    url: target.url.clone(),
                    token: target.token.clone(),
                    project: target.project.clone(),
                    thread,
                },
                None => remote_attention::Scope::Host,
            };
            AbortTask(tokio::spawn(remote_attention::watch(
                attention,
                scope,
                send.clone(),
                working,
            )))
        });
    let runtime_lost = tokio_util::sync::CancellationToken::new();
    let health = runtime_lost.clone();
    let health_target = target.clone();
    let health_http = http.clone();
    let health_send = send.clone();
    let _health = AbortTask(tokio::spawn(async move {
        let mut previous = None;
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tick.tick().await;
            if verify_runtime(&health_http, &health_target).await.is_err() {
                health.cancel();
                break;
            }
            if health_target.thread.is_some() {
                continue;
            }
            let catalog = async {
                health_http
                    .get(format!("{}/v1/host/projects", health_target.url))
                    .bearer_auth(&health_target.token)
                    .send()
                    .await?
                    .error_for_status()?
                    .json::<Value>()
                    .await
            };
            let Ok(Ok(value)) = tokio::time::timeout(Duration::from_secs(5), catalog).await else {
                continue;
            };
            if previous.as_ref() != Some(&value) {
                previous = Some(value);
                if health_send.send(Frame::CatalogChanged).await.is_err() {
                    break;
                }
            }
        }
    }));
    let mut active: HashMap<Uuid, Active> = HashMap::new();
    let mut jobs = JoinSet::new();
    // The request each job serves, so one that panics is still logged and released.
    let mut requests: HashMap<tokio::task::Id, (Uuid, String)> = HashMap::new();
    let mut tick = tokio::time::interval(silence / 5);
    let mut last_message = tokio::time::Instant::now();
    loop {
        tokio::select! {
            ()=runtime_lost.cancelled()=>return Err(Interrupted("Local runtime changed").into()),
            _=tick.tick()=>{
                if last_message.elapsed()>=silence {
                    return Err(Interrupted("Relay heartbeat timed out").into());
                }
                send_frame(&mut socket,Message::Ping(vec![1].into())).await?;
            },
            completed=jobs.join_next_with_id(),if !jobs.is_empty()=>match completed {
                Some(Ok((task,id)))=>{requests.remove(&task);active.remove(&id);},
                Some(Err(error))=>if let Some((id,label))=requests.remove(&error.id()) {
                    active.remove(&id);
                    eprintln!("Remote request {id} {label} failed: {error}");
                    let _=send.try_send(Frame::Error{id,message:"Local request failed".into()});
                },
                None=>{},
            },
            frame=outgoing.recv()=>{let Some(frame)=frame else {break};send_frame(&mut socket,Message::Text(serde_json::to_string(&frame)?.into())).await?;},
            frame=socket.next()=>{
                match frame.context("Relay closed")?? {
                    Message::Text(value)=>{
                        last_message=tokio::time::Instant::now();
                        match serde_json::from_str::<Frame>(&value)? {
                            frame@Frame::Open{id,..}=>{
                                ensure!(!active.contains_key(&id),"Duplicate relay request");
                                // A request routed here proves the relay took this host.
                                served.send_replace(true);
                                let (input,receiver)=mpsc::channel(WINDOW+2);
                                let credit=Arc::new(Semaphore::new(WINDOW));
                                let cancel=tokio_util::sync::CancellationToken::new();
                                active.insert(id,Active{input,credit:credit.clone(),cancel:cancel.clone()});
                                let label=request_label(&frame);
                                let send=send.clone();let target=target.clone();let http=http.clone();let job_label=label.clone();
                                let task=jobs.spawn(async move {
                                    tokio::select! {()=cancel.cancelled()=>{},result=dispatch(frame,target,http,send.clone(),credit,receiver)=>{if let Err(error)=result {
                                        // The remote side only hears that it failed; the connector log says why.
                                        eprintln!("Remote request {id} {job_label} failed: {error:#}");
                                        let _=send.send(Frame::Error{id,message:"Local request failed".into()}).await;
                                    }}}
                                    id
                                });
                                requests.insert(task.id(),(id,label));
                            },
                            Frame::Ack{id}=>{if let Some(stream)=active.get(&id) {ensure!(stream.credit.available_permits()<WINDOW,"Invalid relay credit");stream.credit.add_permits(1);}},
                            Frame::Cancel{id}=>{if let Some(stream)=active.remove(&id) {stream.cancel.cancel();}},
                            frame@Frame::Data{id,..}=>{if let Some(stream)=active.get(&id) {if stream.input.try_send(frame).is_err(){stream.cancel.cancel();}}},
                            _=>anyhow::bail!("Unexpected relay frame"),
                        }
                    },
                    Message::Ping(_)|Message::Pong(_)=>last_message=tokio::time::Instant::now(),
                    _=>break,
                }
            }
        }
    }
    // Dropping JoinSet also cancels requests on every error exit. Accepted turns live in the daemon.
    jobs.abort_all();
    Ok(())
}
/// Send one frame to the relay, as a stall once it takes longer than `RELAY_SEND`.
async fn send_frame(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    message: Message,
) -> Result<()> {
    match tokio::time::timeout(RELAY_SEND, socket.send(message)).await {
        Ok(sent) => Ok(sent?),
        Err(_) => Err(Interrupted("Relay did not take a frame in time").into()),
    }
}
/// How the connector log names a relayed request: its method and path. The
/// query, headers and body never reach the log, and the remote side cannot
/// break the line.
fn request_label(frame: &Frame) -> String {
    let Frame::Open { method, path, .. } = frame else {
        return "request".into();
    };
    let path = path.split(['?', '#']).next().unwrap_or_default();
    format!("{method} {path}")
        .chars()
        .filter(|character| !character.is_control())
        .take(200)
        .collect()
}
fn validate_path(path: &str, target: &Target, thread: Option<Uuid>) -> Result<()> {
    let path = path.split('?').next().unwrap_or(path);
    ensure!(
        path.starts_with("/v1/")
            && !path.contains(['%', '\\', '#'])
            && !path[1..]
                .split('/')
                .any(|part| part == "." || part == ".." || part.is_empty()),
        "Invalid relay path"
    );
    ensure!(
        path == "/v1/info" || path.starts_with("/v1/host/"),
        "Choose a registered host project first"
    );
    if let Some(shared) = target.thread {
        ensure!(
            thread == Some(shared),
            "Only the published chat is accessible"
        );
        ensure!(
            path.starts_with(&format!("/v1/host/workspaces/{}/v1/", target.project)),
            "Project is not published"
        );
    }
    Ok(())
}
async fn data(
    send: &mpsc::Sender<Frame>,
    credit: &Semaphore,
    id: Uuid,
    bytes: &[u8],
    text: bool,
) -> Result<()> {
    // Credit is returned only when the remote consumer has consumed the chunk.
    for chunk in bytes.chunks(CHUNK) {
        let permit = tokio::time::timeout(Duration::from_secs(35), credit.acquire()).await??;
        permit.forget();
        tokio::time::timeout(
            Duration::from_secs(10),
            send.send(Frame::Data {
                id,
                data: STANDARD.encode(chunk),
                text,
            }),
        )
        .await??;
    }
    Ok(())
}
async fn dispatch(
    frame: Frame,
    target: Target,
    http: reqwest::Client,
    send: mpsc::Sender<Frame>,
    credit: Arc<Semaphore>,
    mut incoming: mpsc::Receiver<Frame>,
) -> Result<()> {
    let Frame::Open {
        id,
        method,
        path,
        headers,
        body,
        websocket,
        thread,
    } = frame
    else {
        anyhow::bail!("Expected a request")
    };
    validate_path(&path, &target, thread)?;
    if thread.is_some() {
        let segments = path
            .split('?')
            .next()
            .unwrap_or(&path)
            .split('/')
            .collect::<Vec<_>>();
        let prefix = if segments.get(2) == Some(&"host") && segments.get(3) == Some(&"workspaces") {
            ensure!(
                segments.get(5) == Some(&"v1"),
                "Invalid scoped workspace path"
            );
            format!("/v1/host/workspaces/{}", segments[4])
        } else {
            String::new()
        };
        let info = http
            .get(format!("{}{prefix}/v1/info", target.url))
            .bearer_auth(&target.token)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        ensure!(
            info["capabilities"]
                .as_array()
                .is_some_and(|v| v.iter().any(|c| c == "remote-chat-scope.v1")),
            "Remote chat scope requires a current local runtime"
        );
    }
    let body = STANDARD.decode(body)?;
    ensure!(body.len() <= MAX_BODY, "Request is too large");
    let url = format!("{}{}", target.url.trim_end_matches('/'), path);
    if websocket {
        ensure!(
            thread.is_none(),
            "Terminal access requires full host access"
        );
        let mut request = url.replacen("http", "ws", 1).into_client_request()?;
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {}", target.token).parse()?);
        let (mut local, _) = tokio::time::timeout(
            Duration::from_secs(10),
            tokio_tungstenite::connect_async(request),
        )
        .await??;
        send.send(Frame::Head {
            id,
            status: 101,
            headers: vec![],
        })
        .await?;
        let mut keepalive = tokio::time::interval(Duration::from_secs(15));
        loop {
            tokio::select! {
                _=keepalive.tick()=>{send.send(Frame::Ack{id}).await?;},
                frame=local.next()=>match frame {
                    Some(Ok(Message::Text(value)))=>{ensure!(value.len()<=CHUNK,"Terminal frame too large");data(&send,&credit,id,value.as_bytes(),true).await?},
                    Some(Ok(Message::Binary(value)))=>data(&send,&credit,id,&value,false).await?,
                    Some(Ok(Message::Ping(_)|Message::Pong(_)))=>{},_=>break,
                },
                frame=incoming.recv()=>match frame {
                    Some(Frame::Data{data,text,..})=>{let bytes=STANDARD.decode(data)?;ensure!(bytes.len()<=CHUNK,"Terminal frame too large");let frame=if text {Message::Text(String::from_utf8(bytes)?.into())}else{Message::Binary(bytes.into())};local.send(frame).await?;send.send(Frame::Ack{id}).await?},_=>break,
                }
            }
        }
    } else {
        let mut request = http
            .request(method.parse()?, url)
            .bearer_auth(&target.token);
        for (name, value) in headers {
            if matches!(name.as_str(), "content-type" | "accept" | "last-event-id") {
                request = request.header(name, value);
            }
        }
        if let Some(thread) = thread {
            request = request.header(REMOTE_THREAD_HEADER, thread.to_string());
        }
        // Its errors would name the URL, query included; the log names the path.
        let response = tokio::time::timeout(Duration::from_secs(310), request.body(body).send())
            .await?
            .map_err(reqwest::Error::without_url)?;
        let headers = ["content-type", "content-disposition", "x-accel-buffering"]
            .into_iter()
            .filter_map(|name| {
                response
                    .headers()
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .map(|v| (name.into(), v.into()))
            })
            .collect();
        send.send(Frame::Head {
            id,
            status: response.status().as_u16(),
            headers,
        })
        .await?;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(reqwest::Error::without_url)?;
            data(&send, &credit, id, &chunk, false).await?;
        }
    }
    send.send(Frame::End { id }).await?;
    Ok(())
}

fn account_query(client: &haarness_account::Client) -> String {
    client
        .config
        .selected_account()
        .map(|id| format!("?accountId={id}"))
        .unwrap_or_default()
}

pub(crate) struct AbortTask(pub(crate) tokio::task::JoinHandle<()>);
impl Drop for AbortTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}
fn identity_file(
    paths: &Paths,
    client: &haarness_account::Client,
    account: &str,
    workspace: &Path,
    session: Option<Uuid>,
) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};
    let project = if session.is_some() {
        workspace.to_string_lossy().into_owned()
    } else {
        String::new()
    };
    let identity = format!(
        "{:x}",
        Sha256::digest(format!(
            "{}|{account}|{project}|{session:?}",
            client.config.api_url
        ))
    );
    paths.config_dir().join(format!("remote-{identity}.json"))
}
fn running(file: &Path) -> Result<bool> {
    let path = file.with_extension("lock");
    if !path.exists() {
        return Ok(false);
    }
    let lock = std::fs::OpenOptions::new().write(true).open(path)?;
    match fs2::FileExt::try_lock_exclusive(&lock) {
        Ok(()) => Ok(false),
        Err(error) if crate::lock_busy(&error) => Ok(true),
        Err(error) => Err(error.into()),
    }
}
/// Every connector identity this data directory knows, with its file.
fn identities(paths: &Paths) -> Result<Vec<(std::path::PathBuf, Value)>> {
    identities_in(paths.config_dir())
}
/// [`identities`] of the configuration folder `dir`.
fn identities_in(dir: &Path) -> Result<Vec<(std::path::PathBuf, Value)>> {
    let mut result = Vec::new();
    if !dir.exists() {
        return Ok(result);
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !name.starts_with("remote-") || !name.ends_with(".json") {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&std::fs::read(&path)?) else {
            continue;
        };
        if value["id"].as_str().is_some() {
            result.push((path, value));
        }
    }
    Ok(result)
}
/// The other computers Desktop can open: the account's hosts that share the
/// whole computer, without this one (any host whose connector identity lives
/// in `config_dir`) and without chat-only connectors, which Cloud lists under
/// Sessions instead.
pub(crate) async fn computers(
    client: &haarness_account::Client,
    config_dir: &Path,
) -> Result<Value> {
    let hosts = super::account::list_all(
        client,
        &format!("/api/v1/remote-hosts{}", account_query(client)),
    )
    .await?;
    let local = identities_in(config_dir)?
        .into_iter()
        .filter_map(|(_, identity)| identity["id"].as_str().map(str::to_owned))
        .collect();
    Ok(json!({ "items": other_computers(&hosts, &local) }))
}
/// [`computers`] without the network: each host as Desktop shows it.
fn other_computers(hosts: &Value, local: &std::collections::HashSet<String>) -> Vec<Value> {
    hosts["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|host| host["mode"] == "host")
        .filter(|host| host["id"].as_str().is_some_and(|id| !local.contains(id)))
        .map(|host| {
            json!({
                "id": host["id"],
                "name": host["name"],
                "connected": host["connected"] == true,
                "lastSeenAt": host["lastSeenAt"],
                "platform": host["metadata"]["platform"],
            })
        })
        .collect()
}
fn local_status(paths: &Paths) -> Result<Vec<Value>> {
    identities(paths)?
        .into_iter()
        .map(|(path, mut value)| {
            value["running"] = json!(running(&path)?);
            Ok(value)
        })
        .collect()
}
/// One connector for `remote status`: id, name, state, what it shares, and
/// why it stopped.
fn connector_line(connector: &Value) -> String {
    let text = |key: &str| connector[key].as_str().unwrap_or_default();
    let state = if connector["running"] == true {
        "running"
    } else {
        "stopped"
    };
    let shares = match connector["chatId"].as_str() {
        Some(chat) => format!("chat {chat}"),
        None => "this computer".to_owned(),
    };
    let mut line = format!("{}  {}  {state}  shares {shares}", text("id"), text("name"));
    if let Some(error) = connector["error"]
        .as_str()
        .filter(|error| !error.is_empty())
    {
        line.push_str("\n  ");
        line.push_str(error);
    }
    line.push('\n');
    line
}
fn local_file(paths: &Paths, id: Uuid) -> Result<Option<std::path::PathBuf>> {
    Ok(identities(paths)?
        .into_iter()
        .find(|(_, value)| value["id"].as_str() == Some(&id.to_string()))
        .map(|(path, _)| path))
}

/// Start a background connector and return once it holds its lock, with the
/// log offset its output starts at. The connector outlives the caller: the
/// stop file, `remote disable` or `/remote-control stop` ends it.
async fn spawn_connector(
    paths: &Paths,
    workspace: &Path,
    file: &Path,
    name: &str,
    session: Option<Uuid>,
) -> Result<u64> {
    std::fs::create_dir_all(paths.config_dir())?;
    let log = file.with_extension("log");
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let output = options.open(&log)?;
    let start = output.metadata()?.len();
    let mut command = std::process::Command::new(std::env::current_exe()?);
    if let Some(root) = &paths.root {
        command.arg("--data-dir").arg(root);
    }
    command
        .arg("--workspace")
        .arg(workspace)
        .args(["remote", "run", "--name", name]);
    if let Some(chat) = session {
        command.arg("--chat").arg(chat.to_string());
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    for _ in 0..100 {
        if let Some(status) = child.try_wait()? {
            anyhow::bail!(
                exit_reason(&log, start).unwrap_or_else(|| format!("Connector exited ({status})"))
            )
        }
        if running(file)? {
            // Reap it whenever it ends, so a long-lived daemon that started
            // it never collects zombies.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(start);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    anyhow::bail!("Connector is still starting")
}
/// What a connector said last before it gave up: its `Error:` line, or the
/// last thing it printed.
fn exit_reason(log: &Path, start: u64) -> Option<String> {
    let bytes = std::fs::read(log).ok()?;
    let text = String::from_utf8_lossy(bytes.get(usize::try_from(start).ok()?..)?);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines
        .iter()
        .rev()
        .find_map(|line| line.strip_prefix("Error: "))
        .or_else(|| lines.last().copied())
        .map(str::to_owned)
}

/// Where a published chat opens in the Cloud web app.
pub(crate) fn chat_url(client: &haarness_account::Client, publication: &str) -> Result<String> {
    Ok(client
        .config
        .web_origin()?
        .join(&format!("app/remote/sessions/{publication}"))?
        .to_string())
}

/// Whether one chat can be continued from Cloud, as `/remote-control`
/// reports it in the TUI and Desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ShareState {
    /// No Haarness Cloud sign-in on this computer.
    SignedOut,
    /// Not shared.
    Off,
    /// The connector runs but Cloud does not see it yet.
    Connecting,
    /// Reachable from Cloud.
    On,
    /// Someone stopped sharing or removed access from Cloud.
    Stopped,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatShare {
    pub state: ShareState,
    pub url: Option<String>,
    pub host_id: Option<String>,
    pub publication_id: Option<String>,
    pub computer: Option<String>,
    pub error: Option<String>,
}
impl ChatShare {
    fn new(state: ShareState) -> Self {
        Self {
            state,
            url: None,
            host_id: None,
            publication_id: None,
            computer: None,
            error: None,
        }
    }
}
/// The connector identity that publishes this chat of this project, under
/// any account of the configured Cloud.
fn chat_identity(
    paths: &Paths,
    client: &haarness_account::Client,
    workspace: &Path,
    thread: Uuid,
) -> Result<Option<(std::path::PathBuf, Value)>> {
    let thread = thread.to_string();
    Ok(identities(paths)?.into_iter().find(|(_, value)| {
        value["chatId"].as_str() == Some(thread.as_str())
            && value["apiUrl"].as_str() == Some(client.config.api_url.as_str())
            && value["workspace"].as_str() == workspace.to_str()
    }))
}
fn not_found(error: &haarness_account::Error) -> bool {
    matches!(
        error,
        haarness_account::Error::Api {
            status: 403 | 404,
            ..
        }
    )
}
/// Report one chat's sharing. `check_cloud` asks Cloud whether it reaches
/// the connector; without it the answer is local and instant.
pub(crate) async fn chat_status(
    paths: &Paths,
    workspace: &Path,
    thread: Uuid,
    check_cloud: bool,
) -> Result<ChatShare> {
    status_with(
        &super::account::client()?,
        paths,
        workspace,
        thread,
        check_cloud,
    )
    .await
}
async fn status_with(
    client: &haarness_account::Client,
    paths: &Paths,
    workspace: &Path,
    thread: Uuid,
    check_cloud: bool,
) -> Result<ChatShare> {
    let idle = if client.config.signed_in() {
        ShareState::Off
    } else {
        ShareState::SignedOut
    };
    let Some((file, identity)) = chat_identity(paths, client, workspace, thread)? else {
        return Ok(ChatShare::new(idle));
    };
    if !running(&file)? {
        // A connector that gave up (an outdated protocol, revoked access)
        // said why; keep showing it until the chat is shared again.
        let mut share = ChatShare::new(idle);
        share.error = identity["error"].as_str().map(str::to_owned);
        return Ok(share);
    }
    let publication = identity["publicationId"].as_str().map(str::to_owned);
    let mut share = ChatShare {
        url: publication
            .as_deref()
            .and_then(|id| chat_url(client, id).ok()),
        host_id: identity["id"].as_str().map(str::to_owned),
        publication_id: publication.clone(),
        computer: identity["name"].as_str().map(str::to_owned),
        ..ChatShare::new(ShareState::Connecting)
    };
    if let (true, Some(publication)) = (check_cloud, publication) {
        match client
            .request(
                reqwest::Method::GET,
                &format!("/api/v1/remote-sessions/{publication}"),
                None,
            )
            .await
        {
            Ok(value) if value["connected"] == true => share.state = ShareState::On,
            Ok(_) => {}
            Err(error) if not_found(&error) => {
                share.state = ShareState::Stopped;
                share.url = None;
                // Cloud ended it (Stop sharing, Remove access): the connector
                // has nothing left to serve. Clean up now so the next share
                // registers afresh; a failure here is retried by that share.
                if stop_connector(&file).await.is_ok() {
                    let _ = forget_chat(client, &file, &identity).await;
                }
            }
            Err(haarness_account::Error::NotLoggedIn) => share.state = ShareState::SignedOut,
            Err(error) => share.error = Some(error.to_string()),
        }
    }
    Ok(share)
}
/// Share one chat: start (or restart) its connector and wait until Cloud
/// has published it. Sharing that was stopped from Cloud starts over with a
/// fresh registration, because Cloud never revives a revoked publication.
pub(crate) async fn share_chat(paths: &Paths, workspace: &Path, thread: Uuid) -> Result<ChatShare> {
    let client = super::account::client()?;
    if !client.config.signed_in() {
        return Ok(ChatShare::new(ShareState::SignedOut));
    }
    let status = client.status().await?;
    if status["loggedIn"] != true {
        return Ok(ChatShare::new(ShareState::SignedOut));
    }
    let account = status["selectedAccountId"]
        .as_str()
        .filter(|id| !id.is_empty())
        .context("Choose a Haarness Cloud account first")?;
    if let Some((file, identity)) = chat_identity(paths, &client, workspace, thread)? {
        if running(&file)? {
            let share = chat_status(paths, workspace, thread, true).await?;
            if share.state != ShareState::Stopped {
                return Ok(share);
            }
            stop_connector(&file).await?;
            forget_chat(&client, &file, &identity).await?;
        }
    }
    let file = identity_file(paths, &client, account, workspace, Some(thread));
    if let Some(identity) = std::fs::read(&file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    {
        if let Some(publication) = identity["publicationId"].as_str() {
            match client
                .request(
                    reqwest::Method::GET,
                    &format!("/api/v1/remote-sessions/{publication}"),
                    None,
                )
                .await
            {
                Ok(_) => {}
                Err(error) if not_found(&error) => forget_chat(&client, &file, &identity).await?,
                Err(error) => return Err(error.into()),
            }
        }
    }
    let computer = haarness_account::computer_name();
    let start = spawn_connector(paths, workspace, &file, &computer, Some(thread)).await?;
    // The connector registers and publishes as soon as it holds its lock.
    for _ in 0..150 {
        let published = std::fs::read(&file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|identity| identity["publicationId"].is_string());
        if published {
            break;
        }
        if !running(&file)? {
            anyhow::bail!(
                exit_reason(&file.with_extension("log"), start).unwrap_or_else(|| {
                    "The connector stopped before Cloud published the chat".into()
                })
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Cloud sees the connector a moment after it publishes; a short wait
    // lets the first answer say "on" instead of "connecting".
    let mut share = chat_status(paths, workspace, thread, true).await?;
    for _ in 0..10 {
        if share.state != ShareState::Connecting {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        share = chat_status(paths, workspace, thread, true).await?;
    }
    Ok(share)
}
/// Stop sharing one chat: end its connector, then end Cloud access and
/// forget the registration. Local files and history are untouched.
pub(crate) async fn stop_chat(paths: &Paths, workspace: &Path, thread: Uuid) -> Result<ChatShare> {
    stop_with(&super::account::client()?, paths, workspace, thread).await
}
async fn stop_with(
    client: &haarness_account::Client,
    paths: &Paths,
    workspace: &Path,
    thread: Uuid,
) -> Result<ChatShare> {
    let idle = if client.config.signed_in() {
        ShareState::Off
    } else {
        ShareState::SignedOut
    };
    let Some((file, identity)) = chat_identity(paths, client, workspace, thread)? else {
        return Ok(ChatShare::new(idle));
    };
    stop_connector(&file).await?;
    let mut share = ChatShare::new(idle);
    if let Err(error) = forget_chat(client, &file, &identity).await {
        // The connector is down, so nothing reaches the chat; keep the
        // identity so a later stop (or share) can finish the cleanup.
        share.error = Some(format!(
            "Stopped on this computer, but Haarness Cloud could not be updated: {error}"
        ));
    }
    Ok(share)
}
async fn stop_connector(file: &Path) -> Result<()> {
    if !running(file)? {
        return Ok(());
    }
    std::fs::write(file.with_extension("stop"), "stop")?;
    for _ in 0..150 {
        if !running(file)? {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!("The connector did not stop within 15 seconds")
}
/// End Cloud access for a stopped chat connector and delete its identity.
async fn forget_chat(
    client: &haarness_account::Client,
    file: &Path,
    identity: &Value,
) -> Result<()> {
    let removals = [
        identity["publicationId"]
            .as_str()
            .map(|id| format!("/api/v1/remote-sessions/{id}")),
        identity["id"]
            .as_str()
            .map(|id| format!("/api/v1/remote-hosts/{id}")),
    ];
    for path in removals.into_iter().flatten() {
        match client.request(reqwest::Method::DELETE, &path, None).await {
            Ok(_) => {}
            Err(error) if not_found(&error) => {}
            Err(error) => return Err(error.into()),
        }
    }
    for extension in ["json", "lock", "stop", "log"] {
        match std::fs::remove_file(file.with_extension(extension)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[test]
    fn remote_commands_parse_with_global_options() {
        for command in [
            vec!["run"],
            vec!["enable"],
            vec!["status"],
            vec!["hosts"],
            vec!["sessions"],
            vec!["run", "--chat", "00000000-0000-0000-0000-000000000001"],
            vec!["disable", "00000000-0000-0000-0000-000000000001"],
        ] {
            let mut args = vec!["haarness", "remote"];
            args.extend(command);
            assert!(super::super::Cli::try_parse_from(args).is_ok());
        }
    }
    #[test]
    fn desktop_lists_other_computers_without_this_one_or_shared_chats() {
        let dir = std::env::temp_dir().join(format!("haarness-computers-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let this = "00000000-0000-4000-8000-0000000000a1";
        std::fs::write(
            dir.join("remote-abc.json"),
            json!({"id": this, "name": "This Mac"}).to_string(),
        )
        .unwrap();
        // Files that are not connector identities never hide a host.
        std::fs::write(dir.join("remote-abc.log"), this).unwrap();
        std::fs::write(dir.join("account.json"), json!({"id": "b1"}).to_string()).unwrap();
        let local = identities_in(&dir)
            .unwrap()
            .into_iter()
            .filter_map(|(_, identity)| identity["id"].as_str().map(str::to_owned))
            .collect();
        let hosts = json!({"items": [
            {"id": this, "name": "This Mac", "mode": "host", "connected": true, "metadata": {}},
            {"id": "b1", "name": "Studio", "mode": "host", "connected": true,
             "lastSeenAt": "2026-10-22T10:00:00Z", "metadata": {"platform": "macos", "version": "0.1.3"}},
            {"id": "c1", "name": "Shared investigation", "mode": "sessions", "connected": true, "metadata": {}},
            {"id": "d1", "name": "Linux box", "mode": "host", "connected": false,
             "lastSeenAt": "2026-10-20T08:00:00Z", "metadata": {"platform": "linux"}},
        ]});
        assert_eq!(
            other_computers(&hosts, &local),
            vec![
                json!({"id": "b1", "name": "Studio", "connected": true,
                       "lastSeenAt": "2026-10-22T10:00:00Z", "platform": "macos"}),
                json!({"id": "d1", "name": "Linux box", "connected": false,
                       "lastSeenAt": "2026-10-20T08:00:00Z", "platform": "linux"}),
            ]
        );
        assert!(other_computers(&json!({}), &local).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn exit_reason_prefers_the_final_error_after_the_offset() {
        let root = std::env::temp_dir().join(format!("haarness-remote-log-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let log = root.join("remote.log");
        std::fs::write(&log, "Error: an earlier run\n").unwrap();
        let start = std::fs::metadata(&log).unwrap().len();
        std::fs::write(
            &log,
            "Error: an earlier run\nRemote host 1. Connecting\nError: Remote host access was revoked or is no longer allowed\n\nCaused by:\n    403\n",
        )
        .unwrap();
        assert_eq!(
            exit_reason(&log, start).as_deref(),
            Some("Remote host access was revoked or is no longer allowed")
        );
        std::fs::write(
            &log,
            "Error: an earlier run\nWaiting for the local runtime\n",
        )
        .unwrap();
        assert_eq!(
            exit_reason(&log, start).as_deref(),
            Some("Waiting for the local runtime")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn shared_chat_reports_its_link_and_stopping_ends_cloud_access() {
        use axum::{
            Json, Router,
            extract::Path as Route,
            http::StatusCode,
            routing::{delete, get},
        };
        let root = std::env::temp_dir().join(format!("haarness-remote-chat-{}", Uuid::now_v7()));
        let paths = Paths::new(Some(&root)).unwrap();
        let workspace = root.join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        let (thread, host, publication) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let removed = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let (sessions, hosts) = (removed.clone(), removed.clone());
        let router = Router::new()
            .route(
                "/api/v1/remote-sessions/{id}",
                get(move |Route(id): Route<Uuid>| async move {
                    if id == publication {
                        Ok(Json(json!({"id":id,"connected":true})))
                    } else {
                        Err(StatusCode::NOT_FOUND)
                    }
                })
                .delete(move |Route(id): Route<String>| async move {
                    sessions.lock().unwrap().push(format!("session {id}"));
                    StatusCode::NO_CONTENT
                }),
            )
            .route(
                "/api/v1/remote-hosts/{id}",
                delete(move |Route(id): Route<String>| async move {
                    hosts.lock().unwrap().push(format!("host {id}"));
                    StatusCode::NO_CONTENT
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = haarness_account::Client::new(
            haarness_account::Config::new(&api, root.join("cloud"), "test")
                .unwrap()
                .with_api_key("hck_fixture".into())
                .unwrap(),
        )
        .unwrap();
        let status = |check| status_with(&client, &paths, &workspace, thread, check);
        assert_eq!(status(true).await.unwrap().state, ShareState::Off);

        // A connector that published this chat and still holds its lock.
        let file = paths.config_dir().join("remote-fixture.json");
        let identity = json!({"id":host,"workspace":workspace,"chatId":thread,
            "apiUrl":client.config.api_url,"publicationId":publication,"name":"Desk"});
        std::fs::write(&file, identity.to_string()).unwrap();
        let lock = std::fs::File::create(file.with_extension("lock")).unwrap();
        fs2::FileExt::lock_exclusive(&lock).unwrap();
        let local = status(false).await.unwrap();
        assert_eq!(local.state, ShareState::Connecting);
        let shared = status(true).await.unwrap();
        assert_eq!(shared.state, ShareState::On);
        assert_eq!(
            shared.url.as_deref(),
            Some(format!("{api}/app/remote/sessions/{publication}").as_str())
        );
        assert_eq!(shared.computer.as_deref(), Some("Desk"));
        // Another chat of the same project is not shared.
        assert_eq!(
            status_with(&client, &paths, &workspace, Uuid::now_v7(), true)
                .await
                .unwrap()
                .state,
            ShareState::Off
        );

        // The connector exits when it sees the stop file, as `remote run` does.
        let stop = file.with_extension("stop");
        let connector = std::thread::spawn(move || {
            while !stop.exists() {
                std::thread::sleep(Duration::from_millis(20));
            }
            drop(lock);
        });
        let stopped = stop_with(&client, &paths, &workspace, thread)
            .await
            .unwrap();
        connector.join().unwrap();
        assert_eq!(stopped.state, ShareState::Off);
        assert_eq!(stopped.error, None);
        assert_eq!(
            *removed.lock().unwrap(),
            [format!("session {publication}"), format!("host {host}")]
        );
        assert!(!file.exists() && !file.with_extension("lock").exists());
        assert_eq!(status(true).await.unwrap().state, ShareState::Off);

        // Sharing stopped from Cloud while the connector still runs.
        std::fs::write(
            &file,
            json!({"id":host,"workspace":workspace,"chatId":thread,
                "apiUrl":client.config.api_url,"publicationId":Uuid::now_v7()})
            .to_string(),
        )
        .unwrap();
        let lock = std::fs::File::create(file.with_extension("lock")).unwrap();
        fs2::FileExt::lock_exclusive(&lock).unwrap();
        let revoked = status(true).await.unwrap();
        assert_eq!(revoked.state, ShareState::Stopped);
        assert_eq!(revoked.url, None);
        drop(lock);
        std::fs::remove_dir_all(root).unwrap();
    }
    /// `wait` is `nominal` milliseconds, jittered at most a fifth shorter.
    fn about(wait: Duration, nominal: u64) -> bool {
        let nominal = Duration::from_millis(nominal);
        wait <= nominal && wait >= nominal.mul_f64(0.8)
    }
    #[test]
    fn dropped_connections_back_off_to_minutes_and_only_a_proven_one_resets() {
        let mut backoff = Backoff::new(Pacing::default());
        let dropped = Link::Ended { proven: false };
        for seconds in [1, 2, 4, 60, 120, 240, 480, 600, 600] {
            let wait = backoff.wait(dropped);
            assert!(about(wait, seconds * 1000), "{wait:?} for {seconds}s");
        }
        // Cloud unreachable meanwhile: the short backoff, capped, and the
        // drop count stays.
        assert!(about(backoff.wait(Link::Failed), 60_000));
        assert!(about(backoff.wait(dropped), 600_000));
        // A connection that lasted, then its next drop, start over.
        assert!(about(backoff.wait(Link::Ended { proven: true }), 1000));
        assert!(about(backoff.wait(dropped), 2000));

        let mut backoff = Backoff::new(Pacing::default());
        for seconds in [1, 2, 4, 8, 16, 32, 60, 60] {
            assert!(about(backoff.wait(Link::Failed), seconds * 1000));
        }
    }
    #[derive(Clone, Copy)]
    enum Relay {
        /// Say ready, then close at once.
        Drop,
        /// Say ready and keep the socket this long.
        Hold(Duration),
        /// Take the socket but say nothing, not even ready, for this long.
        Mute(Duration),
        /// Route one request here and wait for its answer.
        Serve,
        /// Announce another protocol version.
        Version(u64),
        /// Announce these features, keep the socket and pass on every frame
        /// the connector sends.
        Record(&'static [&'static str]),
    }
    /// A relay that plays one step per connection, and the frames recorded.
    async fn fake_relay(script: Vec<Relay>) -> (String, mpsc::UnboundedReceiver<Value>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/connect", listener.local_addr().unwrap());
        let (recorded, frames) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            for step in script {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                let ready = match step {
                    Relay::Version(version) => json!({"type":"ready","version":version}),
                    Relay::Record(features) => {
                        json!({"type":"ready","version":1,"features":features})
                    }
                    _ => json!({"type":"ready","version":1}),
                };
                if !matches!(step, Relay::Mute(_)) {
                    socket
                        .send(Message::Text(ready.to_string().into()))
                        .await
                        .unwrap();
                }
                match step {
                    Relay::Hold(time) | Relay::Mute(time) => tokio::time::sleep(time).await,
                    Relay::Serve => {
                        let open = json!({"type":"open","id":Uuid::now_v7(),"method":"GET",
                            "path":"/v1/info","headers":[],"body":"","websocket":false,"thread":null});
                        socket
                            .send(Message::Text(open.to_string().into()))
                            .await
                            .unwrap();
                        while let Some(Ok(message)) = socket.next().await {
                            if let Message::Text(text) = message
                                && serde_json::from_str::<Value>(&text).unwrap()["type"] == "end"
                            {
                                break;
                            }
                        }
                    }
                    Relay::Record(_) => {
                        while let Some(Ok(message)) = socket.next().await {
                            if let Message::Text(text) = message {
                                let _ = recorded.send(serde_json::from_str(&text).unwrap());
                            }
                        }
                    }
                    Relay::Drop | Relay::Version(_) => {}
                }
                let _ = socket.close(None).await;
            }
        });
        (url, frames)
    }
    #[test]
    fn the_relay_upgrade_asks_for_the_features_the_connector_understands() {
        let url: reqwest::Url = "wss://cloud.example/api/v1/remote-hosts/1/connect"
            .parse()
            .unwrap();
        let handshake = relay_handshake(&url, "token").unwrap();
        assert_eq!(
            handshake.uri(),
            "wss://cloud.example/api/v1/remote-hosts/1/connect"
        );
        assert_eq!(handshake.headers()["authorization"], "Bearer token");
        assert_eq!(
            handshake.headers()["x-haarness-relay-features"],
            "attention.v1, attention.working"
        );
    }
    #[test]
    fn a_ready_frame_announces_a_version_and_optional_features() {
        assert_eq!(
            relay_ready(r#"{"type":"ready","version":1}"#),
            Some((1, vec![]))
        );
        assert_eq!(
            relay_ready(
                r#"{"type":"ready","version":1,"features":["attention.v1",7],"later":true}"#
            ),
            Some((1, vec!["attention.v1".to_owned()]))
        );
        assert_eq!(relay_ready(r#"{"type":"ready"}"#), None);
        assert_eq!(relay_ready(r#"{"type":"open","version":1}"#), None);
    }
    /// A Cloud that refuses each relay upgrade with `status` and its error
    /// body; a message starting with `split:` comes in a write of its own,
    /// after the headers.
    async fn refusing_cloud(refusals: Vec<(u16, &'static str)>) -> String {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/connect", listener.local_addr().unwrap());
        tokio::spawn(async move {
            for (status, message) in refusals {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut buffer).await.unwrap();
                    request.extend_from_slice(&buffer[..read]);
                }
                let (split, message) = match message.strip_prefix("split:") {
                    Some(message) => (true, message),
                    None => (false, message),
                };
                let body = json!({"code":"forbidden","message":message,"requestId":"r1","retryable":false}).to_string();
                let head = format!(
                    "HTTP/1.1 {status} Refused\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                if split {
                    stream.write_all(head.as_bytes()).await.unwrap();
                    stream.flush().await.unwrap();
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    let _ = stream.write_all(body.as_bytes()).await;
                } else {
                    stream
                        .write_all(format!("{head}{body}").as_bytes())
                        .await
                        .unwrap();
                }
                let _ = stream.shutdown().await;
            }
        });
        url
    }
    #[tokio::test]
    async fn a_relay_refusal_names_what_cloud_said() {
        let cloud = refusing_cloud(vec![
            (409, "This host is connecting from another computer"),
            (
                403,
                "Only the registered publisher can connect this host\u{1b}[2J",
            ),
            (403, "split:A reason that arrives after the headers"),
        ])
        .await;
        let target = Target {
            url: "http://127.0.0.1:9".into(),
            token: "fixture".into(),
            project: "project".into(),
            thread: None,
        };
        let root = std::env::temp_dir().join(format!("haarness-remote-refusal-{}", Uuid::now_v7()));
        let attention = attention(&Paths::new(Some(&root)).unwrap());
        let said = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let lines = said.clone();
        let mut outage = Outage::new(move |line| lines.lock().unwrap().push(line.to_owned()));
        let http = reqwest::Client::new();
        let link = relay_link(
            cloud.as_str().into_client_request().unwrap(),
            &target,
            &http,
            Duration::from_secs(60),
            RELAY_SILENCE,
            &attention,
            &mut outage,
        )
        .await
        .unwrap();
        assert_eq!(link, Link::Failed);
        assert_eq!(
            said.lock().unwrap().as_slice(),
            [
                "Haarness Cloud refused the connection (409 Conflict): This host is connecting from another computer; reconnecting"
            ]
        );
        let error = relay_link(
            cloud.as_str().into_client_request().unwrap(),
            &target,
            &http,
            Duration::from_secs(60),
            RELAY_SILENCE,
            &attention,
            &mut outage,
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Haarness Cloud refused this computer's connection: Only the registered publisher can connect this host [2J"
        );
        // A body that comes after the headers is not waited for: the refusal
        // still stops the connector, in the words it had before.
        let error = relay_link(
            cloud.as_str().into_client_request().unwrap(),
            &target,
            &http,
            Duration::from_secs(60),
            RELAY_SILENCE,
            &attention,
            &mut outage,
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Remote host access was revoked or is no longer allowed"
        );
    }
    /// Attention context for `paths`, with waits short enough for tests.
    fn attention(paths: &Paths) -> Arc<remote_attention::Context> {
        remote_attention::Context::new(
            paths,
            false,
            remote_attention::Timing {
                debounce: Duration::from_millis(50),
                settle: Duration::from_millis(300),
                scan: Duration::from_millis(100),
                first: Duration::from_secs(5),
            },
        )
    }
    #[tokio::test]
    async fn a_relay_that_keeps_dropping_the_connection_is_retried_less_and_less_often() {
        use axum::{Json, Router, routing::get};
        let runtime = Router::new().route(
            "/v1/info",
            get(|| async { Json(json!({"capabilities":["remote-chat-scope.v1"]})) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = Target {
            url: format!("http://{}", listener.local_addr().unwrap()),
            token: "fixture".into(),
            project: "project".into(),
            thread: None,
        };
        tokio::spawn(async move { axum::serve(listener, runtime).await.unwrap() });
        let http = reqwest::Client::new();
        let pacing = Pacing {
            retry: Duration::from_millis(10),
            retry_max: Duration::from_millis(40),
            recover_for: Duration::ZERO,
            recover_max: Duration::from_millis(40),
            stable: Duration::from_millis(300),
            drops: 4,
            cooldown: Duration::from_millis(100),
            cooldown_max: Duration::from_millis(500),
        };
        let mut script = vec![Relay::Drop; 6];
        script.extend([
            Relay::Hold(Duration::from_millis(450)),
            Relay::Drop,
            Relay::Serve,
            Relay::Drop,
            Relay::Version(2),
        ]);
        let (relay, _) = fake_relay(script).await;
        let root = std::env::temp_dir().join(format!("haarness-remote-backoff-{}", Uuid::now_v7()));
        let attention = attention(&Paths::new(Some(&root)).unwrap());
        let mut outage = Outage::new(|_| {});

        let mut backoff = Backoff::new(pacing);
        let dropped = Link::Ended { proven: false };
        let proven = Link::Ended { proven: true };
        for (expected, nominal) in [
            // Accepted and dropped: the waits grow, then jump to the long ones.
            (dropped, 10),
            (dropped, 20),
            (dropped, 40),
            (dropped, 100),
            (dropped, 200),
            (dropped, 400),
            // Up for the stable period: back to the shortest wait.
            (proven, 10),
            (dropped, 20),
            // Carried a request: proven at once.
            (proven, 10),
            (dropped, 20),
        ] {
            let link = relay_link(
                relay.as_str().into_client_request().unwrap(),
                &target,
                &http,
                pacing.stable,
                RELAY_SILENCE,
                &attention,
                &mut outage,
            )
            .await
            .unwrap();
            assert_eq!(link, expected);
            let wait = backoff.wait(link);
            assert!(
                about(wait, nominal),
                "{wait:?} after {link:?}, expected {nominal}ms"
            );
        }
        // A relay on another protocol stops the connector: only an update helps.
        let error = relay_link(
            relay.as_str().into_client_request().unwrap(),
            &target,
            &http,
            pacing.stable,
            RELAY_SILENCE,
            &attention,
            &mut outage,
        )
        .await
        .unwrap_err();
        assert!(error.is::<Unsupported>(), "{error}");
        assert!(error.to_string().contains("Update Haarness"), "{error}");
        // Nothing listening is a plain failed connect.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let nowhere = format!("ws://{}/connect", closed.local_addr().unwrap());
        drop(closed);
        let link = relay_link(
            nowhere.into_client_request().unwrap(),
            &target,
            &http,
            pacing.stable,
            RELAY_SILENCE,
            &attention,
            &mut outage,
        )
        .await
        .unwrap();
        assert_eq!(link, Link::Failed);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn a_connection_that_stalls_before_it_proves_itself_is_a_failed_attempt_not_a_drop() {
        use axum::{Json, Router, routing::get};
        let runtime = Router::new().route(
            "/v1/info",
            get(|| async { Json(json!({"capabilities":["remote-chat-scope.v1"]})) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = Target {
            url: format!("http://{}", listener.local_addr().unwrap()),
            token: "fixture".into(),
            project: "project".into(),
            thread: None,
        };
        tokio::spawn(async move { axum::serve(listener, runtime).await.unwrap() });
        let http = reqwest::Client::new();
        // A relay that answers nothing after `ready` (the network stalled),
        // twice, one that never says `ready`, twice, then one that drops the
        // connection at once.
        let mut relays = Vec::new();
        for step in [
            Relay::Hold(Duration::from_secs(30)),
            Relay::Hold(Duration::from_secs(30)),
            Relay::Mute(Duration::from_secs(30)),
            Relay::Mute(Duration::from_secs(30)),
            Relay::Drop,
        ] {
            relays.push(fake_relay(vec![step]).await.0);
        }
        let root = std::env::temp_dir().join(format!("haarness-remote-stall-{}", Uuid::now_v7()));
        let attention = attention(&Paths::new(Some(&root)).unwrap());
        let said = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let heard = said.clone();
        let mut outage = Outage::new(move |line| heard.lock().unwrap().push(line.to_owned()));
        let expected = [
            Link::Failed,
            Link::Failed,
            Link::Failed,
            Link::Failed,
            Link::Ended { proven: false },
        ];
        // Two drops in a row would already earn an hour's wait here.
        let mut backoff = Backoff::new(Pacing {
            retry: Duration::from_millis(1),
            retry_max: Duration::from_millis(50),
            recover_for: Duration::ZERO,
            recover_max: Duration::from_millis(50),
            stable: Duration::from_secs(60),
            drops: 2,
            cooldown: Duration::from_secs(3600),
            cooldown_max: Duration::from_secs(3600),
        });
        for (relay, expected) in relays.iter().zip(expected) {
            let started = std::time::Instant::now();
            let link = relay_link(
                relay.as_str().into_client_request().unwrap(),
                &target,
                &http,
                Duration::from_secs(60),
                Duration::from_millis(250),
                &attention,
                &mut outage,
            )
            .await
            .unwrap();
            assert_eq!(link, expected);
            // Ended by the connector's heartbeat, long before the relay lets go.
            assert!(started.elapsed() < Duration::from_secs(10), "{link:?}");
            assert!(backoff.wait(link) <= Duration::from_millis(50), "{link:?}");
        }
        let said = said.lock().unwrap().clone();
        assert!(
            said.iter()
                .any(|line| line
                    == "Remote connection ended: Relay heartbeat timed out; reconnecting"),
            "{said:?}"
        );
        assert!(
            said.iter().any(|line| line
                == "Remote connection ended: Relay did not say it was ready in time; reconnecting"),
            "{said:?}"
        );
        std::fs::remove_dir_all(root).ok();
    }
    /// How a fake token endpoint answers one renewal.
    #[derive(Clone, Copy)]
    enum Renewal {
        /// A load balancer's page while Cloud is down.
        Unavailable,
        /// The connection closes without an answer.
        Dropped,
        /// New tokens.
        Renewed,
        /// The refresh token was revoked.
        Revoked,
    }
    /// A token endpoint that plays one answer per connection.
    async fn fake_token_endpoint(script: Vec<Renewal>) -> String {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            for answer in script {
                let (mut socket, _) = listener.accept().await.unwrap();
                if let Renewal::Dropped = answer {
                    drop(socket);
                    continue;
                }
                let mut request = Vec::new();
                let mut chunk = [0_u8; 4096];
                let header_end = loop {
                    let read = socket.read(&mut chunk).await.unwrap();
                    request.extend_from_slice(&chunk[..read]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let head = String::from_utf8_lossy(&request[..header_end]).to_lowercase();
                let length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                while request.len() < header_end + length {
                    let read = socket.read(&mut chunk).await.unwrap();
                    request.extend_from_slice(&chunk[..read]);
                }
                let (status, kind, body) = match answer {
                    Renewal::Unavailable => (
                        "503 Service Unavailable",
                        "text/html",
                        "<html><body>503 Service Temporarily Unavailable</body></html>".to_owned(),
                    ),
                    Renewal::Renewed => (
                        "200 OK",
                        "application/json",
                        json!({"access_token":"renewed","refresh_token":"next","expires_in":600})
                            .to_string(),
                    ),
                    Renewal::Revoked => (
                        "400 Bad Request",
                        "application/json",
                        json!({"error":"invalid_grant"}).to_string(),
                    ),
                    Renewal::Dropped => unreachable!(),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        url
    }
    #[tokio::test]
    async fn a_cloud_outage_while_the_sign_in_expired_is_waited_out_and_said_once() {
        use axum::{Json, Router, routing::get};
        let runtime = Router::new().route(
            "/v1/info",
            get(|| async { Json(json!({"capabilities":["remote-chat-scope.v1"]})) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = Target {
            url: format!("http://{}", listener.local_addr().unwrap()),
            token: "fixture".into(),
            project: "project".into(),
            thread: None,
        };
        tokio::spawn(async move { axum::serve(listener, runtime).await.unwrap() });
        let root = std::env::temp_dir().join(format!("haarness-remote-outage-{}", Uuid::now_v7()));
        let cloud = fake_token_endpoint(vec![
            Renewal::Unavailable,
            Renewal::Unavailable,
            Renewal::Dropped,
            Renewal::Renewed,
            Renewal::Revoked,
        ])
        .await;
        let config =
            haarness_account::Config::new(&cloud, root.join("account"), "haarness-cli").unwrap();
        // The access token expired while Cloud was away.
        haarness_account::credentials::save(
            &config.path(),
            &haarness_account::Credentials {
                api_url: config.api_url.to_string(),
                access_token: "expired".into(),
                access_expires_at: chrono::Utc::now() - chrono::Duration::minutes(5),
                refresh_token: "refresh".into(),
            },
        )
        .unwrap();
        let client = haarness_account::Client::new(config).unwrap();
        let (relay, _) = fake_relay(vec![Relay::Serve]).await;
        let relay: reqwest::Url = relay.parse().unwrap();
        let attention = attention(&Paths::new(Some(&root)).unwrap());
        let http = reqwest::Client::new();
        let lines = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let heard = lines.clone();
        let mut state = Reconnect::new(Pacing::default(), move |line| {
            heard.lock().unwrap().push(line.to_owned());
        });

        // Cloud answers with a load balancer's page, twice, then not at all:
        // each a failed attempt, each cause said once.
        for said in [1, 1, 2] {
            let link = reconnect_step(&client, &relay, &target, &http, &attention, &mut state)
                .await
                .unwrap();
            assert_eq!(link, Link::Failed);
            assert_eq!(
                lines.lock().unwrap().len(),
                said,
                "{:?}",
                lines.lock().unwrap()
            );
        }
        // Back: the sign-in renews, the relay routes a request, and the
        // connection that proves itself says how long it took.
        let link = reconnect_step(&client, &relay, &target, &http, &attention, &mut state)
            .await
            .unwrap();
        assert_eq!(link, Link::Ended { proven: true });
        let said = lines.lock().unwrap().clone();
        assert!(
            said[0].starts_with("Could not reach Haarness Cloud to renew")
                && said[0].contains("HTTP 503"),
            "{said:?}"
        );
        assert!(
            said[1].starts_with("Could not reach Haarness Cloud to renew")
                && !said[1].contains("HTTP 503"),
            "{said:?}"
        );
        assert!(
            said[2].starts_with("Remote host reconnected after 4 attempts"),
            "{said:?}"
        );
        assert!(
            !said.iter().any(|line| line == "Remote host connected"),
            "{said:?}"
        );
        // A revoked sign-in is not an outage: it ends the connector.
        state.rejected = Some("renewed".into());
        let error = reconnect_step(&client, &relay, &target, &http, &attention, &mut state)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<haarness_account::Error>(),
                Some(haarness_account::Error::NotLoggedIn)
            ),
            "{error:#}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn a_connector_that_gave_up_says_why_in_the_chat_status() {
        let root = std::env::temp_dir().join(format!("haarness-remote-exit-{}", Uuid::now_v7()));
        let paths = Paths::new(Some(&root)).unwrap();
        let workspace = root.join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        let client = haarness_account::Client::new(
            haarness_account::Config::new("http://127.0.0.1:9", root.join("cloud"), "test")
                .unwrap()
                .with_api_key("hck_fixture".into())
                .unwrap(),
        )
        .unwrap();
        let thread = Uuid::now_v7();
        let file = paths.config_dir().join("remote-fixture.json");
        std::fs::write(
            &file,
            json!({"id":Uuid::now_v7(),"workspace":workspace,"chatId":thread,
                "apiUrl":client.config.api_url,"publicationId":Uuid::now_v7()})
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            status_with(&client, &paths, &workspace, thread, true)
                .await
                .unwrap()
                .error,
            None
        );
        record_exit(&file, &anyhow::Error::new(Unsupported(2)));
        let share = status_with(&client, &paths, &workspace, thread, true)
            .await
            .unwrap();
        assert_eq!(share.state, ShareState::Off);
        assert!(
            share
                .error
                .as_deref()
                .is_some_and(|error| error.contains("Update Haarness")),
            "{share:?}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn a_failed_request_is_logged_by_method_and_path_alone() {
        let open = |method: &str, path: &str| Frame::Open {
            id: Uuid::now_v7(),
            method: method.into(),
            path: path.into(),
            headers: vec![("authorization".into(), "Bearer secret".into())],
            body: STANDARD.encode("secret body"),
            websocket: false,
            thread: None,
        };
        assert_eq!(
            request_label(&open(
                "POST",
                "/v1/host/workspaces/p/v1/threads?token=secret#frag"
            )),
            "POST /v1/host/workspaces/p/v1/threads"
        );
        assert_eq!(
            request_label(&open("GET\n", "/v1/info\r\nRemote host connected")),
            "GET /v1/infoRemote host connected",
            "a forged line break cannot start a new log line"
        );
        assert_eq!(request_label(&open("GET", &"/v1".repeat(200))).len(), 200);
    }
    #[test]
    fn relay_paths_cannot_escape_an_advertised_project_or_chat() {
        let chat = Uuid::now_v7();
        let target = Target {
            url: "http://127.0.0.1:1".into(),
            token: "fixture".into(),
            project: "project".into(),
            thread: Some(chat),
        };
        for path in [
            "/v1/info",
            "/v1/threads",
            "/v1/host/projects",
            "/v1/host/workspaces/other/v1/info",
            "/v1/host/workspaces/project/../v1/info",
            "/v1/host/workspaces/project/v1/%2e%2e/info",
        ] {
            assert!(validate_path(path, &target, Some(chat)).is_err(), "{path}");
        }
        assert!(validate_path("/v1/host/workspaces/project/v1/info", &target, None).is_err());
        assert!(validate_path("/v1/host/workspaces/project/v1/info", &target, Some(chat)).is_ok());
    }

    /// What a fake Haarness server holds; the tests change it as they go.
    #[derive(Default)]
    struct FakeState {
        permissions: Vec<Value>,
        questions: Vec<Value>,
        threads: HashMap<Uuid, Value>,
        /// The turn `/threads/{id}/active` reports, by chat; all of them
        /// are what `/turns/active` lists.
        active: HashMap<Uuid, Value>,
        /// What `/turns/{id}` answers.
        turns: HashMap<Uuid, Value>,
        /// The oldest event still kept: a stream resuming before it is told
        /// to resync and replays nothing, as a server whose buffer moved on.
        kept_from: u64,
        /// How many `/info` requests answer only after a second.
        slow_info: usize,
        /// A published chat's tree, for requests scoped to it.
        tree: HashMap<Uuid, Vec<Uuid>>,
        /// Every request's path and scope header.
        requests: Vec<(String, Option<String>)>,
        event_id: u64,
        /// Every event sent, with its chat, for streams that resume.
        log: Vec<(u64, Uuid, String)>,
        /// How many `/threads/{id}/active` checks fail before one answers.
        active_failures: usize,
        /// The event stream ends as soon as it opens.
        close_events: bool,
        /// Whether `/threads/{id}/activity` exists; a server from before it
        /// answers 404 there.
        activity: bool,
        /// The background results `/threads/{id}/activity` reports, by chat.
        background: HashMap<Uuid, u32>,
        /// The newest turn `/threads/{id}/activity` names, by chat; None for
        /// a server from before `lastTurn`, which leaves it out.
        last_turns: Option<HashMap<Uuid, Value>>,
    }
    /// A Haarness server as the attention watcher sees it: a scope-aware
    /// `/v1/permissions`, `/v1/questions`, chats and an event stream.
    #[derive(Clone)]
    struct FakeDaemon {
        url: String,
        instance: Uuid,
        state: Arc<std::sync::Mutex<FakeState>>,
        events: tokio::sync::broadcast::Sender<(Uuid, String)>,
    }
    impl FakeDaemon {
        async fn start(workspace: &Path) -> Self {
            use axum::{
                Json, Router,
                body::Body,
                extract::{Path as Route, State},
                http::{HeaderMap, StatusCode, Uri},
                response::{IntoResponse, Response},
                routing::get,
            };
            let (events, _) = tokio::sync::broadcast::channel(64);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let daemon = Self {
                url: format!("http://{}", listener.local_addr().unwrap()),
                instance: Uuid::now_v7(),
                state: Arc::default(),
                events,
            };
            let workspace = workspace.to_owned();
            let app = Router::new()
                .route(
                    "/v1/info",
                    get(move |State(daemon): State<FakeDaemon>| async move {
                        let slow = {
                            let mut state = daemon.state.lock().unwrap();
                            let slow = state.slow_info > 0;
                            state.slow_info = state.slow_info.saturating_sub(1);
                            slow
                        };
                        if slow {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                        let cursor = daemon.state.lock().unwrap().event_id;
                        Json(json!({"name":"haarness","version":"0.1.1","schemaVersion":"1",
                            "serverInstanceId":daemon.instance,"workspace":workspace,"pid":std::process::id(),
                            "eventCursor":cursor,"projectTrusted":false,
                            "capabilities":["remote-chat-scope.v1","workspace-history.v1","pty.v1"]}))
                    }),
                )
                .route(
                    "/v1/permissions",
                    get(|State(daemon): State<FakeDaemon>, uri: Uri, headers: HeaderMap| async move {
                        let visible = daemon.visible(&uri, &headers);
                        rows(&daemon.state.lock().unwrap().permissions, visible.as_deref())
                    }),
                )
                .route(
                    "/v1/questions",
                    get(|State(daemon): State<FakeDaemon>, uri: Uri, headers: HeaderMap| async move {
                        let visible = daemon.visible(&uri, &headers);
                        rows(&daemon.state.lock().unwrap().questions, visible.as_deref())
                    }),
                )
                .route(
                    "/v1/threads/{id}",
                    get(
                        |State(daemon): State<FakeDaemon>, Route(id): Route<Uuid>, uri: Uri, headers: HeaderMap| async move {
                            if daemon.visible(&uri, &headers).is_some_and(|ids| !ids.contains(&id)) {
                                return StatusCode::FORBIDDEN.into_response();
                            }
                            let thread = daemon.state.lock().unwrap().threads.get(&id).cloned();
                            thread.map_or(StatusCode::NOT_FOUND.into_response(), |thread| {
                                Json(thread).into_response()
                            })
                        },
                    ),
                )
                .route(
                    "/v1/threads/{id}/active",
                    get(
                        |State(daemon): State<FakeDaemon>, Route(id): Route<Uuid>, uri: Uri, headers: HeaderMap| async move {
                            daemon.visible(&uri, &headers);
                            let mut state = daemon.state.lock().unwrap();
                            if state.active_failures > 0 {
                                state.active_failures -= 1;
                                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                            }
                            Json(state.active.get(&id).cloned().unwrap_or(Value::Null)).into_response()
                        },
                    ),
                )
                .route(
                    "/v1/threads/{id}/activity",
                    get(
                        |State(daemon): State<FakeDaemon>, Route(id): Route<Uuid>, uri: Uri, headers: HeaderMap| async move {
                            daemon.visible(&uri, &headers);
                            let state = daemon.state.lock().unwrap();
                            if !state.activity {
                                return StatusCode::NOT_FOUND.into_response();
                            }
                            let mut activity = json!({
                                "activeTurn": state.active.get(&id).cloned().unwrap_or(Value::Null),
                                "background": state.background.get(&id).copied().unwrap_or(0),
                            });
                            if let Some(last) = &state.last_turns {
                                activity["lastTurn"] = last.get(&id).cloned().unwrap_or(Value::Null);
                            }
                            Json(activity).into_response()
                        },
                    ),
                )
                .route(
                    "/v1/turns/active",
                    get(|State(daemon): State<FakeDaemon>, uri: Uri, headers: HeaderMap| async move {
                        let visible = daemon.visible(&uri, &headers);
                        let active: Vec<Value> = daemon.state.lock().unwrap().active.values().cloned().collect();
                        rows(&active, visible.as_deref())
                    }),
                )
                .route(
                    "/v1/turns/{id}",
                    get(
                        |State(daemon): State<FakeDaemon>, Route(id): Route<Uuid>, uri: Uri, headers: HeaderMap| async move {
                            daemon.visible(&uri, &headers);
                            let turn = daemon.state.lock().unwrap().turns.get(&id).cloned();
                            turn.map_or(StatusCode::NOT_FOUND.into_response(), |turn| Json(turn).into_response())
                        },
                    ),
                )
                .route(
                    "/v1/events",
                    get(|State(daemon): State<FakeDaemon>, uri: Uri, headers: HeaderMap| async move {
                        let visible = daemon.visible(&uri, &headers);
                        // Like a real server: everything after the cursor,
                        // or everything kept when there is none.
                        let after = headers
                            .get("last-event-id")
                            .map(|value| value.to_str().unwrap().parse::<u64>().unwrap());
                        let shown = |thread: &Uuid| visible.as_ref().is_none_or(|ids| ids.contains(thread));
                        let (receiver, replay, close) = {
                            let state = daemon.state.lock().unwrap();
                            let replay = if after.is_some_and(|after| after + 1 < state.kept_from) {
                                vec![Ok("event: resync_required\ndata: {\"type\":\"resync_required\"}\n\n".to_owned())]
                            } else {
                                state
                                    .log
                                    .iter()
                                    .filter(|(id, thread, _)| after.is_none_or(|after| *id > after) && shown(thread))
                                    .map(|(_, _, frame)| Ok(frame.clone()))
                                    .collect::<Vec<_>>()
                            };
                            (daemon.events.subscribe(), replay, state.close_events)
                        };
                        if close {
                            return Response::builder()
                                .header("content-type", "text/event-stream")
                                .body(Body::empty())
                                .unwrap();
                        }
                        let live = futures::stream::unfold(receiver, move |mut receiver| {
                            let visible = visible.clone();
                            async move {
                                loop {
                                    match receiver.recv().await {
                                        // Cut: the stream ends.
                                        Ok((thread, _)) if thread.is_nil() => return None,
                                        Ok((thread, frame))
                                            if visible.as_ref().is_none_or(|ids| ids.contains(&thread)) =>
                                        {
                                            return Some((Ok::<_, std::convert::Infallible>(frame), receiver));
                                        }
                                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                                    }
                                }
                            }
                        });
                        let body = futures::stream::once(async { Ok(": connected\n\n".to_owned()) })
                            .chain(futures::stream::iter(replay))
                            .chain(live);
                        Response::builder()
                            .header("content-type", "text/event-stream")
                            .body(Body::from_stream(body))
                            .unwrap()
                    }),
                )
                .with_state(daemon.clone());
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            daemon
        }
        /// The chats a request may see: all, or a published chat's tree.
        /// Records the request.
        fn visible(
            &self,
            uri: &axum::http::Uri,
            headers: &axum::http::HeaderMap,
        ) -> Option<Vec<Uuid>> {
            let scope = headers
                .get(REMOTE_THREAD_HEADER)
                .map(|value| value.to_str().unwrap().to_owned());
            let mut state = self.state.lock().unwrap();
            state.requests.push((uri.path().to_owned(), scope.clone()));
            let root = scope?.parse::<Uuid>().unwrap();
            Some(state.tree.get(&root).cloned().unwrap_or_default())
        }
        fn emit(&self, kind: &str, thread: Uuid, root: Uuid, payload: Value) {
            let mut state = self.state.lock().unwrap();
            state.event_id += 1;
            let id = state.event_id;
            let envelope = json!({"schemaVersion":"1","serverInstanceId":self.instance,"eventId":id,
                "type":kind,"threadId":thread,"rootThreadId":root,"turnId":null,"itemId":null,
                "payload":payload,"createdAt":chrono::Utc::now()});
            let frame = format!("id: {id}\nevent: {kind}\ndata: {envelope}\n\n");
            state.log.push((id, thread, frame.clone()));
            // Sent while the state is locked, so a stream opening now
            // either replays it or receives it, never both.
            let _ = self.events.send((thread, frame));
        }
        /// End every open event stream, as a server does when its client
        /// falls too far behind.
        fn cut(&self) {
            let _ = self.events.send((Uuid::nil(), String::new()));
        }
        /// How many requests asked for `path`.
        fn requested(&self, path: &str) -> usize {
            let state = self.state.lock().unwrap();
            state
                .requests
                .iter()
                .filter(|(asked, _)| asked == path)
                .count()
        }
        /// Wait until more than `count` requests asked for `path`.
        async fn requested_more(&self, path: &str, count: usize) {
            tokio::time::timeout(Duration::from_secs(10), async {
                while self.requested(path) <= count {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("{path} was not requested again"));
        }
        /// Write this server's discovery record, started by `executable`.
        fn record(
            &self,
            paths: &Paths,
            workspace_id: &str,
            workspace: &Path,
            executable: Option<String>,
        ) {
            let folder = paths.cache.join("workspaces").join(workspace_id);
            std::fs::create_dir_all(&folder).unwrap();
            let record = crate::rendezvous::RuntimeDiscovery {
                url: self.url.clone(),
                token: "fixture".into(),
                pid: std::process::id(),
                workspace: Some(workspace.display().to_string()),
                executable_fingerprint: executable,
                access_fingerprint: Some(
                    haarness_server::auth::ServerAccess::Private.fingerprint("fixture"),
                ),
                trace_endpoint: None,
            };
            crate::write_private_json(&folder.join("runtime.json"), &record).unwrap();
        }
        fn target(&self, project: &str, thread: Option<Uuid>) -> Target {
            Target {
                url: self.url.clone(),
                token: "fixture".into(),
                project: project.into(),
                thread,
            }
        }
    }
    /// Pending rows as a server lists them: all, or those of `visible` chats.
    fn rows(rows: &[Value], visible: Option<&[Uuid]>) -> axum::Json<Value> {
        axum::Json(Value::Array(
            rows.iter()
                .filter(|row| {
                    visible.is_none_or(|ids| ids.iter().any(|id| row["threadId"] == json!(id)))
                })
                .cloned()
                .collect(),
        ))
    }
    fn chat(id: Uuid, title: &str) -> Value {
        json!({"id":id,"title":title,"parentId":null,"archived":false,"agent":"build",
            "provider":"echo","model":"echo-1","createdAt":chrono::Utc::now(),"updatedAt":chrono::Utc::now()})
    }
    fn pending_permission(id: &str, thread: Uuid, root: Uuid) -> Value {
        json!({"permissionId":id,"operation":"bash:git push origin main","permission":"bash",
            "detail":{"command":"git push origin main"},"patterns":["git push origin main"],
            "alwaysPatterns":["bash:git push *"],"threadId":thread,"rootThreadId":root,
            "turnId":Uuid::now_v7(),"explicit":false,"createdAt":chrono::Utc::now()})
    }
    fn pending_question(id: &str, thread: Uuid, root: Uuid) -> Value {
        json!({"questionId":id,"prompt":"Which password should I use?","choices":[],"custom":true,
            "threadId":thread,"rootThreadId":root,"createdAt":chrono::Utc::now()})
    }
    fn finished_turn(id: Uuid, thread: Uuid, status: &str) -> Value {
        json!({"id":id,"threadId":thread,"requestId":Uuid::now_v7(),"status":status,"agent":"build",
            "provider":"echo","model":"echo-1","error":null,
            "createdAt":chrono::Utc::now(),"updatedAt":chrono::Utc::now()})
    }
    /// The next attention frame the relay recorded.
    async fn next_attention(frames: &mut mpsc::UnboundedReceiver<Value>) -> Value {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let frame = frames.recv().await.expect("relay is recording");
                if frame["type"] == "attention" {
                    return frame;
                }
            }
        })
        .await
        .expect("an attention frame arrives")
    }
    /// A data root with three registered projects: `home`, the connector's,
    /// `other`, whose server an older build started, and `stopped`, which
    /// has no server.
    struct Computer {
        root: std::path::PathBuf,
        paths: Paths,
        home: haarness_protocol::WorkspaceInfo,
        other: haarness_protocol::WorkspaceInfo,
        stopped: haarness_protocol::WorkspaceInfo,
    }
    impl Computer {
        fn new() -> Self {
            let root = crate::browser::discovery::tests::temp("remote-attention");
            let base = Paths::new(Some(&root)).unwrap();
            let manager = base.workspace_manager().unwrap();
            let register = |name: &str| {
                let folder = root.join(name);
                std::fs::create_dir_all(&folder).unwrap();
                manager.register_local(&folder).unwrap()
            };
            let (home, other, stopped) = (register("home"), register("other"), register("stopped"));
            let paths = base.scoped(&home.id, false).unwrap();
            Self {
                root,
                paths,
                home,
                other,
                stopped,
            }
        }
    }
    #[tokio::test]
    async fn a_relay_announcing_attention_hears_what_waits_on_every_running_project() {
        let computer = Computer::new();
        let home = FakeDaemon::start(Path::new(&computer.home.path)).await;
        let other = FakeDaemon::start(Path::new(&computer.other.path)).await;
        home.record(
            &computer.paths,
            &computer.home.id,
            Path::new(&computer.home.path),
            crate::current_executable_fingerprint().ok(),
        );
        other.record(
            &computer.paths,
            &computer.other.id,
            Path::new(&computer.other.path),
            Some("an-older-build".into()),
        );
        let (login, upgrade, helper, looping) = (
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
        );
        {
            let mut state = home.state.lock().unwrap();
            state
                .permissions
                .push(pending_permission("perm-1", login, login));
            state.threads.insert(login, chat(login, "Fix login"));
            state.threads.insert(looping, chat(looping, "Keep going"));
        }
        {
            let mut state = other.state.lock().unwrap();
            state
                .questions
                .push(pending_question("question-1", helper, upgrade));
            state.threads.insert(upgrade, chat(upgrade, "New chat"));
        }
        let (relay, mut frames) = fake_relay(vec![Relay::Record(&["attention.v1"])]).await;
        let attention = attention(&computer.paths);
        let target = home.target(&computer.home.id, None);
        let http = reqwest::Client::new();
        let link = tokio::spawn(async move {
            relay_link(
                relay.into_client_request().unwrap(),
                &target,
                &http,
                Duration::from_secs(60),
                RELAY_SILENCE,
                &attention,
                &mut Outage::new(|_| {}),
            )
            .await
        });

        let first = next_attention(&mut frames).await;
        assert_eq!(first["revision"], 1);
        assert_eq!(first["finished"], json!([]));
        let items = first["items"].as_array().unwrap();
        assert_eq!(items.len(), 2, "{first}");
        let permission = &items[0];
        assert_eq!(permission["kind"], "permission");
        assert_eq!(permission["id"], "perm-1");
        assert_eq!(permission["projectId"], computer.home.id.as_str());
        assert_eq!(permission["projectName"], "home");
        assert_eq!(permission["rootThreadId"], json!(login));
        assert_eq!(permission["chatTitle"], "Fix login");
        assert_eq!(permission["category"], "bash");
        assert_eq!(permission["actionable"], true);
        let question = &items[1];
        assert_eq!(question["kind"], "question");
        assert_eq!(question["projectId"], computer.other.id.as_str());
        assert_eq!(
            (&question["threadId"], &question["rootThreadId"]),
            (&json!(helper), &json!(upgrade))
        );
        assert_eq!(question["chatTitle"], Value::Null, "an untitled chat");
        assert_eq!(question["custom"], true);
        assert_eq!(question["category"], Value::Null);
        // An older build's server would be replaced, not reused, so Cloud cannot answer it.
        assert_eq!(question["actionable"], false);
        let text = first.to_string();
        for private in [
            "git push",
            "password",
            "operation",
            "patterns",
            "detail",
            "prompt",
        ] {
            assert!(
                !text.contains(private),
                "{private} left the computer: {text}"
            );
        }

        // Answered on the computer: Cloud hears it is gone.
        home.state.lock().unwrap().permissions.clear();
        home.emit(
            "permission.resolved",
            login,
            login,
            json!({"permissionId":"perm-1","allowed":true}),
        );
        let resolved = next_attention(&mut frames).await;
        assert_eq!(resolved["revision"], 2);
        assert_eq!(resolved["items"].as_array().unwrap().len(), 1);

        // A helper agent's turn, a turn a loop continues, and a root turn
        // that stays finished: only the last one is reported.
        let (helper_turn, continued, done) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        other.emit(
            "turn.finished",
            helper,
            upgrade,
            finished_turn(helper_turn, helper, "completed"),
        );
        home.state
            .lock()
            .unwrap()
            .active
            .insert(looping, finished_turn(Uuid::now_v7(), looping, "running"));
        home.emit(
            "turn.finished",
            looping,
            looping,
            finished_turn(continued, looping, "completed"),
        );
        home.emit(
            "turn.finished",
            login,
            login,
            finished_turn(done, login, "failed"),
        );
        let finished = next_attention(&mut frames).await;
        assert_eq!(
            finished["finished"]
                .as_array()
                .unwrap()
                .iter()
                .map(|turn| (
                    &turn["turnId"],
                    &turn["threadId"],
                    &turn["status"],
                    &turn["chatTitle"],
                    &turn["scheduled"]
                ))
                .collect::<Vec<_>>(),
            [(
                &json!(done),
                &json!(login),
                &json!("failed"),
                &json!("Fix login"),
                &json!(false)
            )],
            "{finished}"
        );
        // Nothing changes after: no further frame, and the turn is not reported twice.
        tokio::time::sleep(Duration::from_millis(800)).await;
        while let Ok(frame) = frames.try_recv() {
            assert_ne!(frame["type"], "attention", "{frame}");
        }
        // Watching never started a server for the project that has none.
        assert!(
            !computer
                .paths
                .cache
                .join("workspaces")
                .join(&computer.stopped.id)
                .join("runtime.json")
                .exists()
        );
        link.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    #[tokio::test]
    async fn a_relay_that_does_not_announce_attention_never_receives_it() {
        let computer = Computer::new();
        let home = FakeDaemon::start(Path::new(&computer.home.path)).await;
        home.record(
            &computer.paths,
            &computer.home.id,
            Path::new(&computer.home.path),
            crate::current_executable_fingerprint().ok(),
        );
        let login = Uuid::now_v7();
        home.state
            .lock()
            .unwrap()
            .permissions
            .push(pending_permission("perm-1", login, login));
        let (relay, mut frames) = fake_relay(vec![Relay::Record(&[])]).await;
        let attention = attention(&computer.paths);
        let target = home.target(&computer.home.id, None);
        let http = reqwest::Client::new();
        let link = tokio::spawn(async move {
            relay_link(
                relay.into_client_request().unwrap(),
                &target,
                &http,
                Duration::from_secs(60),
                RELAY_SILENCE,
                &attention,
                &mut Outage::new(|_| {}),
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(1500)).await;
        home.emit(
            "permission.resolved",
            login,
            login,
            json!({"permissionId":"perm-1"}),
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!link.is_finished(), "the connection stays up");
        while let Ok(frame) = frames.try_recv() {
            assert_ne!(frame["type"], "attention", "{frame}");
        }
        assert!(
            home.state.lock().unwrap().requests.is_empty(),
            "nothing watched the server"
        );
        link.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    #[tokio::test]
    async fn a_shared_chat_reports_only_its_own_tree() {
        let computer = Computer::new();
        let home = FakeDaemon::start(Path::new(&computer.home.path)).await;
        let (shared, child, grandchild, unrelated) = (
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
        );
        {
            let mut state = home.state.lock().unwrap();
            state.tree.insert(shared, vec![shared, child, grandchild]);
            state.threads.insert(shared, chat(shared, "Shared work"));
            state.threads.insert(unrelated, chat(unrelated, "Private"));
            state
                .permissions
                .push(pending_permission("in-tree", grandchild, shared));
            state
                .permissions
                .push(pending_permission("elsewhere", unrelated, unrelated));
            state
                .questions
                .push(pending_question("asked-elsewhere", unrelated, unrelated));
        }
        let (relay, mut frames) = fake_relay(vec![Relay::Record(&["attention.v1"])]).await;
        let attention = attention(&computer.paths);
        let target = home.target(&computer.home.id, Some(shared));
        let http = reqwest::Client::new();
        let link = tokio::spawn(async move {
            relay_link(
                relay.into_client_request().unwrap(),
                &target,
                &http,
                Duration::from_secs(60),
                RELAY_SILENCE,
                &attention,
                &mut Outage::new(|_| {}),
            )
            .await
        });
        let first = next_attention(&mut frames).await;
        let items = first["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "{first}");
        assert_eq!(items[0]["id"], "in-tree");
        assert_eq!(items[0]["threadId"], json!(grandchild));
        assert_eq!(items[0]["rootThreadId"], json!(shared));
        assert_eq!(items[0]["projectId"], computer.home.id.as_str());
        assert_eq!(items[0]["projectName"], "home");
        assert_eq!(items[0]["chatTitle"], "Shared work");
        assert_eq!(items[0]["actionable"], true);

        let done = Uuid::now_v7();
        home.emit(
            "turn.finished",
            unrelated,
            unrelated,
            finished_turn(Uuid::now_v7(), unrelated, "completed"),
        );
        home.emit(
            "turn.finished",
            shared,
            shared,
            finished_turn(done, shared, "completed"),
        );
        let finished = next_attention(&mut frames).await;
        let turns = finished["finished"].as_array().unwrap();
        assert_eq!(turns.len(), 1, "{finished}");
        assert_eq!(
            (&turns[0]["turnId"], &turns[0]["threadId"]),
            (&json!(done), &json!(shared))
        );
        // Every request was scoped to the shared chat, as Cloud's are.
        let requests = home.state.lock().unwrap().requests.clone();
        assert!(requests.iter().any(|(path, _)| path == "/v1/events"));
        assert!(
            requests
                .iter()
                .all(|(_, scope)| scope.as_deref() == Some(shared.to_string().as_str())),
            "{requests:?}"
        );
        link.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    /// The connector's link to `relay`, reporting for the projects of
    /// `attention` and for `target`.
    fn link(
        relay: &str,
        target: &Target,
        attention: &Arc<remote_attention::Context>,
    ) -> tokio::task::JoinHandle<Result<Link>> {
        let (handshake, target, attention) = (
            relay.into_client_request().unwrap(),
            target.clone(),
            attention.clone(),
        );
        tokio::spawn(async move {
            relay_link(
                handshake,
                &target,
                &reqwest::Client::new(),
                Duration::from_secs(60),
                RELAY_SILENCE,
                &attention,
                &mut Outage::new(|_| {}),
            )
            .await
        })
    }
    /// The turn ids of a frame's finished chats.
    fn finished_turns(frame: &Value) -> Vec<Value> {
        frame["finished"]
            .as_array()
            .unwrap()
            .iter()
            .map(|turn| turn["turnId"].clone())
            .collect()
    }
    /// A computer whose only running server is the connector's own, with
    /// these chats.
    async fn watched_home(chats: &[(Uuid, &str)]) -> (Computer, FakeDaemon) {
        let computer = Computer::new();
        let home = FakeDaemon::start(Path::new(&computer.home.path)).await;
        home.record(
            &computer.paths,
            &computer.home.id,
            Path::new(&computer.home.path),
            crate::current_executable_fingerprint().ok(),
        );
        let mut state = home.state.lock().unwrap();
        for (id, title) in chats {
            state.threads.insert(*id, chat(*id, title));
        }
        drop(state);
        (computer, home)
    }
    #[tokio::test]
    async fn turns_count_from_when_the_connector_watches_and_survive_its_reconnects() {
        let (earlier, dropped, offline) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let (computer, home) = watched_home(&[
            (earlier, "Earlier"),
            (dropped, "Dropped"),
            (offline, "Offline"),
        ])
        .await;
        // It finished before the connector watched the server: not news.
        let before = Uuid::now_v7();
        home.emit(
            "turn.finished",
            earlier,
            earlier,
            finished_turn(before, earlier, "completed"),
        );
        let (relay, mut frames) = fake_relay(vec![
            Relay::Record(&["attention.v1"]),
            Relay::Record(&["attention.v1"]),
        ])
        .await;
        let attention = attention(&computer.paths);
        let target = home.target(&computer.home.id, None);
        let first = link(&relay, &target, &attention);
        assert_eq!(next_attention(&mut frames).await["finished"], json!([]));

        // One turn finishes as the connection drops, before it settled, and
        // one while the connector is away.
        let (settling, away) = (Uuid::now_v7(), Uuid::now_v7());
        let refreshes = home.requested("/v1/permissions");
        home.emit(
            "turn.finished",
            dropped,
            dropped,
            finished_turn(settling, dropped, "completed"),
        );
        // Read: it refreshes what waits whenever a turn finishes.
        home.requested_more("/v1/permissions", refreshes).await;
        first.abort();
        let _ = first.await;
        home.emit(
            "turn.finished",
            offline,
            offline,
            finished_turn(away, offline, "failed"),
        );

        let second = link(&relay, &target, &attention);
        let mut seen = Vec::new();
        let last = loop {
            let frame = next_attention(&mut frames).await;
            seen.push(frame.clone());
            if frame["finished"].as_array().unwrap().len() == 2 {
                break frame;
            }
        };
        let mut reported = finished_turns(&last);
        reported.sort_by_key(ToString::to_string);
        let mut expected = vec![json!(settling), json!(away)];
        expected.sort_by_key(ToString::to_string);
        assert_eq!(reported, expected, "{last}");
        assert!(
            seen.iter()
                .all(|frame| !finished_turns(frame).contains(&json!(before))),
            "{seen:?}"
        );
        tokio::time::sleep(Duration::from_millis(800)).await;
        while let Ok(frame) = frames.try_recv() {
            assert_ne!(frame["type"], "attention", "{frame}");
        }
        second.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    #[tokio::test]
    async fn a_finished_turn_is_checked_again_when_the_server_does_not_answer() {
        let work = Uuid::now_v7();
        let (computer, home) = watched_home(&[(work, "Work")]).await;
        // The first two checks fail; the last one allowed answers.
        home.state.lock().unwrap().active_failures = 2;
        let (relay, mut frames) = fake_relay(vec![Relay::Record(&["attention.v1"])]).await;
        let attention = attention(&computer.paths);
        let connection = link(&relay, &home.target(&computer.home.id, None), &attention);
        next_attention(&mut frames).await;
        let done = Uuid::now_v7();
        home.emit(
            "turn.finished",
            work,
            work,
            finished_turn(done, work, "completed"),
        );
        let frame = next_attention(&mut frames).await;
        assert_eq!(finished_turns(&frame), [json!(done)], "{frame}");
        assert_eq!(home.requested(&format!("/v1/threads/{work}/active")), 3);
        connection.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    #[tokio::test]
    async fn only_the_last_of_a_chats_finished_turns_is_reported() {
        let work = Uuid::now_v7();
        let (computer, home) = watched_home(&[(work, "Work")]).await;
        let (relay, mut frames) = fake_relay(vec![Relay::Record(&["attention.v1"])]).await;
        let attention = attention(&computer.paths);
        let connection = link(&relay, &home.target(&computer.home.id, None), &attention);
        next_attention(&mut frames).await;
        // A goal ran a second turn at once; both ended before either settled.
        let (first, last) = (Uuid::now_v7(), Uuid::now_v7());
        home.emit(
            "turn.finished",
            work,
            work,
            finished_turn(first, work, "completed"),
        );
        home.emit(
            "turn.finished",
            work,
            work,
            finished_turn(last, work, "completed"),
        );
        let frame = next_attention(&mut frames).await;
        assert_eq!(finished_turns(&frame), [json!(last)], "{frame}");
        tokio::time::sleep(Duration::from_millis(800)).await;
        while let Ok(frame) = frames.try_recv() {
            assert_ne!(frame["type"], "attention", "{frame}");
        }
        connection.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    #[tokio::test]
    async fn a_server_that_keeps_closing_its_event_stream_is_asked_less_and_less_often() {
        let (computer, home) = watched_home(&[]).await;
        home.state.lock().unwrap().close_events = true;
        let (relay, _frames) = fake_relay(vec![Relay::Record(&["attention.v1"])]).await;
        let attention = attention(&computer.paths);
        let connection = link(&relay, &home.target(&computer.home.id, None), &attention);
        home.requested_more("/v1/events", 0).await;
        tokio::time::sleep(Duration::from_millis(1600)).await;
        // Waits of about 0.25, 0.5 and 1 s; reopening at once every 0.25 s
        // would be seven.
        let opened = home.requested("/v1/events");
        assert!((2..=4).contains(&opened), "{opened} streams in 1.6 s");
        connection.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }

    /// The turn ids a frame reports finished, in a stable order.
    fn finished_set(frame: &Value) -> Vec<String> {
        let mut turns: Vec<String> = finished_turns(frame)
            .iter()
            .map(ToString::to_string)
            .collect();
        turns.sort();
        turns
    }
    #[tokio::test]
    async fn a_turn_that_ends_while_cloud_is_unreachable_is_reported_after_the_servers_buffer_moved_on()
     {
        let work = Uuid::now_v7();
        let (computer, home) = watched_home(&[(work, "Work")]).await;
        let (relay, mut frames) = fake_relay(vec![
            Relay::Record(&["attention.v1"]),
            Relay::Record(&["attention.v1"]),
        ])
        .await;
        let attention = attention(&computer.paths);
        let target = home.target(&computer.home.id, None);
        let first = link(&relay, &target, &attention);
        next_attention(&mut frames).await;
        first.abort();
        let _ = first.await;
        // Cloud is away. The turn ends, and so many events follow (a chat
        // streaming its answer) that the server keeps none from before.
        let done = Uuid::now_v7();
        let checks = home.requested(&format!("/v1/threads/{work}/active"));
        home.emit(
            "turn.finished",
            work,
            work,
            finished_turn(done, work, "completed"),
        );
        home.requested_more(&format!("/v1/threads/{work}/active"), checks)
            .await;
        {
            let mut state = home.state.lock().unwrap();
            state.event_id += 3000;
            state.kept_from = state.event_id;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let second = link(&relay, &target, &attention);
        let frame = next_attention(&mut frames).await;
        assert_eq!(finished_turns(&frame), [json!(done)], "{frame}");
        second.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    #[tokio::test]
    async fn root_turns_that_end_while_the_servers_events_are_missed_are_still_reported() {
        let (seeded, started, helper) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let (computer, home) =
            watched_home(&[(seeded, "Seeded"), (started, "Started"), (helper, "Helper")]).await;
        home.state.lock().unwrap().threads.get_mut(&helper).unwrap()["parentId"] = json!(started);
        // One root turn already runs when the connector starts watching, and
        // so does a helper agent's.
        let (early, late, assisting) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        {
            let mut state = home.state.lock().unwrap();
            state
                .active
                .insert(seeded, finished_turn(early, seeded, "running"));
            state
                .active
                .insert(helper, finished_turn(assisting, helper, "running"));
        }
        let (relay, mut frames) = fake_relay(vec![Relay::Record(&["attention.v1"])]).await;
        let attention = attention(&computer.paths);
        let connection = link(&relay, &home.target(&computer.home.id, None), &attention);
        next_attention(&mut frames).await;
        // Another starts once it watches; a prompt after it shows the start was read.
        home.emit(
            "turn.started",
            started,
            started,
            finished_turn(late, started, "running"),
        );
        home.state
            .lock()
            .unwrap()
            .permissions
            .push(pending_permission("perm-1", started, started));
        home.emit("permission.requested", started, started, json!({}));
        let asked = next_attention(&mut frames).await;
        assert_eq!(asked["items"].as_array().unwrap().len(), 1, "{asked}");
        // All three end where the connector cannot see it: its stream is cut
        // and the server keeps none of those events.
        {
            let mut state = home.state.lock().unwrap();
            state.active.clear();
            state.permissions.clear();
            state
                .turns
                .insert(early, finished_turn(early, seeded, "completed"));
            state
                .turns
                .insert(late, finished_turn(late, started, "failed"));
            state
                .turns
                .insert(assisting, finished_turn(assisting, helper, "completed"));
            state.event_id += 3000;
            state.kept_from = state.event_id;
        }
        home.cut();
        let frame = loop {
            let frame = next_attention(&mut frames).await;
            if !finished_turns(&frame).is_empty() {
                break frame;
            }
        };
        let mut expected = vec![json!(early).to_string(), json!(late).to_string()];
        expected.sort();
        // Both may settle in one frame or in two.
        let frame = if finished_set(&frame) == expected {
            frame
        } else {
            next_attention(&mut frames).await
        };
        assert_eq!(finished_set(&frame), expected, "{frame}");
        assert_eq!(frame["items"], json!([]), "{frame}");
        assert_eq!(home.requested(&format!("/v1/turns/{assisting}")), 0);
        connection.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    #[tokio::test]
    async fn a_server_that_answers_late_is_waited_for_by_the_first_snapshot() {
        let computer = Computer::new();
        let home = FakeDaemon::start(Path::new(&computer.home.path)).await;
        let other = FakeDaemon::start(Path::new(&computer.other.path)).await;
        home.record(
            &computer.paths,
            &computer.home.id,
            Path::new(&computer.home.path),
            crate::current_executable_fingerprint().ok(),
        );
        other.record(
            &computer.paths,
            &computer.other.id,
            Path::new(&computer.other.path),
            crate::current_executable_fingerprint().ok(),
        );
        let (upgrade, helper) = (Uuid::now_v7(), Uuid::now_v7());
        {
            // Busy: its first answer misses the probe's time limit.
            let mut state = other.state.lock().unwrap();
            state.slow_info = 1;
            state
                .questions
                .push(pending_question("question-1", helper, upgrade));
            state.threads.insert(upgrade, chat(upgrade, "Upgrade"));
        }
        let (relay, mut frames) = fake_relay(vec![Relay::Record(&["attention.v1"])]).await;
        let attention = attention(&computer.paths);
        let connection = link(&relay, &home.target(&computer.home.id, None), &attention);
        let first = next_attention(&mut frames).await;
        assert_eq!(first["revision"], 1);
        let items = first["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "{first}");
        assert_eq!(items[0]["id"], "question-1");
        assert_eq!(items[0]["projectId"], computer.other.id.as_str());
        connection.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    #[tokio::test]
    async fn servers_of_the_old_build_cannot_be_answered_from_the_phone_after_an_in_place_upgrade()
    {
        let computer = Computer::new();
        let home = FakeDaemon::start(Path::new(&computer.home.path)).await;
        let other = FakeDaemon::start(Path::new(&computer.other.path)).await;
        home.record(
            &computer.paths,
            &computer.home.id,
            Path::new(&computer.home.path),
            Some("build-1".into()),
        );
        other.record(
            &computer.paths,
            &computer.other.id,
            Path::new(&computer.other.path),
            Some("build-1".into()),
        );
        let (fix, upgrade) = (Uuid::now_v7(), Uuid::now_v7());
        home.state
            .lock()
            .unwrap()
            .permissions
            .push(pending_permission("perm-1", fix, fix));
        other.state.lock().unwrap().questions.push(pending_question(
            "question-1",
            upgrade,
            upgrade,
        ));
        let build = Arc::new(std::sync::Mutex::new("build-1".to_owned()));
        let installed = build.clone();
        let attention = remote_attention::Context::with_executable(
            &computer.paths,
            false,
            remote_attention::Timing {
                debounce: Duration::from_millis(50),
                settle: Duration::from_millis(300),
                scan: Duration::from_millis(100),
                first: Duration::from_secs(5),
            },
            Arc::new(move || Some(installed.lock().unwrap().clone())),
        );
        let (relay, mut frames) = fake_relay(vec![Relay::Record(&["attention.v1"])]).await;
        let connection = link(&relay, &home.target(&computer.home.id, None), &attention);
        let actionable = |frame: &Value| -> Vec<(String, bool)> {
            let mut items: Vec<(String, bool)> = frame["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| {
                    (
                        item["id"].as_str().unwrap().to_owned(),
                        item["actionable"] == true,
                    )
                })
                .collect();
            items.sort();
            items
        };
        let first = next_attention(&mut frames).await;
        assert_eq!(
            actionable(&first),
            [("perm-1".to_owned(), true), ("question-1".to_owned(), true)],
            "{first}"
        );
        // Haarness is upgraded in place while the connector keeps running:
        // the gateway would now replace the other project's server.
        *build.lock().unwrap() = "build-2".into();
        let upgraded = next_attention(&mut frames).await;
        assert_eq!(
            actionable(&upgraded),
            [
                ("perm-1".to_owned(), true),
                ("question-1".to_owned(), false)
            ],
            "{upgraded}"
        );
        connection.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    #[tokio::test]
    async fn a_registry_that_cannot_be_read_holds_up_nothing_but_the_next_match() {
        let work = Uuid::now_v7();
        let (computer, home) = watched_home(&[(work, "Work")]).await;
        let (relay, mut frames) = fake_relay(vec![Relay::Record(&["attention.v1"])]).await;
        let attention = attention(&computer.paths);
        let connection = link(&relay, &home.target(&computer.home.id, None), &attention);
        next_attention(&mut frames).await;
        // Another process holds the registry (creating a worktree, say), and
        // the projects closed for remote access change, so it is read again.
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(computer.paths.data.join("workspaces.lock"))
            .unwrap();
        fs2::FileExt::lock_exclusive(&lock).unwrap();
        std::fs::create_dir_all(computer.paths.config_dir()).unwrap();
        std::fs::write(crate::remote::hidden_projects_file(&computer.paths), "[]").unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        home.state
            .lock()
            .unwrap()
            .permissions
            .push(pending_permission("perm-1", work, work));
        home.emit("permission.requested", work, work, json!({}));
        let frame = tokio::time::timeout(Duration::from_secs(3), next_attention(&mut frames))
            .await
            .expect("what waits is reported while the registry is held");
        assert_eq!(frame["items"][0]["id"], "perm-1", "{frame}");
        drop(lock);
        connection.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    /// A `turn.finished` payload that says background work still owes the
    /// chat `background` results.
    fn owing_turn(id: Uuid, thread: Uuid, status: &str, background: u32) -> Value {
        let mut turn = finished_turn(id, thread, status);
        turn["background"] = json!(background);
        turn
    }
    /// The next attention frame for which `wanted` holds.
    async fn attention_where(
        frames: &mut mpsc::UnboundedReceiver<Value>,
        wanted: impl Fn(&Value) -> bool,
    ) -> Value {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let frame = next_attention(frames).await;
                if wanted(&frame) {
                    return frame;
                }
            }
        })
        .await
        .expect("the attention frame arrives")
    }
    #[tokio::test]
    async fn a_chat_with_background_work_is_reported_once_its_last_result_is_in() {
        let work = Uuid::now_v7();
        let (computer, home) = watched_home(&[(work, "Work")]).await;
        home.state.lock().unwrap().activity = true;
        let (relay, mut frames) =
            fake_relay(vec![Relay::Record(&["attention.v1", "attention.working"])]).await;
        let attention = attention(&computer.paths);
        let connection = link(&relay, &home.target(&computer.home.id, None), &attention);
        let first = next_attention(&mut frames).await;
        assert_eq!(first["working"], json!([]), "{first}");

        // The turn launches a child agent in the background and ends.
        let (launch, delivered) = (Uuid::now_v7(), Uuid::now_v7());
        home.emit(
            "turn.started",
            work,
            work,
            finished_turn(launch, work, "running"),
        );
        let started = attention_where(&mut frames, |frame| frame["working"] == json!([work])).await;
        assert_eq!(started["finished"], json!([]));
        home.state.lock().unwrap().background.insert(work, 1);
        home.emit(
            "turn.finished",
            work,
            work,
            owing_turn(launch, work, "completed", 1),
        );
        // Not the end: nothing is reported however long it settles, and the
        // chat is still working.
        tokio::time::sleep(Duration::from_millis(800)).await;
        while let Ok(frame) = frames.try_recv() {
            assert_eq!(frame["finished"], json!([]), "{frame}");
            assert_eq!(frame["working"], json!([work]), "{frame}");
        }

        // The child's result arrives as a new turn, which is the end.
        home.state.lock().unwrap().background.remove(&work);
        home.emit(
            "turn.started",
            work,
            work,
            finished_turn(delivered, work, "running"),
        );
        home.emit(
            "turn.finished",
            work,
            work,
            finished_turn(delivered, work, "completed"),
        );
        let done = attention_where(&mut frames, |frame| !finished_turns(frame).is_empty()).await;
        assert_eq!(finished_turns(&done), [json!(delivered)], "{done}");
        assert_eq!(done["finished"][0]["status"], "completed");
        assert_eq!(done["working"], json!([]), "{done}");
        tokio::time::sleep(Duration::from_millis(800)).await;
        while let Ok(frame) = frames.try_recv() {
            assert_ne!(frame["type"], "attention", "reported once: {frame}");
        }
        connection.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    #[tokio::test]
    async fn a_stopped_chat_is_reported_stopped_unless_it_went_straight_on() {
        let (stopped, continued) = (Uuid::now_v7(), Uuid::now_v7());
        let (computer, home) =
            watched_home(&[(stopped, "Stopped"), (continued, "Continued")]).await;
        home.state.lock().unwrap().activity = true;
        let (relay, mut frames) =
            fake_relay(vec![Relay::Record(&["attention.v1", "attention.working"])]).await;
        let attention = attention(&computer.paths);
        let connection = link(&relay, &home.target(&computer.home.id, None), &attention);
        next_attention(&mut frames).await;
        // A stop owns what the chat launched: even a count of background
        // work that is still being cancelled does not hold the report back.
        let (stop, answered) = (Uuid::now_v7(), Uuid::now_v7());
        home.state.lock().unwrap().background.insert(stopped, 1);
        home.emit(
            "turn.finished",
            stopped,
            stopped,
            owing_turn(stop, stopped, "interrupted", 1),
        );
        // A stop that left steering unread goes on with its answer at once.
        home.state.lock().unwrap().active.insert(
            continued,
            finished_turn(Uuid::now_v7(), continued, "running"),
        );
        home.emit(
            "turn.finished",
            continued,
            continued,
            finished_turn(answered, continued, "interrupted"),
        );
        let frame = attention_where(&mut frames, |frame| !finished_turns(frame).is_empty()).await;
        assert_eq!(finished_turns(&frame), [json!(stop)], "{frame}");
        assert_eq!(frame["finished"][0]["status"], "interrupted");
        assert_eq!(frame["finished"][0]["chatTitle"], "Stopped");
        // Its work still owes it a result: it is working all the same.
        assert_eq!(frame["working"], json!([stopped]), "{frame}");
        tokio::time::sleep(Duration::from_millis(800)).await;
        while let Ok(frame) = frames.try_recv() {
            assert_ne!(frame["type"], "attention", "{frame}");
        }
        connection.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    #[tokio::test]
    async fn working_chats_go_only_to_a_relay_that_asked_for_them() {
        let work = Uuid::now_v7();
        let (computer, home) = watched_home(&[(work, "Work")]).await;
        home.state.lock().unwrap().activity = true;
        let (relay, mut frames) = fake_relay(vec![Relay::Record(&["attention.v1"])]).await;
        let attention = attention(&computer.paths);
        let connection = link(&relay, &home.target(&computer.home.id, None), &attention);
        let first = next_attention(&mut frames).await;
        let mut keys = first.as_object().unwrap().keys().collect::<Vec<_>>();
        keys.sort();
        assert_eq!(keys, ["finished", "items", "revision", "type"], "{first}");
        let running = Uuid::now_v7();
        home.emit(
            "turn.started",
            work,
            work,
            finished_turn(running, work, "running"),
        );
        home.state.lock().unwrap().background.insert(work, 2);
        home.emit(
            "turn.finished",
            work,
            work,
            owing_turn(running, work, "completed", 2),
        );
        // A chat starting or owing work changes nothing a relay that did not
        // ask for working chats is told.
        tokio::time::sleep(Duration::from_millis(800)).await;
        while let Ok(frame) = frames.try_recv() {
            assert_ne!(frame["type"], "attention", "{frame}");
        }
        connection.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    /// A helper agent's `task.updated` in `state`.
    fn task_update(state: &str) -> Value {
        json!({"callId":"call-1","childThreadId":Uuid::now_v7(),"childTurnId":Uuid::now_v7(),
            "state":state,"toolCalls":1,"lastTool":"bash","startedAt":chrono::Utc::now()})
    }
    /// A watched chat whose turn just ended while one helper still works for
    /// it, on a relay that asked for working chats; `names_last`: the server
    /// names each chat's newest turn.
    async fn chat_held_for_background_work(
        names_last: bool,
    ) -> (
        Computer,
        FakeDaemon,
        mpsc::UnboundedReceiver<Value>,
        tokio::task::JoinHandle<Result<Link>>,
        Uuid,
        Uuid,
    ) {
        let work = Uuid::now_v7();
        let (computer, home) = watched_home(&[(work, "Work")]).await;
        home.state.lock().unwrap().activity = true;
        let (relay, mut frames) =
            fake_relay(vec![Relay::Record(&["attention.v1", "attention.working"])]).await;
        let attention = attention(&computer.paths);
        let connection = link(&relay, &home.target(&computer.home.id, None), &attention);
        next_attention(&mut frames).await;
        let launch = Uuid::now_v7();
        if names_last {
            home.state.lock().unwrap().last_turns = Some(HashMap::from([(
                work,
                finished_turn(launch, work, "completed"),
            )]));
        }
        home.emit(
            "turn.started",
            work,
            work,
            finished_turn(launch, work, "running"),
        );
        home.state.lock().unwrap().background.insert(work, 1);
        home.emit(
            "turn.finished",
            work,
            work,
            owing_turn(launch, work, "completed", 1),
        );
        attention_where(&mut frames, |frame| frame["working"] == json!([work])).await;
        (computer, home, frames, connection, work, launch)
    }
    #[tokio::test]
    async fn a_chat_whose_background_work_is_stopped_while_it_is_idle_is_reported_stopped_once() {
        let (computer, home, mut frames, connection, work, launch) =
            chat_held_for_background_work(false).await;
        // Stopped while idle: the helper is cancelled and no turn follows.
        home.state.lock().unwrap().background.remove(&work);
        home.emit("task.updated", work, work, task_update("cancelled"));
        let stopped = attention_where(&mut frames, |frame| !finished_turns(frame).is_empty()).await;
        assert_eq!(finished_turns(&stopped), [json!(launch)], "{stopped}");
        assert_eq!(stopped["finished"][0]["status"], "interrupted");
        assert_eq!(stopped["finished"][0]["chatTitle"], "Work");
        assert_eq!(stopped["working"], json!([]), "{stopped}");
        tokio::time::sleep(Duration::from_millis(1200)).await;
        while let Ok(frame) = frames.try_recv() {
            assert_ne!(frame["type"], "attention", "reported once: {frame}");
        }
        connection.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    #[tokio::test]
    async fn a_chat_whose_background_result_arrives_is_reported_completed_and_never_stopped() {
        let (computer, home, mut frames, connection, work, launch) =
            chat_held_for_background_work(false).await;
        // The helper is done; its result is on its way into the chat.
        home.emit("task.updated", work, work, task_update("done"));
        tokio::time::sleep(Duration::from_millis(100)).await;
        home.state.lock().unwrap().background.remove(&work);
        let delivered = Uuid::now_v7();
        home.emit(
            "turn.started",
            work,
            work,
            finished_turn(delivered, work, "running"),
        );
        home.emit(
            "turn.finished",
            work,
            work,
            finished_turn(delivered, work, "completed"),
        );
        let done = attention_where(&mut frames, |frame| !finished_turns(frame).is_empty()).await;
        assert_eq!(finished_turns(&done), [json!(delivered)], "{done}");
        assert_eq!(done["finished"][0]["status"], "completed");
        tokio::time::sleep(Duration::from_millis(1200)).await;
        while let Ok(frame) = frames.try_recv() {
            if frame["type"] == "attention" {
                assert_eq!(finished_turns(&frame), [json!(delivered)], "{frame}");
                assert!(!frame.to_string().contains(&launch.to_string()), "{frame}");
            }
        }
        connection.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    /// Lose the server's events: the stream is cut and the server keeps
    /// none of what happened since, as when its buffer moved on.
    fn miss_events(home: &FakeDaemon) {
        {
            let mut state = home.state.lock().unwrap();
            state.event_id += 3000;
            state.kept_from = state.event_id;
        }
        home.cut();
    }
    #[tokio::test]
    async fn a_held_chat_whose_result_arrived_unseen_reports_the_turn_the_server_names() {
        let (computer, home, mut frames, connection, work, launch) =
            chat_held_for_background_work(true).await;
        // The result arrives and its turn ends while the events are lost.
        let delivered = Uuid::now_v7();
        {
            let mut state = home.state.lock().unwrap();
            state.background.remove(&work);
            state
                .last_turns
                .as_mut()
                .unwrap()
                .insert(work, finished_turn(delivered, work, "completed"));
        }
        miss_events(&home);
        let done = attention_where(&mut frames, |frame| !finished_turns(frame).is_empty()).await;
        assert_eq!(finished_turns(&done), [json!(delivered)], "{done}");
        assert_eq!(done["finished"][0]["status"], "completed");
        tokio::time::sleep(Duration::from_millis(1200)).await;
        while let Ok(frame) = frames.try_recv() {
            assert!(!frame.to_string().contains(&launch.to_string()), "{frame}");
        }
        connection.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    #[tokio::test]
    async fn a_held_chat_stopped_while_its_events_were_missed_is_reported_stopped() {
        let (computer, home, mut frames, connection, work, launch) =
            chat_held_for_background_work(true).await;
        // Stopped while idle, unseen: no turn followed the held one.
        home.state.lock().unwrap().background.remove(&work);
        miss_events(&home);
        let stopped = attention_where(&mut frames, |frame| !finished_turns(frame).is_empty()).await;
        assert_eq!(finished_turns(&stopped), [json!(launch)], "{stopped}");
        assert_eq!(stopped["finished"][0]["status"], "interrupted");
        connection.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    #[tokio::test]
    async fn an_older_server_that_cannot_name_the_newest_turn_reports_nothing_after_missed_events()
    {
        let (computer, home, mut frames, connection, work, _) =
            chat_held_for_background_work(false).await;
        home.state.lock().unwrap().background.remove(&work);
        miss_events(&home);
        // The chat stops working, but which turn it stopped at is unknown.
        let quiet = attention_where(&mut frames, |frame| frame["working"] == json!([])).await;
        assert_eq!(quiet["finished"], json!([]), "{quiet}");
        tokio::time::sleep(Duration::from_millis(1200)).await;
        while let Ok(frame) = frames.try_recv() {
            assert_eq!(frame["finished"], json!([]), "{frame}");
        }
        connection.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    #[tokio::test]
    async fn a_stop_followed_unseen_by_a_turn_that_left_work_running_is_not_an_end() {
        let work = Uuid::now_v7();
        let (computer, home) = watched_home(&[(work, "Work")]).await;
        home.state.lock().unwrap().activity = true;
        let (relay, mut frames) =
            fake_relay(vec![Relay::Record(&["attention.v1", "attention.working"])]).await;
        let attention = attention(&computer.paths);
        let connection = link(&relay, &home.target(&computer.home.id, None), &attention);
        next_attention(&mut frames).await;
        // A stop, then a follow-up turn whose events are not read yet: it
        // launched a helper and ended while that helper works.
        let (stop, follow_up) = (Uuid::now_v7(), Uuid::now_v7());
        home.emit(
            "turn.finished",
            work,
            work,
            finished_turn(stop, work, "interrupted"),
        );
        {
            let mut state = home.state.lock().unwrap();
            state.background.insert(work, 1);
            state.last_turns = Some(HashMap::from([(
                work,
                finished_turn(follow_up, work, "completed"),
            )]));
        }
        // Held on the follow-up: working, and nothing reported.
        let held = attention_where(&mut frames, |frame| frame["working"] == json!([work])).await;
        assert_eq!(held["finished"], json!([]), "{held}");
        tokio::time::sleep(Duration::from_millis(800)).await;
        while let Ok(frame) = frames.try_recv() {
            assert_eq!(frame["finished"], json!([]), "{frame}");
        }
        // That helper is stopped while the chat is idle: the follow-up is
        // where it stopped.
        home.state.lock().unwrap().background.remove(&work);
        home.emit("task.updated", work, work, task_update("cancelled"));
        let stopped = attention_where(&mut frames, |frame| !finished_turns(frame).is_empty()).await;
        assert_eq!(finished_turns(&stopped), [json!(follow_up)], "{stopped}");
        assert_eq!(stopped["finished"][0]["status"], "interrupted");
        connection.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    #[tokio::test]
    async fn a_paused_workflow_lets_its_chat_stop_and_a_resumed_one_makes_it_work_again() {
        let (computer, home, mut frames, connection, work, launch) =
            chat_held_for_background_work(true).await;
        let run_thread = Uuid::now_v7();
        let run = Uuid::now_v7();
        let workflow =
            |status: &str| json!({"info": {"id": run, "status": status, "threadId": run_thread}});
        // The only work the chat waits on is paused: it is quiet, stopped.
        home.state.lock().unwrap().background.remove(&work);
        home.emit("workflow.updated", run_thread, work, workflow("paused"));
        let stopped = attention_where(&mut frames, |frame| !finished_turns(frame).is_empty()).await;
        assert_eq!(finished_turns(&stopped), [json!(launch)], "{stopped}");
        assert_eq!(stopped["finished"][0]["status"], "interrupted");
        assert_eq!(stopped["working"], json!([]));
        // Resumed, the chat works again before any result arrives.
        home.state.lock().unwrap().background.insert(work, 1);
        home.emit("workflow.updated", run_thread, work, workflow("running"));
        attention_where(&mut frames, |frame| frame["working"] == json!([work])).await;
        // Its result is the chat's last turn.
        home.state.lock().unwrap().background.remove(&work);
        let delivered = Uuid::now_v7();
        home.state
            .lock()
            .unwrap()
            .last_turns
            .as_mut()
            .unwrap()
            .insert(work, finished_turn(delivered, work, "completed"));
        home.emit(
            "turn.started",
            work,
            work,
            finished_turn(delivered, work, "running"),
        );
        home.emit(
            "turn.finished",
            work,
            work,
            finished_turn(delivered, work, "completed"),
        );
        let done = attention_where(&mut frames, |frame| {
            finished_turns(frame).contains(&json!(delivered))
        })
        .await;
        assert_eq!(done["working"], json!([]), "{done}");
        connection.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    /// A `workflow.updated` for run `run`, launched by `launcher`.
    fn workflow_update(run: Uuid, status: &str) -> Value {
        json!({"info": {"id": run, "status": status}})
    }
    #[tokio::test]
    async fn a_busy_workflow_run_costs_its_chat_a_bounded_number_of_reads() {
        let work = Uuid::now_v7();
        let (computer, home) = watched_home(&[(work, "Work")]).await;
        let earlier = Uuid::now_v7();
        {
            let mut state = home.state.lock().unwrap();
            state.activity = true;
            state.background.insert(work, 1);
            state.last_turns = Some(HashMap::from([(
                work,
                finished_turn(earlier, work, "completed"),
            )]));
        }
        let (relay, mut frames) =
            fake_relay(vec![Relay::Record(&["attention.v1", "attention.working"])]).await;
        let attention = attention(&computer.paths);
        let connection = link(&relay, &home.target(&computer.home.id, None), &attention);
        next_attention(&mut frames).await;
        let path = format!("/v1/threads/{work}/activity");
        // A run started for the chat from outside a turn, publishing on
        // every log line: only its start says anything.
        let (run, run_thread) = (Uuid::now_v7(), Uuid::now_v7());
        for _ in 0..30 {
            home.emit(
                "workflow.updated",
                run_thread,
                work,
                workflow_update(run, "running"),
            );
        }
        attention_where(&mut frames, |frame| frame["working"] == json!([work])).await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(home.requested(&path), 1, "one read for thirty updates");
        // Its helpers report in a burst: one check is asked at a time.
        for _ in 0..10 {
            home.emit("task.updated", work, work, task_update("done"));
        }
        tokio::time::sleep(Duration::from_millis(1000)).await;
        assert_eq!(home.requested(&path), 2, "one more read for ten updates");
        connection.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    #[tokio::test]
    async fn a_chat_with_no_turn_yet_works_while_its_run_does_and_its_first_turn_ends_it() {
        let fresh = Uuid::now_v7();
        let (computer, home) = watched_home(&[(fresh, "Fresh")]).await;
        {
            let mut state = home.state.lock().unwrap();
            state.activity = true;
            state.last_turns = Some(HashMap::new());
        }
        let (relay, mut frames) =
            fake_relay(vec![Relay::Record(&["attention.v1", "attention.working"])]).await;
        let attention = attention(&computer.paths);
        let connection = link(&relay, &home.target(&computer.home.id, None), &attention);
        next_attention(&mut frames).await;
        // A workflow run started for a chat that ran no turn yet.
        let (run, run_thread) = (Uuid::now_v7(), Uuid::now_v7());
        home.state.lock().unwrap().background.insert(fresh, 1);
        home.emit(
            "workflow.updated",
            run_thread,
            fresh,
            workflow_update(run, "running"),
        );
        let working =
            attention_where(&mut frames, |frame| frame["working"] == json!([fresh])).await;
        assert_eq!(working["finished"], json!([]), "{working}");
        // Its result is the chat's first turn, which ends it.
        let first = Uuid::now_v7();
        {
            let mut state = home.state.lock().unwrap();
            state.background.remove(&fresh);
            state
                .last_turns
                .as_mut()
                .unwrap()
                .insert(fresh, finished_turn(first, fresh, "completed"));
        }
        home.emit(
            "workflow.updated",
            run_thread,
            fresh,
            workflow_update(run, "completed"),
        );
        home.emit(
            "turn.started",
            fresh,
            fresh,
            finished_turn(first, fresh, "running"),
        );
        home.emit(
            "turn.finished",
            fresh,
            fresh,
            finished_turn(first, fresh, "completed"),
        );
        let done = attention_where(&mut frames, |frame| !finished_turns(frame).is_empty()).await;
        assert_eq!(finished_turns(&done), [json!(first)], "{done}");
        assert_eq!(done["finished"][0]["status"], "completed");
        tokio::time::sleep(Duration::from_millis(1200)).await;
        while let Ok(frame) = frames.try_recv() {
            if frame["type"] == "attention" {
                assert_eq!(
                    finished_turns(&frame),
                    [json!(first)],
                    "reported once: {frame}"
                );
                assert_eq!(frame["working"], json!([]), "{frame}");
            }
        }
        connection.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
    #[tokio::test]
    async fn a_run_a_helper_launched_never_lists_the_helper() {
        let (main, helper) = (Uuid::now_v7(), Uuid::now_v7());
        let (computer, home) = watched_home(&[(main, "Main"), (helper, "Helper")]).await;
        {
            let mut state = home.state.lock().unwrap();
            state.threads.get_mut(&helper).unwrap()["parentId"] = json!(main);
            state.activity = true;
            state.background.insert(helper, 1);
            state.last_turns = Some(HashMap::from([(
                helper,
                finished_turn(Uuid::now_v7(), helper, "completed"),
            )]));
        }
        let (relay, mut frames) =
            fake_relay(vec![Relay::Record(&["attention.v1", "attention.working"])]).await;
        let attention = attention(&computer.paths);
        let connection = link(&relay, &home.target(&computer.home.id, None), &attention);
        next_attention(&mut frames).await;
        // The helper's run starts, pauses and resumes.
        let (run, run_thread) = (Uuid::now_v7(), Uuid::now_v7());
        for status in ["running", "paused", "running"] {
            home.emit(
                "workflow.updated",
                run_thread,
                helper,
                workflow_update(run, status),
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            home.requested(&format!("/v1/threads/{helper}/activity")),
            1,
            "asked once, then known as a helper"
        );
        while let Ok(frame) = frames.try_recv() {
            if frame["type"] == "attention" {
                assert_eq!(frame["working"], json!([]), "{frame}");
                assert_eq!(frame["finished"], json!([]), "{frame}");
            }
        }
        connection.abort();
        std::fs::remove_dir_all(&computer.root).unwrap();
    }
}
