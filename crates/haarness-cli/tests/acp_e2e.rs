use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, Lines},
    process::{ChildStdout, Command},
    time::timeout,
};
use uuid::Uuid;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("haarness-acp-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&path).expect("create isolated ACP test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("preserving failed ACP fixture at {}", self.0.display());
            return;
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct DaemonGuard {
    workspace: PathBuf,
    data: PathBuf,
    home: PathBuf,
    stopped: bool,
}

impl DaemonGuard {
    fn new(workspace: PathBuf, data: PathBuf, home: PathBuf) -> Self {
        Self {
            workspace,
            data,
            home,
            stopped: false,
        }
    }

    async fn stop(&mut self) {
        let result = timeout(
            Duration::from_secs(20),
            base_command(&self.workspace, &self.data, &self.home)
                .arg("serve")
                .arg("--stop")
                .output(),
        )
        .await
        .expect("timed out stopping ACP test daemon")
        .expect("launch Haarness daemon stop command");
        assert!(
            result.status.success(),
            "failed to stop ACP test daemon: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        self.stopped = true;
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = std::process::Command::new(env!("CARGO_BIN_EXE_haarness"))
                .arg("--workspace")
                .arg(&self.workspace)
                .arg("--data-dir")
                .arg(&self.data)
                .arg("serve")
                .arg("--stop")
                .env("HOME", &self.home)
                .env("HAARNESS_TEST_PROVIDER", "1")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn base_command(workspace: &Path, data: &Path, home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_haarness"));
    command
        .arg("--workspace")
        .arg(workspace)
        .arg("--data-dir")
        .arg(data)
        .arg("--trust-project")
        .env("HOME", home)
        .env("HAARNESS_TEST_PROVIDER", "1")
        .env("RUST_LOG", "warn");
    command
}

async fn send(stdin: &mut tokio::process::ChildStdin, message: Value) {
    let mut bytes = serde_json::to_vec(&message).expect("serialize ACP message");
    bytes.push(b'\n');
    stdin.write_all(&bytes).await.expect("write ACP message");
    stdin.flush().await.expect("flush ACP message");
}

async fn receive(lines: &mut Lines<BufReader<ChildStdout>>) -> Value {
    let line = timeout(Duration::from_secs(30), lines.next_line())
        .await
        .expect("timed out waiting for ACP message")
        .expect("read ACP stdout")
        .expect("ACP process closed stdout unexpectedly");
    serde_json::from_str(&line).unwrap_or_else(|error| panic!("invalid ACP JSON {line:?}: {error}"))
}

fn update_text<'a>(message: &'a Value, kind: &str) -> Option<&'a str> {
    (message["method"].as_str() == Some("session/update")
        && message["params"]["update"]["sessionUpdate"].as_str() == Some(kind))
    .then(|| message["params"]["update"]["content"]["text"].as_str())
    .flatten()
}

async fn receive_response(
    lines: &mut Lines<BufReader<ChildStdout>>,
    stdin: &mut tokio::process::ChildStdin,
    expected_id: u64,
    messages: &mut Vec<Value>,
) -> Value {
    loop {
        let message = receive(lines).await;
        if message["method"].as_str() == Some("session/request_permission") {
            assert_eq!(
                message["params"]["options"][0]["optionId"], "once",
                "Haarness should offer a one-shot ACP approval"
            );
            send(
                stdin,
                json!({
                    "jsonrpc":"2.0",
                    "id":message["id"].clone(),
                    "result":{"outcome":{"outcome":"selected", "optionId":"once"}}
                }),
            )
            .await;
            messages.push(message);
            continue;
        }
        if message["id"].as_u64() == Some(expected_id) {
            return message;
        }
        messages.push(message);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serves_editor_session_tools_permissions_and_durable_reload_over_acp() {
    let directory = TestDirectory::new();
    let workspace = directory.0.join("workspace");
    let data = directory.0.join("data");
    let home = directory.0.join("home");
    std::fs::create_dir_all(workspace.join(".haarness")).expect("create ACP workspace config");
    std::fs::write(
        workspace.join(".haarness/config.toml"),
        "[permissions]\nwrite = \"ask\"\n",
    )
    .expect("configure an approval-requiring ACP tool");
    std::fs::create_dir_all(&home).expect("create isolated ACP home");
    let mut daemon = DaemonGuard::new(workspace.clone(), data.clone(), home.clone());

    let mut command = base_command(&workspace, &data, &home);
    command
        .arg("acp")
        .arg("--provider")
        .arg("echo")
        .arg("--model")
        .arg("echo-1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let mut child = command.spawn().expect("launch Haarness ACP agent");
    let mut stdin = child.stdin.take().expect("ACP stdin");
    let mut lines = BufReader::new(child.stdout.take().expect("ACP stdout")).lines();

    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"initialize",
            "params":{
                "protocolVersion":1,
                "clientCapabilities":{},
                "clientInfo":{"name":"haarness-acp-e2e", "version":"1"}
            }
        }),
    )
    .await;
    let initialized = receive(&mut lines).await;
    assert_eq!(initialized["id"], 1);
    assert_eq!(initialized["result"]["protocolVersion"], 1);
    assert_eq!(
        initialized["result"]["agentCapabilities"]["loadSession"],
        true
    );
    assert_eq!(
        initialized["result"]["agentCapabilities"]["promptCapabilities"]["image"],
        true
    );

    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":2,
            "method":"session/new",
            "params":{"cwd":workspace, "mcpServers":[]}
        }),
    )
    .await;
    let created = receive(&mut lines).await;
    let session = created["result"]["sessionId"]
        .as_str()
        .expect("ACP session id")
        .to_owned();
    Uuid::parse_str(&session).expect("Haarness thread UUID is the ACP session id");

    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":3,
            "method":"session/prompt",
            "params":{
                "sessionId":session,
                "prompt":[{"type":"text", "text":"hello ACP"}]
            }
        }),
    )
    .await;
    let mut first_messages = Vec::new();
    let first = receive_response(&mut lines, &mut stdin, 3, &mut first_messages).await;
    assert_eq!(first["result"]["stopReason"], "end_turn");
    assert!(
        first_messages
            .iter()
            .filter_map(|message| update_text(message, "agent_message_chunk"))
            .any(|text| text == "Echo: hello ACP"),
        "ACP did not stream the echo response: {first_messages:?}"
    );

    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":4,
            "method":"session/prompt",
            "params":{
                "sessionId":session,
                "prompt":[{
                    "type":"text",
                    "text":"!tool write {\"path\":\"acp-proof.txt\",\"content\":\"ACP_TOOL_OK\"}"
                }]
            }
        }),
    )
    .await;
    let mut tool_messages = Vec::new();
    let tool_response = receive_response(&mut lines, &mut stdin, 4, &mut tool_messages).await;
    assert_eq!(tool_response["result"]["stopReason"], "end_turn");
    assert!(
        tool_messages.iter().any(|message| {
            message["params"]["update"]["sessionUpdate"].as_str() == Some("tool_call")
                && message["params"]["update"]["toolCallId"].as_str() == Some("echo-call-1")
        }),
        "ACP did not publish tool start: {tool_messages:?}"
    );
    assert!(
        tool_messages
            .iter()
            .any(|message| { message["method"].as_str() == Some("session/request_permission") }),
        "ACP did not bridge the Haarness permission request: {tool_messages:?}"
    );
    assert!(
        tool_messages.iter().any(|message| {
            message["params"]["update"]["sessionUpdate"].as_str() == Some("tool_call_update")
                && message["params"]["update"]["status"].as_str() == Some("completed")
        }),
        "ACP did not publish the completed tool result: {tool_messages:?}"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("acp-proof.txt")).expect("ACP-created file"),
        "ACP_TOOL_OK",
        "the approved ACP tool call did not mutate the workspace"
    );
    assert!(
        tool_messages
            .iter()
            .filter_map(|message| update_text(message, "agent_message_chunk"))
            .any(|text| text.starts_with("Tool write =>")),
        "ACP did not stream the post-tool assistant response: {tool_messages:?}"
    );

    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":5,
            "method":"session/load",
            "params":{"sessionId":session, "cwd":workspace, "mcpServers":[]}
        }),
    )
    .await;
    let mut replayed = Vec::new();
    let loaded = receive_response(&mut lines, &mut stdin, 5, &mut replayed).await;
    assert_eq!(loaded["result"], json!({}));
    assert!(
        replayed
            .iter()
            .filter_map(|message| update_text(message, "user_message_chunk"))
            .any(|text| text == "hello ACP"),
        "ACP load did not replay the user transcript: {replayed:?}"
    );
    assert!(
        replayed
            .iter()
            .filter_map(|message| update_text(message, "agent_message_chunk"))
            .any(|text| text == "Echo: hello ACP"),
        "ACP load did not replay the assistant transcript: {replayed:?}"
    );

    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":6,
            "method":"session/prompt",
            "params":{
                "sessionId":session,
                "prompt":[{"type":"text", "text":"::mdslow"}]
            }
        }),
    )
    .await;
    let first_chunk = receive(&mut lines).await;
    assert_eq!(
        update_text(&first_chunk, "agent_message_chunk"),
        Some("**Live markdown**"),
        "the cancellable ACP turn did not begin streaming"
    );
    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "method":"session/cancel",
            "params":{"sessionId":session}
        }),
    )
    .await;
    let mut cancelled_messages = vec![first_chunk];
    let cancelled = receive_response(&mut lines, &mut stdin, 6, &mut cancelled_messages).await;
    assert_eq!(
        cancelled["result"]["stopReason"], "cancelled",
        "ACP cancellation did not interrupt the active Haarness turn: {cancelled_messages:?}"
    );

    drop(stdin);
    let status = timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("Haarness ACP agent did not exit after stdin closed")
        .expect("wait for Haarness ACP agent");
    assert!(status.success(), "Haarness ACP process failed");
    daemon.stop().await;
}
