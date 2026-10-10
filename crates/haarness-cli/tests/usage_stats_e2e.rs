//! Usage statistics end to end: real history from `haarness run`, the report
//! the server's management API previews (as the TUI and Desktop show it), and
//! the report a mock Haarness Cloud receives.
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde_json::{Value, json};
use tokio::{net::TcpListener, process::Command, time::timeout};
use uuid::Uuid;

#[derive(Clone, Default)]
struct Cloud {
    reports: Arc<Mutex<Vec<(HeaderMap, Value)>>>,
}

async fn ingest(
    State(cloud): State<Cloud>,
    headers: HeaderMap,
    Json(report): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let days = report["days"].as_array().map_or(0, Vec::len);
    cloud
        .reports
        .lock()
        .expect("reports lock")
        .push((headers, report));
    (
        StatusCode::ACCEPTED,
        Json(json!({"accepted": true, "days": days, "linked": false})),
    )
}

struct TestDirectory(PathBuf);

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn haarness(directory: &Path, workspace: &Path, cloud: SocketAddr) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_haarness"));
    command
        .arg("--workspace")
        .arg(workspace)
        .arg("--data-dir")
        .arg(directory.join("data"))
        .current_dir(workspace)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", directory.join("home"))
        .env("XDG_CONFIG_HOME", directory.join("xdg-config"))
        .env("XDG_DATA_HOME", directory.join("xdg-data"))
        .env("XDG_CACHE_HOME", directory.join("xdg-cache"))
        .env("HAARNESS_HOME", directory.join("haarness-home"))
        .env("HAARNESS_TEST_PROVIDER", "1")
        .env("HAARNESS_CLOUD_API_URL", format!("http://{cloud}"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

async fn output(mut command: Command) -> (bool, String) {
    let output = timeout(Duration::from_secs(60), command.output())
        .await
        .expect("haarness finished within a minute")
        .expect("run haarness");
    (
        output.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_statistics_are_sent_from_real_history_and_never_when_off() {
    let cloud = Cloud::default();
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock cloud");
    let address = listener.local_addr().expect("mock cloud address");
    let router = Router::new()
        .route("/api/v1/telemetry/usage", post(ingest))
        .with_state(cloud.clone());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let directory = TestDirectory(
        std::env::temp_dir().join(format!("haarness-usage-e2e-{}", Uuid::now_v7())),
    );
    let workspace = directory.0.join("SENTINEL_PROJECT");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::write(workspace.join("README.md"), "# readme\n").expect("write readme");
    let prompt = r#"!multi [{"name":"read","arguments":{"path":"README.md"}},{"name":"read","arguments":{"path":"SENTINEL_MISSING.txt"}}]"#;
    let (ok, text) = output({
        let mut command = haarness(&directory.0, &workspace, address);
        command.args(["--yolo", "run", "--provider", "echo", "--model", "echo-1", prompt]);
        command
    })
    .await;
    assert!(ok, "run failed: {text}");

    // The TUI and Desktop read status and the preview from the running server.
    let runtime: Value = serde_json::from_slice(
        &std::fs::read(
            walk(&directory.0.join("data/cache"))
                .into_iter()
                .find(|path| path.ends_with("runtime.json"))
                .expect("the run left its server's discovery file"),
        )
        .expect("read runtime.json"),
    )
    .expect("parse runtime.json");
    let view: Value = reqwest::Client::new()
        .post(format!("{}/v1/manage/usage-statistics", runtime["url"].as_str().unwrap()))
        .bearer_auth(runtime["token"].as_str().unwrap())
        .json(&json!({"action": "preview"}))
        .send()
        .await
        .expect("ask for the preview")
        .json()
        .await
        .expect("view");
    assert_eq!(view["data"]["setting"]["enabled"], true, "{view}");
    assert_eq!(view["data"]["report"]["days"][0]["turns"], 1, "{view}");
    assert!(cloud.reports.lock().unwrap().is_empty(), "a preview sends nothing");

    let (ok, text) = output({
        let mut command = haarness(&directory.0, &workspace, address);
        command.args(["telemetry", "send"]);
        command
    })
    .await;
    assert!(ok && text.contains("Sent 1 day"), "{text}");
    let (headers, report) = cloud.reports.lock().unwrap()[0].clone();
    assert!(headers.get("authorization").is_none(), "not signed in");
    assert_eq!(report["schemaVersion"], 1);
    assert!(Uuid::parse_str(report["installId"].as_str().unwrap()).is_ok());
    let day = &report["days"][0];
    assert_eq!(day["turns"], 1);
    assert!(day["inputTokens"].as_u64().unwrap() > 0);
    assert_eq!(day["toolCalls"], 2);
    assert_eq!(day["toolErrors"], 1);
    // The echo test provider is not one Haarness ships: no names leave.
    assert_eq!(day["models"][0]["provider"], "custom");
    assert_eq!(day["models"][0]["model"], "custom");
    assert_eq!(day["tools"][0]["name"], "read");
    let sent = report.to_string();
    for private in ["SENTINEL_PROJECT", "SENTINEL_MISSING", "README", "!multi", "echo-1"] {
        assert!(!sent.contains(private), "{private} was sent: {sent}");
    }

    // `preview` shows the same report.
    let (ok, text) = output({
        let mut command = haarness(&directory.0, &workspace, address);
        command.args(["telemetry", "preview"]);
        command
    })
    .await;
    let preview: Value = serde_json::from_str(&text).expect("preview is JSON");
    assert!(ok);
    assert_eq!(preview["installId"], report["installId"]);
    assert_eq!(preview["days"], report["days"]);

    // Off: by environment, and by the setting.
    for (variable, value) in [("HAARNESS_TELEMETRY", "0"), ("DO_NOT_TRACK", "1")] {
        let (_, text) = output({
            let mut command = haarness(&directory.0, &workspace, address);
            command.env(variable, value).args(["telemetry", "send"]);
            command
        })
        .await;
        assert!(text.contains("off; nothing was sent"), "{variable}: {text}");
    }
    let (ok, text) = output({
        let mut command = haarness(&directory.0, &workspace, address);
        command.args(["telemetry", "off"]);
        command
    })
    .await;
    assert!(ok && text.contains("Usage statistics are off."), "{text}");
    let (_, text) = output({
        let mut command = haarness(&directory.0, &workspace, address);
        command.args(["telemetry", "send"]);
        command
    })
    .await;
    assert!(text.contains("off; nothing was sent"), "{text}");
    assert_eq!(cloud.reports.lock().unwrap().len(), 1, "only the one report");

    let _ = output({
        let mut command = haarness(&directory.0, &workspace, address);
        command.args(["serve", "--stop"]);
        command
    })
    .await;
    server.abort();
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}
