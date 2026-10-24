//! `haarness run --session` on a chat whose turn another window or run
//! started: the run says so and stops, and changes nothing in that chat.

use std::{path::PathBuf, process::Stdio, time::Duration};

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
    api(
        &server,
        reqwest::Method::POST,
        &format!("/permissions/{permission}"),
        Some(json!({"decision":"reject"})),
    )
    .await;
}
