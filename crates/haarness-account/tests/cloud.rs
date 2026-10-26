//! Exercise the account-to-control-plane-to-runtime boundary over real HTTP.
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Request, State, WebSocketUpgrade, ws::Message},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::{Duration, Utc};
use haarness_account::{Client, Config, Credentials, cloud::Cloud, credentials};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

const ACCOUNT: &str = "00000000-0000-4000-8000-000000000001";
const AGENT: &str = "00000000-0000-4000-8000-000000000002";
const WORKSPACE: &str = "00000000-0000-4000-8000-000000000003";
const THREAD: &str = "00000000-0000-4000-8000-000000000004";
const TURN: &str = "00000000-0000-4000-8000-000000000005";
const OTHER: &str = "00000000-0000-4000-8000-000000000006";

#[derive(Default)]
struct Data {
    base: String,
    requests: Vec<(String, String, Value)>,
    attachments: u32,
    workspace_reads: u32,
    created: bool,
    chats: Vec<Value>,
    schedule: Option<Value>,
    started: bool,
    stopped: bool,
    running_turn: bool,
    permission: bool,
    wrong_turn: bool,
    reject_ticket: bool,
    invalid_gateway: bool,
    foreign_account: bool,
    control_events: Option<tokio::sync::broadcast::Sender<Value>>,
    runtime_events: Option<tokio::sync::broadcast::Sender<Value>>,
    control_connections: usize,
    runtime_connections: usize,
    live_connections: usize,
    hold_startup: bool,
    reject_control_token: bool,
    account_rotated: bool,
    control_rejections: usize,
    stale_generation: bool,
    stale_generation_on_write: bool,
}
type Shared = Arc<Mutex<Data>>;
fn agent(d: &Data) -> Value {
    json!({"id":AGENT,"accountId":if d.foreign_account {OTHER}else{ACCOUNT},"name":"fixture","description":"retained","version":4,"sshPublicKey":"ssh-ed25519 public-only","environmentNames":["PRIVATE_ENV"],"configuration":{"model":"openai/gpt-5-nano","reasoningEffort":"low","repository":{"kind":"empty"},"instructions":"retained instructions","diskGb":1}})
}
async fn handler(State(state): State<Shared>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let method = req.method().to_string();
    let bearer = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let bytes = to_bytes(req.into_body(), 1024 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let mut d = state.lock().unwrap();
    d.requests
        .push((method.clone(), format!("{path}?{query}"), body.clone()));
    if path.starts_with("/runtime/") {
        assert!(
            bearer.starts_with("Bearer ticket-"),
            "Account bearer must never go to the runtime"
        );
        if (d.stale_generation || d.stale_generation_on_write && method == "POST")
            && bearer == "Bearer ticket-1"
        {
            return (StatusCode::BAD_REQUEST, Json(json!({"error":{"code":"remote_project","message":"workspace restarted; re-attach"}}))).into_response();
        }
        if d.reject_ticket && bearer == "Bearer ticket-1" {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error":{"message":"expired ticket"}})),
            )
                .into_response();
        }
    } else if path.starts_with("/router/") {
        assert_eq!(bearer, "Bearer router-secret");
    } else if path == "/oauth/token" {
        d.account_rotated = true;
        return Json(json!({"access_token":"account-renewed","refresh_token":"refresh-renewed","expires_in":3600})).into_response();
    } else {
        assert_eq!(
            bearer,
            if d.account_rotated {
                "Bearer account-renewed"
            } else {
                "Bearer account-secret"
            }
        );
    }
    let value = match (method.as_str(), path.as_str()) {
        ("GET", "/api/v1/me") => json!({"user":{},"accounts":[],"defaultAccountId":ACCOUNT}),
        ("GET", "/api/v1/cloud-agents") => {
            if query.contains("cursor=") {
                json!({"items":[agent(&d)],"nextCursor":null})
            } else {
                json!({"items":[{"name":"unrelated"}],"nextCursor":OTHER})
            }
        }
        ("POST", "/api/v1/cloud-agents") => agent(&d),
        ("POST", "/api/v1/account/router-session") => {
            json!({"token":"router-secret","routerUrl":format!("{}/router/v1",d.base),"expiresAt":Utc::now()+Duration::hours(1)})
        }
        ("GET", "/router/v1/models") => {
            json!({"data":[{"id":"openai/gpt-5-nano","reasoningEfforts":["low","high"]}]})
        }
        ("GET", p) if p == format!("/api/v1/cloud-agents/{AGENT}") => agent(&d),
        ("PUT", p) if p == format!("/api/v1/cloud-agents/{AGENT}") => body,
        ("DELETE", p) if p == format!("/api/v1/cloud-agents/{AGENT}") => Value::Null,
        ("POST", "/api/v1/schedules") => {
            let mut row = body.clone();
            row["id"] = json!(AGENT);
            row["accountId"] = json!(ACCOUNT);
            row["version"] = json!(1);
            row["enabled"] = json!(true);
            d.schedule = Some(row.clone());
            row
        }
        ("GET", "/api/v1/schedules") => {
            json!({"items":d.schedule.iter().collect::<Vec<_>>(),"nextCursor":null})
        }
        ("GET", p) if p == format!("/api/v1/schedules/{AGENT}") => {
            let mut row = d.schedule.clone().unwrap();
            if d.foreign_account {
                row["accountId"] = json!(OTHER);
            }
            row
        }
        ("PATCH", p) if p == format!("/api/v1/schedules/{AGENT}") => {
            let row = d.schedule.as_mut().unwrap();
            for (key, value) in body.as_object().unwrap() {
                if !value.is_null() {
                    row[key] = value.clone();
                }
            }
            row["version"] = json!(row["version"].as_i64().unwrap() + 1);
            row.clone()
        }
        ("POST", p) if p == format!("/api/v1/schedules/{AGENT}/run") => {
            json!({"id":TURN,"workspaceId":WORKSPACE,"chatId":THREAD,"status":"pending"})
        }
        ("GET", p) if p == format!("/api/v1/schedules/{AGENT}/runs") => {
            json!({"items":[{"id":TURN,"chatId":THREAD}],"nextCursor":null})
        }
        ("DELETE", p) if p == format!("/api/v1/schedules/{AGENT}") => {
            d.schedule = None;
            Value::Null
        }
        ("POST", "/api/v1/workspaces") => {
            d.created = true;
            json!({"workspace":{"id":WORKSPACE,"observedState":"pending"}})
        }
        ("GET", p) if p == format!("/api/v1/workspaces/{WORKSPACE}") => {
            d.workspace_reads += 1;
            let starting = if d.control_events.is_some() {
                d.hold_startup
            } else {
                d.workspace_reads <= 2
            };
            let observed = if (d.created || d.started) && starting || d.stopped {
                "stopped"
            } else {
                "running"
            };
            json!({"id":WORKSPACE,"accountId":if d.foreign_account {OTHER}else{ACCOUNT},"agentSnapshot":agent(&d),"configuration":agent(&d)["configuration"],"version":3,"sshPublicKey":"ssh-ed25519 workspace-public","observedState":observed,"desiredState":if d.stopped {"stopped"} else {"running"}})
        }
        ("POST", p) if p == format!("/api/v1/workspaces/{WORKSPACE}/attach") => {
            d.attachments += 1;
            json!({"gatewayUrl":if d.invalid_gateway {"http://untrusted.example/runtime/".into()} else {format!("{}/runtime/",d.base)},"ticket":format!("ticket-{}",d.attachments),"expiresAt":Utc::now()+Duration::hours(1)})
        }
        ("POST", p) if p == format!("/api/v1/workspaces/{WORKSPACE}/start") => {
            d.started = true;
            d.stopped = false;
            d.workspace_reads = 0;
            json!({"operation":{"id":"start"}})
        }
        ("POST", p) if p == format!("/api/v1/workspaces/{WORKSPACE}/stop") => {
            d.stopped = true;
            json!({"operation":{"id":"stop"}})
        }
        ("DELETE", p) if p == format!("/api/v1/workspaces/{WORKSPACE}") => {
            json!({"operation":{"id":"delete"}})
        }
        ("PATCH", p) if p == format!("/api/v1/workspaces/{WORKSPACE}") => body,
        ("POST", p) if p == format!("/api/v1/workspaces/{WORKSPACE}/save-agent") => body,
        ("GET", "/runtime/v1/threads") => json!(d.chats),
        ("GET", "/runtime/v1/info") => {
            json!({"defaultAgent":"cloud-agent","defaultProvider":"haarness","defaultModel":"openai/gpt-5-nano"})
        }
        ("POST", "/runtime/v1/threads") => {
            let id = if d.chats.is_empty() { THREAD } else { OTHER };
            let row = json!({"id":id,"title":body["title"],"parentId":null,"archived":false});
            d.chats.push(row.clone());
            row
        }
        ("PATCH", p) if p.starts_with("/runtime/v1/threads/") => {
            let id = p.rsplit('/').next().unwrap();
            let row = d.chats.iter_mut().find(|r| r["id"] == id).unwrap();
            for (key, value) in body.as_object().unwrap() {
                row[key] = value.clone();
            }
            row.clone()
        }
        ("GET", p) if p == format!("/runtime/v1/threads/{OTHER}") => json!({"id":OTHER}),
        ("GET", p) if p == format!("/runtime/v1/threads/{THREAD}") => json!({"id":THREAD}),
        ("POST", p) if p == format!("/runtime/v1/threads/{THREAD}/turns") => {
            json!({"id":TURN,"threadId":THREAD,"status":"running"})
        }
        ("GET", p)
            if p == format!("/runtime/v1/turns/{TURN}")
                || p == format!("/runtime/v1/threads/{THREAD}/active") =>
        {
            json!({"id":TURN,"threadId":if d.wrong_turn {OTHER}else{THREAD},"status":if d.running_turn {"running"}else{"completed"}})
        }
        ("GET", "/runtime/v1/permissions") => {
            if d.permission {
                json!([{"threadId":OTHER,"rootThreadId":THREAD,"permissionId":"approval","operation":"bash:deploy"},{"threadId":OTHER,"permissionId":"unrelated"}])
            } else {
                json!([])
            }
        }
        ("POST", p)
            if p == format!("/runtime/v1/turns/{TURN}/interrupt")
                || p == format!("/runtime/v1/threads/{THREAD}/interrupt") =>
        {
            json!({"interrupted":true})
        }
        ("GET", p) if p == format!("/runtime/v1/threads/{THREAD}/items") => {
            let after = query
                .strip_prefix("after=")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            json!((1..=5).filter(|n|*n>after).map(|n|json!({"sequence":n,"data":{"type":"text","text":format!("message {n}")}})).collect::<Vec<_>>())
        }
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message":format!("Unexpected {method} {path}")})),
            )
                .into_response();
        }
    };
    Json(value).into_response()
}
struct Fixture {
    data: Shared,
    config: Config,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new(signed_in: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let data = Arc::new(Mutex::new(Data {
            base: base.clone(),
            ..Data::default()
        }));
        let app = Router::new()
            .route("/api/v1/events", get(control_events))
            .route("/runtime/v1/events", get(runtime_events))
            .fallback(handler)
            .with_state(data.clone());
        let server = tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
        let config = Config::new(
            &base,
            std::env::temp_dir().join(format!("haarness-cloud-tools-{}", uuid::Uuid::now_v7())),
            "haarness-cli",
        )
        .unwrap();
        if signed_in {
            credentials::save(
                &config.path(),
                &Credentials {
                    api_url: config.api_url.to_string(),
                    access_token: "account-secret".into(),
                    refresh_token: "refresh-secret".into(),
                    access_expires_at: Utc::now() + Duration::hours(1),
                },
            )
            .unwrap();
        }
        Self {
            data,
            config,
            server,
        }
    }
    async fn cloud(&self, cancel: CancellationToken) -> Cloud {
        Cloud::new(Client::new(self.config.clone()).unwrap(), cancel)
            .await
            .unwrap()
    }
    fn count(&self, method: &str, path: &str) -> usize {
        self.data
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r.0 == method && r.1.starts_with(&format!("{path}?")))
            .count()
    }
    fn enable_events(&self) {
        let mut d = self.data.lock().unwrap();
        d.control_events = Some(tokio::sync::broadcast::channel(64).0);
        d.runtime_events = Some(tokio::sync::broadcast::channel(64).0);
    }
}

struct Connection(Shared);
impl Drop for Connection {
    fn drop(&mut self) {
        self.0.lock().unwrap().live_connections -= 1;
    }
}
async fn control_events(
    State(data): State<Shared>,
    ws: WebSocketUpgrade,
    req: Request,
) -> Response {
    assert!(req.headers().get("origin").is_none());
    assert!(req.uri().query().unwrap().contains(ACCOUNT));
    let mut receiver = {
        let mut d = data.lock().unwrap();
        if d.reject_control_token && req.headers()["authorization"] == "Bearer account-secret" {
            d.control_rejections += 1;
            return StatusCode::UNAUTHORIZED.into_response();
        }
        assert_eq!(
            req.headers()["authorization"],
            if d.account_rotated {
                "Bearer account-renewed"
            } else {
                "Bearer account-secret"
            }
        );
        let Some(events) = &d.control_events else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let receiver = events.subscribe();
        d.control_connections += 1;
        d.live_connections += 1;
        receiver
    };
    ws.on_upgrade(move |mut socket| async move {
        let _connection = Connection(data);
        socket.send(Message::Text(json!({"type":"ready","resync":true}).to_string().into())).await.unwrap();
        loop {
            tokio::select! {
                event = receiver.recv() => {
                    let Ok(event) = event else { break; };
                    if event["_close"] == true { break; }
                    if socket.send(Message::Text(event.to_string().into())).await.is_err() { break; }
                }
                _ = socket.recv() => break,
            }
        }
    }).into_response()
}
async fn runtime_events(State(data): State<Shared>, req: Request) -> Response {
    let mut receiver = {
        let mut d = data.lock().unwrap();
        assert!(
            req.headers()["authorization"]
                .to_str()
                .unwrap()
                .starts_with("Bearer ticket-")
        );
        let Some(events) = &d.runtime_events else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let receiver = events.subscribe();
        d.runtime_connections += 1;
        d.live_connections += 1;
        receiver
    };
    let connection = Connection(data);
    let stream = async_stream::stream! {
        let _connection = connection;
        while let Ok(event) = receiver.recv().await {
            if event["_close"] == true { break; }
            yield Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().data(event.to_string()));
        }
    };
    axum::response::Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response()
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_dir_all(&self.config.config_dir);
    }
}

#[tokio::test]
async fn requires_existing_account_login_and_rechecks_logout() {
    let f = Fixture::new(false).await;
    let e = Cloud::new(
        Client::new(f.config.clone()).unwrap(),
        CancellationToken::new(),
    )
    .await
    .err()
    .unwrap();
    assert!(e.to_string().contains("haarness login"));
    assert!(f.data.lock().unwrap().requests.is_empty());
    let f = Fixture::new(true).await;
    let cloud = f.cloud(CancellationToken::new()).await;
    std::fs::remove_file(f.config.path()).unwrap();
    assert!(
        cloud
            .agent(json!({"action":"list"}))
            .await
            .unwrap_err()
            .to_string()
            .contains("haarness login")
    );
    assert_eq!(f.count("GET", "/api/v1/cloud-agents"), 0);
}

#[tokio::test]
async fn templates_preserve_configuration_and_do_not_expose_credentials() {
    let f = Fixture::new(true).await;
    let cloud = f.cloud(CancellationToken::new()).await;
    let list = cloud
        .agent(json!({"action":"list","search":"fixture"}))
        .await
        .unwrap();
    assert_eq!(list["items"][0]["id"], AGENT);
    assert_eq!(f.count("GET", "/api/v1/cloud-agents"), 2);
    let result=cloud.agent(json!({"action":"update","id":AGENT,"version":4,"configuration":{"model":"google/gemini-fixture"}})).await.unwrap();
    assert_eq!(
        result["configuration"]["instructions"],
        "retained instructions"
    );
    assert_eq!(result["configuration"]["model"], "google/gemini-fixture");
    assert!(result.get("environment").is_none());
    assert_eq!(result["version"], 4);
    assert!(
        cloud
            .agent(json!({"action":"update","id":AGENT,"name":"no revision"}))
            .await
            .is_err()
    );
    for input in [
        json!({"action":"create","name":"x","token":"secret"}),
        json!({"action":"create","name":"x","configuration":{"environment":{"SECRET":"value"}}}),
    ] {
        assert!(cloud.agent(input).await.is_err());
    }
    assert_eq!(f.count("POST", "/api/v1/cloud-agents"), 0);
    let models = cloud
        .agent(json!({"action":"models","search":"GPT-5 Nano"}))
        .await
        .unwrap();
    assert_eq!(models["models"][0]["id"], "openai/gpt-5-nano");
    assert!(!models.to_string().contains("secret"));
    let key = cloud
        .agent(json!({"action":"ssh_public_key","id":AGENT}))
        .await
        .unwrap();
    assert_eq!(key["sshPublicKey"], "ssh-ed25519 public-only");
    f.data.lock().unwrap().foreign_account = true;
    assert!(
        cloud
            .agent(json!({"action":"delete","id":AGENT}))
            .await
            .unwrap_err()
            .to_string()
            .contains("another Haarness Cloud account")
    );
    assert_eq!(
        f.count("DELETE", &format!("/api/v1/cloud-agents/{AGENT}")),
        0
    );
}

#[tokio::test]
async fn asynchronous_ask_waits_for_boot_submits_once_and_renews_ticket() {
    let f = Fixture::new(true).await;
    f.data.lock().unwrap().reject_ticket = true;
    let cloud = f.cloud(CancellationToken::new()).await;
    assert!(
        cloud
            .workspace(json!({"action":"ask","cloudAgentId":AGENT,"message":"  "}))
            .await
            .is_err()
    );
    assert_eq!(f.count("POST", "/api/v1/workspaces"), 0);
    let result=cloud.workspace(json!({"action":"ask","cloudAgentId":AGENT,"message":"hello","wait":false,"timeoutSeconds":10})).await.unwrap();
    assert_eq!(result["messageSubmitted"], true);
    assert_eq!(result["turnId"], TURN);
    assert_eq!(result["status"], "running");
    assert_eq!(f.data.lock().unwrap().attachments, 2);
    {
        let data = f.data.lock().unwrap();
        let thread = data
            .requests
            .iter()
            .find(|r| r.0 == "POST" && r.1 == "/runtime/v1/threads?")
            .unwrap();
        assert_eq!(thread.2["agent"], "cloud-agent");
        let ask = data
            .requests
            .iter()
            .find(|r| r.0 == "POST" && r.1 == format!("/runtime/v1/threads/{THREAD}/turns?"))
            .unwrap();
        assert_eq!(ask.2["reasoningEffort"], "low");
        // Marked as an agent's words, never the person's, for Auto's reviewer there.
        assert_eq!(
            ask.2["text"],
            format!("{}hello", haarness_protocol::AGENT_MESSAGE_PREFIX)
        );
        assert!(ask.2.get("autoApprove").is_none());
        assert_eq!(ask.2["tools"]["question"], false);
    }
    let done = cloud
        .workspace(json!({"action":"wait","workspaceId":WORKSPACE,"threadId":THREAD,"turnId":TURN}))
        .await
        .unwrap();
    assert_eq!(done["status"], "completed");
    assert_eq!(f.count("POST", "/api/v1/workspaces"), 1);
    assert_eq!(
        f.count("POST", &format!("/runtime/v1/threads/{THREAD}/turns")),
        1
    );
    assert!(!done.to_string().contains("ticket-"));
    assert!(!done.to_string().contains("account-secret"));
}

#[tokio::test]
async fn stopped_reads_never_start_compute_and_explicit_start_waits() {
    let f = Fixture::new(true).await;
    f.data.lock().unwrap().stopped = true;
    let cloud = f.cloud(CancellationToken::new()).await;
    for action in ["messages", "wait"] {
        let result = cloud
            .workspace(json!({"action":action,"workspaceId":WORKSPACE}))
            .await
            .unwrap();
        assert_eq!(result["status"], "stopped");
    }
    assert_eq!(f.data.lock().unwrap().attachments, 0);
    assert_eq!(
        f.count("POST", &format!("/api/v1/workspaces/{WORKSPACE}/start")),
        0
    );
    let result = cloud
        .workspace(json!({"action":"start","workspaceId":WORKSPACE,"timeoutSeconds":10}))
        .await
        .unwrap();
    assert_eq!(result["status"], "running");
    assert!(f.data.lock().unwrap().workspace_reads >= 3);
}

#[tokio::test]
async fn permissions_are_not_autoapproved_and_wrong_turn_cannot_be_aborted() {
    let f = Fixture::new(true).await;
    {
        let mut d = f.data.lock().unwrap();
        d.permission = true;
        d.running_turn = true;
    }
    let cloud = f.cloud(CancellationToken::new()).await;
    let result = cloud
        .workspace(
            json!({"action":"ask","workspaceId":WORKSPACE,"threadId":THREAD,"message":"deploy"}),
        )
        .await
        .unwrap();
    assert_eq!(result["status"], "needs_attention");
    assert_eq!(result["turn"]["permissions"].as_array().unwrap().len(), 1);
    assert!(
        f.data
            .lock()
            .unwrap()
            .requests
            .iter()
            .all(|r| r.0 != "POST" || !r.1.contains("permissions"))
    );
    f.data.lock().unwrap().wrong_turn = true;
    assert!(
        cloud
            .workspace(
                json!({"action":"abort","workspaceId":WORKSPACE,"threadId":THREAD,"turnId":TURN})
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("does not belong")
    );
    assert_eq!(
        f.count("POST", &format!("/runtime/v1/turns/{TURN}/interrupt")),
        0
    );
}

#[tokio::test]
async fn timeout_and_history_cursors_continue_without_resubmitting() {
    let f = Fixture::new(true).await;
    f.data.lock().unwrap().running_turn = true;
    let cloud = f.cloud(CancellationToken::new()).await;
    let result=cloud.workspace(json!({"action":"ask","workspaceId":WORKSPACE,"threadId":THREAD,"message":"hello","timeoutSeconds":1})).await.unwrap();
    assert_eq!(result["status"], "running");
    assert_eq!(result["turnId"], TURN);
    let first = cloud
        .workspace(
            json!({"action":"messages","workspaceId":WORKSPACE,"threadId":THREAD,"after":0,"limit":2}),
        )
        .await
        .unwrap();
    assert_eq!(first["messages"]["nextAfter"], 2);
    assert_eq!(first["messages"]["hasMore"], true);
    let next = cloud
        .workspace(
            json!({"action":"messages","workspaceId":WORKSPACE,"threadId":THREAD,"after":2,"limit":2}),
        )
        .await
        .unwrap();
    assert_eq!(next["messages"]["items"][0]["sequence"], 3);
    assert_eq!(
        f.count("POST", &format!("/runtime/v1/threads/{THREAD}/turns")),
        1
    );
}

#[tokio::test]
async fn cancellation_interrupts_only_its_own_turn_and_preserves_workspace() {
    let f = Fixture::new(true).await;
    f.data.lock().unwrap().running_turn = true;
    let cancel = CancellationToken::new();
    let cloud = f.cloud(cancel.clone()).await;
    let data = f.data.clone();
    let cancelling = tokio::spawn(async move {
        loop {
            if data
                .lock()
                .unwrap()
                .requests
                .iter()
                .any(|r| r.1.starts_with("/runtime/v1/permissions"))
            {
                cancel.cancel();
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    });
    assert!(
        cloud
            .workspace(
                json!({"action":"ask","workspaceId":WORKSPACE,"threadId":THREAD,"message":"hello"})
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    cancelling.await.unwrap();
    assert_eq!(
        f.count("POST", &format!("/runtime/v1/turns/{TURN}/interrupt")),
        1
    );
    assert_eq!(
        f.count("DELETE", &format!("/api/v1/workspaces/{WORKSPACE}")),
        0
    );
    assert_eq!(
        f.count("POST", &format!("/api/v1/workspaces/{WORKSPACE}/stop")),
        0
    );
}

#[tokio::test]
async fn lifecycle_mutations_use_workspace_routes_and_workspaces_have_no_archive() {
    let f = Fixture::new(true).await;
    let cloud = f.cloud(CancellationToken::new()).await;
    // Cloud workspaces are kept until they are deleted; there is nothing to archive.
    assert!(
        cloud
            .workspace(json!({"action":"archive","workspaceId":WORKSPACE}))
            .await
            .is_err()
    );
    assert_eq!(
        f.count("PATCH", &format!("/api/v1/workspaces/{WORKSPACE}")),
        0
    );
    let renamed = cloud
        .workspace(json!({"action":"rename","workspaceId":WORKSPACE,"title":"Renamed"}))
        .await
        .unwrap();
    assert_eq!(renamed["title"], "Renamed");
    for action in ["stop", "delete"] {
        let result = cloud
            .workspace(json!({"action":action,"workspaceId":WORKSPACE}))
            .await
            .unwrap();
        assert_eq!(result["status"], "accepted");
        assert_eq!(result["operation"]["id"], action);
    }
    let deleted = cloud
        .agent(json!({"action":"delete","id":AGENT}))
        .await
        .unwrap();
    assert_eq!(deleted["existingWorkspacesPreserved"], true);
    assert_eq!(
        f.count("DELETE", &format!("/api/v1/workspaces/{WORKSPACE}")),
        1
    );
    assert_eq!(
        f.count("DELETE", &format!("/api/v1/cloud-agents/{AGENT}")),
        1
    );
}

#[tokio::test]
async fn attachment_cannot_redirect_bearers_to_insecure_hosts() {
    let f = Fixture::new(true).await;
    f.data.lock().unwrap().invalid_gateway = true;
    let cloud = f.cloud(CancellationToken::new()).await;
    assert!(
        cloud
            .workspace(json!({"action":"messages","workspaceId":WORKSPACE}))
            .await
            .unwrap_err()
            .to_string()
            .contains("HTTPS")
    );
    assert!(
        f.data
            .lock()
            .unwrap()
            .requests
            .iter()
            .all(|r| !r.1.starts_with("/runtime/"))
    );
}

#[tokio::test]
async fn independent_workspaces_inline_configuration_revision_and_save_template() {
    let fixture = Fixture::new(true).await;
    let cloud = Cloud::new(
        Client::new(fixture.config.clone()).unwrap(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let configuration = json!({"instructions":"Independent","mcp":{"docs":{"command":"echo","args":["ok"]}},"agents":{"reviewer":{"prompt":"Review"}},"skills":{"review":{"files":{"SKILL.md":"Review"}}},"permissions":{"bash":"ask"}});
    cloud
        .workspace(json!({"action":"create","start":false,"configuration":configuration}))
        .await
        .unwrap();
    {
        let data = fixture.data.lock().unwrap();
        let request = data
            .requests
            .iter()
            .find(|(m, p, _)| m == "POST" && p.starts_with("/api/v1/workspaces?"))
            .unwrap();
        assert!(request.2["cloudAgentId"].is_null());
        assert_eq!(request.2["configuration"], configuration);
        assert_eq!(request.2["accountId"], ACCOUNT);
        assert_eq!(request.2["start"], false);
        assert!(
            !data
                .requests
                .iter()
                .any(|(_, p, _)| p.contains("cloud-agents/"))
        );
    }
    let updated = cloud
        .workspace(
            json!({"action":"update","workspaceId":WORKSPACE,"version":3,"configuration":{"mcp":{}}}),
        )
        .await
        .unwrap();
    assert_eq!(updated, json!({"version":3,"configuration":{"mcp":{}}}));
    assert!(
        cloud
            .workspace(
                json!({"action":"update","workspaceId":WORKSPACE,"version":1,"configuration":{}})
            )
            .await
            .is_err()
    );
    let saved = cloud
        .workspace(
            json!({"action":"save_agent","workspaceId":WORKSPACE,"version":3,"name":"Reusable"}),
        )
        .await
        .unwrap();
    assert_eq!(saved["name"], "Reusable");
    assert!(saved.get("includeEnvironment").is_none());
    let key = cloud
        .workspace(json!({"action":"ssh_public_key","workspaceId":WORKSPACE}))
        .await
        .unwrap();
    assert_eq!(key["sshPublicKey"], "ssh-ed25519 workspace-public");
}

async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !predicate() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture condition");
}
fn turn_event(kind: &str) -> Value {
    json!({"type":kind,"threadId":THREAD,"rootThreadId":THREAD,"turnId":TURN})
}

#[tokio::test]
async fn startup_uses_websocket_and_ignores_other_workspaces_without_polling() {
    let f = Fixture::new(true).await;
    f.enable_events();
    f.data.lock().unwrap().hold_startup = true;
    let cloud = f.cloud(CancellationToken::new()).await;
    let task = tokio::spawn(async move {
        cloud
            .workspace(json!({"action":"start","workspaceId":WORKSPACE,"timeoutSeconds":10}))
            .await
    });
    until(|| {
        let connected = f.data.lock().unwrap().control_connections == 1;
        connected && f.count("GET", &format!("/api/v1/workspaces/{WORKSPACE}")) >= 2
    })
    .await;
    let reads = f.count("GET", &format!("/api/v1/workspaces/{WORKSPACE}"));
    {
        let d = f.data.lock().unwrap();
        d.control_events
            .as_ref()
            .unwrap()
            .send(
                json!({"type":"entity.changed","accountId":ACCOUNT,"entity":"workspace","id":OTHER}),
            )
            .unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    assert_eq!(
        f.count("GET", &format!("/api/v1/workspaces/{WORKSPACE}")),
        reads,
        "healthy stream must not poll"
    );
    let sent = std::time::Instant::now();
    {
        let mut d = f.data.lock().unwrap();
        d.hold_startup = false;
        d.control_events.as_ref().unwrap().send(json!({"type":"entity.changed","accountId":ACCOUNT,"entity":"workspace","id":WORKSPACE})).unwrap();
    }
    let result = tokio::time::timeout(std::time::Duration::from_millis(700), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result["status"], "running");
    assert!(sent.elapsed() < std::time::Duration::from_millis(700));
    until(|| f.data.lock().unwrap().live_connections == 0).await;
}

#[tokio::test]
async fn runtime_completion_is_push_driven_and_tokens_do_not_trigger_reads() {
    let f = Fixture::new(true).await;
    f.enable_events();
    f.data.lock().unwrap().running_turn = true;
    let cloud = f.cloud(CancellationToken::new()).await;
    let task = tokio::spawn(async move {
        cloud
            .workspace(
                json!({"action":"ask","workspaceId":WORKSPACE,"threadId":THREAD,"message":"hello"}),
            )
            .await
    });
    until(|| f.count("GET", "/runtime/v1/permissions") == 1).await;
    assert_eq!(f.data.lock().unwrap().runtime_connections, 1);
    let reads = f.count("GET", &format!("/runtime/v1/turns/{TURN}"));
    {
        let d = f.data.lock().unwrap();
        for _ in 0..20 {
            d.runtime_events
                .as_ref()
                .unwrap()
                .send(turn_event("item.delta"))
                .unwrap();
        }
        d.runtime_events
            .as_ref()
            .unwrap()
            .send(turn_event("turn.text_delta"))
            .unwrap();
        d.runtime_events
            .as_ref()
            .unwrap()
            .send(turn_event("turn.reasoning_delta"))
            .unwrap();
        let mut unrelated = turn_event("turn.finished");
        unrelated["turnId"] = json!(OTHER);
        d.runtime_events.as_ref().unwrap().send(unrelated).unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    assert_eq!(f.count("GET", &format!("/runtime/v1/turns/{TURN}")), reads);
    {
        let mut d = f.data.lock().unwrap();
        d.running_turn = false;
        d.runtime_events
            .as_ref()
            .unwrap()
            .send(turn_event("turn.finished"))
            .unwrap();
    }
    let result = tokio::time::timeout(std::time::Duration::from_millis(700), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["messages"]["items"].as_array().unwrap().len(), 5);
    assert_eq!(
        f.count("POST", &format!("/runtime/v1/threads/{THREAD}/turns")),
        1
    );
    until(|| f.data.lock().unwrap().live_connections == 0).await;
}

#[tokio::test]
async fn child_permission_event_wakes_wait_without_autoapproval() {
    let f = Fixture::new(true).await;
    f.enable_events();
    f.data.lock().unwrap().running_turn = true;
    let cloud = f.cloud(CancellationToken::new()).await;
    let task = tokio::spawn(async move {
        cloud
            .workspace(
                json!({"action":"wait","workspaceId":WORKSPACE,"threadId":THREAD,"turnId":TURN}),
            )
            .await
    });
    until(|| f.count("GET", "/runtime/v1/permissions") == 1).await;
    {
        let mut d = f.data.lock().unwrap();
        d.permission = true;
        d.runtime_events
            .as_ref()
            .unwrap()
            .send(json!({"type":"permission.requested","threadId":OTHER,"rootThreadId":THREAD}))
            .unwrap();
    }
    let result = tokio::time::timeout(std::time::Duration::from_millis(700), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result["status"], "needs_attention");
    assert_eq!(result["turn"]["permissions"].as_array().unwrap().len(), 1);
    assert!(
        f.data
            .lock()
            .unwrap()
            .requests
            .iter()
            .all(|(method, path, _)| method != "POST" || !path.contains("permissions"))
    );
}

#[tokio::test]
async fn runtime_reconnects_reattaches_and_resyncs_a_gap_without_resubmission() {
    let f = Fixture::new(true).await;
    f.enable_events();
    f.data.lock().unwrap().running_turn = true;
    let cloud = f.cloud(CancellationToken::new()).await;
    let task = tokio::spawn(async move {
        cloud
            .workspace(
                json!({"action":"ask","workspaceId":WORKSPACE,"threadId":THREAD,"message":"once"}),
            )
            .await
    });
    until(|| f.count("GET", "/runtime/v1/permissions") == 1).await;
    f.data
        .lock()
        .unwrap()
        .runtime_events
        .as_ref()
        .unwrap()
        .send(json!({"_close":true}))
        .unwrap();
    until(|| f.data.lock().unwrap().runtime_connections >= 2).await;
    // A resync frame represents a server replay gap; completion itself was missed.
    {
        let mut d = f.data.lock().unwrap();
        d.running_turn = false;
        d.runtime_events
            .as_ref()
            .unwrap()
            .send(json!({"type":"resync_required"}))
            .unwrap();
    }
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result["status"], "completed");
    assert!(f.data.lock().unwrap().attachments >= 2);
    assert_eq!(
        f.count("POST", &format!("/runtime/v1/threads/{THREAD}/turns")),
        1
    );
}

#[tokio::test]
async fn unsupported_stream_falls_back_and_dropped_wait_closes_subscription() {
    let f = Fixture::new(true).await;
    f.data.lock().unwrap().running_turn = true;
    let cloud = f.cloud(CancellationToken::new()).await;
    let task = tokio::spawn(async move {
        cloud
            .workspace(
                json!({"action":"wait","workspaceId":WORKSPACE,"threadId":THREAD,"turnId":TURN}),
            )
            .await
    });
    until(|| f.count("GET", "/runtime/v1/permissions") == 1).await;
    f.data.lock().unwrap().running_turn = false;
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap()["status"],
        "completed"
    );

    f.enable_events();
    f.data.lock().unwrap().running_turn = true;
    let cloud = f.cloud(CancellationToken::new()).await;
    let task = tokio::spawn(async move {
        cloud
            .workspace(
                json!({"action":"wait","workspaceId":WORKSPACE,"threadId":THREAD,"turnId":TURN}),
            )
            .await
    });
    until(|| f.data.lock().unwrap().live_connections == 1).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    until(|| f.data.lock().unwrap().live_connections == 0).await;
    assert_eq!(
        f.count("POST", &format!("/runtime/v1/turns/{TURN}/interrupt")),
        0
    );
}

#[tokio::test]
async fn streaming_timeout_and_cancellation_preserve_continuation_and_ownership() {
    let f = Fixture::new(true).await;
    f.enable_events();
    f.data.lock().unwrap().running_turn = true;
    let cloud = f.cloud(CancellationToken::new()).await;
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), cloud.workspace(json!({"action":"ask","workspaceId":WORKSPACE,"threadId":THREAD,"message":"slow","timeoutSeconds":1}))).await.unwrap().unwrap();
    assert_eq!(result["status"], "running");
    assert_eq!(result["turnId"], TURN);
    until(|| f.data.lock().unwrap().live_connections == 0).await;
    for asking in [false, true] {
        let cancel = CancellationToken::new();
        let cloud = f.cloud(cancel.clone()).await;
        let task = tokio::spawn(async move {
            cloud.workspace(json!({"action":if asking {"ask"} else {"wait"},"workspaceId":WORKSPACE,"threadId":THREAD,"turnId":TURN,"message":"cancel"})).await
        });
        until(|| f.data.lock().unwrap().live_connections == 1).await;
        cancel.cancel();
        let error = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        until(|| f.data.lock().unwrap().live_connections == 0).await;
        assert_eq!(
            f.count("POST", &format!("/runtime/v1/turns/{TURN}/interrupt")),
            usize::from(asking)
        );
    }
    assert_eq!(
        f.count("POST", &format!("/api/v1/workspaces/{WORKSPACE}/stop")),
        0
    );
    assert_eq!(
        f.count("DELETE", &format!("/api/v1/workspaces/{WORKSPACE}")),
        0
    );
}

#[tokio::test]
async fn control_socket_refreshes_rejected_oauth_and_resyncs_after_reconnect() {
    let f = Fixture::new(true).await;
    f.enable_events();
    {
        let mut d = f.data.lock().unwrap();
        d.hold_startup = true;
        d.reject_control_token = true;
    }
    let cloud = f.cloud(CancellationToken::new()).await;
    let task = tokio::spawn(async move {
        cloud
            .workspace(json!({"action":"start","workspaceId":WORKSPACE,"timeoutSeconds":10}))
            .await
    });
    until(|| f.data.lock().unwrap().control_connections == 1).await;
    assert!(f.data.lock().unwrap().account_rotated);
    assert_eq!(f.data.lock().unwrap().control_rejections, 1);
    f.data
        .lock()
        .unwrap()
        .control_events
        .as_ref()
        .unwrap()
        .send(json!({"_close":true}))
        .unwrap();
    until(|| f.data.lock().unwrap().control_connections == 2).await;
    {
        let mut d = f.data.lock().unwrap();
        d.hold_startup = false;
        d.control_events
            .as_ref()
            .unwrap()
            .send(json!({"type":"resync_required"}))
            .unwrap();
    }
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap()["status"],
        "running"
    );
}

#[tokio::test]
async fn runtime_generation_changes_reattach_reads_but_never_replay_asks() {
    let f = Fixture::new(true).await;
    f.data.lock().unwrap().stale_generation = true;
    let cloud = f.cloud(CancellationToken::new()).await;
    let result = cloud
        .workspace(json!({"action":"messages","workspaceId":WORKSPACE,"threadId":THREAD}))
        .await
        .unwrap();
    assert_eq!(result["messages"]["items"].as_array().unwrap().len(), 5);
    assert_eq!(f.data.lock().unwrap().attachments, 2);
    let f = Fixture::new(true).await;
    f.data.lock().unwrap().stale_generation_on_write = true;
    let cloud = f.cloud(CancellationToken::new()).await;
    assert!(
        cloud
            .workspace(
                json!({"action":"ask","workspaceId":WORKSPACE,"threadId":THREAD,"message":"once"})
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("workspace restarted")
    );
    assert_eq!(f.data.lock().unwrap().attachments, 1);
    assert_eq!(
        f.count("POST", &format!("/runtime/v1/threads/{THREAD}/turns")),
        1
    );
}

#[tokio::test]
async fn ask_launches_freestyle_or_a_listed_template_with_inline_overrides() {
    for templated in [false, true] {
        let f = Fixture::new(true).await;
        let cloud = f.cloud(CancellationToken::new()).await;
        let mut arguments = json!({"action":"ask","message":"Review this","configuration":{"instructions":"Inline override","model":"openai/gpt-5-nano"},"wait":false});
        if templated {
            let listed = cloud
                .agent(json!({"action":"list","search":"fixture"}))
                .await
                .unwrap();
            arguments["cloudAgentId"] = listed["items"][0]["id"].clone();
        }
        let result = cloud.workspace(arguments).await.unwrap();
        assert_eq!(result["messageSubmitted"], true);
        let d = f.data.lock().unwrap();
        let created = &d
            .requests
            .iter()
            .find(|(m, p, _)| m == "POST" && p == "/api/v1/workspaces?")
            .unwrap()
            .2;
        assert_eq!(created["configuration"]["instructions"], "Inline override");
        assert_eq!(
            created["cloudAgentId"],
            if templated { json!(AGENT) } else { Value::Null }
        );
        if !templated {
            assert!(
                d.requests
                    .iter()
                    .all(|(_, p, _)| !p.contains("cloud-agents"))
            );
        }
    }
}

#[tokio::test]
async fn cancelled_startup_returns_the_retained_workspace_id_and_closes_socket() {
    let f = Fixture::new(true).await;
    f.enable_events();
    f.data.lock().unwrap().hold_startup = true;
    let cancel = CancellationToken::new();
    let cloud = f.cloud(cancel.clone()).await;
    let task = tokio::spawn(async move {
        cloud
            .workspace(json!({"action":"create","configuration":{"model":"openai/gpt-5-mini"}}))
            .await
    });
    until(|| f.data.lock().unwrap().control_connections == 1).await;
    cancel.cancel();
    let error = tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("cancelled") && error.contains(WORKSPACE),
        "{error}"
    );
    until(|| f.data.lock().unwrap().live_connections == 0).await;
    assert_eq!(f.count("POST", "/api/v1/workspaces"), 1);
    assert_eq!(
        f.count("POST", &format!("/api/v1/workspaces/{WORKSPACE}/stop")),
        0
    );
    assert_eq!(
        f.count("DELETE", &format!("/api/v1/workspaces/{WORKSPACE}")),
        0
    );
}

#[tokio::test]
async fn chats_share_one_workspace_but_require_explicit_conversation_targets() {
    let fixture = Fixture::new(true).await;
    let cloud = fixture.cloud(CancellationToken::new()).await;
    let first = cloud
        .chat(json!({"action":"create","workspaceId":WORKSPACE,"title":"Implement"}))
        .await
        .unwrap();
    let second = cloud
        .chat(json!({"action":"create","workspaceId":WORKSPACE,"title":"Review"}))
        .await
        .unwrap();
    assert_ne!(first["chatId"], second["chatId"]);
    assert_eq!(first["workspaceId"], second["workspaceId"]);
    let list = cloud
        .chat(json!({"action":"list","workspaceId":WORKSPACE,"limit":1}))
        .await
        .unwrap();
    assert_eq!(list["chats"].as_array().unwrap().len(), 1);
    assert_eq!(list["nextCursor"], THREAD);
    let next = cloud
        .chat(json!({"action":"list","workspaceId":WORKSPACE,"cursor":THREAD}))
        .await
        .unwrap();
    assert_eq!(next["chats"][0]["id"], OTHER);
    assert!(
        cloud
            .workspace(json!({"action":"ask","workspaceId":WORKSPACE,"message":"ambiguous"}))
            .await
            .unwrap_err()
            .to_string()
            .contains("Multiple chats")
    );
    assert!(
        cloud
            .chat(json!({"action":"ask","workspaceId":WORKSPACE,"message":"ambiguous"}))
            .await
            .unwrap_err()
            .to_string()
            .contains("chatId")
    );
    let result = cloud.chat(json!({"action":"ask","workspaceId":WORKSPACE,"chatId":THREAD,"message":"explicit","wait":false})).await.unwrap();
    assert_eq!(result["chatId"], THREAD);
    cloud
        .chat(
            json!({"action":"rename","workspaceId":WORKSPACE,"chatId":OTHER,"title":"Review only"}),
        )
        .await
        .unwrap();
    cloud
        .chat(json!({"action":"archive","workspaceId":WORKSPACE,"chatId":OTHER}))
        .await
        .unwrap();
    assert_eq!(
        cloud
            .chat(json!({"action":"list","workspaceId":WORKSPACE}))
            .await
            .unwrap()["chats"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        cloud
            .chat(json!({"action":"list","workspaceId":WORKSPACE,"archived":true}))
            .await
            .unwrap()["chats"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let d = fixture.data.lock().unwrap();
    assert!(!d.created && !d.started && !d.stopped);
    assert!(
        !d.requests
            .iter()
            .any(|(method, path, _)| method == "POST" && path.starts_with("/api/v1/workspaces?"))
    );
}

#[tokio::test]
async fn chat_reads_and_creation_do_not_start_stopped_compute_or_cross_accounts() {
    let fixture = Fixture::new(true).await;
    let cloud = fixture.cloud(CancellationToken::new()).await;
    fixture.data.lock().unwrap().stopped = true;
    for action in ["list", "create"] {
        assert!(
            cloud
                .chat(json!({"action":action,"workspaceId":WORKSPACE}))
                .await
                .unwrap_err()
                .to_string()
                .contains("Start the Cloud workspace")
        );
    }
    fixture.data.lock().unwrap().foreign_account = true;
    assert!(
        cloud
            .chat(json!({"action":"list","workspaceId":WORKSPACE}))
            .await
            .is_err()
    );
    assert_eq!(fixture.data.lock().unwrap().attachments, 0);
}

#[tokio::test]
async fn cloud_schedule_adapter_is_explicit_scoped_versioned_and_never_installs_local_timers() {
    let fixture = Fixture::new(true).await;
    let cloud = fixture.cloud(CancellationToken::new()).await;
    let created=cloud.schedule(json!({"action":"create","workspaceId":WORKSPACE,"chatId":THREAD,"name":"Hourly review","prompt":"Review changes","everySeconds":3600})).await.unwrap();
    assert_eq!(created["chatId"], THREAD);
    assert_eq!(
        cloud
            .schedule(json!({"action":"list","workspaceId":WORKSPACE}))
            .await
            .unwrap()["items"][0]["id"],
        AGENT
    );
    let paused = cloud
        .schedule(json!({"action":"pause","id":AGENT,"version":1}))
        .await
        .unwrap();
    assert_eq!(paused["enabled"], false);
    assert!(
        cloud
            .schedule(json!({"action":"resume","id":AGENT,"version":1}))
            .await
            .unwrap_err()
            .to_string()
            .contains("changed")
    );
    assert_eq!(
        cloud
            .schedule(json!({"action":"resume","id":AGENT,"version":2}))
            .await
            .unwrap()["enabled"],
        true
    );
    assert_eq!(
        cloud
            .schedule(json!({"action":"run","id":AGENT}))
            .await
            .unwrap()["chatId"],
        THREAD
    );
    assert_eq!(
        cloud
            .schedule(json!({"action":"history","id":AGENT}))
            .await
            .unwrap()["items"][0]["chatId"],
        THREAD
    );
    fixture.data.lock().unwrap().foreign_account = true;
    assert!(
        cloud
            .schedule(json!({"action":"run","id":AGENT}))
            .await
            .is_err()
    );
    fixture.data.lock().unwrap().foreign_account = false;
    cloud
        .schedule(json!({"action":"delete","id":AGENT}))
        .await
        .unwrap();
    let d = fixture.data.lock().unwrap();
    assert!(d.schedule.is_none());
    assert_eq!(d.attachments, 0);
    assert!(!d.started && !d.created);
}
