use std::{
    collections::HashSet,
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
    // The subagent's call names its file, as the session was opened, and its
    // result shows what it wrote.
    let proof = workspace
        .join("child-proof.txt")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        call["params"]["update"]["locations"],
        json!([{"path": proof}])
    );
    let result = call_update(
        &messages,
        "tool_call_update",
        &call["params"]["update"]["toolCallId"],
    );
    assert_eq!(result["locations"], call["params"]["update"]["locations"]);
    assert_eq!(
        diffs(result),
        [json!({"type":"diff", "path": proof, "newText":"CHILD_OK"})]
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
    running_daemon_api(data).expect("the workspace server's runtime.json")
}

/// [`daemon_api`], or `None` while no workspace server has written one (it
/// is starting again).
fn running_daemon_api(data: &Path) -> Option<(String, String)> {
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
    let runtime: Value = serde_json::from_slice(&std::fs::read(find(data)?).ok()?).ok()?;
    Some((
        runtime["url"].as_str()?.to_owned(),
        runtime["token"].as_str()?.to_owned(),
    ))
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

/// The names the project's server knows the editor's server `shown` of
/// `session` by, one for each `haarness acp` process that set it up: each
/// puts a token of its own in the name.
async fn editor_servers(api: &(String, String), session: &str, shown: &str) -> Vec<String> {
    let start = format!("acp-{}-", &session[..8]);
    let end = format!("-{shown}");
    daemon(api, reqwest::Method::GET, "/mcp", None)
        .await
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|server| server["name"].as_str())
        .filter(|name| name.starts_with(&start) && name.ends_with(&end))
        .map(str::to_owned)
        .collect()
}

/// The one name the project's server knows `shown` of `session` by.
async fn editor_server(api: &(String, String), session: &str, shown: &str) -> String {
    let names = editor_servers(api, session, shown).await;
    assert_eq!(names.len(), 1, "{names:?}");
    names[0].clone()
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
    let started = daemon(
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
    // The turn that started it can still be finishing: a prompt sent before
    // it ends is refused as one for a busy chat.
    let turn = format!("/turns/{}", started["id"].as_str().unwrap());
    timeout(Duration::from_secs(20), async {
        while ["queued", "running"].contains(
            &daemon(&api, reqwest::Method::GET, &turn, None).await["status"]
                .as_str()
                .unwrap_or_default(),
        ) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the turn that started the subagent ends");

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":3, "method":"session/prompt",
               "params":{"sessionId":session, "prompt":[{"type":"text", "text":"hello"}]}}),
    )
    .await;
    let mut messages = Vec::new();
    let response = receive_response(&mut lines, &mut stdin, 3, &mut messages).await;
    assert_eq!(response["result"]["stopReason"], "end_turn", "{response:?}");
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

/// An editor that sends an image only by its address (as a web page's
/// image) gets an answer: the model is told where the image is, and Haarness
/// fetches nothing. One with neither data nor an address is refused in words.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_image_sent_only_by_its_address_reaches_the_model_as_a_link() {
    let directory = TestDirectory::new();
    let (mut daemon_guard, mut child, mut stdin, mut lines, _, _, session) =
        acp_session(&directory).await;
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":3, "method":"session/prompt",
               "params":{"sessionId":session, "prompt":[
                   {"type":"text", "text":"describe it"},
                   {"type":"image", "data":"", "mimeType":"image/png",
                    "uri":"https://example.com/charts/q3.png"}]}}),
    )
    .await;
    let mut messages = Vec::new();
    let response = receive_response(&mut lines, &mut stdin, 3, &mut messages).await;
    assert_eq!(response["result"]["stopReason"], "end_turn", "{response:?}");
    let reply: String = messages
        .iter()
        .filter_map(|message| update_text(message, "agent_message_chunk"))
        .collect();
    assert!(
        reply
            .contains(r#"<resource-link name="q3.png" uri="https://example.com/charts/q3.png" />"#),
        "{reply:?}"
    );

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":4, "method":"session/prompt",
               "params":{"sessionId":session, "prompt":[
                   {"type":"image", "data":"", "mimeType":"image/png"}]}}),
    )
    .await;
    let response = receive_response(&mut lines, &mut stdin, 4, &mut messages).await;
    assert_eq!(response["error"]["code"], -32602, "{response:?}");
    assert!(
        response["error"]
            .to_string()
            .contains("an ACP image needs its data or a uri"),
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

/// `sh` as an absolute path (Git's `sh.exe` on Windows): ACP takes a stdio
/// server's command as one.
fn sh() -> PathBuf {
    let name = if cfg!(windows) { "sh.exe" } else { "sh" };
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|folder| folder.join(name))
        .find(|path| path.is_file())
        .expect("sh on PATH")
}

/// `python3` (`python.exe` on Windows, which has no `python3`) as an
/// absolute path: ACP takes a stdio server's command as one.
fn python3() -> PathBuf {
    let name = if cfg!(windows) {
        "python.exe"
    } else {
        "python3"
    };
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|folder| folder.join(name))
        .find(|path| path.is_file())
        .expect("python3 on PATH")
}

/// An editor's session with an MCP server slow to start (a first `npx -y`
/// download) is set up at once rather than failing when the editor stops
/// waiting: the server goes on starting and the first prompt waits for it.
/// One that fails once the session is set up is told to the editor at the
/// next prompt, which goes on without it. One that cannot start fails the
/// session; either is let go, so an editor retrying does not leave a server
/// behind for every chat.
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
        json!({"name": "late", "command": sh(), "args": ["-c", "sleep 3; exit 1"], "env": []});
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
    let tool = format!(
        "mcp__{}__ping",
        editor_server(&daemon_api(&data), &session, "slow").await
    );
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

    // A program that ends at once without answering (Git's `sh` on Windows,
    // reading the Python fixture as shell).
    let broken = if cfg!(windows) {
        sh()
    } else {
        PathBuf::from("/usr/bin/false")
    };
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":12, "method":"session/new",
               "params":{"cwd":workspace,
                         "mcpServers":[server("broken", broken, "0")]}}),
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
        assert!(!listed.contains(name), "{listed}");
    }

    drop(stdin);
    let status = timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("Haarness ACP agent did not exit after stdin closed")
        .expect("wait for Haarness ACP agent");
    assert!(status.success(), "Haarness ACP process failed");
    daemon_guard.stop().await;
}

/// Prompts sent back to back while the session's MCP server starts run in
/// the order they were sent, the second sharing the first's wait; a Cancel
/// during the wait ends that prompt, and the next one waits again and gets
/// the server's tool.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prompts_sent_while_a_server_starts_keep_their_order() {
    let directory = TestDirectory::new();
    let (mut daemon_guard, mut child, mut stdin, mut lines, workspace, _, _) =
        acp_session(&directory).await;
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../haarness-mcp/tests/fixtures/recovery.py"
    );
    let delay = directory.0.join("delay");
    std::fs::write(&delay, "4").expect("write the server's delay");
    let slow = json!({"name": "slow", "command": python3(), "args": [fixture],
                      "env": [{"name": "INIT_DELAY_FILE", "value": delay},
                              {"name": "CALL_LOG", "value": directory.0.join("calls")}]});
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":20, "method":"session/new",
               "params":{"cwd":workspace, "mcpServers":[slow]}}),
    )
    .await;
    let mut messages = Vec::new();
    let created = receive_response(&mut lines, &mut stdin, 20, &mut messages).await;
    let session = created["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("{created}"))
        .to_owned();
    // An empty prompt among them fails at once rather than waiting its turn.
    for (id, text) in [(21, "first"), (22, "second"), (25, "  ")] {
        send(
            &mut stdin,
            json!({"jsonrpc":"2.0", "id":id, "method":"session/prompt",
                   "params":{"sessionId":session, "prompt":[{"type":"text", "text":text}]}}),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let mut answered = Vec::new();
    let mut texts = Vec::new();
    while answered.len() < 3 {
        let message = receive(&mut lines).await;
        if let Some(id) = message["id"]
            .as_u64()
            .filter(|_| message.get("method").is_none())
        {
            if id == 25 {
                assert!(message.get("error").is_some(), "{message}");
            } else {
                assert_eq!(message["result"]["stopReason"], "end_turn", "{message}");
            }
            answered.push(id);
        } else if let Some(text) = update_text(&message, "agent_message_chunk") {
            texts.push(text.to_owned());
        }
    }
    assert_eq!(
        answered,
        [25, 21, 22],
        "the prompts were answered out of order"
    );
    assert_eq!(texts, ["Echo: first", "Echo: second"], "{texts:?}");

    // A Cancel while a prompt waits for the server ends that prompt. (Another
    // name: chats made within a minute share the first 8 characters of their
    // id, which name an editor's server.)
    std::fs::write(&delay, "30").expect("write the server's delay");
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":23, "method":"session/new",
               "params":{"cwd":workspace, "mcpServers":[json!({"name": "stalled", "command": python3(), "args": [fixture],
                      "env": [{"name": "INIT_DELAY_FILE", "value": delay},
                              {"name": "CALL_LOG", "value": directory.0.join("calls")}]})]}}),
    )
    .await;
    let created = receive_response(&mut lines, &mut stdin, 23, &mut messages).await;
    let session = created["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("{created}"))
        .to_owned();
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":24, "method":"session/prompt",
               "params":{"sessionId":session, "prompt":[{"type":"text", "text":"waiting"}]}}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "method":"session/cancel", "params":{"sessionId":session}}),
    )
    .await;
    let started = std::time::Instant::now();
    let cancelled = receive_response(&mut lines, &mut stdin, 24, &mut messages).await;
    assert_eq!(
        cancelled["result"]["stopReason"], "cancelled",
        "{cancelled}"
    );
    assert!(started.elapsed() < Duration::from_secs(3));

    drop(stdin);
    let status = timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("Haarness ACP agent did not exit after stdin closed")
        .expect("wait for Haarness ACP agent");
    assert!(status.success(), "Haarness ACP process failed");
    daemon_guard.stop().await;
}

/// An editor's MCP servers last as long as its session: a reload of the
/// project's configuration (a skill added) keeps them, and once the
/// project's server has stopped (an update replaces it, it is stopped), the
/// next prompt reaches the one that serves the project now and sets them up
/// there again. One the person disconnected stays disconnected through the
/// reload; a new server starts it again, as it starts a configured one. One
/// turned off through the API stays gone, a restart after included, and the
/// editor quitting lets the rest go.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sessions_mcp_servers_outlast_a_reload_and_a_restart_of_the_server() {
    let directory = TestDirectory::new();
    let (mut daemon_guard, mut child, mut stdin, mut lines, workspace, data, _) =
        acp_session(&directory).await;
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../haarness-mcp/tests/fixtures/recovery.py"
    );
    let launch_log = directory.0.join("launches");
    let server = |name: &str| {
        json!({"name": name, "command": python3(), "args": [fixture],
               "env": [{"name": "LAUNCH_LOG", "value": launch_log},
                       {"name": "CALL_LOG", "value": directory.0.join("calls")}]})
    };
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":30, "method":"session/new",
               "params":{"cwd":workspace, "mcpServers":[server("editor"), server("spare")]}}),
    )
    .await;
    let mut messages = Vec::new();
    let created = receive_response(&mut lines, &mut stdin, 30, &mut messages).await;
    let session = created["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("{created}"))
        .to_owned();
    let api = daemon_api(&data);
    let tool = format!(
        "mcp__{}__ping",
        editor_server(&api, &session, "editor").await
    );
    let spare = editor_server(&api, &session, "spare").await;
    let launched = || std::fs::read_to_string(&launch_log).map_or(0, |text| text.lines().count());
    assert_eq!(launched(), 2);
    let mut prompt = async |id: u64| {
        send(
            &mut stdin,
            json!({"jsonrpc":"2.0", "id":id, "method":"session/prompt",
                   "params":{"sessionId":session,
                             "prompt":[{"type":"text", "text":format!("!tool {tool} {{}}")}]}}),
        )
        .await;
        let mut messages = Vec::new();
        let answered = receive_response(&mut lines, &mut stdin, id, &mut messages).await;
        assert_eq!(answered["result"]["stopReason"], "end_turn", "{answered}");
        assert!(
            messages
                .iter()
                .filter_map(|message| update_text(message, "agent_message_chunk"))
                .any(|text| text == format!("Tool {tool} => pong")),
            "prompt {id} did not get the editor's tool: {messages:?}"
        );
    };
    prompt(31).await;

    let api = daemon_api(&data);
    daemon(
        &api,
        reqwest::Method::POST,
        &format!("/mcp/{spare}/disconnect"),
        Some(json!({})),
    )
    .await;
    let skill = workspace.join(".haarness/skills/late/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).expect("create skill folder");
    std::fs::write(
        &skill,
        "---\nname: late\ndescription: Says late.\n---\n\nSay late.\n",
    )
    .expect("write skill");
    timeout(Duration::from_secs(20), async {
        while !daemon(&api, reqwest::Method::GET, "/skills", None)
            .await
            .as_array()
            .is_some_and(|skills| skills.iter().any(|skill| skill["name"] == "late"))
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the skill never reloaded the configuration");
    prompt(32).await;
    assert_eq!(launched(), 2, "a server was started again after the reload");
    let listed = daemon(&api, reqwest::Method::GET, "/mcp", None).await;
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .any(|server| server["name"] == spare.as_str() && server["status"] == "disconnected"),
        "{listed}"
    );

    // The server stops; the next prompt starts the project's server again
    // and sets the session's servers up there. The guard stops that one,
    // should the test fail before it does.
    daemon_guard.stop().await;
    daemon_guard.stopped = false;
    prompt(33).await;
    assert_eq!(launched(), 4);
    let restarted = daemon_api(&data);
    assert_ne!(restarted, api, "the prompt reached the stopped server");
    let api = restarted;

    // Turned off through the API, it is gone at once, and neither the next
    // prompt nor a later restart brings it back.
    daemon(
        &api,
        reqwest::Method::POST,
        &format!("/mcp/{spare}"),
        Some(json!({"transport": "stdio", "command": python3(), "args": [fixture], "enabled": false})),
    )
    .await;
    prompt(34).await;
    daemon_guard.stop().await;
    daemon_guard.stopped = false;
    prompt(35).await;
    assert_eq!(launched(), 5, "a server turned off was set up again");
    let api = daemon_api(&data);
    let named = |listed: &Value, name: &str| {
        listed.as_array().unwrap().iter().any(|server| {
            server["name"]
                .as_str()
                .is_some_and(|named| named.starts_with(name))
        })
    };
    let listed = daemon(&api, reqwest::Method::GET, "/mcp", None).await;
    assert!(!named(&listed, &spare), "{listed}");

    // The editor quits: the session's servers go with it.
    drop(stdin);
    let status = timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("Haarness ACP agent did not exit after stdin closed")
        .expect("wait for Haarness ACP agent");
    assert!(status.success(), "Haarness ACP process failed");
    let listed = daemon(&api, reqwest::Method::GET, "/mcp", None).await;
    assert!(!named(&listed, "acp-"), "{listed}");
    daemon_guard.stop().await;
}

/// Two sessions made within a minute share an editor's server of the same
/// name: after a restart, their prompts sent together set it up once and
/// both get its tools. Closing one session lets go only the servers no
/// other open session has.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sessions_sharing_an_editors_server_set_it_up_once_and_a_close_lets_go_only_their_own() {
    let directory = TestDirectory::new();
    let (mut daemon_guard, mut child, mut stdin, mut lines, workspace, data, _) =
        acp_session(&directory).await;
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../haarness-mcp/tests/fixtures/recovery.py"
    );
    let server = |name: &str| {
        json!({"name": name, "command": python3(), "args": [fixture],
               "env": [{"name": "LAUNCH_LOG", "value": directory.0.join(name)},
                       {"name": "CALL_LOG", "value": directory.0.join("calls")}]})
    };
    let launched = |name: &str| {
        std::fs::read_to_string(directory.0.join(name)).map_or(0, |text| text.lines().count())
    };
    // A chat id starts with its time in units of 65.536 s: both sessions are
    // made well inside one.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let left = 65_536 - (now % 65_536);
    if left < 10_000 {
        tokio::time::sleep(Duration::from_millis(u64::try_from(left).unwrap() + 100)).await;
    }
    let mut sessions = Vec::new();
    for (id, servers) in [
        (40, json!([server("shared")])),
        (41, json!([server("shared"), server("own")])),
    ] {
        send(
            &mut stdin,
            json!({"jsonrpc":"2.0", "id":id, "method":"session/new",
                   "params":{"cwd":workspace, "mcpServers":servers}}),
        )
        .await;
        let mut messages = Vec::new();
        let created = receive_response(&mut lines, &mut stdin, id, &mut messages).await;
        sessions.push(
            created["result"]["sessionId"]
                .as_str()
                .unwrap_or_else(|| panic!("{created}"))
                .to_owned(),
        );
    }
    assert_eq!(sessions[0][..8], sessions[1][..8]);
    assert_eq!((launched("shared"), launched("own")), (1, 1));
    let shared = editor_server(&daemon_api(&data), &sessions[0], "shared").await;
    let tool = format!("mcp__{shared}__ping");

    daemon_guard.stop().await;
    daemon_guard.stopped = false;
    for (id, session) in [(42, &sessions[0]), (43, &sessions[1])] {
        send(
            &mut stdin,
            json!({"jsonrpc":"2.0", "id":id, "method":"session/prompt",
                   "params":{"sessionId":session,
                             "prompt":[{"type":"text", "text":format!("!tool {tool} {{}}")}]}}),
        )
        .await;
    }
    let mut answered = Vec::new();
    let mut got = HashSet::new();
    while answered.len() < 2 {
        let message = receive(&mut lines).await;
        if let Some(id) = message["id"]
            .as_u64()
            .filter(|_| message.get("method").is_none())
        {
            assert_eq!(message["result"]["stopReason"], "end_turn", "{message}");
            answered.push(id);
        } else if update_text(&message, "agent_message_chunk")
            .is_some_and(|text| text == format!("Tool {tool} => pong"))
        {
            got.insert(message["params"]["sessionId"].as_str().unwrap().to_owned());
        }
    }
    assert_eq!(got.len(), 2, "a session did not get the shared tool");
    assert_eq!(
        (launched("shared"), launched("own")),
        (2, 2),
        "a server was set up twice"
    );

    let api = daemon_api(&data);
    let names = async || -> Vec<String> {
        daemon(&api, reqwest::Method::GET, "/mcp", None)
            .await
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|server| server["name"].as_str().map(str::to_owned))
            .filter(|name| name.starts_with("acp-"))
            .collect()
    };
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":44, "method":"session/close",
               "params":{"sessionId":sessions[1]}}),
    )
    .await;
    let mut messages = Vec::new();
    let closed = receive_response(&mut lines, &mut stdin, 44, &mut messages).await;
    assert!(closed.get("error").is_none(), "{closed}");
    assert_eq!(names().await, [shared]);

    drop(stdin);
    let status = timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("Haarness ACP agent did not exit after stdin closed")
        .expect("wait for Haarness ACP agent");
    assert!(status.success(), "Haarness ACP process failed");
    assert!(names().await.is_empty());
    daemon_guard.stop().await;
}

/// An editor stopping `haarness acp` with a signal (as one quitting may)
/// lets its sessions' servers go too.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stopped_acp_agent_lets_its_sessions_servers_go() {
    let directory = TestDirectory::new();
    let (mut daemon_guard, mut child, mut stdin, mut lines, workspace, data, _) =
        acp_session(&directory).await;
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../haarness-mcp/tests/fixtures/recovery.py"
    );
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":50, "method":"session/new",
               "params":{"cwd":workspace, "mcpServers":[
                   {"name": "editor", "command": python3(), "args": [fixture],
                    "env": [{"name": "CALL_LOG", "value": directory.0.join("calls")}]}]}}),
    )
    .await;
    let mut messages = Vec::new();
    let created = receive_response(&mut lines, &mut stdin, 50, &mut messages).await;
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let api = daemon_api(&data);
    let listed = async || {
        daemon(&api, reqwest::Method::GET, "/mcp", None)
            .await
            .to_string()
    };
    let name = editor_server(&api, &session, "editor").await;
    let pid = child.id().expect("the ACP agent's process").to_string();
    let signalled = std::process::Command::new("kill")
        .args(["-TERM", &pid])
        .status()
        .expect("signal the ACP agent");
    assert!(signalled.success());
    timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("Haarness ACP agent did not exit after SIGTERM")
        .expect("wait for Haarness ACP agent");
    assert!(!listed().await.contains(&name), "{}", listed().await);
    drop(stdin);
    daemon_guard.stop().await;
}

/// Another `haarness acp` on the workspace, initialized: a second editor
/// window, or an editor that reconnected.
async fn second_agent(
    workspace: &Path,
    data: &Path,
    home: &Path,
) -> (
    tokio::process::Child,
    tokio::process::ChildStdin,
    Lines<BufReader<ChildStdout>>,
) {
    let mut command = base_command(workspace, data, home);
    command
        .args(["acp", "--provider", "echo", "--model", "echo-1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let mut child = command.spawn().expect("launch a second Haarness ACP agent");
    let mut stdin = child.stdin.take().expect("ACP stdin");
    let mut lines = BufReader::new(child.stdout.take().expect("ACP stdout")).lines();
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":1, "method":"initialize",
               "params":{"protocolVersion":1, "clientCapabilities":{}}}),
    )
    .await;
    assert_eq!(receive(&mut lines).await["id"], 1);
    (child, stdin, lines)
}

/// Send a prompt that calls `tool` and check that it answered.
async fn prompt_calls(
    stdin: &mut tokio::process::ChildStdin,
    lines: &mut Lines<BufReader<ChildStdout>>,
    id: u64,
    session: &str,
    tool: &str,
) {
    send(
        stdin,
        json!({"jsonrpc":"2.0", "id":id, "method":"session/prompt",
               "params":{"sessionId":session,
                         "prompt":[{"type":"text", "text":format!("!tool {tool} {{}}")}]}}),
    )
    .await;
    let mut messages = Vec::new();
    let answered = receive_response(lines, stdin, id, &mut messages).await;
    assert_eq!(answered["result"]["stopReason"], "end_turn", "{answered}");
    assert!(
        messages
            .iter()
            .filter_map(|message| update_text(message, "agent_message_chunk"))
            .any(|text| text == format!("Tool {tool} => pong")),
        "prompt {id} did not get {tool}: {messages:?}"
    );
}

/// The same session open in two editor windows, or an editor that
/// reconnects and loads the session before its old `haarness acp` has
/// exited, runs two agents on the project. They share the session's server
/// under the chat's name, started once: the first one quitting leaves it to
/// the second, whose prompts keep its tools. The second one killed (as Zed
/// stops its agents, with SIGKILL, which no process can answer), the server
/// is let go once the project's server sees the process gone.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agents_sharing_a_chat_share_its_servers_until_the_last_ends() {
    let directory = TestDirectory::new();
    let (mut daemon_guard, mut child, mut stdin, mut lines, workspace, data, _) =
        acp_session(&directory).await;
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../haarness-mcp/tests/fixtures/recovery.py"
    );
    let launch_log = directory.0.join("launches");
    let launched = || std::fs::read_to_string(&launch_log).map_or(0, |text| text.lines().count());
    let server = json!({"name": "editor", "command": python3(), "args": [fixture],
                        "env": [{"name": "LAUNCH_LOG", "value": launch_log},
                                {"name": "CALL_LOG", "value": directory.0.join("calls")}]});
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":60, "method":"session/new",
               "params":{"cwd":workspace, "mcpServers":[server.clone()]}}),
    )
    .await;
    let mut messages = Vec::new();
    let created = receive_response(&mut lines, &mut stdin, 60, &mut messages).await;
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let api = daemon_api(&data);
    let name = editor_server(&api, &session, "editor").await;
    assert_eq!(name, format!("acp-{}-editor", &session[..8]));

    let (mut second, mut second_in, mut second_lines) =
        second_agent(&workspace, &data, &directory.0.join("home")).await;
    send(
        &mut second_in,
        json!({"jsonrpc":"2.0", "id":61, "method":"session/load",
               "params":{"sessionId":session, "cwd":workspace, "mcpServers":[server]}}),
    )
    .await;
    let mut messages = Vec::new();
    let loaded = receive_response(&mut second_lines, &mut second_in, 61, &mut messages).await;
    assert!(loaded.get("error").is_none(), "{loaded}");
    assert_eq!(editor_server(&api, &session, "editor").await, name);
    assert_eq!(launched(), 1, "the second agent started the server again");

    drop(stdin);
    timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("the first ACP agent did not exit")
        .expect("wait for the first ACP agent");
    assert_eq!(editor_server(&api, &session, "editor").await, name);
    let tool = format!("mcp__{name}__ping");
    for id in [62, 63] {
        prompt_calls(&mut second_in, &mut second_lines, id, &session, &tool).await;
    }

    second.start_kill().expect("kill the second ACP agent");
    timeout(Duration::from_secs(20), second.wait())
        .await
        .expect("the second ACP agent did not end")
        .expect("wait for the second ACP agent");
    // Seen at the next listing.
    assert!(editor_servers(&api, &session, "editor").await.is_empty());
    drop(second_in);
    daemon_guard.stop().await;
}

/// Two agent processes on one chat (the same session open in two windows)
/// while the project's server restarts (an update): the first one's prompt
/// sets the chat's server up on the new server, and the second one's makes
/// it an owner there too, so the server stays for it once the first quits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agents_sharing_a_chat_both_own_its_server_after_a_restart() {
    let directory = TestDirectory::new();
    let (mut daemon_guard, mut child, mut stdin, mut lines, workspace, data, _) =
        acp_session(&directory).await;
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../haarness-mcp/tests/fixtures/recovery.py"
    );
    let launch_log = directory.0.join("launches");
    let launched = || std::fs::read_to_string(&launch_log).map_or(0, |text| text.lines().count());
    let server = json!({"name": "editor", "command": python3(), "args": [fixture],
                        "env": [{"name": "LAUNCH_LOG", "value": launch_log},
                                {"name": "CALL_LOG", "value": directory.0.join("calls")}]});
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":90, "method":"session/new",
               "params":{"cwd":workspace, "mcpServers":[server.clone()]}}),
    )
    .await;
    let mut messages = Vec::new();
    let created = receive_response(&mut lines, &mut stdin, 90, &mut messages).await;
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let name = editor_server(&daemon_api(&data), &session, "editor").await;
    let tool = format!("mcp__{name}__ping");
    let (mut second, mut second_in, mut second_lines) =
        second_agent(&workspace, &data, &directory.0.join("home")).await;
    send(
        &mut second_in,
        json!({"jsonrpc":"2.0", "id":91, "method":"session/load",
               "params":{"sessionId":session, "cwd":workspace, "mcpServers":[server]}}),
    )
    .await;
    let mut messages = Vec::new();
    let loaded = receive_response(&mut second_lines, &mut second_in, 91, &mut messages).await;
    assert!(loaded.get("error").is_none(), "{loaded}");
    assert_eq!(launched(), 1);

    daemon_guard.stop().await;
    daemon_guard.stopped = false;
    prompt_calls(&mut stdin, &mut lines, 92, &session, &tool).await;
    prompt_calls(&mut second_in, &mut second_lines, 93, &session, &tool).await;
    assert_eq!(launched(), 2, "the second agent started the server again");

    drop(stdin);
    timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("the first ACP agent did not exit")
        .expect("wait for the first ACP agent");
    let api = daemon_api(&data);
    assert_eq!(
        editor_servers(&api, &session, "editor").await,
        std::slice::from_ref(&name)
    );
    for id in [94, 95] {
        prompt_calls(&mut second_in, &mut second_lines, id, &session, &tool).await;
    }
    assert_eq!(launched(), 2);

    drop(second_in);
    timeout(Duration::from_secs(20), second.wait())
        .await
        .expect("the second ACP agent did not exit")
        .expect("wait for the second ACP agent");
    assert!(editor_servers(&api, &session, "editor").await.is_empty());
    daemon_guard.stop().await;
}

/// The chat's server turned off through the API on the project's server a
/// restart brought up stays off, also for a second agent process on the
/// chat that had it on the earlier server: turned off before that agent's
/// next prompt (`before`), or while that prompt was setting it up again
/// (the agent made to wait between reading the listing and configuring).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_turned_off_after_a_restart_stays_off_for_a_second_agent() {
    for before in [true, false] {
        let directory = TestDirectory::new();
        let (mut daemon_guard, mut child, mut stdin, mut lines, workspace, data, _) =
            acp_session(&directory).await;
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../haarness-mcp/tests/fixtures/recovery.py"
        );
        let launch_log = directory.0.join("launches");
        let launched =
            || std::fs::read_to_string(&launch_log).map_or(0, |text| text.lines().count());
        let server = json!({"name": "editor", "command": python3(), "args": [fixture],
                            "env": [{"name": "LAUNCH_LOG", "value": launch_log},
                                    {"name": "CALL_LOG", "value": directory.0.join("calls")}]});
        send(
            &mut stdin,
            json!({"jsonrpc":"2.0", "id":100, "method":"session/new",
                   "params":{"cwd":workspace, "mcpServers":[server.clone()]}}),
        )
        .await;
        let mut messages = Vec::new();
        let created = receive_response(&mut lines, &mut stdin, 100, &mut messages).await;
        let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
        let name = editor_server(&daemon_api(&data), &session, "editor").await;
        let tool = format!("mcp__{name}__ping");
        let mut command = base_command(&workspace, &data, &directory.0.join("home"));
        command
            .args(["acp", "--provider", "echo", "--model", "echo-1"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if !before {
            command.env("HAARNESS_TEST_ACP_RESTORE_DELAY_MS", "3000");
        }
        let mut second = command.spawn().expect("launch a second Haarness ACP agent");
        let mut second_in = second.stdin.take().expect("ACP stdin");
        let mut second_lines = BufReader::new(second.stdout.take().expect("ACP stdout")).lines();
        send(
            &mut second_in,
            json!({"jsonrpc":"2.0", "id":1, "method":"initialize",
                   "params":{"protocolVersion":1, "clientCapabilities":{}}}),
        )
        .await;
        assert_eq!(receive(&mut second_lines).await["id"], 1);
        send(
            &mut second_in,
            json!({"jsonrpc":"2.0", "id":101, "method":"session/load",
                   "params":{"sessionId":session, "cwd":workspace, "mcpServers":[server]}}),
        )
        .await;
        let mut messages = Vec::new();
        let loaded = receive_response(&mut second_lines, &mut second_in, 101, &mut messages).await;
        assert!(loaded.get("error").is_none(), "{loaded}");

        daemon_guard.stop().await;
        daemon_guard.stopped = false;
        prompt_calls(&mut stdin, &mut lines, 102, &session, &tool).await;
        assert_eq!(launched(), 2);
        let turn_off = async || {
            daemon(
                &daemon_api(&data),
                reqwest::Method::POST,
                &format!("/mcp/{name}"),
                Some(
                    json!({"transport": "stdio", "command": python3(), "args": [fixture],
                            "enabled": false}),
                ),
            )
            .await;
        };
        if before {
            turn_off().await;
            asked(&mut stdin, &mut lines, 103, &session, &tool, "once").await;
            asked(
                &mut second_in,
                &mut second_lines,
                104,
                &session,
                &tool,
                "once",
            )
            .await;
        } else {
            send(
                &mut second_in,
                json!({"jsonrpc":"2.0", "id":104, "method":"session/prompt",
                       "params":{"sessionId":session,
                                 "prompt":[{"type":"text", "text":format!("!tool {tool} {{}}")}]}}),
            )
            .await;
            // The second agent has read the listing and waits to configure.
            tokio::time::sleep(Duration::from_millis(1000)).await;
            turn_off().await;
            let mut messages = Vec::new();
            let answered =
                receive_response(&mut second_lines, &mut second_in, 104, &mut messages).await;
            assert_eq!(answered["result"]["stopReason"], "end_turn", "{answered}");
        }
        assert!(
            editor_servers(&daemon_api(&data), &session, "editor")
                .await
                .is_empty(),
            "turned off before: {before}"
        );
        assert_eq!(launched(), 2, "turned off before: {before}");

        drop(second_in);
        drop(stdin);
        for agent in [&mut child, &mut second] {
            timeout(Duration::from_secs(20), agent.wait())
                .await
                .expect("an ACP agent did not exit")
                .expect("wait for an ACP agent");
        }
        daemon_guard.stop().await;
    }
}

/// Send a prompt that calls `tool`, answer each request it raises with
/// `answer`, and say how many it raised.
async fn asked(
    stdin: &mut tokio::process::ChildStdin,
    lines: &mut Lines<BufReader<ChildStdout>>,
    id: u64,
    session: &str,
    tool: &str,
    answer: &str,
) -> usize {
    send(
        stdin,
        json!({"jsonrpc":"2.0", "id":id, "method":"session/prompt",
               "params":{"sessionId":session,
                         "prompt":[{"type":"text", "text":format!("!tool {tool} {{}}")}]}}),
    )
    .await;
    let mut asked = 0;
    loop {
        let message = receive(lines).await;
        if message["method"].as_str() == Some("session/request_permission") {
            asked += 1;
            send(
                stdin,
                json!({"jsonrpc":"2.0", "id":message["id"].clone(),
                       "result":{"outcome":{"outcome":"selected", "optionId":answer}}}),
            )
            .await;
        } else if message["id"].as_u64() == Some(id) {
            assert_eq!(message["result"]["stopReason"], "end_turn", "{message}");
            return asked;
        }
    }
}

/// An answer "Allow while Haarness is running" to an editor's MCP tool
/// still holds once the editor runs a new agent process for the chat (Zed
/// starts one for each thread it opens): the tool keeps its name.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_always_answer_holds_for_an_editors_tool_in_a_new_agent_process() {
    let directory = TestDirectory::new();
    let workspace = directory.0.join("workspace");
    let data = directory.0.join("data");
    let home = directory.0.join("home");
    std::fs::create_dir_all(workspace.join(".haarness")).expect("create ACP workspace config");
    std::fs::write(
        workspace.join(".haarness/config.toml"),
        "[defaults]\napproval_mode = \"review\"\n[permissions]\n\"mcp__*\" = \"ask\"\n",
    )
    .expect("configure MCP tools to ask");
    std::fs::create_dir_all(&home).expect("create isolated ACP home");
    let mut daemon_guard = DaemonGuard::new(workspace.clone(), data.clone(), home.clone());
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../haarness-mcp/tests/fixtures/recovery.py"
    );
    let server = json!({"name": "editor", "command": python3(), "args": [fixture],
                        "env": [{"name": "CALL_LOG", "value": directory.0.join("calls")}]});
    let (mut first, mut first_in, mut first_lines) = second_agent(&workspace, &data, &home).await;
    send(
        &mut first_in,
        json!({"jsonrpc":"2.0", "id":2, "method":"session/new",
               "params":{"cwd":workspace, "mcpServers":[server.clone()]}}),
    )
    .await;
    let mut messages = Vec::new();
    let created = receive_response(&mut first_lines, &mut first_in, 2, &mut messages).await;
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let tool = format!(
        "mcp__{}__ping",
        editor_server(&daemon_api(&data), &session, "editor").await
    );
    assert_eq!(
        asked(
            &mut first_in,
            &mut first_lines,
            3,
            &session,
            &tool,
            "always"
        )
        .await,
        1
    );
    assert_eq!(
        asked(&mut first_in, &mut first_lines, 4, &session, &tool, "once").await,
        0
    );
    drop(first_in);
    timeout(Duration::from_secs(20), first.wait())
        .await
        .expect("the first ACP agent did not exit")
        .expect("wait for the first ACP agent");

    let (mut second, mut second_in, mut second_lines) =
        second_agent(&workspace, &data, &home).await;
    send(
        &mut second_in,
        json!({"jsonrpc":"2.0", "id":2, "method":"session/load",
               "params":{"sessionId":session, "cwd":workspace, "mcpServers":[server]}}),
    )
    .await;
    let mut messages = Vec::new();
    let loaded = receive_response(&mut second_lines, &mut second_in, 2, &mut messages).await;
    assert!(loaded.get("error").is_none(), "{loaded}");
    assert_eq!(
        asked(
            &mut second_in,
            &mut second_lines,
            3,
            &session,
            &tool,
            "once"
        )
        .await,
        0,
        "the new agent process asked again"
    );
    drop(second_in);
    timeout(Duration::from_secs(20), second.wait())
        .await
        .expect("the second ACP agent did not exit")
        .expect("wait for the second ACP agent");
    daemon_guard.stop().await;
}

/// Two sessions of one agent share a server. Once the project's server has
/// restarted, the second session's prompt reads the listing while the first
/// session's prompt is about to set the server up again there: it waits for
/// that and gets the tool, rather than taking the server for turned off.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_that_read_the_listing_first_waits_for_the_server_set_up_again() {
    let directory = TestDirectory::new();
    let workspace = directory.0.join("workspace");
    let data = directory.0.join("data");
    let home = directory.0.join("home");
    std::fs::create_dir_all(workspace.join(".haarness")).expect("create ACP workspace config");
    std::fs::create_dir_all(&home).expect("create isolated ACP home");
    let mut daemon_guard = DaemonGuard::new(workspace.clone(), data.clone(), home.clone());
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../haarness-mcp/tests/fixtures/recovery.py"
    );
    let launch_log = directory.0.join("launches");
    let launched = || std::fs::read_to_string(&launch_log).map_or(0, |text| text.lines().count());
    let server = json!({"name": "shared", "command": python3(), "args": [fixture],
                        "env": [{"name": "LAUNCH_LOG", "value": launch_log},
                                {"name": "CALL_LOG", "value": directory.0.join("calls")}]});
    // Configuring a session's server again waits 2 s first.
    let mut command = base_command(&workspace, &data, &home);
    command
        .args(["acp", "--provider", "echo", "--model", "echo-1"])
        .env("HAARNESS_TEST_ACP_RESTORE_DELAY_MS", "2000")
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
    // A chat id starts with its time in units of 65.536 s: both sessions are
    // made well inside one.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let left = 65_536 - (now % 65_536);
    if left < 15_000 {
        tokio::time::sleep(Duration::from_millis(u64::try_from(left).unwrap() + 100)).await;
    }
    let mut sessions = Vec::new();
    for id in [80, 81] {
        send(
            &mut stdin,
            json!({"jsonrpc":"2.0", "id":id, "method":"session/new",
                   "params":{"cwd":workspace, "mcpServers":[server.clone()]}}),
        )
        .await;
        let mut messages = Vec::new();
        let created = receive_response(&mut lines, &mut stdin, id, &mut messages).await;
        sessions.push(created["result"]["sessionId"].as_str().unwrap().to_owned());
    }
    assert_eq!(sessions[0][..8], sessions[1][..8]);
    let api = daemon_api(&data);
    let name = editor_server(&api, &sessions[0], "shared").await;
    let tool = format!("mcp__{name}__ping");
    let ask = |id: u64, session: &str| {
        json!({"jsonrpc":"2.0", "id":id, "method":"session/prompt",
               "params":{"sessionId":session,
                         "prompt":[{"type":"text", "text":format!("!tool {tool} {{}}")}]}})
    };

    daemon_guard.stop().await;
    daemon_guard.stopped = false;
    send(&mut stdin, ask(82, &sessions[0])).await;
    // The new server is up and the first prompt waits before configuring
    // the server again: the second prompt reads a listing without it.
    timeout(Duration::from_secs(30), async {
        while running_daemon_api(&data).is_none_or(|next| next == api) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the first prompt did not start the project's server again");
    tokio::time::sleep(Duration::from_millis(300)).await;
    send(&mut stdin, ask(83, &sessions[1])).await;
    let mut answered = Vec::new();
    let mut got = HashSet::new();
    while answered.len() < 2 {
        let message = receive(&mut lines).await;
        if let Some(id) = message["id"]
            .as_u64()
            .filter(|_| message.get("method").is_none())
        {
            assert_eq!(message["result"]["stopReason"], "end_turn", "{message}");
            answered.push(id);
        } else if update_text(&message, "agent_message_chunk")
            .is_some_and(|text| text == format!("Tool {tool} => pong"))
        {
            got.insert(message["params"]["sessionId"].as_str().unwrap().to_owned());
        }
    }
    assert_eq!(
        got.len(),
        2,
        "a session ran without the shared server's tool"
    );
    assert_eq!(launched(), 2, "the shared server was set up twice");
    assert_eq!(
        editor_servers(&daemon_api(&data), &sessions[0], "shared").await,
        [name]
    );

    drop(stdin);
    timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("Haarness ACP agent did not exit after stdin closed")
        .expect("wait for Haarness ACP agent");
    daemon_guard.stop().await;
}

/// Two sessions of one agent share a server slow to start (a first `npx
/// -y` download). Once the project's server has restarted, the first
/// session's prompt sets it up again; the second session's prompt, sent
/// while that start runs, waits for it rather than running without its
/// tools, and the server is set up once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_waits_for_a_shared_server_another_session_sets_up_again() {
    let directory = TestDirectory::new();
    let (mut daemon_guard, mut child, mut stdin, mut lines, workspace, data, _) =
        acp_session(&directory).await;
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../haarness-mcp/tests/fixtures/recovery.py"
    );
    let launch_log = directory.0.join("launches");
    let server = json!({"name": "slow", "command": python3(), "args": [fixture],
                        "env": [{"name": "INIT_DELAY", "value": "3"},
                                {"name": "LAUNCH_LOG", "value": launch_log},
                                {"name": "CALL_LOG", "value": directory.0.join("calls")}]});
    let launched = || std::fs::read_to_string(&launch_log).map_or(0, |text| text.lines().count());
    // A chat id starts with its time in units of 65.536 s: both sessions are
    // made well inside one.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let left = 65_536 - (now % 65_536);
    if left < 15_000 {
        tokio::time::sleep(Duration::from_millis(u64::try_from(left).unwrap() + 100)).await;
    }
    let mut sessions = Vec::new();
    for id in [70, 71] {
        send(
            &mut stdin,
            json!({"jsonrpc":"2.0", "id":id, "method":"session/new",
                   "params":{"cwd":workspace, "mcpServers":[server.clone()]}}),
        )
        .await;
        let mut messages = Vec::new();
        let created = receive_response(&mut lines, &mut stdin, id, &mut messages).await;
        sessions.push(created["result"]["sessionId"].as_str().unwrap().to_owned());
    }
    assert_eq!(sessions[0][..8], sessions[1][..8]);
    let api = daemon_api(&data);
    let name = editor_server(&api, &sessions[0], "slow").await;
    let tool = format!("mcp__{name}__ping");
    let ask = |id: u64, session: &str| {
        json!({"jsonrpc":"2.0", "id":id, "method":"session/prompt",
               "params":{"sessionId":session,
                         "prompt":[{"type":"text", "text":format!("!tool {tool} {{}}")}]}})
    };

    daemon_guard.stop().await;
    daemon_guard.stopped = false;
    send(&mut stdin, ask(72, &sessions[0])).await;
    // The second prompt goes once the new server lists the server starting.
    let started = timeout(Duration::from_secs(30), async {
        loop {
            if let Some(next) = running_daemon_api(&data).filter(|next| *next != api) {
                let listed = reqwest::Client::new()
                    .get(format!("{}/v1/mcp", next.0))
                    .bearer_auth(&next.1)
                    .send()
                    .await;
                if let Ok(listed) = listed
                    && let Ok(listed) = listed.json::<Value>().await
                    && listed.as_array().is_some_and(|servers| {
                        servers.iter().any(|server| {
                            server["name"] == name.as_str() && server["status"] == "connecting"
                        })
                    })
                {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(
        started.is_ok(),
        "the first prompt never set the server up again"
    );
    send(&mut stdin, ask(73, &sessions[1])).await;
    let mut answered = Vec::new();
    let mut got = HashSet::new();
    while answered.len() < 2 {
        let message = receive(&mut lines).await;
        if let Some(id) = message["id"]
            .as_u64()
            .filter(|_| message.get("method").is_none())
        {
            assert_eq!(message["result"]["stopReason"], "end_turn", "{message}");
            answered.push(id);
        } else if update_text(&message, "agent_message_chunk")
            .is_some_and(|text| text == format!("Tool {tool} => pong"))
        {
            got.insert(message["params"]["sessionId"].as_str().unwrap().to_owned());
        }
    }
    assert_eq!(
        got.len(),
        2,
        "a session ran without the shared server's tool"
    );
    assert_eq!(launched(), 2, "the shared server was set up twice");

    drop(stdin);
    timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("Haarness ACP agent did not exit after stdin closed")
        .expect("wait for Haarness ACP agent");
    daemon_guard.stop().await;
}

/// `haarness acp -m provider/model`, the form the docs show, names that
/// provider as `haarness run` does: the bare id went to the default provider,
/// which could be another account's (Haarness Cloud spending its credit for a
/// model the person meant to run on their own sign-in).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_model_named_with_its_provider_runs_on_that_provider() {
    let directory = TestDirectory::new();
    let workspace = directory.0.join("workspace");
    let data = directory.0.join("data");
    let home = directory.0.join("home");
    std::fs::create_dir_all(workspace.join(".haarness")).expect("create ACP workspace config");
    std::fs::create_dir_all(&home).expect("create isolated ACP home");
    let mut server = DaemonGuard::new(workspace.clone(), data.clone(), home.clone());
    let mut command = base_command(&workspace, &data, &home);
    command
        .arg("acp")
        .arg("-m")
        .arg("echo/echo-1")
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
    let created = receive(&mut lines).await;
    let session = created["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("ACP session id: {created}"))
        .to_owned();
    let thread = daemon(
        &daemon_api(&data),
        reqwest::Method::GET,
        &format!("/threads/{session}"),
        None,
    )
    .await;
    let thread = thread.get("thread").unwrap_or(&thread);
    assert_eq!(thread["provider"], "echo", "{thread}");
    assert_eq!(thread["model"], "echo-1", "{thread}");
    drop(stdin);
    let _ = child.wait().await;
    server.stop().await;
}

/// The session update of `kind` about `call` in `messages`.
fn call_update<'a>(messages: &'a [Value], kind: &str, call: &Value) -> &'a Value {
    messages
        .iter()
        .map(|message| &message["params"]["update"])
        .find(|update| {
            update["sessionUpdate"].as_str() == Some(kind)
                && &update["toolCallId"] == call
                && (kind == "tool_call" || update["status"].as_str() == Some("completed"))
        })
        .unwrap_or_else(|| panic!("no {kind} for {call}: {messages:?}"))
}

/// The diffs among a tool result's content.
fn diffs(update: &Value) -> Vec<Value> {
    update["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|content| content["type"] == "diff")
        .cloned()
        .collect()
}

/// An editor is told which files a call works on, at absolute paths it can
/// open and follow, spelled from the folder it opened the session in, and
/// sees what an edit changed as a diff beside the call's text result, live
/// and when the session is loaded again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn edits_reach_the_editor_as_diffs_at_the_files_they_change() {
    let directory = TestDirectory::new();
    let (mut daemon_guard, mut child, mut stdin, mut lines, workspace, _data, session) =
        acp_session(&directory).await;
    std::fs::write(workspace.join("notes.txt"), "one\ntwo\n").expect("write the edited file");
    std::fs::write(workspace.join("gone.txt"), "bye\n").expect("write the deleted file");
    // As the session was opened: on macOS the temporary folder is reached
    // through a link (`/var` is `/private/var`), and on Windows through a
    // short name, which the server's own name for the workspace resolves.
    let at = |file: &str| workspace.join(file).to_string_lossy().into_owned();

    let edit = json!({"path":"notes.txt", "old":"two", "new":"TWO"});
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":3, "method":"session/prompt",
               "params":{"sessionId":session,
                         "prompt":[{"type":"text", "text":format!("!tool edit {edit}")}]}}),
    )
    .await;
    let mut messages = Vec::new();
    let response = receive_response(&mut lines, &mut stdin, 3, &mut messages).await;
    assert_eq!(response["result"]["stopReason"], "end_turn", "{messages:?}");
    let call = messages
        .iter()
        .map(|message| &message["params"]["update"])
        .find(|update| update["sessionUpdate"] == "tool_call")
        .unwrap_or_else(|| panic!("no tool call: {messages:?}"));
    assert_eq!(call["title"], "Edit notes.txt");
    assert_eq!(call["locations"], json!([{"path": at("notes.txt")}]));
    let result = call_update(&messages, "tool_call_update", &call["toolCallId"]);
    assert_eq!(result["locations"], call["locations"]);
    assert_eq!(
        result["content"][0]["type"], "content",
        "the text result stays: {result}"
    );
    assert_eq!(
        diffs(result),
        [json!({"type":"diff", "path": at("notes.txt"),
                "oldText":"one\ntwo\n", "newText":"one\nTWO\n"})]
    );

    // A patch names its files: one it adds (no old text) and one it deletes
    // (empty new text).
    let patch = json!({"input": "*** Begin Patch\n*** Add File: added.txt\n+fresh\n*** Delete File: gone.txt\n*** End Patch\n"});
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":4, "method":"session/prompt",
               "params":{"sessionId":session,
                         "prompt":[{"type":"text", "text":format!("!tool apply_patch {patch}")}]}}),
    )
    .await;
    let mut patched = Vec::new();
    let response = receive_response(&mut lines, &mut stdin, 4, &mut patched).await;
    assert_eq!(response["result"]["stopReason"], "end_turn", "{patched:?}");
    assert!(!workspace.join("gone.txt").exists(), "{patched:?}");
    let call = patched
        .iter()
        .map(|message| &message["params"]["update"])
        .find(|update| update["sessionUpdate"] == "tool_call")
        .unwrap_or_else(|| panic!("no tool call: {patched:?}"));
    assert_eq!(call["title"], "Edit added.txt and 1 more");
    assert_eq!(call["kind"], "edit");
    assert_eq!(
        call["locations"],
        json!([{"path": at("added.txt")}, {"path": at("gone.txt")}])
    );
    let result = call_update(&patched, "tool_call_update", &call["toolCallId"]);
    assert_eq!(
        diffs(result),
        [
            json!({"type":"diff", "path": at("added.txt"), "newText":"fresh\n"}),
            json!({"type":"diff", "path": at("gone.txt"), "oldText":"bye\n", "newText":""}),
        ]
    );

    // A batch names the files of its steps and shows what each changed.
    let batch = json!({"tool_calls": [
        {"tool":"edit", "parameters":{"path":"notes.txt", "old":"one", "new":"ONE"}},
        {"tool":"write", "parameters":{"path":"batched.txt", "content":"step\n"}},
    ]});
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":5, "method":"session/prompt",
               "params":{"sessionId":session,
                         "prompt":[{"type":"text", "text":format!("!tool batch {batch}")}]}}),
    )
    .await;
    let mut batched = Vec::new();
    let response = receive_response(&mut lines, &mut stdin, 5, &mut batched).await;
    assert_eq!(response["result"]["stopReason"], "end_turn", "{batched:?}");
    let call = batched
        .iter()
        .map(|message| &message["params"]["update"])
        .find(|update| update["sessionUpdate"] == "tool_call")
        .unwrap_or_else(|| panic!("no tool call: {batched:?}"));
    assert_eq!(
        call["locations"],
        json!([{"path": at("notes.txt")}, {"path": at("batched.txt")}])
    );
    let result = call_update(&batched, "tool_call_update", &call["toolCallId"]);
    assert_eq!(
        diffs(result),
        [
            json!({"type":"diff", "path": at("notes.txt"),
                   "oldText":"one\nTWO\n", "newText":"ONE\nTWO\n"}),
            json!({"type":"diff", "path": at("batched.txt"), "newText":"step\n"}),
        ],
        "{result}"
    );

    // A load shows the same, from the stored chat.
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":6, "method":"session/load",
               "params":{"sessionId":session, "cwd":workspace, "mcpServers":[]}}),
    )
    .await;
    let mut replayed = Vec::new();
    let loaded = receive_response(&mut lines, &mut stdin, 6, &mut replayed).await;
    assert_eq!(loaded["result"], json!({}), "{replayed:?}");
    let updates = replayed
        .iter()
        .map(|message| &message["params"]["update"])
        .collect::<Vec<_>>();
    assert!(
        updates
            .iter()
            .any(|update| update["sessionUpdate"] == "tool_call"
                && update["title"] == "Edit notes.txt"
                && update["locations"] == json!([{"path": at("notes.txt")}])),
        "{replayed:?}"
    );
    let replayed_diffs = updates
        .iter()
        .filter(|update| update["sessionUpdate"] == "tool_call_update")
        .flat_map(|update| diffs(update))
        .collect::<Vec<_>>();
    assert_eq!(
        replayed_diffs,
        [
            json!({"type":"diff", "path": at("notes.txt"),
                   "oldText":"one\ntwo\n", "newText":"one\nTWO\n"}),
            json!({"type":"diff", "path": at("added.txt"), "newText":"fresh\n"}),
            json!({"type":"diff", "path": at("gone.txt"), "oldText":"bye\n", "newText":""}),
            json!({"type":"diff", "path": at("notes.txt"),
                   "oldText":"one\nTWO\n", "newText":"ONE\nTWO\n"}),
            json!({"type":"diff", "path": at("batched.txt"), "newText":"step\n"}),
        ]
    );

    drop(stdin);
    let status = timeout(Duration::from_secs(20), child.wait())
        .await
        .expect("Haarness ACP agent did not exit after stdin closed")
        .expect("wait for Haarness ACP agent");
    assert!(status.success(), "Haarness ACP process failed");
    daemon_guard.stop().await;
}

/// After each prompt, and after a load, the editor is told how much of the
/// model's context window the chat fills.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_prompt_and_a_load_report_how_full_the_context_is() {
    // An OpenAI-compatible endpoint, configured with its model's window.
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(|| async {
            (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                "data: {\"choices\":[{\"delta\":{\"content\":\"metered\"}}]}\n\n\
                 data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\
                 \"usage\":{\"prompt_tokens\":1200,\"completion_tokens\":3}}\n\n\
                 data: [DONE]\n\n",
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the model endpoint");
    let port = listener.local_addr().expect("endpoint address").port();
    let endpoint = tokio::spawn(async move { axum::serve(listener, app).await });

    let directory = TestDirectory::new();
    let workspace = directory.0.join("workspace");
    let data = directory.0.join("data");
    let home = directory.0.join("home");
    std::fs::create_dir_all(workspace.join(".haarness")).expect("create ACP workspace config");
    std::fs::write(
        workspace.join(".haarness/config.toml"),
        format!(
            "[providers.meter]\nprotocol = \"openai-chat-completions\"\nname = \"Meter\"\n\
             base_url = \"http://127.0.0.1:{port}/v1\"\nrequires_auth = false\n\
             models = [\"meter-1\"]\ncontext_window = 200000\n"
        ),
    )
    .expect("configure the metered provider");
    std::fs::create_dir_all(&home).expect("create isolated ACP home");
    let mut server = DaemonGuard::new(workspace.clone(), data.clone(), home.clone());
    let mut command = base_command(&workspace, &data, &home);
    command
        .arg("acp")
        .arg("-m")
        .arg("meter/meter-1")
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
    let created = receive(&mut lines).await;
    let session = created["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("ACP session id: {created}"))
        .to_owned();

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":3, "method":"session/prompt",
               "params":{"sessionId":session,
                         "prompt":[{"type":"text", "text":"How full is it?"}]}}),
    )
    .await;
    let mut messages = Vec::new();
    let response = receive_response(&mut lines, &mut stdin, 3, &mut messages).await;
    assert_eq!(response["result"]["stopReason"], "end_turn", "{messages:?}");
    let answer = messages
        .iter()
        .position(|message| update_text(message, "agent_message_chunk") == Some("metered"))
        .unwrap_or_else(|| panic!("no answer: {messages:?}"));
    let usage = messages
        .iter()
        .position(|message| message["params"]["update"]["sessionUpdate"] == "usage_update")
        .unwrap_or_else(|| panic!("no usage update: {messages:?}"));
    assert!(usage > answer, "the usage follows the turn: {messages:?}");
    let update = &messages[usage]["params"]["update"];
    assert_eq!(update["size"], 200_000, "{update}");
    assert!(
        update["used"].as_u64().is_some_and(|used| used >= 1200),
        "the context holds what the endpoint counted: {update}"
    );

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":4, "method":"session/load",
               "params":{"sessionId":session, "cwd":workspace, "mcpServers":[]}}),
    )
    .await;
    let mut replayed = Vec::new();
    let loaded = receive_response(&mut lines, &mut stdin, 4, &mut replayed).await;
    assert_eq!(loaded["result"], json!({}), "{replayed:?}");
    let last = replayed.last().map(|message| &message["params"]["update"]);
    assert_eq!(
        last.map(|update| &update["sessionUpdate"]),
        Some(&json!("usage_update")),
        "a load ends with the chat's usage: {replayed:?}"
    );
    let last = last.unwrap();
    assert!(
        last["used"].as_u64().is_some_and(|used| used >= 1200),
        "{last}"
    );
    assert_eq!(last["size"], 200_000);

    drop(stdin);
    let _ = child.wait().await;
    server.stop().await;
    endpoint.abort();
}
