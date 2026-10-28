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
        "[defaults]\napproval_mode = \"review\"\n[permissions]\nwrite = \"ask\"\n",
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subagent_requests_and_tool_calls_reach_the_client_named_after_the_subagent() {
    let directory = TestDirectory::new();
    let workspace = directory.0.join("workspace");
    let data = directory.0.join("data");
    let home = directory.0.join("home");
    std::fs::create_dir_all(workspace.join(".haarness")).expect("create ACP workspace config");
    std::fs::write(
        workspace.join(".haarness/config.toml"),
        "[defaults]\napproval_mode = \"review\"\n[permissions]\nwrite = \"ask\"\n",
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
            "params":{"protocolVersion":1, "clientCapabilities":{}}
        }),
    )
    .await;
    assert_eq!(receive(&mut lines).await["id"], 1);
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
    let session = receive(&mut lines).await["result"]["sessionId"]
        .as_str()
        .expect("ACP session id")
        .to_owned();

    // The subagent's write asks; before, the request never left Haarness and
    // the turn waited for it forever.
    let write = json!({"path":"child-proof.txt", "content":"CHILD_OK"});
    let task = json!({"description":"Write the proof", "prompt":format!("!tool write {write}")});
    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":3,
            "method":"session/prompt",
            "params":{
                "sessionId":session,
                "prompt":[{"type":"text", "text":format!("!tool task {task}")}]
            }
        }),
    )
    .await;
    let mut messages = Vec::new();
    let response = receive_response(&mut lines, &mut stdin, 3, &mut messages).await;
    assert_eq!(response["result"]["stopReason"], "end_turn", "{messages:?}");

    let request = messages
        .iter()
        .find(|message| message["method"].as_str() == Some("session/request_permission"))
        .unwrap_or_else(|| panic!("the subagent's request did not reach the client: {messages:?}"));
    assert_eq!(request["params"]["sessionId"], session.as_str());
    let child_session = request["params"]["_meta"]["haarness"]["childSessionId"]
        .as_str()
        .expect("the request names the subagent's chat");
    assert_ne!(child_session, session);
    assert_eq!(
        request["params"]["_meta"]["haarness"]["childTitle"],
        "Write the proof"
    );
    assert!(
        request["params"]["toolCall"]["title"]
            .as_str()
            .is_some_and(|title| title.starts_with("Write the proof · ")),
        "{request:?}"
    );
    let call = messages
        .iter()
        .find(|message| {
            message["params"]["update"]["sessionUpdate"].as_str() == Some("tool_call")
                && message["params"]["update"]["_meta"]["haarness"]["childSessionId"].as_str()
                    == Some(child_session)
        })
        .unwrap_or_else(|| {
            panic!("the subagent's tool call did not reach the client: {messages:?}")
        });
    assert_eq!(
        call["params"]["update"]["title"],
        "Write the proof · Edit child-proof.txt"
    );
    assert_eq!(
        request["params"]["toolCall"]["toolCallId"], call["params"]["update"]["toolCallId"],
        "the request points at the subagent's call"
    );
    assert!(
        messages.iter().any(|message| {
            message["params"]["update"]["sessionUpdate"].as_str() == Some("tool_call_update")
                && message["params"]["update"]["toolCallId"]
                    == call["params"]["update"]["toolCallId"]
                && message["params"]["update"]["status"].as_str() == Some("completed")
        }),
        "the subagent's result did not reach the client: {messages:?}"
    );
    assert!(
        !messages.iter().any(|message| {
            message["params"]["update"]["sessionUpdate"].as_str() == Some("agent_message_chunk")
                && message["params"]["update"]["content"]["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("Tool write =>"))
        }),
        "the subagent's own reply stays in its chat: {messages:?}"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("child-proof.txt")).expect("subagent's file"),
        "CHILD_OK"
    );

    drop(stdin);
    let status = timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("Haarness ACP agent did not exit after stdin closed")
        .expect("wait for Haarness ACP agent");
    assert!(status.success(), "Haarness ACP process failed");
    daemon.stop().await;
}

/// The workspace server the ACP agent started, as its discovery file names it.
fn daemon_api(data: &Path) -> (String, String) {
    fn find(directory: &Path) -> Option<PathBuf> {
        for entry in std::fs::read_dir(directory).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = find(&path) {
                    return Some(found);
                }
            } else if path.file_name().is_some_and(|name| name == "runtime.json") {
                return Some(path);
            }
        }
        None
    }
    let path = find(data).expect("the workspace server's runtime.json");
    let runtime: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    (
        runtime["url"].as_str().unwrap().to_owned(),
        runtime["token"].as_str().unwrap().to_owned(),
    )
}

async fn daemon(
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

/// An ACP agent on a workspace whose writes ask, with an open session.
async fn acp_session(
    directory: &TestDirectory,
) -> (
    DaemonGuard,
    tokio::process::Child,
    tokio::process::ChildStdin,
    Lines<BufReader<ChildStdout>>,
    PathBuf,
    PathBuf,
    String,
) {
    let workspace = directory.0.join("workspace");
    let data = directory.0.join("data");
    let home = directory.0.join("home");
    std::fs::create_dir_all(workspace.join(".haarness")).expect("create ACP workspace config");
    std::fs::write(
        workspace.join(".haarness/config.toml"),
        "[defaults]\napproval_mode = \"review\"\n[permissions]\nwrite = \"ask\"\n",
    )
    .expect("configure an approval-requiring ACP tool");
    std::fs::create_dir_all(&home).expect("create isolated ACP home");
    let daemon = DaemonGuard::new(workspace.clone(), data.clone(), home.clone());
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
        json!({"jsonrpc":"2.0", "id":1, "method":"initialize",
               "params":{"protocolVersion":1, "clientCapabilities":{}}}),
    )
    .await;
    assert_eq!(receive(&mut lines).await["id"], 1);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":2, "method":"session/new",
               "params":{"cwd":workspace, "mcpServers":[]}}),
    )
    .await;
    let session = receive(&mut lines).await["result"]["sessionId"]
        .as_str()
        .expect("ACP session id")
        .to_owned();
    (daemon, child, stdin, lines, workspace, data, session)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_settled_elsewhere_is_withdrawn_and_a_late_answer_does_not_fail_the_prompt() {
    let directory = TestDirectory::new();
    let (mut daemon_guard, mut child, mut stdin, mut lines, workspace, data, session) =
        acp_session(&directory).await;
    let write = json!({"path":"settled.txt", "content":"NEVER"});
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":3, "method":"session/prompt",
               "params":{"sessionId":session,
                         "prompt":[{"type":"text", "text":format!("!tool write {write}")}]}}),
    )
    .await;
    let asked = loop {
        let message = receive(&mut lines).await;
        if message["method"].as_str() == Some("session/request_permission") {
            break message;
        }
    };
    // Another app rejects it while the editor still shows it.
    let api = daemon_api(&data);
    let pending = daemon(&api, reqwest::Method::GET, "/permissions", None).await;
    let id = pending[0]["permissionId"].as_str().unwrap().to_owned();
    daemon(
        &api,
        reqwest::Method::POST,
        &format!("/permissions/{id}"),
        Some(json!({"decision":"reject"})),
    )
    .await;
    let mut messages = Vec::new();
    loop {
        let message = receive(&mut lines).await;
        if message["method"].as_str() == Some("$/cancel_request") {
            assert_eq!(message["params"]["requestId"], asked["id"]);
            break;
        }
        messages.push(message);
    }
    // An editor that answers anyway: the answer is moot, not a failure.
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":asked["id"].clone(),
               "result":{"outcome":{"outcome":"selected", "optionId":"once"}}}),
    )
    .await;
    let response = receive_response(&mut lines, &mut stdin, 3, &mut messages).await;
    assert_eq!(
        response["result"]["stopReason"], "end_turn",
        "{response:?} {messages:?}"
    );
    assert!(
        !workspace.join("settled.txt").exists(),
        "the rejection stands"
    );

    drop(stdin);
    let status = timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("Haarness ACP agent did not exit after stdin closed")
        .expect("wait for Haarness ACP agent");
    assert!(status.success(), "Haarness ACP process failed");
    daemon_guard.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_raised_between_prompts_is_asked_on_the_next_one() {
    let directory = TestDirectory::new();
    let (mut daemon_guard, mut child, mut stdin, mut lines, workspace, data, session) =
        acp_session(&directory).await;
    // A background subagent of the session asks while no prompt runs.
    let api = daemon_api(&data);
    let write = json!({"path":"late.txt", "content":"LATE_OK"});
    let task = json!({"description":"Late writer", "prompt":format!("!tool write {write}"), "background":true});
    daemon(
        &api,
        reqwest::Method::POST,
        &format!("/threads/{session}/turns"),
        Some(json!({"requestId":Uuid::new_v4(), "text":format!("!tool task {task}")})),
    )
    .await;
    timeout(Duration::from_secs(20), async {
        loop {
            let pending = daemon(&api, reqwest::Method::GET, "/permissions", None).await;
            if pending.as_array().is_some_and(|pending| {
                pending
                    .iter()
                    .any(|info| info["threadId"] != session.as_str())
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the background subagent asks");

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":3, "method":"session/prompt",
               "params":{"sessionId":session, "prompt":[{"type":"text", "text":"hello"}]}}),
    )
    .await;
    let mut messages = Vec::new();
    let response = receive_response(&mut lines, &mut stdin, 3, &mut messages).await;
    assert_eq!(response["result"]["stopReason"], "end_turn");
    let asked = messages
        .iter()
        .find(|message| message["method"].as_str() == Some("session/request_permission"))
        .unwrap_or_else(|| panic!("the waiting request was not asked: {messages:?}"));
    assert!(
        asked["params"]["toolCall"]["title"]
            .as_str()
            .is_some_and(|title| title.starts_with("Late writer · ")),
        "{asked:?}"
    );
    timeout(Duration::from_secs(20), async {
        while std::fs::read_to_string(workspace.join("late.txt"))
            .ok()
            .as_deref()
            != Some("LATE_OK")
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the approved write runs");

    drop(stdin);
    let status = timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("Haarness ACP agent did not exit after stdin closed")
        .expect("wait for Haarness ACP agent");
    assert!(status.success(), "Haarness ACP process failed");
    daemon_guard.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_auto_review_blocked_shows_its_reason_live_from_a_subagent_and_on_reload() {
    let directory = TestDirectory::new();
    let (mut daemon_guard, mut child, mut stdin, mut lines, workspace, data, session) =
        acp_session(&directory).await;
    let api = daemon_api(&data);
    daemon(
        &api,
        reqwest::Method::PUT,
        &format!("/threads/{session}/approval-mode"),
        Some(json!({"mode":"auto"})),
    )
    .await;
    let blocked = |message: &Value| {
        message["method"].as_str() == Some("session/update")
            && message["params"]["update"]["sessionUpdate"] == "tool_call_update"
            && message["params"]["update"]["content"][0]["content"]["text"]
                == "Stopped by Auto's reviewer: Test verdict: deny."
    };
    // The text written for the model reaches the editor only in the model's
    // own words (the echo provider repeats it, a task returns its child's
    // reply), never as the blocked call's result.
    let model_text = |messages: &[Value]| {
        messages.iter().any(|message| {
            message["params"]["update"]["sessionUpdate"] == "tool_call_update"
                && message["params"]["update"]["status"] == "failed"
                && message.to_string().contains("Do not try to reach")
        })
    };

    // The session's own call, live.
    let write = json!({"path":"own.txt", "content":"NEVER"});
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":3, "method":"session/prompt",
               "params":{"sessionId":session,
                         "prompt":[{"type":"text", "text":format!("!tool write {write} [review:deny]")}]}}),
    )
    .await;
    let mut messages = Vec::new();
    let response = receive_response(&mut lines, &mut stdin, 3, &mut messages).await;
    assert_eq!(response["result"]["stopReason"], "end_turn", "{messages:?}");
    let update = messages
        .iter()
        .find(|message| blocked(message))
        .unwrap_or_else(|| panic!("no blocked update: {messages:?}"));
    assert_eq!(update["params"]["update"]["status"], "failed");
    assert_eq!(
        update["params"]["update"]["rawOutput"]["review"]["status"],
        "blocked"
    );
    assert!(!model_text(&messages), "{messages:?}");
    assert!(
        !messages
            .iter()
            .any(|message| message["method"].as_str() == Some("session/request_permission")),
        "a block asks nobody: {messages:?}"
    );

    // A subagent's call, on the session's updates under the subagent's name.
    let task = json!({
        "description":"Child writer",
        "prompt":format!("!tool write {} [review:deny]", json!({"path":"child.txt", "content":"NEVER"})),
    });
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":4, "method":"session/prompt",
               "params":{"sessionId":session,
                         "prompt":[{"type":"text", "text":format!("!tool task {task}")}]}}),
    )
    .await;
    let mut messages = Vec::new();
    receive_response(&mut lines, &mut stdin, 4, &mut messages).await;
    let update = messages
        .iter()
        .find(|message| blocked(message))
        .unwrap_or_else(|| panic!("no blocked subagent update: {messages:?}"));
    assert!(
        update["params"]["update"]["_meta"].is_object(),
        "the subagent's update names it: {update:?}"
    );
    assert!(!model_text(&messages), "{messages:?}");

    // Loading the session again replays the block the same way.
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":5, "method":"session/load",
               "params":{"sessionId":session, "cwd":workspace, "mcpServers":[]}}),
    )
    .await;
    let mut replayed = Vec::new();
    receive_response(&mut lines, &mut stdin, 5, &mut replayed).await;
    assert!(replayed.iter().any(blocked), "{replayed:?}");
    assert!(!model_text(&replayed), "{replayed:?}");
    assert!(!workspace.join("own.txt").exists());
    assert!(!workspace.join("child.txt").exists());

    drop(stdin);
    let status = timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("Haarness ACP agent did not exit after stdin closed")
        .expect("wait for Haarness ACP agent");
    assert!(status.success(), "Haarness ACP process failed");
    daemon_guard.stop().await;
}

/// A prompt for a session whose turn another window started is refused with
/// the server's sentence as an invalid request, not an internal error; that
/// turn runs on, and the session takes prompts again once it ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_prompt_for_a_session_busy_elsewhere_is_refused_with_the_servers_reason() {
    let directory = TestDirectory::new();
    let (mut daemon_guard, mut child, mut stdin, mut lines, workspace, data, session) =
        acp_session(&directory).await;
    // Another window's turn on the same chat waits for its approval.
    let api = daemon_api(&data);
    let write = json!({"path":"elsewhere.txt", "content":"ELSEWHERE"});
    let held = daemon(
        &api,
        reqwest::Method::POST,
        &format!("/threads/{session}/turns"),
        Some(json!({"requestId":Uuid::new_v4(), "text":format!("!tool write {write}")})),
    )
    .await;
    let permission = timeout(Duration::from_secs(20), async {
        loop {
            let pending = daemon(&api, reqwest::Method::GET, "/permissions", None).await;
            if let Some(id) = pending[0]["permissionId"].as_str() {
                break id.to_owned();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the other window's turn asks");

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":3, "method":"session/prompt",
               "params":{"sessionId":session, "prompt":[{"type":"text", "text":"hello"}]}}),
    )
    .await;
    let mut messages = Vec::new();
    let response = receive_response(&mut lines, &mut stdin, 3, &mut messages).await;
    assert_eq!(response["error"]["code"], -32600, "{response:?}");
    assert_eq!(
        response["error"]["message"],
        "This chat already has a turn running, started in another window or run. It takes a new message once that turn finishes or is stopped.",
        "{response:?}"
    );
    let turn = daemon(
        &api,
        reqwest::Method::GET,
        &format!("/turns/{}", held["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(turn["status"], "running", "the other turn runs on");

    // Once that turn ends, the session takes a prompt again.
    daemon(
        &api,
        reqwest::Method::POST,
        &format!("/permissions/{permission}"),
        Some(json!({"decision":"once"})),
    )
    .await;
    timeout(Duration::from_secs(20), async {
        while daemon(&api, reqwest::Method::GET, "/turns/active", None)
            .await
            .as_array()
            .is_some_and(|active| !active.is_empty())
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the other window's turn ends");
    assert!(workspace.join("elsewhere.txt").exists());
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":4, "method":"session/prompt",
               "params":{"sessionId":session, "prompt":[{"type":"text", "text":"hello again"}]}}),
    )
    .await;
    let response = receive_response(&mut lines, &mut stdin, 4, &mut messages).await;
    assert_eq!(response["result"]["stopReason"], "end_turn", "{response:?}");

    drop(stdin);
    let status = timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("Haarness ACP agent did not exit after stdin closed")
        .expect("wait for Haarness ACP agent");
    assert!(status.success(), "Haarness ACP process failed");
    daemon_guard.stop().await;
}

/// A prompt a `UserPromptSubmit` hook blocks is refused with the hook's
/// reason as an invalid request, not an internal error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_prompt_a_hook_blocks_is_refused_with_the_hooks_reason() {
    let directory = TestDirectory::new();
    let config = directory.0.join("workspace/.haarness");
    std::fs::create_dir_all(&config).expect("create ACP workspace config");
    std::fs::write(
        config.join("haarness.json"),
        json!({"hooks": {"UserPromptSubmit": [{"hooks": [{
            "type": "command",
            "command": r#"printf '{"decision":"block","reason":"prompts are paused here"}'"#,
        }]}]}})
        .to_string(),
    )
    .expect("configure a blocking prompt hook");
    let (mut daemon_guard, mut child, mut stdin, mut lines, _, _, session) =
        acp_session(&directory).await;
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":3, "method":"session/prompt",
               "params":{"sessionId":session, "prompt":[{"type":"text", "text":"hello"}]}}),
    )
    .await;
    let mut messages = Vec::new();
    let response = receive_response(&mut lines, &mut stdin, 3, &mut messages).await;
    assert_eq!(response["error"]["code"], -32600, "{response:?}");
    assert_eq!(
        response["error"]["message"], "prompt blocked by a hook: prompts are paused here",
        "{response:?}"
    );

    drop(stdin);
    let status = timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("Haarness ACP agent did not exit after stdin closed")
        .expect("wait for Haarness ACP agent");
    assert!(status.success(), "Haarness ACP process failed");
    daemon_guard.stop().await;
}

/// `python3` as an absolute path: ACP takes a stdio server's command as one.
fn python3() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|folder| folder.join("python3"))
        .find(|path| path.is_file())
        .expect("python3 on PATH")
}

/// An editor's session with an MCP server slow to start (a first `npx -y`
/// download) is set up at once rather than failing when the editor stops
/// waiting: the server goes on starting and the first prompt waits for it.
/// One that fails once the session is set up is told to the editor at the
/// next prompt, which goes on without it. One that cannot start fails the
/// session; either is left stopped, so an editor retrying does not leave a
/// server behind for every chat.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_with_a_server_slow_to_start_is_set_up_at_once() {
    let directory = TestDirectory::new();
    let (mut daemon_guard, mut child, mut stdin, mut lines, workspace, data, _) =
        acp_session(&directory).await;
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../haarness-mcp/tests/fixtures/recovery.py"
    );
    let server = |name: &str, command: PathBuf, delay: &str| {
        json!({"name": name, "command": command, "args": [fixture],
               "env": [{"name": "INIT_DELAY", "value": delay},
                       {"name": "CALL_LOG", "value": directory.0.join("calls")}]})
    };
    // Ends without answering, 3 s in: after session/new has returned.
    let late =
        json!({"name": "late", "command": "/bin/sh", "args": ["-c", "sleep 3; exit 1"], "env": []});
    let started = std::time::Instant::now();
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":10, "method":"session/new",
               "params":{"cwd":workspace, "mcpServers":[server("slow", python3(), "6"), late]}}),
    )
    .await;
    let mut messages = Vec::new();
    let created = receive_response(&mut lines, &mut stdin, 10, &mut messages).await;
    let waited = started.elapsed();
    // Two servers, at most 2 s each.
    assert!(
        waited < Duration::from_secs(5),
        "session/new took {waited:?}"
    );
    let session = created["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("{created}"))
        .to_owned();
    let tool = format!("mcp__acp-{}-slow__ping", &session[..8]);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":11, "method":"session/prompt",
               "params":{"sessionId":session,
                         "prompt":[{"type":"text", "text":format!("!tool {tool} {{}}")}]}}),
    )
    .await;
    let mut messages = Vec::new();
    let answered = receive_response(&mut lines, &mut stdin, 11, &mut messages).await;
    assert_eq!(answered["result"]["stopReason"], "end_turn", "{answered}");
    assert!(
        messages
            .iter()
            .filter_map(|message| update_text(message, "agent_message_chunk"))
            .any(|text| text == format!("Tool {tool} => pong")),
        "the first prompt did not get the server's tool: {messages:?}"
    );
    assert!(
        messages
            .iter()
            .filter_map(|message| update_text(message, "agent_message_chunk"))
            .any(|text| text.starts_with("MCP server late did not start (")),
        "the editor was not told a server failed: {messages:?}"
    );

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":12, "method":"session/new",
               "params":{"cwd":workspace,
                         "mcpServers":[server("broken", PathBuf::from("/usr/bin/false"), "0")]}}),
    )
    .await;
    let mut messages = Vec::new();
    let refused = receive_response(&mut lines, &mut stdin, 12, &mut messages).await;
    assert!(refused.get("error").is_some(), "{refused}");
    let listed = base_command(&workspace, &data, &directory.0.join("home"))
        .args(["mcp", "list"])
        .output()
        .await
        .expect("list MCP servers");
    let listed = String::from_utf8_lossy(&listed.stdout);
    for name in ["-broken\t", "-late\t"] {
        let line = listed
            .lines()
            .find(|line| line.contains(name))
            .unwrap_or_else(|| panic!("{listed}"));
        assert!(line.contains("\tdisconnected"), "{line}");
    }

    drop(stdin);
    let status = timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("Haarness ACP agent did not exit after stdin closed")
        .expect("wait for Haarness ACP agent");
    assert!(status.success(), "Haarness ACP process failed");
    daemon_guard.stop().await;
}
