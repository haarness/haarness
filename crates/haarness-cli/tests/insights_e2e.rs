//! Actual CLI -> persistent servers -> SQLite -> stats/API -> HTML report.
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};

struct Fixture {
    root: PathBuf,
    workspaces: Vec<PathBuf>,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("haarness-insights-e2e-{}", uuid::Uuid::now_v7()));
        let workspaces = vec![root.join("api-service"), root.join("web-client")];
        for workspace in &workspaces {
            std::fs::create_dir_all(workspace).unwrap();
        }
        Self { root, workspaces }
    }
    fn command(&self, workspace: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_haarness"))
            .args([
                "--data-dir",
                self.root.to_str().unwrap(),
                "--workspace",
                workspace.to_str().unwrap(),
                "--trust-project",
                "--yolo",
            ])
            .args(args)
            .env("HAARNESS_TEST_PROVIDER", "1")
            .env("RUST_LOG", "warn")
            .output()
            .unwrap()
    }
    fn success(&self, workspace: &Path, args: &[&str]) -> String {
        let output = self.command(workspace, args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for workspace in &self.workspaces {
            self.command(workspace, &["serve", "--stop"]);
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn real_cli_multi_project_history_matches_api_and_html_without_mutating_history() {
    let fixture = Fixture::new();
    for (index, workspace) in fixture.workspaces.iter().enumerate() {
        let prompt = format!(
            "!tool write {{\"path\":\"result-{index}.txt\",\"content\":\"analytics end to end\\n\"}}"
        );
        fixture.success(
            workspace,
            &[
                "run",
                "--model",
                "echo/echo-1",
                "--title",
                "Insights E2E",
                &prompt,
            ],
        );
        assert!(
            workspace.join(format!("result-{index}.txt")).exists(),
            "real tool wrote the file"
        );
    }
    let data = fixture.root.join("data");
    let before = std::fs::read(data.join("workspaces.json")).unwrap();
    let text = fixture.success(&fixture.workspaces[0], &["stats", "--json"]);
    let stats: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(stats["projects"].as_array().unwrap().len(), 2);
    assert_eq!(stats["total"]["sessions"], 2);
    assert_eq!(stats["total"]["turns"], 2);
    assert_eq!(stats["total"]["toolCalls"], 2);
    assert_eq!(stats["total"]["fileChanges"], 2);
    assert!(stats["total"]["inputTokens"].as_u64().unwrap() > 0);
    assert!(stats["warnings"].as_array().unwrap().is_empty());
    assert_eq!(std::fs::read(data.join("workspaces.json")).unwrap(), before);
    let current: Value = serde_json::from_str(&fixture.success(
        &fixture.workspaces[0],
        &["stats", "--current-project", "--json"],
    ))
    .unwrap();
    assert_eq!(current["total"]["turns"], 1);
    let limited: Value = serde_json::from_str(
        &fixture.success(&fixture.workspaces[0], &["stats", "--limit", "1", "--json"]),
    )
    .unwrap();
    assert_eq!(limited["total"]["sessions"], 1);
    assert!(
        !fixture
            .command(&fixture.workspaces[0], &["stats", "--days", "0"])
            .status
            .success()
    );
    let output = fixture.success(
        &fixture.workspaces[0],
        &["insights", "--days", "7", "--no-open", "--no-llm", "--json"],
    );
    let generated: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(generated["report"]["total"], stats["total"]);
    let html = std::fs::read_to_string(generated["path"].as_str().unwrap()).unwrap();
    assert!(html.contains("Your work, across projects."));
    assert!(html.contains("api-service"));
    assert!(html.contains("web-client"));
    assert!(html.contains("result-0.txt"));
    assert!(html.contains("result-1.txt"));
    assert!(
        !html.contains("analytics end to end"),
        "tool file contents stay private"
    );

    let registry: Value = serde_json::from_slice(&before).unwrap();
    let id = registry["workspaces"][0]["id"].as_str().unwrap();
    let discovery: Value = serde_json::from_slice(
        &std::fs::read(
            fixture
                .root
                .join(format!("cache/workspaces/{id}/runtime.json")),
        )
        .unwrap(),
    )
    .unwrap();
    let address = discovery["url"].as_str().unwrap();
    let base = if address.starts_with("http") {
        address.to_owned()
    } else {
        format!("http://{address}")
    };
    let token = discovery["token"].as_str().unwrap();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    assert_eq!(
        client
            .get(format!("{base}/v1/stats"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let response = client
        .get(format!("{base}/v1/stats"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(response["total"], stats["total"]);
    let report = client
        .get(format!("{base}/v1/insights?currentProject=true"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(report["summary"].as_str().unwrap().contains("1 project ·"));
    assert!(report["html"].as_str().unwrap().contains("<!doctype html>"));
    let schema = client
        .get(format!("{base}/openapi.json"))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(schema["paths"]["/v1/stats"]["get"].is_object());
    assert!(schema["paths"]["/v1/insights"]["get"].is_object());
    assert_eq!(
        client
            .get(format!("{base}/v1/stats?days=0"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    if std::env::var_os("HAARNESS_TEST_TMUX").is_some() {
        exercise_tui(&fixture, &base, token);
    }
}

struct Terminal(String);
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &self.0])
            .output();
    }
}

fn wait_for_pane(session: &str, needle: &str) -> String {
    let mut pane = String::new();
    for _ in 0..200 {
        let output = Command::new("tmux")
            .args(["capture-pane", "-p", "-t", session])
            .output()
            .unwrap();
        pane = String::from_utf8_lossy(&output.stdout).into_owned();
        if pane.contains(needle) {
            return pane;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("TUI never showed {needle:?}:\n{pane}");
}

fn exercise_tui(fixture: &Fixture, base: &str, token: &str) {
    #[cfg(target_os = "macos")]
    let _clipboard = ClipboardRestore(
        Command::new("pbpaste")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| o.stdout),
    );
    let session = format!("haarness-insights-{}", uuid::Uuid::now_v7());
    let terminal = Terminal(session.clone());
    // tmux receives an argument vector; no shell interpolation of paths or tokens.
    let output = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-x",
            "140",
            "-y",
            "50",
            "-s",
            &session,
            "env",
            &format!("HAARNESS_DATA_DIR={}", fixture.root.display()),
            &format!("HAARNESS_SERVER_TOKEN={token}"),
            "HAARNESS_TEST_PROVIDER=1",
            "HAARNESS_DEFAULT_PROVIDER=echo",
            "HAARNESS_DEFAULT_MODEL=echo-1",
            env!("CARGO_BIN_EXE_haarness"),
            "--workspace",
            fixture.workspaces[0].to_str().unwrap(),
            "attach",
            base,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "tmux launch: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    wait_for_pane(&session, "h a a r n e s s");
    for (command, expected) in [
        ("/stats", "2 projects"),
        ("/stats --current-project", "1 project ·"),
        ("/insights --days 7", "Report:"),
    ] {
        Command::new("tmux")
            .args(["send-keys", "-t", &session, "-l", command])
            .status()
            .unwrap();
        Command::new("tmux")
            .args(["send-keys", "-t", &session, "Enter"])
            .status()
            .unwrap();
        let pane = wait_for_pane(&session, expected);
        assert!(pane.contains("Cross-project insights"));
        if let Some(directory) = std::env::var_os("HAARNESS_TEST_ARTIFACTS") {
            let directory = PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            let name = command.replace(['/', ' ', '-'], "_");
            std::fs::write(directory.join(format!("{name}.txt")), &pane).unwrap();
        }
        Command::new("tmux")
            .args(["send-keys", "-t", &session, "Escape"])
            .status()
            .unwrap();
    }
    drop(terminal);
}

#[cfg(target_os = "macos")]
struct ClipboardRestore(Option<Vec<u8>>);
#[cfg(target_os = "macos")]
impl Drop for ClipboardRestore {
    fn drop(&mut self) {
        use std::io::Write as _;
        if let Some(bytes) = &self.0 {
            if let Ok(mut child) = Command::new("pbcopy")
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                if let Some(mut input) = child.stdin.take() {
                    let _ = input.write_all(bytes);
                }
                let _ = child.wait();
            }
        }
    }
}
