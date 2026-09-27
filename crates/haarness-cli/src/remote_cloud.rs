//! Outbound Cloud connector. Local runtime and provider credentials never cross the relay.
use super::{Paths, ensure_workspace_server};
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
#[derive(Args)]
pub struct RemoteArgs {
    #[command(subcommand)]
    command: RemoteCommand,
}
#[derive(Subcommand)]
enum RemoteCommand {
    /// Connect this machine to Cloud. Keep running under a service supervisor for unattended use.
    Run {
        #[arg(long, default_value = "My computer")]
        name: String,
        /// Publish only this chat and its child chats, instead of host-wide project access.
        #[arg(long = "chat")]
        session: Option<Uuid>,
    },
    /// Start a background connector for this login. Does not install an OS startup service.
    Enable {
        #[arg(long, default_value = "My computer")]
        name: String,
        #[arg(long = "chat")]
        session: Option<Uuid>,
    },
    /// Show local connector IDs and whether their processes are running.
    Status,
    /// Stop a local connector without revoking its registration or stopping local work.
    Disable { id: Uuid },
    /// Forget a stopped connector identity so the next enable registers new access.
    Forget { id: Uuid },
    /// List machines connected to the selected Cloud account.
    Hosts,
    /// List explicitly published chats.
    Sessions,
    /// Revoke Cloud access without deleting any local files or conversations.
    Revoke { id: Uuid },
}
// Kept wire-compatible with cloud-contracts::remote::RelayFrame; protocol fixtures test both ends.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Frame {
    Ready {
        version: u32,
    },
    CatalogChanged,
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
        RemoteCommand::Status => {
            println!("{}", json!({"items":local_status(paths)?}));
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
                .context("Sign in first")?;
            let file = identity_file(paths, &client, account, workspace, session);
            if running(&file)? {
                println!("Connector is already running");
                return Ok(());
            }
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
            let mut command = std::process::Command::new(std::env::current_exe()?);
            if let Some(root) = &paths.root {
                command.arg("--data-dir").arg(root);
            }
            command
                .arg("--workspace")
                .arg(workspace)
                .args(["remote", "run", "--name", &name]);
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
                    anyhow::bail!("Connector exited ({status}); inspect {}", log.display())
                }
                if running(&file)? {
                    println!(
                        "Connector started. Use `haarness remote status` for its host ID. Log: {}",
                        log.display()
                    );
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            anyhow::bail!("Connector is starting; inspect {}", log.display())
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
            let discovery = ensure_workspace_server(paths, workspace, trust, false).await?;
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
            super::write_private_json(
                &file,
                &json!({"id":id,"workspace":workspace,"chatId":session,"apiUrl":client.config.api_url,"pid":std::process::id()}),
            )?;
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
                println!(
                    "Published chat {}",
                    published["id"].as_str().unwrap_or_default()
                );
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
            let mut failures = 0u32;
            let mut rejected = None;
            loop {
                // Daemons can be replaced by a local client upgrade or restarted after a crash.
                // Resolve the current address/token before each transport generation.
                let discovery = tokio::select! {
                    () = stop.cancelled() => break,
                    result = ensure_workspace_server(paths, workspace, trust, false) => result,
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
                rejected = None;
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
                let result = tokio::select! {()=stop.cancelled()=>break,result=attempt=>result};
                match result {
                    Ok(Ok((socket, _))) => {
                        failures = 0;
                        println!("Remote host connected");
                        tokio::select! {()=stop.cancelled()=>break,result=connected(socket,target.clone(),http.clone())=>{if let Err(error)=result {eprintln!("Remote connection ended: {error}");}}}
                    }
                    Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response)))
                        if response.status() == reqwest::StatusCode::UNAUTHORIZED =>
                    {
                        rejected = Some(bearer);
                        failures += 1;
                        if failures > 1 {
                            anyhow::bail!(
                                "Remote authentication rejected; sign in again or replace the expired/revoked API key"
                            )
                        }
                    }
                    Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response)))
                        if matches!(response.status().as_u16(), 403 | 404) =>
                    {
                        anyhow::bail!("Remote host access was revoked or is no longer allowed")
                    }
                    _ => {
                        failures += 1;
                        eprintln!("Remote host disconnected; reconnecting");
                    }
                }
                let delay = Duration::from_millis(
                    (500u64 << failures.min(5)) + u64::from(rand::random::<u16>() % 500),
                );
                tokio::select! {()=stop.cancelled()=>break,()=tokio::time::sleep(delay)=>{}}
            }
            println!("Remote connector stopped. Local work continues.");
        }
    }
    Ok(())
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
async fn connected(
    mut socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    target: Target,
    http: reqwest::Client,
) -> Result<()> {
    let ready = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await?
        .context("Relay closed")??;
    ensure!(
        matches!(ready,Message::Text(ref value) if matches!(serde_json::from_str::<Frame>(value),Ok(Frame::Ready{version:1}))),
        "Unsupported remote relay protocol"
    );
    let (send, mut outgoing) = mpsc::channel::<Frame>(128);
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
                                if active.len()>=32 {send.send(Frame::Error{id,message:"Host request capacity reached".into()}).await?;continue}
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
            request = request.header("x-haarness-remote-thread", thread.to_string());
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

struct AbortTask(tokio::task::JoinHandle<()>);
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
fn local_status(paths: &Paths) -> Result<Vec<Value>> {
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
        let Ok(mut value) = serde_json::from_slice::<Value>(&std::fs::read(&path)?) else {
            continue;
        };
        if value["id"].as_str().is_none() {
            continue;
        }
        value["running"] = json!(running(&path)?);
        result.push(value);
    }
    Ok(result)
}
fn local_file(paths: &Paths, id: Uuid) -> Result<Option<std::path::PathBuf>> {
    for entry in std::fs::read_dir(paths.config_dir())? {
        let path = entry?.path();
        if path.extension().is_none_or(|v| v != "json")
            || !path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .starts_with("remote-")
        {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&std::fs::read(&path)?) else {
            continue;
        };
        if value["id"].as_str() == Some(&id.to_string()) {
            return Ok(Some(path));
        }
    }
    Ok(None)
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
}
