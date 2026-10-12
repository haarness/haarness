//! Outbound Cloud connector. Local runtime and provider credentials never cross the relay.
use super::{
    Paths, ensure_workspace_server,
    reconnect::{Backoff, Link, Pacing},
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
            let discovery = ensure_workspace_server(paths, workspace, trust).await?;
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
            let mut backoff = Backoff::new(Pacing::default());
            let mut rejected = None;
            let ended: Result<()> = async {
                loop {
                    // Daemons can be replaced by a local client upgrade or restarted after a crash.
                    // Resolve the current address/token before each transport generation.
                    let discovery = tokio::select! {
                        () = stop.cancelled() => break,
                        result = ensure_workspace_server(paths, workspace, trust) => result,
                    };
                    match discovery {
                        Ok(discovery) => {
                            target.url = discovery.url;
                            target.token = discovery.token;
                        }
                        Err(error) => {
                            eprintln!("Waiting for the local runtime: {error}");
                            tokio::select! { () = stop.cancelled() => break, () = tokio::time::sleep(Duration::from_secs(5)) => {} }
                            continue;
                        }
                    }
                    verify_runtime(&http, &target).await?;
                    let bearer = client.bearer(rejected.as_deref()).await?;
                    let reauthorized = rejected.take().is_some();
                    let mut url = client
                        .config
                        .api_url
                        .join(&format!("api/v1/remote-hosts/{id}/connect"))?;
                    url.set_scheme(if url.scheme() == "https" { "wss" } else { "ws" })
                        .map_err(|()| anyhow::anyhow!("Invalid Cloud URL"))?;
                    let mut handshake = url.as_str().into_client_request()?;
                    handshake
                        .headers_mut()
                        .insert("authorization", format!("Bearer {bearer}").parse()?);
                    let link = tokio::select! {
                        () = stop.cancelled() => break,
                        link = relay_link(handshake, &target, &http, backoff.pacing.stable, &attention) => link?,
                        () = follow_title(&client, &http, &target, id, &file, &mut shown_title) => continue,
                    };
                    if link == Link::Unauthorized {
                        // One retry with a fresh token; a second refusal will not pass either.
                        ensure!(
                            !reauthorized,
                            "Remote authentication rejected; sign in again or replace the expired/revoked API key"
                        );
                        rejected = Some(bearer);
                    }
                    let delay = backoff.wait(link);
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
    .await??;
    ensure!(
        info["capabilities"]
            .as_array()
            .is_some_and(|values| values.iter().any(|v| v == "remote-chat-scope.v1")),
        "Restart this project's Haarness server with the current version before enabling remote access"
    );
    Ok(())
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
const RELAY_VERSION: u64 = 1;
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
/// answer that reconnecting cannot change is an error.
async fn relay_link(
    handshake: tokio_tungstenite::tungstenite::handshake::client::Request,
    target: &Target,
    http: &reqwest::Client,
    stable: Duration,
    attention: &Arc<remote_attention::Context>,
) -> Result<Link> {
    let attempt = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async_with_config(
            handshake,
            Some(
                tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                    .max_message_size(Some(24 * 1024 * 1024))
                    .max_frame_size(Some(24 * 1024 * 1024)),
            ),
            false,
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
            anyhow::bail!("Remote host access was revoked or is no longer allowed")
        }
        Ok(Err(error)) => {
            eprintln!("Could not reach Haarness Cloud: {error}; reconnecting");
            return Ok(Link::Failed);
        }
        Err(_) => {
            eprintln!("Haarness Cloud did not answer in time; reconnecting");
            return Ok(Link::Failed);
        }
    };
    println!("Remote host connected");
    let since = tokio::time::Instant::now();
    let mut served = false;
    if let Err(error) = connected(
        socket,
        target.clone(),
        http.clone(),
        &mut served,
        attention.clone(),
    )
    .await
    {
        if error.is::<Unsupported>() {
            return Err(error);
        }
        eprintln!("Remote connection ended: {error}");
    }
    Ok(Link::Ended {
        proven: served || since.elapsed() >= stable,
    })
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
    served: &mut bool,
    attention: Arc<remote_attention::Context>,
) -> Result<()> {
    let ready = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await?
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
    // An older relay drops the socket on a frame it does not know.
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
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    let mut last_message = tokio::time::Instant::now();
    loop {
        tokio::select! {
            ()=runtime_lost.cancelled()=>anyhow::bail!("Local runtime changed; reconnecting"),
            _=tick.tick()=>{ensure!(last_message.elapsed()<Duration::from_secs(25),"Relay heartbeat timed out");socket.send(Message::Ping(vec![1].into())).await?;},
            completed=jobs.join_next(),if !jobs.is_empty()=>{if let Some(Ok(id))=completed {active.remove(&id);}},
            frame=outgoing.recv()=>{let Some(frame)=frame else {break};tokio::time::timeout(Duration::from_secs(10),socket.send(Message::Text(serde_json::to_string(&frame)?.into()))).await??;},
            frame=socket.next()=>{
                match frame.context("Relay closed")?? {
                    Message::Text(value)=>{
                        last_message=tokio::time::Instant::now();
                        match serde_json::from_str::<Frame>(&value)? {
                            frame@Frame::Open{id,..}=>{
                                ensure!(!active.contains_key(&id),"Duplicate relay request");
                                // A request routed here proves the relay took this host.
                                *served=true;
                                let (input,receiver)=mpsc::channel(WINDOW+2);
                                let credit=Arc::new(Semaphore::new(WINDOW));
                                let cancel=tokio_util::sync::CancellationToken::new();
                                active.insert(id,Active{input,credit:credit.clone(),cancel:cancel.clone()});
                                let send=send.clone();let target=target.clone();let http=http.clone();
                                jobs.spawn(async move {
                                    tokio::select! {()=cancel.cancelled()=>{},result=dispatch(frame,target,http,send.clone(),credit,receiver)=>{if result.is_err(){let _=send.send(Frame::Error{id,message:"Local request failed".into()}).await;}}}
                                    id
                                });
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
        let response =
            tokio::time::timeout(Duration::from_secs(310), request.body(body).send()).await??;
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
            data(&send, &credit, id, &chunk?, false).await?;
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
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Err(error) => Err(error.into()),
    }
}
/// Every connector identity this data directory knows, with its file.
fn identities(paths: &Paths) -> Result<Vec<(std::path::PathBuf, Value)>> {
    let mut result = Vec::new();
    if !paths.config_dir().exists() {
        return Ok(result);
    }
    for entry in std::fs::read_dir(paths.config_dir())? {
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

/// The name Cloud shows for this computer next to its shared chats.
pub(crate) fn computer_name() -> String {
    #[cfg(target_os = "macos")]
    if let Ok(output) = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
    {
        let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if output.status.success() && !name.is_empty() {
            return name.chars().take(100).collect();
        }
    }
    ["COMPUTERNAME", "HOSTNAME"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .chain(std::fs::read_to_string("/etc/hostname").ok())
        .map(|name| name.trim().to_owned())
        .find(|name| !name.is_empty())
        .map_or_else(
            || "My computer".into(),
            |name| name.chars().take(100).collect(),
        )
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
    let start = spawn_connector(paths, workspace, &file, &computer_name(), Some(thread)).await?;
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
        assert!(about(backoff.wait(Link::Failed), 30_000));
        assert!(about(backoff.wait(dropped), 600_000));
        // A connection that lasted, then its next drop, start over.
        assert!(about(backoff.wait(Link::Ended { proven: true }), 1000));
        assert!(about(backoff.wait(dropped), 2000));

        let mut backoff = Backoff::new(Pacing::default());
        for seconds in [1, 2, 4, 8, 16, 30, 30] {
            assert!(about(backoff.wait(Link::Failed), seconds * 1000));
        }
    }
    #[derive(Clone, Copy)]
    enum Relay {
        /// Say ready, then close at once.
        Drop,
        /// Say ready and keep the socket this long.
        Hold(Duration),
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
                socket
                    .send(Message::Text(ready.to_string().into()))
                    .await
                    .unwrap();
                match step {
                    Relay::Hold(time) => tokio::time::sleep(time).await,
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
        let connect = || {
            relay_link(
                relay.as_str().into_client_request().unwrap(),
                &target,
                &http,
                pacing.stable,
                &attention,
            )
        };

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
            let link = connect().await.unwrap();
            assert_eq!(link, expected);
            let wait = backoff.wait(link);
            assert!(
                about(wait, nominal),
                "{wait:?} after {link:?}, expected {nominal}ms"
            );
        }
        // A relay on another protocol stops the connector: only an update helps.
        let error = connect().await.unwrap_err();
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
            &attention,
        )
        .await
        .unwrap();
        assert_eq!(link, Link::Failed);
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
        /// The turn `/threads/{id}/active` reports.
        active: HashMap<Uuid, Value>,
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
                            let replay = state
                                .log
                                .iter()
                                .filter(|(id, thread, _)| after.is_none_or(|after| *id > after) && shown(thread))
                                .map(|(_, _, frame)| Ok(frame.clone()))
                                .collect::<Vec<_>>();
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
                &attention,
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
                &attention,
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
                &attention,
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
                &attention,
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
}
