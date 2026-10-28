//! Haarness answers before its MCP servers have started. A server slow to
//! start (a first `npx -y` download) once kept the whole server from
//! listening until it was done, and launchers gave up after 8 seconds. A turn
//! begun while a server is still starting waits for it, so it gets the
//! server's tools as a later turn would, and Stop ends that wait.

use std::{
    net::SocketAddr,
    path::PathBuf,
    process::Stdio,
    time::{Duration, Instant},
};

use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tokio::{net::TcpListener, process::Command, time::timeout};
use uuid::Uuid;

const TOKEN: &str = "mcp-start-e2e-token";
const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../haarness-mcp/tests/fixtures/recovery.py"
);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("haarness-mcp-start-{}", Uuid::now_v7()));
        std::fs::create_dir_all(path.join("workspace/.haarness")).expect("create test project");
        std::fs::create_dir_all(path.join("home")).expect("create isolated home");
        Self(path)
    }
    fn workspace(&self) -> PathBuf {
        self.0.join("workspace")
    }
    /// A local server answering `initialize` after `delay` seconds.
    fn slow_server(&self, delay: &str) -> Value {
        json!({
            "command": "python3",
            "args": [FIXTURE],
            "env": {"INIT_DELAY": delay, "CALL_LOG": self.0.join("calls")}
        })
    }
    fn configure(&self, servers: &Value) {
        std::fs::write(
            self.workspace().join(".haarness/haarness.json"),
            serde_json::to_string_pretty(&json!({ "mcp": servers })).expect("serialize config"),
        )
        .expect("write project config");
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn unused_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    listener.local_addr().expect("ephemeral address")
}

fn server_command(directory: &TestDirectory, address: SocketAddr) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_haarness"));
    command
        .arg("--workspace")
        .arg(directory.workspace())
        .arg("--data-dir")
        .arg(directory.0.join("data"))
        .arg("--trust-project")
        .arg("serve")
        .arg("--host")
        .arg(address.ip().to_string())
        .arg("--port")
        .arg(address.port().to_string())
        .arg("--token")
        .arg(TOKEN)
        .env("HOME", directory.0.join("home"))
        .env("HAARNESS_TEST_PROVIDER", "1")
        .env("HAARNESS_TELEMETRY", "0")
        .env("HAARNESS_CLOUD_API_URL", "http://127.0.0.1:9")
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}

/// How long the server took to answer its first request.
async fn wait_until_answering(client: &Client, address: SocketAddr) -> Duration {
    let started = Instant::now();
    for _ in 0..600 {
        if client
            .get(format!("http://{address}/v1/mcp"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            return started.elapsed();
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("Haarness did not answer on {address}");
}

async fn get_json(client: &Client, address: SocketAddr, path: &str) -> Value {
    client
        .get(format!("http://{address}/v1{path}"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .expect("send API request")
        .error_for_status()
        .expect("successful API response")
        .json()
        .await
        .expect("JSON API response")
}

async fn post_json(client: &Client, address: SocketAddr, path: &str, body: &Value) -> Value {
    let response = client
        .post(format!("http://{address}/v1{path}"))
        .bearer_auth(TOKEN)
        .json(body)
        .send()
        .await
        .expect("send API request");
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    assert!(status.is_success(), "{path}: {status}: {text}");
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

fn mcp_status(servers: &Value, name: &str) -> Option<String> {
    servers.as_array()?.iter().find_map(|server| {
        (server["name"] == name).then(|| server["status"].as_str().unwrap_or_default().to_owned())
    })
}

async fn wait_for_mcp_status(client: &Client, address: SocketAddr, name: &str, status: &str) {
    let mut latest = Value::Null;
    for _ in 0..400 {
        latest = get_json(client, address, "/mcp").await;
        if mcp_status(&latest, name).as_deref() == Some(status) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("MCP server {name} did not reach {status}; latest={latest}");
}

async fn new_chat(client: &Client, address: SocketAddr) -> String {
    let thread = post_json(
        client,
        address,
        "/threads",
        &json!({"title": "MCP start", "provider": "echo", "model": "echo-1", "agent": "build"}),
    )
    .await;
    thread["id"].as_str().expect("thread id").to_owned()
}

/// Starts a turn in `thread` and returns its id.
async fn start_turn(client: &Client, address: SocketAddr, thread: &str, text: &str) -> String {
    let turn = post_json(
        client,
        address,
        &format!("/threads/{thread}/turns"),
        &json!({"requestId": Uuid::now_v7(), "text": text, "provider": "echo", "model": "echo-1"}),
    )
    .await;
    turn["id"].as_str().expect("turn id").to_owned()
}

async fn wait_until_idle(client: &Client, address: SocketAddr, thread: &str) -> Duration {
    let started = Instant::now();
    for _ in 0..600 {
        if get_json(client, address, &format!("/threads/{thread}/active")).await == Value::Null {
            return started.elapsed();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the turn in {thread} did not end");
}

/// The results of the tool calls in `thread`, as `(tool, output, is_error)`.
async fn tool_results(
    client: &Client,
    address: SocketAddr,
    thread: &str,
) -> Vec<(String, String, bool)> {
    get_json(client, address, &format!("/threads/{thread}/items"))
        .await
        .as_array()
        .expect("items")
        .iter()
        .map(|item| &item["data"])
        .filter(|data| data["type"] == "tool_result")
        .map(|data| {
            (
                data["name"].as_str().unwrap_or_default().to_owned(),
                data["output"].as_str().unwrap_or_default().to_owned(),
                data["is_error"].as_bool().unwrap_or_default(),
            )
        })
        .collect()
}

async fn shut_down(client: &Client, address: SocketAddr, child: tokio::process::Child) {
    let shutdown = client
        .post(format!("http://{address}/v1/shutdown"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .expect("request shutdown");
    assert_eq!(shutdown.status(), StatusCode::ACCEPTED);
    let status = timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .expect("server shutdown timeout")
        .expect("wait for server")
        .status;
    assert!(status.success(), "server exited with {status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_server_answers_while_a_server_starts_and_a_turn_waits_for_its_tools() {
    let directory = TestDirectory::new();
    directory.configure(&json!({"slow": directory.slow_server("4")}));
    let address = unused_address().await;
    let child = server_command(&directory, address)
        .spawn()
        .expect("launch Haarness server");
    let client = Client::new();
    let answered = wait_until_answering(&client, address).await;
    assert_eq!(
        mcp_status(&get_json(&client, address, "/mcp").await, "slow").as_deref(),
        Some("connecting"),
        "the server answered only once the MCP server had started ({answered:?})"
    );

    // A turn begun now waits for the server and is offered its tool.
    let thread = new_chat(&client, address).await;
    start_turn(&client, address, &thread, "!tool mcp__slow__ping {}").await;
    wait_until_idle(&client, address, &thread).await;
    assert_eq!(
        tool_results(&client, address, &thread).await,
        [("mcp__slow__ping".to_owned(), "pong".to_owned(), false)]
    );
    wait_for_mcp_status(&client, address, "slow", "connected").await;

    // A turn begun after it started does not wait.
    start_turn(&client, address, &thread, "!tool mcp__slow__ping {}").await;
    let waited = wait_until_idle(&client, address, &thread).await;
    assert!(waited < Duration::from_secs(3), "{waited:?}");

    // A reload that adds a server applies at once and starts it the same way.
    directory.configure(&json!({
        "slow": directory.slow_server("4"),
        "later": directory.slow_server("3"),
    }));
    wait_for_mcp_status(&client, address, "later", "connecting").await;
    start_turn(&client, address, &thread, "!tool mcp__later__ping {}").await;
    wait_until_idle(&client, address, &thread).await;
    let results = tool_results(&client, address, &thread).await;
    assert_eq!(
        results.last(),
        Some(&("mcp__later__ping".to_owned(), "pong".to_owned(), false)),
        "{results:?}"
    );
    // The unchanged server kept its connection through the reload.
    assert_eq!(
        mcp_status(&get_json(&client, address, "/mcp").await, "slow").as_deref(),
        Some("connected")
    );
    shut_down(&client, address, child).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_ends_a_turn_waiting_for_a_server_to_start() {
    let directory = TestDirectory::new();
    let mut stuck = directory.slow_server("20");
    stuck["timeoutMs"] = json!(60_000);
    directory.configure(&json!({"stuck": stuck}));
    let address = unused_address().await;
    let child = server_command(&directory, address)
        .spawn()
        .expect("launch Haarness server");
    let client = Client::new();
    wait_until_answering(&client, address).await;
    let thread = new_chat(&client, address).await;
    let turn = start_turn(&client, address, &thread, "!tool mcp__stuck__ping {}").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_ne!(
        get_json(&client, address, &format!("/threads/{thread}/active")).await,
        Value::Null,
        "the turn did not wait for the server"
    );
    post_json(
        &client,
        address,
        &format!("/turns/{turn}/interrupt"),
        &json!({}),
    )
    .await;
    let stopped = wait_until_idle(&client, address, &thread).await;
    assert!(stopped < Duration::from_secs(3), "Stop took {stopped:?}");
    assert!(tool_results(&client, address, &thread).await.is_empty());
    // Only the turn's wait ended: the server is still starting.
    assert_eq!(
        mcp_status(&get_json(&client, address, "/mcp").await, "stuck").as_deref(),
        Some("connecting")
    );
    shut_down(&client, address, child).await;
}

/// `haarness mcp` commands and the API reach a server that is still starting
/// once it has: a request waits for its start instead of failing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_to_a_server_still_starting_waits_for_it() {
    let directory = TestDirectory::new();
    directory.configure(&json!({"slow": directory.slow_server("3")}));
    let address = unused_address().await;
    let child = server_command(&directory, address)
        .spawn()
        .expect("launch Haarness server");
    let client = Client::new();
    wait_until_answering(&client, address).await;
    let tools = get_json(&client, address, "/mcp/slow/tools").await;
    assert!(
        tools
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "mcp__slow__ping")),
        "{tools}"
    );
    let ping = post_json(&client, address, "/mcp/slow/tools/ping", &json!({})).await;
    assert_eq!(ping["text"], "pong", "{ping}");
    shut_down(&client, address, child).await;
}
