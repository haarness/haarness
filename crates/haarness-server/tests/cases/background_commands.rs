//! The agents' shell commands, which can run on in the background: the
//! routes that list, read, stop and move them, and what a published chat may
//! see of them.
use super::*;
use haarness_protocol::{BACKGROUND_COMMANDS_CAPABILITY, ShellCommandInfo, ThreadId};

async fn chat(runtime: &AgentRuntime, parent: Option<ThreadId>) -> ThreadId {
    runtime
        .create_thread(CreateThreadRequest {
            parent_id: parent,
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn the_command_routes_answer_for_a_chat_and_stay_inside_a_published_chat() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, true);
    let runtime = Arc::new(runtime);
    let handle = activity_server(runtime.clone(), None, &workspace).await;
    let base = format!("http://{}", handle.address);
    let thread = chat(&runtime, None).await;
    let child = chat(&runtime, Some(thread)).await;
    let other = chat(&runtime, None).await;
    let client = reqwest::Client::new();
    let send = |method: reqwest::Method, path: String, scope: Option<ThreadId>| {
        let mut request = client
            .request(method, format!("{base}/v1/{path}"))
            .bearer_auth("activity-token");
        if let Some(scope) = scope {
            request = request.header("x-haarness-remote-thread", scope.to_string());
        }
        request.send()
    };
    let get = reqwest::Method::GET;
    let post = reqwest::Method::POST;

    let info: Value = send(get.clone(), "info".into(), None)
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        info["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == BACKGROUND_COMMANDS_CAPABILITY),
        "{info}"
    );
    // A chat with no commands lists none, and so does every chat.
    for path in [
        format!("threads/{thread}/commands"),
        format!("threads/{child}/commands"),
        "commands/active".to_owned(),
    ] {
        let response = send(get.clone(), path.clone(), None).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK, "{path}");
        let listed: Vec<ShellCommandInfo> = response.json().await.unwrap();
        assert!(listed.is_empty(), "{path}");
    }
    // An unknown command, or one of an unknown chat, is not found.
    for (method, path) in [
        (get.clone(), format!("threads/{thread}/commands/c-missing")),
        (
            get.clone(),
            format!("threads/{thread}/commands/c-missing/output?cursor=0&limit=10&tail=true"),
        ),
        (
            post.clone(),
            format!("threads/{thread}/commands/c-missing/stop"),
        ),
        (
            post.clone(),
            format!("threads/{thread}/commands/background?commandId=c-missing"),
        ),
        (get.clone(), format!("threads/{}/commands", ThreadId::new())),
    ] {
        let response = send(method, path.clone(), None).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND, "{path}");
    }
    // Moving every command of a chat with none moves nothing.
    let moved: Vec<ShellCommandInfo> = send(
        post.clone(),
        format!("threads/{thread}/commands/background"),
        None,
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(moved.is_empty());
    // Slash commands still run on the same path with POST.
    let command = client
        .post(format!("{base}/v1/threads/{thread}/commands"))
        .bearer_auth("activity-token")
        .json(&serde_json::json!({"name": "no-such-command"}))
        .send()
        .await
        .unwrap();
    assert_ne!(command.status(), reqwest::StatusCode::METHOD_NOT_ALLOWED);
    assert!(command.status().is_client_error(), "{}", command.status());
    let activity: Value = send(get.clone(), format!("threads/{thread}/activity"), None)
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(activity["backgroundCommands"], 0, "{activity}");

    // A published chat reads its own tree's commands and every chat's
    // running ones (filtered to its tree), never another chat's.
    for path in [
        format!("threads/{thread}/commands"),
        format!("threads/{child}/commands"),
        "commands/active".to_owned(),
    ] {
        let response = send(get.clone(), path.clone(), Some(thread)).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK, "{path}");
    }
    for (method, path) in [
        (get.clone(), format!("threads/{other}/commands")),
        (post.clone(), format!("threads/{other}/commands/background")),
        (
            post.clone(),
            format!("threads/{other}/commands/c-missing/stop"),
        ),
    ] {
        let response = send(method, path.clone(), Some(thread)).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN, "{path}");
    }
    handle.shutdown().await.unwrap();
}

/// A folder of the test's own, removed with it.
struct Workspace(std::path::PathBuf);
impl Workspace {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "haarness-background-commands-{}",
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A runtime over the scripted provider whose agents may run any command,
/// with `execution` settings changed by `settings`.
fn commands_runtime(
    workspace: &std::path::Path,
    settings: impl FnOnce(&mut haarness_core::ExecutionSettings),
) -> (Arc<AgentRuntime>, Capturing) {
    let provider = Capturing::default();
    let mut config = haarness_core::RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..haarness_core::RuntimeConfig::default()
    };
    config.base_rules.push(haarness_core::PermissionRule {
        pattern: "*".into(),
        decision: haarness_core::PermissionDecision::Allow,
    });
    config.execution.shell_stop_grace_ms = 300;
    settings(&mut config.execution);
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(workspace.to_path_buf())),
        EventBus::new(4096),
        workspace.to_path_buf(),
        String::new(),
        config,
    );
    (Arc::new(runtime), provider)
}

/// A turn running `script`, from a client that follows background work
/// up when `follows`.
async fn scripted(
    runtime: &AgentRuntime,
    thread: ThreadId,
    script: &str,
    follows: bool,
) -> haarness_protocol::Turn {
    let mut request = turn_request(script);
    request.follows_background = follows;
    runtime.start_turn(thread, request).await.unwrap()
}

/// The result the chat's last `bash`-family call got.
async fn last_result(runtime: &AgentRuntime, thread: ThreadId) -> (String, Value) {
    runtime
        .items(thread, None)
        .await
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|item| match item.data {
            haarness_protocol::ItemData::ToolResult {
                output, metadata, ..
            } => Some((output, metadata)),
            _ => None,
        })
        .expect("a tool result")
}

/// Wait until `check` holds for the chat's command `id`.
async fn command_until(
    runtime: &AgentRuntime,
    thread: ThreadId,
    id: &str,
    check: impl Fn(&ShellCommandInfo) -> bool,
) -> ShellCommandInfo {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let info = runtime.shell_command(thread, id).await.unwrap();
            if check(&info) {
                return info;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("command {id} never got there"))
}

/// The hidden notices the chat holds.
async fn notices(runtime: &AgentRuntime, thread: ThreadId) -> Vec<String> {
    runtime
        .items(thread, None)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item.data {
            haarness_protocol::ItemData::UserMessage {
                text,
                synthetic: true,
                ..
            } if text.starts_with("<command ") => Some(text),
            _ => None,
        })
        .collect()
}

/// The command checkpoints the chat holds, as (id, state).
async fn checkpoints(
    runtime: &AgentRuntime,
    thread: ThreadId,
) -> Vec<(String, haarness_protocol::ShellCommandState)> {
    runtime
        .items(thread, None)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item.data {
            haarness_protocol::ItemData::CommandState {
                command_id, state, ..
            } => Some((command_id, state)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_command_started_in_the_background_returns_at_once_and_tells_its_chat_when_it_ends() {
    use haarness_protocol::ShellCommandState as State;
    let workspace = Workspace::new();
    let (runtime, provider) = commands_runtime(&workspace.0, |_| {});
    let events = runtime.events().subscribe(None);
    let mut events = events.receiver;
    let thread = chat(&runtime, None).await;
    let started = std::time::Instant::now();
    let turn = scripted(
        &runtime,
        thread,
        r#"!tool bash {"command":"printf 'ready\n'; sleep 3; printf 'done\n'; exit 3","run_in_background":true}"#,
        true,
    )
    .await;
    wait_done(&runtime, &turn).await;
    // The call answered before the command could have ended.
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    let (text, metadata) = last_result(&runtime, thread).await;
    assert!(text.contains("runs in the background"), "{text}");
    assert_eq!(metadata["command"]["background"], true, "{metadata}");
    let id = metadata["command"]["id"].as_str().unwrap().to_owned();
    assert!(text.contains(&id), "{text}");
    let running = runtime.shell_command(thread, &id).await.unwrap();
    assert!(running.background && running.state.is_live());
    assert_eq!(
        running.moved_by,
        Some(haarness_protocol::ShellCommandMovedBy::Agent)
    );
    assert!(running.delivers_notice);
    assert_eq!(
        runtime
            .thread_activity(thread)
            .await
            .unwrap()
            .background_commands,
        1
    );
    let ended = command_until(&runtime, thread, &id, |info| !info.state.is_live()).await;
    assert_eq!((ended.state, ended.exit_code), (State::Exited, Some(3)));
    // Its end reaches the chat as a turn of its own, after its call's result.
    let told = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let told = notices(&runtime, thread).await;
            if !told.is_empty() && runtime.active_turn(thread).await.unwrap().is_none() {
                return told;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(
        told[0].starts_with(&format!(
            "<command id=\"{id}\" state=\"exited\" exit=\"3\">"
        )),
        "{}",
        told[0]
    );
    assert!(told[0].contains("ready\ndone"), "{}", told[0]);
    assert!(
        provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| format!("{:?}", request.messages).contains("exited with code 3")),
        "the model read the notice"
    );
    assert_eq!(
        checkpoints(&runtime, thread).await,
        [(id.clone(), State::Running), (id.clone(), State::Exited)]
    );
    assert_eq!(
        runtime
            .thread_activity(thread)
            .await
            .unwrap()
            .background_commands,
        0
    );
    // Clients followed it: an update when it started, and when it ended.
    let mut updates = Vec::new();
    let mut printed = Vec::new();
    while let Ok(event) = events.try_recv() {
        if event.event_type == "command.updated" {
            updates.push(event.payload["state"].as_str().unwrap().to_owned());
        } else if event.event_type == "command.output" {
            assert_eq!(event.payload["id"], id.as_str());
            assert_eq!(event.thread_id, Some(thread));
            printed.push(event.payload["size"].as_u64().unwrap());
        }
    }
    assert_eq!(updates.first().map(String::as_str), Some("running"));
    assert_eq!(updates.last().map(String::as_str), Some("exited"));
    // Output arrives as sizes, never text, and no more than once a second.
    assert!(!printed.is_empty() && printed.len() <= 4, "{printed:?}");
    assert!(
        printed.windows(2).all(|pair| pair[0] < pair[1]),
        "{printed:?}"
    );
    // Its whole output stays readable for a while, by cursor.
    let page = runtime
        .shell_command_output(thread, &id, Some(0), Some(6), false)
        .await
        .unwrap();
    assert_eq!((page.text.as_str(), page.cursor), ("ready\n", 6));
    assert!(page.more && page.settled);
    let rest = runtime
        .shell_command_output(thread, &id, Some(page.cursor), None, false)
        .await
        .unwrap();
    assert_eq!(rest.text, "done\n");
    assert!(!rest.more);
    let end = runtime
        .shell_command_output(thread, &id, None, Some(5), true)
        .await
        .unwrap();
    assert_eq!(end.text, "done\n");
}

#[tokio::test]
async fn a_long_foreground_command_moves_to_the_background_only_where_a_client_follows() {
    use haarness_protocol::{ShellCommandMovedBy, ShellCommandState as State};
    let workspace = Workspace::new();
    let (runtime, _) = commands_runtime(&workspace.0, |settings| {
        settings.shell_move_after_ms = 400;
    });
    let thread = chat(&runtime, None).await;
    let script = r#"!tool bash {"command":"printf 'started\n'; sleep 2; printf 'finished\n'"}"#;
    // A client that follows background work up: the reply goes on.
    let turn = scripted(&runtime, thread, script, true).await;
    let finished = wait_done(&runtime, &turn).await;
    assert_eq!(finished.status, TurnStatus::Completed);
    let (text, metadata) = last_result(&runtime, thread).await;
    assert!(text.contains("moved to the background"), "{text}");
    assert!(text.contains("started"), "{text}");
    let id = metadata["command"]["id"].as_str().unwrap().to_owned();
    let moved = runtime.shell_command(thread, &id).await.unwrap();
    assert_eq!(moved.moved_by, Some(ShellCommandMovedBy::Limit));
    assert!(moved.background && moved.background_at.is_some());
    let ended = command_until(&runtime, thread, &id, |info| !info.state.is_live()).await;
    assert_eq!((ended.state, ended.exit_code), (State::Exited, Some(0)));
    tokio::time::timeout(Duration::from_secs(10), async {
        while notices(&runtime, thread).await.is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    // Anywhere else nothing moves: the call waits as it always did.
    let other = chat(&runtime, None).await;
    let turn = scripted(&runtime, other, script, false).await;
    wait_done(&runtime, &turn).await;
    let (text, metadata) = last_result(&runtime, other).await;
    assert!(text.starts_with("exit: 0\n"), "{text}");
    assert!(text.contains("finished"), "{text}");
    assert_eq!(metadata["command"]["background"], false);
    assert!(notices(&runtime, other).await.is_empty());
    assert!(checkpoints(&runtime, other).await.is_empty());
}

#[tokio::test]
async fn a_person_moves_and_stops_a_command_and_the_agent_is_told() {
    use haarness_protocol::{ShellCommandEndedBy, ShellCommandMovedBy, ShellCommandState as State};
    let workspace = Workspace::new();
    let (runtime, _) = commands_runtime(&workspace.0, |_| {});
    let thread = chat(&runtime, None).await;
    let turn = scripted(
        &runtime,
        thread,
        r#"!tool bash {"command":"printf 'serving\n'; sleep 30"}"#,
        false,
    )
    .await;
    let id = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(info) = runtime.shell_commands(thread).await.unwrap().pop() {
                return info.id;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let moved = runtime
        .background_shell_commands(thread, None)
        .await
        .unwrap();
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].moved_by, Some(ShellCommandMovedBy::Person));
    wait_done(&runtime, &turn).await;
    let (text, _) = last_result(&runtime, thread).await;
    assert!(
        text.contains("the person sent it to the background"),
        "{text}"
    );
    // Moving it again is refused; so is an unknown command.
    assert!(matches!(
        runtime.background_shell_commands(thread, Some(&id)).await,
        Err(haarness_core::CoreError::Conflict(_))
    ));
    let stopping = runtime.stop_shell_command(thread, &id).await.unwrap();
    assert!(matches!(stopping.state, State::Stopping | State::Stopped));
    let stopped = command_until(&runtime, thread, &id, |info| !info.state.is_live()).await;
    assert_eq!(stopped.state, State::Stopped);
    assert_eq!(stopped.ended_by, Some(ShellCommandEndedBy::Person));
    assert_eq!(stopped.signal.as_deref(), Some("SIGINT"));
    // A second stop changes nothing.
    assert_eq!(
        runtime.stop_shell_command(thread, &id).await.unwrap().state,
        State::Stopped
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while notices(&runtime, thread).await.is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let told = notices(&runtime, thread).await;
    assert!(
        told[0].contains("state=\"stopped\"") && told[0].contains("stopped by the person"),
        "{}",
        told[0]
    );
}

#[tokio::test]
async fn a_stop_during_a_reply_spares_background_commands_and_a_stop_when_idle_ends_them() {
    use haarness_protocol::{ShellCommandEndedBy, ShellCommandState as State};
    let workspace = Workspace::new();
    let (runtime, _) = commands_runtime(&workspace.0, |_| {});
    let thread = chat(&runtime, None).await;
    let turn = scripted(
        &runtime,
        thread,
        r#"!tool bash {"command":"sleep 30","run_in_background":true}"#,
        true,
    )
    .await;
    wait_done(&runtime, &turn).await;
    let background = last_result(&runtime, thread).await.1["command"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let turn = scripted(
        &runtime,
        thread,
        r#"!tool bash {"command":"sleep 30"}"#,
        true,
    )
    .await;
    let foreground = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(info) = runtime
                .shell_commands(thread)
                .await
                .unwrap()
                .into_iter()
                .find(|info| !info.background)
            {
                return info.id;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    runtime.interrupt_thread_tree(thread).await.unwrap();
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Interrupted
    );
    let stopped = command_until(&runtime, thread, &foreground, |info| !info.state.is_live()).await;
    assert_eq!(stopped.ended_by, Some(ShellCommandEndedBy::TurnEnded));
    assert!(
        runtime
            .shell_command(thread, &background)
            .await
            .unwrap()
            .state
            .is_live(),
        "the dev server survives a change of course"
    );
    // A composer's Stop that lands after the reply ended (it asks to answer
    // steering) leaves them running too.
    runtime
        .interrupt_thread_tree_with(thread, true)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        runtime
            .shell_command(thread, &background)
            .await
            .unwrap()
            .state
            .is_live(),
        "a late click on Stop leaves the dev server running"
    );
    // An explicit stop with nothing running ends the rest, without telling
    // the agent.
    runtime.interrupt_thread_tree(thread).await.unwrap();
    let ended = command_until(&runtime, thread, &background, |info| !info.state.is_live()).await;
    assert_eq!(
        (ended.state, ended.ended_by),
        (State::Stopped, Some(ShellCommandEndedBy::ChatStopped))
    );
    assert!(!ended.delivers_notice);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(notices(&runtime, thread).await.is_empty());
}

#[tokio::test]
async fn a_subagents_background_command_stops_untold_when_its_turn_ends() {
    use haarness_protocol::{ShellCommandEndedBy, ShellCommandState as State};
    let workspace = Workspace::new();
    let (runtime, _) = commands_runtime(&workspace.0, |_| {});
    let thread = chat(&runtime, None).await;
    let turn = scripted(
        &runtime,
        thread,
        r#"!tool task {"prompt":"!tool bash {\"command\":\"sleep 30\",\"run_in_background\":true}","description":"Starts a server"}"#,
        true,
    )
    .await;
    wait_done(&runtime, &turn).await;
    let commands = runtime.shell_commands(thread).await.unwrap();
    assert_eq!(commands.len(), 1, "{commands:?}");
    let command = &commands[0];
    assert_ne!(command.thread_id, thread);
    assert_eq!(command.root_thread_id, thread);
    assert_eq!(
        (command.state, command.ended_by),
        (State::Stopped, Some(ShellCommandEndedBy::TurnEnded))
    );
    assert!(!command.delivers_notice);
    assert!(notices(&runtime, command.thread_id).await.is_empty());
    assert!(notices(&runtime, thread).await.is_empty());
}

#[tokio::test]
async fn the_agent_reads_waits_for_and_stops_its_commands() {
    use haarness_protocol::ShellCommandState as State;
    let workspace = Workspace::new();
    let (runtime, _) = commands_runtime(&workspace.0, |_| {});
    let thread = chat(&runtime, None).await;
    let turn = scripted(
        &runtime,
        thread,
        r#"!tool bash {"command":"printf 'Server READY on 3000\n'; sleep 30","run_in_background":true}"#,
        true,
    )
    .await;
    wait_done(&runtime, &turn).await;
    let id = last_result(&runtime, thread).await.1["command"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    command_until(&runtime, thread, &id, |info| info.output_bytes > 0).await;
    // The match is in output that came before the wait began, and a
    // leading (?i) ignores case.
    let turn = scripted(
        &runtime,
        thread,
        &format!(
            r#"!tool bash_output {{"id":"{id}","wait_seconds":5,"until":"match","pattern":"(?i)ready on \\d+"}}"#
        ),
        true,
    )
    .await;
    wait_done(&runtime, &turn).await;
    let (text, metadata) = last_result(&runtime, thread).await;
    assert!(text.contains("Matched: Server READY on 3000"), "{text}");
    assert_eq!(metadata["command"]["id"], id.as_str());
    // Without an id it lists the chat's commands.
    let turn = scripted(&runtime, thread, "!tool bash_output {}", true).await;
    wait_done(&runtime, &turn).await;
    assert!(last_result(&runtime, thread).await.0.contains(&id));
    let turn = scripted(
        &runtime,
        thread,
        &format!(r#"!tool bash_stop {{"id":"{id}"}}"#),
        true,
    )
    .await;
    wait_done(&runtime, &turn).await;
    let (text, _) = last_result(&runtime, thread).await;
    assert!(text.contains("was stopped"), "{text}");
    let info = runtime.shell_command(thread, &id).await.unwrap();
    assert_eq!(info.state, State::Stopped);
    // The agent stopped it itself: no notice follows.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(notices(&runtime, thread).await.is_empty());
}

#[tokio::test]
async fn a_chat_runs_no_more_background_commands_than_its_cap_and_archiving_stops_them() {
    let workspace = Workspace::new();
    let (runtime, _) = commands_runtime(&workspace.0, |settings| {
        settings.shell_background_per_chat = 1;
    });
    let thread = chat(&runtime, None).await;
    let start = r#"!tool bash {"command":"sleep 30","run_in_background":true}"#;
    let turn = scripted(&runtime, thread, start, true).await;
    wait_done(&runtime, &turn).await;
    let turn = scripted(&runtime, thread, start, true).await;
    wait_done(&runtime, &turn).await;
    let (text, _) = last_result(&runtime, thread).await;
    assert!(text.contains("already runs 1 commands"), "{text}");
    assert_eq!(runtime.shell_commands(thread).await.unwrap().len(), 1);
    runtime
        .update_thread(
            thread,
            haarness_protocol::UpdateThreadRequest {
                title: None,
                archived: Some(true),
                agent: None,
                provider: None,
                model: None,
                ultracode: None,
            },
        )
        .await
        .unwrap();
    let id = runtime.shell_commands(thread).await.unwrap()[0].id.clone();
    let stopped = command_until(&runtime, thread, &id, |info| !info.state.is_live()).await;
    assert_eq!(
        stopped.ended_by,
        Some(haarness_protocol::ShellCommandEndedBy::ChatStopped)
    );
}

#[tokio::test]
async fn an_agent_command_without_a_deadline_saves_only_the_start_of_its_output() {
    let workspace = Workspace::new();
    let (runtime, _) = commands_runtime(&workspace.0, |settings| {
        settings.shell_timeout_ms = 0;
    });
    let thread = chat(&runtime, None).await;
    let past = haarness_core::OUTPUT_SAVED_LIMIT + 1024 * 1024;
    let turn = scripted(
        &runtime,
        thread,
        &format!(r#"!tool bash {{"command":"yes 0123456789 | head -c {past}"}}"#),
        false,
    )
    .await;
    wait_done(&runtime, &turn).await;
    let (text, metadata) = last_result(&runtime, thread).await;
    assert_eq!(
        metadata["outputSavedBytes"],
        haarness_core::OUTPUT_SAVED_LIMIT,
        "{text:.200}"
    );
    let path = metadata["outputPath"].as_str().unwrap();
    assert_eq!(
        std::fs::metadata(path).unwrap().len(),
        haarness_core::OUTPUT_SAVED_LIMIT
    );
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn a_foreground_command_a_person_stopped_says_so_in_its_result() {
    let workspace = Workspace::new();
    let (runtime, _) = commands_runtime(&workspace.0, |_| {});
    let thread = chat(&runtime, None).await;
    let turn = scripted(
        &runtime,
        thread,
        r#"!tool bash {"command":"printf 'working\n'; sleep 30"}"#,
        false,
    )
    .await;
    let id = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(info) = runtime.shell_commands(thread).await.unwrap().pop()
                && info.output_bytes > 0
            {
                return info.id;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    runtime.stop_shell_command(thread, &id).await.unwrap();
    wait_done(&runtime, &turn).await;
    let (text, metadata) = last_result(&runtime, thread).await;
    assert!(text.contains("The person stopped this command"), "{text}");
    assert!(text.contains("SIGINT"), "{text}");
    assert!(text.contains("working"), "{text}");
    assert_eq!(metadata["command"]["background"], false);
}

/// A store that counts how often every chat is listed.
struct CountingLists {
    inner: SqliteStore,
    lists: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl ThreadRepository for CountingLists {
    async fn create_thread(&self, request: CreateThreadRequest) -> CoreResult<Thread> {
        self.inner.create_thread(request).await
    }
    async fn list_threads(&self, include_archived: bool) -> CoreResult<Vec<Thread>> {
        self.lists.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.list_threads(include_archived).await
    }
    async fn get_thread(&self, id: ThreadId) -> CoreResult<Thread> {
        self.inner.get_thread(id).await
    }
    async fn update_thread(
        &self,
        id: ThreadId,
        request: haarness_protocol::UpdateThreadRequest,
    ) -> CoreResult<Thread> {
        self.inner.update_thread(id, request).await
    }
    async fn list_items(
        &self,
        id: ThreadId,
        after: Option<u64>,
    ) -> CoreResult<Vec<haarness_protocol::Item>> {
        self.inner.list_items(id, after).await
    }
    async fn start_turn(
        &self,
        id: ThreadId,
        request: StartTurnRequest,
    ) -> CoreResult<(Turn, haarness_protocol::Item, bool)> {
        self.inner.start_turn(id, request).await
    }
    async fn get_turn(&self, id: haarness_protocol::TurnId) -> CoreResult<Turn> {
        self.inner.get_turn(id).await
    }
    async fn append_item(
        &self,
        thread: ThreadId,
        turn: Option<haarness_protocol::TurnId>,
        data: haarness_protocol::ItemData,
    ) -> CoreResult<haarness_protocol::Item> {
        self.inner.append_item(thread, turn, data).await
    }
    async fn set_turn_status(
        &self,
        id: haarness_protocol::TurnId,
        status: TurnStatus,
        error: Option<String>,
    ) -> CoreResult<Turn> {
        self.inner.set_turn_status(id, status, error).await
    }
    async fn compact(
        &self,
        id: ThreadId,
        request: haarness_protocol::CompactRequest,
        expected_tail: Option<(haarness_protocol::ItemId, u64)>,
    ) -> CoreResult<haarness_protocol::Item> {
        self.inner.compact(id, request, expected_tail).await
    }
    async fn fork_thread(
        &self,
        id: ThreadId,
        title: Option<String>,
        up_to_sequence: Option<u64>,
    ) -> CoreResult<Thread> {
        self.inner.fork_thread(id, title, up_to_sequence).await
    }
    async fn set_revert_state(
        &self,
        id: ThreadId,
        state: Option<(u64, String, Option<String>)>,
    ) -> CoreResult<()> {
        self.inner.set_revert_state(id, state).await
    }
    async fn soft_delete_items_after(&self, id: ThreadId, sequence: u64) -> CoreResult<u64> {
        self.inner.soft_delete_items_after(id, sequence).await
    }
    async fn save_permission_rule(
        &self,
        workspace: &str,
        rule: &haarness_core::PermissionRule,
    ) -> CoreResult<()> {
        self.inner.save_permission_rule(workspace, rule).await
    }
    async fn load_permission_rules(
        &self,
        workspace: &str,
    ) -> CoreResult<Vec<haarness_core::PermissionRule>> {
        self.inner.load_permission_rules(workspace).await
    }
    async fn recover_interrupted(&self) -> CoreResult<u64> {
        self.inner.recover_interrupted().await
    }
    async fn thread_extras(
        &self,
        id: ThreadId,
    ) -> CoreResult<(Option<String>, Option<haarness_protocol::LoopConfig>)> {
        self.inner.thread_extras(id).await
    }
    async fn set_goal(&self, id: ThreadId, goal: Option<String>) -> CoreResult<()> {
        self.inner.set_goal(id, goal).await
    }
    async fn goal_state(&self, id: ThreadId) -> CoreResult<Option<haarness_protocol::GoalState>> {
        self.inner.goal_state(id).await
    }
    async fn save_goal_state(
        &self,
        id: ThreadId,
        state: Option<haarness_protocol::GoalState>,
    ) -> CoreResult<()> {
        self.inner.save_goal_state(id, state).await
    }
    async fn set_loop(
        &self,
        id: ThreadId,
        config: Option<haarness_protocol::LoopConfig>,
    ) -> CoreResult<()> {
        self.inner.set_loop(id, config).await
    }
}

#[tokio::test]
async fn following_a_chats_commands_never_lists_every_chat() {
    use std::sync::atomic::Ordering;
    let workspace = Workspace::new();
    let store = Arc::new(CountingLists {
        inner: SqliteStore::in_memory().unwrap(),
        lists: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut config = haarness_core::RuntimeConfig::default();
    config.base_rules.push(haarness_core::PermissionRule {
        pattern: "*".into(),
        decision: haarness_core::PermissionDecision::Allow,
    });
    let runtime = AgentRuntime::with_config(
        store.clone(),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(workspace.0.clone())),
        EventBus::new(256),
        workspace.0.clone(),
        String::new(),
        config,
    );
    let thread = chat(&runtime, None).await;
    let turn = scripted(
        &runtime,
        thread,
        r#"!tool bash {"command":"sleep 30","run_in_background":true}"#,
        true,
    )
    .await;
    wait_done(&runtime, &turn).await;
    let id = last_result(&runtime, thread).await.1["command"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    store.lists.store(0, Ordering::SeqCst);
    // What a client polls while it shows the chat.
    for _ in 0..5 {
        assert_eq!(
            runtime
                .thread_activity(thread)
                .await
                .unwrap()
                .background_commands,
            1
        );
        assert_eq!(runtime.shell_commands(thread).await.unwrap().len(), 1);
        runtime.shell_command(thread, &id).await.unwrap();
        runtime
            .shell_command_output(thread, &id, Some(0), None, false)
            .await
            .unwrap();
    }
    assert_eq!(store.lists.load(Ordering::SeqCst), 0);
    runtime.stop_shell_command(thread, &id).await.unwrap();
}

#[tokio::test]
async fn a_commands_end_starts_its_turn_with_the_configuration_of_that_moment() {
    let workspace = Workspace::new();
    let (runtime, first) = commands_runtime(&workspace.0, |_| {});
    let thread = chat(&runtime, None).await;
    let turn = scripted(
        &runtime,
        thread,
        r#"!tool bash {"command":"sleep 1","run_in_background":true}"#,
        true,
    )
    .await;
    wait_done(&runtime, &turn).await;
    // The configuration is reloaded while the command runs.
    let second = Capturing::default();
    let mut config = haarness_core::RuntimeConfig::default();
    config.base_rules.push(haarness_core::PermissionRule {
        pattern: "*".into(),
        decision: haarness_core::PermissionDecision::Allow,
    });
    assert!(runtime.replace_components_when_idle(
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(second.clone()) as Arc<dyn ModelProvider>
        ])),
        Arc::new(NativeToolRegistry::new(workspace.0.clone())),
        String::new(),
        config,
        None,
        || {},
    ));
    let before = first.requests.lock().unwrap().len();
    tokio::time::timeout(Duration::from_secs(10), async {
        while notices(&runtime, thread).await.is_empty()
            || runtime.active_turn(thread).await.unwrap().is_some()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        first.requests.lock().unwrap().len(),
        before,
        "the notice's turn ran on the configuration the command started with"
    );
    assert!(!second.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn bash_output_and_bash_stop_are_offered_only_where_bash_is() {
    let workspace = Workspace::new();
    let (runtime, provider) = commands_runtime(&workspace.0, |_| {});
    let offered = |provider: &Capturing| {
        provider
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<std::collections::HashSet<_>>()
    };
    let thread = chat(&runtime, None).await;
    let turn = scripted(&runtime, thread, "hello", true).await;
    wait_done(&runtime, &turn).await;
    let tools = offered(&provider);
    assert!(tools.contains("bash_output") && tools.contains("bash_stop"));
    // A turn without bash gets neither, and a call of one is refused.
    let before = provider.requests.lock().unwrap().len();
    let mut request = turn_request("!tool bash_output {}");
    request.tools = Some(std::collections::HashMap::from([("bash".into(), false)]));
    let turn = runtime.start_turn(thread, request).await.unwrap();
    wait_done(&runtime, &turn).await;
    for request in &provider.requests.lock().unwrap()[before..] {
        let tools = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        assert!(
            !tools.contains(&"bash")
                && !tools.contains(&"bash_output")
                && !tools.contains(&"bash_stop"),
            "{tools:?}"
        );
    }
    let (text, _) = last_result(&runtime, thread).await;
    assert!(text.contains("disabled"), "{text}");
    // Nor an agent whose rules deny bash.
    let mut engine = haarness_core::PermissionEngine::build_defaults();
    engine.add_rule(haarness_core::PermissionRule {
        pattern: "bash:*".into(),
        decision: haarness_core::PermissionDecision::Deny,
    });
    assert!(engine.disables_tool("bash_output") && engine.disables_tool("bash_stop"));
    assert!(!haarness_core::PermissionEngine::build_defaults().disables_tool("bash_output"));
}

#[tokio::test]
async fn archiving_a_chat_while_a_command_stops_tells_nobody() {
    use haarness_protocol::ShellCommandEndedBy;
    let workspace = Workspace::new();
    let (runtime, _) = commands_runtime(&workspace.0, |settings| {
        settings.shell_stop_grace_ms = 1_500;
    });
    let thread = chat(&runtime, None).await;
    let turn = scripted(
        &runtime,
        thread,
        r#"!tool bash {"command":"trap '' INT; printf ready; sleep 30","run_in_background":true}"#,
        true,
    )
    .await;
    wait_done(&runtime, &turn).await;
    let id = last_result(&runtime, thread).await.1["command"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    command_until(&runtime, thread, &id, |info| info.output_bytes > 0).await;
    // A person stops it; it ignores the interrupt, and the chat is
    // archived before the terminate.
    runtime.stop_shell_command(thread, &id).await.unwrap();
    runtime
        .update_thread(
            thread,
            haarness_protocol::UpdateThreadRequest {
                title: None,
                archived: Some(true),
                agent: None,
                provider: None,
                model: None,
                ultracode: None,
            },
        )
        .await
        .unwrap();
    let ended = command_until(&runtime, thread, &id, |info| !info.state.is_live()).await;
    assert_eq!(ended.ended_by, Some(ShellCommandEndedBy::ChatStopped));
    assert!(!ended.delivers_notice);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(notices(&runtime, thread).await.is_empty());
}

#[tokio::test]
async fn a_chat_at_its_cap_keeps_a_long_command_in_the_foreground() {
    let workspace = Workspace::new();
    let (runtime, _) = commands_runtime(&workspace.0, |settings| {
        settings.shell_background_per_chat = 1;
        settings.shell_move_after_ms = 300;
    });
    let thread = chat(&runtime, None).await;
    let turn = scripted(
        &runtime,
        thread,
        r#"!tool bash {"command":"sleep 30","run_in_background":true}"#,
        true,
    )
    .await;
    wait_done(&runtime, &turn).await;
    let server = last_result(&runtime, thread).await.1["command"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // It would move after 300 ms, but the chat runs as many in the
    // background as it may: it stays, and the call waits for it.
    let turn = scripted(
        &runtime,
        thread,
        r#"!tool bash {"command":"sleep 1.5; echo held"}"#,
        true,
    )
    .await;
    let held = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(info) = runtime
                .shell_commands(thread)
                .await
                .unwrap()
                .into_iter()
                .find(|info| info.id != server)
            {
                return info;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    // Nor can a person send it there.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(matches!(
        runtime
            .background_shell_commands(thread, Some(&held.id))
            .await,
        Err(haarness_core::CoreError::Conflict(_))
    ));
    wait_done(&runtime, &turn).await;
    let (text, metadata) = last_result(&runtime, thread).await;
    assert!(text.starts_with("exit: 0\nheld"), "{text}");
    assert_eq!(metadata["command"]["background"], false);
    runtime.stop_shell_command(thread, &server).await.unwrap();
}

#[tokio::test]
async fn a_command_a_person_moves_after_the_deadline_gets_its_limit_from_the_move() {
    let workspace = Workspace::new();
    let (runtime, _) = commands_runtime(&workspace.0, |settings| {
        settings.shell_timeout_ms = 600;
    });
    let thread = chat(&runtime, None).await;
    // No deadline in the foreground (timeout 0), in a chat nothing follows
    // up; it has run past the configured deadline when a person moves it.
    let turn = scripted(
        &runtime,
        thread,
        r#"!tool bash {"command":"sleep 30","timeout":0}"#,
        false,
    )
    .await;
    let id = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(info) = runtime.shell_commands(thread).await.unwrap().pop() {
                return info.id;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(900)).await;
    let moved = runtime
        .background_shell_commands(thread, Some(&id))
        .await
        .unwrap();
    let stops_at = moved[0].stops_at.expect("a limit in the background");
    assert!(stops_at > chrono::Utc::now(), "{moved:?}");
    wait_done(&runtime, &turn).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        runtime
            .shell_command(thread, &id)
            .await
            .unwrap()
            .state
            .is_live(),
        "a move must not stop it"
    );
    runtime.stop_shell_command(thread, &id).await.unwrap();
}

#[tokio::test]
async fn a_command_ends_when_its_shell_does_though_what_it_left_never_stops_printing() {
    let workspace = Workspace::new();
    let (runtime, _) = commands_runtime(&workspace.0, |_| {});
    let thread = chat(&runtime, None).await;
    let began = std::time::Instant::now();
    let turn = scripted(
        &runtime,
        thread,
        r#"!tool bash {"command":"yes printing & sleep 1; echo started","timeout":20000}"#,
        false,
    )
    .await;
    wait_done(&runtime, &turn).await;
    let (text, metadata) = last_result(&runtime, thread).await;
    assert!(
        began.elapsed() < Duration::from_secs(8),
        "{:?}",
        began.elapsed()
    );
    assert!(text.starts_with("exit: 0"), "{text:.200}");
    assert_eq!(metadata["leftovers"], true);
    if let Some(path) = metadata["outputPath"].as_str() {
        let _ = std::fs::remove_file(path);
    }
}
