//! Exercise the account-to-control-plane-to-runtime boundary over real HTTP.
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::{Duration, Utc};
use haarness_account::{Client, Config, Credentials, cloud::Cloud, credentials};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

const ACCOUNT: &str = "00000000-0000-4000-8000-000000000001";
const AGENT: &str = "00000000-0000-4000-8000-000000000002";
const SESSION: &str = "00000000-0000-4000-8000-000000000003";
const THREAD: &str = "00000000-0000-4000-8000-000000000004";
const TURN: &str = "00000000-0000-4000-8000-000000000005";
const OTHER: &str = "00000000-0000-4000-8000-000000000006";

#[derive(Default)]
struct Data {
    base: String,
    requests: Vec<(String, String, Value)>,
    attachments: u32,
    session_reads: u32,
    created: bool,
    started: bool,
    stopped: bool,
    running_turn: bool,
    permission: bool,
    wrong_turn: bool,
    reject_ticket: bool,
    invalid_gateway: bool,
    foreign_account: bool,
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
        if d.reject_ticket && bearer == "Bearer ticket-1" {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error":{"message":"expired ticket"}})),
            )
                .into_response();
        }
    } else if path.starts_with("/router/") {
        assert_eq!(bearer, "Bearer router-secret");
    } else {
        assert_eq!(bearer, "Bearer account-secret");
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
        ("POST", "/api/v1/sessions") => {
            d.created = true;
            json!({"session":{"id":SESSION,"workspace":{"observedState":"pending"}}})
        }
        ("GET", p) if p == format!("/api/v1/sessions/{SESSION}") => {
            d.session_reads += 1;
            let observed = if (d.created || d.started) && d.session_reads <= 2 || d.stopped {
                "stopped"
            } else {
                "running"
            };
            json!({"id":SESSION,"accountId":if d.foreign_account {OTHER}else{ACCOUNT},"agentSnapshot":agent(&d),"workspace":{"observedState":observed,"desiredState":if d.stopped {"stopped"} else {"running"}}})
        }
        ("POST", p) if p == format!("/api/v1/sessions/{SESSION}/attach") => {
            d.attachments += 1;
            json!({"gatewayUrl":if d.invalid_gateway {"http://untrusted.example/runtime/".into()} else {format!("{}/runtime/",d.base)},"ticket":format!("ticket-{}",d.attachments),"expiresAt":Utc::now()+Duration::hours(1)})
        }
        ("POST", p) if p == format!("/api/v1/sessions/{SESSION}/start") => {
            d.started = true;
            d.stopped = false;
            d.session_reads = 0;
            json!({"operation":{"id":"start"}})
        }
        ("POST", p) if p == format!("/api/v1/sessions/{SESSION}/stop") => {
            d.stopped = true;
            json!({"operation":{"id":"stop"}})
        }
        ("DELETE", p) if p == format!("/api/v1/sessions/{SESSION}") => {
            json!({"operation":{"id":"delete"}})
        }
        ("PATCH", p) if p == format!("/api/v1/sessions/{SESSION}") => body,
        ("GET", "/runtime/v1/threads") => json!([]),
        ("GET", "/runtime/v1/info") => {
            json!({"defaultAgent":"cloud-agent","defaultProvider":"haarness","defaultModel":"openai/gpt-5-nano"})
        }
        ("POST", "/runtime/v1/threads") => json!({"id":THREAD}),
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
        let app = Router::new().fallback(handler).with_state(data.clone());
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
            .session(json!({"action":"ask","cloudAgentId":AGENT,"message":"  "}))
            .await
            .is_err()
    );
    assert_eq!(f.count("POST", "/api/v1/sessions"), 0);
    let result=cloud.session(json!({"action":"ask","cloudAgentId":AGENT,"message":"hello","wait":false,"timeoutSeconds":10})).await.unwrap();
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
        assert!(ask.2.get("autoApprove").is_none());
        assert_eq!(ask.2["tools"]["question"], false);
    }
    let done = cloud
        .session(json!({"action":"wait","sessionId":SESSION,"threadId":THREAD,"turnId":TURN}))
        .await
        .unwrap();
    assert_eq!(done["status"], "completed");
    assert_eq!(f.count("POST", "/api/v1/sessions"), 1);
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
            .session(json!({"action":action,"sessionId":SESSION}))
            .await
            .unwrap();
        assert_eq!(result["status"], "stopped");
    }
    assert_eq!(f.data.lock().unwrap().attachments, 0);
    assert_eq!(
        f.count("POST", &format!("/api/v1/sessions/{SESSION}/start")),
        0
    );
    let result = cloud
        .session(json!({"action":"start","sessionId":SESSION,"timeoutSeconds":10}))
        .await
        .unwrap();
    assert_eq!(result["status"], "running");
    assert!(f.data.lock().unwrap().session_reads >= 3);
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
        .session(json!({"action":"ask","sessionId":SESSION,"threadId":THREAD,"message":"deploy"}))
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
            .session(json!({"action":"abort","sessionId":SESSION,"threadId":THREAD,"turnId":TURN}))
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
    let result=cloud.session(json!({"action":"ask","sessionId":SESSION,"threadId":THREAD,"message":"hello","timeoutSeconds":1})).await.unwrap();
    assert_eq!(result["status"], "running");
    assert_eq!(result["turnId"], TURN);
    let first = cloud
        .session(
            json!({"action":"messages","sessionId":SESSION,"threadId":THREAD,"after":0,"limit":2}),
        )
        .await
        .unwrap();
    assert_eq!(first["messages"]["nextAfter"], 2);
    assert_eq!(first["messages"]["hasMore"], true);
    let next = cloud
        .session(
            json!({"action":"messages","sessionId":SESSION,"threadId":THREAD,"after":2,"limit":2}),
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
async fn cancellation_interrupts_only_its_own_turn_and_preserves_session() {
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
            .session(
                json!({"action":"ask","sessionId":SESSION,"threadId":THREAD,"message":"hello"})
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
    assert_eq!(f.count("DELETE", &format!("/api/v1/sessions/{SESSION}")), 0);
    assert_eq!(
        f.count("POST", &format!("/api/v1/sessions/{SESSION}/stop")),
        0
    );
}

#[tokio::test]
async fn lifecycle_mutations_use_session_routes_and_archive_defaults_to_true() {
    let f = Fixture::new(true).await;
    let cloud = f.cloud(CancellationToken::new()).await;
    let archived = cloud
        .session(json!({"action":"archive","sessionId":SESSION}))
        .await
        .unwrap();
    assert_eq!(archived["archived"], true);
    let restored = cloud
        .session(json!({"action":"archive","sessionId":SESSION,"archived":false}))
        .await
        .unwrap();
    assert_eq!(restored["archived"], false);
    let renamed = cloud
        .session(json!({"action":"rename","sessionId":SESSION,"title":"Renamed"}))
        .await
        .unwrap();
    assert_eq!(renamed["title"], "Renamed");
    for action in ["stop", "delete"] {
        let result = cloud
            .session(json!({"action":action,"sessionId":SESSION}))
            .await
            .unwrap();
        assert_eq!(result["status"], "accepted");
        assert_eq!(result["operation"]["id"], action);
    }
    let deleted = cloud
        .agent(json!({"action":"delete","id":AGENT}))
        .await
        .unwrap();
    assert_eq!(deleted["existingSessionsPreserved"], true);
    assert_eq!(f.count("DELETE", &format!("/api/v1/sessions/{SESSION}")), 1);
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
            .session(json!({"action":"messages","sessionId":SESSION}))
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
