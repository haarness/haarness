//! `haarness run --session` on a chat whose turn another window or run
//! started: the run says so and stops, and changes nothing in that chat. A
//! workflow, which runs in a chat of its own, still runs. A turn that starts
//! after the run looked is refused by the server, and the run puts the
//! chat's mode back.

use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use axum::{
    Router,
    extract::{Request, State},
    response::{IntoResponse, Response},
};

use serde_json::{Value, json};
use tokio::{io::AsyncWriteExt as _, process::Command, time::timeout};
use uuid::Uuid;

const BUSY: &str = "This chat already has a turn running, started in another window or run. It takes a new message once that turn finishes or is stopped.";

struct Fixture {
    root: PathBuf,
    workspace: PathBuf,
    data: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("haarness-held-{}", Uuid::now_v7()));
        let fixture = Self {
            workspace: root.join("workspace"),
            data: root.join("data"),
            home: root.join("home"),
            root,
        };
        for directory in [&fixture.workspace, &fixture.data, &fixture.home] {
            std::fs::create_dir_all(directory).expect("create held-session fixture directory");
        }
        // A write asks, so a turn that writes waits for a person.
        std::fs::create_dir_all(fixture.workspace.join(".haarness")).unwrap();
        std::fs::write(
            fixture.workspace.join(".haarness/config.toml"),
            "[defaults]\nprovider = \"echo\"\nmodel = \"echo-1\"\n[permissions]\nwrite = \"ask\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(fixture.workspace.join(".haarness/workflows")).unwrap();
        std::fs::write(
            fixture.workspace.join(".haarness/workflows/hello.js"),
            "export const meta = { name: \"hello\" }\nreturn \"hello from the workflow\"\n",
        )
        .unwrap();
        fixture
    }

    fn haarness(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_haarness"));
        command
            .arg("--workspace")
            .arg(&self.workspace)
            .arg("--data-dir")
            .arg(&self.data)
            .arg("--trust-project")
            .current_dir(&self.workspace)
            .env("HOME", &self.home)
            .env("HAARNESS_HOME", &self.home)
            .env("HAARNESS_TEST_PROVIDER", "1")
            .env("RUST_LOG", "warn")
            .stdin(Stdio::null());
        command
    }

    /// The folder's server, started the way Desktop starts it.
    async fn connect(&self) -> (String, String) {
        let mut command = self.haarness();
        let mut child = command
            .arg("desktop")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("launch haarness desktop");
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(br#"{"action":"connect"}"#).await.unwrap();
        drop(stdin);
        let output = timeout(Duration::from_secs(90), child.wait_with_output())
            .await
            .expect("the server starts")
            .unwrap();
        let connection: Value = serde_json::from_slice(&output.stdout).unwrap();
        (
            connection["url"]
                .as_str()
                .unwrap()
                .trim_end_matches('/')
                .to_owned(),
            connection["token"].as_str().unwrap().to_owned(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_haarness"))
            .arg("--workspace")
            .arg(&self.workspace)
            .arg("--data-dir")
            .arg(&self.data)
            .args(["serve", "--stop"])
            .env("HOME", &self.home)
            .env("HAARNESS_HOME", &self.home)
            .env("HAARNESS_TEST_PROVIDER", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

async fn api(
    (url, token): &(String, String),
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> Value {
    let mut request = reqwest::Client::new()
        .request(method, format!("{url}/v1{path}"))
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    assert!(
        response.status().is_success(),
        "{path}: {}",
        response.status()
    );
    let bytes = response.bytes().await.unwrap();
    if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    }
}

async fn approval_mode(server: &(String, String), thread: &str) -> Value {
    api(
        server,
        reqwest::Method::GET,
        &format!("/threads/{thread}/approval-mode"),
        None,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_for_a_chat_busy_elsewhere_says_so_and_leaves_the_chat_as_it_was() {
    let fixture = Fixture::new();
    let server = fixture.connect().await;
    let thread = api(
        &server,
        reqwest::Method::POST,
        "/threads",
        Some(json!({"provider":"echo", "model":"echo-1"})),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // Another window's turn waits for its approval.
    let write = json!({"path":"elsewhere.txt", "content":"ELSEWHERE"});
    let held = api(
        &server,
        reqwest::Method::POST,
        &format!("/threads/{thread}/turns"),
        Some(json!({"requestId":Uuid::new_v4(), "text":format!("!tool write {write}")})),
    )
    .await;
    let permission = timeout(Duration::from_secs(20), async {
        loop {
            let pending = api(&server, reqwest::Method::GET, "/permissions", None).await;
            if let Some(id) = pending[0]["permissionId"].as_str() {
                break id.to_owned();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the other window's turn asks");
    let before = approval_mode(&server, &thread).await;
    assert_ne!(before["mode"], "auto");

    for auto in [true, false] {
        let mut command = fixture.haarness();
        if auto {
            command.arg("--auto");
        }
        let output = timeout(
            Duration::from_secs(60),
            command
                .args(["run", "--session", &thread, "hello"])
                .output(),
        )
        .await
        .expect("the run ends")
        .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "auto={auto}: {stderr}");
        assert!(stderr.contains(BUSY), "auto={auto}: {stderr}");
        let after = approval_mode(&server, &thread).await;
        assert_eq!(after["mode"], before["mode"], "auto={auto}: {after}");
        assert_eq!(
            after["revision"], before["revision"],
            "auto={auto}: {after}"
        );
    }
    let turn = api(
        &server,
        reqwest::Method::GET,
        &format!("/turns/{}", held["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(turn["status"], "running", "the other turn runs on");
    // A workflow runs in a chat of its own: the busy chat does not refuse it.
    if std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        let output = timeout(
            Duration::from_secs(120),
            fixture
                .haarness()
                .args(["--auto", "run", "--session", &thread, "--workflow", "hello"])
                .output(),
        )
        .await
        .expect("the workflow run ends")
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("hello from the workflow"),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    } else {
        eprintln!("skipping the workflow run: Bun is unavailable");
    }
    // `--auto` put the busy chat in Auto for the workflow, as it always did,
    // which may already have answered the other turn's request.
    let _ = reqwest::Client::new()
        .post(format!("{}/v1/permissions/{permission}", server.0))
        .bearer_auth(&server.1)
        .json(&json!({"decision":"reject"}))
        .send()
        .await;
}

/// Forwards every request to `upstream`, except that `hidden` (a chat's
/// active turn) answers that none runs: the run looks before another window
/// starts its turn, which the server then holds when the run's turn arrives.
struct Proxy {
    upstream: String,
    hidden: String,
    client: reqwest::Client,
}

async fn forward(State(proxy): State<Arc<Proxy>>, request: Request) -> Response {
    let path = request
        .uri()
        .path_and_query()
        .map(|path| path.as_str().to_owned())
        .unwrap_or_default();
    if request.method() == axum::http::Method::GET && path == proxy.hidden {
        return ([("content-type", "application/json")], "null").into_response();
    }
    let method = reqwest::Method::from_bytes(request.method().as_str().as_bytes()).unwrap();
    let mut forwarded = proxy
        .client
        .request(method, format!("{}{path}", proxy.upstream));
    for name in ["authorization", "content-type", "accept"] {
        if let Some(value) = request.headers().get(name) {
            forwarded = forwarded.header(name, value.as_bytes());
        }
    }
    let body = axum::body::to_bytes(request.into_body(), usize::MAX)
        .await
        .unwrap();
    let response = forwarded.body(body).send().await.unwrap();
    let status = axum::http::StatusCode::from_u16(response.status().as_u16()).unwrap();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_owned();
    let bytes = response.bytes().await.unwrap();
    (status, [("content-type", content_type)], bytes).into_response()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_refused_after_it_looked_puts_the_chats_mode_back() {
    let fixture = Fixture::new();
    let server = fixture.connect().await;
    let thread = api(
        &server,
        reqwest::Method::POST,
        "/threads",
        Some(json!({"provider":"echo", "model":"echo-1"})),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let write = json!({"path":"elsewhere.txt", "content":"ELSEWHERE"});
    api(
        &server,
        reqwest::Method::POST,
        &format!("/threads/{thread}/turns"),
        Some(json!({"requestId":Uuid::new_v4(), "text":format!("!tool write {write}")})),
    )
    .await;
    let permission = timeout(Duration::from_secs(20), async {
        loop {
            let pending = api(&server, reqwest::Method::GET, "/permissions", None).await;
            if let Some(id) = pending[0]["permissionId"].as_str() {
                break id.to_owned();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the other window's turn asks");
    let before = approval_mode(&server, &thread).await;
    assert_eq!(before["mode"], "review");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    let proxy = Arc::new(Proxy {
        upstream: server.0.clone(),
        hidden: format!("/v1/threads/{thread}/active"),
        client: reqwest::Client::new(),
    });
    let proxied = tokio::spawn(async move {
        axum::serve(listener, Router::new().fallback(forward).with_state(proxy))
            .await
            .unwrap();
    });
    let output = timeout(
        Duration::from_secs(60),
        fixture
            .haarness()
            .args([
                "--auto",
                "run",
                "--attach",
                &proxy_url,
                "--token",
                &server.1,
                "--session",
                &thread,
                "hello",
            ])
            .output(),
    )
    .await
    .expect("the run ends")
    .unwrap();
    proxied.abort();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains(BUSY), "{stderr}");
    let after = approval_mode(&server, &thread).await;
    assert_eq!(after["mode"], before["mode"], "{after}");
    assert_eq!(after["explicit"], before["explicit"], "{after}");
    // While the chat was in Auto, its reviewer may have answered the other
    // turn's request already: putting the mode back cannot undo that.
    let _ = reqwest::Client::new()
        .post(format!("{}/v1/permissions/{permission}", server.0))
        .bearer_auth(&server.1)
        .json(&json!({"decision":"reject"}))
        .send()
        .await;
}
